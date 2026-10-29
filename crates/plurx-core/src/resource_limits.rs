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

// Retain the same native context until every admission-vector dimension has
// been sampled. It is only an observation owner, never an admission ticket.
struct SampledLimits {
    at: Instant,
    value: EffectiveLimits,
    #[cfg(target_os = "linux")]
    view: Option<LinuxView>,
}

struct CachedLimits {
    at: Instant,
    value: EffectiveLimits,
    #[cfg(target_os = "linux")]
    view: Option<CgroupCacheKey>,
}

static CACHE: LazyLock<Mutex<Option<CachedLimits>>> = LazyLock::new(Mutex::default);

/// Controller values are sampled at most once per second for the same native
/// view and calling-thread width. A cache hit still verifies the actual current
/// root, leaf and leader/thread membership; another worker cannot reuse a wider
/// or differently contained caller's observation. No Store access or mutation.
pub fn snapshot() -> EffectiveLimits {
    snapshot_with_sample().value
}

fn available_threads() -> Metric<u64> {
    std::thread::available_parallelism()
        .ok()
        .and_then(|value| u64::try_from(value.get()).ok())
        .map(Metric::Known)
        .unwrap_or(Metric::Unknown(UnknownReason::Inaccessible))
}

// Keep the precise original sample time across cache hits. Admission must use
// the oldest sampled component, not the time a poll revalidated its identity.
fn snapshot_with_sample() -> SampledLimits {
    let sampled_at = Instant::now();
    let threads = available_threads();
    #[cfg(target_os = "linux")]
    let view = LinuxView::capture(threads);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    if let Some(cached) = cache.as_ref() {
        let age = now.saturating_duration_since(cached.at);
        #[cfg(target_os = "linux")]
        let same_view = view.as_ref().is_some_and(|current| {
            cached.view.as_ref() == Some(&current.key) && current.matches_current()
        });
        #[cfg(not(target_os = "linux"))]
        let same_view = cached.value.available_threads == threads;
        if age < Duration::from_secs(1) && same_view {
            let mut value = cached.value;
            value.age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
            return SampledLimits {
                at: cached.at,
                value,
                #[cfg(target_os = "linux")]
                view,
            };
        }
    }
    #[cfg(target_os = "linux")]
    let (cpu, memory) = containment(threads, view.as_ref());
    #[cfg(not(target_os = "linux"))]
    let (cpu, memory) = containment(threads);
    let value = EffectiveLimits {
        available_threads: threads,
        cpu_ns_per_second: cpu,
        memory_ceiling_bytes: memory,
        open_file_ceiling: file_limit(),
        age_ms: 0,
    };
    *cache = Some(CachedLimits {
        at: sampled_at,
        value,
        #[cfg(target_os = "linux")]
        view: view.as_ref().map(|current| current.key.clone()),
    });
    SampledLimits {
        at: sampled_at,
        value,
        #[cfg(target_os = "linux")]
        view,
    }
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
fn containment(threads: Metric<u64>, view: Option<&LinuxView>) -> (Metric<u64>, Metric<u64>) {
    let unknown = Metric::Unknown(UnknownReason::Inaccessible);
    let result = (|| {
        let view = view?;
        if !view.matches_current() {
            return None;
        }
        let threads = match threads {
            Metric::Known(value) => value,
            _ => return None,
        };
        let (cpu, memory) = hierarchy_limits(&view.root, &view.leaf, threads, |path| {
            view.native.read_controller(path)
        })?;
        if !view.matches_current() {
            return None;
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

// Cache identity is observed only from retained native descriptors and the
// coherent current leader/thread proc view. No caller-supplied proof bit or
// pathname-only parser can create a reusable positive capacity certificate.
#[cfg(target_os = "linux")]
#[derive(Clone, PartialEq, Eq)]
struct CgroupCacheKey {
    cgroups: String,
    mounts: String,
    root_device: u64,
    root_inode: u64,
    leaf_device: u64,
    leaf_inode: u64,
    available_threads: Metric<u64>,
}

#[cfg(target_os = "linux")]
struct LinuxView {
    root: std::path::PathBuf,
    leaf: std::path::PathBuf,
    native: UnifiedRoot,
    key: CgroupCacheKey,
}

#[cfg(target_os = "linux")]
impl LinuxView {
    fn capture(available_threads: Metric<u64>) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let (cgroups, mounts) = current_cgroup_view()?;
        let (root, leaf) = unified_path(&cgroups, &mounts)?;
        // Mountinfo's '/' is namespace-relative. Only an actual root-only
        // kernel interface establishes the complete hierarchy boundary.
        let native = verified_unified_root(&root, &leaf)?;
        if !native.matches_view(&root, &leaf, &cgroups, &mounts) {
            return None;
        }
        let root_metadata = native.directory.metadata().ok()?;
        let leaf_metadata = native.leaf.metadata().ok()?;
        let key = CgroupCacheKey {
            cgroups,
            mounts,
            root_device: root_metadata.dev(),
            root_inode: root_metadata.ino(),
            leaf_device: leaf_metadata.dev(),
            leaf_inode: leaf_metadata.ino(),
            available_threads,
        };
        Some(Self {
            root,
            leaf,
            native,
            key,
        })
    }

    fn matches_current(&self) -> bool {
        self.key.available_threads == available_threads()
            && self
                .native
                .matches_view(&self.root, &self.leaf, &self.key.cgroups, &self.key.mounts)
    }
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

/// Retained native identity of the TRUE cgroup2 hierarchy root. This is a
/// short-lived observation owner, never a reservation or native-close witness.
#[cfg(target_os = "linux")]
struct UnifiedRoot {
    directory: std::fs::File,
    _root_only_interface: std::fs::File,
    leaf: std::fs::File,
}

#[cfg(target_os = "linux")]
impl UnifiedRoot {
    fn read_controller(&self, path: &std::path::Path) -> Option<String> {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        // Every ancestor sample must come from this same kernel hierarchy,
        // not an ordinary file or an overmounted controller pathname.
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path.parent()?)
            .ok()?;
        let metadata = directory.metadata().ok()?;
        if !metadata.is_dir() || metadata.dev() != self.directory.metadata().ok()?.dev() {
            return None;
        }
        let name: &[u8] = match path.file_name()?.to_str()? {
            "cpu.max" => b"cpu.max\0",
            "memory.max" => b"memory.max\0",
            "pids.max" => b"pids.max\0",
            "pids.current" => b"pids.current\0",
            _ => return None,
        };
        read_interface_at(&directory, name)
    }

    fn matches_view(
        &self,
        root: &std::path::Path,
        leaf: &std::path::Path,
        cgroups: &str,
        mounts: &str,
    ) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        let (Ok(actual), Ok(named), Ok(actual_leaf), Ok(named_leaf)) = (
            self.directory.metadata(),
            std::fs::symlink_metadata(root),
            self.leaf.metadata(),
            std::fs::symlink_metadata(leaf),
        ) else {
            return false;
        };
        if !named.is_dir()
            || !named_leaf.is_dir()
            || actual.dev() != named.dev()
            || actual.ino() != named.ino()
            || actual_leaf.dev() != named_leaf.dev()
            || actual_leaf.ino() != named_leaf.ino()
            || actual_leaf.dev() != actual.dev()
        {
            return false;
        }
        if actual_leaf.ino() != actual.ino()
            && read_interface_at(&self.leaf, b"cgroup.type\0")
                .is_none_or(|value| value.trim() != "domain")
        {
            return false;
        } // Unsupported threaded groups remain Unknown.
          // Verify BOTH actual leader and calling-thread membership. The sample
          // runs in Tokio/blocking workers; a narrower worker must not inherit a
          // leader-only capacity certificate. A private namespace/global-bind
          // mismatch also fails this positive kernel membership test.
        let Some(processes) = read_interface_at(&self.leaf, b"cgroup.procs\0") else {
            return false;
        };
        let Some(threads) = read_interface_at(&self.leaf, b"cgroup.threads\0") else {
            return false;
        };
        // SAFETY: gettid returns the current calling thread's kernel TID.
        let Ok(tid) = u32::try_from(unsafe { libc::syscall(libc::SYS_gettid) }) else {
            return false;
        };
        member_present(&processes, std::process::id())
            && member_present(&threads, tid)
            && current_cgroup_view().is_some_and(|(now_cgroups, now_mounts)| {
                now_cgroups == cgroups && now_mounts == mounts
            })
    }
}

#[cfg(target_os = "linux")]
fn current_cgroup_view() -> Option<(String, String)> {
    let cgroups = read_bounded(std::path::Path::new("/proc/self/cgroup"))?;
    let mounts = read_bounded(std::path::Path::new("/proc/self/mountinfo"))?;
    if read_bounded(std::path::Path::new("/proc/thread-self/cgroup"))? != cgroups
        || read_bounded(std::path::Path::new("/proc/thread-self/mountinfo"))? != mounts
    {
        return None;
    }
    Some((cgroups, mounts))
}

#[cfg(target_os = "linux")]
fn read_interface_at(directory: &std::fs::File, name: &[u8]) -> Option<String> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::fs::MetadataExt as _;
    let name = std::ffi::CStr::from_bytes_with_nul(name).ok()?;
    // SAFETY: a validated NUL-terminated component is opened relative to the
    // same retained directory. Symlinks and pathname fallbacks are refused.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: successful openat returned a new uniquely owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.dev() != directory.metadata().ok()?.dev() {
        return None;
    }
    let mut value = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut value).ok()?;
    (value.len() <= 64 * 1024).then_some(value)
}

