//! One Analysis-owned generation of the observed copy engine. It produces the
//! canonical rows a playback range left unobserved: the same recipe, mux,
//! normalizer and companion verification, with nothing kept for serving.
use super::{
    production::landing_failure, run_source_census, spawn_source_generation, CopyClockCensus,
    CopyGenerationOwner, CopyMuxEvent, ObservedCopyMux,
};
use crate::{
    copy_analysis::{AnalysisCopyRows, CopyRecipe, CopyRowInspection},
    copy_normalization::CopyNormalizationContract,
    prodrun::ProducerSlot,
    source_input::{SourceInput, SourceSeekBootstrap},
};
use plurx_core::{
    domain::MediaFile,
    fmp4::Init,
    hevc_configuration::RangeInspection,
    segplan::{
        ranges::{RangeBoundaries, RangeGap},
        IndexRow,
    },
    store::analysis_coordination::AnalysisPhysicalPhase,
};
use std::{future::Future, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) struct CopyGapFill<'a> {
    pub(crate) file: &'a MediaFile,
    pub(crate) recipe: &'a CopyRecipe,
    /// The exact phase whose active custody every Input here reads under.
    pub(crate) phase: &'a AnalysisPhysicalPhase,
    /// What the rows already observed for this source were produced under.
    /// None when nothing has been observed: this generation's own head then
    /// establishes the contract instead of being checked against one.
    pub(crate) init_contract_sha256: Option<&'a str>,
    pub(crate) canonical_title_start: u64,
    pub(crate) timescale: u32,
    pub(crate) gap: RangeGap,
    pub(crate) max_rows: usize,
    pub(crate) runtime_cache: &'a Path,
    /// The phase's loss token. There is no deadline: a paused budget waits.
    pub(crate) stop: &'a CancellationToken,
}

#[derive(Debug)]
pub(crate) enum CopyGapFillError {
    /// This attempt did not finish; the observations already held stand.
    Failed(String),
    /// The source no longer produces what the held observations bind.
    Contradicted(String),
    /// This gap can never be filled by repeating the same generation: the
    /// answer is a property of the file, not of this attempt.
    Refused(String),
}

/// Causes that recur on every repeat of the same generation, so a retry only
/// spends another phase. Anything else (an I/O error, a lost reader, a
/// cancelled phase) is this attempt's and is retried.
fn failure(reason: String) -> CopyGapFillError {
    match reason.as_str() {
        // More rows than one index blob can hold.
        "copy_range_row_limit"
        // The container exposes no format start to place a seek against.
        | "copy_clock_unknown"
        // The seek landed beyond the bootstrap allowance (sparse cues), or
        // landed after the gap's first missing row.
        | "copy_sync_unverified"
        | "copy_fill_landed_late"
        // The muxer wrote a second init: the decoder contract changed.
        | "copy_init_contract_changed" => CopyGapFillError::Refused(reason),
        // The held object changed after the phase resolved it.
        "source changed after cluster index resolution" => {
            CopyGapFillError::Contradicted(reason)
        }
        _ => CopyGapFillError::Failed(reason),
    }
}

enum FillEnd {
    /// A row at or past the gap's end was verified: the child is retired.
    NextObservedRow,
    Eof(Option<RangeInspection>),
}

struct FillBatch<'a> {
    census: &'a CopyClockCensus,
    seek_bootstrap: &'a SourceSeekBootstrap,
    wanted_ms: i64,
    max_rows: usize,
    init: Option<Arc<Init>>,
    rows: Vec<IndexRow>,
    row_inspections: Vec<CopyRowInspection>,
}
impl FillBatch<'_> {
    fn apply(&mut self, events: Vec<CopyMuxEvent>) -> Result<(), String> {
        for event in events {
            match event {
                CopyMuxEvent::Init { canonical, .. } => {
                    if self.init.replace(canonical).is_some() {
                        return Err("copy_init_contract_changed".into());
                    }
                }
                CopyMuxEvent::Origin { first_packet, .. } => {
                    self.census
                        .verify_seek_landing(first_packet, self.wanted_ms)?;
                    self.seek_bootstrap.verified_landing();
                }
                // The kernel verified this segment's AV companion against its
                // canonical rows before emitting it. Only now is the AV dropped.
                CopyMuxEvent::Segment(output) => {
                    if self.rows.len().saturating_add(output.segment.rows.len()) > self.max_rows {
                        return Err("copy_range_row_limit".into());
                    }
                    self.rows.extend(output.segment.rows);
                    self.row_inspections.extend(output.row_inspections);
                }
            }
        }
        Ok(())
    }
}

/// Where a generation that fills `gap` asks its child to start, in film
/// milliseconds from the container's format start. The gap's first missing
/// row begins at `gap.start`; a keyframe presented later than that decode
/// time lands the seek on the row before it, which is then compared for
/// equality with the row already observed there.
fn fill_start_ms(
    census: &CopyClockCensus,
    gap: RangeGap,
    canonical_title_start: u64,
    timescale: u32,
) -> Result<i64, String> {
    match gap.start {
        None => Ok(0),
        Some(ticks) => census
            .media_origin_ms(ticks, canonical_title_start, 0, timescale)
            .map(|start| start.max(0)),
    }
}

fn reached(gap: RangeGap, rows: &[IndexRow]) -> bool {
    gap.end
        .is_some_and(|end| rows.last().is_some_and(|row| row.dts >= end))
}

