//! Cross-platform control of an owned, unreaped child process.

use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessSignal {
    Suspend,
    Resume,
    Terminate,
}

pub(crate) struct ChildJob {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

/// Spawn without giving the child a chance to create descendants before its
/// lifetime is tied to the daemon. On Unix the setup calls are no-ops; on
/// Windows the child starts suspended, enters a kill-on-close Job Object, and
/// only then resumes.
pub(crate) fn spawn_job_owned(
    command: &mut tokio::process::Command,
) -> io::Result<(tokio::process::Child, ChildJob)> {
    configure_suspended(command);
    let mut child = command.spawn()?;
    let job = ChildJob::attach(&child).inspect_err(|_| {
        let _ = child.start_kill();
    })?;
    resume_suspended(&child).inspect_err(|_| {
        let _ = child.start_kill();
    })?;
    Ok((child, job))
}

/// Run a short-lived helper under the same descendant lifetime contract as a
/// long-lived transcode, with [`tokio::process::Command::output`] semantics.
///
/// Both streams are captured even when the caller configured them otherwise.
/// The audited consumers depend on that override:
///
/// | file | calls | output consumed |
/// |---|---:|---|
/// | `pipeprobe.rs` | 2 | stdout and stderr |
/// | `ffmpeg.rs` | 4 | status, stdout and stderr |
/// | `transcode.rs` | 1 | stdout and stderr |
/// | `subtitles.rs` | 2 | status and stderr |
/// | `live_tv.rs` | 1 | status and stderr |
///
/// Dropping this future drops the job after Tokio has requested child
/// termination, so timeout and cancellation cannot strand a grandchild
/// holding a Windows cache file open. This helper deliberately does not bound
/// captured output; callers with that contract use the bounded primitive.
pub(crate) async fn output_job_owned(
    command: &mut tokio::process::Command,
) -> io::Result<std::process::Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (child, _job) = spawn_job_owned(command)?;
    child.wait_with_output().await
}

pub(crate) async fn status_job_owned(
    command: &mut tokio::process::Command,
) -> io::Result<std::process::ExitStatus> {
    let (mut child, _job) = spawn_job_owned(command)?;
    child.wait().await
}

#[cfg(unix)]
impl ChildJob {
    pub(crate) fn attach(_child: &tokio::process::Child) -> io::Result<Self> {
        Ok(Self {})
    }
}

#[cfg(unix)]
pub(crate) fn configure_suspended(_command: &mut tokio::process::Command) {}

#[cfg(windows)]
pub(crate) fn configure_suspended(command: &mut tokio::process::Command) {
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
}

#[cfg(unix)]
pub(crate) fn resume_suspended(_child: &tokio::process::Child) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub(crate) fn resume_suspended(child: &tokio::process::Child) -> io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("suspended child has no process identifier"))?;
    signal(pid, ProcessSignal::Resume).and_then(|resumed| {
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other("suspended child exited before resume"))
        }
    })
}

#[cfg(windows)]
unsafe impl Send for ChildJob {}

#[cfg(windows)]
impl Drop for ChildJob {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
impl ChildJob {
    pub(crate) fn attach(child: &tokio::process::Child) -> io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(error);
        }
        let pid = child.id().ok_or_else(|| {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            io::Error::other("child has no process identifier")
        })?;
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            let error = io::Error::last_os_error();
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(error);
        }
        let assigned = unsafe { AssignProcessToJobObject(handle, process) };
        unsafe { windows_sys::Win32::Foundation::CloseHandle(process) };
        if assigned == 0 {
            let error = io::Error::last_os_error();
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(error);
        }
        Ok(Self { handle })
    }
}

