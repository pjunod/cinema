//! Plurx patch 27: the applied mark.
//!
//! A process-local, lock-free account of what this node's SQLite image has
//! applied and which registered tables changed. The single writer thread is
//! the only writer: an SQLite update hook sets a bit per registered table, and
//! [`AppliedMark::publish`] turns those bits into revision bumps and stores the
//! applied log index *after* the SQLite work has committed (or rolled back)
//! and *before* the writer replies, so a reader that sees a revision also sees
//! the rows behind it and an acknowledged write is never served from an older
//! memo on this node.
//!
//! Image replacement (a snapshot restore) is owned by the state machine, not
//! the writer arm: an [`ImageReplacement`] guard makes `generation` odd before
//! `SnapshotApply` is sent and even again only after the read pool has been
//! reconnected. A guard dropped without completing leaves it odd: the view
//! stays closed until restart.
//!
//! Nothing here crosses the wire; revisions are meaningless on another node.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

/// Registered tables a node may name in [`crate::NodeConfig::applied_mark_tables`].
pub const APPLIED_MARK_MAX_TABLES: usize = 8;

#[derive(Debug)]
pub(crate) struct AppliedMark {
    /// Even: the image is stable. Odd: it is being replaced, or a replacement
    /// failed and the image is not trusted until restart.
    generation: AtomicU64,
    /// The last log index whose effects the read pool can see.
    applied_index: AtomicU64,
    /// One dedicated revision per registered table, in registration order.
    tables: [AtomicU64; APPLIED_MARK_MAX_TABLES],
    names: Vec<String>,
    /// Writer-local: set by the update hook on the writer thread, taken by
    /// `publish` on the same thread.
    pending: AtomicU8,
    /// Replacements that were begun and never completed.
    failed_replacements: AtomicU64,
}

impl AppliedMark {
    pub(crate) fn new<S: AsRef<str>>(tables: &[S]) -> Arc<Self> {
        assert!(
            tables.len() <= APPLIED_MARK_MAX_TABLES,
            "NodeConfig::is_valid bounds the registered applied-mark tables"
        );
        Arc::new(Self {
            generation: AtomicU64::new(0),
            applied_index: AtomicU64::new(0),
            tables: Default::default(),
            names: tables.iter().map(|name| name.as_ref().to_owned()).collect(),
            pending: AtomicU8::new(0),
            failed_replacements: AtomicU64::new(0),
        })
    }

    fn all_bits(&self) -> u8 {
        if self.names.len() >= 8 {
            u8::MAX
        } else {
            (1_u8 << self.names.len()) - 1
        }
    }

    /// The update hook: one short compare per registered table, no hashing.
    /// Unregistered tables cost the compares and nothing else.
    pub(crate) fn note_row_change(&self, database: &str, table: &str) {
        if database != "main" {
            return;
        }
        for (slot, name) in self.names.iter().enumerate() {
            if name == table {
                self.pending.fetch_or(1 << slot, Ordering::Relaxed);
                return;
            }
        }
    }

    /// DDL and table rebuilds are reported by the hook under a temporary name
    /// or not at all, so the arms that carry them invalidate every slot.
    pub(crate) fn note_every_table(&self) {
        self.pending.fetch_or(self.all_bits(), Ordering::Relaxed);
    }

    /// Bump each table the hook saw, then publish the applied index. Called
    /// after commit or rollback and before the reply, from the writer only.
    pub(crate) fn publish(&self, applied_index: Option<u64>) {
        let pending = self.pending.swap(0, Ordering::Relaxed);
        for (slot, revision) in self.tables.iter().enumerate().take(self.names.len()) {
            if pending & (1 << slot) != 0 {
                revision.fetch_add(1, Ordering::Release);
            }
        }
        if let Some(index) = applied_index {
            self.applied_index.store(index, Ordering::Release);
        }
    }

