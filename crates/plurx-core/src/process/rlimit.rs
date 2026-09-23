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
    /// The soft limit in force after it, never above `hard`.
    pub soft_after: LimitValue,
    /// The hard limit, which this call never changes.
    pub hard: LimitValue,
}

/// Raise this process's soft `RLIMIT_NOFILE` to its hard limit.
///
/// A service manager hands a process a soft limit far below the hard one and
/// nothing in the daemon raises it. systemd's `DefaultLimitNOFILESoft` is
/// 1024 against a 524288 hard limit, and the fleet's containers inherit
/// exactly that pair: `cat /proc/1/limits` inside the `plurxd` container read
/// `Max open files  1024  524288` on nuc3, nuc4 and nynuc on 2026-09-23
/// (`docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md` §3.1).
/// A playback session holds pipes, the source descriptor, up to seven sidecar
/// descriptors, segment files and sockets, so the soft limit is the one that
/// runs out first, and it runs out 512 times sooner than the ceiling the
/// deployment actually chose.
///
/// Raising it needs no privilege: a process may set its soft limit anywhere
/// up to its hard limit. The hard limit is left alone, so `LimitNOFILE` and
/// `ulimits.nofile` remain the operative ceiling on every install path.
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
    if limit.rlim_cur >= limit.rlim_max {
        // Already at the ceiling. Calling `setrlimit` anyway would turn the
        // common "both are infinity" case into a spurious error, because
        // Linux refuses a soft limit above `fs.nr_open`.
        return Ok(Some(observed));
    }
    let raised = libc::rlimit {
        rlim_cur: limit.rlim_max,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: `raised` is a live, correctly typed `rlimit` that outlives the
    // call, and `setrlimit` reads only through that pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(OpenFileLimit {
        soft_after: limit.rlim_max,
        ..observed
    }))
}

/// Nothing to raise: this platform has no POSIX resource limits.
#[cfg(not(unix))]
pub fn raise_open_file_limit() -> io::Result<Option<OpenFileLimit>> {
    Ok(None)
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
    fn the_soft_limit_is_raised_to_the_hard_limit() {
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

        assert_eq!(
            soft_before, LOWERED_SOFT,
            "the child reported a soft limit it did not start from"
        );
        assert!(
            hard > LOWERED_SOFT,
            "the test host's hard limit ({hard}) cannot demonstrate a raise"
        );
        assert_eq!(
            soft_after, hard,
            "the soft limit was left below the hard limit"
        );
        assert_eq!(
            soft_now, hard,
            "the process's actual soft limit was left below the hard limit"
        );
        assert_eq!(hard_now, hard, "the hard limit was changed");
    }
}
