//! Bounded same-input MP4/packet-clock association, independent of pipe arrival
//! order. These observations do not authenticate source, origin, grant or EOF.

use super::packet_clock::{
    associate_fragment_clock, PacketClockParser, SourceFragmentClock, SourcePacketObservation,
    MAX_PACKET_CLOCK_LINE_BYTES,
};
use super::ranges::SourceTimestamp;
use crate::fmp4::{Fmp4Error, FragmentReader, FragmentTrimmer, Init, TrimmedFragment, Unit};
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
    /// The fragment is kept without its slice data, which nothing reads.
    Fragment {
        fragment: TrimmedFragment,
        clock: SourceFragmentClock,
    },
}

/// Where a top-level box's bytes go as they arrive.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Reader,
    Moof,
    Mdat,
}

fn media_error(error: Fmp4Error) -> ObservationError {
    match error.unsupported_kind() {
        Some(crate::fmp4::Fmp4UnsupportedKind::InitContractChanged) => {
            ObservationError::InitContractChanged
        }
        None => ObservationError::Media,
    }
}

#[derive(Debug)]
pub struct FeedProgress {
    pub consumed: usize,
    pub event: Option<ObservedUnit>,
}

/// Callers retain unconsumed input and concurrently drain both pipes. In
/// particular, media feeding makes no progress while its sole held fragment
/// awaits clocks. No async task or independent source reader is created here.
///
/// A fragment's slice data is dropped as its `mdat` arrives
/// ([`FragmentTrimmer`]), so neither the framing nor the fragment waiting for
/// its clock ever holds it.
pub struct ObservedRangeAssembler {
    /// Frames the init, the trailer and every box that is not a fragment.
    media: FragmentReader,
    /// Where the top-level box being framed goes.
    route: Route,
    /// A fragment's `moof`, arriving or waiting for its `mdat`.
    moof: Vec<u8>,
    moof_ready: bool,
    trimmer: Option<FragmentTrimmer>,
    clocks: PacketClockParser,
    packets: VecDeque<SourcePacketObservation>,
    line: Vec<u8>,
    header: Vec<u8>,
    box_remaining: usize,
    held_media_bytes: usize,
    init: Option<Init>,
    fragment: Option<TrimmedFragment>,
    next_sequence: u64,
    /// The strict-muxer decode time the previous fragment ended on.
    muxed_dts: Option<i64>,
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
            route: Route::Reader,
            moof: Vec::new(),
            moof_ready: false,
            trimmer: None,
            clocks: PacketClockParser::new(),
            packets: VecDeque::new(),
            line: Vec::new(),
            header: Vec::with_capacity(16),
            box_remaining: 0,
            held_media_bytes: 0,
            init: None,
            fragment: None,
            next_sequence: 0,
            muxed_dts: None,
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

