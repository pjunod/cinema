//! Stock Pi kernels can omit Landlock. The alternative remains a mandatory
//! namespace boundary followed by the same descriptor-bound seccomp launch.
use super::*;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;

const BOOTSTRAP: &str = "--internal-pi-probe-bootstrap";
const BWRAP: &str = "/usr/bin/bwrap";
#[cfg(target_arch = "aarch64")]
const LIBRARY_DIRECTORY: &str = "/usr/lib/aarch64-linux-gnu";
#[cfg(target_arch = "aarch64")]
const LOADER_TARGET: &str = "aarch64-linux-gnu/ld-linux-aarch64.so.1";
#[cfg(target_arch = "aarch64")]
const LOADER_PATH: &str = "/lib/ld-linux-aarch64.so.1";
#[cfg(not(target_arch = "aarch64"))]
const LIBRARY_DIRECTORY: &str = "/usr/lib/x86_64-linux-gnu";
#[cfg(not(target_arch = "aarch64"))]
const LOADER_TARGET: &str = "/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2";
#[cfg(not(target_arch = "aarch64"))]
const LOADER_PATH: &str = "/lib64/ld-linux-x86-64.so.2";
const NAMESPACES: [&str; 6] = ["user", "pid", "mnt", "net", "ipc", "uts"];

fn compatible_is_pi(bytes: &[u8]) -> bool {
    bytes
        .split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(b"raspberrypi,"))
}

fn unsupported_landlock(error: Option<i32>) -> bool {
    matches!(error, Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP))
}

pub(super) fn required(mode: ProbeLaunchMode) -> bool {
    #[cfg(test)]
    if mode == ProbeLaunchMode::ProductionNamespace {
        return true;
    }
    if !mode.is_production() || !cfg!(target_arch = "aarch64") {
        return false;
    }
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi >= 0 || !unsupported_landlock(std::io::Error::last_os_error().raw_os_error()) {
        return false;
    }
    [
        "/sys/firmware/devicetree/base/compatible",
        "/run/plurx-platform/compatible",
    ]
    .iter()
    .any(|path| {
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return false;
        }
        let mut bytes = Vec::new();
        use std::io::Read;
        file.take(4097).read_to_end(&mut bytes).is_ok()
            && bytes.len() <= 4096
            && compatible_is_pi(&bytes)
    })
}

pub(super) fn launch_path(
    snapshot: &ExecutableSnapshot,
    mode: ProbeLaunchMode,
) -> Result<PathBuf, DecodeFactError> {
    if !required(mode) {
        return Ok(snapshot_execution_path(snapshot));
    }
    // Never accept a caller-supplied launcher or a setuid launcher.
    let path =
        std::fs::canonicalize(BWRAP).map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let mut checked = Some(path.as_path());
    while let Some(component) = checked {
        let metadata = std::fs::metadata(component)
            .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
        if metadata.uid() != 0 || metadata.mode() & 0o6022 != 0 {
            return Err(DecodeFactError::Spawn("Pi namespace launcher and ancestors must be root-owned and immutable to other users".into()));
        }
        checked = component.parent();
    }
    Ok(PathBuf::from(BWRAP))
}

