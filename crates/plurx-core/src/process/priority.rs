//! Child priority classes: every child the daemon starts says whether
//! somebody is waiting on it, runs below the daemon accordingly, and can be
//! seen and stopped from inside the product while it runs.
//!
//! Plan P-02 §3.2 (`docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md`).
//! There are two classes, and [`super::spawn_job_owned`] takes one on every
//! call, so a spawn cannot reach a process without having said which:
//!
//! | class | who waits | nice | I/O | `oom_score_adj` |
//! |---|---|---:|---|---:|
//! | [`ChildClass::Realtime`] | a viewer, or a recording that cannot fall behind the wall clock | 5 | best-effort 4 | 500 |
//! | [`ChildClass::Background`] | nobody: scan thumbnails, covers, capability probes, fragment indexing, subtitle extraction, cache producers, conversions | 15 | best-effort 7 | 800 |
//!
//! Both sit below the daemon (nice 0, `oom_score_adj` 0 as observed on the
//! fleet), so the scheduler serves the process that answers every viewer
//! first, and the OOM killer prefers a background child, then a playback
//! child, and the daemon last. The values are the plan's proposals; §3.2's
//! realtime measurement on a busy host is what may move them.
//!
//! The priority is applied by the child to itself between `fork` and `exec`,
//! because Linux keeps `nice` per thread: a parent that lowered the child's
//! pid after the fact would miss every worker thread ffmpeg had already
//! started. Everything that runs there is a raw syscall on a value computed
//! before the fork, and every failure is ignored: a priority that cannot be
//! applied must never keep a transcode from starting. A class never raises a
//! child's CPU priority above what it would have inherited.
//!
//! Only Unix applies a policy (Linux all three parts, other Unix `nice`
//! only). Windows children are registered and listed with no policy; the Job
//! Object contract there is unchanged.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Who is waiting on a child. See the module table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildClass {
    /// A viewer, or a recording, is waiting on this child's output now.
    Realtime,
    /// Nobody is waiting on this child; it yields CPU and disk to playback.
    Background,
}

impl ChildClass {
    pub const ALL: [ChildClass; 2] = [ChildClass::Realtime, ChildClass::Background];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::Background => "background",
        }
    }

    /// Why a child of this class runs at its priority, in the words the
    /// Activity page shows beside it.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Realtime => "a viewer or a recording is waiting on it",
            Self::Background => "nobody is waiting on it, so it yields to playback",
        }
    }

    pub const fn policy(self) -> ChildPolicy {
        match self {
            Self::Realtime => ChildPolicy {
                nice: 5,
                io_level: 4,
                oom_score_adj: 500,
            },
            Self::Background => ChildPolicy {
                nice: 15,
                io_level: 7,
                oom_score_adj: 800,
            },
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Realtime => 0,
            Self::Background => 1,
        }
    }
}

/// What a class asks of the kernel. I/O is always the best-effort class at
/// `io_level` (0 highest, 7 lowest); `nice` is a floor, never a raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ChildPolicy {
    pub nice: i32,
    pub io_level: u8,
    pub oom_score_adj: i32,
}

/// One spawn's class and the few words that say what the child is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildWork {
    pub class: ChildClass,
    pub purpose: &'static str,
}

impl ChildWork {
    pub const fn realtime(purpose: &'static str) -> Self {
        Self {
            class: ChildClass::Realtime,
            purpose,
        }
    }

    pub const fn background(purpose: &'static str) -> Self {
        Self {
            class: ChildClass::Background,
            purpose,
        }
    }
}

/// Register the class's priority on `command` so the child applies it to
/// itself before `exec`.
///
/// [`super::spawn_job_owned`] calls this for every spawn. A caller that
/// registers its own `pre_exec` which does not return (the decode-fact
/// probe `execveat`s from inside one) must call this first: `pre_exec`
/// closures run in registration order, and one registered after a closure
/// that execs never runs. Registering twice is harmless.
pub fn apply(command: &mut tokio::process::Command, class: ChildClass) {
    apply_policy(command.as_std_mut(), class.policy());
}

