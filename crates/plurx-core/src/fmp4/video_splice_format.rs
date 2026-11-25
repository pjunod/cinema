//! A video init and in-band configuration for one native HLS timeline.

use std::collections::BTreeMap;

use super::video_splice::{
    author_video_fragment, copy_picture_schedule, retime_video_fragment, unsupported,
    CopyPictureTime, VideoSpliceClock,
};
use super::{
    be_u32, classify, copy_box_end, copy_video_sample_entry_facts,
    declare_hevc_parameter_sets_in_band, dolby_vision_record, find_child, has_leading_picture,
    hevc_hdr10_sample_entry_facts, hevc_nal_type, hvcc_parameter_set_nals, locate_configuring_hvcc,
    malformed, sei_number, shrink_box, validate_hevc_sample_description_reference,
    visit_length_prefixed_hevc_nals, without_trailing_zero_bytes, Fmp4Error, Fragment,
    HvcCLocation, Init, Sample, Track, VideoCodec,
};

/// One fixed clear Main10 Profile 8.1 video initialization. Every new
/// coded video sequence repeats its producer's original parameter sets.
pub struct SpliceVideoFormat {
    init: Init,
    static_metadata: BTreeMap<u16, Vec<u8>>,
}

/// A producer's original configuration. The caller binds it to that
/// producer's verified output, including each picture's source identity.
pub struct SpliceVideoRendition<'a> {
    producer: &'a Init,
    format: &'a SpliceVideoFormat,
    parameter_sets: [&'a [u8]; 3],
    slice_pps_id: u32,
    copy_static: Option<&'a CopyStaticMetadata>,
}

/// The HDR10 static SEI values one ordinary copy segment carries in band.
/// A replacement for that segment must carry the same values or none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyStaticMetadata(BTreeMap<u16, Vec<u8>>);

impl CopyStaticMetadata {
    /// Read every sample of `track` in `fragment`. A segment whose own copy
    /// changes a value is refused, since no replacement could match it.
    pub fn of(fragment: &Fragment, track: &Track) -> Result<Self, Fmp4Error> {
        let mut values = BTreeMap::<u16, Vec<u8>>::new();
        for (_, bytes) in fragment.samples_with_bytes(track.id) {
            let bytes = bytes.ok_or_else(|| unsupported("copy sample lies outside its mdat"))?;
            let mut outcome = Ok(());
            visit_length_prefixed_hevc_nals(bytes, track.nal_length_size, |framed, kind| {
                if outcome.is_err() || !matches!(kind, Some(39 | 40)) {
                    return;
                }
                let nal = &framed[usize::from(track.nal_length_size)..];
                let parsed = parse_sei(nal, |kind, data| {
                    if static_metadata_size(kind).is_none() {
                        return Ok(true);
                    }
                    match values.get(&kind) {
                        Some(previous) if previous != data => {
                            Err(unsupported("copy segment changes HDR10 static metadata"))
                        }
                        Some(_) => Ok(true),
                        None => {
                            values.insert(kind, data.to_vec());
                            Ok(true)
                        }
                    }
                });
                if let Err(error) = parsed {
                    outcome = Err(error);
                }
            })?;
            outcome?;
        }
        Ok(Self(values))
    }
}

/// Random-access requirements for the fragment's place in the rendition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpliceBoundary {
    Fragment,
    SegmentStart,
    ProducerChange,
}

