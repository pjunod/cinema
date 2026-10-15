//! Repair only the dvh1 array-completeness omission proven by the opening AU.
//! FFmpeg 8's MOV muxer sets these flags for hvc1, but not for dvh1. Presence
//! alone is insufficient: the complete authored tuple must agree both ways.
use super::*;

fn is_dvh1(bytes: &[u8], location: &HvcCLocation) -> bool {
    let entry = location.ancestors[1];
    bytes.get(entry.start + 4..entry.start + 8) == Some(b"dvh1")
}

fn canonical(nals: impl IntoIterator<Item = Vec<u8>>) -> Vec<Vec<u8>> {
    let mut nals: Vec<_> = nals
        .into_iter()
        .map(|nal| without_trailing_zero_bytes(&nal).to_vec())
        .collect();
    nals.sort_unstable();
    nals.dedup();
    nals
}

pub(super) fn capture(
    init: &Init,
    sample: &[u8],
    length_size: u8,
) -> Result<Vec<Vec<u8>>, Fmp4Error> {
    // Selection/structure failures retain their existing typed owner; they
    // are not malformed opening-sample evidence for this narrow repair.
    let Ok(Some(location)) = locate_configuring_hvcc(&init.bytes) else {
        return Ok(Vec::new());
    };
    if !is_dvh1(&init.bytes, &location) {
        return Ok(Vec::new());
    }
    let (present, complete) = hvcc_nal_arrays(&init.bytes[location.payload.clone()])?;
    if complete || !(32..=34).all(|kind| present.contains(&kind)) {
        return Ok(Vec::new());
    }
    let mut nals = Vec::new();
    visit_length_prefixed_hevc_nals(sample, length_size, |framed, kind| {
        if matches!(kind, Some(32..=34)) {
            nals.push(framed[usize::from(length_size)..].to_vec());
        }
    })?;
    // Keep partial and conflicting tuples: the repair refuses them, and an
    // index must distinguish a contradiction from a later absent repetition.
    Ok(canonical(nals))
}

pub(super) fn complete(init: &mut Init, opening: &[Vec<u8>]) -> Result<bool, Fmp4Error> {
    if opening.is_empty() {
        return Ok(false);
    }
    let Some(location) = locate_hvcc(&init.bytes)? else {
        return Ok(false);
    };
    if !is_dvh1(&init.bytes, &location) {
        return Ok(false);
    }
    let record = &init.bytes[location.payload.clone()];
    if hvcc_parameter_set_arrays_complete(record)? {
        return Ok(false);
    }
    // Validate every array and collect offsets before the first write.
    let mut offsets = Vec::new();
    let mut configured = Vec::new();
    let mut pos = 23;
    for _ in 0..record[22] {
        let head = record[pos];
        let kind = head & 0x3f;
        let parameter = matches!(kind, 32..=34);
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]);
        if parameter {
            if count == 0 || head & 0x40 != 0 {
                return malformed("dvh1 parameter array is empty or has a reserved flag");
            }
            offsets.push(location.payload.start + pos);
        }
        pos += 3;
        for _ in 0..count {
            let len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
            pos += 2;
            let nal = &record[pos..pos + len];
            if parameter {
                if hevc_nal_type(nal) != Some(kind) || nal[0] & 0x80 != 0 || nal[1] & 7 == 0 {
                    return malformed("dvh1 parameter NAL disagrees with its array header");
                }
                configured.push(nal.to_vec());
            }
            pos += len;
        }
    }
    if !(32..=34).all(|kind| {
        configured
            .iter()
            .any(|nal| hevc_nal_type(nal) == Some(kind))
    }) {
        // Empty hvcC follows the existing missing-array promotion path.
        return Ok(false);
    }
    let opening = canonical(opening.iter().cloned());
    if canonical(configured) != opening
        || !(32..=34).all(|kind| opening.iter().any(|nal| hevc_nal_type(nal) == Some(kind)))
    {
        return Err(Fmp4Error::Unsupported(
            "dvh1 completeness requires the same complete opening VPS/SPS/PPS tuple".into(),
        ));
    }
    for offset in offsets {
        init.bytes[offset] |= 0x80;
    }
    Ok(true)
}

/// Prove that removing in-band definitions from this rolling fragment loses
/// nothing. Unlike a sampled census, every NAL in every video sample is read
/// strictly before the caller can publish or buffer any of the fragment.
/// The caller keeps this same repaired init for the lifetime of the stream.
pub fn validate_rolling_dvh1_fragment(init: &Init, fragment: &Fragment) -> Result<(), Fmp4Error> {
    if init
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .count()
        != 1
    {
        return malformed("rolling dvh1 proof requires exactly one video track");
    }
    if validate_hevc_sample_entries(init)? != HevcSampleEntryLayout::Single {
        return malformed("rolling dvh1 proof requires one sample description");
    }
    let location = locate_hvcc(&init.bytes)?
        .ok_or_else(|| Fmp4Error::Malformed("rolling dvh1 proof has no hvcC".into()))?;
    if !is_dvh1(&init.bytes, &location) {
        return malformed("rolling dvh1 proof requires dvh1");
    }
    let configured = canonical(
        hvcc_parameter_set_nals(&init.bytes[location.payload])?
            .into_iter()
            .map(<[u8]>::to_vec),
    );
    let video = init
        .video()
        .ok_or_else(|| Fmp4Error::Malformed("rolling dvh1 proof has no video".into()))?;
    if fragment
        .tracks
        .iter()
        .any(|track| !init.tracks.iter().any(|known| known.id == track.track_id))
    {
        return malformed("rolling dvh1 fragment names an unknown track");
    }
    if fragment
        .tracks
        .iter()
        .any(|track| track.track_id == video.id)
    {
        validate_hevc_sample_description_reference(init, fragment)?;
    }
    for track in fragment
        .tracks
        .iter()
        .filter(|track| track.track_id == video.id)
    {
        for run in &track.runs {
            let mut start = run.data_offset;
            for sample in &run.samples {
                if start < fragment.mdat_payload.start {
                    return malformed("rolling dvh1 sample begins outside mdat");
                }
                let end = start
                    .checked_add(sample.size as usize)
                    .filter(|end| *end <= fragment.bytes.len() && *end <= fragment.mdat_payload.end)
                    .ok_or_else(|| {
                        Fmp4Error::Malformed("rolling dvh1 sample exceeds its fragment".into())
                    })?;
                let bytes = &fragment.bytes[start..end];
                if bytes.is_empty() {
                    return malformed("rolling dvh1 sample is empty");
                }
                let mut invalid = false;
                visit_length_prefixed_hevc_nals(bytes, video.nal_length_size, |framed, kind| {
                    let nal = &framed[usize::from(video.nal_length_size)..];
                    invalid |= nal.len() < 2
                        || nal[0] & 0x80 != 0
                        || nal[1] & 7 == 0
                        || (matches!(kind, Some(32..=34))
                            && !configured
                                .iter()
                                .any(|known| known.as_slice() == without_trailing_zero_bytes(nal)));
                })?;
                if invalid {
                    return malformed("rolling dvh1 sample contradicts its decoder configuration");
                }
                start = end;
            }
        }
    }
    Ok(())
}
