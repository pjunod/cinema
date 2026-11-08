//! Atomic Linux native-domain entry. This foundation exposes no production
//! launch-profile constructor and grants no whole-attempt resource bound.
//!
//! Domain ownership, root wait, exec-error reporting, and output-pipe closure
//! remain separate facts. In particular EOF on the exec-error pipe is not
//! proof of exec: a signal before exec can also close that pipe.

#![cfg(all(target_os = "linux", any(test, feature = "fixtures")))]

use super::containment::{NativeDomainExtinction, OwnedNativeDomain};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

mod linux;
mod sandbox;

const MAX_ROOTS: usize = 16;
const MAX_STRINGS: usize = 64;
const MAX_STRING_BYTES: usize = 16 * 1024;
static ROOTS: AtomicUsize = AtomicUsize::new(0);

/// No production constructor exists until a concrete executable/profile and
/// descriptor policy have been audited. Arbitrary Command/pre_exec is absent.
pub(super) struct SealedLaunchSpec {
    executable: File,
    input: File,
    arguments: Vec<CString>,
    environment: Vec<CString>,
    sandbox: Option<sandbox::PreparedSandbox>,
    finite_inputs: Vec<File>,
}

impl SealedLaunchSpec {
    #[cfg(test)]
    fn test_executable(
        executable: File,
        input: File,
        arguments: Vec<CString>,
        environment: Vec<CString>,
    ) -> io::Result<Self> {
        for file in [&executable, &input] {
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "test executable/input must be regular files",
                ));
            }
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            if flags & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "test executable/input must be read-only",
                ));
            }
        }
        let spec = Self {
            executable,
            input,
            arguments,
            environment,
            sandbox: None,
            finite_inputs: Vec::new(),
        };
        spec.validate()?;
        Ok(spec)
    }

    #[cfg(test)]
    fn test_finite_executable(
        executable: File,
        inputs: Vec<File>,
        arguments: Vec<CString>,
        environment: Vec<CString>,
    ) -> io::Result<Self> {
        if !(1..=2).contains(&inputs.len()) || inputs.capacity() > 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "finite input slots exceed fixed profile",
            ));
        }
        for file in std::iter::once(&executable).chain(inputs.iter()) {
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            if !file.metadata()?.is_file() || flags & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "finite executable/input must be a read-only regular object",
                ));
            }
        }
        let spec = Self {
            executable,
            input: File::open("/dev/null")?,
            arguments,
            environment,
            sandbox: None,
            finite_inputs: inputs,
        };
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> io::Result<()> {
        let count = self.arguments.len().checked_add(self.environment.len());
        let capacity = self
            .arguments
            .capacity()
            .checked_add(self.environment.capacity());
        let bytes = self
            .arguments
            .iter()
            .chain(&self.environment)
            .try_fold(0usize, |sum, value| {
                sum.checked_add(value.as_bytes_with_nul().len())
            });
        if self.finite_inputs.len() > 2
            || self.finite_inputs.capacity() > 2
            || self.arguments.is_empty()
            || count.is_none_or(|n| n > MAX_STRINGS)
            || capacity.is_none_or(|n| n > MAX_STRINGS)
            || bytes.is_none_or(|n| n > MAX_STRING_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sealed launch strings exceed fixed budget",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) enum LaunchRefusal {
    Unavailable(io::Error),
    Refused(io::Error),
}

/// A launch refusal retains the original owner, including uncertain cleanup.
pub(super) struct LaunchFailure {
    pub domain: OwnedNativeDomain,
    pub reason: LaunchRefusal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExecReport {
    Pending,
    ClosedWithoutReportedError,
    Failed { stage: u32, errno: i32 },
    InvalidProtocol,
}

struct Slot;
impl Slot {
    fn acquire() -> io::Result<Self> {
        ROOTS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_ROOTS).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "contained root custody slots exhausted",
                )
            })
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ROOTS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Custody {
    domain: OwnedNativeDomain,
    root: Root,
    _slot: Slot,
}

struct Root {
    pid: libc::pid_t,
    pidfd: OwnedFd,
    wait_status: Option<i32>,
}

impl Root {
    fn try_wait(&mut self) -> io::Result<Option<i32>> {
        if self.wait_status.is_some() {
            return Ok(self.wait_status);
        }
        loop {
            let mut information = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    self.pidfd.as_raw_fd() as libc::id_t,
                    &mut information,
                    libc::WEXITED | libc::WNOHANG,
                )
            };
            if result == 0 {
                let observed_pid = unsafe { information.si_pid() };
                if observed_pid == 0 {
                    return Ok(None);
                }
                if observed_pid != self.pid {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "original pidfd returned inconsistent root identity",
                    ));
                }
                self.wait_status = Some(unix_wait_status(information.si_code, unsafe {
                    information.si_status()
                })?);
                return Ok(self.wait_status);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn request_kill(&self) -> io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn unix_wait_status(code: i32, status: i32) -> io::Result<i32> {
    match code {
        libc::CLD_EXITED if (0..=255).contains(&status) => Ok(status << 8),
        libc::CLD_KILLED if (1..=64).contains(&status) => Ok(status),
        libc::CLD_DUMPED if (1..=64).contains(&status) => Ok(status | 0x80),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "original pidfd returned no recognized terminal root status",
        )),
    }
}

