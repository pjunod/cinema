//! Bounded exact observations from a selected-video `framecrc` output.
//! Packet order and clocks are mathematical observations, not authenticated
//! source identity, grant authority or proof of fragment/sample equivalence.

pub const MAX_PACKET_CLOCK_LINE_BYTES: usize = 4_096;
pub const MAX_PACKET_CLOCK_HEADER_BYTES: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketClockError {
    LineTooLong,
    HeaderTooLarge,
    InvalidHeader,
    InvalidPacket,
    MissingTimeBase,
    WrongStream,
    Ordering,
    Overflow,
    Incomplete,
    Failed,
    FragmentMismatch,
    SourceTime(super::ranges::RangeError),
}

/// Sizes/checksums are after FFmpeg filters, before the MP4 muxer frames the
/// packet and before post-mux Rust conversion. A CBS bitstream filter leaves
/// HEVC as Annex B, so compare a size with a sample only through
/// [`packet_size_frames_sample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedVideoPacket<Digest> {
    pub sequence: u64,
    pub dts: i64,
    pub pts: i64,
    pub duration: u64,
    pub size: u64,
    pub checksum: Digest,
    /// Framecrc omits F= for a key packet (AV_PKT_FLAG_KEY == 1).
    pub flags: u32,
    pub side_data_count: u32,
    pub time_base_num: u32,
    pub time_base_den: u32,
}

pub type SourcePacketObservation = ObservedVideoPacket<u32>;

/// A packet from a SHA-256 framehash stream filtered to coded picture units.
pub type SourcePictureObservation = ObservedVideoPacket<[u8; 32]>;

/// Exact source timing associated with one pre-conversion canonical fragment.
/// This is an observation, not authority to publish or proof of full coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFragmentClock {
    pub first_packet: SourcePacketObservation,
    pub canonical_dts: u64,
    pub duration: u64,
    pub next_packet_sequence: u64,
    /// The MP4 decode time, in track ticks, of this fragment's last sample:
    /// what the next fragment's strict-muxer timeline continues from. See
    /// [`associate_fragment_clock`].
    pub last_muxed_dts: i64,
}

/// Whether a clock packet's size is that of the muxed sample's own units.
///
/// The packet reaches the MP4 muxer in one of two framings. Passed through
/// unfiltered it is the source's length-prefixed sample, byte for byte. Out of
/// a CBS bitstream filter (`filter_units`, `dovi_rpu`) it is the same NAL units
/// as Annex B, which the muxer reframes with length prefixes; its size is then
/// exactly [`crate::fmp4::hevc_sample_cbs_annex_b_len`] of the sample. Either
/// equality is exact: a sample whose bytes are not inside its fragment, or
/// that does not parse, matches only by the first.
pub fn packet_size_frames_sample(
    packet_size: u64,
    sample: &crate::fmp4::Sample,
    bytes: Option<&[u8]>,
    video: &crate::fmp4::Track,
) -> bool {
    packet_size == u64::from(sample.size)
        || (video.codec == Some(crate::fmp4::VideoCodec::Hevc)
            && bytes.is_some_and(|bytes| {
                crate::fmp4::hevc_sample_cbs_annex_b_len(bytes, video.nal_length_size)
                    .is_ok_and(|len| u64::try_from(len).is_ok_and(|len| len == packet_size))
            }))
}

