//! Capability-style filesystem opens for immutable media artifacts.
//!
//! Path validation followed by an ordinary open is not a security boundary:
//! an intermediate directory can be swapped for a symlink between those two
//! operations. These helpers walk an absolute path from `/`, or a relative
//! path from a held current-directory descriptor, one component at a time
//! with `openat(O_NOFOLLOW)` and keep the resulting descriptor as authority.

use std::ffi::{CStr, CString, OsString};
use std::fs::File;
use std::io;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

fn invalid_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn component_name(component: Component<'_>) -> io::Result<CString> {
    match component {
        Component::Normal(name) => CString::new(name.as_bytes())
            .map_err(|_| invalid_path("filesystem path contains a NUL byte")),
        _ => Err(invalid_path("filesystem path contains an unsafe component")),
    }
}

fn openat_owned(parent: RawFd, name: &CString, flags: i32) -> io::Result<OwnedFd> {
    // SAFETY: `parent` is an open directory descriptor, `name` is NUL
    // terminated, and the returned descriptor is immediately owned.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `openat` returned a new descriptor that no other owner has.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn independent_directory_stream(directory: &File) -> io::Result<*mut libc::DIR> {
    // `dup` would share the directory cursor with the capability and make a
    // second traversal start at EOF. Opening `.` relative to the held inode
    // creates an independent open-file description without returning to a
    // pathname an attacker can swap.
    let current = CString::new(".").expect("static directory child");
    let fd = openat_owned(
        directory.as_raw_fd(),
        &current,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    )?
    .into_raw_fd();
    let entries = unsafe { libc::fdopendir(fd) };
    if entries.is_null() {
        unsafe { libc::close(fd) };
        Err(io::Error::last_os_error())
    } else {
        Ok(entries)
    }
}

/// Zero the calling thread's `errno`, reporting whether this target exposes
/// the slot at all.
///
/// `readdir` signals end-of-directory and a failed read the same way — a NULL
/// return — and only `errno` separates them. Clearing before the call is the
/// only way to know that a non-zero value afterwards belongs to `readdir`
/// rather than to some earlier, successful syscall.
#[must_use]
fn clear_errno() -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: the returned pointer is this thread's private errno slot.
        unsafe { *libc::__errno_location() = 0 };
        true
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        // SAFETY: the returned pointer is this thread's private errno slot.
        unsafe { *libc::__error() = 0 };
        true
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        false
    }
}

/// Read one directory entry, distinguishing end-of-directory from a failed
/// read.
///
/// Every traversal in this file walks a directory until `readdir` returns
/// NULL. On a faulting backing store — FUSE, NFS, mergerfs, exactly the shape
/// a media host uses for scratch — an `EIO` from `readdir` is indistinguishable
/// from an exhausted directory unless `errno` is inspected, and a populated
/// stranger's directory then looks empty enough to claim and erase.
///
/// # Safety
/// `entries` must be a live `DIR` pointer. The returned pointer borrows the
/// stream's internal entry and stays valid until the next call on that stream.
unsafe fn readdir_checked(entries: *mut libc::DIR) -> io::Result<Option<*mut libc::dirent>> {
    let cleared = clear_errno();
    let entry = libc::readdir(entries);
    if !entry.is_null() {
        return Ok(Some(entry));
    }
    if cleared {
        if let Some(errno) = io::Error::last_os_error()
            .raw_os_error()
            .filter(|errno| *errno != 0)
        {
            return Err(io::Error::from_raw_os_error(errno));
        }
    }
    Ok(None)
}

fn child_name(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(invalid_path("unsafe child filename"));
    }
    CString::new(name).map_err(|_| invalid_path("child filename contains a NUL byte"))
}

fn root_directory() -> io::Result<OwnedFd> {
    let root = CString::new("/").expect("root path has no NUL");
    // SAFETY: the static root path is NUL terminated; ownership transfers to
    // `OwnedFd` on success.
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `open` returned a new descriptor that no other owner has.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn current_directory() -> io::Result<OwnedFd> {
    let current = CString::new(".").expect("current path has no NUL");
    // SAFETY: the static current-directory path is NUL terminated and the
    // returned descriptor is immediately owned.
    let fd = unsafe {
        libc::open(
            current.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `open` returned a new descriptor that no other owner has.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn components(path: &Path) -> io::Result<Vec<Component<'_>>> {
    let components = path
        .components()
        .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(invalid_path("secure filesystem path may not be root"));
    }
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_path("filesystem path contains an unsafe component"));
    }
    Ok(components)
}

fn open_parent(path: &Path) -> io::Result<(OwnedFd, CString)> {
    let mut components = components(path)?;
    let name = component_name(
        components
            .pop()
            .ok_or_else(|| invalid_path("filesystem path has no filename"))?,
    )?;
    let mut directory = if path.is_absolute() {
        root_directory()?
    } else {
        current_directory()?
    };
    for component in components {
        let name = component_name(component)?;
        directory = openat_owned(
            directory.as_raw_fd(),
            &name,
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )?;
    }
    Ok((directory, name))
}

/// Open a regular-file candidate without following any symlink in the path.
/// Relative paths are anchored to a held current-directory descriptor.
/// `O_NONBLOCK` prevents a FIFO/device candidate from pinning the blocking
/// pool before the caller can inspect and reject its metadata.
pub fn open_read_nofollow_blocking(path: &Path) -> io::Result<File> {
    let (parent, name) = open_parent(path)?;
    let fd = openat_owned(
        parent.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    )?;
    Ok(File::from(fd))
}

/// Tokio wrapper around [`open_read_nofollow_blocking`].
pub async fn open_read_nofollow(path: &Path) -> io::Result<tokio::fs::File> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || open_read_nofollow_blocking(&path))
        .await
        .map_err(io::Error::other)?
        .map(tokio::fs::File::from_std)
}

/// Read one regular file through a no-follow capability with an exact physical
/// allocation ceiling. The extra EOF read rejects a concurrent same-prefix
/// replacement or a file that grew after its opened descriptor was measured.
pub async fn read_bounded_regular(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    if max_bytes == 0 || max_bytes > usize::MAX as u64 {
        return Err(invalid_path("invalid bounded-file read ceiling"));
    }
    let file = open_read_nofollow(path).await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(io::Error::other(
            "secure bounded-file candidate is not a regular file within the ceiling",
        ));
    }
    let expected = metadata.len() as usize;
    let mut bytes = Vec::with_capacity(expected.saturating_add(1));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() != expected || bytes.len() as u64 > max_bytes {
        return Err(io::Error::other(
            "secure bounded-file candidate changed while it was read",
        ));
    }
    Ok(bytes)
}

/// Open a directory while refusing every symlink component.
pub fn open_directory_nofollow_blocking(path: &Path) -> io::Result<File> {
    let (parent, name) = open_parent(path)?;
    let fd = openat_owned(
        parent.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    )?;
    Ok(File::from(fd))
}

/// Stable identity of one opened filesystem object. Comparing this after a
/// quarantine rename binds cleanup to the inode that was inspected, rather
/// than to a pathname another process could have replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub changed_seconds: i64,
    pub changed_nanoseconds: i64,
}

