//! Descriptor-bound decoder fact collection and its bounded node-local cache.
//!
//! The source handle is opened and authorized by the preparation owner. This
//! module never reopens its pathname: FFprobe receives a duplicate of that
//! exact handle, and metadata is checked again after probing before the facts
//! can be returned or cached.

use std::collections::{BTreeMap, VecDeque};
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::ffi::{CString, OsStr};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use plurx_core::transcode::{DecodeCatalogMetadata, DecodeFacts, DecodeSourceIdentity};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

const MAX_PROBE_STDOUT_BYTES: usize = 256 * 1024;
const MAX_PROBE_STDERR_BYTES: usize = 16 * 1024;
const MAX_PROBE_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64 * 1024;
const IDENTITY_DEADLINE: Duration = Duration::from_secs(10);
const VERSION_DEADLINE: Duration = Duration::from_secs(5);
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
const MAX_CACHE_ENTRIES: usize = 256;
#[cfg(unix)]
const HELD_PROBE_FD: std::os::fd::RawFd = 4;

fn identity_gate() -> Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

fn version_gate() -> Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

#[cfg(test)]
fn test_discovery_gate() -> Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    source: DecodeSourceIdentity,
    ffprobe_build_digest: String,
    catalog_digest: Option<String>,
    selected_stream: ProbeStreamSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // FirstPlayable is consumed by M2's selected-stream adapter.
pub(crate) enum ProbeStreamSelection {
    FirstPlayable,
    LegacyVideoOrdinal(u32),
    Absolute(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeLaunchMode {
    Production,
    #[cfg(test)]
    Fixture,
    #[cfg(all(test, target_os = "linux"))]
    ProductionPidfdOpenFailure,
    #[cfg(all(test, target_os = "linux"))]
    ProductionPidfdReadFailure,
    #[cfg(all(test, target_os = "linux"))]
    ProductionSupervisorDelay,
    #[cfg(all(test, target_os = "linux"))]
    ProductionSupervisorReceiveFailure,
    #[cfg(all(test, target_os = "linux"))]
    ProductionBootstrapInterrupted,
    #[cfg(all(test, target_os = "linux"))]
    ProductionBootstrapInterruptedUntilDeadline(LinuxBootstrapPhase),
    #[cfg(all(test, target_os = "linux"))]
    ProductionSteadyResponseInterruptedUntilDeadline,
    #[cfg(all(test, target_os = "linux"))]
    ProductionSteadyResponseInterruptedUntilStop,
    #[cfg(all(test, target_os = "linux"))]
    ProductionFdExport(std::os::fd::RawFd),
}

impl ProbeLaunchMode {
    fn is_production(self) -> bool {
        match self {
            Self::Production => true,
            #[cfg(test)]
            Self::Fixture => false,
            #[cfg(all(test, target_os = "linux"))]
            Self::ProductionPidfdOpenFailure
            | Self::ProductionPidfdReadFailure
            | Self::ProductionSupervisorDelay
            | Self::ProductionSupervisorReceiveFailure
            | Self::ProductionBootstrapInterrupted
            | Self::ProductionBootstrapInterruptedUntilDeadline(_)
            | Self::ProductionSteadyResponseInterruptedUntilDeadline
            | Self::ProductionSteadyResponseInterruptedUntilStop
            | Self::ProductionFdExport(_) => true,
        }
    }

    #[cfg(target_os = "linux")]
    fn inject_pidfd_open_failure(self) -> bool {
        #[cfg(test)]
        if self == Self::ProductionPidfdOpenFailure {
            return true;
        }
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_pidfd_read_failure(self) -> bool {
        if self == Self::ProductionPidfdReadFailure {
            return true;
        }
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_slow_reap(self) -> bool {
        matches!(
            self,
            Self::ProductionPidfdOpenFailure
                | Self::ProductionPidfdReadFailure
                | Self::ProductionSteadyResponseInterruptedUntilDeadline
        )
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_supervisor_delay(self) -> bool {
        self == Self::ProductionSupervisorDelay
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_supervisor_receive_failure(self) -> bool {
        self == Self::ProductionSupervisorReceiveFailure
    }

    #[cfg(all(test, target_os = "linux"))]
    fn export_socket_fd(self) -> Option<std::os::fd::RawFd> {
        match self {
            Self::ProductionFdExport(fd) => Some(fd),
            _ => None,
        }
    }

    #[cfg(target_os = "linux")]
    fn bootstrap_interrupts(self) -> LinuxBootstrapInterrupts {
        #[cfg(test)]
        match self {
            Self::ProductionBootstrapInterrupted => {
                return LinuxBootstrapInterrupts {
                    ready_send: 1,
                    acknowledgement_send: 1,
                    first_notification_receive: 1,
                    first_notification_response: 1,
                    ..LinuxBootstrapInterrupts::default()
                };
            }
            Self::ProductionBootstrapInterruptedUntilDeadline(phase) => {
                let mut interrupts = LinuxBootstrapInterrupts::default();
                *match phase {
                    LinuxBootstrapPhase::ReadySend => &mut interrupts.ready_send,
                    LinuxBootstrapPhase::AcknowledgementSend => {
                        &mut interrupts.acknowledgement_send
                    }
                    LinuxBootstrapPhase::FirstNotificationReceive => {
                        &mut interrupts.first_notification_receive
                    }
                    LinuxBootstrapPhase::FirstNotificationInvalidated => {
                        interrupts.invalidate_first_notification_on_interrupt = true;
                        &mut interrupts.first_notification_receive
                    }
                    LinuxBootstrapPhase::FirstNotificationResponse => {
                        &mut interrupts.first_notification_response
                    }
                } = if phase == LinuxBootstrapPhase::FirstNotificationInvalidated {
                    1
                } else {
                    u8::MAX
                };
                return interrupts;
            }
            Self::ProductionSteadyResponseInterruptedUntilDeadline => {
                return LinuxBootstrapInterrupts {
                    steady_notification_response: u8::MAX,
                    steady_notification_response_interrupts: Some(
                        &STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS,
                    ),
                    steady_notification_response_deadline_exits: Some(
                        &STEADY_RESPONSE_DEADLINE_EXITS,
                    ),
                    ..LinuxBootstrapInterrupts::default()
                };
            }
            Self::ProductionSteadyResponseInterruptedUntilStop => {
                return LinuxBootstrapInterrupts {
                    steady_notification_response: u8::MAX,
                    steady_notification_response_interrupts: Some(
                        &STEADY_RESPONSE_STOP_INTERRUPT_HITS,
                    ),
                    steady_notification_response_stop_exits: Some(&STEADY_RESPONSE_STOP_EXITS),
                    stop_after_steady_notification_response_interrupt: true,
                    ..LinuxBootstrapInterrupts::default()
                };
            }
            _ => {}
        }
        LinuxBootstrapInterrupts::default()
    }

    #[cfg(all(test, target_os = "linux"))]
    fn supervisor_test_counter(self) -> Option<&'static std::sync::atomic::AtomicUsize> {
        match self {
            Self::ProductionPidfdOpenFailure => Some(&PIDFD_OPEN_SUPERVISOR_OWNERS),
            Self::ProductionPidfdReadFailure => Some(&PIDFD_READ_SUPERVISOR_OWNERS),
            Self::ProductionSupervisorDelay => Some(&DELAYED_SUPERVISOR_OWNERS),
            Self::ProductionSupervisorReceiveFailure => Some(&FAILED_SUPERVISOR_OWNERS),
            Self::ProductionBootstrapInterruptedUntilDeadline(_) => {
                Some(&INTERRUPTED_SUPERVISOR_OWNERS)
            }
            Self::ProductionSteadyResponseInterruptedUntilDeadline => {
                Some(&STEADY_RESPONSE_SUPERVISOR_OWNERS)
            }
            Self::ProductionSteadyResponseInterruptedUntilStop => {
                Some(&STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS)
            }
            _ => None,
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxBootstrapPhase {
    ReadySend,
    AcknowledgementSend,
    FirstNotificationReceive,
    FirstNotificationInvalidated,
    FirstNotificationResponse,
}

/// A held source descriptor and the exclusive ownership lane for every child
/// that can seek its shared open-file description.
#[derive(Clone)]
pub(crate) struct DecodeFactSource {
    handle: Arc<std::fs::File>,
    offset_gate: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    initial_identity_delay: Duration,
    #[cfg(test)]
    final_identity_delay: Duration,
}

impl DecodeFactSource {
    pub(crate) fn new(
        handle: Arc<std::fs::File>,
        offset_gate: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            handle,
            offset_gate,
            #[cfg(test)]
            initial_identity_delay: Duration::ZERO,
            #[cfg(test)]
            final_identity_delay: Duration::ZERO,
        }
    }

    #[cfg(test)]
    fn isolated(handle: Arc<std::fs::File>) -> Self {
        Self::new(handle, Arc::new(tokio::sync::Semaphore::new(1)))
    }

    #[cfg(test)]
    fn with_identity_delay(mut self, delay: Duration) -> Self {
        self.initial_identity_delay = delay;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_final_identity_delay(mut self, delay: Duration) -> Self {
        self.final_identity_delay = delay;
        self
    }

    #[cfg(test)]
    fn initial_identity_delay(&self) -> Duration {
        self.initial_identity_delay
    }

    #[cfg(not(test))]
    fn initial_identity_delay(&self) -> Duration {
        Duration::ZERO
    }

    #[cfg(test)]
    fn final_identity_delay(&self) -> Duration {
        self.final_identity_delay
    }

    #[cfg(not(test))]
    fn final_identity_delay(&self) -> Duration {
        Duration::ZERO
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeFileIdentity {
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    device: u64,
    inode: u64,
    content_digest: String,
}

/// Startup-bound FFprobe identity. Production collection accepts only an
/// operator-trusted, self-contained Linux ELF, copies its primary image into a
/// sealed anonymous executable, and prevents that child from performing a
/// later exec. This binds the primary image; it is not a sandbox for malicious
/// parser behavior such as interpreting or mapping readable bytes as code.
/// Path revalidation before and after collection detects replacement of the
/// configured artifact without allowing a transient swap to choose another
/// primary executable object.
#[derive(Debug, Clone)]
pub(crate) struct DecodeProbeIdentity {
    executable: PathBuf,
    executable_file: Arc<std::fs::File>,
    executable_snapshot: Arc<ExecutableSnapshot>,
    build_digest: String,
    file: ProbeFileIdentity,
    snapshot_file: ProbeFileIdentity,
    launch_mode: ProbeLaunchMode,
}

#[derive(Debug)]
struct ExecutableSnapshot {
    file: std::fs::File,
    // Non-Linux test fixtures execute an owned temporary path. Production
    // support is Linux-only and uses a sealed anonymous descriptor.
    #[cfg(not(target_os = "linux"))]
    _path: tempfile::TempPath,
}

impl ExecutableSnapshot {
    fn as_file(&self) -> &std::fs::File {
        &self.file
    }

    #[cfg(not(target_os = "linux"))]
    fn path(&self) -> &Path {
        self._path.as_ref()
    }
}

#[cfg(target_os = "macos")]
impl Drop for ExecutableSnapshot {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        unsafe {
            libc::fchflags(self.file.as_raw_fd(), 0);
        }
    }
}

impl DecodeProbeIdentity {
    pub(crate) async fn discover(bin: &str) -> Result<Self, DecodeFactError> {
        Self::discover_with_mode(bin, ProbeLaunchMode::Production).await
    }

    #[cfg(test)]
    pub(crate) async fn discover_fixture(bin: &str) -> Result<Self, DecodeFactError> {
        Self::discover_with_mode(bin, ProbeLaunchMode::Fixture).await
    }

    async fn discover_with_mode(
        bin: &str,
        launch_mode: ProbeLaunchMode,
    ) -> Result<Self, DecodeFactError> {
        // Unit fixtures are executable scripts and exercise many independent
        // startup identities in parallel. Serialize their full discovery so
        // host scheduler load cannot consume a production-scale version
        // deadline before an individual fixture runs.
        #[cfg(test)]
        let _test_discovery = test_discovery_gate()
            .acquire_owned()
            .await
            .map_err(|_| DecodeFactError::CacheInvariant)?;
        let executable = resolve_executable(bin)?;
        let executable_file = Arc::new(
            std::fs::File::open(&executable)
                .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
        );
        let before =
            held_probe_file_identity_within(Arc::clone(&executable_file), IDENTITY_DEADLINE, None)
                .await?;
        let snapshot_source = Arc::clone(&executable_file);
        let executable_snapshot = Arc::new(
            run_bounded_identity_task(IDENTITY_DEADLINE, None, move || {
                snapshot_executable(&snapshot_source)
            })
            .await?,
        );
        let snapshot_file = held_probe_file_identity_within(
            Arc::new(
                executable_snapshot
                    .as_file()
                    .try_clone()
                    .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
            ),
            IDENTITY_DEADLINE,
            None,
        )
        .await?;
        if snapshot_file.content_digest != before.content_digest {
            return Err(DecodeFactError::ProbeChanged);
        }
        if launch_mode.is_production() {
            require_direct_probe_executable(executable_snapshot.as_file())?;
        }
        let version = probe_version(&executable_snapshot, &executable, launch_mode).await?;
        let after_path = executable.clone();
        let after = path_probe_file_identity_within(after_path, IDENTITY_DEADLINE, None).await?;
        if before != after {
            return Err(DecodeFactError::ProbeChanged);
        }
        let digest_input = serde_json::json!({
            "canonical_path": &executable,
            "content_digest": &before.content_digest,
            "version": &version,
        });
        Ok(Self {
            executable,
            executable_file,
            executable_snapshot,
            build_digest: hex::encode(Sha256::digest(digest_input.to_string().as_bytes())),
            file: before,
            snapshot_file,
            launch_mode,
        })
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn build_digest(&self) -> &str {
        &self.build_digest
    }

    async fn validate_current(
        &self,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(), DecodeFactError> {
        let started = std::time::Instant::now();
        let held =
            held_probe_file_identity_within(Arc::clone(&self.executable_file), budget, cancelled)
                .await?;
        let remaining = budget.saturating_sub(started.elapsed());
        let snapshot = held_probe_file_identity_within(
            Arc::new(
                self.executable_snapshot
                    .as_file()
                    .try_clone()
                    .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
            ),
            remaining,
            cancelled,
        )
        .await?;
        let remaining = budget.saturating_sub(started.elapsed());
        let current =
            path_probe_file_identity_within(self.executable.clone(), remaining, cancelled).await?;
        if held == self.file && current == self.file && snapshot == self.snapshot_file {
            Ok(())
        } else {
            Err(DecodeFactError::ProbeChanged)
        }
    }
}

/// Qualified fact collection executes one self-contained primary parser image.
/// Scripts and dynamic ELF images have an external launch/dependency closure,
/// so production refuses them. Structural ELF inspection does not establish
/// that arbitrary parser code is trustworthy or unable to interpret readable
/// input as code; safe enablement requires an operator-trusted artifact.
#[cfg(unix)]
fn require_direct_probe_executable(file: &std::fs::File) -> Result<(), DecodeFactError> {
    #[cfg(target_os = "linux")]
    {
        require_self_contained_linux_elf(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(DecodeFactError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
fn require_self_contained_linux_elf(file: &std::fs::File) -> Result<(), DecodeFactError> {
    use std::os::unix::fs::FileExt;

    fn value(bytes: &[u8], little_endian: bool) -> u64 {
        let mut padded = [0_u8; 8];
        padded[..bytes.len()].copy_from_slice(bytes);
        if little_endian {
            u64::from_le_bytes(padded)
        } else {
            padded[..bytes.len()].reverse();
            u64::from_le_bytes(padded)
        }
    }

    let mut header = [0_u8; 64];
    file.read_exact_at(&mut header, 0).map_err(|error| {
        DecodeFactError::ProbeIdentity(format!("FFprobe is not a complete ELF executable: {error}"))
    })?;
    if header[..4] != *b"\x7fELF" {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe must be a self-contained Linux ELF; wrappers are not build-bound"
                .to_owned(),
        ));
    }
    let little_endian = match header[5] {
        1 => true,
        2 => false,
        _ => {
            return Err(DecodeFactError::ProbeIdentity(
                "configured FFprobe has an unsupported ELF byte order".to_owned(),
            ));
        }
    };
    let (program_offset, entry_size, entry_count) = match header[4] {
        1 => (
            value(&header[28..32], little_endian),
            value(&header[42..44], little_endian),
            value(&header[44..46], little_endian),
        ),
        2 => (
            value(&header[32..40], little_endian),
            value(&header[54..56], little_endian),
            value(&header[56..58], little_endian),
        ),
        _ => {
            return Err(DecodeFactError::ProbeIdentity(
                "configured FFprobe has an unsupported ELF class".to_owned(),
            ));
        }
    };
    if entry_size < 4 || entry_count == 0 || entry_count > 1_024 {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe has an invalid ELF program table".to_owned(),
        ));
    }
    let table_bytes = entry_size
        .checked_mul(entry_count)
        .and_then(|bytes| program_offset.checked_add(bytes))
        .ok_or_else(|| {
            DecodeFactError::ProbeIdentity("FFprobe ELF program table overflowed".to_owned())
        })?;
    if table_bytes
        > file
            .metadata()
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?
            .len()
    {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe has a truncated ELF program table".to_owned(),
        ));
    }
    let mut program_type = [0_u8; 4];
    for entry in 0..entry_count {
        file.read_exact_at(&mut program_type, program_offset + entry * entry_size)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        let program_type = value(&program_type, little_endian);
        if program_type == 2 || program_type == 3 {
            return Err(DecodeFactError::ProbeIdentity(
                "configured FFprobe must be statically linked with no interpreter or dynamic dependency closure"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn resolve_executable(bin: &str) -> Result<PathBuf, DecodeFactError> {
    use std::os::unix::fs::PermissionsExt;

    let candidate = Path::new(bin);
    let resolved = if candidate.components().count() > 1 {
        candidate.to_owned()
    } else {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .map(|directory| directory.join(candidate))
            .find(|path| {
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable is not on PATH".to_owned()))?
    };
    std::fs::canonicalize(resolved)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))
}

#[cfg(not(unix))]
fn resolve_executable(_bin: &str) -> Result<PathBuf, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(unix)]
fn duplicate_child_fd(source: std::os::fd::RawFd) -> std::io::Result<std::os::fd::RawFd> {
    let duplicate = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(duplicate)
    }
}

#[cfg(unix)]
fn assign_child_fd(
    duplicate: std::os::fd::RawFd,
    target: std::os::fd::RawFd,
) -> std::io::Result<()> {
    if unsafe { libc::dup2(duplicate, target) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(target, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(target, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn install_child_fd(source: std::os::fd::RawFd, target: std::os::fd::RawFd) -> std::io::Result<()> {
    let duplicate = duplicate_child_fd(source)?;
    let result = assign_child_fd(duplicate, target);
    unsafe { libc::close(duplicate) };
    result
}

#[cfg(unix)]
fn install_probe_child_fds(
    source: std::os::fd::RawFd,
    executable: std::os::fd::RawFd,
) -> std::io::Result<()> {
    let source_duplicate = duplicate_child_fd(source)?;
    let executable_duplicate = match duplicate_child_fd(executable) {
        Ok(duplicate) => duplicate,
        Err(error) => {
            unsafe { libc::close(source_duplicate) };
            return Err(error);
        }
    };
    let result = assign_child_fd(source_duplicate, 3)
        .and_then(|()| assign_child_fd(executable_duplicate, HELD_PROBE_FD));
    unsafe {
        libc::close(source_duplicate);
        libc::close(executable_duplicate);
    }
    result
}

#[cfg(target_os = "linux")]
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1;

#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

/// Deny execution from every path-backed filesystem. The first parser exec is
/// performed directly from the sealed anonymous descriptor, which Landlock
/// deliberately does not mediate. A separate one-shot seccomp supervisor
/// admits that exact exec and denies every later exec attempt.
#[cfg(target_os = "linux")]
fn install_linux_filesystem_execute_denial() -> std::io::Result<()> {
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 1 {
        return Err(std::io::Error::last_os_error());
    }
    let ruleset_attr = LandlockRulesetAttr {
        handled_access_fs: LANDLOCK_ACCESS_FS_EXECUTE,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const ruleset_attr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0,
        )
    };
    if ruleset_fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == -1 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(ruleset_fd as libc::c_int) };
        return Err(error);
    }
    let restrict_result = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0) };
    unsafe { libc::close(ruleset_fd as libc::c_int) };
    if restrict_result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
#[cfg(target_os = "linux")]
const SECCOMP_DATA_ARGS_OFFSET: u32 = 16;
#[cfg(target_os = "linux")]
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
#[cfg(target_os = "linux")]
const SECCOMP_FILTER_FLAG_NEW_LISTENER: u32 = 1 << 3;
#[cfg(target_os = "linux")]
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const LINUX_AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const LINUX_AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
const LINUX_AUDIT_ARCH: u32 = 0;

#[cfg(target_os = "linux")]
fn seccomp_statement(code: u16, value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k: value,
    }
}

#[cfg(target_os = "linux")]
fn seccomp_jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: yes,
        jf: no,
        k: value,
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
struct LinuxProbeSeccomp {
    filter: Vec<libc::sock_filter>,
    filter_len: u16,
}

#[cfg(target_os = "linux")]
impl LinuxProbeSeccomp {
    fn install(&self) -> std::io::Result<()> {
        let program = libc::sock_fprog {
            len: self.filter_len,
            filter: self.filter.as_ptr().cast_mut(),
        };
        if unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0,
                &raw const program,
            )
        } == -1
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn install_listener(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;

        let program = libc::sock_fprog {
            len: self.filter_len,
            filter: self.filter.as_ptr().cast_mut(),
        };
        let listener = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                SECCOMP_FILTER_FLAG_NEW_LISTENER,
                &raw const program,
            )
        };
        if listener == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            let listener = libc::c_int::try_from(listener)
                .map_err(|_| std::io::Error::from_raw_os_error(libc::EOVERFLOW))?;
            Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(listener) })
        }
    }
}

/// Keep FD 4 permanently bound to the sealed parser, deny process-group escape,
/// and route all descriptor execution to a one-shot supervisor.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn build_linux_probe_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    const LOAD_WORD_ABSOLUTE: u16 = 0x20;
    const JUMP_EQUAL: u16 = 0x15;
    const JUMP_GREATER_THAN: u16 = 0x25;
    const JUMP_GREATER_OR_EQUAL: u16 = 0x35;
    const RETURN_CONSTANT: u16 = 0x06;
    const DENIED: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let mut filter = vec![
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARCH_OFFSET),
        seccomp_jump(JUMP_EQUAL, LINUX_AUDIT_ARCH, 1, 0),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_KILL_PROCESS),
        seccomp_statement(LOAD_WORD_ABSOLUTE, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    filter.extend([
        // x32 shares AUDIT_ARCH_X86_64 but offsets its syscall table. Refuse
        // that alternate ABI so native-number checks cannot be bypassed.
        seccomp_jump(0x45, 0x4000_0000, 0, 1),
        seccomp_statement(RETURN_CONSTANT, DENIED),
    ]);
    for syscall in [
        libc::SYS_execve,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
        libc::SYS_sendmmsg,
        libc::SYS_pidfd_getfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_setsid,
        libc::SYS_setpgid,
    ] {
        filter.push(seccomp_jump(JUMP_EQUAL, syscall as u32, 0, 1));
        filter.push(seccomp_statement(RETURN_CONSTANT, DENIED));
    }
    let descriptor_mutations = [
        (libc::SYS_close, 0_u32),
        (libc::SYS_fcntl, 0_u32),
        (libc::SYS_dup3, 1_u32),
        #[cfg(target_arch = "x86_64")]
        (libc::SYS_dup2, 1_u32),
    ];
    for (syscall, argument) in descriptor_mutations {
        filter.push(seccomp_jump(JUMP_EQUAL, syscall as u32, 0, 3));
        filter.push(seccomp_statement(
            LOAD_WORD_ABSOLUTE,
            SECCOMP_DATA_ARGS_OFFSET + argument * 8,
        ));
        filter.push(seccomp_jump(JUMP_EQUAL, HELD_PROBE_FD as u32, 0, 1));
        filter.push(seccomp_statement(RETURN_CONSTANT, DENIED));
        filter.push(seccomp_statement(LOAD_WORD_ABSOLUTE, 0));
    }
    filter.extend([
        seccomp_jump(JUMP_EQUAL, libc::SYS_close_range as u32, 0, 5),
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARGS_OFFSET),
        seccomp_jump(JUMP_GREATER_THAN, HELD_PROBE_FD as u32, 3, 0),
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARGS_OFFSET + 8),
        seccomp_jump(JUMP_GREATER_OR_EQUAL, HELD_PROBE_FD as u32, 0, 1),
        seccomp_statement(RETURN_CONSTANT, DENIED),
        seccomp_statement(LOAD_WORD_ABSOLUTE, 0),
        // A classic BPF filter cannot inspect the pathname or remember that
        // the first exec already happened. Every execveat therefore goes to a
        // one-shot supervisor: it continues only the trusted pre-exec call and
        // returns EPERM for every request made by the installed image or a
        // descendant.
        seccomp_jump(JUMP_EQUAL, libc::SYS_execveat as u32, 0, 1),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_USER_NOTIF),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_ALLOW),
    ]);
    let filter_len = u16::try_from(filter.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe seccomp program exceeded the kernel length field",
        )
    })?;
    Ok(LinuxProbeSeccomp { filter, filter_len })
}

