//! Bounded ownership for short-lived helper processes.
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
    )
    .await
}

#[derive(Clone, Copy)]
enum Environment {
    Service,
    Clean,
    Inherited,
}

async fn output_command_environment(
    command: &mut Command,
    wall_time: Duration,
    max_output_bytes: usize,
    work: super::ChildWork,
    environment: Environment,
    stderr_selector: Option<fn(&[u8]) -> bool>,
) -> io::Result<Output> {
    check_cancellation()?;
    let cancellation = cancellation().unwrap_or_default();
    if !matches!(environment, Environment::Inherited) {
        command.env_clear().env("LC_ALL", "C");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
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

    let (mut child, job) = super::spawn_job_owned(command, work)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("probe stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("probe stderr was not piped"))?;
    let mut stdout = tokio::spawn(drain_capped(stdout, max_output_bytes));
    let mut stderr = tokio::spawn(async move {
        match stderr_selector {
            Some(select) => drain_selected_lines(stderr, max_output_bytes, select).await,
            None => drain_capped(stderr, max_output_bytes).await,
        }
    });
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
