//! Target-local storage admission for SQLite snapshots. No Raft write occurs here.
use std::io;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_SNAPSHOT_STORAGE_DEFERRAL: Duration = Duration::from_secs(600);
const RETRY: Duration = Duration::from_secs(10);
const MIB: u64 = 1024 * 1024;

struct StorageSample {
    minimum: u64,
    required: u64,
    available: u64,
}

/// What admission decided. `WriterGone` means the SQL writer has stopped, so
/// the node is shutting down and the build must end instead of waiting.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Admission {
    Go,
    WriterGone,
}

/// Two database images, the live WAL (at least two Plurx WAL segments), and margin.
#[must_use]
pub fn required_snapshot_storage_bytes(database_bytes: u64, wal_bytes: u64) -> u64 {
    minimum_snapshot_storage_bytes(database_bytes, wal_bytes).saturating_add(database_bytes)
}

/// What one build writes: the copied image (`<uuid>.temp`, an online backup
/// of the pinned cut), the WAL that grows while the cut stays pinned, and
/// margin for SQLite's rollback journal on the copy. Below this the build
/// cannot fit.
#[must_use]
pub fn minimum_snapshot_storage_bytes(database_bytes: u64, wal_bytes: u64) -> u64 {
    database_bytes
        .saturating_add(wal_bytes.max(32 * MIB))
        .saturating_add(64 * MIB)
}

