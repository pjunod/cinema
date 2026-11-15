//! Bounded same-input MP4/packet-clock association, independent of pipe arrival
//! order. These observations do not authenticate source, origin, grant or EOF.

use super::packet_clock::{
    associate_fragment_clock, PacketClockParser, SourceFragmentClock, SourcePacketObservation,
    MAX_PACKET_CLOCK_LINE_BYTES,
};
use super::ranges::SourceTimestamp;
use crate::fmp4::{Fragment, FragmentReader, Init, Unit};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationError {
    InvalidLimit,
    Media,
    InitContractChanged,
    Clock,
    Limit,
    Incomplete,
    Closed,
    Failed,
}

#[derive(Debug)]
pub enum ObservedUnit {
    Init(Init),
    /// Before Rust conversion: `clock` supplies timing, not final byte counts.
    Fragment {
        fragment: Fragment,
        clock: SourceFragmentClock,
    },
}

#[derive(Debug)]
pub struct FeedProgress {
    pub consumed: usize,
    pub event: Option<ObservedUnit>,
}

/// Callers retain unconsumed input and concurrently drain both pipes. In
/// particular, media feeding makes no progress while its sole held fragment
/// awaits clocks. No async task or independent source reader is created here.
pub struct ObservedRangeAssembler {
    media: FragmentReader,
    clocks: PacketClockParser,
    packets: VecDeque<SourcePacketObservation>,
    line: Vec<u8>,
    header: Vec<u8>,
    box_remaining: usize,
    held_media_bytes: usize,
    init: Option<Init>,
    fragment: Option<Fragment>,
    next_sequence: u64,
    origin: SourceTimestamp,
    canonical_start: u64,
    max_packets: usize,
    max_media: usize,
    media_eof: bool,
    clock_eof: bool,
    trailer: bool,
    failed: bool,
}

impl ObservedRangeAssembler {
    pub fn new(
        origin: SourceTimestamp,
        canonical_title_start: u64,
        max_pending_packets: usize,
        max_held_fragment_bytes: usize,
    ) -> Result<Self, ObservationError> {
        if max_pending_packets == 0 || max_held_fragment_bytes < 16 {
            return Err(ObservationError::InvalidLimit);
        }
        let mut media = FragmentReader::new();
        media.reject_reinitialization();
        Ok(Self {
            media,
            clocks: PacketClockParser::new(),
            packets: VecDeque::new(),
            line: Vec::new(),
            header: Vec::with_capacity(16),
            box_remaining: 0,
            held_media_bytes: 0,
            init: None,
            fragment: None,
            next_sequence: 0,
            origin,
            canonical_start: canonical_title_start,
            max_packets: max_pending_packets,
            max_media: max_held_fragment_bytes,
            media_eof: false,
            clock_eof: false,
            trailer: false,
            failed: false,
        })
    }

