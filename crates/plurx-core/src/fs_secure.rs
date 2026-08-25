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
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::path::{Component, Path};
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
                    let entry = unsafe { libc::readdir(entries) };
                    if entry.is_null() {
                        break;
                    }
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
            let entry = unsafe { libc::readdir(entries) };
            if entry.is_null() {
                break;
            }
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
            let entry = unsafe { libc::readdir(entries) };
            if entry.is_null() {
                break;
            }
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
                let entry = unsafe { libc::readdir(entries) };
                if entry.is_null() {
                    break;
                }
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
                    let entry = unsafe { libc::readdir(entries) };
                    if entry.is_null() {
                        break;
                    }
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

fn scratch_marker_identity(
    directory: &File,
    marker: &CString,
    expected: &[u8],
) -> io::Result<FileIdentity> {
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
    if actual != expected || file_identity(&file)? != before {
        return Err(io::Error::other(
            "scratch ownership marker contents or identity do not match",
        ));
    }
    Ok(before)
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

fn validate_scratch_entry(stat: &libc::stat) -> io::Result<bool> {
    if stat.st_uid != unsafe { libc::geteuid() } {
        return Err(io::Error::other(
            "scratch tree contains an entry owned by another uid",
        ));
    }
    let is_directory = match stat.st_mode & libc::S_IFMT {
        libc::S_IFDIR => true,
        libc::S_IFREG => false,
        libc::S_IFLNK => Err(io::Error::other("scratch tree contains a symbolic link"))?,
        _ => Err(io::Error::other(
            "scratch tree contains a device or other special file",
        ))?,
    };
    if stat.st_mode & 0o022 != 0 {
        return Err(io::Error::other(
            "scratch tree contains an entry with group/world-writable permissions",
        ));
    }
    Ok(is_directory)
}

fn scratch_has_only_preserved(directory: &File, preserved: &[&CStr]) -> io::Result<bool> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| loop {
        let entry = unsafe { libc::readdir(entries) };
        if entry.is_null() {
            return Ok(true);
        }
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

fn count_scratch_capability(
    directory: &File,
    marker: Option<&CStr>,
    counted: &mut usize,
    depth: usize,
    root_device: u64,
    protected: &[FileIdentity],
) -> io::Result<()> {
    let entries = independent_directory_stream(directory)?;
    let result = (|| {
        loop {
            let entry = unsafe { libc::readdir(entries) };
            if entry.is_null() {
                break;
            }
            let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if child.to_bytes() == b"."
                || child.to_bytes() == b".."
                || (depth == 0 && marker.is_some_and(|name| name.to_bytes() == child.to_bytes()))
            {
                continue;
            }
            *counted = counted.saturating_add(1);
            if *counted > SCRATCH_CLEANUP_MAX_ENTRIES {
                return Err(io::Error::other(
                    "scratch tree exceeded its cleanup entry bound",
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
                    return Err(io::Error::other(
                        "scratch tree exceeded its cleanup depth bound",
                    ));
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
                count_scratch_capability(
                    &child_directory,
                    None,
                    counted,
                    depth + 1,
                    root_device,
                    protected,
                )?;
            }
        }
        Ok(())
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
            let entry = unsafe { libc::readdir(entries) };
            if entry.is_null() {
                break;
            }
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
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::other(
            "scratch root must be owned by the daemon uid and not group/world-writable",
        ));
    }
    let marker = marker_name.map(child_name).transpose()?;
    let marker_identity = if let Some(marker) = marker.as_ref() {
        let pending = child_name(&format!(
            "{}.claiming",
            marker_name.expect("marker CString came from marker name")
        ))?;
        match scratch_marker_identity(&directory, marker, expected_marker) {
            Ok(identity) => match scratch_marker_identity(&directory, &pending, expected_marker) {
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
                let pending_identity = match scratch_marker_identity(
                    &directory,
                    &pending,
                    expected_marker,
                ) {
                    Ok(identity) => identity,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        // A durable pending marker is deliberately not an
                        // ownership grant. Crash recovery may promote it only
                        // while it remains the directory's sole entry.
                        if !scratch_has_only_preserved(&directory, &[])? {
                            return Err(io::Error::other(
                                "explicit scratch root was populated before ownership could be claimed",
                            ));
                        }
                        let raw = unsafe {
                            libc::openat(
                                directory.as_raw_fd(),
                                pending.as_ptr(),
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
                        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        file.write_all(expected_marker)?;
                        file.sync_all()?;
                        directory.sync_all()?;
                        if let Some(hook) = hook {
                            hook(ScratchHookPhase::AfterPendingCreate);
                        }
                        scratch_marker_identity(&directory, &pending, expected_marker)?
                    }
                    Err(error) => return Err(error),
                };
                if !scratch_has_only_preserved(&directory, &[pending.as_c_str()])? {
                    return Err(io::Error::other(
                        "explicit scratch root was populated before ownership could be claimed",
                    ));
                }
                if !scratch_marker_identity(&directory, &pending, expected_marker)?
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
                scratch_marker_identity(&directory, marker, expected_marker)?
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
        let current_marker = scratch_marker_identity(&directory, marker, expected_marker)?;
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
    count_scratch_capability(
        &directory,
        marker.as_deref(),
        &mut counted,
        0,
        root_identity.device,
        protected,
    )?;
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
        let current_marker = scratch_marker_identity(&directory, marker, expected_marker)?;
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
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

    /// Pin the complete descriptor contract used by authenticated response
    /// snapshots on every supported platform. This catches both a macOS
    /// `shm_open` descriptor that is not a seekable regular file and a Linux
    /// memfd whose sealing/readback path has regressed.
    #[tokio::test]
    async fn anonymous_memory_file_round_trips_after_sealing() {
        let expected = b"authenticated response snapshot";
        let mut file = anonymous_memory_file().expect("anonymous snapshot file");

        file.write_all(expected).await.expect("write snapshot");
        file.flush().await.expect("flush snapshot");
        seal_anonymous_memory_file(&file).expect("seal snapshot");
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .expect("rewind snapshot");

        let mut actual = Vec::new();
        file.read_to_end(&mut actual).await.expect("read snapshot");
        assert_eq!(actual, expected);
    }

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

    #[test]
    fn scratch_claim_requires_private_daemon_owned_permissions() {
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
        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("world-writable scratch must fail");
        assert!(error.to_string().contains("not group/world-writable"));
        assert!(!scratch.join(".owner").exists());

        let mut permissions = std::fs::metadata(&scratch)
            .expect("scratch metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&scratch, permissions).expect("private scratch");
        claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect("private scratch claim");
        let marker = std::fs::metadata(scratch.join(".owner")).expect("marker metadata");
        assert_eq!(marker.uid(), unsafe { libc::geteuid() });
        assert_eq!(marker.mode() & 0o777, 0o600);
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

    #[test]
    fn scratch_cleanup_refuses_unsafe_child_permissions_without_deleting_data() {
        let root = tempfile::tempdir().expect("scratch root");
        let scratch = claimed_scratch(root.path());
        let unsafe_child = scratch.join("group-writable-segment");
        std::fs::write(&unsafe_child, b"must survive").expect("unsafe child");
        let mut permissions = std::fs::metadata(&unsafe_child)
            .expect("unsafe child metadata")
            .permissions();
        permissions.set_mode(0o660);
        std::fs::set_permissions(&unsafe_child, permissions).expect("unsafe permissions");

        let error = claim_and_clear_owned_scratch_blocking(&scratch, ".owner", b"exact-owner\n")
            .expect_err("unsafe child permissions must fail closed");
        assert!(
            error.to_string().contains("group/world-writable"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(unsafe_child).expect("unsafe child survives"),
            b"must survive"
        );
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
        let tree = root.path().join("tree");
        tokio::fs::create_dir(&tree).await.expect("tree");
        tokio::fs::write(tree.join("first"), b"one")
            .await
            .expect("first child");
        tokio::fs::write(tree.join("second"), b"two")
            .await
            .expect("second child");

        let error = remove_bounded_directory_tree_child(root.path(), "tree", 1, 1)
            .await
            .expect_err("tree exceeds one-entry bound");
        assert!(error.to_string().contains("exceeded"));
        assert!(tree.join("first").is_file());
        assert!(tree.join("second").is_file());
    }

    #[tokio::test]
    async fn bounded_tree_removal_clears_a_nonempty_preflighted_tree() {
        let root = tempfile::tempdir().expect("removal root");
        let tree = root.path().join("tree");
        tokio::fs::create_dir_all(tree.join("part-000"))
            .await
            .expect("nested tree");
        tokio::fs::write(tree.join("part-000/segment.ts"), b"segment")
            .await
            .expect("nested child");

        remove_bounded_directory_tree_child(root.path(), "tree", 2, 2)
            .await
            .expect("bounded removal");
        assert!(!tree.exists());
    }

    #[tokio::test]
    async fn regular_child_placement_accepts_hardlink_ctime_change() {
        let root = tempfile::tempdir().expect("placement root");
        tokio::fs::create_dir(root.path().join("source"))
            .await
            .expect("source directory");
        tokio::fs::create_dir(root.path().join("destination"))
            .await
            .expect("destination directory");
        tokio::fs::write(root.path().join("source/segment.ts"), b"segment")
            .await
            .expect("source segment");
        let source = SecureDirectory::open(&root.path().join("source"))
            .await
            .expect("source capability");
        let destination = SecureDirectory::open(&root.path().join("destination"))
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
            tokio::fs::read(root.path().join("destination/seg00000.ts"))
                .await
                .expect("placed bytes"),
            b"segment"
        );
    }
}
