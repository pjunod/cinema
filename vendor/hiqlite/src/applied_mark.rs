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
//! stays closed until a later replacement completes or the node restarts.
//!
//! Schema changes move every slot. The update hook reports a table rebuild
//! under its temporary name and a `DROP`/`RENAME`/`ALTER` not at all, so the
//! writer re-reads `PRAGMA schema_version` before every publish and bumps all
//! slots when it moved, whichever arm carried the DDL (`Transaction`,
//! `Execute`, `Batch`, `Migrate`).
//!
//! Nothing here crosses the wire; revisions are meaningless on another node.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// Registered tables a node may name in [`crate::NodeConfig::applied_mark_tables`].
pub const APPLIED_MARK_MAX_TABLES: usize = 32;

#[derive(Debug)]
pub(crate) struct AppliedMark {
    /// Even: the image is stable. Odd: it is being replaced, or a replacement
    /// failed and the image is not trusted until a later replacement
    /// completes or the node restarts.
    generation: AtomicU64,
    /// The last log index whose effects the read pool can see.
    applied_index: AtomicU64,
    /// One dedicated revision per registered table, in registration order.
    tables: [AtomicU64; APPLIED_MARK_MAX_TABLES],
    names: Vec<String>,
    /// Writer-local: set by the update hook on the writer thread, taken by
    /// `publish` on the same thread.
    pending: AtomicU32,
    /// Registered tables whose slot moves on a deleted row only (a table
    /// rewritten in place by routine activity, whose removal is the event).
    deletes_only: u32,
    /// Live [`AppliedWatch`]es per slot. `publish` notifies only when a slot
    /// it bumped has one, so a busy table nobody waits on costs a mask test.
    interest: [AtomicUsize; APPLIED_MARK_MAX_TABLES],
    /// Woken after a publish that moved a watched slot, and on every
    /// generation change.
    changed: tokio::sync::Notify,
    /// Replacements that were begun and never completed.
    failed_replacements: AtomicU64,
    /// Writer-local: the `PRAGMA schema_version` seen at the last publish,
    /// [`SCHEMA_UNKNOWN`] when it could not be read.
    schema_version: AtomicI64,
}

/// `schema_version` before the writer first reads it, or after a read failed.
const SCHEMA_UNKNOWN: i64 = i64::MIN;

impl AppliedMark {
    #[cfg(test)]
    pub(crate) fn new<S: AsRef<str>>(tables: &[S]) -> Arc<Self> {
        Self::with_deletes_only(tables, &[] as &[&str])
    }

    /// `deletes_only` names registered tables whose slot moves only when a
    /// row is deleted.
    pub(crate) fn with_deletes_only<S: AsRef<str>, D: AsRef<str>>(
        tables: &[S],
        deletes_only: &[D],
    ) -> Arc<Self> {
        assert!(
            tables.len() <= APPLIED_MARK_MAX_TABLES,
            "NodeConfig::is_valid bounds the registered applied-mark tables"
        );
        let names: Vec<String> = tables.iter().map(|name| name.as_ref().to_owned()).collect();
        let deletes_only = deletes_only.iter().fold(0_u32, |mask, table| {
            let slot = names
                .iter()
                .position(|name| name == table.as_ref())
                .expect("NodeConfig::is_valid keeps delete-only tables registered");
            mask | (1 << slot)
        });
        Arc::new(Self {
            generation: AtomicU64::new(0),
            applied_index: AtomicU64::new(0),
            tables: Default::default(),
            names,
            pending: AtomicU32::new(0),
            deletes_only,
            interest: Default::default(),
            changed: tokio::sync::Notify::new(),
            failed_replacements: AtomicU64::new(0),
            schema_version: AtomicI64::new(SCHEMA_UNKNOWN),
        })
    }

    fn all_bits(&self) -> u32 {
        if self.names.len() == APPLIED_MARK_MAX_TABLES {
            u32::MAX
        } else {
            (1_u32 << self.names.len()) - 1
        }
    }

