//! Bounded ownership for short-lived helper processes.
//!
//! Startup probes are evidence, not services. They receive a small explicit
//! environment, bounded output and wall time, and one owner that kills its
//! process group and joins the original leader and pipe tasks. These joins
//! alone do not prove complete native-group absence.

use std::ffi::OsStr;
use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

mod pause;
#[cfg(target_os = "linux")]
pub use pause::verified_group as source_group_quiescent;
pub use pause::{
    pausable, source_pause_controller, ChildPauseGuard, SourcePauseController,
    SourcePauseObservation, SourcePausePhase, SourceQuotaWait,
};

tokio::task_local! {
    static WORK_CANCELLATION: CancellationToken;
    static ORIGINAL_WORK_KEEPER: Arc<dyn OriginalWorkKeeper>;
}

/// Give a directly awaited operation cooperative cancellation. The caller must
/// signal this token and await the operation, not drop it: owned children reap
/// before returning. Spawned tasks deliberately do not inherit this scope.
pub async fn cancellable<T>(
    cancellation: CancellationToken,
    operation: impl std::future::Future<Output = T>,
) -> T {
    WORK_CANCELLATION.scope(cancellation, operation).await
}

/// Diagnostic uncertainty only; none of these values grants native closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OriginalWorkUncertainty {
    CollectorDropped,
    LeaderReapFailed,
    PipeDrainFailed,
    RuntimeUnavailable,
}

/// An opt-in caller retains its SAME admitted owner through original child
/// reaping and original pipe tasks. The callback must remain fail closed;
/// success of a leader wait cannot clear an earlier uncertainty.
pub trait OriginalWorkKeeper: Send + Sync {
    fn retain_uncertainty(&self, reason: OriginalWorkUncertainty);

    /// Some incumbent lanes may overlap a trial only after the original
    /// trusted helper's process group has also stopped doing work.
    #[cfg(target_os = "linux")]
    fn requires_group_quiescence(&self) -> bool {
        false
    }
}

/// Directly awaited bounded work inherits this original keeper. Spawned work
/// does not inherit task locals; this module explicitly carries the keeper to
/// its own reaper and pipe tasks. This scope grants no native absence proof.
pub async fn with_original_work_keeper<T>(
    keeper: Arc<dyn OriginalWorkKeeper>,
    operation: impl std::future::Future<Output = T>,
) -> T {
    ORIGINAL_WORK_KEEPER.scope(keeper, operation).await
}

/// Clone only the same opt-in original owner. Existing worker launchers must
/// capture this before spawning: task locals are not inherited by workers.
/// Presence/completion supplies no acquisition, native absence or refund proof.
pub fn original_keeper() -> Option<Arc<dyn OriginalWorkKeeper>> {
    ORIGINAL_WORK_KEEPER.try_with(Arc::clone).ok()
}

fn retain_uncertainty(
    keeper: &Option<Arc<dyn OriginalWorkKeeper>>,
    reason: OriginalWorkUncertainty,
) {
    if let Some(keeper) = keeper {
        keeper.retain_uncertainty(reason);
    }
}

struct CollectorKeeper {
    keeper: Option<Arc<dyn OriginalWorkKeeper>>,
    completed: bool,
}

impl Drop for CollectorKeeper {
    fn drop(&mut self) {
        if !self.completed {
            retain_uncertainty(&self.keeper, OriginalWorkUncertainty::CollectorDropped);
        }
    }
}

/// Capture the current operation's cancellation for explicitly owned blocking
/// helpers. Task-local scopes do not automatically propagate into worker threads;
/// callers must keep awaiting those helpers until their reads have settled.
pub fn cancellation() -> Option<CancellationToken> {
    WORK_CANCELLATION.try_with(Clone::clone).ok()
}

