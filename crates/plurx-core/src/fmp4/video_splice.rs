//! Exact clock mapping for the video rendition of a single-playlist splice.

use std::borrow::Cow;
use std::collections::BTreeMap;

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

/// Original presentation clock and sample duration of one proved source
/// picture, expressed in the ordinary media track's timescale.
#[derive(Clone, Copy)]
pub struct CopyPictureTime {
    pub pts: u64,
    pub duration: u32,
}

/// Match the first FEL source pictures to the first unpublished ordinary
/// segment. The publisher binds both inputs to the same selected source
/// and rechecks its ownership and segment frontier at publication.
pub fn match_splice_pictures(
    ordinary: &Fragment,
    input: &Track,
    source_pictures: &[[u8; 32]],
) -> Result<Vec<CopyPictureTime>, Fmp4Error> {
    if input.kind != TrackKind::Video || input.nal_length_size != 4 {
        return Err(unsupported(
            "splice witness requires four-byte HEVC framing",
        ));
    }
    let track = ordinary
        .track(input.id)
        .ok_or_else(|| unsupported("ordinary splice anchor has no video track"))?;
    let moof = check_fragment_boxes(ordinary)?;
    let parsed = parse_moof(
        &ordinary.bytes[..moof.size],
        moof.header_len,
        std::slice::from_ref(input),
    )?;
    if parsed != ordinary.tracks {
        return malformed("ordinary splice anchor differs from its serialized samples");
    }
    let mut dts = track.base_decode_time;
    let mut pictures = Vec::new();
    for (sample, bytes) in ordinary.samples_with_bytes(input.id) {
        let pts = u64::try_from(i128::from(dts) + i128::from(sample.cto))
            .map_err(|_| unsupported("ordinary splice presentation time is out of range"))?;
        let bytes = bytes.ok_or_else(|| unsupported("ordinary splice sample lies outside mdat"))?;
        let time = CopyPictureTime {
            pts,
            duration: sample.duration,
        };
        pictures.push((time, super::hevc_base_layer_vcl_sha256(bytes)?));
        dts = dts
            .checked_add(u64::from(sample.duration))
            .ok_or_else(|| unsupported("ordinary splice decode time is out of range"))?;
    }

    pictures.sort_unstable_by_key(|(time, _)| time.pts);
    if pictures
        .windows(2)
        .any(|pair| pair[0].0.pts == pair[1].0.pts)
    {
        return Err(unsupported(
            "ordinary splice starts on an ambiguous presentation clock",
        ));
    }
    let overlap = source_pictures.len();
    if overlap < 2 || overlap > pictures.len() {
        return Err(unsupported(
            "ordinary segment does not cover the complete witnessed FEL window",
        ));
    }
    let matches = |window: &[(CopyPictureTime, [u8; 32])]| {
        window.iter().map(|(_, digest)| digest).eq(source_pictures)
    };
    if !matches(&pictures[..overlap]) || pictures.windows(overlap).skip(1).any(matches) {
        return Err(unsupported(
            "FEL source pictures do not uniquely match the next ordinary interval",
        ));
    }
    Ok(pictures[..overlap].iter().map(|(time, _)| *time).collect())
}

