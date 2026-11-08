//! Ephemeral structural support, separate from native producer authority.
//! The only profile constructor is a fixture until the real pool/frame audit.
#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Support construction remains fixture-only until the production finalizer layout is audited."
    )
)]
use super::*;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const FRESH_FOR: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SupportKind {
    Ordinary,
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Portable accounting retains the separate kind; only Linux constructs the pre-P attempt root."
        )
    )]
    Attempt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SupportIdentity {
    pub(super) kind: SupportKind,
    pub(super) ledger: u64,
    pub(super) owner: Uuid,
    pub(super) original: Uuid,
    pub(super) generation: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FinalizationSupportProfile {
    pub(super) kind: SupportKind,
    requested_bytes: u64,
    record_slots: u16,
    task_slots: u16,
}
impl FinalizationSupportProfile {
    #[cfg(test)]
    pub(super) fn fixture() -> Self {
        Self {
            kind: SupportKind::Ordinary,
            requested_bytes: 64 * 1024,
            record_slots: 1,
            task_slots: 1,
        }
    }

    #[cfg(unix)]
    pub(super) fn ordinary(requested: usize) -> Result<Self, AdmissionRefusal> {
        Ok(Self {
            kind: SupportKind::Ordinary,
            requested_bytes: u64::try_from(requested)
                .map_err(|_| AdmissionRefusal::ArithmeticOverflow)?,
            record_slots: 256,
            task_slots: 1,
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn attempt() -> Result<Self, AdmissionRefusal> {
        Ok(Self {
            kind: SupportKind::Attempt,
            requested_bytes: u64::try_from(
                crate::adaptive_resources::runtime::attempt_root::requested_storage_bytes()?,
            )
            .map_err(|_| AdmissionRefusal::ArithmeticOverflow)?,
            record_slots: 64,
            task_slots: 1,
        })
    }

    pub(super) fn resources(self) -> ResourceVector {
        ResourceVector {
            peak_memory_bytes: self.requested_bytes,
            ..ResourceVector::default()
        }
    }

    pub(super) fn fits_inline(self, requested: usize) -> bool {
        u64::try_from(requested).is_ok_and(|bytes| bytes <= self.requested_bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalizationSupportPhase {
    ReservedUnallocated,
    Active,
    Closing,
    UnknownHeld,
}

/// Requested structural storage, never a complete producer/executor peak.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct FinalizationSupportSnapshot {
    pub requested_bytes: u64,
    pub logical_record_slots: u16,
    pub logical_task_slots: u16,
    pub phase: FinalizationSupportPhase,
}

#[derive(Clone, Copy)]
pub(super) struct SupportEntry {
    pub(super) identity: SupportIdentity,
    pub(super) profile: FinalizationSupportProfile,
    pub(super) phase: FinalizationSupportPhase,
}
impl SupportEntry {
    pub(super) fn snapshot(self, unknown: bool) -> FinalizationSupportSnapshot {
        FinalizationSupportSnapshot {
            requested_bytes: self.profile.requested_bytes,
            logical_record_slots: self.profile.record_slots,
            logical_task_slots: self.profile.task_slots,
            phase: if unknown {
                FinalizationSupportPhase::UnknownHeld
            } else {
                self.phase
            },
        }
    }
}

impl State {
    fn support_entry(&self, kind: SupportKind) -> Option<SupportEntry> {
        match kind {
            SupportKind::Ordinary => self.support,
            SupportKind::Attempt => self.attempt_support,
        }
    }
    fn support_entry_mut(&mut self, kind: SupportKind) -> &mut Option<SupportEntry> {
        match kind {
            SupportKind::Ordinary => &mut self.support,
            SupportKind::Attempt => &mut self.attempt_support,
        }
    }
    pub(super) fn any_support(&self) -> bool {
        self.support.is_some() || self.attempt_support.is_some()
    }
    fn support_preflight(
        &self,
        profile: FinalizationSupportProfile,
        epoch: u64,
        now: Instant,
    ) -> Result<ResourceVector, AdmissionRefusal> {
        if self.invalid || self.draining || self.native_overage_fenced {
            return Err(AdmissionRefusal::Unavailable);
        }
        if epoch != self.epoch {
            return Err(AdmissionRefusal::StaleEpoch);
        }
        if self.support_entry(profile.kind).is_some() {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        if self.trial.is_some() {
            return Err(AdmissionRefusal::TrialAlreadyHeld);
        }
        if !self.capacity_observed_at.is_some_and(|at| {
            now.checked_duration_since(at)
                .is_some_and(|age| age <= FRESH_FOR)
        }) || !matches!(self.cap.memory_bytes, Metric::Known(_))
        {
            return Err(AdmissionRefusal::InsufficientEvidence);
        }
        if profile.requested_bytes == 0
            || !matches!(profile.record_slots, 1 | 64 | 256)
            || profile.task_slots != 1
        {
            return Err(AdmissionRefusal::InvalidBundle);
        }
        // Installation must itself preserve the unknown-cohort invariant.
        // A direct-I/O producer can have zero OS roots and still counts here.
        if self
            .entries
            .values()
            .filter(|entry| entry.memory_claim.is_active_producer())
            .count()
            > 1
        {
            return Err(AdmissionRefusal::InsufficientEvidence);
        }
        let total = self
            .used
            .checked_add(profile.resources())
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        self.cap.check(total, profile.resources(), false)?;
        Ok(total)
    }
}

impl ResourceLedger {
    pub(super) fn support_marker(&self, kind: SupportKind) -> &Arc<AtomicBool> {
        match kind {
            SupportKind::Ordinary => &self.support_unknown,
            SupportKind::Attempt => &self.attempt_support_unknown,
        }
    }

    /// Caller already owns census -> support-slot; no external allocation or
    /// await occurs while the final ledger validation/commit is held.
    pub(in crate::adaptive_resources) fn reserve_finalization_support<T>(
        &self,
        profile: FinalizationSupportProfile,
        owner: Uuid,
        original: Uuid,
        epoch: u64,
        register: impl FnOnce(SupportIdentity) -> Result<T, AdmissionRefusal>,
    ) -> Result<(SupportIdentity, T), AdmissionRefusal> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let used = state.support_preflight(profile, epoch, Instant::now())?;
        if owner.is_nil() || original.is_nil() {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        let generation = state.next_support_generation;
        let next = generation
            .checked_add(1)
            .ok_or(AdmissionRefusal::IdentityExhausted)?;
        let identity = SupportIdentity {
            kind: profile.kind,
            ledger: self.id,
            owner,
            original,
            generation,
        };
        // Every fallible ledger check precedes census insertion. Nothing
        // fallible follows the matching context's successful registration.
        let registered = register(identity)?;
        *state.support_entry_mut(profile.kind) = Some(SupportEntry {
            identity,
            profile,
            phase: FinalizationSupportPhase::ReservedUnallocated,
        });
        state.used = used;
        state.next_support_generation = next;
        self.support_marker(profile.kind)
            .store(false, Ordering::Release);
        Ok((identity, registered))
    }

    pub(in crate::adaptive_resources) fn support_transition(
        &self,
        identity: SupportIdentity,
        from: FinalizationSupportPhase,
        to: FinalizationSupportPhase,
        commit_census: impl FnOnce() -> Result<(), AdmissionRefusal>,
    ) -> Result<(), AdmissionRefusal> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let entry = state
            .support_entry_mut(identity.kind)
            .as_mut()
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.identity != identity
            || entry.phase != from
            || self.support_marker(identity.kind).load(Ordering::Acquire)
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        commit_census()?;
        entry.phase = to;
        Ok(())
    }

    pub(in crate::adaptive_resources) fn finalization_support_present(&self) -> bool {
        self.inner.lock().map_or(true, |state| state.any_support())
    }

    pub(in crate::adaptive_resources) fn finish_finalization_support<T>(
        &self,
        closed: &crate::adaptive_resources::runtime::finalization_support::FinalizationSupportClosed,
        close_census: impl FnOnce() -> Result<T, AdmissionRefusal>,
    ) -> Result<T, AdmissionRefusal> {
        let identity = closed.identity();
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let entry = state
            .support_entry(identity.kind)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.identity != identity
            || entry.phase != FinalizationSupportPhase::Closing
            || self.support_marker(identity.kind).load(Ordering::Acquire)
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        let used = state
            .used
            .checked_sub(entry.profile.resources())
            .ok_or(AdmissionRefusal::Unavailable)?;
        let result = close_census()?;
        *state.support_entry_mut(identity.kind) = None;
        state.used = used;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
