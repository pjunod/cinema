//! Read managed M7 captures against independent packet censuses.
//!
//! Source and candidate captures retain separate clocks. The authoring binding
//! supplies the source-to-encoder ordinal map and both exact clock assignments.
//! A frame's score can enter a provisional accumulator as soon as both frames
//! bind. Publication requires successful completion of both capture readers.

use std::{
    cmp::Ordering,
    io::{self, Read},
    process::ExitStatus,
};

use sha2::{Digest, Sha256};

use super::{
    aggregation::MAX_FRAMES_PER_WINDOW, FrameScorer, LatticeSample, ModelSampleScore, ScoreError,
    RPU_NAL_HEADER,
};
use crate::transcode::{dv_refit::ServableCurves, Rational};

const HEADER_BYTES: usize = 88;
const FRAME_FIELDS_BYTES: usize = 32;
const DIGEST_BYTES: usize = 64;
const FRAME_PREFIX_BYTES: usize = FRAME_FIELDS_BYTES + 3 * DIGEST_BYTES;
const MAX_RPU_BYTES: usize = 4097;
const STRIDE: usize = 8;
const PHASES: u32 = 3;
const MAX_SITES_PER_FRAME: usize = 503_040;
const MAX_SOURCE_ORDINAL: u64 = 999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRole {
    SourceFel,
    AuthoredCandidate,
}