/// The bootstrap filter must allow one `sendmsg` so the child can transfer
/// its listener. Stack this filter immediately afterwards; the installed
/// parser and every descendant then fail closed if they try to export a held
/// descriptor with SCM_RIGHTS.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn build_linux_probe_post_transfer_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    const LOAD_WORD_ABSOLUTE: u16 = 0x20;
    const JUMP_EQUAL: u16 = 0x15;
    const RETURN_CONSTANT: u16 = 0x06;
    const DENIED: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let mut filter = vec![
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARCH_OFFSET),
        seccomp_jump(JUMP_EQUAL, LINUX_AUDIT_ARCH, 1, 0),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_KILL_PROCESS),
        seccomp_statement(LOAD_WORD_ABSOLUTE, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    filter.extend([
        seccomp_jump(0x45, 0x4000_0000, 0, 1),
        seccomp_statement(RETURN_CONSTANT, DENIED),
    ]);
    for syscall in [libc::SYS_sendmsg, libc::SYS_sendmmsg] {
        filter.push(seccomp_jump(JUMP_EQUAL, syscall as u32, 0, 1));
        filter.push(seccomp_statement(RETURN_CONSTANT, DENIED));
    }
    filter.push(seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_ALLOW));
    let filter_len = u16::try_from(filter.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "post-transfer seccomp program exceeded the kernel length field",
        )
    })?;
    Ok(LinuxProbeSeccomp { filter, filter_len })
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn build_linux_probe_post_transfer_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "qualified FFprobe post-transfer seccomp is unsupported on this architecture",
    ))
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn build_linux_probe_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "qualified FFprobe seccomp is unsupported on this architecture",
    ))
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Default)]
struct LinuxSeccompData {
    nr: libc::c_int,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Default)]
struct LinuxSeccompNotification {
    id: u64,
    pid: u32,
    flags: u32,
    data: LinuxSeccompData,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Default)]
struct LinuxSeccompResponse {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

/// Per-launch EINTR injection stays on the stack so parallel tests cannot
/// interfere with one another. Production always constructs the zero value.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
struct LinuxBootstrapInterrupts {
    ready_send: u8,
    acknowledgement_send: u8,
    first_notification_receive: u8,
    first_notification_response: u8,
    steady_notification_response: u8,
    /// Test-only launch modes attach a mode-local observation counter so the
    /// regression cannot pass merely because some earlier operation stalled.
    steady_notification_response_interrupts: Option<&'static std::sync::atomic::AtomicUsize>,
    /// Distinct exit observations make deadline and stop mutations fail their
    /// corresponding regressions even if outer task cleanup eventually stops
    /// the same supervisor thread.
    steady_notification_response_deadline_exits: Option<&'static std::sync::atomic::AtomicUsize>,
    steady_notification_response_stop_exits: Option<&'static std::sync::atomic::AtomicUsize>,
    /// Test-only stop-path injection. Production constructs `false` through
    /// `Default`, so only the explicit regression can request this signal.
    stop_after_steady_notification_response_interrupt: bool,
    invalidate_first_notification_on_interrupt: bool,
    first_notification_invalidated: bool,
}

#[cfg(target_os = "linux")]
fn consume_linux_bootstrap_interrupt(remaining: &mut u8) -> bool {
    if *remaining == 0 {
        false
    } else {
        // `u8::MAX` is the test-only persistent mode. It exercises expiry of
        // the shared absolute launch deadline without process-global signals
        // or errno mutation.
        if *remaining != u8::MAX {
            *remaining -= 1;
        }
        true
    }
}

#[cfg(target_os = "linux")]
fn ensure_linux_probe_deadline(deadline: Option<std::time::Instant>) -> std::io::Result<()> {
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const fn linux_ioctl_read_write<T>(number: u8) -> libc::c_ulong {
    const IOC_WRITE: libc::c_ulong = 1;
    const IOC_READ: libc::c_ulong = 2;
    const IOC_DIRECTION_SHIFT: libc::c_ulong = 30;
    const IOC_SIZE_SHIFT: libc::c_ulong = 16;
    const IOC_TYPE_SHIFT: libc::c_ulong = 8;
    ((IOC_READ | IOC_WRITE) << IOC_DIRECTION_SHIFT)
        | ((std::mem::size_of::<T>() as libc::c_ulong) << IOC_SIZE_SHIFT)
        | ((b'!' as libc::c_ulong) << IOC_TYPE_SHIFT)
        | number as libc::c_ulong
}

#[cfg(target_os = "linux")]
fn receive_linux_seccomp_notification(
    listener: std::os::fd::RawFd,
    injected_interrupts: &mut u8,
) -> std::io::Result<LinuxSeccompNotification> {
    let mut notification = LinuxSeccompNotification::default();
    let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
    let received = if injected {
        -1
    } else {
        unsafe {
            libc::ioctl(
                listener,
                linux_ioctl_read_write::<LinuxSeccompNotification>(0),
                &raw mut notification,
            )
        }
    };
    if received == 0 {
        Ok(notification)
    } else if injected {
        // A just-consumed test injection represents EINTR without mutating
        // process-global errno.
        Err(std::io::Error::from_raw_os_error(libc::EINTR))
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn answer_linux_seccomp_notification_until(
    listener: std::os::fd::RawFd,
    response: &mut LinuxSeccompResponse,
    deadline: Option<std::time::Instant>,
    stop: Option<&AtomicBool>,
    injected_interrupts: &mut u8,
    injected_interrupt_observer: Option<&std::sync::atomic::AtomicUsize>,
    deadline_exit_observer: Option<&std::sync::atomic::AtomicUsize>,
    stop_exit_observer: Option<&std::sync::atomic::AtomicUsize>,
    stop_after_injected_interrupt: bool,
) -> std::io::Result<()> {
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            if let Some(observer) = stop_exit_observer {
                observer.fetch_add(1, Ordering::Release);
            }
            return Ok(());
        }
        if let Err(error) = ensure_linux_probe_deadline(deadline) {
            if let Some(observer) = deadline_exit_observer {
                observer.fetch_add(1, Ordering::Release);
            }
            return Err(error);
        }
        let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
        if injected {
            if let Some(observer) = injected_interrupt_observer {
                observer.fetch_add(1, Ordering::Release);
            }
            if stop_after_injected_interrupt {
                if let Some(stop) = stop {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        let answered = if injected {
            -1
        } else {
            unsafe {
                libc::ioctl(
                    listener,
                    linux_ioctl_read_write::<LinuxSeccompResponse>(1),
                    response as *mut LinuxSeccompResponse,
                )
            }
        };
        if answered == 0 {
            return Ok(());
        }
        let error = if injected {
            std::io::Error::from_raw_os_error(libc::EINTR)
        } else {
            std::io::Error::last_os_error()
        };
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn send_linux_seccomp_listener(
    socket: std::os::fd::RawFd,
    listener: std::os::fd::RawFd,
) -> std::io::Result<()> {
    let mut payload = [0_u8; 1];
    let mut io = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    // CMSG_* may dereference a cmsghdr at this address, so the backing storage
    // must have native alignment rather than the byte alignment of `[u8; N]`.
    let mut control = [0 as libc::c_ulong; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut io;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
    if header.is_null() {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    unsafe {
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>(), listener);
        message.msg_controllen = (*header).cmsg_len;
    }
    if unsafe {
        libc::sendmsg(
            socket,
            &raw const message,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        )
    } == 1
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn receive_linux_seccomp_listener(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;

    let mut payload = [0_u8; 1];
    let mut io = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    // Match the native cmsghdr alignment required by CMSG_FIRSTHDR/CMSG_DATA.
    let mut control = [0 as libc::c_ulong; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut io;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    let received = loop {
        let received = unsafe { libc::recvmsg(socket, &raw mut message, libc::MSG_CMSG_CLOEXEC) };
        if received != -1 {
            break received;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
        wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    };
    if received != 1 {
        return Err(if received == -1 {
            std::io::Error::last_os_error()
        } else {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "probe exited before transferring its seccomp listener",
            )
        });
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe seccomp listener control message was truncated",
        ));
    }
    let header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
    if header.is_null()
        || unsafe { (*header).cmsg_level } != libc::SOL_SOCKET
        || unsafe { (*header).cmsg_type } != libc::SCM_RIGHTS
        || unsafe { (*header).cmsg_len }
            < unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize }
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe transferred an invalid seccomp listener",
        ));
    }
    let listener =
        unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>()) };
    if listener < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe transferred a negative seccomp listener",
        ));
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(listener) })
}

