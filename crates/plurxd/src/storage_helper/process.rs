//! Owned direct-child execution for the migration helper, which launches no
//! subprocesses. The parent separately retains its generation through root
//! reacquisition. Dropping this waiter requests cancellation; it never aborts
//! the owner responsible for reaping the actual child.
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use plurx_core::error::StoreError;
use plurx_core::process::{spawn_job_owned, ChildJob, ChildWork};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const STDOUT_LIMIT: usize = 65_536;
const STDERR_LIMIT: usize = 16_384;
const DRAIN_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct Drain {
    bytes: Vec<u8>,
    exceeded: bool,
}
struct Reader {
    task: Option<JoinHandle<std::io::Result<Drain>>>,
    result: Option<Drain>,
}
impl Reader {
    async fn finish(&mut self) -> Result<(), StoreError> {
        if let Some(task) = &mut self.task {
            let result = task.await;
            self.task = None;
            self.result = Some(
                result
                    .map_err(|error| failed(format!("output reader failed: {error}")))?
                    .map_err(|error| failed(format!("output read failed: {error}")))?,
            );
        }
        Ok(())
    }
}

/// This guard also covers unexpected unwinding of the owner task. It never
/// signals a numeric PID or process group; Tokio retains the actual Child.
struct Ownership<G: Send + 'static> {
    child: Option<Child>,
    job: Option<ChildJob>,
    generation: Option<G>,
    stdout: Option<Reader>,
    stderr: Option<Reader>,
}
impl<G: Send + 'static> Ownership<G> {
    async fn drain(&mut self) -> Result<(), StoreError> {
        if let Some(reader) = &mut self.stdout {
            reader.finish().await?;
        }
        if let Some(reader) = &mut self.stderr {
            reader.finish().await?;
        }
        Ok(())
    }
    fn retain_unproven(&mut self) {
        // The launcher/wait API supplied no terminal-reap proof. Deliberately
        // retain the generation/job rather than release installation ownership
        // or signal a PID whose identity can no longer be established.
        std::mem::forget(self.child.take());
        std::mem::forget(self.job.take());
        std::mem::forget(self.generation.take());
        std::mem::forget(self.stdout.take());
        std::mem::forget(self.stderr.take());
    }
}
impl<G: Send + 'static> Drop for Ownership<G> {
    fn drop(&mut self) {
        if self.child.is_none() && self.stdout.is_none() && self.stderr.is_none() {
            return;
        }
        // A runtime that is itself shutting down may drop the detached cleanup
        // future. Its default must retain unproven ownership, not recursively
        // spawn another cleanup task or release a live generation.
        let mut retained = std::mem::ManuallyDrop::new(Self {
            child: self.child.take(),
            job: self.job.take(),
            generation: self.generation.take(),
            stdout: self.stdout.take(),
            stderr: self.stderr.take(),
        });
        if let Some(child) = &mut retained.child {
            if child.id().is_some() {
                let _ = child.start_kill();
            }
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Some(child) = &mut retained.child {
                        if child.wait().await.is_err() {
                            retained.retain_unproven();
                            return;
                        }
                        retained.child = None;
                    }
                    // Even a reader failure must not abandon the other pipe.
                    if let Some(reader) = &mut retained.stdout {
                        let _ = reader.finish().await;
                    }
                    if let Some(reader) = &mut retained.stderr {
                        let _ = reader.finish().await;
                    }
                    retained.stdout = None;
                    retained.stderr = None;
                    retained.job = None;
                    retained.generation = None;
                });
            }
            Err(_) => retained.retain_unproven(),
        }
    }
}

fn reader<R: AsyncRead + Unpin + Send + 'static>(
    mut pipe: R,
    limit: usize,
    failed_output: Arc<AtomicBool>,
    changed: Arc<Notify>,
) -> Reader {
    Reader {
        task: Some(tokio::spawn(async move {
            let mut bytes = Vec::new();
            let mut exceeded = false;
            let mut buffer = [0_u8; 8192];
            loop {
                let count = match pipe.read(&mut buffer).await {
                    Ok(count) => count,
                    Err(error) => {
                        failed_output.store(true, Ordering::Release);
                        changed.notify_one();
                        return Err(error);
                    }
                };
                if count == 0 {
                    break;
                }
                let retained = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..retained]);
                if retained != count {
                    exceeded = true;
                    failed_output.store(true, Ordering::Release);
                    changed.notify_one();
                }
            }
            Ok(Drain { bytes, exceeded })
        })),
        result: None,
    }
}

#[cfg(test)]
pub(super) async fn run_owned<G: Send + 'static>(
    generation: G,
    executable: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
) -> Result<(G, Output), StoreError> {
    run_owned_inheriting(generation, executable, args, timeout, Vec::new()).await
}