impl CaptureRole {
    fn wire(self) -> (&'static [u8; 8], usize) {
        match self {
            Self::SourceFel => (b"PLXM7F01", 16),
            Self::AuthoredCandidate => (b"PLXM7C01", 10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureLayout {
    pub role: CaptureRole,
    pub video_index: u32,
    pub time_base: Rational,
    pub raster: [u16; 2],
    /// The half-open active rectangle [x0, y0, x1, y1].
    pub roi: [u16; 4],
}

impl CaptureLayout {
    pub fn sites_per_frame(self) -> Result<usize, CaptureError> {
        let [width, height] = self.raster;
        let [x0, y0, x1, y1] = self.roi;
        if !(64..=3840).contains(&width)
            || !(64..=2160).contains(&height)
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
            || x1 > width
            || y1 > height
            || x1.saturating_sub(x0) < 2
            || y1.saturating_sub(y0) < 2
        {
            return Err(CaptureError::Layout);
        }
        let samples = axis_count(x1 - x0) * axis_count(y1 - y0);
        if samples > MAX_SITES_PER_FRAME {
            return Err(CaptureError::Layout);
        }
        Ok(samples)
    }
}

/// A row from an independently held source or authored packet census.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameEvidence {
    pub ordinal: u64,
    pub pts: i64,
    pub duration: i64,
    pub time_base: Rational,
    pub access_unit_sha256: [u8; 32],
    pub rpu_sha256: [u8; 32],
    /// SHA-256 of the ordered VCL NALs with four-byte size prefixes.
    pub bl_vcl_sha256: [u8; 32],
}

impl FrameEvidence {
    fn same_frame(self, other: Self) -> bool {
        self.ordinal == other.ordinal
            && cmp_ticks(self.pts, self.time_base, other.pts, other.time_base).is_eq()
            && cmp_ticks(
                self.duration,
                self.time_base,
                other.duration,
                other.time_base,
            )
            .is_eq()
            && self.access_unit_sha256 == other.access_unit_sha256
            && self.rpu_sha256 == other.rpu_sha256
            && self.bl_vcl_sha256 == other.bl_vcl_sha256
    }

    fn end(self) -> Option<i64> {
        self.pts.checked_add(self.duration)
    }
}

/// Renderer invocation and complete frame census verified independently of
/// the capture stream.
#[derive(Debug, Clone, Copy)]
pub struct CapturePlan<'a> {
    pub layout: CaptureLayout,
    pub frames: &'a [FrameEvidence],
}

impl CapturePlan<'_> {
    fn validate(self) -> Result<usize, CaptureError> {
        let sites = self.layout.sites_per_frame()?;
        let Some((first, last)) = self.frames.first().zip(self.frames.last()) else {
            return Err(CaptureError::Census);
        };
        if self.layout.video_index > 255
            || self.layout.time_base.numerator() > i32::MAX as u32
            || self.layout.time_base.denominator() > i32::MAX as u32
            || self.frames.len() > MAX_FRAMES_PER_WINDOW
            || first.ordinal > last.ordinal
            || last.ordinal > MAX_SOURCE_ORDINAL
            || self.layout.role == CaptureRole::AuthoredCandidate
                && (first.ordinal != 0 || first.pts != 0)
        {
            return Err(CaptureError::Census);
        }
        let end = last.end().ok_or(CaptureError::Census)?;
        for (index, row) in self.frames.iter().enumerate() {
            if row.ordinal != first.ordinal + index as u64
                || row.pts == i64::MIN
                || row.duration <= 0
                || !row
                    .end()
                    .is_some_and(|tick| cmp_ticks(tick, row.time_base, end, last.time_base).is_le())
            {
                return Err(CaptureError::Census);
            }
        }
        if self.frames.windows(2).any(|pair| {
            cmp_ticks(
                pair[0].pts,
                pair[0].time_base,
                pair[1].pts,
                pair[1].time_base,
            )
            .is_ge()
        }) {
            return Err(CaptureError::Census);
        }
        Ok(sites)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("M7 capture helper exited unsuccessfully: {0}")]
    Child(ExitStatus),
    #[error("unsupported M7 capture raster or lattice")]
    Layout,
    #[error("invalid or incomplete independent M7 frame census")]
    Census,
    #[error("M7 capture header differs from the expected invocation")]
    Header,
    #[error("unexpected M7 capture chunk or body length")]
    Chunk,
    #[error("M7 capture clock or packet evidence differs from its independent census")]
    Frame,
    #[error("invalid M7 capture SHA-256 spelling or fresh RPU bytes")]
    Digest,
    #[error("M7 capture sample coordinates or ten-bit codes differ from the lattice")]
    Sample,
    #[error("M7 capture is truncated")]
    Truncated,
    #[error("M7 capture completion count, EOF or frame coverage differs")]
    Completion,
    #[error("M7 capture does not match the source-to-candidate authoring binding")]
    Authoring,
    #[error("M7 whole or seek publication does not match the source frame or fitted RPU evidence")]
    Readback,
    #[error("M7 frame scoring was cancelled")]
    Cancelled,
    #[error("M7 capture I/O: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Score(#[from] ScoreError),
}

/// One frame independently checked by a capture reader.
pub struct CapturedFrame {
    layout: CaptureLayout,
    evidence: FrameEvidence,
    first_ordinal: u64,
    frame_count: usize,
    rpu_nal: Vec<u8>,
    records: Vec<u8>,
}

impl CapturedFrame {
    pub fn evidence(&self) -> FrameEvidence {
        self.evidence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureCompletion {
    pub layout: CaptureLayout,
    pub frames: usize,
    pub samples: u64,
}

/// At most one frame of samples is allocated per read.
pub struct CaptureReader<'a, R: Read> {
    input: R,
    plan: CapturePlan<'a>,
    sites: usize,
    record_bytes: usize,
    first_pts: i64,
    end_pts: i64,
    read_frames: usize,
    failed: bool,
}

impl<'a, R: Read> CaptureReader<'a, R> {
    /// The caller reaps the renderer and hands over its successful exit
    /// status and the capture from that exact invocation.
    pub fn new(
        mut input: R,
        plan: CapturePlan<'a>,
        status: ExitStatus,
    ) -> Result<Self, CaptureError> {
        if !status.success() {
            return Err(CaptureError::Child(status));
        }
        let sites = plan.validate()?;
        let first = plan.frames[0];
        let last = plan.frames[plan.frames.len() - 1];
        let layout = plan.layout;
        let (magic, record_bytes) = layout.role.wire();
        let header = read_fixed::<HEADER_BYTES>(&mut input)?;
        let mut fields = [0_u32; 16];
        for (i, field) in fields.iter_mut().enumerate() {
            *field = u32_at(&header, 8 + i * 4);
        }
        let [video, num, den, width, height, x0, y0, x1, y1, stride, phases, count, bytes, frames, first_ordinal, last_ordinal] =
            fields;
        let first_pts = i64_at(&header, 72);
        let end_pts = i64_at(&header, 80);
        if &header[..8] != magic
            || video != layout.video_index
            || num > i32::MAX as u32
            || den > i32::MAX as u32
            || Rational::new(num, den) != Some(layout.time_base)
            || [width, height] != layout.raster.map(u32::from)
            || [x0, y0, x1, y1] != layout.roi.map(u32::from)
            || stride != STRIDE as u32
            || phases != PHASES
            || count as usize != sites
            || bytes as usize != record_bytes
            || frames as usize != plan.frames.len()
            || u64::from(first_ordinal) != first.ordinal
            || u64::from(last_ordinal) != last.ordinal
            || first_pts == i64::MIN
            || first_pts >= end_pts
            || !cmp_ticks(first_pts, layout.time_base, first.pts, first.time_base).is_eq()
            || !last.end().is_some_and(|tick| {
                cmp_ticks(end_pts, layout.time_base, tick, last.time_base).is_eq()
            })
        {
            return Err(CaptureError::Header);
        }
        Ok(Self {
            input,
            plan,
            sites,
            record_bytes,
            first_pts,
            end_pts,
            read_frames: 0,
            failed: false,
        })
    }