pub fn check_cancellation() -> io::Result<()> {
    if WORK_CANCELLATION
        .try_with(CancellationToken::is_cancelled)
        .unwrap_or(false)
    {
        Err(io::Error::new(io::ErrorKind::Interrupted, "work cancelled"))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

struct OwnedChild {
    child: Option<Child>,
    job: Option<super::ChildJob>,
    keeper: Option<Arc<dyn OriginalWorkKeeper>>,
    #[cfg(unix)]
    process_group: Option<i32>,
}

impl OwnedChild {
    fn new(child: Child, job: super::ChildJob) -> Self {
        Self {
            job: Some(job),
            keeper: original_keeper(),
            #[cfg(unix)]
            process_group: child.id().and_then(|pid| i32::try_from(pid).ok()),
            child: Some(child),
        }
    }

    async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self
            .child
            .as_mut()
            .expect("owned process is present")
            .wait()
            .await?;
        self.child = None;
        Ok(status)
    }

    async fn wait_pausable(
        &mut self,
        guard: &ChildPauseGuard,
        active_budget: Duration,
    ) -> io::Result<ExitStatus> {
        let process_id = self
            .child
            .as_ref()
            .and_then(Child::id)
            .ok_or_else(|| io::Error::other("source child has no live process identity"))?;
        guard
            .run_group(process_id, active_budget, self.wait())
            .await
    }

    #[cfg(target_os = "linux")]
    async fn wait_group_settled(group: Option<i32>) {
        if let Some(group) = group {
            // Returning a killed leader is not proof that a D-state helper
            // stopped reading. Keep the cleanup owner until all group threads
            // are dead or gone; neither a timeout nor SIGKILL invents credit.
            while !pause::verified_group(group, false).await.unwrap_or(false) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    async fn kill_and_reap(&mut self) {
        self.kill_process_group();
        // Borrow the SAME original child across await. If this cleanup waiter
        // is dropped, OwnedChild::Drop still owns child + Job + keeper and can
        // transfer them together to its original reaper.
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            match child.wait().await {
                Ok(_) => self.child = None,
                Err(_) => {
                    retain_uncertainty(&self.keeper, OriginalWorkUncertainty::LeaderReapFailed)
                }
            }
        }
    }

    fn kill_process_group(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.process_group.take() {
            // SAFETY: the child was placed in a new process group whose id is
            // its pid. A negative pid addresses only that owned group.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.process_group.take() {
            // SAFETY: see `kill_and_reap`; Drop owns the same process group.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        retain_uncertainty(&self.keeper, OriginalWorkUncertainty::CollectorDropped);
        let _ = child.start_kill();
        let job = self.job.take();
        let keeper = self.keeper.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                // Keep the actual original Job and admitted owner through the
                // leader wait. This wait alone does not prove group absence.
                let original_job = job;
                if child.wait().await.is_err() {
                    retain_uncertainty(&keeper, OriginalWorkUncertainty::LeaderReapFailed);
                }
                drop(child);
                drop(original_job);
                drop(keeper);
            });
        } else {
            retain_uncertainty(&keeper, OriginalWorkUncertainty::RuntimeUnavailable);
            drop(child);
            drop(job);
            drop(keeper);
        }
    }
}

pub async fn output<P, A>(
    program: P,
    args: &[A],
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
) -> io::Result<Output>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args);
    output_command(&mut command, wall_time, max_output_bytes, work).await
}

/// A bounded child with caller-installed held descriptors and arguments.
/// The environment and group lifetime remain identical to `output`.
pub async fn output_command(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
) -> io::Result<Output> {
    output_command_environment(
        command,
        wall_time,
        max_output_bytes,
        work,
        Environment::Service,
        None,
        &[],
        None,
        None,
    )
    .await
}

/// Bounded CPU child with held descriptors and an LC_ALL-only environment.
/// Absolute executable and library-qualified package paths are required.
pub async fn output_command_clean_env(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
) -> io::Result<Output> {
    output_command_clean_environment(command, wall_time, max_output_bytes, work, None).await
}

/// A bounded clean-environment child launched under its prepared descriptor manifest.
pub async fn output_command_clean_env_with_descriptors(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    manifest: std::sync::Arc<super::source_lifetime::DescriptorManifest>,
) -> io::Result<Output> {
    output_command_clean_environment(command, wall_time, max_output_bytes, work, Some(manifest))
        .await
}

async fn output_command_clean_environment(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    manifest: Option<std::sync::Arc<super::source_lifetime::DescriptorManifest>>,
) -> io::Result<Output> {
    if !std::path::Path::new(command.as_std().get_program()).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed child executable must be absolute",
        ));
    }
    output_command_environment(
        command,
        wall_time,
        max_output_bytes,
        work,
        Environment::Clean,
        None,
        &[],
        None,
        manifest,
    )
    .await
}

/// Native host tools may require their existing user/IPC environment. Keep
/// that environment intact while using the same bounded process-group owner.
/// Inspection probes must continue using the service or clean environment APIs.
pub async fn output_command_inherited_env(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
) -> io::Result<Output> {
    output_command_environment(
        command,
        wall_time,
        max_output_bytes,
        work,
        Environment::Inherited,
        None,
        &[],
        None,
        None,
    )
    .await
}