#[cfg(target_os = "linux")]
fn wait_for_linux_fd(
    fd: std::os::fd::RawFd,
    events: libc::c_short,
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        let timeout_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, timeout_ms) };
        if ready > 0 && descriptor.revents & events != 0 {
            return Ok(());
        }
        if ready == 0 {
            return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
    }
}

#[cfg(target_os = "linux")]
fn receive_linux_seccomp_notification_before(
    listener: std::os::fd::RawFd,
    deadline: std::time::Instant,
    interrupts: &mut LinuxBootstrapInterrupts,
) -> std::io::Result<LinuxSeccompNotification> {
    loop {
        wait_for_linux_fd(listener, libc::POLLIN, deadline)?;
        let received = if interrupts.first_notification_invalidated {
            Err(std::io::Error::from_raw_os_error(libc::ENOENT))
        } else {
            receive_linux_seccomp_notification(listener, &mut interrupts.first_notification_receive)
        };
        match received {
            Ok(notification) => return Ok(notification),
            Err(error)
                if error.kind() == std::io::ErrorKind::Interrupted
                    && interrupts.invalidate_first_notification_on_interrupt =>
            {
                // Model the kernel race where target interruption invalidates
                // the ready notification. Every retry must return to poll and
                // the absolute deadline rather than blocking in RECV.
                interrupts.first_notification_invalidated = true;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::Interrupted
                    || matches!(
                        error.raw_os_error(),
                        Some(libc::ENOENT) | Some(libc::EAGAIN)
                    ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxProbeReceiver {
    descriptor: std::sync::Mutex<Option<std::os::fd::OwnedFd>>,
}

#[cfg(target_os = "linux")]
impl LinuxProbeReceiver {
    fn raw_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        self.descriptor
            .lock()
            .map_err(|_| std::io::Error::other("probe receiver owner was poisoned"))?
            .as_ref()
            .map(std::os::fd::AsRawFd::as_raw_fd)
            .ok_or_else(|| std::io::Error::other("probe receiver was already closed"))
    }

    fn shutdown(&self) {
        if let Ok(descriptor) = self.descriptor.lock() {
            if let Some(descriptor) = descriptor.as_ref() {
                use std::os::fd::AsRawFd;
                unsafe {
                    libc::shutdown(descriptor.as_raw_fd(), libc::SHUT_RDWR);
                }
            }
        }
    }

    fn close(&self) {
        self.shutdown();
        if let Ok(mut descriptor) = self.descriptor.lock() {
            descriptor.take();
        }
    }
}

#[cfg(target_os = "linux")]
fn receive_linux_probe_fork_ready(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    let mut byte = 0_u8;
    loop {
        let received = unsafe { libc::recv(socket, (&raw mut byte).cast(), 1, 0) };
        if received == 1 && byte == b'F' {
            return Ok(());
        }
        if received == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
                continue;
            }
            return Err(error);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "probe child did not confirm its post-fork descriptor table",
        ));
    }
}

#[cfg(target_os = "linux")]
fn acknowledge_linux_probe_fork(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
    injected_interrupts: &mut u8,
) -> std::io::Result<()> {
    let byte = b'A';
    loop {
        ensure_linux_probe_deadline(Some(deadline))?;
        let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
        if !injected
            && unsafe {
                libc::send(
                    socket,
                    (&raw const byte).cast(),
                    1,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            } == 1
        {
            return Ok(());
        }
        let error = if injected {
            std::io::Error::from_raw_os_error(libc::EINTR)
        } else {
            std::io::Error::last_os_error()
        };
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn confirm_linux_probe_fork(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
    injected_interrupts: &mut u8,
) -> std::io::Result<()> {
    let byte = b'F';
    loop {
        ensure_linux_probe_deadline(Some(deadline))?;
        let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
        if !injected
            && unsafe {
                libc::send(
                    socket,
                    (&raw const byte).cast(),
                    1,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            } == 1
        {
            break;
        }
        let error = if injected {
            std::io::Error::from_raw_os_error(libc::EINTR)
        } else {
            std::io::Error::last_os_error()
        };
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    let mut acknowledgement = 0_u8;
    loop {
        let received = unsafe { libc::recv(socket, (&raw mut acknowledgement).cast(), 1, 0) };
        if received == 1 && acknowledgement == b'A' {
            return Ok(());
        }
        if received == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
                continue;
            }
            return Err(error);
        }
        return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
    }
}

#[cfg(target_os = "linux")]
fn supervise_linux_probe_execs(
    receiver: Arc<LinuxProbeReceiver>,
    stop: Arc<AtomicBool>,
    launch_mode: ProbeLaunchMode,
    launch_deadline: std::time::Instant,
) -> std::io::Result<()> {
    let mut interrupts = launch_mode.bootstrap_interrupts();

    struct ReceiverCleanup {
        receiver: Arc<LinuxProbeReceiver>,
        fork_confirmed: bool,
    }
    impl Drop for ReceiverCleanup {
        fn drop(&mut self) {
            if self.fork_confirmed {
                self.receiver.close();
            } else {
                // Until the child confirms fork, keep the descriptor number
                // occupied while shutdown unblocks any eventual handshake.
                self.receiver.shutdown();
            }
        }
    }
    let socket = receiver.raw_fd()?;
    let mut receiver_cleanup = ReceiverCleanup {
        receiver: Arc::clone(&receiver),
        fork_confirmed: false,
    };
    receive_linux_probe_fork_ready(socket, launch_deadline)?;
    receiver_cleanup.fork_confirmed = true;
    #[cfg(test)]
    if launch_mode.inject_supervisor_delay() {
        std::thread::sleep(Duration::from_millis(250));
    }
    #[cfg(test)]
    if launch_mode.inject_supervisor_receive_failure() {
        return Err(std::io::Error::other(
            "injected seccomp listener receive failure",
        ));
    }
    if std::time::Instant::now() >= launch_deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "probe supervisor launch handshake timed out",
        ));
    }
    acknowledge_linux_probe_fork(
        socket,
        launch_deadline,
        &mut interrupts.acknowledgement_send,
    )?;
    let listener = receive_linux_seccomp_listener(socket, launch_deadline)?;
    receiver.close();
    use std::os::fd::AsRawFd;
    let first = receive_linux_seccomp_notification_before(
        listener.as_raw_fd(),
        launch_deadline,
        &mut interrupts,
    )?;
    if first.flags != 0
        || first.data.arch != LINUX_AUDIT_ARCH
        || first.data.nr != libc::SYS_execveat as libc::c_int
        || first.data.args[0] != HELD_PROBE_FD as u64
        || first.data.args[4] != libc::AT_EMPTY_PATH as u64
    {
        let mut denied = LinuxSeccompResponse {
            id: first.id,
            error: -libc::EPERM,
            ..LinuxSeccompResponse::default()
        };
        let _ = answer_linux_seccomp_notification_until(
            listener.as_raw_fd(),
            &mut denied,
            Some(launch_deadline),
            None,
            &mut interrupts.first_notification_response,
            None,
            None,
            None,
            false,
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "first supervised probe exec did not match the bound descriptor call",
        ));
    }
    // The first notification can only originate in the single-threaded
    // post-fork pre-exec closure: parser code has not run yet and cannot race
    // its arguments. Continuing it is therefore the one safe use of the
    // notification API; every request after the image starts is denied.
    let mut allowed = LinuxSeccompResponse {
        id: first.id,
        flags: SECCOMP_USER_NOTIF_FLAG_CONTINUE,
        ..LinuxSeccompResponse::default()
    };
    answer_linux_seccomp_notification_until(
        listener.as_raw_fd(),
        &mut allowed,
        Some(launch_deadline),
        None,
        &mut interrupts.first_notification_response,
        None,
        None,
        None,
        false,
    )?;

    while !stop.load(Ordering::Acquire) {
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, 25) };
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        if descriptor.revents & libc::POLLIN == 0 {
            // HUP after the last filtered task exits is the normal terminal
            // event. Closing this listener also leaves every racing notified
            // exec failed closed by the kernel.
            break;
        }
        let mut no_interrupts = 0;
        let request =
            match receive_linux_seccomp_notification(listener.as_raw_fd(), &mut no_interrupts) {
                Ok(request) => request,
                Err(error)
                    if error.kind() == std::io::ErrorKind::Interrupted
                        || matches!(
                            error.raw_os_error(),
                            Some(libc::ENOENT) | Some(libc::EAGAIN)
                        ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
        let mut denied = LinuxSeccompResponse {
            id: request.id,
            error: -libc::EPERM,
            ..LinuxSeccompResponse::default()
        };
        match answer_linux_seccomp_notification_until(
            listener.as_raw_fd(),
            &mut denied,
            Some(launch_deadline),
            Some(stop.as_ref()),
            &mut interrupts.steady_notification_response,
            interrupts.steady_notification_response_interrupts,
            interrupts.steady_notification_response_deadline_exits,
            interrupts.steady_notification_response_stop_exits,
            interrupts.stop_after_steady_notification_response_interrupt,
        ) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct LinuxProbeExecSupervisor {
    child_sender: Option<std::os::fd::OwnedFd>,
    receiver: Option<Arc<LinuxProbeReceiver>>,
    child_sender_fd: std::os::fd::RawFd,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    #[cfg(test)]
    test_counter: Option<&'static std::sync::atomic::AtomicUsize>,
}

#[cfg(target_os = "linux")]
impl LinuxProbeExecSupervisor {
    fn start(
        launch_mode: ProbeLaunchMode,
        launch_deadline: std::time::Instant,
    ) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};

        let mut sockets = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                sockets.as_mut_ptr(),
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        let receiver_fd = unsafe { libc::fcntl(sockets[0], libc::F_DUPFD_CLOEXEC, 10) };
        let sender_fd = unsafe { libc::fcntl(sockets[1], libc::F_DUPFD_CLOEXEC, 10) };
        unsafe {
            libc::close(sockets[0]);
            libc::close(sockets[1]);
        }
        if receiver_fd == -1 || sender_fd == -1 {
            if receiver_fd != -1 {
                unsafe { libc::close(receiver_fd) };
            }
            if sender_fd != -1 {
                unsafe { libc::close(sender_fd) };
            }
            return Err(std::io::Error::last_os_error());
        }
        let receiver = Arc::new(LinuxProbeReceiver {
            descriptor: std::sync::Mutex::new(Some(unsafe {
                std::os::fd::OwnedFd::from_raw_fd(receiver_fd)
            })),
        });
        let child_sender = unsafe { std::os::fd::OwnedFd::from_raw_fd(sender_fd) };
        let child_sender_fd = child_sender.as_raw_fd();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_receiver = Arc::clone(&receiver);
        let worker = std::thread::Builder::new()
            .name("plurx-probe-exec".to_owned())
            .spawn(move || {
                supervise_linux_probe_execs(
                    worker_receiver,
                    worker_stop,
                    launch_mode,
                    launch_deadline,
                )
            })?;
        #[cfg(test)]
        let test_counter = launch_mode.supervisor_test_counter();
        #[cfg(test)]
        if let Some(counter) = test_counter {
            counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        Ok(Self {
            child_sender: Some(child_sender),
            receiver: Some(receiver),
            child_sender_fd,
            stop,
            worker: Some(worker),
            #[cfg(test)]
            test_counter,
        })
    }

    fn child_sender_fd(&self) -> std::os::fd::RawFd {
        self.child_sender_fd
    }

    fn child_receiver_fd(&self) -> std::os::fd::RawFd {
        self.receiver
            .as_ref()
            .expect("receiver exists through spawn")
            .raw_fd()
            .expect("receiver descriptor exists through fork")
    }

    fn parent_after_spawn(&mut self) {
        self.child_sender.take();
        self.receiver.take();
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.parent_after_spawn();
        self.stop.store(true, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| std::io::Error::other("probe seccomp supervisor panicked"))?
    }
}

#[cfg(target_os = "linux")]
impl Drop for LinuxProbeExecSupervisor {
    fn drop(&mut self) {
        self.parent_after_spawn();
        self.stop.store(true, Ordering::Release);
        #[cfg(test)]
        if let Some(counter) = self.test_counter.take() {
            counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
static PIDFD_OPEN_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static PIDFD_READ_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static DELAYED_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static FAILED_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static INTERRUPTED_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_INTERRUPT_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_DEADLINE_EXITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_EXITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(unix)]
struct ProbeExecutionSupervisor {
    #[cfg(target_os = "linux")]
    linux: Option<LinuxProbeExecSupervisor>,
}

#[cfg(unix)]
impl ProbeExecutionSupervisor {
    fn none() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            linux: None,
        }
    }

    #[cfg(target_os = "linux")]
    fn linux(supervisor: LinuxProbeExecSupervisor) -> Self {
        Self {
            linux: Some(supervisor),
        }
    }

    fn parent_after_spawn(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = self.linux.as_mut() {
            supervisor.parent_after_spawn();
        }
    }

    fn finish(&mut self) -> Result<(), DecodeFactError> {
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = self.linux.as_mut() {
            supervisor
                .finish()
                .map_err(|error| DecodeFactError::Read(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(unix)]
async fn spawn_configured_probe(
    mut command: tokio::process::Command,
    mut supervisor: ProbeExecutionSupervisor,
) -> Result<(tokio::process::Child, ProbeExecutionSupervisor), DecodeFactError> {
    tokio::task::spawn_blocking(move || {
        let spawned = command.spawn();
        supervisor.parent_after_spawn();
        match spawned {
            Ok(child) => Ok((child, supervisor)),
            Err(error) => {
                let _ = supervisor.finish();
                Err(DecodeFactError::Spawn(error.to_string()))
            }
        }
    })
    .await
    .map_err(|error| DecodeFactError::Spawn(error.to_string()))?
}

#[cfg(target_os = "linux")]
fn mark_unrelated_fds_close_on_exec() -> std::io::Result<()> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            HELD_PROBE_FD + 1,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
struct LinuxExecveArguments {
    _argv: Vec<CString>,
    argv_pointers: Vec<usize>,
    _environment: Vec<CString>,
    environment_pointers: Vec<usize>,
}

#[cfg(target_os = "linux")]
impl LinuxExecveArguments {
    fn new(arg0: &OsStr, arguments: &[OsString]) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;

        let mut argv = Vec::with_capacity(arguments.len() + 1);
        argv.push(CString::new(arg0.as_bytes())?);
        for argument in arguments {
            argv.push(CString::new(argument.as_os_str().as_bytes())?);
        }
        let environment = std::env::vars_os()
            .map(|(key, value)| {
                let mut entry = key.as_os_str().as_bytes().to_vec();
                entry.push(b'=');
                entry.extend_from_slice(value.as_os_str().as_bytes());
                CString::new(entry)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut argv_pointers = argv
            .iter()
            .map(|argument| argument.as_ptr() as usize)
            .collect::<Vec<_>>();
        argv_pointers.push(0);
        let mut environment_pointers = environment
            .iter()
            .map(|entry| entry.as_ptr() as usize)
            .collect::<Vec<_>>();
        environment_pointers.push(0);
        Ok(Self {
            _argv: argv,
            argv_pointers,
            _environment: environment,
            environment_pointers,
        })
    }

    fn execute_held_probe(&self) -> std::io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_execveat,
                HELD_PROBE_FD,
                c"".as_ptr(),
                self.argv_pointers.as_ptr().cast::<*const libc::c_char>(),
                self.environment_pointers
                    .as_ptr()
                    .cast::<*const libc::c_char>(),
                libc::AT_EMPTY_PATH,
            )
        };
        let _ = result;
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn configure_probe_execution(
    command: &mut tokio::process::Command,
    launch_mode: ProbeLaunchMode,
    launch_deadline: std::time::Instant,
    executable_fd: std::os::fd::RawFd,
    source_fd: Option<std::os::fd::RawFd>,
    arg0: &Path,
    arguments: &[OsString],
) -> Result<ProbeExecutionSupervisor, DecodeFactError> {
    #[cfg(not(target_os = "linux"))]
    let _ = launch_deadline;
    #[cfg(target_os = "linux")]
    let production_arguments = if launch_mode.is_production() {
        Some(
            LinuxExecveArguments::new(arg0.as_os_str(), arguments)
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let production_seccomp = if launch_mode.is_production() {
        Some(
            build_linux_probe_seccomp()
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let production_post_transfer_seccomp = if launch_mode.is_production() {
        Some(
            build_linux_probe_post_transfer_seccomp()
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let _ = (arg0, arguments);
    #[cfg(target_os = "linux")]
    let production_supervisor = if launch_mode.is_production() {
        Some(
            LinuxProbeExecSupervisor::start(launch_mode, launch_deadline)
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let supervisor_sender_fd = production_supervisor
        .as_ref()
        .map(LinuxProbeExecSupervisor::child_sender_fd);
    #[cfg(target_os = "linux")]
    let supervisor_receiver_fd = production_supervisor
        .as_ref()
        .map(LinuxProbeExecSupervisor::child_receiver_fd);
    unsafe {
        command.pre_exec(move || {
            start_probe_session()?;
            match source_fd {
                Some(source_fd) => install_probe_child_fds(source_fd, executable_fd)?,
                None => install_child_fd(executable_fd, HELD_PROBE_FD)?,
            }
            if !launch_mode.is_production() {
                return Ok(());
            }
            #[cfg(target_os = "linux")]
            {
                let mut interrupts = launch_mode.bootstrap_interrupts();
                let Some(receiver_fd) = supervisor_receiver_fd else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                let Some(sender_fd) = supervisor_sender_fd else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                confirm_linux_probe_fork(sender_fd, launch_deadline, &mut interrupts.ready_send)?;
                libc::close(receiver_fd);
                mark_unrelated_fds_close_on_exec()?;
                #[cfg(test)]
                if let Some(export_socket) = launch_mode.export_socket_fd() {
                    install_child_fd(export_socket, 5)?;
                }
                install_linux_filesystem_execute_denial()?;
                let Some(seccomp) = production_seccomp.as_ref() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                let Some(arguments) = production_arguments.as_ref() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                let listener = seccomp.install_listener()?;
                use std::os::fd::AsRawFd;
                send_linux_seccomp_listener(sender_fd, listener.as_raw_fd())?;
                let Some(post_transfer_seccomp) = production_post_transfer_seccomp.as_ref() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                post_transfer_seccomp.install()?;
                drop(listener);
                arguments.execute_held_probe()
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
            }
        });
    }
    #[cfg(target_os = "linux")]
    if let Some(supervisor) = production_supervisor {
        return Ok(ProbeExecutionSupervisor::linux(supervisor));
    }
    Ok(ProbeExecutionSupervisor::none())
}

#[cfg(unix)]
fn start_probe_session() -> std::io::Result<()> {
    if unsafe { libc::setsid() } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
async fn terminate_probe_session(
    child: &mut tokio::process::Child,
    session: &ProbeSessionGuard,
    supervisor: &mut ProbeExecutionSupervisor,
    _launch_mode: ProbeLaunchMode,
) {
    // This potentially unbounded reap runs only inside an owned task. The
    // caller-facing deadline detaches that task while it retains every source
    // and admission guard needed for safe cleanup.
    session.kill_once();
    let _ = wait_for_probe_reap(child, _launch_mode).await;
    let _ = supervisor.finish();
}

#[cfg(unix)]
async fn wait_for_probe_reap(
    child: &mut tokio::process::Child,
    _launch_mode: ProbeLaunchMode,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(all(test, target_os = "linux"))]
    if _launch_mode.inject_slow_reap() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    child.wait().await
}

#[cfg(unix)]
fn kill_probe_session(process_group: Option<libc::pid_t>) {
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(target_os = "linux")]
struct ProbeExitAnchor {
    pidfd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    #[cfg(test)]
    inject_read_failure: bool,
}

#[cfg(target_os = "linux")]
impl ProbeExitAnchor {
    fn open(
        process_group: Option<libc::pid_t>,
        launch_mode: ProbeLaunchMode,
    ) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;

        if launch_mode.inject_pidfd_open_failure() {
            return Err(std::io::Error::other("injected FFprobe pidfd open failure"));
        }
        let pid = process_group.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "spawned FFprobe has no process identifier",
            )
        })?;
        let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw_fd == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let raw_fd = libc::c_int::try_from(raw_fd).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "FFprobe pidfd exceeded the descriptor range",
            )
        })?;
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };
        Ok(Self {
            pidfd: tokio::io::unix::AsyncFd::new(owned)?,
            #[cfg(test)]
            inject_read_failure: launch_mode.inject_pidfd_read_failure(),
        })
    }

    async fn wait_until_exit(&self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.inject_read_failure {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Err(std::io::Error::other(
                "injected FFprobe pidfd readiness failure",
            ));
        }
        let _readiness = self.pidfd.readable().await?;
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct ProbeSessionTerminator {
    process_group: Arc<std::sync::Mutex<Option<libc::pid_t>>>,
}

#[cfg(unix)]
impl ProbeSessionTerminator {
    fn take_process_group(&self) -> Option<libc::pid_t> {
        match self.process_group.lock() {
            Ok(mut process_group) => process_group.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    fn kill_once(&self) {
        kill_probe_session(self.take_process_group());
    }
}

#[cfg(unix)]
struct ProbeSessionGuard {
    terminator: ProbeSessionTerminator,
}

#[cfg(unix)]
impl ProbeSessionGuard {
    fn new(process_group: Option<libc::pid_t>) -> Self {
        Self {
            terminator: ProbeSessionTerminator {
                process_group: Arc::new(std::sync::Mutex::new(process_group)),
            },
        }
    }

    #[cfg(test)]
    fn terminator(&self) -> ProbeSessionTerminator {
        self.terminator.clone()
    }

    fn kill_once(&self) {
        self.terminator.kill_once();
    }
}

#[cfg(unix)]
impl Drop for ProbeSessionGuard {
    fn drop(&mut self) {
        self.kill_once();
    }
}

#[cfg(unix)]
fn copy_executable(
    source: &std::fs::File,
    snapshot: &mut std::fs::File,
) -> Result<(), DecodeFactError> {
    use std::os::unix::fs::FileExt;

    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = source
            .read_at(&mut buffer, offset)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        offset = offset
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable size overflow".to_owned()))?;
        if offset > MAX_PROBE_EXECUTABLE_BYTES {
            return Err(DecodeFactError::ProbeIdentity(
                "executable exceeded identity limit".to_owned(),
            ));
        }
        snapshot
            .write_all(&buffer[..read])
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    }
    snapshot
        .flush()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn snapshot_executable(source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;

    let raw_fd = unsafe {
        libc::memfd_create(
            c"plurx-ffprobe".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw_fd == -1 {
        return Err(DecodeFactError::ProbeIdentity(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(raw_fd) };
    copy_executable(source, &mut file)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o500))
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } == -1 {
        return Err(DecodeFactError::ProbeIdentity(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(ExecutableSnapshot { file })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn snapshot_executable(source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    use std::os::unix::fs::PermissionsExt;

    let mut snapshot = tempfile::Builder::new()
        .prefix("plurx-ffprobe-")
        .tempfile()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    copy_executable(source, snapshot.as_file_mut())?;
    snapshot
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;

        if unsafe { libc::fchflags(snapshot.as_file().as_raw_fd(), libc::UF_IMMUTABLE) } == -1 {
            return Err(DecodeFactError::ProbeIdentity(
                std::io::Error::last_os_error().to_string(),
            ));
        }
    }
    let path = snapshot.into_temp_path();
    let file = std::fs::File::open(&path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    Ok(ExecutableSnapshot { file, _path: path })
}

#[cfg(not(unix))]
fn snapshot_executable(_source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn snapshot_execution_path(_snapshot: &ExecutableSnapshot) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{HELD_PROBE_FD}"))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn snapshot_execution_path(snapshot: &ExecutableSnapshot) -> PathBuf {
    snapshot.path().to_owned()
}

#[cfg(unix)]
fn probe_file_identity(path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    let file = std::fs::File::open(path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    probe_file_identity_from_file(&file)
}

#[cfg(unix)]
fn probe_file_identity_from_file(
    file: &std::fs::File,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    use std::os::unix::fs::FileExt;
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PROBE_EXECUTABLE_BYTES {
        return Err(DecodeFactError::ProbeIdentity(
            "executable size is outside the bounded identity envelope".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = file
            .read_at(&mut buffer, offset)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        offset = offset
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable size overflow".to_owned()))?;
        if offset > MAX_PROBE_EXECUTABLE_BYTES {
            return Err(DecodeFactError::ProbeIdentity(
                "executable exceeded identity limit".to_owned(),
            ));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ProbeFileIdentity {
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        device: metadata.dev(),
        inode: metadata.ino(),
        content_digest: hex::encode(hasher.finalize()),
    })
}

async fn run_bounded_identity_task<T, F>(
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: F,
) -> Result<T, DecodeFactError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DecodeFactError> + Send + 'static,
{
    run_bounded_identity_task_on(identity_gate(), budget, cancelled, work).await
}

async fn run_bounded_identity_task_on<T, F>(
    gate: Arc<tokio::sync::Semaphore>,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: F,
) -> Result<T, DecodeFactError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DecodeFactError> + Send + 'static,
{
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let started = std::time::Instant::now();
    let permit = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        permit = tokio::time::timeout(budget, gate.acquire_owned()) => {
            permit
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|_| DecodeFactError::CacheInvariant)?
        }
    };
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    // The owned permit lives in the worker. If this waiter is cancelled or
    // times out, the blocking hash finishes in the sole lane before another
    // hash can begin; detached work therefore cannot fan out.
    let mut task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        result = tokio::time::timeout(remaining, &mut task) => {
            result
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?
        }
    }
}

async fn path_probe_file_identity_within(
    path: PathBuf,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    run_bounded_identity_task(budget, cancelled, move || probe_file_identity(&path)).await
}

async fn held_probe_file_identity_within(
    file: Arc<std::fs::File>,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    run_bounded_identity_task(budget, cancelled, move || {
        probe_file_identity_from_file(&file)
    })
    .await
}

#[cfg(not(unix))]
fn probe_file_identity(_path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(not(unix))]
fn probe_file_identity_from_file(
    _file: &std::fs::File,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(unix)]
async fn probe_version(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    launch_mode: ProbeLaunchMode,
) -> Result<Vec<u8>, DecodeFactError> {
    probe_version_with_deadline(executable, configured_path, launch_mode, VERSION_DEADLINE).await
}

#[cfg(unix)]
async fn probe_version_with_deadline(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    launch_mode: ProbeLaunchMode,
    deadline: Duration,
) -> Result<Vec<u8>, DecodeFactError> {
    probe_version_with_deadline_on(
        executable,
        configured_path,
        launch_mode,
        deadline,
        version_gate(),
    )
    .await
}

#[cfg(unix)]
async fn probe_version_with_deadline_on(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    launch_mode: ProbeLaunchMode,
    deadline: Duration,
    gate: Arc<tokio::sync::Semaphore>,
) -> Result<Vec<u8>, DecodeFactError> {
    let started = std::time::Instant::now();
    let launch_deadline = started + deadline;
    let version_permit = tokio::time::timeout(deadline, gate.acquire_owned())
        .await
        .map_err(|_| DecodeFactError::Deadline)?
        .map_err(|_| DecodeFactError::CacheInvariant)?;
    let remaining = deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let executable = Arc::clone(executable);
    let configured_path = configured_path.to_owned();
    let mut task = tokio::spawn(async move {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let _version_permit = version_permit;
        let executable_fd = executable.as_file().as_raw_fd();
        let arguments = vec![OsString::from("-version")];
        let mut command = tokio::process::Command::new(snapshot_execution_path(&executable));
        command.as_std_mut().arg0(&configured_path);
        command
            .args(&arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let supervisor = configure_probe_execution(
            &mut command,
            launch_mode,
            launch_deadline,
            executable_fd,
            None,
            &configured_path,
            &arguments,
        )?;
        let (mut child, mut supervisor) = spawn_configured_probe(command, supervisor).await?;
        let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
        let session = ProbeSessionGuard::new(process_group);
        #[cfg(target_os = "linux")]
        let exit_anchor = match ProbeExitAnchor::open(process_group, launch_mode) {
            Ok(anchor) => anchor,
            Err(error) => {
                terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
                return Err(DecodeFactError::Spawn(format!(
                    "opening FFprobe exit anchor: {error}"
                )));
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
                return Err(DecodeFactError::MissingPipe);
            }
        };
        let remaining = launch_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
            return Err(DecodeFactError::Deadline);
        }
        let outcome = tokio::time::timeout(remaining, async {
            #[cfg(target_os = "linux")]
            let (stdout, exited) = tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), async {
                let exited = exit_anchor.wait_until_exit().await;
                // Kill descendants as soon as the leader exits so inherited
                // pipe writers cannot hold the drain open.
                session.kill_once();
                exited
            });
            #[cfg(target_os = "linux")]
            let (status, exit_error) = {
                // pidfd readiness proves exit without reaping. The zombie anchors
                // the PID/PGID until descendants are killed and only then is the
                // leader reaped.
                let exit_error = exited
                    .err()
                    .map(|error| DecodeFactError::Read(error.to_string()));
                (
                    wait_for_probe_reap(&mut child, launch_mode).await,
                    exit_error,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let (stdout, status) =
                tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), child.wait());
            #[cfg(not(target_os = "linux"))]
            session.kill_once();
            #[cfg(not(target_os = "linux"))]
            let exit_error: Option<DecodeFactError> = None;
            (stdout, status, exit_error)
        })
        .await;
        let (stdout, status, exit_error) = match outcome {
            Ok(outcome) => outcome,
            Err(_) => {
                terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
                return Err(DecodeFactError::Deadline);
            }
        };
        let supervisor_result = supervisor.finish();
        if let Some(error) = exit_error {
            return Err(error);
        }
        supervisor_result?;
        let stdout = stdout?;
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                return Err(DecodeFactError::Read(error.to_string()));
            }
        };
        if stdout.1 || stdout.0.is_empty() || !status.success() {
            return Err(DecodeFactError::ProbeIdentity(
                "bounded version probe failed".to_owned(),
            ));
        }
        Ok(stdout.0)
    });
    tokio::select! {
        biased;
        result = &mut task => {
            result.map_err(|error| DecodeFactError::Read(error.to_string()))?
        }
        _ = tokio::time::sleep(remaining) => {
            Err(DecodeFactError::Deadline)
        }
    }
}

#[cfg(not(unix))]
async fn probe_version(
    _executable: &Arc<ExecutableSnapshot>,
    _configured_path: &Path,
    _launch_mode: ProbeLaunchMode,
) -> Result<Vec<u8>, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

/// Bounded FIFO cache. The preparation path supplies the exact FFprobe build
/// digest, so replacing the binary cannot reuse facts from an older parser.
pub(crate) struct DecodeFactCache {
    entries: tokio::sync::Mutex<BTreeMap<CacheKey, DecodeFacts>>,
    order: tokio::sync::Mutex<VecDeque<CacheKey>>,
    /// One bounded global lane serializes executable revalidation and cache
    /// misses. Fact preparation cannot fan out unbounded hashing or child
    /// processes when several sessions start together.
    probe_gate: Arc<tokio::sync::Semaphore>,
    capacity: usize,
}

impl DecodeFactCache {
    pub(crate) fn new() -> Self {
        Self::with_capacity(MAX_CACHE_ENTRIES)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: tokio::sync::Mutex::new(BTreeMap::new()),
            order: tokio::sync::Mutex::new(VecDeque::new()),
            probe_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            capacity: capacity.clamp(1, MAX_CACHE_ENTRIES),
        }
    }

    pub(crate) async fn get_or_probe(
        &self,
        probe: &DecodeProbeIdentity,
        source: DecodeFactSource,
        catalog: Option<&DecodeCatalogMetadata>,
        selected_stream: ProbeStreamSelection,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<DecodeFacts, DecodeFactError> {
        let started = std::time::Instant::now();
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let gate = tokio::select! {
            biased;
            _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
            gate = tokio::time::timeout(remaining, Arc::clone(&self.probe_gate).acquire_owned()) => {
                gate
                    .map_err(|_| DecodeFactError::Deadline)?
                    .map_err(|_| DecodeFactError::CacheInvariant)?
            }
        };
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        probe.validate_current(remaining, cancelled).await?;
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        let (bound_source, gate) = source_observation_with_probe_gate(
            Arc::clone(&source.handle),
            gate,
            remaining,
            cancelled,
            source.initial_identity_delay(),
        )
        .await?;
        let bound_identity = bound_source.identity.clone();
        let key = CacheKey {
            source: bound_identity.clone(),
            ffprobe_build_digest: probe.build_digest().to_owned(),
            catalog_digest: catalog.map(|metadata| metadata.digest().to_owned()),
            selected_stream,
        };
        if let Some(facts) = self.entries.lock().await.get(&key).cloned() {
            return Ok(facts);
        }
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let source_offset_permit = tokio::select! {
            biased;
            _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
            permit = tokio::time::timeout(
                remaining,
                Arc::clone(&source.offset_gate).acquire_owned(),
            ) => {
                permit
                    .map_err(|_| DecodeFactError::Deadline)?
                    .map_err(|_| DecodeFactError::CacheInvariant)?
            }
        };
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let owned_probe = probe.clone();
        let owned_catalog = catalog.cloned();
        let owned_cancelled = cancelled.cloned();
        // The task owns both the source descriptor and the singleflight
        // permit. Dropping or aborting this waiter cannot abandon a live child
        // or restore the shared source offset before kill/reap completes.
        let collection = tokio::spawn(async move {
            let result = collect(
                &owned_probe,
                DecodeFactCollectionSource {
                    handle: Arc::clone(&source.handle),
                    observation: bound_source,
                    offset_permit: source_offset_permit,
                },
                owned_catalog.as_ref(),
                selected_stream,
                remaining,
                owned_cancelled.as_ref(),
            )
            .await;
            (result, gate, source)
        });
        let (facts, gate, source) =
            await_owned_collection(collection, remaining, cancelled).await?;
        let facts = facts?;
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        probe.validate_current(remaining, cancelled).await?;
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        let (after_source, gate) = source_observation_with_probe_gate(
            Arc::clone(&source.handle),
            gate,
            remaining,
            cancelled,
            source.final_identity_delay(),
        )
        .await?;
        if after_source.identity != key.source {
            return Err(DecodeFactError::SourceChanged);
        }
        let mut entries = self.entries.lock().await;
        if let Some(existing) = entries.get(&key) {
            return Ok(existing.clone());
        }
        let mut order = self.order.lock().await;
        while entries.len() >= self.capacity {
            let Some(oldest) = order.pop_front() else {
                return Err(DecodeFactError::CacheInvariant);
            };
            entries.remove(&oldest);
        }
        entries.insert(key.clone(), facts.clone());
        order.push_back(key);
        drop(gate);
        Ok(facts)
    }
}

/// Wait for caller-visible completion without abandoning cleanup ownership.
/// Dropping a timed-out join handle detaches the task; the task continues to
/// own the child, source descriptor, offset restoration, and both permits.
async fn await_owned_collection<T>(
    mut collection: tokio::task::JoinHandle<T>,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<T, DecodeFactError>
where
    T: Send + 'static,
{
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        _ = tokio::time::sleep(budget) => Err(DecodeFactError::Deadline),
        result = &mut collection => {
            result.map_err(|error| DecodeFactError::Read(error.to_string()))
        }
    }
}

async fn wait_for_cancellation(cancelled: Option<&tokio_util::sync::CancellationToken>) {
    match cancelled {
        Some(cancelled) => cancelled.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeFactError {
    ProbeIdentity(String),
    ProbeChanged,
    SourceMetadata(String),
    SourceChanged,
    Spawn(String),
    MissingPipe,
    Deadline,
    Cancelled,
    Read(String),
    OversizedOutput,
    Failed(Option<i32>, String),
    InvalidJson(String),
    InvalidFacts(String),
    CacheInvariant,
    #[cfg(not(target_os = "linux"))]
    UnsupportedPlatform,
}

impl std::fmt::Display for DecodeFactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProbeIdentity(error) => write!(formatter, "identifying FFprobe: {error}"),
            Self::ProbeChanged => formatter.write_str("FFprobe changed after startup identity"),
            Self::SourceMetadata(error) => {
                write!(formatter, "reading bound source metadata: {error}")
            }
            Self::SourceChanged => {
                formatter.write_str("bound source changed during decoder probing")
            }
            Self::Spawn(error) => write!(formatter, "starting bound FFprobe: {error}"),
            Self::MissingPipe => formatter.write_str("bound FFprobe pipe was unavailable"),
            Self::Deadline => formatter.write_str("bound FFprobe exceeded its deadline"),
            Self::Cancelled => formatter.write_str("bound FFprobe was cancelled"),
            Self::Read(error) => write!(formatter, "reading bound FFprobe output: {error}"),
            Self::OversizedOutput => formatter.write_str("bound FFprobe output exceeded its limit"),
            Self::Failed(code, reason) => {
                write!(formatter, "bound FFprobe failed ({code:?}): {reason}")
            }
            Self::InvalidJson(error) => write!(formatter, "parsing bound FFprobe JSON: {error}"),
            Self::InvalidFacts(error) => {
                write!(formatter, "validating bound decode facts: {error}")
            }
            Self::CacheInvariant => {
                formatter.write_str("decoder fact cache ordering is inconsistent")
            }
            #[cfg(not(target_os = "linux"))]
            Self::UnsupportedPlatform => formatter
                .write_str("descriptor-bound decoder probing is unsupported on this platform"),
        }
    }
}

impl std::error::Error for DecodeFactError {}

#[derive(Debug)]
struct DecodeSourceObservation {
    identity: DecodeSourceIdentity,
    executable: bool,
}

#[derive(Debug)]
struct DecodeFactCollectionSource {
    handle: Arc<std::fs::File>,
    observation: DecodeSourceObservation,
    offset_permit: tokio::sync::OwnedSemaphorePermit,
}

#[cfg(unix)]
fn source_observation(source: &std::fs::File) -> Result<DecodeSourceObservation, DecodeFactError> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let metadata = source
        .metadata()
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DecodeFactError::SourceMetadata(
            "source handle is not a regular file".to_owned(),
        ));
    }
    let body = serde_json::json!({
        "bytes": metadata.len(),
        "modified_seconds": metadata.mtime(),
        "modified_nanoseconds": metadata.mtime_nsec(),
        "changed_seconds": metadata.ctime(),
        "changed_nanoseconds": metadata.ctime_nsec(),
        "device": metadata.dev(),
        "inode": metadata.ino(),
    });
    let identity =
        DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(body.to_string().as_bytes())))
            .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    Ok(DecodeSourceObservation {
        identity,
        executable: metadata.permissions().mode() & 0o111 != 0,
    })
}

#[cfg(unix)]
async fn source_observation_with_probe_gate(
    source: Arc<std::fs::File>,
    probe_gate: tokio::sync::OwnedSemaphorePermit,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    delay: Duration,
) -> Result<(DecodeSourceObservation, tokio::sync::OwnedSemaphorePermit), DecodeFactError> {
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let started = std::time::Instant::now();
    let identity_permit = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        permit = tokio::time::timeout(budget, identity_gate().acquire_owned()) => {
            permit
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|_| DecodeFactError::CacheInvariant)?
        }
    };
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    // Both admissions move into the blocking owner. A timed-out caller can
    // detach, but another probe or metadata syscall cannot enter until this
    // one actually returns.
    let mut task = tokio::task::spawn_blocking(move || {
        let _identity_permit = identity_permit;
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        (source_observation(&source), probe_gate)
    });
    let (observation, probe_gate) = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        result = tokio::time::timeout(remaining, &mut task) => {
            result
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?
        }
    };
    Ok((observation?, probe_gate))
}

#[cfg(not(unix))]
async fn source_observation_with_probe_gate(
    _source: Arc<std::fs::File>,
    _probe_gate: tokio::sync::OwnedSemaphorePermit,
    _budget: Duration,
    _cancelled: Option<&tokio_util::sync::CancellationToken>,
    _delay: Duration,
) -> Result<(DecodeSourceObservation, tokio::sync::OwnedSemaphorePermit), DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool), DecodeFactError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut scratch = [0_u8; 8 * 1024];
    let mut overflow = false;
    loop {
        let count = reader
            .read(&mut scratch)
            .await
            .map_err(|error| DecodeFactError::Read(error.to_string()))?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&scratch[..count.min(remaining)]);
        overflow |= count > remaining;
    }
    Ok((retained, overflow))
}

