//! Bounded ownership for short-lived media inspection processes.
//!
//! Startup probes are evidence, not services. They receive a small explicit
//! environment, bounded output and wall time, and one owner that kills and
//! reaps the complete process group when the future is cancelled.

use std::ffi::OsStr;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static WORK_CANCELLATION: CancellationToken;
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
    _job: super::ChildJob,
    #[cfg(unix)]
    process_group: Option<i32>,
}

impl OwnedChild {
    fn new(child: Child, job: super::ChildJob) -> Self {
        Self {
            _job: job,
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
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
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
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
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
    check_cancellation()?;
    let cancellation = cancellation().unwrap_or_default();
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
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
    #[cfg(unix)]
    command.process_group(0);

    let (mut child, job) = super::spawn_job_owned(&mut command, work)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("probe stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("probe stderr was not piped"))?;
    let mut stdout = tokio::spawn(drain_capped(stdout, max_output_bytes));
    let mut stderr = tokio::spawn(drain_capped(stderr, max_output_bytes));
    let mut child = OwnedChild::new(child, job);

    let completed = tokio::select! {
        biased;
        () = cancellation.cancelled() => Ok(Err(io::Error::new(
            io::ErrorKind::Interrupted, "work cancelled"))),
        result = tokio::time::timeout(wall_time, async {
        let status = child.wait().await?;
        // A short-lived probe may not daemonize. Once its leader exits, kill
        // any descendant left in the owned group before waiting for pipe EOF.
        child.kill_process_group();
        let stdout = (&mut stdout)
            .await
            .map_err(|error| io::Error::other(error.to_string()))??;
        let stderr = (&mut stderr)
            .await
            .map_err(|error| io::Error::other(error.to_string()))??;
        Ok::<_, io::Error>(Output {
            status,
            stdout,
            stderr,
        })
        }) => result,
    };
    match completed {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => {
            child.kill_and_reap().await;
            if !stdout.is_finished() {
                stdout.abort();
                let _ = stdout.await;
            }
            if !stderr.is_finished() {
                stderr.abort();
                let _ = stderr.await;
            }
            Err(error)
        }
        Err(_) => {
            child.kill_and_reap().await;
            if !stdout.is_finished() {
                stdout.abort();
                let _ = stdout.await;
            }
            if !stderr.is_finished() {
                stderr.abort();
                let _ = stderr.await;
            }
            Err(io::Error::new(io::ErrorKind::TimedOut, "probe timed out"))
        }
    }
}

/// Keep draining after the retained cap. Closing the reader at the cap can
/// fill the child's pipe and deadlock it before the wall-time owner can reap.
async fn drain_capped(
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
        let output = output(
            "/bin/sh",
            &["-c", "sleep 300 & exit 0"],
            Duration::from_millis(50),
            1024,
            TEST_WORK,
        )
        .await
        .expect("the leader exit must reap its background process group");
        assert!(output.status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
