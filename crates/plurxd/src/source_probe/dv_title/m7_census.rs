//! The whole-title scanner's selective, exact display-ordinal census.

use crate::{dv_tools::ToolSet, fragment_index_cluster::SourceFence};
use plurx_core::transcode::{
    dv_m7_scoring::{
        aggregation::MAX_FRAMES_PER_WINDOW,
        capture::{supported_raster, CaptureLayout, CaptureRole, FrameEvidence},
    },
    Rational,
};
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

pub(super) const OUTPUT: &str = "m7-window-census.json";
const MAX_WINDOWS: usize = 16;
const MAX_BYTES: u64 = 512 * 1024;

#[derive(Clone, Copy)]
pub(in crate::source_probe) struct M7WindowRange {
    pub first_pts: i64,
    pub end_pts: i64,
}

pub(in crate::source_probe) struct VerifiedSourceCensus {
    source_version: String,
    scanner: String,
    video_index: u32,
    time_base: Rational,
    raster: [u16; 2],
    windows: Vec<Window>,
}

struct Window {
    range: M7WindowRange,
    frames: Vec<FrameEvidence>,
}

impl VerifiedSourceCensus {
    pub(in crate::source_probe) fn window(
        &self,
        source: &SourceFence,
        tools: &ToolSet,
        layout: CaptureLayout,
        first_pts: i64,
        end_pts: i64,
    ) -> Option<&[FrameEvidence]> {
        if source.object_version() != self.source_version
            || tools.title_scanner_digest() != self.scanner
            || layout.role != CaptureRole::SourceFel
            || layout.video_index != self.video_index
            || layout.time_base != self.time_base
            || layout.raster != self.raster
        {
            return None;
        }
        self.windows
            .iter()
            .find(|window| window.range.first_pts == first_pts && window.range.end_pts == end_pts)
            .map(|window| window.frames.as_slice())
    }
}

pub(super) fn arguments(ranges: &[M7WindowRange]) -> Result<Vec<String>, String> {
    if !(1..=MAX_WINDOWS).contains(&ranges.len())
        || ranges.iter().any(|range| {
            range.first_pts < 0 || range.first_pts >= range.end_pts || range.end_pts == i64::MAX
        })
        || ranges
            .windows(2)
            .any(|pair| pair[0].end_pts > pair[1].first_pts)
    {
        return Err("M7 requires ordered disjoint title census windows".into());
    }
    let mut args = vec!["--m7-window-census-v1".into(), OUTPUT.into()];
    for range in ranges {
        args.extend([range.first_pts.to_string(), range.end_pts.to_string()]);
    }
    Ok(args)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u8,
    complete: bool,
    video_index: u32,
    time_base_num: u32,
    time_base_den: u32,
    width: u16,
    height: u16,
    title_frames: u64,
    windows: Vec<WireWindow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWindow {
    first_pts: i64,
    end_pts: i64,
    frames: Vec<WireFrame>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFrame {
    ordinal: u64,
    pts: i64,
    duration: i64,
    au: String,
    rpu: String,
    vcl: String,
}

pub(super) fn read(
    path: &Path,
    source: &SourceFence,
    tools: &ToolSet,
    video_index: u8,
    title_frames: u64,
    ranges: &[M7WindowRange],
) -> Result<VerifiedSourceCensus, String> {
    let file = plurx_core::fs_secure::open_read_nofollow_blocking(path).map_err(string)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(string)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("M7 title census exceeds its bounded sidecar".into());
    }
    let document: Document = serde_json::from_slice(&bytes).map_err(string)?;
    let time_base = Rational::new(document.time_base_num, document.time_base_den)
        .ok_or("invalid M7 title source clock")?;
    if document.schema != 1
        || !document.complete
        || document.video_index != u32::from(video_index)
        || document.title_frames != title_frames
        || document.windows.len() != ranges.len()
        || !supported_raster([document.width, document.height])
    {
        return Err("M7 census whole-title coverage differs".into());
    }
    let mut windows = Vec::with_capacity(ranges.len());
    let mut previous_ordinal = None;
    for (observed, range) in document.windows.into_iter().zip(ranges) {
        if observed.first_pts != range.first_pts
            || observed.end_pts != range.end_pts
            || !(1..=MAX_FRAMES_PER_WINDOW).contains(&observed.frames.len())
        {
            return Err("M7 census window differs from its request".into());
        }
        let mut frames = Vec::<FrameEvidence>::with_capacity(observed.frames.len());
        for frame in observed.frames {
            if frame.ordinal >= title_frames
                || frame.duration <= 0
                || frame
                    .pts
                    .checked_add(frame.duration)
                    .is_none_or(|end| end > range.end_pts)
                || frames.last().map_or(
                    frame.pts != range.first_pts
                        || previous_ordinal.is_some_and(|prior| frame.ordinal <= prior),
                    |prior| frame.pts <= prior.pts || frame.ordinal != prior.ordinal + 1,
                )
            {
                return Err("M7 coded picture does not match the planned ordinal and clock".into());
            }
            previous_ordinal = Some(frame.ordinal);
            frames.push(FrameEvidence {
                ordinal: frame.ordinal,
                pts: frame.pts,
                duration: frame.duration,
                time_base,
                access_unit_sha256: digest(&frame.au)?,
                rpu_sha256: digest(&frame.rpu)?,
                bl_vcl_sha256: digest(&frame.vcl)?,
            });
        }
        if frames
            .last()
            .and_then(|frame| frame.pts.checked_add(frame.duration))
            != Some(range.end_pts)
        {
            return Err("M7 title census did not reach the exact window endpoint".into());
        }
        windows.push(Window {
            range: *range,
            frames,
        });
    }
    Ok(VerifiedSourceCensus {
        source_version: source.object_version().to_owned(),
        scanner: tools.title_scanner_digest(),
        video_index: document.video_index,
        time_base,
        raster: [document.width, document.height],
        windows,
    })
}

fn digest(hexadecimal: &str) -> Result<[u8; 32], String> {
    let mut bytes = [0; 32];
    if !hexadecimal
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid M7 census SHA-256 spelling".into());
    }
    hex::decode_to_slice(hexadecimal, &mut bytes).map_err(string)?;
    Ok(bytes)
}

fn string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
