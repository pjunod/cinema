//! Pinned descriptor evidence supplied only by the audited child builder.
//! Regular files remain Source unless their exact identity has an active
//! daemon-asset registration. A role such as "output" grants no authority.
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdioMode {
    Null,
    Piped,
    /// The builder installs this target (0, 1 or 2) from its pinned mapping.
    Descriptor(i32),
    /// Opaque inherited stdio cannot verify an explicit access declaration.
    Inherited,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdioFacts {
    pub stdin: StdioMode,
    pub stdout: StdioMode,
    pub stderr: StdioMode,
}
impl StdioFacts {
    pub const NULL: Self = Self {
        stdin: StdioMode::Null,
        stdout: StdioMode::Null,
        stderr: StdioMode::Null,
    };
    pub const CAPTURED: Self = Self {
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    };
}

#[cfg(unix)]
#[derive(Clone)]
pub struct InheritedFd {
    file: Arc<File>,
    target: i32,
}
#[cfg(unix)]
impl InheritedFd {
    /// Duplicate atomically before ownership can leave the builder. Invalid
    /// raw descriptors return an OS error and never manufacture a capability.
    pub fn duplicate_raw(source: std::os::fd::RawFd, target: i32) -> io::Result<Self> {
        use std::os::fd::FromRawFd;
        if !(0..=65_535).contains(&target) {
            return Err(io::Error::other("invalid child descriptor target"));
        }
        // SAFETY: fcntl consumes only an integer descriptor; a stale/closed
        // descriptor fails atomically. The returned new descriptor is owned.
        let duplicate = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful fcntl above created this unique descriptor.
        let file = unsafe { File::from_raw_fd(duplicate) };
        Ok(Self {
            file: Arc::new(file),
            target,
        })
    }
    pub fn duplicate(source: &File, target: i32) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        Self::duplicate_raw(source.as_raw_fd(), target)
    }
    pub fn target(&self) -> i32 {
        self.target
    }
    pub fn file(&self) -> &File {
        &self.file
    }
}

#[cfg(not(unix))]
#[derive(Clone)]
pub struct InheritedFd {
    _private: (),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandBinding {
    address: usize,
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, Option<OsString>)>,
    cwd: Option<PathBuf>,
}
impl CommandBinding {
    fn of(command: &std::process::Command) -> Self {
        Self {
            address: command as *const std::process::Command as usize,
            program: command.get_program().to_owned(),
            args: command.get_args().map(ToOwned::to_owned).collect(),
            env: command
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
                .collect(),
            cwd: command.get_current_dir().map(ToOwned::to_owned),
        }
    }
}

/// The builder owns this manifest through the exact final launcher scope.
/// Stdio facts must describe the builder's actual Command configuration;
/// the source census forbids file stdio and descriptor pre_exec elsewhere.
pub struct DescriptorManifest {
    inherited: Vec<InheritedFd>,
    pub(super) stdio: StdioFacts,
    pub(super) cwd_target: Option<i32>,
    binding: OnceLock<CommandBinding>,
}
impl DescriptorManifest {
    pub fn new(
        inherited: Vec<InheritedFd>,
        stdio: StdioFacts,
        cwd_target: Option<i32>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        let inherited = {
            use std::collections::BTreeSet;
            use std::os::fd::{AsRawFd, FromRawFd};
            if inherited.len() > 256 {
                return Err(io::Error::other("too many inherited descriptors"));
            }
            let mut targets = BTreeSet::new();
            for entry in &inherited {
                if !targets.insert(entry.target) {
                    return Err(io::Error::other("duplicate child descriptor target"));
                }
            }
            for (target, mode) in [(0, stdio.stdin), (1, stdio.stdout), (2, stdio.stderr)] {
                if targets.contains(&target) && mode != StdioMode::Descriptor(target) {
                    return Err(io::Error::other(
                        "stdio mapping and builder stream configuration differ",
                    ));
                }
                if let StdioMode::Descriptor(declared) = mode {
                    if declared != target || !targets.contains(&target) {
                        return Err(io::Error::other(
                            "stdio descriptor is not the matching planned target",
                        ));
                    }
                }
            }
            if cwd_target.is_some_and(|fd| !targets.contains(&fd)) {
                return Err(io::Error::other("child cwd descriptor is not registered"));
            }
            let floor = targets
                .last()
                .copied()
                .unwrap_or(2)
                .checked_add(1)
                .ok_or_else(|| io::Error::other("child descriptor target overflow"))?
                .max(3);
            // Every parent-side source is pinned above every target before
            // fork. The inline installer consequently allocates nothing and
            // cannot clobber a still-needed source while installing a target.
            let mut pinned = Vec::with_capacity(inherited.len());
            for entry in inherited {
                // SAFETY: integer-only duplication; ownership follows success.
                let fd =
                    unsafe { libc::fcntl(entry.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, floor) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                pinned.push(InheritedFd {
                    file: Arc::new(unsafe { File::from_raw_fd(fd) }),
                    target: entry.target,
                });
            }
            pinned
        };
        #[cfg(not(unix))]
        if !inherited.is_empty() || cwd_target.is_some() {
            return Err(io::Error::other("descriptor manifests require Unix"));
        }
        Ok(Self {
            inherited,
            stdio,
            cwd_target,
            binding: OnceLock::new(),
        })
    }

    /// Freeze after wiring the final Command, immediately before the launch
    /// scope. Moving or mutating that command afterwards fails verification.
    pub fn bind(&self, command: &std::process::Command) -> io::Result<()> {
        let binding = CommandBinding::of(command);
        if let Some(existing) = self.binding.get() {
            if existing == &binding {
                return Ok(());
            }
            return Err(io::Error::other(
                "descriptor manifest already bound to another command",
            ));
        }
        self.binding
            .set(binding)
            .map_err(|_| io::Error::other("descriptor manifest binding raced"))
    }
    pub(super) fn matches(&self, command: &std::process::Command) -> bool {
        self.binding
            .get()
            .is_some_and(|binding| binding == &CommandBinding::of(command))
    }
    pub fn inherited(&self) -> &[InheritedFd] {
        &self.inherited
    }

    /// Execute only in the child between fork and exec. All descriptor
    /// duplication/allocation happened in new(); this is also usable before a
    /// decoder's custom exec hook which never returns to later pre_exec hooks.
    #[cfg(unix)]
    pub fn inherit(&self) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        for entry in &self.inherited {
            // SAFETY: only async-signal-safe descriptor syscalls on pinned FDs.
            if unsafe { libc::dup2(entry.file.as_raw_fd(), entry.target) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if let Some(target) = self.cwd_target {
            // SAFETY: target is a validated registered directory descriptor.
            if unsafe { libc::fchdir(target) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    /// Reinstall one planned target after a custom exec hook marks a range
    /// CLOEXEC. The pinned original remains alive and no allocation occurs.
    #[cfg(unix)]
    pub fn inherit_target(&self, target: i32) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let entry = self
            .inherited
            .iter()
            .find(|entry| entry.target == target)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: integer-only async-signal-safe duplication of a pinned FD.
        if unsafe { libc::dup2(entry.file.as_raw_fd(), target) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    pub fn inherit(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Identity and path must still agree at launch; replacing an asset does not
/// bless the replacement, and dropping its guard revokes the registration.
pub struct DaemonFileRegistration {
    pub(super) id: u64,
}
impl Drop for DaemonFileRegistration {
    fn drop(&mut self) {
        super::policy::unregister_asset(self.id);
    }
}

pub fn register_daemon_file(file: &File, path: &Path) -> io::Result<DaemonFileRegistration> {
    super::policy::register_asset(file, path)
}
