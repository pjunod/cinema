//! Cold, original P-owned Linux FFmpeg engine observation. The two command
//! children and every dependency File are entered only after the P arm; this
//! module returns no Child, File, task, or descriptor number to application
//! code. Errors retain the enclosing P obligation as Unknown until recovery.
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::process::observation::{Metric, ProcessIdentity, ProcessObserver};
use crate::process::{spawn_job_custodied, ChildJob, ChildWork};
use crate::resource_custody::codec_v2::{
    FixedPlanningEngineTaskV1, FIXED_PLANNING_ENGINE_MAX_OBJECTS,
};

const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OBJECT_BYTES: u64 = 512 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 256 * 1024;
const ENGINE_WORK: ChildWork = ChildWork::background("fixed planning engine observation");

pub(crate) struct FixedEngineNativeProfile {
    pub original_roots: u64,
    pub file_slots: u64,
    pub inherited_child_file_slots: u64,
    pub pipe_readers: u64,
    pub max_object_bytes: u64,
    pub hash_buffer_bytes: u64,
    pub max_command_output_bytes: u64,
    /// Bounded retained Rust custody and closed-object inventory storage.
    pub retained_custody_bytes: u64,
}
pub(crate) fn fixed_engine_native_profile() -> FixedEngineNativeProfile {
    use std::mem::{align_of, size_of};
    use std::sync::atomic::AtomicUsize;
    // PATH_MAX bounds each canonical path accepted by the native open. Seven
    // signed/unsigned 64-bit stat fields use at most 7*20+6 ASCII bytes.
    // Actual Vec/String capacities remain checked before a future refund.
    let object_version_bytes = 7 * 20 + 6;
    let file_inventory_bytes = FIXED_PLANNING_ENGINE_MAX_OBJECTS as u64
        * (size_of::<FixedEngineFileClosed>() as u64
            + libc::PATH_MAX as u64
            + object_version_bytes
            + size_of::<(PathBuf, String)>() as u64
            + 64
            + size_of::<PathBuf>() as u64
            + libc::PATH_MAX as u64
            + size_of::<Vec<u8>>() as u64
            + 32);
    let arc_fd_bytes =
        (size_of::<OwnedFd>() + 2 * size_of::<AtomicUsize>() + 2 * align_of::<OwnedFd>()) as u64;
    let custody_bytes = file_inventory_bytes
        + size_of::<FixedEngineNativeCompletion>() as u64
        + size_of::<FixedEngineNativeFailure>() as u64
        + size_of::<EngineCommandCustody>() as u64
        + 2 * arc_fd_bytes
        + 2 * libc::PATH_MAX as u64
        + 2 * size_of::<Vec<u8>>() as u64;
    FixedEngineNativeProfile {
        original_roots: 2,
        file_slots: FIXED_PLANNING_ENGINE_MAX_OBJECTS as u64,
        inherited_child_file_slots: 1,
        pipe_readers: 4,
        max_object_bytes: MAX_OBJECT_BYTES,
        hash_buffer_bytes: HASH_BUFFER_BYTES as u64,
        max_command_output_bytes: MAX_COMMAND_OUTPUT_BYTES as u64,
        retained_custody_bytes: custody_bytes,
    }
}

pub(crate) struct FixedEngineRootClosed {
    identity: ProcessIdentity,
    close_operation_id: Uuid,
    exit_status: std::process::ExitStatus,
    stdout: FixedEnginePipeClosed,
    stderr: FixedEnginePipeClosed,
    session_closed: SessionClosed,
    pidfd_closed: PidfdClosed,
    // The child-local descriptor was installed from the original prearmed
    // producer File before exec. It becomes extinct with this original group.
    inherited_producer_file_owner_id: Option<Uuid>,
}
enum SessionClosed {
    KilledOriginalGroupAndObservedAbsent,
    OriginalGroupAlreadyAbsent,
}
struct PidfdClosed;
pub(crate) struct FixedEnginePipeClosed {
    owner_id: Uuid,
    close_operation_id: Uuid,
    drained_bytes: usize,
}
pub(crate) struct FixedEngineHashBufferClosed {
    owner_id: Uuid,
    close_operation_id: Uuid,
    capacity_bytes: usize,
}
impl FixedEnginePipeClosed {
    pub(crate) fn verified(&self) -> bool {
        self.drained_bytes <= MAX_COMMAND_OUTPUT_BYTES
    }
    pub(crate) fn owner_id(&self) -> Uuid {
        self.owner_id
    }
    pub(crate) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
}
impl FixedEngineHashBufferClosed {
    pub(crate) fn verified(&self) -> bool {
        self.capacity_bytes <= HASH_BUFFER_BYTES
    }
    pub(crate) fn owner_id(&self) -> Uuid {
        self.owner_id
    }
    pub(crate) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
}
impl FixedEngineRootClosed {
    pub(crate) fn identity(&self) -> ProcessIdentity {
        self.identity
    }
    pub(crate) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
    pub(crate) fn inherited_producer_file_owner_id(&self) -> Option<Uuid> {
        self.inherited_producer_file_owner_id
    }
    pub(crate) fn verified(&self) -> bool {
        self.exit_status.success()
            && self.identity.pid != 0
            && self.identity.start_marker != 0
            && self.stdout.verified()
            && self.stderr.verified()
            && matches!(
                self.session_closed,
                SessionClosed::KilledOriginalGroupAndObservedAbsent
                    | SessionClosed::OriginalGroupAlreadyAbsent
            )
            && matches!(self.pidfd_closed, PidfdClosed)
    }
    pub(crate) fn pipe_closures(&self) -> (&FixedEnginePipeClosed, &FixedEnginePipeClosed) {
        (&self.stdout, &self.stderr)
    }
}

