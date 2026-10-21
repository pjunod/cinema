//! Node-local atomic resource accounting. These are physical resource bounds,
//! NOT the legacy software thread-service budget (whose idle-pool exception is
//! preserved separately). Unknown evidence never grants an expansion.
//!
//! Assessment is advisory; reservation rechecks the epoch and every dimension
//! under one short lock. No lock crosses I/O or await. Post-spawn Drop retains
//! accounting; only the actual cleanup owner may present a non-wire receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::process::observation::Metric;

pub mod physical_graph;

static NEXT_LEDGER: AtomicU64 = AtomicU64::new(1);
const MAX_RESERVATIONS: usize = 1024;
const MAX_FAMILY_ROLES: usize = 3;
const MAX_OVERLAP_FAMILIES: usize = 2;

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

/// Ledger-minted family identity. A successor uses a distinct identity; sharing
/// a role requires borrowing the actual retained reservation, not an integer
/// supplied over the wire. This identity carries no executable authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FamilyIdentity {
    ledger: u64,
    family: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FamilyRole {
    Video,
    Audio,
    Companion,
}

pub struct FamilyRoleDemand<'a> {
    pub role: FamilyRole,
    pub demand: WorkloadDemand,
    /// A retained physical role is checked against this ledger under its lock.
    /// It is counted once even if shared by both requested families.
    pub retained: Option<RetainedRole<'a>>,
}

/// Borrowed identity of an actual reservation owner, including a graph that
/// consumed the original reservation. This grants neither acquisition nor
/// release authority. The accounting transaction still verifies the live entry.
#[derive(Clone, Copy)]
pub struct RetainedRole<'a> {
    binding: ReservationBinding,
    owner: std::marker::PhantomData<&'a ()>,
}

/// Complete requested role set, not only the missing roles. At most three
/// distinct roles per family; no retained reservation is stolen or rebound.
pub struct FamilyDemand<'a> {
    pub identity: FamilyIdentity,
    pub roles: &'a [FamilyRoleDemand<'a>],
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
    InvalidBundle,
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
    binding: ReservationBinding,
}

struct Entry {
    binding: ReservationBinding,
    family_role: Option<(FamilyIdentity, FamilyRole)>,
    resources: ResourceVector,
    state: ReservationState,
    custody: Option<Arc<physical_graph::GraphCustody>>,
}

struct State {
    epoch: u64,
    cap: Metric<ResourceVector>,
    used: ResourceVector,
    next_reservation: u64,
    next_family: u64,
    entries: BTreeMap<u64, Entry>,
    trial: Option<BTreeSet<u64>>,
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
    transferred: bool,
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
        if let Some(members) = self.trial.as_mut() {
            members.remove(&id);
            if members.is_empty() {
                self.trial = None;
            }
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
                next_family: 1,
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
        self.reserve_roles(&[(None, demand, None)])?
            .pop()
            .ok_or(AdmissionRefusal::InvalidBundle)
    }