struct Reaper {
    sender: mpsc::SyncSender<Custody>,
}
static REAPER: OnceLock<Option<Reaper>> = OnceLock::new();

fn reaper() -> io::Result<&'static Reaper> {
    REAPER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<Custody>(MAX_ROOTS);
            let mut retained = Vec::<Custody>::with_capacity(MAX_ROOTS);
            std::thread::Builder::new()
                .name("contained-native-reaper".into())
                .spawn(move || loop {
                    match receiver.recv_timeout(Duration::from_millis(20)) {
                        Ok(mut custody) => {
                            let _ = custody.root.request_kill();
                            let _ = custody.domain.request_termination();
                            retained.push(custody);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    retained.retain_mut(|custody| {
                        if !matches!(custody.root.try_wait(), Ok(Some(_))) {
                            return true;
                        }
                        let Ok(Some(proof)) = custody.domain.try_extinction() else {
                            return true;
                        };
                        custody.domain.remove_extinct(proof).is_err()
                    });
                })
                .ok()
                .map(|_| Reaper { sender })
        })
        .as_ref()
        .ok_or_else(|| io::Error::other("contained reaper could not start"))
}

/// Holds the original domain and root until explicit closure or transfer to the
/// bounded reaper. Root wait never substitutes for descendant extinction.
#[must_use]
pub(super) struct OwnedContainedChild {
    custody: Option<Custody>,
    stdout: Option<File>,
    stderr: Option<File>,
    error_pipe: File,
    report: ExecReport,
    error_frame: [u8; 8],
    error_length: usize,
}

impl OwnedContainedChild {
    pub(super) fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.custody
            .as_mut()
            .map_or(Ok(None), |custody| custody.root.try_wait())
    }

    pub(super) fn request_kill(&mut self) -> io::Result<()> {
        let Some(custody) = self.custody.as_mut() else {
            return Ok(());
        };
        // Attempt both original kill capabilities even if one refuses.
        let domain = custody.domain.request_termination();
        let root = custody.root.request_kill();
        domain.and(root)
    }

    pub(super) fn poll_exec_report(&mut self) -> io::Result<ExecReport> {
        if self.report != ExecReport::Pending {
            return Ok(self.report);
        }
        loop {
            let result = unsafe {
                libc::read(
                    self.error_pipe.as_raw_fd(),
                    self.error_frame[self.error_length..].as_mut_ptr().cast(),
                    self.error_frame.len() - self.error_length,
                )
            };
            if result == 0 {
                self.report = if self.error_length == 0 {
                    ExecReport::ClosedWithoutReportedError
                } else {
                    ExecReport::InvalidProtocol
                };
                return Ok(self.report);
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => return Ok(ExecReport::Pending),
                    _ => return Err(error),
                }
            }
            self.error_length += result as usize;
            if self.error_length == self.error_frame.len() {
                let stage = u32::from_ne_bytes(
                    self.error_frame[..4].try_into().expect("fixed stage field"),
                );
                let errno = i32::from_ne_bytes(
                    self.error_frame[4..].try_into().expect("fixed errno field"),
                );
                self.report = ExecReport::Failed { stage, errno };
                return Ok(self.report);
            }
        }
    }

    /// These nonblocking readers remain explicit application-owned resources.
    pub(super) fn stdout(&mut self) -> Option<&mut File> {
        self.stdout.as_mut()
    }
    pub(super) fn stderr(&mut self) -> Option<&mut File> {
        self.stderr.as_mut()
    }
    pub(super) fn close_output_pipes(&mut self) {
        self.stdout.take();
        self.stderr.take();
    }

    pub(super) fn try_native_extinction(&mut self) -> io::Result<Option<NativeDomainExtinction>> {
        self.custody
            .as_mut()
            .map_or(Ok(None), |custody| custody.domain.try_extinction())
    }
}

