//! One-input observed copy runtime shared by Source playback, Analysis gap
//! fills and later local playback. Whole-source attestation and publication
//! remain Analysis work.
#[cfg(unix)]
mod census;
#[cfg(unix)]
pub(crate) use census::{run_source_census, CopyClockCensus};
#[cfg(unix)]
pub(crate) mod pipes;
#[cfg(unix)]
pub(crate) use pipes::{spawn_source_generation, CopyGenerationOwner, RunningCopyGeneration};
#[cfg(all(test, target_os = "linux"))]
pub(crate) use pipes::{CopyPipeEvent, CopyPipeKind};
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
mod meter;
#[cfg(unix)]
mod production;
#[cfg(unix)]
pub(crate) use observed::admission_charge;
#[cfg(unix)]
pub(crate) use production::{CommitWaiter, CopyRangeProduction, DeadlineEnd};

/// The source read a sparse-cue title lands within before its landing is
/// verified, unchanged from before copies were charged by what they hold.
#[cfg(unix)]
const SEEK_LANDING_READ_BYTES: u64 = 96 * 1024 * 1024;
/// What a seeked generation may read from its source before its landing is
/// verified: a source-read allowance, not a memory figure. It equals the
/// allowance before copies were charged by what they hold: the landing read,
/// one segment and the census.
#[cfg(unix)]
pub(crate) const SOURCE_SEEK_BOOTSTRAP_BYTES: u64 = SEEK_LANDING_READ_BYTES
    + plurx_core::transcode::COPY_SEGMENT_MAX_BYTES as u64
    + census::MAX_CENSUS_SOURCE_BYTES;
