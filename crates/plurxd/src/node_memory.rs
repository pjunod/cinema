//! How much memory this node may use: the one place plurxd reads it.
//!
//! A container's limit is the node's memory, not the host's. On Linux the
//! cgroup limit (v2 `memory.max`, else v1 `memory.limit_in_bytes`) is taken
//! when it is set and below the host total from `/proc/meminfo`; a NUC
//! container limited to 20 GiB on a 64 GiB host has 20 GiB. macOS reports
//! `hw.memsize`. Anywhere else, and whenever a read fails, the answer is
//! unknown and callers choose their own conservative default.

/// This node's memory in bytes, or `None` when it cannot be read.
pub(crate) fn node_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let host = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|meminfo| meminfo_total_bytes(&meminfo));
        let cgroup = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .ok()
            .and_then(|limit| cgroup_limit_bytes(&limit))
            .or_else(|| {
                std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
                    .ok()
                    .and_then(|limit| cgroup_limit_bytes(&limit))
            });
        effective(host, cgroup)
    }
    #[cfg(target_os = "macos")]
    {
        let mut bytes: u64 = 0;
        let mut size = std::mem::size_of::<u64>();
        // SAFETY: hw.memsize is a u64; the buffer and its size describe one.
        let status = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&mut bytes as *mut u64).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (status == 0 && size == std::mem::size_of::<u64>() && bytes > 0).then_some(bytes)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The smaller of the host total and a set cgroup limit.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn effective(host: Option<u64>, cgroup: Option<u64>) -> Option<u64> {
    match (host, cgroup) {
        (Some(host), Some(limit)) => Some(host.min(limit)),
        (host, limit) => host.or(limit),
    }
}

/// `MemTotal` from `/proc/meminfo`, which reports kibibytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        let kib = rest.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()?;
        kib.checked_mul(1024).filter(|bytes| *bytes > 0)
    })
}

/// A cgroup memory limit, or `None` when unlimited. v2 writes `max`; v1
/// writes a page-rounded `i64::MAX` when no limit is set.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_limit_bytes(limit: &str) -> Option<u64> {
    let limit = limit.trim();
    if limit == "max" {
        return None;
    }
    let bytes = limit.parse::<u64>().ok()?;
    (bytes > 0 && bytes < (1u64 << 62)).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_container_limit_below_the_host_is_the_nodes_memory() {
        let meminfo = "MemTotal:       65536000 kB\nMemFree:  1 kB\n";
        let host = meminfo_total_bytes(meminfo);
        assert_eq!(host, Some(65_536_000 * 1024));
        let limit = cgroup_limit_bytes("21474836480\n");
        assert_eq!(effective(host, limit), Some(21_474_836_480));
        // An unlimited cgroup, v2 or v1, leaves the host total.
        assert_eq!(cgroup_limit_bytes("max\n"), None);
        assert_eq!(cgroup_limit_bytes("9223372036854771712\n"), None);
        assert_eq!(effective(host, None), host);
        // A limit above the host (a lazy container default) changes nothing.
        assert_eq!(
            effective(Some(8 << 30), cgroup_limit_bytes("68719476736")),
            Some(8 << 30)
        );
        assert_eq!(meminfo_total_bytes("MemFree: 3 kB\n"), None);
        assert_eq!(cgroup_limit_bytes("garbage"), None);
    }

    #[test]
    fn this_node_reports_its_memory() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(node_memory_bytes().is_some_and(|bytes| bytes >= 256 << 20));
        }
    }
}
