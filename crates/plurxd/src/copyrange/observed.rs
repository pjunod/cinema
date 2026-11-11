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
pub(super) const MAX_HELD: usize = 96 * 1024 * 1024;
const MAX_FRAGMENTS: usize = 8;
const MAX_PACKETS: usize = 131072;

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
}
impl ObservedCopyMux {
    pub(crate) fn new(
        contract: Arc<CopyNormalizationContract>,
        origin: SourceTimestamp,
        canonical_title_start: u64,
        tracks: TrackDurations,
        retain_parameter_sets: bool,
        inspect_hevc: bool,
    ) -> Result<Self, String> {
        let mut av_reader = FragmentReader::new();
        av_reader.reject_reinitialization();
        Ok(Self {
            contract,
            assembler: ObservedRangeAssembler::new(
                origin,
                canonical_title_start,
                MAX_PACKETS,
                MAX_HELD,
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
        })
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
            CopyPipeEvent::Failed(_, error) => return Err(error),
            CopyPipeEvent::Data(kind, bytes) => match kind {
                CopyPipeKind::Av => {
                    if self.av_eof
                        || self.av_reader.buffered().saturating_add(bytes.len()) > MAX_HELD
                    {
                        return Err("copy_range_input_limit".into());
                    }
                    self.av_reader.push(&bytes);
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
        if self.queued_bytes > MAX_HELD {
            return Err("copy_range_input_limit".into());
        }
        Ok(())
    }
    fn hold_fragment(&mut self, bytes: usize) -> Result<(), String> {
        self.hold_bytes(bytes)?;
        self.queued_fragments += 1;
        if self.queued_fragments > MAX_FRAGMENTS {
            return Err("copy_range_input_limit".into());
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
        .with_strict_max_seconds(16, scale);
        self.kernel = Some(
            VerifiedCopyKernel::new(
                (*canonical).clone(),
                (*av).clone(),
                policy,
                self.tracks,
                self.retain_parameter_sets,
                MAX_HELD,
                MAX_HELD,
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
        loop {
            let Some((canonical, clock)) = self.canonical.front() else {
                break;
            };
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
                inspection = Some(CopyRowInspection {
                    dts: clock.canonical_dts,
                    duration: clock.duration,
                    packets: packets as u64,
                    slices,
                    configuration_sha256: configuration.ok_or("copy_inspection_missing")?,
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