/// Runs the head census and one primary generation, both owned by `phase`,
/// and returns only after every child, pipe task and Input has joined.
/// `open` yields a fresh background Input of the exact held object, read
/// under the phase's budget; each Input serves one child.
pub(crate) async fn fill_copy_gap<Open, Opening>(
    fill: CopyGapFill<'_>,
    mut open: Open,
) -> Result<AnalysisCopyRows, CopyGapFillError>
where
    Open: FnMut() -> Opening,
    Opening: Future<Output = Result<SourceInput, String>>,
{
    use CopyGapFillError::{Contradicted, Failed};
    let file = fill.file;
    let failed = failure;
    let video = fill.recipe.video;
    let census = run_source_census(
        open().await.map_err(failed)?,
        fill.phase.source.object_version.clone(),
        file,
        video,
        CopyGenerationOwner::Analysis { phase: fill.phase },
        Arc::new(ProducerSlot::new()),
        fill.runtime_cache,
        None,
        fill.stop,
    )
    .await
    .map_err(failed)?;
    if census.format_start.is_none() {
        return Err(failed("copy_clock_unknown".into()));
    }
    let contract = Arc::new(
        CopyNormalizationContract::from_head(
            file,
            video,
            &census.raw_canonical_init,
            &census.raw_canonical_fragment,
        )
        .map_err(|error| Failed(error.to_string()))?,
    );
    if fill
        .init_contract_sha256
        .is_some_and(|held| held != contract.init_contract_sha256())
    {
        return Err(Contradicted("copy_init_contract_changed".into()));
    }
    let wanted_ms = fill_start_ms(
        &census,
        fill.gap,
        fill.canonical_title_start,
        fill.timescale,
    )
    .map_err(Failed)?;
    let mut mux = ObservedCopyMux::new(
        Arc::clone(&contract),
        census.first_dts,
        fill.canonical_title_start,
        fill.recipe.tracks(file),
        video.retains_hevc_parameter_sets(),
        file.video_codec.as_deref() == Some("hevc"),
    )
    .map_err(Failed)?;
    let args = fill
        .recipe
        .args(file, wanted_ms as f64 / 1000.0)
        .map_err(|error| Failed(error.into()))?;
    // The census Input and its readers are joined. A seeked start may read
    // only the bounded bootstrap allowance until its landing is verified.
    let input = open().await.map_err(failed)?;
    let seek_bootstrap = match input.seal_seek_bootstrap_limit(super::SOURCE_SEEK_BOOTSTRAP_BYTES) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = input.close_and_wait().await;
            return Err(Failed(error));
        }
    };
    let mut running = spawn_source_generation(
        input,
        &file.path,
        &args,
        fill.runtime_cache,
        CopyGenerationOwner::Analysis { phase: fill.phase },
        Arc::new(ProducerSlot::new()),
    )
    .await
    .map_err(Failed)?;
    let mut batch = FillBatch {
        census: &census,
        seek_bootstrap: &seek_bootstrap,
        wanted_ms,
        max_rows: fill.max_rows,
        init: None,
        rows: Vec::new(),
        row_inspections: Vec::new(),
    };
    let observed = async {
        loop {
            let pipes = running
                .pipes
                .as_mut()
                .ok_or("copy_output_readers_missing")?;
            let event = tokio::select! {biased;
                ()=fill.stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),
                event=pipes.next()=>event.ok_or("copy_output_readers_lost")?,
            };
            let eof = pipes.all_eof();
            batch.apply(mux.feed(event)?)?;
            if reached(fill.gap, &batch.rows) {
                return Ok(FillEnd::NextObservedRow);
            }
            if eof {
                break;
            }
        }
        running
            .slot
            .request_registered_completion(&running.registration)
            .await
            .map_err(|_| "copy_reap_unavailable".to_owned())?;
        let status = tokio::select! {biased;
            ()=fill.stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),
            status=running.registration.wait_child_exit()=>status,
        };
        if !status.success() {
            return Err("copy_child_failed".into());
        }
        running.join_completed_outputs().await?;
        let (tail, inspection) = mux.finish()?;
        batch.apply(tail)?;
        Ok::<_, String>(FillEnd::Eof(inspection))
    }
    .await;
    let observed = observed.map_err(|error| landing_failure(&seek_bootstrap, error));
    // Retirement, every pipe task, the Input and the reap join here, on
    // success, early stop and failure alike, before the phase can finish.
    let settled = running
        .settle(matches!(observed, Ok(FillEnd::Eof(_))))
        .await;
    let end = observed.map_err(failed)?;
    let receipt = settled.map_err(Failed)?;
    let (eof, inspection) = match end {
        FillEnd::Eof(inspection) => {
            if !receipt.exit_success() {
                return Err(Failed("copy_child_failed".into()));
            }
            (true, inspection)
        }
        FillEnd::NextObservedRow => (false, None),
    };
    let FillBatch {
        init,
        rows,
        row_inspections,
        ..
    } = batch;
    let init = init.ok_or_else(|| Failed("copy_init_missing".into()))?;
    let first = rows
        .first()
        .ok_or_else(|| Failed("copy_fill_empty".into()))?
        .dts;
    // The generation must cover the gap's first missing row itself; a later
    // landing would leave it open and repeat identically on every retry.
    if first > fill.gap.start.unwrap_or(fill.canonical_title_start) {
        return Err(failed("copy_fill_landed_late".into()));
    }
    let begins_title = first == fill.canonical_title_start;
    Ok(AnalysisCopyRows {
        init,
        source_origin: census.first_dts,
        canonical_title_start: fill.canonical_title_start,
        rows,
        boundaries: RangeBoundaries { begins_title, eof },
        inspection,
        row_inspections,
        promotion: contract.promotion().clone(),
    })
}