impl FileIdentity {
    /// Stable identity across a rename. ctime is intentionally excluded:
    /// Unix updates it for the rename operation itself.
    pub fn same_inode(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

#[allow(clippy::unnecessary_cast)]
fn stat_device(stat: &libc::stat) -> u64 {
    // `dev_t` is already u64 on Linux but narrower on other supported Unix
    // targets, so the explicit normalization is intentionally portable.
    stat.st_dev as u64
}

/// A directory opened component-by-component with `O_NOFOLLOW`. Clone keeps
/// the same directory inode alive, so every later child operation remains
/// relative to that authority even if its original pathname is renamed or
/// replaced.
#[derive(Clone)]
pub struct SecureDirectory {
    file: Arc<File>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecureChildMetadata {
    pub identity: FileIdentity,
    pub is_file: bool,
    pub is_directory: bool,
}

impl SecureDirectory {
    pub async fn open(path: &Path) -> io::Result<Self> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            Ok(Self {
                file: Arc::new(open_directory_nofollow_blocking(&path)?),
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub fn raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub async fn identity(&self) -> io::Result<FileIdentity> {
        let file = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || file_identity(&file))
            .await
            .map_err(io::Error::other)?
    }

    pub async fn open_child_directory(&self, name: &str) -> io::Result<Self> {
        let parent = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            Ok(Self {
                file: Arc::new(File::from(openat_owned(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )?)),
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn create_child_directory(&self, name: &str) -> io::Result<Self> {
        let parent = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            Ok(Self {
                file: Arc::new(File::from(openat_owned(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )?)),
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn child_metadata(&self, name: &str) -> io::Result<SecureChildMetadata> {
        let directory = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            let kind = stat.st_mode & libc::S_IFMT;
            Ok(SecureChildMetadata {
                identity: FileIdentity {
                    device: stat_device(&stat),
                    inode: stat.st_ino,
                    size: stat.st_size.max(0) as u64,
                    changed_seconds: stat.st_ctime,
                    changed_nanoseconds: stat.st_ctime_nsec,
                },
                is_file: kind == libc::S_IFREG,
                is_directory: kind == libc::S_IFDIR,
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn child_names(&self, max_entries: usize) -> io::Result<Vec<String>> {
        if max_entries == 0 || max_entries > 120_100 {
            return Err(invalid_path("invalid directory-listing bound"));
        }
        let directory = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || {
            let entries = independent_directory_stream(&directory)?;
            let result = (|| {
                let mut names = Vec::new();
                loop {
                    let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                        break;
                    };
                    let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                    if child.to_bytes() == b"." || child.to_bytes() == b".." {
                        continue;
                    }
                    if names.len() >= max_entries {
                        return Err(io::Error::other("directory exceeded its listing bound"));
                    }
                    names.push(
                        String::from_utf8(child.to_bytes().to_vec())
                            .map_err(|_| invalid_path("directory child is not UTF-8"))?,
                    );
                }
                Ok(names)
            })();
            unsafe { libc::closedir(entries) };
            result
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn read_bounded_child(&self, name: &str, max_bytes: u64) -> io::Result<Vec<u8>> {
        use std::io::Read;
        if max_bytes == 0 || max_bytes > usize::MAX as u64 {
            return Err(invalid_path("invalid bounded-file read ceiling"));
        }
        let directory = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            let mut file = File::from(openat_owned(
                directory.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )?);
            let before = file_identity(&file)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("bounded child is not a regular file"));
            }
            if before.size > max_bytes {
                return Err(io::Error::other("bounded child exceeds its byte cap"));
            }
            let mut bytes = Vec::with_capacity(before.size as usize);
            std::io::Read::by_ref(&mut file)
                .take(max_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 != before.size || file_identity(&file)? != before {
                return Err(io::Error::other("bounded child changed while it was read"));
            }
            Ok(bytes)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn open_read_child(&self, name: &str) -> io::Result<tokio::fs::File> {
        let directory = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            Ok(tokio::fs::File::from_std(File::from(openat_owned(
                directory.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )?)))
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Append bytes to an existing regular child and durably flush them while
    /// enforcing a total-file ceiling from the same opened descriptor. A
    /// torn final write remains detectable by record-oriented callers, while
    /// successful progress never requires rewriting the existing prefix.
    pub async fn append_bounded_child(
        &self,
        name: &str,
        bytes: &[u8],
        max_bytes: u64,
    ) -> io::Result<bool> {
        if max_bytes == 0 || bytes.len() as u64 > max_bytes {
            return Err(invalid_path("invalid bounded append ceiling"));
        }
        let directory = Arc::clone(&self.file);
        let name = name.to_owned();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            let mut file = File::from(openat_owned(
                directory.as_raw_fd(),
                &name,
                libc::O_WRONLY
                    | libc::O_APPEND
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )?);
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::other(
                    "bounded append child is not a regular file",
                ));
            }
            if metadata.len().saturating_add(bytes.len() as u64) > max_bytes {
                return Ok(false);
            }
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(true)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn unlink_child(&self, name: &str) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            directory.sync_all()
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn atomic_write_child(&self, destination: &str, bytes: &[u8]) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let destination = destination.to_owned();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let destination = child_name(&destination)?;
            let temporary = child_name(&format!(
                ".{}.{}.part",
                destination.to_string_lossy(),
                uuid::Uuid::new_v4().simple()
            ))?;
            let raw = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    temporary.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let result = (|| {
                file.write_all(&bytes)?;
                file.sync_all()?;
                if unsafe {
                    libc::renameat(
                        directory.as_raw_fd(),
                        temporary.as_ptr(),
                        directory.as_raw_fd(),
                        destination.as_ptr(),
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                directory.sync_all()
            })();
            if result.is_err() {
                unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
            }
            result
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn rename_child(&self, from: &str, to: &str) -> io::Result<()> {
        let directory = Arc::clone(&self.file);
        let from = from.to_owned();
        let to = to.to_owned();
        tokio::task::spawn_blocking(move || {
            let from = child_name(&from)?;
            let to = child_name(&to)?;
            if unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    from.as_ptr(),
                    directory.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            directory.sync_all()
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn rename_child_noreplace(&self, from: &str, to: &str) -> io::Result<bool> {
        let directory = Arc::clone(&self.file);
        let from = from.to_owned();
        let to = to.to_owned();
        tokio::task::spawn_blocking(move || {
            let from = child_name(&from)?;
            let to = child_name(&to)?;
            #[cfg(target_os = "macos")]
            let renamed = unsafe {
                libc::renameatx_np(
                    directory.as_raw_fd(),
                    from.as_ptr(),
                    directory.as_raw_fd(),
                    to.as_ptr(),
                    libc::RENAME_EXCL,
                )
            };
            #[cfg(target_os = "linux")]
            let renamed = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    directory.as_raw_fd(),
                    from.as_ptr(),
                    directory.as_raw_fd(),
                    to.as_ptr(),
                    libc::RENAME_NOREPLACE,
                ) as i32
            };
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let renamed = -1;
            if renamed == 0 {
                directory.sync_all()?;
                return Ok(true);
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exclusive directory rename is unsupported on this platform",
            ));
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn remove_child_tree(
        &self,
        name: &str,
        max_entries: usize,
        max_depth: usize,
    ) -> io::Result<()> {
        if max_entries == 0 || max_entries > 120_100 || max_depth == 0 || max_depth > 4 {
            return Err(invalid_path("invalid cache-tree removal bound"));
        }
        let parent = Arc::clone(&self.file);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let name = child_name(&name)?;
            let directory = File::from(openat_owned(
                parent.as_raw_fd(),
                &name,
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )?);
            let mut counted = 0usize;
            count_directory_capability(&directory, &mut counted, max_entries, 0, max_depth)?;
            let mut removed = 0usize;
            clear_directory_capability(&directory, &mut removed, max_entries, 0, max_depth)?;
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            parent.sync_all()
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn rename_child_to(
        &self,
        from: &str,
        destination: &Path,
        to: &str,
    ) -> io::Result<()> {
        let source = Arc::clone(&self.file);
        let destination = destination.to_owned();
        let from = from.to_owned();
        let to = to.to_owned();
        tokio::task::spawn_blocking(move || {
            let destination = open_directory_nofollow_blocking(&destination)?;
            let from = child_name(&from)?;
            let to = child_name(&to)?;
            if unsafe {
                libc::renameat(
                    source.as_raw_fd(),
                    from.as_ptr(),
                    destination.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            source.sync_all()?;
            destination.sync_all()
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn place_regular_child_from(
        &self,
        source: &SecureDirectory,
        source_name: &str,
        destination_name: &str,
        max_bytes: u64,
    ) -> io::Result<u64> {
        use std::io::{Read, Seek, SeekFrom};
        let destination = Arc::clone(&self.file);
        let source = Arc::clone(&source.file);
        let source_name = source_name.to_owned();
        let destination_name = destination_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let source_name = child_name(&source_name)?;
            let destination_name = child_name(&destination_name)?;
            let mut input = File::from(openat_owned(
                source.as_raw_fd(),
                &source_name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )?);
            let identity = file_identity(&input)?;
            if !input.metadata()?.is_file() || identity.size > max_bytes {
                return Err(io::Error::other(
                    "source child exceeds its regular-file bound",
                ));
            }
            let linked = unsafe {
                libc::linkat(
                    source.as_raw_fd(),
                    source_name.as_ptr(),
                    destination.as_raw_fd(),
                    destination_name.as_ptr(),
                    0,
                )
            };
            if linked == 0 {
                let placed = File::from(openat_owned(
                    destination.as_raw_fd(),
                    &destination_name,
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )?);
                let placed_identity = file_identity(&placed)?;
                if !placed_identity.same_inode(identity) || placed_identity.size != identity.size {
                    unsafe {
                        libc::unlinkat(destination.as_raw_fd(), destination_name.as_ptr(), 0)
                    };
                    return Err(io::Error::other("source changed while it was linked"));
                }
                // The file inode already existed, but this directory entry did
                // not. Persist the destination directory before a caller can
                // rename and publish its enclosing generation.
                destination.sync_all()?;
                return Ok(identity.size);
            }
            let link_error = io::Error::last_os_error();
            if !matches!(
                link_error.raw_os_error(),
                Some(libc::EXDEV) | Some(libc::EPERM) | Some(libc::EOPNOTSUPP)
            ) {
                return Err(link_error);
            }
            let raw = unsafe {
                libc::openat(
                    destination.as_raw_fd(),
                    destination_name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut output = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let result = (|| {
                input.seek(SeekFrom::Start(0))?;
                let copied = io::copy(
                    &mut std::io::Read::by_ref(&mut input).take(max_bytes.saturating_add(1)),
                    &mut output,
                )?;
                output.sync_all()?;
                if copied != identity.size
                    || file_identity(&input)? != identity
                    || output.metadata()?.len() != identity.size
                {
                    return Err(io::Error::other("source changed while it was copied"));
                }
                Ok(identity.size)
            })();
            match result {
                Ok(size) => {
                    // `output.sync_all()` persists contents; this persists the
                    // new name in the destination directory as well.
                    destination.sync_all()?;
                    Ok(size)
                }
                Err(error) => {
                    unsafe {
                        libc::unlinkat(destination.as_raw_fd(), destination_name.as_ptr(), 0)
                    };
                    let _ = destination.sync_all();
                    Err(error)
                }
            }
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// Remove one empty directory beneath a held parent directory.
///
/// The operation stays relative to the parent capability, refuses symlinks,
/// and fails with `DirectoryNotEmpty` when a concurrent producer has placed a
/// new child in the directory. It is therefore safe for housekeeping to use
/// after removing the final known cache generation from a fanout prefix.
pub async fn remove_empty_directory_child(directory: &Path, name: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let name = child_name(&name)?;
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        directory.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}

fn clear_directory_capability(
    directory: &File,
    removed: &mut usize,
    max_entries: usize,
    depth: usize,
    max_depth: usize,
) -> io::Result<()> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| {
        loop {
            let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                break;
            };
            let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if child.to_bytes() == b"." || child.to_bytes() == b".." {
                continue;
            }
            *removed = removed.saturating_add(1);
            if *removed > max_entries {
                return Err(io::Error::other(
                    "cache tree exceeded its removal entry bound",
                ));
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    child.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(error);
            }
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                if depth >= max_depth {
                    return Err(io::Error::other(
                        "cache tree exceeded its removal depth bound",
                    ));
                }
                let child_name = CString::new(child.to_bytes())
                    .map_err(|_| invalid_path("cache child contains NUL"))?;
                let child_directory = File::from(openat_owned(
                    directory.as_raw_fd(),
                    &child_name,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )?);
                clear_directory_capability(
                    &child_directory,
                    removed,
                    max_entries,
                    depth + 1,
                    max_depth,
                )?;
                if unsafe {
                    libc::unlinkat(directory.as_raw_fd(), child.as_ptr(), libc::AT_REMOVEDIR)
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
            } else if unsafe { libc::unlinkat(directory.as_raw_fd(), child.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    })();
    unsafe { libc::closedir(entries) };
    result
}

fn count_directory_capability(
    directory: &File,
    counted: &mut usize,
    max_entries: usize,
    depth: usize,
    max_depth: usize,
) -> io::Result<()> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| {
        loop {
            let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                break;
            };
            let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if child.to_bytes() == b"." || child.to_bytes() == b".." {
                continue;
            }
            *counted = counted.saturating_add(1);
            if *counted > max_entries {
                return Err(io::Error::other(
                    "cache tree exceeded its removal entry bound",
                ));
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    child.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(error);
            }
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                if depth >= max_depth {
                    return Err(io::Error::other(
                        "cache tree exceeded its removal depth bound",
                    ));
                }
                let child_name = CString::new(child.to_bytes())
                    .map_err(|_| invalid_path("cache child contains NUL"))?;
                let child_directory = File::from(openat_owned(
                    directory.as_raw_fd(),
                    &child_name,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )?);
                count_directory_capability(
                    &child_directory,
                    counted,
                    max_entries,
                    depth + 1,
                    max_depth,
                )?;
            }
        }
        Ok(())
    })();
    unsafe { libc::closedir(entries) };
    result
}

/// Inspect one directory through a no-follow descriptor and return its inode
/// identity for a later quarantine comparison.
pub fn directory_identity_nofollow_blocking(path: &Path) -> io::Result<FileIdentity> {
    let directory = open_directory_nofollow_blocking(path)?;
    file_identity(&directory)
}

/// Return the held directory and parent inode chain through the current mount
/// namespace. This catches ordinary path aliases; Linux bind-source ancestry
/// additionally requires [`directory_source_ancestry_alias_blocking`].
pub fn directory_ancestor_identities_nofollow_blocking(
    path: &Path,
) -> io::Result<Vec<FileIdentity>> {
    let mut directory = open_directory_nofollow_blocking(path)?;
    let parent_name = CString::new("..").expect("static parent component");
    let mut identities = Vec::new();
    for _ in 0..256 {
        let identity = file_identity(&directory)?;
        identities.push(identity);
        let parent = File::from(openat_owned(
            directory.as_raw_fd(),
            &parent_name,
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )?);
        let parent_identity = file_identity(&parent)?;
        if parent_identity.same_inode(identity) {
            return Ok(identities);
        }
        directory = parent;
    }
    Err(io::Error::other(
        "directory ancestry exceeded the filesystem depth bound",
    ))
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct LinuxMountCoordinate {
    major: u64,
    minor: u64,
    path_within_filesystem: PathBuf,
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(encoded: &str) -> io::Result<PathBuf> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if index + 3 >= bytes.len()
                || !bytes[index + 1..=index + 3]
                    .iter()
                    .all(|byte| matches!(byte, b'0'..=b'7'))
            {
                return Err(invalid_path("invalid escaped path in Linux mountinfo"));
            }
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

#[cfg(target_os = "linux")]
fn normalized_absolute_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid_path("mount coordinate is not absolute"));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(invalid_path("mount coordinate escapes filesystem root"));
                }
            }
            Component::Prefix(_) => {
                return Err(invalid_path("invalid Linux mount coordinate prefix"));
            }
        }
    }
    Ok(normalized)
}

#[cfg(target_os = "linux")]
fn linux_mount_coordinate_from(
    mountinfo: &str,
    path: &Path,
    mount_id: u64,
) -> io::Result<LinuxMountCoordinate> {
    if !path.is_absolute() {
        return Err(invalid_path("storage root is not absolute"));
    }
    let mut selected: Option<(u64, u64, PathBuf, PathBuf)> = None;
    for line in mountinfo.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 || !fields.contains(&"-") {
            return Err(invalid_path("malformed Linux mountinfo record"));
        }
        let record_mount_id = fields[0]
            .parse::<u64>()
            .map_err(|_| invalid_path("invalid Linux mountinfo mount id"))?;
        if record_mount_id != mount_id {
            continue;
        }
        let (major, minor) = fields[2]
            .split_once(':')
            .ok_or_else(|| invalid_path("invalid Linux mountinfo device"))?;
        let major = major
            .parse::<u64>()
            .map_err(|_| invalid_path("invalid Linux mountinfo major device"))?;
        let minor = minor
            .parse::<u64>()
            .map_err(|_| invalid_path("invalid Linux mountinfo minor device"))?;
        let filesystem_root = decode_mountinfo_path(fields[3])?;
        let mount_point = decode_mountinfo_path(fields[4])?;
        if !path.starts_with(&mount_point) {
            continue;
        }
        if selected.is_some() {
            return Err(io::Error::other(
                "Linux mountinfo contains duplicate records for one mount id",
            ));
        }
        selected = Some((major, minor, filesystem_root, mount_point));
    }
    let (major, minor, filesystem_root, mount_point) =
        selected.ok_or_else(|| io::Error::other("storage root has no Linux mountinfo entry"))?;
    let relative = path
        .strip_prefix(&mount_point)
        .map_err(|_| io::Error::other("storage root escaped its selected mount point"))?;
    let path_within_filesystem = normalized_absolute_path(&filesystem_root.join(relative))?;
    Ok(LinuxMountCoordinate {
        major,
        minor,
        path_within_filesystem,
    })
}

#[cfg(target_os = "linux")]
fn linux_directory_mount_id(path: &Path) -> io::Result<u64> {
    // Bind the namespace record to the exact no-follow descriptor used for
    // identity validation. This avoids selecting a hidden lower record when
    // two mounts are stacked at one visible mount point.
    let directory = open_directory_nofollow_blocking(path)?;
    let fdinfo = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", directory.as_raw_fd()))?;
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .map(str::trim)
        .ok_or_else(|| io::Error::other("directory fdinfo has no Linux mount id"))?
        .parse::<u64>()
        .map_err(|_| io::Error::other("directory fdinfo has an invalid Linux mount id"))
}

/// Return whether two Linux directory paths name the same underlying
/// filesystem subtree in either direction, including through bind mounts.
/// `/proc/self/mountinfo` is the kernel's view of the current mount namespace;
/// parse or lookup failure is returned so destructive callers fail closed.
#[cfg(target_os = "linux")]
pub fn directory_source_ancestry_alias_blocking(first: &Path, second: &Path) -> io::Result<bool> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let first_mount_id = linux_directory_mount_id(first)?;
    let second_mount_id = linux_directory_mount_id(second)?;
    let first = linux_mount_coordinate_from(&mountinfo, first, first_mount_id)?;
    let second = linux_mount_coordinate_from(&mountinfo, second, second_mount_id)?;
    Ok(first.major == second.major
        && first.minor == second.minor
        && (first
            .path_within_filesystem
            .starts_with(&second.path_within_filesystem)
            || second
                .path_within_filesystem
                .starts_with(&first.path_within_filesystem)))
}

/// Non-Linux Unix targets do not expose Linux bind mounts or mountinfo. Their
/// supported path aliases remain covered by canonical paths and inode-parent
/// ancestry checks.
#[cfg(not(target_os = "linux"))]
pub fn directory_source_ancestry_alias_blocking(_first: &Path, _second: &Path) -> io::Result<bool> {
    Ok(false)
}

/// Inspect one regular file through a no-follow descriptor for use as a
/// protected identity during descriptor-relative cleanup.
pub fn regular_file_identity_nofollow_blocking(path: &Path) -> io::Result<FileIdentity> {
    let file = open_read_nofollow_blocking(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("protected path is not a regular file"));
    }
    file_identity(&file)
}

/// Tokio wrapper for [`directory_identity_nofollow_blocking`].
pub async fn directory_identity_nofollow(path: &Path) -> io::Result<FileIdentity> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || directory_identity_nofollow_blocking(&path))
        .await
        .map_err(io::Error::other)?
}

/// Return the final path component without lossy UTF-8 conversion.
pub fn final_name(path: &Path) -> io::Result<OsString> {
    let component = components(path)?
        .pop()
        .ok_or_else(|| invalid_path("filesystem path has no filename"))?;
    match component {
        Component::Normal(name) => Ok(OsString::from_vec(name.as_bytes().to_vec())),
        _ => Err(invalid_path("filesystem path has an unsafe filename")),
    }
}

/// Atomically publish one bounded child file relative to a held, no-follow
/// directory capability.
pub async fn atomic_write_child(
    directory: &Path,
    destination: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let directory = directory.to_owned();
    let destination = destination.to_owned();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let destination = child_name(&destination)?;
        let temporary_name = format!(
            ".{}.{}.part",
            destination.to_string_lossy(),
            uuid::Uuid::new_v4().simple()
        );
        let temporary = child_name(&temporary_name)?;
        // SAFETY: `temporary` is a valid C string, the directory descriptor
        // is held, and the creation mode is supplied for `O_CREAT`.
        let raw_fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a fresh descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let mut file = File::from(fd);
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            // SAFETY: both names are valid C strings and both directory
            // descriptors remain open for the duration of the rename.
            let renamed = unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    temporary.as_ptr(),
                    directory.as_raw_fd(),
                    destination.as_ptr(),
                )
            };
            if renamed != 0 {
                return Err(io::Error::last_os_error());
            }
            directory.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: the directory descriptor and temporary C string are
            // valid. Failure is best-effort cleanup of an unpublished child.
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
            }
        }
        result
    })
    .await
    .map_err(io::Error::other)?
}

/// Create an unlinked read/write regular file inside a securely opened
/// directory. Once the name is removed, only the returned descriptor can
/// mutate the snapshot; this is used to stream exactly the bytes that were
/// authenticated rather than a live inode another writer may edit in place.
fn anonymous_file_in_directory_blocking(directory: &Path) -> io::Result<File> {
    let directory = open_directory_nofollow_blocking(directory)?;
    let name = child_name(&format!(
        ".plurx-snapshot-{}",
        uuid::Uuid::new_v4().simple()
    ))?;
    let raw_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(fd))
}

pub async fn anonymous_file_in_directory(directory: &Path) -> io::Result<tokio::fs::File> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        anonymous_file_in_directory_blocking(&directory).map(tokio::fs::File::from_std)
    })
    .await
    .map_err(io::Error::other)?
}

/// Create a pathname-free memory-backed file for authenticated response
/// snapshots. Linux uses a sealable memfd. macOS POSIX shared-memory objects
/// do not support the file read/write/seek contract this caller needs, so it
/// uses a securely created, immediately unlinked file in the canonical system
/// temp directory instead of consuming media-cache space.
pub fn anonymous_memory_file() -> io::Result<tokio::fs::File> {
    #[cfg(target_os = "linux")]
    let raw_fd = {
        let name = CString::new("plurx-authenticated-object").expect("static memfd name");
        unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            ) as i32
        }
    };
    #[cfg(target_os = "macos")]
    {
        let directory = std::fs::canonicalize(std::env::temp_dir())?;
        anonymous_file_in_directory_blocking(&directory).map(tokio::fs::File::from_std)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let raw_fd = -1;

    #[cfg(not(target_os = "macos"))]
    {
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        Ok(tokio::fs::File::from_std(File::from(fd)))
    }
}

/// Seal a completed Linux memfd against every later write/resize. Other
/// platforms already expose only an unlinked private descriptor; plurx
/// retains only the read path after this call.
pub fn seal_anonymous_memory_file(file: &tokio::fs::File) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let _ = file;
    Ok(())
}

/// Rename a bare child within one securely opened directory.
pub async fn rename_child(directory: &Path, from: &str, to: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let from = from.to_owned();
    let to = to.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let from = child_name(&from)?;
        let to = child_name(&to)?;
        // SAFETY: names are valid C strings and the directory descriptor is
        // held across the operation.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// Rename one child between two securely opened directory capabilities.
pub async fn rename_child_between(
    from_directory: &Path,
    from: &str,
    to_directory: &Path,
    to: &str,
) -> io::Result<()> {
    let from_directory = from_directory.to_owned();
    let from = from.to_owned();
    let to_directory = to_directory.to_owned();
    let to = to.to_owned();
    tokio::task::spawn_blocking(move || {
        let from_directory = open_directory_nofollow_blocking(&from_directory)?;
        let to_directory = open_directory_nofollow_blocking(&to_directory)?;
        let from = child_name(&from)?;
        let to = child_name(&to)?;
        if unsafe {
            libc::renameat(
                from_directory.as_raw_fd(),
                from.as_ptr(),
                to_directory.as_raw_fd(),
                to.as_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        to_directory.sync_all()?;
        from_directory.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}

/// Rename one child only when the destination is absent. This is used to put a
/// failed or mismatched directory quarantine back without overwriting a newer
/// producer. `Ok(false)` means the newer destination won and was preserved.
pub async fn rename_child_noreplace(directory: &Path, from: &str, to: &str) -> io::Result<bool> {
    let directory = directory.to_owned();
    let from = from.to_owned();
    let to = to.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let from = child_name(&from)?;
        let to = child_name(&to)?;
        #[cfg(target_os = "macos")]
        let renamed = unsafe {
            // SAFETY: both names and the held directory descriptor remain
            // valid across the exclusive rename.
            libc::renameatx_np(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let renamed = unsafe {
            // SAFETY: renameat2 receives valid descriptors and C strings. The
            // no-replace flag makes destination races fail closed.
            libc::syscall(
                libc::SYS_renameat2,
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            ) as i32
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let renamed = -1;

        if renamed == 0 {
            directory.sync_all()?;
            return Ok(true);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive directory rename is unsupported on this platform",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(false)
            } else {
                Err(error)
            }
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// Remove a bare non-directory child from one securely opened directory.
pub async fn unlink_child(directory: &Path, name: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let name = child_name(&name)?;
        // SAFETY: the name is a valid C string and the directory descriptor
        // remains open across `unlinkat`.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// Create one bare directory child relative to a held no-follow parent.
pub async fn create_directory_child(directory: &Path, name: &str) -> io::Result<()> {
    let directory = directory.to_owned();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let name = child_name(&name)?;
        // SAFETY: the name is a valid C string and the parent capability is
        // held for the complete operation.
        if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        directory.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}

/// Remove one already-quarantined flat cache directory through held
/// descriptors. Generation/staging layouts are flat by contract; encountering
/// a nested directory or exceeding the explicit entry ceiling fails closed
/// and leaves the quarantine for operator inspection.
pub async fn remove_flat_directory_child(
    parent: &Path,
    name: &str,
    max_entries: usize,
) -> io::Result<()> {
    if max_entries == 0 || max_entries > 120_100 {
        return Err(invalid_path("invalid flat-directory removal bound"));
    }
    let parent = parent.to_owned();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        let parent = open_directory_nofollow_blocking(&parent)?;
        let name = child_name(&name)?;
        let directory = File::from(openat_owned(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )?);
        // Count and validate the complete flat shape before unlinking any
        // child, so an overflow or nested-directory refusal is non-destructive.
        let mut counted = 0usize;
        count_directory_capability(&directory, &mut counted, max_entries, 0, 0)?;
        let entries = independent_directory_stream(&directory)?;
        let result = (|| {
            let mut removed = 0usize;
            loop {
                // SAFETY: entries remains a live DIR pointer until closed.
                let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                    break;
                };
                // SAFETY: a returned dirent has a NUL-terminated name.
                let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                if child.to_bytes() == b"." || child.to_bytes() == b".." {
                    continue;
                }
                removed = removed.saturating_add(1);
                if removed > max_entries {
                    return Err(io::Error::other(
                        "flat cache directory exceeded its removal bound",
                    ));
                }
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: valid descriptor/name and writable stat storage.
                if unsafe {
                    libc::fstatat(
                        directory.as_raw_fd(),
                        child.as_ptr(),
                        stat.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::NotFound {
                        continue;
                    }
                    return Err(error);
                }
                // SAFETY: fstatat succeeded.
                let stat = unsafe { stat.assume_init() };
                if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                    return Err(io::Error::other(
                        "flat cache directory contains a nested directory",
                    ));
                }
                // SAFETY: only the inspected non-directory entry is unlinked.
                if unsafe { libc::unlinkat(directory.as_raw_fd(), child.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            // SAFETY: parent/name remain valid; contents were exhausted.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            parent.sync_all()
        })();
        // SAFETY: entries is a live DIR pointer and owns the duplicate fd.
        unsafe { libc::closedir(entries) };
        result
    })
    .await
    .map_err(io::Error::other)?
}

/// Remove an already-quarantined cache tree through held descriptors, with
/// explicit entry and nesting ceilings. Resumable staging contains numbered
/// part directories (unlike flat final generations), so cleanup needs bounded
/// recursion without ever returning to pathname-based `remove_dir_all`.
pub async fn remove_bounded_directory_tree_child(
    parent: &Path,
    name: &str,
    max_entries: usize,
    max_depth: usize,
) -> io::Result<()> {
    if max_entries == 0 || max_entries > 120_100 || max_depth == 0 || max_depth > 4 {
        return Err(invalid_path("invalid cache-tree removal bound"));
    }
    let parent = parent.to_owned();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        fn clear(
            directory: &File,
            removed: &mut usize,
            max_entries: usize,
            depth: usize,
            max_depth: usize,
        ) -> io::Result<()> {
            let entries = independent_directory_stream(directory)?;
            let result = (|| {
                loop {
                    // SAFETY: entries remains live until closed below.
                    let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                        break;
                    };
                    // SAFETY: dirent names are NUL terminated.
                    let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                    if child.to_bytes() == b"." || child.to_bytes() == b".." {
                        continue;
                    }
                    *removed = removed.saturating_add(1);
                    if *removed > max_entries {
                        return Err(io::Error::other(
                            "cache tree exceeded its removal entry bound",
                        ));
                    }
                    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                    if unsafe {
                        libc::fstatat(
                            directory.as_raw_fd(),
                            child.as_ptr(),
                            stat.as_mut_ptr(),
                            libc::AT_SYMLINK_NOFOLLOW,
                        )
                    } != 0
                    {
                        let error = io::Error::last_os_error();
                        if error.kind() == io::ErrorKind::NotFound {
                            continue;
                        }
                        return Err(error);
                    }
                    let stat = unsafe { stat.assume_init() };
                    if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                        if depth >= max_depth {
                            return Err(io::Error::other(
                                "cache tree exceeded its removal depth bound",
                            ));
                        }
                        let child_name = CString::new(child.to_bytes())
                            .map_err(|_| invalid_path("cache child contains NUL"))?;
                        let child_directory = File::from(openat_owned(
                            directory.as_raw_fd(),
                            &child_name,
                            libc::O_RDONLY
                                | libc::O_DIRECTORY
                                | libc::O_NOFOLLOW
                                | libc::O_CLOEXEC
                                | libc::O_NONBLOCK,
                        )?);
                        clear(&child_directory, removed, max_entries, depth + 1, max_depth)?;
                        if unsafe {
                            libc::unlinkat(
                                directory.as_raw_fd(),
                                child.as_ptr(),
                                libc::AT_REMOVEDIR,
                            )
                        } != 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                    } else if unsafe { libc::unlinkat(directory.as_raw_fd(), child.as_ptr(), 0) }
                        != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            })();
            unsafe { libc::closedir(entries) };
            result
        }

        let parent = open_directory_nofollow_blocking(&parent)?;
        let name = child_name(&name)?;
        let directory = File::from(openat_owned(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )?);
        // Validate the complete quarantined shape before unlinking its first
        // entry. An exceeded ceiling therefore preserves the original tree
        // intact for restoration/operator inspection rather than returning a
        // partially deleted directory.
        let mut counted = 0usize;
        count_directory_capability(&directory, &mut counted, max_entries, 0, max_depth)?;
        let mut removed = 0usize;
        clear(&directory, &mut removed, max_entries, 0, max_depth)?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        parent.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}

/// Restore a quarantined regular file only if no writer has recreated the
/// destination. A hard link provides portable no-replace semantics; the
/// quarantine name is then removed. `Ok(false)` means a newer destination won
/// the race and was preserved.
pub async fn restore_child_noreplace(
    directory: &Path,
    quarantine: &str,
    destination: &str,
) -> io::Result<bool> {
    let directory = directory.to_owned();
    let quarantine = quarantine.to_owned();
    let destination = destination.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
        let quarantine = child_name(&quarantine)?;
        let destination = child_name(&destination)?;
        // SAFETY: names are valid C strings and both operations stay relative
        // to the held directory capability. No AT_SYMLINK_FOLLOW is used.
        let linked = unsafe {
            libc::linkat(
                directory.as_raw_fd(),
                quarantine.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                0,
            )
        };
        if linked == 0 {
            // SAFETY: the quarantine name and descriptor remain valid.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), quarantine.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            directory.sync_all()?;
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::AlreadyExists {
            // A newer writer won. Remove only the quarantined old inode.
            // SAFETY: the quarantine name and descriptor remain valid.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), quarantine.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            directory.sync_all()?;
            Ok(false)
        } else {
            Err(error)
        }
    })
    .await
    .map_err(io::Error::other)?
}

const SCRATCH_CLEANUP_MAX_ENTRIES: usize = 120_100;
const SCRATCH_CLEANUP_MAX_DEPTH: usize = 16;

/// What a well-formed (regular, daemon-owned, 0600) ownership marker holds.
///
/// The three non-matching shapes are deliberately distinct, because they call
/// for three different operator actions and the single "contents do not match"
/// refusal collapsed all of them into one dead end.
enum ScratchMarkerState {
    /// The payload is exactly the expected marker for this durable root.
    Match(FileIdentity),
    /// Empty, or a strict prefix of the expected payload: a claim interrupted
    /// between `openat(O_CREAT|O_EXCL)` and `write_all`.
    Interrupted,
    /// A well-formed marker of this version that names a different durable
    /// root — the operator moved `storage.data_dir` and kept the scratch.
    OtherOwner(String),
    /// Anything else: not a marker this daemon wrote.
    Mismatch,
}

/// The `owner=` line a marker payload carries, if it is shaped like one.
fn scratch_marker_owner(payload: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(payload).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("owner="))
        .map(str::to_owned)
}

fn scratch_marker_state(
    directory: &File,
    marker: &CString,
    expected: &[u8],
) -> io::Result<ScratchMarkerState> {
    use std::io::Read;

    let mut file = File::from(openat_owned(
        directory.as_raw_fd(),
        marker,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(io::Error::other(
            "scratch ownership marker must be a daemon-owned regular file with mode 0600",
        ));
    }
    let before = file_identity(&file)?;
    let mut actual = Vec::with_capacity(expected.len().saturating_add(1));
    std::io::Read::by_ref(&mut file)
        .take(expected.len().saturating_add(1) as u64)
        .read_to_end(&mut actual)?;
    if file_identity(&file)? != before {
        return Ok(ScratchMarkerState::Mismatch);
    }
    if actual == expected {
        return Ok(ScratchMarkerState::Match(before));
    }
    // A torn write is a strict prefix of what this boot would have written,
    // including the zero-length case a crash between create and write leaves.
    if expected.starts_with(actual.as_slice()) {
        return Ok(ScratchMarkerState::Interrupted);
    }
    match (
        scratch_marker_owner(&actual),
        scratch_marker_owner(expected),
    ) {
        (Some(found), Some(wanted)) if found != wanted => {
            Ok(ScratchMarkerState::OtherOwner(found))
        }
        _ => Ok(ScratchMarkerState::Mismatch),
    }
}

/// Where a marker lives, for an error an operator can act on.
fn marker_path(root: &Path, marker: &CString) -> PathBuf {
    root.join(std::ffi::OsStr::from_bytes(marker.to_bytes()))
}

/// Turn a non-matching marker into an error that names the file and the move
/// that produces it.
fn scratch_marker_error(root: &Path, marker: &CString, state: ScratchMarkerState) -> io::Error {
    match state {
        ScratchMarkerState::Match(_) => {
            unreachable!("a matching marker is not an error")
        }
        ScratchMarkerState::Interrupted => io::Error::other(format!(
            "scratch ownership marker {} is empty or truncated; \
             remove it and restart the daemon to redo the claim",
            marker_path(root, marker).display()
        )),
        ScratchMarkerState::OtherOwner(owner) => io::Error::other(format!(
            "transcode scratch {} is claimed by another durable root: its ownership \
             marker {} names owner {owner}. Point storage.data_dir back at that root, \
             or — if that root is gone — empty this scratch directory, including the \
             marker, before pointing a different data_dir at it",
            root.display(),
            marker_path(root, marker).display()
        )),
        ScratchMarkerState::Mismatch => io::Error::other(format!(
            "scratch ownership marker {} contents or identity do not match",
            marker_path(root, marker).display()
        )),
    }
}

fn scratch_marker_identity(
    root: &Path,
    directory: &File,
    marker: &CString,
    expected: &[u8],
) -> io::Result<FileIdentity> {
    match scratch_marker_state(directory, marker, expected)? {
        ScratchMarkerState::Match(identity) => Ok(identity),
        state => Err(scratch_marker_error(root, marker, state)),
    }
}

fn scratch_child_identity(directory: &File, name: &CString) -> io::Result<FileIdentity> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(FileIdentity {
        device: stat_device(&stat),
        inode: stat.st_ino,
        size: stat.st_size.max(0) as u64,
        changed_seconds: stat.st_ctime,
        changed_nanoseconds: stat.st_ctime_nsec,
    })
}

/// Classify one entry inside an already-proven scratch root: `true` for a
/// directory to descend into, `false` for a regular file to unlink.
///
/// Ownership and permission bits are deliberately **not** consulted here.
/// The root is proven to be owned by the daemon euid and repaired to 0700
/// before any traversal starts, so no other uid can introduce an entry
/// underneath it. A permissive-mode or foreign-uid regular file inside
/// declared-disposable scratch is therefore the daemon's own leftover — a
/// umask change, a PUID/PGID change, a restored backup — not an attack
/// signal, and refusing it turned a disposable directory into a permanent
/// boot failure. What actually prevents deletion escaping the tree is the
/// descriptor-relative `O_NOFOLLOW` traversal, not the mode bits.
///
/// File type still fails closed, because a symlink or a device node is a way
/// *out* of the tree rather than a datum inside it.
fn validate_scratch_entry(stat: &libc::stat) -> io::Result<bool> {
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Ok(true),
        libc::S_IFREG => Ok(false),
        libc::S_IFLNK => Err(io::Error::other("scratch tree contains a symbolic link")),
        _ => Err(io::Error::other(
            "scratch tree contains a device or other special file",
        )),
    }
}

fn scratch_has_only_preserved(directory: &File, preserved: &[&CStr]) -> io::Result<bool> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| loop {
        let Some(entry) = (unsafe { readdir_checked(entries) })? else {
            return Ok(true);
        };
        let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if child.to_bytes() == b"."
            || child.to_bytes() == b".."
            || preserved
                .iter()
                .any(|name| child.to_bytes() == name.to_bytes())
        {
            continue;
        }
        return Ok(false);
    })();
    unsafe { libc::closedir(entries) };
    result
}

/// Which cleanup ceiling an oversized scratch tree hit.
#[derive(Clone, Copy)]
enum ScratchBound {
    Entries,
    Depth,
}

impl ScratchBound {
    fn describe(self) -> String {
        match self {
            Self::Entries => format!("entry ceiling of {SCRATCH_CLEANUP_MAX_ENTRIES} entries"),
            Self::Depth => format!("depth ceiling of {SCRATCH_CLEANUP_MAX_DEPTH} levels"),
        }
    }
}

/// Preflight the tree the boot clear is about to remove.
///
/// `Ok(None)` means the tree is within both ceilings and may be cleared.
/// `Ok(Some(bound))` means it is not, which is a reason to leave a root the
/// daemon owns alone for this boot rather than an error: the ceilings exist to
/// bound a walk into an *unknown* directory, and an oversized scratch is
/// exactly the ungraceful-crash case the boot clear was built to survive.
fn count_scratch_capability(
    directory: &File,
    marker: Option<&CStr>,
    counted: &mut usize,
    depth: usize,
    root_device: u64,
    protected: &[FileIdentity],
) -> io::Result<Option<ScratchBound>> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| {
        loop {
            let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                break;
            };
            let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if child.to_bytes() == b"."
                || child.to_bytes() == b".."
                || (depth == 0 && marker.is_some_and(|name| name.to_bytes() == child.to_bytes()))
            {
                continue;
            }
            *counted = counted.saturating_add(1);
            if *counted > SCRATCH_CLEANUP_MAX_ENTRIES {
                return Ok(Some(ScratchBound::Entries));
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    child.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            if stat_device(&stat) != root_device {
                return Err(io::Error::other(
                    "scratch tree crosses a filesystem mount boundary",
                ));
            }
            let child_identity = FileIdentity {
                device: stat_device(&stat),
                inode: stat.st_ino,
                size: stat.st_size.max(0) as u64,
                changed_seconds: stat.st_ctime,
                changed_nanoseconds: stat.st_ctime_nsec,
            };
            if protected
                .iter()
                .any(|identity| identity.same_inode(child_identity))
            {
                return Err(io::Error::other(
                    "scratch tree overlaps a protected storage identity",
                ));
            }
            if validate_scratch_entry(&stat)? {
                if depth >= SCRATCH_CLEANUP_MAX_DEPTH {
                    return Ok(Some(ScratchBound::Depth));
                }
                let name = CString::new(child.to_bytes())
                    .map_err(|_| invalid_path("scratch child contains NUL"))?;
                let child_directory = File::from(openat_owned(
                    directory.as_raw_fd(),
                    &name,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )?);
                if !file_identity(&child_directory)?.same_inode(child_identity) {
                    return Err(io::Error::other(
                        "scratch directory child changed during cleanup preflight",
                    ));
                }
                if let Some(bound) = count_scratch_capability(
                    &child_directory,
                    None,
                    counted,
                    depth + 1,
                    root_device,
                    protected,
                )? {
                    return Ok(Some(bound));
                }
            }
        }
        Ok(None)
    })();
    unsafe { libc::closedir(entries) };
    result
}

fn clear_scratch_capability(
    directory: &File,
    marker: Option<&CStr>,
    depth: usize,
    removed: &mut usize,
    root_device: u64,
    protected: &[FileIdentity],
) -> io::Result<()> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| {
        loop {
            let Some(entry) = (unsafe { readdir_checked(entries) })? else {
                break;
            };
            let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if child.to_bytes() == b"."
                || child.to_bytes() == b".."
                || (depth == 0 && marker.is_some_and(|name| name.to_bytes() == child.to_bytes()))
            {
                continue;
            }
            *removed = removed.saturating_add(1);
            if *removed > SCRATCH_CLEANUP_MAX_ENTRIES {
                return Err(io::Error::other(
                    "scratch tree exceeded its cleanup entry bound",
                ));
            }
            let name = CString::new(child.to_bytes())
                .map_err(|_| invalid_path("scratch child contains NUL"))?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(error);
            }
            let stat = unsafe { stat.assume_init() };
            if stat_device(&stat) != root_device {
                return Err(io::Error::other(
                    "scratch tree crosses a filesystem mount boundary",
                ));
            }
            let observed = FileIdentity {
                device: stat_device(&stat),
                inode: stat.st_ino,
                size: stat.st_size.max(0) as u64,
                changed_seconds: stat.st_ctime,
                changed_nanoseconds: stat.st_ctime_nsec,
            };
            if protected
                .iter()
                .any(|identity| identity.same_inode(observed))
            {
                return Err(io::Error::other(
                    "scratch tree overlaps a protected storage identity",
                ));
            }
            let is_directory = validate_scratch_entry(&stat)?;
            let quarantine = CString::new(format!(
                ".plurx-scratch-delete-{}",
                uuid::Uuid::new_v4().simple()
            ))
            .expect("UUID scratch quarantine contains no NUL");
            if unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    directory.as_raw_fd(),
                    quarantine.as_ptr(),
                )
            } != 0
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                // `EBUSY` from `renameat` on a directory means the kernel is
                // refusing to move a mount point. A bare "resource busy" sends
                // the operator looking for a lock; naming the child and the
                // cause points at the actual fix.
                if error.raw_os_error() == Some(libc::EBUSY) {
                    return Err(io::Error::other(format!(
                        "scratch tree contains a mount point at {}; unmount it or move it \
                         outside the scratch root, which is cleared at every boot",
                        String::from_utf8_lossy(name.to_bytes())
                    )));
                }
                return Err(error);
            }
            let quarantined = scratch_child_identity(directory, &quarantine)?;
            if !quarantined.same_inode(observed) {
                return Err(io::Error::other(
                    "scratch child changed before quarantine; replacement was not deleted",
                ));
            }
            if is_directory {
                if depth >= SCRATCH_CLEANUP_MAX_DEPTH {
                    return Err(io::Error::other(
                        "scratch tree exceeded its cleanup depth bound",
                    ));
                }
                let child_directory = File::from(openat_owned(
                    directory.as_raw_fd(),
                    &quarantine,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )?);
                if !file_identity(&child_directory)?.same_inode(quarantined) {
                    return Err(io::Error::other(
                        "scratch directory changed after quarantine",
                    ));
                }
                clear_scratch_capability(
                    &child_directory,
                    None,
                    depth + 1,
                    removed,
                    root_device,
                    protected,
                )?;
                if !scratch_child_identity(directory, &quarantine)?.same_inode(quarantined) {
                    return Err(io::Error::other(
                        "scratch directory quarantine changed before removal",
                    ));
                }
                if unsafe {
                    libc::unlinkat(
                        directory.as_raw_fd(),
                        quarantine.as_ptr(),
                        libc::AT_REMOVEDIR,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
            } else if unsafe { libc::unlinkat(directory.as_raw_fd(), quarantine.as_ptr(), 0) } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        directory.sync_all()
    })();
    unsafe { libc::closedir(entries) };
    result
}

#[derive(Clone, Copy)]
enum ScratchHookPhase {
    AfterPendingCreate,
    AfterMarkerCreate,
    BeforeCleanup,
    AfterCleanup,
}

/// Create the pending ownership marker in an otherwise-empty scratch root and
/// return its verified identity.
fn create_pending_scratch_marker(
    path: &Path,
    directory: &File,
    pending: &CString,
    expected_marker: &[u8],
    hook: Option<&dyn Fn(ScratchHookPhase)>,
) -> io::Result<FileIdentity> {
    // A durable pending marker is deliberately not an ownership grant. Crash
    // recovery may promote it only while it remains the directory's sole
    // entry.
    if !scratch_has_only_preserved(directory, &[])? {
        return Err(io::Error::other(
            "explicit scratch root was populated before ownership could be claimed",
        ));
    }
    let raw = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            pending.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    file.write_all(expected_marker)?;
    file.sync_all()?;
    directory.sync_all()?;
    if let Some(hook) = hook {
        hook(ScratchHookPhase::AfterPendingCreate);
    }
    scratch_marker_identity(path, directory, pending, expected_marker)
}

fn claim_and_clear_scratch_inner(
    path: &Path,
    marker_name: Option<&str>,
    expected_marker: &[u8],
    protected: &[FileIdentity],
    hook: Option<&dyn Fn(ScratchHookPhase)>,
) -> io::Result<()> {
    let directory = open_directory_nofollow_blocking(path)?;
    let root_identity = file_identity(&directory)?;
    if protected
        .iter()
        .any(|identity| identity.same_inode(root_identity))
    {
        return Err(io::Error::other(
            "scratch root aliases a protected storage identity",
        ));
    }
    let metadata = directory.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(
            "scratch root must be owned by the daemon uid",
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        // The daemon creates this directory itself, under whatever umask the
        // host gave the process: `umask 002` — Ubuntu's user-private-group
        // default — makes it group-writable at creation, so refusing here
        // made the legacy compatibility path unbootable on an ordinary host.
        // A root the daemon already owns is ours to tighten. `fchmod` on the
        // descriptor already proven to be that inode, never on the pathname,
        // so a concurrent rename cannot redirect the mode change.
        if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        tracing::warn!(
            scratch = %path.display(),
            mode = format!("{:04o}", metadata.mode() & 0o7777),
            "transcode scratch root was group- or world-writable; repaired it to 0700"
        );
    }
    let marker = marker_name.map(child_name).transpose()?;
    let marker_identity = if let Some(marker) = marker.as_ref() {
        let pending = child_name(&format!(
            "{}.claiming",
            marker_name.expect("marker CString came from marker name")
        ))?;
        match scratch_marker_identity(path, &directory, marker, expected_marker) {
            Ok(identity) => match scratch_marker_identity(path, &directory, &pending, expected_marker)
            {
                Ok(pending_identity) => {
                    if !pending_identity.same_inode(identity)
                        || !scratch_has_only_preserved(
                            &directory,
                            &[pending.as_c_str(), marker.as_c_str()],
                        )?
                    {
                        return Err(io::Error::other(
                            "scratch ownership claim is incomplete; refusing destructive cleanup",
                        ));
                    }
                    if unsafe { libc::unlinkat(directory.as_raw_fd(), pending.as_ptr(), 0) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    directory.sync_all()?;
                    identity
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => identity,
                Err(error) => return Err(error),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let pending_identity = match scratch_marker_state(
                    &directory,
                    &pending,
                    expected_marker,
                ) {
                    Ok(ScratchMarkerState::Match(identity)) => identity,
                    Ok(ScratchMarkerState::Interrupted) => {
                        // A crash between `openat(O_CREAT|O_EXCL)` and
                        // `write_all` leaves a zero-length — or partly
                        // written — pending marker. That is an interrupted
                        // claim, not corruption: only this daemon can have
                        // created it (the file is regular, daemon-owned and
                        // 0600, checked above), and the published marker is
                        // absent, so nothing ever trusted it. Treating it as
                        // permanent corruption bricked the scratch root on
                        // every later boot. Remove it and redo the claim,
                        // which still requires the root to be otherwise
                        // empty.
                        tracing::warn!(
                            scratch = %path.display(),
                            marker = %marker_path(path, &pending).display(),
                            "found an interrupted transcode scratch ownership claim; redoing it"
                        );
                        if unsafe { libc::unlinkat(directory.as_raw_fd(), pending.as_ptr(), 0) } != 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                        directory.sync_all()?;
                        create_pending_scratch_marker(
                            path,
                            &directory,
                            &pending,
                            expected_marker,
                            hook,
                        )?
                    }
                    Ok(state) => return Err(scratch_marker_error(path, &pending, state)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        create_pending_scratch_marker(
                            path,
                            &directory,
                            &pending,
                            expected_marker,
                            hook,
                        )?
                    }
                    Err(error) => return Err(error),
                };
                if !scratch_has_only_preserved(&directory, &[pending.as_c_str()])? {
                    return Err(io::Error::other(
                        "explicit scratch root was populated before ownership could be claimed",
                    ));
                }
                if !scratch_marker_identity(path, &directory, &pending, expected_marker)?
                    .same_inode(pending_identity)
                {
                    return Err(io::Error::other(
                        "scratch pending ownership marker changed before publication",
                    ));
                }
                if unsafe {
                    libc::linkat(
                        directory.as_raw_fd(),
                        pending.as_ptr(),
                        directory.as_raw_fd(),
                        marker.as_ptr(),
                        0,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                directory.sync_all()?;
                if let Some(hook) = hook {
                    hook(ScratchHookPhase::AfterMarkerCreate);
                }
                if !scratch_has_only_preserved(
                    &directory,
                    &[pending.as_c_str(), marker.as_c_str()],
                )? {
                    return Err(io::Error::other(
                        "explicit scratch root was populated before ownership could be finalized",
                    ));
                }
                if unsafe { libc::unlinkat(directory.as_raw_fd(), pending.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                directory.sync_all()?;
                scratch_marker_identity(path, &directory, marker, expected_marker)?
            }
            Err(error) => return Err(error),
        }
    } else {
        root_identity
    };

    let current = open_directory_nofollow_blocking(path)?;
    if !file_identity(&current)?.same_inode(root_identity) {
        return Err(io::Error::other(
            "scratch root changed before cleanup; refusing path-based replacement",
        ));
    }
    if let Some(marker) = marker.as_ref() {
        let current_marker = scratch_marker_identity(path, &directory, marker, expected_marker)?;
        if !current_marker.same_inode(marker_identity) {
            return Err(io::Error::other(
                "scratch ownership marker changed before cleanup",
            ));
        }
    }
    if let Some(hook) = hook {
        hook(ScratchHookPhase::BeforeCleanup);
    }
    let mut counted = 0usize;
    // Reaching here means the root is one the daemon owns: the euid check at
    // the top passed, and an explicit root additionally carries a published
    // marker naming this durable root. The ceilings exist to bound a walk into
    // an *unknown* directory, so on a root we own an oversized tree is a
    // reason to leave the scratch alone for this boot rather than to refuse
    // the boot — it is exactly the ungraceful-crash shape the clear exists to
    // survive, and a permanent startup failure is the worse outcome. The
    // survivor checks below are skipped with it, because nothing was touched.
    if let Some(bound) = count_scratch_capability(
        &directory,
        marker.as_deref(),
        &mut counted,
        0,
        root_identity.device,
        protected,
    )? {
        tracing::warn!(
            scratch = %path.display(),
            bound = %bound.describe(),
            "transcode scratch exceeds its cleanup bound; leaving it untouched this boot. \
             Stop the daemon and empty this directory to reclaim the space"
        );
        return Ok(());
    }
    let mut removed = 0usize;
    clear_scratch_capability(
        &directory,
        marker.as_deref(),
        0,
        &mut removed,
        root_identity.device,
        protected,
    )?;
    if let Some(hook) = hook {
        hook(ScratchHookPhase::AfterCleanup);
    }
    let only_preserved = match marker.as_deref() {
        Some(marker) => scratch_has_only_preserved(&directory, &[marker])?,
        None => scratch_has_only_preserved(&directory, &[])?,
    };
    if !only_preserved {
        return Err(io::Error::other(
            "scratch directory received new entries during cleanup",
        ));
    }
    if let Some(marker) = marker.as_ref() {
        let current_marker = scratch_marker_identity(path, &directory, marker, expected_marker)?;
        if !current_marker.same_inode(marker_identity) {
            return Err(io::Error::other(
                "scratch ownership marker changed during cleanup",
            ));
        }
    }
    let current = open_directory_nofollow_blocking(path)?;
    if !file_identity(&current)?.same_inode(root_identity) {
        return Err(io::Error::other(
            "scratch root changed during cleanup; replacement was not touched",
        ));
    }
    Ok(())
}

/// Claim an empty explicit scratch directory and clear only the held,
/// marker-bound inode on later boots. Every traversal stays relative to one
/// no-follow descriptor, so path or marker replacement cannot redirect
/// deletion into an unowned tree.
pub fn claim_and_clear_owned_scratch_blocking(
    path: &Path,
    marker_name: &str,
    expected_marker: &[u8],
) -> io::Result<()> {
    claim_and_clear_owned_scratch_with_protected_blocking(path, marker_name, expected_marker, &[])
}

/// Marker-bound scratch cleanup with additional authoritative/persistent
/// inode guards. Protected identities are rejected at the root and anywhere
/// below it, including through bind-mount aliases invisible to lexical paths.
pub fn claim_and_clear_owned_scratch_with_protected_blocking(
    path: &Path,
    marker_name: &str,
    expected_marker: &[u8],
    protected: &[FileIdentity],
) -> io::Result<()> {
    if expected_marker.is_empty() || expected_marker.len() > 4_096 {
        return Err(invalid_path("invalid scratch ownership marker"));
    }
    claim_and_clear_scratch_inner(path, Some(marker_name), expected_marker, protected, None)
}

/// Clear a legacy scratch directory through a retained no-follow descriptor.
/// The caller still owns compatibility policy for whether that path may be a
/// symlink; this helper ensures a later rename cannot redirect cleanup.
pub fn clear_scratch_blocking(path: &Path) -> io::Result<()> {
    clear_scratch_with_protected_blocking(path, &[])
}

/// Legacy scratch cleanup with additional authoritative/persistent inode
/// guards for bind-alias safety.
pub fn clear_scratch_with_protected_blocking(
    path: &Path,
    protected: &[FileIdentity],
) -> io::Result<()> {
    claim_and_clear_scratch_inner(path, None, &[], protected, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mount_coordinates_expose_bind_source_ancestry() {
        let mountinfo = "1 0 8:1 / / rw - ext4 /dev/root rw\n\
                         2 1 8:1 /safe-lower /scratch rw - ext4 /dev/root rw\n\
                         3 1 8:1 /durable/cache /scratch rw - ext4 /dev/root rw\n";
        let durable = linux_mount_coordinate_from(mountinfo, Path::new("/durable"), 1)
            .expect("durable coordinate");
        let hidden_lower = linux_mount_coordinate_from(mountinfo, Path::new("/scratch"), 2)
            .expect("hidden lower bind coordinate");
        let scratch = linux_mount_coordinate_from(mountinfo, Path::new("/scratch"), 3)
            .expect("visible top bind coordinate");
        assert_eq!(
            hidden_lower.path_within_filesystem,
            Path::new("/safe-lower")
        );
        assert_eq!(durable.major, scratch.major);
        assert_eq!(durable.minor, scratch.minor);
        assert!(
            scratch
                .path_within_filesystem
                .starts_with(&durable.path_within_filesystem),
            "the bind target must retain its source position inside the filesystem"
        );
    }

    /// Opt-in real mount-namespace regression for privileged Linux validation:
    /// `PLURX_RUN_BIND_MOUNT_TEST=1 cargo test -p plurx-core real_bind`.
    #[cfg(target_os = "linux")]
    #[test]
    fn real_bind_mount_source_ancestry_is_refused() {
        if std::env::var_os("PLURX_RUN_BIND_MOUNT_TEST").is_none() {
            return;
        }
        struct Mounted(PathBuf, usize);
        impl Drop for Mounted {
            fn drop(&mut self) {
                for _ in 0..self.1 {
                    let _ = std::process::Command::new("umount").arg(&self.0).status();
                }
            }
        }

        let root = tempfile::tempdir().expect("mount test root");
        let durable = root.path().join("durable");
        let source = durable.join("persistent-cache");
        let safe_lower = root.path().join("safe-lower");
        let scratch = root.path().join("scratch-bind");
        std::fs::create_dir_all(&source).expect("bind source");
        std::fs::create_dir(&safe_lower).expect("safe lower bind source");
        std::fs::create_dir(&scratch).expect("bind target");
        let mut mounted = Mounted(scratch.clone(), 0);
        for source in [&safe_lower, &source] {
            let status = std::process::Command::new("mount")
                .arg("--bind")
                .arg(source)
                .arg(&scratch)
                .status()
                .expect("run mount --bind");
            assert!(status.success(), "opt-in bind mount setup failed: {status}");
            mounted.1 += 1;
        }
        let _mounted = mounted;
        let durable = std::fs::canonicalize(durable).expect("canonical durable root");
        let scratch = std::fs::canonicalize(scratch).expect("canonical scratch root");
        assert!(
            directory_source_ancestry_alias_blocking(&scratch, &durable)
                .expect("read mount source ancestry"),
            "a bind of a durable descendant must be rejected as scratch"
        );
    }

    /// A `umask 002` host — Ubuntu's user-private-group default, and what
    /// every container image that sets PUID/PGID inherits — makes the daemon's
    /// own `create_dir_all` produce a group-writable scratch root. Refusing
    /// that made the compatibility path unbootable, so a root the daemon
    /// already owns is tightened rather than rejected.
    #[test]
    fn a_daemon_owned_group_writable_scratch_root_is_repaired_not_refused() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        let mut permissions = std::fs::metadata(&scratch)
            .expect("scratch metadata")
            .permissions();
        permissions.set_mode(0o777);
        std::fs::set_permissions(&scratch, permissions).expect("world-writable scratch");

        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("a daemon-owned world-writable scratch is repaired and claimed");
        assert_eq!(
            std::fs::metadata(&scratch)
                .expect("repaired scratch metadata")
                .mode()
                & 0o777,
            0o700,
            "the repair must leave the root private"
        );
        let marker = std::fs::metadata(scratch.join(".owner")).expect("marker metadata");
        assert_eq!(marker.uid(), unsafe { libc::geteuid() });
        assert_eq!(marker.mode() & 0o777, 0o600);
    }

    /// The same repair applies to the legacy `<data_dir>/transcode` root,
    /// which carries no marker at all.
    #[test]
    fn a_group_writable_legacy_scratch_root_is_repaired_not_refused() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("transcode");
        std::fs::create_dir(&scratch).expect("legacy scratch directory");
        std::fs::write(scratch.join("partial.m4s"), b"discard").expect("stale segment");
        let mut permissions = std::fs::metadata(&scratch)
            .expect("scratch metadata")
            .permissions();
        permissions.set_mode(0o775);
        std::fs::set_permissions(&scratch, permissions).expect("group-writable scratch");

        clear_scratch_blocking(&scratch).expect("legacy scratch is repaired and cleared");
        assert_eq!(
            std::fs::metadata(&scratch)
                .expect("repaired scratch metadata")
                .mode()
                & 0o777,
            0o700
        );
        assert!(!scratch.join("partial.m4s").exists());
    }

    /// Repair is bounded by ownership: a scratch root belonging to another uid
    /// is still a hard refusal, because the daemon has no claim to it.
    #[test]
    fn a_scratch_root_owned_by_another_uid_is_still_refused() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: chowning a directory to another uid needs root");
            return;
        }
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        std::fs::write(scratch.join("stranger-data"), b"must survive").expect("stranger data");
        let name = CString::new(scratch.as_os_str().as_bytes()).expect("scratch path CString");
        assert_eq!(unsafe { libc::chown(name.as_ptr(), 65_534, 65_534) }, 0);

        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("a foreign-owned scratch root must fail closed");
        assert!(error.to_string().contains("owned by the daemon uid"), "{error}");
        assert_eq!(
            std::fs::read(scratch.join("stranger-data")).expect("stranger data survives"),
            b"must survive"
        );
    }

    fn claimed_scratch(root: &Path) -> std::path::PathBuf {
        let scratch = std::fs::canonicalize(root)
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("claim scratch");
        scratch
    }

    #[test]
    fn scratch_cleanup_refuses_symbolic_links_without_touching_their_targets() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let target = root.path().join("outside-target");
        std::fs::write(&target, b"must survive").expect("outside target");
        std::os::unix::fs::symlink(&target, scratch.join("link")).expect("scratch symlink");

        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("a scratch symlink must fail closed");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert!(scratch.join("link").is_symlink());
        assert_eq!(
            std::fs::read(target).expect("target survives"),
            b"must survive"
        );
    }

    /// Once the root is proven daemon-owned and repaired to 0700, no other uid
    /// can put anything inside it, so a group- or world-writable child is this
    /// daemon's own leftover from a umask change — disposable scratch, not an
    /// attack signal. Refusing it made a permissive segment a permanent boot
    /// failure on a directory the daemon declares disposable.
    #[test]
    fn scratch_cleanup_clears_a_permissive_mode_child() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let permissive_child = scratch.join("group-writable-segment");
        std::fs::write(&permissive_child, b"discard").expect("permissive child");
        let mut permissions = std::fs::metadata(&permissive_child)
            .expect("permissive child metadata")
            .permissions();
        permissions.set_mode(0o666);
        std::fs::set_permissions(&permissive_child, permissions).expect("permissive mode");
        let permissive_directory = scratch.join("group-writable-directory");
        std::fs::create_dir(&permissive_directory).expect("permissive directory");
        std::fs::write(permissive_directory.join("nested.m4s"), b"discard").expect("nested child");
        let mut permissions = std::fs::metadata(&permissive_directory)
            .expect("permissive directory metadata")
            .permissions();
        permissions.set_mode(0o777);
        std::fs::set_permissions(&permissive_directory, permissions).expect("permissive mode");

        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("a permissive-mode leftover is cleaned, not refused");
        assert!(!permissive_child.exists());
        assert!(!permissive_directory.exists());
        assert!(scratch.join(".owner").exists());
    }

    /// A PUID/PGID change, or a restored backup, leaves entries owned by the
    /// uid that wrote them. Inside a root this daemon owns they are still its
    /// own leftovers.
    #[test]
    fn scratch_cleanup_clears_a_child_owned_by_another_uid() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: chowning a child to another uid needs root");
            return;
        }
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let foreign = scratch.join("previous-puid-segment");
        std::fs::write(&foreign, b"discard").expect("foreign child");
        let name = CString::new(foreign.as_os_str().as_bytes()).expect("child path CString");
        assert_eq!(unsafe { libc::chown(name.as_ptr(), 65_534, 65_534) }, 0);

        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("a foreign-uid leftover inside an owned root is cleaned");
        assert!(!foreign.exists());
    }

    #[test]
    fn scratch_cleanup_refuses_special_files_without_unlinking_them() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let fifo = scratch.join("unexpected-fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path CString");
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("a scratch special file must fail closed");
        assert!(
            error.to_string().contains("device or other special file"),
            "{error}"
        );
        assert!(std::fs::symlink_metadata(fifo).is_ok(), "FIFO must survive");
    }

    #[test]
    fn scratch_claim_linearizes_before_accepting_directory_contents() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        let intruder = scratch.join("concurrent-unowned-file");
        let hook = |phase| {
            if matches!(phase, ScratchHookPhase::AfterPendingCreate) {
                std::fs::write(&intruder, b"must survive").expect("racing child");
            }
        };

        let error = claim_and_clear_scratch_inner(
            &scratch,
            Some(".owner"),
            b"exact-owner\n",
            &[],
            Some(&hook),
        )
        .expect_err("concurrent pre-claim child must refuse ownership");
        assert!(error.to_string().contains("populated before ownership"));
        assert_eq!(
            std::fs::read(&intruder).expect("intruder survives"),
            b"must survive"
        );
        assert!(!scratch.join(".owner").exists());
        assert!(
            scratch.join(".owner.claiming").exists(),
            "the durable pending state must never authorize cleanup"
        );
    }

