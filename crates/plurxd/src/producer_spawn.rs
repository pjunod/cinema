//! One construction path for long-lived FFmpeg producers.
//!
//! The caller still owns stdout, stderr, progress interpretation, and child
//! reaping. This module owns only the launch invariants that had drifted:
//! runtime environment, descriptor inheritance, piped stdio, kill-on-drop,
//! and Windows Job Object attachment.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    Stdout,
    Stderr,
    None,
}

#[derive(Clone, Default)]
pub(crate) struct Descriptors {
    #[cfg(unix)]
    inherited: Vec<(std::os::fd::RawFd, i32)>,
    #[cfg(unix)]
    output_is_cwd: bool,
    #[cfg(windows)]
    handoffs: Vec<WindowsPathHandoff>,
}

impl Descriptors {
    #[cfg(unix)]
    pub(crate) fn from_raw_fds(
        source: Option<std::os::fd::RawFd>,
        output: Option<std::os::fd::RawFd>,
        subtitle: Option<std::os::fd::RawFd>,
        output_is_cwd: bool,
    ) -> Self {
        let inherited = [(source, 3), (output, 4), (subtitle, 5)]
            .into_iter()
            .filter_map(|(fd, target)| fd.map(|fd| (fd, target)))
            .collect();
        Self {
            inherited,
            output_is_cwd,
        }
    }

    #[cfg(unix)]
    pub(crate) fn from_files(
        source: Option<&std::fs::File>,
        output: Option<&std::fs::File>,
        subtitle: Option<&std::fs::File>,
        output_is_cwd: bool,
    ) -> Self {
        use std::os::fd::AsRawFd as _;
        Self::from_raw_fds(
            source.map(std::fs::File::as_raw_fd),
            output.map(std::fs::File::as_raw_fd),
            subtitle.map(std::fs::File::as_raw_fd),
            output_is_cwd,
        )
    }

    #[cfg(unix)]
    fn configure(&self, command: &mut tokio::process::Command) {
        crate::ffmpeg::inherit_raw_file_descriptors_with_cwd(
            command,
            &self.inherited,
            self.output_is_cwd.then_some(4),
        );
    }

    #[cfg(windows)]
    pub(crate) fn with_file(
        mut self,
        role: &'static str,
        file: &std::fs::File,
    ) -> Result<Self, String> {
        self.handoffs.push(WindowsPathHandoff::file(role, file)?);
        Ok(self)
    }

    #[cfg(windows)]
    pub(crate) fn with_directory(
        mut self,
        role: &'static str,
        directory: &plurx_core::fs_secure::SecureDirectory,
    ) -> Result<Self, String> {
        self.handoffs
            .push(WindowsPathHandoff::directory(role, directory)?);
        Ok(self)
    }

    #[cfg(windows)]
    fn configure(&self, _command: &mut tokio::process::Command) {}

    #[cfg(windows)]
    fn verify(&self) -> Result<(), String> {
        self.handoffs
            .iter()
            .try_for_each(WindowsPathHandoff::verify)
    }

    #[cfg(not(windows))]
    fn verify(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(windows)]
#[derive(Clone)]
struct WindowsPathHandoff {
    role: &'static str,
    path: std::path::PathBuf,
    identity: plurx_core::fs_secure::FileIdentity,
    directory: bool,
}

#[cfg(windows)]
impl WindowsPathHandoff {
    fn file(role: &'static str, file: &std::fs::File) -> Result<Self, String> {
        Ok(Self {
            role,
            path: plurx_core::fs_secure::std_file_path(file)
                .map_err(|error| format!("resolving held {role} path: {error}"))?,
            identity: plurx_core::fs_secure::std_file_identity(file)
                .map_err(|error| format!("reading held {role} identity: {error}"))?,
            directory: false,
        })
    }

    fn directory(
        role: &'static str,
        directory: &plurx_core::fs_secure::SecureDirectory,
    ) -> Result<Self, String> {
        Ok(Self {
            role,
            path: directory.path().to_owned(),
            identity: directory
                .identity_blocking()
                .map_err(|error| format!("reading held {role} identity: {error}"))?,
            directory: true,
        })
    }

