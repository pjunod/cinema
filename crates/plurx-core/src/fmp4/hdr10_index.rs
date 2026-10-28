//! Whole-title copy metadata and authoritative post-encode HDR10 declarations.
use super::*;

/// Binds both local and shared indexes to the full-sample metadata scan.
pub const HDR10_INDEX_METADATA_REVISION: u8 = 1;

#[derive(Default)]
pub struct Hdr10IndexMetadata {
    mastering: Option<Vec<u8>>,
    light: Option<[u16; 2]>,
}

impl Hdr10IndexMetadata {
    /// Inspect every sample, including non-random-access pictures. Samples are
    /// never rewritten; their per-sequence SEIs remain authoritative in-band.
    pub fn observe_fragment(&mut self, fragment: &Fragment, init: &Init) -> Result<(), Fmp4Error> {
        let Some(video) = init.video().filter(|v| v.codec == Some(VideoCodec::Hevc)) else {
            return Ok(());
        };
        let Some(track) = fragment.track(video.id) else {
            return Ok(());
        };
        for run in &track.runs {
            let mut offset = run.data_offset;
            for sample in &run.samples {
                let end = offset
                    .checked_add(sample.size as usize)
                    .ok_or_else(|| Fmp4Error::Malformed("HDR sample size overflow".into()))?;
                let bytes = fragment
                    .bytes
                    .get(offset..end)
                    .ok_or_else(|| Fmp4Error::Malformed("HDR sample outside fragment".into()))?;
                self.observe_sample(bytes, video.nal_length_size)?;
                offset = end;
            }
        }
        Ok(())
    }

    fn observe_sample(&mut self, bytes: &[u8], length_size: u8) -> Result<(), Fmp4Error> {
        let mut result = Ok(());
        visit_length_prefixed_hevc_nals(bytes, length_size, |framed, kind| {
            if result.is_ok() {
                match kind {
                    Some(39) => result = self.observe_nal(&framed[usize::from(length_size)..]),
                    None => result = malformed("truncated HEVC NAL header"),
                    _ => {}
                }
            }
        })?;
        result
    }