/// A clean-environment child of a validated helper bundle: `LC_ALL` and only
/// the bundle's own declared variables (its library path), never the
/// daemon's environment. Otherwise identical to [`output_command_clean_env`].
pub async fn output_command_bundle_env(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    bundle_env: &[(String, std::ffi::OsString)],
) -> io::Result<Output> {
    if !std::path::Path::new(command.as_std().get_program()).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed child executable must be absolute",
        ));
    }
    output_command_environment(
        command,
        wall_time,
        max_output_bytes,
        work,
        Environment::Clean,
        None,
        bundle_env,
        None,
        None,
    )
    .await
}

/// [`output_command_bundle_env`] with the child's stdout written straight to
/// `stdout` (an open, writable file) as it is produced, so whatever a long
/// run printed survives its interruption. The returned output's `stdout` is
/// empty; `max_stderr_bytes` bounds stderr and `max_stdout_bytes` bounds
/// the child's regular-file writes. Exceeding it terminates the writer.
/// A supplied manifest must come from the audited descriptor builder, including
/// its installed file-stdio mappings; it is bound after final configuration.
#[expect(
    clippy::too_many_arguments,
    reason = "file-backed output retains its final descriptor manifest through the bounded owner"
)]
pub async fn output_command_bundle_env_to_file(
    command: &mut Command,
    wall_time: Duration,
    max_stderr_bytes: usize,
    max_stdout_bytes: usize,
    work: super::ChildWork,
    bundle_env: &[(String, std::ffi::OsString)],
    stdout: std::fs::File,
    manifest: Option<std::sync::Arc<super::source_lifetime::DescriptorManifest>>,
) -> io::Result<Output> {
    if !std::path::Path::new(command.as_std().get_program()).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed child executable must be absolute",
        ));
    }
    #[cfg(unix)]
    {
        // Keep stdout as a direct child descriptor: cancellation/reaping
        // then settles every write before the caller reads its progress.
        // A separate async file pump could outlive cancellation with an
        // in-flight filesystem write. The kernel bounds the child's file.
        super::child_limits::file_size(command, max_stdout_bytes);
    }
    #[cfg(not(unix))]
    {
        let _ = max_stdout_bytes;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file-backed helper output needs Unix file limits",
        ));
    }
    output_command_environment(
        command,
        wall_time,
        max_stderr_bytes,
        work,
        Environment::Clean,
        None,
        bundle_env,
        Some(stdout),
        manifest,
    )
    .await
}

/// A clean-environment child whose stderr retains only selected complete lines.
/// The selector receives each line with its original newline, if present. All
/// stderr is drained; selected overflow or any line above 8 KiB fails at EOF.
/// Stdout and child lifetime/cancellation retain the ordinary bounded semantics.
pub async fn output_command_clean_env_selected_stderr(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    select: fn(&[u8]) -> bool,
) -> io::Result<Output> {
    if !std::path::Path::new(command.as_std().get_program()).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed child executable must be absolute",
        ));
    }
    output_command_environment(
        command,
        wall_time,
        max_output_bytes,
        work,
        Environment::Clean,
        Some(select),
        &[],
        None,
        None,
    )
    .await
}

