//! Exact clock mapping for the video rendition of a single-playlist splice.

use std::borrow::Cow;

pub use super::video_splice_format::{SpliceBoundary, SpliceVideoFormat, SpliceVideoRendition};

use super::{
    be_u32, build_moof, children, malformed, parse_moof, peek_box, Fmp4Error, Fragment, Plan,
    Sample, Track, TrackKind, VideoCodec,
};

/// The two presentation anchors must name the same source picture. The
/// publisher establishes that identity from producer observations before
/// using this mapping. The decode clock instead starts where the previous
/// published fragment ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSpliceClock {
    pub input_presentation_anchor: i128,
    pub output_presentation_anchor: u64,
    pub output_decode_start: u64,
}

pub(super) fn unsupported(reason: &str) -> Fmp4Error {
    Fmp4Error::Unsupported(reason.to_owned())
}

fn exact_rescale(ticks: i128, input_scale: u32, output_scale: u32) -> Result<i128, Fmp4Error> {
    let product = ticks
        .checked_mul(i128::from(output_scale))
        .ok_or_else(|| unsupported("splice timestamp exceeds the output clock"))?;
    let divisor = i128::from(input_scale);
    if product % divisor != 0 {
        return Err(unsupported("splice timestamp falls between output ticks"));
    }
    Ok(product / divisor)
}

/// Rewrite one clear HEVC video fragment against a previously published
/// clock. Sample data and presentation intervals stay intact. The sample
/// description and in-band HEVC parameter sets must already be compatible
/// with the output init.
pub(super) fn retime_video_fragment(
    fragment: &Fragment,
    input: &Track,
    output: &Track,
    clock: VideoSpliceClock,
    sequence: u32,
    mut rewrite: impl FnMut(&[u8], &Sample, bool) -> Result<Option<Vec<u8>>, Fmp4Error>,
) -> Result<Fragment, Fmp4Error> {
    if [input, output].iter().any(|track| {
        track.kind != TrackKind::Video
            || track.codec != Some(VideoCodec::Hevc)
            || track.has_edit_list
            || track.timescale == 0
            || track.id == 0
    }) || !matches!(input.nal_length_size, 1 | 2 | 4)
        || input.nal_length_size != output.nal_length_size
    {
        return Err(unsupported(
            "splice requires compatible clear HEVC video tracks",
        ));
    }

    let [track] = fragment.tracks.as_slice() else {
        return Err(unsupported("splice requires one video track per fragment"));
    };
    if track.track_id != input.id {
        return malformed("splice track id differs from the video init");
    }
    let moof = check_fragment_boxes(fragment)?;
    if parse_moof(
        &fragment.bytes[..moof.size],
        moof.header_len,
        std::slice::from_ref(input),
    )? != fragment.tracks
    {
        return malformed("splice sample metadata differs from the fragment bytes");
    }

    let mut plan = Plan {
        track_id: output.id,
        base_decode_time: clock.output_decode_start,
        samples: Vec::new(),
        slices: Vec::new(),
    };
    let mut input_dts = i128::from(track.base_decode_time);
    let mut output_dts = clock.output_decode_start;
    let mut payload = Vec::new();
    let mut payload_cursor = fragment.mdat_payload.start;
    let mut sample_bytes = fragment.samples_with_bytes(input.id).into_iter();
    for run in &track.runs {
        if run.data_offset != payload_cursor {
            return Err(unsupported("splice sample payload is not contiguous"));
        }
        for (sample, bytes) in sample_bytes.by_ref().take(run.samples.len()) {
            let bytes = bytes.ok_or_else(|| unsupported("splice sample lies outside mdat"))?;
            if sample.duration == 0 || bytes.is_empty() {
                return malformed("splice sample has zero duration or size");
            }
            let pts_from_anchor = (input_dts + i128::from(sample.cto))
                .checked_sub(clock.input_presentation_anchor)
                .ok_or_else(|| unsupported("splice presentation anchor overflows"))?;
            let output_pts = i128::from(clock.output_presentation_anchor)
                .checked_add(exact_rescale(
                    pts_from_anchor,
                    input.timescale,
                    output.timescale,
                )?)
                .ok_or_else(|| unsupported("splice presentation time exceeds the output clock"))?;
            let output_pts = u64::try_from(output_pts)
                .map_err(|_| unsupported("splice presentation time exceeds the output clock"))?;
            let cto = i32::try_from(i128::from(output_pts) - i128::from(output_dts))
                .map_err(|_| unsupported("splice composition offset exceeds the trun field"))?;
            let duration = u32::try_from(exact_rescale(
                i128::from(sample.duration),
                input.timescale,
                output.timescale,
            )?)
            .map_err(|_| unsupported("splice sample duration exceeds the trun field"))?;
            let packet = match rewrite(bytes, sample, plan.samples.is_empty())? {
                Some(changed) => Cow::Owned(changed),
                None => Cow::Borrowed(bytes),
            };
            let size = u32::try_from(packet.len())
                .map_err(|_| unsupported("splice sample size exceeds the trun field"))?;
            payload.push(packet);
            plan.samples.push(Sample {
                duration,
                size,
                cto: i64::from(cto),
                size_at: None,
                ..*sample
            });
            payload_cursor += bytes.len();
            input_dts += i128::from(sample.duration);
            output_dts = output_dts
                .checked_add(u64::from(duration))
                .ok_or_else(|| unsupported("splice decode time exceeds the output clock"))?;
        }
    }
    if plan.samples.is_empty() || payload_cursor != fragment.mdat_payload.end {
        return malformed("splice samples do not cover the mdat");
    }

    let pieces: Vec<_> = payload.iter().map(Cow::as_ref).collect();
    write_single_track_fragment(plan, output, sequence, &pieces)
}

