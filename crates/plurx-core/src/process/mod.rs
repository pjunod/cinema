//! Cross-platform control of an owned, unreaped child process, and the
//! daemon's own inherited limits on how many descriptors it may hold.

pub mod bounded;
#[cfg(test)]
mod census;
#[doc(hidden)]
pub mod fixed_probe;
#[cfg(unix)]
pub mod inherited_descriptors;
mod job_control;
pub mod observation;
pub mod priority;
pub mod rlimit;

pub use priority::{ChildClass, ChildWork};

use std::io;

/// Shared scanner/producer resolution: explicit nonempty operator paths win;
/// packaged macOS/Windows tools are ordinary executable siblings; an unbundled
/// process retains PATH lookup. The optional directory makes this same policy
/// usable by hosts with an already-resolved executable location.
pub fn resolve_media_tool(
    override_value: Option<String>,
    name: &str,
    executable_directory: Option<&std::path::Path>,
) -> String {
    if let Some(value) = override_value.filter(|value| !value.is_empty()) {
        return value;
    }
    if let Some(directory) = executable_directory {
        #[cfg(windows)]
        let filename = format!("{name}.exe");
        #[cfg(not(windows))]
        let filename = name.to_owned();
        let sibling = directory.join(filename);
        if sibling.is_file() {
            return sibling.to_string_lossy().into_owned();
        }
    }
    name.to_owned()
}

/// Resolve a media tool for this process using the shared package policy.
pub fn media_tool_bin(name: &str, override_value: Option<String>) -> String {
    #[cfg(any(windows, target_os = "macos"))]
    let executable = std::env::current_exe().ok();
    #[cfg(any(windows, target_os = "macos"))]
    let directory = executable.as_deref().and_then(std::path::Path::parent);
    #[cfg(not(any(windows, target_os = "macos")))]
    let directory = None;
    resolve_media_tool(override_value, name, directory)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSignal {
    Suspend,
    Resume,
    Terminate,
}

pub struct ChildJob {
    #[cfg(windows)]
    job: job_control::WindowsJob,
    /// The child's row in [`priority::running`]; it leaves the list when the
    /// owner drops this job.
    _registration: Option<priority::Registration>,
}

impl ChildJob {
    /// Control the actual registered root while another task holds the Child
    /// for `wait()`. This does not extend the registration's lifetime.
    pub(crate) fn registered_control(&self) -> Option<priority::RegisteredChildControl> {
        self._registration
            .as_ref()
            .map(priority::Registration::control)
    }
}

/// The one launcher every production child goes through.
///
/// Spawn without giving the child a chance to create descendants before its
/// lifetime is tied to the daemon. Linux spawns on a persistent owner thread
/// and installs a parent-death SIGKILL before exec, including stopped children.
/// Other Unix platforms retain their existing lifetime behavior. On Windows the child starts suspended, enters a kill-on-close Job Object, and
/// only then resumes.
///
/// `work` names the child's [`ChildClass`] and purpose: the child lowers its
/// own CPU, I/O and OOM priority to the class's policy before `exec`
/// ([`priority::apply`]; priority refusals never fail the spawn, but Linux
/// ownership setup must succeed), and it is
/// listed in [`priority::running`] until the returned job is dropped.
/// `process::census::every_production_spawn_goes_through_the_launcher` fails
/// on any production spawn that does not come through here.
pub fn spawn_job_owned(
    command: &mut tokio::process::Command,
    work: ChildWork,
) -> io::Result<(tokio::process::Child, ChildJob)> {
    spawn_job_custodied(command, work).map_err(|failure| failure.error)
}

/// The graph owner must retain partially successful launches too. Legacy
/// callers still receive io::Error; physical custody consumes this internal
/// result before any actual child/job handles can be dropped on failure.
pub(crate) struct OwnedSpawnFailure {
    pub error: io::Error,
    pub child: Option<tokio::process::Child>,
    pub job: Option<ChildJob>,
}

pub(crate) fn spawn_job_custodied(
    command: &mut tokio::process::Command,
    work: ChildWork,
) -> Result<(tokio::process::Child, ChildJob), Box<OwnedSpawnFailure>> {
    priority::apply(command, work.class);
    configure_suspended(command);
    #[allow(clippy::disallowed_methods)] // the launcher itself
    let mut child = spawn_on_owner(
        command,
        || tokio::process::Command::new(""),
        |command| command.spawn(),
    )
    .map_err(|error| {
        Box::new(OwnedSpawnFailure {
            error,
            child: None,
            job: None,
        })
    })?;
    let mut job = match ChildJob::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.start_kill();
            return Err(Box::new(OwnedSpawnFailure {
                error,
                child: Some(child),
                job: None,
            }));
        }
    };
    // Assignment succeeded while the root was still suspended. Preserve this
    // fact even if resume fails and the actual partial launch needs cleanup.
    #[cfg(windows)]
    {
        job.job.mark_pre_execution();
    }
    if let Err(error) = resume_suspended(&child) {
        let _ = child.start_kill();
        return Err(Box::new(OwnedSpawnFailure {
            error,
            child: Some(child),
            job: Some(job),
        }));
    }
    job._registration = priority::register(child.id(), command.as_std().get_program(), work);
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
/// | `subtitles.rs` | 1 | whole-track status and stderr |
/// | `live_tv.rs` | 1 | status and stderr |
/// | `live_tv/caption_probe.rs` | 1 | status, stdout and stderr |
/// | `subtitle_ride_along.rs` | 1 | status, stdout and stderr |
///
/// Subtitle window extraction uses the bounded diagnostic child instead of
/// this helper, so its VTT pipe and diagnostics have explicit byte limits.
///
/// Dropping this future drops the job after Tokio has requested child
/// termination, so timeout and cancellation cannot strand a grandchild
/// holding a Windows cache file open. This helper deliberately does not bound
/// captured output; callers with that contract use the bounded primitive.
pub async fn output_job_owned(
    command: &mut tokio::process::Command,
    work: ChildWork,
) -> io::Result<std::process::Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (child, _job) = spawn_job_owned(command, work)?;
    child.wait_with_output().await
}

