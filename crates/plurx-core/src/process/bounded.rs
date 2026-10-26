//! Bounded ownership for short-lived media inspection processes.
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

pub(crate) fn cancellation() -> Option<CancellationToken> {
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
    output_command_environment(command, wall_time, max_output_bytes, work, true).await
}

/// Bounded CPU child with held descriptors and an LC_ALL-only environment.
/// Absolute executable and library-qualified package paths are required.
pub async fn output_command_clean_env(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
) -> io::Result<Output> {
    if !std::path::Path::new(command.as_std().get_program()).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed child executable must be absolute",
        ));
    }
    output_command_environment(command, wall_time, max_output_bytes, work, false).await
}

async fn output_command_environment(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    inherit_service_environment: bool,
) -> io::Result<Output> {
    check_cancellation()?;
    let cancellation = cancellation().unwrap_or_default();
    command
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if inherit_service_environment {
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

    let (child, job) = super::spawn_job_owned(command, work)?;
    // Install ownership before any fallible pipe extraction. Even early
    // errors now retain the original Job/keeper in the original reaper.
    let mut child = OwnedChild::new(child, job);
    let mut collector = CollectorKeeper {
        keeper: original_keeper(),
        completed: false,
    };
    let stdout = child
        .child
        .as_mut()
        .expect("owned child")
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("probe stdout was not piped"))?;
    let stderr = child
        .child
        .as_mut()
        .expect("owned child")
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("probe stderr was not piped"))?;
    // Construct each whole reader/keeper owner synchronously, before spawn.
    // Even a task aborted before its first poll drops one OriginalPipe value.
    let stdout = OriginalPipe {
        reader: stdout,
        keeper: original_keeper(),
    };
    let stderr = OriginalPipe {
        reader: stderr,
        keeper: original_keeper(),
    };
    let mut stdout = Some(tokio::spawn(drain_capped_owned(stdout, max_output_bytes)));
    let mut stderr = Some(tokio::spawn(drain_capped_owned(stderr, max_output_bytes)));

    let completed = tokio::select! {
        biased;
        () = cancellation.cancelled() => Ok(Err(io::Error::new(
            io::ErrorKind::Interrupted, "work cancelled"))),
        result = tokio::time::timeout(wall_time, async {
        let status = child.wait().await?;
        // A short-lived probe may not daemonize. Once its leader exits, kill
        // any descendant left in the owned group before waiting for pipe EOF.
        child.kill_process_group();
        let stdout = join_original_pipe(&mut stdout, &collector.keeper).await?;
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
            abort_and_join_original_pipe(&mut stdout, &collector.keeper).await;
            abort_and_join_original_pipe(&mut stderr, &collector.keeper).await;
            Err(error)
        }
        Err(_) => {
            child.kill_and_reap().await;
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
) -> io::Result<Vec<u8>> {
    let result = drain_capped(&mut original.reader, cap).await;
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
        let unpolled = drain_capped_owned(original, 8);
        drop(unpolled);
        assert_eq!(
            *drops.lock().expect("actual drop order"),
            ["reader", "keeper"]
        );
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
