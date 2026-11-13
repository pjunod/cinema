//! One construction path for long-lived FFmpeg producers.
//!
//! The caller still owns stdout, stderr, progress interpretation, and child
//! reaping. This module owns only the launch invariants that had drifted:
//! runtime environment, descriptor inheritance, piped stdio, kill-on-drop,
//! and Windows Job Object attachment.

#[cfg(unix)]
use plurx_core::process::source_lifetime::InheritedFd;
pub(crate) use plurx_core::process::source_lifetime::{DescriptorManifest, StdioFacts, StdioMode};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) enum ChildStdio<'a> {
    Null,
    Piped,
    // The file-stdio boundary is exercised by the descriptor contract tests;
    // current production launches use Null/Piped. Keep its audit in the builder.
    #[cfg_attr(not(test), allow(dead_code))]
    File(&'a std::fs::File),
}

#[derive(Clone, Copy)]
pub(crate) struct ChildStdios<'a> {
    pub(crate) stdin: ChildStdio<'a>,
    pub(crate) stdout: ChildStdio<'a>,
    pub(crate) stderr: ChildStdio<'a>,
}
impl ChildStdios<'_> {
    pub(crate) const CAPTURED: Self = Self {
        stdin: ChildStdio::Null,
        stdout: ChildStdio::Piped,
        stderr: ChildStdio::Piped,
    };
}

/// Bind only after all command configuration, in the exact synchronous launch
/// scope. A future must call this at its actual spawn, not while being created.
pub(crate) fn with_manifest<T>(
    manifest: &Arc<DescriptorManifest>,
    command: &mut tokio::process::Command,
    launch: impl FnOnce(&mut tokio::process::Command) -> std::io::Result<T>,
) -> std::io::Result<T> {
    manifest.bind(command.as_std())?;
    plurx_core::process::source_lifetime::with_descriptors(Arc::clone(manifest), || launch(command))
}

#[cfg(test)]
pub(crate) fn with_std_manifest<T>(
    manifest: &Arc<DescriptorManifest>,
    command: &mut std::process::Command,
    launch: impl FnOnce(&mut std::process::Command) -> std::io::Result<T>,
) -> std::io::Result<T> {
    manifest.bind(command)?;
    plurx_core::process::source_lifetime::with_descriptors(Arc::clone(manifest), || launch(command))
}

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
    #[cfg(unix)]
    cwd_target: Option<i32>,
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
            cwd_target: None,
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

    /// One-input copy observation outputs. The caller retains these handles
    /// through spawn and concurrently drains both read ends until joined.
    #[cfg(unix)]
    pub(crate) fn with_copy_measurements(
        mut self,
        canonical: &std::fs::File,
        clock: &std::fs::File,
    ) -> Self {
        use std::os::fd::AsRawFd as _;
        self.inherited.push((canonical.as_raw_fd(), 6));
        self.inherited.push((clock.as_raw_fd(), 7));
        self
    }

    #[cfg(unix)]
    pub(crate) fn from_mappings(
        mappings: &[(std::os::fd::RawFd, i32)],
        cwd_target: Option<i32>,
    ) -> Self {
        Self {
            inherited: mappings.to_vec(),
            output_is_cwd: false,
            cwd_target,
        }
    }

    /// Inline plans are for the decoder's custom exec hook: every descriptor
    /// is pinned before fork; inherit is allocation-free and may run there.
    #[cfg(unix)]
    pub(crate) fn prepare_inline(
        &self,
        stdio: StdioFacts,
    ) -> std::io::Result<Arc<DescriptorManifest>> {
        let inherited = self
            .inherited
            .iter()
            .map(|&(source, target)| InheritedFd::duplicate_raw(source, target))
            .collect::<std::io::Result<Vec<_>>>()?;
        DescriptorManifest::new(inherited, stdio, self.effective_cwd()).map(Arc::new)
    }

    #[cfg(unix)]
    fn effective_cwd(&self) -> Option<i32> {
        if self.output_is_cwd && cfg!(target_os = "macos") {
            return Some(4);
        }
        self.cwd_target
    }

    /// The only ordinary daemon descriptor-install hook. Security hooks with a
    /// custom exec use prepare_inline and invoke the same prepared plan there.
    pub(crate) fn install(manifest: &Arc<DescriptorManifest>, command: &mut std::process::Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let manifest = Arc::clone(manifest);
            // SAFETY: the prepared plan performs only descriptor syscalls.
            unsafe {
                command.pre_exec(move || manifest.inherit());
            }
        }
        #[cfg(not(unix))]
        let _ = (manifest, command);
    }

    pub(crate) fn prepare(
        &self,
        command: &mut tokio::process::Command,
        stdio: ChildStdios<'_>,
    ) -> std::io::Result<Arc<DescriptorManifest>> {
        self.prepare_std(command.as_std_mut(), stdio)
    }

    pub(crate) fn prepare_std(
        &self,
        command: &mut std::process::Command,
        stdio: ChildStdios<'_>,
    ) -> std::io::Result<Arc<DescriptorManifest>> {
        #[cfg(unix)]
        let mut inherited = self
            .inherited
            .iter()
            .map(|&(source, target)| InheritedFd::duplicate_raw(source, target))
            .collect::<std::io::Result<Vec<_>>>()?;
        #[cfg(not(unix))]
        let inherited = Vec::new();
        let mut facts = StdioFacts::NULL;
        for (target, setting) in [(0, stdio.stdin), (1, stdio.stdout), (2, stdio.stderr)] {
            let (value, mode) = match setting {
                ChildStdio::Null => (Stdio::null(), StdioMode::Null),
                ChildStdio::Piped => (Stdio::piped(), StdioMode::Piped),
                #[cfg(unix)]
                ChildStdio::File(file) => {
                    inherited.push(InheritedFd::duplicate(file, target)?);
                    (Stdio::null(), StdioMode::Descriptor(target))
                }
                #[cfg(not(unix))]
                ChildStdio::File(file) => (Stdio::from(file.try_clone()?), StdioMode::Inherited),
            };
            match target {
                0 => {
                    command.stdin(value);
                    facts.stdin = mode;
                }
                1 => {
                    command.stdout(value);
                    facts.stdout = mode;
                }
                _ => {
                    command.stderr(value);
                    facts.stderr = mode;
                }
            }
        }
        #[cfg(unix)]
        let cwd = self.effective_cwd();
        #[cfg(not(unix))]
        let cwd = None;
        let manifest = Arc::new(DescriptorManifest::new(inherited, facts, cwd)?);
        Self::install(&manifest, command);
        Ok(manifest)
    }

    /// Compatibility for held-source callers: installing a descriptor does
    /// not by itself verify their declaration or manufacture output identity.
    #[cfg(unix)]
    pub(crate) fn install_legacy(
        command: &mut tokio::process::Command,
        mappings: &[(std::os::fd::RawFd, i32)],
        cwd_target: Option<i32>,
    ) {
        if mappings.is_empty() {
            return;
        }
        plurx_core::process::source_lifetime::tag_descriptor_handoff(command);
        let result = plurx_core::process::source_lifetime::unmediated_descriptor().and_then(|()| {
            let plan = Self::from_mappings(mappings, cwd_target).prepare_inline(StdioFacts {
                stdin: StdioMode::Inherited,
                stdout: StdioMode::Inherited,
                stderr: StdioMode::Inherited,
            })?;
            Self::install(&plan, command.as_std_mut());
            Ok(())
        });
        if let Err(error) = result {
            Self::reject(command.as_std_mut(), error);
        }
    }

    #[cfg(unix)]
    pub(crate) fn reject(command: &mut std::process::Command, error: std::io::Error) {
        use std::os::unix::process::CommandExt;
        let code = error.raw_os_error().unwrap_or(libc::EIO);
        // SAFETY: only a captured errno is returned in the child.
        unsafe {
            command.pre_exec(move || Err(std::io::Error::from_raw_os_error(code)));
        }
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
    /// Who is waiting on this producer: a viewer's session, or nobody (the
    /// pre-transcode pass). Sets the child's priority class.
    pub(crate) work: crate::process_control::ChildWork,
}

