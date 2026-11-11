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

/// Sizes/checksums are after FFmpeg filters, before post-mux Rust conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePacketObservation {
    pub sequence: u64,
    pub dts: i64,
    pub pts: i64,
    pub duration: u64,
    pub size: u64,
    pub checksum: u32,
    /// Framecrc omits F= for a key packet (AV_PKT_FLAG_KEY == 1).
    pub flags: u32,
    pub side_data_count: u32,
    pub time_base_num: u32,
    pub time_base_den: u32,
}

/// Exact source timing associated with one pre-conversion canonical fragment.
/// This is an observation, not authority to publish or proof of full coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFragmentClock {
    pub canonical_dts: u64,
    pub duration: u64,
    pub next_packet_sequence: u64,
}

/// Associate every selected-video sample with its same-input clock packet.
/// Run before Rust sample conversion. The final canonical row's byte counts
/// must be observed again after conversion; these packet sizes cannot replace
/// them. No packet may be dropped, duplicated or guessed by pipe arrival time.
pub fn associate_fragment_clock(
    fragment: &crate::fmp4::Fragment,
    init: &crate::fmp4::Init,
    packets: &[SourcePacketObservation],
    expected_sequence: u64,
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
    let mut next_sequence = expected_sequence;
    let mut next_source_dts = i128::from(first.dts);
    let mut duration = 0u64;
    for (sample, packet) in track.samples().zip(packets) {
        if packet.sequence != next_sequence
            || i128::from(packet.dts) != next_source_dts
            || packet.time_base_num == 0
            || packet.time_base_den == 0
            || packet.time_base_num != first.time_base_num
            || packet.time_base_den != first.time_base_den
            || packet.duration == 0
            || packet.size != u64::from(sample.size)
        {
            return Err(PacketClockError::FragmentMismatch);
        }
        let numerator = u128::from(packet.duration)
            .checked_mul(u128::from(packet.time_base_num))
            .and_then(|value| value.checked_mul(u128::from(video.timescale)))
            .ok_or(PacketClockError::Overflow)?;
        let denominator = u128::from(packet.time_base_den);
        if numerator % denominator != 0
            || numerator / denominator != u128::from(sample.duration)
            || sample.duration == 0
        {
            return Err(PacketClockError::FragmentMismatch);
        }
        duration = duration
            .checked_add(u64::from(sample.duration))
            .ok_or(PacketClockError::Overflow)?;
        next_sequence = next_sequence
            .checked_add(1)
            .ok_or(PacketClockError::Overflow)?;
        next_source_dts = next_source_dts
            .checked_add(i128::from(packet.duration))
            .ok_or(PacketClockError::Overflow)?;
    }
    let canonical_dts = normalize_to_index_ticks(
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
    canonical_dts
        .checked_add(duration)
        .ok_or(PacketClockError::Overflow)?;
    Ok(SourceFragmentClock {
        canonical_dts,
        duration,
        next_packet_sequence: next_sequence,
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
        if self.failed {
            return Err(PacketClockError::Failed);
        }
        let result = self.parse_line(line);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn parse_line(
        &mut self,
        line: &str,
    ) -> Result<Option<SourcePacketObservation>, PacketClockError> {
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
            } else if line.starts_with("#software:") {
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
        let checksum = parse_checksum(fields.next().ok_or(PacketClockError::InvalidPacket)?)?;
        let (flags, side_data_count) = parse_suffix(fields)?;
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
        let observation = SourcePacketObservation {
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
        let timing = associate_fragment_clock(&fragment, &init, &packets, 7, origin, 11)
            .expect("exact association");
        assert_eq!(timing.canonical_dts, 11);
        assert_eq!(timing.duration, track.duration());
        assert_eq!(timing.next_packet_sequence, 7 + packets.len() as u64);
        assert_eq!(
            associate_fragment_clock(&fragment, &init, &packets, 6, origin, 11),
            Err(PacketClockError::FragmentMismatch)
        );
        for field in 0..4 {
            let mut wrong = packets.clone();
            match field {
                0 => wrong[0].size += 1,
                1 => wrong[0].duration += 1,
                2 => wrong[0].time_base_den = 0,
                _ => wrong[0].sequence += 1,
            }
            assert_eq!(
                associate_fragment_clock(&fragment, &init, &wrong, 7, origin, 11),
                Err(PacketClockError::FragmentMismatch)
            );
        }
        assert_eq!(
            associate_fragment_clock(
                &fragment,
                &init,
                &packets[..packets.len() - 1],
                7,
                origin,
                11
            ),
            Err(PacketClockError::FragmentMismatch)
        );
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
}
