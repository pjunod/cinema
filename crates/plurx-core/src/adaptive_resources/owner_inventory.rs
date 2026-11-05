//! Bounded original-owner census, separate from service observations and release.
//!
//! This is a whole-node domain: it deliberately cannot certify disjoint CPU,
//! memory, device, source or storage domains. Every adapter must register before
//! native entry and retain its move-only token through cleanup. Observation is
//! allocation-free. A snapshot alone never serializes admission: the actual
//! reservation must execute inside `with_current_complete_snapshot`, and every
//! affected adapter must participate in this same registry.
//!
//! There is intentionally no production constructor for audited coverage yet.
//! Named, source-backed adapter integrations must supply those capabilities;
//! an unbridged family is unknown even when no owners have been observed.

use super::{PhysicalCloseReceipt, ReservationBinding, ResourceLedger};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Includes producers, references and cleanup selectors, not just the policy's
/// protected playback rows. Exhaustion permanently makes the census unknown;
/// public registration refuses while the selected ledger bridge retains an
/// explicit overflow selector. No old owner is evicted to admit a new one.
pub const MAX_INVENTORY_OWNERS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OwnerFamily {
    BufferedVod,
    RollingVod,
    LiveTransport,
    CaptureSink,
    SourceReader,
    BackgroundPreparation,
    Planning,
    RelayDelivery,
    RetainedResidency,
    NativeCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerFamilies(u16);

impl OwnerFamilies {
    pub const ALL_REQUIRED: Self = Self((1 << 10) - 1);

    pub fn contains(self, family: OwnerFamily) -> bool {
        self.0 & (1 << family as u8) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Original identity only. A new generation must use its new reservation or
/// actual original application UUID, never a truncated hash or serving name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryIdentity {
    Reservation(ReservationBinding),
    Original(Uuid),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    Producer,
    Sink,
    Residency,
    /// Separate consumers retain obligations without counting another producer.
    Reference {
        producer: InventoryIdentity,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerPhase {
    Registered,
    Active,
    Closing,
    UnknownHeld,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryRecord {
    pub identity: InventoryIdentity,
    pub family: OwnerFamily,
    pub kind: OwnerKind,
    pub phase: OwnerPhase,
    // Only an original runtime registration can obtain a binding accepted by
    // physical-close validation. Producer closure never proves the destruction
    // of separately retained consumer/application owners.
    physical_binding: Option<ReservationBinding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryError {
    Unavailable,
    InvalidIdentity,
    InvalidKind,
    DuplicateIdentity,
    Full,
    MissingOwner,
    InvalidPhase,
    WrongInventory,
    ReceiptMismatch,
    NotFinalized,
    ReferencesRemain,
    Incomplete,
    StaleSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryUnknown {
    pub unbridged: OwnerFamilies,
    pub overflowed: bool,
    pub epoch_exhausted: bool,
    pub unknown_held: bool,
}

impl InventoryUnknown {
    pub fn is_complete(self) -> bool {
        self.unbridged.is_empty() && !self.overflowed && !self.epoch_exhausted && !self.unknown_held
    }
}

/// Sealed coverage capability. No settings writer, observer or daemon caller
/// can assert completeness with a boolean or an arbitrary family list.
pub struct AuditedAdapterCoverage {
    families: OwnerFamilies,
}

struct InventoryState {
    epoch: u64,
    coverage: OwnerFamilies,
    overflowed: bool,
    epoch_exhausted: bool,
    records: [Option<InventoryRecord>; MAX_INVENTORY_OWNERS],
}

impl InventoryState {
    fn changed(&mut self) {
        match self.epoch.checked_add(1) {
            Some(epoch) => self.epoch = epoch,
            None => self.epoch_exhausted = true,
        }
    }

    fn unknown(&self) -> InventoryUnknown {
        InventoryUnknown {
            unbridged: OwnerFamilies(OwnerFamilies::ALL_REQUIRED.0 & !self.coverage.0),
            overflowed: self.overflowed,
            epoch_exhausted: self.epoch_exhausted,
            unknown_held: self
                .records
                .iter()
                .flatten()
                .any(|record| record.phase == OwnerPhase::UnknownHeld),
        }
    }
}

struct SharedInventory {
    locked: Mutex<InventoryState>,
    // Token Drop can run inside an admission callback already holding locked.
    // These fixed markers defer the lifecycle mutation without locking again.
    dropped: [AtomicBool; MAX_INVENTORY_OWNERS],
}

impl SharedInventory {
    fn reconcile(&self, state: &mut InventoryState) {
        for (slot, dropped) in self.dropped.iter().enumerate() {
            if dropped.load(Ordering::Acquire) {
                if let Some(record) = state.records[slot].as_mut() {
                    if record.phase != OwnerPhase::UnknownHeld {
                        record.phase = OwnerPhase::UnknownHeld;
                        state.changed();
                    }
                } else {
                    // Impossible with original move-only tokens. Preserve an
                    // unexpected stale marker as unknown, never absence.
                    state.overflowed = true;
                    state.changed();
                }
            }
        }
    }

    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, InventoryState>> {
        let mut state = self.locked.lock()?;
        self.reconcile(&mut state);
        Ok(state)
    }

    #[cfg(test)]
    fn try_lock(&self) -> std::sync::TryLockResult<std::sync::MutexGuard<'_, InventoryState>> {
        let mut state = self.locked.try_lock()?;
        self.reconcile(&mut state);
        Ok(state)
    }
}

#[derive(Clone)]
pub struct OwnerInventory {
    state: Arc<SharedInventory>,
}

impl Default for OwnerInventory {
    fn default() -> Self {
        Self::new()
    }
}

impl OwnerInventory {
    /// Startup allocation only; snapshots and mutations do not grow this book.
    pub fn new() -> Self {
        Self {
            state: Arc::new(SharedInventory {
                locked: Mutex::new(InventoryState {
                    epoch: 1,
                    coverage: OwnerFamilies(0),
                    overflowed: false,
                    epoch_exhausted: false,
                    records: [None; MAX_INVENTORY_OWNERS],
                }),
                dropped: [const { AtomicBool::new(false) }; MAX_INVENTORY_OWNERS],
            }),
        }
    }

    /// Ordinary runtime mutations share the census boundary even while wider
    /// adapter coverage is unknown. The borrowed guard cannot leave this
    /// synchronous closure; callers acquire operations and ledger locks inside.
    pub(in crate::adaptive_resources) fn with_runtime_mutation<T>(
        &self,
        mutate: impl FnOnce(&mut InventoryAdmission<'_>) -> T,
    ) -> Result<T, InventoryError> {
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        Ok(mutate(&mut InventoryAdmission {
            inventory: self,
            state: &mut state,
        }))
    }

    pub fn install_audited_coverage(
        &self,
        coverage: AuditedAdapterCoverage,
    ) -> Result<(), InventoryError> {
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        if state.coverage.0 & coverage.families.0 != coverage.families.0 {
            state.coverage.0 |= coverage.families.0;
            state.changed();
        }
        Ok(())
    }

    pub fn register_runtime(
        &self,
        binding: ReservationBinding,
        family: OwnerFamily,
        kind: OwnerKind,
    ) -> Result<InventoryOwner, InventoryError> {
        self.register(InventoryRecord {
            identity: InventoryIdentity::Reservation(binding),
            family,
            kind,
            phase: OwnerPhase::Registered,
            physical_binding: Some(binding),
        })
    }

    /// Legacy native owners are visible but cannot be removed by an observed
    /// phase or fabricated close flag. A concrete positive-close adapter is
    /// required before this module can retire their original UUIDs.
    pub fn register_original(
        &self,
        original: Uuid,
        family: OwnerFamily,
        kind: OwnerKind,
    ) -> Result<InventoryOwner, InventoryError> {
        if original.is_nil() {
            return Err(InventoryError::InvalidIdentity);
        }
        self.register(InventoryRecord {
            identity: InventoryIdentity::Original(original),
            family,
            kind,
            phase: OwnerPhase::Registered,
            physical_binding: None,
        })
    }

    fn register(&self, record: InventoryRecord) -> Result<InventoryOwner, InventoryError> {
        if matches!(record.kind, OwnerKind::Reference { .. }) {
            return Err(InventoryError::InvalidKind);
        }
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        self.insert(&mut state, record)
    }

    fn insert(
        &self,
        state: &mut InventoryState,
        record: InventoryRecord,
    ) -> Result<InventoryOwner, InventoryError> {
        self.state.reconcile(state);
        if state
            .records
            .iter()
            .flatten()
            .any(|old| old.identity == record.identity)
        {
            return Err(InventoryError::DuplicateIdentity);
        }
        let Some(slot) = state.records.iter().position(Option::is_none) else {
            state.overflowed = true;
            state.changed();
            return Err(InventoryError::Full);
        };
        self.state.dropped[slot].store(false, Ordering::Release);
        state.records[slot] = Some(record);
        state.changed();
        Ok(InventoryOwner {
            state: Arc::clone(&self.state),
            identity: record.identity,
            slot,
            registered: true,
        })
    }

    /// Registers a separately retained consumer against the same original
    /// producer or residency. This never duplicates the original owner's cost.
    pub fn register_reference(
        &self,
        producer: &InventoryOwner,
        original_reference: Uuid,
        family: OwnerFamily,
    ) -> Result<InventoryOwner, InventoryError> {
        if !Arc::ptr_eq(&self.state, &producer.state) {
            return Err(InventoryError::WrongInventory);
        }
        if original_reference.is_nil() {
            return Err(InventoryError::InvalidIdentity);
        }
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        let parent = state
            .records
            .iter()
            .flatten()
            .find(|record| producer.registered && record.identity == producer.identity)
            .ok_or(InventoryError::MissingOwner)?;
        if !matches!(parent.kind, OwnerKind::Producer | OwnerKind::Residency)
            || !matches!(parent.phase, OwnerPhase::Registered | OwnerPhase::Active)
        {
            return Err(InventoryError::InvalidPhase);
        }
        let record = InventoryRecord {
            identity: InventoryIdentity::Original(original_reference),
            family,
            kind: OwnerKind::Reference {
                producer: parent.identity,
            },
            phase: OwnerPhase::Registered,
            physical_binding: None,
        };
        self.insert(&mut state, record)
    }

    pub fn snapshot(&self) -> Result<InventorySnapshot, InventoryError> {
        let state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        Ok(InventorySnapshot {
            state: Arc::clone(&self.state),
            epoch: state.epoch,
            unknown: state.unknown(),
            records: state.records,
        })
    }

    /// Executes the actual synchronous reservation under the census mutation
    /// lock. The callback must neither await nor reenter this inventory; use
    /// its admission guard to register the same new reservation before return.
    /// It may reserve and install owners only; native entry must wait until
    /// this method returns success. Dropped-owner evidence rejects the result.
    /// This only supplies serialization when ALL relevant adapters participate;
    /// service telemetry and a prior snapshot cannot supply that guarantee.
    /// The callback's success/failure retains the existing ledger authority.
    pub fn with_current_complete_snapshot<T>(
        &self,
        snapshot: &InventorySnapshot,
        reserve: impl FnOnce(&mut InventoryAdmission<'_>) -> T,
    ) -> Result<T, InventoryError> {
        if !Arc::ptr_eq(&self.state, &snapshot.state) {
            return Err(InventoryError::WrongInventory);
        }
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        if !state.unknown().is_complete() || !snapshot.unknown.is_complete() {
            return Err(InventoryError::Incomplete);
        }
        if state.epoch != snapshot.epoch {
            return Err(InventoryError::StaleSnapshot);
        }
        let result = reserve(&mut InventoryAdmission {
            inventory: self,
            state: &mut state,
        });
        self.state.reconcile(&mut state);
        // A reservation attempt itself invalidates parallel prepared snapshots,
        // including a failed one; this prevents independent reuse of one census.
        state.changed();
        if !state.unknown().is_complete() {
            // A result can itself own tokens or other destructors that inspect
            // the inventory. Release the mutex before discarding any result
            // that lost its original evidence during the reservation callback.
            drop(state);
            drop(result);
            return Err(InventoryError::Incomplete);
        }
        Ok(result)
    }
}

/// Mutation access inside the original snapshot's admission transaction. A
/// real ledger reserve and installation of its original census token can run
/// under one lock without deadlocking by reentering the public register API.
pub struct InventoryAdmission<'a> {
    inventory: &'a OwnerInventory,
    state: &'a mut InventoryState,
}

impl InventoryAdmission<'_> {
    pub fn register_runtime(
        &mut self,
        binding: ReservationBinding,
        family: OwnerFamily,
        kind: OwnerKind,
    ) -> Result<InventoryOwner, InventoryError> {
        if matches!(kind, OwnerKind::Reference { .. }) {
            return Err(InventoryError::InvalidKind);
        }
        self.inventory.insert(
            self.state,
            InventoryRecord {
                identity: InventoryIdentity::Reservation(binding),
                family,
                kind,
                phase: OwnerPhase::Registered,
                physical_binding: Some(binding),
            },
        )
    }
}

pub struct InventorySnapshot {
    // Retaining the registry prevents allocator-address reuse from making an
    // old snapshot appear to originate from a new inventory instance.
    state: Arc<SharedInventory>,
    epoch: u64,
    unknown: InventoryUnknown,
    records: [Option<InventoryRecord>; MAX_INVENTORY_OWNERS],
}

impl InventorySnapshot {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn completeness(&self) -> InventoryUnknown {
        self.unknown
    }

    pub fn records(&self) -> impl Iterator<Item = &InventoryRecord> {
        self.records.iter().flatten()
    }
}

/// Original move-only lifecycle selector. Losing it retains the record as
/// UnknownHeld; neither Drop nor a diagnostic phase proves native closure.
#[must_use = "Retain the original inventory owner through positive cleanup"]
pub struct InventoryOwner {
    state: Arc<SharedInventory>,
    identity: InventoryIdentity,
    slot: usize,
    registered: bool,
}

impl InventoryOwner {
    pub fn identity(&self) -> InventoryIdentity {
        self.identity
    }

    pub fn observe_phase(&mut self, phase: OwnerPhase) -> Result<(), InventoryError> {
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        let record = state
            .records
            .iter_mut()
            .flatten()
            .find(|record| self.registered && record.identity == self.identity)
            .ok_or(InventoryError::MissingOwner)?;
        let allowed = record.phase == phase
            || matches!(
                (record.phase, phase),
                (
                    OwnerPhase::Registered,
                    OwnerPhase::Active | OwnerPhase::Closing | OwnerPhase::UnknownHeld
                ) | (
                    OwnerPhase::Active,
                    OwnerPhase::Closing | OwnerPhase::UnknownHeld
                ) | (OwnerPhase::Closing, OwnerPhase::UnknownHeld)
            );
        if !allowed {
            return Err(InventoryError::InvalidPhase);
        }
        if record.phase != phase {
            record.phase = phase;
            state.changed();
        }
        Ok(())
    }

    /// Borrow the original nonclone proof after the matching core settlement
    /// has released its full ledger entry. On every failure this selector stays
    /// registered and usable. Lock order is census, then physical ledger.
    /// No root-exit sample, boolean, UUID, Store phase or Drop can call this.
    pub fn close_after_finalized_receipt(
        &mut self,
        receipt: &PhysicalCloseReceipt,
        ledger: &ResourceLedger,
    ) -> Result<(), InventoryError> {
        let mut state = self.state.lock().map_err(|_| InventoryError::Unavailable)?;
        let slot = state
            .records
            .iter()
            .position(|slot| {
                slot.as_ref()
                    .is_some_and(|record| self.registered && record.identity == self.identity)
            })
            .ok_or(InventoryError::MissingOwner)?;
        let record = state.records[slot]
            .as_ref()
            .ok_or(InventoryError::MissingOwner)?;
        if record.physical_binding != Some(receipt.custody_binding()) {
            return Err(InventoryError::ReceiptMismatch);
        }
        let binding = receipt.custody_binding();
        if ledger.id != binding.ledger {
            return Err(InventoryError::ReceiptMismatch);
        }
        let ledger_state = ledger
            .inner
            .lock()
            .map_err(|_| InventoryError::Unavailable)?;
        if ledger_state.entries.contains_key(&binding.reservation) {
            return Err(InventoryError::NotFinalized);
        }
        if state.records.iter().flatten().any(|other| {
            matches!(other.kind, OwnerKind::Reference { producer } if producer == self.identity)
        }) {
            return Err(InventoryError::ReferencesRemain);
        }
        self.registered = false;
        self.state.dropped[slot].store(false, Ordering::Release);
        state.records[slot] = None;
        state.changed();
        Ok(())
    }
}

impl Drop for InventoryOwner {
    fn drop(&mut self) {
        if self.registered {
            // This may run while the same thread holds the admission mutex or
            // is unwinding. It must neither lock, allocate nor panic. The slot
            // cannot be reused until this original token positively closes;
            // successful close disarms it before clearing the original row.
            self.state.dropped[self.slot].store(true, Ordering::Release);
        }
    }
}

mod runtime_bridge;
mod support;
pub(in crate::adaptive_resources) use runtime_bridge::{
    PendingRuntimeClose, RuntimeInventoryOwner,
};
pub(in crate::adaptive_resources) use support::{SupportInventoryMarker, SupportInventoryOwner};

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
mod application;
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources) use application::ApplicationReference;