    pub fn next_frame(&mut self) -> Result<Option<CapturedFrame>, CaptureError> {
        if self.failed {
            return Err(CaptureError::Completion);
        }
        let Some(expected) = self.plan.frames.get(self.read_frames).copied() else {
            return Ok(None);
        };
        let result = self.read_frame(expected);
        self.failed = result.is_err();
        result.map(Some)
    }

    fn read_frame(&mut self, expected: FrameEvidence) -> Result<CapturedFrame, CaptureError> {
        let chunk = read_fixed::<8>(&mut self.input)?;
        let frame_bytes = self.sites * self.record_bytes;
        let length = u32_at(&chunk, 4) as usize;
        if &chunk[..4] != b"FRM1"
            || !(FRAME_PREFIX_BYTES + 3 + frame_bytes
                ..=FRAME_PREFIX_BYTES + MAX_RPU_BYTES + frame_bytes)
                .contains(&length)
        {
            return Err(CaptureError::Chunk);
        }
        let prefix = read_fixed::<FRAME_PREFIX_BYTES>(&mut self.input)?;
        let count = u32_at(&prefix, 24) as usize;
        let rpu_bytes = u32_at(&prefix, 28) as usize;
        if count != self.sites
            || !(3..=MAX_RPU_BYTES).contains(&rpu_bytes)
            || length != FRAME_PREFIX_BYTES + rpu_bytes + frame_bytes
        {
            return Err(CaptureError::Chunk);
        }
        let layout = self.plan.layout;
        let evidence = FrameEvidence {
            ordinal: u64_at(&prefix, 0),
            pts: i64_at(&prefix, 8),
            duration: i64_at(&prefix, 16),
            time_base: layout.time_base,
            access_unit_sha256: digest_at(&prefix, FRAME_FIELDS_BYTES)?,
            rpu_sha256: digest_at(&prefix, FRAME_FIELDS_BYTES + DIGEST_BYTES)?,
            bl_vcl_sha256: digest_at(&prefix, FRAME_FIELDS_BYTES + 2 * DIGEST_BYTES)?,
        };
        if !evidence.same_frame(expected)
            || evidence.pts < self.first_pts
            || evidence.duration <= 0
            || evidence.end().is_none_or(|end| end > self.end_pts)
        {
            return Err(CaptureError::Frame);
        }
        let mut rpu_nal = vec![0; rpu_bytes];
        read_exact(&mut self.input, &mut rpu_nal)?;
        if !valid_rpu(&rpu_nal, evidence.rpu_sha256) {
            return Err(CaptureError::Digest);
        }
        let mut records = vec![0; frame_bytes];
        read_exact(&mut self.input, &mut records)?;
        let [x0, y0, x1, _] = layout.roi;
        let columns = axis_count(x1 - x0);
        for (site, record) in records.chunks_exact(self.record_bytes).enumerate() {
            let x = lattice_coordinate(x0, site % columns);
            let y = lattice_coordinate(y0, site / columns);
            if [u16_at(record, 0), u16_at(record, 2)] != [x, y]
                || (4..self.record_bytes)
                    .step_by(2)
                    .any(|offset| u16_at(record, offset) > 1023)
            {
                return Err(CaptureError::Sample);
            }
        }
        self.read_frames += 1;
        Ok(CapturedFrame {
            layout,
            evidence,
            first_ordinal: self.plan.frames[0].ordinal,
            frame_count: self.plan.frames.len(),
            rpu_nal,
            records,
        })
    }