/// Bytes of a status-only child's stderr retained for the daemon log: room
/// for an FFmpeg refusal and the libva lines printed ahead of it.
const STATUS_DIAGNOSTIC_BYTES: usize = 16 * 1024;

/// Run a child for its exit status alone.
///
/// The child never writes to the daemon's own descriptors. Its stdin and
/// stdout are null and its stderr is drained, bounded, into the daemon's log
/// through `tracing`: `debug` when it succeeds, `warn` with what it wrote when
/// it fails. An inherited stderr put the startup capability probes' libva
/// banners and FFmpeg errors raw into a log that is otherwise JSON
/// (`PLURX_LOG_FORMAT=json`), 21 of 80 lines at boot on the 2026-10-11 lab.
pub async fn status_job_owned(
    command: &mut tokio::process::Command,
    work: ChildWork,
) -> io::Result<std::process::ExitStatus> {
    let (status, diagnostics) = status_with_diagnostics(command, work).await?;
    let diagnostics = String::from_utf8_lossy(&diagnostics);
    let diagnostics = diagnostics.trim();
    if !status.success() {
        tracing::warn!(purpose = work.purpose, %status, diagnostics, "child process failed");
    } else if !diagnostics.is_empty() {
        tracing::debug!(purpose = work.purpose, %status, diagnostics, "child process diagnostics");
    }
    Ok(status)
}

async fn status_with_diagnostics(
    command: &mut tokio::process::Command,
    work: ChildWork,
) -> io::Result<(std::process::ExitStatus, Vec<u8>)> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let (mut child, _job) = spawn_job_owned(command, work)?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("status child stderr was not piped"))?;
    let (status, diagnostics) = tokio::join!(
        child.wait(),
        bounded::drain_capped(stderr, STATUS_DIAGNOSTIC_BYTES)
    );
    settle_status(status, diagnostics, work)
}