/// Associate every selected-video sample with its same-input clock packet.
/// Run before Rust sample conversion. The final canonical row's byte counts
/// must be observed again after conversion; these packet sizes cannot replace
/// them. No packet may be dropped, duplicated or guessed by pipe arrival time.
///
/// The clock and the canonical MP4 are two outputs of one FFmpeg child, fed
/// the identical packets. They differ in one documented way: FFmpeg gives a
/// strict muxer (MP4) a decode time one tick past the last one it wrote,
/// `max(dts, last + 1)`, raising the presentation time with it, while the
/// NONSTRICT framecrc clock keeps an equal decode time. Matroska stores no
/// decode times, so an equal run is what FFmpeg synthesizes at a reordered
/// head and after a seek. Each sample is therefore checked against that
/// strict timeline, computed exactly in the track's own ticks from the clock.
/// `previous_muxed_dts` is the previous fragment's
/// [`SourceFragmentClock::last_muxed_dts`] in this same child, `None` for its
/// first fragment.
pub fn associate_fragment_clock(
    fragment: &crate::fmp4::Fragment,
    init: &crate::fmp4::Init,
    packets: &[SourcePacketObservation],
    expected_sequence: u64,
    previous_muxed_dts: Option<i64>,
    whole_title_origin: super::ranges::SourceTimestamp,
    canonical_title_start: u64,
) -> Result<SourceFragmentClock, PacketClockError> {
    use super::ranges::{normalize_to_index_ticks, SourceTimestamp};
    let video = init.video().ok_or(PacketClockError::FragmentMismatch)?;
    if init.tracks.len() != 1 || fragment.tracks.len() != 1 {
        return Err(PacketClockError::FragmentMismatch);
    }
    let track = fragment
        .track(video.id)
        .ok_or(PacketClockError::FragmentMismatch)?;
    if packets.is_empty() || packets.len() != track.sample_count() {
        return Err(PacketClockError::FragmentMismatch);
    }
    let first = packets[0];
    // A clock time in track ticks, exactly or not at all.
    let ticks = |value: i64| -> Result<i128, PacketClockError> {
        let numerator = i128::from(value)
            .checked_mul(i128::from(first.time_base_num))
            .and_then(|value| value.checked_mul(i128::from(video.timescale)))
            .ok_or(PacketClockError::Overflow)?;
        let denominator = i128::from(first.time_base_den);
        if denominator == 0 || numerator % denominator != 0 {
            return Err(PacketClockError::FragmentMismatch);
        }
        Ok(numerator / denominator)
    };
    // The strict muxer's (decode, presentation) time for every packet.
    let mut previous = previous_muxed_dts.map(i128::from);
    let mut muxed = Vec::with_capacity(packets.len());
    let mut next_sequence = expected_sequence;
    // A container's per-packet duration is rounded to its own time base (a
    // Matroska 1/1000 clock gives 41 ms to every 41.708 ms frame), so it is
    // never the sample's duration. The exact authority is the next muxed
    // decode time, as the mov muxer itself uses: each sample's duration must
    // equal its decode-time delta. The fragment's last sample is closed by the
    // next fragment's first decode time, which range continuity checks
    // against this row's end.
    let samples = fragment.samples_with_bytes(video.id);
    for (&(sample, bytes), packet) in samples.iter().zip(packets) {
        if packet.sequence != next_sequence
            || packet.time_base_num == 0
            || packet.time_base_den == 0
            || packet.time_base_num != first.time_base_num
            || packet.time_base_den != first.time_base_den
            || packet.duration == 0
            || !packet_size_frames_sample(packet.size, sample, bytes, video)
            || sample.duration == 0
        {
            return Err(PacketClockError::FragmentMismatch);
        }
        let dts = ticks(packet.dts)?;
        let pts = ticks(packet.pts)?;
        let strict = match previous {
            Some(last) => dts.max(last.checked_add(1).ok_or(PacketClockError::Overflow)?),
            None => dts,
        };
        // FFmpeg raises the presentation time only of a packet whose own
        // times are ordered; a reversed one is left as it is (and refused
        // below unless the sample's offset says exactly that).
        let presentation = if pts >= dts { pts.max(strict) } else { pts };
        muxed.push((strict, presentation));
        previous = Some(strict);
        next_sequence = next_sequence
            .checked_add(1)
            .ok_or(PacketClockError::Overflow)?;
    }
    let mut duration = 0u64;
    for (at, &(sample, _)) in samples.iter().enumerate() {
        let (dts, pts) = muxed[at];
        if let Some(&(following, _)) = muxed.get(at + 1) {
            if following - dts != i128::from(sample.duration) {
                return Err(PacketClockError::FragmentMismatch);
            }
        }
        // Decode order alone cannot establish the presentation clock of a
        // reordered stream. Preserve the exact signed composition offset.
        if pts - dts != i128::from(sample.cto) {
            return Err(PacketClockError::FragmentMismatch);
        }
        duration = duration
            .checked_add(u64::from(sample.duration))
            .ok_or(PacketClockError::Overflow)?;
    }
    let normalized = normalize_to_index_ticks(
        SourceTimestamp {
            dts: first.dts,
            time_base_num: first.time_base_num,
            time_base_den: first.time_base_den,
        },
        whole_title_origin,
        canonical_title_start,
        video.timescale,
    )
    .map_err(PacketClockError::SourceTime)?;
    // The row begins where the MP4 sample does: the source decode time plus
    // any tick the strict muxer added to it.
    let raised = u64::try_from(muxed[0].0 - ticks(first.dts)?)
        .map_err(|_| PacketClockError::FragmentMismatch)?;
    let canonical_dts = normalized
        .checked_add(raised)
        .ok_or(PacketClockError::Overflow)?;
    canonical_dts
        .checked_add(duration)
        .ok_or(PacketClockError::Overflow)?;
    let last_muxed_dts =
        i64::try_from(muxed[muxed.len() - 1].0).map_err(|_| PacketClockError::Overflow)?;
    Ok(SourceFragmentClock {
        first_packet: first,
        canonical_dts,
        duration,
        next_packet_sequence: next_sequence,
        last_muxed_dts,
    })
}

/// One line at a time; the reader must also bound its buffer before newline.
/// An error permanently fails this parser so callers cannot skip a bad packet
/// and subsequently mistake shifted packet sequence numbers for valid ones.
#[derive(Debug, Default)]
pub struct PacketClockParser {
    time_base: Option<(u32, u32)>,
    header_bytes: usize,
    packets: u64,
    last_dts: Option<i64>,
    declared_sha256: bool,
    failed: bool,
}