#[cfg(any(target_os = "linux", test))]
fn member_present(document: &str, id: u32) -> bool {
    let mut found = false;
    for line in document.lines() {
        let value = line.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let Ok(member) = value.parse::<u32>() else {
            return false;
        };
        found |= member == id;
    }
    found
}

#[cfg(target_os = "linux")]
fn verified_unified_root(root: &std::path::Path, leaf: &std::path::Path) -> Option<UnifiedRoot> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .ok()?;
    let metadata = directory.metadata().ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the actual owned directory stays live; fstatfs initializes stats.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    if stats.f_type != libc::CGROUP2_SUPER_MAGIC {
        return None;
    }
    // Kernel cgroup-v2 ABI: cpuset.cpus.isolated exists ONLY at the true root.
    // A non-init cgroup namespace may call its subtree '/', but cannot create
    // this root-only kernfs interface. Absence/unsupported kernels stay Unknown.
    // SAFETY: a static NUL-terminated component is opened relative to the SAME
    // owned cgroup2 directory; no pathname fallback or symlink adoption exists.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cpuset.cpus.isolated".as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: openat returned a new, uniquely owned descriptor.
    let marker = unsafe { std::fs::File::from_raw_fd(fd) };
    let marker_metadata = marker.metadata().ok()?;
    if !metadata.is_dir() || !marker_metadata.is_file() || metadata.dev() != marker_metadata.dev() {
        return None;
    }
    let leaf = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(leaf)
        .ok()?;
    let leaf_metadata = leaf.metadata().ok()?;
    if !leaf_metadata.is_dir() || leaf_metadata.dev() != metadata.dev() {
        return None;
    }
    Some(UnifiedRoot {
        directory,
        _root_only_interface: marker,
        leaf,
    })
}