pub(crate) struct FixedEngineFileClosed {
    owner_id: Uuid,
    close_operation_id: Uuid,
    path: PathBuf,
    object_version: String,
    sha256: [u8; 32],
}
impl FixedEngineFileClosed {
    pub(crate) fn owner_id(&self) -> Uuid {
        self.owner_id
    }
    pub(crate) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn object_version(&self) -> &str {
        &self.object_version
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

/// Attested by the collector that owned the dependency-slot iterator: the
/// number of prearmed File slots it never opened. Slots are handed out in
/// order (producer first), so the opened set is exactly the prefix
/// `[0, file_slots.len() - count)`. Only `collect_fixed_engine` constructs it.
pub(crate) struct FixedEngineUnusedSlots {
    count: u16,
}
impl FixedEngineUnusedSlots {
    pub(crate) fn count(&self) -> u16 {
        self.count
    }
}

pub(crate) struct FixedEngineNativeCompletion {
    pub(crate) unused_slots: FixedEngineUnusedSlots,
    pub(crate) producer_path: PathBuf,
    pub(crate) producer_object_version: String,
    pub(crate) producer_sha256: String,
    pub(crate) dependencies: Vec<(PathBuf, String)>,
    pub(crate) media_digest: String,
    pub(crate) build_line: String,
    pub(crate) boot_id: Option<Uuid>,
    pub(crate) version_root: FixedEngineRootClosed,
    pub(crate) dependency_root: FixedEngineRootClosed,
    pub(crate) closed_files: Vec<FixedEngineFileClosed>,
    pub(crate) hash_buffer: FixedEngineHashBufferClosed,
}

/// An error is kept in the original P task book. This type intentionally has
/// no clone or positive-close conversion: a partially entered native command
/// must remain charged until its original handles are resolved.
#[allow(dead_code)] // The recovery path is added only after the native close proof.
pub(crate) struct FixedEngineNativeFailure {
    pub(crate) error: io::Error,
    pub(crate) phase: FixedEngineFailurePhase,
    command: Option<EngineCommandCustody>,
    version_root: Option<FixedEngineRootClosed>,
    dependency_root: Option<FixedEngineRootClosed>,
    closed_files: Vec<FixedEngineFileClosed>,
    hash_buffer: Option<FixedEngineHashBufferClosed>,
    live_file: Option<BoundEngineFile>,
    live_hash_buffer: Option<BoundHashBuffer>,
    cleanup_closed_file: Option<(Uuid, Uuid, PathBuf)>,
    uncertain_native_fds: Vec<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum FixedEngineFailurePhase {
    BeforeNativeEntry,
    VersionEntered,
    VersionClosedDependencyUnentered,
    DependencyEntered,
    BothRootsClosedFilesPartial,
    AllNativeClosedLogicalFailure,
}

/// Original child/job/pipe/pidfd ownership at an uncertain boundary. Merely
/// retaining these fields never represents a settlement or a close receipt.
#[allow(dead_code)]
struct EngineCommandCustody {
    child: Option<tokio::process::Child>,
    job: Option<ChildJob>,
    anchor: Option<tokio::io::unix::AsyncFd<SharedPidfd>>,
    pidfd_owner: Option<Arc<OwnedFd>>,
    stdout: Option<BoundPipeReader<tokio::process::ChildStdout>>,
    stderr: Option<BoundPipeReader<tokio::process::ChildStderr>>,
    stdout_closed: Option<FixedEnginePipeClosed>,
    stderr_closed: Option<FixedEnginePipeClosed>,
    reaped_status: Option<std::process::ExitStatus>,
    original_group_absent: bool,
    pidfd_checked_closed: bool,
    uncertain_native_fds: Vec<i32>,
}

impl EngineCommandCustody {
    fn new(child: Option<tokio::process::Child>, job: Option<ChildJob>) -> Self {
        Self {
            child,
            job,
            anchor: None,
            stdout: None,
            stderr: None,
            pidfd_owner: None,
            stdout_closed: None,
            stderr_closed: None,
            reaped_status: None,
            original_group_absent: false,
            pidfd_checked_closed: false,
            uncertain_native_fds: Vec::new(),
        }
    }

    /// Retry only the original owners retained after an uncertain command.
    /// This is cleanup, never a root-close proof: the successful-command
    /// identity and bounded output observations may already be unavailable.
    async fn retry_cleanup(&mut self, deadline: Instant) -> io::Result<()> {
        let group = self
            .child
            .as_ref()
            .and_then(tokio::process::Child::id)
            .and_then(|pid| libc::pid_t::try_from(pid).ok());
        if let Some(group) = group {
            signal_original_group(group)?;
        }
        if let Some(child) = self.child.as_mut() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let status = tokio::time::timeout(remaining, child.wait())
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "original engine child cleanup deadline",
                    )
                })??;
            self.reaped_status = Some(status);
        }
        if let Some(group) = group {
            observe_original_group_absent(group, deadline).await?;
            self.original_group_absent = true;
        }
        if let Some(stdout) = self.stdout.as_mut() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let drained = tokio::time::timeout(remaining, read_bounded(stdout))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "original engine stdout cleanup deadline",
                    )
                })??;
            let owner = self.stdout.take().expect("retained original stdout");
            match owner.checked_close(drained.len()) {
                Ok(closed) => self.stdout_closed = Some(closed),
                Err((error, raw)) => {
                    self.uncertain_native_fds.push(raw);
                    return Err(error);
                }
            }
        }
        if let Some(stderr) = self.stderr.as_mut() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let drained = tokio::time::timeout(remaining, read_bounded(stderr))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "original engine stderr cleanup deadline",
                    )
                })??;
            let owner = self.stderr.take().expect("retained original stderr");
            match owner.checked_close(drained.len()) {
                Ok(closed) => self.stderr_closed = Some(closed),
                Err((error, raw)) => {
                    self.uncertain_native_fds.push(raw);
                    return Err(error);
                }
            }
        }
        if let Some(anchor) = self.anchor.take() {
            drop(anchor.into_inner());
        }
        if let Some(owner) = self.pidfd_owner.take() {
            match Arc::try_unwrap(owner) {
                Ok(fd) => match checked_close_owned_fd(fd) {
                    Ok(()) => self.pidfd_checked_closed = true,
                    Err((error, raw)) => {
                        self.uncertain_native_fds.push(raw);
                        return Err(error);
                    }
                },
                Err(owner) => {
                    self.pidfd_owner = Some(owner);
                    return Err(io::Error::other("original engine pidfd has another owner"));
                }
            }
        }
        drop(self.child.take());
        drop(self.job.take());
        Ok(())
    }
}

