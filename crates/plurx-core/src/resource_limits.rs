//! Effective constraints are dimensions, not a mixed "spare" score. Unknown
//! containment limits stay unknown; host totals cannot authorize expansion.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::process::observation::{Metric, UnknownReason};

pub mod admission;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EffectiveLimits {
    /// The standard library's affinity/container-aware scheduling width, not
    /// measured CPU use or a promise that every thread can be busy at once.
    pub available_threads: Metric<u64>,
    /// Aggregate CPU nanoseconds that may be consumed per wall second.
    pub cpu_ns_per_second: Metric<u64>,
    /// Effective containment memory ceiling; not current free memory.
    pub memory_ceiling_bytes: Metric<u64>,
    pub open_file_ceiling: Metric<u64>,
    pub age_ms: u64,
}

static CACHE: LazyLock<Mutex<Option<(Instant, EffectiveLimits)>>> = LazyLock::new(Mutex::default);

/// At most one OS constraint read per second for this node. No Store access,
/// subprocess, device probe, or configuration change occurs here.
pub fn snapshot() -> EffectiveLimits {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    if let Some((at, mut value)) = *cache {
        let age = now.saturating_duration_since(at);
        if age < Duration::from_secs(1) {
            value.age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
            return value;
        }
    }
    let threads = std::thread::available_parallelism()
        .ok()
        .and_then(|value| u64::try_from(value.get()).ok())
        .map(Metric::Known)
        .unwrap_or(Metric::Unknown(UnknownReason::Inaccessible));
    let (cpu, memory) = containment(threads);
    let value = EffectiveLimits {
        available_threads: threads,
        cpu_ns_per_second: cpu,
        memory_ceiling_bytes: memory,
        open_file_ceiling: file_limit(),
        age_ms: 0,
    };
    *cache = Some((now, value));
    value
}

#[cfg(unix)]
fn file_limit() -> Metric<u64> {
    // SAFETY: getrlimit writes one initialized rlimit of the documented size.
    let mut limits: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } != 0 {
        return Metric::Unknown(UnknownReason::Inaccessible);
    }
    if limits.rlim_cur == libc::RLIM_INFINITY {
        return Metric::Unknown(UnknownReason::Unbounded);
    }
    Metric::Known(limits.rlim_cur)
}

#[cfg(not(unix))]
fn file_limit() -> Metric<u64> {
    Metric::Unknown(UnknownReason::Unsupported)
}

#[cfg(target_os = "linux")]
fn containment(threads: Metric<u64>) -> (Metric<u64>, Metric<u64>) {
    let unknown = Metric::Unknown(UnknownReason::Inaccessible);
    let result = (|| {
        let cgroups = read_bounded(std::path::Path::new("/proc/self/cgroup"))?;
        let mounts = read_bounded(std::path::Path::new("/proc/self/mountinfo"))?;
        let (root, leaf) = unified_path(&cgroups, &mounts)?;
        let mut cpu = match threads {
            Metric::Known(value) => value.checked_mul(1_000_000_000)?,
            _ => return None,
        };
        let mut memory = None;
        let mut current = leaf.as_path();
        let mut ancestors = 0_u8;
        loop {
            ancestors = ancestors.checked_add(1)?;
            if ancestors > 64 {
                return None;
            }
            // Read EVERY visible ancestor: a leaf's `max` does not override a
            // finite parent. Failure at any ancestor invalidates the vector.
            let quota = parse_cpu_max(&read_bounded(&current.join("cpu.max"))?)?;
            if let Some(quota) = quota {
                cpu = cpu.min(quota);
            }
            if let Some(limit) = parse_memory_max(&read_bounded(&current.join("memory.max"))?)? {
                memory = Some(memory.map_or(limit, |old: u64| old.min(limit)));
            }
            if current == root {
                break;
            }
            current = current.parent()?;
        }
        Some((
            Metric::Known(cpu),
            memory
                .map(Metric::Known)
                .unwrap_or(Metric::Unknown(UnknownReason::Unbounded)),
        ))
    })();
    result.unwrap_or((unknown, unknown))
}

