//! One accounting transaction that hands a positively closed producer's charge
//! to its successor generation: no moment holds both charges, and no moment
//! holds neither while the successor still needs it.
//!
//! The predecessor stays charged until its physical close receipt. The
//! successor's reservation is then made in the same ledger critical section
//! that releases the predecessor. A successor demand no larger than the
//! predecessor's is a pure rebind and is never re-admitted. A larger demand
//! is admitted with the predecessor removed, so only the difference must fit.
//! A refusal changes nothing; the caller then closes the predecessor through
//! its ordinary receipt, which refunds it whole.
//!
//! The successor reservation is ordinary and not yet durable. Its journal
//! obligation is written only after this commit, so a restart before that
//! write recovers neither charge: a clean refund, never two entries.
// The finite successor generation that calls this lands in the next commit.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "wired by the finite successor generation")
)]
use super::{
    AdmissionRefusal, CleanupAuthority, Entry, FamilyDemand, FamilyIdentity, FamilyRole,
    MemoryClaim, Metric, PhysicalCloseReceipt, ReservationBinding, ReservationState,
    ResourceLedger, ResourceReservation, ResourceVector, State, TaskClaim, WorkloadDemand,
};

impl ResourceLedger {
    /// Replace the closed `predecessor`'s charge with exactly one successor
    /// role reservation. `families` names one family with one role, which
    /// must not adopt a retained role.
    pub(crate) fn rebind_closed_successor(
        &self,
        predecessor: &CleanupAuthority,
        receipt: &PhysicalCloseReceipt,
        families: &[FamilyDemand<'_>],
        claim: MemoryClaim,
    ) -> Result<(ResourceReservation, CleanupAuthority), AdmissionRefusal> {
        if predecessor.ledger.id != self.id || receipt.custody_binding() != predecessor.binding {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        let [family] = families else {
            return Err(AdmissionRefusal::InvalidBundle);
        };
        let [role] = family.roles else {
            return Err(AdmissionRefusal::InvalidBundle);
        };
        if family.identity.ledger != self.id || role.retained.is_some() || role.demand.trial {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        rebind_locked(
            self,
            &mut state,
            predecessor.binding,
            (Some((family.identity, role.role)), role.demand),
            claim,
        )
    }
}

/// The successor transaction under the held lock, for a caller that has
/// already validated the predecessor's close and cleared its durable
/// operation identity under this same lock.
pub(super) fn rebind_locked(
    ledger: &ResourceLedger,
    state: &mut State,
    predecessor: ReservationBinding,
    (family_role, demand): (Option<(FamilyIdentity, FamilyRole)>, WorkloadDemand),
    claim: MemoryClaim,
) -> Result<(ResourceReservation, CleanupAuthority), AdmissionRefusal> {
    if state.invalid || state.draining {
        return Err(AdmissionRefusal::Unavailable);
    }
    if demand.limit_epoch != state.epoch {
        return Err(AdmissionRefusal::StaleEpoch);
    }
    let Metric::Known(resources) = demand.resources else {
        return Err(AdmissionRefusal::InsufficientEvidence);
    };
    if resources == ResourceVector::default() {
        return Err(AdmissionRefusal::InsufficientEvidence);
    }
    let old = state
        .entries
        .get(&predecessor.reservation)
        .ok_or(AdmissionRefusal::UnknownReservation)?;
    if old.binding != predecessor
        || old.durable.is_some()
        || old.state == ReservationState::Reserved
        || old.memory_claim != claim
        || state
            .trial
            .as_ref()
            .is_some_and(|members| members.contains(&predecessor.reservation))
    {
        return Err(AdmissionRefusal::InvalidTransition);
    }
    if state.native_overages.values().any(|overage| {
        overage.binding == predecessor && overage.observed_extra != ResourceVector::default()
    }) {
        return Err(AdmissionRefusal::InvalidTransition);
    }
    // A contained predecessor's task ceilings are re-derived by admission.
    let no_larger =
        old.contained.is_none() && super::retained::componentwise_le(resources, old.resources);
    let old_resources = old.resources;
    let old_task_claim = old.task_claim;
    let Some(without_predecessor) = state.used.checked_sub(old_resources) else {
        state.invalid = true;
        return Err(AdmissionRefusal::Unavailable);
    };
    let removed = state
        .entries
        .remove(&predecessor.reservation)
        .expect("validated predecessor");
    let used_before = state.used;
    state.used = without_predecessor;
    let result = if no_larger {
        // The successor's charge already fits inside the predecessor's: no
        // re-admission, so capacity drift since then cannot refuse it.
        commit_rebound(ledger, state, family_role, resources, claim, old_task_claim)
    } else {
        ledger
            .reserve_roles_locked(
                state,
                &[(family_role, demand, None)],
                Some(&[claim]),
                false,
                None,
            )
            .and_then(|mut owned| owned.pop().ok_or(AdmissionRefusal::InvalidBundle))
    };
    match result {
        Ok(owned) => {
            state
                .native_overages
                .retain(|_, overage| overage.binding != predecessor);
            drop(removed);
            Ok(owned)
        }
        Err(refusal) => {
            // Nothing changed: the predecessor keeps its whole charge until
            // its ordinary close refunds it.
            state.used = used_before;
            state.entries.insert(predecessor.reservation, removed);
            Err(refusal)
        }
    }
}

fn commit_rebound(
    ledger: &ResourceLedger,
    state: &mut State,
    family_role: Option<(FamilyIdentity, FamilyRole)>,
    resources: ResourceVector,
    memory_claim: MemoryClaim,
    task_claim: TaskClaim,
) -> Result<(ResourceReservation, CleanupAuthority), AdmissionRefusal> {
    if family_role.is_some()
        && state
            .entries
            .values()
            .any(|entry| entry.family_role == family_role)
    {
        return Err(AdmissionRefusal::InvalidBundle);
    }
    let id = state.next_reservation;
    let next = id
        .checked_add(1)
        .ok_or(AdmissionRefusal::IdentityExhausted)?;
    if id == 0 || state.entries.contains_key(&id) {
        return Err(AdmissionRefusal::IdentityExhausted);
    }
    let used = state
        .used
        .checked_add(resources)
        .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
    let binding = ReservationBinding {
        ledger: ledger.id,
        reservation: id,
        allocation_epoch: state.epoch,
    };
    state.entries.insert(
        id,
        Entry {
            binding,
            family_role,
            resources,
            memory_claim,
            task_claim,
            contained: None,
            state: ReservationState::Reserved,
            custody: None,
            durable: None,
        },
    );
    state.used = used;
    state.next_reservation = next;
    Ok((
        ResourceReservation {
            ledger: ledger.clone(),
            binding,
            transferred: false,
        },
        CleanupAuthority {
            ledger: ledger.clone(),
            binding,
        },
    ))
}

#[cfg(test)]
#[path = "successor/tests.rs"]
mod tests;
