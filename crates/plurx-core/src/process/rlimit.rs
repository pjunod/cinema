//! The daemon's own inherited resource limits.
//!
//! The rest of this module tree is about children the daemon owns. This file
//! is about one limit the daemon inherits for itself and can widen without
//! any privilege.

use std::io;

/// The type a platform uses for a resource-limit value.
///
/// A plain `u64` would need a cast on Unix, and the width of `rlim_t` is a
/// platform's own business.
#[cfg(unix)]
pub type LimitValue = libc::rlim_t;

/// The type a platform uses for a resource-limit value.
#[cfg(not(unix))]
pub type LimitValue = u64;

/// The soft and hard `RLIMIT_NOFILE` values around a raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFileLimit {
    /// The soft limit in force before the raise.
    pub soft_before: LimitValue,
    /// The soft limit in force after it: the hard limit, or the platform's
    /// per-process ceiling where that is lower (macOS). Never above `hard`.
    pub soft_after: LimitValue,
    /// The hard limit, which this call never changes.
    pub hard: LimitValue,
}

/// Raise this process's soft `RLIMIT_NOFILE` as far as the platform allows:
/// to the hard limit, or on macOS to `kern.maxfilesperproc` where the hard
/// limit is higher than that (typically `RLIM_INFINITY`).
///
/// A service manager hands a process a soft limit far below the hard one and
/// nothing in the daemon raises it. systemd's `DefaultLimitNOFILESoft` is
/// 1024 against a 524288 hard limit, and the fleet's containers inherit
/// exactly that pair: `cat /proc/1/limits` inside the `plurxd` container read
/// `Max open files  1024  524288` on nuc3, nuc4 and nynuc on 2026-09-23
/// (`docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md` §3.1).
/// launchd's default for a LaunchAgent such as `deploy/com.plurx.plurxd.plist`
/// is `256` soft against an unlimited hard limit, the lowest of any install
/// path. A playback session holds pipes, the source descriptor, up to seven
/// sidecar descriptors, segment files and sockets, so the soft limit is the
/// one that runs out first.
///
/// Raising it needs no privilege: a process may set its soft limit anywhere
/// up to its hard limit. The hard limit is left alone, so `LimitNOFILE` and
/// `ulimits.nofile` remain the operative ceiling where a deployment sets one.
///
/// Returns `Ok(None)` where the platform has no POSIX resource limits, which
/// is every non-Unix target including the Windows service.
#[cfg(unix)]
pub fn raise_open_file_limit() -> io::Result<Option<OpenFileLimit>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live, correctly typed `rlimit` that outlives the
    // call, and `getrlimit` writes only through that pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let observed = OpenFileLimit {
        soft_before: limit.rlim_cur,
        soft_after: limit.rlim_cur,
        hard: limit.rlim_max,
    };
    let Some(target) =
        raised_soft_limit(limit.rlim_cur, limit.rlim_max, platform_open_file_ceiling())
    else {
        // Already at the ceiling. Calling `setrlimit` anyway would turn the
        // "both are infinity" case into a spurious error on a platform that
        // refuses an infinite soft `RLIMIT_NOFILE`.
        return Ok(Some(observed));
    };
    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: `raised` is a live, correctly typed `rlimit` that outlives the
    // call, and `setrlimit` reads only through that pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(OpenFileLimit {
        soft_after: target,
        ..observed
    }))
}

/// Nothing to raise: this platform has no POSIX resource limits.
#[cfg(not(unix))]
pub fn raise_open_file_limit() -> io::Result<Option<OpenFileLimit>> {
    Ok(None)
}

/// The soft limit to raise to, or `None` when `soft` is already there.
///
/// The target is the hard limit, clamped to the platform's own per-process
/// ceiling when it has one below the hard limit. Kept free of system calls so
/// the clamp can be tested on every host, not only on the platform that
/// needs it.
#[cfg(unix)]
fn raised_soft_limit(
    soft: LimitValue,
    hard: LimitValue,
    platform_ceiling: Option<LimitValue>,
) -> Option<LimitValue> {
    let target = platform_ceiling.map_or(hard, |ceiling| ceiling.min(hard));
    (soft < target).then_some(target)
}

/// `OPEN_MAX` from macOS `<sys/syslimits.h>`: the value `setrlimit(2)`'s
/// COMPATIBILITY note tells callers to clamp to when the per-process ceiling
/// cannot be read.
#[cfg(target_vendor = "apple")]
const APPLE_OPEN_MAX: LimitValue = 10240;

/// The highest soft `RLIMIT_NOFILE` this platform actually honours, where
/// that is a fixed value rather than the hard limit.
///
/// macOS enforces `kern.maxfilesperproc` whatever the hard limit says, and
/// launchd hands every LaunchAgent an unlimited hard limit. `setrlimit(2)`'s
/// COMPATIBILITY note says a soft `RLIM_INFINITY` for `RLIMIT_NOFILE` is
/// refused with `EINVAL`. macOS 27 on the lab Mac instead accepts it,
/// reports it back from `getrlimit`, and still stops at
/// `kern.maxfilesperproc`: 122877 descriptors opened, then `EMFILE`. On
/// either behaviour the honest target is the per-process ceiling, so clamp
/// to it, as Go's runtime does for the same raise, falling back to
/// `OPEN_MAX`.
#[cfg(target_vendor = "apple")]
fn platform_open_file_ceiling() -> Option<LimitValue> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated C string, `value` is a live
    // `c_int` whose size is passed in `size`, and no new value is written.
    let read = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    match LimitValue::try_from(value) {
        Ok(ceiling) if read == 0 && ceiling > 0 => Some(ceiling),
        _ => Some(APPLE_OPEN_MAX),
    }
}