    pub fn feed_media(&mut self, bytes: &[u8]) -> Result<FeedProgress, ObservationError> {
        if self.failed {
            return Err(ObservationError::Failed);
        }
        let result = self.media_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn media_inner(&mut self, bytes: &[u8]) -> Result<FeedProgress, ObservationError> {
        if self.media_eof {
            return Err(ObservationError::Closed);
        }
        if let Some(event) = self.ready_fragment()? {
            return Ok(FeedProgress {
                consumed: 0,
                event: Some(event),
            });
        }
        let mut consumed = 0;
        if self.fragment.is_some() {
            return Ok(FeedProgress {
                consumed,
                event: None,
            });
        }
        while consumed < bytes.len() {
            if self.trailer {
                return Err(ObservationError::Media);
            }
            // Frame top-level boxes before feeding the existing parser. This
            // stops exactly at a completed fragment without queuing a second.
            if self.box_remaining == 0 {
                let wanted = if self.header.len() < 8 { 8 } else { 16 };
                let take = (wanted - self.header.len()).min(bytes.len() - consumed);
                self.header
                    .extend_from_slice(&bytes[consumed..consumed + take]);
                consumed += take;
                if self.header.len() < 8 {
                    continue;
                }
                let size = u32::from_be_bytes(
                    self.header[..4]
                        .try_into()
                        .map_err(|_| ObservationError::Media)?,
                );
                let header_size = if size == 1 { 16 } else { 8 };
                if self.header.len() < header_size {
                    continue;
                }
                let size = if size == 1 {
                    u64::from_be_bytes(
                        self.header[8..16]
                            .try_into()
                            .map_err(|_| ObservationError::Media)?,
                    )
                } else {
                    u64::from(size)
                };
                let size = usize::try_from(size).map_err(|_| ObservationError::Limit)?;
                if size < header_size {
                    return Err(ObservationError::Media);
                }
                if size > self.max_media.saturating_sub(self.held_media_bytes) {
                    return Err(ObservationError::Limit);
                }
                self.media.push(&self.header);
                self.held_media_bytes += header_size;
                self.header.clear();
                self.box_remaining = size - header_size;
            }
            let take = self.box_remaining.min(bytes.len() - consumed);
            self.media.push(&bytes[consumed..consumed + take]);
            self.held_media_bytes += take;
            consumed += take;
            self.box_remaining -= take;
            if self.box_remaining != 0 {
                continue;
            }
            match self
                .media
                .next_unit()
                .map_err(|error| match error.unsupported_kind() {
                    Some(crate::fmp4::Fmp4UnsupportedKind::InitContractChanged) => {
                        ObservationError::InitContractChanged
                    }
                    None => ObservationError::Media,
                })? {
                Some(Unit::Init(init)) => {
                    if self.init.is_some() || init.tracks.len() != 1 || init.video().is_none() {
                        return Err(ObservationError::Media);
                    }
                    self.held_media_bytes = 0;
                    self.init = Some(init.clone());
                    return Ok(FeedProgress {
                        consumed,
                        event: Some(ObservedUnit::Init(init)),
                    });
                }
                Some(Unit::Fragment(fragment)) => {
                    self.held_media_bytes = 0;
                    if fragment.len() > self.max_media {
                        return Err(ObservationError::Limit);
                    }
                    self.fragment = Some(fragment);
                    return Ok(FeedProgress {
                        consumed,
                        event: self.ready_fragment()?,
                    });
                }
                Some(Unit::Trailer) => {
                    self.held_media_bytes = 0;
                    self.trailer = true;
                }
                None => {}
            }
        }
        Ok(FeedProgress {
            consumed,
            event: None,
        })
    }

    pub fn feed_clock(&mut self, bytes: &[u8]) -> Result<FeedProgress, ObservationError> {
        if self.failed {
            return Err(ObservationError::Failed);
        }
        let result = self.clock_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn clock_inner(&mut self, bytes: &[u8]) -> Result<FeedProgress, ObservationError> {
        if self.clock_eof {
            return Err(ObservationError::Closed);
        }
        if let Some(event) = self.ready_fragment()? {
            return Ok(FeedProgress {
                consumed: 0,
                event: Some(event),
            });
        }
        for (at, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                let line = std::str::from_utf8(&self.line).map_err(|_| ObservationError::Clock)?;
                if let Some(packet) = self
                    .clocks
                    .push_line(line)
                    .map_err(|_| ObservationError::Clock)?
                {
                    if self.packets.len() == self.max_packets {
                        return Err(ObservationError::Limit);
                    }
                    self.packets.push_back(packet);
                }
                self.line.clear();
                if let Some(event) = self.ready_fragment()? {
                    return Ok(FeedProgress {
                        consumed: at + 1,
                        event: Some(event),
                    });
                }
            } else {
                if self.line.len() == MAX_PACKET_CLOCK_LINE_BYTES {
                    return Err(ObservationError::Limit);
                }
                self.line.push(*byte);
            }
        }
        Ok(FeedProgress {
            consumed: bytes.len(),
            event: None,
        })
    }

    fn ready_fragment(&mut self) -> Result<Option<ObservedUnit>, ObservationError> {
        let Some(fragment) = self.fragment.as_ref() else {
            return Ok(None);
        };
        let init = self.init.as_ref().ok_or(ObservationError::Media)?;
        let video = init.video().ok_or(ObservationError::Media)?;
        let count = fragment
            .track(video.id)
            .ok_or(ObservationError::Media)?
            .sample_count();
        if count == 0 {
            return Err(ObservationError::Media);
        }
        if count > self.max_packets {
            return Err(ObservationError::Limit);
        }
        if self.packets.len() < count {
            return Ok(None);
        }
        let packets = self.packets.make_contiguous();
        let clock = associate_fragment_clock(
            fragment,
            init,
            &packets[..count],
            self.next_sequence,
            self.origin,
            self.canonical_start,
        )
        .map_err(|_| ObservationError::Clock)?;
        drop(self.packets.drain(..count));
        self.next_sequence = clock.next_packet_sequence;
        let fragment = self.fragment.take().ok_or(ObservationError::Media)?;
        Ok(Some(ObservedUnit::Fragment { fragment, clock }))
    }

    pub fn mark_media_eof(&mut self) -> Result<(), ObservationError> {
        if self.failed {
            return Err(ObservationError::Failed);
        }
        if self.box_remaining != 0 || !self.header.is_empty() || self.media.buffered() != 0 {
            self.failed = true;
            return Err(ObservationError::Incomplete);
        }
        self.media_eof = true;
        Ok(())
    }

    pub fn mark_clock_eof(&mut self) -> Result<(), ObservationError> {
        if self.failed {
            return Err(ObservationError::Failed);
        }
        if !self.line.is_empty() || self.clocks.finish().is_err() {
            self.failed = true;
            return Err(ObservationError::Incomplete);
        }
        self.clock_eof = true;
        Ok(())
    }

    /// Both diagnostic streams ended cleanly, with no unassociated samples.
    /// This is NOT whole-title coverage: a seeked tail can satisfy it too.
    pub fn finish(&mut self) -> Result<(), ObservationError> {
        if self.failed {
            return Err(ObservationError::Failed);
        }
        if !self.media_eof
            || !self.clock_eof
            || !self.trailer
            || self.init.is_none()
            || self.fragment.is_some()
            || !self.packets.is_empty()
            || !self.line.is_empty()
            || !self.header.is_empty()
            || self.box_remaining != 0
            || self.media.buffered() != 0
        {
            self.failed = true;
            return Err(ObservationError::Incomplete);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Vec<u8>, Vec<u8>, SourceTimestamp, usize, u64) {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("authored fixture unit parses")
        else {
            panic!("init");
        };
        let Some(Unit::Fragment(fragment)) =
            reader.next_unit().expect("authored fixture unit parses")
        else {
            panic!("fragment");
        };
        let video = init.video().expect("authored fixture has video metadata");
        let track = fragment
            .track(video.id)
            .expect("authored fixture has a video track fragment");
        let mut text = format!("#tb 0: 1/{}\n#media_type 0: video\n", video.timescale);
        let mut dts = -1_000_000i64;
        for sample in track.samples() {
            text.push_str(&format!(
                "0, {}, {}, {}, {}, 0x00000000\n",
                dts,
                dts + sample.cto,
                sample.duration,
                sample.size
            ));
            dts += i64::from(sample.duration);
        }
        let count = track.sample_count();
        let duration = track.duration();
        let timescale = video.timescale;
        let mut media = init.bytes;
        media.extend_from_slice(&fragment.bytes);
        media.extend_from_slice(&[0, 0, 0, 8, b'm', b'f', b'r', b'a']);
        (
            media,
            text.into_bytes(),
            SourceTimestamp {
                dts: -1_000_000,
                time_base_num: 1,
                time_base_den: timescale,
            },
            count,
            duration,
        )
    }

    #[test]
    fn unrelated_pipe_chunking_associates_exactly_without_arrival_guesses() {
        let (media, clocks, origin, count, duration) = fixture();
        let mut assembler = ObservedRangeAssembler::new(origin, 17, count + 1, media.len())
            .expect("valid bounded assembler limits");
        let (mut m, mut c, mut inits, mut fragments) = (0, 0, 0, 0);
        while m < media.len() || c < clocks.len() {
            let progress = assembler
                .feed_media(&media[m..(m + 7).min(media.len())])
                .expect("valid fixture media chunk");
            m += progress.consumed;
            if let Some(event) = progress.event {
                match event {
                    ObservedUnit::Init(_) => inits += 1,
                    ObservedUnit::Fragment { clock, .. } => {
                        fragments += 1;
                        assert_eq!((clock.canonical_dts, clock.duration), (17, duration));
                    }
                }
            }
            let progress = assembler
                .feed_clock(&clocks[c..(c + 3).min(clocks.len())])
                .expect("valid fixture packet-clock chunk");
            c += progress.consumed;
            if let Some(ObservedUnit::Fragment { clock, .. }) = progress.event {
                fragments += 1;
                assert_eq!((clock.canonical_dts, clock.duration), (17, duration));
            }
        }
        assembler
            .mark_media_eof()
            .expect("media ends at a complete box");
        assembler
            .mark_clock_eof()
            .expect("clock stream ends at a complete record");
        assert_eq!(assembler.finish(), Ok(()));
        assert_eq!((inits, fragments), (1, 1));
    }

    #[test]
    fn media_backpressure_holds_one_fragment_until_its_packets_arrive() {
        let (media, clocks, origin, count, _) = fixture();
        let mut assembler = ObservedRangeAssembler::new(origin, 0, count + 1, media.len())
            .expect("valid bounded assembler limits");
        let init = assembler
            .feed_media(&media)
            .expect("valid fixture media chunk");
        assert!(matches!(init.event, Some(ObservedUnit::Init(_))));
        let fragment = assembler
            .feed_media(&media[init.consumed..])
            .expect("valid fixture media chunk");
        assert!(fragment.event.is_none());
        let blocked = assembler
            .feed_media(&media[init.consumed + fragment.consumed..])
            .expect("valid fixture media chunk");
        assert_eq!(blocked.consumed, 0);
        let associated = assembler
            .feed_clock(&clocks)
            .expect("valid fixture packet-clock chunk");
        assert!(matches!(
            associated.event,
            Some(ObservedUnit::Fragment { .. })
        ));
        assembler
            .feed_media(&media[init.consumed + fragment.consumed..])
            .expect("valid fixture media chunk");
        assembler
            .mark_clock_eof()
            .expect("clock stream ends at a complete record");
        assembler
            .mark_media_eof()
            .expect("media ends at a complete box");
        assert_eq!(assembler.finish(), Ok(()));
    }

    #[test]
    fn truncated_streams_and_resource_failures_cannot_be_resumed() {
        let (media, _, origin, count, _) = fixture();
        let mut limited = ObservedRangeAssembler::new(origin, 0, 1, media.len())
            .expect("valid bounded assembler limits");
        assert_eq!(
            limited
                .feed_clock(b"#tb 0: 1/1\n0, 0, 0, 1, 1, 0x00000000\n0, 1, 1, 1, 1, 0x00000000\n")
                .expect_err("invalid or over-limit clock input is refused"),
            ObservationError::Limit
        );
        assert_eq!(
            limited
                .feed_clock(b"\n")
                .expect_err("invalid or over-limit clock input is refused"),
            ObservationError::Failed
        );
        let mut line = ObservedRangeAssembler::new(origin, 0, count + 1, media.len())
            .expect("valid bounded assembler limits");
        assert_eq!(
            line.feed_clock(&vec![b'x'; MAX_PACKET_CLOCK_LINE_BYTES + 1])
                .expect_err("invalid or over-limit clock input is refused"),
            ObservationError::Limit
        );
        let mut partial = ObservedRangeAssembler::new(origin, 0, count + 1, media.len())
            .expect("valid bounded assembler limits");
        partial
            .feed_media(&media[..3])
            .expect("valid fixture media chunk");
        assert_eq!(partial.mark_media_eof(), Err(ObservationError::Incomplete));
        assert_eq!(partial.finish(), Err(ObservationError::Failed));
    }
    #[test]
    fn clocks_can_arrive_first_but_trailer_and_exhausted_packets_are_required() {
        let (media, clocks, origin, count, _) = fixture();
        let mut assembler = ObservedRangeAssembler::new(origin, 23, count + 1, media.len())
            .expect("valid bounded assembler limits");
        assembler
            .feed_clock(&clocks)
            .expect("valid fixture packet-clock chunk");
        assembler
            .mark_clock_eof()
            .expect("clock stream ends at a complete record");
        let mut consumed = 0;
        while consumed < media.len() - 8 {
            let progress = assembler
                .feed_media(&media[consumed..media.len() - 8])
                .expect("valid fixture media chunk");
            consumed += progress.consumed;
            assert!(progress.consumed != 0 || progress.event.is_some());
        }
        assembler
            .mark_media_eof()
            .expect("media ends at a complete box");
        assert_eq!(assembler.finish(), Err(ObservationError::Incomplete));

        let mut assembler = ObservedRangeAssembler::new(origin, 23, count + 1, media.len())
            .expect("valid bounded assembler limits");
        assembler
            .feed_clock(&clocks)
            .expect("valid fixture packet-clock chunk");
        let mut consumed = 0;
        while consumed < media.len() {
            let progress = assembler
                .feed_media(&media[consumed..])
                .expect("valid fixture media chunk");
            consumed += progress.consumed;
            assert!(progress.consumed != 0 || progress.event.is_some());
        }
        assembler
            .feed_clock(b"0, 9223372036854775807, 9223372036854775807, 1, 1, 0x00000000\n")
            .expect("valid fixture packet-clock chunk");
        assembler
            .mark_clock_eof()
            .expect("clock stream ends at a complete record");
        assembler
            .mark_media_eof()
            .expect("media ends at a complete box");
        assert_eq!(assembler.finish(), Err(ObservationError::Incomplete));
    }

    #[test]
    fn packet_sample_mismatch_and_declared_box_limits_fail_before_publication() {
        let (media, clocks, origin, count, _) = fixture();
        let mut assembler = ObservedRangeAssembler::new(origin, 0, count + 1, media.len())
            .expect("valid bounded assembler limits");
        let text = String::from_utf8(clocks).expect("authored clock records are UTF-8");
        let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
        let fields: Vec<_> = lines[2].split(',').map(str::trim).collect();
        lines[2] = format!(
            "0, {}, {}, {}, {}, 0x00000000",
            fields[1],
            fields[2],
            fields[3],
            fields[4]
                .parse::<u64>()
                .expect("fixture packet size is an integer")
                + 1
        );
        assembler
            .feed_clock((lines.join("\n") + "\n").as_bytes())
            .expect("valid fixture packet-clock chunk");
        let init = assembler
            .feed_media(&media)
            .expect("valid fixture media chunk");
        assert_eq!(
            assembler
                .feed_media(&media[init.consumed..])
                .expect_err("invalid or over-limit media input is refused"),
            ObservationError::Clock
        );
        assert_eq!(
            assembler
                .feed_media(&[])
                .expect_err("invalid or over-limit media input is refused"),
            ObservationError::Failed
        );
        let mut limited = ObservedRangeAssembler::new(origin, 0, count + 1, 16)
            .expect("valid bounded assembler limits");
        assert_eq!(
            limited
                .feed_media(&[0, 0, 1, 0, b'm', b'd', b'a', b't'])
                .expect_err("invalid or over-limit media input is refused"),
            ObservationError::Limit
        );
    }
}

#[cfg(test)]
mod late_init_tests {
    use super::*;
    #[test]
    fn replacement_init_is_typed_and_latches_observation_failure() {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("authored init parses") else {
            panic!("authored init");
        };
        let mut assembler = ObservedRangeAssembler::new(
            SourceTimestamp {
                dts: 0,
                time_base_num: 1,
                time_base_den: 1,
            },
            0,
            4,
            init.bytes.len() * 2,
        )
        .expect("bounded assembler");
        let first = assembler
            .feed_media(&init.bytes)
            .expect("initial decoder contract");
        assert!(matches!(first.event, Some(ObservedUnit::Init(_))));
        assert_eq!(
            assembler
                .feed_media(&init.bytes)
                .expect_err("replacement init rejected"),
            ObservationError::InitContractChanged
        );
        assert_eq!(
            assembler
                .feed_media(&[])
                .expect_err("failure cannot resume"),
            ObservationError::Failed
        );
    }
}
