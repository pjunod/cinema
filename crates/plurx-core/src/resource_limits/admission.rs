//! Fresh observational headroom for a caller that will reserve under its
//! ledger. These measurements can race external processes and filesystem use;
//! they are not guarantees or persistent reservations.

use std::path::Path;
use std::time::Instant;

use crate::adaptive_resources::ResourceCapacity;
use crate::process::observation::{Metric, UnknownReason};

#[derive(Clone, Copy, Debug)]
pub struct AdmissionObservation {
    pub measured_at: Instant,
    pub capacity: ResourceCapacity,
}

/// Measures current per-dimension headroom without creating or opening a
/// resource root. Callers must still reserve under their ledger and retain
/// failed or uncertain effects. Scratch is available logical filesystem space,
/// not a physical-device or future-allocation guarantee.
pub fn snapshot(output_root: &Path) -> AdmissionObservation {
    let measured_at = Instant::now();
    let limits = super::snapshot();
    #[cfg(not(target_os = "linux"))]
    let _ = output_root;
    #[cfg(target_os = "linux")]
    let (handles, processes) = (
        linux_handle_headroom(limits.open_file_ceiling),
        linux_process_headroom(),
    );
    #[cfg(not(target_os = "linux"))]
    let (handles, processes) = (
        Metric::Unknown(UnknownReason::Unsupported),
        Metric::Unknown(UnknownReason::Unsupported),
    );

    #[cfg(target_os = "linux")]
    let scratch_bytes = filesystem_headroom(output_root);
    #[cfg(not(target_os = "linux"))]
    let scratch_bytes = Metric::Unknown(UnknownReason::Unsupported);

    AdmissionObservation {
        measured_at,
        capacity: ResourceCapacity {
            cpu_ns_per_second: limits.cpu_ns_per_second,
            memory_bytes: limits.memory_ceiling_bytes,
            handles,
            processes,
            hardware_slots: Metric::Unknown(UnknownReason::InsufficientEvidence),
            tuner_slots: Metric::Unknown(UnknownReason::InsufficientEvidence),
            scratch_bytes,
        },
    }
}

#[cfg(target_os = "linux")]
const MAX_PROC_ENTRIES: usize = 32_768;

#[cfg(target_os = "linux")]
fn linux_handle_headroom(ceiling: Metric<u64>) -> Metric<u64> {
    use std::fs;

    let ceiling = match ceiling {
        Metric::Known(ceiling) => ceiling,
        Metric::Unknown(reason) => return Metric::Unknown(reason),
    };
    let Ok(entries) = fs::read_dir("/proc/self/fd") else {
        return Metric::Unknown(UnknownReason::Inaccessible);
    };
    let mut open = 0_u64;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_PROC_ENTRIES {
            return Metric::Unknown(UnknownReason::InvalidCounter);
        }
        if entry.is_err() {
            return Metric::Unknown(UnknownReason::Inaccessible);
        }
        let Some(next) = open.checked_add(1) else {
            return Metric::Unknown(UnknownReason::InvalidCounter);
        };
        open = next;
    }
    ceiling
        .checked_sub(open)
        .map(Metric::Known)
        .unwrap_or(Metric::Unknown(UnknownReason::InvalidCounter))
}