/// Linux caps the hard limit itself at `fs.nr_open`, so any hard limit a
/// process holds is also an acceptable soft limit, and the other Unix targets
/// the daemon builds for clamp silently rather than refusing.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn platform_open_file_ceiling() -> Option<LimitValue> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Low enough to be nowhere near any hard limit a test host sets.
    const LOWERED_SOFT: LimitValue = 256;

    /// Read the soft and hard limit without going through the code under
    /// test, so the assertions below do not depend on what it reports.
    fn observe() -> (LimitValue, LimitValue) {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: as in `raise_open_file_limit`.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0,
            "reading the open-file limit"
        );
        (limit.rlim_cur, limit.rlim_max)
    }

    /// The soft limit this host should end up at, worked out without the
    /// code under test: the hard limit, or on macOS the smaller of it and
    /// `kern.maxfilesperproc` as the `sysctl` tool reports it.
    fn expected_raised_soft(hard: LimitValue) -> LimitValue {
        if cfg!(target_vendor = "apple") {
            let output = std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.maxfilesperproc"])
                .output()
                .expect("running sysctl");
            let ceiling = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<LimitValue>()
                .expect("kern.maxfilesperproc is a number");
            ceiling.min(hard)
        } else {
            hard
        }
    }

    #[test]
    fn the_raise_targets_the_hard_limit_where_no_platform_ceiling_applies() {
        assert_eq!(raised_soft_limit(1024, 524_288, None), Some(524_288));
        assert_eq!(raised_soft_limit(524_288, 524_288, None), None);
        assert_eq!(
            raised_soft_limit(libc::RLIM_INFINITY, libc::RLIM_INFINITY, None),
            None,
            "an infinite soft limit is already at an infinite hard limit"
        );
    }

    /// The macOS LaunchAgent case, pinned on every host: launchd's
    /// `256 / unlimited`. Asking for `RLIM_INFINITY` there is refused on the
    /// macOS versions `setrlimit(2)` describes, and on macOS 27 is accepted
    /// but reported as a limit the kernel does not enforce, so the target
    /// must be the platform ceiling, never the hard limit.
    #[test]
    fn an_unlimited_hard_limit_is_clamped_to_the_platform_ceiling() {
        assert_eq!(
            raised_soft_limit(256, libc::RLIM_INFINITY, Some(24_576)),
            Some(24_576),
            "an unlimited hard limit must be clamped to the per-process ceiling"
        );
        assert_eq!(
            raised_soft_limit(256, 4096, Some(24_576)),
            Some(4096),
            "a hard limit below the ceiling is the target, never exceeded"
        );
        assert_eq!(
            raised_soft_limit(24_576, libc::RLIM_INFINITY, Some(24_576)),
            None,
            "a soft limit already at the ceiling calls nothing"
        );
    }

    /// The child half of the test below.
    ///
    /// `setrlimit` is process-wide, so lowering the soft limit inside the
    /// shared test process would change it for every other test in this
    /// binary. Re-exec instead, which is the pattern the sibling
    /// process-ownership tests use.
    #[test]
    fn child_main() {
        if std::env::var("PLURX_RLIMIT_CHILD").is_err() {
            return;
        }
        let (_, hard) = observe();
        assert!(
            hard > LOWERED_SOFT,
            "this host's hard limit ({hard}) cannot demonstrate a raise"
        );
        let lowered = libc::rlimit {
            rlim_cur: LOWERED_SOFT,
            rlim_max: hard,
        };
        // SAFETY: as in `raise_open_file_limit`.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) },
            0,
            "lowering this child's own soft limit"
        );

        let reported = raise_open_file_limit()
            .expect("raising the soft limit")
            .expect("Unix reports a limit");
        let (soft_now, hard_now) = observe();
        println!(
            "rlimit={}|{}|{}|{}|{}",
            reported.soft_before, reported.soft_after, reported.hard, soft_now, hard_now
        );
    }

    #[test]
    fn the_soft_limit_is_raised_as_far_as_the_platform_allows() {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "process::rlimit::tests::child_main",
                "--nocapture",
            ])
            .env("PLURX_RLIMIT_CHILD", "1")
            .output()
            .expect("child output");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "child failed: {stdout}\n{stderr}");
        let fields = stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("rlimit="))
            .unwrap_or_else(|| panic!("child reported no limits: {stdout}\n{stderr}"))
            .split('|')
            .map(|field| field.parse::<LimitValue>().expect("numeric limit"))
            .collect::<Vec<_>>();
        let [soft_before, soft_after, hard, soft_now, hard_now] = fields[..] else {
            panic!("child reported {} fields: {stdout}", fields.len());
        };
        let expected = expected_raised_soft(hard);

        assert_eq!(
            soft_before, LOWERED_SOFT,
            "the child reported a soft limit it did not start from"
        );
        assert!(
            expected > LOWERED_SOFT,
            "the test host's ceiling ({expected}) cannot demonstrate a raise"
        );
        assert_eq!(
            soft_after, expected,
            "the soft limit was not raised to the platform's ceiling"
        );
        assert_eq!(
            soft_now, expected,
            "the process's actual soft limit was not raised to the platform's ceiling"
        );
        assert_eq!(hard_now, hard, "the hard limit was changed");
    }
}