    fn verify(&self) -> Result<(), String> {
        let current = if self.directory {
            plurx_core::fs_secure::directory_identity_nofollow_blocking(&self.path)
        } else {
            plurx_core::fs_secure::regular_file_identity_nofollow_blocking(&self.path)
        }
        .map_err(|error| format!("reopening held {} path: {error}", self.role))?;
        if current == self.identity {
            Ok(())
        } else {
            Err(format!(
                "held {} path changed before ffmpeg launch",
                self.role
            ))
        }
    }
}

pub(crate) struct SpawnOptions<'a> {
    pub(crate) runtime_cache: &'a Path,
    pub(crate) progress: Progress,
    pub(crate) descriptors: Descriptors,
    pub(crate) env: &'a [(&'a str, &'a OsStr)],
}

pub(crate) struct Spawned {
    pub(crate) child: tokio::process::Child,
    pub(crate) child_job: crate::process_control::ChildJob,
    pub(crate) stdout: tokio::process::ChildStdout,
    pub(crate) stderr: tokio::process::ChildStderr,
}

pub(crate) fn spawn(
    program: &Path,
    args: &[String],
    options: SpawnOptions<'_>,
) -> Result<Spawned, String> {
    let mut command = tokio::process::Command::new(program);
    command.args(producer_args(options.progress, args));
    configure_ffmpeg_runtime(&mut command, options.runtime_cache);
    for (key, value) in options.env {
        command.env(key, value);
    }
    options.descriptors.configure(&mut command);
    options.descriptors.verify()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (mut child, child_job) = crate::process_control::spawn_job_owned(&mut command)
        .map_err(|error| format!("spawning job-owned producer: {error}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.start_kill();
        "producer started without a stdout pipe".to_owned()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.start_kill();
        "producer started without a stderr pipe".to_owned()
    })?;
    Ok(Spawned {
        child,
        child_job,
        stdout,
        stderr,
    })
}

fn producer_args(progress: Progress, args: &[String]) -> Vec<String> {
    let mut full = Vec::with_capacity(args.len() + usize::from(progress != Progress::None) * 2);
    match progress {
        Progress::Stdout => full.extend(["-progress".to_owned(), "pipe:1".to_owned()]),
        Progress::Stderr => full.extend(["-progress".to_owned(), "pipe:2".to_owned()]),
        Progress::None => {}
    }
    full.extend_from_slice(args);
    full
}

