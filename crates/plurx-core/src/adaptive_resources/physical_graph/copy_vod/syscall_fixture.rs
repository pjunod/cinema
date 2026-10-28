//! Test-only exact-syscall denial on this thread. No descriptor is closed or
//! reaped outside its original owner. The enclosing test subprocess must exit.
use super::*;
use std::os::fd::AsRawFd;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

impl SealedPhysicalGraph {
    /// Install only after the original root wait. The descriptor number is
    /// read under original custody solely to select its actual close syscall;
    /// no raw descriptor or native capability escapes this helper.
    pub(in crate::adaptive_resources) async fn fixture_deny_original_unclaimed_stdout_close(
        &self,
        root: GraphRoot,
    ) -> io::Result<()> {
        if std::env::var_os("PLURX_COPY_STDOUT_EBADF_CHILD_ROOT").is_none() {
            return Err(io::Error::other(
                "irreversible filter requires isolated fixture child",
            ));
        }
        let mut members = self.custody.members.lock().await;
        if !members.writers.is_empty() || members.roots.len() != 1 || members.copy_hold.is_some() {
            return Err(io::Error::other(
                "expected original unclaimed root-only graph",
            ));
        }
        let original = root_member(&self.custody, &mut members, root)?;
        if !original.waited || original.exit_status.is_none() || original.pipes_claimed {
            return Err(io::Error::other(
                "same original root must already be waited",
            ));
        }
        let fd = original
            .child
            .stdout
            .as_ref()
            .ok_or_else(|| io::Error::other("original unclaimed stdout missing"))?
            .as_raw_fd();
        if fd < 0 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // seccomp_data: nr@0, arch@4, args[0]@16. close takes an int, so
        // matching its low word matches exactly the descriptor the kernel uses.
        // No TSYNC: existing journal/runtime worker threads stay unfiltered.
        let filter = [
            statement(0x20, 4),
            jump(0x15, AUDIT_ARCH, 1, 0),
            statement(0x06, 0x8000_0000), // SECCOMP_RET_KILL_PROCESS
            statement(0x20, 0),
            jump(0x15, libc::SYS_close as u32, 0, 3),
            statement(0x20, 16),
            jump(0x15, fd as u32, 0, 1),
            statement(0x06, 0x0005_0000 | libc::EBADF as u32),
            statement(0x06, 0x7fff_0000), // SECCOMP_RET_ALLOW
        ];
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len()).map_err(io::Error::other)?,
            filter: filter.as_ptr().cast_mut(),
        };
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0,
                &raw const program,
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(in crate::adaptive_resources) fn fixture_original_unclaimed_stdout_close_calls(
        &self,
    ) -> usize {
        super::super::finite_pipes::fixture_unclaimed_stdout_close_calls()
    }
}
