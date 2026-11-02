//! Fixed one-input rolling copy ownership. A command is a plan, while the
//! native graph and journal cells in this module own the actual entry.
use super::operations::OwnedPreparation;
use super::*;
use crate::adaptive_resources::runtime::finite_vod::{FiniteOutputLimits, FiniteSourceRequest};
use crate::adaptive_resources::WorkloadDemand;
use crate::copy_segmenter::Limits;
use crate::fmp4::FragmentReaderBounds;
use std::time::Duration;

mod cleanup_custody;
pub(in crate::adaptive_resources::runtime) mod cleanup_memory;
mod command;
mod owner;
mod sink;

pub(in crate::adaptive_resources::runtime) use cleanup_custody::{
    CopyCleanupCustody, CopyCloseClaim,
};
pub use command::CopyPipeCommand;
pub use owner::{
    CopyCleanupHoldDiagnostic, CopyCleanupHoldReason, CopyCloseTicket, CopyFile, CopyOutput,
    CopyProductionResult, CopyServedRevision, CopyVodOwner, COPY_VOD_MAX_SEGMENT_BYTES,
};
pub(in crate::adaptive_resources) use sink::CopySink;

impl NodeResourceRuntime {
    /// Read-only maintenance diagnostic for original copy cleanup without a
    /// live close claim. Busy book cells conservatively
    /// report true; a false answer is never physical close authority.
    pub fn has_failed_copy_admission(&self) -> bool {
        self.has_failed_copy_admission_inner()
    }

    /// Retry the SAME charged copy role and original settlement. The caller
    /// invokes this from its existing reap cadence; no new timer or watchdog
    /// is created, and unknown journal work remains in the runtime book.
    pub async fn maintain_failed_copy_admissions(
        &self,
        deadline: std::time::Instant,
    ) -> Result<usize, super::finite_vod::FiniteVodError> {
        self.maintain_failed_copy_admissions_inner(deadline)
            .await
            .map_err(Into::into)
    }
}

/// A selected copy graph has an immutable source and a finite output inventory.
/// The daemon supplies policy bounds; they confer no native entry authority.
pub struct CopyVodRequest {
    pub rendition_id: String,
    /// Nonzero typed asset generation, fixed before admission.
    pub generation_epoch: u64,
    pub recipe_source_binding: String,
    pub source: FiniteSourceRequest,
    pub command: CopyPipeCommand,
    pub max_segments: u32,
    pub parser_limits: Limits,
    pub reader_bounds: FragmentReaderBounds,
    pub max_pending_fragments: usize,
    pub operation_timeout: Duration,
    pub demand: WorkloadDemand,
    /// One original daemon session/worker and its rich future, held until its
    /// registered task and all response selectors have actually disappeared.
    pub application_owner_bytes: u64,
    pub output: FiniteOutputLimits,
}

impl CopyVodRequest {
    /// First typed asset generation of a newly admitted immutable owner.
    /// The legacy scheduler's zero-based epoch is a separate counter; this
    /// value never resets or rewrites an existing admitted asset.
    pub const INITIAL_GENERATION_EPOCH: u64 = 1;

    /// The fixed parser/name cardinality. A source duration cannot prove a
    /// smaller count: the rolling strict-target cut can precede the nominal
    /// six-second floor, and EOF can emit another short segment. Exhausting
    /// this range is a typed producer failure, never an uncharged new name.
    /// The supported count is derived from the same entry and playlist
    /// layout as the sink under its existing 16 MiB allocation ceiling.
    /// Resident file count is a separate enforced rolling-window limit.
    pub fn maximum_segment_names() -> u32 {
        CopySink::maximum_admissible_segments(Limits::default())
    }
}