/// Give libraries loaded by FFmpeg a cache owned by plurxd.
pub(crate) fn configure_ffmpeg_runtime(
    command: &mut tokio::process::Command,
    runtime_cache: &Path,
) {
    command.env("XDG_CACHE_HOME", runtime_cache);
    command.env("AV_LOG_FORCE_NOCOLOR", "1");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Seek as _, Write as _};
    use tokio::io::AsyncReadExt as _;

    fn child(mode: &str) -> Vec<String> {
        vec![
            "--exact".to_owned(),
            "producer_spawn::tests::child_main".to_owned(),
            "--nocapture".to_owned(),
            format!("--ignored={mode}"),
        ]
    }

    #[test]
    fn child_main() {
        match std::env::var("PLURX_PRODUCER_CHILD_MODE").as_deref() {
            Ok("environment") => {
                let cache = std::env::var("XDG_CACHE_HOME").expect("runtime cache");
                let no_color = std::env::var("AV_LOG_FORCE_NOCOLOR").expect("no color");
                let caller = std::env::var("PLURX_CALLER_ENV").expect("caller env");
                println!("environment={cache}|{no_color}|{caller}");
            }
            #[cfg(unix)]
            Ok("descriptors") => {
                let mut values = Vec::new();
                for target in 3..=5 {
                    let mut value = String::new();
                    std::fs::File::open(format!("/dev/fd/{target}"))
                        .expect("inherited descriptor")
                        .read_to_string(&mut value)
                        .expect("descriptor contents");
                    values.push(value);
                }
                assert!(
                    std::fs::File::open("/dev/fd/6").is_err(),
                    "the descriptor setup leaked a non-reserved child fd"
                );
                println!("descriptors={};fd6=closed", values.join("|"));
            }
            Ok("pipes") => {
                std::io::stdout()
                    .write_all(b"stdout-owned")
                    .expect("stdout");
                std::io::stderr()
                    .write_all(b"stderr-owned")
                    .expect("stderr");
            }
            Ok("hold") => {
                println!("pid={}", std::process::id());
                std::io::stdout().flush().expect("flush child pid");
                std::thread::sleep(std::time::Duration::from_secs(300));
            }
            #[cfg(unix)]
            Ok("low-fd-harness") => run_low_fd_harness(),
            _ => {}
        }
    }

    async fn output(mut spawned: Spawned) -> (String, String) {
        let mut stdout = String::new();
        let mut stderr = String::new();
        spawned
            .stdout
            .read_to_string(&mut stdout)
            .await
            .expect("stdout");
        spawned
            .stderr
            .read_to_string(&mut stderr)
            .await
            .expect("stderr");
        assert!(spawned.child.wait().await.expect("wait").success());
        (stdout, stderr)
    }

    fn holding_spawn() -> Spawned {
        let cache = tempfile::tempdir().expect("runtime cache");
        let mut args = child("hold");
        args.pop();
        let spawned = spawn(
            &std::env::current_exe().expect("test executable"),
            &args,
            SpawnOptions {
                runtime_cache: cache.path(),
                progress: Progress::None,
                descriptors: Descriptors::default(),
                env: &[("PLURX_PRODUCER_CHILD_MODE", OsStr::new("hold"))],
            },
        )
        .expect("spawn holding child");
        // The environment is copied into the child during spawn; the fixture
        // does not need the directory after that point.
        drop(cache);
        spawned
    }

    async fn child_pid(stdout: &mut tokio::process::ChildStdout) -> u32 {
        use tokio::io::AsyncBufReadExt as _;

        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.expect("read child pid") {
            if let Some(pid) = line.trim().strip_prefix("pid=") {
                return pid.parse().expect("numeric child pid");
            }
        }
        panic!("child exited before reporting its pid")
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    #[cfg(windows)]
    fn process_exists(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return false;
        }
        let wait = unsafe { WaitForSingleObject(process, 0) };
        unsafe { CloseHandle(process) };
        wait == WAIT_TIMEOUT
    }

    async fn wait_until_reaped(pid: u32) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while process_exists(pid) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("spawned child was killed and reaped");
    }

    #[tokio::test]
    async fn every_spawn_carries_the_runtime_environment_and_caller_env() {
        let cache = tempfile::tempdir().expect("runtime cache");
        let caller = OsStr::new("present");
        let mut args = child("environment");
        args.pop();
        let mut spawned = spawn(
            &std::env::current_exe().expect("test executable"),
            &args,
            SpawnOptions {
                runtime_cache: cache.path(),
                progress: Progress::None,
                descriptors: Descriptors::default(),
                env: &[
                    ("PLURX_PRODUCER_CHILD_MODE", OsStr::new("environment")),
                    ("PLURX_CALLER_ENV", caller),
                ],
            },
        )
        .expect("spawn child");
        let mut stdout = String::new();
        spawned
            .stdout
            .read_to_string(&mut stdout)
            .await
            .expect("stdout");
        assert!(spawned.child.wait().await.expect("wait").success());
        assert!(stdout.contains(&format!("environment={}|1|present", cache.path().display())));
    }

    #[test]
    fn progress_flag_leads_argv_per_option() {
        let tail = vec!["-hide_banner".to_owned(), "input.mkv".to_owned()];
        assert_eq!(
            producer_args(Progress::Stdout, &tail),
            ["-progress", "pipe:1", "-hide_banner", "input.mkv"].map(str::to_owned)
        );
        assert_eq!(
            producer_args(Progress::Stderr, &tail),
            ["-progress", "pipe:2", "-hide_banner", "input.mkv"].map(str::to_owned)
        );
        assert_eq!(producer_args(Progress::None, &tail), tail);
    }

    #[tokio::test]
    async fn both_pipes_are_returned_and_owned_by_the_caller() {
        let cache = tempfile::tempdir().expect("runtime cache");
        let mut args = child("pipes");
        args.pop();
        let spawned = spawn(
            &std::env::current_exe().expect("test executable"),
            &args,
            SpawnOptions {
                runtime_cache: cache.path(),
                progress: Progress::None,
                descriptors: Descriptors::default(),
                env: &[("PLURX_PRODUCER_CHILD_MODE", OsStr::new("pipes"))],
            },
        )
        .expect("spawn child");
        let (stdout, stderr) = output(spawned).await;
        assert!(stdout.contains("stdout-owned"));
        assert!(stderr.contains("stderr-owned"));
    }

    #[tokio::test]
    async fn dropping_a_live_spawned_child_kills_and_reaps_it() {
        let mut spawned = holding_spawn();
        let pid = child_pid(&mut spawned.stdout).await;
        assert!(process_exists(pid), "fixture child is live before drop");
        drop(spawned);
        wait_until_reaped(pid).await;
    }

    #[tokio::test]
    async fn job_owned_spawn_follows_the_vod_slot_through_confirmed_reap() {
        let mut spawned = holding_spawn();
        let pid = child_pid(&mut spawned.stdout).await;
        let Spawned {
            child,
            child_job,
            stdout,
            stderr,
        } = spawned;
        drop((stdout, stderr));

        let slot = crate::prodrun::ProducerSlot::new();
        slot.attach_job_owned(child, child_job, 0, None).await;
        slot.perform(
            crate::prodexec::Step::Terminate {
                why: crate::prodexec::Termination::Idle,
            },
            || {},
        )
        .await
        .expect("terminate and reap the shared-builder child");
        wait_until_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptors_land_on_three_four_five() {
        let cache = tempfile::tempdir().expect("runtime cache");
        let mut files = Vec::new();
        for value in ["source", "output", "subtitle"] {
            let mut file = tempfile::tempfile().expect("descriptor file");
            file.write_all(value.as_bytes()).expect("fixture contents");
            file.rewind().expect("rewind fixture");
            files.push(file);
        }
        let mut args = child("descriptors");
        args.pop();
        let spawned = spawn(
            &std::env::current_exe().expect("test executable"),
            &args,
            SpawnOptions {
                runtime_cache: cache.path(),
                progress: Progress::None,
                descriptors: Descriptors::from_files(
                    files.first(),
                    files.get(1),
                    files.get(2),
                    false,
                ),
                env: &[("PLURX_PRODUCER_CHILD_MODE", OsStr::new("descriptors"))],
            },
        )
        .expect("spawn child");
        let (stdout, _) = output(spawned).await;
        assert!(stdout.contains("descriptors=source|output|subtitle;fd6=closed"));
    }

    #[cfg(unix)]
    fn run_low_fd_harness() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let mut original = tempfile::tempfile().expect("source descriptor");
        original.write_all(b"source").expect("source contents");
        original.rewind().expect("rewind source");
        let source = if original.as_raw_fd() == 4 {
            original
        } else {
            assert_eq!(unsafe { libc::dup2(original.as_raw_fd(), 4) }, 4);
            unsafe { std::fs::File::from_raw_fd(4) }
        };
        assert_eq!(
            source.as_raw_fd(),
            4,
            "fixture source must collide with output target"
        );

        let mut output_file = tempfile::tempfile().expect("output descriptor");
        output_file.write_all(b"output").expect("output contents");
        output_file.rewind().expect("rewind output");
        let mut subtitle = tempfile::tempfile().expect("subtitle descriptor");
        subtitle.write_all(b"subtitle").expect("subtitle contents");
        subtitle.rewind().expect("rewind subtitle");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("low-fd runtime");
        let (stdout, _) = runtime.block_on(async {
            let cache = tempfile::tempdir().expect("runtime cache");
            let mut args = child("descriptors");
            args.pop();
            let spawned = spawn(
                &std::env::current_exe().expect("test executable"),
                &args,
                SpawnOptions {
                    runtime_cache: cache.path(),
                    progress: Progress::None,
                    descriptors: Descriptors::from_files(
                        Some(&source),
                        Some(&output_file),
                        Some(&subtitle),
                        false,
                    ),
                    env: &[("PLURX_PRODUCER_CHILD_MODE", OsStr::new("descriptors"))],
                },
            )
            .expect("spawn low-fd child");
            output(spawned).await
        });
        assert!(stdout.contains("descriptors=source|output|subtitle;fd6=closed"));
        println!("low-fd-harness=ok");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_dup_survives_an_unlucky_low_fd() {
        let mut args = child("low-fd-harness");
        args.pop();
        let result =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(args)
                .env("PLURX_PRODUCER_CHILD_MODE", "low-fd-harness")
                .kill_on_drop(true)
                .output()
                .await
                .expect("run isolated low-fd harness");
        assert!(
            result.status.success(),
            "low-fd harness failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("low-fd-harness=ok"));
    }
}