impl SpliceVideoFormat {
    /// Pin the public sample entry before exposing any segments. The ordinary
    /// rendition supplies its HEVC envelope and existing Dolby signaling.
    pub fn new(ordinary: &Init, timescale: u32) -> Result<Self, Fmp4Error> {
        let (parameter_sets, location) = splice_configuration(ordinary)?;
        check_parameter_links(parameter_sets)?;
        if timescale == 0 {
            return malformed("splice output clock is zero");
        }

        let static_metadata = source_static_metadata(ordinary, &location)?;
        let mut init = ordinary.clone();
        declare_hevc_parameter_sets_in_band(&mut init)?;
        require_plain_hls_brand(&init)?;
        let arrays = location.payload.start + 22;
        let static_sei = retained_static_sei(&init.bytes[location.payload.clone()])?;
        let removed = location.payload.end - arrays - static_sei.len();
        init.bytes.splice(arrays..location.payload.end, static_sei);
        for at in location.ancestors.iter().copied() {
            shrink_box(&mut init.bytes, at, removed)?;
        }
        // Unknown segmentation, parallelism, and average/constant frame
        // rate apply to the public envelope. Keep the length and layer bits.
        let header = &mut init.bytes[location.payload.start..arrays];
        header[13..16].copy_from_slice(&[0xf0, 0, 0xfc]);
        header[19..21].fill(0);
        header[21] &= 0x3f;

        let mdia = location.ancestors[5];
        let (mdhd, header) = find_child(
            &init.bytes,
            mdia.start + mdia.header_len..copy_box_end(&init.bytes, mdia)?,
            b"mdhd",
        )?
        .ok_or_else(|| unsupported("splice init needs an mdhd"))?;
        let mdhd_start = mdhd.start + mdhd.header_len;
        let mdhd_body = &mut init.bytes[mdhd_start..mdhd.start + header.size];
        let (clock_at, duration) = match mdhd_body {
            [0, 0, 0, 0, ..] if mdhd_body.len() == 24 => (12, &mdhd_body[16..20]),
            [1, 0, 0, 0, ..] if mdhd_body.len() == 36 => (20, &mdhd_body[24..32]),
            _ => return malformed("invalid splice mdhd"),
        };
        if duration.iter().any(|byte| *byte != 0) {
            return Err(unsupported("splice init must declare zero media duration"));
        }
        mdhd_body[clock_at..clock_at + 4].copy_from_slice(&timescale.to_be_bytes());
        init.tracks[0].timescale = timescale;
        Ok(Self {
            init,
            static_metadata,
        })
    }

    pub fn init(&self) -> &Init {
        &self.init
    }

    /// Returning to the ordinary encoder needs an IDR and no leading
    /// presentation pictures, including the leading pictures IDR_W_RADL allows.
    pub fn admits_ordinary_return(&self, fragment: &Fragment) -> bool {
        classify(fragment, &self.init) == super::CutClass::CleanIdr
            && self
                .init
                .video()
                .and_then(|track| fragment.track(track.id))
                .is_some_and(|track| !has_leading_picture(track))
    }

    /// Establish that both renditions can declare the same sample entry.
    pub fn admit<'a>(&'a self, producer: &'a Init) -> Result<SpliceVideoRendition<'a>, Fmp4Error> {
        let (parameter_sets, location) = splice_configuration(producer)?;
        let public = locate_configuring_hvcc(&self.init.bytes)?
            .ok_or_else(|| unsupported("splice output has no HEVC configuration"))?;
        let source_config = &producer.bytes[location.payload.clone()];
        let output_config = &self.init.bytes[public.payload.clone()];
        // The init must never claim constraints that the new stream lacks.
        // A producer may signal more restrictive capabilities in its own SPS.
        if source_config[1] != output_config[1]
            || output_config[2..12]
                .iter()
                .zip(&source_config[2..12])
                .any(|(output, source)| output & !source != 0)
            || source_config[12] > output_config[12]
            || source_config[21] & 0x3f != output_config[21]
            || hevc_hdr10_sample_entry_facts(producer)?
                != hevc_hdr10_sample_entry_facts(&self.init)?
            || dolby_vision_record(producer)? != dolby_vision_record(&self.init)?
        {
            return Err(unsupported(
                "splice producer changes the published video format",
            ));
        }
        if source_static_metadata(producer, &location)?
            .iter()
            .any(|(kind, data)| self.static_metadata.get(kind) != Some(data))
        {
            return Err(unsupported("splice producer changes HDR10 static metadata"));
        }
        let slice_pps_id = check_parameter_links(parameter_sets)?;
        Ok(SpliceVideoRendition {
            slice_pps_id,
            format: self,
            producer,
            parameter_sets,
            copy_static: None,
        })
    }
}

impl<'a> SpliceVideoRendition<'a> {
    /// Bind a replacement producer to the copy segment it replaces. Its
    /// in-band HDR10 static SEI must then repeat that copy's values.
    pub fn replacing(self, copy_static: &'a CopyStaticMetadata) -> Self {
        Self {
            copy_static: Some(copy_static),
            ..self
        }
    }

    /// Author one fragment. The publisher proves source and clock continuity
    /// across fragments and identifies the producer change before publication.
    pub fn fragment(
        &self,
        input: &Fragment,
        clock: VideoSpliceClock,
        sequence: u32,
        boundary: SpliceBoundary,
    ) -> Result<Fragment, Fmp4Error> {
        let source = &self.producer.tracks[0];
        let output = &self.format.init.tracks[0];
        validate_hevc_sample_description_reference(self.producer, input)?;
        let result = retime_video_fragment(
            input,
            source,
            output,
            clock,
            sequence,
            |bytes, sample, first| {
                self.verify_sample(
                    bytes,
                    sample,
                    if first {
                        boundary
                    } else {
                        SpliceBoundary::Fragment
                    },
                )
            },
        )?;
        self.validate_authored(result, boundary)
    }