struct SharedPidfd(Arc<OwnedFd>);
impl AsRawFd for SharedPidfd {
    fn as_raw_fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
}

impl FixedEngineNativeFailure {
    fn at(
        error: io::Error,
        phase: FixedEngineFailurePhase,
        command: Option<EngineCommandCustody>,
    ) -> Self {
        Self {
            error,
            phase,
            command,
            version_root: None,
            dependency_root: None,
            closed_files: Vec::new(),
            hash_buffer: None,
            live_file: None,
            live_hash_buffer: None,
            cleanup_closed_file: None,
            uncertain_native_fds: Vec::new(),
        }
    }

    fn with_version(mut self, version: FixedEngineRootClosed) -> Self {
        self.version_root = Some(version);
        self
    }

    fn with_dependency(mut self, dependency: FixedEngineRootClosed) -> Self {
        self.dependency_root = Some(dependency);
        self
    }

    fn with_live_file(mut self, owner: BoundEngineFile) -> Self {
        self.live_file = Some(owner);
        self
    }

    fn with_files(
        mut self,
        files: Vec<FixedEngineFileClosed>,
        hash_buffer: Option<FixedEngineHashBufferClosed>,
    ) -> Self {
        self.closed_files = files;
        self.hash_buffer = hash_buffer;
        self
    }

    fn with_file_failure(
        mut self,
        failure: EngineFileFailure,
        files: Vec<FixedEngineFileClosed>,
        buffer: BoundHashBuffer,
    ) -> Self {
        self.error = failure.error;
        self.closed_files = files;
        self.live_file = failure.owner;
        self.live_hash_buffer = Some(buffer);
        if let Some(raw) = failure.uncertain_fd {
            self.uncertain_native_fds.push(raw);
        }
        self
    }

    /// Maintenance may retry this on the already-entered original P owner.
    /// An unsuccessful close keeps the owner here; raw descriptor tombstones
    /// are diagnostic only and are never passed to `close` on a later retry.
    pub(crate) async fn retry_cleanup(&mut self, deadline: Instant) -> io::Result<()> {
        if let Some(command) = self.command.as_mut() {
            command.retry_cleanup(deadline).await?;
        }
        if let Some(owner) = self.live_file.take() {
            let bound = (owner.owner_id, owner.close_operation_id, owner.path.clone());
            if let Err((error, raw)) = checked_close_owned_fd(owner.file.into()) {
                self.uncertain_native_fds.push(raw);
                return Err(error);
            }
            self.cleanup_closed_file = Some(bound);
        }
        if let Some(buffer) = self.live_hash_buffer.take() {
            self.hash_buffer = Some(buffer.checked_close());
        }
        Ok(())
    }
}

impl From<io::Error> for FixedEngineNativeFailure {
    /// Only original task setup before its start latch uses this conversion.
    fn from(error: io::Error) -> Self {
        Self::at(error, FixedEngineFailurePhase::BeforeNativeEntry, None)
    }
}

struct ClosedCommand {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    root: FixedEngineRootClosed,
}

fn engine_object_version(metadata: &std::fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

fn resolve_executable_path(locator: &str) -> io::Result<PathBuf> {
    let path = Path::new(locator);
    if path.components().count() > 1 {
        return std::fs::canonicalize(path);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(locator))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "engine executable not found"))
        .and_then(std::fs::canonicalize)
}

fn dependency_paths_from_report(report: &[u8]) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(report).lines() {
        if line.contains("=> not found") {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "unresolved FFmpeg dependency",
            ));
        }
        let trimmed = line.trim();
        let candidate = trimmed
            .split_once("=>")
            .map_or(trimmed, |(_, rest)| rest.trim())
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if candidate.starts_with('/') {
            paths.push(std::fs::canonicalize(candidate)?);
        }
    }
    Ok(paths)
}

fn checked_close_owned_fd(fd: OwnedFd) -> Result<(), (io::Error, i32)> {
    let raw = fd.into_raw_fd();
    if unsafe { libc::close(raw) } == 0 {
        Ok(())
    } else {
        Err((io::Error::last_os_error(), raw))
    }
}