impl Drop for OwnedContainedChild {
    fn drop(&mut self) {
        self.close_output_pipes();
        if let Some(mut custody) = self.custody.take() {
            let _ = custody.domain.request_termination();
            let _ = custody.root.request_kill();
            // Every active/queued/reaping owner holds one of MAX_ROOTS slots;
            // consequently this MAX_ROOTS queue cannot be full here.
            let Some(Some(reaper)) = REAPER.get() else {
                std::mem::forget(custody);
                return;
            };
            match reaper.sender.try_send(custody) {
                Ok(()) => {}
                Err(error) => {
                    let mut custody = match error {
                        mpsc::TrySendError::Full(custody)
                        | mpsc::TrySendError::Disconnected(custody) => custody,
                    };
                    // Retain the exact owner on impossible reaper failure;
                    // never turn missing cleanup into apparent closure.
                    let _ = custody.domain.request_termination();
                    let _ = custody.root.request_kill();
                    std::mem::forget(custody);
                }
            }
        }
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { File::from_raw_fd(descriptors[0]) };
    let write = unsafe { File::from_raw_fd(descriptors[1]) };
    let flags = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

/// Clone failures return the original domain. Successful clones always return
/// actual root custody, including setup/exec failure reported asynchronously.
#[allow(
    clippy::result_large_err,
    reason = "A failed launch returns its original inline native owner; error handling must not allocate another custody box."
)]
pub(super) fn launch(
    mut domain: OwnedNativeDomain,
    spec: SealedLaunchSpec,
) -> Result<OwnedContainedChild, LaunchFailure> {
    let prepared = (|| {
        spec.validate()?;
        let _ = reaper()?;
        let slot = Slot::acquire()?;
        let (stdout, output) = pipe()?;
        let (stderr, errors) = pipe()?;
        let (error_pipe, error_writer) = pipe()?;
        let child = linux::PreparedChild::new(&spec, output, errors, error_writer)?;
        Ok::<_, io::Error>((slot, stdout, stderr, error_pipe, child))
    })();
    let (slot, stdout, stderr, error_pipe, child) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(LaunchFailure {
                domain,
                reason: LaunchRefusal::Refused(error),
            });
        }
    };
    let seed = match domain.take_entry_seed() {
        Ok(seed) => seed,
        Err(error) => {
            return Err(LaunchFailure {
                domain,
                reason: LaunchRefusal::Refused(error),
            });
        }
    };
    let result = child.spawn(seed.directory_fd());
    drop(seed);
    domain.seal_entry();
    match result {
        Ok((pid, pidfd)) => Ok(OwnedContainedChild {
            custody: Some(Custody {
                domain,
                root: Root {
                    pid,
                    pidfd,
                    wait_status: None,
                },
                _slot: slot,
            }),
            stdout: Some(stdout),
            stderr: Some(stderr),
            error_pipe,
            report: ExecReport::Pending,
            error_frame: [0; 8],
            error_length: 0,
        }),
        Err(error) => {
            let reason = if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::EPERM | libc::EACCES | libc::EOPNOTSUPP)
            ) {
                LaunchRefusal::Unavailable(error)
            } else {
                LaunchRefusal::Refused(error)
            };
            Err(LaunchFailure { domain, reason })
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Kernel fixture assertions fail the test on invalid setup."
)]
mod tests {
    use super::*;
    use crate::process::containment::{NativeDomainLimits, NativeDomainRoot};
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::time::Instant;

    fn spec(arguments: &[&str], environment: &[&str]) -> SealedLaunchSpec {
        SealedLaunchSpec::test_executable(
            File::open(std::env::current_exe().unwrap()).unwrap(),
            File::open(std::env::current_exe().unwrap()).unwrap(),
            arguments
                .iter()
                .map(|value| CString::new(*value).unwrap())
                .collect(),
            environment
                .iter()
                .map(|value| CString::new(*value).unwrap())
                .collect(),
        )
        .unwrap()
    }

    fn launch_failure(failure: &LaunchFailure) -> String {
        let (class, error) = match &failure.reason {
            LaunchRefusal::Unavailable(error) => ("unavailable", error),
            LaunchRefusal::Refused(error) => ("refused", error),
        };
        format!(
            "{class}: {error}; retained inode {}",
            failure.domain.identity().inode
        )
    }

    #[test]
    fn sealed_launch_rejects_unbounded_argument_storage() {
        let mut launch = spec(&["contained-test"], &[]);
        launch
            .arguments
            .push(CString::new(vec![b'x'; MAX_STRING_BYTES]).unwrap());
        assert!(launch.validate().is_err());
    }

    #[test]
    fn sealed_launch_rejects_excess_argument_and_environment_capacity() {
        let mut launch = spec(&["contained-test"], &[]);
        launch.environment = Vec::with_capacity(MAX_STRINGS + 1);
        assert!(
            launch.validate().is_err(),
            "empty environment still owns its allocated vector storage"
        );
        launch.environment = Vec::new();
        launch.arguments.reserve(MAX_STRINGS + 1);
        assert!(
            launch.validate().is_err(),
            "one argument cannot hide excessive vector capacity"
        );
    }