#[cfg(unix)]
async fn collect(
    probe: &DecodeProbeIdentity,
    source: DecodeFactCollectionSource,
    catalog: Option<&DecodeCatalogMetadata>,
    selected_stream: ProbeStreamSelection,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<DecodeFacts, DecodeFactError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let DecodeFactCollectionSource {
        handle,
        observation,
        offset_permit,
    } = source;
    let started = std::time::Instant::now();
    let launch_deadline = started + budget.min(PROBE_DEADLINE);
    let source_fd = handle.as_raw_fd();
    if probe.launch_mode.is_production() && observation.executable {
        return Err(DecodeFactError::SourceMetadata(
            "bound media source must not have executable mode bits".to_owned(),
        ));
    }
    let source_offset = unsafe { libc::lseek(source_fd, 0, libc::SEEK_CUR) };
    if source_offset == -1 {
        return Err(DecodeFactError::SourceMetadata(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    // A duplicated descriptor shares its open-file offset. FFprobe is allowed
    // to seek while inspecting the held inode, but the later FFmpeg spawn must
    // inherit the same offset the preparation owner held before probing.
    struct RestoreOffset {
        fd: std::os::fd::RawFd,
        offset: libc::off_t,
    }
    impl Drop for RestoreOffset {
        fn drop(&mut self) {
            unsafe {
                libc::lseek(self.fd, self.offset, libc::SEEK_SET);
            }
        }
    }
    let restore_offset = RestoreOffset {
        fd: source_fd,
        offset: source_offset,
    };
    let executable_fd = probe.executable_snapshot.as_file().as_raw_fd();
    let arguments = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_entries",
        "stream=index,codec_type,codec_name,profile,pix_fmt,width,height,bits_per_raw_sample,avg_frame_rate,r_frame_rate,color_range,color_space,color_transfer,color_primaries:stream_disposition=attached_pic:stream_side_data=side_data_type",
        "-show_streams",
        "/dev/fd/3",
    ]
    .map(OsString::from);
    let mut command =
        tokio::process::Command::new(snapshot_execution_path(&probe.executable_snapshot));
    command.as_std_mut().arg0(probe.executable());
    #[cfg(test)]
    command.env("PLURX_TEST_PROBE_PATH", probe.executable());
    command.args(&arguments);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let supervisor = configure_probe_execution(
        &mut command,
        probe.launch_mode,
        launch_deadline,
        executable_fd,
        Some(source_fd),
        probe.executable(),
        &arguments,
    )?;
    let (mut child, mut supervisor) = spawn_configured_probe(command, supervisor).await?;
    let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
    let session = ProbeSessionGuard::new(process_group);
    #[cfg(target_os = "linux")]
    let exit_anchor = match ProbeExitAnchor::open(process_group, probe.launch_mode) {
        Ok(anchor) => anchor,
        Err(error) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Spawn(format!(
                "opening FFprobe exit anchor: {error}"
            )));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::MissingPipe);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::MissingPipe);
        }
    };
    let remaining = launch_deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
        return Err(DecodeFactError::Deadline);
    }
    let outcome = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => {
            terminate_probe_session(
                &mut child,
                &session,
                &mut supervisor,
                probe.launch_mode,
            ).await;
            return Err(DecodeFactError::Cancelled);
        }
        outcome = tokio::time::timeout(remaining, async {
            #[cfg(target_os = "linux")]
            let (stdout, stderr, exited) = tokio::join!(
                read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                async {
                    let exited = exit_anchor.wait_until_exit().await;
                    session.kill_once();
                    exited
                },
            );
            #[cfg(target_os = "linux")]
            let (status, exit_error) = {
                let exit_error = exited
                    .err()
                    .map(|error| DecodeFactError::Read(error.to_string()));
                (
                    wait_for_probe_reap(&mut child, probe.launch_mode).await,
                    exit_error,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let (stdout, stderr, status) = tokio::join!(
                read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                child.wait(),
            );
            #[cfg(not(target_os = "linux"))]
            session.kill_once();
            #[cfg(not(target_os = "linux"))]
            let exit_error: Option<DecodeFactError> = None;
            Ok::<_, DecodeFactError>((stdout, stderr, status, exit_error))
        }) => outcome,
    };
    let (stdout, stderr, status, exit_error) = match outcome {
        Ok(Ok((stdout, stderr, status, exit_error))) => (stdout, stderr, status, exit_error),
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Deadline);
        }
    };
    let supervisor_result = supervisor.finish();
    // The probe process tree is reaped now. Restore the shared open-file
    // offset and release the producer lane before parsing or the authoritative
    // final metadata observation. A blocked final fstat must never prevent the
    // neutral M1 fallback from launching the legacy FFmpeg producer.
    drop(restore_offset);
    drop(offset_permit);
    if let Some(error) = exit_error {
        return Err(error);
    }
    supervisor_result?;
    let stdout = stdout?;
    let stderr = stderr?;
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            return Err(DecodeFactError::Read(error.to_string()));
        }
    };
    if stdout.1 || stderr.1 {
        return Err(DecodeFactError::OversizedOutput);
    }
    if !status.success() {
        let reason = String::from_utf8_lossy(&stderr.0)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("FFprobe gave no reason")
            .chars()
            .take(512)
            .collect();
        return Err(DecodeFactError::Failed(status.code(), reason));
    }
    let before = observation.identity.clone();
    let json: serde_json::Value = serde_json::from_slice(&stdout.0)
        .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))?;
    match selected_stream {
        ProbeStreamSelection::FirstPlayable => match catalog {
            Some(catalog) => DecodeFacts::from_ffprobe_json_with_catalog(&json, before, catalog),
            None => DecodeFacts::from_ffprobe_json(&json, before),
        },
        ProbeStreamSelection::LegacyVideoOrdinal(ordinal) => {
            let index = absolute_video_ordinal(&json, ordinal).ok_or_else(|| {
                DecodeFactError::InvalidFacts("legacy video stream is missing".to_owned())
            })?;
            match catalog.filter(|_| first_playable_video_index(&json) == Some(index)) {
                Some(catalog) => {
                    DecodeFacts::from_ffprobe_json_at_with_catalog(&json, before, index, catalog)
                }
                None => DecodeFacts::from_ffprobe_json_at(&json, before, index),
            }
        }
        ProbeStreamSelection::Absolute(index) => {
            match catalog.filter(|_| first_playable_video_index(&json) == Some(index)) {
                Some(catalog) => {
                    DecodeFacts::from_ffprobe_json_at_with_catalog(&json, before, index, catalog)
                }
                None => DecodeFacts::from_ffprobe_json_at(&json, before, index),
            }
        }
    }
    .map_err(|error| DecodeFactError::InvalidFacts(error.to_string()))
}

