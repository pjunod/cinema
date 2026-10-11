//! What this binary is, for everything that has to report it.
//!
//! Two numbers, deliberately kept apart:
//!
//! * [`SEMVER`] is the released version and nothing else. Clients compare it,
//!   so it stays a bare `MAJOR.MINOR.PATCH` that a semver parser accepts.
//! * [`BUILD`] is the git description of the exact commit — the thing that
//!   makes a bug report actionable when the reporter is running `main` rather
//!   than a release.
//!
//! Version policy lives in `docs/RELEASING.md`; the short form is that plurx is
//! 0.x, so the minor number moves on features *and* on breaking changes, and
//! the patch number on fixes.

/// The released version: bare semver, safe to parse and compare.
pub const SEMVER: &str = env!("CARGO_PKG_VERSION");

/// Git description of the commit this binary was built from — `v0.1.0`,
/// `v0.1.0-14-gc0ffee`, `…-dirty`, or `unknown` when built without a checkout.
/// Stamped by `build.rs`; overridable with `PLURX_BUILD_REF` for package builds
/// that have no `.git`.
pub const BUILD: &str = env!("PLURX_BUILD");

/// The build's source date, `YYYY-MM-DDTHH:MM:SSZ`. Always present, even when
/// [`BUILD`] is `unknown`.
///
/// It is `SOURCE_DATE_EPOCH` when the build set it (every image build passes
/// the commit's committer time), else the `HEAD` commit time, else the compile
/// clock -- see `build_support/source_date.rs`. It was the compile clock
/// always, which made two builds of one commit different binaries.
///
/// A container built from a context with no `.git` and no `PLURX_BUILD_REF`
/// cannot say *which* commit it is -- but it can still say it is not
/// yesterday's, and "did my change land?" is the question somebody who just
/// deployed is actually asking. `unknown` alone could not answer it, which is
/// how the System page read `plurx 0.1.0` through weeks of daily deploys.
pub const BUILT_AT: &str = env!("PLURX_BUILT_AT");

/// Both, for `--version` and the startup log: `0.1.0 (v0.1.0-14-gc0ffee)`.
pub const LONG: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("PLURX_BUILD"), ")");

#[cfg(test)]
#[path = "../build_support/source_date.rs"]
mod source_date;

#[cfg(test)]
mod tests {
    use super::source_date::{format_utc, resolve_source_date};
    use super::*;

    fn never() -> i64 {
        panic!("the clock must not be read when a source date exists")
    }

    #[test]
    fn source_date_epoch_wins_over_the_commit_and_the_clock() {
        // Two builds of one commit must stamp the same date: the variable the
        // image build passes is used verbatim, not the clock it ran at.
        let got = resolve_source_date(Some("1759388856"), false, || Some(1), never);
        assert_eq!(got, Ok(1_759_388_856));
        assert_eq!(format_utc(1_759_388_856), "2025-10-02T07:07:36Z");
    }

    #[test]
    fn without_source_date_epoch_the_commit_time_is_used_before_the_clock() {
        assert_eq!(resolve_source_date(None, false, || Some(42), never), Ok(42));
        // Compose passes an empty value when nothing derived one: that is
        // "unset", not malformed.
        assert_eq!(
            resolve_source_date(Some(" "), false, || Some(42), never),
            Ok(42)
        );
        assert_eq!(resolve_source_date(None, false, || None, || 7), Ok(7));
    }

    #[test]
    fn a_dirty_build_is_not_dated_with_head_s_commit_time() {
        // A `-dirty` tree is not HEAD; dating it with HEAD's time would claim
        // the edited binary is the committed one. Only the clock may date it,
        // unless a caller deliberately passes SOURCE_DATE_EPOCH.
        let never_commit =
            || -> Option<i64> { panic!("a dirty build must not read the commit time") };
        assert_eq!(resolve_source_date(None, true, never_commit, || 7), Ok(7));
        assert_eq!(
            resolve_source_date(Some(""), true, never_commit, || 7),
            Ok(7)
        );
        assert_eq!(
            resolve_source_date(Some("99"), true, never_commit, never),
            Ok(99)
        );
    }

    #[test]
    fn a_malformed_source_date_epoch_fails_instead_of_reading_the_clock() {
        for bad in ["yesterday", "-1", "+5", "1.5", "17e8"] {
            let err = resolve_source_date(Some(bad), false, || Some(1), never)
                .expect_err("malformed SOURCE_DATE_EPOCH must be refused");
            assert!(err.contains("SOURCE_DATE_EPOCH"), "{err}");
        }
    }

    #[test]
    fn utc_formatting_covers_the_epoch_and_a_leap_day() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_709_251_199), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn the_compiled_in_stamp_is_a_utc_timestamp() {
        let b = BUILT_AT.as_bytes();
        assert_eq!(b.len(), 20, "{BUILT_AT}");
        assert_eq!(
            (b[4], b[7], b[10], b[13], b[16], b[19]),
            (b'-', b'-', b'T', b':', b':', b'Z')
        );
    }

    #[test]
    fn semver_is_three_numeric_parts() {
        let parts: Vec<&str> = SEMVER.split('.').collect();
        assert_eq!(parts.len(), 3, "SEMVER must be MAJOR.MINOR.PATCH: {SEMVER}");
        for p in parts {
            assert!(
                !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()),
                "non-numeric semver component in {SEMVER}"
            );
        }
    }

    #[test]
    fn build_stamp_is_present() {
        assert!(!BUILD.is_empty());
        assert!(LONG.starts_with(SEMVER));
        assert!(LONG.contains(BUILD));
    }
}