pub(crate) fn apply_policy(command: &mut std::process::Command, policy: ChildPolicy) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let nice = policy.nice;
        #[cfg(target_os = "linux")]
        let ioprio = linux::ioprio_value(policy.io_level);
        #[cfg(target_os = "linux")]
        let oom = linux::OomBytes::new(policy.oom_score_adj);
        // SAFETY: the closure makes raw syscalls on values computed above,
        // before the fork: no allocation, no lock, no logging. Each result is
        // ignored so a failure cannot turn into a failed spawn.
        unsafe {
            command.pre_exec(move || {
                let current = libc::getpriority(libc::PRIO_PROCESS, 0);
                if current < nice {
                    libc::setpriority(libc::PRIO_PROCESS, 0, nice);
                }
                #[cfg(target_os = "linux")]
                {
                    libc::syscall(libc::SYS_ioprio_set, linux::IOPRIO_WHO_PROCESS, 0, ioprio);
                    let fd = libc::open(
                        c"/proc/self/oom_score_adj".as_ptr(),
                        libc::O_WRONLY | libc::O_CLOEXEC,
                    );
                    if fd >= 0 {
                        let bytes = oom.as_bytes();
                        libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                        libc::close(fd);
                    }
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (command, policy);
}

/// What the kernel reports for a running child.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Observed {
    pub nice: Option<i32>,
    /// `best_effort`, `realtime`, `idle` or `none`.
    pub io_class: Option<&'static str>,
    pub io_level: Option<u8>,
    pub oom_score_adj: Option<i32>,
}

impl Observed {
    /// Whether the child runs at or below its class's policy. `None` when
    /// the child could not be read (it has already exited, or this platform
    /// applies no policy).
    pub fn applied(&self, policy: ChildPolicy) -> Option<bool> {
        if cfg!(target_os = "linux") {
            let (nice, class, level, oom) = (
                self.nice?,
                self.io_class?,
                self.io_level?,
                self.oom_score_adj?,
            );
            Some(
                nice >= policy.nice
                    && class == "best_effort"
                    && level >= policy.io_level
                    && oom >= policy.oom_score_adj,
            )
        } else if cfg!(unix) {
            Some(self.nice? >= policy.nice)
        } else {
            None
        }
    }
}

/// Read a child's scheduling state back from the kernel.
pub fn observe(pid: u32) -> Observed {
    #[cfg(target_os = "linux")]
    {
        linux::observe(pid)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Ok(pid) = libc::id_t::try_from(pid) else {
            return Observed::default();
        };
        // SAFETY: plain syscall; errno distinguishes -1 as a value from -1
        // as an error, and a pid that no longer exists reads as unknown.
        let nice = unsafe {
            *errno_location() = 0;
            let value = libc::getpriority(libc::PRIO_PROCESS, pid);
            (value != -1 || *errno_location() == 0).then_some(value)
        };
        Observed {
            nice,
            ..Observed::default()
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Observed::default()
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn errno_location() -> *mut libc::c_int {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        libc::__error()
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    {
        libc::__errno()
    }
}

/// One running child as the Activity page and `/metrics` see it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RunningChild {
    pub pid: u32,
    pub class: ChildClass,
    pub purpose: &'static str,
    pub reason: &'static str,
    pub program: String,
    pub started_at_ms: u64,
    pub requested: Option<ChildPolicy>,
    pub observed: Observed,
    pub applied: Option<bool>,
    /// Whether [`stop`] can kill it without any risk of signalling a
    /// recycled pid (Linux, through the pidfd opened at spawn).
    pub stoppable: bool,
}

/// Per-class counters for `/metrics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassCounters {
    pub class: ChildClass,
    pub running: u64,
    pub spawned: u64,
    pub unapplied: u64,
}

struct Entry {
    pid: u32,
    work: ChildWork,
    program: String,
    started_at_ms: u64,
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
}

#[derive(Default)]
struct Registry {
    next: u64,
    entries: BTreeMap<u64, Entry>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);
static SPAWNED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static UNAPPLIED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A running child's place in the registry; dropping it (with the
/// [`super::ChildJob`] that holds it) removes the child from the list.
#[derive(Debug)]
pub struct Registration {
    key: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        registry().entries.remove(&self.key);
    }
}

/// Record a child the launcher has just started. Never fails a spawn: a
/// child that cannot be observed is listed with what is known about it.
pub(crate) fn register(
    pid: Option<u32>,
    program: &std::ffi::OsStr,
    work: ChildWork,
) -> Option<Registration> {
    let pid = pid?;
    SPAWNED[work.class.index()].fetch_add(1, Ordering::Relaxed);
    if observe(pid).applied(work.class.policy()) == Some(false) {
        UNAPPLIED[work.class.index()].fetch_add(1, Ordering::Relaxed);
    }
    let program = std::path::Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let entry = Entry {
        pid,
        work,
        program,
        started_at_ms,
        #[cfg(target_os = "linux")]
        pidfd: linux::pidfd_open(pid),
    };
    let mut registry = registry();
    let key = registry.next;
    registry.next += 1;
    registry.entries.insert(key, entry);
    Some(Registration { key })
}

/// Every child the launcher started that its owner still holds, oldest
/// first, with its scheduling state read now.
pub fn running() -> Vec<RunningChild> {
    let rows = registry()
        .entries
        .values()
        .map(|entry| {
            #[cfg(target_os = "linux")]
            let stoppable = entry.pidfd.is_some();
            #[cfg(not(target_os = "linux"))]
            let stoppable = false;
            (
                entry.pid,
                entry.work,
                entry.program.clone(),
                entry.started_at_ms,
                stoppable,
            )
        })
        .collect::<Vec<_>>();
    rows.into_iter()
        .map(|(pid, work, program, started_at_ms, stoppable)| {
            let policy = work.class.policy();
            let observed = observe(pid);
            RunningChild {
                pid,
                class: work.class,
                purpose: work.purpose,
                reason: work.class.reason(),
                program,
                started_at_ms,
                requested: cfg!(unix).then_some(policy),
                applied: observed.applied(policy),
                observed,
                stoppable,
            }
        })
        .collect()
}

/// Kill one listed child. `Ok(false)` when no listed child has that pid.
///
/// The kill goes through the pidfd opened at spawn, which names that process
/// and no other, so a child its owner has already reaped cannot be confused
/// with an unrelated process that reused the number. Its owner sees an
/// ordinary child exit and handles it as it handles any other.
pub fn stop(pid: u32) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let registry = registry();
        let Some(entry) = registry.entries.values().find(|entry| entry.pid == pid) else {
            return Ok(false);
        };
        let Some(pidfd) = entry.pidfd.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this kernel gave no pidfd for the child, so it cannot be stopped safely",
            ));
        };
        linux::pidfd_kill(pidfd)?;
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let known = registry().entries.values().any(|entry| entry.pid == pid);
        if known {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stopping a listed child is only available on Linux",
            ))
        } else {
            Ok(false)
        }
    }
}

