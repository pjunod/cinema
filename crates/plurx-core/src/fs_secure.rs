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
                    device: stat.st_dev as u64,
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
            if !file.metadata()?.is_file() || before.size > max_bytes {
                return Err(io::Error::other("bounded child is not a regular file"));
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
            if result.is_err() {
                unsafe { libc::unlinkat(destination.as_raw_fd(), destination_name.as_ptr(), 0) };
            }
            result
        })
        .await
        .map_err(io::Error::other)?
    }
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
pub async fn directory_identity_nofollow(path: &Path) -> io::Result<FileIdentity> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&path)?;
        file_identity(&directory)
    })
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
pub async fn anonymous_file_in_directory(directory: &Path) -> io::Result<tokio::fs::File> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = open_directory_nofollow_blocking(&directory)?;
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
        Ok(tokio::fs::File::from_std(File::from(fd)))
    })
    .await
    .map_err(io::Error::other)?
}

/// Create a pathname-free memory-backed file for authenticated response
/// snapshots. Linux uses a sealable memfd; macOS uses `SHM_ANON`. Neither
/// consumes free space from the media cache filesystem.
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
    let raw_fd = unsafe {
        // Darwin defines SHM_ANON as the sentinel pointer `(char *)1`; the
        // libc crate does not currently expose that macro.
        libc::shm_open(
            std::ptr::dangling::<libc::c_char>(),
            libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let raw_fd = -1;

    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    Ok(tokio::fs::File::from_std(File::from(fd)))
}

/// Seal a completed Linux memfd against every later write/resize. macOS's
/// anonymous shared-memory object has no public name and remains private to
/// this process; plurx retains only the read path after this call.
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

#[cfg(test)]
mod tests {
    use super::*;

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
