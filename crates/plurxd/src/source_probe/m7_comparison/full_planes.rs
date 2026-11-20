//! Stage one verified full-raster Model B input crop and its per-frame ROI maps.

use super::census::{i64_at, sha_at, u32_at, u64_at};
use super::{
    admit_renderer, check_cancel, digest, execution, off_runtime, request, run_renderer,
    verify_authority, verify_source_census, CensusRequest, Error, HeldTitle, TitleContext,
    CAPTURE_HEADER_AND_TRAILER, CAPTURE_OUTPUT, FRAME_PREFIX_BYTES, MAX_NAL_BYTES,
};
use crate::{fragment_index_cluster::SourceFence, source_probe::m7_candidate};
use plurx_core::transcode::{
    dv_m7_scoring::capture::{
        CaptureError, CaptureLayout, CapturePlan, CaptureRole, FrameEvidence,
    },
    Rational,
};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const FRAMES: usize = 48;
const DIGEST_BYTES: usize = 64;
const COMPOSED: &str = "composed.yuv";

pub(in crate::source_probe) struct PreparedPlaneFrame {
    pub source: FrameEvidence,
    pub source_rpu: Vec<u8>,
    pub original_bl_sha256: [u8; 32],
    pub composed_sha256: [u8; 32],
    pub benefit_map: m7_candidate::BenefitMap,
}

/// Pins the staged dense file until all encoder children have joined.
pub(in crate::source_probe) struct PreparedPlanes {
    pub composed: SourceFence,
    pub source_sha256: [u8; 32],
    pub probe_sha256: [u8; 32],
    pub source_object_version: String,
    pub layout: CaptureLayout,
    pub frames: Vec<PreparedPlaneFrame>,
    path: PathBuf,
    _dir: TempDir,
}

impl PreparedPlanes {
    pub(in crate::source_probe) fn composed_path(&self) -> &Path {
        &self.path
    }
}

pub(in crate::source_probe) async fn prepare_planes(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    plan: CapturePlan<'_>,
    scratch: &TempDir,
    staged_bytes: u64,
) -> Result<PreparedPlanes, Error> {
    let selection = request(plan)?;
    let layout = plan.layout;
    layout.sites_per_frame()?;
    if layout.role != CaptureRole::SourceFel || plan.frames.len() != FRAMES {
        return Err(Error::Unsupported(
            "full-plane FEL input requires exactly 48 frames",
        ));
    }
    let [width, height] = layout.raster.map(usize::from);
    let plane_bytes = 3 * width * height;
    let capture_bytes = CAPTURE_HEADER_AND_TRAILER
        + FRAMES * (FRAME_PREFIX_BYTES + 2 * DIGEST_BYTES + MAX_NAL_BYTES + 2 * plane_bytes);
    let staged_composed_bytes = FRAMES * plane_bytes;
    if (capture_bytes + staged_composed_bytes) as u64 > staged_bytes {
        return Err(Error::Busy(
            "admitted full-plane scratch is too small".into(),
        ));
    }
    let cancel = plurx_core::process::bounded::cancellation().ok_or(Error::Cancelled)?;
    verify_authority(context, title, &cancel).await?;
    let _extension = admit_renderer(context).await?;
    verify_source_census(context, title, plan, scratch).await?;
    let capture = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    let staged = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    run_renderer(
        context,
        &title.source,
        selection,
        "--m7-export-full-planes-v1",
        &capture,
        capture_bytes,
    )
    .await?;
    let frames = plan.frames.to_vec();
    let title_binding = (
        title.content_sha256,
        title.probe_sha256,
        title.source.object_version().to_owned(),
    );
    let prepared = off_runtime({
        let cancel = cancel.clone();
        move || {
            stage(
                staged,
                capture,
                selection,
                frames,
                plane_bytes,
                title_binding,
                &cancel,
            )
        }
    })
    .await?;
    verify_authority(context, title, &cancel).await?;
    Ok(prepared)
}