    pub(crate) fn begin_replacement(self: &Arc<Self>) -> ImageReplacement {
        // Even -> odd. An odd value (an earlier replacement failed) moves by
        // two, staying odd while still changing, so no stamp survives it.
        let _ = self
            .generation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                Some(if generation & 1 == 0 {
                    generation.wrapping_add(1)
                } else {
                    generation.wrapping_add(2)
                })
            });
        ImageReplacement {
            mark: Arc::clone(self),
            completed: false,
        }
    }

    pub(crate) fn handle(self: &Arc<Self>) -> AppliedMarkHandle {
        AppliedMarkHandle {
            mark: Arc::clone(self),
        }
    }
}

/// Owned by whoever sends `SnapshotApply`; completed only after the read pool
/// has been reconnected to the replaced image.
#[derive(Debug)]
pub(crate) struct ImageReplacement {
    mark: Arc<AppliedMark>,
    completed: bool,
}

impl ImageReplacement {
    pub(crate) fn complete(mut self) {
        self.completed = true;
        self.mark.generation.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for ImageReplacement {
    fn drop(&mut self) {
        if !self.completed {
            self.mark
                .failed_replacements
                .fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                "SQLite image replacement did not complete; the applied mark stays \
                 odd and local views stay closed until this node restarts"
            );
        }
    }
}

/// Read-only handle to this node's applied mark. Loads are lock-free and
/// perform no IO.
#[derive(Clone, Debug)]
pub struct AppliedMarkHandle {
    mark: Arc<AppliedMark>,
}

/// One table revision as seen by a reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedSlot(usize);

/// Everything an advisory surface shows, loaded at read time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMarkObservation {
    pub generation: u64,
    pub applied_index: u64,
    pub failed_replacements: u64,
    /// `(table, revision)` in registration order.
    pub tables: Vec<(String, u64)>,
}

