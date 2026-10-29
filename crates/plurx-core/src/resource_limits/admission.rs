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
    // CPU/RAM/NOFILE may be cached. Preserve their precise oldest sample time
    // even though FD/task/filesystem headroom below is sampled for this call.
    let sample = super::snapshot_with_sample();
    let measured_at = sample.at;
    let limits = sample.value;
    #[cfg(not(target_os = "linux"))]
    let _ = output_root;
    #[cfg(target_os = "linux")]
    let (handles, kernel_tasks) = (
        linux_handle_headroom(limits.open_file_ceiling),
        linux_task_headroom(sample.view.as_ref()),
    );
    #[cfg(not(target_os = "linux"))]
    let (handles, kernel_tasks) = (
        Metric::Unknown(UnknownReason::Unsupported),
        Metric::Unknown(UnknownReason::Unsupported),
    );

    #[cfg(target_os = "linux")]
    let scratch_bytes = filesystem_headroom(output_root);
    #[cfg(not(target_os = "linux"))]
    let scratch_bytes = Metric::Unknown(UnknownReason::Unsupported);

    #[cfg(target_os = "linux")]
    let context_unchanged = sample
        .view
        .as_ref()
        .is_some_and(super::LinuxView::matches_current);
    #[cfg(not(target_os = "linux"))]
    let context_unchanged = true;
    let context_bound = |value| {
        if context_unchanged {
            value
        } else {
            Metric::Unknown(UnknownReason::IncompleteTree)
        }
    };

    AdmissionObservation {
        measured_at,
        capacity: ResourceCapacity {
            cpu_ns_per_second: context_bound(limits.cpu_ns_per_second),
            memory_bytes: context_bound(limits.memory_ceiling_bytes),
            // Preserve one coherent observation vector. In particular a
            // changed mount view cannot leave old scratch headroom usable.
            handles: context_bound(handles),
            // Every root process requires at least one Linux kernel task, so
            // observed task headroom is a conservative process-count ceiling.
            processes: context_bound(kernel_tasks),
            kernel_tasks: context_bound(kernel_tasks),
            hardware_slots: Metric::Unknown(UnknownReason::InsufficientEvidence),
            tuner_slots: Metric::Unknown(UnknownReason::InsufficientEvidence),
            scratch_bytes: context_bound(scratch_bytes),
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
fn linux_task_headroom(view: Option<&super::LinuxView>) -> Metric<u64> {
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

    let Some(view) = view else {
        return Metric::Unknown(UnknownReason::IncompleteTree);
    };
    if !view.matches_current() {
        return Metric::Unknown(UnknownReason::IncompleteTree);
    }
    available = match hierarchy_task_headroom(&view.root, &view.leaf, available, |path| {
        view.native.read_controller(path)
    }) {
        Ok(value) => value,
        Err(reason) => return Metric::Unknown(reason),
    };

    // Namespace UID 0 is not proof of the initial-user NPROC exemption.
    // A private PID namespace also cannot census all tasks of a real UID.
    // Kernel loadavg reports global nr_threads, an upper bound on that UID's
    // tasks. Subtracting it is conservative and never invents spare tasks.
    match rlimit_nproc_limit() {
        Some(Some(limit)) => {
            let Some(headroom) = conservative_nproc_headroom(limit, global_tasks) else {
                return Metric::Unknown(UnknownReason::IncompleteTree);
            };
            available = available.min(headroom);
        }
        Some(None) => {}
        None => return Metric::Unknown(UnknownReason::Inaccessible),
    }
    if !view.matches_current() {
        return Metric::Unknown(UnknownReason::IncompleteTree);
    }
    Metric::Known(available)
}

#[cfg(target_os = "linux")]
fn hierarchy_task_headroom(
    root: &Path,
    leaf: &Path,
    mut available: u64,
    mut read: impl FnMut(&Path) -> Option<String>,
) -> Result<u64, UnknownReason> {
    leaf.strip_prefix(root)
        .map_err(|_| UnknownReason::IncompleteTree)?;
    let mut current = leaf;
    let mut ancestors = 1_u8;
    while current != root {
        ancestors = ancestors
            .checked_add(1)
            .filter(|count| *count <= 64)
            .ok_or(UnknownReason::IncompleteTree)?;
        let limit = read(&current.join("pids.max")).ok_or(UnknownReason::Inaccessible)?;
        let used = read(&current.join("pids.current")).ok_or(UnknownReason::Inaccessible)?;
        let used = parse_decimal(&used).ok_or(UnknownReason::InvalidCounter)?;
        if let Some(limit) = parse_limit(&limit).ok_or(UnknownReason::InvalidCounter)? {
            available = available.min(
                limit
                    .checked_sub(used)
                    .ok_or(UnknownReason::InvalidCounter)?,
            );
        }
        current = current.parent().ok_or(UnknownReason::IncompleteTree)?;
    }
    Ok(available)
}

#[cfg(target_os = "linux")]
fn conservative_nproc_headroom(limit: u64, global_tasks: u64) -> Option<u64> {
    if global_tasks == 0 {
        return None;
    }
    limit
        .checked_sub(global_tasks)
        .filter(|headroom| *headroom > 0)
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
// libc filesystem counters vary by Linux ABI. Keep checked conversion so
// an unrepresentable counter remains Unknown instead of narrowing silently.
#[allow(clippy::useless_conversion)]
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

#[cfg(all(test, target_os = "linux"))]
mod hierarchy_tests {
    use super::*;

    #[test]
    fn task_headroom_keeps_parent_limit_and_never_reads_true_root_interfaces() {
        let values = [
            ("/h/team/job/pids.max", "100"),
            ("/h/team/job/pids.current", "5"),
            ("/h/team/pids.max", "20"),
            ("/h/team/pids.current", "7"),
        ];
        let result =
            hierarchy_task_headroom(Path::new("/h"), Path::new("/h/team/job"), 1000, |path| {
                assert_ne!(path.parent(), Some(Path::new("/h")));
                values
                    .iter()
                    .find(|(name, _)| path == Path::new(name))
                    .map(|(_, value)| (*value).into())
            });
        assert_eq!(result, Ok(13));
    }

    #[test]
    fn missing_or_malformed_nonroot_task_interface_stays_unknown() {
        for missing in ["pids.max", "pids.current"] {
            assert_eq!(
                hierarchy_task_headroom(Path::new("/h"), Path::new("/h/job"), 1000, |path| {
                    match path.file_name()?.to_str()? {
                        name if name == missing => None,
                        "pids.max" => Some("100".into()),
                        "pids.current" => Some("5".into()),
                        _ => None,
                    }
                }),
                Err(UnknownReason::Inaccessible)
            );
        }
        assert_eq!(
            hierarchy_task_headroom(Path::new("/h"), Path::new("/h/job"), 1000, |path| Some(
                if path.ends_with("pids.max") {
                    "invalid"
                } else {
                    "5"
                }
                .into()
            )),
            Err(UnknownReason::InvalidCounter)
        );
    }

    #[test]
    fn finite_nproc_uses_global_task_upper_bound_for_every_uid() {
        assert_eq!(conservative_nproc_headroom(100, 80), Some(20));
        assert_eq!(conservative_nproc_headroom(100, 100), None);
        assert_eq!(conservative_nproc_headroom(100, 101), None);
        assert_eq!(conservative_nproc_headroom(100, 0), None);
        // Hidden peer UID tasks cannot turn a partial PID view into headroom.
        assert!(
            conservative_nproc_headroom(100, 80).expect("positive conservative headroom") < 100 - 5
        );
    }
}