/// Fold controller data only. The caller must first retain a verified true
/// root and must revalidate its native identity and current membership after
/// this fold. Unit data fixtures cannot publish capacity or fabricate that proof.
#[cfg(any(target_os = "linux", test))]
fn hierarchy_limits(
    root: &std::path::Path,
    leaf: &std::path::Path,
    threads: u64,
    mut read: impl FnMut(&std::path::Path) -> Option<String>,
) -> Option<(u64, Option<u64>)> {
    leaf.strip_prefix(root).ok()?;
    let mut cpu = threads.checked_mul(1_000_000_000)?;
    let mut memory = None;
    let mut current = leaf;
    let mut ancestors = 1_u8; // Includes the independently verified root.
    while current != root {
        ancestors = ancestors.checked_add(1)?;
        if ancestors > 64 {
            return None;
        }
        // All NON-ROOT ancestors remain mandatory; leaf 'max' cannot hide a
        // stricter parent. The true root has no controller max interfaces.
        if let Some(quota) = parse_cpu_max(&read(&current.join("cpu.max"))?)? {
            cpu = cpu.min(quota);
        }
        if let Some(limit) = parse_memory_max(&read(&current.join("memory.max"))?)? {
            memory = Some(memory.map_or(limit, |old: u64| old.min(limit)));
        }
        current = current.parent()?;
    }
    Some((cpu, memory))
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
    // Text alone cannot distinguish a true-root member from a namespace's
    // synthetic '/'. Production callers additionally verify the actual root.
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
        // Reject aliases of this member's actual ancestry/interfaces. An
        // unrelated sibling (for example a legacy net_cls mount) cannot hide
        // this unified member's constraints.
        if !hierarchy_mount_is_unshadowed(mount, &leaf, mounts)? {
            continue;
        }
        return Some((mount.to_owned(), leaf));
    }
    None
}