/// The IDs are bound in the same operation that opens the original File.
/// Only this wrapper can turn its consumed File into a close record.
struct BoundEngineFile {
    file: std::fs::File,
    owner_id: Uuid,
    close_operation_id: Uuid,
    path: PathBuf,
}
struct EngineFileFailure {
    error: io::Error,
    owner: Option<BoundEngineFile>,
    uncertain_fd: Option<i32>,
}
impl EngineFileFailure {
    fn held(error: io::Error, owner: BoundEngineFile) -> Self {
        Self {
            error,
            owner: Some(owner),
            uncertain_fd: None,
        }
    }
}
impl BoundEngineFile {
    fn open(
        path: PathBuf,
        slot: &crate::resource_custody::codec_v2::FixedPlanningEngineFileSlotV1,
    ) -> io::Result<Self> {
        let file = std::fs::File::open(&path)?;
        Ok(Self {
            file,
            owner_id: slot.owner_id.uuid(),
            close_operation_id: slot.close_operation_id.uuid(),
            path,
        })
    }
    fn checked_close(
        self,
        object_version: String,
        sha256: [u8; 32],
    ) -> Result<FixedEngineFileClosed, EngineFileFailure> {
        if let Err((error, raw)) = checked_close_owned_fd(self.file.into()) {
            return Err(EngineFileFailure {
                error,
                owner: None,
                uncertain_fd: Some(raw),
            });
        }
        Ok(FixedEngineFileClosed {
            owner_id: self.owner_id,
            close_operation_id: self.close_operation_id,
            path: self.path,
            object_version,
            sha256,
        })
    }
}

struct BoundHashBuffer {
    bytes: Vec<u8>,
    owner_id: Uuid,
    close_operation_id: Uuid,
}
impl BoundHashBuffer {
    fn new(task: &FixedPlanningEngineTaskV1) -> Self {
        Self {
            bytes: vec![0u8; HASH_BUFFER_BYTES],
            owner_id: task.hash_buffer_owner_id.uuid(),
            close_operation_id: task.hash_buffer_close_operation_id.uuid(),
        }
    }
    fn checked_close(self) -> FixedEngineHashBufferClosed {
        let capacity_bytes = self.bytes.capacity();
        drop(self.bytes);
        FixedEngineHashBufferClosed {
            owner_id: self.owner_id,
            close_operation_id: self.close_operation_id,
            capacity_bytes,
        }
    }
}

fn hash_original_file(
    path: PathBuf,
    slot: &crate::resource_custody::codec_v2::FixedPlanningEngineFileSlotV1,
    buffer: &mut BoundHashBuffer,
    deadline: Instant,
) -> Result<FixedEngineFileClosed, EngineFileFailure> {
    let owner = BoundEngineFile::open(path, slot).map_err(|error| EngineFileFailure {
        error,
        owner: None,
        uncertain_fd: None,
    })?;
    hash_bound_file(owner, buffer, deadline)
}

fn hash_bound_file(
    mut owner: BoundEngineFile,
    buffer: &mut BoundHashBuffer,
    deadline: Instant,
) -> Result<FixedEngineFileClosed, EngineFileFailure> {
    // The exec/ldd child receives a descriptor duplicate of this open file
    // description. Its offset may have moved before the original root exited.
    if let Err(error) = owner.file.seek(SeekFrom::Start(0)) {
        return Err(EngineFileFailure::held(error, owner));
    }
    let before = match owner.file.metadata() {
        Ok(value) => value,
        Err(error) => return Err(EngineFileFailure::held(error, owner)),
    };
    if !before.is_file() || before.len() > MAX_OBJECT_BYTES {
        return Err(EngineFileFailure::held(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "engine object exceeds fixed regular-file bound",
            ),
            owner,
        ));
    }
    let version = engine_object_version(&before);
    let mut digest = Sha256::new();
    loop {
        if Instant::now() >= deadline {
            return Err(EngineFileFailure::held(
                io::Error::new(io::ErrorKind::TimedOut, "engine object hash deadline"),
                owner,
            ));
        }
        let read = match owner.file.read(&mut buffer.bytes) {
            Ok(value) => value,
            Err(error) => return Err(EngineFileFailure::held(error, owner)),
        };
        if read == 0 {
            break;
        }
        digest.update(&buffer.bytes[..read]);
    }
    let after = match owner.file.metadata() {
        Ok(value) => value,
        Err(error) => return Err(EngineFileFailure::held(error, owner)),
    };
    if engine_object_version(&after) != version {
        return Err(EngineFileFailure::held(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "engine object changed while hashing",
            ),
            owner,
        ));
    }
    let sha256: [u8; 32] = digest.finalize().into();
    owner.checked_close(version, sha256)
}

