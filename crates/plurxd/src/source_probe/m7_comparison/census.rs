//! Read the independent coded AU census before a sample writer can run.

use super::{
    capture_bytes, digest, execution, off_runtime, run_renderer, Error, TitleContext, WindowShape,
    CAPTURE_OUTPUT, FRAME_PREFIX_BYTES, MAX_NAL_BYTES,
};
use crate::fragment_index_cluster::SourceFence;
use plurx_core::transcode::{
    dv_m7_scoring::capture::{CaptureError, CaptureLayout, CaptureRole, FrameEvidence},
    Rational,
};
use std::io::Read;
use tempfile::TempDir;

const MAX_PREFIX_FRAMES: u64 = 4096;
const HEADER_BYTES: usize = 88;
const TRAILER_BYTES: usize = 24;

#[derive(Clone, Copy)]
pub(in crate::source_probe) struct CensusRequest {
    pub layout: CaptureLayout,
    pub first: u64,
    pub last: u64,
    pub first_pts: i64,
    pub end_pts: i64,
}

impl CensusRequest {
    fn frames(self) -> Result<usize, Error> {
        let frames = self
            .last
            .checked_sub(self.first)
            .ok_or(CaptureError::Census)?
            + 1;
        self.layout.sites_per_frame()?;
        if frames > 48
            || self.layout.video_index > 255
            || self.first_pts == i64::MIN
            || self.end_pts == i64::MAX
            || self.first_pts >= self.end_pts
        {
            return Err(CaptureError::Census.into());
        }
        match self.layout.role {
            CaptureRole::SourceFel if self.first > MAX_PREFIX_FRAMES => Err(Error::Unsupported(
                "absolute source census outside the bounded prefix",
            )),
            CaptureRole::AuthoredCandidate if self.first != 0 || self.first_pts != 0 => {
                Err(CaptureError::Census.into())
            }
            _ => Ok(frames as usize),
        }
    }
}

/// Like sample capture, this runs inside the current source-admitted scope.
/// The independent pass observes original packet bytes before layer split
/// and establishes absolute presentation ordinal from the decoded prefix.
pub(in crate::source_probe) async fn observe(
    context: &TitleContext<'_>,
    source: &SourceFence,
    request: CensusRequest,
    job_dir: &TempDir,
) -> Result<Vec<FrameEvidence>, Error> {
    let frames = request.frames()?;
    let limit = capture_bytes(
        WindowShape {
            frames,
            sites_per_frame: 0,
        },
        0,
    );
    let mode = match request.layout.role {
        CaptureRole::SourceFel => "--m7-census-fel-v1",
        CaptureRole::AuthoredCandidate => "--m7-census-candidate-v1",
    };
    let dir = tempfile::tempdir_in(job_dir.path()).map_err(execution)?;
    run_renderer(context, source, request, mode, &dir, limit).await?;
    off_runtime(move || {
        let file =
            plurx_core::fs_secure::open_read_nofollow_blocking(&dir.path().join(CAPTURE_OUTPUT))
                .map_err(execution)?;
        let mut bytes = Vec::with_capacity(limit);
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(execution)?;
        if bytes.len() > limit {
            return Err(CaptureError::Chunk.into());
        }
        parse(&bytes, request, frames)
    })
    .await
}

fn parse(
    mut bytes: &[u8],
    request: CensusRequest,
    frames: usize,
) -> Result<Vec<FrameEvidence>, Error> {
    let header = take(&mut bytes, HEADER_BYTES)?;
    let magic = match request.layout.role {
        CaptureRole::SourceFel => b"PLXM7S01",
        CaptureRole::AuthoredCandidate => b"PLXM7A01",
    };
    let clock =
        Rational::new(u32_at(header, 12), u32_at(header, 16)).ok_or(CaptureError::Header)?;
    let [width, height] = request.layout.raster;
    let [x0, y0, x1, y1] = request.layout.roi;
    if &header[..8] != magic
        || u32_at(header, 8) != request.layout.video_index
        || clock != request.layout.time_base
        || (20..72)
            .step_by(4)
            .map(|offset| u32_at(header, offset))
            .ne([
                u32::from(width),
                u32::from(height),
                u32::from(x0),
                u32::from(y0),
                u32::from(x1),
                u32::from(y1),
                8,
                3,
                0,
                0,
                frames as u32,
                request.first as u32,
                request.last as u32,
            ])
        || i64_at(header, 72) != request.first_pts
        || i64_at(header, 80) != request.end_pts
    {
        return Err(CaptureError::Header.into());
    }

    let mut rows = Vec::<FrameEvidence>::with_capacity(frames);
    let mut end = None;
    for index in 0..frames {
        let prefix = take(&mut bytes, FRAME_PREFIX_BYTES)?;
        let rpu_len = u32_at(prefix, 36) as usize;
        if &prefix[..4] != b"FRM1"
            || u32_at(prefix, 4) as usize != FRAME_PREFIX_BYTES - 8 + rpu_len
            || u32_at(prefix, 32) != 0
            || !(3..=MAX_NAL_BYTES).contains(&rpu_len)
        {
            return Err(CaptureError::Chunk.into());
        }
        let frame = FrameEvidence {
            ordinal: u64_at(prefix, 8),
            pts: i64_at(prefix, 16),
            duration: i64_at(prefix, 24),
            time_base: clock,
            access_unit_sha256: sha_at(prefix, 40)?,
            rpu_sha256: sha_at(prefix, 104)?,
            bl_vcl_sha256: sha_at(prefix, 168)?,
        };
        let rpu = take(&mut bytes, rpu_len)?;
        if !rpu.starts_with(&[0x7c, 1]) || digest(rpu) != frame.rpu_sha256 {
            return Err(CaptureError::Digest.into());
        }
        end = frame.pts.checked_add(frame.duration);
        if frame.ordinal != request.first + index as u64
            || frame.duration <= 0
            || end.is_none_or(|tick| tick > request.end_pts)
            || rows
                .last()
                .map_or(frame.pts != request.first_pts, |previous| {
                    frame.pts <= previous.pts
                })
        {
            return Err(CaptureError::Census.into());
        }
        rows.push(frame);
    }
    let trailer = take(&mut bytes, TRAILER_BYTES)?;
    if &trailer[..4] != b"END1"
        || u32_at(trailer, 4) != 16
        || u64_at(trailer, 8) != frames as u64
        || u64_at(trailer, 16) != 0
        || end != Some(request.end_pts)
        || !bytes.is_empty()
    {
        return Err(CaptureError::Completion.into());
    }
    Ok(rows)
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], CaptureError> {
    let (prefix, rest) = bytes
        .split_at_checked(count)
        .ok_or(CaptureError::Truncated)?;
    *bytes = rest;
    Ok(prefix)
}

fn sha_at(bytes: &[u8], offset: usize) -> Result<[u8; 32], CaptureError> {
    let value = &bytes[offset..offset + 64];
    if !value
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(CaptureError::Digest);
    }
    let mut digest = [0; 32];
    hex::decode_to_slice(value, &mut digest).map_err(|_| CaptureError::Digest)?;
    Ok(digest)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("checked census record"),
    )
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("checked census record"),
    )
}

fn i64_at(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("checked census record"),
    )
}
