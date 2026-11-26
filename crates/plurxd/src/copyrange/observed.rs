//! Bounded pairing of AV/canonical/packet-clock/inspection output from ONE
//! child. The pure kernel alone decides cuts and verifies every new fragment.
use super::pipes::{CopyPipeEvent, CopyPipeKind};
use crate::{
    copy_analysis::CopyRowInspection,
    copy_normalization::{CopyNormalizationContract, CopyNormalizationError, CopyNormalizers},
};
use plurx_core::{
    fmp4::{self, Fragment, FragmentReader, Init, Unit},
    hevc_configuration::{PacketInspection, RangeInspection, Trace},
    segplan::{
        observed_range::{ObservedRangeAssembler, ObservedUnit},
        packet_clock::SourceFragmentClock,
        ranges::SourceTimestamp,
        verified_copy::{VerifiedCopyKernel, VerifiedCopySegment},
        TrackDurations,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
/// What a copy is charged when it is admitted, before any fragment of it is
/// seen: the verified copy kernel's time-bounded holds, its pending AV and
/// that AV's merge clone at a cut, for one segment of the file's own rate
/// (size over duration, so audio is included). A segment never spans more
/// than 16 s of video and is cut at [`COPY_SEGMENT_MAX_BYTES`] where it can
/// be. The fragment holds join as fragments arrive ([`ObservedCopyMux::need_for`]).
///
/// [`COPY_SEGMENT_MAX_BYTES`]: plurx_core::transcode::COPY_SEGMENT_MAX_BYTES
pub(crate) fn admission_charge(
    file_size: i64,
    duration_ms: Option<i64>,
    audio_bits_per_second: u32,
) -> u64 {
    segment_window(file_size, duration_ms, audio_bits_per_second).saturating_mul(2)
}
/// The longest a segment may span, in seconds: a single fragment longer is
/// `copy_target_exceeded`.
pub(crate) const SEGMENT_STRICT_MAX_SECONDS: u32 = 16;
/// The bytes of one longest segment of the file at its own rate, capped at
/// the segment byte ceiling. A size or duration that cannot describe the
/// file is unknown, and a full segment is assumed: not positive, or a rate
/// below the copy's own audio track's, which the file carries in full (as a
/// file scanned while it was still growing reports).
fn segment_window(file_size: i64, duration_ms: Option<i64>, audio_bits_per_second: u32) -> u64 {
    let segment_max = plurx_core::transcode::COPY_SEGMENT_MAX_BYTES as u64;
    match (
        u64::try_from(file_size).ok().filter(|size| *size > 0),
        duration_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .filter(|ms| *ms > 0),
    ) {
        (Some(size), Some(ms))
            if u128::from(size) * 8 * 1000 / u128::from(ms)
                >= u128::from(audio_bits_per_second) =>
        {
            let sixteen =
                u128::from(size) * u128::from(SEGMENT_STRICT_MAX_SECONDS) * 1000 / u128::from(ms);
            u64::try_from(sixteen).unwrap_or(u64::MAX).min(segment_max)
        }
        _ => segment_max,
    }
}
/// The pairs one child may have in flight on one pipe beyond the other: a
/// protocol bound on skew, not a memory bound.
const MAX_FRAGMENTS: usize = 8;
/// What one held packet inspection costs: the record and its digest string.
const INSPECTION_BYTES: usize = std::mem::size_of::<PacketInspection>() + 64;
/// What one row inspection costs: the record, its configuration digest and
/// its hex mastering-display payload (24 bytes as 48 hex characters).
const ROW_INSPECTION_BYTES: usize = std::mem::size_of::<CopyRowInspection>() + 64 + 48;
const MAX_PACKETS: usize = 131072;

/// Each hold the mux has, in bytes, or in records for inspections. The two
/// inits it keeps are a few kilobytes and not counted.
#[derive(Clone, Copy, Debug, Default)]
struct HeldParts {
    /// Pipe bytes not yet framed and fragments waiting for their pair.
    queued: usize,
    /// The AV reader's in-progress fragment.
    av_reader: usize,
    /// The assembler's framing and its fragment waiting for a clock.
    assembler: usize,
    /// The kernel's pending AV before a cut; a cut clones it once more.
    kernel_pending: usize,
    /// Packet inspections held for rows not yet cut.
    inspections: usize,
    /// Row inspections attached to rows awaiting their segment.
    rows: usize,
}
impl HeldParts {
    fn figure(self) -> u64 {
        self.queued
            .saturating_add(self.av_reader)
            .saturating_add(self.assembler)
            .saturating_add(self.kernel_pending.saturating_mul(2))
            .saturating_add(self.inspections.saturating_mul(INSPECTION_BYTES))
            .saturating_add(self.rows.saturating_mul(ROW_INSPECTION_BYTES)) as u64
    }
}

/// Whether `events` carry a cut: a segment whose media the caller holds
/// until it is committed.
pub(crate) fn cuts(events: &[CopyMuxEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, CopyMuxEvent::Segment(_)))
}

pub(crate) struct ObservedCopySegment {
    pub(crate) segment: VerifiedCopySegment,
    pub(crate) row_inspections: Vec<CopyRowInspection>,
}
pub(crate) enum CopyMuxEvent {
    Init {
        canonical: Arc<Init>,
        av: Arc<Init>,
    },
    Origin {
        first_packet: plurx_core::segplan::packet_clock::SourcePacketObservation,
        canonical_dts: u64,
        av_dts: u64,
        timescale: u32,
    },
    Segment(ObservedCopySegment),
}

pub(crate) struct ObservedCopyMux {
    contract: Arc<CopyNormalizationContract>,
    assembler: ObservedRangeAssembler,
    av_reader: FragmentReader,
    canonical_init: Option<Init>,
    av_init: Option<Init>,
    normalizers: Option<CopyNormalizers>,
    kernel: Option<VerifiedCopyKernel>,
    canonical: VecDeque<(Fragment, SourceFragmentClock)>,
    av: VecDeque<Fragment>,
    pending_media: VecDeque<(Vec<u8>, usize)>,
    pending_clock: VecDeque<(Vec<u8>, usize)>,
    queued_bytes: usize,
    queued_fragments: usize,
    trace: Option<Trace>,
    inspected: VecDeque<PacketInspection>,
    inspection: Option<RangeInspection>,
    row_inspections: BTreeMap<u64, CopyRowInspection>,
    tracks: TrackDurations,
    retain_parameter_sets: bool,
    inspect_hevc: bool,
    first_pair: bool,
    media_eof: bool,
    clock_eof: bool,
    av_eof: bool,
    diagnostic_eof: bool,
    media_marked: bool,
    clock_marked: bool,
    failed: bool,
    /// The most this mux has held at once, for the production meter.
    peak_held: Arc<std::sync::atomic::AtomicUsize>,
    /// The largest fragment seen on either pipe.
    largest_fragment: usize,
    /// What this node can hold at all: no single hold may pass it.
    ceiling: usize,
    /// This copy's charge against the node's copy memory budget, returned
    /// when the last holder (the mux or its drive loop) drops it, however
    /// the copy ends.
    memory: Arc<crate::admission::CopyMemoryPermit>,
}
impl ObservedCopyMux {
    pub(crate) fn new(
        contract: Arc<CopyNormalizationContract>,
        origin: SourceTimestamp,
        canonical_title_start: u64,
        tracks: TrackDurations,
        retain_parameter_sets: bool,
        inspect_hevc: bool,
        memory: Arc<crate::admission::CopyMemoryPermit>,
    ) -> Result<Self, String> {
        let mut av_reader = FragmentReader::new();
        av_reader.reject_reinitialization();
        // No single hold may exceed what this node can hold at all; the copy
        // as a whole is held to its permit by its drive loop.
        let ceiling = usize::try_from(memory.ceiling()).unwrap_or(usize::MAX);
        Ok(Self {
            contract,
            assembler: ObservedRangeAssembler::new(
                origin,
                canonical_title_start,
                MAX_PACKETS,
                ceiling,
            )
            .map_err(|_| "copy_observation_limit".to_owned())?,
            av_reader,
            canonical_init: None,
            av_init: None,
            normalizers: None,
            kernel: None,
            canonical: VecDeque::new(),
            av: VecDeque::new(),
            pending_media: VecDeque::new(),
            pending_clock: VecDeque::new(),
            queued_bytes: 0,
            queued_fragments: 0,
            trace: inspect_hevc.then(Trace::default),
            inspected: VecDeque::new(),
            inspection: None,
            row_inspections: BTreeMap::new(),
            tracks,
            retain_parameter_sets,
            inspect_hevc,
            first_pair: true,
            media_eof: false,
            clock_eof: false,
            av_eof: false,
            diagnostic_eof: false,
            media_marked: false,
            clock_marked: false,
            failed: false,
            peak_held: Arc::default(),
            largest_fragment: 0,
            ceiling,
            memory,
        })
    }
    /// The copy's charge, which its drive loop resizes to [`Self::need_for`].
    pub(crate) fn memory(&self) -> Arc<crate::admission::CopyMemoryPermit> {
        Arc::clone(&self.memory)
    }
    /// Every byte this mux holds now: pipe bytes not yet framed and the
    /// fragments waiting for their pair; the AV reader's in-progress
    /// fragment; the assembler's framing and its fragment waiting for a
    /// clock; the kernel's pending AV and its merge clone at a cut; and the
    /// held packet inspections.
    pub(crate) fn held(&self) -> u64 {
        HeldParts {
            queued: self.queued_bytes,
            av_reader: self.av_reader.buffered(),
            assembler: self.assembler.held_bytes(),
            kernel_pending: self
                .kernel
                .as_ref()
                .map_or(0, |kernel| kernel.pending_bytes()),
            inspections: self.inspected.len(),
            rows: self.row_inspections.len(),
        }
        .figure()
    }
    /// What this mux may hold once it accepts `event`: what it holds now,
    /// the event's bytes, and the transient copies its next step can make,
    /// at most two of the largest fragment seen (a reader's copy of a unit
    /// out of its buffer, or the conversion of a popped pair's twins; never
    /// both at once).
    pub(crate) fn need_for(&self, event: &CopyPipeEvent) -> u64 {
        let (kind, bytes) = match event {
            CopyPipeEvent::Data(kind, bytes) => (Some(*kind), bytes.len()),
            _ => (None, 0),
        };
        // The largest fragment this event can complete, counted before it is
        // framed: the AV reader's fragment or the assembler's, with this
        // event's bytes, if larger than any seen.
        let framing = match kind {
            Some(CopyPipeKind::Av) => self.av_reader.buffered().saturating_add(bytes),
            // A canonical chunk, or a clock line that releases the
            // fragment its unframed backlog completes.
            Some(CopyPipeKind::Canonical | CopyPipeKind::Clock) => self
                .assembler
                .held_bytes()
                .saturating_add(self.pending_media_bytes())
                .saturating_add(if kind == Some(CopyPipeKind::Canonical) {
                    bytes
                } else {
                    0
                }),
            _ => 0,
        };
        let largest = self.largest_fragment.max(framing) as u64;
        self.held()
            .saturating_add(bytes as u64)
            .saturating_add(largest.saturating_mul(2))
    }
    fn pending_media_bytes(&self) -> usize {
        self.pending_media
            .iter()
            .map(|(bytes, at)| bytes.len().saturating_sub(*at))
            .sum()
    }
    /// Fit this copy's charge to `event`, then feed it: the one step both
    /// drive loops take per event. The caller applies the returned events,
    /// then calls [`Self::after_apply`]. Also returns how long the fit
    /// waited for memory, which `waiting` mirrors while it lasts.
    pub(crate) async fn step(
        &mut self,
        event: CopyPipeEvent,
        stop: &tokio_util::sync::CancellationToken,
        waiting: &super::meter::HoldMirror,
    ) -> Result<(Vec<CopyMuxEvent>, std::time::Duration), String> {
        let waited = self.fit(&event, stop, waiting).await?;
        let events = self.feed(event)?;
        self.covers(&events);
        Ok((events, waited))
    }
    /// After the events of a step are applied (a cut's segment committed and
    /// dropped), return what the copy no longer holds.
    pub(crate) async fn after_apply(&self, cut: bool) {
        if cut {
            self.settle().await;
        }
    }
    /// The charge covers everything held now and the segments about to be
    /// applied: a step whose charge fell short charged too late or returned
    /// memory too early.
    fn covers(&self, events: &[CopyMuxEvent]) {
        let segments: u64 = events
            .iter()
            .map(|event| match event {
                CopyMuxEvent::Segment(segment) => segment.segment.media.bytes.len() as u64,
                _ => 0,
            })
            .sum();
        debug_assert!(
            self.memory.bytes() >= self.held().saturating_add(segments),
            "copy memory charge {} below its hold {} and segments {}",
            self.memory.bytes(),
            self.held(),
            segments
        );
    }
    /// The most this mux has held at once, readable after it is consumed.
    pub(crate) fn peak_held(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        Arc::clone(&self.peak_held)
    }
    /// Fit this copy's charge to what accepting `event` needs, before it is
    /// fed, and return how long that waited. Growth waits (its drive loop
    /// reads no pipe meanwhile, so the child blocks on all of them together)
    /// while the node's total would pass its memory, and a background copy
    /// also waits for the budget ([`crate::admission::CopyMemoryPermit::resize`]),
    /// until there is room or `stop` ends it. More than this node can hold
    /// even alone ends the copy as too large for it.
    ///
    /// `waiting` is set for as long as the growth waits, so the copy's stall
    /// rule judges that wait as its own cause, never as a stuck producer.
    /// Growth granted at once is not a wait and leaves it untouched.
    pub(crate) async fn fit(
        &self,
        event: &CopyPipeEvent,
        stop: &tokio_util::sync::CancellationToken,
        waiting: &super::meter::HoldMirror,
    ) -> Result<std::time::Duration, String> {
        let need = self.need_for(event);
        if need <= self.memory.bytes() {
            return Ok(std::time::Duration::ZERO);
        }
        let refused = |_| "copy_range_input_limit".to_owned();
        let grow = self.memory.resize(need);
        tokio::pin!(grow);
        let first = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(grow.as_mut(), cx))
        })
        .await;
        if let std::task::Poll::Ready(grown) = first {
            return grown.map(|()| std::time::Duration::ZERO).map_err(refused);
        }
        struct Waiting<'a>(&'a super::meter::HoldMirror);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        waiting.set(true);
        let _waiting = Waiting(waiting);
        let began = tokio::time::Instant::now();
        tokio::select! {biased;
            () = stop.cancelled() => Err("copy_range_cancelled".to_owned()),
            grown = &mut grow => grown.map(|()| began.elapsed()).map_err(refused),
        }
    }
    /// After a cut's segment has been committed and dropped, return what
    /// the copy no longer holds to the pool. Called while the segment is
    /// still alive, it would hand memory still in use to another copy.
    pub(crate) async fn settle(&self) {
        let floor = self
            .held()
            .saturating_add((self.largest_fragment as u64).saturating_mul(2));
        if floor < self.memory.bytes() {
            // Shrinking never waits and never fails.
            let _ = self.memory.resize(floor).await;
        }
    }
    fn note_held(&self) {
        self.peak_held.fetch_max(
            usize::try_from(self.held()).unwrap_or(usize::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    /// Errors latch. A returned segment was checked in full; an earlier good
    /// prefix never gives credit to a later fragment or another seek.
    pub(crate) fn feed(&mut self, event: CopyPipeEvent) -> Result<Vec<CopyMuxEvent>, String> {
        if self.failed {
            return Err("copy_range_failed".into());
        }
        let result = self.feed_inner(event);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn feed_inner(&mut self, event: CopyPipeEvent) -> Result<Vec<CopyMuxEvent>, String> {
        match event {
            CopyPipeEvent::Failed(kind, error) => return Err(format!("{error}: {kind:?}")),
            CopyPipeEvent::Data(kind, bytes) => match kind {
                CopyPipeKind::Av => {
                    // Data after its own end is a pipe protocol fault, not a
                    // fact about the source's size.
                    if self.av_eof {
                        return Err("copy_observation_closed".into());
                    }
                    if self.av_reader.buffered().saturating_add(bytes.len()) > self.ceiling {
                        return Err("copy_range_input_limit".into());
                    }
                    self.av_reader.push(&bytes);
                    self.note_held();
                    while let Some(unit) =
                        self.av_reader.next_unit().map_err(av_observation_error)?
                    {
                        match unit {
                            Unit::Init(init) => {
                                if self.av_init.replace(init).is_some() {
                                    return Err("copy_init_contract_changed".into());
                                }
                            }
                            Unit::Fragment(fragment) => {
                                self.hold_fragment(fragment.len())?;
                                self.av.push_back(fragment);
                            }
                            Unit::Trailer => {}
                        }
                    }
                }
                CopyPipeKind::Canonical => {
                    if self.media_eof {
                        return Err("copy_observation_closed".into());
                    }
                    self.hold_bytes(bytes.len())?;
                    self.pending_media.push_back((bytes, 0));
                }
                CopyPipeKind::Clock => {
                    if self.clock_eof {
                        return Err("copy_observation_closed".into());
                    }
                    self.hold_bytes(bytes.len())?;
                    self.pending_clock.push_back((bytes, 0));
                }
                CopyPipeKind::Diagnostic => {
                    if self.diagnostic_eof {
                        return Err("copy_observation_closed".into());
                    }
                    if let Some(trace) = self.trace.as_mut() {
                        let inspected = &mut self.inspected;
                        trace.feed_with_packets(&bytes, |packet| {
                            if inspected.len() <= MAX_PACKETS {
                                inspected.push_back(packet);
                            }
                        });
                        if inspected.len() > MAX_PACKETS {
                            return Err("copy_inspection_limit".into());
                        }
                        // A refused trace emits no more packets, so pairing
                        // would wait on them until the hold overflowed and
                        // name the hold. End here instead. A refusal of the
                        // source's own configuration (the copy contract is
                        // one complete source-extradata VPS/SPS/PPS) is VOD's
                        // typed code; a damaged read or a parser limit is
                        // this attempt's inspection failure.
                        if let Some(reason) = trace.refusal() {
                            tracing::warn!(
                                target: "plurxd::transcode",
                                reason,
                                fault = trace.source_fault().unwrap_or("none"),
                                "Source copy HEVC trace refused"
                            );
                            return Err(if trace.refused_configuration() {
                                "hevc_configuration_unsupported"
                            } else {
                                "copy_hevc_inspection_failed"
                            }
                            .into());
                        }
                    }
                }
            },
            CopyPipeEvent::Eof(kind) => match kind {
                CopyPipeKind::Av => {
                    self.av_eof = true;
                    if self.av_reader.buffered() != 0 || !self.av_reader.saw_trailer() {
                        return Err("copy_av_truncated".into());
                    }
                }
                CopyPipeKind::Canonical => self.media_eof = true,
                CopyPipeKind::Clock => self.clock_eof = true,
                CopyPipeKind::Diagnostic => {
                    self.diagnostic_eof = true;
                    if let Some(trace) = self.trace.take() {
                        let inspected = &mut self.inspected;
                        self.inspection = Some(
                            trace
                                .finish_range(|packet| {
                                    if inspected.len() <= MAX_PACKETS {
                                        inspected.push_back(packet);
                                    }
                                })
                                .map_err(|_| "copy_hevc_inspection_failed".to_owned())?,
                        );
                        if inspected.len() > MAX_PACKETS {
                            return Err("copy_inspection_limit".into());
                        }
                    }
                }
            },
        }
        self.pump_observations()?;
        let mut events = Vec::new();
        self.establish(&mut events)?;
        self.pair(&mut events)?;
        Ok(events)
    }
    fn hold_bytes(&mut self, bytes: usize) -> Result<(), String> {
        self.queued_bytes = self
            .queued_bytes
            .checked_add(bytes)
            .ok_or("copy_range_input_limit")?;
        if self.queued_bytes > self.ceiling {
            return Err("copy_range_input_limit".into());
        }
        self.note_held();
        Ok(())
    }
    fn hold_fragment(&mut self, bytes: usize) -> Result<(), String> {
        self.hold_bytes(bytes)?;
        self.queued_fragments += 1;
        self.largest_fragment = self.largest_fragment.max(bytes);
        // One pipe this far ahead of the other is a protocol fault of the
        // child's outputs, not a fact about the source's size.
        if self.queued_fragments > MAX_FRAGMENTS {
            return Err("copy_observation_skew".into());
        }
        Ok(())
    }
    fn observed(&mut self, event: ObservedUnit) -> Result<(), String> {
        match event {
            ObservedUnit::Init(init) => {
                if self.canonical_init.replace(init).is_some() {
                    return Err("copy_init_contract_changed".into());
                }
            }
            ObservedUnit::Fragment { fragment, clock } => {
                self.hold_fragment(fragment.len())?;
                self.canonical.push_back((fragment, clock));
            }
        }
        Ok(())
    }
    fn pump_observations(&mut self) -> Result<(), String> {
        loop {
            let mut progress = false;
            if let Some((bytes, at)) = self.pending_media.front() {
                let fed = self
                    .assembler
                    .feed_media(&bytes[*at..])
                    .map_err(observation_error)?;
                let consumed = fed.consumed;
                progress |= consumed > 0 || fed.event.is_some();
                if let Some((bytes, at)) = self.pending_media.front_mut() {
                    *at += consumed;
                    self.queued_bytes -= consumed;
                    if *at == bytes.len() {
                        self.pending_media.pop_front();
                    }
                }
                if let Some(event) = fed.event {
                    self.observed(event)?;
                }
            }
            if let Some((bytes, at)) = self.pending_clock.front() {
                let fed = self
                    .assembler
                    .feed_clock(&bytes[*at..])
                    .map_err(observation_error)?;
                let consumed = fed.consumed;
                progress |= consumed > 0 || fed.event.is_some();
                if let Some((bytes, at)) = self.pending_clock.front_mut() {
                    *at += consumed;
                    self.queued_bytes -= consumed;
                    if *at == bytes.len() {
                        self.pending_clock.pop_front();
                    }
                }
                if let Some(event) = fed.event {
                    self.observed(event)?;
                }
            }
            if !progress {
                break;
            }
        }
        // A ready held fragment can be released by the last clock line even
        // when there is no following media chunk. Empty feeds drain that event.
        loop {
            let mut event = None;
            if !self.media_marked {
                event = self
                    .assembler
                    .feed_media(&[])
                    .map_err(observation_error)?
                    .event;
            }
            if event.is_none() && !self.clock_marked {
                event = self
                    .assembler
                    .feed_clock(&[])
                    .map_err(observation_error)?
                    .event;
            }
            if let Some(event) = event {
                self.observed(event)?;
            } else {
                break;
            }
        }
        if self.media_eof && self.pending_media.is_empty() && !self.media_marked {
            self.assembler.mark_media_eof().map_err(observation_error)?;
            self.media_marked = true;
        }
        if self.clock_eof && self.pending_clock.is_empty() && !self.clock_marked {
            self.assembler.mark_clock_eof().map_err(observation_error)?;
            self.clock_marked = true;
        }
        Ok(())
    }
    fn establish(&mut self, events: &mut Vec<CopyMuxEvent>) -> Result<(), String> {
        if self.normalizers.is_some() {
            return Ok(());
        }
        if self.canonical_init.is_none() || self.av_init.is_none() {
            return Ok(());
        }
        let normalizers = self
            .contract
            .begin(
                self.canonical_init.take().ok_or("copy_init_missing")?,
                self.av_init.take().ok_or("copy_init_missing")?,
            )
            .map_err(normalization_error)?;
        let canonical = Arc::new(normalizers.canonical_init().clone());
        let av = Arc::new(normalizers.av_init().clone());
        let scale = canonical.video().ok_or("copy_video_missing")?.timescale;
        let policy = fmp4::CutPolicy::new(
            plurx_core::transcode::COPY_SEGMENT_SECONDS,
            plurx_core::transcode::COPY_FIRST_SEGMENT_SECONDS,
            plurx_core::transcode::COPY_SEGMENT_MAX_BYTES,
            plurx_core::transcode::COPY_SEGMENT_MAX_SECS,
            scale,
        )
        .with_strict_max_seconds(SEGMENT_STRICT_MAX_SECONDS, scale);
        self.kernel = Some(
            VerifiedCopyKernel::new(
                (*canonical).clone(),
                (*av).clone(),
                policy,
                self.tracks,
                self.retain_parameter_sets,
                self.ceiling,
                self.ceiling,
            )
            .map_err(|error| format!("copy_kernel_init:{error:?}"))?,
        );
        self.normalizers = Some(normalizers);
        events.push(CopyMuxEvent::Init { canonical, av });
        Ok(())
    }
    fn pair(&mut self, events: &mut Vec<CopyMuxEvent>) -> Result<(), String> {
        if self.kernel.is_none() {
            return Ok(());
        }
        while let Some((canonical, clock)) = self.canonical.front() {
            let Some(av) = self.av.front() else {
                break;
            };
            let normalizers = self.normalizers.as_ref().ok_or("copy_init_missing")?;
            let video = normalizers
                .canonical_init()
                .video()
                .ok_or("copy_video_missing")?;
            let packets = canonical
                .track(video.id)
                .ok_or("copy_video_missing")?
                .sample_count();
            let first_sequence = clock
                .next_packet_sequence
                .checked_sub(packets as u64)
                .ok_or("copy_inspection_sequence")?;
            if self.inspect_hevc && self.inspected.len() < packets {
                break;
            }
            if av
                .track(
                    normalizers
                        .av_init()
                        .video()
                        .ok_or("copy_video_missing")?
                        .id,
                )
                .is_none()
            {
                return Err("copy_companion_video_missing".into());
            }
            let mut inspection = None;
            if self.inspect_hevc {
                let mut slices = 0u64;
                let mut configuration = None;
                for (offset, packet) in self.inspected.iter().take(packets).enumerate() {
                    if packet.sequence != first_sequence + offset as u64 || packet.slices == 0 {
                        return Err("copy_inspection_sequence".into());
                    }
                    if configuration
                        .as_ref()
                        .is_some_and(|digest| digest != &packet.configuration_sha256)
                    {
                        return Err("copy_init_contract_changed".into());
                    }
                    configuration = Some(packet.configuration_sha256.clone());
                    slices = slices
                        .checked_add(packet.slices)
                        .ok_or("copy_inspection_overflow")?;
                }
                // Every sample of the canonical fragment, as the legacy index
                // pass scans it. Legacy scans after Dolby Vision conversion;
                // conversion rewrites only RPU NAL units (type 62), never the
                // prefix SEI that carries 137/144, so before is the same scan.
                // An unreadable SEI is a fact about the row, not a reason to
                // end this viewer's copy: only the seal refuses it.
                let mut hdr10 = fmp4::Hdr10IndexMetadata::default();
                let hdr10_conflict = hdr10
                    .observe_fragment(canonical, normalizers.canonical_init())
                    .is_err();
                if hdr10_conflict {
                    hdr10 = fmp4::Hdr10IndexMetadata::default();
                }
                inspection = Some(CopyRowInspection {
                    dts: clock.canonical_dts,
                    duration: clock.duration,
                    packets: packets as u64,
                    slices,
                    configuration_sha256: configuration.ok_or("copy_inspection_missing")?,
                    hdr10_mastering: hdr10.mastering().map(hex::encode),
                    hdr10_light: hdr10.light(),
                    hdr10_conflict,
                });
            }
            let (mut canonical, clock) = self.canonical.pop_front().ok_or("copy_pair_missing")?;
            let mut av = self.av.pop_front().ok_or("copy_pair_missing")?;
            self.queued_fragments -= 2;
            self.queued_bytes -= canonical.len() + av.len();
            if self.inspect_hevc {
                self.inspected.drain(..packets);
            }
            let normalizers = self.normalizers.as_mut().ok_or("copy_init_missing")?;
            normalizers
                .normalize_pair(&mut canonical, &mut av)
                .map_err(normalization_error)?;
            let av_video = normalizers.av_init().video().ok_or("copy_video_missing")?;
            let av_dts = av
                .track(av_video.id)
                .ok_or("copy_video_missing")?
                .base_decode_time;
            let timescale = av_video.timescale;
            let segments = self
                .kernel
                .as_mut()
                .ok_or("copy_kernel_missing")?
                .push(&canonical, clock, av, None)
                .map_err(|error| match error {
                    plurx_core::segplan::verified_copy::VerifiedCopyError::TargetExceeded => {
                        "copy_target_exceeded".to_owned()
                    }
                    other => format!("copy_fragment_verification:{other:?}"),
                })?;
            if let Some(inspection) = inspection {
                if self
                    .row_inspections
                    .insert(inspection.dts, inspection)
                    .is_some()
                {
                    return Err("copy_row_overlap".into());
                }
            }
            if self.first_pair {
                self.first_pair = false;
                events.push(CopyMuxEvent::Origin {
                    first_packet: clock.first_packet,
                    canonical_dts: clock.canonical_dts,
                    av_dts,
                    timescale,
                });
            }
            self.outputs(segments, events)?;
        }
        Ok(())
    }
    fn outputs(
        &mut self,
        segments: Vec<VerifiedCopySegment>,
        events: &mut Vec<CopyMuxEvent>,
    ) -> Result<(), String> {
        for segment in segments {
            let mut inspections = Vec::new();
            if self.inspect_hevc {
                for row in &segment.rows {
                    inspections.push(
                        self.row_inspections
                            .remove(&row.dts)
                            .ok_or("copy_inspection_missing")?,
                    );
                }
            }
            events.push(CopyMuxEvent::Segment(ObservedCopySegment {
                segment,
                row_inspections: inspections,
            }));
        }
        Ok(())
    }
    /// Caller must first establish successful actual child exit and all four
    /// joined pipes. This validates observations only, never whole-source proof.
    pub(crate) fn finish(mut self) -> Result<(Vec<CopyMuxEvent>, Option<RangeInspection>), String> {
        if self.failed || !self.av_eof || !self.media_eof || !self.clock_eof || !self.diagnostic_eof
        {
            return Err("copy_output_incomplete".into());
        }
        self.pump_observations()?;
        self.assembler.finish().map_err(observation_error)?;
        let mut events = Vec::new();
        self.pair(&mut events)?;
        if !self.canonical.is_empty()
            || !self.pending_media.is_empty()
            || !self.pending_clock.is_empty()
            || !self.inspected.is_empty()
        {
            return Err("copy_pair_incomplete".into());
        }
        let video_id = self
            .normalizers
            .as_ref()
            .ok_or("copy_init_missing")?
            .av_init()
            .video()
            .ok_or("copy_video_missing")?
            .id;
        while let Some(audio) = self.av.pop_front() {
            if audio.track(video_id).is_some() {
                return Err("copy_pair_incomplete".into());
            }
            self.kernel
                .as_mut()
                .ok_or("copy_kernel_missing")?
                .push_audio_only(audio)
                .map_err(|error| format!("copy_audio_tail:{error:?}"))?;
        }
        let segments = self
            .kernel
            .take()
            .ok_or("copy_kernel_missing")?
            .finish()
            .map_err(|error| match error {
                plurx_core::segplan::verified_copy::VerifiedCopyError::TargetExceeded => {
                    "copy_target_exceeded".to_owned()
                }
                other => format!("copy_finish:{other:?}"),
            })?;
        self.outputs(segments, &mut events)?;
        if !self.row_inspections.is_empty() {
            return Err("copy_inspection_incomplete".into());
        }
        Ok((events, self.inspection))
    }
}

fn normalization_error(error: CopyNormalizationError) -> String {
    match error {
        CopyNormalizationError::ContractChanged | CopyNormalizationError::ParameterSetsChanged => {
            "copy_init_contract_changed".into()
        }
        other => other.to_string(),
    }
}

fn av_observation_error(error: fmp4::Fmp4Error) -> String {
    if error.unsupported_kind() == Some(fmp4::Fmp4UnsupportedKind::InitContractChanged) {
        "copy_init_contract_changed".into()
    } else {
        "copy_av_invalid".into()
    }
}
fn observation_error(error: plurx_core::segplan::observed_range::ObservationError) -> String {
    use plurx_core::segplan::observed_range::ObservationError;
    match error {
        ObservationError::InitContractChanged => "copy_init_contract_changed".into(),
        ObservationError::Clock => "copy_clock_unknown".into(),
        _ => "copy_canonical_observation_failed".into(),
    }
}

#[cfg(test)]
mod hold_tests {
    use super::{admission_charge, CopyMuxEvent, ObservedCopyMux};
    use crate::copyrange::meter::HoldMirror;
    /// A copy's audio track rate, as the recipe reserves for AAC.
    const AUDIO: u32 = 256_000;
    use crate::admission::{CopyMemoryPool, CopyPriority};
    use crate::copyrange::pipes::{CopyPipeEvent, CopyPipeKind};
    use plurx_core::fmp4::{FragmentReader, Unit};
    use std::sync::Arc;

    const AUTHORED: &[u8] =
        include_bytes!("../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4");

    /// The fixture's init bytes and its one fragment's bytes.
    fn authored() -> (
        Vec<u8>,
        Vec<u8>,
        plurx_core::fmp4::Init,
        plurx_core::fmp4::Fragment,
    ) {
        let mut reader = FragmentReader::new();
        reader.push(AUTHORED);
        let (mut init, mut fragment) = (None, None);
        while let Some(unit) = reader.next_unit().expect("authored MP4 parses") {
            match unit {
                Unit::Init(value) => init = Some(value),
                Unit::Fragment(value) => fragment = Some(value),
                Unit::Trailer => {}
            }
        }
        let (init, fragment) = (init.expect("init"), fragment.expect("fragment"));
        (init.bytes.clone(), fragment.bytes.clone(), init, fragment)
    }

    async fn mux() -> ObservedCopyMux {
        let pool = CopyMemoryPool::new(1 << 30);
        let memory = pool
            .acquire(1, CopyPriority::Live, tokio::time::Instant::now())
            .await
            .expect("memory");
        mux_with(Arc::new(memory)).await
    }

    async fn mux_with(memory: Arc<crate::admission::CopyMemoryPermit>) -> ObservedCopyMux {
        let (_, _, init, fragment) = authored();
        let mut file = crate::copy_normalization::tests::non_dolby_file();
        file.size = AUTHORED.len() as i64;
        let contract = Arc::new(
            crate::copy_normalization::CopyNormalizationContract::from_head(
                &file,
                plurx_core::transcode::CopyVideoOptions::new(false, false),
                &init,
                &fragment,
            )
            .expect("contract"),
        );
        ObservedCopyMux::new(
            contract,
            plurx_core::segplan::ranges::SourceTimestamp {
                dts: 0,
                time_base_num: 1,
                time_base_den: 1000,
            },
            0,
            plurx_core::segplan::TrackDurations {
                video_ms: 125,
                audio_ms: 0,
                audio_bits_per_second: 0,
            },
            false,
            false,
            memory,
        )
        .expect("mux")
    }

    /// A copy is admitted for one segment of the file's own rate, twice
    /// (the kernel's pending AV and its merge clone), never more than two
    /// byte-capped segments, and two full segments when the rate is unknown.
    #[test]
    fn a_copy_is_admitted_for_its_files_rate() {
        let segment_max = plurx_core::transcode::COPY_SEGMENT_MAX_BYTES as u64;
        // 1 MB/s: 16 MB per 16 s.
        assert_eq!(
            admission_charge(100_000_000, Some(100_000), AUDIO),
            32_000_000
        );
        // 70 MB/s (a 4K remux): capped at the segment byte ceiling.
        assert_eq!(
            admission_charge(7_000_000_000, Some(100_000), AUDIO),
            2 * segment_max
        );
        assert_eq!(admission_charge(100, None, AUDIO), 2 * segment_max);
        assert_eq!(admission_charge(100, Some(0), AUDIO), 2 * segment_max);
    }

    /// The held figure counts every hold the mux has: here the AV pipe's
    /// framed fragment waiting for its pair, then the canonical pipe's bytes
    /// not yet framed. What accepting the next event needs adds its bytes
    /// and two of the largest fragment seen, for the transient copies.
    #[tokio::test]
    async fn the_held_figure_counts_every_hold() {
        let (init, fragment, _, framed) = authored();
        let mut mux = mux().await;
        assert_eq!(mux.held(), 0);
        mux.feed(CopyPipeEvent::Data(CopyPipeKind::Av, init.clone()))
            .expect("AV init");
        mux.feed(CopyPipeEvent::Data(CopyPipeKind::Av, fragment.clone()))
            .expect("AV fragment");
        assert_eq!(mux.held(), framed.len() as u64, "the unpaired AV fragment");
        let half = fragment[..fragment.len() / 2].to_vec();
        let next = CopyPipeEvent::Data(CopyPipeKind::Canonical, half.clone());
        assert_eq!(
            mux.need_for(&next),
            framed.len() as u64 + half.len() as u64 + 2 * framed.len() as u64
        );
        let events = mux
            .feed(CopyPipeEvent::Data(CopyPipeKind::Canonical, init))
            .expect("canonical init");
        assert!(events
            .iter()
            .all(|event| !matches!(event, CopyMuxEvent::Segment(_))));
        mux.feed(next).expect("half a canonical fragment");
        assert!(
            mux.held() >= framed.len() as u64 + half.len() as u64,
            "the canonical bytes being framed are held too"
        );
    }

    /// Before an event is fed, the copy's charge is fitted to what it needs:
    /// a live copy grows at once; a background copy that does not fit waits
    /// (no pipe is read meanwhile), and its phase's stop ends that wait.
    #[tokio::test(start_paused = true)]
    async fn an_event_is_fed_only_once_its_memory_is_charged() {
        let (init, _, _, _) = authored();
        let event = CopyPipeEvent::Data(CopyPipeKind::Av, init.clone());
        let stop = tokio_util::sync::CancellationToken::new();
        let live = mux().await;
        live.fit(&event, &stop, &HoldMirror::default())
            .await
            .expect("a live copy grows at once");
        assert!(live.memory().bytes() >= init.len() as u64);

        let pool = CopyMemoryPool::with_ceiling(100, 1 << 40);
        let fill = pool
            .acquire(1, CopyPriority::Background, tokio::time::Instant::now())
            .await
            .expect("an empty pool grants");
        let _viewer = pool
            .acquire(99, CopyPriority::Live, tokio::time::Instant::now())
            .await
            .expect("the viewer fits");
        let background = mux_with(Arc::new(fill)).await;
        let stopping = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            stopping.cancel();
        });
        let began = tokio::time::Instant::now();
        // Bounded: a wait that ignored its stop would hang here.
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                background.fit(&event, &stop, &HoldMirror::default())
            )
            .await
            .expect("the stop ends the wait"),
            Err("copy_range_cancelled".to_owned()),
            "it waited for memory until its phase stopped"
        );
        assert_eq!(began.elapsed(), std::time::Duration::from_secs(5));
    }

    /// A live copy whose growth would pass the node's total waits for
    /// another copy to end. The wait is mirrored for exactly as long as it
    /// lasts (so the stall rule can excuse it) and its length is returned;
    /// growth granted at once leaves the mirror untouched.
    #[tokio::test(start_paused = true)]
    async fn a_live_wait_at_the_node_total_is_mirrored_and_timed() {
        let (init, _, _, _) = authored();
        let event = CopyPipeEvent::Data(CopyPipeKind::Av, init.clone());
        let stop = tokio_util::sync::CancellationToken::new();
        let waiting = HoldMirror::default();
        let quiet = mux().await;
        assert_eq!(
            quiet.fit(&event, &stop, &waiting).await,
            Ok(std::time::Duration::ZERO)
        );
        let untouched = waiting.mark();
        assert!(!waiting.held_since(untouched), "granted at once is no wait");

        let ceiling = 1 << 20;
        let pool = CopyMemoryPool::with_ceiling(1 << 30, ceiling);
        let other = pool
            .acquire(ceiling - 1, CopyPriority::Live, tokio::time::Instant::now())
            .await
            .expect("the other copy");
        let live = mux_with(Arc::new(
            pool.acquire(1, CopyPriority::Live, tokio::time::Instant::now())
                .await
                .expect("an over-budget live arrival within the node"),
        ))
        .await;
        let mark = waiting.mark();
        let ending = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(7)).await;
            drop(other);
        });
        let fitting = live.fit(&event, &stop, &waiting);
        tokio::pin!(fitting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut fitting)
                .await
                .is_err(),
            "the node's total cannot take it yet"
        );
        assert!(waiting.is_held(), "mirrored while it waits");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(30), fitting)
                .await
                .expect("the other copy's ending relieves it"),
            Ok(std::time::Duration::from_secs(7))
        );
        assert!(!waiting.is_held(), "cleared once granted");
        assert!(waiting.held_since(mark), "a wait since the mark is visible");
        ending.await.expect("ending");
    }

    /// After a cut, the charge returns to what the copy still holds; a fit
    /// that would pass the node's memory ends the copy as too large.
    #[tokio::test]
    async fn a_charge_settles_and_never_passes_the_node() {
        let (init, _, _, _) = authored();
        let pool = CopyMemoryPool::with_ceiling(1 << 30, 1 << 20);
        let memory = Arc::new(
            pool.acquire(1 << 19, CopyPriority::Live, tokio::time::Instant::now())
                .await
                .expect("memory"),
        );
        let mux = mux_with(Arc::clone(&memory)).await;
        mux.settle().await;
        assert!(memory.bytes() < 1 << 19, "an idle mux keeps only its floor");
        let huge = CopyPipeEvent::Data(CopyPipeKind::Av, vec![0; 2 << 20]);
        let stop = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            mux.fit(&huge, &stop, &HoldMirror::default()).await,
            Err("copy_range_input_limit".to_owned())
        );
        let small = CopyPipeEvent::Data(CopyPipeKind::Av, init);
        mux.fit(&small, &stop, &HoldMirror::default())
            .await
            .expect("within the node");
    }

    /// The held figure is every hold: dropping any term from it fails here.
    #[test]
    fn the_held_figure_sums_every_hold() {
        let one = |part: &str| {
            let mut parts = super::HeldParts::default();
            match part {
                "queued" => parts.queued = 1000,
                "av_reader" => parts.av_reader = 1000,
                "assembler" => parts.assembler = 1000,
                "kernel" => parts.kernel_pending = 1000,
                "inspections" => parts.inspections = 10,
                "rows" => parts.rows = 10,
                _ => unreachable!(),
            }
            parts.figure()
        };
        assert_eq!(one("queued"), 1000);
        assert_eq!(one("av_reader"), 1000);
        assert_eq!(one("assembler"), 1000);
        assert_eq!(one("kernel"), 2000, "the pending AV and its merge clone");
        assert_eq!(one("inspections"), 10 * super::INSPECTION_BYTES as u64);
        assert_eq!(one("rows"), 10 * super::ROW_INSPECTION_BYTES as u64);
    }

    /// Before a fragment is framed, the transient copies it will cost are
    /// already charged: the event that completes a larger fragment than any
    /// seen needs two of it.
    #[tokio::test]
    async fn a_larger_fragment_is_charged_before_it_is_framed() {
        let (init, fragment, _, _) = authored();
        let mut mux = mux().await;
        mux.feed(CopyPipeEvent::Data(CopyPipeKind::Av, init))
            .expect("AV init");
        let event = CopyPipeEvent::Data(CopyPipeKind::Av, fragment.clone());
        assert!(mux.need_for(&event) >= 3 * fragment.len() as u64);
    }

    /// A clock line can release the framed fragment and frame the next from
    /// the canonical backlog: that backlog is charged as framing before the
    /// clock is fed.
    #[tokio::test]
    async fn a_clock_is_charged_for_the_backlog_it_releases() {
        let (init, fragment, _, _) = authored();
        let mut mux = mux().await;
        for bytes in [init, fragment.clone(), fragment.clone()] {
            mux.feed(CopyPipeEvent::Data(CopyPipeKind::Canonical, bytes))
                .expect("canonical");
        }
        let clock = CopyPipeEvent::Data(CopyPipeKind::Clock, Vec::new());
        assert!(
            mux.need_for(&clock) >= mux.held() + 2 * 2 * fragment.len() as u64,
            "two fragments' backlog: {} over {}",
            mux.need_for(&clock),
            mux.held()
        );
    }

    /// A zero or implausible size is not a rate: such a file is admitted as
    /// unknown, for two full segments.
    #[test]
    fn an_unknown_or_implausible_size_is_not_a_zero_charge() {
        let full = 2 * plurx_core::transcode::COPY_SEGMENT_MAX_BYTES as u64;
        assert_eq!(admission_charge(0, Some(100_000), AUDIO), full);
        assert_eq!(admission_charge(-5, Some(100_000), AUDIO), full);
        // 100 bytes over 100 s, as a file scanned while still growing reports.
        assert_eq!(admission_charge(100, Some(100_000), AUDIO), full);
        // The floor is the copy's own audio rate: 500 kb/s is a rate beside
        // 256 kb/s audio, and below the 640 kb/s audio a passthrough carries.
        let rate = 500_000 / 8 * 100;
        assert!(admission_charge(rate, Some(100_000), 256_000) < full);
        assert_eq!(admission_charge(rate, Some(100_000), 640_000), full);
    }

    /// One pipe far ahead of the other is the child's protocol fault, named
    /// as such, never "too large".
    #[tokio::test]
    async fn skew_between_pipes_has_its_own_code() {
        let (init, fragment, _, _) = authored();
        let mut mux = mux().await;
        mux.feed(CopyPipeEvent::Data(CopyPipeKind::Av, init))
            .expect("AV init");
        let mut refused = None;
        for _ in 0..16 {
            if let Err(error) = mux.feed(CopyPipeEvent::Data(CopyPipeKind::Av, fragment.clone())) {
                refused = Some(error);
                break;
            }
        }
        assert_eq!(refused.as_deref(), Some("copy_observation_skew"));
    }

    /// 3 Body Problem S01E01 (file 1421, 2026-11-23): its `hvcC` carries no
    /// parameter sets (FFmpeg: "No parameter sets in the extradata"), so the
    /// trace refuses at the first packet and emits none after. The mux waited
    /// for them, queued nine unpaired fragments in 0.12 s of a real capture
    /// and named its hold, `copy_range_input_limit`. The refusal is the
    /// source's own and must end the copy as itself, at once.
    #[tokio::test]
    async fn a_refused_hevc_trace_ends_the_copy_as_unsupported_configuration() {
        let mut mux = mux().await;
        mux.inspect_hevc = true;
        mux.trace = Some(plurx_core::hevc_configuration::Trace::default());
        let trace = b"[trace_headers @ 0x1] Extradata\n\
[hevc_mp4toannexb @ 0x2] No parameter sets in the extradata\n\
[trace_headers @ 0x1] Packet: 2143 bytes, key frame, pts 0, dts 0, duration 41.\n"
            .to_vec();
        assert_eq!(
            mux.feed(CopyPipeEvent::Data(CopyPipeKind::Diagnostic, trace))
                .err()
                .as_deref(),
            Some("hevc_configuration_unsupported")
        );
    }

    /// A trace that refused for a damaged read is this attempt's inspection
    /// failure, never a verdict on the source's configuration.
    #[tokio::test]
    async fn a_damaged_hevc_trace_is_an_inspection_failure_not_a_configuration() {
        let mut mux = mux().await;
        mux.inspect_hevc = true;
        mux.trace = Some(plurx_core::hevc_configuration::Trace::default());
        let trace = b"[trace_headers @ 0x1] Failed to read unit 0 (type 33).\n".to_vec();
        assert_eq!(
            mux.feed(CopyPipeEvent::Data(CopyPipeKind::Diagnostic, trace))
                .err()
                .as_deref(),
            Some("copy_hevc_inspection_failed")
        );
    }
}
