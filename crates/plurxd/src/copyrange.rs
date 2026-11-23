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

/// What a seeked generation may read from its source before its landing is
/// verified. A source-read allowance, not a memory figure: the read that
/// sparse-cue titles have always landed within (96 MiB, one segment and the
/// census), or 16 s of the source and one segment when that is more.
#[cfg(unix)]
pub(crate) fn seek_bootstrap_bytes(file_size: i64, duration_ms: Option<i64>) -> u64 {
    const LANDING_READ_BYTES: u64 = 96 * 1024 * 1024;
    let segment = plurx_core::transcode::COPY_SEGMENT_MAX_BYTES as u64;
    LANDING_READ_BYTES
        .max(observed::admission_charge(file_size, duration_ms) / 2)
        .saturating_add(segment)
        .saturating_add(census::MAX_CENSUS_SOURCE_BYTES)
}