    /// Author a witnessed FEL window on the exact copy presentation times.
    /// A different encoder may reorder the same pictures and use a different
    /// clock or cadence rounding.
    pub fn fragment_at_copy_times(
        &self,
        input: &Fragment,
        copy_times: &[CopyPictureTime],
        copy_timescale: u32,
        decode_start: u64,
        sequence: u32,
        boundary: SpliceBoundary,
    ) -> Result<Fragment, Fmp4Error> {
        if self.copy_static.is_none() {
            return Err(unsupported(
                "a replacement is not bound to the copy segment it replaces",
            ));
        }
        let source = &self.producer.tracks[0];
        let output = &self.format.init.tracks[0];
        validate_hevc_sample_description_reference(self.producer, input)?;
        let mut schedule =
            copy_picture_schedule(input, source, output, copy_times, copy_timescale)?;
        let result = author_video_fragment(
            input,
            source,
            output,
            decode_start,
            sequence,
            |pts, _| {
                schedule.remove(&pts).ok_or_else(|| {
                    unsupported("replacement picture is absent from the copy schedule")
                })
            },
            |bytes, sample, first| {
                self.verify_sample(
                    bytes,
                    sample,
                    if first {
                        boundary
                    } else {
                        SpliceBoundary::Fragment
                    },
                )
            },
        )?;
        if !schedule.is_empty() {
            return Err(unsupported("replacement omitted a witnessed copy picture"));
        }
        self.validate_authored(result, boundary)
    }

    fn validate_authored(
        &self,
        result: Fragment,
        boundary: SpliceBoundary,
    ) -> Result<Fragment, Fmp4Error> {
        validate_hevc_sample_description_reference(&self.format.init, &result)?;
        if boundary != SpliceBoundary::Fragment && !classify(&result, &self.format.init).is_clean()
        {
            return Err(unsupported(
                "splice segment opens on a picture with leading frames",
            ));
        }
        Ok(result)
    }

    /// A Dolby Vision copy's init declares no HDR10 static metadata, because
    /// its promotion is withheld from a Dolby Vision sample entry. The source's
    /// own mastering-display and light-level SEI then travel only in band.
    /// The ordinary copy keeps them exactly as the muxed layout serves them.
    /// A replacement must repeat the values of the copy it replaces, so the
    /// published rendition never changes its static metadata.
    fn static_sei_admitted(&self, kind: u16, data: &[u8]) -> bool {
        if static_metadata_size(kind).is_none() {
            return true;
        }
        match self.format.static_metadata.get(&kind) {
            Some(declared) => declared == data,
            None => self
                .copy_static
                .is_none_or(|copy| copy.0.get(&kind).is_some_and(|value| value == data)),
        }
    }