    /// The update hook: one short compare per registered table, no hashing.
    /// Unregistered tables cost the compares and nothing else. A delete-only
    /// table ignores inserts and updates.
    pub(crate) fn note_row_change(&self, deleted: bool, database: &str, table: &str) {
        if database != "main" {
            return;
        }
        for (slot, name) in self.names.iter().enumerate() {
            if name == table {
                if deleted || self.deletes_only & (1 << slot) == 0 {
                    self.pending.fetch_or(1 << slot, Ordering::Relaxed);
                }
                return;
            }
        }
    }

    /// DDL and table rebuilds are reported by the hook under a temporary name
    /// or not at all, so the arms that carry them invalidate every slot.
    pub(crate) fn note_every_table(&self) {
        self.pending.fetch_or(self.all_bits(), Ordering::Relaxed);
    }

    /// The writer's starting schema, recorded without moving any slot.
    pub(crate) fn prime_schema_version(&self, version: Option<i64>) {
        self.schema_version
            .store(version.unwrap_or(SCHEMA_UNKNOWN), Ordering::Relaxed);
    }

    /// Called by the writer before every publish with the current
    /// `PRAGMA schema_version`: a change, or a value it could not read, moves
    /// every slot.
    pub(crate) fn note_schema_version(&self, version: Option<i64>) {
        let version = version.unwrap_or(SCHEMA_UNKNOWN);
        let previous = self.schema_version.swap(version, Ordering::Relaxed);
        if version == SCHEMA_UNKNOWN || previous != version {
            self.note_every_table();
        }
    }

