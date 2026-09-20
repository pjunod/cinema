//! One authoritative charge per rolling scratch incarnation.
//!
//! The predecessor of this module was a sum: fold the live session map, fold
//! the retired map, then infer "provisional" work by subtracting both folds
//! from a free-running atomic. Every one of those three steps was a separate
//! critical section, so a session that moved between the folds was counted
//! twice, its reservation was subtracted twice when deriving provisional
//! usage, and the zero clamp that hid the negative result also hid genuinely
//! provisional starts. A start could be refused because a *retiring* session
//! was counted in both registries.
//!
//! The replacement is a ledger. One entry per exact incarnation, created
//! before a producer can make bytes and destroyed only when its names are
//! gone and its readers have closed. Admission, growth, conversion and
//! release all linearize on the same short critical section, and the sum is
//! over entries — registry membership is not part of the charge formula.
//!
//! What an entry charges:
//!
//! ```text
//! charge = max(grant_bytes, used_bytes) + unlinked_pinned_bytes
//! ```
//!
//! * `grant_bytes` is future capacity this incarnation is authorized to
//!   materialize. Admission reserves it; [`ScratchLedger::grow`] raises it;
//!   [`ScratchLedger::commit_quiescent_measurement`] collapses it onto the
//!   measured inventory once no writer can consume it.
//! * `used_bytes` is the apparent length of the regular files this
//!   incarnation actually has in its scratch directory. It is a measurement,
//!   so it can exceed the grant — an overrun is charged, never clamped down
//!   to the reservation that failed to contain it. Because it is only
//!   refreshed on a directory walk, two further terms carry what the walk
//!   has not seen yet: `pending_bytes`, authorized for writes that are in
//!   flight, and `written_bytes`, for writes that landed since the last
//!   complete measurement. Without them an authorization would be
//!   re-usable an unlimited number of times per scan interval, which is a
//!   timing bound dressed up as a write bound.
//! * `unlinked_pinned_bytes` is the apparent length of objects whose names
//!   have been removed while an accepted read still holds them open. A
//!   directory scan cannot see those bytes; the filesystem still owes them.
//!
//! These are logical, apparent lengths in one namespace — `metadata.len()`,
//! not `st_blocks`. Compression, sparse files and allocation granularity all
//! make them differ from physical reclamation, and the physical free-space
//! sample remains a separate guard.
//!
//! Nothing here awaits. The lock is a `std::sync::Mutex` held across a few
//! map operations and integer adds, never across filesystem I/O, a process
//! wait, an actor exchange or a grace timer. The one asynchronous affordance
//! is [`WriterBarrier`], which hands the caller a `Notify` to await *outside*
//! the lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};

/// Process-local identity of one scratch allocation.
///
/// Minted before a producer or a session id exists, so a start that has not
/// chosen its uuid yet can still hold capacity. Session ids are reused across
/// incarnations in cluster recovery; this never is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ScratchKey(u64);

impl std::fmt::Display for ScratchKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "scratch-{}", self.0)
    }
}

/// Where an allocation is in its life. Exactly one applies at a time, so the
/// per-state sums in a [`ScratchSnapshot`] add up to its total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScratchLifecycle {
    /// Admitted, but not yet visible in any registry. An actual unpublished
    /// start — never cleanup residue.
    Provisional,
    /// Registered and producing.
    Producing,
    /// Retirement began. New writers are fenced; the ones already registered
    /// still hold the conservative charge.
    Retiring,
    /// Writers settled and the final inventory measured. The charge is the
    /// measured retained bytes plus any reader pins.
    Retained,
    /// Cleanup owns the names. Bytes stay charged until deletion is proven.
    Releasing,
}

impl ScratchLifecycle {
    fn label(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::Producing => "producing",
            Self::Retiring => "retiring",
            Self::Retained => "retained",
            Self::Releasing => "releasing",
        }
    }
}

/// Why an admission was refused, in the units the operator needs to see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScratchRefusal {
    pub charged: i64,
    pub requested: i64,
    pub configured: i64,
}

impl std::fmt::Display for ScratchRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "rolling scratch capacity unavailable: {} bytes charged, {} requested, {} configured",
            self.charged, self.requested, self.configured
        )
    }
}

/// The categories of one consistent read of the ledger.
///
/// `total` is what admission compares against the configured ceiling. The
/// five lifecycle figures partition it; `grant_unused` and `pinned` describe
/// the same bytes from another angle and deliberately overlap them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScratchSnapshot {
    pub generation: u64,
    pub total: i64,
    pub provisional: i64,
    pub producing: i64,
    pub retiring: i64,
    pub retained: i64,
    pub releasing: i64,
    pub grant_unused: i64,
    pub pinned: i64,
    pub entries: usize,
    pub writers: usize,
}

/// One tracked object, grouped so concurrent range readers of the same bytes
/// charge once.
///
/// Grouping by `(name, length)` rather than by inode is deliberate: the
/// ledger never opens the file, and two objects that share a name *and* an
/// apparent length inside one incarnation are byte-identical in every case
/// this serves (a republished playlist changes length). It can over-count a
/// coincidence, never under-count a distinct object.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PinObject {
    name: String,
    bytes: i64,
}

#[derive(Clone, Copy, Debug)]
struct PinGroup {
    bytes: i64,
    readers: usize,
    /// While the name is still in the directory the scanner already counts
    /// these bytes; only an unlinked-but-pinned object adds to the charge.
    linked: bool,
}