/// The exit status is the answer; the diagnostics only explain it. A failure
/// to read the child's stderr is logged and leaves the status standing, so a
/// broken pipe cannot turn a probe that passed into one that errored.
fn settle_status(
    status: io::Result<std::process::ExitStatus>,
    diagnostics: io::Result<Vec<u8>>,
    work: ChildWork,
) -> io::Result<(std::process::ExitStatus, Vec<u8>)> {
    let status = status?;
    let diagnostics = diagnostics.unwrap_or_else(|error| {
        tracing::warn!(
            purpose = work.purpose,
            %status,
            %error,
            "reading a child process's stderr failed; its exit status stands"
        );
        Vec::new()
    });
    Ok((status, diagnostics))
}

/// [`output_job_owned`] for synchronous code that already runs off the async
/// runtime (inside `spawn_blocking`): the same class, the same Activity row,
/// both streams captured. The child is attached to its job right after the
/// spawn rather than created suspended, which is the contract for a probe
/// that starts no descendants of its own.
pub fn output_job_owned_blocking(
    command: &mut std::process::Command,
    work: ChildWork,
) -> io::Result<std::process::Output> {
    priority::apply_policy(command, work.class.policy());
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[allow(clippy::disallowed_methods)] // the blocking launcher itself
    let mut child = spawn_on_owner(
        command,
        || std::process::Command::new(""),
        |command| command.spawn(),
    )?;
    let mut job = match ChildJob::attach_pid(child.id()) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    job._registration = priority::register(Some(child.id()), command.get_program(), work);
    let output = child.wait_with_output();
    drop(job);
    output
}

/// Linux parent-death signals follow the thread that forks, so an idle Tokio
/// blocking worker cannot own a long-lived child. Keep one spawn thread for
/// the process lifetime; enter the caller's runtime only for its spawn so the
/// returned Tokio child remains bound to the caller's I/O reactor.
fn spawn_on_owner<C, T>(
    command: &mut C,
    replacement: impl FnOnce() -> C,
    spawn: impl FnOnce(&mut C) -> io::Result<T> + Send + 'static,
) -> io::Result<T>
where
    C: Send + 'static,
    T: Send + 'static,
{
    #[cfg(target_os = "linux")]
    {
        use std::sync::{mpsc, LazyLock};
        type Request = Box<dyn FnOnce() + Send>;
        static OWNER: LazyLock<Result<mpsc::SyncSender<Request>, String>> = LazyLock::new(|| {
            let (sender, receiver) = mpsc::sync_channel::<Request>(0);
            std::thread::Builder::new()
                .name("plurx-child-owner".into())
                .spawn(move || {
                    while let Ok(request) = receiver.recv() {
                        request();
                    }
                })
                .map(|_| sender)
                .map_err(|error| error.to_string())
        });
        let owner = OWNER
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        let runtime = tokio::runtime::Handle::try_current().ok();
        let mut owned = std::mem::replace(command, replacement());
        let (sender, receiver) = mpsc::sync_channel(1);
        owner
            .send(Box::new(move || {
                let _runtime = runtime.as_ref().map(tokio::runtime::Handle::enter);
                let result = spawn(&mut owned);
                let _ = sender.send((owned, result));
            }))
            .map_err(|_| io::Error::other("child owner thread stopped"))?;
        let (owned, result) = receiver
            .recv()
            .map_err(|_| io::Error::other("child owner thread lost spawn result"))?;
        *command = owned;
        result
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = replacement;
        spawn(command)
    }
}

#[cfg(unix)]
impl ChildJob {
    pub fn attach(_child: &tokio::process::Child) -> io::Result<Self> {
        Self::attach_pid(0)
    }

    pub(crate) fn attach_pid(_pid: u32) -> io::Result<Self> {
        Ok(Self {
            _registration: None,
        })
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
impl ChildJob {
    pub fn attach(child: &tokio::process::Child) -> io::Result<Self> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("child has no process identifier"))?;
        Self::attach_pid(pid)
    }

