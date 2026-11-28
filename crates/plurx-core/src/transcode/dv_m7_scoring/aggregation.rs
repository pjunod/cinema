//! Exact rank selection over frame, window and pooled M7 samples.
//!
//! The caller joins the decoded frames and supplies the attested sample count
//! for each window. The spill retains f64 values in sorted runs partitioned by
//! model, arm, reference band and frame. Merging the runs selects the original
//! sample at each rank. Means use compensated summation in sorted order.

use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    fs::File,
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    mem::size_of,
    path::Path,
};

use serde::{Deserialize, Serialize};

pub use super::capture::MAX_SITES_PER_FRAME;
use super::{Model, ModelSampleScore, LUMINANCE_BAND_UPPER_NITS};

/// Version of the exact-rank, compensated-mean managed Rust contract.
pub const AGGREGATION_REVISION: u32 = 1;

const MODELS: usize = Model::ALL.len();
const BANDS: usize = LUMINANCE_BAND_UPPER_NITS.len() + 1;
const F64_BYTES: usize = size_of::<f64>();
const BUFFER_VALUES: usize = 1024;
const BUFFER_BYTES: usize = BUFFER_VALUES * F64_BYTES;
const SPILL_BYTES_PER_SITE: u64 = (MODELS * 3 * F64_BYTES) as u64;
pub const MAX_FRAMES_PER_WINDOW: usize = 48;
pub const MAX_EVALUATION_WINDOWS: usize = 8;
// RGB16 PQ inputs have ICtCp components bounded by the matrix row sums.
// This upper bound is wider than their maximum possible Delta E ITP.
const MAX_DELTA_E_ITP: f64 = 65_535.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowShape {
    pub frames: usize,
    pub sites_per_frame: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum AggregationError {
    #[error("unsupported M7 evaluation window or lattice size")]
    UnsupportedShape,
    #[error("M7 needs {required} spill bytes, with {budget} admitted")]
    SpillBudget { required: u64, budget: u64 },
    #[error("M7 frame or lattice coverage differs from the window plan")]
    Coverage,
    #[error("M7 comparison includes invalid luminance or Delta E ITP")]
    InvalidScore,
    #[error("M7 requires a finite, nonnegative frozen improvement margin")]
    InvalidMargin,
    #[error("M7 aggregation was cancelled")]
    Cancelled,
    #[error("M7 spill length or run coverage differs")]
    SpillCoverage,
    #[error("M7 spill I/O: {0}")]
    SpillIo(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FrozenMargin(f64);

impl FrozenMargin {
    /// Takes the margin after the caller has frozen it against calibration.
    pub fn new(delta_e_itp: f64) -> Result<Self, AggregationError> {
        if delta_e_itp.is_finite() && delta_e_itp >= 0.0 {
            Ok(Self(delta_e_itp))
        } else {
            Err(AggregationError::InvalidMargin)
        }
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Distribution {
    pub count: usize,
    pub mean: f64,
    pub min: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BandedDistribution {
    pub full: Distribution,
    /// An empty reference band has no statistics and a sample count of zero.
    pub bands: [Option<Distribution>; BANDS],
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairedDistribution {
    pub advantage: Distribution,
    pub candidate_closer_count: usize,
    pub candidate_worse_count: usize,
    pub ties: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStatistics {
    pub served_fplus: BandedDistribution,
    pub candidate: BandedDistribution,
    pub paired: PairedDistribution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Passed,
    Failed,
    MissingBand,
}

impl Check {
    fn from_test(passed: bool) -> Self {
        if passed {
            Self::Passed
        } else {
            Self::Failed
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeanGain {
    pub delta_e_itp: f64,
    pub check: Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowChecks {
    pub mean_gain: MeanGain,
    pub shadow_mean: Check,
    pub shadow_p99: Check,
    pub dark_p99: Check,
    pub largest_frame_shadow_p99: Check,
    pub largest_served_fplus_frame_shadow_p99: Option<f64>,
    pub largest_candidate_frame_shadow_p99: Option<f64>,
}

impl WindowChecks {
    pub(in crate::transcode) fn checks(self) -> [Check; 5] {
        [
            self.mean_gain.check,
            self.shadow_mean,
            self.shadow_p99,
            self.dark_p99,
            self.largest_frame_shadow_p99,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Refused,
    Inconclusive,
    Satisfied,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct WindowReport {
    /// Input frame order, with each array in Model::ALL order.
    pub frames: Vec<[ModelStatistics; MODELS]>,
    pub statistics: [ModelStatistics; MODELS],
    pub checks: [WindowChecks; MODELS],
}

#[derive(Debug, PartialEq, Serialize)]
pub struct ComparisonReport {
    pub margin: FrozenMargin,
    /// Evaluation windows in input order. Calibration is excluded.
    pub windows: Vec<WindowReport>,
    pub pooled_statistics: [ModelStatistics; MODELS],
    pub pooled_mean_gain: [MeanGain; MODELS],
    pub disposition: Disposition,
}

/// Owns an ephemeral spill inside the caller's admitted job directory.
/// A failed or cancelled operation invalidates the measurement.
pub struct Aggregator {
    spill: BufWriter<File>,
    spill_bytes: u64,
    offset: u64,
    plan: Vec<WindowShape>,
    frame_sites: Vec<usize>,
    runs: Vec<[ModelRuns; MODELS]>,
    margin: FrozenMargin,
    failed: bool,
}

impl Aggregator {
    /// Calculate the exact file length before opening the spill.
    pub fn required_spill_bytes(plan: &[WindowShape]) -> Result<u64, AggregationError> {
        if plan.is_empty()
            || plan.len() > MAX_EVALUATION_WINDOWS
            || plan.iter().any(|window| {
                !(1..=MAX_FRAMES_PER_WINDOW).contains(&window.frames)
                    || !(1..=MAX_SITES_PER_FRAME).contains(&window.sites_per_frame)
            })
        {
            return Err(AggregationError::UnsupportedShape);
        }
        plan.iter()
            .try_fold(0_u64, |total, window| {
                let sites = (window.frames as u64).checked_mul(window.sites_per_frame as u64)?;
                total.checked_add(sites.checked_mul(SPILL_BYTES_PER_SITE)?)
            })
            .ok_or(AggregationError::UnsupportedShape)
    }

    pub fn new(
        job_dir: &Path,
        plan: &[WindowShape],
        margin: FrozenMargin,
        spill_budget_bytes: u64,
    ) -> Result<Self, AggregationError> {
        let spill_bytes = Self::required_spill_bytes(plan)?;
        if spill_bytes > spill_budget_bytes {
            return Err(AggregationError::SpillBudget {
                required: spill_bytes,
                budget: spill_budget_bytes,
            });
        }
        let frame_sites: Vec<_> = plan
            .iter()
            .flat_map(|w| std::iter::repeat_n(w.sites_per_frame, w.frames))
            .collect();
        Ok(Self {
            spill: BufWriter::new(tempfile::tempfile_in(job_dir)?),
            spill_bytes,
            offset: 0,
            plan: plan.to_vec(),
            runs: Vec::with_capacity(frame_sites.len()),
            frame_sites,
            margin,
            failed: false,
        })
    }

    /// Add one completely validated frame in source lattice order.
    pub fn push_frame(
        &mut self,
        scores: &[[ModelSampleScore; MODELS]],
        cancelled: impl Fn() -> bool,
    ) -> Result<(), AggregationError> {
        if self.failed {
            return Err(AggregationError::Coverage);
        }
        let result = self.capture_frame(scores, &cancelled);
        self.failed = result.is_err();
        result
    }

    fn capture_frame(
        &mut self,
        scores: &[[ModelSampleScore; MODELS]],
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), AggregationError> {
        if self.frame_sites.get(self.runs.len()) != Some(&scores.len()) {
            return Err(AggregationError::Coverage);
        }
        for chunk in scores.chunks(BUFFER_VALUES) {
            check_cancelled(cancelled)?;
            if !chunk.iter().flatten().copied().all(valid_score) {
                return Err(AggregationError::InvalidScore);
            }
        }

        let mut models = [ModelRuns::default(); MODELS];
        for (model, runs) in models.iter_mut().enumerate() {
            let mut served: [Vec<f64>; BANDS] = std::array::from_fn(|_| Vec::new());
            let mut candidate: [Vec<f64>; BANDS] = std::array::from_fn(|_| Vec::new());
            let mut paired = Vec::with_capacity(scores.len());
            for chunk in scores.chunks(BUFFER_VALUES) {
                check_cancelled(cancelled)?;
                for row in chunk {
                    let score = row[model];
                    let band = score.luminance_band();
                    served[band].push(score.served_fplus_delta_e_itp);
                    candidate[band].push(score.candidate_delta_e_itp);
                    paired.push(score.paired_advantage());
                }
            }
            for (band, (fplus, np1)) in served.iter_mut().zip(&mut candidate).enumerate() {
                runs.served[band] = self.append_run(fplus, cancelled)?;
                runs.candidate[band] = self.append_run(np1, cancelled)?;
            }
            runs.paired = self.append_run(&mut paired, cancelled)?;
        }
        self.runs.push(models);
        Ok(())
    }

    fn append_run(
        &mut self,
        values: &mut [f64],
        cancelled: &impl Fn() -> bool,
    ) -> Result<Run, AggregationError> {
        check_cancelled(cancelled)?;
        values.sort_unstable_by(f64::total_cmp);
        let run = Run {
            offset: self.offset,
            count: values.len(),
        };
        let end = self.offset + (values.len() * F64_BYTES) as u64;
        if end > self.spill_bytes {
            return Err(AggregationError::SpillCoverage);
        }
        let mut bytes = [0; BUFFER_BYTES];
        for chunk in values.chunks(BUFFER_VALUES) {
            check_cancelled(cancelled)?;
            for (value, slot) in chunk.iter().zip(bytes.chunks_exact_mut(F64_BYTES)) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
            self.spill.write_all(&bytes[..chunk.len() * F64_BYTES])?;
        }
        self.offset = end;
        Ok(run)
    }

    /// Consume a complete capture and check every window and pooled mean.
    pub fn finish(
        mut self,
        cancelled: impl Fn() -> bool,
    ) -> Result<ComparisonReport, AggregationError> {
        check_cancelled(&cancelled)?;
        if self.failed || self.runs.len() != self.frame_sites.len() {
            return Err(AggregationError::Coverage);
        }
        self.spill.flush()?;
        let spill = self.spill.get_mut();
        if self.offset != self.spill_bytes || spill.metadata()?.len() != self.spill_bytes {
            return Err(AggregationError::SpillCoverage);
        }

        let mut first = 0;
        let mut windows = Vec::with_capacity(self.plan.len());
        for shape in self.plan {
            let last = first + shape.frames;
            let range = &self.runs[first..last];
            let mut frames = Vec::with_capacity(shape.frames);
            for frame in range {
                frames.push(statistics(spill, std::slice::from_ref(frame), &cancelled)?);
            }
            let statistics = if let [frame] = frames.as_slice() {
                *frame
            } else {
                statistics(spill, range, &cancelled)?
            };
            let checks = std::array::from_fn(|model| {
                window_checks(&statistics[model], &frames, model, self.margin)
            });
            windows.push(WindowReport {
                frames,
                statistics,
                checks,
            });
            first = last;
        }
        let pooled_statistics = if let [window] = windows.as_slice() {
            window.statistics
        } else {
            statistics(spill, &self.runs, &cancelled)?
        };
        let pooled_mean_gain = pooled_statistics.map(|model| mean_gain(&model, self.margin));
        let disposition = comparison_disposition(
            windows
                .iter()
                .flat_map(|window| window.checks.into_iter().flat_map(WindowChecks::checks))
                .chain(pooled_mean_gain.map(|gain| gain.check)),
        );
        check_cancelled(&cancelled)?;
        Ok(ComparisonReport {
            margin: self.margin,
            windows,
            pooled_statistics,
            pooled_mean_gain,
            disposition,
        })
    }
}

fn valid_score(score: ModelSampleScore) -> bool {
    score.reference_luminance_nits.is_finite()
        && score.reference_luminance_nits >= 0.0
        && (0.0..=MAX_DELTA_E_ITP).contains(&score.served_fplus_delta_e_itp)
        && (0.0..=MAX_DELTA_E_ITP).contains(&score.candidate_delta_e_itp)
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), AggregationError> {
    if cancelled() {
        Err(AggregationError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Run {
    offset: u64,
    count: usize,
}

#[derive(Clone, Copy, Default)]
struct ModelRuns {
    served: [Run; BANDS],
    candidate: [Run; BANDS],
    paired: Run,
}

struct Cursor {
    offset: u64,
    remaining: usize,
    buffer: Vec<u8>,
    index: usize,
}

impl Cursor {
    fn new(run: Run) -> Self {
        let buffer = vec![0; run.count.min(BUFFER_VALUES) * F64_BYTES];
        Self {
            offset: run.offset,
            remaining: run.count,
            index: buffer.len(),
            buffer,
        }
    }

    fn next(&mut self, spill: &mut File) -> Result<Option<u64>, AggregationError> {
        if self.index == self.buffer.len() {
            if self.remaining == 0 {
                return Ok(None);
            }
            let count = self.remaining.min(BUFFER_VALUES);
            self.buffer.resize(count * F64_BYTES, 0);
            spill.seek(SeekFrom::Start(self.offset))?;
            spill.read_exact(&mut self.buffer)?;
            self.offset += self.buffer.len() as u64;
            self.remaining -= count;
            self.index = 0;
        }
        let end = self.index + F64_BYTES;
        let raw = u64::from_le_bytes(self.buffer[self.index..end].try_into().expect("f64 bytes"));
        self.index = end;
        Ok(Some(raw))
    }
}

fn ordered_bits(raw: u64) -> u64 {
    // Match f64::total_cmp, including -0 before +0.
    const SIGN: u64 = 1 << 63;
    if raw & SIGN == 0 {
        raw ^ SIGN
    } else {
        !raw
    }
}

fn from_ordered_bits(key: u64) -> f64 {
    const SIGN: u64 = 1 << 63;
    f64::from_bits(if key & SIGN == 0 { !key } else { key ^ SIGN })
}

fn merge(
    spill: &mut File,
    runs: &[(usize, Run)],
    cancelled: &impl Fn() -> bool,
    mut observe: impl FnMut(usize, f64),
) -> Result<(), AggregationError> {
    let mut cursors = Vec::with_capacity(runs.len());
    let mut heap = BinaryHeap::with_capacity(runs.len());
    for &(band, run) in runs {
        check_cancelled(cancelled)?;
        if run.count != 0 {
            let index = cursors.len();
            let mut cursor = Cursor::new(run);
            let first = cursor.next(spill)?.ok_or(AggregationError::SpillCoverage)?;
            heap.push(Reverse((ordered_bits(first), index)));
            cursors.push((band, cursor));
        }
    }

    let mut sites = 0usize;
    while let Some(Reverse((key, index))) = heap.pop() {
        if sites.is_multiple_of(BUFFER_VALUES) {
            check_cancelled(cancelled)?;
        }
        let (band, cursor) = &mut cursors[index];
        let value = from_ordered_bits(key);
        if !value.is_finite() {
            return Err(AggregationError::SpillCoverage);
        }
        observe(*band, value);
        if let Some(raw) = cursor.next(spill)? {
            let next = ordered_bits(raw);
            if next < key {
                return Err(AggregationError::SpillCoverage);
            }
            heap.push(Reverse((next, index)));
        }
        sites += 1;
    }
    Ok(())
}

struct Accumulator {
    expected: usize,
    count: usize,
    sum: f64,
    correction: f64,
    min: f64,
    max: f64,
    p95: f64,
    p99: f64,
    p95_rank: usize,
    p99_rank: usize,
}

impl Accumulator {
    fn new(expected: usize) -> Self {
        let last = expected.saturating_sub(1) as u64;
        Self {
            expected,
            count: 0,
            sum: 0.0,
            correction: 0.0,
            min: 0.0,
            max: 0.0,
            p95: 0.0,
            p99: 0.0,
            p95_rank: (last * 95 / 100) as usize,
            p99_rank: (last * 99 / 100) as usize,
        }
    }

    fn observe(&mut self, value: f64) {
        if self.count == 0 {
            self.min = value;
        }
        self.max = value;
        if self.count == self.p95_rank {
            self.p95 = value;
        }
        if self.count == self.p99_rank {
            self.p99 = value;
        }
        self.count += 1;
        let sum = self.sum + value;
        // Neumaier correction also retains small terms between paired losses
        // and gains.
        self.correction += if self.sum.abs() >= value.abs() {
            (self.sum - sum) + value
        } else {
            (value - sum) + self.sum
        };
        self.sum = sum;
    }

    fn finish(self) -> Result<Option<Distribution>, AggregationError> {
        if self.count != self.expected {
            return Err(AggregationError::SpillCoverage);
        }
        Ok((self.count != 0).then(|| Distribution {
            count: self.count,
            mean: (self.sum + self.correction) / self.count as f64,
            min: self.min,
            p95: self.p95,
            p99: self.p99,
            max: self.max,
        }))
    }
}

fn statistics(
    spill: &mut File,
    frames: &[[ModelRuns; MODELS]],
    cancelled: &impl Fn() -> bool,
) -> Result<[ModelStatistics; MODELS], AggregationError> {
    let mut summarize = |model: usize| -> Result<ModelStatistics, AggregationError> {
        let rows = || frames.iter().map(|frame| &frame[model]);
        let served = banded(spill, rows().map(|r| r.served), cancelled)?;
        let candidate = banded(spill, rows().map(|r| r.candidate), cancelled)?;
        let paired = paired(spill, rows().map(|r| r.paired), cancelled)?;
        Ok(ModelStatistics {
            served_fplus: served,
            candidate,
            paired,
        })
    };
    Ok([summarize(0)?, summarize(1)?, summarize(2)?, summarize(3)?])
}

fn banded(
    spill: &mut File,
    frames: impl Iterator<Item = [Run; BANDS]>,
    cancelled: &impl Fn() -> bool,
) -> Result<BandedDistribution, AggregationError> {
    let runs: Vec<_> = frames
        .flat_map(|frame| frame.into_iter().enumerate())
        .collect();
    let mut counts = [0; BANDS];
    for (band, run) in &runs {
        counts[*band] += run.count;
    }
    let mut bands = counts.map(Accumulator::new);
    let mut full = Accumulator::new(counts.into_iter().sum());
    merge(spill, &runs, cancelled, |band, value| {
        bands[band].observe(value);
        full.observe(value);
    })?;
    let mut summaries = [None; BANDS];
    for (summary, band) in summaries.iter_mut().zip(bands) {
        *summary = band.finish()?;
    }
    Ok(BandedDistribution {
        full: full.finish()?.ok_or(AggregationError::SpillCoverage)?,
        bands: summaries,
    })
}

fn paired(
    spill: &mut File,
    frames: impl Iterator<Item = Run>,
    cancelled: &impl Fn() -> bool,
) -> Result<PairedDistribution, AggregationError> {
    let runs: Vec<_> = frames.map(|run| (0, run)).collect();
    let mut closer = 0;
    let mut worse = 0;
    let mut distribution = Accumulator::new(runs.iter().map(|(_, run)| run.count).sum());
    merge(spill, &runs, cancelled, |_, value| {
        distribution.observe(value);
        closer += (value > 0.0) as usize;
        worse += (value < 0.0) as usize;
    })?;
    let advantage = distribution
        .finish()?
        .ok_or(AggregationError::SpillCoverage)?;
    Ok(PairedDistribution {
        advantage,
        candidate_closer_count: closer,
        candidate_worse_count: worse,
        ties: advantage.count - closer - worse,
    })
}

pub(in crate::transcode) fn comparison_disposition(
    checks: impl IntoIterator<Item = Check>,
) -> Disposition {
    let mut disposition = Disposition::Satisfied;
    for check in checks {
        match check {
            Check::Failed => return Disposition::Refused,
            Check::MissingBand => disposition = Disposition::Inconclusive,
            Check::Passed => {}
        }
    }
    disposition
}

pub(in crate::transcode) fn mean_gain(stats: &ModelStatistics, margin: FrozenMargin) -> MeanGain {
    let delta_e_itp = stats.served_fplus.full.mean - stats.candidate.full.mean;
    MeanGain {
        delta_e_itp,
        check: Check::from_test(delta_e_itp >= margin.0),
    }
}

pub(in crate::transcode) fn window_checks(
    stats: &ModelStatistics,
    frames: &[[ModelStatistics; MODELS]],
    model: usize,
    margin: FrozenMargin,
) -> WindowChecks {
    let mut frames_with_shadow = 0;
    let mut worst_served: Option<f64> = None;
    let mut worst_candidate: Option<f64> = None;
    for frame in frames {
        let frame = &frame[model];
        if let (Some(served), Some(candidate)) =
            (frame.served_fplus.bands[0], frame.candidate.bands[0])
        {
            frames_with_shadow += 1;
            worst_served = Some(worst_served.map_or(served.p99, |old| old.max(served.p99)));
            worst_candidate =
                Some(worst_candidate.map_or(candidate.p99, |old| old.max(candidate.p99)));
        }
    }
    window_checks_from_shadow(
        stats,
        margin,
        frames_with_shadow == frames.len(),
        (worst_served, worst_candidate),
    )
}

pub(in crate::transcode) fn window_checks_from_shadow(
    stats: &ModelStatistics,
    margin: FrozenMargin,
    every_frame_has_shadow: bool,
    (worst_served, worst_candidate): (Option<f64>, Option<f64>),
) -> WindowChecks {
    let compare_band = |band: usize, metric: fn(Distribution) -> f64| match (
        stats.candidate.bands[band],
        stats.served_fplus.bands[band],
    ) {
        (Some(candidate), Some(served)) => Check::from_test(metric(candidate) <= metric(served)),
        _ => Check::MissingBand,
    };
    WindowChecks {
        mean_gain: mean_gain(stats, margin),
        shadow_mean: compare_band(0, |s| s.mean),
        shadow_p99: compare_band(0, |s| s.p99),
        dark_p99: compare_band(1, |s| s.p99),
        largest_frame_shadow_p99: match (worst_candidate, worst_served) {
            (Some(candidate), Some(served)) if every_frame_has_shadow => {
                Check::from_test(candidate <= served)
            }
            _ => Check::MissingBand,
        },
        largest_served_fplus_frame_shadow_p99: worst_served,
        largest_candidate_frame_shadow_p99: worst_candidate,
    }
}

#[cfg(test)]
mod tests;