struct BoundPipeReader<R> {
    reader: R,
    owner_id: Uuid,
    close_operation_id: Uuid,
}
trait EngineOwnedPipe: tokio::io::AsyncRead + Unpin + AsRawFd {
    fn into_engine_owned_fd(self) -> io::Result<OwnedFd>;
}
impl EngineOwnedPipe for tokio::process::ChildStdout {
    fn into_engine_owned_fd(self) -> io::Result<OwnedFd> {
        self.into_owned_fd()
    }
}
impl EngineOwnedPipe for tokio::process::ChildStderr {
    fn into_engine_owned_fd(self) -> io::Result<OwnedFd> {
        self.into_owned_fd()
    }
}
impl<R> BoundPipeReader<R> {
    fn new(reader: R, owner_id: Uuid, close_operation_id: Uuid) -> Self {
        Self {
            reader,
            owner_id,
            close_operation_id,
        }
    }
}
impl<R: EngineOwnedPipe> BoundPipeReader<R> {
    fn checked_close(
        self,
        drained_bytes: usize,
    ) -> Result<FixedEnginePipeClosed, (io::Error, i32)> {
        let raw = self.reader.as_raw_fd();
        let fd = self
            .reader
            .into_engine_owned_fd()
            .map_err(|error| (error, raw))?;
        checked_close_owned_fd(fd)?;
        Ok(FixedEnginePipeClosed {
            owner_id: self.owner_id,
            close_operation_id: self.close_operation_id,
            drained_bytes,
        })
    }
}
async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    owner: &mut BoundPipeReader<R>,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = owner.reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if bytes
            .len()
            .checked_add(count)
            .is_none_or(|length| length > MAX_COMMAND_OUTPUT_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "engine command output exceeded fixed bound",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn signal_original_group(pid: libc::pid_t) -> io::Result<bool> {
    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

async fn observe_original_group_absent(pid: libc::pid_t, deadline: Instant) -> io::Result<()> {
    loop {
        if unsafe { libc::kill(-pid, 0) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "original engine process group has not exited",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn run_original_command(
    executable: &Path,
    args: &[&Path],
    version: bool,
    held_producer: Option<&BoundEngineFile>,
    close_operation_id: Uuid,
    stdout_owner_id: Uuid,
    stdout_close_operation_id: Uuid,
    stderr_owner_id: Uuid,
    stderr_close_operation_id: Uuid,
    phase: FixedEngineFailurePhase,
    deadline: Instant,
) -> Result<ClosedCommand, FixedEngineNativeFailure> {
    if close_operation_id.is_nil() || Instant::now() >= deadline {
        return Err(FixedEngineNativeFailure::at(
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "engine root has no prearmed close operation or deadline",
            ),
            phase,
            None,
        ));
    }
    let held_producer_fd = held_producer.map(|owner| owner.file.as_raw_fd());
    let held_producer_owner_id = held_producer.map(|owner| owner.owner_id);
    let held_path = held_producer_fd.map(|fd| PathBuf::from(format!("/proc/self/fd/{fd}")));
    let launch = if version {
        held_path.as_deref().unwrap_or(executable)
    } else {
        executable
    };
    let mut command = tokio::process::Command::new(launch);
    // Match the existing media-engine digest capture argv exactly.
    if version {
        command.arg("-version");
    } else if let Some(path) = held_path.as_ref() {
        command.arg(path);
    } else {
        for arg in args {
            command.arg(arg);
        }
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(original) = held_producer_fd {
                // The forked child already owns the original descriptor copy.
                // Clear CLOEXEC only there, keeping the parent File sealed from
                // unrelated concurrent launches and avoiding a fixed-fd clash.
                if libc::fcntl(original, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let (child, child_job) = match spawn_job_custodied(&mut command, ENGINE_WORK) {
        Ok(value) => value,
        Err(failure) => {
            let failure = *failure;
            return Err(FixedEngineNativeFailure::at(
                failure.error,
                phase,
                Some(EngineCommandCustody::new(failure.child, failure.job)),
            ));
        }
    };
    let mut custody = EngineCommandCustody::new(Some(child), Some(child_job));
    let Some(pid) = custody.child.as_ref().and_then(tokio::process::Child::id) else {
        return Err(FixedEngineNativeFailure::at(
            io::Error::other("engine child has no PID"),
            phase,
            Some(custody),
        ));
    };
    let identity = match ProcessObserver::new(pid).snapshot(Instant::now()).counters {
        Metric::Known(counters) if counters.identity.pid == pid => counters.identity,
        _ => {
            if let Ok(group) = libc::pid_t::try_from(pid) {
                let _ = signal_original_group(group);
            }
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("engine original process identity unknown"),
                phase,
                Some(custody),
            ));
        }
    };
    let raw_pid = match libc::pid_t::try_from(pid) {
        Ok(value) => value,
        Err(_) => {
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("engine PID outside process-group range"),
                phase,
                Some(custody),
            ));
        }
    };
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, raw_pid, 0) };
    if pidfd < 0 {
        let error = io::Error::last_os_error();
        let _ = signal_original_group(raw_pid);
        return Err(FixedEngineNativeFailure::at(error, phase, Some(custody)));
    }
    let pidfd = match i32::try_from(pidfd) {
        Ok(fd) => unsafe { OwnedFd::from_raw_fd(fd) },
        Err(_) => {
            let _ = signal_original_group(raw_pid);
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("engine pidfd outside descriptor range"),
                phase,
                Some(custody),
            ));
        }
    };
    custody.pidfd_owner = Some(Arc::new(pidfd));
    let anchor = match tokio::io::unix::AsyncFd::new(SharedPidfd(Arc::clone(
        custody.pidfd_owner.as_ref().expect("original pidfd"),
    ))) {
        Ok(value) => value,
        Err(error) => {
            let _ = signal_original_group(raw_pid);
            return Err(FixedEngineNativeFailure::at(error, phase, Some(custody)));
        }
    };
    custody.anchor = Some(anchor);
    let Some(stdout) = custody.child.as_mut().and_then(|child| child.stdout.take()) else {
        let _ = signal_original_group(raw_pid);
        return Err(FixedEngineNativeFailure::at(
            io::Error::other("engine stdout missing"),
            phase,
            Some(custody),
        ));
    };
    custody.stdout = Some(BoundPipeReader::new(
        stdout,
        stdout_owner_id,
        stdout_close_operation_id,
    ));
    let Some(stderr) = custody.child.as_mut().and_then(|child| child.stderr.take()) else {
        let _ = signal_original_group(raw_pid);
        return Err(FixedEngineNativeFailure::at(
            io::Error::other("engine stderr missing"),
            phase,
            Some(custody),
        ));
    };
    custody.stderr = Some(BoundPipeReader::new(
        stderr,
        stderr_owner_id,
        stderr_close_operation_id,
    ));
    let remaining = deadline.saturating_duration_since(Instant::now());
    let observed = tokio::time::timeout(remaining, async {
        tokio::join!(
            read_bounded(custody.stdout.as_mut().expect("original stdout")),
            read_bounded(custody.stderr.as_mut().expect("original stderr")),
            async {
                custody
                    .anchor
                    .as_ref()
                    .expect("original pidfd")
                    .readable()
                    .await?;
                signal_original_group(raw_pid)
            },
            custody.child.as_mut().expect("original child").wait(),
        )
    })
    .await;
    let (stdout, stderr, signal, status) = match observed {
        Ok(value) => value,
        Err(_) => {
            let _ = signal_original_group(raw_pid);
            return Err(FixedEngineNativeFailure::at(
                io::Error::new(io::ErrorKind::TimedOut, "engine original command deadline"),
                phase,
                Some(custody),
            ));
        }
    };
    let signal = match signal {
        Ok(value) => value,
        Err(error) => return Err(FixedEngineNativeFailure::at(error, phase, Some(custody))),
    };
    let stdout = match stdout {
        Ok(value) => value,
        Err(error) => return Err(FixedEngineNativeFailure::at(error, phase, Some(custody))),
    };
    let stderr = match stderr {
        Ok(value) => value,
        Err(error) => return Err(FixedEngineNativeFailure::at(error, phase, Some(custody))),
    };
    let status = match status {
        Ok(value) => value,
        Err(error) => return Err(FixedEngineNativeFailure::at(error, phase, Some(custody))),
    };
    if !status.success() {
        return Err(FixedEngineNativeFailure::at(
            io::Error::other("engine original command failed"),
            phase,
            Some(custody),
        ));
    }
    if let Err(error) = observe_original_group_absent(raw_pid, deadline).await {
        return Err(FixedEngineNativeFailure::at(error, phase, Some(custody)));
    }
    let session_closed = if signal {
        SessionClosed::KilledOriginalGroupAndObservedAbsent
    } else {
        SessionClosed::OriginalGroupAlreadyAbsent
    };
    let stdout_closed = match custody
        .stdout
        .take()
        .expect("original stdout")
        .checked_close(stdout.len())
    {
        Ok(closed) => closed,
        Err((error, raw)) => {
            custody.uncertain_native_fds.push(raw);
            return Err(FixedEngineNativeFailure::at(error, phase, Some(custody)));
        }
    };
    custody.stdout_closed = Some(stdout_closed);
    let stderr_closed = match custody
        .stderr
        .take()
        .expect("original stderr")
        .checked_close(stderr.len())
    {
        Ok(closed) => closed,
        Err((error, raw)) => {
            custody.uncertain_native_fds.push(raw);
            return Err(FixedEngineNativeFailure::at(error, phase, Some(custody)));
        }
    };
    custody.stderr_closed = Some(stderr_closed);
    let anchor = custody.anchor.take().expect("original pidfd").into_inner();
    drop(anchor);
    let fd = match Arc::try_unwrap(custody.pidfd_owner.take().expect("original pidfd owner")) {
        Ok(fd) => fd,
        Err(shared) => {
            custody.pidfd_owner = Some(shared);
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("original pidfd still has an alias"),
                phase,
                Some(custody),
            ));
        }
    };
    if let Err((error, raw)) = checked_close_owned_fd(fd) {
        custody.uncertain_native_fds.push(raw);
        return Err(FixedEngineNativeFailure::at(error, phase, Some(custody)));
    }
    let _ = custody.child.take();
    let _ = custody.job.take();
    Ok(ClosedCommand {
        root: FixedEngineRootClosed {
            identity,
            close_operation_id,
            exit_status: status,
            stdout: custody.stdout_closed.take().expect("closed stdout"),
            stderr: custody.stderr_closed.take().expect("closed stderr"),
            session_closed,
            pidfd_closed: PidfdClosed,
            inherited_producer_file_owner_id: held_producer_owner_id,
        },
        stdout,
        stderr,
    })
}

