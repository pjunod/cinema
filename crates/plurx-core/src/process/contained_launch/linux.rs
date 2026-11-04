//! The child branch invokes only async-signal-safe libc syscall wrappers and
//! never allocates, unwinds, invokes callbacks, or drops inherited Rust owners.

use super::SealedLaunchSpec;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

const CLONE_PIDFD: u64 = 0x0000_1000;
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;
const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

// Audited 64-bit Linux kernel ABI; libc's public sigaction layout differs.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[repr(C)]
struct KernelSignalAction {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

fn high_copy(file: &File) -> io::Result<OwnedFd> {
    let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
}

pub(super) struct PreparedChild {
    executable: OwnedFd,
    input: OwnedFd,
    output: OwnedFd,
    errors: OwnedFd,
    error_writer: OwnedFd,
    arguments: Vec<*const libc::c_char>,
    environment: Vec<*const libc::c_char>,
}

impl PreparedChild {
    pub(super) fn new(
        spec: &SealedLaunchSpec,
        output: File,
        errors: File,
        error_writer: File,
    ) -> io::Result<Self> {
        let mut arguments: Vec<_> = spec.arguments.iter().map(|value| value.as_ptr()).collect();
        arguments.push(std::ptr::null());
        let mut environment: Vec<_> = spec
            .environment
            .iter()
            .map(|value| value.as_ptr())
            .collect();
        environment.push(std::ptr::null());
        Ok(Self {
            executable: high_copy(&spec.executable)?,
            input: high_copy(&spec.input)?,
            output: high_copy(&output)?,
            errors: high_copy(&errors)?,
            error_writer: high_copy(&error_writer)?,
            arguments,
            environment,
        })
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    pub(super) fn spawn(&self, _domain: BorrowedFd<'_>) -> io::Result<(libc::pid_t, OwnedFd)> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    pub(super) fn spawn(&self, domain: BorrowedFd<'_>) -> io::Result<(libc::pid_t, OwnedFd)> {
        let mut child_action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut child_action) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if child_action.sa_sigaction == libc::SIG_IGN
            || child_action.sa_flags & libc::SA_NOCLDWAIT != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SIGCHLD policy would discard original root wait custody",
            ));
        }
        // Raw mask includes glibc-reserved cancellation signals 32/33. No
        // libc atfork callbacks or inherited signal handlers may run here.
        let blocked = u64::MAX;
        let mut previous = 0u64;
        if unsafe {
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_SETMASK,
                &blocked,
                &mut previous,
                8usize,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut pidfd: libc::c_int = -1;
        let args = CloneArgs {
            flags: CLONE_PIDFD | CLONE_INTO_CGROUP,
            pidfd: (&mut pidfd as *mut libc::c_int) as u64,
            exit_signal: libc::SIGCHLD as u64,
            cgroup: domain.as_raw_fd() as u64,
            ..CloneArgs::default()
        };
        let result =
            unsafe { libc::syscall(libc::SYS_clone3, &args, std::mem::size_of::<CloneArgs>()) };
        if result == 0 {
            unsafe { self.child(&previous) }
        }
        let clone_error = if result < 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        // An unmask failure cannot discard actual child custody. Restoring the
        // old valid mask cannot normally fail; successful clone always returns
        // its original root/pidfd to the owner.
        unsafe {
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_SETMASK,
                &previous,
                std::ptr::null_mut::<u64>(),
                8usize,
            );
        }
        if let Some(error) = clone_error {
            return Err(error);
        }
        Ok((result as libc::pid_t, unsafe {
            OwnedFd::from_raw_fd(pidfd)
        }))
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    unsafe fn child(&self, previous: &u64) -> ! {
        let error_fd = self.error_writer.as_raw_fd();
        // Reset inherited handlers before unblocking; no parent Rust signal
        // handler may execute in this multithreaded-fork child.
        let action = KernelSignalAction {
            handler: 0,
            flags: 0,
            restorer: 0,
            mask: 0,
        };
        for signal in 1..=64 {
            if signal != libc::SIGKILL
                && signal != libc::SIGSTOP
                && unsafe {
                    libc::syscall(
                        libc::SYS_rt_sigaction,
                        signal,
                        &action,
                        std::ptr::null_mut::<KernelSignalAction>(),
                        8usize,
                    )
                } != 0
            {
                unsafe { fail(error_fd, 1) }
            }
        }
        if unsafe {
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_SETMASK,
                previous,
                std::ptr::null_mut::<u64>(),
                8usize,
            )
        } != 0
        {
            unsafe { fail(error_fd, 2) }
        }
        if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) } != 0
        {
            unsafe { fail(error_fd, 3) }
        }
        for (source, target) in [
            (self.input.as_raw_fd(), 0),
            (self.output.as_raw_fd(), 1),
            (self.errors.as_raw_fd(), 2),
        ] {
            if unsafe { libc::dup3(source, target, 0) } < 0 {
                unsafe { fail(error_fd, 4) }
            }
        }
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            unsafe { fail(error_fd, 5) }
        }
        unsafe {
            libc::syscall(
                libc::SYS_execveat,
                self.executable.as_raw_fd(),
                c"".as_ptr(),
                self.arguments.as_ptr(),
                self.environment.as_ptr(),
                libc::AT_EMPTY_PATH,
            );
            fail(error_fd, 6)
        }
    }
}

unsafe fn fail(error_fd: RawFd, stage: u32) -> ! {
    let errno = unsafe { *libc::__errno_location() };
    let mut frame = [0u8; 8];
    let stage_bytes = stage.to_ne_bytes();
    let errno_bytes = errno.to_ne_bytes();
    // Fixed indexes, no slice copying/assertion paths in the child.
    frame[0] = stage_bytes[0];
    frame[1] = stage_bytes[1];
    frame[2] = stage_bytes[2];
    frame[3] = stage_bytes[3];
    frame[4] = errno_bytes[0];
    frame[5] = errno_bytes[1];
    frame[6] = errno_bytes[2];
    frame[7] = errno_bytes[3];
    loop {
        let result = unsafe { libc::write(error_fd, frame.as_ptr().cast(), frame.len()) };
        if result >= 0 || unsafe { *libc::__errno_location() } != libc::EINTR {
            break;
        }
    }
    unsafe { libc::_exit(127) }
}