pub(crate) struct Spawned {
    pub(crate) child: tokio::process::Child,
    pub(crate) child_job: crate::process_control::ChildJob,
    pub(crate) stdout: tokio::process::ChildStdout,
    pub(crate) stderr: tokio::process::ChildStderr,
}

/// A cache part may retain daemon-only reader lifetime proof only when both
/// original input and output use the existing loopback brokers. The caller
/// has already rewritten the source argv and selected the upload URL; a raw
/// subtitle (including an embedded-subtitle filter without a subtitle FD) or
/// any unknown descriptor keeps the conservative handoff path.
pub(crate) fn brokered_cache_part<T>(
    input_url: Option<&str>,
    output_url: Option<&str>,
    subtitle_burn: bool,
    descriptors: Descriptors,
    spawn: impl FnOnce(Descriptors) -> T,
) -> T {
    #[cfg(unix)]
    {
        let loopback = |raw: &str| {
            reqwest::Url::parse(raw).ok().is_some_and(|url| {
                url.scheme() == "http"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url
                        .host_str()
                        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                        .is_some_and(|host| host.is_loopback())
            })
        };
        if !subtitle_burn
            && input_url.is_some_and(loopback)
            && output_url.is_some_and(loopback)
            && descriptors
                .inherited
                .iter()
                .all(|(_, target)| matches!(*target, 3 | 4))
        {
            // Neither capability is used after its argv was brokered. In
            // particular, do not hand the original source FD to an HTTP child.
            return plurx_core::process::source_lifetime::mediated_spawn(|| {
                spawn(Descriptors::default())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (input_url, output_url, subtitle_burn);
    spawn(descriptors)
}

pub(crate) fn spawn(
    program: &Path,
    args: &[String],
    options: SpawnOptions<'_>,
) -> Result<Spawned, String> {
    let mut command = tokio::process::Command::new(program);
    command.args(producer_args(options.progress, args));
    #[cfg(target_os = "linux")]
    command.process_group(0);
    configure_ffmpeg_runtime(&mut command, options.runtime_cache);
    for (key, value) in options.env {
        command.env(key, value);
    }
    options.descriptors.verify()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let manifest = options
        .descriptors
        .prepare(&mut command, ChildStdios::CAPTURED)
        .map_err(|error| format!("preparing producer descriptors: {error}"))?;
    let (mut child, child_job) = with_manifest(&manifest, &mut command, |command| {
        crate::process_control::spawn_job_owned(command, options.work)
    })
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
    let driver_cache = runtime_cache.join("gpu-cache");
    command.env("XDG_CACHE_HOME", &driver_cache);
    // CUDA's driver JIT cache does not follow XDG_CACHE_HOME. In a service
    // with HOME=/, its default is unwritable and every process recompiles.
    command.env("CUDA_CACHE_PATH", &driver_cache);
    command.env("CUDA_CACHE_MAXSIZE", "134217728");
    command.env("MESA_SHADER_CACHE_MAX_SIZE", "128M");
    // Driver writes happen below FFmpeg's output API and cannot participate
    // in producer admission. Keep those unmanaged disk writers off; existing
    // disposable GPU artifacts remain eligible for the shared LRU.
    command.env("CUDA_CACHE_DISABLE", "1");
    command.env("MESA_SHADER_CACHE_DISABLE", "true");
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
                assert_eq!(std::env::var("CUDA_CACHE_PATH").expect("CUDA cache"), cache);
                let no_color = std::env::var("AV_LOG_FORCE_NOCOLOR").expect("no color");
                let caller = std::env::var("PLURX_CALLER_ENV").expect("caller env");
                println!("environment={cache}|{no_color}|{caller}");
            }
            #[cfg(unix)]
            Ok("custom-descriptors") => {
                let mut input = String::new();
                std::io::stdin()
                    .read_to_string(&mut input)
                    .expect("file stdin");
                let mut held = String::new();
                std::fs::File::open("/dev/fd/198")
                    .expect("fd 198")
                    .read_to_string(&mut held)
                    .expect("held bytes");
                let relative = std::fs::read_to_string("relative-marker").expect("descriptor cwd");
                eprintln!("file-stdio-diagnostic");
                println!("custom={input}|{held}|{relative}");
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
                work: crate::process_control::ChildWork::background("producer test"),
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
                work: crate::process_control::ChildWork::background("producer test"),
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
        assert!(stdout.contains(&format!(
            "environment={}|1|present",
            cache.path().join("gpu-cache").display()
        )));
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
                work: crate::process_control::ChildWork::background("producer test"),
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
                work: crate::process_control::ChildWork::background("producer test"),
            },
        )
        .expect("spawn child");
        let (stdout, _) = output(spawned).await;
        assert!(stdout.contains("descriptors=source|output|subtitle;fd6=closed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn arbitrary_target_cwd_and_file_stdio_reach_the_exact_child() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir().expect("child directory");
        std::fs::write(directory.path().join("relative-marker"), "cwd-bound").expect("cwd marker");
        let cwd = std::fs::File::open(directory.path()).expect("held cwd");
        let mut source = tempfile::tempfile().expect("held source");
        source.write_all(b"fd-198").expect("source bytes");
        source.rewind().expect("rewind source");
        let mut input = tempfile::tempfile().expect("input file");
        input.write_all(b"file-stdin").expect("input bytes");
        input.rewind().expect("rewind input");
        let mut diagnostics = tempfile::tempfile().expect("diagnostics file");
        let mut command =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"));
        let mut args = child("custom-descriptors");
        args.pop();
        command
            .args(args)
            .env("PLURX_PRODUCER_CHILD_MODE", "custom-descriptors");
        let descriptors =
            Descriptors::from_mappings(&[(source.as_raw_fd(), 198), (cwd.as_raw_fd(), 4)], Some(4));
        let manifest = descriptors
            .prepare(
                &mut command,
                ChildStdios {
                    stdin: ChildStdio::File(&input),
                    stdout: ChildStdio::Piped,
                    stderr: ChildStdio::File(&diagnostics),
                },
            )
            .expect("pinned descriptors and stdio");
        let (child, _job) = with_manifest(&manifest, &mut command, |command| {
            crate::process_control::spawn_job_owned(
                command,
                crate::process_control::ChildWork::background("descriptor builder test"),
            )
        })
        .expect("job-owned child");
        let output = child.wait_with_output().await.expect("joined child");
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("custom=file-stdin|fd-198|cwd-bound")
        );
        diagnostics.rewind().expect("rewind diagnostics");
        let mut log = String::new();
        diagnostics.read_to_string(&mut log).expect("file stderr");
        assert!(log.contains("file-stdio-diagnostic"));
        assert!(
            Descriptors::from_mappings(
                &[(source.as_raw_fd(), 198), (input.as_raw_fd(), 198)],
                None
            )
            .prepare_inline(StdioFacts::CAPTURED)
            .is_err(),
            "two different capabilities cannot target the same child descriptor"
        );
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
                    work: crate::process_control::ChildWork::background("producer test"),
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