#[derive(Clone, Copy)]
enum Environment {
    Service,
    Clean,
    Inherited,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one process owner binds environment, pipe selection and retained output destination"
)]
async fn output_command_environment(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    environment: Environment,
    stderr_selector: Option<fn(&[u8]) -> bool>,
    bundle_env: &[(String, std::ffi::OsString)],
    stdout_file: Option<std::fs::File>,
    manifest: Option<std::sync::Arc<super::source_lifetime::DescriptorManifest>>,
) -> io::Result<Output> {
    check_cancellation()?;
    let cancellation = cancellation().unwrap_or_default();
    if !matches!(environment, Environment::Inherited) {
        let descriptor_handoff = super::source_lifetime::has_descriptor_handoff(command.as_std());
        command.env_clear().env("LC_ALL", "C");
        if descriptor_handoff {
            super::source_lifetime::tag_descriptor_handoff(command);
        }
    }
    for (name, value) in bundle_env {
        command.env(name, value);
    }
    let stdout_to_file = stdout_file.is_some();
    command
        .stdin(std::process::Stdio::null())
        .stdout(stdout_file.map_or_else(std::process::Stdio::piped, std::process::Stdio::from))
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if matches!(environment, Environment::Service) {
        for name in [
            "PATH",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "LIBVA_DRIVER_NAME",
            "LIBVA_DRIVERS_PATH",
            "VDPAU_DRIVER",
            "CUDA_VISIBLE_DEVICES",
            "NVIDIA_VISIBLE_DEVICES",
            "NVIDIA_DRIVER_CAPABILITIES",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }
    #[cfg(unix)]
    command.process_group(0);

    let controller = source_pause_controller();
    #[cfg(not(target_os = "linux"))]
    if controller.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "source pause requires verified Linux process groups",
        ));
    }
    let pause_guard = match controller.as_ref() {
        Some(controller) => Some(controller.enlist(&cancellation).await?),
        None => None,
    };
    let (child, job) = if let Some(manifest) = manifest {
        manifest.bind(command.as_std())?;
        super::source_lifetime::with_descriptors(manifest, || {
            super::spawn_job_owned(command, work)
        })?
    } else {
        super::spawn_job_owned(command, work)?
    };
    let mut child = OwnedChild::new(child, job);
    let mut collector = CollectorKeeper {
        keeper: original_keeper(),
        completed: false,
    };
    let stdout = child.child.as_mut().expect("owned child").stdout.take();
    if stdout.is_none() && !stdout_to_file {
        return Err(io::Error::other("probe stdout was not piped"));
    }
    let stderr = child
        .child
        .as_mut()
        .expect("owned child")
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("probe stderr was not piped"))?;
    // Move the complete original pipe and keeper into each task together.
    let mut stdout = stdout.map(|reader| {
        let pipe = OriginalPipe {
            reader,
            keeper: original_keeper(),
        };
        tokio::spawn(drain_capped_owned(pipe, max_output_bytes, None))
    });
    let stderr = OriginalPipe {
        reader: stderr,
        keeper: original_keeper(),
    };
    let mut stderr = Some(tokio::spawn(drain_capped_owned(
        stderr,
        max_output_bytes,
        stderr_selector,
    )));
    #[cfg(target_os = "linux")]
    let source_group = (pause_guard.is_some()
        || collector
            .keeper
            .as_ref()
            .is_some_and(|keeper| keeper.requires_group_quiescence()))
    .then_some(child.process_group)
    .flatten();
    let deadline = controller.as_ref().map_or_else(
        || tokio::time::Instant::now() + wall_time,
        |controller| controller.hard_deadline(wall_time),
    );

    let completed = tokio::select! {
        biased;
        () = cancellation.cancelled() => Ok(Err(io::Error::new(
            io::ErrorKind::Interrupted, "work cancelled"))),
        result = tokio::time::timeout_at(deadline, async {
        let status = match pause_guard.as_ref() {
            Some(guard) => child.wait_pausable(guard, wall_time).await?,
            None => child.wait().await?,
        };
        // A short-lived probe may not daemonize. Once its leader exits, kill
        // any descendant left in the owned group before waiting for pipe EOF.
        child.kill_process_group();
        #[cfg(target_os = "linux")]
        OwnedChild::wait_group_settled(source_group).await;
        let stdout = if stdout.is_some() {
            join_original_pipe(&mut stdout, &collector.keeper).await?
        } else {
            Vec::new()
        };
        let stderr = join_original_pipe(&mut stderr, &collector.keeper).await?;
        Ok::<_, io::Error>(Output {
            status,
            stdout,
            stderr,
        })
        }) => result,
    };
    let result = match completed {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => {
            child.kill_and_reap().await;
            #[cfg(target_os = "linux")]
            OwnedChild::wait_group_settled(source_group).await;
            abort_and_join_original_pipe(&mut stdout, &collector.keeper).await;
            abort_and_join_original_pipe(&mut stderr, &collector.keeper).await;
            Err(error)
        }
        Err(_) => {
            child.kill_and_reap().await;
            #[cfg(target_os = "linux")]
            OwnedChild::wait_group_settled(source_group).await;
            abort_and_join_original_pipe(&mut stdout, &collector.keeper).await;
            abort_and_join_original_pipe(&mut stderr, &collector.keeper).await;
            Err(io::Error::new(io::ErrorKind::TimedOut, "probe timed out"))
        }
    };
    collector.completed = true;
    result
}