    /// Require the exact counted END1 and EOF after the final frame.
    pub fn finish(mut self) -> Result<CaptureCompletion, CaptureError> {
        if self.failed || self.read_frames != self.plan.frames.len() {
            return Err(CaptureError::Completion);
        }
        let trailer = read_fixed::<24>(&mut self.input)?;
        let sample_count = (self.read_frames * self.sites) as u64;
        if &trailer[..4] != b"END1"
            || u32_at(&trailer, 4) != 16
            || u64_at(&trailer, 8) != self.read_frames as u64
            || u64_at(&trailer, 16) != sample_count
        {
            return Err(CaptureError::Completion);
        }
        let mut extra = [0];
        match self.input.read_exact(&mut extra) {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(CaptureCompletion {
                layout: self.plan.layout,
                frames: self.read_frames,
                samples: sample_count,
            }),
            Ok(()) => Err(CaptureError::Completion),
            Err(error) => Err(error.into()),
        }
    }
}

/// One row from the independently verified authoring receipt and the
/// exact source crop that produced the authored candidate clip.
pub struct AuthoringFrameBinding {
    pub source: FrameEvidence,
    pub candidate: FrameEvidence,
    pub crop_first_source_ordinal: u64,
    pub crop_frames: u32,
}

/// A whole or seek production readback, associated to the original source
/// clock by the publication verifier.
pub struct FplusFrameEvidence<'a> {
    pub source: FrameEvidence,
    pub bl_vcl_sha256: [u8; 32],
    pub rpu_sha256: [u8; 32],
    pub rpu_nal: &'a [u8],
}

impl FplusFrameEvidence<'_> {
    fn matches_source(&self, source: FrameEvidence) -> bool {
        self.source.same_frame(source)
            && self.bl_vcl_sha256 == source.bl_vcl_sha256
            && valid_rpu(self.rpu_nal, self.rpu_sha256)
    }
}

