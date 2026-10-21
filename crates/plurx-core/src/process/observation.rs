//! Dimensioned, identity-bound process observations. These are diagnostics, not
//! admission permits. A leader's counters never claim complete graph coverage.
//! All reads are bounded and missing data is explicitly unknown, never idle.

use std::time::{Duration, Instant};

tokio::task_local! {
    static OBSERVATION_GRAPH: u64;
}
static NEXT_GRAPH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Associate actual launcher registrations in this task with one managed
/// graph. An observation scope is NOT admission, custody, descendant closure,
/// or a reap receipt. Spawned tasks do not inherit task-local association.
pub async fn graph_scope<F: std::future::Future>(future: F) -> F::Output {
    if current_graph().is_some() {
        return future.await;
    }
    use std::sync::atomic::Ordering;
    let Ok(id) =
        NEXT_GRAPH.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
    else {
        // Exhausted diagnostic identifiers grant no grouping or authority.
        return future.await;
    };
    OBSERVATION_GRAPH.scope(id, future).await
}

pub(crate) fn current_graph() -> Option<u64> {
    OBSERVATION_GRAPH.try_with(|id| *id).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    Unsupported,
    Inaccessible,
    InvalidCounter,
    IdentityChanged,
    UnboundIdentity,
    FirstSample,
    Stale,
    CounterRegressed,
    IncompleteTree,
    InsufficientEvidence,
    Unbounded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Metric<T> {
    Known(T),
    Unknown(UnknownReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// OS creation marker. Only compared on this node during this boot.
    pub start_marker: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProcessCounters {
    pub identity: ProcessIdentity,
    /// This process's user + kernel CPU, excluding recursive child totals.
    pub cpu_ns: u64,
    pub resident_bytes: Metric<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProcessObservation {
    pub counters: Metric<ProcessCounters>,
    /// CPU nanoseconds per elapsed wall second; may exceed one CPU core.
    pub cpu_ns_per_second: Metric<u64>,
    /// No complete descendant census is implied by a successful leader read.
    pub graph_cpu_ns: Metric<u64>,
    pub age_ms: u64,
}

/// One cache per owned child. No OS read under the process-registry lock.
/// API callers share the one-second cadence instead of sampling per request.
pub struct ProcessObserver {
    pid: u32,
    identity: Option<ProcessIdentity>,
    previous: Option<(Instant, ProcessCounters)>,
    cached: Option<(Instant, ProcessObservation)>,
}

impl ProcessObserver {
    pub fn new(pid: u32) -> Self {
        // Capture identity once while the launcher still owns the child. If
        // that fails, never bind a later, possibly recycled numeric pid.
        let counters = read(pid);
        let mut observer = Self {
            pid,
            identity: match counters {
                Metric::Known(value) => Some(value.identity),
                Metric::Unknown(_) => None,
            },
            previous: None,
            cached: None,
        };
        observer.record(Instant::now(), counters);
        observer
    }

    pub fn snapshot(&mut self, now: Instant) -> ProcessObservation {
        if let Some((at, mut observation)) = self.cached {
            let age = now.saturating_duration_since(at);
            if age < Duration::from_secs(1) {
                observation.age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
                return observation;
            }
        }
        self.record(now, read(self.pid))
    }

    fn record(&mut self, now: Instant, counters: Metric<ProcessCounters>) -> ProcessObservation {
        let counters = match counters {
            Metric::Known(_) if self.identity.is_none() => {
                Metric::Unknown(UnknownReason::UnboundIdentity)
            }
            Metric::Known(value) if self.identity.is_some_and(|id| id != value.identity) => {
                Metric::Unknown(UnknownReason::IdentityChanged)
            }
            Metric::Known(value) => Metric::Known(value),
            unknown => unknown,
        };
        let rate = match (self.previous, counters) {
            (Some((before, old)), Metric::Known(current)) => rate(before, old, now, current),
            (_, Metric::Unknown(reason)) => Metric::Unknown(reason),
            _ => Metric::Unknown(UnknownReason::FirstSample),
        };
        // Missing samples break the window. Never bridge a gap as positive proof.
        self.previous = match counters {
            Metric::Known(value) => Some((now, value)),
            Metric::Unknown(_) => None,
        };
        let observation = ProcessObservation {
            counters,
            cpu_ns_per_second: rate,
            graph_cpu_ns: Metric::Unknown(UnknownReason::IncompleteTree),
            age_ms: 0,
        };
        self.cached = Some((now, observation));
        observation
    }
}

fn rate(
    before: Instant,
    old: ProcessCounters,
    now: Instant,
    current: ProcessCounters,
) -> Metric<u64> {
    if old.identity != current.identity {
        return Metric::Unknown(UnknownReason::IdentityChanged);
    }
    let Some(elapsed) = now.checked_duration_since(before) else {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    };
    if elapsed > Duration::from_secs(2) {
        return Metric::Unknown(UnknownReason::Stale);
    }
    if elapsed.is_zero() {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    }
    let Some(cpu) = current.cpu_ns.checked_sub(old.cpu_ns) else {
        return Metric::Unknown(UnknownReason::CounterRegressed);
    };
    let value = u128::from(cpu) * 1_000_000_000 / elapsed.as_nanos();
    u64::try_from(value)
        .map(Metric::Known)
        .unwrap_or(Metric::Unknown(UnknownReason::InvalidCounter))
}

/// Node-local sampler owned by the daemon's ordinary shutdown token. Missed
/// ticks are skipped instead of replayed as fabricated observations.
pub async fn sample_loop(shutdown: tokio_util::sync::CancellationToken) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            _ = timer.tick() => super::priority::sample_resources(),
        }
    }
}

#[cfg(target_os = "linux")]
fn read(pid: u32) -> Metric<ProcessCounters> {
    use std::io::Read as _;
    // proc stat is one bounded kernel-generated line. A name may contain ')'.
    let result = (|| {
        let file = std::fs::File::open(format!("/proc/{pid}/stat")).ok()?;
        let mut stat = String::new();
        file.take(16 * 1024).read_to_string(&mut stat).ok()?;
        // SAFETY: sysconf has no pointer parameters or side effects.
        let ticks = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
        let pages = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        parse_linux(pid, &stat, ticks, pages)
    })();
    result
        .map(Metric::Known)
        .unwrap_or(Metric::Unknown(UnknownReason::Inaccessible))
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux(pid: u32, stat: &str, ticks: u64, page_size: u64) -> Option<ProcessCounters> {
    let (prefix, rest) = stat.rsplit_once(')')?;
    let (reported_pid, _) = prefix.split_once('(')?;
    if reported_pid.trim().parse::<u32>().ok()? != pid || ticks == 0 || page_size == 0 {
        return None;
    }
    let mut fields = rest.split_whitespace();
    // Rest starts at field 3 (state). utime=14, stime=15, starttime=22, rss=24.
    let user = fields.nth(11)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    let start_marker = fields.nth(6)?.parse::<u64>().ok()?;
    let resident_pages = fields.nth(1)?.parse::<u64>().ok()?;
    let cpu_ns = u128::from(user.checked_add(system)?) * 1_000_000_000 / u128::from(ticks);
    Some(ProcessCounters {
        identity: ProcessIdentity { pid, start_marker },
        cpu_ns: u64::try_from(cpu_ns).ok()?,
        resident_bytes: resident_pages
            .checked_mul(page_size)
            .map(Metric::Known)
            .unwrap_or(Metric::Unknown(UnknownReason::InvalidCounter)),
    })
}

#[cfg(target_os = "macos")]
fn read(pid: u32) -> Metric<ProcessCounters> {
    let Ok(pid_native) = i32::try_from(pid) else {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    };
    // SAFETY: rusage_info_v2 is the buffer type for RUSAGE_INFO_V2 and is
    // fully initialized before use. The API writes at most this flavor's size.
    let mut usage: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let status =
        unsafe { libc::proc_pid_rusage(pid_native, libc::RUSAGE_INFO_V2, (&raw mut usage).cast()) };
    if status != 0 {
        return Metric::Unknown(UnknownReason::Inaccessible);
    }
    let Some(cpu_ns) = usage.ri_user_time.checked_add(usage.ri_system_time) else {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    };
    // ri_child_* are deliberately excluded: summing recursive child totals
    // with independently sampled helpers double counts the same work.
    Metric::Known(ProcessCounters {
        identity: ProcessIdentity {
            pid,
            start_marker: usage.ri_proc_start_abstime,
        },
        cpu_ns,
        resident_bytes: Metric::Known(usage.ri_resident_size),
    })
}

#[cfg(windows)]
fn read(pid: u32) -> Metric<ProcessCounters> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
        },
    };
    // SAFETY: the handle is only queried, every output points to initialized
    // storage of the documented size, and all paths close the process handle.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return Metric::Unknown(UnknownReason::Inaccessible);
        }
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let times = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let size = std::mem::size_of_val(&memory) as u32;
        memory.cb = size;
        let has_memory = GetProcessMemoryInfo(handle, &mut memory, size) != 0;
        CloseHandle(handle);
        if times == 0 {
            return Metric::Unknown(UnknownReason::Inaccessible);
        }
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        let Some(cpu_ns) = ticks(kernel)
            .checked_add(ticks(user))
            .and_then(|v| v.checked_mul(100))
        else {
            return Metric::Unknown(UnknownReason::InvalidCounter);
        };
        Metric::Known(ProcessCounters {
            identity: ProcessIdentity {
                pid,
                start_marker: ticks(created),
            },
            cpu_ns,
            resident_bytes: if has_memory {
                Metric::Known(memory.WorkingSetSize as u64)
            } else {
                Metric::Unknown(UnknownReason::Inaccessible)
            },
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read(_pid: u32) -> Metric<ProcessCounters> {
    Metric::Unknown(UnknownReason::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter(cpu_ns: u64, start_marker: u64) -> ProcessCounters {
        ProcessCounters {
            identity: ProcessIdentity {
                pid: 42,
                start_marker,
            },
            cpu_ns,
            resident_bytes: Metric::Known(4096),
        }
    }

    #[test]
    fn counter_identity_and_missing_samples_never_imply_idle() {
        let now = Instant::now();
        let mut observer = ProcessObserver {
            pid: 42,
            identity: Some(counter(0, 1).identity),
            previous: None,
            cached: None,
        };
        assert_eq!(
            observer
                .record(now, Metric::Known(counter(10, 1)))
                .cpu_ns_per_second,
            Metric::Unknown(UnknownReason::FirstSample)
        );
        let at = now + Duration::from_secs(1);
        assert_eq!(
            observer
                .record(at, Metric::Unknown(UnknownReason::Inaccessible))
                .cpu_ns_per_second,
            Metric::Unknown(UnknownReason::Inaccessible)
        );
        assert_eq!(
            observer
                .record(at + Duration::from_secs(1), Metric::Known(counter(20, 1)))
                .cpu_ns_per_second,
            Metric::Unknown(UnknownReason::FirstSample)
        );
        let reused = observer.record(at + Duration::from_secs(2), Metric::Known(counter(30, 2)));
        assert_eq!(
            reused.counters,
            Metric::Unknown(UnknownReason::IdentityChanged)
        );
        assert_eq!(
            reused.graph_cpu_ns,
            Metric::Unknown(UnknownReason::IncompleteTree)
        );
    }

    #[test]
    fn failed_initial_identity_binding_cannot_attach_to_a_later_pid() {
        let mut observer = ProcessObserver {
            pid: 42,
            identity: None,
            previous: None,
            cached: None,
        };
        let observed = observer.record(Instant::now(), Metric::Known(counter(10, 2)));
        assert_eq!(
            observed.counters,
            Metric::Unknown(UnknownReason::UnboundIdentity)
        );
        assert_eq!(
            observed.cpu_ns_per_second,
            Metric::Unknown(UnknownReason::UnboundIdentity)
        );
    }

    #[test]
    fn cpu_rate_is_dimensioned_and_rejects_stale_regressed_and_zero_windows() {
        let now = Instant::now();
        let old = counter(10, 1);
        assert_eq!(
            rate(
                now,
                old,
                now + Duration::from_secs(1),
                counter(2_000_000_010, 1)
            ),
            Metric::Known(2_000_000_000)
        );
        assert_eq!(
            rate(now, old, now + Duration::from_secs(3), counter(20, 1)),
            Metric::Unknown(UnknownReason::Stale)
        );
        assert_eq!(
            rate(now, old, now + Duration::from_secs(1), counter(9, 1)),
            Metric::Unknown(UnknownReason::CounterRegressed)
        );
        assert_eq!(
            rate(now, old, now, old),
            Metric::Unknown(UnknownReason::InvalidCounter)
        );
    }

    #[test]
    fn linux_parser_excludes_child_totals_and_handles_parentheses_in_names() {
        let stat = "42 (helper (name)) S 1 1 1 0 0 0 0 0 0 0 100 50 9999 9999 0 0 1 0 123 4096 2";
        let result = parse_linux(42, stat, 100, 4096).expect("stat");
        assert_eq!(result.cpu_ns, 1_500_000_000);
        assert_eq!(result.identity.start_marker, 123);
        assert_eq!(result.resident_bytes, Metric::Known(8192));
        assert!(parse_linux(43, stat, 100, 4096).is_none());
        assert!(parse_linux(42, stat, 0, 4096).is_none());
        assert!(parse_linux(42, "truncated", 100, 4096).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_owned_child_has_identity_bound_counters_until_reap() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "exec sleep 30"]).kill_on_drop(true);
        let (mut child, job) = crate::process::spawn_job_owned(
            &mut command,
            crate::process::ChildWork::background("resource_observation_fixture"),
        )
        .expect("owned fixture");
        let mut observer = ProcessObserver::new(child.id().expect("pid"));
        let first = observer.snapshot(Instant::now());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let second = observer.snapshot(Instant::now());
        // Clean up even when a platform's observation assertion will fail.
        child.start_kill().expect("kill");
        child.wait().await.expect("reap");
        drop(job);
        let Metric::Known(a) = first.counters else {
            panic!("first counters: {first:?}");
        };
        let Metric::Known(b) = second.counters else {
            panic!("second counters: {second:?}");
        };
        assert_eq!(a.identity, b.identity);
        assert!(b.cpu_ns >= a.cpu_ns);
        assert!(matches!(b.resident_bytes, Metric::Known(_)));
        assert!(matches!(second.cpu_ns_per_second, Metric::Known(_)));
        assert_eq!(
            second.graph_cpu_ns,
            Metric::Unknown(UnknownReason::IncompleteTree)
        );
    }

    #[tokio::test]
    async fn resource_sampler_exits_on_ordinary_daemon_shutdown() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), sample_loop(shutdown))
            .await
            .expect("sampler stopped");
    }

    #[tokio::test]
    async fn managed_graph_scope_is_task_local_not_a_cleanup_receipt() {
        assert_eq!(current_graph(), None);
        let first = graph_scope(async {
            let id = current_graph().expect("managed scope");
            assert_eq!(graph_scope(async { current_graph() }).await, Some(id));
            assert_eq!(
                tokio::spawn(async { current_graph() })
                    .await
                    .expect("child task"),
                None
            );
            id
        })
        .await;
        assert_eq!(current_graph(), None);
        let second = graph_scope(async { current_graph().expect("second scope") }).await;
        assert_ne!(first, second);
    }
}
