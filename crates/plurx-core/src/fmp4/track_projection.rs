//! Split the live copy muxer's authored audio and video without encoding,
//! resetting either track's timestamps, or changing its samples.

use super::video_splice::{check_track_boxes, unsupported, write_single_track_fragment};
use super::{
    aac_lc_sample_entry_facts, be_u32, children, malformed, mp4_box, parse_moof,
    replace_dolby_file_type_brand, track_id_in_trak, Fmp4Error, Fragment, FragmentReader, Init,
    Plan, TrackFragment, TrackKind, Unit,
};

pub struct CopyTrackProjection {
    multiplexed: Init,
    audio: Init,
    video: Init,
}

pub struct ProjectedCopySegment {
    pub audio: Fragment,
    pub video: Fragment,
}

impl CopyTrackProjection {
    /// Split the single-video/single-AAC rolling copy output. Preserve track
    /// IDs and each track's original initialization data.
    pub fn new(multiplexed: Init) -> Result<Self, Fmp4Error> {
        let [first, second] = multiplexed.tracks.as_slice() else {
            return Err(unsupported("split copy needs exactly two tracks"));
        };
        let (video_id, audio_id) = match (first.kind, second.kind) {
            (TrackKind::Video, TrackKind::Audio) => (first.id, second.id),
            (TrackKind::Audio, TrackKind::Video) => (second.id, first.id),
            _ => return Err(unsupported("split copy needs video and audio")),
        };
        if video_id == audio_id || multiplexed.tracks.iter().any(|track| track.has_edit_list) {
            return Err(unsupported(
                "split copy cannot preserve track edits or aliases",
            ));
        }

        let mut audio = project_init(&multiplexed, audio_id)?;
        replace_dolby_file_type_brand(&mut audio.bytes);
        aac_lc_sample_entry_facts(&audio)?;
        let video = project_init(&multiplexed, video_id)?;
        Ok(Self {
            multiplexed,
            audio,
            video,
        })
    }

    pub fn audio_init(&self) -> &Init {
        &self.audio
    }

    pub fn video_init(&self) -> &Init {
        &self.video
    }

    /// Project one already-authored rolling copy segment. Each media
    /// fragment has its own absolute decode origin, including the audio.
    /// The caller checks that audio joins exactly to the previous segment.
    pub fn segment(&self, bytes: &[u8], sequence: u32) -> Result<ProjectedCopySegment, Fmp4Error> {
        let top = children(bytes)?;
        let [index @ .., (moof, moof_at, moof_end), (mdat, mdat_at, segment_end)] = top.as_slice()
        else {
            return malformed("split copy needs moof followed by mdat");
        };
        if index
            .iter()
            .any(|(header, _, _)| !matches!(header.kind(), b"styp" | b"sidx"))
            || moof.kind() != b"moof"
            || mdat.kind() != b"mdat"
            || *segment_end != bytes.len()
        {
            return Err(unsupported("split copy segment contains unsupported boxes"));
        }
        let moof_body = &bytes[*moof_at..*moof_end];
        let structure = children(moof_body)?;
        let [(mfhd, start, end), tracks @ ..] = structure.as_slice() else {
            return malformed("split copy moof has no header");
        };
        if mfhd.kind() != b"mfhd"
            || *end - *start != 8
            || be_u32(moof_body, *start) != 0
            || tracks.len() != 2
            || !tracks
                .last()
                .is_some_and(|(_, _, end)| *end == moof_body.len())
        {
            return Err(unsupported(
                "split copy moof requires two clear track fragments",
            ));
        }
        for (header, start, end) in tracks {
            if header.kind() != b"traf" {
                return Err(unsupported("split copy moof cannot carry auxiliary data"));
            }
            check_track_boxes(&moof_body[*start..*end])?;
        }

        let moof_start = *moof_at - moof.header_len;
        let tracks = parse_moof(
            &bytes[moof_start..*moof_end],
            moof.header_len,
            &self.multiplexed.tracks,
        )?;
        let fragment = Fragment {
            bytes: bytes[moof_start..*segment_end].to_vec(),
            mdat_payload: (*mdat_at - moof_start)..(*segment_end - moof_start),
            tracks,
        };
        check_media_extents(&fragment)?;
        let audio_track = select_track(&fragment, &self.audio)?;
        let video_track = select_track(&fragment, &self.video)?;
        Ok(ProjectedCopySegment {
            audio: project_track(&fragment, audio_track, &self.audio, sequence)?,
            video: project_track(&fragment, video_track, &self.video, sequence)?,
        })
    }
}

pub(super) fn check_media_extents(fragment: &Fragment) -> Result<(), Fmp4Error> {
    let mut extents = Vec::new();
    for track in &fragment.tracks {
        for run in &track.runs {
            let mut end = run.data_offset;
            for sample in &run.samples {
                end = end
                    .checked_add(sample.size as usize)
                    .filter(|end| *end <= fragment.mdat_payload.end)
                    .ok_or_else(|| unsupported("split copy run exceeds mdat"))?;
            }
            if end <= run.data_offset {
                return malformed("split copy has an empty media run");
            }
            extents.push(run.data_offset..end);
        }
    }
    extents.sort_unstable_by_key(|range| range.start);
    let mut next = fragment.mdat_payload.start;
    for extent in extents {
        if extent.start != next {
            return malformed("split copy runs overlap or leave bytes outside samples");
        }
        next = extent.end;
    }
    if next != fragment.mdat_payload.end {
        return malformed("split copy runs do not cover mdat");
    }
    Ok(())
}