fn absolute_video_ordinal(json: &serde_json::Value, ordinal: u32) -> Option<u32> {
    json.get("streams")?
        .as_array()?
        .iter()
        .filter(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
        })
        .nth(usize::try_from(ordinal).ok()?)?
        .get("index")?
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
}

fn first_playable_video_index(json: &serde_json::Value) -> Option<u32> {
    json.get("streams")?
        .as_array()?
        .iter()
        .find(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
                && !stream
                    .get("disposition")
                    .and_then(|value| value.get("attached_pic"))
                    .and_then(serde_json::Value::as_i64)
                    .is_some_and(|value| value != 0)
        })?
        .get("index")?
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
}

#[cfg(not(unix))]
async fn collect(
    _probe: &DecodeProbeIdentity,
    _source: DecodeFactCollectionSource,
    _catalog: Option<&DecodeCatalogMetadata>,
    _selected_stream: ProbeStreamSelection,
    _budget: Duration,
    _cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<DecodeFacts, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_pre_exec_transitive_source_audit_rejects_allocation_and_panic_forms() {
        fn item_body<'a>(source: &'a str, needle: &str) -> &'a str {
            let start = source
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"));
            let open = source[start..]
                .find('{')
                .map(|offset| start + offset)
                .unwrap_or_else(|| panic!("missing body for {needle}"));
            let mut depth = 0_usize;
            for (offset, byte) in source.as_bytes()[open..].iter().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &source[open..=open + offset];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unterminated body for {needle}")
        }

        let source = include_str!("decode_facts.rs");
        for item in [
            "command.pre_exec(move ||",
            "fn is_production(self)",
            "fn bootstrap_interrupts(self)",
            "fn export_socket_fd(self)",
            "fn start_probe_session(",
            "fn duplicate_child_fd(",
            "fn assign_child_fd(",
            "fn install_child_fd(",
            "fn install_probe_child_fds(",
            "fn consume_linux_bootstrap_interrupt(",
            "fn ensure_linux_probe_deadline(",
            "fn confirm_linux_probe_fork(",
            "fn wait_for_linux_fd(",
            "fn mark_unrelated_fds_close_on_exec(",
            "fn install_linux_filesystem_execute_denial(",
            "fn send_linux_seccomp_listener(",
            "fn install(&self)",
            "fn install_listener(&self)",
            "fn execute_held_probe(&self)",
        ] {
            let body = item_body(source, item);
            for allocating in [
                "std::io::Error::new(",
                "std::io::Error::other(",
                "format!(",
                "to_owned(",
                "to_string(",
                "to_vec(",
                "Box::new(",
                "Vec::new(",
                "Vec::with_capacity(",
                "String::new(",
                "String::from(",
                "CString::new(",
                "OsString::from(",
                "vec![",
                ".push(",
                ".extend(",
                ".collect(",
                ".collect::<",
                "panic!(",
                "assert!(",
                "assert_eq!(",
            ] {
                assert!(
                    !body.contains(allocating),
                    "post-fork item {item} contains allocating or panic path {allocating}"
                );
            }
        }
    }

    #[cfg(unix)]
    fn executable(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).expect("write probe");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("make probe executable");
    }

    #[cfg(target_os = "linux")]
    fn build_static_probe(path: &std::path::Path, mode: u8) {
        use std::os::unix::fs::PermissionsExt;

        let source = path.with_extension("c");
        std::fs::write(
            &source,
            r#"
#if !defined(__x86_64__) && !defined(__aarch64__)
#error unsupported test architecture
#endif

#define NR_READ 0
#define NR_WRITE 1
#define NR_CLOSE 3
#define NR_LSEEK 8
#define NR_NANOSLEEP 35
#define NR_FORK 57
#define NR_EXECVE 59
#define NR_FCHMOD 91
#define NR_FCNTL 72
#define NR_SETPGID 109
#define NR_SETSID 112
#define NR_SENDMSG 46
#define NR_EXIT 60
#define NR_MEMFD_CREATE 319
#define NR_EXECVEAT 322
#if defined(__aarch64__)
#undef NR_READ
#undef NR_WRITE
#undef NR_CLOSE
#undef NR_LSEEK
#undef NR_NANOSLEEP
#undef NR_FORK
#undef NR_EXECVE
#undef NR_FCHMOD
#undef NR_FCNTL
#undef NR_SETPGID
#undef NR_SETSID
#undef NR_SENDMSG
#undef NR_EXIT
#undef NR_MEMFD_CREATE
#undef NR_EXECVEAT
#define NR_READ 63
#define NR_WRITE 64
#define NR_CLOSE 57
#define NR_LSEEK 62
#define NR_NANOSLEEP 101
#define NR_FORK 220
#define NR_EXECVE 221
#define NR_FCHMOD 52
#define NR_FCNTL 25
#define NR_SETPGID 154
#define NR_SETSID 157
#define NR_SENDMSG 211
#define NR_EXIT 93
#define NR_MEMFD_CREATE 279
#define NR_EXECVEAT 281
#endif
#define AT_EMPTY_PATH 0x1000
#define FD_CLOEXEC 1
#define F_DUPFD 0
#define F_SETFD 2
#define SIGCHLD 17
#define SOL_SOCKET 1
#define SCM_RIGHTS 1

struct probe_iovec { void *base; unsigned long length; };
struct probe_msghdr {
    void *name;
    unsigned name_length;
    struct probe_iovec *iov;
    unsigned long iov_length;
    void *control;
    unsigned long control_length;
    int flags;
};
struct probe_cmsghdr { unsigned long length; int level; int type; };
struct probe_control { struct probe_cmsghdr header; int fd; int padding; };

#if defined(__x86_64__)
static long syscall6(long number, long a0, long a1, long a2, long a3, long a4, long a5) {
    long result;
    register long r10 __asm__("r10") = a3;
    register long r8 __asm__("r8") = a4;
    register long r9 __asm__("r9") = a5;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
    return result;
}
__asm__(".global _start\n.type _start,@function\n_start:\n mov %rsp,%rdi\n andq $-16,%rsp\n call probe_main\n");
#else
static long syscall6(long number, long a0, long a1, long a2, long a3, long a4, long a5) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x5 __asm__("x5") = a5;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}
__asm__(".global _start\n.type _start,%function\n_start:\n mov x0,sp\n bl probe_main\n");
#endif

static unsigned long length(const char *text) {
    unsigned long count = 0;
    while (text[count]) count++;
    return count;
}

static int same(const char *left, const char *right) {
    while (*left && *right && *left == *right) { left++; right++; }
    return *left == *right;
}

static void write_text(long fd, const char *text) {
    syscall6(NR_WRITE, fd, (long)text, length(text), 0, 0, 0);
}

static __attribute__((noreturn)) void finish(long status) {
    syscall6(NR_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {}
}

static const char facts[] = "{\"streams\":[{\"index\":4,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"profile\":\"High\",\"pix_fmt\":\"yuv420p\",\"width\":1920,\"height\":1080,\"avg_frame_rate\":\"24000/1001\",\"r_frame_rate\":\"24000/1001\",\"disposition\":{\"attached_pic\":0}}]}\n";

void probe_main(unsigned long *stack) {
    long argc = (long)stack[0];
    char **argv = (char **)(stack + 1);
#if PROBE_MODE == 0
    if (argc > 1 && same(argv[1], "-v")) {
        char source[64];
        syscall6(NR_LSEEK, 3, 0, 0, 0, 0, 0);
        long count = syscall6(NR_READ, 3, (long)source, sizeof(source) - 1, 0, 0, 0);
        if (count != 23) finish(84);
        source[count] = 0;
        if (!same(source, "production-bound source")) finish(85);
    }
#elif PROBE_MODE == 1
    char *next[] = { "/usr/bin/false", 0 };
    write_text(1, "entered:path\n");
    syscall6(NR_EXECVE, (long)next[0], (long)next, 0, 0, 0, 0);
    write_text(1, "blocked:path\n");
#elif PROBE_MODE == 2
    if (argc > 1 && same(argv[1], "copied")) finish(92);
    write_text(1, "entered:memfd\n");
    long copy = syscall6(NR_MEMFD_CREATE, (long)"copy", 0, 0, 0, 0, 0);
    if (copy < 0) finish(81);
    syscall6(NR_LSEEK, 4, 0, 0, 0, 0, 0);
    char buffer[4096];
    for (;;) {
        long count = syscall6(NR_READ, 4, (long)buffer, sizeof(buffer), 0, 0, 0);
        if (count < 0) finish(82);
        if (count == 0) break;
        if (syscall6(NR_WRITE, copy, (long)buffer, count, 0, 0, 0) != count) finish(83);
    }
    syscall6(NR_FCHMOD, copy, 0500, 0, 0, 0, 0);
    syscall6(NR_LSEEK, copy, 0, 0, 0, 0, 0);
    char *next[] = { "copy", "copied", 0 };
    syscall6(NR_EXECVEAT, copy, (long)"", (long)next, 0, AT_EMPTY_PATH, 0);
    syscall6(NR_EXECVEAT, 4, (long)"/proc/self/fd/5", (long)next, 0, AT_EMPTY_PATH, 0);
    write_text(1, "blocked:memfd\n");
#elif PROBE_MODE == 3
    if (argc > 1 && same(argv[1], "reentered")) {
        long rebound = syscall6(NR_FCNTL, 5, F_DUPFD, 4, 0, 0, 0);
        char *again[] = { "replacement", "copied", 0 };
        syscall6(NR_EXECVEAT, rebound, (long)"", (long)again, 0, AT_EMPTY_PATH, 0);
        finish(93);
    }
    long copy = syscall6(NR_MEMFD_CREATE, (long)"replacement", 0, 0, 0, 0, 0);
    if (copy < 0) finish(86);
    syscall6(NR_LSEEK, 4, 0, 0, 0, 0, 0);
    char buffer[4096];
    for (;;) {
        long count = syscall6(NR_READ, 4, (long)buffer, sizeof(buffer), 0, 0, 0);
        if (count < 0) finish(87);
        if (count == 0) break;
        if (syscall6(NR_WRITE, copy, (long)buffer, count, 0, 0, 0) != count) finish(88);
    }
    syscall6(NR_FCHMOD, copy, 0500, 0, 0, 0, 0);
    syscall6(NR_LSEEK, copy, 0, 0, 0, 0, 0);
    long changed = syscall6(NR_FCNTL, 4, F_SETFD, FD_CLOEXEC, 0, 0, 0);
    char *next[] = { "sealed", "reentered", 0 };
    if (changed == 0) {
        syscall6(NR_EXECVEAT, 4, (long)"", (long)next, 0, AT_EMPTY_PATH, 0);
        finish(94);
    }
    write_text(1, "blocked:fd-reuse\n");
#elif PROBE_MODE == 4
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, facts);
        finish(0);
    }
    long child;
#if defined(__x86_64__)
    child = syscall6(NR_FORK, 0, 0, 0, 0, 0, 0);
#else
    child = syscall6(NR_FORK, SIGCHLD, 0, 0, 0, 0, 0);
#endif
    if (child < 0) finish(89);
    if (child == 0) {
        long escaped_session = syscall6(NR_SETSID, 0, 0, 0, 0, 0, 0);
        long escaped_group = syscall6(NR_SETPGID, 0, 0, 0, 0, 0, 0);
        unsigned long delay[2] = { 0, 500000000 };
        syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0);
        if (escaped_session >= 0 || escaped_group >= 0) write_text(1, "escaped-session\n");
        finish(0);
    }
