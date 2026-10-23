//! Restricted LIVE whole-graph proof, never a restart or journal audit receipt.
//!
//! The original sealed graph owns every root, pipe, audited task and read-only
//! source handle. No arbitrary task factory or external-resource dimension is
//! eligible. Durable settlement must still validate exact member facts and
//! commit before the matching cleanup owner may refund the reservation.
use super::*;
use crate::adaptive_resources::PhysicalCloseReceipt;

impl SealedPhysicalGraph {
    /// Errors preserve custody and charges; existing supervision can retry native
    /// population observations. No replacement object, timeout or callback mint.
    pub(in crate::adaptive_resources) async fn try_live_close_receipt(
        &self,
    ) -> io::Result<PhysicalCloseReceipt> {
        {
            let members = self.custody.members.lock().await;
            self.live_close_eligible(&members)?;
        }
        // Real waits, audited abort+joins and source close joins. Cancellation
        // loses only this waiter; every actual owner stays in graph custody.
        match self.close().await {
            GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable) => {}
            _ => return Err(io::Error::other("physical owners did not finish cleanup")),
        }
        let processes = self.try_live_process_closure().await?;
        let mut members = self.custody.members.lock().await;
        self.live_close_eligible(&members)?;
        if !processes.matches(self)
            || processes.roots().len() != members.roots.len()
            || processes.roots().iter().enumerate().any(|(index, root)| {
                root.index != index || root.process != members.roots[index].process
            })
        {
            return Err(io::Error::other("process proof does not cover this graph"));
        }
        check_local_owners_closed(self.custody.identity.generation, &members)?;
        // Unclaimed pipes remain in the real retained Child. Original Job zero
        // precedes disposal; no task receives or exports these handles here.
        for root in &mut members.roots {
            drop(root.child.stdin.take());
            drop(root.child.stdout.take());
            drop(root.child.stderr.take());
        }
        // Waited Child/Job owners may still retain native process/containment
        // handles. Dispose the actual owners, not just their pipes or counters,
        // AFTER observing every original Job empty and BEFORE minting proof.
        members.roots.clear();
        // No await between issuing the one nonclone proof and handing it back.
        // Concurrent reconciliation sees the same marker under this graph lock.
        members.receipt_issued = true;
        Ok(PhysicalCloseReceipt {
            binding: self.custody.binding,
            #[cfg(target_os = "linux")]
            _recovered_r: None,
        })
    }

    fn live_close_eligible(&self, members: &Members) -> io::Result<()> {
        if !self.custody.sealed.load(Ordering::Acquire)
            || !members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.receipt_issued
            || members.roots.is_empty()
            || members.writers.iter().any(|writer| writer.drain.is_none())
        {
            return Err(io::Error::other(
                "graph lacks exclusive audited live custody",
            ));
        }
        let state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| io::Error::other("resource ledger unavailable"))?;
        let entry = state
            .entries
            .get(&self.custody.binding.reservation)
            .filter(|entry| {
                !state.invalid
                    && entry.binding == self.custody.binding
                    && entry
                        .custody
                        .as_ref()
                        .is_some_and(|custody| Arc::ptr_eq(custody, &self.custody))
            })
            .ok_or_else(|| io::Error::other("original graph reservation unavailable"))?;
        // No audited terminal adapters exist for these external dimensions.
        // Never refund them just because local processes and files are gone.
        if entry.resources.hardware_slots != 0
            || entry.resources.tuner_slots != 0
            || entry.resources.scratch_bytes != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "external resource adapters are not closed by local graph proof",
            ));
        }
        Ok(())
    }
}

fn check_local_owners_closed(generation: Uuid, members: &Members) -> io::Result<()> {
    if members.source_files.iter().any(|source| !source.closed()) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "source operations or original handles remain owned",
        ));
    }
    for (index, writer) in members.writers.iter().enumerate() {
        let drain = writer
            .drain
            .as_ref()
            .ok_or_else(|| io::Error::other("unaudited writer cannot certify closure"))?;
        if !writer.joined {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "audited task has not actually joined",
            ));
        }
        if !drain.matches_writer(generation, index, &members.roots) {
            return Err(io::Error::other(
                "audited pipe ownership inventory mismatched",
            ));
        }
    }
    for (index, root) in members.roots.iter().enumerate() {
        if !root.waited
            || (root.pipes_claimed
                && members
                    .writers
                    .iter()
                    .filter(|writer| {
                        writer
                            .drain
                            .as_ref()
                            .is_some_and(|drain| drain.owns_root(index))
                    })
                    .count()
                    != 1)
        {
            return Err(io::Error::other("root or claimed pipe owner is incomplete"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