/// These descriptors are duplicated leases owned by this operation. CLOEXEC
/// changes happen only in this particular child, never in the daemon or in an
/// unrelated media subprocess. The owner retains them through actual reap.
pub(super) async fn run_owned_inheriting<G: Send + 'static>(
    generation: G,
    executable: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
    inherited_leases: Vec<std::fs::File>,
) -> Result<(G, Output), StoreError> {
    if !executable.is_absolute() || timeout.is_zero() {
        return Err(failed(
            "an absolute authenticated executable and nonzero deadline are required",
        ));
    }
    let cancellation = CancellationToken::new();
    let waiter = CancelOnDrop(cancellation.clone());
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = execute(
            (generation, inherited_leases),
            executable,
            args,
            timeout,
            cancellation,
        )
        .await
        .map(|((generation, _leases), output)| (generation, output));
        let _ = send.send(result);
    });
    let result = receive
        .await
        .map_err(|_| failed("helper process owner failed; cleanup retains its ownership"))?;
    drop(waiter);
    result
}

async fn execute<G: Send + 'static>(
    generation: (G, Vec<std::fs::File>),
    executable: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
    cancellation: CancellationToken,
) -> Result<((G, Vec<std::fs::File>), Output), StoreError> {
    if cancellation.is_cancelled() {
        return Err(failed("helper launch cancelled"));
    }
    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| failed("invalid helper execution deadline"))?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let descriptors = generation
            .1
            .iter()
            .map(AsRawFd::as_raw_fd)
            .collect::<Vec<_>>();
        // SAFETY: owned Files outlive spawn/reap, and this closure performs
        // only async-signal-safe fcntl operations in this child before exec.
        unsafe {
            command.pre_exec(move || {
                for descriptor in &descriptors {
                    let flags = libc::fcntl(*descriptor, libc::F_GETFD);
                    if flags < 0
                        || libc::fcntl(*descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    let (child, job) = match spawn_job_owned(
        &mut command,
        ChildWork::background("storage migration helper"),
    ) {
        Ok(owned) => owned,
        Err(error) => {
            // Core's Windows attach/resume errors can occur after spawning.
            // Its error API exposes no Child to reap; do not claim safe release.
            std::mem::forget(generation);
            return Err(failed(format!(
                "helper launcher failed; generation retained without reap proof: {error}"
            )));
        }
    };
    let mut owned = Ownership {
        child: Some(child),
        job: Some(job),
        generation: Some(generation),
        stdout: None,
        stderr: None,
    };
    let failed_output = Arc::new(AtomicBool::new(false));
    let changed = Arc::new(Notify::new());
    let child = owned.child.as_mut().expect("owned helper child");
    owned.stdout = Some(reader(
        child
            .stdout
            .take()
            .ok_or_else(|| failed("helper stdout is not piped"))?,
        STDOUT_LIMIT,
        Arc::clone(&failed_output),
        Arc::clone(&changed),
    ));
    owned.stderr = Some(reader(
        child
            .stderr
            .take()
            .ok_or_else(|| failed("helper stderr is not piped"))?,
        STDERR_LIMIT,
        Arc::clone(&failed_output),
        Arc::clone(&changed),
    ));
    let mut refusal = None;
    let waited = tokio::select! {
        result = child.wait() => Some(result),
        _ = cancellation.cancelled() => { refusal = Some("helper caller cancelled"); None },
        _ = changed.notified() => { refusal = Some("helper output exceeded its bound or could not be read"); None },
        _ = tokio::time::sleep_until(deadline.into()) => { refusal = Some("helper execution deadline exceeded"); None },
    };
    let waited = match waited {
        Some(result) => result,
        None => {
            if child.id().is_some() {
                let _ = child.start_kill();
            }
            // No timeout can turn an un-reaped child into successful cleanup.
            child.wait().await
        }
    };
    let status = match waited {
        Ok(status) => status,
        Err(error) => {
            owned.retain_unproven();
            return Err(failed(format!(
                "helper reap could not be proven; ownership retained: {error}"
            )));
        }
    };
    owned.child = None;
    tokio::time::timeout(DRAIN_BUDGET, owned.drain())
        .await
        .map_err(|_| {
            failed("helper exited but output pipes remain open; cleanup retains the generation")
        })??;
    let stdout = owned
        .stdout
        .take()
        .and_then(|reader| reader.result)
        .ok_or_else(|| failed("missing helper stdout completion"))?;
    let stderr = owned
        .stderr
        .take()
        .and_then(|reader| reader.result)
        .ok_or_else(|| failed("missing helper stderr completion"))?;
    owned.job = None;
    if stdout.exceeded || stderr.exceeded || failed_output.load(Ordering::Acquire) {
        return Err(failed("helper output exceeded its bound or failed"));
    }
    if let Some(reason) = refusal {
        return Err(failed(reason));
    }
    if !status.success() {
        return Err(failed(format!(
            "helper exited with {status}: {}",
            String::from_utf8_lossy(&stderr.bytes)
        )));
    }
    let generation = owned
        .generation
        .take()
        .expect("owned executable generation");
    Ok((
        generation,
        Output {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        },
    ))
}

fn failed(message: impl AsRef<str>) -> StoreError {
    StoreError::Migration(format!("storage migration process: {}", message.as_ref()))
}

#[cfg(test)]
#[path = "process/tests.rs"]
mod tests;