#elif PROBE_MODE == 5
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, facts);
        finish(0);
    }
    char byte = 'x';
    struct probe_iovec io = { &byte, 1 };
    struct probe_control control = {
        { sizeof(struct probe_cmsghdr) + sizeof(int), SOL_SOCKET, SCM_RIGHTS },
        3,
        0
    };
    struct probe_msghdr message = { 0, 0, &io, 1, &control, sizeof(control), 0 };
    if (syscall6(NR_SENDMSG, 5, (long)&message, 0, 0, 0, 0) >= 0) finish(95);
#elif PROBE_MODE == 6
    write_text(1, facts);
    if (!(argc > 1 && same(argv[1], "-version"))) {
        unsigned long delay[2] = { 5, 0 };
        syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0);
    }
#endif
    write_text(1, facts);
    finish(0);
}
"#,
        )
        .expect("write static probe source");
        let status = std::process::Command::new("cc")
            .arg("-nostdlib")
            .arg("-static")
            .arg("-fno-stack-protector")
            .arg("-fno-pie")
            .arg("-no-pie")
            .arg("-Wl,--build-id=none")
            .arg("-Wl,-e,_start")
            .arg(format!("-DPROBE_MODE={mode}"))
            .arg(&source)
            .arg("-o")
            .arg(path)
            .status()
            .expect("start static probe compiler");
        assert!(status.success(), "compile static production-path probe");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))
            .expect("make static probe executable");
    }

    #[cfg(unix)]
    #[test]
    fn qualified_probe_identity_refuses_indirect_wrappers() {
        let root = crate::test_tempdir().expect("tempdir");
        let wrapper = root.path().join("ffprobe-wrapper");
        executable(&wrapper, "#!/bin/sh\nexec /usr/bin/ffprobe \"$@\"\n");
        let wrapper = std::fs::File::open(wrapper).expect("open wrapper");
        #[cfg(target_os = "linux")]
        assert!(matches!(
            require_direct_probe_executable(&wrapper),
            Err(DecodeFactError::ProbeIdentity(_))
        ));
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            require_direct_probe_executable(&wrapper),
            Err(DecodeFactError::UnsupportedPlatform)
        );

        let current = std::fs::File::open(std::env::current_exe().expect("current executable"))
            .expect("open current native executable");
        #[cfg(target_os = "linux")]
        assert!(matches!(
            require_direct_probe_executable(&current),
            Err(DecodeFactError::ProbeIdentity(reason))
                if reason.contains("statically linked")
        ));
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            require_direct_probe_executable(&current),
            Err(DecodeFactError::UnsupportedPlatform)
        );
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    #[tokio::test]
    async fn production_discovery_refuses_unsupported_unix_within_its_deadline() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("native-looking-ffprobe");
        executable(
            &probe,
            "#!/bin/sh\nprintf '%s\\n' 'unsupported Unix probe must not execute'\n",
        );
        let result = tokio::time::timeout(
            IDENTITY_DEADLINE + VERSION_DEADLINE,
            DecodeProbeIdentity::discover(probe.to_str().expect("probe path")),
        )
        .await
        .expect("unsupported Unix discovery remains bounded");
        assert!(matches!(result, Err(DecodeFactError::UnsupportedPlatform)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn self_contained_elf_has_no_external_parser_dependency() {
        let root = crate::test_tempdir().expect("tempdir");
        let artifact = root.path().join("static-ffprobe");
        let mut elf = vec![0_u8; 64 + 56];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[6] = 1;
        elf[16..18].copy_from_slice(&2_u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62_u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
        elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1_u32.to_le_bytes());
        std::fs::write(&artifact, elf).expect("write structural static ELF");
        let artifact = std::fs::File::open(artifact).expect("open structural static ELF");
        require_direct_probe_executable(&artifact).expect("static ELF has no external closure");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_classification_is_bound_to_the_sealed_snapshot() {
        let root = crate::test_tempdir().expect("tempdir");
        let artifact = root.path().join("mutable-probe");
        let mut elf = vec![0_u8; 64 + 56];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[6] = 1;
        elf[16..18].copy_from_slice(&2_u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62_u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
        elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1_u32.to_le_bytes());
        std::fs::write(&artifact, elf).expect("write initially accepted ELF");
        let opened = std::fs::File::open(&artifact).expect("open initially accepted ELF");
        require_direct_probe_executable(&opened).expect("initial bytes qualify structurally");
        std::fs::copy(
            std::env::current_exe().expect("current dynamic executable"),
            &artifact,
        )
        .expect("replace configured bytes in place");
        let snapshot = snapshot_executable(&opened).expect("snapshot replacement bytes");
        assert!(matches!(
            require_direct_probe_executable(snapshot.as_file()),
            Err(DecodeFactError::ProbeIdentity(reason)) if reason.contains("statically linked")
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sealed_snapshot_refuses_in_place_mutation() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileExt;

        let current = std::fs::File::open(std::env::current_exe().expect("current executable"))
            .expect("open current executable");
        let snapshot = snapshot_executable(&current).expect("sealed executable snapshot");
        let seals = unsafe { libc::fcntl(snapshot.as_file().as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(seals & libc::F_SEAL_WRITE, libc::F_SEAL_WRITE);
        assert!(
            snapshot.as_file().write_at(b"X", 0).is_err(),
            "sealed snapshot bytes must reject an in-place overwrite"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn production_probe_executes_sealed_bytes_and_collects_bound_json() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity and version");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"production-bound source").expect("media");
        let source = Arc::new(std::fs::File::open(media).expect("open media"));
        let facts = DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(5),
                None,
            )
            .await
            .expect("sealed production collection");
        assert_eq!(facts.input_video_stream(), 4);
        assert_eq!(facts.codec(), Some("h264"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn production_bootstrap_eintr_retries_share_the_absolute_deadline() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let mut identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity and version");
        identity.launch_mode = ProbeLaunchMode::ProductionBootstrapInterrupted;
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"production-bound source").expect("media");
        let source = Arc::new(std::fs::File::open(media).expect("open media"));
        let started = std::time::Instant::now();
        let facts = DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(5),
                None,
            )
            .await
            .expect("all injected bootstrap EINTR paths retry successfully");
        assert_eq!(facts.input_video_stream(), 4);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "ready send, acknowledgement, first receive, and first response retries remain bounded"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn persistent_bootstrap_eintr_expires_and_reclaims_supervisor_ownership() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity and version");

        for phase in [
            LinuxBootstrapPhase::ReadySend,
            LinuxBootstrapPhase::AcknowledgementSend,
            LinuxBootstrapPhase::FirstNotificationReceive,
            LinuxBootstrapPhase::FirstNotificationInvalidated,
            LinuxBootstrapPhase::FirstNotificationResponse,
        ] {
            assert_eq!(INTERRUPTED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
            let ownership = Arc::new(tokio::sync::Semaphore::new(1));
            let started = std::time::Instant::now();
            let result = probe_version_with_deadline_on(
                &identity.executable_snapshot,
                identity.executable(),
                ProbeLaunchMode::ProductionBootstrapInterruptedUntilDeadline(phase),
                Duration::from_millis(100),
                Arc::clone(&ownership),
            )
            .await;
            assert!(
                result.is_err(),
                "phase {phase:?} must not pass an interrupted bootstrap"
            );
            assert!(
                started.elapsed() >= Duration::from_millis(75),
                "phase {phase:?} must keep retrying against the launch deadline"
            );
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "phase {phase:?} must fail at its absolute deadline"
            );
            let _ownership = tokio::time::timeout(
                Duration::from_secs(2),
                Arc::clone(&ownership).acquire_owned(),
            )
            .await
            .unwrap_or_else(|_| panic!("phase {phase:?} cleanup did not return ownership"))
            .expect("version ownership semaphore remains open");
            assert_eq!(
                INTERRUPTED_SUPERVISOR_OWNERS.load(Ordering::Acquire),
                0,
                "phase {phase:?} supervisor is reclaimed only after cleanup"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn persistent_steady_response_eintr_cannot_wedge_detached_cleanup() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("second-exec-static-ffprobe");
        build_static_probe(&probe, 2);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity and version");
        assert_eq!(STEADY_RESPONSE_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS.store(0, Ordering::Release);
        STEADY_RESPONSE_DEADLINE_EXITS.store(0, Ordering::Release);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        assert_eq!(
            probe_version_with_deadline_on(
                &identity.executable_snapshot,
                identity.executable(),
                ProbeLaunchMode::ProductionSteadyResponseInterruptedUntilDeadline,
                Duration::from_millis(100),
                Arc::clone(&ownership),
            )
            .await,
            Err(DecodeFactError::Deadline)
        );
        assert!(
            STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS.load(Ordering::Acquire) > 1,
            "the production supervisor must receive the second exec notification and consume persistent response interruptions"
        );
        tokio::time::timeout(Duration::from_millis(250), async {
            while STEADY_RESPONSE_DEADLINE_EXITS.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the response loop itself must observe the shared deadline");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the caller must detach at the shared launch deadline"
        );
        assert!(
            Arc::clone(&ownership).try_acquire_owned().is_err(),
            "the version owner remains held during deliberately slow reap"
        );
        assert_eq!(STEADY_RESPONSE_SUPERVISOR_OWNERS.load(Ordering::Acquire), 1);
        let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
            .await
            .expect("steady-response cleanup cannot wedge supervisor join")
            .expect("version ownership returns after reap and supervisor teardown");
        assert_eq!(STEADY_RESPONSE_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn supervisor_stop_bounds_persistent_steady_response_interrupts() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("second-exec-stop-static-ffprobe");
        build_static_probe(&probe, 2);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity and version");
        assert_eq!(
            STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS.load(Ordering::Acquire),
            0
        );
        STEADY_RESPONSE_STOP_INTERRUPT_HITS.store(0, Ordering::Release);
        STEADY_RESPONSE_STOP_EXITS.store(0, Ordering::Release);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        probe_version_with_deadline_on(
            &identity.executable_snapshot,
            identity.executable(),
            ProbeLaunchMode::ProductionSteadyResponseInterruptedUntilStop,
            Duration::from_secs(2),
            Arc::clone(&ownership),
        )
        .await
        .expect("the supervisor stop signal releases the response loop before deadline");
        assert!(
            STEADY_RESPONSE_STOP_INTERRUPT_HITS.load(Ordering::Acquire) >= 1,
            "the stop-path proof must first consume a steady response interruption"
        );
        assert!(
            STEADY_RESPONSE_STOP_EXITS.load(Ordering::Acquire) >= 1,
            "the response loop itself must observe the supervisor stop signal"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the stop signal, not the long launch deadline, must release the response loop"
        );
        let _ownership = ownership
            .try_acquire_owned()
            .expect("stop-path supervisor and version ownership return before completion");
        assert_eq!(
            STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS.load(Ordering::Acquire),
            0
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn production_probe_allows_only_the_initial_bound_exec() {
        let root = crate::test_tempdir().expect("tempdir");
        for (mode, name) in [
            (1, "path-launcher"),
            (2, "direct-and-proc-memfd-launcher"),
            (3, "fd-reuse-launcher"),
        ] {
            let probe = root.path().join(name);
            build_static_probe(&probe, mode);
            DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
                .await
                .unwrap_or_else(|error| {
                    panic!("{name} must enter and observe blocked exec: {error}")
                });
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn production_probe_descendants_cannot_leave_the_kill_domain() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("session-escape-probe");
        build_static_probe(&probe, 4);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(media).expect("open media"));
        tokio::time::timeout(
            Duration::from_millis(1_500),
            DecodeFactCache::new().get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(2),
                None,
            ),
        )
        .await
        .expect("escaped child must not retain the probe pipes")
        .expect("denied setsid child remains in the killed process group");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn production_probe_cannot_export_the_bound_source_descriptor() {
        use std::os::fd::{AsRawFd, FromRawFd as _};

        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("fd-export-probe");
        build_static_probe(&probe, 5);
        let mut identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity");
        let (exporter, receiver) = std::os::unix::net::UnixDatagram::pair().expect("socket pair");
        let exporter_fd = unsafe { libc::fcntl(exporter.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
        assert!(
            exporter_fd >= 10,
            "reserve a collision-free export descriptor"
        );
        let exporter_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(exporter_fd) };
        identity.launch_mode = ProbeLaunchMode::ProductionFdExport(exporter_fd.as_raw_fd());
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(media).expect("open media"));
        DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(5),
                None,
            )
            .await
            .expect("post-bootstrap sendmsg is denied");
        receiver
            .set_nonblocking(true)
            .expect("nonblocking receiver");
        let mut byte = [0_u8; 1];
        assert!(
            receiver.recv(&mut byte).is_err_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
            }),
            "the external process must receive neither payload nor SCM_RIGHTS"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pidfd_open_failure_detaches_bounded_version_cleanup() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production identity");
        assert_eq!(PIDFD_OPEN_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        assert_eq!(
            probe_version_with_deadline_on(
                &identity.executable_snapshot,
                identity.executable(),
                ProbeLaunchMode::ProductionPidfdOpenFailure,
                Duration::from_millis(100),
                Arc::clone(&ownership),
            )
            .await,
            Err(DecodeFactError::Deadline)
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "pidfd-open cleanup must remain behind the caller deadline"
        );
        assert!(
            Arc::clone(&ownership).try_acquire_owned().is_err(),
            "version ownership remains held until explicit reap"
        );
        assert!(
            PIDFD_OPEN_SUPERVISOR_OWNERS.load(Ordering::Acquire) == 1,
            "production supervisor ownership remains attached through reap"
        );
        let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
            .await
            .expect("detached pidfd-open cleanup finishes")
            .expect("version ownership returns after reap");
        assert_eq!(PIDFD_OPEN_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn delayed_supervisor_handshake_obeys_the_version_deadline() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production identity");
        assert_eq!(DELAYED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        assert_eq!(
            probe_version_with_deadline_on(
                &identity.executable_snapshot,
                identity.executable(),
                ProbeLaunchMode::ProductionSupervisorDelay,
                Duration::from_millis(100),
                Arc::clone(&ownership),
            )
            .await,
            Err(DecodeFactError::Deadline)
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the caller must not block in the pre-exec notification handshake"
        );
        assert!(Arc::clone(&ownership).try_acquire_owned().is_err());
        assert_eq!(DELAYED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 1);
        let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
            .await
            .expect("detached launch owner finishes")
            .expect("version ownership returns after launch cleanup");
        assert_eq!(DELAYED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn failed_listener_receive_cannot_leak_or_deadlock_spawn() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production identity");
        assert_eq!(FAILED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        let result = probe_version_with_deadline_on(
            &identity.executable_snapshot,
            identity.executable(),
            ProbeLaunchMode::ProductionSupervisorReceiveFailure,
            Duration::from_secs(1),
            Arc::clone(&ownership),
        )
        .await;
        assert!(
            !matches!(result, Err(DecodeFactError::Deadline)),
            "the injected receiver failure must actively unblock spawn"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        let _ownership = ownership
            .try_acquire_owned()
            .expect("failed launch returns version ownership");
        assert_eq!(FAILED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pidfd_read_failure_retains_source_ownership_until_reap() {
        use std::io::{Seek, SeekFrom};

        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 6);
        let mut identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production identity");
        identity.launch_mode = ProbeLaunchMode::ProductionPidfdReadFailure;
        assert_eq!(PIDFD_READ_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"0123456789").expect("media");
        let mut opened = std::fs::File::open(media).expect("open media");
        opened.seek(SeekFrom::Start(3)).expect("set source offset");
        let source = Arc::new(opened);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        assert_eq!(
            DecodeFactCache::new()
                .get_or_probe(
                    &identity,
                    DecodeFactSource::new(Arc::clone(&source), Arc::clone(&ownership)),
                    None,
                    ProbeStreamSelection::Absolute(4),
                    Duration::from_millis(400),
                    None,
                )
                .await,
            Err(DecodeFactError::Deadline)
        );
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "pidfd-read cleanup must remain behind the caller deadline"
        );
        assert!(
            Arc::clone(&ownership).try_acquire_owned().is_err(),
            "source ownership remains held until explicit reap"
        );
        assert!(
            PIDFD_READ_SUPERVISOR_OWNERS.load(Ordering::Acquire) == 1,
            "production supervisor ownership remains attached through reap"
        );
        let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
            .await
            .expect("detached pidfd-read cleanup finishes")
            .expect("source ownership returns after reap");
        assert_eq!(PIDFD_READ_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        assert_eq!(
            source
                .try_clone()
                .expect("source view")
                .stream_position()
                .expect("restored source offset"),
            3
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn blocked_source_identity_returns_deadline_without_releasing_probe_ownership() {
        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            "#!/bin/sh\nprintf '%s\\n' 'ffprobe version source-identity'\n",
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("fixture identity");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source =
            DecodeFactSource::isolated(Arc::new(std::fs::File::open(media).expect("open media")))
                .with_identity_delay(Duration::from_secs(1));
        let cache = DecodeFactCache::new();
        let ownership = Arc::clone(&cache.probe_gate);
        let started = std::time::Instant::now();
        assert_eq!(
            cache
                .get_or_probe(
                    &identity,
                    source,
                    None,
                    ProbeStreamSelection::FirstPlayable,
                    Duration::from_millis(100),
                    None,
                )
                .await,
            Err(DecodeFactError::Deadline)
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(
            Arc::clone(&ownership).try_acquire_owned().is_err(),
            "the blocked source syscall must retain the single probe lane"
        );
        let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
            .await
            .expect("detached source observation finishes")
            .expect("probe ownership returns after the syscall owner exits");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn production_probe_refuses_an_executable_source_fd() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::test_tempdir().expect("tempdir");
        let probe = root.path().join("static-ffprobe");
        build_static_probe(&probe, 0);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("production probe identity");
        let source_path = root.path().join("executable-media");
        std::fs::write(&source_path, b"not executable content").expect("source");
        std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o500))
            .expect("mark source executable");
        let source = Arc::new(std::fs::File::open(source_path).expect("open source"));
        let error = DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(5),
                None,
            )
            .await
            .expect_err("executable source must be refused before FD 3 installation");
        assert!(matches!(
            error,
            DecodeFactError::SourceMetadata(reason) if reason.contains("executable mode")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn process_group_termination_is_consumed_exactly_once() {
        let guard = ProbeSessionGuard::new(Some(libc::pid_t::MAX));
        let terminator = guard.terminator();
        assert_eq!(terminator.take_process_group(), Some(libc::pid_t::MAX));
        assert_eq!(terminator.take_process_group(), None);
        drop(guard);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn facts_are_collected_from_the_held_descriptor_and_cached_by_build() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"bound source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open media"));
        std::fs::remove_file(&media).expect("unlink path after opening");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then
  printf '%s\n' 'ffprobe version test-build'
  exit 0
fi
test ! -e "$PLURX_TEST_PROBE_PATH.used" || exit 93
touch "$PLURX_TEST_PROBE_PATH.used"
test "$8" = "/dev/fd/3" || exit 91
test "$(cat "$8")" = "bound source" || exit 92
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001","disposition":{"attached_pic":0}}]}'
"###,
        );
        let cache = DecodeFactCache::with_capacity(2);
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("probe identity");
        let first = cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&source)),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("bound facts");
        assert_eq!(first.input_video_stream(), 4);
        let mut offset_view = source.try_clone().expect("clone source descriptor");
        assert_eq!(
            std::io::Seek::stream_position(&mut offset_view).expect("source offset"),
            0,
            "fact collection must restore the held descriptor's file offset"
        );
        let cached = cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("cache hit does not respawn");
        assert_eq!(cached.facts_digest(), first.facts_digest());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_selection_binds_attached_art_and_catalog_to_the_resolved_absolute_stream() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"multi-video source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open media"));
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version stream-selection'; exit 0; fi
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"mjpeg","profile":"Baseline","pix_fmt":"yuvj420p","width":600,"height":600,"avg_frame_rate":"25/1","r_frame_rate":"25/1","color_transfer":"bt709","disposition":{"attached_pic":1}},{"index":7,"codec_type":"video","codec_name":"hevc","profile":"Main 10","pix_fmt":"yuv420p10le","width":3840,"height":2160,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"smpte2084","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("probe identity");
        let cache = DecodeFactCache::new();
        let attached = cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&source)),
                None,
                ProbeStreamSelection::LegacyVideoOrdinal(0),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("legacy ordinal zero includes attached video");
        assert_eq!(attached.input_video_stream(), 4);
        assert_eq!(attached.codec(), Some("mjpeg"));

        let catalog = DecodeCatalogMetadata::new(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
            plurx_core::domain::DolbyVisionFacts {
                profile: Some(8),
                level: Some(6),
                bl_compat_id: Some(1),
                el_present: Some(false),
                rpu_present: Some(true),
            },
        )
        .expect("catalog facts");
        let attached_with_catalog = cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&source)),
                Some(&catalog),
                ProbeStreamSelection::LegacyVideoOrdinal(0),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("first-playable catalog is not applied to attached art");
        assert_eq!(attached_with_catalog.input_video_stream(), 4);
        assert_eq!(attached_with_catalog.dynamic_range(), Some("sdr"));
        let playable = cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                Some(&catalog),
                ProbeStreamSelection::LegacyVideoOrdinal(1),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("catalog binds after ordinal resolves to an absolute stream");
        assert_eq!(playable.input_video_stream(), 7);
        assert_eq!(playable.codec(), Some("hevc"));
        assert_eq!(playable.dynamic_range(), Some("dolby_vision"));
        assert_eq!(playable.dolby_vision().profile, Some(8));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_probe_identity_is_refused_before_fact_collection() {
        let error = DecodeProbeIdentity::discover_fixture("plurx-no-such-ffprobe")
            .await
            .expect_err("missing probe");
        assert!(matches!(error, DecodeFactError::ProbeIdentity(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_probe_cannot_reuse_cached_facts() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open"));
        let probe = root.path().join("ffprobe-test");
        let body = r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###;
        executable(&probe, body);
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = DecodeFactCache::new();
        cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&source)),
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("initial facts");
        executable(&probe, &body.replace("version one", "version two"));
        assert_eq!(
            cache
                .get_or_probe(
                    &identity,
                    DecodeFactSource::isolated(source),
                    None,
                    ProbeStreamSelection::FirstPlayable,
                    Duration::from_secs(2),
                    None,
                )
                .await,
            Err(DecodeFactError::ProbeChanged)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transient_path_swap_cannot_change_the_executable_object() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = std::fs::File::open(&media).expect("open source");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version held-a'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let original = root.path().join("ffprobe-original");
        std::fs::rename(&probe, &original).expect("hold original inode under another name");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version transient-b'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","profile":"Main","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let observation = source_observation(&source).expect("source identity");
        let facts = collect(
            &identity,
            DecodeFactCollectionSource {
                handle: Arc::new(source),
                observation,
                offset_permit: Arc::new(tokio::sync::Semaphore::new(1))
                    .acquire_owned()
                    .await
                    .expect("source offset permit"),
            },
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_secs(2),
            None,
        )
        .await
        .expect("held executable runs even while its path names another program");
        assert_eq!(facts.codec(), Some("h264"));
        std::fs::remove_file(&probe).expect("remove transient executable");
        std::fs::rename(&original, &probe).expect("restore original path");
        assert_eq!(
            probe_file_identity(&probe)
                .expect("restored executable identity")
                .content_digest,
            identity.file.content_digest
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn snapshot_path_swap_cannot_change_the_executable_object() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = std::fs::File::open(&media).expect("open source");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version snapshot-a'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let snapshot_path = identity.executable_snapshot.path().to_owned();
        let held_path = snapshot_path.with_extension("held");
        assert!(
            std::fs::rename(&snapshot_path, &held_path).is_err(),
            "the immutable snapshot path must refuse replacement"
        );
        let observation = source_observation(&source).expect("source identity");
        let facts = collect(
            &identity,
            DecodeFactCollectionSource {
                handle: Arc::new(source),
                observation,
                offset_permit: Arc::new(tokio::sync::Semaphore::new(1))
                    .acquire_owned()
                    .await
                    .expect("source offset permit"),
            },
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_secs(2),
            None,
        )
        .await
        .expect("held snapshot descriptor remains the execution object");
        assert_eq!(facts.codec(), Some("h264"));
        assert!(
            !probe.with_extension("snapshot-b").exists(),
            "replacement at the snapshot pathname must never execute"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_is_revalidated_after_post_probe_build_validation() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open source"));
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version source-fence'; exit 0; fi
touch "$PLURX_TEST_PROBE_PATH.started"
sleep 1
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
touch "$PLURX_TEST_PROBE_PATH.done"
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = Arc::new(DecodeFactCache::new());
        let task_cache = Arc::clone(&cache);
        let task_identity = identity.clone();
        let task_source = Arc::clone(&source);
        let task = tokio::spawn(async move {
            task_cache
                .get_or_probe(
                    &task_identity,
                    DecodeFactSource::isolated(task_source),
                    None,
                    ProbeStreamSelection::FirstPlayable,
                    Duration::from_secs(4),
                    None,
                )
                .await
        });
        for _ in 0..100 {
            if probe.with_extension("started").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(probe.with_extension("started").exists(), "probe started");
        let hash_barrier = identity_gate()
            .acquire_owned()
            .await
            .expect("identity gate");
        for _ in 0..150 {
            if probe.with_extension("done").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(probe.with_extension("done").exists(), "probe completed");
        std::fs::write(&media, b"changed source").expect("mutate source after probe");
        drop(hash_barrier);
        assert_eq!(
            task.await.expect("join collector"),
            Err(DecodeFactError::SourceChanged)
        );
    }

    #[tokio::test]
    async fn cancelled_identity_hash_keeps_the_single_worker_admission() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let cancelled = tokio_util::sync::CancellationToken::new();
        let worker_active = Arc::clone(&active);
        let worker_maximum = Arc::clone(&maximum);
        let task_gate = Arc::clone(&gate);
        let task_cancelled = cancelled.clone();
        let first = tokio::spawn(async move {
            run_bounded_identity_task_on(
                task_gate,
                Duration::from_secs(1),
                Some(&task_cancelled),
                move || {
                    let now = worker_active.fetch_add(1, Ordering::SeqCst) + 1;
                    worker_maximum.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(150));
                    worker_active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancelled.cancel();
        assert_eq!(
            first.await.expect("join cancelled identity waiter"),
            Err(DecodeFactError::Cancelled)
        );
        assert_eq!(
            run_bounded_identity_task_on(
                Arc::clone(&gate),
                Duration::from_millis(20),
                None,
                || Ok(())
            )
            .await,
            Err(DecodeFactError::Deadline)
        );
        tokio::time::sleep(Duration::from_millis(160)).await;
        run_bounded_identity_task_on(gate, Duration::from_secs(1), None, || Ok(()))
            .await
            .expect("lane reopens after the cancelled worker completes");
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn caller_deadline_detaches_cleanup_without_releasing_its_ownership() {
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&gate)
            .acquire_owned()
            .await
            .expect("owned cleanup permit");
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let cleanup = tokio::spawn(async move {
            let _permit = permit;
            let _ = released.await;
            7_u8
        });

        assert_eq!(
            await_owned_collection(cleanup, Duration::from_millis(20), None).await,
            Err(DecodeFactError::Deadline)
        );
        assert!(
            Arc::clone(&gate).try_acquire_owned().is_err(),
            "detached cleanup must retain its owned permit"
        );
        release.send(()).expect("release detached cleanup");
        let _returned =
            tokio::time::timeout(Duration::from_secs(1), Arc::clone(&gate).acquire_owned())
                .await
                .expect("detached cleanup finishes")
                .expect("cleanup permit is returned");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_same_key_misses_spawn_one_probe() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open"));
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
test ! -e "$PLURX_TEST_PROBE_PATH.used" || exit 93
touch "$PLURX_TEST_PROBE_PATH.used"
sleep 1
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = Arc::new(DecodeFactCache::new());
        let first_cache = Arc::clone(&cache);
        let first_source = Arc::clone(&source);
        let first_identity = identity.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get_or_probe(
                    &first_identity,
                    DecodeFactSource::isolated(first_source),
                    None,
                    ProbeStreamSelection::Absolute(4),
                    Duration::from_secs(3),
                    None,
                )
                .await
        });
        let second = cache.get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(3),
            None,
        );
        let (first, second) = tokio::join!(first, second);
        let first = first.expect("join first").expect("first facts");
        let second = second.expect("second facts");
        assert_eq!(first.facts_digest(), second.facts_digest());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_while_waiting_for_probe_admission_is_bounded() {
        let root = crate::test_tempdir().expect("tempdir");
        let first_media = root.path().join("first.bin");
        let second_media = root.path().join("second.bin");
        std::fs::write(&first_media, b"first").expect("first media");
        std::fs::write(&second_media, b"second").expect("second media");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
touch "$PLURX_TEST_PROBE_PATH.started"
sleep 1
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = Arc::new(DecodeFactCache::new());
        let first_cache = Arc::clone(&cache);
        let first_identity = identity.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get_or_probe(
                    &first_identity,
                    DecodeFactSource::isolated(Arc::new(
                        std::fs::File::open(first_media).expect("first source"),
                    )),
                    None,
                    ProbeStreamSelection::FirstPlayable,
                    Duration::from_secs(3),
                    None,
                )
                .await
        });
        for _ in 0..100 {
            if probe.with_extension("started").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            probe.with_extension("started").exists(),
            "first probe started"
        );
        let cancellation = tokio_util::sync::CancellationToken::new();
        let waiter = cache.get_or_probe(
            &identity,
            DecodeFactSource::isolated(Arc::new(
                std::fs::File::open(second_media).expect("second source"),
            )),
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_secs(3),
            Some(&cancellation),
        );
        tokio::pin!(waiter);
        tokio::select! {
            result = &mut waiter => panic!("waiter returned before cancellation: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
        cancellation.cancel();
        assert_eq!(waiter.await, Err(DecodeFactError::Cancelled));
        first.await.expect("join first").expect("first facts");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn running_cancellation_reaps_before_restoring_and_releasing_the_source() {
        use std::io::{Seek, SeekFrom};

        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"0123456789").expect("media");
        let mut opened = std::fs::File::open(&media).expect("open source");
        opened.seek(SeekFrom::Start(3)).expect("set source offset");
        let source = Arc::new(opened);
        let offset_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version running-cancel'; exit 0; fi
( sleep 1
  dd if="$8" of=/dev/null bs=1 skip=6 count=1 >/dev/null 2>&1
  touch "$PLURX_TEST_PROBE_PATH.descendant"
) &
touch "$PLURX_TEST_PROBE_PATH.started"
wait
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cancellation = tokio_util::sync::CancellationToken::new();
        let cache = DecodeFactCache::new();
        let collection = cache.get_or_probe(
            &identity,
            DecodeFactSource::new(Arc::clone(&source), Arc::clone(&offset_gate)),
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_secs(8),
            Some(&cancellation),
        );
        tokio::pin!(collection);
        for _ in 0..100 {
            if probe.with_extension("started").exists() {
                break;
            }
            tokio::select! {
                result = &mut collection => panic!("collector returned before cancellation: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
        assert!(probe.with_extension("started").exists(), "probe started");
        assert!(offset_gate.clone().try_acquire_owned().is_err());
        cancellation.cancel();
        assert_eq!(collection.await, Err(DecodeFactError::Cancelled));
        assert!(
            offset_gate.clone().try_acquire_owned().is_err(),
            "caller cancellation must not release ownership before cleanup"
        );
        let _permit =
            tokio::time::timeout(Duration::from_secs(1), offset_gate.clone().acquire_owned())
                .await
                .expect("cleanup must finish after cancellation")
                .expect("source lease released only after child reap");
        let mut view = source.try_clone().expect("source view");
        assert_eq!(view.stream_position().expect("restored source offset"), 3);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !probe.with_extension("descendant").exists(),
            "cancellation must terminate every inherited-descriptor descendant"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn normal_completion_kills_descendants_before_reaping_the_group_leader() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(media).expect("open source"));
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version normal-group-exit'; exit 0; fi
( sleep 1; touch "$PLURX_TEST_PROBE_PATH.descendant" ) &
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(3),
                None,
            )
            .await
            .expect("facts");
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !probe.with_extension("descendant").exists(),
            "normal completion must kill the process group while the leader PID remains anchored"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_lease_wait_is_charged_to_the_probe_deadline() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open source"));
        let offset_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&offset_gate)
            .acquire_owned()
            .await
            .expect("hold source lease");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version lease-deadline'; exit 0; fi
touch "$PLURX_TEST_PROBE_PATH.started"
sleep 5
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = DecodeFactCache::new();
        let started = std::time::Instant::now();
        let collection = cache.get_or_probe(
            &identity,
            DecodeFactSource::new(source, offset_gate),
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_millis(600),
            None,
        );
        tokio::pin!(collection);
        tokio::select! {
            result = &mut collection => panic!("collector returned while source lease was held: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(450)) => {}
        }
        drop(held);
        assert_eq!(collection.await, Err(DecodeFactError::Deadline));
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "waiting for the source lease must not restart the full probe budget"
        );
        assert!(
            probe.with_extension("started").exists(),
            "the regression must exercise collection after lease acquisition"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn crossed_source_and_executable_fds_are_duplicated_before_assignment() {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let root = crate::test_tempdir().expect("tempdir");
        let source_path = root.path().join("source");
        let executable_path = root.path().join("executable");
        std::fs::write(&source_path, b"bound-source\n").expect("source bytes");
        std::fs::write(&executable_path, b"held-probe\n").expect("probe bytes");
        let source = std::fs::File::open(source_path).expect("source");
        let executable = std::fs::File::open(executable_path).expect("executable");
        let source_fd = source.as_raw_fd();
        let executable_fd = executable.as_raw_fd();
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "test \"$(cat <&3)\" = bound-source && test \"$(cat <&4)\" = held-probe",
        ]);
        unsafe {
            command.pre_exec(move || {
                let source_cross = duplicate_child_fd(source_fd)?;
                let executable_cross = match duplicate_child_fd(executable_fd) {
                    Ok(duplicate) => duplicate,
                    Err(error) => {
                        libc::close(source_cross);
                        return Err(error);
                    }
                };
                let crossed = assign_child_fd(executable_cross, 3)
                    .and_then(|()| assign_child_fd(source_cross, HELD_PROBE_FD));
                libc::close(source_cross);
                libc::close(executable_cross);
                crossed?;
                install_probe_child_fds(HELD_PROBE_FD, 3)
            });
        }
        assert!(command.status().expect("run descriptor check").success());
        drop((source, executable));
    }

    #[tokio::test]
    async fn bounded_reader_drains_but_reports_oversize() {
        let bytes = vec![b'x'; 65];
        let (retained, overflow) = read_bounded(bytes.as_slice(), 64).await.expect("read");
        assert_eq!(retained.len(), 64);
        assert!(overflow);
    }

    #[test]
    fn legacy_video_ordinals_preserve_the_command_builders_exact_mapping() {
        let json = serde_json::json!({"streams": [
            {"index": 2, "codec_type": "audio"},
            {"index": 4, "codec_type": "video", "disposition": {"attached_pic": 1}},
            {"index": 7, "codec_type": "video", "disposition": {"attached_pic": 0}}
        ]});
        assert_eq!(absolute_video_ordinal(&json, 0), Some(4));
        assert_eq!(absolute_video_ordinal(&json, 1), Some(7));
        assert_eq!(absolute_video_ordinal(&json, 2), None);
    }
}