pub(super) fn copy_picture_schedule(
    input: &Fragment,
    input_track: &Track,
    output: &Track,
    copy_times: &[CopyPictureTime],
    copy_scale: u32,
) -> Result<BTreeMap<i128, (u64, u32)>, Fmp4Error> {
    let video = input
        .track(input_track.id)
        .ok_or_else(|| unsupported("replacement window has no video track"))?;
    if copy_scale == 0 || output.timescale == 0 || video.sample_count() != copy_times.len() {
        return Err(unsupported(
            "replacement window differs from the copy picture schedule",
        ));
    }
    let mut dts = i128::from(video.base_decode_time);
    let mut order = Vec::with_capacity(video.sample_count());
    for sample in video.samples() {
        order.push(dts + i128::from(sample.cto));
        dts += i128::from(sample.duration);
    }
    order.sort_unstable();
    if order.windows(2).any(|pair| pair[0] == pair[1])
        || copy_times.windows(2).any(|pair| pair[0].pts >= pair[1].pts)
    {
        return Err(unsupported(
            "replacement picture schedule repeats or reorders a presentation time",
        ));
    }
    order
        .into_iter()
        .zip(copy_times)
        .map(|(from, to)| {
            let pts = u64::try_from(exact_rescale(
                i128::from(to.pts),
                copy_scale,
                output.timescale,
            )?)
            .map_err(|_| unsupported("replacement presentation time exceeds the output clock"))?;
            let duration = u32::try_from(exact_rescale(
                i128::from(to.duration),
                copy_scale,
                output.timescale,
            )?)
            .map_err(|_| unsupported("replacement duration exceeds the trun field"))?;
            if duration == 0 {
                return malformed("replacement picture duration is zero");
            }
            Ok((from, (pts, duration)))
        })
        .collect()
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
    rewrite: impl FnMut(&[u8], &Sample, bool) -> Result<Option<Vec<u8>>, Fmp4Error>,
) -> Result<Fragment, Fmp4Error> {
    let timing = |pts: i128, duration: u32| {
        let relative = pts
            .checked_sub(clock.input_presentation_anchor)
            .ok_or_else(|| unsupported("splice presentation anchor overflows"))?;
        let mapped = i128::from(clock.output_presentation_anchor)
            .checked_add(exact_rescale(relative, input.timescale, output.timescale)?)
            .ok_or_else(|| unsupported("splice presentation time exceeds the output clock"))?;
        let mapped = u64::try_from(mapped)
            .map_err(|_| unsupported("splice presentation time exceeds the output clock"))?;
        let duration = u32::try_from(exact_rescale(
            i128::from(duration),
            input.timescale,
            output.timescale,
        )?)
        .map_err(|_| unsupported("splice sample duration exceeds the trun field"))?;
        Ok((mapped, duration))
    };
    author_video_fragment(
        fragment,
        input,
        output,
        clock.output_decode_start,
        sequence,
        timing,
        rewrite,
    )
}

pub(super) fn author_video_fragment(
    fragment: &Fragment,
    input: &Track,
    output: &Track,
    decode_start: u64,
    sequence: u32,
    mut timing: impl FnMut(i128, u32) -> Result<(u64, u32), Fmp4Error>,
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
        base_decode_time: decode_start,
        samples: Vec::new(),
        slices: Vec::new(),
    };
    let mut input_dts = i128::from(track.base_decode_time);
    let mut output_dts = decode_start;
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
            let (output_pts, duration) =
                timing(input_dts + i128::from(sample.cto), sample.duration)?;
            let cto = i32::try_from(i128::from(output_pts) - i128::from(output_dts))
                .map_err(|_| unsupported("splice composition offset exceeds the trun field"))?;
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
    fn witness_uses_display_order_and_refuses_a_later_matching_interval() {
        let input = video(1, 24_000);
        let mut ordinary = fragment(&input);
        let byte_to_mark = ordinary.mdat_payload.start + 2 * 7 + 6;
        ordinary.bytes[byte_to_mark] = 0x81;
        let digests: Vec<_> = ordinary
            .samples_with_bytes(input.id)
            .into_iter()
            .map(|(_, bytes)| {
                super::super::hevc_base_layer_vcl_sha256(bytes.expect("coded source sample"))
                    .expect("valid coded picture")
            })
            .collect();
        assert_eq!(
            match_splice_pictures(&ordinary, &input, &[digests[0], digests[2], digests[1]])
                .expect("source pictures in presentation order")[0]
                .pts,
            50_002
        );
        assert!(match_splice_pictures(&ordinary, &input, &[digests[0], digests[1]]).is_err());
        assert!(match_splice_pictures(&ordinary, &input, &[digests[2], digests[1]]).is_err());
    }

    #[test]
    fn replacement_uses_the_copy_cadence_even_when_decode_order_differs() {
        let input = video(1, 24_000);
        let output = video(1, 48_000);
        let original = fragment(&input);
        let times = [
            CopyPictureTime {
                pts: 12_000,
                duration: 667,
            },
            CopyPictureTime {
                pts: 12_667,
                duration: 668,
            },
            CopyPictureTime {
                pts: 13_335,
                duration: 667,
            },
        ];
        let mut schedule = copy_picture_schedule(&original, &input, &output, &times, 16_000)
            .expect("copy clock is exactly representable");
        let mapped = author_video_fragment(
            &original,
            &input,
            &output,
            33_998,
            9,
            |pts, _| Ok(schedule.remove(&pts).expect("one mapped source picture")),
            |_, _, _| Ok(None),
        )
        .expect("write matched picture schedule");
        assert!(schedule.is_empty());
        let mapped_track = mapped.track(output.id).expect("mapped track");
        assert_eq!(mapped_track.base_decode_time, 33_998);
        assert_eq!(mapped_track.duration(), 6_006);
        let timing: Vec<_> = mapped_track
            .samples()
            .map(|sample| (sample.duration, sample.cto))
            .collect();
        assert_eq!(timing, [(2_001, 2_002), (2_001, 4_006), (2_004, 1)]);
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