    fn verify_sample(
        &self,
        bytes: &[u8],
        sample: &Sample,
        boundary: SpliceBoundary,
    ) -> Result<Option<Vec<u8>>, Fmp4Error> {
        let mut first_vcl = None;
        // The first check this sample fails, named for the decline log.
        let mut failure = None;
        let mut ordered_sample = Vec::new();
        let mut aud_len = 0;
        let seed = sample.is_sync();
        visit_length_prefixed_hevc_nals(bytes, 4, |framed, kind| {
            let nal = &framed[4..];
            if nal.len() < 3 || nal[0] & 0x81 != 0 || nal[1] & 0xf8 != 0 || nal[1] & 7 == 0 {
                failure.get_or_insert("splice sample has a NAL outside the base layer");
                return;
            }
            match kind.expect("NAL with two header bytes") {
                kind @ 0..=31 => {
                    let irap = (16..=23).contains(&kind);
                    let mut bits = SliceBits::new(&nal[2..]);
                    let first = bits.read(1) == Some(1);
                    if irap {
                        bits.read(1);
                    }
                    if bits.ue() != Some(self.slice_pps_id) {
                        failure.get_or_insert("splice slice refers to another PPS");
                    } else if first != first_vcl.is_none()
                        || first_vcl.is_some_and(|prior| prior != (kind, nal[1] & 7))
                    {
                        failure.get_or_insert("splice sample's slices do not form one picture");
                    }
                    first_vcl.get_or_insert((kind, nal[1] & 7));
                }
                kind @ 32..=34 => {
                    let expected = self.parameter_sets[usize::from(kind - 32)];
                    if without_trailing_zero_bytes(nal) != without_trailing_zero_bytes(expected) {
                        failure.get_or_insert("splice sample repeats different parameter sets");
                    } else if first_vcl.is_some() {
                        failure
                            .get_or_insert("splice sample sends a parameter set after its picture");
                    }
                    if seed {
                        return;
                    }
                }
                35 if seed => {
                    if !std::ptr::eq(framed.as_ptr(), bytes.as_ptr()) || aud_len != 0 {
                        failure
                            .get_or_insert("splice sample has a misplaced access unit delimiter");
                    }
                    aud_len = framed.len();
                    return;
                }
                // EOB cannot end a continuing rendition.
                37 => {
                    failure.get_or_insert("splice sample ends the bitstream");
                }
                39 | 40 => {
                    let mut changed = false;
                    let parsed = parse_sei(nal, |kind, data| {
                        changed |= !self.static_sei_admitted(kind, data);
                        Ok(true)
                    });
                    match parsed {
                        _ if changed => {
                            failure.get_or_insert("splice sample changes HDR10 static metadata");
                        }
                        Ok(true) => {}
                        Ok(false) | Err(_) => {
                            failure.get_or_insert("splice sample has an unreadable SEI");
                        }
                    }
                }
                _ => {}
            }
            if seed {
                ordered_sample.extend_from_slice(framed);
            }
        })?;
        if let Some(failure) = failure {
            return Err(unsupported(failure));
        }
        if first_vcl.is_none() {
            return Err(unsupported("splice sample has no picture"));
        }
        if boundary == SpliceBoundary::ProducerChange
            && (!seed || !matches!(first_vcl, Some((19 | 20, 1))))
        {
            return Err(unsupported("splice producer change needs a layer-zero IDR"));
        }
        if boundary == SpliceBoundary::SegmentStart && !seed {
            return Err(unsupported(
                "splice segment does not begin at a sync sample",
            ));
        }
        if !seed {
            return Ok(None);
        }
        if !matches!(first_vcl, Some((16..=21, 1))) {
            return Err(unsupported("a splice sync sample is not a layer-zero IRAP"));
        }
        let mut packet = Vec::new();
        packet.extend_from_slice(&bytes[..aud_len]);
        for set in self.parameter_sets {
            packet.extend_from_slice(&(set.len() as u32).to_be_bytes());
            packet.extend_from_slice(set);
        }
        packet.extend_from_slice(&ordered_sample);
        Ok(Some(packet))
    }
}

fn splice_configuration(init: &Init) -> Result<([&[u8]; 3], HvcCLocation), Fmp4Error> {
    let [track] = init.tracks.as_slice() else {
        return Err(unsupported("splice init requires a single video track"));
    };
    if track.codec != Some(VideoCodec::Hevc) || track.timescale == 0 || track.nal_length_size != 4 {
        return Err(unsupported(
            "splice requires an HEVC video clock and four-byte NAL lengths",
        ));
    }
    let facts = copy_video_sample_entry_facts(init)?;
    if !matches!(&facts.sample_entry, b"hvc1" | b"hev1") {
        return Err(unsupported(
            "splice needs a clear HEVC base-layer sample entry",
        ));
    }
    hevc_hdr10_sample_entry_facts(init)?;
    if !dolby_vision_record(init)?.is_some_and(|record| {
        record.profile == 8
            && record.bl_signal_compatibility_id == 1
            && record.bl_present
            && record.rpu_present
            && !record.el_present
    }) {
        return Err(unsupported(
            "splice requires single-layer Dolby Vision Profile 8.1",
        ));
    }
    let location = locate_configuring_hvcc(&init.bytes)?
        .ok_or_else(|| unsupported("splice needs an HEVC decoder configuration"))?;
    require_square_pixels(init, &location)?;
    require_one_default_description(init, track.id)?;
    let record = &init.bytes[location.payload.clone()];
    let mut nals = hvcc_parameter_set_nals(record)?;
    nals.sort_by_key(|nal| hevc_nal_type(nal));
    let [vps, sps, pps] = nals.as_slice() else {
        return Err(unsupported("splice requires exactly one VPS, SPS and PPS"));
    };
    if ![vps, sps, pps]
        .into_iter()
        .zip([32, 33, 34])
        .all(|(nal, kind)| hevc_nal_type(nal) == Some(kind) && nal[0] & 0x81 == 0 && nal[1] == 1)
    {
        return Err(unsupported("splice requires layer-zero parameter sets"));
    }
    Ok(([*vps, *sps, *pps], location))
}

