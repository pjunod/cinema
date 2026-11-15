//! One-input observed copy runtime shared by Source playback, Analysis gap
//! fills and later local playback. Whole-source attestation and publication
//! remain Analysis work.
#[cfg(unix)]
mod census;
#[cfg(unix)]
pub(crate) use census::{run_source_census, CopyClockCensus};
#[cfg(unix)]
mod pipes;
#[cfg(unix)]
pub(crate) use pipes::{spawn_source_generation, CopyGenerationOwner, RunningCopyGeneration};
#[cfg(unix)]
mod observed;
#[cfg(unix)]
pub(crate) use observed::{CopyMuxEvent, ObservedCopyMux, ObservedCopySegment};
#[cfg(unix)]
mod census_format;
#[cfg(unix)]
mod storage;
#[cfg(unix)]
pub(crate) use storage::{CopyRangeFiles, CopyRangePublisher};

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