    fn observe_nal(&mut self, nal: &[u8]) -> Result<(), Fmp4Error> {
        for (kind, payload) in sei_messages(nal)? {
            match kind {
                137 => {
                    if payload.len() != 24 || self.mastering.as_ref().is_some_and(|v| v != &payload)
                    {
                        return malformed("mastering-display metadata changes within the title");
                    }
                    self.mastering = Some(payload);
                }
                144 => {
                    let bytes: [u8; 4] = payload.try_into().map_err(|_| {
                        Fmp4Error::Malformed("content-light metadata is not four bytes".into())
                    })?;
                    let next = [
                        u16::from_be_bytes([bytes[0], bytes[1]]),
                        u16::from_be_bytes([bytes[2], bytes[3]]),
                    ];
                    self.light = Some(match self.light {
                        None => next,
                        Some(old) => std::array::from_fn(|i| {
                            // Zero means unspecified, not a measured zero. A
                            // partly unknown film cannot claim a known maximum.
                            if old[i] == 0 || next[i] == 0 {
                                0
                            } else {
                                old[i].max(next[i])
                            }
                        }),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn promotion_nals(&self) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        if let Some(mastering) = &self.mastering {
            result.push(encode_sei(&[0x4e, 1], &[(137, mastering.clone())]));
        }
        if let Some(light) = self.light {
            let payload = light.into_iter().flat_map(u16::to_be_bytes).collect();
            result.push(encode_sei(&[0x4e, 1], &[(144, payload)]));
        }
        result
    }
}

fn sei_messages(nal: &[u8]) -> Result<Vec<(usize, Vec<u8>)>, Fmp4Error> {
    if nal.len() < 3 {
        return malformed("truncated prefix SEI");
    }
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in &nal[2..] {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    let mut pos = 0;
    let mut result = Vec::new();
    while pos < rbsp.len() {
        if rbsp[pos] == 0x80 && rbsp[pos + 1..].iter().all(|b| *b == 0) {
            return Ok(result);
        }
        let kind = read_sei_number(&rbsp, &mut pos)
            .ok_or_else(|| Fmp4Error::Malformed("invalid SEI type".into()))?;
        let size = read_sei_number(&rbsp, &mut pos)
            .ok_or_else(|| Fmp4Error::Malformed("invalid SEI size".into()))?;
        let end = pos
            .checked_add(size)
            .filter(|end| *end <= rbsp.len())
            .ok_or_else(|| Fmp4Error::Malformed("truncated SEI payload".into()))?;
        result.push((kind, rbsp[pos..end].to_vec()));
        pos = end;
    }
    malformed("SEI trailing bits missing")
}

// SEI fields are byte-summed numbers, not 16-bit lengths. Unrelated user-data
// messages can exceed 64 KiB and must not disqualify an otherwise valid title.
fn read_sei_number(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let mut result = 0usize;
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        result = result.checked_add(usize::from(byte))?;
        if byte != 255 {
            return Some(result);
        }
    }
}

fn encode_sei(header: &[u8], messages: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut rbsp = Vec::new();
    for (kind, payload) in messages {
        for mut value in [*kind, payload.len()] {
            while value >= 255 {
                rbsp.push(255);
                value -= 255;
            }
            rbsp.push(value as u8);
        }
        rbsp.extend_from_slice(payload);
    }
    rbsp.push(0x80);
    let mut nal = header.to_vec();
    let mut zeros = 0;
    for byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    nal
}

/// Canonicalize the HDR metadata proved by a complete index scan. Presence,
/// NAL grouping, and sample-entry box position are independent of the landing.
/// Conflicting mastering metadata is refused; unrelated metadata is retained.
/// The caller opts in through the persisted `indexed_hdr10_metadata` policy.
pub fn normalize_indexed_hdr10_metadata(
    init: &mut Init,
    inputs: &[Vec<u8>],
) -> Result<bool, Fmp4Error> {
    let mut metadata = Hdr10IndexMetadata::default();
    for nal in inputs {
        metadata.observe_nal(nal)?;
    }
    normalize_hdr10_metadata(init, &metadata, false)
}

fn normalize_hdr10_metadata(
    init: &mut Init,
    metadata: &Hdr10IndexMetadata,
    replace_mastering: bool,
) -> Result<bool, Fmp4Error> {
    let mut declarations: Vec<(usize, [u8; 4], Vec<u8>)> = Vec::new();
    if let Some(mastering) = &metadata.mastering {
        declarations.push((137, *b"mdcv", mastering.clone()));
    }
    if let Some(light) = metadata.light {
        declarations.push((
            144,
            *b"clli",
            light.into_iter().flat_map(u16::to_be_bytes).collect(),
        ));
    }
    if declarations.is_empty() {
        return Ok(false);
    }
    // Work on a candidate so a malformed box or conflicting mastering value
    // cannot partially mutate the caller's init before returning an error.
    let mut candidate = init.clone();
    let Some(location) = locate_hvcc(&candidate.bytes)? else {
        return Ok(false);
    };
    let record = &candidate.bytes[location.payload.clone()];
    hvcc_nal_arrays(record)?;
    let mut rewritten = record[..23].to_vec();
    rewritten[22] = 0;
    let mut pos = 23;
    for _ in 0..record[22] {
        let header = record[pos];
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]);
        pos += 3;
        let mut kept = Vec::new();
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([record[pos], record[pos + 1]]));
            pos += 2;
            let nal = &record[pos..pos + len];
            pos += len;
            let mut remaining = None;
            if header & 63 == 39 {
                let messages = sei_messages(nal)?;
                let mut retained = Vec::new();
                let mut removed = false;
                for (kind, payload) in messages {
                    if let Some((_, _, canonical)) =
                        declarations.iter().find(|(k, _, _)| *k == kind)
                    {
                        if (!replace_mastering && kind == 137 && payload != *canonical)
                            || payload.len() != canonical.len()
                        {
                            return malformed("muxer HDR metadata conflicts with the index");
                        }
                        removed = true;
                    } else {
                        retained.push((kind, payload));
                    }
                }
                if removed {
                    if retained.is_empty() {
                        continue;
                    }
                    remaining = Some(encode_sei(&nal[..2], &retained));
                }
            }
            kept.push(remaining.unwrap_or_else(|| nal.to_vec()));
        }
        // Strip an array emptied by removing the old declarations. All
        // canonical declarations go into one final, deterministic array.
        if !kept.is_empty() || header & 63 != 39 {
            append_array(&mut rewritten, header, &kept)?;
        }
    }
    append_array(&mut rewritten, 39, &metadata.promotion_nals())?;
    let old_len = location.payload.len();
    let new_len = rewritten.len();
    candidate.bytes.splice(location.payload, rewritten);
    for ancestor in location.ancestors {
        resize_box(&mut candidate.bytes, ancestor, old_len, new_len)?;
    }
    // Remove existing declarations, then write one canonical copy in a stable
    // position, including when the landing did not declare either box.
    let location = locate_hvcc(&candidate.bytes)?.expect("hvcC retained");
    let entry = location.ancestors[1];
    let children = entry.start + entry.header_len + 78..location.sample_entry_end;
    let mut removals = Vec::new();
    for (kind, box_kind, canonical) in &declarations {
        for (at, header) in find_children(&candidate.bytes, children.clone(), box_kind)? {
            let payload = &candidate.bytes[at.start + header.header_len..at.start + header.size];
            if payload.len() != canonical.len()
                || (!replace_mastering && *kind == 137 && payload != canonical)
            {
                return malformed("sample-entry HDR metadata conflicts with the index");
            }
            removals.push((at.start, header.size));
        }
    }
    removals.sort_unstable();
    for (start, len) in removals.into_iter().rev() {
        candidate.bytes.drain(start..start + len);
        for &ancestor in &location.ancestors[1..] {
            shrink_box(&mut candidate.bytes, ancestor, len)?;
        }
    }
    let location = locate_hvcc(&candidate.bytes)?.expect("hvcC retained");
    let mut boxes = Vec::new();
    for (_, kind, payload) in declarations {
        boxes.extend(mp4_box(&kind, &payload)?);
    }
    let delta = boxes.len();
    candidate
        .bytes
        .splice(location.sample_entry_end..location.sample_entry_end, boxes);
    for &ancestor in &location.ancestors[1..] {
        grow_box(&mut candidate.bytes, ancestor, delta)?;
    }
    let changed = candidate.bytes != init.bytes;
    *init = candidate;
    Ok(changed)
}

/// SDR must not inherit static HDR declarations from any encoder family.
/// Preserve other SEI (including captions) and all coded picture/audio bytes.
pub fn strip_sdr_hdr_init(init: &mut Init) -> Result<(), Fmp4Error> {
    let mut candidate = init.clone();
    let location = locate_hvcc(&candidate.bytes)?
        .ok_or_else(|| Fmp4Error::Malformed("SDR output has no hvcC".into()))?;
    let record = &candidate.bytes[location.payload.clone()];
    hvcc_nal_arrays(record)?;
    let mut rewritten = record[..23].to_vec();
    rewritten[22] = 0;
    let mut pos = 23;
    for _ in 0..record[22] {
        let header = record[pos];
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]);
        pos += 3;
        let mut kept = Vec::new();
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([record[pos], record[pos + 1]]));
            pos += 2;
            let nal = &record[pos..pos + len];
            pos += len;
            if let Some(nal) = strip_sdr_hdr_nal(nal)? {
                kept.push(nal);
            }
        }
        if !kept.is_empty() {
            append_array(&mut rewritten, header, &kept)?;
        }
    }
    let old = location.payload.len();
    let new = rewritten.len();
    candidate.bytes.splice(location.payload, rewritten);
    for ancestor in location.ancestors {
        resize_box(&mut candidate.bytes, ancestor, old, new)?;
    }
    let location = locate_hvcc(&candidate.bytes)?.expect("hvcC retained");
    let entry = location.ancestors[1];
    let children = entry.start + entry.header_len + 78..location.sample_entry_end;
    let mut removals = Vec::new();
    for kind in [b"mdcv", b"clli"] {
        for (at, header) in find_children(&candidate.bytes, children.clone(), kind)? {
            removals.push((at.start, header.size));
        }
    }
    removals.sort_unstable();
    for (start, len) in removals.into_iter().rev() {
        candidate.bytes.drain(start..start + len);
        for &ancestor in &location.ancestors[1..] {
            shrink_box(&mut candidate.bytes, ancestor, len)?;
        }
    }
    *init = candidate;
    Ok(())
}
fn strip_sdr_hdr_nal(nal: &[u8]) -> Result<Option<Vec<u8>>, Fmp4Error> {
    if nal.len() < 2 {
        return malformed("truncated SDR NAL");
    }
    if !matches!((nal[0] >> 1) & 63, 39 | 40) {
        return Ok(Some(nal.to_vec()));
    }
    let messages = sei_messages(nal)?;
    let kept: Vec<_> = messages
        .into_iter()
        .filter(|(kind, _)| !matches!(kind, 137 | 144 | 147))
        .collect();
    Ok((!kept.is_empty()).then(|| encode_sei(&nal[..2], &kept)))
}
pub fn strip_sdr_hdr_fragment(fragment: &mut Fragment, init: &Init) -> Result<(), Fmp4Error> {
    let video = init
        .video()
        .ok_or_else(|| Fmp4Error::Malformed("SDR has no video".into()))?;
    rewrite_video_samples(fragment, &init.tracks, video.id, |sample| {
        let mut output = Vec::new();
        let mut error = None;
        visit_length_prefixed_hevc_nals(sample, video.nal_length_size, |framed, _| {
            let size = usize::from(video.nal_length_size);
            match strip_sdr_hdr_nal(&framed[size..]) {
                Ok(Some(nal)) => {
                    let length = (nal.len() as u32).to_be_bytes();
                    output.extend_from_slice(&length[4 - size..]);
                    output.extend_from_slice(&nal);
                }
                Ok(None) => {}
                Err(err) => error = Some(err),
            }
        })?;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(output)
    })?;
    Ok(())
}

