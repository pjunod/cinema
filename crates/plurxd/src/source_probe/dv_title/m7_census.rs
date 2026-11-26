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
const MAX_PACKET_BYTES: u64 = 64 * 1024 * 1024;

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
    video_packet_bytes: u64,
    presentation_ticks: u64,
    windows: Vec<Window>,
}

struct Window {
    range: M7WindowRange,
    frames: Vec<FrameEvidence>,
}

impl VerifiedSourceCensus {
    /// Coded byte sum, presentation span in ticks, and tick duration for the
    /// same complete selected video track as the authenticated windows.
    pub(in crate::source_probe) fn coded_video_extent(&self) -> (u64, u64, Rational) {
        (
            self.video_packet_bytes,
            self.presentation_ticks,
            self.time_base,
        )
    }

    /// The authenticated pictures of the window whose first presentation is
    /// `first_pts`. A window's range ends where its successor starts, which a
    /// consumer holding only the pictures cannot rebuild: on a rounded clock the
    /// last picture's stored end falls short of it. The start alone names the
    /// window because `read` admitted only the requested, ordered, disjoint
    /// ranges, each beginning with a picture at its `first_pts`.
    pub(in crate::source_probe) fn window(
        &self,
        source: &SourceFence,
        tools: &ToolSet,
        layout: CaptureLayout,
        first_pts: i64,
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
        window_starting(&self.windows, first_pts)
    }
}

fn window_starting(windows: &[Window], first_pts: i64) -> Option<&[FrameEvidence]> {
    windows
        .iter()
        .find(|window| window.range.first_pts == first_pts)
        .map(|window| window.frames.as_slice())
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
    let mut args = vec!["--m7-window-census-v3".into(), OUTPUT.into()];
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
    video_packet_bytes: u64,
    first_pts: i64,
    end_pts: i64,
    windows: Vec<WireWindow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWindow {
    first_pts: i64,
    end_pts: i64,
    frames: Vec<WireFrame>,
    /// Where the presentation after the window's last picture starts on the
    /// sorted title clock, or the last picture's end when it ends the title.
    successor_pts: i64,
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
    let presentation_ticks = document
        .end_pts
        .checked_sub(document.first_pts)
        .and_then(|span| u64::try_from(span).ok())
        .filter(|&span| span != 0)
        .ok_or("invalid M7 selected video presentation span")?;
    let max_packet_sum = title_frames
        .checked_mul(MAX_PACKET_BYTES)
        .ok_or("M7 selected video packet extent exceeds its bound")?;
    if document.schema != 3
        || !document.complete
        || document.video_index != u32::from(video_index)
        || document.title_frames != title_frames
        || !(title_frames..=max_packet_sum).contains(&document.video_packet_bytes)
        || document.first_pts < 0
        || document.end_pts == i64::MAX
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
            || observed.first_pts < document.first_pts
            || observed.end_pts > document.end_pts
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
        if !frames.last().is_some_and(|last| {
            reaches_window_end(
                (last.ordinal, last.pts, last.duration),
                observed.successor_pts,
                range.end_pts,
                title_frames,
            )
        }) {
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
        video_packet_bytes: document.video_packet_bytes,
        presentation_ticks,
        windows,
    })
}

/// Whether a window whose last picture is `(ordinal, pts, duration)` ends
/// exactly at `end_pts`. The window is the presentations starting in
/// `[first_pts, end_pts)`, so the next one on the title clock (the scanner's
/// `successor_pts`) starts at `end_pts`. A container's rounded duration need
/// not reach it (see `m7_title_census.h`); a window that ends the title has no
/// successor and is bounded by its last picture's own end instead.
fn reaches_window_end(
    (ordinal, pts, duration): (u64, i64, i64),
    successor_pts: i64,
    end_pts: i64,
    title_frames: u64,
) -> bool {
    successor_pts == end_pts
        && if ordinal + 1 == title_frames {
            pts.checked_add(duration) == Some(successor_pts)
        } else {
            successor_pts > pts
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Eclipse's 23.976 fps Matroska clock: 1 ms ticks, each picture stored as
    /// 41 ticks while its PTS steps 41 or 42, and 48 pictures spanning exactly
    /// 2002 ticks. A window ending one tick past its last picture's stored end
    /// still ends exactly where the next picture starts.
    #[test]
    fn a_window_ends_where_its_successor_starts_not_where_a_rounded_duration_does() {
        let (start, end) = (125, 125 + 2002);
        let last = (47, start + 1961, 41);
        assert!(reaches_window_end(last, end, end, 1000));
        assert!(
            !reaches_window_end(last, end + 1, end, 1000),
            "late successor"
        );
        assert!(
            !reaches_window_end(last, end - 1, end, 1000),
            "early successor"
        );
        assert!(
            !reaches_window_end((47, end, 41), end, end, 1000),
            "a successor cannot start at or before the last picture"
        );
        // The title's last picture has no successor: its own end bounds it.
        assert!(reaches_window_end((999, end - 41, 41), end, end, 1000));
        assert!(!reaches_window_end((999, end - 42, 41), end, end, 1000));
    }

    /// Eclipse's comparison holds only the 48 pictures, so the end it can
    /// derive is the last picture's rounded stored end, one tick short of the
    /// successor-bounded range. Its window must still be found by its start.
    #[test]
    fn a_window_is_found_from_its_pictures_when_its_last_duration_is_rounded() {
        let time_base = Rational::new(1, 1000).expect("Matroska clock");
        let picture = |ordinal: u64, pts: i64| FrameEvidence {
            ordinal,
            pts,
            duration: 41,
            time_base,
            access_unit_sha256: [ordinal as u8; 32],
            rpu_sha256: [1; 32],
            bl_vcl_sha256: [2; 32],
        };
        let windows = [
            Window {
                range: M7WindowRange {
                    first_pts: 125,
                    end_pts: 2127,
                },
                frames: vec![picture(3, 125), picture(4, 2085)],
            },
            Window {
                range: M7WindowRange {
                    first_pts: 4000,
                    end_pts: 6002,
                },
                frames: vec![picture(96, 4000)],
            },
        ];
        let found = window_starting(&windows, 125).expect("first window");
        let last = found.last().expect("pictures");
        assert_eq!(last.pts + last.duration, 2126, "derived end is short");
        assert_eq!(found, windows[0].frames.as_slice());
        assert_eq!(
            window_starting(&windows, 4000),
            Some(windows[1].frames.as_slice())
        );
        assert_eq!(window_starting(&windows, 126), None, "mid-window start");
    }

    #[test]
    fn the_census_request_names_the_successor_bounded_schema() {
        let args = arguments(&[M7WindowRange {
            first_pts: 125,
            end_pts: 2127,
        }])
        .expect("one ordered window");
        assert_eq!(args[0], "--m7-window-census-v3");
    }
}
