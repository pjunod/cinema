//! The date a build stamps as `PLURX_BUILT_AT`, as pure functions.
//!
//! `build.rs` includes this file with `#[path]`, and `src/version.rs` includes
//! it again under `#[cfg(test)]`, so the precedence below is tested by the
//! crate's own unit tests instead of only by reading the build script.
//!
//! **What the stamp means.** It is the build's *source date*: the instant the
//! source being compiled was fixed, not the instant the compiler ran. In order:
//!
//! 1. `SOURCE_DATE_EPOCH`, when set and non-empty, in the
//!    [reproducible-builds.org](https://reproducible-builds.org/specs/source-date-epoch/)
//!    sense. Every image build passes the commit's committer time here, because
//!    the Docker context carries no `.git`.
//! 2. The `HEAD` commit's committer time, when the crate is built from a
//!    checkout.
//! 3. The clock, only when neither exists (a source tarball built with no
//!    `SOURCE_DATE_EPOCH`).
//!
//! Using the clock first, as this stamp once did, made no two builds of one
//! commit byte-identical, so an image could never be reproduced and compared.
//! The commit time still answers the question the stamp exists for -- "is this
//! the deploy I just made, or the last one?" -- because two commits differ in
//! it, while the same commit built twice now yields the same binary.

/// Resolve the build's source date in Unix seconds.
///
/// A `SOURCE_DATE_EPOCH` that is set but is not a decimal count of seconds is
/// an error rather than a silent fall back to the clock: the spec asks for the
/// build to fail, and a fallback would quietly produce the unreproducible
/// binary the variable was set to prevent.
pub fn resolve_source_date(
    source_date_epoch: Option<&str>,
    commit_time: impl FnOnce() -> Option<i64>,
    now: impl FnOnce() -> i64,
) -> Result<i64, String> {
    if let Some(raw) = source_date_epoch.map(str::trim).filter(|v| !v.is_empty()) {
        return match raw.parse::<i64>() {
            Ok(secs) if secs >= 0 && raw.bytes().all(|b| b.is_ascii_digit()) => Ok(secs),
            _ => Err(format!(
                "SOURCE_DATE_EPOCH must be a non-negative decimal count of seconds, got {raw:?}"
            )),
        };
    }
    Ok(commit_time().unwrap_or_else(now))
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Hand-rolled rather than pulling a date crate into the build graph: this is
/// the only place the daemon needs to format a wall clock at build time, and a
/// dependency that exists to print seven numbers is a dependency that has to be
/// audited, updated and explained forever.
pub fn format_utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm), shifted to a March-based
    // year so the leap day lands at the end and needs no special case.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = era * 400 + yoe + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}