fn stage(
    staged: TempDir,
    capture: TempDir,
    selection: CensusRequest,
    frames: Vec<FrameEvidence>,
    plane_bytes: usize,
    title_binding: ([u8; 32], [u8; 32], String),
    cancel: &CancellationToken,
) -> Result<PreparedPlanes, Error> {
    let path = staged.path().join(COMPOSED);
    let mut writer = BufWriter::new(File::create_new(&path).map_err(execution)?);
    let input =
        plurx_core::fs_secure::open_read_nofollow_blocking(&capture.path().join(CAPTURE_OUTPUT))
            .map_err(execution)?;
    let mut reader = BufReader::new(input);
    let mut header = [0; 88];
    read_exact(&mut reader, &mut header)?;
    verify_header(&header, selection)?;
    let mut original = vec![0; plane_bytes];
    let mut composed = vec![0; plane_bytes];
    let mut prepared = Vec::with_capacity(FRAMES);
    for source in frames {
        check_cancel(cancel)?;
        let mut prefix = [0; FRAME_PREFIX_BYTES + 2 * DIGEST_BYTES];
        read_exact(&mut reader, &mut prefix)?;
        let nalu_bytes = u32_at(&prefix, 36) as usize;
        if &prefix[..4] != b"FRM1"
            || !(3..=MAX_NAL_BYTES).contains(&nalu_bytes)
            || u32_at(&prefix, 4) as usize != prefix.len() - 8 + nalu_bytes + 2 * plane_bytes
            || u32_at(&prefix, 32) != 0
            || u64_at(&prefix, 8) != source.ordinal
            || i64_at(&prefix, 16) != source.pts
            || i64_at(&prefix, 24) != source.duration
            || sha_at(&prefix, 40)? != source.access_unit_sha256
            || sha_at(&prefix, 104)? != source.rpu_sha256
            || sha_at(&prefix, 168)? != source.bl_vcl_sha256
        {
            return Err(CaptureError::Frame.into());
        }

        let original_bl_sha256 = sha_at(&prefix, 232)?;
        let composed_sha256 = sha_at(&prefix, 296)?;
        let mut source_rpu = vec![0; nalu_bytes];
        read_exact(&mut reader, &mut source_rpu)?;
        read_exact(&mut reader, &mut original)?;
        read_exact(&mut reader, &mut composed)?;
        if !source_rpu.starts_with(&[0x7c, 1]) || digest(&source_rpu) != source.rpu_sha256 {
            return Err(CaptureError::Digest.into());
        }
        // The ROI operator validates the plane digests and every ten-bit code
        // before the dense composed bytes become the encoder's held input.
        let benefit_map = m7_candidate::build_map(
            m7_candidate::FullPlanePair {
                source,
                raster: selection.layout.raster,
                original_bl: &original,
                composed: &composed,
                original_bl_sha256,
                composed_sha256,
            },
            || cancel.is_cancelled(),
        )
        .map_err(execution)?;
        writer.write_all(&composed).map_err(execution)?;
        prepared.push(PreparedPlaneFrame {
            source,
            source_rpu,
            original_bl_sha256,
            composed_sha256,
            benefit_map,
        });
    }
    let mut trailer = [0; 24];
    read_exact(&mut reader, &mut trailer)?;
    if &trailer[..4] != b"END1"
        || u32_at(&trailer, 4) != 16
        || u64_at(&trailer, 8) != FRAMES as u64
        || u64_at(&trailer, 16) != (FRAMES * 2 * plane_bytes) as u64
        || reader.read(&mut [0]).map_err(execution)? != 0
    {
        return Err(CaptureError::Completion.into());
    }
    writer.flush().map_err(execution)?;
    drop(writer);
    let composed = plurx_core::fs_secure::open_read_nofollow_blocking(&path).map_err(execution)?;
    if composed.metadata().map_err(execution)?.len() != (FRAMES * plane_bytes) as u64 {
        return Err(CaptureError::Completion.into());
    }
    let composed = SourceFence::clone_current_held(&composed).map_err(Error::Execution)?;
    check_cancel(cancel)?;
    Ok(PreparedPlanes {
        composed,
        source_sha256: title_binding.0,
        probe_sha256: title_binding.1,
        source_object_version: title_binding.2,
        layout: selection.layout,
        frames: prepared,
        path,
        _dir: staged,
    })
}

fn verify_header(header: &[u8; 88], selection: CensusRequest) -> Result<(), CaptureError> {
    let layout = selection.layout;
    let [width, height] = layout.raster;
    let [x0, y0, x1, y1] = layout.roi;
    if &header[..8] != b"PLXM7P01"
        || u32_at(header, 8) != layout.video_index
        || Rational::new(u32_at(header, 12), u32_at(header, 16)) != Some(layout.time_base)
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
                FRAMES as u32,
                selection.first as u32,
                selection.last as u32,
            ])
        || i64_at(header, 72) != selection.first_pts
        || i64_at(header, 80) != selection.end_pts
    {
        return Err(CaptureError::Header);
    }
    Ok(())
}

fn read_exact(stream: &mut impl Read, bytes: &mut [u8]) -> Result<(), Error> {
    stream.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            CaptureError::Truncated.into()
        } else {
            execution(error)
        }
    })
}