pub(super) fn write_single_track_fragment(
    plan: Plan,
    output: &Track,
    sequence: u32,
    payload: &[&[u8]],
) -> Result<Fragment, Fmp4Error> {
    let mut offset_slots = Vec::new();
    let mut bytes = build_moof(&[plan], sequence, &mut offset_slots);
    let payload_start = bytes.len() + 8;
    let offset = i32::try_from(payload_start)
        .map_err(|_| unsupported("splice payload offset exceeds the trun field"))?;
    let slot = offset_slots[0];
    bytes[slot..slot + 4].copy_from_slice(&offset.to_be_bytes());
    let tracks = parse_moof(&bytes, 8, std::slice::from_ref(output))?;
    let box_size = u32::try_from(8 + payload.iter().map(|piece| piece.len()).sum::<usize>())
        .map_err(|_| unsupported("splice mdat exceeds the box field"))?;
    bytes.extend_from_slice(&box_size.to_be_bytes());
    bytes.extend_from_slice(b"mdat");
    for piece in payload {
        bytes.extend_from_slice(piece);
    }
    let payload_end = bytes.len();
    Ok(Fragment {
        bytes,
        mdat_payload: payload_start..payload_end,
        tracks,
    })
}

fn check_fragment_boxes(fragment: &Fragment) -> Result<super::BoxHeader, Fmp4Error> {
    let moof = peek_box(&fragment.bytes, 0)?
        .filter(|hdr| hdr.kind() == b"moof" && hdr.size <= fragment.bytes.len())
        .ok_or_else(|| unsupported("splice requires a bounded moof"))?;
    let mdat = peek_box(&fragment.bytes, moof.size)?
        .filter(|hdr| hdr.kind() == b"mdat")
        .ok_or_else(|| unsupported("splice requires mdat immediately after moof"))?;
    if fragment.mdat_payload != (moof.size + mdat.header_len..moof.size + mdat.size)
        || fragment.mdat_payload.end != fragment.bytes.len()
    {
        return malformed("splice mdat range differs from the fragment bytes");
    }
    let body = &fragment.bytes[moof.header_len..moof.size];
    let boxes = children(body)?;
    let [(mfhd, mfhd_start, mfhd_end), (traf, start, end)] = boxes.as_slice() else {
        return Err(unsupported("splice requires one mfhd and one traf"));
    };
    if mfhd.kind() != b"mfhd"
        || *mfhd_end - *mfhd_start != 8
        || be_u32(body, *mfhd_start) != 0
        || traf.kind() != b"traf"
        || *end != body.len()
    {
        return malformed("invalid splice moof");
    }
    check_track_boxes(&body[*start..*end])?;
    Ok(moof)
}