#[cfg(unix)]
pub(crate) fn signal(pid: u32, signal: ProcessSignal) -> io::Result<bool> {
    let signal = match signal {
        ProcessSignal::Suspend => libc::SIGSTOP,
        ProcessSignal::Resume => libc::SIGCONT,
        ProcessSignal::Terminate => libc::SIGKILL,
    };
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::other("child pid does not fit pid_t"))?;
    // SAFETY: the caller owns the unreaped child, so this pid cannot have
    // been recycled before the signal is sent.
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::path::Path;
    use std::time::Duration;

    /// Re-exec the portable test binary instead of relying on a platform shell.
    fn child(mode: &str) -> tokio::process::Command {
        let mut command =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "process_control::tests::child_main",
                "--nocapture",
            ])
            .env("PLURX_CHILD_MODE", mode);
        command
    }

    #[test]
    fn child_main() {
        match std::env::var("PLURX_CHILD_MODE").as_deref() {
            Ok("echo") => {
                std::io::stdout().write_all(b"out-bytes").expect("stdout");
                std::io::stderr().write_all(b"err-bytes").expect("stderr");
            }
            Ok("fail") => {
                std::io::stderr().write_all(b"reason").expect("stderr");
                std::process::exit(3);
            }
            Ok("sleep") => {
                if let Ok(path) = std::env::var("PLURX_CHILD_PID_FILE") {
                    std::fs::write(path, std::process::id().to_string()).expect("pid file");
                }
                std::thread::sleep(Duration::from_secs(300));
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn both_streams_are_captured() {
        let output = output_job_owned(&mut child("echo"))
            .await
            .expect("child output");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("out-bytes"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("err-bytes"));
    }

    #[tokio::test]
    async fn a_failing_child_reports_status_and_stderr() {
        let output = output_job_owned(&mut child("fail"))
            .await
            .expect("child output");
        assert_eq!(output.status.code(), Some(3));
        assert!(String::from_utf8_lossy(&output.stderr).contains("reason"));
    }

    #[tokio::test]
    async fn a_caller_that_configured_inherit_still_gets_bytes() {
        let mut command = child("echo");
        command.stdout(std::process::Stdio::inherit());
        let output = output_job_owned(&mut command).await.expect("child output");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("out-bytes"));
    }

    #[tokio::test]
    async fn dropping_the_future_kills_the_child() {
        let directory = tempfile::tempdir().expect("pid directory");
        let pid_file = directory.path().join("child.pid");
        let mut command = child("sleep");
        command.env("PLURX_CHILD_PID_FILE", &pid_file);

        let result =
            tokio::time::timeout(Duration::from_millis(200), output_job_owned(&mut command)).await;
        assert!(result.is_err(), "sleeping child exceeded the deadline");
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("sleeping child published its pid")
            .parse()
            .expect("numeric child pid");

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            !signal(pid, ProcessSignal::Terminate).expect("probe dead child"),
            "the dropped output future left its child alive"
        );
    }

    fn collect_output_call_sites(
        directory: &Path,
        source_root: &Path,
        sites: &mut BTreeMap<String, usize>,
    ) -> io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                collect_output_call_sites(&path, source_root, sites)?;
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs")
                && path.file_name().and_then(|value| value.to_str()) != Some("process_control.rs")
            {
                let source = std::fs::read_to_string(&path)?;
                let production = source
                    .split("#[cfg(test)]\nmod tests {")
                    .next()
                    .expect("source prefix");
                let count = production.matches("output_job_owned(").count();
                if count > 0 {
                    let file = path
                        .strip_prefix(source_root)
                        .expect("source below manifest src")
                        .to_string_lossy()
                        .replace('\\', "/")
                        .to_owned();
                    sites.insert(file, count);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn output_job_owned_call_sites_are_the_audited_set() {
        let expected = BTreeMap::from([
            ("ffmpeg.rs".to_owned(), 4),
            ("live_tv.rs".to_owned(), 1),
            ("pipeprobe.rs".to_owned(), 2),
            ("subtitles.rs".to_owned(), 2),
            ("transcode.rs".to_owned(), 1),
        ]);
        let mut actual = BTreeMap::new();
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        collect_output_call_sites(&source_root, &source_root, &mut actual)
            .expect("audit plurxd call sites");
        assert_eq!(actual, expected, "update the helper's caller audit");
    }
}

#[cfg(windows)]
pub(crate) fn signal(pid: u32, signal: ProcessSignal) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, RtlNtStatusToDosError, STATUS_INVALID_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
    };

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: *mut core::ffi::c_void) -> i32;
        fn NtResumeProcess(process: *mut core::ffi::c_void) -> i32;
    }

    let access = match signal {
        ProcessSignal::Suspend | ProcessSignal::Resume => PROCESS_SUSPEND_RESUME,
        ProcessSignal::Terminate => PROCESS_TERMINATE,
    };
    // SAFETY: the pid belongs to the caller's unreaped child. The returned
    // process handle is checked and closed below.
    let process = unsafe { OpenProcess(access, 0, pid) };
    if process.is_null() {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error)
        };
    }
    // SAFETY: `process` is a live process handle with the access required by
    // the selected operation.
    let status = unsafe {
        match signal {
            ProcessSignal::Suspend => NtSuspendProcess(process),
            ProcessSignal::Resume => NtResumeProcess(process),
            ProcessSignal::Terminate => {
                if TerminateProcess(process, 1) == 0 {
                    -1
                } else {
                    0
                }
            }
        }
    };
    // SAFETY: `process` was returned by OpenProcess and is closed once.
    unsafe { CloseHandle(process) };
    if status >= 0 {
        Ok(true)
    } else if status == STATUS_INVALID_HANDLE {
        Ok(false)
    } else if signal == ProcessSignal::Terminate && status == -1 {
        Err(io::Error::last_os_error())
    } else {
        Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    }
}
