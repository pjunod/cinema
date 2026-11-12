//! One-input observed copy runtime shared by Source playback, Analysis gap
//! fills and local playback, whose owner of a range and of a generation
//! exists but which no local session uses yet. Whole-source attestation and
//! publication remain Analysis work.
#[cfg(unix)]
mod census;
#[cfg(unix)]
pub(crate) use census::{head_origin, run_source_census, CopyClockCensus};
#[cfg(unix)]
mod pipes;
#[cfg(unix)]
pub(crate) use pipes::{spawn_source_generation, CopyGenerationOwner, RunningCopyGeneration};
#[cfg(unix)]
mod observed;
#[cfg(unix)]
pub(crate) use observed::{
    copy_cut_policy, inspect_canonical_row, CopyMuxEvent, ObservedCopyMux, ObservedCopySegment,
    COPY_TARGET_SECONDS, MAX_HELD, MAX_PACKETS,
};
#[cfg(unix)]
mod census_format;
#[cfg(unix)]
mod storage;
#[cfg(all(test, unix))]
pub(crate) use storage::a_source_owner_through_its_real_binding;
#[cfg(unix)]
pub(crate) use storage::{CopyRangeFiles, CopyRangeOwner, CopyRangePublisher};

#[cfg(unix)]
mod fill;
#[cfg(unix)]
pub(crate) use fill::{fill_copy_gap, CopyGapFill, CopyGapFillError};
#[cfg(unix)]
mod production;
#[cfg(unix)]
pub(crate) use production::CopyRangeProduction;

#[cfg(unix)]
pub(crate) const SOURCE_SEEK_BOOTSTRAP_BYTES: u64 = observed::MAX_HELD as u64
    + plurx_core::transcode::COPY_SEGMENT_MAX_BYTES as u64
    + census::MAX_CENSUS_SOURCE_BYTES;