fn require_square_pixels(init: &Init, location: &HvcCLocation) -> Result<(), Fmp4Error> {
    let entry = location.ancestors[1];
    let bounds = entry.start + entry.header_len + 78..location.sample_entry_end;
    let Some((at, header)) = find_child(&init.bytes, bounds, b"pasp")? else {
        return Ok(());
    };
    if header.size != at.header_len + 8 {
        return malformed("invalid splice pixel aspect ratio");
    }
    let payload = at.start + at.header_len;
    let h = be_u32(&init.bytes, payload);
    let v = be_u32(&init.bytes, payload + 4);
    if h == 0 || h != v {
        return Err(unsupported("splice requires square pixels"));
    }
    Ok(())
}

fn require_one_default_description(init: &Init, track_id: u32) -> Result<(), Fmp4Error> {
    let nested = |range, name| {
        let (at, header) = find_child(&init.bytes, range, name)?
            .ok_or_else(|| unsupported("splice init is missing its track defaults"))?;
        Ok::<_, Fmp4Error>(at.start + at.header_len..at.start + header.size)
    };
    let moov = nested(0..init.bytes.len(), b"moov")?;
    let mvex = nested(moov, b"mvex")?;
    let boxes = super::children(&init.bytes[mvex.clone()])?;
    let [(trex, start, end)] = boxes.as_slice() else {
        return Err(unsupported("splice init needs one trex"));
    };
    let data = &init.bytes[mvex.start + *start..mvex.start + *end];
    if trex.kind() != b"trex"
        || data.len() != 24
        || be_u32(data, 0) != 0
        || be_u32(data, 4) != track_id
        || be_u32(data, 8) != 1
        || be_u32(data, 12) != 0
    {
        return Err(unsupported(
            "splice track defaults must select one sample entry without a duration",
        ));
    }
    Ok(())
}

fn require_plain_hls_brand(init: &Init) -> Result<(), Fmp4Error> {
    let Some((at, header)) = find_child(&init.bytes, 0..init.bytes.len(), b"ftyp")? else {
        return Err(unsupported("splice init has no ftyp"));
    };
    let data = &init.bytes[at.start + at.header_len..at.start + header.size];
    if data.len() < 12 || !data.len().is_multiple_of(4) {
        return malformed("invalid splice ftyp");
    }
    let mut brands = std::iter::once(&data[..4]).chain(data[8..].chunks_exact(4));
    if !brands.clone().any(|brand| brand == b"iso6")
        || brands.any(|brand| !matches!(brand, b"iso5" | b"iso6" | b"dby1" | b"mp41"))
    {
        return Err(unsupported(
            "splice requires generic iso6-compatible fMP4 brands",
        ));
    }
    Ok(())
}

fn hvcc_arrays(record: &[u8]) -> impl Iterator<Item = (u8, &[u8], &[u8])> {
    // The caller has structurally validated the complete hvcC.
    let mut pos = 23;
    let mut array_type = 0;
    let mut remaining = 0;
    std::iter::from_fn(move || {
        while remaining == 0 {
            if pos == record.len() {
                return None;
            }
            array_type = record[pos] & 63;
            remaining = u16::from_be_bytes([record[pos + 1], record[pos + 2]]);
            pos += 3;
        }
        let length_at = pos;
        let len = usize::from(u16::from_be_bytes([record[pos], record[pos + 1]]));
        pos += 2 + len;
        remaining -= 1;
        Some((
            array_type,
            &record[length_at + 2..pos],
            &record[length_at..pos],
        ))
    })
}

fn retained_static_sei(record: &[u8]) -> Result<Vec<u8>, Fmp4Error> {
    let mut preserved = Vec::new();
    for (kind, _, framed) in hvcc_arrays(record) {
        if kind == 39 {
            preserved.push(framed);
        }
    }

    let count = u16::try_from(preserved.len())
        .map_err(|_| unsupported("splice init carries too many SEIs"))?;
    let mut arrays = vec![u8::from(count != 0)];
    if count != 0 {
        arrays.push(39);
        arrays.extend_from_slice(&count.to_be_bytes());
        for nal in preserved {
            arrays.extend_from_slice(nal);
        }
    }
    Ok(arrays)
}

