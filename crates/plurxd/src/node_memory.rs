//! How much memory this node may use: the one place plurxd reads it.
//!
//! A container's limit is the node's memory, not the host's. On Linux the
//! lowest limit on this process's own cgroup and its ancestors is taken (v2
//! `memory.max` and `memory.high`, else v1 `memory.limit_in_bytes`), when it
//! is below the host total from `/proc/meminfo`: a NUC container limited to
//! 20 GiB on a 64 GiB host has 20 GiB, and so does a systemd service with
//! `MemoryMax=20G` in a cgroup below the root. macOS reports
//! `hw.memsize`. Anywhere else, and whenever a read fails, the answer is
//! unknown and callers choose their own conservative default.

/// This node's memory in bytes, or `None` when it cannot be read.
pub(crate) fn node_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let host = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|meminfo| meminfo_total_bytes(&meminfo));
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        let (v2, v1) = own_cgroups(&own);
        let read = |path: std::path::PathBuf| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|limit| cgroup_limit_bytes(&limit))
        };
        let lowest = |root: &str, relative: &str, files: &[&str]| {
            ancestors(root, relative)
                .flat_map(|dir| files.iter().map(move |file| dir.join(file)))
                .filter_map(read)
                .min()
        };
        let cgroup = lowest(
            "/sys/fs/cgroup",
            v2.unwrap_or("/"),
            &["memory.max", "memory.high"],
        )
        .or_else(|| {
            lowest(
                "/sys/fs/cgroup/memory",
                v1.unwrap_or("/"),
                &["memory.limit_in_bytes"],
            )
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

/// This process's own cgroup paths from `/proc/self/cgroup`: the v2 unified
/// one (`0::/path`) and the v1 one carrying the memory controller.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn own_cgroups(own: &str) -> (Option<&str>, Option<&str>) {
    let mut v2 = None;
    let mut v1 = None;
    for line in own.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if id == "0" && controllers.is_empty() {
            v2 = Some(path);
        } else if controllers
            .split(',')
            .any(|controller| controller == "memory")
        {
            v1 = Some(path);
        }
    }
    (v2, v1)
}

/// The cgroup directory of `relative` under `root`, then each ancestor up to
/// `root`. A path that leaves the hierarchy yields only `root`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ancestors<'a>(
    root: &'a str,
    relative: &'a str,
) -> impl Iterator<Item = std::path::PathBuf> + 'a {
    let parts: Vec<&str> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let escaping = parts.contains(&"..");
    let depth = if escaping { 0 } else { parts.len() };
    (0..=depth).rev().map(move |keep| {
        let mut dir = std::path::PathBuf::from(root);
        for part in &parts[..keep] {
            dir.push(part);
        }
        dir
    })
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
    fn the_process_own_cgroup_and_its_ancestors_are_read() {
        let own = "12:memory:/docker/abc\n0::/system.slice/plurxd.service\n";
        assert_eq!(
            own_cgroups(own),
            (Some("/system.slice/plurxd.service"), Some("/docker/abc"))
        );
        let dirs: Vec<_> = ancestors("/sys/fs/cgroup", "/system.slice/plurxd.service").collect();
        assert_eq!(
            dirs,
            [
                "/sys/fs/cgroup/system.slice/plurxd.service",
                "/sys/fs/cgroup/system.slice",
                "/sys/fs/cgroup"
            ]
            .map(std::path::PathBuf::from)
        );
        assert_eq!(
            ancestors("/sys/fs/cgroup", "/../etc").collect::<Vec<_>>(),
            [std::path::PathBuf::from("/sys/fs/cgroup")]
        );
        assert_eq!(own_cgroups("garbage\n"), (None, None));
    }

    #[test]
    fn this_node_reports_its_memory() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(node_memory_bytes().is_some_and(|bytes| bytes >= 256 << 20));
        }
    }
}