/// Static declarations for a completed processed HDR10 render. Coordinates are
/// R,G,B,white x/y pairs in ST 2086 units (1/50000); luminance minimum is in
/// 1/10000 cd/m². This deliberately has no inferred gamut or luminance defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hdr10OutputMetadata {
    mastering: [u8; 24],
    light: [u16; 2],
}

impl Hdr10OutputMetadata {
    pub fn new(
        primaries_rgbw: [u16; 8],
        max_nits: u32,
        min_1e4_nits: u32,
        max_cll: u16,
    ) -> Result<Self, Fmp4Error> {
        if primaries_rgbw.chunks_exact(2).any(|xy| {
            xy[0] > 50_000
                || xy[1] == 0
                || xy[1] > 50_000
                || u32::from(xy[0]) + u32::from(xy[1]) > 50_000
        }) || max_nits == 0
            || max_nits > 10_000
            || u64::from(min_1e4_nits) >= u64::from(max_nits) * 10_000
            || u32::from(max_cll) > max_nits
        {
            return malformed("invalid processed HDR10 mastering or content-light values");
        }
        // HEVC ST 2086 orders the display primaries G,B,R, then white.
        let mut mastering = Vec::with_capacity(24);
        for index in [2, 3, 4, 5, 0, 1, 6, 7] {
            mastering.extend_from_slice(&primaries_rgbw[index].to_be_bytes());
        }
        mastering.extend_from_slice(&(max_nits * 10_000).to_be_bytes());
        mastering.extend_from_slice(&min_1e4_nits.to_be_bytes());
        Ok(Self {
            mastering: mastering.try_into().expect("fixed ST 2086 payload"),
            // L1 avg_pq is not frame-average light. Zero explicitly means unknown.
            light: [max_cll, 0],
        })
    }

