//! Descriptor-bound decoder fact collection and its bounded node-local cache.
//!
//! The source handle is opened and authorized by the preparation owner. This
//! module never reopens its pathname: FFprobe receives a duplicate of that
//! exact handle, and metadata is checked again after probing before the facts
//! can be returned or cached.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
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

/// A held source descriptor and the exclusive ownership lane for every child
/// that can seek its shared open-file description.
#[derive(Clone)]
pub(crate) struct DecodeFactSource {
    handle: Arc<std::fs::File>,
    offset_gate: Arc<tokio::sync::Semaphore>,
}

impl DecodeFactSource {
    pub(crate) fn new(
        handle: Arc<std::fs::File>,
        offset_gate: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            handle,
            offset_gate,
        }
    }

    #[cfg(test)]
    fn isolated(handle: Arc<std::fs::File>) -> Self {
        Self::new(handle, Arc::new(tokio::sync::Semaphore::new(1)))
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

/// Startup-bound FFprobe identity. Linux executes the held snapshot descriptor;
/// macOS executes a snapshot protected from mutation or replacement by the
/// user-immutable file flag. Path revalidation before and after collection
/// detects replacement of the configured executable without allowing a
/// transient swap to choose a different executable object.
#[derive(Debug, Clone)]
pub(crate) struct DecodeProbeIdentity {
    executable: PathBuf,
    executable_file: Arc<std::fs::File>,
    executable_snapshot: Arc<ExecutableSnapshot>,
    build_digest: String,
    file: ProbeFileIdentity,
    snapshot_file: ProbeFileIdentity,
}

#[derive(Debug)]
struct ExecutableSnapshot {
    file: std::fs::File,
    path: tempfile::TempPath,
}

impl ExecutableSnapshot {
    fn as_file(&self) -> &std::fs::File {
        &self.file
    }

    #[cfg(any(test, not(target_os = "linux")))]
    fn path(&self) -> &Path {
        self.path.as_ref()
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
        let version = probe_version(&executable_snapshot, &executable).await?;
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
    process_group: Option<libc::pid_t>,
) {
    kill_probe_session(process_group);
    let _ = child.wait().await;
}

#[cfg(unix)]
fn kill_probe_session(process_group: Option<libc::pid_t>) {
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(unix)]
fn snapshot_executable(source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    use std::os::unix::fs::{FileExt, PermissionsExt};

    let mut snapshot = tempfile::Builder::new()
        .prefix("plurx-ffprobe-")
        .tempfile()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
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
    Ok(ExecutableSnapshot { file, path })
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
    executable: &ExecutableSnapshot,
    configured_path: &Path,
) -> Result<Vec<u8>, DecodeFactError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let executable_fd = executable.as_file().as_raw_fd();
    let mut command = tokio::process::Command::new(snapshot_execution_path(executable));
    command.as_std_mut().arg0(configured_path);
    command
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            start_probe_session()?;
            install_child_fd(executable_fd, HELD_PROBE_FD)
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
    let stdout = child.stdout.take().ok_or(DecodeFactError::MissingPipe)?;
    let outcome = tokio::time::timeout(VERSION_DEADLINE, async {
        let (stdout, status) = tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), child.wait());
        (stdout, status)
    })
    .await;
    let (stdout, status) = match outcome {
        Ok((stdout, status)) => (
            stdout?,
            status.map_err(|error| DecodeFactError::Read(error.to_string()))?,
        ),
        Err(_) => {
            terminate_probe_session(&mut child, process_group).await;
            return Err(DecodeFactError::Deadline);
        }
    };
    kill_probe_session(process_group);
    if stdout.1 || stdout.0.is_empty() || !status.success() {
        return Err(DecodeFactError::ProbeIdentity(
            "bounded version probe failed".to_owned(),
        ));
    }
    Ok(stdout.0)
}

#[cfg(not(unix))]
async fn probe_version(
    _executable: &ExecutableSnapshot,
    _configured_path: &Path,
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
        let bound_identity = source_identity(&source.handle)?;
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
            let _source_offset_permit = source_offset_permit;
            let result = collect(
                &owned_probe,
                &source.handle,
                bound_identity,
                owned_catalog.as_ref(),
                selected_stream,
                remaining,
                owned_cancelled.as_ref(),
            )
            .await;
            (result, gate, source)
        });
        let (facts, gate, source) = collection
            .await
            .map_err(|error| DecodeFactError::Read(error.to_string()))?;
        let facts = facts?;
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        probe.validate_current(remaining, cancelled).await?;
        if source_identity(&source.handle)? != key.source {
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
    #[cfg(not(unix))]
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
            #[cfg(not(unix))]
            Self::UnsupportedPlatform => formatter
                .write_str("descriptor-bound decoder probing is unsupported on this platform"),
        }
    }
}