// The reader field drops before the original keeper, including task abort or
// panic. No replacement reader or native authority is constructed.
struct OriginalPipe<R> {
    reader: R,
    keeper: Option<Arc<dyn OriginalWorkKeeper>>,
}

async fn drain_capped_owned<R: AsyncRead + Unpin>(
    mut original: OriginalPipe<R>,
    cap: usize,
    select: Option<fn(&[u8]) -> bool>,
) -> io::Result<Vec<u8>> {
    let result = match select {
        Some(select) => drain_selected_lines(&mut original.reader, cap, select).await,
        None => drain_capped(&mut original.reader, cap).await,
    };
    if result.is_err() {
        retain_uncertainty(&original.keeper, OriginalWorkUncertainty::PipeDrainFailed);
    }
    result
}

type OriginalPipeTask = Option<tokio::task::JoinHandle<io::Result<Vec<u8>>>>;

async fn join_original_pipe(
    task: &mut OriginalPipeTask,
    keeper: &Option<Arc<dyn OriginalWorkKeeper>>,
) -> io::Result<Vec<u8>> {
    // Borrow the SAME handle across await; cancellation leaves it in its slot.
    let result = task.as_mut().expect("original pipe task").await;
    task.take();
    match result {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => {
            retain_uncertainty(keeper, OriginalWorkUncertainty::PipeDrainFailed);
            Err(error)
        }
        Err(error) => {
            retain_uncertainty(keeper, OriginalWorkUncertainty::PipeDrainFailed);
            Err(io::Error::other(error.to_string()))
        }
    }
}

async fn abort_and_join_original_pipe(
    task: &mut OriginalPipeTask,
    keeper: &Option<Arc<dyn OriginalWorkKeeper>>,
) {
    if let Some(task) = task.take() {
        task.abort();
        match task.await {
            Ok(Ok(_)) => {}
            Err(error) if error.is_cancelled() => {}
            _ => retain_uncertainty(keeper, OriginalWorkUncertainty::PipeDrainFailed),
        }
    }
}

/// Keep draining after the retained cap. Closing the reader at the cap can
/// fill the child's pipe and deadlock it before the wall-time owner can reap.
pub(super) async fn drain_capped(
    mut reader: impl AsyncRead + Unpin,
    max_output_bytes: usize,
) -> io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(max_output_bytes.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(retained);
        }
        let keep = read.min(max_output_bytes.saturating_sub(retained.len()));
        retained.extend_from_slice(&chunk[..keep]);
    }
}

const SELECTED_STDERR_LINE_BYTES: usize = 8 * 1024;