    fn metadata(self) -> Hdr10IndexMetadata {
        Hdr10IndexMetadata {
            mastering: Some(self.mastering.to_vec()),
            light: Some(self.light),
        }
    }

    /// Replace encoder hints, including hvcC SEI and mdcv/clli boxes. Indexed
    /// copies use their separate conflict-refusing policy above; processed
    /// output has an authoritative completed render policy instead.
    pub fn write_init(self, init: &mut Init) -> Result<bool, Fmp4Error> {
        let Some(video) = init.video() else {
            return malformed("processed HDR10 output has no video");
        };
        if video.codec != Some(VideoCodec::Hevc) || video.dolby_vision_config {
            return malformed("processed HDR10 output requires non-DV HEVC");
        }
        if locate_hvcc(&init.bytes)?.is_none() {
            return malformed("processed HDR10 output has no hvcC");
        }
        normalize_hdr10_metadata(init, &self.metadata(), true)
    }

    /// Every IRAP receives prefix SEI immediately before its first VCL NAL.
    /// Remove encoder MDCV/CLL everywhere (including suffix SEI), retaining
    /// unrelated messages and all coded pictures/audio. NAL type, rather than
    /// potentially broad muxer sync flags, identifies the IRAP pictures.
    pub fn write_fragment(self, fragment: &mut Fragment, init: &Init) -> Result<bool, Fmp4Error> {
        let video = init
            .video()
            .ok_or_else(|| Fmp4Error::Malformed("processed HDR10 output has no video".into()))?;
        if video.codec != Some(VideoCodec::Hevc) || video.dolby_vision_config {
            return malformed("processed HDR10 output requires non-DV HEVC");
        }
        rewrite_video_samples(fragment, &init.tracks, video.id, |sample| {
            self.write_sample(sample, video.nal_length_size)
        })
    }

    fn write_sample(self, sample: &[u8], length_size: u8) -> Result<Vec<u8>, Fmp4Error> {
        let mut nals = Vec::new();
        let mut invalid = false;
        let mut offset = 0;
        visit_length_prefixed_hevc_nals(sample, length_size, |framed, kind| {
            invalid |= kind.is_none();
            nals.push((
                kind,
                offset + usize::from(length_size)..offset + framed.len(),
            ));
            offset += framed.len();
        })?;
        if invalid {
            return malformed("truncated processed HEVC NAL header");
        }
        let first_vcl = nals
            .iter()
            .position(|(kind, _)| kind.is_some_and(|k| k < 32))
            .ok_or_else(|| Fmp4Error::Malformed("processed HEVC sample has no picture".into()))?;
        let irap = nals[first_vcl]
            .0
            .is_some_and(|kind| (16..=23).contains(&kind));
        let mut result = Vec::with_capacity(sample.len() + 80);
        for (index, (kind, range)) in nals.into_iter().enumerate() {
            let nal = &sample[range];
            if index == first_vcl && irap {
                // Use the picture's layer and temporal ID, changing only NAL type.
                let header = [(nal[0] & 0x81) | (39 << 1), nal[1]];
                let metadata = self.metadata();
                let messages = vec![
                    (137, metadata.mastering.expect("processed mastering")),
                    (
                        144,
                        self.light.into_iter().flat_map(u16::to_be_bytes).collect(),
                    ),
                ];
                append_framed_nal(&mut result, &encode_sei(&header, &messages), length_size)?;
            }
            if matches!(kind, Some(39 | 40)) {
                let messages = sei_messages(nal)?;
                if messages.iter().any(|(kind, _)| matches!(kind, 137 | 144)) {
                    let retained: Vec<_> = messages
                        .into_iter()
                        .filter(|(kind, _)| !matches!(kind, 137 | 144))
                        .collect();
                    if !retained.is_empty() {
                        append_framed_nal(
                            &mut result,
                            &encode_sei(&nal[..2], &retained),
                            length_size,
                        )?;
                    }
                    continue;
                }
            }
            append_framed_nal(&mut result, nal, length_size)?;
        }
        Ok(result)
    }
}

fn append_framed_nal(out: &mut Vec<u8>, nal: &[u8], length_size: u8) -> Result<(), Fmp4Error> {
    let len = u32::try_from(nal.len())
        .map_err(|_| Fmp4Error::Unsupported("processed HDR10 NAL exceeds u32".into()))?;
    if !(1..=4).contains(&length_size) || u64::from(len) >= (1u64 << (length_size * 8)) {
        return malformed("processed HDR10 NAL exceeds its length field");
    }
    out.extend_from_slice(&len.to_be_bytes()[4 - usize::from(length_size)..]);
    out.extend_from_slice(nal);
    Ok(())
}

fn append_array(record: &mut Vec<u8>, header: u8, nals: &[Vec<u8>]) -> Result<(), Fmp4Error> {
    record[22] = record[22]
        .checked_add(1)
        .ok_or_else(|| Fmp4Error::Unsupported("too many canonical hvcC arrays".into()))?;
    record.push(header);
    let count = u16::try_from(nals.len())
        .map_err(|_| Fmp4Error::Unsupported("too many canonical HDR NALs".into()))?;
    record.extend_from_slice(&count.to_be_bytes());
    for nal in nals {
        let len = u16::try_from(nal.len())
            .map_err(|_| Fmp4Error::Unsupported("canonical HDR SEI too large".into()))?;
        record.extend_from_slice(&len.to_be_bytes());
        record.extend_from_slice(nal);
    }
    Ok(())
}