fn source_static_metadata(
    init: &Init,
    location: &HvcCLocation,
) -> Result<BTreeMap<u16, Vec<u8>>, Fmp4Error> {
    let mut metadata = BTreeMap::<u16, Vec<u8>>::new();
    let mut record = |kind, data: &[u8]| {
        if static_metadata_size(kind) != Some(data.len()) {
            return malformed("invalid splice HDR10 static metadata");
        }
        if metadata.get(&kind).is_some_and(|previous| previous != data) {
            return Err(unsupported(
                "splice init contradicts its own HDR10 declaration",
            ));
        }
        metadata.entry(kind).or_insert_with(|| data.to_vec());
        Ok(true)
    };
    for (kind, nal, _) in hvcc_arrays(&init.bytes[location.payload.clone()]) {
        match kind {
            32..=34 => {}
            39 if parse_sei(nal, &mut record)? => {}
            _ => {
                return Err(unsupported(
                    "splice init has unsupported out-of-band HEVC metadata",
                ))
            }
        }
    }

    let entry = location.ancestors[1];
    let bounds = entry.start + entry.header_len + 78..location.sample_entry_end;
    for (name, kind) in [(b"mdcv", 137), (b"clli", 144)] {
        if let Some((at, header)) = find_child(&init.bytes, bounds.clone(), name)? {
            let data = &init.bytes[at.start + at.header_len..at.start + header.size];
            record(kind, data)?;
        }
    }
    Ok(metadata)
}

fn static_metadata_size(kind: u16) -> Option<usize> {
    match kind {
        137 => Some(24),
        144 => Some(4),
        _ => None,
    }
}

fn parse_sei(
    nal: &[u8],
    mut check: impl FnMut(u16, &[u8]) -> Result<bool, Fmp4Error>,
) -> Result<bool, Fmp4Error> {
    if !matches!(hevc_nal_type(nal), Some(39 | 40)) {
        return malformed("splice SEI array has a different NAL type");
    }
    let rbsp: Vec<_> = Rbsp::new(&nal[2..]).collect();
    let mut pos = 0;
    let mut valid = true;
    let mut messages = 0;
    while let Some(byte) = rbsp.get(pos) {
        if *byte == 0x80 && rbsp[pos + 1..].iter().all(|byte| *byte == 0) {
            return Ok(valid && messages > 0);
        }
        let Some(kind) = sei_number(&rbsp, &mut pos) else {
            break;
        };
        let Some(len) = sei_number(&rbsp, &mut pos) else {
            break;
        };
        let end = pos + usize::from(len);
        let Some(data) = rbsp.get(pos..end) else {
            break;
        };
        valid &= check(kind, data)?;
        messages += 1;
        pos = end;
    }
    malformed("truncated splice SEI")
}

fn check_parameter_links([vps, sps, pps]: [&[u8]; 3]) -> Result<u32, Fmp4Error> {
    let vps_id = SliceBits::new(&vps[2..]).read(4);
    let mut sps = SliceBits::new(&sps[2..]);
    if vps_id.is_none() || sps.read(4) != vps_id || sps.read(3) != Some(0) {
        return Err(unsupported(
            "splice SPS needs the admitted VPS and one temporal layer",
        ));
    }
    // temporal_id_nesting_flag and general profile_tier_level.
    for _ in 0..97 {
        if sps.read(1).is_none() {
            return malformed("truncated splice SPS");
        }
    }
    let sps_id = sps.ue().filter(|id| *id <= 15);
    let mut pps = SliceBits::new(&pps[2..]);
    let pps_id = pps.ue().filter(|id| *id <= 63);
    if sps_id.is_none() || pps.ue() != sps_id {
        return malformed("splice PPS does not resolve to its SPS");
    }
    pps_id.ok_or_else(|| unsupported("splice PPS id is invalid"))
}

/// `(bit_depth, chroma_format_idc)` from an AVC SPS NAL (header byte
/// included), for an `avcC` that omits its optional High-profile
/// depth/chroma extension. Reads only the leading fields: profile, the
/// parameter-set id and, for the high profiles, `chroma_format_idc` and the
/// two bit depths. `None` when the fields are truncated or out of range, or
/// when luma and chroma depths differ (the extension path refuses that too).
pub(super) fn avc_sps_depth_chroma(nal: &[u8]) -> Option<(u8, u8)> {
    let mut bits = SliceBits::new(nal.get(1..)?);
    let profile = bits.read(8)?;
    bits.read(16)?; // constraint flags and level_idc
    if bits.ue()? > 31 {
        return None;
    }
    if !matches!(
        profile,
        100 | 110 | 122 | 144 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134
    ) {
        return Some((8, 1));
    }
    let chroma = bits.ue()?;
    if chroma > 3 {
        return None;
    }
    if chroma == 3 {
        bits.read(1)?; // separate_colour_plane_flag
    }
    let luma = bits.ue()?;
    let chroma_depth = bits.ue()?;
    if luma != chroma_depth || luma > 6 {
        return None;
    }
    Some((8 + luma as u8, chroma as u8))
}