/// Retention is bounded independently of ignored metadata. Invalid input is
/// remembered until EOF so no early error can close a full child pipe.
async fn drain_selected_lines(
    mut reader: impl AsyncRead + Unpin,
    max_output_bytes: usize,
    select: fn(&[u8]) -> bool,
) -> io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(max_output_bytes.min(SELECTED_STDERR_LINE_BYTES));
    let mut line = Vec::with_capacity(SELECTED_STDERR_LINE_BYTES);
    let mut chunk = [0_u8; 8 * 1024];
    let mut discarding_line = false;
    let mut invalid = None;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            if !discarding_line && !line.is_empty() && select(&line) {
                if line.len() > max_output_bytes.saturating_sub(retained.len()) {
                    invalid = Some("selected stderr exceeds retained byte limit");
                } else {
                    retained.reserve_exact(line.len());
                    retained.extend_from_slice(&line);
                }
            }
            return match invalid {
                Some(reason) => Err(io::Error::new(io::ErrorKind::InvalidData, reason)),
                None => Ok(retained),
            };
        }
        for byte in &chunk[..read] {
            if !discarding_line {
                if line.len() == SELECTED_STDERR_LINE_BYTES {
                    invalid = Some("stderr line exceeds 8 KiB");
                    discarding_line = true;
                    line.clear();
                } else {
                    line.push(*byte);
                }
            }
            if *byte == b'\n' {
                if !discarding_line && select(&line) {
                    if line.len() > max_output_bytes.saturating_sub(retained.len()) {
                        invalid = Some("selected stderr exceeds retained byte limit");
                    } else {
                        retained.reserve_exact(line.len());
                        retained.extend_from_slice(&line);
                    }
                }
                line.clear();
                discarding_line = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_WORK: super::super::ChildWork = super::super::ChildWork::background("bounded test");

    #[test]
    fn unpolled_pipe_future_drops_reader_before_original_keeper() {
        struct Reader {
            drops: Arc<std::sync::Mutex<Vec<&'static str>>>,
        }
        impl AsyncRead for Reader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buffer: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                panic!("the original drain future must never be polled");
            }
        }
        impl Drop for Reader {
            fn drop(&mut self) {
                self.drops.lock().expect("reader drop trace").push("reader");
            }
        }
        struct Keeper {
            drops: Arc<std::sync::Mutex<Vec<&'static str>>>,
        }
        impl OriginalWorkKeeper for Keeper {
            fn retain_uncertainty(&self, _reason: OriginalWorkUncertainty) {
                panic!("an unentered pipe cannot invent native uncertainty");
            }
        }
        impl Drop for Keeper {
            fn drop(&mut self) {
                self.drops.lock().expect("keeper drop trace").push("keeper");
            }
        }
        let drops = Arc::new(std::sync::Mutex::new(Vec::new()));
        let original = OriginalPipe {
            reader: Reader {
                drops: Arc::clone(&drops),
            },
            keeper: Some(Arc::new(Keeper {
                drops: Arc::clone(&drops),
            })),
        };
        let unpolled = drain_capped_owned(original, 8, None);
        drop(unpolled);
        assert_eq!(
            *drops.lock().expect("actual drop order"),
            ["reader", "keeper"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_output_is_kernel_bounded_and_owned_until_reaped() {
        let dir = tempfile::tempdir().expect("scratch");
        let path = dir.path().join("stdout");
        let file = std::fs::File::create(&path).expect("stdout file");
        let mut command = Command::new("/usr/bin/head");
        command.args(["-c", "131072", "/dev/zero"]);
        let output = output_command_bundle_env_to_file(
            &mut command,
            Duration::from_secs(5),
            1024,
            17,
            TEST_WORK,
            &[],
            file,
            None,
        )
        .await
        .expect("file limit still reaps the owned child");
        assert!(
            !output.status.success(),
            "the writer must observe its file limit"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(std::fs::read(path).expect("saved prefix"), vec![0; 17]);
    }

    fn selected_record(line: &[u8]) -> bool {
        line.starts_with(b"frame:")
    }

    #[tokio::test]
    async fn selected_stderr_ignores_large_metadata_and_preserves_split_records() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(11);
        let writing = tokio::spawn(async move {
            writer.write_all(b"frame: first\n").await?;
            for _ in 0..32768 {
                writer
                    .write_all(b"ignored metadata continuation and side data\n")
                    .await?;
            }
            writer.write_all(b"frame: final without newline").await
        });
        let retained = tokio::time::timeout(
            Duration::from_secs(10),
            drain_selected_lines(reader, 64, selected_record),
        )
        .await
        .expect("reader reaches EOF")
        .expect("selected bounded records");
        writing
            .await
            .expect("writer joined")
            .expect("all metadata drained");
        assert_eq!(retained, b"frame: first\nframe: final without newline");
    }

    #[tokio::test]
    async fn selected_stderr_overflow_and_overlong_lines_drain_before_refusal() {
        use tokio::io::AsyncWriteExt;
        for (bytes, cap) in [
            (b"frame: over cap\n".to_vec(), 3),
            (
                [
                    vec![b'x'; SELECTED_STDERR_LINE_BYTES + 1],
                    b"\nframe: valid\n".to_vec(),
                ]
                .concat(),
                64,
            ),
            (vec![b'x'; SELECTED_STDERR_LINE_BYTES + 1], 64),
        ] {
            let (mut writer, reader) = tokio::io::duplex(7);
            let writing = tokio::spawn(async move {
                writer.write_all(&bytes).await?;
                if bytes.last() == Some(&b'\n') {
                    writer.write_all(b"ignored tail\n").await?;
                }
                Ok::<_, io::Error>(())
            });
            let failure = tokio::time::timeout(
                Duration::from_secs(5),
                drain_selected_lines(reader, cap, selected_record),
            )
            .await
            .expect("refusal waits for EOF")
            .expect_err("incomplete records cannot be accepted");
            assert_eq!(failure.kind(), io::ErrorKind::InvalidData);
            writing
                .await
                .expect("writer joined")
                .expect("overflow did not close the pipe early");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selected_stderr_child_uses_existing_clean_environment_owner() {
        let relative = output_command_clean_env_selected_stderr(
            &mut Command::new("sh"),
            Duration::from_secs(5),
            64,
            TEST_WORK,
            selected_record,
        )
        .await
        .expect_err("absolute executable required");
        assert_eq!(relative.kind(), io::ErrorKind::InvalidInput);
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf 'ignored\\nframe: final\\n' >&2; printf '%s' \"$LC_ALL\"",
        ]);
        let output = output_command_clean_env_selected_stderr(
            &mut command,
            Duration::from_secs(5),
            64,
            TEST_WORK,
            selected_record,
        )
        .await
        .expect("owned selected output");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"C");
        assert_eq!(output.stderr, b"frame: final\n");
    }

    #[tokio::test]
    async fn excessive_output_is_drained_and_retained_at_the_cap() {
        let output = output(
            "/bin/sh",
            &["-c", "yes x | head -c 131072"],
            Duration::from_secs(5),
            1024,
            TEST_WORK,
        )
        .await
        .expect("bounded output");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1024);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_helper_environment_preservation_is_explicit() {
        for inherited in [false, true] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", "printf %s \"$PLURX_NATIVE_HELPER_FIXTURE\""])
                .env("PLURX_NATIVE_HELPER_FIXTURE", "native-host-value");
            let output = if inherited {
                output_command_inherited_env(&mut command, Duration::from_secs(5), 1024, TEST_WORK)
                    .await
            } else {
                output_command(&mut command, Duration::from_secs(5), 1024, TEST_WORK).await
            }
            .expect("bounded native helper");
            assert!(output.status.success());
            assert_eq!(
                output.stdout,
                if inherited {
                    b"native-host-value".as_slice()
                } else {
                    b"".as_slice()
                }
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_cpu_environment_excludes_ambient_service_and_loader_values() {
        const CHILD: &str = "PLURX_TEST_MANAGED_CPU_ENV_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "process::bounded::tests::managed_cpu_environment_excludes_ambient_service_and_loader_values", "--nocapture"])
                .env(CHILD, "1")
                .env("PATH", "/sentinel-only")
                .env("LD_LIBRARY_PATH", "/sentinel-loader")
                .env("DYLD_LIBRARY_PATH", "/sentinel-dyld")
                .env("GGML_BACKEND_PATH", "/sentinel-ggml")
                .env("CUDA_VISIBLE_DEVICES", "sentinel-gpu")
                .env("HTTPS_PROXY", "https://credential-sentinel.invalid")
                .output().await.expect("isolated ambient environment test");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let relative = output_command_clean_env(
            &mut Command::new("env"),
            Duration::from_secs(5),
            16384,
            TEST_WORK,
        )
        .await
        .expect_err("bare executable is not package qualification");
        assert_eq!(relative.kind(), io::ErrorKind::InvalidInput);
        let clean = output_command_clean_env(
            &mut Command::new("/usr/bin/env"),
            Duration::from_secs(5),
            16384,
            TEST_WORK,
        )
        .await
        .expect("clean child");
        assert!(clean.status.success());
        assert_eq!(
            String::from_utf8(clean.stdout).expect("UTF-8 environment"),
            "LC_ALL=C\n"
        );
        let legacy = output_command(
            &mut Command::new("/usr/bin/env"),
            Duration::from_secs(5),
            16384,
            TEST_WORK,
        )
        .await
        .expect("legacy child");
        let legacy = String::from_utf8(legacy.stdout).expect("UTF-8 environment");
        assert!(legacy.lines().any(|line| line == "PATH=/sentinel-only"));
        assert!(legacy
            .lines()
            .any(|line| line == "LD_LIBRARY_PATH=/sentinel-loader"));
        assert!(legacy
            .lines()
            .any(|line| line == "CUDA_VISIBLE_DEVICES=sentinel-gpu"));
        assert!(!legacy.contains("HTTPS_PROXY="));
        assert!(!legacy.contains("GGML_BACKEND_PATH="));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cooperative_cancellation_reaps_the_child_before_returning() {
        let dir = tempfile::tempdir().expect("tmp");
        let pid_file = dir.path().join("child.pid");
        let cancellation = CancellationToken::new();
        let child_cancel = cancellation.clone();
        let path = pid_file.clone();
        let child = tokio::spawn(async move {
            cancellable(
                child_cancel,
                output(
                    "/bin/sh",
                    &[
                        "-c",
                        "echo $$ > \"$1\"; exec sleep 300",
                        "probe",
                        path.to_str().expect("path"),
                    ],
                    Duration::from_secs(300),
                    1024,
                    TEST_WORK,
                ),
            )
            .await
        });
        let pid: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&pid_file).await {
                    if let Ok(pid) = text.trim().parse() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child announced itself");
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(5), child)
            .await
            .expect("bounded cancellation")
            .expect("joined owner")
            .expect_err("cancelled");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        // SAFETY: signal zero observes this fixture's child without signalling it.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "unreaped child outlived its admission"
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn owned_group_pause_preserves_active_budget_and_cancel_reaps_the_stopped_group() {
        let directory = tempfile::tempdir().expect("source pause fixture");
        let leader = directory.path().join("leader.pid");
        let helper = directory.path().join("helper.pid");
        let progress = directory.path().join("progress");
        let active_budget = Duration::from_secs(15);
        let control = SourcePauseController::new(Duration::from_secs(30));
        let cancellation = CancellationToken::new();
        let worker_control = control.clone();
        let worker_cancel = cancellation.clone();
        let args=vec!["-c".to_owned(),
            "echo $$ > \"$1\"; (while :; do printf x >> \"$2\"; sleep 0.02; done) & echo $! > \"$3\"; wait".to_owned(),
            "pause-fixture".to_owned(),leader.to_string_lossy().into_owned(),progress.to_string_lossy().into_owned(),helper.to_string_lossy().into_owned()];
        let child = tokio::spawn(async move {
            pausable(
                worker_control,
                cancellable(
                    worker_cancel,
                    output("/bin/sh", &args, active_budget, 1024, TEST_WORK),
                ),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !leader.exists() || !helper.exists() || !progress.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the owned group is reading");
        let pid: i32 = tokio::fs::read_to_string(&leader)
            .await
            .expect("leader")
            .trim()
            .parse()
            .expect("pid");
        let first = control.request_pause();
        tokio::time::timeout(Duration::from_secs(5), async {
            while control.observation().phase != SourcePausePhase::Paused {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("every group thread stopped");
        assert!(source_group_quiescent(pid, true)
            .await
            .expect("whole group observation"));
        let before = tokio::fs::metadata(&progress)
            .await
            .expect("progress")
            .len();
        // Observe the real stopped group before advancing the budget clock.
        // Actual stop, resume and reap still use the running clock.
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::time::pause();
        tokio::time::advance(active_budget + Duration::from_secs(1)).await;
        tokio::time::resume();
        assert_eq!(
            before,
            tokio::fs::metadata(&progress)
                .await
                .expect("held progress")
                .len()
        );
        assert!(
            !child.is_finished(),
            "the verified hold outlasts the active budget without spending it"
        );
        assert!(!control.resume(first + 1));
        assert!(control.resume(first));
        tokio::time::timeout(Duration::from_secs(2), async {
            while tokio::fs::metadata(&progress)
                .await
                .expect("resumed progress")
                .len()
                == before
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the same helper resumes progress");
        let second = control.request_pause();
        tokio::time::timeout(Duration::from_secs(5), async {
            while control.observation().phase != SourcePausePhase::Paused {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("second owned pause");
        assert!(!control.resume(first));
        assert!(second > first);
        cancellation.cancel();
        let failure = tokio::time::timeout(Duration::from_secs(5), child)
            .await
            .expect("stopped group cleanup")
            .expect("owner joined")
            .expect_err("cancelled while paused");
        assert_eq!(failure.kind(), io::ErrorKind::Interrupted);
        assert!(source_group_quiescent(pid, false)
            .await
            .expect("no live reading thread"));
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "leader reaped before ownership returns"
        );
    }

    #[tokio::test]
    async fn a_hanging_process_is_killed_within_its_wall_time() {
        let started = tokio::time::Instant::now();
        let error = output(
            "/bin/sh",
            &["-c", "sleep 300"],
            Duration::from_millis(50),
            1024,
            TEST_WORK,
        )
        .await
        .expect_err("hanging process must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_descendant_cannot_hold_probe_pipes_past_the_wall_time() {
        let started = tokio::time::Instant::now();
        // This proves cleanup after the leader exits, not that the runner can
        // schedule a shell and drain two pipes within 50 ms under suite load.
        // Keep the success bound below the probe deadline: a descendant left
        // holding a pipe must still fail rather than pass by timing out.
        let output = output(
            "/bin/sh",
            &["-c", "sleep 300 & exit 0"],
            Duration::from_secs(5),
            1024,
            TEST_WORK,
        )
        .await
        .expect("the leader exit must reap its background process group");
        assert!(output.status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