struct Entry {
    lifecycle: ScratchLifecycle,
    session_id: Option<String>,
    attempt: u64,
    grant_bytes: i64,
    used_bytes: i64,
    /// Authorized for writes that have not landed yet. Debited when the
    /// write settles, so N consecutive writes between two measurements each
    /// consume allowance instead of all passing the same stale comparison.
    pending_bytes: i64,
    /// Landed since the last complete measurement. Folded in until a scan
    /// subsumes it, so the window between a write and the next directory
    /// walk is charged rather than free.
    written_bytes: i64,
    pins: HashMap<PinObject, PinGroup>,
    /// Registered scratch writers that have not proven they can no longer
    /// write. Retirement fences new ones and waits for these.
    writers: usize,
    next_writer: u64,
    live_writers: Vec<u64>,
    /// Bumped whenever a writer registers or finishes. A final measurement
    /// commits only if the generation it was taken under still holds.
    inventory_generation: u64,
    /// New writer registration is refused once retirement begins.
    writers_fenced: bool,
    /// The owning permit is gone. The entry survives until cleanup releases
    /// it, because a dropped `Arc` is not proof that a file disappeared.
    owner_dropped: bool,
    /// Why the last conversion attempt kept the conservative charge.
    conservative_reason: Option<&'static str>,
    barrier: Arc<tokio::sync::Notify>,
}

impl Entry {
    fn pinned_extra(&self) -> i64 {
        self.pins
            .values()
            .filter(|group| !group.linked)
            .fold(0_i64, |total, group| total.saturating_add(group.bytes))
    }

    /// Everything this incarnation has, owes or is authorized to make.
    fn materialized(&self) -> i64 {
        self.used_bytes
            .saturating_add(self.written_bytes)
            .saturating_add(self.pending_bytes)
    }

    fn charge(&self) -> i64 {
        self.grant_bytes
            .max(self.materialized())
            .saturating_add(self.pinned_extra())
    }

    fn grant_unused(&self) -> i64 {
        (self.grant_bytes - self.materialized()).max(0)
    }

    /// Nothing can write here, nothing is linked, nobody is reading, and
    /// either the owner is gone or cleanup has proved the names are. The only
    /// state in which forgetting the entry is honest.
    fn releasable(&self) -> bool {
        self.writers == 0
            && self.materialized() == 0
            && self.grant_bytes == 0
            && self.pins.is_empty()
            && (self.owner_dropped || self.lifecycle == ScratchLifecycle::Releasing)
    }
}

struct LedgerState {
    generation: u64,
    next_key: u64,
    entries: HashMap<ScratchKey, Entry>,
}

impl LedgerState {
    fn total(&self) -> i64 {
        self.entries
            .values()
            .fold(0_i64, |total, entry| total.saturating_add(entry.charge()))
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

/// The scratch accounting authority for one `TranscodeManager`.
pub(crate) struct ScratchLedger {
    state: Mutex<LedgerState>,
    /// Test seam: how many times a measurement was refused because the
    /// inventory generation moved under it.
    stale_measurements: AtomicU64,
}

impl ScratchLedger {
    pub(crate) fn new() -> Arc<ScratchLedger> {
        Arc::new(ScratchLedger {
            state: Mutex::new(LedgerState {
                generation: 0,
                next_key: 1,
                entries: HashMap::new(),
            }),
            stale_measurements: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admit `grant` bytes of future capacity, or refuse with the exact
    /// figures. `configured <= 0` disables the ceiling and admits a zero
    /// charge so callers keep one uniform permit type.
    ///
    /// This is the linearization point for admission: the check and the add
    /// happen under one lock, so two starts racing an empty directory cannot
    /// both observe the same remaining capacity.
    pub(crate) fn reserve(
        self: &Arc<Self>,
        grant: i64,
        configured: i64,
    ) -> Result<ScratchPermit, ScratchRefusal> {
        let mut state = self.lock();
        let grant = if configured <= 0 { 0 } else { grant.max(0) };
        let charged = state.total();
        if configured > 0 {
            let projected = charged.checked_add(grant).unwrap_or(i64::MAX);
            if projected > configured {
                return Err(ScratchRefusal {
                    charged,
                    requested: grant,
                    configured,
                });
            }
        }
        let key = ScratchKey(state.next_key);
        state.next_key = state.next_key.saturating_add(1);
        state.entries.insert(
            key,
            Entry {
                lifecycle: ScratchLifecycle::Provisional,
                session_id: None,
                attempt: 0,
                grant_bytes: grant,
                used_bytes: 0,
                pending_bytes: 0,
                written_bytes: 0,
                pins: HashMap::new(),
                writers: 0,
                next_writer: 1,
                live_writers: Vec::new(),
                inventory_generation: 1,
                writers_fenced: false,
                owner_dropped: false,
                conservative_reason: None,
                barrier: Arc::new(tokio::sync::Notify::new()),
            },
        );
        state.touch();
        drop(state);
        Ok(ScratchPermit {
            ledger: Arc::clone(self),
            key,
            bytes: grant,
        })
    }

    /// Name the incarnation this allocation belongs to, without releasing and
    /// reacquiring the capacity. The session id is minted after admission, so
    /// binding is the only honest way to keep one charge across the gap.
    pub(crate) fn bind_session(&self, key: ScratchKey, session_id: &str, attempt: u64) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.session_id = Some(session_id.to_owned());
            entry.attempt = attempt;
            if entry.lifecycle == ScratchLifecycle::Provisional {
                entry.lifecycle = ScratchLifecycle::Producing;
            }
        }
        state.touch();
    }

    /// Register a writer before it can make bytes. `None` means retirement
    /// already fenced this allocation and the caller must not start writing.
    pub(crate) fn register_writer(self: &Arc<Self>, key: ScratchKey) -> Option<ScratchWriter> {
        let mut state = self.lock();
        let entry = state.entries.get_mut(&key)?;
        if entry.writers_fenced {
            return None;
        }
        let id = entry.next_writer;
        entry.next_writer = entry.next_writer.saturating_add(1);
        entry.writers += 1;
        entry.live_writers.push(id);
        entry.inventory_generation = entry.inventory_generation.saturating_add(1);
        state.touch();
        drop(state);
        Some(ScratchWriter {
            ledger: Arc::clone(self),
            key,
            id,
        })
    }

    fn finish_writer(&self, key: ScratchKey, id: u64) {
        let mut state = self.lock();
        let Some(entry) = state.entries.get_mut(&key) else {
            return;
        };
        let Some(at) = entry.live_writers.iter().position(|live| *live == id) else {
            return;
        };
        entry.live_writers.swap_remove(at);
        entry.writers = entry.writers.saturating_sub(1);
        entry.inventory_generation = entry.inventory_generation.saturating_add(1);
        let barrier = Arc::clone(&entry.barrier);
        let settled = entry.writers == 0;
        state.touch();
        drop(state);
        if settled {
            barrier.notify_waiters();
            // An entry whose names were already released while a writer was
            // still registered becomes collectable only now. Nothing else
            // would ever look at it again.
            self.collect(key);
        }
    }

    /// Fence new writers and hand back the means to wait for the registered
    /// ones. The charge is untouched: retirement is not evidence that any
    /// byte disappeared.
    pub(crate) fn begin_retirement(&self, key: ScratchKey) -> Option<WriterBarrier> {
        let mut state = self.lock();
        let entry = state.entries.get_mut(&key)?;
        entry.writers_fenced = true;
        if matches!(
            entry.lifecycle,
            ScratchLifecycle::Provisional | ScratchLifecycle::Producing
        ) {
            entry.lifecycle = ScratchLifecycle::Retiring;
        }
        let barrier = WriterBarrier {
            notify: Arc::clone(&entry.barrier),
            settled: entry.writers == 0,
        };
        state.touch();
        Some(barrier)
    }

    pub(crate) fn writers_settled(&self, key: ScratchKey) -> bool {
        self.lock()
            .entries
            .get(&key)
            .is_none_or(|entry| entry.writers == 0)
    }

    pub(crate) fn inventory_generation(&self, key: ScratchKey) -> Option<u64> {
        self.lock()
            .entries
            .get(&key)
            .map(|entry| entry.inventory_generation)
    }

    /// Record a measurement taken while the producer is still running. It can
    /// only raise the charge — a periodic scan is not evidence that future
    /// capacity is no longer needed.
    pub(crate) fn observe_used(&self, key: ScratchKey, bytes: i64) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.used_bytes = bytes.max(0);
            // The walk saw the directory as it is now, so everything that had
            // landed is in that number. Writes still in flight are not.
            entry.written_bytes = 0;
        }
        state.touch();
    }

    /// Collapse future capacity onto a complete final inventory.
    ///
    /// Refused — leaving the conservative charge in place — when a writer is
    /// still registered or when the inventory generation moved while the
    /// directory was being walked outside the lock. Both are the same class
    /// of mistake: believing a scan that raced a write.
    pub(crate) fn commit_quiescent_measurement(
        &self,
        key: ScratchKey,
        inventory_generation: u64,
        bytes: i64,
    ) -> bool {
        let mut state = self.lock();
        let Some(entry) = state.entries.get_mut(&key) else {
            return false;
        };
        if entry.writers > 0 {
            entry.conservative_reason = Some("writers_outstanding");
            return false;
        }
        if entry.inventory_generation != inventory_generation {
            entry.conservative_reason = Some("inventory_moved");
            drop(state);
            self.stale_measurements.fetch_add(1, Relaxed);
            return false;
        }
        let measured = bytes.max(0);
        entry.used_bytes = measured;
        entry.written_bytes = 0;
        entry.grant_bytes = measured;
        entry.conservative_reason = None;
        if entry.lifecycle == ScratchLifecycle::Retiring {
            entry.lifecycle = ScratchLifecycle::Retained;
        }
        state.touch();
        true
    }

    /// Keep the conservative charge and say why. Used when the scan could not
    /// complete, so an operator can tell a stalled writer from a full disk.
    pub(crate) fn note_conservative(&self, key: ScratchKey, reason: &'static str) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.conservative_reason = Some(reason);
        }
    }