#[cfg(any(target_os = "linux", test))]
fn hierarchy_mount_is_unshadowed(
    root: &std::path::Path,
    leaf: &std::path::Path,
    mounts: &str,
) -> Option<bool> {
    use std::path::{Component, Path};
    leaf.strip_prefix(root).ok()?;
    for line in mounts.lines() {
        let (before, _) = line.split_once(" - ")?;
        let mount = Path::new(before.split_whitespace().nth(4)?);
        if !mount.is_absolute()
            || mount
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return None;
        }
        if mount == root || !mount.starts_with(root) {
            continue;
        }
        if leaf.starts_with(mount) {
            return Some(false);
        }
        let parent = mount.parent()?;
        let name = mount.file_name()?.to_str()?;
        let root_marker = parent == root && name == "cpuset.cpus.isolated";
        let membership = parent == leaf
            && (matches!(name, "cgroup.procs" | "cgroup.threads")
                || (leaf != root && name == "cgroup.type"));
        let controller = parent != root
            && parent.starts_with(root)
            && leaf.starts_with(parent)
            && matches!(name, "cpu.max" | "memory.max" | "pids.max" | "pids.current");
        if root_marker || membership || controller {
            return Some(false);
        }
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "linux", feature = "fixtures"))]
    static CACHE_FIXTURE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn nested_cgroup_directory_and_controller_binds_do_not_certify_complete_hierarchy() {
        let root = "41 25 0:32 / /sys/fs/cgroup ro - cgroup2 cgroup ro";
        for mount in [
            "/sys/fs/cgroup/team",
            "/sys/fs/cgroup/team/job",
            "/sys/fs/cgroup/team/cpu.max",
            "/sys/fs/cgroup/team/job/pids.current",
            "/sys/fs/cgroup/team/job/cgroup.procs",
            "/sys/fs/cgroup/team/job/cgroup.threads",
            "/sys/fs/cgroup/team/job/cgroup.type",
            "/sys/fs/cgroup/cpuset.cpus.isolated",
        ] {
            let views = format!("{root}\n42 41 0:32 /other {mount} ro - cgroup2 cgroup ro");
            assert!(unified_path("0::/team/job", &views).is_none());
        }
        let sibling = format!("{root}\n42 25 0:42 / /sys/fs/cgroup-backup ro - tmpfs tmpfs ro");
        assert!(unified_path("0::/team/job", &sibling).is_some());
    }

    #[test]
    fn unrelated_legacy_controller_mount_does_not_hide_unified_member_limits() {
        let mounts = "41 25 0:32 / /sys/fs/cgroup ro - cgroup2 cgroup2 rw\n42 41 0:77 / /sys/fs/cgroup/net_cls ro - cgroup net_cls rw,net_cls";
        let (root, leaf) = unified_path("0::/system.slice/job\n", mounts)
            .expect("unrelated legacy controller does not shadow this member");
        assert_eq!(root, std::path::Path::new("/sys/fs/cgroup"));
        assert_eq!(leaf, root.join("system.slice/job"));
        // Parsing paths supplies no native root or membership authority.
    }

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

    #[test]
    fn complete_controller_fold_stops_before_verified_root_and_keeps_stricter_parent() {
        use std::path::Path;
        let root = Path::new("/hierarchy");
        let values = [
            ("/hierarchy/team/job/cpu.max", "250000 100000"),
            ("/hierarchy/team/job/memory.max", "15032385536"),
            ("/hierarchy/team/cpu.max", "100000 100000"),
            ("/hierarchy/team/memory.max", "10737418240"),
        ];
        let result = hierarchy_limits(root, Path::new("/hierarchy/team/job"), 8, |path| {
            assert_ne!(path.parent(), Some(root), "root has no max interfaces");
            values
                .iter()
                .find(|(name, _)| path == Path::new(name))
                .map(|(_, text)| (*text).into())
        });
        assert_eq!(result, Some((1_000_000_000, Some(10_737_418_240))));
    }

    #[test]
    fn missing_nonroot_controller_stays_unknown_even_with_other_finite_limits() {
        use std::path::Path;
        for missing in ["cpu.max", "memory.max"] {
            assert!(
                hierarchy_limits(Path::new("/h"), Path::new("/h/job"), 8, |path| {
                    match path.file_name()?.to_str()? {
                        name if name == missing => None,
                        "cpu.max" => Some("200000 100000".into()),
                        "memory.max" => Some("15032385536".into()),
                        _ => None,
                    }
                })
                .is_none()
            );
        }
    }

    #[test]
    fn true_root_member_text_does_not_itself_authorize_capacity() {
        let mounts = "41 25 0:32 / /sys/fs/cgroup ro - cgroup2 cgroup ro";
        let (root, leaf) = unified_path("0::/\n", mounts).expect("text selection only");
        assert_eq!(root, leaf);
        // No verified native root is constructed by this parser/data test.
        assert_eq!(
            hierarchy_limits(&root, &leaf, 2, |_| panic!("no nonroot interface")),
            Some((2_000_000_000, None))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ordinary_directory_or_missing_kernel_root_marker_is_not_cgroup_root_proof() {
        let directory = tempfile::tempdir().expect("owned capacity fixture directory");
        assert!(
            verified_unified_root(directory.path(), directory.path()).is_none(),
            "a pathname or absent controller files cannot manufacture kernel root identity"
        );
    }

    #[test]
    fn membership_data_requires_the_actual_id_and_rejects_malformed_rows() {
        assert!(member_present("12\n34\n", 34));
        assert!(!member_present("12\n34\n", 35));
        assert!(!member_present("34\ninvalid\n", 34));
        assert!(!member_present("", 34));
    }

    /// Controlled native fixture: either a private namespace root or the
    /// actual non-root container member under a visible full hierarchy.
    /// It never creates a marker or fabricates a positive root identity.
    #[cfg(all(target_os = "linux", feature = "fixtures"))]
    #[test]
    fn actual_nonroot_cgroup_cannot_supply_kernel_root_authority() {
        let (cgroups, mounts) = current_cgroup_view().expect("actual coherent caller view");
        let (root, leaf) = unified_path(&cgroups, &mounts).expect("actual unified mount");
        assert!(
            root != leaf || std::fs::symlink_metadata(root.join("cpuset.cpus.isolated")).is_err(),
            "fixture requires an actual non-root container cgroup, not a true-root member"
        );
        assert!(
            verified_unified_root(&leaf, &leaf).is_none(),
            "a real cgroup2 subtree/private namespace root is not global-root proof"
        );
    }

    /// Actual controlled finite-container observation, not an injected cap.
    #[cfg(all(target_os = "linux", feature = "fixtures"))]
    #[test]
    fn actual_complete_hierarchy_reports_finite_cpu_memory_and_task_evidence() {
        let limits = snapshot();
        assert!(
            matches!(limits.cpu_ns_per_second, Metric::Known(value) if value > 0),
            "actual containment CPU remains unknown: {limits:?}"
        );
        assert!(
            matches!(limits.memory_ceiling_bytes, Metric::Known(value) if value > 0),
            "actual containment memory remains unknown: {limits:?}"
        );
        let root = tempfile::tempdir().expect("owned capacity fixture directory");
        let observed = super::admission::snapshot(root.path());
        assert!(
            matches!(observed.capacity.processes, Metric::Known(value) if value > 0),
            "actual task headroom remains unknown: {observed:?}"
        );
        assert!(
            matches!(observed.capacity.kernel_tasks, Metric::Known(value) if value > 0),
            "actual kernel-task evidence remains unknown: {observed:?}"
        );
    }

    /// Controlled finite Linux runner: the child changes only its own TID's
    /// affinity, then exits. It never mutates a cgroup or the parent's affinity.
    /// Both observations use actual native view verification and controller IO.
    #[cfg(all(target_os = "linux", feature = "fixtures"))]
    #[test]
    fn actual_narrower_thread_cannot_reuse_parent_capacity_cache() {
        let _cache_fixture = CACHE_FIXTURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fn affinity() -> libc::cpu_set_t {
            // SAFETY: sched_getaffinity initializes this caller-owned set for
            // pid zero, which denotes the calling thread.
            let mut set = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
            assert_eq!(
                unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set) },
                0,
                "actual calling-thread affinity must be readable"
            );
            set
        }
        fn members(set: &libc::cpu_set_t) -> Vec<usize> {
            (0..libc::CPU_SETSIZE as usize)
                // SAFETY: each index is below CPU_SETSIZE, and set is initialized.
                .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, set) })
                .collect()
        }
        let parent_affinity = members(&affinity());
        assert!(
            parent_affinity.len() >= 2,
            "controlled runner needs two actual eligible CPUs, got {parent_affinity:?}"
        );
        // Discard only an older observation. No injected cap/native authority.
        *CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let started = Instant::now();
        let parent = snapshot();
        assert!(
            matches!(parent.available_threads, Metric::Known(value) if value >= 2),
            "controlled runner needs an actual parent width >=2: {parent:?}"
        );
        assert!(
            matches!(parent.cpu_ns_per_second, Metric::Known(value) if value > 1_000_000_000),
            "controlled runner needs an actual CPU ceiling above one core: {parent:?}"
        );
        let cpu = parent_affinity[0];
        let child = std::thread::spawn(move || {
            // SAFETY: zeroed set is initialized by CPU_ZERO/CPU_SET; the chosen
            // CPU came from the actual eligible set. PID zero changes only this
            // new thread, which is joined below; no external thread is changed.
            let mut narrowed = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
            unsafe {
                libc::CPU_ZERO(&mut narrowed);
                libc::CPU_SET(cpu, &mut narrowed);
            }
            assert_eq!(
                unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&narrowed), &narrowed) },
                0,
                "narrow only the owned fixture thread"
            );
            assert_eq!(members(&affinity()), vec![cpu]);
            snapshot()
        })
        .join()
        .expect("owned narrowed thread completed");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "controlled fixture must exercise the live one-second cache window"
        );
        assert_eq!(child.available_threads, Metric::Known(1), "{child:?}");
        assert!(matches!(child.cpu_ns_per_second, Metric::Known(value) if value > 0 && value <= 1_000_000_000),
            "same native hierarchy with narrower calling-thread width: parent={parent:?}, child={child:?}");
        assert_eq!(
            members(&affinity()),
            parent_affinity,
            "joining the owned thread leaves parent affinity unchanged"
        );
    }

    #[cfg(all(target_os = "linux", feature = "fixtures"))]
    #[test]
    fn actual_admission_poll_preserves_original_controller_sample_time() {
        let _cache_fixture = CACHE_FIXTURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let sample = snapshot_with_sample();
        let sampled_at = sample.at;
        let limits = sample.value;
        assert!(
            matches!(limits.cpu_ns_per_second, Metric::Known(value) if value > 0)
                && matches!(limits.memory_ceiling_bytes, Metric::Known(value) if value > 0),
            "controlled finite runner must provide actual controller evidence: {limits:?}"
        );
        let directory = tempfile::tempdir().expect("owned capacity fixture directory");
        for _ in 0..3 {
            let observed = super::admission::snapshot(directory.path());
            assert!(
                sampled_at.elapsed() < Duration::from_secs(1),
                "controlled fixture must poll within the live cache window"
            );
            assert_eq!(
                observed.measured_at, sampled_at,
                "fresh FD/task/filesystem samples cannot renew cached controller freshness"
            );
            assert_eq!(
                observed.capacity.cpu_ns_per_second,
                limits.cpu_ns_per_second
            );
            assert_eq!(observed.capacity.memory_bytes, limits.memory_ceiling_bytes);
        }
    }
}