    #[test]
    fn original_pidfd_terminal_status_preserves_unix_exit_signal_and_core_facts() {
        let exited = unix_wait_status(libc::CLD_EXITED, 23).unwrap();
        assert!(libc::WIFEXITED(exited));
        assert_eq!(libc::WEXITSTATUS(exited), 23);
        let killed = unix_wait_status(libc::CLD_KILLED, libc::SIGKILL).unwrap();
        assert!(libc::WIFSIGNALED(killed));
        assert_eq!(libc::WTERMSIG(killed), libc::SIGKILL);
        let dumped = unix_wait_status(libc::CLD_DUMPED, libc::SIGSEGV).unwrap();
        assert!(libc::WCOREDUMP(dumped));
        assert!(unix_wait_status(libc::CLD_STOPPED, libc::SIGSTOP).is_err());
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root; refusal occurs before any clone"]
    fn strict_preclone_refusal_preserves_original_unentered_domain() {
        let root =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let root = NativeDomainRoot::open(Path::new(&root)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(64 * 1024 * 1024, 8).unwrap())
            .unwrap();
        let identity = domain.identity();
        let mut spec = spec(&["contained-test"], &[]);
        spec.arguments.clear();
        let mut failure = match launch(domain, spec) {
            Err(failure) => failure,
            Ok(_) => panic!("invalid sealed arguments must refuse before clone"),
        };
        assert!(matches!(failure.reason, LaunchRefusal::Refused(_)));
        assert_eq!(failure.domain.identity(), identity);
        failure.domain.seal_entry();
        let proof = failure
            .domain
            .try_extinction()
            .unwrap()
            .expect("refused launch never entered original domain");
        failure.domain.remove_extinct(proof).unwrap();
    }

    #[test]
    #[ignore = "private child executable entry; invoked only by delegated cgroup test"]
    fn private_contained_child_entry() {
        assert_eq!(
            std::env::var("PLURX_PRIVATE_CONTAINED_TEST").as_deref(),
            Ok("atomic-entry-v1")
        );
        let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let membership = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .unwrap();
        let mount = std::env::var("PLURX_PRIVATE_CGROUP_MOUNT_ID").unwrap();
        let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        let mountpoint = mounts
            .lines()
            .find_map(|line| {
                let (before, after) = line.split_once(" - ")?;
                let fields: Vec<_> = before.split_whitespace().collect();
                (fields.first() == Some(&mount.as_str())
                    && fields.get(3) == Some(&"/")
                    && after.starts_with("cgroup2 "))
                .then(|| fields[4])
            })
            .unwrap();
        let directory = Path::new(mountpoint).join(membership.trim_start_matches('/'));
        let inode = std::fs::metadata(directory).unwrap().ino();
        assert_eq!(
            inode.to_string(),
            std::env::var("PLURX_PRIVATE_CGROUP_INODE").unwrap()
        );
        println!("contained-inode:{inode}");
        let mode = std::env::var("PLURX_PRIVATE_CHILD_MODE").unwrap_or_default();
        if mode == "thread-limit" {
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut threads = Vec::new();
            let mut refused = false;
            for _ in 0..64 {
                let worker_shutdown = std::sync::Arc::clone(&shutdown);
                match std::thread::Builder::new()
                    .stack_size(2 * 1024 * 1024)
                    .spawn(move || {
                        while !worker_shutdown.load(Ordering::Acquire) {
                            std::thread::park();
                        }
                    }) {
                    Ok(thread) => threads.push(thread),
                    Err(error) => {
                        assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
                        refused = true;
                        break;
                    }
                }
            }
            assert!(refused, "actual pids.max must refuse thread creation");
            shutdown.store(true, Ordering::Release);
            for thread in &threads {
                thread.thread().unpark();
            }
            for thread in threads {
                thread.join().unwrap();
            }
            println!("contained-thread-limit-enforced");
        } else if mode == "fork-limit" {
            let mut children = Vec::new();
            let mut refused = false;
            for _ in 0..64 {
                let pid = unsafe { libc::fork() };
                if pid == 0 {
                    loop {
                        unsafe {
                            libc::pause();
                        }
                    }
                }
                if pid < 0 {
                    assert_eq!(
                        io::Error::last_os_error().raw_os_error(),
                        Some(libc::EAGAIN)
                    );
                    refused = true;
                    break;
                }
                children.push(pid);
            }
            assert!(refused, "actual pids.max must refuse fork");
            for pid in children {
                assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
                let mut status = 0;
                loop {
                    let result = unsafe { libc::waitpid(pid, &mut status, 0) };
                    if result == pid {
                        break;
                    }
                    assert_eq!(
                        io::Error::last_os_error().kind(),
                        io::ErrorKind::Interrupted
                    );
                }
            }
            println!("contained-fork-limit-enforced");
        } else if mode == "memory-limit" {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"contained-allocation-started\n").unwrap();
            stdout.flush().unwrap();
            drop(stdout);
            let mut memory = Vec::<u8>::with_capacity(128 * 1024 * 1024);
            memory.resize(128 * 1024 * 1024, 0xa5);
            std::hint::black_box(&memory);
            panic!("actual memory.max must kill the private allocating root");
        } else if mode == "orphan" {
            let mut ready = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            let result = unsafe { libc::fork() };
            assert!(result >= 0, "private descendant creation required");
            if result == 0 {
                unsafe {
                    libc::close(ready[0]);
                    if libc::setsid() < 0 {
                        libc::_exit(126);
                    }
                    let byte = [1u8];
                    if libc::write(ready[1], byte.as_ptr().cast(), 1) != 1 {
                        libc::_exit(126);
                    }
                    libc::close(ready[1]);
                }
                loop {
                    unsafe {
                        libc::pause();
                    }
                }
            }
            unsafe {
                libc::close(ready[1]);
            }
            let mut byte = [0u8];
            loop {
                let read = unsafe { libc::read(ready[0], byte.as_mut_ptr().cast(), 1) };
                if read >= 0 {
                    assert_eq!(read, 1, "descendant must acknowledge successful setsid");
                    break;
                }
                assert_eq!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::Interrupted
                );
            }
            unsafe {
                libc::close(ready[0]);
            }
            assert_eq!(byte[0], 1);
            use std::io::Write;
            println!("contained-descendant-created");
            std::io::stdout().flush().unwrap();
            unsafe {
                libc::_exit(0);
            }
        }
    }

    #[test]
    #[ignore = "private sandbox child; invoked only by the capped delegated parent"]
    fn private_restricted_child_entry() {
        assert_eq!(
            std::env::var("PLURX_PRIVATE_SANDBOX_TEST").as_deref(),
            Ok("read-only-v1")
        );
        let denied = std::env::var("PLURX_PRIVATE_SANDBOX_FILE").unwrap();
        assert_eq!(
            File::open(&denied).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&denied)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let root = std::env::var("PLURX_TEST_CGROUP_ROOT").unwrap();
        assert_eq!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(Path::new(&root).join("cgroup.procs"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let path = CString::new(root).unwrap();
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        // O_PATH is deliberately not a filesystem read. The syscall policy
        // must deny clone3 even when an untrusted child acquires this fd.
        assert!(fd >= 0);
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut args = [0u64; 11];
        args[0] = 0x2_0000_0000; // CLONE_INTO_CGROUP
        args[4] = libc::SIGCHLD as u64;
        args[10] = fd.as_raw_fd() as u64;
        let result = unsafe {
            libc::syscall(
                libc::SYS_clone3,
                args.as_ptr(),
                std::mem::size_of_val(&args),
            )
        };
        if result == 0 {
            unsafe { libc::_exit(71) }
        }
        if result > 0 {
            unsafe {
                libc::waitpid(result as i32, std::ptr::null_mut(), 0);
            }
        }
        assert_eq!(result, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOSYS)
        );
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWUSER) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_pidfd_getfd, -1, 0, 0) },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_ptrace, u64::MAX, 0, 0, 0) },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_socket, libc::AF_UNIX, libc::SOCK_STREAM, 0) },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        let thread = std::thread::spawn(|| 42);
        assert_eq!(
            thread.join().unwrap(),
            42,
            "constrained legacy thread creation still works"
        );
        println!("restricted-file-controller-namespace-and-descriptor-access");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    #[ignore = "requires capped delegated cgroup2 root and Landlock ABI6; run explicitly with --ignored"]
    fn strict_sandbox_denies_controller_namespace_and_descriptor_escape() {
        let parent = std::env::var_os("PLURX_TEST_CGROUP_ROOT").unwrap();
        let root = NativeDomainRoot::open(Path::new(&parent)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(2 * 1024 * 1024 * 1024, 32).unwrap())
            .unwrap();
        let forbidden = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(forbidden.path(), b"original must remain untouched").unwrap();
        let parent_env = format!("PLURX_TEST_CGROUP_ROOT={}", Path::new(&parent).display());
        let denied_env = format!("PLURX_PRIVATE_SANDBOX_FILE={}", forbidden.path().display());
        let mut spec = spec(
            &[
                "sandbox-test",
                "--exact",
                "process::contained_launch::tests::private_restricted_child_entry",
                "--ignored",
                "--nocapture",
                "--color",
                "never",
            ],
            &[
                "PLURX_PRIVATE_SANDBOX_TEST=read-only-v1",
                "RUST_TEST_THREADS=1",
                &parent_env,
                &denied_env,
            ],
        );
        let interpreter = File::open("/lib64/ld-linux-x86-64.so.2").unwrap();
        let read_only = [
            "/lib",
            "/lib64",
            "/usr/lib",
            "/usr/lib64",
            "/etc/ld.so.cache",
        ]
        .into_iter()
        .filter_map(|name| File::open(name).ok())
        .collect::<Vec<_>>();
        spec.sandbox = Some(
            sandbox::PreparedSandbox::read_only(
                &spec.executable,
                &spec.input,
                &[interpreter],
                &read_only,
            )
            .unwrap(),
        );
        let mut child = launch(domain, spec).unwrap_or_else(|failure| {
            panic!(
                "actual restricted launch required: {}",
                launch_failure(&failure)
            )
        });
        let status = wait_private_root(&mut child);
        let mut output = String::new();
        child.stdout().unwrap().read_to_string(&mut output).unwrap();
        let mut errors = String::new();
        child.stderr().unwrap().read_to_string(&mut errors).unwrap();
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "restricted child: {status}; {output}; {errors}"
        );
        assert!(
            output.contains("restricted-file-controller-namespace-and-descriptor-access"),
            "{output}; {errors}"
        );
        assert_eq!(
            std::fs::read(forbidden.path()).unwrap(),
            b"original must remain untouched"
        );
        child.close_output_pipes();
        assert!(child.try_native_extinction().unwrap().is_some());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    #[ignore = "requires explicit capped delegated cgroup root, native FFmpeg and Landlock ABI6"]
    fn strict_restricted_ffmpeg_uses_original_fd_three_four_and_nine() {
        use std::io::Write;
        let parent = std::env::var_os("PLURX_TEST_CGROUP_ROOT").unwrap();
        let program = std::env::var_os("PLURX_TEST_FFMPEG").unwrap();
        let libraries = std::env::var_os("PLURX_TEST_FFMPEG_LIBDIR").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("original.y4m");
        let audio = dir.path().join("original.pcm");
        let mut file = File::create(&video).unwrap();
        file.write_all(b"YUV4MPEG2 W64 H64 F4:1 Ip A1:1 C420jpeg\n")
            .unwrap();
        for _ in 0..2 {
            file.write_all(b"FRAME\n").unwrap();
            file.write_all(&[128; 64 * 64 * 3 / 2]).unwrap();
        }
        drop(file);
        std::fs::write(&audio, [0; 8000]).unwrap();
        let inputs = vec![File::open(&video).unwrap(), File::open(&audio).unwrap()];
        let arguments = [
            "/proc/self/fd/9",
            "-nostdin",
            "-v",
            "error",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-i",
            "/dev/fd/3",
            "-f",
            "s16le",
            "-ar",
            "8000",
            "-ac",
            "1",
            "-i",
            "/dev/fd/4",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-c:a",
            "aac",
            "-shortest",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov",
            "pipe:1",
        ]
        .into_iter()
        .map(|x| CString::new(x).unwrap())
        .collect();
        let env = [
            format!("LD_LIBRARY_PATH={}", Path::new(&libraries).display()),
            "LANG=C".into(),
            "LC_ALL=C".into(),
            "AV_LOG_FORCE_NOCOLOR=1".into(),
        ]
        .into_iter()
        .map(|x| CString::new(x).unwrap())
        .collect();
        let mut spec = SealedLaunchSpec::test_finite_executable(
            File::open(program).unwrap(),
            inputs,
            arguments,
            env,
        )
        .unwrap();
        let interpreter = File::open("/lib64/ld-linux-x86-64.so.2").unwrap();
        let mut reads = [
            "/lib",
            "/lib64",
            "/usr/lib",
            "/usr/lib64",
            "/etc/ld.so.cache",
        ]
        .into_iter()
        .filter_map(|x| File::open(x).ok())
        .collect::<Vec<_>>();
        reads.push(File::open(&libraries).unwrap());
        reads.extend(spec.finite_inputs.iter().map(|f| f.try_clone().unwrap()));
        spec.sandbox = Some(
            sandbox::PreparedSandbox::read_only(
                &spec.executable,
                &spec.input,
                &[interpreter],
                &reads,
            )
            .unwrap(),
        );
        let root = NativeDomainRoot::open(Path::new(&parent)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(2 * 1024 * 1024 * 1024, 32).unwrap())
            .unwrap();
        let mut child = launch(domain, spec).unwrap_or_else(|failure| {
            panic!(
                "actual FFmpeg launch required: {}",
                launch_failure(&failure)
            )
        });
        let status = wait_private_root(&mut child);
        let mut output = Vec::new();
        child.stdout().unwrap().read_to_end(&mut output).unwrap();
        let mut errors = String::new();
        child.stderr().unwrap().read_to_string(&mut errors).unwrap();
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "FFmpeg status {status}: {errors}"
        );
        assert!(
            output.windows(4).any(|x| x == b"ftyp")
                && output.windows(4).any(|x| x == b"moof")
                && output.windows(4).any(|x| x == b"mdat"),
            "actual fragmented output required: {errors}"
        );
        child.close_output_pipes();
        assert!(child.try_native_extinction().unwrap().is_some());
    }

    #[test]
    fn finite_descriptor_profile_refuses_writable_original_input_before_entry() {
        let input = tempfile::NamedTempFile::new().unwrap();
        let writable = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(input.path())
            .unwrap();
        let result = SealedLaunchSpec::test_finite_executable(
            File::open(std::env::current_exe().unwrap()).unwrap(),
            vec![writable],
            vec![CString::new("fixed-finite").unwrap()],
            Vec::new(),
        );
        assert_eq!(
            result
                .err()
                .expect("inherited writable source cannot be sandboxed retroactively")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    fn delegated_child(mode: &str, memory: u64, tasks: u32) -> OwnedContainedChild {
        let root =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let root = NativeDomainRoot::open(Path::new(&root)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(memory, tasks).unwrap())
            .unwrap();
        let mount = format!("PLURX_PRIVATE_CGROUP_MOUNT_ID={}", domain.identity().mount);
        let inode = format!("PLURX_PRIVATE_CGROUP_INODE={}", domain.identity().inode);
        let mode = format!("PLURX_PRIVATE_CHILD_MODE={mode}");
        let spec = spec(
            &[
                "contained-test",
                "--exact",
                "process::contained_launch::tests::private_contained_child_entry",
                "--ignored",
                "--nocapture",
            ],
            &[
                "PLURX_PRIVATE_CONTAINED_TEST=atomic-entry-v1",
                "RUST_TEST_THREADS=1",
                &mount,
                &inode,
                &mode,
            ],
        );
        match launch(domain, spec) {
            Ok(child) => child,
            Err(failure) => panic!(
                "actual delegated launch required: {}",
                launch_failure(&failure)
            ),
        }
    }

    fn wait_private_root(child: &mut OwnedContainedChild) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "private root deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root and clone3; tests actual pthread admission refusal"]
    fn strict_native_pids_limit_refuses_descendant_threads() {
        let mut child = delegated_child("thread-limit", 64 * 1024 * 1024, 8);
        assert!(child.stderr().is_some(), "original stderr reader retained");
        assert_eq!(wait_private_root(&mut child), 0);
        let mut output = String::new();
        child.stdout().unwrap().read_to_string(&mut output).unwrap();
        assert!(
            output.contains("contained-thread-limit-enforced"),
            "private enforcement entry must run: {output}"
        );
        assert!(child.try_native_extinction().unwrap().is_some());
        assert!(
            child
                .custody
                .as_ref()
                .unwrap()
                .domain
                .observed_pids_limit_events()
                .unwrap()
                > 0
        );
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root and clone3; tests actual fork admission refusal"]
    fn strict_native_pids_limit_refuses_descendant_forks() {
        let mut child = delegated_child("fork-limit", 64 * 1024 * 1024, 8);
        assert_eq!(wait_private_root(&mut child), 0);
        let mut output = String::new();
        child.stdout().unwrap().read_to_string(&mut output).unwrap();
        assert!(
            output.contains("contained-fork-limit-enforced"),
            "private enforcement entry must run: {output}"
        );
        assert!(child.try_native_extinction().unwrap().is_some());
        assert!(
            child
                .custody
                .as_ref()
                .unwrap()
                .domain
                .observed_pids_limit_events()
                .unwrap()
                > 0
        );
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root and clone3; proves original owner reaches the bounded reaper"]
    fn strict_drop_retains_original_domain_until_descendants_close() {
        let mut child = delegated_child("orphan", 64 * 1024 * 1024, 16);
        let inode = child.custody.as_ref().unwrap().domain.identity().inode;
        let root = std::env::var_os("PLURX_TEST_CGROUP_ROOT").unwrap();
        let directory = std::fs::read_dir(root)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.metadata().unwrap().ino() == inode)
            .unwrap()
            .path();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = String::new();
        while !output.contains("contained-descendant-created") {
            match child.stdout().unwrap().read_to_string(&mut output) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("private output: {error}"),
            }
            assert!(Instant::now() < deadline, "descendant creation deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child.try_native_extinction().unwrap().is_none());
        drop(child);
        while directory.exists() {
            assert!(
                Instant::now() < deadline,
                "bounded reaper must remove original extinct domain"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root and clone3; deliberately executes a pinned non-executable file"]
    fn strict_exec_failure_retains_original_root_and_domain_custody() {
        let root =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let root = NativeDomainRoot::open(Path::new(&root)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(64 * 1024 * 1024, 8).unwrap())
            .unwrap();
        let inode = domain.identity().inode;
        let executable = File::open("/proc/version").unwrap();
        let input = File::open(std::env::current_exe().unwrap()).unwrap();
        let spec = SealedLaunchSpec::test_executable(
            executable,
            input,
            vec![CString::new("deliberate-exec-refusal").unwrap()],
            vec![],
        )
        .unwrap();
        let mut child = match launch(domain, spec) {
            Ok(child) => child,
            Err(failure) => panic!(
                "clone must occur before deliberate exec failure: {:?}",
                failure.reason
            ),
        };
        let status = wait_private_root(&mut child);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 127);
        assert!(matches!(
            child.poll_exec_report().unwrap(),
            ExecReport::Failed { stage: 6, .. }
        ));
        assert_eq!(
            child.custody.as_ref().unwrap().domain.identity().inode,
            inode
        );
        assert!(child.try_native_extinction().unwrap().is_some());
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root and clone3; intentionally causes a private domain OOM"]
    fn strict_native_memory_limit_kills_original_allocating_root() {
        let mut child = delegated_child("memory-limit", 64 * 1024 * 1024, 16);
        let status = wait_private_root(&mut child);
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
        let mut output = String::new();
        child.stdout().unwrap().read_to_string(&mut output).unwrap();
        assert!(
            output.contains("contained-allocation-started"),
            "the original allocating helper must run before OOM: {output}"
        );
        assert!(
            child
                .custody
                .as_ref()
                .unwrap()
                .domain
                .observed_oom_kill_events()
                .unwrap()
                > 0
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_native_extinction().unwrap().is_none() {
            assert!(Instant::now() < deadline, "private OOM extinction deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires explicit writable delegated cgroup2 root and clone3 support; run with PLURX_TEST_CGROUP_ROOT and --ignored"]
    fn strict_atomic_launch_is_born_inside_original_domain() {
        let root =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let root = NativeDomainRoot::open(Path::new(&root)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(64 * 1024 * 1024, 16).unwrap())
            .unwrap();
        let inode = domain.identity().inode;
        let mount = format!("PLURX_PRIVATE_CGROUP_MOUNT_ID={}", domain.identity().mount);
        let expected_inode = format!("PLURX_PRIVATE_CGROUP_INODE={inode}");
        let spec = spec(
            &[
                "contained-test",
                "--exact",
                "process::contained_launch::tests::private_contained_child_entry",
                "--ignored",
                "--nocapture",
            ],
            &[
                "PLURX_PRIVATE_CONTAINED_TEST=atomic-entry-v1",
                "RUST_TEST_THREADS=1",
                &mount,
                &expected_inode,
            ],
        );
        let mut child = match launch(domain, spec) {
            Ok(child) => child,
            Err(failure) => panic!(
                "actual delegated atomic launch required: {:?}; retained inode {}",
                failure.reason,
                failure.domain.identity().inode
            ),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        loop {
            if let Some(stdout) = child.stdout() {
                let mut bytes = [0; 4096];
                match stdout.read(&mut bytes) {
                    Ok(length) => output.extend_from_slice(&bytes[..length]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("output read: {error}"),
                }
            }
            if child.try_wait().unwrap().is_some() {
                child.stdout().unwrap().read_to_end(&mut output).unwrap();
                break;
            }
            assert!(Instant::now() < deadline, "private child deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(child.try_wait().unwrap(), Some(0));
        assert_eq!(
            child.poll_exec_report().unwrap(),
            ExecReport::ClosedWithoutReportedError
        );
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains(&format!("contained-inode:{inode}")),
            "private test entry must actually execute inside original domain: {output}"
        );
        // Child proves cgroup membership by opening its actual /proc row;
        // parent independently retains the original kernel domain identity.
        assert_eq!(
            child.custody.as_ref().unwrap().domain.identity().inode,
            inode
        );
        assert!(child.try_native_extinction().unwrap().is_some());
    }

    #[test]
    #[ignore = "requires explicit delegated cgroup2 root and clone3; no fixture files substitute for enforcement"]
    fn strict_root_exit_does_not_prove_descendant_extinction() {
        let root =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let root = NativeDomainRoot::open(Path::new(&root)).unwrap();
        let domain = root
            .create_domain(NativeDomainLimits::new(64 * 1024 * 1024, 16).unwrap())
            .unwrap();
        let mount = format!("PLURX_PRIVATE_CGROUP_MOUNT_ID={}", domain.identity().mount);
        let inode = format!("PLURX_PRIVATE_CGROUP_INODE={}", domain.identity().inode);
        let spec = spec(
            &[
                "contained-test",
                "--exact",
                "process::contained_launch::tests::private_contained_child_entry",
                "--ignored",
                "--nocapture",
            ],
            &[
                "PLURX_PRIVATE_CONTAINED_TEST=atomic-entry-v1",
                "RUST_TEST_THREADS=1",
                &mount,
                &inode,
                "PLURX_PRIVATE_CHILD_MODE=orphan",
            ],
        );
        let mut child = match launch(domain, spec) {
            Ok(child) => child,
            Err(failure) => panic!("actual delegated launch required: {:?}", failure.reason),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "private root deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(child.try_wait().unwrap(), Some(0));
        assert!(
            child.try_native_extinction().unwrap().is_none(),
            "root exit cannot close the still-live descendant"
        );
        child.request_kill().unwrap();
        while child.try_native_extinction().unwrap().is_none() {
            assert!(Instant::now() < deadline, "native extinction deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