/// Measure the live image and WAL, never the coordinator's database.
fn image_and_wal_bytes(database: &Path) -> io::Result<(u64, u64)> {
    let image = std::fs::metadata(database)?.len();
    let mut wal_path = database.as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal = match std::fs::metadata(wal_path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    Ok((image, wal))
}

/// The comfortable storage floor for the live image and WAL.
pub fn snapshot_storage_requirement(database: &Path) -> io::Result<u64> {
    let (image, wal) = image_and_wal_bytes(database)?;
    let required = required_snapshot_storage_bytes(image, wal);
    crate::snapshot_metrics::record_required_storage(required);
    Ok(required)
}

fn snapshot_storage_sample(database: &Path) -> io::Result<StorageSample> {
    let (image, wal) = image_and_wal_bytes(database)?;
    let required = required_snapshot_storage_bytes(image, wal);
    crate::snapshot_metrics::record_required_storage(required);
    Ok(StorageSample {
        minimum: minimum_snapshot_storage_bytes(image, wal),
        required,
        available: fs4::available_space(database)?,
    })
}

pub(crate) async fn wait_for_storage(
    database: &Path,
    limit: Duration,
    writer_gone: impl FnMut() -> bool,
) -> Admission {
    let database = database.to_owned();
    wait_with_probe(limit, writer_gone, || snapshot_storage_sample(&database)).await
}

/// Wait until a build may be attempted, re-probing every [`RETRY`].
///
/// - At or above the comfortable floor the build goes at once.
/// - Below what one build writes ([`minimum_snapshot_storage_bytes`]) it
///   CANNOT fit: it waits without a bound. Attempting could only fail, and in
///   OpenRaft 0.9 a `build_snapshot` storage error ends the Raft core for
///   good, while waiting delays nothing but the next snapshot (and so log
///   purging and a snapshot-anchored backup, which carries its own deadline).
/// - Between the two it can fit, and storage that cannot be measured might:
///   both wait at most `limit` (`snapshot_storage_deferral`; 0 attempts at
///   once), then attempt. They share one clock, which a cannot-fit answer
///   restarts.
///
/// A build also stops waiting, with [`Admission::WriterGone`], once the SQL
/// writer has stopped, so a deferred build cannot outlive its node.
async fn wait_with_probe(
    mut limit: Duration,
    mut writer_gone: impl FnMut() -> bool,
    mut probe: impl FnMut() -> io::Result<StorageSample>,
) -> Admission {
    limit = limit.min(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL);
    let mut bounded_since = None::<tokio::time::Instant>;
    let mut next_warning = tokio::time::Instant::now();
    loop {
        if writer_gone() {
            tracing::info!("snapshot build abandoned: the SQL writer has stopped");
            return Admission::WriterGone;
        }
        let sample = probe();
        if sample
            .as_ref()
            .is_ok_and(|sample| sample.available >= sample.required)
        {
            return Admission::Go;
        }
        let now = tokio::time::Instant::now();
        let cannot_fit = sample
            .as_ref()
            .is_ok_and(|sample| sample.available < sample.minimum);
        let deadline = if cannot_fit {
            bounded_since = None;
            None
        } else {
            Some(*bounded_since.get_or_insert(now) + limit)
        };
        if deadline.is_some_and(|deadline| now >= deadline) {
            tracing::warn!(
                required_bytes = ?sample.as_ref().ok().map(|sample| sample.required),
                available_bytes = ?sample.as_ref().ok().map(|sample| sample.available),
                error = ?sample.as_ref().err(),
                "snapshot storage stayed short of its floor or unknown past its deferral; attempting existing snapshot path"
            );
            return Admission::Go;
        }
        crate::snapshot_metrics::record_storage_deferral();
        if now >= next_warning {
            tracing::warn!(
                minimum_bytes = ?sample.as_ref().ok().map(|sample| sample.minimum),
                required_bytes = ?sample.as_ref().ok().map(|sample| sample.required),
                available_bytes = ?sample.as_ref().ok().map(|sample| sample.available),
                error = ?sample.as_ref().err(),
                cannot_fit,
                "snapshot deferred: insufficient or unknown storage headroom"
            );
            next_warning = now + Duration::from_secs(60);
        }
        let retry = now + RETRY;
        tokio::time::sleep_until(deadline.map_or(retry, |deadline| retry.min(deadline))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum 2, comfortable floor 4.
    fn sample(available: u64) -> io::Result<StorageSample> {
        Ok(StorageSample {
            minimum: 2,
            required: 4,
            available,
        })
    }

    fn unknown() -> io::Result<StorageSample> {
        Err(io::Error::other("unknown filesystem"))
    }

    fn writer_alive() -> bool {
        false
    }

    #[test]
    fn exact_storage_floor_and_overflow_fail_closed() {
        assert_eq!(required_snapshot_storage_bytes(7 * MIB, 1), 110 * MIB);
        assert_eq!(
            required_snapshot_storage_bytes(7 * MIB, 40 * MIB),
            118 * MIB
        );
        assert_eq!(minimum_snapshot_storage_bytes(7 * MIB, 1), 103 * MIB);
        assert_eq!(minimum_snapshot_storage_bytes(7 * MIB, 40 * MIB), 111 * MIB);
        assert_eq!(
            required_snapshot_storage_bytes(u64::MAX, u64::MAX),
            u64::MAX
        );
        assert_eq!(minimum_snapshot_storage_bytes(u64::MAX, u64::MAX), u64::MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn admission_retries_and_releases_without_an_error() {
        let before = crate::snapshot_metrics::LocalDbSnapshotMetrics::new()
            .snapshot()
            .storage_deferrals_total;
        let start = tokio::time::Instant::now();
        let mut probes = 0;
        let admission = wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, writer_alive, || {
            probes += 1;
            sample(if probes >= 3 { 4 } else { 3 })
        })
        .await;
        assert_eq!(admission, Admission::Go);
        assert_eq!(probes, 3);
        assert_eq!(start.elapsed(), Duration::from_secs(20));
        assert!(
            crate::snapshot_metrics::LocalDbSnapshotMetrics::new()
                .snapshot()
                .storage_deferrals_total
                >= before + 2
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_storage_is_bounded_at_ten_minutes() {
        let before = crate::snapshot_metrics::LocalDbSnapshotMetrics::new()
            .snapshot()
            .storage_deferrals_total;
        let start = tokio::time::Instant::now();
        let mut probes = 0;
        let admission = wait_with_probe(Duration::MAX, writer_alive, || {
            probes += 1;
            unknown()
        })
        .await;
        assert_eq!(admission, Admission::Go);
        assert_eq!(probes, 61);
        assert_eq!(start.elapsed(), DEFAULT_SNAPSHOT_STORAGE_DEFERRAL);
        assert!(
            crate::snapshot_metrics::LocalDbSnapshotMetrics::new()
                .snapshot()
                .storage_deferrals_total
                >= before + 60
        );
    }

    #[tokio::test(start_paused = true)]
    async fn storage_that_can_fit_but_is_short_of_the_floor_is_bounded_at_ten_minutes() {
        let start = tokio::time::Instant::now();
        let mut probes = 0_u32;
        // Exactly the minimum, and one short of the comfortable floor.
        for available in [2, 3] {
            let band_start = tokio::time::Instant::now();
            let admission =
                wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, writer_alive, || {
                    probes += 1;
                    sample(available)
                })
                .await;
            assert_eq!(admission, Admission::Go);
            assert_eq!(band_start.elapsed(), DEFAULT_SNAPSHOT_STORAGE_DEFERRAL);
        }
        assert_eq!(probes, 2 * 61);
        assert_eq!(start.elapsed(), 2 * DEFAULT_SNAPSHOT_STORAGE_DEFERRAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_deferral_attempts_a_build_that_can_fit_at_once() {
        for answer in [sample as fn(u64) -> io::Result<StorageSample>, |_| {
            unknown()
        }] {
            let start = tokio::time::Instant::now();
            let mut probes = 0_u32;
            let admission = wait_with_probe(Duration::ZERO, writer_alive, || {
                probes += 1;
                answer(3)
            })
            .await;
            assert_eq!(admission, Admission::Go);
            assert_eq!((probes, start.elapsed()), (1, Duration::ZERO));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_build_that_cannot_fit_is_never_attempted() {
        // Zero is the operator's "attempt at once"; it still never attempts
        // a build that cannot fit.
        for limit in [DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, Duration::ZERO] {
            let start = tokio::time::Instant::now();
            let mut probes = 0_u32;
            // One byte short of the minimum for 30 minutes, three times the
            // bound, then enough.
            let waited = tokio::time::timeout(
                Duration::from_secs(3_600),
                wait_with_probe(limit, writer_alive, || {
                    probes += 1;
                    let fits = tokio::time::Instant::now() - start >= Duration::from_secs(1_800);
                    sample(if fits { 4 } else { 1 })
                }),
            )
            .await;
            assert_eq!(
                waited,
                Ok(Admission::Go),
                "the build proceeds once storage fits"
            );
            assert_eq!(
                start.elapsed(),
                Duration::from_secs(1_800),
                "storage that cannot fit is never attempted, however long it lasts"
            );
            assert_eq!(probes, 181);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_cannot_fit_answer_restarts_the_bound() {
        let start = tokio::time::Instant::now();
        // Unknown for five minutes, cannot fit for five, then unknown again.
        wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, writer_alive, || {
            let elapsed = tokio::time::Instant::now() - start;
            if elapsed >= Duration::from_secs(300) && elapsed < Duration::from_secs(600) {
                sample(1)
            } else {
                unknown()
            }
        })
        .await;
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(600) + DEFAULT_SNAPSHOT_STORAGE_DEFERRAL,
            "only a stretch with no cannot-fit answer is bounded"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_and_short_of_the_floor_share_one_bound() {
        let start = tokio::time::Instant::now();
        // Unknown for five minutes, then can fit but short of the floor.
        wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, writer_alive, || {
            if tokio::time::Instant::now() - start < Duration::from_secs(300) {
                unknown()
            } else {
                sample(3)
            }
        })
        .await;
        assert_eq!(start.elapsed(), DEFAULT_SNAPSHOT_STORAGE_DEFERRAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deferred_build_ends_when_its_writer_is_gone() {
        let start = tokio::time::Instant::now();
        let mut probes = 0_u32;
        let waited = tokio::time::timeout(
            Duration::from_secs(3_600),
            wait_with_probe(
                DEFAULT_SNAPSHOT_STORAGE_DEFERRAL,
                || tokio::time::Instant::now() - start >= Duration::from_secs(60),
                || {
                    probes += 1;
                    sample(1)
                },
            ),
        )
        .await;
        assert_eq!(waited, Ok(Admission::WriterGone));
        assert_eq!((probes, start.elapsed()), (6, Duration::from_secs(60)));
    }

    #[tokio::test(start_paused = true)]
    async fn the_real_probe_sees_a_disconnected_writer_before_measuring() {
        let (write_tx, write_rx) = flume::unbounded::<()>();
        drop(write_rx);
        let missing = std::env::temp_dir().join("hiqlite-admission-missing-database");
        let admission = wait_for_storage(&missing, DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, || {
            write_tx.is_disconnected()
        })
        .await;
        assert_eq!(admission, Admission::WriterGone);
    }
}
