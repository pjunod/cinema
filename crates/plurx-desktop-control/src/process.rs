//! Kernel identity: never infer peer authority from JSON or a loopback listener.
use crate::{protocol::ErrorCode, Error, Result};
use std::{
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelProcess {
    pub pid: u32,
    pub uid: u32,
    pub generation: String,
}

#[cfg(target_os = "linux")]
pub fn inspect(pid: u32) -> Result<KernelProcess> {
    use std::os::unix::fs::MetadataExt;
    if pid == 0 {
        return Err(Error::new(ErrorCode::IdentityMismatch, "zero service PID"));
    }
    let path = format!("/proc/{pid}");
    let m = std::fs::metadata(&path)?;
    let stat = std::fs::read_to_string(format!("{path}/stat"))?;
    // comm may contain spaces and ')'; split after the LAST ')' (field 2).
    let (_, rest) = stat
        .rsplit_once(')')
        .ok_or_else(|| Error::new(ErrorCode::IdentityMismatch, "malformed process stat"))?;
    let generation = rest
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| Error::new(ErrorCode::IdentityMismatch, "missing process starttime"))?;
    if !generation.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "invalid process starttime",
        ));
    }
    Ok(KernelProcess {
        pid,
        uid: m.uid(),
        generation: generation.to_string(),
    })
}
#[cfg(target_os = "macos")]
pub fn inspect(pid: u32) -> Result<KernelProcess> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "invalid service PID",
        ));
    }
    // SAFETY: POD kernel result buffer, size passed to libproc.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: valid caller-owned buffer and nonzero positive PID.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if n != size {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "cannot inspect exact service generation",
        ));
    }
    Ok(KernelProcess {
        pid,
        uid: info.pbi_uid,
        generation: format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    })
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn inspect(_pid: u32) -> Result<KernelProcess> {
    Err(Error::new(
        ErrorCode::Unsupported,
        "native control requires macOS or Linux",
    ))
}

#[cfg(target_os = "linux")]
pub fn socket_peer(stream: &UnixStream) -> Result<KernelProcess> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    // SAFETY: owned socket and properly sized output.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of_val(&cred)
        || cred.pid <= 0
    {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "kernel peer credentials unavailable",
        ));
    }
    let p = inspect(cred.pid as u32)?;
    if p.uid != cred.uid {
        return Err(Error::new(ErrorCode::IdentityMismatch, "peer UID changed"));
    }
    Ok(p)
}
#[cfg(target_os = "macos")]
pub fn socket_peer(stream: &UnixStream) -> Result<KernelProcess> {
    let mut uid = 0;
    let mut gid = 0;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    // SAFETY: held socket, valid output pointers.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
        || unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut len,
            )
        } != 0
        || pid <= 0
    {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "kernel peer identity unavailable",
        ));
    }
    let p = inspect(pid as u32)?;
    if p.uid != uid {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "kernel/process UID mismatch",
        ));
    }
    Ok(p)
}
pub fn still_same(expected: &KernelProcess) -> Result<bool> {
    match inspect(expected.pid) {
        Ok(p) => Ok(p == *expected),
        Err(e)
            if e.code == ErrorCode::Io
                && !std::path::Path::new(&format!("/proc/{}", expected.pid)).exists()
                && cfg!(target_os = "linux") =>
        {
            Ok(false)
        }
        #[cfg(target_os = "macos")]
        Err(_) => {
            // ESRCH only; an EPERM/process-inspection denial is NOT exit proof.
            let r = unsafe { libc::kill(expected.pid as i32, 0) };
            if r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(Error::new(
                    ErrorCode::IdentityMismatch,
                    "process inspection unavailable",
                ))
            }
        }
        #[cfg(not(target_os = "macos"))]
        Err(e) => Err(e),
    }
}
pub fn wait_gone(expected: &KernelProcess, timeout: Duration) -> Result<()> {
    wait_gone_with(expected, timeout, still_same)
}
fn wait_gone_with(
    expected: &KernelProcess,
    timeout: Duration,
    mut inspect: impl FnMut(&KernelProcess) -> Result<bool>,
) -> Result<()> {
    let until = Instant::now() + timeout;
    loop {
        match inspect(expected) {
            Ok(false) => return Ok(()),
            Ok(true) => {}
            Err(error) if error.code == ErrorCode::IdentityMismatch => {
                // A terminating macOS process can temporarily reject proc_pidinfo
                // before the kernel reports ESRCH. Unknown is not exit proof:
                // keep waiting, bounded by the same shutdown deadline.
                if Instant::now() >= until {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
        if Instant::now() >= until {
            return Err(Error::new(
                ErrorCode::UnsafeShutdown,
                "daemon did not cleanly exit in time; no force-stop was attempted",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod exit_wait_tests {
    use super::*;

    fn expected() -> KernelProcess {
        KernelProcess {
            pid: 123,
            uid: 501,
            generation: "bound-generation".into(),
        }
    }

    #[test]
    fn wait_gone_retries_unavailable_inspection_without_treating_it_as_exit() {
        let mut calls = 0;
        wait_gone_with(&expected(), Duration::from_secs(1), |_| {
            calls += 1;
            match calls {
                1 => Err(Error::new(
                    ErrorCode::IdentityMismatch,
                    "process inspection unavailable",
                )),
                2 => Ok(true),
                _ => Ok(false),
            }
        })
        .expect("only confirmed exit completes the wait");
        assert_eq!(calls, 3);
    }

    #[test]
    fn wait_gone_never_accepts_unknown_or_live_process_at_deadline() {
        let error = wait_gone_with(&expected(), Duration::ZERO, |_| {
            Err(Error::new(ErrorCode::IdentityMismatch, "still unknown"))
        })
        .expect_err("unknown is not exit");
        assert_eq!(error.code, ErrorCode::IdentityMismatch);
        assert!(wait_gone_with(&expected(), Duration::ZERO, |_| Ok(true)).is_err());
        wait_gone_with(&expected(), Duration::ZERO, |_| Ok(false)).expect("confirmed absent");
    }
}