#[cfg(not(target_os = "linux"))]
fn containment(_threads: Metric<u64>) -> (Metric<u64>, Metric<u64>) {
    // Windows job constraints and macOS containment are not inferred from host
    // CPU/RAM. Their adapters must establish an effective hard bound first.
    (
        Metric::Unknown(UnknownReason::Unsupported),
        Metric::Unknown(UnknownReason::Unsupported),
    )
}

#[cfg(target_os = "linux")]
fn read_bounded(path: &std::path::Path) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut value = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut value).ok()?;
    (value.len() <= 64 * 1024).then_some(value)
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_max(value: &str) -> Option<Option<u64>> {
    let mut words = value.split_whitespace();
    let quota = words.next()?;
    let period = words.next()?.parse::<u64>().ok()?;
    if words.next().is_some() || period == 0 {
        return None;
    }
    if quota == "max" {
        return Some(None);
    }
    let quota = quota.parse::<u64>().ok()?;
    let budget = u128::from(quota) * 1_000_000_000 / u128::from(period);
    Some(Some(u64::try_from(budget).ok()?))
}

#[cfg(any(target_os = "linux", test))]
fn parse_memory_max(value: &str) -> Option<Option<u64>> {
    if value.trim() == "max" {
        Some(None)
    } else {
        value.trim().parse().ok().map(Some)
    }
}

#[cfg(any(target_os = "linux", test))]
fn unified_path(cgroups: &str, mounts: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    use std::path::{Component, Path};
    let group = cgroups.lines().find_map(|line| line.strip_prefix("0::"))?;
    let group = Path::new(group);
    // A namespace root may hide constraints above it. Without a verified
    // containing hierarchy it cannot supply evidence for expansion.
    if group == Path::new("/") {
        return None;
    }
    if !group.is_absolute()
        || group
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    // Mountinfo escapes are intentionally not guessed. An unusual escaped
    // mount/root remains Unknown rather than reading a different hierarchy.
    for line in mounts.lines() {
        let (before, after) = line.split_once(" - ")?;
        if after.split_whitespace().next()? != "cgroup2" {
            continue;
        }
        let fields: Vec<_> = before.split_whitespace().take(6).collect();
        let root = *fields.get(3)?;
        let mount = *fields.get(4)?;
        if root.contains('\\') || mount.contains('\\') {
            continue;
        }
        let root = Path::new(root);
        let mount = Path::new(mount);
        if !root.is_absolute() || !mount.is_absolute() {
            continue;
        }
        // A subtree mount hides possibly stricter parents. Do not call the
        // visible leaf's allowance the effective containment ceiling.
        if root != Path::new("/") {
            continue;
        }
        let Ok(relative) = group.strip_prefix(root) else {
            continue;
        };
        let leaf = mount.join(relative);
        return Some((mount.to_owned(), leaf));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_quota_is_not_rounded_up_to_an_idle_core() {
        assert_eq!(parse_cpu_max("50000 100000\n"), Some(Some(500_000_000)));
        assert_eq!(parse_cpu_max("max 100000"), Some(None));
        assert_eq!(parse_cpu_max("max 0"), None);
        assert_eq!(parse_cpu_max("1 0"), None);
        assert_eq!(parse_cpu_max("1 1 extra"), None);
        assert_eq!(parse_memory_max("max\n"), Some(None));
        assert_eq!(parse_memory_max("0"), Some(Some(0)));
        assert_eq!(parse_memory_max("-1"), None);
    }

    #[test]
    fn cgroup_hierarchy_is_resolved_relative_to_its_actual_mount_root() {
        let mounts = "41 25 0:32 / /sys/fs/cgroup rw - cgroup2 cgroup rw";
        let (root, leaf) = unified_path("0::/tenant/app\n", mounts).expect("mount");
        assert_eq!(root, std::path::Path::new("/sys/fs/cgroup"));
        assert_eq!(leaf, std::path::Path::new("/sys/fs/cgroup/tenant/app"));
        assert!(unified_path("0::/tenant/../other", mounts).is_none());
        assert!(unified_path(
            "0::/tenant/app",
            "41 25 0:32 /tenant /sys/fs/cgroup rw - cgroup2 cgroup rw"
        )
        .is_none());
        assert!(unified_path("1:cpu:/app", mounts).is_none());
    }
}