/// Invoke from the original, retained P task only after its durable arm. The
/// task owns the command roots and the sequential object File loop. A failure
/// does not produce a close capability; P remains held until explicit recovery.
pub(crate) async fn collect_fixed_engine(
    task: FixedPlanningEngineTaskV1,
    boot_id: Option<Uuid>,
    deadline: Instant,
) -> Result<FixedEngineNativeCompletion, FixedEngineNativeFailure> {
    if task.file_slots.len() != FIXED_PLANNING_ENGINE_MAX_OBJECTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "engine file-slot count differs from prearmed plan",
        )
        .into());
    }
    let producer = resolve_executable_path(&task.configured_ffmpeg_locator)?;
    let dependency_tool = resolve_executable_path(&task.dependency_tool_locator)?;
    let producer_file = BoundEngineFile::open(producer.clone(), &task.file_slots[0])?;
    if producer_file.file.as_raw_fd() < 3 {
        return Err(FixedEngineNativeFailure::at(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "prearmed producer File occupies a child stdio descriptor",
            ),
            FixedEngineFailurePhase::BeforeNativeEntry,
            None,
        )
        .with_live_file(producer_file));
    }
    let producer_before_version = match producer_file.file.metadata() {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_OBJECT_BYTES => {
            engine_object_version(&metadata)
        }
        Ok(_) => {
            return Err(FixedEngineNativeFailure::at(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "prearmed producer exceeds fixed regular-file bound",
                ),
                FixedEngineFailurePhase::BeforeNativeEntry,
                None,
            )
            .with_live_file(producer_file))
        }
        Err(error) => {
            return Err(FixedEngineNativeFailure::at(
                error,
                FixedEngineFailurePhase::BeforeNativeEntry,
                None,
            )
            .with_live_file(producer_file))
        }
    };
    let version = run_original_command(
        &producer,
        &[],
        true,
        Some(&producer_file),
        task.version_close_operation_id.uuid(),
        task.version_stdout_owner_id.uuid(),
        task.version_stdout_close_operation_id.uuid(),
        task.version_diagnostic_owner_id.uuid(),
        task.version_diagnostic_close_operation_id.uuid(),
        FixedEngineFailurePhase::VersionEntered,
        deadline,
    )
    .await;
    let version = match version {
        Ok(value) => value,
        Err(failure) => return Err(failure.with_live_file(producer_file)),
    };
    let ClosedCommand {
        stdout: version_stdout,
        stderr: version_stderr,
        root: version_root,
    } = version;
    let first_line = match std::str::from_utf8(&version_stdout)
        .ok()
        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
    {
        Some(value) => value,
        None => {
            return Err(FixedEngineNativeFailure::at(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "FFmpeg version output has no build line",
                ),
                FixedEngineFailurePhase::VersionClosedDependencyUnentered,
                None,
            )
            .with_version(version_root)
            .with_live_file(producer_file))
        }
    };
    let build_line = format!("{} ({first_line})", task.configured_ffmpeg_locator);
    let dependency = match run_original_command(
        &dependency_tool,
        &[],
        false,
        Some(&producer_file),
        task.dependency_close_operation_id.uuid(),
        task.dependency_stdout_owner_id.uuid(),
        task.dependency_stdout_close_operation_id.uuid(),
        task.dependency_diagnostic_owner_id.uuid(),
        task.dependency_diagnostic_close_operation_id.uuid(),
        FixedEngineFailurePhase::DependencyEntered,
        deadline,
    )
    .await
    {
        Ok(value) => value,
        Err(failure) => {
            return Err(failure
                .with_version(version_root)
                .with_live_file(producer_file))
        }
    };
    let ClosedCommand {
        stdout: dependency_stdout,
        stderr: _dependency_stderr,
        root: dependency_root,
    } = dependency;
    let mut paths = match dependency_paths_from_report(&dependency_stdout) {
        Ok(value) => value,
        Err(error) => {
            return Err(FixedEngineNativeFailure::at(
                error,
                FixedEngineFailurePhase::BothRootsClosedFilesPartial,
                None,
            )
            .with_version(version_root)
            .with_dependency(dependency_root)
            .with_live_file(producer_file))
        }
    };
    paths.push(producer.clone());
    paths.sort();
    paths.dedup();
    if paths.is_empty() || paths.len() > task.file_slots.len() {
        return Err(FixedEngineNativeFailure::at(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "FFmpeg dependency set exceeds prearmed file slots",
            ),
            FixedEngineFailurePhase::BothRootsClosedFilesPartial,
            None,
        )
        .with_version(version_root)
        .with_dependency(dependency_root)
        .with_live_file(producer_file));
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/engine\0");
    digest.update((version_stdout.len() as u64).to_be_bytes());
    digest.update(&version_stdout);
    digest.update((version_stderr.len() as u64).to_be_bytes());
    digest.update(&version_stderr);
    let mut closed_files = Vec::with_capacity(paths.len());
    let mut hash_buffer = BoundHashBuffer::new(&task);
    let mut object_digests = Vec::with_capacity(paths.len());
    let mut producer_object = None;
    // Hash and checked-close the same File that both child commands inherited.
    // The remaining file slots belong only to dependency paths from ldd.
    let producer_closed = match hash_bound_file(producer_file, &mut hash_buffer, deadline) {
        Ok(value) => value,
        Err(file_failure) => {
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("original producer File failed"),
                FixedEngineFailurePhase::BothRootsClosedFilesPartial,
                None,
            )
            .with_version(version_root)
            .with_dependency(dependency_root)
            .with_file_failure(file_failure, closed_files, hash_buffer))
        }
    };
    let mut producer_closed = Some(producer_closed);
    let mut dependency_slots = task.file_slots.iter().skip(1);
    for path in paths {
        let closed = if path == producer {
            producer_closed
                .take()
                .expect("original producer File closed once")
        } else {
            let slot = dependency_slots
                .next()
                .expect("prearmed dependency file slot");
            match hash_original_file(path, slot, &mut hash_buffer, deadline) {
                Ok(value) => value,
                Err(file_failure) => {
                    if let Some(producer_closed) = producer_closed.take() {
                        closed_files.push(producer_closed);
                    }
                    return Err(FixedEngineNativeFailure::at(
                        io::Error::other("original engine file failed"),
                        FixedEngineFailurePhase::BothRootsClosedFilesPartial,
                        None,
                    )
                    .with_version(version_root)
                    .with_dependency(dependency_root)
                    .with_file_failure(file_failure, closed_files, hash_buffer));
                }
            }
        };
        if closed.path == producer {
            producer_object = Some((
                format!("s1:{}", closed.object_version),
                hex::encode(closed.sha256),
            ));
        }
        object_digests.push(closed.sha256.to_vec());
        closed_files.push(closed);
    }
    // The prefix of slots handed out above is exactly the opened set. Attest
    // the never-opened suffix here, where the slot iterator is still owned.
    let unused = dependency_slots.count();
    let unused_slots = match u16::try_from(unused) {
        Ok(count) if closed_files.len().checked_add(unused) == Some(task.file_slots.len()) => {
            FixedEngineUnusedSlots { count }
        }
        _ => {
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("engine file-slot accounting differs from the opened prefix"),
                FixedEngineFailurePhase::AllNativeClosedLogicalFailure,
                None,
            )
            .with_version(version_root)
            .with_dependency(dependency_root)
            .with_files(closed_files, Some(hash_buffer.checked_close())))
        }
    };
    object_digests.sort();
    for object_digest in object_digests {
        digest.update((object_digest.len() as u64).to_be_bytes());
        digest.update(object_digest);
    }
    let (producer_object_version, producer_sha256) = match producer_object {
        Some(value) => value,
        None => {
            return Err(FixedEngineNativeFailure::at(
                io::Error::other("prearmed FFmpeg executable File was not hashed"),
                FixedEngineFailurePhase::AllNativeClosedLogicalFailure,
                None,
            )
            .with_version(version_root)
            .with_dependency(dependency_root)
            .with_files(closed_files, Some(hash_buffer.checked_close())))
        }
    };
    let producer_after = match std::fs::metadata(&producer) {
        Ok(value) => engine_object_version(&value),
        Err(error) => {
            return Err(FixedEngineNativeFailure::at(
                error,
                FixedEngineFailurePhase::AllNativeClosedLogicalFailure,
                None,
            )
            .with_version(version_root)
            .with_dependency(dependency_root)
            .with_files(closed_files, Some(hash_buffer.checked_close())))
        }
    };
    if producer_object_version != format!("s1:{producer_before_version}")
        || format!("s1:{producer_after}") != producer_object_version
    {
        return Err(FixedEngineNativeFailure::at(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "configured FFmpeg object changed across version, dependency and hash collection",
            ),
            FixedEngineFailurePhase::AllNativeClosedLogicalFailure,
            None,
        )
        .with_version(version_root)
        .with_dependency(dependency_root)
        .with_files(closed_files, Some(hash_buffer.checked_close())));
    }
    let dependencies = closed_files
        .iter()
        .map(|file| (file.path.clone(), file.object_version.clone()))
        .collect();
    Ok(FixedEngineNativeCompletion {
        unused_slots,
        producer_path: producer,
        producer_object_version,
        producer_sha256,
        dependencies,
        media_digest: hex::encode(digest.finalize()),
        build_line,
        boot_id,
        version_root,
        dependency_root,
        closed_files,
        hash_buffer: hash_buffer.checked_close(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn original_engine_child_reads_prearmed_file_after_path_replacement() {
        let cat = ["/usr/bin/cat", "/bin/cat"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("host cat executable");
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("producer");
        std::fs::write(&path, b"original image").expect("write original fixture");
        let owner_id = Uuid::new_v4();
        let slot = crate::resource_custody::codec_v2::FixedPlanningEngineFileSlotV1 {
            owner_id: crate::resource_custody::codec_v2::Id::new(owner_id).expect("owner id"),
            close_operation_id: crate::resource_custody::codec_v2::Id::new(Uuid::new_v4())
                .expect("close operation"),
        };
        let owner = BoundEngineFile::open(path.clone(), &slot).expect("prearmed original File");
        std::fs::rename(&path, directory.path().join("replaced")).expect("replace original path");
        std::fs::write(&path, b"replacement image").expect("write replacement");
        let ids = [(); 5].map(|_| Uuid::new_v4());
        let result = run_original_command(
            cat,
            &[],
            false,
            Some(&owner),
            ids[0],
            ids[1],
            ids[2],
            ids[3],
            ids[4],
            FixedEngineFailurePhase::DependencyEntered,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap_or_else(|failure| panic!("original child: {}", failure.error));
        assert_eq!(result.stdout, b"original image");
        assert_eq!(
            result.root.inherited_producer_file_owner_id(),
            Some(owner_id)
        );
    }

    #[tokio::test]
    async fn timed_out_original_engine_child_retains_and_cleans_its_own_handles() {
        let executable = ["/usr/bin/sleep", "/bin/sleep"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("host sleep executable");
        let ids = [(); 5].map(|_| Uuid::new_v4());
        let mut failure = match run_original_command(
            executable,
            &[Path::new("60")],
            false,
            None,
            ids[0],
            ids[1],
            ids[2],
            ids[3],
            ids[4],
            FixedEngineFailurePhase::VersionEntered,
            Instant::now() + Duration::from_millis(150),
        )
        .await
        {
            Ok(_) => panic!("sleep should exceed the original command deadline"),
            Err(failure) => failure,
        };
        assert_eq!(failure.phase, FixedEngineFailurePhase::VersionEntered);
        let original = failure.command.as_ref().expect("entered original child");
        assert!(original.child.is_some());
        assert!(original.job.is_some());
        failure
            .retry_cleanup(Instant::now() + Duration::from_secs(5))
            .await
            .expect("retry original child cleanup");
        let original = failure
            .command
            .as_ref()
            .expect("retained original cleanup evidence");
        assert!(original.child.is_none());
        assert!(original.job.is_none());
        assert!(original.reaped_status.is_some());
        assert!(original.original_group_absent);
        assert!(original.stdout_closed.is_some());
        assert!(original.stderr_closed.is_some());
        assert!(original.pidfd_checked_closed);
        assert!(original.uncertain_native_fds.is_empty());
    }
}

#[cfg(test)]
#[path = "fixed_planning_engine/tests.rs"]
pub(crate) mod token_tests;