impl PacketClockParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(
        &mut self,
        line: &str,
    ) -> Result<Option<SourcePacketObservation>, PacketClockError> {
        self.push_line_with(line, false, parse_checksum)
    }

    fn push_line_with<Digest>(
        &mut self,
        line: &str,
        picture_hash: bool,
        parse_digest: impl FnOnce(&str) -> Result<Digest, PacketClockError>,
    ) -> Result<Option<ObservedVideoPacket<Digest>>, PacketClockError> {
        if self.failed {
            return Err(PacketClockError::Failed);
        }
        let result = self.parse_line(line, picture_hash, parse_digest);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn parse_line<Digest>(
        &mut self,
        line: &str,
        picture_hash: bool,
        parse_digest: impl FnOnce(&str) -> Result<Digest, PacketClockError>,
    ) -> Result<Option<ObservedVideoPacket<Digest>>, PacketClockError> {
        if line.len() > MAX_PACKET_CLOCK_LINE_BYTES {
            return Err(PacketClockError::LineTooLong);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.contains(['\r', '\n', '\0']) {
            return Err(PacketClockError::InvalidPacket);
        }
        if line.starts_with('#') {
            if self.packets != 0 {
                return Err(PacketClockError::Ordering);
            }
            self.header_bytes = self
                .header_bytes
                .checked_add(line.len())
                .ok_or(PacketClockError::HeaderTooLarge)?;
            if self.header_bytes > MAX_PACKET_CLOCK_HEADER_BYTES {
                return Err(PacketClockError::HeaderTooLarge);
            }
            if let Some(tb) = line.strip_prefix("#tb ") {
                if self.time_base.is_some() {
                    return Err(PacketClockError::Ordering);
                }
                let (stream, rational) =
                    tb.split_once(':').ok_or(PacketClockError::InvalidHeader)?;
                check_stream(stream)?;
                let (num, den) = rational
                    .trim()
                    .split_once('/')
                    .ok_or(PacketClockError::InvalidHeader)?;
                let num = num
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| PacketClockError::InvalidHeader)?;
                let den = den
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| PacketClockError::InvalidHeader)?;
                if num == 0 || den == 0 {
                    return Err(PacketClockError::InvalidHeader);
                }
                self.time_base = Some((num, den));
            } else if let Some(hash) = line.strip_prefix("#hash: ") {
                if !picture_hash || self.declared_sha256 || hash != "SHA256" {
                    return Err(PacketClockError::InvalidHeader);
                }
                self.declared_sha256 = true;
            } else if let Some(extra) = line.strip_prefix("#extradata ").filter(|_| picture_hash) {
                let mut fields = extra.split(',').map(str::trim);
                check_stream(fields.next().ok_or(PacketClockError::InvalidHeader)?)?;
                fields
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|size| *size != 0)
                    .ok_or(PacketClockError::InvalidHeader)?;
                decode_picture_sha256(fields.next().ok_or(PacketClockError::InvalidHeader)?)
                    .map_err(|_| PacketClockError::InvalidHeader)?;
                if !self.declared_sha256 || fields.next().is_some() {
                    return Err(PacketClockError::InvalidHeader);
                }
            } else if line.starts_with("#software:")
                || picture_hash
                    && (matches!(line, "#format: frame checksums" | "#version: 2")
                        || line.starts_with("#stream#"))
            {
                // Bounded provenance text; never trusted as engine identity.
            } else {
                let (kind, value) = line
                    .split_once(' ')
                    .ok_or(PacketClockError::InvalidHeader)?;
                if !matches!(
                    kind,
                    "#media_type" | "#codec_id" | "#dimensions" | "#sar" | "#extradata"
                ) {
                    return Err(PacketClockError::InvalidHeader);
                }
                let (stream, value) = value
                    .split_once(':')
                    .ok_or(PacketClockError::InvalidHeader)?;
                check_stream(stream)?;
                if value.trim().is_empty() || kind == "#media_type" && value.trim() != "video" {
                    return Err(PacketClockError::InvalidHeader);
                }
            }
            return Ok(None);
        }
        let (num, den) = self.time_base.ok_or(PacketClockError::MissingTimeBase)?;
        if picture_hash && !self.declared_sha256 {
            return Err(PacketClockError::InvalidHeader);
        }
        // A bounded fixed record, no Vec allocation proportional to input.
        let mut fields = line.split(',').map(str::trim);
        check_stream(fields.next().ok_or(PacketClockError::InvalidPacket)?)?;
        let dts = fields
            .next()
            .ok_or(PacketClockError::InvalidPacket)?
            .parse::<i64>()
            .map_err(|_| PacketClockError::InvalidPacket)?;
        let pts = fields
            .next()
            .ok_or(PacketClockError::InvalidPacket)?
            .parse::<i64>()
            .map_err(|_| PacketClockError::InvalidPacket)?;
        let duration = fields
            .next()
            .ok_or(PacketClockError::InvalidPacket)?
            .parse::<u64>()
            .map_err(|_| PacketClockError::InvalidPacket)?;
        let size = fields
            .next()
            .ok_or(PacketClockError::InvalidPacket)?
            .parse::<u64>()
            .map_err(|_| PacketClockError::InvalidPacket)?;
        let checksum = parse_digest(fields.next().ok_or(PacketClockError::InvalidPacket)?)?;
        let (flags, side_data_count) = if picture_hash {
            if fields.next().is_some() || size == 0 {
                return Err(PacketClockError::InvalidPacket);
            }
            // This format has no packet flags. The matched media sample
            // establishes its sync and leading-picture properties.
            (0, 0)
        } else {
            parse_suffix(fields)?
        };
        if duration == 0 || duration > i64::MAX as u64 || dts == i64::MIN || pts == i64::MIN {
            return Err(PacketClockError::InvalidPacket);
        }
        // Framecrc explicitly supports nonstrict DTS. Equal decode timestamps
        // still have distinct packet sequence numbers; backwards DTS do not.
        if self.last_dts.is_some_and(|last| dts < last) {
            return Err(PacketClockError::Ordering);
        }
        let next = self
            .packets
            .checked_add(1)
            .ok_or(PacketClockError::Overflow)?;
        let observation = ObservedVideoPacket {
            sequence: self.packets,
            dts,
            pts,
            duration,
            size,
            checksum,
            flags,
            side_data_count,
            time_base_num: num,
            time_base_den: den,
        };
        self.packets = next;
        self.last_dts = Some(dts);
        Ok(Some(observation))
    }

    /// Text exhaustion is not physical source EOF proof.
    pub fn finish(&self) -> Result<(), PacketClockError> {
        if self.failed {
            return Err(PacketClockError::Failed);
        }
        if self.time_base.is_none() || self.packets == 0 {
            return Err(PacketClockError::Incomplete);
        }
        Ok(())
    }
}

