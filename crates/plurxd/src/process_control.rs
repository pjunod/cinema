//! Cross-platform control of an owned, unreaped child process.

use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessSignal {
    Suspend,
    Resume,
    Terminate,
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

#[cfg(windows)]
pub(crate) fn signal(pid: u32, signal: ProcessSignal) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, STATUS_INVALID_HANDLE};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
    };

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
        Err(io::Error::from_raw_os_error(status))
    }
}