    /// Bytes this assembler holds now: the canonical box being framed, what
    /// is kept of the fragment being framed and of the framed fragment
    /// waiting for its clock, and the clock packets queued.
    pub fn held_bytes(&self) -> usize {
        self.media
            .buffered()
            .saturating_add(self.moof.len())
            .saturating_add(self.trimmer.as_ref().map_or(0, FragmentTrimmer::held))
            .saturating_add(self.fragment.as_ref().map_or(0, TrimmedFragment::held_len))
            .saturating_add(
                self.packets
                    .len()
                    .saturating_mul(std::mem::size_of::<SourcePacketObservation>()),
            )
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
                // After the init, a `moof` and the `mdat` that must follow it
                // are a fragment, framed here so its slice data is never held.
                let kind = &self.header[4..8];
                self.route = if self.moof_ready {
                    if kind != b"mdat" {
                        return Err(ObservationError::Media);
                    }
                    let init = self.init.as_ref().ok_or(ObservationError::Media)?;
                    self.trimmer = Some(
                        FragmentTrimmer::begin(std::mem::take(&mut self.moof), &self.header, init)
                            .map_err(media_error)?,
                    );
                    self.moof_ready = false;
                    Route::Mdat
                } else if self.init.is_some() && kind == b"moof" {
                    self.moof.extend_from_slice(&self.header);
                    Route::Moof
                } else {
                    self.media.push(&self.header);
                    Route::Reader
                };
                self.held_media_bytes += header_size;
                self.header.clear();
                self.box_remaining = size - header_size;
            }
            let take = self.box_remaining.min(bytes.len() - consumed);
            let body = &bytes[consumed..consumed + take];
            match self.route {
                Route::Reader => self.media.push(body),
                Route::Moof => self.moof.extend_from_slice(body),
                Route::Mdat => self
                    .trimmer
                    .as_mut()
                    .ok_or(ObservationError::Media)?
                    .push(body),
            }
            self.held_media_bytes += take;
            consumed += take;
            self.box_remaining -= take;
            if self.box_remaining != 0 {
                continue;
            }
            match self.route {
                Route::Reader => {}
                Route::Moof => {
                    self.moof_ready = true;
                    continue;
                }
                Route::Mdat => {
                    let fragment = self
                        .trimmer
                        .take()
                        .ok_or(ObservationError::Media)?
                        .finish()
                        .map_err(media_error)?;
                    self.held_media_bytes = 0;
                    if fragment.wire_len() > self.max_media as u64 {
                        return Err(ObservationError::Limit);
                    }
                    self.fragment = Some(fragment);
                    return Ok(FeedProgress {
                        consumed,
                        event: self.ready_fragment()?,
                    });
                }
            }
            match self.media.next_unit().map_err(media_error)? {
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
                // Every fragment after the init is framed above, and the
                // reader steps over a `moof` that precedes it.
                Some(Unit::Fragment(_)) => return Err(ObservationError::Media),
                Some(Unit::Trailer) => {
                    self.held_media_bytes = 0;
                    self.trailer = true;
                }
                // A box the reader stepped over after the init is not held
                // for the next. Before it, the reader is keeping the `ftyp`.
                None if self.init.is_some() && self.media.buffered() == 0 => self.media.compact(),
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
            .kept()
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
            self.muxed_dts,
            self.origin,
            self.canonical_start,
        )
        .map_err(|_| ObservationError::Clock)?;
        drop(self.packets.drain(..count));
        self.next_sequence = clock.next_packet_sequence;
        self.muxed_dts = Some(clock.last_muxed_dts);
        let fragment = self.fragment.take().ok_or(ObservationError::Media)?;
        Ok(Some(ObservedUnit::Fragment { fragment, clock }))
    }

    pub fn mark_media_eof(&mut self) -> Result<(), ObservationError> {
        if self.failed {
            return Err(ObservationError::Failed);
        }
        if self.box_remaining != 0
            || !self.header.is_empty()
            || self.media.buffered() != 0
            || !self.moof.is_empty()
        {
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
            || !self.moof.is_empty()
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
    /// Clock packets queued ahead of their media are held bytes too.
    #[test]
    fn queued_clock_packets_are_held() {
        let (media, clocks, origin, count, _) = fixture();
        let mut assembler = ObservedRangeAssembler::new(origin, 23, count + 1, media.len())
            .expect("valid bounded assembler limits");
        assert_eq!(assembler.held_bytes(), 0);
        assembler
            .feed_clock(&clocks)
            .expect("valid fixture packet-clock chunk");
        assert!(
            assembler.held_bytes()
                >= assembler.packets.len() * std::mem::size_of::<SourcePacketObservation>()
        );
        assert!(!assembler.packets.is_empty());
    }

    /// A fragment's slice data is dropped as its `mdat` arrives: from its
    /// first byte to its release, the assembler holds no more than what is
    /// kept of it, and what it releases is that, associated with its clock.
    #[test]
    fn a_fragment_is_never_held_with_its_slice_data() {
        let stream = crate::testfixtures::with_dolby_vision_rpus(
            &crate::testfixtures::video_only_pipe("closed-gop"),
        );
        let mut reader = FragmentReader::new();
        reader.push(&stream);
        let Some(Unit::Init(init)) = reader.next_unit().expect("init") else {
            panic!("init");
        };
        let Some(Unit::Fragment(whole)) = reader.next_unit().expect("fragment") else {
            panic!("fragment");
        };
        let kept = crate::testfixtures::trimmed(&init, &whole, usize::MAX);
        assert!(kept.held_len() < whole.len());
        let video = init.video().expect("video");
        let track = whole.track(video.id).expect("video");
        let mut clocks = format!("#tb 0: 1/{}\n#media_type 0: video\n", video.timescale);
        let mut dts = 0i64;
        for sample in track.samples() {
            clocks.push_str(&format!(
                "0, {}, {}, {}, {}, 0x00000000\n",
                dts,
                dts + sample.cto,
                sample.duration,
                sample.size
            ));
            dts += i64::from(sample.duration);
        }
        let origin = SourceTimestamp {
            dts: 0,
            time_base_num: 1,
            time_base_den: video.timescale,
        };
        let mut assembler =
            ObservedRangeAssembler::new(origin, 0, track.sample_count() + 1, stream.len())
                .expect("valid bounded assembler limits");
        let media = [init.bytes.as_slice(), whole.bytes.as_slice()].concat();
        let (mut fed, mut peak) = (0, 0);
        while fed < media.len() {
            let progress = assembler
                .feed_media(&media[fed..(fed + 512).min(media.len())])
                .expect("valid media");
            fed += progress.consumed;
            assert!(!matches!(
                progress.event,
                Some(ObservedUnit::Fragment { .. })
            ));
            if fed > init.bytes.len() {
                peak = peak.max(assembler.held_bytes());
            }
        }
        assert!(
            peak <= kept.held_len() + 8,
            "held {peak} of a {} byte fragment, of which {} are kept",
            whole.len(),
            kept.held_len()
        );
        assert_eq!(assembler.held_bytes(), kept.held_len());
        let Some(ObservedUnit::Fragment { fragment, clock }) = assembler
            .feed_clock(clocks.as_bytes())
            .expect("valid clocks")
            .event
        else {
            panic!("the clocks release the fragment");
        };
        assert_eq!(fragment, kept);
        assert_eq!(clock.duration, track.duration());
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