fn namespace_id(name: &str) -> std::io::Result<u64> {
    Ok(std::fs::metadata(format!("/proc/self/ns/{name}"))?.ino())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn configure(
    command: &mut tokio::process::Command,
    mode: ProbeLaunchMode,
    executable_fd: std::os::fd::RawFd,
    source_fd: Option<std::os::fd::RawFd>,
    arg0: &Path,
    arguments: &[OsString],
    class: crate::process_control::ChildClass,
    deadline: std::time::Instant,
) -> Result<ProbeExecutionSupervisor, DecodeFactError> {
    let bootstrap_path = PathBuf::from("/proc/self/exe");
    #[cfg(test)]
    let bootstrap_path = if mode == ProbeLaunchMode::ProductionNamespace {
        PathBuf::from(
            std::env::var_os("PLURX_TEST_NAMESPACE_BOOTSTRAP").ok_or_else(|| {
                DecodeFactError::Spawn(
                    "namespace regression requires a compiled daemon bootstrap".into(),
                )
            })?,
        )
    } else {
        bootstrap_path
    };
    let bootstrap = std::fs::File::open(bootstrap_path)
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let supervisor = LinuxProbeExecSupervisor::start(mode, deadline)
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let sender = supervisor.child_sender_fd();
    let receiver = supervisor.child_receiver_fd();
    let namespace_ids = NAMESPACES
        .iter()
        .map(|name| namespace_id(name).map(|id| id.to_string()))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let remaining = deadline
        .saturating_duration_since(std::time::Instant::now())
        .as_millis();
    if remaining == 0 {
        return Err(DecodeFactError::Deadline);
    }
    command.as_std_mut().arg0(BWRAP);
    command
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--as-pid-1",
            "--die-with-parent",
            "--cap-drop",
            "ALL",
            "--clearenv",
            "--chdir",
            "/",
            "--perms",
            "0500",
            "--ro-bind-data",
            "7",
            "/probe-bootstrap",
            "--ro-bind",
            LIBRARY_DIRECTORY,
            LIBRARY_DIRECTORY,
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            LOADER_TARGET,
            LOADER_PATH,
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--",
            "/probe-bootstrap",
            BOOTSTRAP,
        ])
        .arg(remaining.to_string())
        .arg(namespace_ids.join(","))
        .arg(arg0)
        .args(arguments);
    crate::process_control::priority::apply(command, class);
    unsafe {
        command.pre_exec(move || {
            start_probe_session()?;
            // Duplicate every additional capability before the fixed-FD moves.
            let bootstrap_copy = duplicate_child_fd(bootstrap.as_raw_fd())?;
            let sender_copy = match duplicate_child_fd(sender) {
                Ok(fd) => fd,
                Err(error) => {
                    libc::close(bootstrap_copy);
                    return Err(error);
                }
            };
            let result = (|| {
                match source_fd {
                    Some(source) => install_probe_child_fds(source, executable_fd)?,
                    None => {
                        install_child_fd(executable_fd, HELD_PROBE_FD)?;
                        libc::close(3);
                    }
                }
                assign_child_fd(bootstrap_copy, 5)?;
                assign_child_fd(sender_copy, 6)?;
                assign_child_fd(bootstrap_copy, 7)?;
                Ok(())
            })();
            libc::close(bootstrap_copy);
            libc::close(sender_copy);
            result?;
            libc::close(receiver);
            // Preserve only parser/bootstrap/control and consumed bootstrap-data FD7 across bwrap.
            if libc::syscall(
                libc::SYS_close_range,
                8_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(ProbeExecutionSupervisor::linux(supervisor))
}

/// Called synchronously before a Tokio runtime or configuration access. The
/// namespace bootstrap is a launcher entry, never a daemon or a parser shim.
pub(super) fn dispatch() -> Option<std::io::Result<()>> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    if arguments.next().as_deref() != Some(OsStr::new(BOOTSTRAP)) {
        return None;
    }
    Some(bootstrap(arguments.collect()))
}

fn bootstrap(arguments: Vec<OsString>) -> std::io::Result<()> {
    if arguments.len() < 3 {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    let millis = arguments[0]
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0 && *value <= 10_000)
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let identities = arguments[1]
        .to_str()
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?
        .split(',')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    if identities.len() != NAMESPACES.len()
        || NAMESPACES
            .iter()
            .zip(identities)
            .any(|(name, host)| namespace_id(name).map_or(true, |current| current == host))
    {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    // The executable bind must refer to the exact descriptor held by the parent,
    // not a mutable configured pathname or another namespace bootstrap image.
    if bootstrap_digest("/proc/self/fd/5")? != bootstrap_digest("/proc/self/exe")? {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    // No unsandboxed init peer may remain visible in this namespace. PID 1
    // also guarantees the kernel kills every descendant when this probe exits.
    if unsafe { libc::getpid() } != 1 {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    let seals = unsafe { libc::fcntl(HELD_PROBE_FD, libc::F_GET_SEALS) };
    let required = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if seals < 0 || seals & required != required {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(millis);
    let args = LinuxExecveArguments::new(&arguments[2], &arguments[3..])?;
    let seccomp = build_linux_probe_seccomp()?;
    let post_transfer = build_linux_probe_post_transfer_seccomp()?;
    let mut interrupts = 0;
    confirm_linux_probe_fork(6, deadline, &mut interrupts)?;
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // PID/mount isolation removes peer /proc and every host path except the
    // read-only runtime libraries; also prohibit cross-process memory APIs.
    let isolation = build_namespace_memory_seccomp()?;
    isolation.install()?;
    let listener = seccomp.install_listener()?;
    send_linux_seccomp_listener(6, listener.as_raw_fd())?;
    post_transfer.install()?;
    unsafe {
        libc::close(5);
        libc::close(6);
    }
    mark_unrelated_fds_close_on_exec()?;
    drop(listener);
    args.execute_held_probe()
}

fn bootstrap_digest(path: &str) -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?.take(MAX_PROBE_EXECUTABLE_BYTES + 1);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_PROBE_EXECUTABLE_BYTES {
            return Err(std::io::Error::from_raw_os_error(libc::EFBIG));
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

fn build_namespace_memory_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    let mut filter = vec![
        seccomp_statement(0x20, SECCOMP_DATA_ARCH_OFFSET),
        seccomp_jump(0x15, LINUX_AUDIT_ARCH, 1, 0),
        seccomp_statement(0x06, SECCOMP_RET_KILL_PROCESS),
        seccomp_statement(0x20, 0),
    ];
    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
    ] {
        filter.push(seccomp_jump(0x15, syscall as u32, 0, 1));
        filter.push(seccomp_statement(
            0x06,
            SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ));
    }
    // Namespace creation must not be recovered through clone or clone3.
    filter.extend([
        seccomp_jump(0x15, libc::SYS_clone3 as u32, 0, 1),
        seccomp_statement(0x06, SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
        seccomp_jump(0x15, libc::SYS_clone as u32, 0, 3),
        seccomp_statement(0x20, SECCOMP_DATA_ARGS_OFFSET),
        seccomp_jump(
            0x45,
            (libc::CLONE_NEWUSER
                | libc::CLONE_NEWPID
                | libc::CLONE_NEWNS
                | libc::CLONE_NEWNET
                | libc::CLONE_NEWIPC
                | libc::CLONE_NEWUTS
                | libc::CLONE_NEWCGROUP) as u32,
            0,
            1,
        ),
        seccomp_statement(0x06, SECCOMP_RET_ERRNO | libc::EPERM as u32),
    ]);
    filter.push(seccomp_statement(0x06, SECCOMP_RET_ALLOW));
    let filter_len =
        u16::try_from(filter.len()).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    Ok(LinuxProbeSeccomp { filter, filter_len })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pi_namespace_requires_exact_compatible_prefix_and_unsupported_landlock() {
        assert!(compatible_is_pi(b"raspberrypi,5-model-b\0brcm,bcm2712\0"));
        assert!(!compatible_is_pi(b"brcm,bcm2712\0"));
        assert!(!compatible_is_pi(b"other,raspberrypi,5-model-b\0"));
        assert!(unsupported_landlock(Some(libc::ENOSYS)));
        assert!(unsupported_landlock(Some(libc::EOPNOTSUPP)));
        assert!(!unsupported_landlock(Some(libc::EPERM)));
        assert!(!unsupported_landlock(None));
    }
    #[test]
    fn namespace_bootstrap_refuses_missing_boundary_metadata() {
        assert_eq!(
            bootstrap(Vec::new()).unwrap_err().raw_os_error(),
            Some(libc::EINVAL)
        );
        assert_eq!(
            bootstrap(vec!["1".into(), "0".into(), "probe".into()])
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
    }
}