/// Remove HEVC emulation-prevention bytes.
struct Rbsp<'a> {
    raw: std::slice::Iter<'a, u8>,
    zeros: u8,
}

impl<'a> Rbsp<'a> {
    fn new(raw: &'a [u8]) -> Self {
        Self {
            raw: raw.iter(),
            zeros: 0,
        }
    }
}

impl Iterator for Rbsp<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        let mut byte = *self.raw.next()?;
        if self.zeros == 2 && byte == 3 {
            byte = *self.raw.next()?;
            self.zeros = 0;
        }
        self.zeros = if byte == 0 {
            self.zeros.saturating_add(1)
        } else {
            0
        };
        Some(byte)
    }
}

/// Read only the parameter and slice header fields used to bind a sample to
/// its producer's configuration.
struct SliceBits<'a> {
    bytes: Rbsp<'a>,
    current: u8,
    remaining: u8,
}

impl<'a> SliceBits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes: Rbsp::new(bytes),
            current: 0,
            remaining: 0,
        }
    }

    fn read(&mut self, count: u8) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            if self.remaining == 0 {
                self.current = self.bytes.next()?;
                self.remaining = 8;
            }
            self.remaining -= 1;
            value = (value << 1) | u32::from((self.current >> self.remaining) & 1);
        }
        Some(value)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 31 {
                return None;
            }
        }
        Some((1 << zeros) - 1 + self.read(zeros)?)
    }
}

#[cfg(test)]
mod avc_sps_depth_chroma_tests {
    use super::avc_sps_depth_chroma;

    /// The SPS of "Kiss Kiss, Cam Cam" (M6 file 3057): High profile, level
    /// 4.1, copied from Matroska into an avcC with no depth/chroma extension.
    const KISS_KISS_SPS: &str = "67640029acd9805005ba6a040402800001f480005dc0478c18cd";

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn a_high_profile_sps_declares_its_depth_and_chroma() {
        assert_eq!(avc_sps_depth_chroma(&bytes(KISS_KISS_SPS)), Some((8, 1)));
        // Truncated right after level_idc: no declaration to trust.
        assert_eq!(avc_sps_depth_chroma(&bytes(&KISS_KISS_SPS[..8])), None);
        // Main profile (77) carries no extension fields: 8-bit 4:2:0.
        let mut main = bytes(KISS_KISS_SPS);
        main[1] = 77;
        assert_eq!(avc_sps_depth_chroma(&main), Some((8, 1)));
    }
}

#[cfg(test)]
mod hdr10_sei_tests {
    use super::super::{
        build_moof, parse_moof, set_dolby_vision_record, DolbyVisionRecord, FragmentReader, Plan,
        Unit,
    };
    use super::*;

    // The first two pictures of a Profile 7 UHD remux copied from a mid-film
    // resume, NAL headers and metadata only. The init (udta removed) declares
    // no HDR10 static metadata; the IRAP carries it in band.
    const IRAP: [&str; 9] = [
        "460110",
        "4e0181010f80",
        "4e010006800e6e73c54c80",
        "4e010102080780",
        "4e010601d080",
        "4e01891833c286c41d4c0bb884d03e803d13404200989680000003000180",
        "4e019004000003000080",
        "4e010440b5003c0001040140000c803d32186d0a54801564080000284cd050018cc8000190005a58013ed00392f806871841a80043a03d246b35d436b6159f2e5d4baa0080",
        "2801ac0577809f2b1bd2ffc256ed0dfc62960dd4e647090d",
    ];
    const TRAIL: [&str; 4] = [
        "460130",
        "4e010102001580",
        "4e010440b5003c0001040140000c803d32186d0a54801564080000284cd050018cc8000190005a58013ed00392f806871841a80043a03d246b35d436b6159f2e5d4baa0080",
        "0201d1051183c87ef0a8bf49296c3fc5f78365ac",
    ];

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn framed(nals: &[Vec<u8>]) -> Vec<u8> {
        nals.iter()
            .flat_map(|nal| {
                (nal.len() as u32)
                    .to_be_bytes()
                    .into_iter()
                    .chain(nal.clone())
            })
            .collect()
    }