    pub(crate) fn conservative_reason(&self, key: ScratchKey) -> Option<&'static str> {
        self.lock()
            .entries
            .get(&key)
            .and_then(|entry| entry.conservative_reason)
    }

    /// Raise the authorized ceiling, if the global budget has room. Returns
    /// the granted total, or `None` when the budget refuses the growth and
    /// the caller must keep its producer held.
    pub(crate) fn grow(&self, key: ScratchKey, additional: i64, configured: i64) -> Option<i64> {
        if additional <= 0 {
            return self.lock().entries.get(&key).map(|entry| entry.grant_bytes);
        }
        let mut state = self.lock();
        let charged = state.total();
        let entry = state.entries.get(&key)?;
        if entry.writers_fenced {
            return None;
        }
        let before = entry.charge();
        let raised = entry.grant_bytes.checked_add(additional)?;
        let after = raised
            .max(entry.materialized())
            .saturating_add(entry.pinned_extra());
        if configured > 0 {
            let projected = charged
                .saturating_sub(before)
                .checked_add(after)
                .unwrap_or(i64::MAX);
            if projected > configured {
                return None;
            }
        }
        let entry = state.entries.get_mut(&key)?;
        entry.grant_bytes = raised;
        state.touch();
        Some(raised)
    }

    /// Re-authorize a producing allocation to `measured + envelope`.
    ///
    /// This is the growth half of `charge = actual + envelope`. It runs on
    /// the flow evaluation, so the authorized ceiling tracks what the
    /// producer has actually written rather than ratcheting up forever, and
    /// a session that has drained its retention gives capacity back to the
    /// budget without waiting for retirement.
    ///
    /// Returns false when the budget cannot carry the envelope. The caller
    /// keeps its producer held: a denied grant is a hold, never a licence to
    /// write into space nobody accounted for. The grant is never lowered
    /// below what is already on the disk.
    pub(crate) fn regrant(&self, key: ScratchKey, envelope: i64, configured: i64) -> bool {
        let mut state = self.lock();
        let charged = state.total();
        let Some(entry) = state.entries.get(&key) else {
            return false;
        };
        if entry.writers_fenced {
            return false;
        }
        let before = entry.charge();
        // Never below what is already materialized or authorized: pulling the
        // ceiling in under an in-flight write would hand the budget capacity
        // a writer is about to spend, and two starts would then be admitted
        // against the same bytes.
        let wanted = entry
            .used_bytes
            .saturating_add(envelope.max(0))
            .max(entry.materialized());
        let after = wanted
            .max(entry.materialized())
            .saturating_add(entry.pinned_extra());
        if configured > 0 {
            let projected = charged
                .saturating_sub(before)
                .checked_add(after)
                .unwrap_or(i64::MAX);
            if projected > configured {
                // Keep whatever is already granted — pulling it in under a
                // writer would leave issued obligations uncharged.
                return false;
            }
        }
        let Some(entry) = state.entries.get_mut(&key) else {
            return false;
        };
        entry.grant_bytes = wanted;
        state.touch();
        true
    }

    /// Reserve `bytes` for one imminent write, growing the ceiling if the
    /// budget allows, and hold the reservation until the write settles.
    ///
    /// This is the whole write bound. It debits `pending_bytes` under the
    /// same lock that checks the allowance, so N consecutive writes between
    /// two directory walks each consume capacity rather than all passing the
    /// same stale comparison — which is what makes "the writer waits" true
    /// rather than "the writer waits once per scan interval".
    ///
    /// `None` is the signal to backpressure. A fenced allocation always
    /// refuses: retirement has decided nothing more may be written here, and
    /// a writer parked on a grant that can never be issued is a stall.
    pub(crate) fn authorize_write(
        self: &Arc<Self>,
        key: ScratchKey,
        bytes: i64,
        configured: i64,
        headroom: i64,
    ) -> Option<ScratchWrite> {
        let bytes = bytes.max(0);
        let shortfall = {
            let mut state = self.lock();
            let entry = state.entries.get_mut(&key)?;
            if entry.writers_fenced {
                return None;
            }
            if entry.grant_unused() >= bytes {
                entry.pending_bytes = entry.pending_bytes.saturating_add(bytes);
                state.touch();
                return Some(ScratchWrite {
                    ledger: Arc::clone(self),
                    key,
                    bytes,
                    settled: false,
                });
            }
            bytes.saturating_sub(entry.grant_unused())
        };
        self.grow(key, shortfall.saturating_add(headroom.max(0)), configured)?;
        let mut state = self.lock();
        let entry = state.entries.get_mut(&key)?;
        if entry.writers_fenced || entry.grant_unused() < bytes {
            // A concurrent regrant or fence moved under us between the grow
            // and here. Refuse rather than write into space the ledger did
            // not keep.
            return None;
        }
        entry.pending_bytes = entry.pending_bytes.saturating_add(bytes);
        state.touch();
        drop(state);
        Some(ScratchWrite {
            ledger: Arc::clone(self),
            key,
            bytes,
            settled: false,
        })
    }

    /// The write landed (or did not). Move the reservation out of
    /// `pending_bytes`; a write that produced bytes keeps them charged until
    /// the next complete measurement subsumes them.
    fn settle_write(&self, key: ScratchKey, bytes: i64, written: i64) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.pending_bytes = (entry.pending_bytes - bytes).max(0);
            entry.written_bytes = entry.written_bytes.saturating_add(written.max(0));
        }
        state.touch();
    }

    pub(crate) fn writers_fenced(&self, key: ScratchKey) -> bool {
        self.lock()
            .entries
            .get(&key)
            .is_none_or(|entry| entry.writers_fenced)
    }

    /// Track an object an accepted read has opened. Concurrent range readers
    /// of the same object share one charge.
    pub(crate) fn acquire_pin(
        self: &Arc<Self>,
        key: ScratchKey,
        name: &str,
        bytes: i64,
    ) -> Option<ScratchPin> {
        if bytes < 0 {
            return None;
        }
        let object = PinObject {
            name: name.to_owned(),
            bytes,
        };
        let mut state = self.lock();
        let entry = state.entries.get_mut(&key)?;
        // A pin taken after cleanup has removed the names is an
        // unlinked-but-open object from the start: the directory walk can
        // never see it again, so it has to carry its own charge.
        let linked = entry.lifecycle != ScratchLifecycle::Releasing;
        let group = entry.pins.entry(object.clone()).or_insert(PinGroup {
            bytes,
            readers: 0,
            linked,
        });
        group.readers += 1;
        state.touch();
        drop(state);
        Some(ScratchPin {
            ledger: Arc::clone(self),
            key,
            object,
        })
    }

    fn release_pin(&self, key: ScratchKey, object: &PinObject) {
        let mut state = self.lock();
        let Some(entry) = state.entries.get_mut(&key) else {
            return;
        };
        let drop_group = match entry.pins.get_mut(object) {
            Some(group) => {
                group.readers = group.readers.saturating_sub(1);
                group.readers == 0
            }
            None => false,
        };
        if drop_group {
            entry.pins.remove(object);
        }
        state.touch();
        drop(state);
        self.collect(key);
    }

    /// Every name this allocation owned has been removed from the directory.
    /// Objects still held open move from namespace ownership to reader-pin
    /// ownership; everything else stops being charged.
    pub(crate) fn account_unlink_all(&self, key: ScratchKey) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.pins.retain(|_, group| group.readers > 0);
            for group in entry.pins.values_mut() {
                group.linked = false;
            }
            entry.used_bytes = 0;
            entry.written_bytes = 0;
            entry.grant_bytes = 0;
            entry.lifecycle = ScratchLifecycle::Releasing;
        }
        state.touch();
        drop(state);
        self.collect(key);
    }

    /// Cleanup has taken ownership of the names but has not proven they are
    /// gone. Bytes stay charged; only the category changes.
    pub(crate) fn begin_release(&self, key: ScratchKey) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.writers_fenced = true;
            entry.lifecycle = ScratchLifecycle::Releasing;
        }
        state.touch();
    }

    /// Forget the allocation once nothing can write it, no name remains and
    /// no reader holds it. Idempotent: a duplicate cleanup completion cannot
    /// subtract twice, because the second call finds no entry.
    fn collect(&self, key: ScratchKey) {
        let mut state = self.lock();
        let releasable = state
            .entries
            .get(&key)
            .is_some_and(|entry| entry.releasable());
        if releasable {
            state.entries.remove(&key);
            state.touch();
        }
    }

    fn abandon(&self, key: ScratchKey) {
        {
            let mut state = self.lock();
            let Some(entry) = state.entries.get_mut(&key) else {
                return;
            };
            entry.owner_dropped = true;
            // A start that never reached a registry has no cleanup owner of
            // its own — its directory, if it made one, goes to the same
            // startup/maintenance orphan sweep it always did. Keeping its
            // charge would be a ledger leak with nobody left to settle it.
            if entry.lifecycle == ScratchLifecycle::Provisional
                && entry.writers == 0
                && entry.pins.is_empty()
            {
                state.entries.remove(&key);
            }
            state.touch();
        }
        self.collect(key);
    }

    /// One consistent read. Every figure below comes from the same lock
    /// acquisition, so the categories add to the total a refusal quotes.
    pub(crate) fn snapshot(&self) -> ScratchSnapshot {
        let state = self.lock();
        let mut out = ScratchSnapshot {
            generation: state.generation,
            entries: state.entries.len(),
            ..ScratchSnapshot::default()
        };
        for entry in state.entries.values() {
            let charge = entry.charge();
            out.total = out.total.saturating_add(charge);
            out.grant_unused = out.grant_unused.saturating_add(entry.grant_unused());
            out.pinned = out.pinned.saturating_add(entry.pinned_extra());
            out.writers = out.writers.saturating_add(entry.writers);
            let bucket = match entry.lifecycle {
                ScratchLifecycle::Provisional => &mut out.provisional,
                ScratchLifecycle::Producing => &mut out.producing,
                ScratchLifecycle::Retiring => &mut out.retiring,
                ScratchLifecycle::Retained => &mut out.retained,
                ScratchLifecycle::Releasing => &mut out.releasing,
            };
            *bucket = bucket.saturating_add(charge);
        }
        out
    }

    /// True when this allocation has materialized everything it is
    /// authorized to. The producer must stay held until a regrant succeeds.
    pub(crate) fn grant_exhausted(&self, key: ScratchKey) -> bool {
        self.lock()
            .entries
            .get(&key)
            .is_some_and(|entry| entry.materialized() >= entry.grant_bytes)
    }

    pub(crate) fn grant_of(&self, key: ScratchKey) -> Option<i64> {
        self.lock().entries.get(&key).map(|entry| entry.grant_bytes)
    }

    /// The charge one allocation contributes, for diagnostics and tests.
    pub(crate) fn charge_of(&self, key: ScratchKey) -> Option<i64> {
        self.lock().entries.get(&key).map(Entry::charge)
    }

    pub(crate) fn lifecycle_of(&self, key: ScratchKey) -> Option<ScratchLifecycle> {
        self.lock().entries.get(&key).map(|entry| entry.lifecycle)
    }

    /// A bounded, operator-facing description of what is holding the budget.
    pub(crate) fn describe(&self, limit: usize) -> Vec<String> {
        let state = self.lock();
        let mut rows: Vec<_> = state
            .entries
            .iter()
            .map(|(key, entry)| {
                (
                    entry.charge(),
                    format!(
                        "{key} {state} charge={} used={} grant={} pinned={} writers={}{}{}",
                        entry.charge(),
                        entry.used_bytes,
                        entry.grant_bytes,
                        entry.pinned_extra(),
                        entry.writers,
                        entry
                            .session_id
                            .as_deref()
                            .map(|id| format!(" session={id}"))
                            .unwrap_or_default(),
                        entry
                            .conservative_reason
                            .map(|reason| format!(" held={reason}"))
                            .unwrap_or_default(),
                        state = entry.lifecycle.label(),
                    ),
                )
            })
            .collect();
        rows.sort_by(|left, right| right.0.cmp(&left.0));
        rows.into_iter()
            .take(limit)
            .map(|(_, row)| row)
            .collect::<Vec<_>>()
    }

    #[cfg(test)]
    pub(crate) fn stale_measurements(&self) -> u64 {
        self.stale_measurements.load(Relaxed)
    }
}

