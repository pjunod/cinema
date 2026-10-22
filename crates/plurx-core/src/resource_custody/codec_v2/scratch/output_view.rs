//! Nonserialized output policy shared by exact V2 and bootstrap V3 histories.
//! No parent reference or native ownership is synthesized by this view.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct OutputPlanView<'a> {
    pub asset_id: Id,
    pub generation_epoch: u64,
    pub filesystem_domain: &'a FilesystemDomain,
    pub outputs: &'a OutputNamesV1,
    pub limits: &'a ScratchLimitsV1,
    // None for V3: its rendition child can only come from bootstrap history.
    pub directory_mode: Option<&'a DirectoryModeV1>,
}
impl ScratchPlanV2 {
    pub(super) fn output_view(&self) -> OutputPlanView<'_> {
        OutputPlanView {
            asset_id: self.asset_id,
            generation_epoch: self.generation_epoch,
            filesystem_domain: &self.filesystem_domain,
            outputs: &self.outputs,
            limits: &self.limits,
            directory_mode: Some(&self.directory.mode),
        }
    }
}

pub(super) fn validate_outputs_limits(
    generation_epoch: u64,
    outputs: &OutputNamesV1,
    l: &ScratchLimitsV1,
) -> Result<(), CustodyError> {
    if generation_epoch == 0
        || outputs.segment_end_exclusive > u64::from(u32::MAX) + 1
        || outputs.segment_end_exclusive < u64::from(outputs.first_segment)
        || (!outputs.init && outputs.segment_end_exclusive == u64::from(outputs.first_segment))
        || l.resident_file_objects == 0
        || l.resident_file_objects > 65536
        || l.one_io_payload_bytes == 0
        || l.one_io_payload_bytes > l.one_file_logical_bytes
        || l.one_file_logical_bytes > l.resident_logical_bytes
        || l.pending_native_jobs != 1
        || l.reader_leases > 16
        || l.live_handles < 2
        || l.resident_memory_bytes < l.one_io_payload_bytes
        || l.directory_charge() == 0
        || l.storage_for(l.one_file_logical_bytes)?
            .checked_add(l.directory_charge())
            .is_none_or(|n| n > l.reserved_storage_bytes)
    {
        return Err(codec::invalid());
    }
    Ok(())
}