    fn copy_init() -> Init {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/hdr10-sei-copy-init.mp4"
        ));
        let Some(Unit::Init(mut init)) = reader.next_unit().expect("fixture init") else {
            panic!("missing fixture init");
        };
        // The copy writer converts the Profile 7 record before splitting.
        let converted = DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("8.1 record");
        set_dolby_vision_record(&mut init, &converted).expect("convert record");
        init
    }

    /// The IRAP with the init's own parameter sets after its AUD, as the copy
    /// repeats them, then the picture that follows it.
    fn copy_fragment(init: &Init, irap_vps: Option<Vec<u8>>) -> Fragment {
        let location = locate_configuring_hvcc(&init.bytes)
            .expect("parse")
            .expect("hvcC");
        let mut sets: Vec<Vec<u8>> = hvcc_parameter_set_nals(&init.bytes[location.payload])
            .expect("parameter sets")
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        sets.sort_by_key(|nal| hevc_nal_type(nal));
        if let Some(vps) = irap_vps {
            sets[0] = vps;
        }
        let mut irap: Vec<_> = IRAP.iter().map(|nal| hex(nal)).collect();
        irap.splice(1..1, sets);
        let samples = [framed(&irap), framed(&TRAIL.map(hex))];
        let track = init.video().expect("video track");
        let plan = Plan {
            track_id: track.id,
            base_decode_time: 0,
            samples: samples
                .iter()
                .enumerate()
                .map(|(index, bytes)| Sample {
                    duration: 667,
                    size: bytes.len() as u32,
                    size_at: None,
                    flags: if index == 0 { 0x0200_0000 } else { 0x0101_0000 },
                    cto: 0,
                })
                .collect(),
            slices: Vec::new(),
        };
        let mut slots = Vec::new();
        let mut bytes = build_moof(&[plan], 1, &mut slots);
        let payload_start = bytes.len() + 8;
        bytes[slots[0]..slots[0] + 4].copy_from_slice(&(payload_start as i32).to_be_bytes());
        let tracks = parse_moof(&bytes, 8, std::slice::from_ref(track)).expect("parse moof");
        let payload = samples.concat();
        bytes.extend_from_slice(&super::super::mp4_box(b"mdat", &payload).expect("mdat"));
        Fragment {
            mdat_payload: payload_start..bytes.len(),
            bytes,
            tracks,
        }
    }

    #[test]
    fn a_dolby_vision_copy_keeps_its_in_band_hdr10_metadata_on_the_split() {
        let init = copy_init();
        let format = SpliceVideoFormat::new(&init, 48_000).expect("split video format");
        assert!(format.static_metadata.is_empty(), "the copy declares none");
        let rendition = format.admit(&init).expect("the copy is its own producer");
        let fragment = copy_fragment(&init, None);
        let clock = VideoSpliceClock {
            input_presentation_anchor: 0,
            output_presentation_anchor: 0,
            output_decode_start: 0,
        };
        rendition
            .fragment(&fragment, clock, 1, SpliceBoundary::Fragment)
            .expect("the ordinary copy passes its own in-band HDR10 SEI");

        let track = init.video().expect("video track");
        let copy = CopyStaticMetadata::of(&fragment, track).expect("copy static metadata");
        assert_eq!(copy.0.keys().copied().collect::<Vec<_>>(), [137, 144]);
        let (sample, bytes) = fragment.samples_with_bytes(track.id)[0];
        let bytes = bytes.expect("IRAP bytes");
        let replace = |copy: &CopyStaticMetadata| {
            let replacement = format.admit(&init).expect("producer").replacing(copy);
            replacement
                .verify_sample(bytes, sample, SpliceBoundary::Fragment)
                .map(|_| ())
        };
        replace(&copy).expect("a replacement repeating the copy's values");
        let unmatched =
            |copy: &CopyStaticMetadata| replace(copy).expect_err("replacement must match the copy");
        for refused in [CopyStaticMetadata::default(), {
            let mut other = copy.clone();
            other.0.insert(144, vec![0, 1, 0, 1]);
            other
        }] {
            assert_eq!(
                unmatched(&refused).to_string(),
                unsupported("splice sample changes HDR10 static metadata").to_string()
            );
        }

        // The decline log names the check: here, a repeated VPS that differs.
        let location = locate_configuring_hvcc(&init.bytes)
            .expect("parse")
            .expect("hvcC");
        let mut vps = hvcc_parameter_set_nals(&init.bytes[location.payload])
            .expect("parameter sets")
            .into_iter()
            .find(|nal| hevc_nal_type(nal) == Some(32))
            .expect("VPS")
            .to_vec();
        *vps.last_mut().expect("VPS byte") ^= 1;
        let error = rendition
            .fragment(
                &copy_fragment(&init, Some(vps)),
                clock,
                1,
                SpliceBoundary::Fragment,
            )
            .expect_err("a different VPS");
        assert!(
            error
                .to_string()
                .contains("splice sample repeats different parameter sets"),
            "{error}"
        );
    }
}