/// The admission grant, owned by the start and then by the session.
///
/// Dropping it is a backstop, not the event that returns capacity. A start
/// that failed before it made a byte releases immediately; anything with a
/// writer, a file or a reader stays charged until cleanup proves otherwise.
pub(crate) struct ScratchPermit {
    ledger: Arc<ScratchLedger>,
    key: ScratchKey,
    bytes: i64,
}

impl ScratchPermit {
    pub(crate) fn key(&self) -> ScratchKey {
        self.key
    }

    /// Name the incarnation this grant belongs to. Consuming and returning
    /// `self` keeps the charge unbroken across the moment the session id is
    /// minted — the alternative, releasing and reacquiring, is a window in
    /// which another start can take the capacity this one already holds.
    pub(crate) fn bound_to(self, session_id: &str, attempt: u64) -> Self {
        self.ledger.bind_session(self.key, session_id, attempt);
        self
    }

    /// The bytes admission charged. Diagnostics only — the live charge is
    /// [`ScratchLedger::charge_of`].
    pub(crate) fn admitted_bytes(&self) -> i64 {
        self.bytes
    }

    pub(crate) fn ledger(&self) -> &Arc<ScratchLedger> {
        &self.ledger
    }
}

impl std::fmt::Debug for ScratchPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ScratchPermit({}, {} bytes)", self.key, self.bytes)
    }
}