    #[test]
    fn interrupted_owner_publication_never_authorizes_raced_content() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        let unrelated = scratch.join("raced-host-data");
        let crash_after_marker = |phase| {
            if matches!(phase, ScratchHookPhase::AfterMarkerCreate) {
                std::fs::write(&unrelated, b"must survive").expect("raced data");
                panic!("simulated process crash after durable marker publication");
            }
        };

        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            claim_and_clear_scratch_inner(
                &scratch,
                Some(".owner"),
                b"exact-owner\n",
                &[],
                Some(&crash_after_marker),
            )
        }));
        assert!(
            attempt.is_err(),
            "the failpoint must simulate death before ownership finalization"
        );
        assert!(scratch.join(".owner").exists());
        assert!(scratch.join(".owner.claiming").exists());

        let restart = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("an interrupted ownership grant must fail closed on restart");
        assert!(restart.to_string().contains("claim is incomplete"));
        assert_eq!(
            std::fs::read(&unrelated).expect("raced data survives restart"),
            b"must survive"
        );
    }

    #[test]
    fn pending_owner_claim_promotes_on_restart_only_while_empty() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        let crash_after_pending = |phase| {
            if matches!(phase, ScratchHookPhase::AfterPendingCreate) {
                panic!("simulated process crash with pending ownership only");
            }
        };

        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            claim_and_clear_scratch_inner(
                &scratch,
                Some(".owner"),
                b"exact-owner\n",
                &[],
                Some(&crash_after_pending),
            )
        }));
        assert!(attempt.is_err());
        assert!(!scratch.join(".owner").exists());
        assert!(scratch.join(".owner.claiming").exists());

        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("an otherwise-empty pending claim may finish on restart");
        assert!(scratch.join(".owner").exists());
        assert!(!scratch.join(".owner.claiming").exists());
    }

    #[test]
    fn scratch_cleanup_holds_the_verified_inode_across_path_replacement() {
        let root = tempfile::tempdir().expect("scratch root");
        let canonical_root = std::fs::canonicalize(root.path()).expect("canonical scratch parent");
        let scratch = canonical_root.join("scratch");
        let moved = canonical_root.join("verified-scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("claim scratch");
        std::fs::write(scratch.join("owned-stale"), b"discard").expect("owned stale child");
        let replacement = scratch.join("unowned-replacement");
        let hook = |phase| {
            if matches!(phase, ScratchHookPhase::BeforeCleanup) {
                std::fs::rename(&scratch, &moved).expect("move verified root");
                std::fs::create_dir(&scratch).expect("replacement root");
                std::fs::write(&replacement, b"must survive").expect("replacement child");
            }
        };

        let error = claim_and_clear_scratch_inner(
            &scratch,
            Some(".owner"),
            b"exact-owner\n",
            &[],
            Some(&hook),
        )
        .expect_err("path replacement must fail startup");
        assert!(error.to_string().contains("replacement was not touched"));
        assert_eq!(
            std::fs::read(&replacement).expect("replacement survives"),
            b"must survive"
        );
        assert!(!moved.join("owned-stale").exists());
        assert!(moved.join(".owner").exists());
    }

    #[test]
    fn scratch_cleanup_refuses_success_when_a_late_child_survives() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("claim scratch");
        std::fs::write(scratch.join("stale"), b"discard").expect("stale child");
        let late = scratch.join("late-child");
        let hook = |phase| {
            if matches!(phase, ScratchHookPhase::AfterCleanup) {
                std::fs::write(&late, b"must force failure").expect("late child");
            }
        };

        let error = claim_and_clear_scratch_inner(
            &scratch,
            Some(".owner"),
            b"exact-owner\n",
            &[],
            Some(&hook),
        )
        .expect_err("surviving late child must fail startup");
        assert!(error.to_string().contains("new entries during cleanup"));
        assert!(late.exists());
        assert!(!scratch.join("stale").exists());
    }

    #[test]
    fn scratch_cleanup_preflights_protected_identities_at_any_depth() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("claim scratch");
        let protected = scratch.join("nested/persistent-cache");
        std::fs::create_dir_all(&protected).expect("protected nested directory");
        std::fs::write(protected.join("offline-package"), b"must survive")
            .expect("protected bytes");
        let identity =
            directory_identity_nofollow_blocking(&protected).expect("protected directory identity");

        let error = claim_and_clear_owned_scratch_with_protected_blocking(
            &scratch,
            ".owner",
            b"exact-owner\n",
            &[identity],
        )
        .expect_err("protected identity inside scratch must fail");
        assert!(error.to_string().contains("protected storage identity"));
        assert_eq!(
            std::fs::read(protected.join("offline-package")).expect("protected bytes survive"),
            b"must survive"
        );
    }

    #[tokio::test]
    async fn bounded_tree_removal_preflights_before_unlinking_any_entry() {
        let root = tempfile::tempdir().expect("removal root");
        // Capability opens walk every component with `O_NOFOLLOW`, so a
        // symlinked TMPDIR (`/tmp` -> `/private/tmp` on macOS) is refused
        // before the property under test is reached.
        let root = std::fs::canonicalize(root.path()).expect("canonical removal root");
        let tree = root.join("tree");
        tokio::fs::create_dir(&tree).await.expect("tree");
        tokio::fs::write(tree.join("first"), b"one")
            .await
            .expect("first child");
        tokio::fs::write(tree.join("second"), b"two")
            .await
            .expect("second child");

        let error = remove_bounded_directory_tree_child(&root, "tree", 1, 1)
            .await
            .expect_err("tree exceeds one-entry bound");
        assert!(error.to_string().contains("exceeded"));
        assert!(tree.join("first").is_file());
        assert!(tree.join("second").is_file());
    }

    #[tokio::test]
    async fn bounded_tree_removal_clears_a_nonempty_preflighted_tree() {
        let root = tempfile::tempdir().expect("removal root");
        let root = std::fs::canonicalize(root.path()).expect("canonical removal root");
        let tree = root.join("tree");
        tokio::fs::create_dir_all(tree.join("part-000"))
            .await
            .expect("nested tree");
        tokio::fs::write(tree.join("part-000/segment.ts"), b"segment")
            .await
            .expect("nested child");

        remove_bounded_directory_tree_child(&root, "tree", 2, 2)
            .await
            .expect("bounded removal");
        assert!(!tree.exists());
    }

    #[tokio::test]
    async fn regular_child_placement_accepts_hardlink_ctime_change() {
        let root = tempfile::tempdir().expect("placement root");
        let root = std::fs::canonicalize(root.path()).expect("canonical placement root");
        tokio::fs::create_dir(root.join("source"))
            .await
            .expect("source directory");
        tokio::fs::create_dir(root.join("destination"))
            .await
            .expect("destination directory");
        tokio::fs::write(root.join("source/segment.ts"), b"segment")
            .await
            .expect("source segment");
        let source = SecureDirectory::open(&root.join("source"))
            .await
            .expect("source capability");
        let destination = SecureDirectory::open(&root.join("destination"))
            .await
            .expect("destination capability");

        assert_eq!(
            destination
                .place_regular_child_from(&source, "segment.ts", "seg00000.ts", 1024)
                .await
                .expect("hardlink placement"),
            7
        );
        assert_eq!(
            tokio::fs::read(root.join("destination/seg00000.ts"))
                .await
                .expect("placed bytes"),
            b"segment"
        );
    }

    /// `readdir` reports end-of-directory and a failed read the same way. On a
    /// FUSE/NFS/mergerfs scratch — the media-server pattern — treating an EIO
    /// as "empty" let the daemon plant an ownership marker in a populated
    /// stranger's directory and delete its contents on the same boot.
    #[test]
    fn a_faulting_readdir_is_an_error_not_an_empty_directory() {
        let root = tempfile::tempdir().expect("listing root");
        let root = std::fs::canonicalize(root.path()).expect("canonical listing root");
        std::fs::write(root.join("present"), b"must not look absent").expect("child");
        let directory = open_directory_nofollow_blocking(&root).expect("directory");
        let entries = independent_directory_stream(&directory).expect("directory stream");
        // Point the stream's descriptor at a non-directory, so the next
        // `getdents` fails exactly the way a faulting backing store does:
        // `readdir` returns NULL and only errno separates that from an
        // exhausted directory.
        let devnull = File::open("/dev/null").expect("/dev/null");
        let fd = unsafe { libc::dirfd(entries) };
        assert!(fd >= 0, "the stream must expose its descriptor");
        assert!(unsafe { libc::dup2(devnull.as_raw_fd(), fd) } >= 0);

        let error = unsafe { readdir_checked(entries) }
            .expect_err("a faulting readdir must not read as end-of-directory");
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR), "{error}");
        unsafe { libc::closedir(entries) };
    }

    /// A crash between `openat(O_CREAT|O_EXCL)` and `write_all` leaves a
    /// zero-length pending marker. Every later boot then died on a
    /// contents-mismatch that named neither the file nor a remedy — permanently.
    #[test]
    fn an_interrupted_pending_claim_is_redone_rather_than_bricking_the_scratch() {
        for torn in [b"".as_slice(), b"exact-".as_slice()] {
            let root = tempfile::tempdir().expect("scratch root");
            let scratch = std::fs::canonicalize(root.path())
                .expect("canonical scratch parent")
                .join("scratch");
            std::fs::create_dir(&scratch).expect("scratch directory");
            let pending = scratch.join(".owner.claiming");
            std::fs::write(&pending, torn).expect("torn pending marker");
            std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o600))
                .expect("pending marker mode");

            claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
                .expect("an interrupted claim is redone on the next boot");
            assert!(scratch.join(".owner").exists());
            assert!(!pending.exists());
        }
    }

    /// The torn-claim recovery is scoped to the pending marker: a published
    /// marker that does not match still fails closed.
    #[test]
    fn a_truncated_published_marker_still_fails_closed() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        std::fs::write(scratch.join(".owner"), b"exact-").expect("truncate published marker");

        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("a torn published marker is not an interrupted claim");
        assert!(error.to_string().contains(".owner"), "{error}");
        assert!(
            error.to_string().contains("empty or truncated"),
            "{error}"
        );
    }

    /// After OPERATIONS.md's own "set the new paths and restart" move, the
    /// scratch still carries the old root's marker. The refusal has to name
    /// what claimed it and what to do, not just report a mismatch.
    #[test]
    fn a_marker_naming_another_durable_root_names_it_and_the_remedy() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = std::fs::canonicalize(root.path())
            .expect("canonical scratch parent")
            .join("scratch");
        std::fs::create_dir(&scratch).expect("scratch directory");
        claim_and_clear_owned_scratch_blocking(
            &scratch,
            ".owner",
            b"plurx-transcode-scratch-v1\nowner=\"/srv/old-durable\"\n",
        )
        .expect("claim for the original durable root");

        let error = claim_and_clear_owned_scratch_blocking(
            &scratch,
            ".owner",
            b"plurx-transcode-scratch-v1\nowner=\"/srv/new-durable\"\n",
        )
        .expect_err("a scratch claimed by another durable root must fail closed");
        let error = error.to_string();
        assert!(error.contains("claimed by another durable root"), "{error}");
        assert!(error.contains("/srv/old-durable"), "{error}");
        assert!(error.contains(".owner"), "{error}");
        assert!(error.contains("storage.data_dir"), "{error}");
    }

    /// The cleanup ceilings bound a walk into an *unknown* directory. On a
    /// root the daemon owns, an oversized tree is the ungraceful-crash case
    /// the boot clear exists to survive — so it leaves the tree alone for this
    /// boot instead of making startup permanently impossible.
    #[test]
    fn an_oversized_owned_scratch_is_left_untouched_instead_of_failing_the_boot() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let mut deep = scratch.clone();
        for level in 0..(SCRATCH_CLEANUP_MAX_DEPTH + 4) {
            deep = deep.join(format!("level-{level}"));
        }
        std::fs::create_dir_all(&deep).expect("over-deep scratch tree");
        std::fs::write(deep.join("segment.m4s"), b"deep").expect("deep segment");
        let shallow = scratch.join("shallow.m4s");
        std::fs::write(&shallow, b"shallow").expect("shallow segment");

        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("an over-deep owned scratch must not fail the boot");
        assert!(
            deep.join("segment.m4s").exists(),
            "the skipped tree must be left untouched, not half-cleared"
        );
        assert!(shallow.exists());
        assert!(scratch.join(".owner").exists());
    }

    /// Opt-in real mount-namespace regression for privileged Linux validation:
    /// `PLURX_RUN_BIND_MOUNT_TEST=1 cargo test -p plurx-core mount_point_inside`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_mount_point_inside_scratch_is_named_in_the_error() {
        if std::env::var_os("PLURX_RUN_BIND_MOUNT_TEST").is_none() {
            return;
        }
        struct Mounted(PathBuf);
        impl Drop for Mounted {
            fn drop(&mut self) {
                let _ = std::process::Command::new("umount").arg(&self.0).status();
            }
        }

        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let source = root.path().join("mounted-source");
        std::fs::create_dir(&source).expect("mount source");
        let target = scratch.join("mounted-child");
        std::fs::create_dir(&target).expect("mount target");
        let status = std::process::Command::new("mount")
            .arg("--bind")
            .arg(&source)
            .arg(&target)
            .status()
            .expect("run mount --bind");
        assert!(status.success(), "opt-in bind mount setup failed: {status}");
        let _mounted = Mounted(target.clone());

        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("a mount point inside scratch must fail closed");
        let error = error.to_string();
        assert!(error.contains("mount point"), "{error}");
        assert!(error.contains("mounted-child"), "{error}");
    }
}