pub(super) fn check_track_boxes(traf: &[u8]) -> Result<(), Fmp4Error> {
    let boxes = children(traf)?;
    let [(tfhd, tfhd_start, tfhd_end), (tfdt, tfdt_start, tfdt_end), runs @ ..] = boxes.as_slice()
    else {
        return malformed("splice traf is missing its headers and runs");
    };
    let tfhd_body = &traf[*tfhd_start..*tfhd_end];
    if tfhd.kind() != b"tfhd" || tfhd_body.len() < 8 {
        return malformed("splice traf is missing a valid tfhd");
    }
    let flags = be_u32(tfhd_body, 0);
    if flags & !0x38 != 0x020000 || tfhd_body.len() != 8 + (flags & 0x38).count_ones() as usize * 4
    {
        return Err(unsupported("splice tfhd has unsupported flags"));
    }
    let tfdt_body = &traf[*tfdt_start..*tfdt_end];
    if tfdt.kind() != b"tfdt"
        || !matches!(
            tfdt_body,
            [0, 0, 0, 0, _, _, _, _] | [1, 0, 0, 0, _, _, _, _, _, _, _, _]
        )
    {
        return malformed("splice traf is missing a valid tfdt");
    }
    if !runs.last().is_some_and(|(_, _, end)| *end == traf.len()) {
        return malformed("splice traf is missing complete sample runs");
    }
    for (hdr, start, end) in runs {
        let body = &traf[*start..*end];
        if hdr.kind() != b"trun" || body.len() < 8 {
            return Err(unsupported("splice cannot rewrite a non-trun traf box"));
        }
        let version_and_flags = be_u32(body, 0);
        let flags = version_and_flags & 0x00ff_ffff;
        let sample_count = be_u32(body, 4);
        if version_and_flags >> 24 > 1
            || flags & !0x0f05 != 0
            || flags & 0x0404 == 0x0404
            || sample_count == 0
            || 8 + u64::from((flags & 5).count_ones()) * 4
                + u64::from((flags & 0x0f00).count_ones()) * u64::from(sample_count) * 4
                != body.len() as u64
        {
            return malformed("invalid splice trun");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::mp4_box;

    fn video(id: u32, timescale: u32) -> Track {
        Track {
            id,
            kind: TrackKind::Video,
            timescale,
            has_edit_list: false,
            codec: Some(VideoCodec::Hevc),
            dolby_vision_config: true,
            nal_length_size: 4,
            default_sample_duration: 0,
            default_sample_size: 0,
            default_sample_flags: 0,
        }
    }

    fn fragment(video: &Track) -> Fragment {
        let plan = Plan {
            track_id: video.id,
            base_decode_time: 48_000,
            samples: [2_002, 4_004, 1_001]
                .into_iter()
                .enumerate()
                .map(|(index, cto)| Sample {
                    duration: 1_001,
                    size: 7,
                    size_at: None,
                    flags: if index == 0 { 0x0200_0000 } else { 0x0101_0000 },
                    cto,
                })
                .collect(),
            slices: Vec::new(),
        };
        let mut slots = Vec::new();
        let mut bytes = build_moof(&[plan], 3, &mut slots);
        let payload_start = bytes.len() + 8;
        let offset = payload_start as i32;
        bytes[slots[0]..slots[0] + 4].copy_from_slice(&offset.to_be_bytes());
        let tracks =
            parse_moof(&bytes, 8, std::slice::from_ref(video)).expect("parse fixture moof");
        let payload = [
            0, 0, 0, 3, 38, 1, 0x80, 0, 0, 0, 3, 2, 1, 0x80, 0, 0, 0, 3, 2, 1, 0x80,
        ];
        bytes.extend_from_slice(&mp4_box(b"mdat", &payload).expect("write fixture mdat"));
        Fragment {
            mdat_payload: payload_start..bytes.len(),
            bytes,
            tracks,
        }
    }

    #[test]
    fn moves_decode_clock_at_the_splice_without_moving_presented_pictures() {
        let input = video(1, 24_000);
        let output = video(3, 48_000);
        let original = fragment(&input);
        let mapped = retime_video_fragment(
            &original,
            &input,
            &output,
            VideoSpliceClock {
                input_presentation_anchor: 50_002,
                output_presentation_anchor: 216_216,
                output_decode_start: 214_214,
            },
            47,
            |_, _, _| Ok(None),
        )
        .expect("retime a representable splice");

        assert_eq!(&mapped.bytes[20..24], &47u32.to_be_bytes());
        let track = mapped.track(output.id).expect("output video track");
        assert_eq!(track.base_decode_time, 214_214);
        assert_eq!(track.duration(), 6_006);
        assert_eq!(
            track.samples().map(|sample| sample.cto).collect::<Vec<_>>(),
            [2_002, 6_006, 0]
        );
        let originals = original.samples_with_bytes(input.id);
        for ((sample, bytes), (original, original_bytes)) in mapped
            .samples_with_bytes(output.id)
            .into_iter()
            .zip(originals)
        {
            assert_eq!(sample.flags, original.flags);
            assert_eq!(sample.duration, original.duration * 2);
            assert_eq!(
                bytes.expect("mapped video sample"),
                original_bytes.expect("source video sample")
            );
        }
    }

    #[test]
    fn requires_a_single_explicit_decode_time_and_nonempty_track() {
        let input = video(1, 24_000);
        let output = video(3, 48_000);
        let clock = VideoSpliceClock {
            input_presentation_anchor: 50_002,
            output_presentation_anchor: 216_216,
            output_decode_start: 214_214,
        };
        let original = fragment(&input);
        let mut missing_clock = original.clone();
        missing_clock.bytes[52..56].copy_from_slice(b"free");
        assert!(
            retime_video_fragment(&missing_clock, &input, &output, clock, 47, |_, _, _| Ok(
                None
            ))
            .is_err()
        );

        let mut duplicate_clock = original.clone();
        duplicate_clock.bytes[72..76].copy_from_slice(b"tfdt");
        assert!(
            retime_video_fragment(&duplicate_clock, &input, &output, clock, 47, |_, _, _| Ok(
                None
            ))
            .is_err()
        );

        let mut empty_track = original;
        empty_track.bytes[41] |= 1;
        assert!(
            retime_video_fragment(&empty_track, &input, &output, clock, 47, |_, _, _| Ok(None))
                .is_err()
        );
    }

    #[test]
    fn refuses_fractional_tick_conversion_and_overlapping_payloads() {
        let input = video(1, 24_000);
        let output = video(3, 16_000);
        let original = fragment(&input);
        let clock = VideoSpliceClock {
            input_presentation_anchor: 50_002,
            output_presentation_anchor: 216_216,
            output_decode_start: 214_214,
        };
        assert!(matches!(
            retime_video_fragment(&original, &input, &output, clock, 47, |_, _, _| Ok(None)),
            Err(Fmp4Error::Unsupported(reason)) if reason.contains("between output ticks")
        ));

        let mut invalid = original;
        let track = &mut invalid.tracks[0];
        track.runs.push(track.runs[0].clone());
        assert!(retime_video_fragment(
            &invalid,
            &input,
            &video(3, 48_000),
            clock,
            47,
            |_, _, _| Ok(None)
        )
        .is_err());
    }
}
