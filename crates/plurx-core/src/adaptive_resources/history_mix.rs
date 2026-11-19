//! Advisory history key for the actual live ledger owner multiset. This
//! neither identifies arbitrary concurrent recipes from resource numbers nor
//! predicts a second producer from one. It grants no admission right.
use super::*;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

pub(super) struct ObservationMix {
    pub(super) full: [u8; 32],
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Only live Linux finite telemetry consumes the placement projection."
        )
    )]
    pub(super) incumbents: Option<[u8; 32]>,
}

impl ResourceLedger {
    /// Hash actual incumbent unsettled owners under the same ledger lock used
    /// by admission. Bind role, claim, state, trial status and every numeric
    /// resource dimension. The selected family must own exactly one active
    /// unknown video producer with a process reservation; that selected row
    /// is excluded because its own recipe and demand have their own key fields.
    /// Every other owner must be process-free retained S or an admitted
    /// heap-only planning M role. Its live memory remains in the mix. No other active
    /// workload recipe is silently collapsed into resource totals.
    /// Random reservation IDs are omitted so equivalent resident mixtures
    /// can still have a useful prior after restart.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current live producer telemetry is Linux-only; retain portable ledger mix queries and their fixture coverage."
        )
    )]
    pub(crate) fn actual_history_mix_for(
        &self,
        selected: FamilyIdentity,
    ) -> Result<[u8; 32], AdmissionRefusal> {
        self.observation_history_mix(Some(selected), None)
            .map(|mix| mix.full)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn prospective_history_mix(&self) -> Result<[u8; 32], AdmissionRefusal> {
        self.observation_history_mix(None, None).map(|mix| mix.full)
    }

    pub(super) fn observation_history_mix(
        &self,
        selected: Option<FamilyIdentity>,
        own_metadata: Option<ReservationBinding>,
    ) -> Result<ObservationMix, AdmissionRefusal> {
        let state = if selected.is_some() {
            self.inner.lock().map_err(|_| AdmissionRefusal::Unavailable)
        } else {
            self.inner
                .try_lock()
                .map_err(|_| AdmissionRefusal::Unavailable)
        }?;
        if state.invalid || state.native_overage_fenced || state.entries.len() > MAX_RESERVATIONS {
            return Err(AdmissionRefusal::InsufficientEvidence);
        }
        let mut rows = BTreeMap::<[u8; 4 + 7 * 8], u32>::new();
        let mut own_metadata_row = None;
        let mut selected_count = 0u32;
        for entry in state.entries.values() {
            if entry.state == ReservationState::UnknownHeld {
                return Err(AdmissionRefusal::InsufficientEvidence);
            }
            let is_selected = entry.family_role.is_some_and(|(family, role)| {
                Some(family) == selected && role == FamilyRole::Video
            }) && entry.memory_claim == MemoryClaim::UnknownProducerPeak;
            if is_selected {
                if entry.resources.processes == 0 || entry.state == ReservationState::Reaping {
                    return Err(AdmissionRefusal::InsufficientEvidence);
                }
                selected_count = selected_count
                    .checked_add(1)
                    .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
            } else if entry.memory_claim.is_active_producer()
                || entry.resources.processes != 0
                || (entry.state != ReservationState::Reaping
                    && entry.memory_claim != MemoryClaim::PlanningMetadata)
            {
                return Err(AdmissionRefusal::InsufficientEvidence);
            }
            if is_selected {
                continue;
            }
            let role = match entry.family_role.map(|(_, role)| role) {
                None => 0,
                Some(FamilyRole::Video) => 1,
                Some(FamilyRole::Audio) => 2,
                Some(FamilyRole::Companion) => 3,
            };
            let claim = match entry.memory_claim {
                MemoryClaim::KnownBounded {
                    active_producer: true,
                } => 1,
                MemoryClaim::KnownBounded {
                    active_producer: false,
                } => 2,
                MemoryClaim::UnknownProducerPeak => 3,
                MemoryClaim::PlanningMetadata => 4,
            };
            let phase = match entry.state {
                ReservationState::Reserved => 1,
                ReservationState::SpawnPossible => 2,
                ReservationState::Reaping => 3,
                ReservationState::UnknownHeld => unreachable!("refused above"),
            };
            let mut row = [0; 4 + 7 * 8];
            row[..4].copy_from_slice(&[
                role,
                claim,
                phase,
                u8::from(
                    state
                        .trial
                        .as_ref()
                        .is_some_and(|trial| trial.contains(&entry.binding.reservation)),
                ),
            ]);
            let resources = entry.resources;
            for (bytes, value) in row[4..].chunks_exact_mut(8).zip([
                resources.cpu_ns_per_second,
                resources.peak_memory_bytes,
                resources.handles,
                resources.processes,
                resources.hardware_slots,
                resources.tuner_slots,
                resources.scratch_bytes,
            ]) {
                bytes.copy_from_slice(&value.to_le_bytes());
            }
            if own_metadata == Some(entry.binding)
                && entry.memory_claim == MemoryClaim::PlanningMetadata
                && entry
                    .family_role
                    .is_some_and(|(_, role)| role == FamilyRole::Companion)
                && entry.resources
                    == (ResourceVector {
                        peak_memory_bytes: entry.resources.peak_memory_bytes,
                        ..ResourceVector::default()
                    })
            {
                own_metadata_row = Some(row);
            }
            let tally = rows.entry(row).or_default();
            *tally = tally
                .checked_add(1)
                .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        }
        if selected_count != u32::from(selected.is_some()) {
            return Err(AdmissionRefusal::InsufficientEvidence);
        }
        let hash = |excluded: Option<[u8; 4 + 7 * 8]>| {
            let mut digest = Sha256::new();
            digest.update(b"plurx-actual-ledger-mix-v1");
            digest.update(state.epoch.to_le_bytes());
            for (row, count) in &rows {
                let count = count - u32::from(excluded.as_ref() == Some(row));
                if count != 0 {
                    digest.update((row.len() as u64).to_le_bytes());
                    digest.update(row);
                    digest.update(count.to_le_bytes());
                }
            }
            digest.finalize().into()
        };
        let full = hash(None);
        // The current mix for placement precedes the asker's P/M and R.
        // Omit exactly one original M row, and only from this observation.
        // Every other owner remains visible, including equal-valued M rows.
        let incumbents = own_metadata_row.map(|row| hash(Some(row)));
        Ok(ObservationMix { full, incumbents })
    }
}
