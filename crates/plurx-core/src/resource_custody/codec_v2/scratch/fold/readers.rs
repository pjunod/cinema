//! Reader audit transitions. A response drop/EOF is not closure: only a
//! prearmed, joined CloseReader result removes the named handle/buffer lease.
use super::*;
impl ScratchFold {
    pub(super) fn arm_reader(
        &self,
        plan: OutputPlanView<'_>,
        io_id: Id,
        op: &AssetIoV1,
    ) -> Result<(), CustodyError> {
        match op {
            AssetIoV1::OpenReader { lease } => {
                let file = self.file(lease.file_id)?;
                if lease.asset_id != plan.asset_id
                    || lease.acquisition_io_id != io_id
                    || lease.lease_id != io_id
                    || self.readers.iter().any(|r| r.lease_id == lease.lease_id)
                    || self.copy_exists(lease.copy_id)
                    || file.published.is_none()
                    || file.temporary
                    || file.held_copy_ids.len() >= 16
                    || self.readers.len() >= plan.limits.reader_leases as usize
                    || lease.memory_cap == 0
                    || lease.read_range.start > lease.read_range.end_exclusive
                    || lease.read_range.end_exclusive > file.logical_len
                    || lease.cursor != lease.read_range.start
                {
                    return Err(codec::invalid());
                }
            }
            AssetIoV1::ReadRange { lease_id, range } => {
                let lease = self.reader(*lease_id)?;
                if range.start != lease.cursor
                    || range.end_exclusive < range.start
                    || range.end_exclusive > lease.read_range.end_exclusive
                    || range
                        .end_exclusive
                        .checked_sub(range.start)
                        .is_none_or(|n| n > lease.memory_cap)
                {
                    return Err(codec::invalid());
                }
            }
            AssetIoV1::CloseReader { lease_id } => {
                self.reader(*lease_id)?;
            }
            _ => return Err(codec::invalid()),
        }
        Ok(())
    }
    pub(super) fn observe_reader(
        &mut self,
        op: &AssetIoV1,
        observation: &AssetObservationV1,
        outcome: &NativeOutcomeV1,
    ) -> Result<(), CustodyError> {
        match (op, observation) {
            (
                AssetIoV1::OpenReader { lease: planned },
                AssetObservationV1::ReaderOpened {
                    lease,
                    native_object,
                },
            ) => {
                let file = self.file(lease.file_id)?;
                if planned != lease
                    || native_object != &file.native_object
                    || matches!(outcome, NativeOutcomeV1::NeverEntered)
                    || !matches!(native_object.device, NativeValue::Known { .. })
                    || !matches!(native_object.inode, NativeValue::Known { .. })
                {
                    return Err(codec::invalid());
                }
                // Actual adapter still must own and verify the descriptor; this
                // equality is an audit guard, never native acquisition proof.
                let file = self
                    .files
                    .iter_mut()
                    .find(|f| f.file_id == lease.file_id)
                    .ok_or_else(codec::invalid)?;
                file.held_copy_ids.push(lease.copy_id);
                self.readers.push(lease.clone());
            }
            (
                AssetIoV1::ReadRange { lease_id, range },
                AssetObservationV1::ReaderRead {
                    lease_id: observed,
                    bytes_read,
                },
            ) => {
                if lease_id != observed
                    || matches!(outcome, NativeOutcomeV1::NeverEntered)
                    || range
                        .end_exclusive
                        .checked_sub(range.start)
                        .is_none_or(|n| *bytes_read > n)
                {
                    return Err(codec::invalid());
                }
                let reader = self
                    .readers
                    .iter_mut()
                    .find(|r| r.lease_id == *lease_id)
                    .ok_or_else(codec::invalid)?;
                reader.cursor = reader
                    .cursor
                    .checked_add(*bytes_read)
                    .ok_or_else(codec::invalid)?;
            }
            (
                AssetIoV1::CloseReader { lease_id },
                AssetObservationV1::ReaderClosed { lease_id: observed },
            ) => {
                if lease_id != observed || !matches!(outcome, NativeOutcomeV1::Completed) {
                    return Err(codec::invalid());
                }
                let index = self
                    .readers
                    .iter()
                    .position(|r| r.lease_id == *lease_id)
                    .ok_or_else(codec::invalid)?;
                let lease = &self.readers[index];
                let file = self
                    .files
                    .iter_mut()
                    .find(|f| f.file_id == lease.file_id)
                    .ok_or_else(codec::invalid)?;
                let copy = file
                    .held_copy_ids
                    .iter()
                    .position(|c| *c == lease.copy_id)
                    .ok_or_else(codec::invalid)?;
                file.held_copy_ids.remove(copy);
                self.readers.remove(index);
            }
            _ => return Err(codec::invalid()),
        }
        Ok(())
    }
    fn reader(&self, id: Id) -> Result<&ReaderLeaseV1, CustodyError> {
        self.readers
            .iter()
            .find(|r| r.lease_id == id)
            .ok_or_else(codec::invalid)
    }
    pub(super) fn only_reader_copies(&self, file: &FileObservedV1) -> bool {
        file.held_copy_ids.iter().all(|copy| {
            self.readers
                .iter()
                .any(|lease| lease.file_id == file.file_id && lease.copy_id == *copy)
        })
    }
    pub(super) fn count_readers(&self, totals: &mut AssetTotalsV1) -> Result<(), CustodyError> {
        totals.readers = u32::try_from(self.readers.len()).map_err(|_| codec::invalid())?;
        for reader in &self.readers {
            totals.memory_reserved_bytes = totals
                .memory_reserved_bytes
                .checked_add(reader.memory_cap)
                .ok_or_else(codec::invalid)?;
        }
        Ok(())
    }
}
