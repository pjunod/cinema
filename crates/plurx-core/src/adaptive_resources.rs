//! Node-local atomic resource accounting. These are physical resource bounds,
//! NOT the legacy software thread-service budget (whose idle-pool exception is
//! preserved separately). Unknown evidence never grants an expansion.
//!
//! Assessment is advisory; reservation rechecks the epoch and every dimension
//! under one short lock. No lock crosses I/O or await. Post-spawn Drop retains
//! accounting; only the actual cleanup owner may present a non-wire receipt.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::process::observation::Metric;

static NEXT_LEDGER: AtomicU64 = AtomicU64::new(1);
const MAX_RESERVATIONS: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ResourceVector {
    pub cpu_ns_per_second: u64,
    pub peak_memory_bytes: u64,
    pub handles: u64,
    pub processes: u64,
    pub hardware_slots: u64,
    pub tuner_slots: u64,
    pub scratch_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceDimension {
    Cpu,
    Memory,
    Handles,
    Processes,
    Hardware,
    Tuner,
    Scratch,
}

impl ResourceVector {
    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            cpu_ns_per_second: self
                .cpu_ns_per_second
                .checked_add(other.cpu_ns_per_second)?,
            peak_memory_bytes: self
                .peak_memory_bytes
                .checked_add(other.peak_memory_bytes)?,
            handles: self.handles.checked_add(other.handles)?,
            processes: self.processes.checked_add(other.processes)?,
            hardware_slots: self.hardware_slots.checked_add(other.hardware_slots)?,
            tuner_slots: self.tuner_slots.checked_add(other.tuner_slots)?,
            scratch_bytes: self.scratch_bytes.checked_add(other.scratch_bytes)?,
        })
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            cpu_ns_per_second: self
                .cpu_ns_per_second
                .checked_sub(other.cpu_ns_per_second)?,
            peak_memory_bytes: self
                .peak_memory_bytes
                .checked_sub(other.peak_memory_bytes)?,
            handles: self.handles.checked_sub(other.handles)?,
            processes: self.processes.checked_sub(other.processes)?,
            hardware_slots: self.hardware_slots.checked_sub(other.hardware_slots)?,
            tuner_slots: self.tuner_slots.checked_sub(other.tuner_slots)?,
            scratch_bytes: self.scratch_bytes.checked_sub(other.scratch_bytes)?,
        })
    }

    fn exceeded(self, cap: Self) -> Option<ResourceDimension> {
        [
            (
                self.cpu_ns_per_second,
                cap.cpu_ns_per_second,
                ResourceDimension::Cpu,
            ),
            (
                self.peak_memory_bytes,
                cap.peak_memory_bytes,
                ResourceDimension::Memory,
            ),
            (self.handles, cap.handles, ResourceDimension::Handles),
            (self.processes, cap.processes, ResourceDimension::Processes),
            (
                self.hardware_slots,
                cap.hardware_slots,
                ResourceDimension::Hardware,
            ),
            (self.tuner_slots, cap.tuner_slots, ResourceDimension::Tuner),
            (
                self.scratch_bytes,
                cap.scratch_bytes,
                ResourceDimension::Scratch,
            ),
        ]
        .into_iter()
        .find_map(|(used, limit, dimension)| (used > limit).then_some(dimension))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorkloadDemand {
    /// Complete conservative resource bounds, not sampled RSS mistaken for a
    /// memory upper bound. A missing bound is Unknown, never a zero-cost role.
    pub resources: Metric<ResourceVector>,
    pub limit_epoch: u64,
    pub trial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionRefusal {
    InsufficientEvidence,
    StaleEpoch,
    HardLimit(ResourceDimension),
    TrialAlreadyHeld,
    LedgerFull,
    ArithmeticOverflow,
    IdentityExhausted,
    UnknownReservation,
    InvalidTransition,
    ReceiptMismatch,
    Unavailable,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct AdmissionAssessment {
    pub limit_epoch: u64,
    pub used: ResourceVector,
    pub refusal: Option<AdmissionRefusal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState {
    Reserved,
    SpawnPossible,
    Reaping,
    UnknownHeld,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservationBinding {
    ledger: u64,
    reservation: u64,
    allocation_epoch: u64,
}

/// Not serializable, clonable, or constructible by a remote request. Actual
/// process/non-process cleanup integration must mint this inside the core only
/// after closing the entire bound graph; PID leader waits cannot mint it.
/// There is deliberately no public "trust me, reaped" constructor.
#[derive(Debug)]
pub struct PhysicalCloseReceipt {
    pub(crate) binding: ReservationBinding,
}

struct Entry {
    binding: ReservationBinding,
    resources: ResourceVector,
    state: ReservationState,
}

struct State {
    epoch: u64,
    cap: Metric<ResourceVector>,
    used: ResourceVector,
    next_reservation: u64,
    entries: BTreeMap<u64, Entry>,
    trial: Option<u64>,
    invalid: bool,
}

#[derive(Clone)]
pub struct ResourceLedger {
    id: u64,
    inner: Arc<Mutex<State>>,
}

pub struct ResourceReservation {
    ledger: ResourceLedger,
    binding: ReservationBinding,
}

/// Retained by the SAME cleanup owner outside potentially panicking work. It
/// survives loss of the reservation handle, but cannot spawn or release merely
/// because cancellation, a deadline, or an uncertain Drop happened.
pub struct CleanupAuthority {
    ledger: ResourceLedger,
    binding: ReservationBinding,
}

#[derive(Debug, serde::Serialize)]
pub struct LedgerSnapshot {
    pub limit_epoch: u64,
    pub used: ResourceVector,
    pub held: usize,
    pub unknown_held: usize,
    pub trial_held: bool,
}

impl State {
    fn assess(&self, demand: WorkloadDemand) -> Result<ResourceVector, AdmissionRefusal> {
        if self.invalid {
            return Err(AdmissionRefusal::Unavailable);
        }
        if demand.limit_epoch != self.epoch {
            return Err(AdmissionRefusal::StaleEpoch);
        }
        let (Metric::Known(resources), Metric::Known(cap)) = (demand.resources, self.cap) else {
            return Err(AdmissionRefusal::InsufficientEvidence);
        };
        if resources == ResourceVector::default() {
            return Err(AdmissionRefusal::InsufficientEvidence);
        }
        if demand.trial && self.trial.is_some() {
            return Err(AdmissionRefusal::TrialAlreadyHeld);
        }
        if self.entries.len() >= MAX_RESERVATIONS {
            return Err(AdmissionRefusal::LedgerFull);
        }
        let total = self
            .used
            .checked_add(resources)
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        if let Some(dimension) = total.exceeded(cap) {
            return Err(AdmissionRefusal::HardLimit(dimension));
        }
        Ok(resources)
    }

    fn release(&mut self, id: u64) -> Result<(), AdmissionRefusal> {
        let entry = self
            .entries
            .get(&id)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        let Some(next_used) = self.used.checked_sub(entry.resources) else {
            self.invalid = true;
            return Err(AdmissionRefusal::Unavailable);
        };
        self.used = next_used;
        self.entries.remove(&id);
        if self.trial == Some(id) {
            self.trial = None;
        }
        Ok(())
    }
}

impl ResourceLedger {
    pub fn new(
        limit_epoch: u64,
        hard_cap: Metric<ResourceVector>,
    ) -> Result<Self, AdmissionRefusal> {
        let id = NEXT_LEDGER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| AdmissionRefusal::IdentityExhausted)?;
        Ok(Self {
            id,
            inner: Arc::new(Mutex::new(State {
                epoch: limit_epoch,
                cap: hard_cap,
                used: ResourceVector::default(),
                next_reservation: 1,
                entries: BTreeMap::new(),
                trial: None,
                invalid: false,
            })),
        })
    }

    pub fn assess(&self, demand: WorkloadDemand) -> Result<AdmissionAssessment, AdmissionRefusal> {
        let state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        Ok(AdmissionAssessment {
            limit_epoch: state.epoch,
            used: state.used,
            refusal: state.assess(demand).err(),
        })
    }

    pub fn reserve(
        &self,
        demand: WorkloadDemand,
    ) -> Result<(ResourceReservation, CleanupAuthority), AdmissionRefusal> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let resources = state.assess(demand)?;
        let id = state.next_reservation;
        let next = id
            .checked_add(1)
            .ok_or(AdmissionRefusal::IdentityExhausted)?;
        let used = state
            .used
            .checked_add(resources)
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        let binding = ReservationBinding {
            ledger: self.id,
            reservation: id,
            allocation_epoch: state.epoch,
        };
        // All fallible validation precedes the single accounting commit.
        state.entries.insert(
            id,
            Entry {
                binding,
                resources,
                state: ReservationState::Reserved,
            },
        );
        state.used = used;
        state.next_reservation = next;
        if demand.trial {
            state.trial = Some(id);
        }
        Ok((
            ResourceReservation {
                ledger: self.clone(),
                binding,
            },
            CleanupAuthority {
                ledger: self.clone(),
                binding,
            },
        ))
    }

    pub fn update_hard_cap(
        &self,
        epoch: u64,
        cap: Metric<ResourceVector>,
    ) -> Result<(), AdmissionRefusal> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if epoch <= state.epoch {
            return Err(AdmissionRefusal::StaleEpoch);
        }
        state.epoch = epoch;
        state.cap = cap;
        // A lower cap never discards already held physical work.
        Ok(())
    }

    pub fn snapshot(&self) -> Result<LedgerSnapshot, AdmissionRefusal> {
        let state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        Ok(LedgerSnapshot {
            limit_epoch: state.epoch,
            used: state.used,
            held: state.entries.len(),
            unknown_held: state
                .entries
                .values()
                .filter(|entry| entry.state == ReservationState::UnknownHeld)
                .count(),
            trial_held: state.trial.is_some(),
        })
    }
}

impl ResourceReservation {
    /// Must happen before calling any launcher, including a launcher that can
    /// fail after creating a child. This consumes exclusive no-spawn certainty.
    pub fn mark_spawn_possible(&mut self) -> Result<(), AdmissionRefusal> {
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let entry = state
            .entries
            .get_mut(&self.binding.reservation)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.state != ReservationState::Reserved {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        entry.state = ReservationState::SpawnPossible;
        Ok(())
    }
}

impl Drop for ResourceReservation {
    fn drop(&mut self) {
        let Ok(mut state) = self.ledger.inner.lock() else {
            return;
        };
        let Some(entry) = state.entries.get_mut(&self.binding.reservation) else {
            return;
        };
        if entry.state == ReservationState::Reserved {
            // This unique, non-clonable handle still owns exclusive no-spawn
            // custody; the separate cleanup authority has no spawn operation.
            let _ = state.release(self.binding.reservation);
        } else {
            entry.state = ReservationState::UnknownHeld;
        }
    }
}

impl CleanupAuthority {
    pub fn mark_reaping(&self) -> Result<(), AdmissionRefusal> {
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let entry = state
            .entries
            .get_mut(&self.binding.reservation)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.state == ReservationState::Reserved {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        entry.state = ReservationState::Reaping;
        Ok(())
    }

    pub fn close(&self, receipt: PhysicalCloseReceipt) -> Result<(), AdmissionRefusal> {
        if receipt.binding != self.binding {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let entry = state
            .entries
            .get(&self.binding.reservation)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.binding != receipt.binding {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        if entry.state == ReservationState::Reserved {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        state.release(self.binding.reservation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::observation::UnknownReason;

    fn vector(value: u64) -> ResourceVector {
        ResourceVector {
            cpu_ns_per_second: value,
            peak_memory_bytes: value,
            handles: value,
            processes: value,
            hardware_slots: value,
            tuner_slots: value,
            scratch_bytes: value,
        }
    }
    fn demand(value: u64, trial: bool) -> WorkloadDemand {
        WorkloadDemand {
            resources: Metric::Known(vector(value)),
            limit_epoch: 1,
            trial,
        }
    }

    #[test]
    fn reservation_rechecks_all_dimensions_and_epochs_atomically() {
        let ledger = ResourceLedger::new(1, Metric::Known(vector(2))).expect("ledger fixture");
        assert!(ledger
            .assess(demand(2, false))
            .expect("assessment")
            .refusal
            .is_none());
        let (mut first, cleanup) = ledger.reserve(demand(1, false)).expect("first reservation");
        first.mark_spawn_possible().expect("spawn custody");
        assert!(matches!(
            ledger.reserve(demand(2, false)),
            Err(AdmissionRefusal::HardLimit(ResourceDimension::Cpu))
        ));
        let mut memory = vector(1);
        memory.peak_memory_bytes = 2;
        assert!(matches!(
            ledger.reserve(WorkloadDemand {
                resources: Metric::Known(memory),
                ..demand(1, false)
            }),
            Err(AdmissionRefusal::HardLimit(ResourceDimension::Memory))
        ));
        assert_eq!(ledger.snapshot().expect("held snapshot").used, vector(1));
        ledger
            .update_hard_cap(2, Metric::Known(vector(0)))
            .expect("lower physical cap");
        assert!(matches!(
            ledger.reserve(demand(1, false)),
            Err(AdmissionRefusal::StaleEpoch)
        ));
        drop(first);
        assert_eq!(
            ledger.snapshot().expect("uncertain snapshot").unknown_held,
            1
        );
        cleanup
            .close(PhysicalCloseReceipt {
                binding: cleanup.binding,
            })
            .expect("exact physical closure");
        assert_eq!(ledger.snapshot().expect("closed snapshot").used, vector(0));
    }

    #[test]
    fn uncertain_drop_holds_trial_until_original_cleanup_reconciles() {
        let ledger = ResourceLedger::new(1, Metric::Known(vector(10))).expect("ledger fixture");
        let (mut held, cleanup) = ledger.reserve(demand(1, true)).expect("trial reservation");
        held.mark_spawn_possible().expect("spawn custody");
        drop(held);
        assert!(ledger.snapshot().expect("held snapshot").trial_held);
        assert!(matches!(
            ledger.reserve(demand(1, true)),
            Err(AdmissionRefusal::TrialAlreadyHeld)
        ));
        cleanup.mark_reaping().expect("same owner reaping");
        let binding = cleanup.binding;
        cleanup
            .close(PhysicalCloseReceipt { binding })
            .expect("confirmed closure");
        let (not_spawned, _cleanup) = ledger.reserve(demand(1, true)).expect("replacement trial");
        drop(not_spawned);
        assert_eq!(ledger.snapshot().expect("no-spawn snapshot").held, 0);
        assert!(!ledger.snapshot().expect("no-spawn snapshot").trial_held);
    }

    #[test]
    fn unknown_bounds_and_overflow_never_become_spare_capacity() {
        let ledger =
            ResourceLedger::new(1, Metric::Known(vector(u64::MAX))).expect("ledger fixture");
        let (mut held, _cleanup) = ledger
            .reserve(demand(u64::MAX, false))
            .expect("maximum vector");
        held.mark_spawn_possible().expect("spawn custody");
        assert!(matches!(
            ledger.reserve(demand(1, false)),
            Err(AdmissionRefusal::ArithmeticOverflow)
        ));
        assert!(matches!(
            ledger.reserve(WorkloadDemand {
                resources: Metric::Unknown(UnknownReason::InsufficientEvidence),
                ..demand(1, true)
            }),
            Err(AdmissionRefusal::InsufficientEvidence)
        ));
        assert_eq!(
            ledger.snapshot().expect("unchanged snapshot").used,
            vector(u64::MAX)
        );
    }

    #[test]
    fn simultaneous_reservations_never_split_hard_dimensions() {
        let ledger = ResourceLedger::new(1, Metric::Known(vector(1))).expect("ledger fixture");
        let barrier = std::sync::Barrier::new(8);
        let successes: usize = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let ledger = ledger.clone();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        let Ok((mut reservation, _cleanup)) = ledger.reserve(demand(1, false))
                        else {
                            return 0;
                        };
                        reservation.mark_spawn_possible().expect("spawn custody");
                        drop(reservation); // UnknownHeld still occupies every dimension.
                        1
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("reservation contender"))
                .sum()
        });
        assert_eq!(successes, 1);
        assert_eq!(ledger.snapshot().expect("atomic snapshot").used, vector(1));
    }

    #[test]
    fn mismatched_receipt_cannot_release_or_destroy_reconciliation_authority() {
        let first = ResourceLedger::new(1, Metric::Known(vector(1))).expect("first ledger");
        let second = ResourceLedger::new(1, Metric::Known(vector(1))).expect("second ledger");
        let (mut reservation, authority) =
            first.reserve(demand(1, false)).expect("first reservation");
        let (_other_reservation, other_authority) =
            second.reserve(demand(1, false)).expect("other reservation");
        reservation.mark_spawn_possible().expect("spawn custody");
        drop(reservation);
        assert_eq!(
            authority.close(PhysicalCloseReceipt {
                binding: other_authority.binding
            }),
            Err(AdmissionRefusal::ReceiptMismatch)
        );
        assert_eq!(
            first
                .snapshot()
                .expect("wrong-receipt snapshot")
                .unknown_held,
            1
        );
        authority
            .close(PhysicalCloseReceipt {
                binding: authority.binding,
            })
            .expect("matching closure after refusal");
        assert_eq!(first.snapshot().expect("closed snapshot").held, 0);
    }
}