#[cfg(target_os = "linux")]
fn linux_process_headroom() -> Metric<u64> {
    let Some(global_limit) = read_decimal(Path::new("/proc/sys/kernel/threads-max")) else {
        return Metric::Unknown(UnknownReason::Inaccessible);
    };
    let Some(global_tasks) = global_task_count() else {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    };
    let Some(global_available) = global_limit.checked_sub(global_tasks) else {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    };
    let mut available = global_available;

    let Some(cgroups) = super::read_bounded(Path::new("/proc/self/cgroup")) else {
        return Metric::Unknown(UnknownReason::Inaccessible);
    };
    let Some(mounts) = super::read_bounded(Path::new("/proc/self/mountinfo")) else {
        return Metric::Unknown(UnknownReason::Inaccessible);
    };
    let Some((root, leaf)) = super::unified_path(&cgroups, &mounts) else {
        return Metric::Unknown(UnknownReason::IncompleteTree);
    };
    let mut current = leaf.as_path();
    let mut ancestors = 0_u8;
    loop {
        ancestors = match ancestors.checked_add(1) {
            Some(value) if value <= 64 => value,
            _ => return Metric::Unknown(UnknownReason::IncompleteTree),
        };
        let Some(limit_text) = super::read_bounded(&current.join("pids.max")) else {
            return Metric::Unknown(UnknownReason::Inaccessible);
        };
        let Some(current_text) = super::read_bounded(&current.join("pids.current")) else {
            return Metric::Unknown(UnknownReason::Inaccessible);
        };
        let Some(cgroup_current) = parse_decimal(&current_text) else {
            return Metric::Unknown(UnknownReason::InvalidCounter);
        };
        match parse_limit(&limit_text) {
            Some(Some(limit)) => {
                let Some(headroom) = limit.checked_sub(cgroup_current) else {
                    return Metric::Unknown(UnknownReason::InvalidCounter);
                };
                available = available.min(headroom);
            }
            Some(None) => {}
            None => return Metric::Unknown(UnknownReason::InvalidCounter),
        }
        if current == root {
            break;
        }
        let Some(parent) = current.parent() else {
            return Metric::Unknown(UnknownReason::IncompleteTree);
        };
        current = parent;
    }

    let uid = current_real_uid();
    if uid != 0 {
        match rlimit_nproc_limit() {
            Some(Some(limit)) => {
                let Some(uid_tasks) = same_uid_task_count(uid) else {
                    return Metric::Unknown(UnknownReason::IncompleteTree);
                };
                let Some(headroom) = limit.checked_sub(uid_tasks) else {
                    return Metric::Unknown(UnknownReason::InvalidCounter);
                };
                available = available.min(headroom);
            }
            Some(None) => {}
            None => return Metric::Unknown(UnknownReason::Inaccessible),
        }
    }
    Metric::Known(available)
}

#[cfg(target_os = "linux")]
fn current_real_uid() -> u32 {
    // SAFETY: getuid reads the calling process's real UID and has no pointer arguments.
    unsafe { libc::getuid() }
}

#[cfg(target_os = "linux")]
fn same_uid_task_count(uid: u32) -> Option<u64> {
    use std::fs;

    let entries = fs::read_dir("/proc").ok()?;
    let mut seen = 0_usize;
    let mut tasks = 0_u64;
    for entry in entries {
        let entry = entry.ok()?;
        seen = seen.checked_add(1)?;
        if seen > MAX_PROC_ENTRIES {
            return None;
        }
        let name = entry.file_name();
        if name
            .to_str()
            .is_none_or(|value| !value.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let status = super::read_bounded(&entry.path().join("status"))?;
        let (mut real_uid, mut thread_count) = (None, None);
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("Uid:") {
                real_uid = value.split_whitespace().next()?.parse::<u32>().ok();
            } else if let Some(value) = line.strip_prefix("Threads:") {
                thread_count = Some(parse_decimal(value)?);
            }
        }
        let real_uid = real_uid?;
        let thread_count = thread_count?;
        if real_uid == uid {
            tasks = tasks.checked_add(thread_count)?;
        }
    }
    Some(tasks)
}

#[cfg(target_os = "linux")]
fn global_task_count() -> Option<u64> {
    let loadavg = super::read_bounded(Path::new("/proc/loadavg"))?;
    let total = loadavg.split_whitespace().nth(3)?.split_once('/')?.1;
    parse_decimal(total)
}

#[cfg(target_os = "linux")]
fn rlimit_nproc_limit() -> Option<Option<u64>> {
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: getrlimit initializes the documented rlimit structure.
    if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) } != 0 {
        return None;
    }
    if limit.rlim_cur == libc::RLIM_INFINITY {
        return Some(None);
    }
    Some(Some(limit.rlim_cur))
}

#[cfg(target_os = "linux")]
fn read_decimal(path: &Path) -> Option<u64> {
    parse_decimal(&super::read_bounded(path)?)
}

#[cfg(target_os = "linux")]
fn parse_decimal(value: &str) -> Option<u64> {
    let value = value.trim();
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u64>().ok())?
}

#[cfg(target_os = "linux")]
fn parse_limit(value: &str) -> Option<Option<u64>> {
    let value = value.trim();
    if value == "max" {
        Some(None)
    } else {
        parse_decimal(value).map(Some)
    }
}

#[cfg(target_os = "linux")]
fn filesystem_headroom(path: &Path) -> Metric<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return Metric::Unknown(UnknownReason::InvalidCounter);
    };
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: statvfs initializes the structure on success; the path is NUL-terminated.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Metric::Unknown(UnknownReason::Inaccessible);
    }
    let stats = unsafe { stats.assume_init() };
    match u64::try_from(stats.f_bavail)
        .ok()
        .and_then(|blocks| blocks.checked_mul(u64::try_from(stats.f_frsize).ok()?))
    {
        Some(bytes) => Metric::Known(bytes),
        None => Metric::Unknown(UnknownReason::InvalidCounter),
    }
}