/// Bounded parser for the same-input SHA-256 picture clock. A malformed
/// record fences the entire observation, rather than skipping a picture.
#[derive(Debug, Default)]
pub struct PictureClockParser(PacketClockParser);

impl PictureClockParser {
    pub fn push_line(
        &mut self,
        line: &str,
    ) -> Result<Option<SourcePictureObservation>, PacketClockError> {
        self.0.push_line_with(line, true, decode_picture_sha256)
    }

    pub fn finish(&self) -> Result<(), PacketClockError> {
        self.0.finish()
    }
}

fn decode_picture_sha256(value: &str) -> Result<[u8; 32], PacketClockError> {
    let mut digest = [0; 32];
    hex::decode_to_slice(value, &mut digest).map_err(|_| PacketClockError::InvalidPacket)?;
    Ok(digest)
}

fn parse_checksum(value: &str) -> Result<u32, PacketClockError> {
    value
        .strip_prefix("0x")
        .filter(|value| value.len() == 8)
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .ok_or(PacketClockError::InvalidPacket)
}

/// FFmpeg 7.1 emits S=N followed by N sizes; 8.0 emits N triples of
/// side-data name, size and checksum. Neither changes the six packet fields.
fn parse_suffix<'a>(
    fields: impl Iterator<Item = &'a str> + Clone,
) -> Result<(u32, u32), PacketClockError> {
    let mut fields = fields.peekable();
    let flags = if fields.peek().is_some_and(|value| value.starts_with("F=")) {
        fields
            .next()
            .and_then(|value| value.strip_prefix("F=0x"))
            .filter(|value| !value.is_empty() && value.len() <= 8)
            .and_then(|value| u32::from_str_radix(value, 16).ok())
            .ok_or(PacketClockError::InvalidPacket)?
    } else {
        1
    };
    let Some(side_data) = fields.next() else {
        return Ok((flags, 0));
    };
    let count = side_data
        .strip_prefix("S=")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|count| *count > 0 && *count <= 64)
        .ok_or(PacketClockError::InvalidPacket)?;
    let remaining = fields.clone().count();
    if remaining == count as usize {
        for size in fields {
            size.parse::<u64>()
                .map_err(|_| PacketClockError::InvalidPacket)?;
        }
    } else if remaining == count as usize * 3 {
        for _ in 0..count {
            let name = fields.next().ok_or(PacketClockError::InvalidPacket)?;
            if name.is_empty() {
                return Err(PacketClockError::InvalidPacket);
            }
            fields
                .next()
                .ok_or(PacketClockError::InvalidPacket)?
                .parse::<u64>()
                .map_err(|_| PacketClockError::InvalidPacket)?;
            parse_checksum(fields.next().ok_or(PacketClockError::InvalidPacket)?)?;
        }
    } else {
        return Err(PacketClockError::InvalidPacket);
    }
    Ok((flags, count))
}

fn check_stream(value: &str) -> Result<(), PacketClockError> {
    match value.trim().parse::<u32>() {
        Ok(0) => Ok(()),
        Ok(_) => Err(PacketClockError::WrongStream),
        Err(_) => Err(PacketClockError::InvalidPacket),
    }
}

#[cfg(test)]
mod association_tests {
    use super::*;
    use crate::fmp4::{FragmentReader, Unit};
    use crate::segplan::ranges::SourceTimestamp;