fn select_track<'a>(fragment: &'a Fragment, init: &Init) -> Result<&'a TrackFragment, Fmp4Error> {
    fragment
        .track(init.tracks[0].id)
        .ok_or_else(|| unsupported("split copy segment does not carry both admitted tracks"))
}

fn project_track(
    fragment: &Fragment,
    track: &TrackFragment,
    init: &Init,
    sequence: u32,
) -> Result<Fragment, Fmp4Error> {
    let mut payload = Vec::with_capacity(track.sample_count());
    let mut samples = Vec::with_capacity(track.sample_count());
    for (sample, bytes) in fragment.samples_with_bytes(track.track_id) {
        let bytes = bytes.ok_or_else(|| unsupported("projected sample lies outside mdat"))?;
        if sample.duration == 0 || bytes.is_empty() || i32::try_from(sample.cto).is_err() {
            return Err(unsupported(
                "projected sample duration, size or composition offset is invalid",
            ));
        }
        payload.push(bytes);
        samples.push(*sample);
    }
    let plan = Plan {
        track_id: track.track_id,
        base_decode_time: track.base_decode_time,
        samples,
        slices: Vec::new(),
    };
    write_single_track_fragment(plan, &init.tracks[0], sequence, &payload)
}

fn project_init(multiplexed: &Init, id: u32) -> Result<Init, Fmp4Error> {
    let bytes = &multiplexed.bytes;
    let top = children(bytes)?;
    let [(ftyp, _, ftyp_end), (moov, moov_start, end)] = top.as_slice() else {
        return unsupported_init();
    };
    if ftyp.kind() != b"ftyp" || moov.kind() != b"moov" || *end != bytes.len() {
        return unsupported_init();
    }
    let mut movie = Vec::new();
    for (header, start, end) in children(&bytes[*moov_start..])? {
        let start = *moov_start + start;
        let end = *moov_start + end;
        match header.kind() {
            b"mvhd" => movie.extend_from_slice(&bytes[start - header.header_len..end]),
            b"trak" => {
                if track_id_in_trak(bytes, start..end)? == Some(id) {
                    movie.extend_from_slice(&bytes[start - header.header_len..end]);
                }
            }
            b"mvex" => {
                let mut defaults = None;
                for (entry, entry_start, entry_end) in children(&bytes[start..end])? {
                    let payload = &bytes[start + entry_start..start + entry_end];
                    if entry.kind() != b"trex" || payload.len() != 24 || be_u32(payload, 0) != 0 {
                        return unsupported_init();
                    }
                    if be_u32(payload, 4) == id
                        && (be_u32(payload, 8) != 1
                            || defaults
                                .replace(
                                    &bytes
                                        [start + entry_start - entry.header_len..start + entry_end],
                                )
                                .is_some())
                    {
                        return unsupported_init();
                    }
                }
                let defaults =
                    defaults.ok_or_else(|| unsupported("split copy track has no trex"))?;
                movie.extend_from_slice(&mp4_box(b"mvex", defaults)?);
            }
            b"udta" => {}
            _ => return unsupported_init(),
        }
    }
    let mut init_bytes = bytes[..*ftyp_end].to_vec();
    init_bytes.extend_from_slice(&mp4_box(b"moov", &movie)?);
    let mut reader = FragmentReader::new();
    reader.push(&init_bytes);
    let Some(Unit::Init(init)) = reader.next_unit()? else {
        return unsupported_init();
    };
    if init.tracks.len() != 1 || init.tracks.first() != multiplexed.track(id) {
        return unsupported_init();
    }
    Ok(init)
}

fn unsupported_init<T>() -> Result<T, Fmp4Error> {
    Err(unsupported(
        "split copy init is outside the supported fMP4 layout",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_copy_preserves_aac_packet_bytes_and_original_timestamps() {
        let bytes = crate::testfixtures::pipe("closed-gop");
        let mut reader = FragmentReader::new();
        reader.push(&bytes);
        let Some(Unit::Init(init)) = reader.next_unit().expect("read fixture initialization")
        else {
            panic!("fixture must start with an init");
        };
        let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("read fixture fragment")
        else {
            panic!("fixture must have a media fragment");
        };
        let projection = CopyTrackProjection::new(init).expect("project fixture initialization");
        let projected = projection
            .segment(&fragment.bytes, 12)
            .expect("project fixture segment");
        let audio = projection.audio_init();
        let id = audio.tracks[0].id;
        assert_eq!(audio.tracks[0].timescale, 48_000);
        assert_eq!(projected.audio.tracks.len(), 1);
        assert_eq!(projected.audio.tracks[0].track_id, id);
        assert_eq!(
            projected.audio.tracks[0].base_decode_time,
            fragment
                .track(id)
                .expect("original audio track")
                .base_decode_time
        );
        let original_samples = fragment.samples_with_bytes(id);
        let projected_samples = projected.audio.samples_with_bytes(id);
        assert_eq!(original_samples.len(), projected_samples.len());
        for ((expected, original), (actual, actual_bytes)) in
            original_samples.into_iter().zip(projected_samples)
        {
            assert_eq!(expected.duration, actual.duration);
            assert_eq!(expected.flags, actual.flags);
            assert_eq!(expected.cto, actual.cto);
            assert_eq!(
                original.expect("source AAC bytes"),
                actual_bytes.expect("projected AAC bytes")
            );
        }

        let mut alias = fragment.clone();
        alias.tracks[1].runs[0].data_offset = alias.tracks[0].runs[0].data_offset;
        assert!(check_media_extents(&alias).is_err());
        let mut oversized = fragment;
        oversized.tracks[1].runs[0].samples[0].size = u32::MAX;
        assert!(check_media_extents(&oversized).is_err());
    }
}