fn resize_box(
    bytes: &mut [u8],
    at: BoxAt,
    old_len: usize,
    new_len: usize,
) -> Result<(), Fmp4Error> {
    if new_len > old_len {
        grow_box(bytes, at, new_len - old_len)
    } else if old_len > new_len {
        shrink_box(bytes, at, old_len - new_len)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P3: [u16; 8] = [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450];

    #[test]
    fn sdr_authoring_removes_hdr_declarations_without_changing_audio_pictures_or_timing() {
        use crate::transcode::dv_processing::{dv_author_hdr10_window, dv_author_sdr_window};
        let source = crate::testfixtures::pipe("closed-gop");
        let stale = dv_author_hdr10_window(
            &source,
            Hdr10OutputMetadata::new(P3, 1600, 50, 1600).expect("HDR metadata"),
        )
        .expect("stale encoder declarations");
        let output = dv_author_sdr_window(&stale).expect("SDR cleanup");
        assert_eq!(dv_author_sdr_window(&output).expect("idempotent"), output);
        let mut before = FragmentReader::new();
        before.push(&source);
        let mut after = FragmentReader::new();
        after.push(&output);
        let Some(Unit::Init(init)) = after.next_unit().expect("init") else {
            panic!("init");
        };
        let Some(Unit::Init(original)) = before.next_unit().expect("source init") else {
            panic!("init");
        };
        let location = locate_hvcc(&init.bytes).expect("config").expect("HEVC");
        let entry = location.ancestors[1];
        let children = entry.start + entry.header_len + 78..location.sample_entry_end;
        for kind in [b"mdcv", b"clli"] {
            assert!(find_children(&init.bytes, children.clone(), kind)
                .expect("boxes")
                .is_empty());
        }
        let mut video_samples = 0;
        let mut audio_samples = 0;
        while let Some(unit) = after.next_unit().expect("output unit") {
            let old = before
                .next_unit()
                .expect("source unit")
                .expect("matching unit");
            let (Unit::Fragment(fragment), Unit::Fragment(old)) = (unit, old) else {
                continue;
            };
            for track in &fragment.tracks {
                let original_track = old.track(track.track_id).expect("track");
                assert_eq!(track.base_decode_time, original_track.base_decode_time);
                for (run, original_run) in track.runs.iter().zip(&original_track.runs) {
                    assert_eq!(run.samples.len(), original_run.samples.len());
                    let (mut at, mut old_at) = (run.data_offset, original_run.data_offset);
                    for (sample, old_sample) in run.samples.iter().zip(&original_run.samples) {
                        assert_eq!(
                            (sample.duration, sample.cto, sample.flags),
                            (old_sample.duration, old_sample.cto, old_sample.flags)
                        );
                        let bytes = &fragment.bytes[at..at + sample.size as usize];
                        let old_bytes = &old.bytes[old_at..old_at + old_sample.size as usize];
                        if track.track_id == init.video().expect("video").id {
                            video_samples += 1;
                            let nals = length_prefixed_nals(
                                bytes,
                                init.video().expect("video").nal_length_size,
                            );
                            for nal in &nals {
                                if matches!(hevc_nal_type(nal), Some(39 | 40)) {
                                    assert!(!sei_messages(nal)
                                        .expect("SEI")
                                        .iter()
                                        .any(|(kind, _)| matches!(kind, 137 | 144 | 147)));
                                }
                            }
                            let pictures = |nals: Vec<&[u8]>| {
                                nals.into_iter()
                                    .filter(|nal| hevc_nal_type(nal).is_some_and(|kind| kind < 32))
                                    .map(<[u8]>::to_vec)
                                    .collect::<Vec<_>>()
                            };
                            assert_eq!(
                                pictures(nals),
                                pictures(length_prefixed_nals(
                                    old_bytes,
                                    original.video().expect("video").nal_length_size
                                ))
                            );
                        } else {
                            audio_samples += 1;
                            assert_eq!(bytes, old_bytes);
                        }
                        at += sample.size as usize;
                        old_at += old_sample.size as usize;
                    }
                }
            }
        }
        assert!(video_samples > 0 && audio_samples > 0);
        for header in [[0x4e, 1], [0x50, 1]] {
            let nal = encode_sei(
                &header,
                &[
                    (137, vec![0; 24]),
                    (144, vec![0; 4]),
                    (147, vec![16]),
                    (4, vec![9]),
                ],
            );
            assert_eq!(
                sei_messages(
                    &strip_sdr_hdr_nal(&nal)
                        .expect("strip")
                        .expect("caption retained")
                )
                .expect("SEI"),
                vec![(4, vec![9])]
            );
        }
    }
    #[test]
    fn processed_metadata_replaces_stale_sei_on_every_irap_and_preserves_other_nals() {
        let metadata = Hdr10OutputMetadata::new(P3, 1600, 17, 1600).expect("valid metadata");
        for length_size in [1, 2, 4] {
            for kind in [1, 16, 19, 20, 21, 23] {
                let unrelated = vec![0, 0, 1, 2, 3, 0, 0, 0];
                let prefix = encode_sei(&[0x4e, 1], &[(5, unrelated.clone()), (137, vec![0; 24])]);
                let suffix = encode_sei(&[0x50, 1], &[(144, vec![0xff; 4]), (4, vec![9])]);
                let picture = vec![kind << 1, 1, 0x80, 0x12, 0x34];
                let mut sample = Vec::new();
                for nal in [&prefix, &picture, &suffix] {
                    append_framed_nal(&mut sample, nal, length_size).expect("framed sample");
                }
                let output = metadata
                    .write_sample(&sample, length_size)
                    .expect("rewritten sample");
                let nals = length_prefixed_nals(&output, length_size);
                assert_eq!(
                    nals.iter()
                        .filter(|nal| hevc_nal_type(nal) == Some(kind))
                        .copied()
                        .collect::<Vec<_>>(),
                    [&picture[..]]
                );
                let messages: Vec<_> = nals
                    .iter()
                    .filter(|nal| matches!(hevc_nal_type(nal), Some(39 | 40)))
                    .flat_map(|nal| sei_messages(nal).expect("valid SEI"))
                    .collect();
                assert!(messages.contains(&(5, unrelated)));
                assert!(messages.contains(&(4, vec![9])));
                if kind >= 16 {
                    assert_eq!(
                        messages
                            .iter()
                            .filter(|(k, _)| *k == 137)
                            .collect::<Vec<_>>(),
                        [&(137, metadata.mastering.to_vec())]
                    );
                    assert!(messages.contains(&(144, vec![6, 64, 0, 0])));
                    let at = nals
                        .iter()
                        .position(|nal| hevc_nal_type(nal) == Some(kind))
                        .expect("picture");
                    assert_eq!(
                        hevc_nal_type(nals[at - 1]),
                        Some(39),
                        "IRAP preceded by prefix SEI"
                    );
                } else {
                    assert!(!messages.iter().any(|(k, _)| matches!(k, 137 | 144)));
                }
                assert_eq!(
                    metadata
                        .write_sample(&output, length_size)
                        .expect("repeat authoring"),
                    output
                );
            }
        }
        for invalid in [
            &[0, 0, 0][..],
            &[0, 0, 0, 2, 0x4e][..],
            &[0, 0, 0, 1, 0x4e][..],
        ] {
            assert!(metadata.write_sample(invalid, 4).is_err());
        }
    }

    #[test]
    fn processed_metadata_matches_init_irap_and_ffprobe_without_changing_audio_or_coded_pictures() {
        use crate::transcode::dv_processing::dv_author_hdr10_window;
        let source = crate::testfixtures::pipe("closed-gop");
        let old = Hdr10OutputMetadata::new(
            [35400, 14600, 8500, 39850, 6550, 2300, 15635, 16450],
            4000,
            50,
            4000,
        )
        .expect("old metadata");
        let stale = dv_author_hdr10_window(&source, old).expect("old encoder declarations");
        use crate::{scan::dv_title::*, transcode::dv_processing::*};
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 1,
            valid_l1_frames: 1,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        let DvDisplayTargetChoice::Target(target) =
            DvDisplayTarget::for_title(1600, Some(stats)).expect("title")
        else {
            panic!("target");
        };
        let target = target
            .resolve_source(Some(DvSourceMastering {
                primaries_rgbw: P3,
                min_1e4_nits: 17,
            }))
            .expect("one effective render target");
        assert_eq!(target.endpoint_args(), ["17", "4000"]);
        let metadata = target
            .output_metadata()
            .expect("metadata uses the resolved helper floor");
        let output = dv_author_hdr10_window(&stale, metadata).expect("post-encode metadata");
        assert_eq!(
            dv_author_hdr10_window(&output, metadata).expect("repeat authoring"),
            output
        );
        let mut before = FragmentReader::new();
        before.push(&source);
        let mut after = FragmentReader::new();
        after.push(&output);
        let Some(Unit::Init(init)) = after.next_unit().expect("output init") else {
            panic!("init");
        };
        let Some(Unit::Init(original_init)) = before.next_unit().expect("original init") else {
            panic!("init");
        };
        let location = locate_hvcc(&init.bytes).expect("hvcC").expect("HEVC");
        let entry = location.ancestors[1];
        let children = entry.start + entry.header_len + 78..location.sample_entry_end;
        let mut values = Vec::new();
        for kind in [b"mdcv", b"clli"] {
            let boxes = find_children(&init.bytes, children.clone(), kind).expect("metadata boxes");
            assert_eq!(boxes.len(), 1);
            let (at, header) = boxes[0];
            values.push(init.bytes[at.start + header.header_len..at.start + header.size].to_vec());
        }
        // Independently stated G,B,R,white and luminance wire values.
        let expected: Vec<u8> = [13250_u16, 34500, 7500, 3000, 34000, 16000, 15635, 16450]
            .into_iter()
            .flat_map(u16::to_be_bytes)
            .chain(16_000_000_u32.to_be_bytes())
            .chain(17_u32.to_be_bytes())
            .collect();
        assert_eq!(values, [expected.clone(), vec![6, 64, 0, 0]]);
        let record = &init.bytes[location.payload];
        let arrays = hvcc_nal_arrays(record).expect("hvcC arrays");
        assert!(arrays.0.contains(&39));
        let mut pos = 23;
        let mut configured = Hdr10IndexMetadata::default();
        for _ in 0..record[22] {
            let kind = record[pos] & 63;
            let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]);
            pos += 3;
            for _ in 0..count {
                let len = usize::from(u16::from_be_bytes([record[pos], record[pos + 1]]));
                pos += 2;
                if kind == 39 {
                    configured
                        .observe_nal(&record[pos..pos + len])
                        .expect("configured SEI");
                }
                pos += len;
            }
        }
        assert_eq!(configured.mastering, Some(expected.clone()));
        assert_eq!(configured.light, Some([1600, 0]));
        let video = init.video().expect("video");
        let mut iraps = 0;
        let mut audio = 0;
        while let Some(unit) = after.next_unit().expect("output fragment") {
            let original = before
                .next_unit()
                .expect("original fragment")
                .expect("matching unit");
            let (Unit::Fragment(fragment), Unit::Fragment(original)) = (unit, original) else {
                continue;
            };
            for track in &fragment.tracks {
                let old_track = original.track(track.track_id).expect("same track");
                assert_eq!(track.base_decode_time, old_track.base_decode_time);
                for (run, old_run) in track.runs.iter().zip(&old_track.runs) {
                    assert_eq!(run.samples.len(), old_run.samples.len());
                    let mut at = run.data_offset;
                    let mut old_at = old_run.data_offset;
                    for (sample, old_sample) in run.samples.iter().zip(&old_run.samples) {
                        assert_eq!(
                            (sample.duration, sample.cto, sample.flags),
                            (old_sample.duration, old_sample.cto, old_sample.flags)
                        );
                        let bytes = &fragment.bytes[at..at + sample.size as usize];
                        let old_bytes = &original.bytes[old_at..old_at + old_sample.size as usize];
                        if track.track_id == video.id {
                            let nals = length_prefixed_nals(bytes, video.nal_length_size);
                            let old_nals = length_prefixed_nals(
                                old_bytes,
                                original_init
                                    .video()
                                    .expect("original video")
                                    .nal_length_size,
                            );
                            let picture_nals = |nals: Vec<&[u8]>| {
                                nals.into_iter()
                                    .filter(|nal| hevc_nal_type(nal).is_some_and(|kind| kind < 32))
                                    .map(<[u8]>::to_vec)
                                    .collect::<Vec<_>>()
                            };
                            assert_eq!(picture_nals(nals.clone()), picture_nals(old_nals));
                            if nals.iter().any(|nal| {
                                hevc_nal_type(nal).is_some_and(|kind| (16..=23).contains(&kind))
                            }) {
                                iraps += 1;
                                let mut found = Hdr10IndexMetadata::default();
                                found
                                    .observe_sample(bytes, video.nal_length_size)
                                    .expect("IRAP SEI");
                                assert_eq!(found.mastering, Some(expected.clone()));
                                assert_eq!(found.light, Some([1600, 0]));
                            }
                        } else {
                            audio += 1;
                            assert_eq!(bytes, old_bytes, "audio payload");
                        }
                        at += sample.size as usize;
                        old_at += old_sample.size as usize;
                    }
                }
            }
        }
        assert!(
            iraps > 1 && audio > 0,
            "multiple IRAPs and audio actually checked"
        );
        assert!(after.saw_trailer() && after.buffered() == 0);
        let directory = tempfile::Builder::new()
            .prefix("processed-metadata-")
            .tempdir_in(crate::testfixtures::fixture_dir())
            .expect("fixture directory");
        let path = directory.path().join("output.mp4");
        std::fs::write(&path, output).expect("probe input");
        let probe = crate::testfixtures::run(
            std::process::Command::new(crate::testfixtures::ffprobe())
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-show_frames",
                    "-read_intervals",
                    "%+#1",
                    "-of",
                    "json",
                ])
                .arg(path),
        );
        let document: serde_json::Value = serde_json::from_slice(&probe).expect("ffprobe JSON");
        let side = document["frames"][0]["side_data_list"]
            .as_array()
            .expect("decoded frame side data");
        let mastering = side
            .iter()
            .find(|row| row["side_data_type"] == "Mastering display metadata")
            .expect("decoded MDCV");
        for (field, numerator, scale) in [
            ("green_x", 13250, 50000),
            ("green_y", 34500, 50000),
            ("blue_x", 7500, 50000),
            ("blue_y", 3000, 50000),
            ("red_x", 34000, 50000),
            ("red_y", 16000, 50000),
            ("white_point_x", 15635, 50000),
            ("white_point_y", 16450, 50000),
            ("max_luminance", 16000000, 10000),
            ("min_luminance", 17, 10000),
        ] {
            let (n, d) = mastering[field]
                .as_str()
                .expect("rational field")
                .split_once('/')
                .expect("rational");
            assert_eq!(
                n.parse::<u64>().expect("numerator") * scale,
                d.parse::<u64>().expect("denominator") * numerator,
                "{field}"
            );
        }
        let light = side
            .iter()
            .find(|row| row["side_data_type"] == "Content light level metadata")
            .expect("decoded CLL");
        assert_eq!(light["max_content"], 1600);
        assert_eq!(light["max_average"], 0);
    }

    fn light(max: u16, average: u16) -> Vec<u8> {
        encode_sei(
            &[0x4e, 1],
            &[(
                144,
                [max, average]
                    .into_iter()
                    .flat_map(u16::to_be_bytes)
                    .collect(),
            )],
        )
    }

    #[test]
    fn title_light_metadata_merges_maxima_without_losing_unknown_or_mastering_conflicts() {
        let mut meta = Hdr10IndexMetadata::default();
        for (max, average) in [(230, 197), (283, 200), (281, 139)] {
            meta.observe_nal(&light(max, average))
                .expect("valid HDR fixture");
        }
        assert_eq!(meta.light, Some([283, 200]));
        meta.observe_nal(&light(0, 150)).expect("valid HDR fixture");
        meta.observe_nal(&light(1000, 190))
            .expect("valid HDR fixture");
        assert_eq!(
            meta.light,
            Some([0, 200]),
            "unspecified is not a measured maximum"
        );
        let mastering = encode_sei(&[0x4e, 1], &[(137, vec![0; 24])]);
        meta.observe_nal(&mastering).expect("valid HDR fixture");
        meta.observe_nal(&mastering).expect("valid HDR fixture");
        assert!(meta
            .observe_nal(&encode_sei(&[0x4e, 1], &[(137, vec![1; 24])]))
            .is_err());
        assert!(meta
            .observe_nal(&encode_sei(&[0x4e, 1], &[(144, vec![0; 3])]))
            .is_err());
    }

    #[test]
    fn title_light_metadata_rejects_truncated_sample_framing() {
        let nal = light(230, 197);
        let mut framed = (nal.len() as u32).to_be_bytes().to_vec();
        framed.extend(nal);
        let mut metadata = Hdr10IndexMetadata::default();
        metadata
            .observe_sample(&framed, 4)
            .expect("valid HDR fixture");
        framed.extend([0, 0, 0, 10, 0x4e, 1]);
        assert!(metadata.observe_sample(&framed, 4).is_err());
        assert!(metadata.observe_sample(&[0, 0, 0], 4).is_err());
    }

    #[test]
    fn title_light_metadata_ignores_large_unrelated_sei_messages() {
        let nal = encode_sei(
            &[0x4e, 1],
            &[(5, vec![7; 70_000]), (144, vec![1, 27, 0, 200])],
        );
        let mut metadata = Hdr10IndexMetadata::default();
        metadata.observe_nal(&nal).expect("valid HDR fixture");
        assert_eq!(metadata.light, Some([283, 200]));
    }

    #[test]
    fn title_light_metadata_refuses_muxer_mastering_conflict_without_mutating_init() {
        let mut reader = FragmentReader::new();
        reader.push(&crate::testfixtures::pipe("closed-gop"));
        let Some(Unit::Init(mut init)) = reader.next_unit().expect("fixture init") else {
            panic!("fixture init");
        };
        let original = encode_sei(&[0x4e, 1], &[(137, vec![1; 24])]);
        promote_hdr10_static_metadata_from(&mut init, &[original]).expect("mastering init");
        let before = init.bytes.clone();
        let conflicting = encode_sei(&[0x4e, 1], &[(137, vec![2; 24])]);
        assert!(normalize_indexed_hdr10_metadata(&mut init, &[conflicting]).is_err());
        assert_eq!(init.bytes, before);
    }

    #[test]
    fn title_light_metadata_makes_different_landing_inits_identical() {
        let mut reader = FragmentReader::new();
        reader.push(&crate::testfixtures::pipe("closed-gop"));
        let Some(Unit::Init(muxer)) = reader.next_unit().expect("valid HDR fixture") else {
            panic!("init");
        };
        let mut early = muxer.clone();
        let mut late = muxer;
        promote_hdr10_static_metadata_from(&mut early, &[light(230, 197)])
            .expect("valid HDR fixture");
        promote_hdr10_static_metadata_from(&mut late, &[light(283, 200)])
            .expect("valid HDR fixture");
        assert_ne!(early.bytes, late.bytes);
        let mut meta = Hdr10IndexMetadata::default();
        meta.observe_nal(&light(230, 197))
            .expect("valid HDR fixture");
        meta.observe_nal(&light(283, 200))
            .expect("valid HDR fixture");
        let inputs = PromotionInputs {
            indexed_hdr10_metadata: true,
            hdr10_sei: meta.promotion_nals(),
            ..Default::default()
        };
        promote_from(&mut early, &inputs).expect("valid HDR fixture");
        promote_from(&mut late, &inputs).expect("valid HDR fixture");
        assert_eq!(
            early.bytes, late.bytes,
            "hvcC and clli both use the title maxima"
        );
        let same = early.bytes.clone();
        promote_from(&mut early, &inputs).expect("valid HDR fixture");
        assert_eq!(early.bytes, same, "canonical promotion is idempotent");
        let location = locate_hvcc(&early.bytes)
            .expect("valid HDR fixture")
            .expect("valid HDR fixture");
        let entry = location.ancestors[1];
        let (at, header) = find_child(
            &early.bytes,
            entry.start + entry.header_len + 78..location.sample_entry_end,
            b"clli",
        )
        .expect("valid HDR fixture")
        .expect("valid HDR fixture");
        assert_eq!(
            &early.bytes[at.start + header.header_len..at.start + header.size],
            &[1, 27, 0, 200]
        );
    }
}