impl Drop for ScratchPermit {
    fn drop(&mut self) {
        self.ledger.abandon(self.key);
    }
}

/// Proof that one scratch writer has not yet finished. Installed *before* the
/// writer can make bytes, so retirement can never observe zero writers in the
/// window between spawning and the first write.
pub(crate) struct ScratchWriter {
    ledger: Arc<ScratchLedger>,
    key: ScratchKey,
    id: u64,
}

impl Drop for ScratchWriter {
    fn drop(&mut self) {
        self.ledger.finish_writer(self.key, self.id);
    }
}

/// The means to await writer settlement outside the ledger lock.
pub(crate) struct WriterBarrier {
    notify: Arc<tokio::sync::Notify>,
    settled: bool,
}

impl WriterBarrier {
    /// Wait until no registered writer remains, or the deadline passes.
    /// A timeout is reported, never assumed to be settlement: a stalled
    /// writer keeps the conservative charge.
    pub(crate) async fn settle(
        self,
        ledger: &ScratchLedger,
        key: ScratchKey,
        deadline: tokio::time::Instant,
    ) -> bool {
        if self.settled {
            return true;
        }
        loop {
            // Register interest before re-reading, or a `finish_writer`
            // between the read and the await is a lost wakeup.
            let waiting = self.notify.notified();
            if ledger.writers_settled(key) {
                return true;
            }
            if tokio::time::timeout_at(deadline, waiting).await.is_err() {
                return ledger.writers_settled(key);
            }
        }
    }
}