impl std::error::Error for DecodeFactError {}

#[cfg(unix)]
fn source_identity(source: &std::fs::File) -> Result<DecodeSourceIdentity, DecodeFactError> {
    use std::os::unix::fs::MetadataExt;
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
    DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(body.to_string().as_bytes())))
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))
}

#[cfg(not(unix))]
fn source_identity(_source: &std::fs::File) -> Result<DecodeSourceIdentity, DecodeFactError> {
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
    source: &std::fs::File,
    before: DecodeSourceIdentity,
    catalog: Option<&DecodeCatalogMetadata>,
    selected_stream: ProbeStreamSelection,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<DecodeFacts, DecodeFactError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let source_fd = source.as_raw_fd();
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
    let _restore_offset = RestoreOffset {
        fd: source_fd,
        offset: source_offset,
    };
    let executable_fd = probe.executable_snapshot.as_file().as_raw_fd();
    let mut command =
        tokio::process::Command::new(snapshot_execution_path(&probe.executable_snapshot));
    command.as_std_mut().arg0(probe.executable());
    #[cfg(test)]
    command.env("PLURX_TEST_PROBE_PATH", probe.executable());
    command.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_entries",
        "stream=index,codec_type,codec_name,profile,pix_fmt,width,height,bits_per_raw_sample,avg_frame_rate,r_frame_rate,color_range,color_space,color_transfer,color_primaries:stream_disposition=attached_pic:stream_side_data=side_data_type",
        "-show_streams",
        "/dev/fd/3",
    ]);
    unsafe {
        command.pre_exec(move || {
            start_probe_session()?;
            install_probe_child_fds(source_fd, executable_fd)
        });
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
    let stdout = child.stdout.take().ok_or(DecodeFactError::MissingPipe)?;
    let stderr = child.stderr.take().ok_or(DecodeFactError::MissingPipe)?;
    let outcome = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => {
            terminate_probe_session(&mut child, process_group).await;
            return Err(DecodeFactError::Cancelled);
        }
        outcome = tokio::time::timeout(budget.min(PROBE_DEADLINE), async {
            let (stdout, stderr, status) = tokio::join!(
                read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                child.wait(),
            );
            (stdout, stderr, status)
        }) => outcome,
    };
    let (stdout, stderr, status) = match outcome {
        Ok((stdout, stderr, status)) => (
            stdout?,
            stderr?,
            status.map_err(|error| DecodeFactError::Read(error.to_string()))?,
        ),
        Err(_) => {
            terminate_probe_session(&mut child, process_group).await;
            return Err(DecodeFactError::Deadline);
        }
    };
    kill_probe_session(process_group);
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
    if source_identity(source)? != before {
        return Err(DecodeFactError::SourceChanged);
    }
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
    _source: &std::fs::File,
    _before: DecodeSourceIdentity,
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

    #[cfg(unix)]
    fn executable(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).expect("write probe");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("make probe executable");
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let error = DecodeProbeIdentity::discover("plurx-no-such-ffprobe")
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let before = source_identity(&source).expect("source identity");
        let facts = collect(
            &identity,
            &source,
            before,
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

    #[cfg(unix)]
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let snapshot_path = identity.executable_snapshot.path().to_owned();
        let held_path = snapshot_path.with_extension("held");
        #[cfg(target_os = "linux")]
        {
            std::fs::rename(&snapshot_path, &held_path).expect("move snapshot pathname");
            executable(
                &snapshot_path,
                r###"#!/bin/sh
touch "$PLURX_TEST_PROBE_PATH.snapshot-b"
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","profile":"Main","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
            );
        }
        #[cfg(target_os = "macos")]
        assert!(
            std::fs::rename(&snapshot_path, &held_path).is_err(),
            "the immutable snapshot path must refuse replacement"
        );
        let before = source_identity(&source).expect("source identity");
        let facts = collect(
            &identity,
            &source,
            before,
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
        #[cfg(target_os = "linux")]
        {
            std::fs::remove_file(&snapshot_path).expect("remove replacement snapshot");
            std::fs::remove_file(&held_path).expect("remove displaced snapshot link");
        }
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
        let _permit = offset_gate
            .clone()
            .try_acquire_owned()
            .expect("source lease released only after child reap");
        let mut view = source.try_clone().expect("source view");
        assert_eq!(view.stream_position().expect("restored source offset"), 3);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !probe.with_extension("descendant").exists(),
            "cancellation must terminate every inherited-descriptor descendant"
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
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
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