    pub(crate) fn attach_pid(pid: u32) -> io::Result<Self> {
        Ok(Self {
            job: job_control::WindowsJob::attach_pid(pid)?,
            _registration: None,
        })
    }
}

#[cfg(unix)]
pub fn signal(pid: u32, signal: ProcessSignal) -> io::Result<bool> {
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

    const TEST_WORK: ChildWork = ChildWork::background("process test");

    /// Re-exec the portable test binary instead of relying on a platform shell.
    fn child(mode: &str) -> tokio::process::Command {
        let mut command =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", "process::tests::child_main", "--nocapture"])
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
            // A daemon stand-in: runs a failing status child and reports only
            // its exit code, so the test can read this process's own stderr.
            Ok("status-parent") => {
                let status = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime")
                    .block_on(status_job_owned(&mut child("fail"), TEST_WORK))
                    .expect("status child");
                print!("status={}", status.code().unwrap_or(-1));
            }
            Ok("sleep") => {
                if let Ok(path) = std::env::var("PLURX_CHILD_PID_FILE") {
                    let path = Path::new(&path);
                    let ready = path.with_extension("ready");
                    std::fs::write(&ready, std::process::id().to_string()).expect("pid file");
                    std::fs::rename(ready, path).expect("publish complete pid file");
                }
                std::thread::sleep(Duration::from_secs(300));
            }
            #[cfg(windows)]
            Ok("live-job-descendant") => {
                let pid_file = std::env::var_os("PLURX_CHILD_PID_FILE").expect("pid file");
                // Test-only deliberate inheritance of the ORIGINAL outer Job,
                // not a second launcher-created Job whose Drop kills the child.
                let _descendant =
                    std::process::Command::new(std::env::current_exe().expect("test executable"))
                        .args(["--exact", "process::tests::child_main", "--nocapture"])
                        .env("PLURX_CHILD_MODE", "sleep")
                        .env("PLURX_CHILD_PID_FILE", &pid_file)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .expect("inherited descendant");
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !Path::new(&pid_file).is_file() {
                    assert!(std::time::Instant::now() < deadline, "descendant not ready");
                    std::thread::sleep(Duration::from_millis(10));
                }
                // std::process::Child Drop does not kill. The root exits and
                // leaves a real active descendant in the retained outer Job.
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn both_streams_are_captured() {
        let output = output_job_owned(&mut child("echo"), TEST_WORK)
            .await
            .expect("child output");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("out-bytes"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("err-bytes"));
    }

    #[tokio::test]
    async fn a_failing_child_reports_status_and_stderr() {
        let output = output_job_owned(&mut child("fail"), TEST_WORK)
            .await
            .expect("child output");
        assert_eq!(output.status.code(), Some(3));
        assert!(String::from_utf8_lossy(&output.stderr).contains("reason"));
    }

    /// Lab F7: a status child inherited the daemon's stderr, so libva and
    /// FFmpeg wrote raw lines into the JSON log.
    #[tokio::test]
    async fn a_status_child_writes_nothing_to_the_daemons_stderr() {
        let output = output_job_owned(&mut child("status-parent"), TEST_WORK)
            .await
            .expect("daemon stand-in");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("status=3"));
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("reason"),
            "the status child's stderr reached the parent's own stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn a_status_childs_stderr_is_captured_for_the_log() {
        let (status, diagnostics) = status_with_diagnostics(&mut child("fail"), TEST_WORK)
            .await
            .expect("failing child");
        assert_eq!(status.code(), Some(3));
        assert!(String::from_utf8_lossy(&diagnostics).contains("reason"));
        let (status, diagnostics) = status_with_diagnostics(&mut child("echo"), TEST_WORK)
            .await
            .expect("succeeding child");
        assert!(status.success());
        let diagnostics = String::from_utf8_lossy(&diagnostics);
        assert!(diagnostics.contains("err-bytes"));
        assert!(!diagnostics.contains("out-bytes"), "stdout is discarded");
    }

    #[tokio::test]
    async fn a_stderr_read_error_leaves_the_exit_status_standing() {
        let status = output_job_owned(&mut child("echo"), TEST_WORK)
            .await
            .expect("child output")
            .status;
        let (settled, diagnostics) = settle_status(
            Ok(status),
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "stderr closed")),
            TEST_WORK,
        )
        .expect("the status wins over a diagnostics read error");
        assert!(settled.success());
        assert!(diagnostics.is_empty());
        assert!(settle_status(
            Err(io::Error::other("wait failed")),
            Ok(b"text".to_vec()),
            TEST_WORK
        )
        .is_err());
    }

    #[tokio::test]
    async fn a_caller_that_configured_inherit_still_gets_bytes() {
        let mut command = child("echo");
        command.stdout(std::process::Stdio::inherit());
        let output = output_job_owned(&mut command, TEST_WORK)
            .await
            .expect("child output");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("out-bytes"));
    }

    #[tokio::test]
    async fn dropping_the_future_kills_the_child() {
        let directory = tempfile::tempdir().expect("pid directory");
        let pid_file = directory.path().join("child.pid");
        let mut command = child("sleep");
        command.env("PLURX_CHILD_PID_FILE", &pid_file);

        let pid: u32 = {
            let output = output_job_owned(&mut command, TEST_WORK);
            tokio::pin!(output);
            // Startup can exceed the cancellation deadline on a busy runner.
            // Keep polling the output future until the child confirms readiness;
            // only then measure cancellation of a running, sleeping child.
            let ready = async {
                loop {
                    match std::fs::read_to_string(&pid_file) {
                        Ok(pid) => break pid.parse::<u32>().expect("numeric child pid"),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        Err(error) => panic!("read sleeping child pid: {error}"),
                    }
                }
            };
            let pid = tokio::select! {
                result = &mut output => panic!("sleeping child exited before readiness: {result:?}"),
                result = tokio::time::timeout(Duration::from_secs(10), ready) => {
                    result.expect("sleeping child published its pid within startup budget")
                }
            };
            let result = tokio::time::timeout(Duration::from_millis(200), &mut output).await;
            assert!(result.is_err(), "sleeping child exceeded the deadline");
            // Leave this scope to drop the actual future, not just its pinned
            // reference, before checking that cancellation killed the child.
            pid
        };

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            !signal(pid, ProcessSignal::Terminate).expect("probe dead child"),
            "the dropped output future left its child alive"
        );
    }

    mod source_inventory {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source_inventory.rs"
        ));
    }

    fn collect_output_call_sites(
        source_root: &Path,
        sites: &mut BTreeMap<String, usize>,
    ) -> io::Result<()> {
        for path in source_inventory::production_files(source_root, &[]) {
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
                    .replace('\\', "/");
                sites.insert(file, count);
            }
        }
        Ok(())
    }

    #[test]
    fn output_job_owned_call_sites_are_the_audited_set() {
        let expected = BTreeMap::from([
            ("ffmpeg.rs".to_owned(), 4),
            ("live_tv.rs".to_owned(), 1),
            ("live_tv/caption_probe.rs".to_owned(), 1),
            ("pipeprobe.rs".to_owned(), 2),
            ("subtitle_ride_along.rs".to_owned(), 1),
            ("subtitles.rs".to_owned(), 1),
            ("transcode/media_origin.rs".to_owned(), 1),
        ]);
        let mut actual = BTreeMap::new();
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory")
            .join("plurxd/src");
        collect_output_call_sites(&source_root, &mut actual).expect("audit plurxd call sites");
        assert_eq!(actual, expected, "update the helper's caller audit");
    }
}

#[cfg(windows)]
pub fn signal(pid: u32, signal: ProcessSignal) -> io::Result<bool> {
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
