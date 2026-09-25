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
) -> io::Result<Output>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
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

    let (mut child, job) = super::spawn_job_owned(&mut command)?;
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

    let completed = tokio::time::timeout(wall_time, async {
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
    })
    .await;
    match completed {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => {
            child.kill_and_reap().await;
            stdout.abort();
            stderr.abort();
            Err(error)
        }
        Err(_) => {
            child.kill_and_reap().await;
            stdout.abort();
            stderr.abort();
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

    #[tokio::test]
    async fn excessive_output_is_drained_and_retained_at_the_cap() {
        let output = output(
            "/bin/sh",
            &["-c", "yes x | head -c 131072"],
            Duration::from_secs(5),
            1024,
        )
        .await
        .expect("bounded output");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1024);
    }

    #[tokio::test]
    async fn a_hanging_process_is_killed_within_its_wall_time() {
        let started = tokio::time::Instant::now();
        let error = output(
            "/bin/sh",
            &["-c", "sleep 300"],
            Duration::from_millis(50),
            1024,
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
        )
        .await
        .expect("the leader exit must reap its background process group");
        assert!(output.status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
