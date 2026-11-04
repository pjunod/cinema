//! Original native absence only. This grants no application or ledger refund.
use super::*;

/// Compact, nonclone witness from the original sealed graph. Application-side
/// owners and ledger finalization remain separate runtime obligations.
pub(in crate::adaptive_resources) struct NativeNeverAcquired {
    binding: ReservationBinding,
}
impl NativeNeverAcquired {
    pub(in crate::adaptive_resources) fn binding(&self) -> ReservationBinding {
        self.binding
    }
}

impl SealedPhysicalGraph {
    pub(in crate::adaptive_resources) fn try_never_acquired(
        &self,
    ) -> io::Result<NativeNeverAcquired> {
        let state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| io::Error::other("original ledger unavailable"))?;
        let entry = state
            .entries
            .get(&self.custody.binding.reservation)
            .ok_or_else(|| io::Error::other("original reservation unavailable"))?;
        if entry.binding != self.custody.binding
            || entry.state != ReservationState::Reserved
            || !entry
                .custody
                .as_ref()
                .is_some_and(|original| Arc::ptr_eq(original, &self.custody))
        {
            return Err(io::Error::other(
                "original reservation lacks native absence",
            ));
        }
        let mut members = self
            .custody
            .members
            .try_lock()
            .map_err(|_| io::Error::other("original graph members unavailable"))?;
        if !self.custody.sealed.load(Ordering::Acquire)
            || members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.receipt_issued
            || members.finite_process_closed
            || !members.roots.is_empty()
            || !members.writers.is_empty()
            || !members.source_files.is_empty()
            || members.joined_writers != 0
            || members.abandoned_writers != 0
        {
            return Err(io::Error::other(
                "original graph has no native absence proof",
            ));
        }
        #[cfg(target_os = "linux")]
        if members.copy_process_closed || members.copy_hold.is_some() {
            return Err(io::Error::other("original copy graph remains uncertain"));
        }
        members.receipt_issued = true;
        Ok(NativeNeverAcquired {
            binding: self.custody.binding,
        })
    }
}