impl AppliedMarkHandle {
    /// The slot registered for `table`, if any.
    #[must_use]
    pub fn slot(&self, table: &str) -> Option<AppliedSlot> {
        self.mark
            .names
            .iter()
            .position(|name| name == table)
            .map(AppliedSlot)
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.mark.generation.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn applied_index(&self) -> u64 {
        self.mark.applied_index.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn revision(&self, slot: AppliedSlot) -> u64 {
        self.mark.tables[slot.0].load(Ordering::Acquire)
    }

    #[must_use]
    pub fn observation(&self) -> AppliedMarkObservation {
        AppliedMarkObservation {
            generation: self.generation(),
            applied_index: self.applied_index(),
            failed_replacements: self.mark.failed_replacements.load(Ordering::Relaxed),
            tables: self
                .mark
                .names
                .iter()
                .zip(&self.mark.tables)
                .map(|(name, revision)| (name.clone(), revision.load(Ordering::Acquire)))
                .collect(),
        }
    }
}

/// Fixed upper bounds for the read-pool wait histogram, in nanoseconds.
pub const DB_READ_POOL_WAIT_BOUNDS_NANOS: [u64; 8] = [
    100_000,
    500_000,
    1_000_000,
    5_000_000,
    10_000_000,
    50_000_000,
    100_000_000,
    500_000_000,
];

/// Cumulative read-pool wait: time a local query spent waiting for a pool
/// connection before it ran. Process-wide, like the snapshot hooks: the pool
/// handle the query path holds carries no node identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbReadPoolWait {
    pub count: u64,
    pub sum_nanos: u64,
    pub cumulative_buckets: [u64; DB_READ_POOL_WAIT_BOUNDS_NANOS.len()],
}

struct ReadPoolWaitAtomics {
    count: AtomicU64,
    sum_nanos: AtomicU64,
    buckets: [AtomicU64; DB_READ_POOL_WAIT_BOUNDS_NANOS.len()],
}

static READ_POOL_WAIT: ReadPoolWaitAtomics = ReadPoolWaitAtomics {
    count: AtomicU64::new(0),
    sum_nanos: AtomicU64::new(0),
    buckets: [const { AtomicU64::new(0) }; DB_READ_POOL_WAIT_BOUNDS_NANOS.len()],
};

pub(crate) fn record_read_pool_wait(waited: Duration) {
    let nanos = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);
    for (bound, bucket) in DB_READ_POOL_WAIT_BOUNDS_NANOS
        .iter()
        .zip(&READ_POOL_WAIT.buckets)
    {
        if nanos <= *bound {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
    }
    READ_POOL_WAIT.sum_nanos.fetch_add(nanos, Ordering::Relaxed);
    READ_POOL_WAIT.count.fetch_add(1, Ordering::Relaxed);
}

/// Read the process-wide read-pool wait histogram. Counters are loaded
/// individually, so a scrape racing a query may see a bucket one ahead of
/// the count; every value is monotonic.
#[must_use]
pub fn db_read_pool_wait() -> DbReadPoolWait {
    let mut cumulative_buckets = [0; DB_READ_POOL_WAIT_BOUNDS_NANOS.len()];
    for (out, bucket) in cumulative_buckets.iter_mut().zip(&READ_POOL_WAIT.buckets) {
        *out = bucket.load(Ordering::Relaxed);
    }
    DbReadPoolWait {
        count: READ_POOL_WAIT.count.load(Ordering::Relaxed),
        sum_nanos: READ_POOL_WAIT.sum_nanos.load(Ordering::Relaxed),
        cumulative_buckets,
    }
}

/// Take one read-pool connection, recording how long the wait was.
pub(crate) async fn timed_read_connection(
    pool: &crate::store::state_machine::sqlite::state_machine::SqlitePool,
) -> Result<deadpool::unmanaged::Object<rusqlite::Connection>, deadpool::unmanaged::PoolError> {
    let started = std::time::Instant::now();
    let connection = pool.get().await;
    record_read_pool_wait(started.elapsed());
    connection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registered_tables_in_main_set_their_own_bit() {
        let mark = AppliedMark::new(&["settings", "media_sessions"]);
        let handle = mark.handle();
        let settings = handle.slot("settings").unwrap();
        let sessions = handle.slot("media_sessions").unwrap();
        assert!(handle.slot("items").is_none());

        mark.note_row_change("main", "items");
        mark.note_row_change("temp", "settings");
        mark.publish(Some(4));
        assert_eq!(
            (handle.revision(settings), handle.revision(sessions)),
            (0, 0)
        );
        assert_eq!(handle.applied_index(), 4);

        mark.note_row_change("main", "media_sessions");
        mark.note_row_change("main", "media_sessions");
        mark.publish(Some(5));
        assert_eq!(
            (handle.revision(settings), handle.revision(sessions)),
            (0, 1)
        );

        mark.note_every_table();
        mark.publish(None);
        assert_eq!(
            (handle.revision(settings), handle.revision(sessions)),
            (1, 2)
        );
        assert_eq!(
            handle.applied_index(),
            5,
            "a publish without an index keeps it"
        );
    }

    #[test]
    fn a_replacement_is_odd_until_completed_and_a_failed_one_stays_odd() {
        let mark = AppliedMark::new(&["settings"]);
        let handle = mark.handle();
        assert_eq!(handle.generation(), 0);
        let replacing = mark.begin_replacement();
        assert_eq!(handle.generation(), 1);
        replacing.complete();
        assert_eq!(handle.generation(), 2);

        drop(mark.begin_replacement());
        assert_eq!(handle.generation(), 3);
        assert_eq!(handle.observation().failed_replacements, 1);
        let again = mark.begin_replacement();
        assert_eq!(handle.generation(), 5, "a stuck image moves but stays odd");
        again.complete();
        assert_eq!(handle.generation(), 6);
    }

    #[test]
    fn read_pool_wait_buckets_are_cumulative() {
        let before = db_read_pool_wait();
        record_read_pool_wait(Duration::from_micros(700));
        let after = db_read_pool_wait();
        assert!(after.count > before.count);
        // 700 µs is above the 100 µs and 500 µs bounds, inside the 1 ms one.
        assert!(after.cumulative_buckets[2] > before.cumulative_buckets[2]);
        assert!(after.cumulative_buckets[7] > before.cumulative_buckets[7]);
    }
}
