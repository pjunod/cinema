//! Target-local storage admission for SQLite snapshots. No Raft write occurs here.
use std::io;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_SNAPSHOT_STORAGE_DEFERRAL: Duration = Duration::from_secs(600);
const RETRY: Duration = Duration::from_secs(10);
const MIB: u64 = 1024 * 1024;

struct StorageSample {
    required: u64,
    available: u64,
}

/// Two database images, the live WAL (at least two Plurx WAL segments), and margin.
#[must_use]
pub fn required_snapshot_storage_bytes(database_bytes: u64, wal_bytes: u64) -> u64 {
    database_bytes
        .saturating_mul(2)
        .saturating_add(wal_bytes.max(32 * MIB))
        .saturating_add(64 * MIB)
}

/// Measure the live image and WAL, never the coordinator's database.
pub fn snapshot_storage_requirement(database: &Path) -> io::Result<u64> {
    let image = std::fs::metadata(database)?.len();
    let mut wal_path = database.as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal = match std::fs::metadata(wal_path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    let required = required_snapshot_storage_bytes(image, wal);
    crate::snapshot_metrics::record_required_storage(required);
    Ok(required)
}

pub(crate) async fn wait_for_storage(database: &Path, limit: Duration) {
    let database = database.to_owned();
    wait_with_probe(limit, || {
        let required = snapshot_storage_requirement(&database)?;
        let available = fs4::available_space(&database)?;
        Ok(StorageSample {
            required,
            available,
        })
    })
    .await;
}

/// Wait until a build can fit. Storage KNOWN to be short keeps the build
/// waiting, re-probed every [`RETRY`]: attempting it could only fail, and in
/// OpenRaft 0.9 a `build_snapshot` storage error ends the Raft core for good,
/// while waiting delays nothing but the next snapshot (and so log purging and
/// a snapshot-anchored backup, which carries its own deadline). Only storage
/// that cannot be measured is bounded by `limit`: an unknown answer stays
/// unknown, so after `limit` of it the existing path is attempted.
async fn wait_with_probe(
    mut limit: Duration,
    mut probe: impl FnMut() -> io::Result<StorageSample>,
) {
    limit = limit.min(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL);
    let start = tokio::time::Instant::now();
    let mut unknown_since = None::<tokio::time::Instant>;
    let mut next_warning = start;
    loop {
        let sample = probe();
        if sample
            .as_ref()
            .is_ok_and(|sample| sample.available >= sample.required)
        {
            return;
        }
        let now = tokio::time::Instant::now();
        let deadline = match &sample {
            Ok(_) => {
                unknown_since = None;
                None
            }
            Err(_) => Some(*unknown_since.get_or_insert(now) + limit),
        };
        if deadline.is_some_and(|deadline| now >= deadline) {
            tracing::warn!(
                "snapshot storage stayed unknown past its deferral; attempting existing snapshot path"
            );
            return;
        }
        crate::snapshot_metrics::record_storage_deferral();
        if now >= next_warning {
            tracing::warn!(
                required_bytes = ?sample.as_ref().ok().map(|sample| sample.required),
                available_bytes = ?sample.as_ref().ok().map(|sample| sample.available),
                error = ?sample.as_ref().err(),
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
    #[test]
    fn exact_storage_floor_and_overflow_fail_closed() {
        assert_eq!(required_snapshot_storage_bytes(7 * MIB, 1), 110 * MIB);
        assert_eq!(
            required_snapshot_storage_bytes(7 * MIB, 40 * MIB),
            118 * MIB
        );
        assert_eq!(
            required_snapshot_storage_bytes(u64::MAX, u64::MAX),
            u64::MAX
        );
    }
    #[tokio::test(start_paused = true)]
    async fn admission_retries_and_releases_without_an_error() {
        let before = crate::snapshot_metrics::LocalDbSnapshotMetrics::new()
            .snapshot()
            .storage_deferrals_total;
        let start = tokio::time::Instant::now();
        let mut probes = 0;
        wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, || {
            probes += 1;
            Ok(StorageSample {
                required: 1,
                available: u64::from(probes >= 3),
            })
        })
        .await;
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
        wait_with_probe(Duration::MAX, || {
            probes += 1;
            Err(io::Error::other("unknown filesystem"))
        })
        .await;
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
    async fn known_insufficient_storage_never_attempts_the_build() {
        let start = tokio::time::Instant::now();
        let mut probes = 0_u32;
        // Short for 30 minutes, three times the unknown bound, then enough.
        let waited = tokio::time::timeout(
            Duration::from_secs(3_600),
            wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, || {
                probes += 1;
                Ok(StorageSample {
                    required: 2,
                    available: if tokio::time::Instant::now() - start
                        >= Duration::from_secs(1_800)
                    {
                        2
                    } else {
                        1
                    },
                })
            }),
        )
        .await;
        assert!(waited.is_ok(), "the build proceeds once storage fits");
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(1_800),
            "storage known to be short is never attempted, however long it lasts"
        );
        assert_eq!(probes, 181);
    }

    #[tokio::test(start_paused = true)]
    async fn a_known_answer_restarts_the_unknown_bound() {
        let start = tokio::time::Instant::now();
        let mut probes = 0_u32;
        wait_with_probe(DEFAULT_SNAPSHOT_STORAGE_DEFERRAL, || {
            probes += 1;
            // Unknown for five minutes, short for five, then unknown again.
            let elapsed = tokio::time::Instant::now() - start;
            if elapsed >= Duration::from_secs(300) && elapsed < Duration::from_secs(600) {
                Ok(StorageSample {
                    required: 2,
                    available: 1,
                })
            } else {
                Err(io::Error::other("unknown filesystem"))
            }
        })
        .await;
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(600) + DEFAULT_SNAPSHOT_STORAGE_DEFERRAL,
            "only a continuous unknown stretch is bounded"
        );
    }
}