/// Score one bound source/candidate pair in lattice order using actual whole
/// and seek production readback. The caller still completes both captures
/// before consuming the accumulator as a comparison report.
pub fn score_paired_frame(
    source: &CapturedFrame,
    candidate: &CapturedFrame,
    authoring: &AuthoringFrameBinding,
    whole: &FplusFrameEvidence<'_>,
    seek: &FplusFrameEvidence<'_>,
    pinned: &ServableCurves,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<[ModelSampleScore; 4]>, CaptureError> {
    let frames = authoring.crop_frames as usize;
    if source.layout.role != CaptureRole::SourceFel
        || candidate.layout.role != CaptureRole::AuthoredCandidate
        || source.layout.raster != candidate.layout.raster
        || source.layout.roi != candidate.layout.roi
        || source.first_ordinal != authoring.crop_first_source_ordinal
        || candidate.first_ordinal != 0
        || source.frame_count != frames
        || candidate.frame_count != frames
        || authoring
            .crop_first_source_ordinal
            .checked_add(candidate.evidence.ordinal)
            != Some(source.evidence.ordinal)
        || !authoring.source.same_frame(source.evidence)
        || !authoring.candidate.same_frame(candidate.evidence)
    {
        return Err(CaptureError::Authoring);
    }
    if !whole.matches_source(source.evidence) || !seek.matches_source(source.evidence) {
        return Err(CaptureError::Readback);
    }
    let scorer = FrameScorer::from_rpus(
        &source.rpu_nal,
        whole.rpu_nal,
        seek.rpu_nal,
        pinned,
        &candidate.rpu_nal,
    )?;
    let mut scores = Vec::with_capacity(source.layout.sites_per_frame()?);
    let samples = source
        .records
        .chunks_exact(CaptureRole::SourceFel.wire().1)
        .zip(
            candidate
                .records
                .chunks_exact(CaptureRole::AuthoredCandidate.wire().1),
        );
    for (i, (source_sample, candidate_sample)) in samples.enumerate() {
        if i.is_multiple_of(1024) && cancelled() {
            return Err(CaptureError::Cancelled);
        }
        scores.push(scorer.score_sample(LatticeSample {
            source_bl: codes_at(source_sample, 4),
            source_el: codes_at(source_sample, 10),
            candidate: codes_at(candidate_sample, 4),
        })?);
    }
    if cancelled() {
        return Err(CaptureError::Cancelled);
    }
    Ok(scores)
}

fn axis_count(extent: u16) -> usize {
    let extent = usize::from(extent);
    extent.div_ceil(STRIDE) + (extent - 1).div_ceil(STRIDE)
}

fn lattice_coordinate(origin: u16, index: usize) -> u16 {
    origin + ((index / 2) * STRIDE + index % 2) as u16
}

fn cmp_ticks(left: i64, left_base: Rational, right: i64, right_base: Rational) -> Ordering {
    // Both products fit i128 for all i64 ticks and u32 rational components.
    (i128::from(left) * i128::from(left_base.numerator()) * i128::from(right_base.denominator()))
        .cmp(
            &(i128::from(right)
                * i128::from(right_base.numerator())
                * i128::from(left_base.denominator())),
        )
}

fn valid_rpu(nal: &[u8], digest: [u8; 32]) -> bool {
    (3..=MAX_RPU_BYTES).contains(&nal.len())
        && nal.starts_with(&RPU_NAL_HEADER)
        && <[u8; 32]>::from(Sha256::digest(nal)) == digest
}

fn digest_at(bytes: &[u8], offset: usize) -> Result<[u8; 32], CaptureError> {
    let text = &bytes[offset..offset + DIGEST_BYTES];
    if !text
        .iter()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(CaptureError::Digest);
    }
    let mut value = [0; 32];
    hex::decode_to_slice(text, &mut value).map_err(|_| CaptureError::Digest)?;
    Ok(value)
}

fn read_exact(input: &mut impl Read, bytes: &mut [u8]) -> Result<(), CaptureError> {
    input.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            CaptureError::Truncated
        } else {
            error.into()
        }
    })
}

fn read_fixed<const N: usize>(input: &mut impl Read) -> Result<[u8; N], CaptureError> {
    let mut bytes = [0; N];
    read_exact(input, &mut bytes)?;
    Ok(bytes)
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("u16 field"))
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 field"))
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 field"))
}

fn i64_at(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("i64 field"))
}

fn codes_at(bytes: &[u8], offset: usize) -> [u16; 3] {
    std::array::from_fn(|i| u16_at(bytes, offset + 2 * i))
}

#[cfg(test)]
mod tests;