    #[test]
    fn picture_clock_requires_exact_sha256_and_preserves_source_order() {
        let mut clock = PictureClockParser::default();
        for header in [
            "#format: frame checksums",
            "#version: 2",
            "#hash: SHA256",
            "#extradata 0, 2357, abababababababababababababababababababababababababababababababab",
            "#tb 0: 1/1000",
            "#stream#, dts, pts, duration, size, hash",
        ] {
            assert!(clock.push_line(header).expect("valid header").is_none());
        }
        let hash = "ab".repeat(32);
        let first = clock
            .push_line(&format!("0, 591216, 591299, 41, 1000, {hash}"))
            .expect("complete picture observation")
            .expect("picture packet");
        assert_eq!(first.sequence, 0);
        assert_eq!(first.pts, 591299);
        assert_eq!(first.dts, 591216);
        assert_eq!(first.checksum, [0xab; 32]);
        clock.finish().expect("complete observed prefix");
        assert_eq!(
            clock.push_line("0, 591257, 591257, 42, 18, 0x0badcafe"),
            Err(PacketClockError::InvalidPacket)
        );
        assert_eq!(clock.finish(), Err(PacketClockError::Failed));

        let mut undeclared = PictureClockParser::default();
        undeclared.push_line("#tb 0: 1/1000").expect("valid header");
        assert_eq!(
            undeclared.push_line(&format!("0, 591216, 591299, 41, 1000, {hash}")),
            Err(PacketClockError::InvalidHeader)
        );
    }

