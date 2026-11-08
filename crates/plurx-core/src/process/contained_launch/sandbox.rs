//! Pre-exec restrictions for the contained-launch foundation.
//! This is not a production recipe constructor or a complete resource bound.
//! Filesystem rules and the architecture-checked syscall policy are both
//! mandatory: O_PATH acquisition alone is not restricted by Landlock.
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const MIN_ABI: libc::c_long = 6;
const FILTER_CAPACITY: usize = 256;
const MAX_RULES: usize = 256;
const EXECUTE: u64 = 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
// ABI5 filesystem rights, including REFER, TRUNCATE and device ioctl.
const HANDLED_FS: u64 = (1 << 16) - 1;

#[repr(C)]
struct RulesetAttributes {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}
#[repr(C, packed)]
struct PathRule {
    allowed_access: u64,
    parent_fd: i32,
}

pub(super) struct PreparedSandbox {
    ruleset: OwnedFd,
    filter: [libc::sock_filter; FILTER_CAPACITY],
    filter_len: u16,
}

impl PreparedSandbox {
    /// The profile accepts read-only object capabilities, not path strings or
    /// arbitrary callbacks. No write rule exists. This foundation remains
    /// inaccessible to production until its concrete recipe is audited.
    pub(super) fn read_only(
        executable: &File,
        input: &File,
        interpreters: &[File],
        read_only: &[File],
    ) -> io::Result<Self> {
        if !cfg!(target_arch = "x86_64") {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        // This first profile has no root-service credential transition. Do
        // not infer a safe root profile from no_new_privs alone.
        if unsafe { libc::geteuid() } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sandbox profile requires an unprivileged launcher",
            ));
        }
        if interpreters
            .len()
            .saturating_add(read_only.len())
            .saturating_add(2)
            > MAX_RULES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sandbox object count exceeds fixed limit",
            ));
        }
        let version = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<u8>(),
                0usize,
                1u32,
            )
        };
        if version < MIN_ABI {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "required filesystem and signal-scope enforcement unavailable",
            ));
        }
        let attributes = RulesetAttributes {
            handled_access_fs: HANDLED_FS,
            handled_access_net: 0,
            // ABI6: abstract Unix sockets and signals are scoped to descendants.
            scoped: 3,
        };
        let raw = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attributes,
                std::mem::size_of::<RulesetAttributes>(),
                0u32,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let ruleset = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
        let mut sandbox = Self {
            ruleset,
            filter: [libc::sock_filter {
                code: 0,
                jt: 0,
                jf: 0,
                k: 0,
            }; FILTER_CAPACITY],
            filter_len: 0,
        };
        sandbox.add(executable, true)?;
        sandbox.add(input, false)?;
        for file in interpreters {
            sandbox.add(file, true)?;
        }
        for file in read_only {
            sandbox.add(file, false)?;
        }
        sandbox.build_filter()?;
        Ok(sandbox)
    }
    fn add(&self, file: &File, execute: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_ACCMODE != libc::O_RDONLY || (execute && !metadata.is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sandbox grants require read-only objects and regular executables",
            ));
        }
        let rule = PathRule {
            allowed_access: READ_FILE
                | if metadata.is_dir() { READ_DIR } else { 0 }
                | if execute { EXECUTE } else { 0 },
            parent_fd: file.as_raw_fd(),
        };
        if unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.ruleset.as_raw_fd(),
                1u32,
                &rule,
                0u32,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(target_arch = "x86_64")]
    fn instruction(&mut self, code: u16, jt: u8, jf: u8, k: u32) -> io::Result<()> {
        let index = usize::from(self.filter_len);
        if index == FILTER_CAPACITY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sandbox filter exceeds fixed limit",
            ));
        }
        self.filter[index] = libc::sock_filter { code, jt, jf, k };
        self.filter_len += 1;
        Ok(())
    }
    #[cfg(target_arch = "x86_64")]
    fn finish_branch(&mut self, branch: usize) -> io::Result<()> {
        self.filter[branch].jf =
            u8::try_from(usize::from(self.filter_len) - branch - 1).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "sandbox branch exceeds fixed limit",
                )
            })?;
        Ok(())
    }
    #[cfg(not(target_arch = "x86_64"))]
    fn build_filter(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    #[cfg(target_arch = "x86_64")]
    fn build_filter(&mut self) -> io::Result<()> {
        const LOAD: u16 = 0x20;
        const EQ: u16 = 0x15;
        const AND: u16 = 0x54;
        const RET: u16 = 0x06;
        const ALLOW: u32 = 0x7fff0000;
        const DENY: u32 = 0x00050000 | libc::EPERM as u32;
        self.instruction(LOAD, 0, 0, 4)?; // seccomp_data.arch
        self.instruction(EQ, 1, 0, 0xc000003e)?; // AUDIT_ARCH_X86_64
        self.instruction(RET, 0, 0, 0x80000000)?; // KILL_PROCESS on other ABIs
        self.instruction(LOAD, 0, 0, 0)?; // syscall number
                                          // Classic BPF cannot inspect clone3's pointer payload. Returning
                                          // ENOSYS lets libc use the separately constrained legacy thread path.
        self.instruction(EQ, 0, 1, libc::SYS_clone3 as u32)?;
        self.instruction(RET, 0, 0, 0x00050000 | libc::ENOSYS as u32)?;
        let clone_branch = usize::from(self.filter_len);
        self.instruction(EQ, 0, 0, libc::SYS_clone as u32)?;
        self.instruction(LOAD, 0, 0, 20)?; // high half of flags must be zero
        self.instruction(EQ, 1, 0, 0)?;
        self.instruction(RET, 0, 0, DENY)?;
        self.instruction(LOAD, 0, 0, 16)?;
        self.instruction(AND, 0, 0, 0x7e028080)?; // namespaces and CLONE_PARENT
        self.instruction(EQ, 1, 0, 0)?;
        self.instruction(RET, 0, 0, DENY)?;
        self.instruction(RET, 0, 0, ALLOW)?;
        self.finish_branch(clone_branch)?;
        // Query-only prlimit64: no limit mutation or other-process operation.
        let limit_branch = usize::from(self.filter_len);
        self.instruction(EQ, 0, 0, libc::SYS_prlimit64 as u32)?;
        self.instruction(LOAD, 0, 0, 16)?; // pid == 0, including high bits
        self.instruction(EQ, 1, 0, 0)?;
        self.instruction(RET, 0, 0, DENY)?;
        self.instruction(LOAD, 0, 0, 20)?;
        self.instruction(EQ, 1, 0, 0)?;
        self.instruction(RET, 0, 0, DENY)?;
        self.instruction(LOAD, 0, 0, 32)?; // new_limit == NULL, including high bits
        self.instruction(EQ, 1, 0, 0)?;
        self.instruction(RET, 0, 0, DENY)?;
        self.instruction(LOAD, 0, 0, 36)?;
        self.instruction(EQ, 1, 0, 0)?;
        self.instruction(RET, 0, 0, DENY)?;
        self.instruction(RET, 0, 0, ALLOW)?;
        self.finish_branch(limit_branch)?;
        // Only harmless/name prctl operations. In particular there is no
        // ptracer, capability, seccomp-notification or privilege escape API.
        self.instruction(LOAD, 0, 0, 0)?;
        let prctl_branch = usize::from(self.filter_len);
        self.instruction(EQ, 0, 0, libc::SYS_prctl as u32)?;
        self.instruction(LOAD, 0, 0, 16)?;
        for option in [
            libc::PR_SET_NAME,
            libc::PR_GET_NAME,
            libc::PR_GET_NO_NEW_PRIVS,
            libc::PR_GET_SECCOMP,
            libc::PR_GET_DUMPABLE,
        ] {
            self.instruction(EQ, 0, 1, option as u32)?;
            self.instruction(RET, 0, 0, ALLOW)?;
        }
        self.instruction(RET, 0, 0, DENY)?;
        self.finish_branch(prctl_branch)?;
        self.instruction(LOAD, 0, 0, 0)?;
        for number in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_readv,
            libc::SYS_writev,
            libc::SYS_pread64,
            libc::SYS_close,
            libc::SYS_lseek,
            libc::SYS_fstat,
            libc::SYS_newfstatat,
            libc::SYS_statx,
            libc::SYS_access,
            libc::SYS_faccessat,
            libc::SYS_faccessat2,
            libc::SYS_readlink,
            libc::SYS_readlinkat,
            libc::SYS_getdents64,
            libc::SYS_open,
            libc::SYS_openat,
            libc::SYS_openat2,
            libc::SYS_fcntl,
            libc::SYS_dup,
            libc::SYS_dup2,
            libc::SYS_dup3,
            libc::SYS_pipe,
            libc::SYS_pipe2,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mprotect,
            libc::SYS_madvise,
            libc::SYS_mremap,
            libc::SYS_brk,
            libc::SYS_clock_gettime,
            libc::SYS_gettimeofday,
            libc::SYS_time,
            libc::SYS_nanosleep,
            libc::SYS_clock_nanosleep,
            libc::SYS_sched_yield,
            libc::SYS_futex,
            libc::SYS_futex_waitv,
            libc::SYS_sched_getaffinity,
            libc::SYS_getcpu,
            libc::SYS_rseq,
            libc::SYS_membarrier,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigreturn,
            libc::SYS_sigaltstack,
            libc::SYS_tgkill,
            libc::SYS_kill,
            libc::SYS_getpid,
            libc::SYS_gettid,
            libc::SYS_getppid,
            libc::SYS_getuid,
            libc::SYS_geteuid,
            libc::SYS_getgid,
            libc::SYS_getegid,
            libc::SYS_getresuid,
            libc::SYS_getresgid,
            libc::SYS_getgroups,
            libc::SYS_uname,
            libc::SYS_sysinfo,
            libc::SYS_getrandom,
            libc::SYS_getrlimit,
            libc::SYS_arch_prctl,
            libc::SYS_set_tid_address,
            libc::SYS_set_robust_list,
            libc::SYS_getcwd,
            libc::SYS_fstatfs,
            libc::SYS_statfs,
            libc::SYS_getrusage,
            libc::SYS_times,
            libc::SYS_poll,
            libc::SYS_ppoll,
            libc::SYS_select,
            libc::SYS_pselect6,
            libc::SYS_restart_syscall,
            libc::SYS_capget,
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_execveat,
        ] {
            self.instruction(EQ, 0, 1, number as u32)?;
            self.instruction(RET, 0, 0, ALLOW)?;
        }
        self.instruction(RET, 0, 0, DENY)
    }
    /// Fixed pre-exec syscall sequence; no allocation, callback or unwinding.
    /// The caller has already set no_new_privs and retains its error writer.
    pub(super) unsafe fn apply(&self) -> bool {
        #[repr(C)]
        struct CapabilityHeader {
            version: u32,
            pid: i32,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct CapabilityData {
            effective: u32,
            permitted: u32,
            inheritable: u32,
        }
        let header = CapabilityHeader {
            version: 0x20080522,
            pid: 0,
        };
        let data = [CapabilityData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }; 2];
        let program = libc::sock_fprog {
            len: self.filter_len,
            filter: self.filter.as_ptr().cast_mut(),
        };
        unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL,
                0,
                0,
                0,
            ) == 0
                && libc::syscall(libc::SYS_capset, &header, data.as_ptr()) == 0
                && libc::syscall(
                    libc::SYS_landlock_restrict_self,
                    self.ruleset.as_raw_fd(),
                    0u32,
                ) == 0
                && libc::prctl(libc::PR_SET_SECCOMP, 2, &program) == 0
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    // Interpret the actual emitted classic-BPF program, independently of the
    // builder's branches. The delegated test separately exercises the kernel.
    fn evaluate(program: &PreparedSandbox, arch: u32, number: u32, args: [u64; 6]) -> u32 {
        let mut bytes = [0u8; 64];
        bytes[..4].copy_from_slice(&number.to_ne_bytes());
        bytes[4..8].copy_from_slice(&arch.to_ne_bytes());
        for (index, arg) in args.iter().enumerate() {
            bytes[16 + index * 8..24 + index * 8].copy_from_slice(&arg.to_ne_bytes());
        }
        let mut accumulator = 0u32;
        let mut pc = 0usize;
        for _ in 0..FILTER_CAPACITY {
            assert!(
                pc < usize::from(program.filter_len),
                "BPF branch left program"
            );
            let op = program.filter[pc];
            pc += 1;
            match op.code {
                0x20 => {
                    let offset = op.k as usize;
                    accumulator =
                        u32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("BPF word"));
                }
                0x15 => pc += usize::from(if accumulator == op.k { op.jt } else { op.jf }),
                0x54 => accumulator &= op.k,
                0x06 => return op.k,
                other => panic!("unsupported emitted BPF instruction {other}"),
            }
        }
        panic!("BPF program did not terminate");
    }

    #[test]
    fn emitted_filter_denies_escape_and_mutation_on_every_conditional_path() {
        let mut policy = PreparedSandbox {
            ruleset: File::open("/dev/null")
                .expect("inert policy fixture")
                .into(),
            filter: [libc::sock_filter {
                code: 0,
                jt: 0,
                jf: 0,
                k: 0,
            }; FILTER_CAPACITY],
            filter_len: 0,
        };
        policy.build_filter().expect("bounded filter");
        const ARCH: u32 = 0xc000003e;
        const ALLOW: u32 = 0x7fff0000;
        const DENY: u32 = 0x00050000 | libc::EPERM as u32;
        for number in [libc::SYS_read, libc::SYS_write, libc::SYS_execveat] {
            assert_eq!(evaluate(&policy, ARCH, number as u32, [0; 6]), ALLOW);
        }
        for number in [
            libc::SYS_unshare,
            libc::SYS_setns,
            libc::SYS_pidfd_getfd,
            libc::SYS_ptrace,
            libc::SYS_socket,
            libc::SYS_io_uring_setup,
            libc::SYS_capset,
            libc::SYS_open_by_handle_at,
        ] {
            assert_eq!(evaluate(&policy, ARCH, number as u32, [0; 6]), DENY);
        }
        assert_eq!(
            evaluate(&policy, ARCH, libc::SYS_clone3 as u32, [0; 6]),
            0x00050000 | libc::ENOSYS as u32
        );
        for flags in [
            libc::CLONE_NEWUSER as u64,
            libc::CLONE_NEWCGROUP as u64,
            libc::CLONE_NEWNS as u64,
            libc::CLONE_PARENT as u64,
            1u64 << 33,
        ] {
            let mut args = [0; 6];
            args[0] = flags;
            assert_eq!(evaluate(&policy, ARCH, libc::SYS_clone as u32, args), DENY);
        }
        let mut args = [0; 6];
        args[0] = (libc::CLONE_VM
            | libc::CLONE_THREAD
            | libc::CLONE_SIGHAND
            | libc::CLONE_FILES
            | libc::CLONE_FS
            | libc::CLONE_SETTLS
            | libc::CLONE_PARENT_SETTID
            | libc::CLONE_CHILD_CLEARTID) as u64;
        assert_eq!(evaluate(&policy, ARCH, libc::SYS_clone as u32, args), ALLOW);
        assert_eq!(
            evaluate(&policy, ARCH, libc::SYS_prlimit64 as u32, [0; 6]),
            ALLOW
        );
        for (index, value) in [(0, 1), (0, 1 << 32), (2, 1), (2, 1 << 32)] {
            let mut args = [0; 6];
            args[index] = value;
            assert_eq!(
                evaluate(&policy, ARCH, libc::SYS_prlimit64 as u32, args),
                DENY
            );
        }
        let mut args = [0; 6];
        args[0] = libc::PR_SET_NAME as u64;
        assert_eq!(evaluate(&policy, ARCH, libc::SYS_prctl as u32, args), ALLOW);
        args[0] = libc::PR_SET_PTRACER as u64;
        assert_eq!(evaluate(&policy, ARCH, libc::SYS_prctl as u32, args), DENY);
        assert_eq!(evaluate(&policy, ARCH, 0xffff, [0; 6]), DENY);
        assert_eq!(
            evaluate(&policy, ARCH, 0x40000000 | libc::SYS_read as u32, [0; 6]),
            DENY
        );
        assert_eq!(
            evaluate(&policy, 0x40000003, libc::SYS_read as u32, [0; 6]),
            0x80000000
        );
    }
}