/// One authorized, not-yet-landed write.
///
/// Held from before the temporary file is created until after the rename,
/// and settled exactly once: `landed` charges what was actually produced,
/// dropping it without landing returns the reservation. The bytes it holds
/// are part of the charge for its whole life, so a second write cannot be
/// authorized against them.
pub(crate) struct ScratchWrite {
    ledger: Arc<ScratchLedger>,
    key: ScratchKey,
    bytes: i64,
    settled: bool,
}

impl ScratchWrite {
    pub(crate) fn landed(mut self, written: i64) {
        self.settled = true;
        self.ledger.settle_write(self.key, self.bytes, written);
    }
}

impl Drop for ScratchWrite {
    fn drop(&mut self) {
        if !self.settled {
            self.ledger.settle_write(self.key, self.bytes, 0);
        }
    }
}

/// An accepted read holding one object open. Dropping it — at EOF, on
/// cancellation, or on error — settles the pin exactly once.
pub(crate) struct ScratchPin {
    ledger: Arc<ScratchLedger>,
    key: ScratchKey,
    object: PinObject,
}

impl Drop for ScratchPin {
    fn drop(&mut self) {
        self.ledger.release_pin(self.key, &self.object);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: i64 = 8 * 1024 * 1024 * 1024;
    const R: i64 = 2 * 1024 * 1024 * 1024 + 64 * 1024 * 1024;

    #[test]
    fn scratch_charge_admission_serializes_and_refuses_with_exact_figures() {
        let ledger = ScratchLedger::new();
        let first = ledger.reserve(R, CAP).expect("first admission");
        let second = ledger.reserve(R, CAP).expect("second admission");
        let third = ledger.reserve(R, CAP).expect("third admission");
        let refusal = ledger.reserve(R, CAP).expect_err("fourth must not fit");
        assert_eq!(refusal.requested, R);
        assert_eq!(refusal.configured, CAP);
        assert_eq!(refusal.charged, 3 * R);
        assert_eq!(ledger.snapshot().total, 3 * R);
        drop(third);
        assert_eq!(ledger.snapshot().total, 2 * R);
        drop((first, second));
        assert_eq!(ledger.snapshot().total, 0);
        assert_eq!(ledger.snapshot().entries, 0);
    }

    #[test]
    fn scratch_charge_counts_one_incarnation_once_across_registry_transfer() {
        // The defect this replaces: the live fold and the retired fold were
        // separate critical sections, and retirement inserts into retired
        // before removing from live. A ledger has no registries to disagree.
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        ledger.bind_session(permit.key(), "s1", 1);
        let provisional = ledger.reserve(R, CAP).expect("a genuinely provisional start");
        let before = ledger.snapshot();
        assert_eq!(before.producing, R);
        assert_eq!(before.provisional, R);
        assert_eq!(before.total, 2 * R);

        ledger.begin_retirement(permit.key());
        let during = ledger.snapshot();
        assert_eq!(during.total, 2 * R, "retirement moves a category, not bytes");
        assert_eq!(during.retiring, R);
        assert_eq!(
            during.provisional, R,
            "the provisional start must not be hidden by a subtraction"
        );
        drop((permit, provisional));
    }

    #[test]
    fn scratch_charge_keeps_the_producer_charge_until_writers_settle() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        let key = permit.key();
        ledger.bind_session(key, "s1", 1);
        let writer = ledger.register_writer(key).expect("writer registers");
        let generation = ledger.inventory_generation(key).expect("generation");
        ledger.begin_retirement(key);
        assert!(
            ledger.register_writer(key).is_none(),
            "retirement fences new writers"
        );
        assert!(!ledger.commit_quiescent_measurement(key, generation, 1024));
        assert_eq!(ledger.charge_of(key), Some(R));
        assert_eq!(
            ledger.conservative_reason(key),
            Some("writers_outstanding")
        );

        drop(writer);
        let settled = ledger.inventory_generation(key).expect("generation");
        assert!(ledger.commit_quiescent_measurement(key, settled, 1024));
        assert_eq!(ledger.charge_of(key), Some(1024));
        assert_eq!(ledger.lifecycle_of(key), Some(ScratchLifecycle::Retained));
        drop(permit);
    }