    #[test]
    fn every_fragment_sample_requires_its_exact_ordered_source_clock() {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("fixture init") else {
            panic!("missing init");
        };
        let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("fixture fragment") else {
            panic!("missing fragment");
        };
        let video = init.video().expect("fixture video");
        let track = fragment.track(video.id).expect("fixture samples");
        let mut dts = -1_000_000i64;
        let packets: Vec<_> = track
            .samples()
            .enumerate()
            .map(|(at, sample)| {
                let packet = SourcePacketObservation {
                    sequence: 7 + at as u64,
                    dts,
                    pts: dts + sample.cto,
                    duration: u64::from(sample.duration),
                    size: u64::from(sample.size),
                    checksum: 0,
                    flags: 1,
                    side_data_count: 0,
                    time_base_num: 1,
                    time_base_den: video.timescale,
                };
                dts += i64::from(sample.duration);
                packet
            })
            .collect();
        let origin = SourceTimestamp {
            dts: -1_000_000,
            time_base_num: 1,
            time_base_den: video.timescale,
        };
        let timing = associate_fragment_clock(&fragment, &init, &packets, 7, None, origin, 11)
            .expect("exact association");
        assert_eq!(timing.first_packet, packets[0]);
        assert_eq!(timing.canonical_dts, 11);
        assert_eq!(timing.duration, track.duration());
        assert_eq!(timing.next_packet_sequence, 7 + packets.len() as u64);
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &packets, 6, None, origin, 11),
            Err(PacketClockError::FragmentMismatch)
        );
        // A rounded container duration is not the sample clock: only the
        // DTS deltas are, so packets that keep them still associate.
        let mut rounded = packets.clone();
        for packet in &mut rounded {
            packet.duration = packet.duration.saturating_sub(1).max(1);
        }
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &rounded, 7, None, origin, 11)
                .expect("rounded packet durations")
                .duration,
            track.duration()
        );
        for field in 0..5 {
            let mut wrong = packets.clone();
            match field {
                0 => wrong[0].size += 1,
                1 => {
                    wrong[1].dts += 1;
                    wrong[1].pts += 1;
                }
                2 => wrong[0].time_base_den = 0,
                3 => wrong[0].sequence += 1,
                _ => wrong[0].pts += 1,
            }
            assert_eq!(
                associate_fragment_clock(&fragment, &init, &wrong, 7, None, origin, 11),
                Err(PacketClockError::FragmentMismatch)
            );
        }
        assert_eq!(
            associate_fragment_clock(
                &fragment,
                &init,
                &packets[..packets.len() - 1],
                7,
                None,
                origin,
                11
            ),
            Err(PacketClockError::FragmentMismatch)
        );
    }

    /// The fixture's first fragment, cut to `table.len()` samples carrying
    /// exactly the MP4 (duration, composition offset) FFmpeg wrote, and the
    /// clock packets `(dts, pts)` beside them, in the track's own ticks.
    fn retimed(
        table: &[(u32, i64)],
        clock: &[(i64, i64)],
    ) -> (
        crate::fmp4::Init,
        crate::fmp4::Fragment,
        Vec<SourcePacketObservation>,
    ) {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("fixture init") else {
            panic!("missing init");
        };
        let Some(Unit::Fragment(mut fragment)) = reader.next_unit().expect("fixture fragment")
        else {
            panic!("missing fragment");
        };
        let timescale = init.video().expect("fixture video").timescale;
        fragment.tracks[0].runs.truncate(1);
        let samples = &mut fragment.tracks[0].runs[0].samples;
        // Only sizes and timing are compared; padding repeats the last sample.
        while samples.len() < table.len() {
            let last = *samples.last().expect("fixture sample");
            samples.push(last);
        }
        samples.truncate(table.len());
        for (sample, &(duration, cto)) in samples.iter_mut().zip(table) {
            sample.duration = duration;
            sample.cto = cto;
        }
        let packets = clock
            .iter()
            .zip(fragment.tracks[0].runs[0].samples.iter())
            .enumerate()
            .map(|(at, (&(dts, pts), sample))| SourcePacketObservation {
                sequence: at as u64,
                dts,
                pts,
                duration: 41,
                size: u64::from(sample.size),
                checksum: 0,
                flags: u32::from(at == 0),
                side_data_count: 0,
                time_base_num: 1,
                time_base_den: timescale,
            })
            .collect();
        (init, fragment, packets)
    }

    /// Regression (Countdown, file 1421, from its start): at a reordered
    /// Matroska head FFmpeg gives four packets the decode time 125 ms. The
    /// NONSTRICT framecrc clock keeps them equal; the strict MP4 muxer writes
    /// `last + 1` ticks, so the samples last 1, 1, 1 and 669 ticks. These are
    /// the real values, read from both outputs of the production recipe.
    #[test]
    fn an_equal_decode_run_associates_on_the_strict_muxers_own_timeline() {
        let clock = [
            (0, 0),
            (656, 2672),
            (1312, 1328),
            (1313, 1313),
            (2000, 2000),
            (2000, 5344),
            (2000, 4000),
            (2000, 3344),
            (2672, 4672),
        ];
        let table = [
            (656, 0),
            (656, 2016),
            (1, 16),
            (687, 0),
            (1, 0),
            (1, 3343),
            (1, 1998),
            (669, 1341),
            (672, 2000),
        ];
        let (init, fragment, packets) = retimed(&table, &clock);
        let timescale = init.video().expect("video").timescale;
        let origin = SourceTimestamp {
            dts: 0,
            time_base_num: 1,
            time_base_den: timescale,
        };
        let timing = associate_fragment_clock(&fragment, &init, &packets, 0, None, origin, 0)
            .expect("the strict timeline is exact");
        assert_eq!(timing.canonical_dts, 0);
        assert_eq!(timing.last_muxed_dts, 2672);
        assert_eq!(timing.next_packet_sequence, packets.len() as u64);
        // Anything but the strict muxer's own repair is still refused.
        let mut wrong = table;
        wrong[4].0 = 2;
        wrong[5].0 = 0;
        let (init, fragment, packets) = retimed(&wrong, &clock);
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &packets, 0, None, origin, 0),
            Err(PacketClockError::FragmentMismatch)
        );
        // The repair continues across fragments of one child: a fragment
        // that begins on its predecessor's last decode time starts a tick on.
        let (init, fragment, packets) = retimed(&table, &clock);
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &packets, 0, Some(-1), origin, 0)
                .expect("an earlier predecessor does not move this fragment")
                .canonical_dts,
            0
        );
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &packets, 0, Some(0), origin, 0),
            Err(PacketClockError::FragmentMismatch)
        );
        // A fragment whose first packet the clock stamps with its
        // predecessor's last decode time (2000) is written at 2001 by the
        // MP4, and its row begins there.
        let next_clock = [(2000, 2000), (2000, 3344), (2672, 4672)];
        let next_table = [(1, 0), (670, 1342), (672, 2000)];
        let (init, fragment, packets) = retimed(&next_table, &next_clock);
        let timing = associate_fragment_clock(&fragment, &init, &packets, 0, Some(2000), origin, 0)
            .expect("a run that crosses a fragment boundary");
        assert_eq!(timing.canonical_dts, 2001);
        assert_eq!(timing.last_muxed_dts, 2672);
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &packets, 0, None, origin, 0),
            Err(PacketClockError::FragmentMismatch),
            "without its predecessor the fragment is not what the MP4 wrote"
        );
    }

    /// Regression (Countdown restarted at 18.493 s, and the 'Avatar' recovery
    /// restart): after a seek FFmpeg synthesizes the first decode times below
    /// a millisecond. The MP4 wrote 256259 ticks of 1/16000 where a 1/1000
    /// clock read 16016; only a clock in the track's own ticks associates.
    #[test]
    fn a_seeked_matroska_head_associates_only_in_the_mp4_tracks_time_base() {
        let table = [
            (656, 2669),
            (656, 4685),
            (656, 2685),
            (701, 1373),
            (672, 2000),
        ];
        let track = [
            (256259, 258928),
            (256915, 261600),
            (257571, 260256),
            (258227, 259600),
            (258928, 260928),
        ];
        let (init, fragment, packets) = retimed(&table, &track);
        let timescale = init.video().expect("video").timescale;
        let origin = SourceTimestamp {
            dts: 0,
            time_base_num: 1,
            time_base_den: timescale,
        };
        let timing = associate_fragment_clock(&fragment, &init, &packets, 0, None, origin, 0)
            .expect("same-time-base clock");
        assert_eq!(timing.canonical_dts, 256259);
        // The same packets as the source's 1/1000 clock reported them.
        let millis = [
            (16016, 16183),
            (16057, 16350),
            (16098, 16266),
            (16139, 16225),
            (16183, 16308),
        ];
        let mut rounded = packets;
        for (packet, &(dts, pts)) in rounded.iter_mut().zip(&millis) {
            (packet.dts, packet.pts, packet.time_base_den) = (dts, pts, 1000);
        }
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &rounded, 0, None, origin, 0),
            Err(PacketClockError::FragmentMismatch)
        );
    }

    /// A CBS filter's Annex B packet counts the units FFmpeg reframed: a
    /// four-byte start code for the first unit and VPS/SPS/PPS, three for
    /// every other unit, where the muxed sample has a four-byte length each.
    #[test]
    fn an_annex_b_clock_packet_frames_the_same_units_as_its_muxed_sample() {
        fn unit(kind: u8, payload: usize) -> Vec<u8> {
            let len = 2 + payload;
            let mut framed = u32::try_from(len)
                .expect("small unit")
                .to_be_bytes()
                .to_vec();
            framed.extend([kind << 1, 1]);
            framed.extend(std::iter::repeat_n(0x55, payload));
            framed
        }
        let sample: Vec<u8> = [unit(39, 5), unit(20, 9), unit(1, 3)].concat();
        // 4 + 7, then 3 + 11 and 3 + 5: one byte less for each later unit.
        assert_eq!(crate::fmp4::hevc_sample_cbs_annex_b_len(&sample, 4), Ok(33));
        assert_eq!(sample.len(), 35);
        let sets: Vec<u8> = [
            unit(39, 5),
            unit(32, 4),
            unit(33, 4),
            unit(34, 4),
            unit(21, 9),
        ]
        .concat();
        assert_eq!(
            crate::fmp4::hevc_sample_cbs_annex_b_len(&sets, 4),
            Ok(sets.len() - 1)
        );
        assert!(crate::fmp4::hevc_sample_cbs_annex_b_len(&[], 4).is_err());
        assert!(crate::fmp4::hevc_sample_cbs_annex_b_len(&sample[..sample.len() - 1], 4).is_err());

        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("fixture init") else {
            panic!("missing init");
        };
        let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("fixture fragment") else {
            panic!("missing fragment");
        };
        let video = init.video().expect("fixture video");
        let samples = fragment.samples_with_bytes(video.id);
        let mut dts = 0i64;
        let packets: Vec<_> = samples
            .iter()
            .enumerate()
            .map(|(at, &(sample, bytes))| {
                let size = crate::fmp4::hevc_sample_cbs_annex_b_len(
                    bytes.expect("fixture sample bytes"),
                    video.nal_length_size,
                )
                .expect("fixture sample units");
                let packet = SourcePacketObservation {
                    sequence: at as u64,
                    dts,
                    pts: dts + sample.cto,
                    duration: u64::from(sample.duration),
                    size: size as u64,
                    checksum: 0,
                    flags: 1,
                    side_data_count: 0,
                    time_base_num: 1,
                    time_base_den: video.timescale,
                };
                dts += i64::from(sample.duration);
                packet
            })
            .collect();
        assert!(
            packets
                .iter()
                .zip(&samples)
                .any(|(packet, (sample, _))| packet.size != u64::from(sample.size)),
            "the fixture has a multi-unit sample whose framings differ"
        );
        let origin = SourceTimestamp {
            dts: 0,
            time_base_num: 1,
            time_base_den: video.timescale,
        };
        associate_fragment_clock(&fragment, &init, &packets, 0, None, origin, 0)
            .expect("the Annex B framing of each sample associates");
        for delta in [-1i64, 1] {
            let mut wrong = packets.clone();
            wrong[0].size = wrong[0].size.checked_add_signed(delta).expect("size");
            if wrong[0].size == u64::from(samples[0].0.size) {
                continue;
            }
            assert_eq!(
                associate_fragment_clock(&fragment, &init, &wrong, 0, None, origin, 0),
                Err(PacketClockError::FragmentMismatch)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser_with_header() -> PacketClockParser {
        let mut parser = PacketClockParser::new();
        parser
            .push_line("#software: Lavf")
            .expect("valid framecrc header or packet record");
        parser
            .push_line("#tb 0: 1/90000")
            .expect("valid framecrc header or packet record");
        parser
            .push_line("#media_type 0: video")
            .expect("valid framecrc header or packet record");
        parser
    }

    #[test]
    fn exact_signed_decode_and_reordered_presentation_clocks_keep_packet_sequence() {
        let mut parser = parser_with_header();
        let first = parser
            .push_line("0, -6000, 3000, 3000, 1048576, 0x12345678\n")
            .expect("valid framecrc header or packet record")
            .expect("packet record emits an observation");
        assert_eq!(
            first,
            SourcePacketObservation {
                sequence: 0,
                dts: -6000,
                pts: 3000,
                duration: 3000,
                size: 1048576,
                checksum: 0x12345678,
                flags: 1,
                side_data_count: 0,
                time_base_num: 1,
                time_base_den: 90000,
            }
        );
        let next = parser
            .push_line("0, -3000, 0, 3000, 5, 0xffffffff")
            .expect("valid framecrc header or packet record")
            .expect("packet record emits an observation");
        assert_eq!((next.sequence, next.dts, next.pts), (1, -3000, 0));
        assert_eq!(parser.finish(), Ok(()));
    }

    #[test]
    fn corrupt_packet_cannot_be_skipped_to_shift_sample_association() {
        for bad in [
            "1, 0, 0, 1, 5, 0x12345678",
            "0, NOPTS, 0, 1, 5, 0x12345678",
            "0, 9223372036854775808, 0, 1, 5, 0x12345678",
            "0, 0, 0, 0, 5, 0x12345678",
            "0, 0, 0, -1, 5, 0x12345678",
            "0, 0, 0, 1, 18446744073709551616, 0x12345678",
            "0, 0, 0, 1, 5, 0xNOTHEX00",
            "0, 0, 0, 1, 5, 0x12345678, S=1",
        ] {
            let mut parser = parser_with_header();
            assert!(parser.push_line(bad).is_err(), "{bad}");
            assert_eq!(
                parser.push_line("0, 1, 1, 1, 5, 0x12345678"),
                Err(PacketClockError::Failed)
            );
        }
    }

    #[test]
    fn missing_clock_backwards_packets_and_late_headers_fail_closed() {
        let mut missing = PacketClockParser::new();
        assert_eq!(
            missing.push_line("0, 0, 0, 1, 5, 0x12345678"),
            Err(PacketClockError::MissingTimeBase)
        );
        for late in ["#tb 0: 1/1", "0, -1, 0, 1, 5, 0x12345678"] {
            let mut parser = parser_with_header();
            parser
                .push_line("0, 0, 0, 1, 5, 0x12345678")
                .expect("valid framecrc header or packet record");
            assert_eq!(parser.push_line(late), Err(PacketClockError::Ordering));
        }
    }

    #[test]
    fn clock_and_header_resource_bounds_are_enforced() {
        for header in [
            "#tb 0: 0/1",
            "#tb 0: 1/0",
            "#tb 0: 1/4294967296",
            "#tb 1: 1/1",
        ] {
            assert!(PacketClockParser::new().push_line(header).is_err());
        }
        let mut parser = PacketClockParser::new();
        assert_eq!(
            parser.push_line(&"x".repeat(MAX_PACKET_CLOCK_LINE_BYTES + 1)),
            Err(PacketClockError::LineTooLong)
        );
        let mut parser = PacketClockParser::new();
        let header = format!("#software:{}", "x".repeat(MAX_PACKET_CLOCK_LINE_BYTES - 10));
        for _ in 0..4 {
            parser
                .push_line(&header)
                .expect("valid framecrc header or packet record");
        }
        assert_eq!(
            parser.push_line(&header),
            Err(PacketClockError::HeaderTooLarge)
        );
        assert_eq!(
            PacketClockParser::new().finish(),
            Err(PacketClockError::Incomplete)
        );
    }
    #[test]
    fn legacy_and_named_hdr_side_data_preserve_packet_association() {
        let mut parser = PacketClockParser::new();
        parser
            .push_line("#extradata 0: 10, 0x12345678")
            .expect("valid framecrc header or packet record");
        parser
            .push_line("#software: Lavf")
            .expect("valid framecrc header or packet record");
        parser
            .push_line("#tb 0: 1/90000")
            .expect("valid framecrc header or packet record");
        for (sequence, suffix) in [
            ", F=0x0, S=2, 24, 8",
            ", S=2, Mastering display metadata, 24, 0x12345678, Content light level metadata, 8, 0x87654321",
        ].iter().enumerate() {
            let packet = parser.push_line(&format!("0, 0, 0, 3000, 100, 0x00000000{suffix}"))
                .expect("valid framecrc header or packet record").expect("packet record emits an observation");
            assert_eq!(packet.sequence, sequence as u64);
            assert_eq!(packet.dts, 0); // equal DTS is valid; sequence is exact.
            assert_eq!(packet.side_data_count, 2);
            assert_eq!(packet.flags, if sequence == 0 { 0 } else { 1 });
        }
        assert_eq!(parser.finish(), Ok(()));
        for suffix in [
            ", S=2, 8",
            ", F=oops",
            ", S=1, HDR, 8, 0xINVALID0",
            ", S=65",
            ", S=1, 8, F=0x1",
        ] {
            assert!(parser_with_header()
                .push_line(&format!("0, 1, 1, 1, 5, 0x00000000{suffix}"))
                .is_err());
        }
    }

    /// The F+ packet identity and the copy packet clock share one CBS
    /// framing: the identity is the SHA-256 of exactly the Annex B bytes the
    /// clock counts, including the four-byte start code for the first unit
    /// and for VPS/SPS/PPS.
    #[test]
    fn the_refit_packet_identity_hashes_the_bytes_the_clock_counts() {
        use sha2::{Digest, Sha256};
        let units: [(u8, usize); 6] = [(35, 1), (32, 4), (33, 4), (34, 4), (39, 5), (1, 9)];
        let mut sample = Vec::new();
        let mut annex_b = Vec::new();
        for (index, (kind, payload)) in units.into_iter().enumerate() {
            let mut nal = vec![kind << 1, 1];
            nal.extend(std::iter::repeat_n(0x55, payload));
            sample.extend(u32::try_from(nal.len()).expect("small unit").to_be_bytes());
            sample.extend(&nal);
            if index == 0 || matches!(kind, 32..=34) {
                annex_b.push(0);
            }
            annex_b.extend([0, 0, 1]);
            annex_b.extend(&nal);
        }
        assert_eq!(
            crate::fmp4::hevc_sample_cbs_annex_b_len(&sample, 4),
            Ok(annex_b.len())
        );
        let expected: [u8; 32] = Sha256::digest(&annex_b).into();
        assert_eq!(
            crate::fmp4::hevc_cbs_annex_b_sample_hash(&sample, 4),
            Ok(expected)
        );
    }
}