pub fn class_counters() -> Vec<ClassCounters> {
    let mut running = [0u64; 2];
    for entry in registry().entries.values() {
        running[entry.work.class.index()] += 1;
    }
    ChildClass::ALL
        .into_iter()
        .map(|class| ClassCounters {
            class,
            running: running[class.index()],
            spawned: SPAWNED[class.index()].load(Ordering::Relaxed),
            unapplied: UNAPPLIED[class.index()].load(Ordering::Relaxed),
        })
        .collect()
}

/// The `/metrics` series: children running now, children started, and
/// children whose priority read back above their class's policy, each by
/// class.
pub fn prometheus() -> String {
    let rows = class_counters();
    let mut out = String::from(
        "# HELP plurx_child_processes Child processes this node is running now, by priority class.\n\
         # TYPE plurx_child_processes gauge\n",
    );
    for row in &rows {
        out.push_str(&format!(
            "plurx_child_processes{{class=\"{}\"}} {}\n",
            row.class.as_str(),
            row.running
        ));
    }
    out.push_str(
        "# HELP plurx_child_spawns_total Child processes started through the launcher, by priority class.\n\
         # TYPE plurx_child_spawns_total counter\n",
    );
    for row in &rows {
        out.push_str(&format!(
            "plurx_child_spawns_total{{class=\"{}\"}} {}\n",
            row.class.as_str(),
            row.spawned
        ));
    }
    out.push_str(
        "# HELP plurx_child_priority_unapplied_total Children whose kernel-reported priority read back above their class's policy at spawn.\n\
         # TYPE plurx_child_priority_unapplied_total counter\n",
    );
    for row in &rows {
        out.push_str(&format!(
            "plurx_child_priority_unapplied_total{{class=\"{}\"}} {}\n",
            row.class.as_str(),
            row.unapplied
        ));
    }
    out
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Observed;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub(super) const IOPRIO_WHO_PROCESS: libc::c_int = 1;
    const IOPRIO_CLASS_SHIFT: libc::c_int = 13;
    const IOPRIO_CLASS_BE: libc::c_int = 2;

    pub(super) fn ioprio_value(level: u8) -> libc::c_int {
        (IOPRIO_CLASS_BE << IOPRIO_CLASS_SHIFT) | libc::c_int::from(level.min(7))
    }

    /// `oom_score_adj` as the decimal text `/proc` expects, formatted before
    /// the fork into a fixed buffer the child only reads.
    #[derive(Clone, Copy)]
    pub(super) struct OomBytes {
        bytes: [u8; 8],
        len: usize,
    }

    impl OomBytes {
        pub(super) fn new(value: i32) -> Self {
            let value = value.clamp(-1000, 1000);
            let mut bytes = [0u8; 8];
            let mut len = 0;
            if value < 0 {
                bytes[len] = b'-';
                len += 1;
            }
            for digit in value.unsigned_abs().to_string().bytes() {
                bytes[len] = digit;
                len += 1;
            }
            bytes[len] = b'\n';
            len += 1;
            Self { bytes, len }
        }

        pub(super) fn as_bytes(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }

    pub(super) fn observe(pid: u32) -> Observed {
        let nice = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| parse_stat_nice(&stat));
        let oom_score_adj = std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))
            .ok()
            .and_then(|text| text.trim().parse().ok());
        let (io_class, io_level) = match libc::pid_t::try_from(pid) {
            // SAFETY: plain syscall reading another process's I/O priority.
            Ok(pid) => {
                match unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, pid) } {
                    value if value < 0 => (None, None),
                    value => {
                        let value = value as libc::c_int;
                        let class = match value >> IOPRIO_CLASS_SHIFT {
                            0 => "none",
                            1 => "realtime",
                            2 => "best_effort",
                            3 => "idle",
                            _ => "unknown",
                        };
                        (Some(class), u8::try_from(value & 0x7).ok())
                    }
                }
            }
            Err(_) => (None, None),
        };
        Observed {
            nice,
            io_class,
            io_level,
            oom_score_adj,
        }
    }

    /// Field 19 of `/proc/<pid>/stat`. The command name (field 2) is
    /// parenthesised and may itself hold spaces or parentheses, so fields are
    /// counted from the last `)`.
    pub(super) fn parse_stat_nice(stat: &str) -> Option<i32> {
        let (_, rest) = stat.rsplit_once(')')?;
        rest.split_whitespace().nth(16)?.parse().ok()
    }

    pub(super) fn pidfd_open(pid: u32) -> Option<OwnedFd> {
        let pid = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: pidfd_open returns a new descriptor or -1; the owner has
        // not reaped the child yet, so the pid still names it.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        let fd = libc::c_int::try_from(fd).ok().filter(|fd| *fd >= 0)?;
        // SAFETY: `fd` was just returned by the kernel and is owned here.
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub(super) fn pidfd_kill(pidfd: &OwnedFd) -> std::io::Result<()> {
        // SAFETY: the descriptor is a live pidfd owned by the registry.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // The child exited between the listing and the stop: already done.
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::spawn_job_owned;
    use std::process::Stdio;
    use std::time::Duration;

    const CHILD_MODE: &str = "PLURX_PRIORITY_CHILD";

    /// Re-exec this test binary as a child that sleeps until killed.
    fn sleeper() -> tokio::process::Command {
        let mut command =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(CHILD_ARGS)
            .env(CHILD_MODE, "sleep")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    const CHILD_ARGS: [&str; 3] = [
        "--exact",
        "process::priority::tests::child_main",
        "--nocapture",
    ];

    #[test]
    fn child_main() {
        if std::env::var(CHILD_MODE).as_deref() == Ok("sleep") {
            std::thread::sleep(Duration::from_secs(120));
        }
    }

    #[cfg(target_os = "linux")]
    fn own_nice() -> i32 {
        // SAFETY: plain syscall on this thread; for the caller itself
        // getpriority cannot fail, so -1 is a value, not an error.
        unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }
    }

    #[cfg(target_os = "linux")]
    fn read_oom(pid: &str) -> i32 {
        std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))
            .expect("oom_score_adj")
            .trim()
            .parse()
            .expect("numeric oom_score_adj")
    }

    /// Each class's child, read back from `/proc/<pid>/stat` and
    /// `ioprio_get`, runs at exactly its policy, and the daemon (this test
    /// process) keeps its own.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn each_class_runs_its_child_at_the_class_nice_ionice_and_oom_score() {
        let daemon_nice = own_nice();
        let daemon_oom = read_oom("self");
        for class in ChildClass::ALL {
            let policy = class.policy();
            let spawned_before = class_counters()
                .into_iter()
                .find(|row| row.class == class)
                .expect("counter row")
                .spawned;
            let (child, _job) = spawn_job_owned(
                &mut sleeper(),
                ChildWork {
                    class,
                    purpose: "priority test",
                },
            )
            .expect("spawn");
            let pid = child.id().expect("running child");

            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
            assert_eq!(
                linux::parse_stat_nice(&stat),
                Some(policy.nice.max(daemon_nice)),
                "{class:?} nice"
            );
            // SAFETY: plain syscall reading the child's I/O priority.
            let ioprio = unsafe {
                libc::syscall(
                    libc::SYS_ioprio_get,
                    linux::IOPRIO_WHO_PROCESS,
                    libc::pid_t::try_from(pid).expect("pid_t"),
                )
            } as libc::c_int;
            assert_eq!(ioprio >> 13, 2, "{class:?} I/O class is best-effort");
            assert_eq!(
                ioprio & 0x7,
                libc::c_int::from(policy.io_level),
                "{class:?} I/O level"
            );
            assert_eq!(
                read_oom(&pid.to_string()),
                policy.oom_score_adj.max(daemon_oom),
                "{class:?} oom_score_adj"
            );

            let row = running()
                .into_iter()
                .find(|row| row.pid == pid)
                .expect("the child is listed while its job is held");
            assert_eq!(row.class, class);
            assert_eq!(row.purpose, "priority test");
            assert_eq!(row.reason, class.reason());
            assert_eq!(row.applied, Some(true));
            assert!(row.stoppable);
            assert!(
                class_counters()
                    .into_iter()
                    .find(|row| row.class == class)
                    .expect("counter row")
                    .spawned
                    > spawned_before
            );
        }
        assert_eq!(own_nice(), daemon_nice, "the daemon keeps its own nice");
        assert_eq!(
            read_oom("self"),
            daemon_oom,
            "the daemon keeps its own OOM score"
        );
        assert!(
            ChildClass::Background.policy().nice > ChildClass::Realtime.policy().nice
                && ChildClass::Background.policy().io_level
                    > ChildClass::Realtime.policy().io_level
                && ChildClass::Background.policy().oom_score_adj
                    > ChildClass::Realtime.policy().oom_score_adj,
            "background work runs below playback on every axis"
        );
    }

    /// A kernel that refuses part of the policy (an unprivileged process may
    /// not lower its OOM score) must not turn into a failed spawn.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_priority_the_kernel_refuses_never_fails_the_spawn() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "cat /proc/self/oom_score_adj"]);
        apply_policy(
            command.as_std_mut(),
            ChildPolicy {
                nice: -20,
                io_level: 0,
                oom_score_adj: -1000,
            },
        );
        let output = crate::process::output_job_owned(
            &mut command,
            ChildWork::realtime("refused priority test"),
        )
        .await
        .expect("a refused priority must not fail the spawn");
        assert!(output.status.success());
        // SAFETY: plain syscall.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "500",
                "the refused -1000 left the class's own value in place"
            );
        }
    }

    /// The decode-fact probe execs from inside its own `pre_exec`. A priority
    /// registered before that closure applies; the launcher's own
    /// registration, which lands after it, never runs. This is why
    /// `decode_facts::configure_probe_execution` calls [`apply`] first.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_priority_registered_before_a_closure_that_execs_still_applies() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::process::CommandExt;

        struct Argv {
            _strings: Vec<CString>,
            pointers: Vec<*const libc::c_char>,
        }
        // SAFETY: the pointers point into `_strings`, which the closure owns
        // and never mutates.
        unsafe impl Send for Argv {}
        unsafe impl Sync for Argv {}

        let daemon_nice = own_nice();
        let class = ChildClass::Background;
        for (apply_first, expected) in [
            (true, class.policy().nice.max(daemon_nice)),
            (false, daemon_nice),
        ] {
            let executable = std::env::current_exe().expect("test executable");
            let mut strings = vec![CString::new(executable.as_os_str().as_bytes()).expect("path")];
            strings.extend(
                CHILD_ARGS
                    .iter()
                    .map(|arg| CString::new(*arg).expect("arg")),
            );
            let mut pointers = strings.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
            pointers.push(std::ptr::null());
            let argv = Argv {
                _strings: strings,
                pointers,
            };

            let mut command = sleeper();
            if apply_first {
                apply(&mut command, class);
            }
            // SAFETY: execv on pointers prepared before the fork.
            unsafe {
                command.as_std_mut().pre_exec(move || {
                    // The whole `Argv` moves in, not just its pointer field.
                    let argv = &argv;
                    libc::execv(argv.pointers[0], argv.pointers.as_ptr());
                    Err(io::Error::last_os_error())
                });
            }
            let (child, _job) =
                spawn_job_owned(&mut command, ChildWork::background("exec ordering test"))
                    .expect("spawn");
            let pid = child.id().expect("running child");
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
            assert_eq!(
                linux::parse_stat_nice(&stat),
                Some(expected),
                "apply before the exec closure: {apply_first}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_listed_child_can_be_stopped_and_leaves_the_list_with_its_job() {
        use std::os::unix::process::ExitStatusExt;

        let (mut child, job) =
            spawn_job_owned(&mut sleeper(), ChildWork::background("stop test")).expect("spawn");
        let pid = child.id().expect("running child");
        assert!(running().iter().any(|row| row.pid == pid && row.stoppable));
        assert!(stop(pid).expect("stop"));
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("the stopped child exits")
            .expect("wait");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        drop(job);
        assert!(!running().iter().any(|row| row.pid == pid));
        assert!(!stop(pid).expect("an unlisted pid is not an error"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_nice_is_read_after_a_command_name_holding_parentheses() {
        let stat = "4242 (ff) (mpeg) S 1 4242 4242 0 -1 4194560 1 0 0 0 0 0 0 0 30 10 1 0 5";
        assert_eq!(linux::parse_stat_nice(stat), Some(10));
        assert_eq!(linux::OomBytes::new(800).as_bytes(), b"800\n");
        assert_eq!(linux::OomBytes::new(-1000).as_bytes(), b"-1000\n");
        assert_eq!(linux::ioprio_value(7), (2 << 13) | 7);
    }

    #[test]
    fn metrics_carry_every_series_for_every_class() {
        let text = prometheus();
        for series in [
            "plurx_child_processes",
            "plurx_child_spawns_total",
            "plurx_child_priority_unapplied_total",
        ] {
            assert!(text.contains(&format!("# TYPE {series} ")), "{text}");
            for class in ChildClass::ALL {
                assert!(
                    text.contains(&format!("{series}{{class=\"{}\"}} ", class.as_str())),
                    "{series} for {class:?}: {text}"
                );
            }
        }
    }

    #[test]
    fn a_child_reading_at_or_below_its_policy_counts_as_applied() {
        let policy = ChildClass::Realtime.policy();
        let observed = Observed {
            nice: Some(policy.nice),
            io_class: Some("best_effort"),
            io_level: Some(policy.io_level),
            oom_score_adj: Some(policy.oom_score_adj),
        };
        if cfg!(unix) {
            assert_eq!(observed.applied(policy), Some(true));
            let unapplied = Observed {
                nice: Some(policy.nice - 1),
                ..observed
            };
            assert_eq!(unapplied.applied(policy), Some(false));
            assert_eq!(Observed::default().applied(policy), None);
        } else {
            assert_eq!(observed.applied(policy), None);
        }
    }
}