    #[test]
    fn scratch_charge_refuses_a_measurement_that_raced_a_writer() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        let key = permit.key();
        let generation = ledger.inventory_generation(key).expect("generation");
        // A writer registered and finished while the directory was walked.
        let writer = ledger.register_writer(key).expect("writer");
        drop(writer);
        assert!(!ledger.commit_quiescent_measurement(key, generation, 10));
        assert_eq!(ledger.charge_of(key), Some(R));
        assert_eq!(ledger.stale_measurements(), 1);
        let current = ledger.inventory_generation(key).expect("generation");
        assert!(ledger.commit_quiescent_measurement(key, current, 10));
        assert_eq!(ledger.charge_of(key), Some(10));
        drop(permit);
    }

    #[test]
    fn scratch_charge_measures_an_overrun_instead_of_clamping_it() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(1024, CAP).expect("admission");
        let key = permit.key();
        ledger.observe_used(key, 4096);
        assert_eq!(ledger.charge_of(key), Some(4096));
        let generation = ledger.inventory_generation(key).expect("generation");
        assert!(ledger.commit_quiescent_measurement(key, generation, 4096));
        assert_eq!(ledger.charge_of(key), Some(4096));
        drop(permit);
    }

    #[test]
    fn scratch_charge_survives_the_owner_and_releases_once_cleanup_proves_it() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        let key = permit.key();
        ledger.bind_session(key, "s1", 1);
        ledger.observe_used(key, 4096);
        ledger.begin_retirement(key);
        let generation = ledger.inventory_generation(key).expect("generation");
        assert!(ledger.commit_quiescent_measurement(key, generation, 4096));
        // An unrelated status reader still holds the session; the owner is
        // gone. Dropping it must not free bytes that are still on the disk.
        drop(permit);
        assert_eq!(ledger.charge_of(key), Some(4096));
        ledger.begin_release(key);
        assert_eq!(ledger.charge_of(key), Some(4096));
        ledger.account_unlink_all(key);
        assert_eq!(ledger.charge_of(key), None, "released exactly once");
        assert_eq!(ledger.snapshot().total, 0);
        // A duplicate completion cannot subtract twice.
        ledger.account_unlink_all(key);
        assert_eq!(ledger.snapshot().total, 0);
    }

    #[test]
    fn scratch_charge_keeps_an_unlinked_object_charged_until_its_last_reader_closes() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        let key = permit.key();
        ledger.observe_used(key, 3_000);
        let generation = ledger.inventory_generation(key).expect("generation");
        ledger.begin_retirement(key);
        assert!(ledger.commit_quiescent_measurement(key, generation, 3_000));
        let first = ledger
            .acquire_pin(key, "seg00001.m4s", 2_000)
            .expect("a range reader");
        let second = ledger
            .acquire_pin(key, "seg00001.m4s", 2_000)
            .expect("a second range over the same object");
        assert_eq!(
            ledger.charge_of(key),
            Some(3_000),
            "a pinned but linked object is already in the scan"
        );
        drop(permit);
        ledger.account_unlink_all(key);
        assert_eq!(
            ledger.charge_of(key),
            Some(2_000),
            "one object charge, not two, and the unpinned bytes are gone"
        );
        drop(first);
        assert_eq!(ledger.charge_of(key), Some(2_000));
        drop(second);
        assert_eq!(ledger.charge_of(key), None);
    }

    #[test]
    fn scratch_charge_failed_cleanup_keeps_the_bytes_charged() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        let key = permit.key();
        ledger.observe_used(key, 5_000);
        ledger.begin_retirement(key);
        let generation = ledger.inventory_generation(key).expect("generation");
        assert!(ledger.commit_quiescent_measurement(key, generation, 5_000));
        drop(permit);
        ledger.begin_release(key);
        ledger.note_conservative(key, "unlink_failed");
        assert_eq!(ledger.charge_of(key), Some(5_000));
        assert_eq!(ledger.snapshot().releasing, 5_000);
        ledger.account_unlink_all(key);
        assert_eq!(ledger.snapshot().total, 0);
    }

    #[test]
    fn scratch_charge_growth_respects_the_configured_ceiling() {
        let ledger = ScratchLedger::new();
        let small = ledger.reserve(1_000, 10_000).expect("admission");
        let key = small.key();
        assert_eq!(ledger.grow(key, 4_000, 10_000), Some(5_000));
        let other = ledger.reserve(4_000, 10_000).expect("a second producer");
        assert_eq!(ledger.snapshot().total, 9_000);
        assert_eq!(
            ledger.grow(key, 2_000, 10_000),
            None,
            "growth must not exceed the budget"
        );
        assert_eq!(ledger.charge_of(key), Some(5_000));
        assert!(ledger.authorize_write(key, 3_000, 10_000, 0).is_some());
        assert!(
            ledger.authorize_write(key, 9_000, 10_000, 0).is_none(),
            "an oversized next slice is refused before it is written"
        );
        drop((small, other));
    }

    #[test]
    fn scratch_charge_authorize_write_grows_only_by_the_shortfall() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(1_000, 100_000).expect("admission");
        let key = permit.key();
        ledger.observe_used(key, 900);
        // 100 unused; a 500-byte slice needs 400 more plus the headroom.
        let first = ledger
            .authorize_write(key, 500, 100_000, 250)
            .expect("growth covers the shortfall");
        assert_eq!(ledger.grant_of(key), Some(1_650));
        assert_eq!(ledger.charge_of(key), Some(1_650));
        drop(first);
        drop(permit);
    }

    /// The whole write bound in one case: an authorization is *held*, so two
    /// outstanding writes cannot be satisfied out of the same allowance.
    ///
    /// Without the debit both calls compared against the same
    /// `grant - used`, and `used` only moves on a directory walk — so a
    /// writer could publish object after object between two walks against a
    /// single authorization, and "the writer waits" became "the writer waits
    /// once per poll interval". That is a timing bound wearing a write
    /// bound's clothes, and it is exactly what the smaller copy-path
    /// admission rests on not being.
    #[test]
    fn scratch_charge_two_writes_cannot_share_one_allowance() {
        let ledger = ScratchLedger::new();
        // A budget with no room to grow into, so the allowance is the only
        // thing that can answer.
        let permit = ledger.reserve(1_000, 1_000).expect("admission");
        let key = permit.key();

        let first = ledger
            .authorize_write(key, 600, 1_000, 0)
            .expect("the first write fits");
        assert_eq!(
            ledger.charge_of(key),
            Some(1_000),
            "an authorized write is charged before a single byte is written"
        );
        assert!(
            ledger.authorize_write(key, 600, 1_000, 0).is_none(),
            "the second cannot have the bytes the first is holding"
        );

        // Abandoned: the reservation comes back.
        drop(first);
        let second = ledger
            .authorize_write(key, 600, 1_000, 0)
            .expect("the allowance returned");

        // Landed: the bytes stay charged until a walk subsumes them, so the
        // window between the write and the next scan is not free either.
        second.landed(600);
        assert!(
            ledger.authorize_write(key, 600, 1_000, 0).is_none(),
            "bytes that landed are still bytes; the allowance is spent"
        );
        assert_eq!(ledger.charge_of(key), Some(1_000));
        ledger.observe_used(key, 600);
        assert_eq!(
            ledger.charge_of(key),
            Some(1_000),
            "the walk moves the measurement, not the ceiling"
        );
        assert!(
            ledger.authorize_write(key, 400, 1_000, 0).is_some(),
            "and what the walk proved is not there is available again"
        );
        drop(permit);
    }

    #[test]
    fn scratch_charge_disabled_ceiling_admits_a_zero_charge() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, 0).expect("admission");
        assert_eq!(ledger.charge_of(permit.key()), Some(0));
        assert_eq!(ledger.snapshot().total, 0);
        drop(permit);
    }

    #[tokio::test]
    async fn scratch_charge_barrier_waits_for_a_writer_and_reports_a_stall() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(R, CAP).expect("admission");
        let key = permit.key();
        let writer = ledger.register_writer(key).expect("writer");
        let barrier = ledger.begin_retirement(key).expect("barrier");
        let stalled = barrier
            .settle(
                &ledger,
                key,
                tokio::time::Instant::now() + std::time::Duration::from_millis(30),
            )
            .await;
        assert!(!stalled, "a stalled writer is reported, not assumed done");

        let barrier = ledger.begin_retirement(key).expect("barrier");
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            drop(writer);
        });
        assert!(
            barrier
                .settle(
                    &ledger,
                    key,
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5)
                )
                .await
        );
        releaser.await.expect("releaser");
        drop(permit);
    }

    #[test]
    fn scratch_charge_regrant_tracks_actual_bytes_instead_of_ratcheting() {
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(1_000, 10_000).expect("admission");
        let key = permit.key();
        // A slice bigger than the unused allowance grows the grant.
        let authorized = ledger
            .authorize_write(key, 2_000, 10_000, 500)
            .expect("growth covers an oversized slice");
        assert!(ledger.grant_of(key).expect("grant") > 1_000);
        authorized.landed(1_800);
        assert_eq!(
            ledger.charge_of(key),
            Some(ledger.grant_of(key).expect("grant")),
            "bytes that landed are charged before the next walk sees them"
        );
        // Then the producer's own bytes are measured and the envelope is
        // re-granted from them, so the ceiling follows the disk rather than
        // the high-water mark of every write it ever authorized.
        ledger.observe_used(key, 1_800);
        assert!(ledger.regrant(key, 600, 10_000));
        assert_eq!(ledger.grant_of(key), Some(2_400));
        assert_eq!(ledger.charge_of(key), Some(2_400));
        // Retention frees bytes; the charge follows it down.
        ledger.observe_used(key, 400);
        assert!(ledger.regrant(key, 600, 10_000));
        assert_eq!(ledger.charge_of(key), Some(1_000));
        drop(permit);
    }

    #[test]
    fn scratch_charge_a_denied_regrant_keeps_the_producer_held() {
        let ledger = ScratchLedger::new();
        let small = ledger.reserve(1_000, 10_000).expect("admission");
        let key = small.key();
        let _other = ledger.reserve(8_500, 10_000).expect("a second producer");
        ledger.observe_used(key, 1_000);
        assert!(
            !ledger.regrant(key, 5_000, 10_000),
            "the budget cannot carry that envelope"
        );
        assert_eq!(
            ledger.grant_of(key),
            Some(1_000),
            "a denied growth never lowers a grant below what is on the disk"
        );
        assert!(ledger.grant_exhausted(key), "and the producer stays held");
        assert!(ledger.regrant(key, 400, 10_000));
        assert!(!ledger.grant_exhausted(key));
        drop(small);
    }

    #[test]
    fn scratch_charge_four_low_bitrate_producers_fit_under_the_unchanged_cap() {
        // R4's product claim, in the accounting that decides it. Four startup
        // allowances sized for a 48 s publish gate plus one 16 s segment at
        // 8 Mb/s, growing as they write, all under the shipped 8 GiB default.
        const CAP: i64 = 8 * 1024 * 1024 * 1024;
        let startup = 64 * 8_000_000 / 8 + 256 * 1024 * 1024; // media + envelope
        let ledger = ScratchLedger::new();
        let mut producers = Vec::new();
        for index in 0..4 {
            let permit = ledger
                .reserve(startup, CAP)
                .unwrap_or_else(|refusal| panic!("producer {index} must fit: {refusal}"));
            ledger.bind_session(permit.key(), &format!("copy-{index}"), 1);
            producers.push(permit);
        }
        assert!(ledger.snapshot().total <= CAP);
        // Each grows past its startup allowance as it produces.
        for permit in &producers {
            ledger.observe_used(permit.key(), 400 * 1024 * 1024);
            assert!(ledger.regrant(permit.key(), 256 * 1024 * 1024, CAP));
        }
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.entries, 4);
        assert!(
            snapshot.total <= CAP,
            "four grown producers still fit: {}",
            snapshot.total
        );
        // The old sizing could not have admitted the third, let alone a fourth.
        let old_reservation = 2 * 1024 * 1024 * 1024 + 64 * 1024 * 1024;
        assert!(4 * old_reservation > CAP);
        drop(producers);
    }

    #[test]
    fn scratch_charge_twenty_retired_inventories_fit_beside_a_live_pair() {
        // Twenty 128 MiB retained generations plus one incumbent and one
        // successor: 2.5 GiB of real bytes and two full reservations. Under
        // the old max(live, reservation) fold this was 20 x R and refused.
        let ledger = ScratchLedger::new();
        let mut retired = Vec::new();
        for index in 0..20 {
            let permit = ledger.reserve(R, CAP).expect("admission");
            let key = permit.key();
            ledger.bind_session(key, &format!("retired-{index}"), 1);
            ledger.observe_used(key, 128 * 1024 * 1024);
            ledger.begin_retirement(key);
            let generation = ledger.inventory_generation(key).expect("generation");
            assert!(ledger.commit_quiescent_measurement(
                key,
                generation,
                128 * 1024 * 1024
            ));
            retired.push(permit);
        }
        let incumbent = ledger.reserve(R, CAP).expect("incumbent");
        let successor = ledger.reserve(R, CAP).expect("successor");
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.retained, 20 * 128 * 1024 * 1024);
        assert_eq!(snapshot.total, 20 * 128 * 1024 * 1024 + 2 * R);
        assert!(
            snapshot.total <= CAP,
            "2.5 GiB retained plus two producers fits in 8 GiB: {}",
            snapshot.total
        );
        drop((incumbent, successor, retired));
    }
}