    /// Bump each table the hook saw, then publish the applied index. Called
    /// after commit or rollback and before the reply, from the writer only.
    pub(crate) fn publish(&self, applied_index: Option<u64>) {
        let pending = self.pending.swap(0, Ordering::Relaxed);
        let mut watched = false;
        for (slot, revision) in self.tables.iter().enumerate().take(self.names.len()) {
            if pending & (1 << slot) != 0 {
                // SeqCst with `watch`: bump-then-load against add-then-load
                // on two atomics is store buffering; weaker orders could let
                // both sides miss each other.
                revision.fetch_add(1, Ordering::SeqCst);
                watched |= self.interest[slot].load(Ordering::SeqCst) > 0;
            }
        }
        if let Some(index) = applied_index {
            self.applied_index.store(index, Ordering::Release);
        }
        // Revisions first, then the wake: a woken watch sees what moved.
        if watched {
            self.changed.notify_waiters();
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
        self.changed.notify_waiters();
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
        self.mark.changed.notify_waiters();
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
                 odd and local views stay closed until a later replacement \
                 completes or this node restarts"
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

    /// Watch `slots` and the image generation from now. Arm the watch before
    /// the read it guards: a change applied after this call wakes it.
    #[must_use]
    pub fn watch(&self, slots: &[AppliedSlot]) -> AppliedWatch {
        for slot in slots {
            self.mark.interest[slot.0].fetch_add(1, Ordering::SeqCst);
        }
        AppliedWatch {
            mark: Arc::clone(&self.mark),
            revisions: slots
                .iter()
                .map(|slot| (*slot, self.mark.tables[slot.0].load(Ordering::SeqCst)))
                .collect(),
            generation: self.generation(),
        }
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

/// A set of slot revisions and the image generation captured by
/// [`AppliedMarkHandle::watch`].
#[derive(Debug)]
pub struct AppliedWatch {
    mark: Arc<AppliedMark>,
    revisions: Vec<(AppliedSlot, u64)>,
    generation: u64,
}

impl AppliedWatch {
    /// Whether a watched slot or the image generation moved since capture.
    #[must_use]
    pub fn moved(&self) -> bool {
        self.mark.generation.load(Ordering::Acquire) != self.generation
            || self.revisions.iter().any(|(slot, revision)| {
                self.mark.tables[slot.0].load(Ordering::Acquire) != *revision
            })
    }

    /// Whether the image was trusted (even generation) when captured.
    #[must_use]
    pub fn trusted(&self) -> bool {
        self.generation & 1 == 0
    }

    /// Returns once a watched slot or the generation moved since capture.
    pub async fn changed(&self) {
        loop {
            // Registered before the compare: `notify_waiters` stores no
            // permit, so a publish between the compare and the await would
            // otherwise be lost.
            let notified = self.mark.changed.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if self.moved() {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for AppliedWatch {
    fn drop(&mut self) {
        for (slot, _) in &self.revisions {
            self.mark.interest[slot.0].fetch_sub(1, Ordering::AcqRel);
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
    fn upper_slots_move_independently_and_full_width_invalidation_is_safe() {
        let names: Vec<_> = (0..32).map(|slot| format!("table_{slot}")).collect();
        let mark = AppliedMark::with_deletes_only(&names, &["table_31"]);
        let handle = mark.handle();
        let revision = |slot: usize| handle.revision(handle.slot(&names[slot]).unwrap());
        mark.note_row_change(false, "main", "table_16");
        mark.note_row_change(false, "main", "table_31");
        mark.publish(Some(1));
        for slot in 0..32 {
            assert_eq!(revision(slot), u64::from(slot == 16), "slot {slot}");
        }
        mark.note_row_change(true, "main", "table_31");
        mark.publish(Some(2));
        assert_eq!(revision(16), 1);
        assert_eq!(revision(31), 1);
        mark.prime_schema_version(Some(1));
        mark.note_schema_version(Some(2));
        mark.publish(Some(3));
        for slot in 0..32 {
            assert_eq!(
                revision(slot),
                1 + u64::from(slot == 16 || slot == 31),
                "slot {slot}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "bounds the registered applied-mark tables")]
    fn a_thirty_third_table_is_rejected() {
        let names: Vec<_> = (0..33).map(|slot| format!("table_{slot}")).collect();
        let _ = AppliedMark::new(&names);
    }

    #[tokio::test]
    async fn the_highest_slot_watch_ignores_updates_but_observes_deletion_and_replacement() {
        let names: Vec<_> = (0..32).map(|slot| format!("table_{slot}")).collect();
        let mark = AppliedMark::with_deletes_only(&names, &["table_31"]);
        let handle = mark.handle();
        let watch = handle.watch(&[handle.slot("table_31").unwrap()]);
        mark.note_row_change(false, "main", "table_31");
        mark.note_row_change(true, "main", "table_16");
        mark.publish(Some(1));
        assert!(!watch.moved());
        mark.note_row_change(true, "main", "table_31");
        mark.publish(Some(2));
        tokio::time::timeout(Duration::from_secs(1), watch.changed())
            .await
            .expect("high-bit deletion wake");
        let watch = handle.watch(&[handle.slot("table_31").unwrap()]);
        let replacement = mark.begin_replacement();
        tokio::time::timeout(Duration::from_secs(1), watch.changed())
            .await
            .expect("high-bit replacement wake");
        assert!(!handle.watch(&[]).trusted());
        replacement.complete();
        assert!(handle.watch(&[]).trusted());
    }

    #[test]
    fn only_registered_tables_in_main_set_their_own_bit() {
        let mark = AppliedMark::new(&["settings", "media_sessions"]);
        let handle = mark.handle();
        let settings = handle.slot("settings").unwrap();
        let sessions = handle.slot("media_sessions").unwrap();
        assert!(handle.slot("items").is_none());

        mark.note_row_change(false, "main", "items");
        mark.note_row_change(false, "temp", "settings");
        mark.publish(Some(4));
        assert_eq!(
            (handle.revision(settings), handle.revision(sessions)),
            (0, 0)
        );
        assert_eq!(handle.applied_index(), 4);

        mark.note_row_change(false, "main", "media_sessions");
        mark.note_row_change(false, "main", "media_sessions");
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
    fn a_schema_change_or_an_unreadable_schema_moves_every_slot() {
        let mark = AppliedMark::new(&["settings", "media_sessions"]);
        let handle = mark.handle();
        let slots = [
            handle.slot("settings").unwrap(),
            handle.slot("media_sessions").unwrap(),
        ];
        let revisions = || slots.map(|slot| handle.revision(slot));
        mark.prime_schema_version(Some(3));
        mark.note_schema_version(Some(3));
        mark.publish(Some(1));
        assert_eq!(
            revisions(),
            [0, 0],
            "priming and an unchanged cookie move nothing"
        );
        mark.note_schema_version(Some(4));
        mark.publish(Some(2));
        assert_eq!(revisions(), [1, 1]);
        mark.note_schema_version(None);
        mark.publish(Some(3));
        assert_eq!(
            revisions(),
            [2, 2],
            "a cookie it cannot read is treated as moved"
        );
        mark.note_schema_version(Some(4));
        mark.publish(Some(4));
        assert_eq!(revisions(), [3, 3], "and so is the first read after it");
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

#[cfg(test)]
mod watch_tests {
    use super::AppliedMark;
    use std::time::Duration;

    const TABLES: [&str; 10] = [
        "settings",
        "media_sessions",
        "t2",
        "t3",
        "t4",
        "t5",
        "t6",
        "t7",
        "sharing_imports",
        "tokens",
    ];

    fn mark() -> std::sync::Arc<AppliedMark> {
        AppliedMark::with_deletes_only(&TABLES, &["tokens"])
    }

    fn revisions(mark: &std::sync::Arc<AppliedMark>) -> Vec<u64> {
        let handle = mark.handle();
        TABLES
            .iter()
            .map(|table| handle.revision(handle.slot(table).unwrap()))
            .collect()
    }

    #[test]
    fn a_slot_past_the_eighth_moves_alone() {
        let mark = mark();
        mark.note_row_change(false, "main", "sharing_imports");
        mark.publish(Some(1));
        let mut expected = vec![0; TABLES.len()];
        expected[8] = 1;
        assert_eq!(revisions(&mark), expected);
    }

    #[test]
    fn a_delete_only_slot_moves_on_a_delete_alone() {
        let mark = mark();
        mark.note_row_change(false, "main", "tokens");
        mark.publish(Some(1));
        assert_eq!(revisions(&mark)[9], 0);
        mark.note_row_change(true, "main", "tokens");
        mark.publish(Some(2));
        assert_eq!(revisions(&mark)[9], 1);
        mark.note_every_table();
        mark.publish(Some(3));
        // A schema change still moves it: the table may have been rebuilt.
        assert_eq!(revisions(&mark)[9], 2);
    }

    #[tokio::test]
    async fn a_watch_wakes_on_its_slot_and_not_on_another() {
        let mark = mark();
        let handle = mark.handle();
        let watch = handle.watch(&[handle.slot("sharing_imports").unwrap()]);
        mark.note_row_change(false, "main", "media_sessions");
        mark.publish(Some(1));
        assert!(!watch.moved());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), watch.changed())
                .await
                .is_err()
        );
        let waker = std::sync::Arc::clone(&mark);
        let publisher = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            waker.note_row_change(true, "main", "sharing_imports");
            waker.publish(Some(2));
        });
        tokio::time::timeout(Duration::from_secs(5), watch.changed())
            .await
            .expect("the watched slot's publish wakes the watch");
        publisher.await.unwrap();
    }

    #[tokio::test]
    async fn a_change_before_the_await_is_never_lost() {
        let mark = mark();
        let handle = mark.handle();
        let watch = handle.watch(&[handle.slot("settings").unwrap()]);
        mark.note_row_change(false, "main", "settings");
        mark.publish(Some(1));
        tokio::time::timeout(Duration::from_millis(50), watch.changed())
            .await
            .expect("a change between arming and waiting returns at once");
    }

    #[test]
    fn interest_is_counted_per_watch() {
        let mark = mark();
        let handle = mark.handle();
        let slot = handle.slot("settings").unwrap();
        let first = handle.watch(&[slot]);
        let second = handle.watch(&[slot]);
        drop(first);
        assert_eq!(
            mark.interest[0].load(std::sync::atomic::Ordering::Acquire),
            1
        );
        drop(second);
        assert_eq!(
            mark.interest[0].load(std::sync::atomic::Ordering::Acquire),
            0
        );
    }

    #[tokio::test]
    async fn an_image_replacement_wakes_every_watch_and_untrusts_the_next() {
        let mark = mark();
        let handle = mark.handle();
        let watch = handle.watch(&[handle.slot("settings").unwrap()]);
        assert!(watch.trusted());
        let replacement = mark.begin_replacement();
        tokio::time::timeout(Duration::from_millis(50), watch.changed())
            .await
            .expect("a replacement wakes the watch");
        assert!(!handle.watch(&[]).trusted());
        replacement.complete();
        assert!(handle.watch(&[]).trusted());
    }
}