    pub fn new_family(&self) -> Result<FamilyIdentity, AdmissionRefusal> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let family = state.next_family;
        state.next_family = family
            .checked_add(1)
            .ok_or(AdmissionRefusal::IdentityExhausted)?;
        Ok(FamilyIdentity {
            ledger: self.id,
            family,
        })
    }

    /// Reserve the complete missing-role set in one transaction, never by
    /// repeated single-role debits. A two-family overlap has at most six roles;
    /// this does NOT widen an ordinary single family to six. No incumbent is
    /// replaced, refunded or stolen. Only actual physical closure can debit
    /// the individual returned role reservations later.
    pub fn reserve_families(
        &self,
        families: &[FamilyDemand<'_>],
    ) -> Result<Vec<(ResourceReservation, CleanupAuthority)>, AdmissionRefusal> {
        if families.is_empty()
            || families.len() > MAX_OVERLAP_FAMILIES
            || families.iter().any(|family| {
                family.identity.ledger != self.id
                    || family.roles.is_empty()
                    || family.roles.len() > MAX_FAMILY_ROLES
            })
        {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        let mut seen_families = BTreeSet::new();
        let mut roles = Vec::new();
        for family in families {
            if !seen_families.insert(family.identity) {
                return Err(AdmissionRefusal::InvalidBundle);
            }
            let mut seen_roles = BTreeSet::new();
            for role in family.roles {
                if !seen_roles.insert(role.role) {
                    return Err(AdmissionRefusal::InvalidBundle);
                }
                roles.push((
                    Some((family.identity, role.role)),
                    role.demand,
                    role.retained,
                ));
            }
        }
        self.reserve_roles(&roles)
    }

    #[allow(clippy::type_complexity)]
    fn reserve_roles(
        &self,
        roles: &[(
            Option<(FamilyIdentity, FamilyRole)>,
            WorkloadDemand,
            Option<RetainedRole<'_>>,
        )],
    ) -> Result<Vec<(ResourceReservation, CleanupAuthority)>, AdmissionRefusal> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let mut missing = Vec::new();
        if state.invalid {
            return Err(AdmissionRefusal::Unavailable);
        }
        for (family_role, demand, retained) in roles {
            if demand.limit_epoch != state.epoch {
                return Err(AdmissionRefusal::StaleEpoch);
            }
            if let Some(held) = retained {
                if held.binding.ledger != self.id {
                    return Err(AdmissionRefusal::ReceiptMismatch);
                }
                let entry = state
                    .entries
                    .get(&held.binding.reservation)
                    .ok_or(AdmissionRefusal::UnknownReservation)?;
                if entry.binding != held.binding
                    || !matches!(
                        entry.state,
                        ReservationState::Reserved | ReservationState::SpawnPossible
                    )
                    || demand.resources != Metric::Known(entry.resources)
                    || entry.family_role.map(|(_, role)| role) != family_role.map(|(_, role)| role)
                {
                    return Err(AdmissionRefusal::InvalidBundle);
                }
                if state
                    .entries
                    .values()
                    .any(|other| other.family_role == *family_role && other.binding != held.binding)
                {
                    return Err(AdmissionRefusal::InvalidBundle);
                }
            } else {
                if family_role.is_some()
                    && state
                        .entries
                        .values()
                        .any(|entry| entry.family_role == *family_role)
                {
                    return Err(AdmissionRefusal::InvalidBundle);
                }
                missing.push((*family_role, *demand));
            }
        }
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        let trial = missing
            .first()
            .ok_or(AdmissionRefusal::InvalidBundle)?
            .1
            .trial;
        if missing.iter().any(|(_, demand)| demand.trial != trial) {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        if state
            .entries
            .len()
            .checked_add(missing.len())
            .is_none_or(|count| count > MAX_RESERVATIONS)
        {
            return Err(AdmissionRefusal::LedgerFull);
        }
        let resources: Vec<_> = missing
            .iter()
            .map(|(_, demand)| state.assess(*demand))
            .collect::<Result<_, _>>()?;
        let total = resources
            .iter()
            .try_fold(ResourceVector::default(), |sum, role| {
                sum.checked_add(*role)
            })
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        let used = state
            .used
            .checked_add(total)
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        let Metric::Known(cap) = state.cap else {
            return Err(AdmissionRefusal::InsufficientEvidence);
        };
        if let Some(dimension) = used.exceeded(cap) {
            return Err(AdmissionRefusal::HardLimit(dimension));
        }
        let first_id = state.next_reservation;
        let count =
            u64::try_from(resources.len()).map_err(|_| AdmissionRefusal::ArithmeticOverflow)?;
        let next = first_id
            .checked_add(count)
            .ok_or(AdmissionRefusal::IdentityExhausted)?;
        let mut owned = Vec::with_capacity(resources.len());
        let mut trial_members = BTreeSet::new();
        // All fallible validation precedes the single accounting commit; at
        // most six plain-u64 map entries, and no external code under this lock.
        for ((id, resources), (family_role, _)) in (first_id..next).zip(resources).zip(missing) {
            let binding = ReservationBinding {
                ledger: self.id,
                reservation: id,
                allocation_epoch: state.epoch,
            };
            state.entries.insert(
                id,
                Entry {
                    binding,
                    family_role,
                    resources,
                    state: ReservationState::Reserved,
                    custody: None,
                },
            );
            if trial {
                trial_members.insert(id);
            }
            owned.push((
                ResourceReservation {
                    ledger: self.clone(),
                    binding,
                    transferred: false,
                },
                CleanupAuthority {
                    ledger: self.clone(),
                    binding,
                },
            ));
        }
        state.used = used;
        state.next_reservation = next;
        if trial {
            state.trial = Some(trial_members);
        }
        Ok(owned)
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
    pub fn retained_role(&self) -> RetainedRole<'_> {
        RetainedRole {
            binding: self.binding,
            owner: std::marker::PhantomData,
        }
    }

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
        if self.transferred {
            // Core graph custody now owns acquisition and no-spawn certainty.
            return;
        }
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
        if let Some(custody) = entry.custody.as_ref() {
            custody.seal_entry();
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

    fn family_roles(count: usize, trial: bool) -> Vec<FamilyRoleDemand<'static>> {
        [FamilyRole::Video, FamilyRole::Audio, FamilyRole::Companion]
            .into_iter()
            .take(count)
            .map(|role| FamilyRoleDemand {
                role,
                demand: demand(1, trial),
                retained: None,
            })
            .collect()
    }

    #[test]
    fn adaptive_overlap_is_atomic_for_four_and_six_roles() {
        for count in [2, 3] {
            let ledger = ResourceLedger::new(1, Metric::Known(vector(6))).expect("ledger");
            let first = ledger.new_family().expect("first family");
            let second = ledger.new_family().expect("second family");
            let roles = family_roles(count, true);
            let mut owned = ledger
                .reserve_families(&[
                    FamilyDemand {
                        identity: first,
                        roles: &roles,
                    },
                    FamilyDemand {
                        identity: second,
                        roles: &roles,
                    },
                ])
                .expect("complete overlap");
            assert_eq!(owned.len(), count * 2);
            assert_eq!(
                ledger.snapshot().expect("snapshot").used,
                vector((count * 2) as u64)
            );
            for (reservation, _) in &mut owned {
                reservation.mark_spawn_possible().expect("spawn");
            }
            let (last, last_cleanup) = owned.pop().expect("last trial role");
            drop(owned); // Uncertain siblings retain the one graph trial claim.
            drop(last);
            last_cleanup
                .close(PhysicalCloseReceipt {
                    binding: last_cleanup.binding,
                })
                .expect("one confirmed role");
            assert!(ledger.snapshot().expect("partial closure").trial_held);
            assert!(matches!(
                ledger.reserve(demand(1, true)),
                Err(AdmissionRefusal::TrialAlreadyHeld)
            ));
        }
    }

    #[test]
    fn overlap_rejects_seventh_role_third_family_and_overflow_without_mutation() {
        let ledger = ResourceLedger::new(1, Metric::Known(vector(6))).expect("ledger");
        let identities = [
            ledger.new_family().expect("family"),
            ledger.new_family().expect("family"),
            ledger.new_family().expect("family"),
        ];
        let roles = family_roles(3, false);
        let third = family_roles(1, false);
        assert!(matches!(
            ledger.reserve_families(&[
                FamilyDemand {
                    identity: identities[0],
                    roles: &roles
                },
                FamilyDemand {
                    identity: identities[1],
                    roles: &roles
                },
                FamilyDemand {
                    identity: identities[2],
                    roles: &third
                },
            ]),
            Err(AdmissionRefusal::InvalidBundle)
        ));
        let mut four = family_roles(3, false);
        four.push(FamilyRoleDemand {
            role: FamilyRole::Video,
            demand: demand(1, false),
            retained: None,
        });
        assert!(matches!(
            ledger.reserve_families(&[FamilyDemand {
                identity: identities[0],
                roles: &four
            }]),
            Err(AdmissionRefusal::InvalidBundle)
        ));
        let mut huge = family_roles(2, false);
        huge[0].demand = demand(u64::MAX, false);
        ledger
            .update_hard_cap(2, Metric::Known(vector(u64::MAX)))
            .expect("cap");
        for role in &mut huge {
            role.demand.limit_epoch = 2;
        }
        assert!(matches!(
            ledger.reserve_families(&[FamilyDemand {
                identity: identities[0],
                roles: &huge
            }]),
            Err(AdmissionRefusal::ArithmeticOverflow)
        ));
        assert_eq!(ledger.snapshot().expect("unchanged").held, 0);
    }

    #[test]
    fn shared_retained_role_is_counted_once_and_never_rebound() {
        let ledger = ResourceLedger::new(1, Metric::Known(vector(3))).expect("ledger");
        let first = ledger.new_family().expect("first family");
        let second = ledger.new_family().expect("second family");
        let initial = [FamilyRoleDemand {
            role: FamilyRole::Audio,
            demand: demand(1, false),
            retained: None,
        }];
        let mut audio = ledger
            .reserve_families(&[FamilyDemand {
                identity: first,
                roles: &initial,
            }])
            .expect("shared audio");
        let (audio, _) = audio.first_mut().expect("audio role");
        audio.mark_spawn_possible().expect("audio spawn");
        let roles = [
            FamilyRoleDemand {
                role: FamilyRole::Video,
                demand: demand(1, false),
                retained: None,
            },
            FamilyRoleDemand {
                role: FamilyRole::Audio,
                demand: demand(1, false),
                retained: Some(audio.retained_role()),
            },
        ];
        let owned = ledger
            .reserve_families(&[
                FamilyDemand {
                    identity: first,
                    roles: &roles,
                },
                FamilyDemand {
                    identity: second,
                    roles: &roles,
                },
            ])
            .expect("two missing videos only");
        assert_eq!(owned.len(), 2);
        assert_eq!(ledger.snapshot().expect("all roles").used, vector(3));
        assert_eq!(
            ledger
                .inner
                .lock()
                .expect("state")
                .entries
                .get(&audio.binding.reservation)
                .expect("audio")
                .family_role,
            Some((first, FamilyRole::Audio))
        );
        assert!(matches!(
            ledger.reserve_families(&[FamilyDemand {
                identity: second,
                roles: &roles
            }]),
            Err(AdmissionRefusal::InvalidBundle)
        ));
        assert_eq!(
            ledger.snapshot().expect("duplicate refused").used,
            vector(3)
        );
        drop(owned); // Never spawned: only the original audio remains.
        assert_eq!(ledger.snapshot().expect("no-spawn refund").used, vector(1));
    }

    #[test]
    fn family_identity_retained_cost_and_physical_zero_are_not_waived() {
        let ledger = ResourceLedger::new(1, Metric::Known(vector(0))).expect("zero physical cap");
        let foreign = ResourceLedger::new(1, Metric::Known(vector(10))).expect("foreign ledger");
        let family = ledger.new_family().expect("family");
        let roles = family_roles(1, false);
        assert!(matches!(
            ledger.reserve_families(&[FamilyDemand {
                identity: family,
                roles: &roles
            }]),
            Err(AdmissionRefusal::HardLimit(ResourceDimension::Cpu))
        ));
        assert!(matches!(
            ledger.reserve_families(&[FamilyDemand {
                identity: foreign.new_family().expect("foreign family"),
                roles: &roles
            }]),
            Err(AdmissionRefusal::InvalidBundle)
        ));
        let (_reservation, _) = foreign
            .reserve(demand(1, false))
            .expect("foreign reservation");
        let changed = [FamilyRoleDemand {
            role: FamilyRole::Video,
            demand: demand(2, false),
            retained: Some(_reservation.retained_role()),
        }];
        assert!(matches!(
            ledger.reserve_families(&[FamilyDemand {
                identity: family,
                roles: &changed
            }]),
            Err(AdmissionRefusal::ReceiptMismatch)
        ));
        assert_eq!(ledger.snapshot().expect("unchanged zero cap").held, 0);
    }
}
