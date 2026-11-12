//! The copy index rides the content-analysis pass.
//!
//! The fragment-index pass already reads the whole file once and writes the
//! canonical video-only stream of its lead identity on `pipe:1`. That stream
//! is what a v3 copy-index row is defined on. This module turns the same read
//! into the title's whole-title copy observation, so the Analysis worker can
//! seal and publish the v3 index without a second pass over the video. The
//! standalone form of the same job is a row-less demand filled by
//! `copyrange::fill_copy_gap`.
//!
//! What it adds to the pass, and nothing else:
//!
//! - **One output.** The packet-clock block the copy engine uses
//!   (`transcode::copy_packet_clock_output_args`) is appended after the index
//!   outputs, on inherited descriptor 7. It is never inside
//!   `copy_index_pipe_args*`, so no cluster cache key changes, and the pass
//!   takes no global option: `-copyts` would move every other output of the
//!   pass, the stored subtitle tracks among them.
//! - **One clock correction.** Without `-copyts` FFmpeg's input clock sits
//!   the format start time before the source's. The pass's own probe of the
//!   held descriptor gives that start and the video stream's first
//!   presentation time; the source origin is recovered from the first and
//!   checked against the second. Anything inexact abandons the ride.
//! - **No second derivation.** The canonical bytes go through the engine's
//!   own packet-clock association, head normalization contract, row
//!   measurement (`CopyCompanionVerifier`, the stream standing as its own
//!   companion: the index holds no AV) and row inspection.
//!
//! The ride can only ever add an answer. Its clock reader drains descriptor 7
//! to the end of the file whatever happens, so FFmpeg never waits on it. Its
//! observer runs behind a bounded backlog that the pass's readers never wait
//! on: when it falls behind, or anything it sees is not what the engine would
//! accept, the ride is abandoned and the pass builds its index as before.
use crate::copy_analysis::{AnalysisCopyRows, CopyRecipe, CopyRowInspection, PendingCopyRide};
use crate::copy_normalization::{CanonicalNormalizer, CopyNormalizationContract};
use crate::copyrange::{
    copy_cut_policy, head_origin, inspect_canonical_row, COPY_TARGET_SECONDS, MAX_HELD, MAX_PACKETS,
};
use plurx_core::{
    domain::MediaFile,
    fmp4::{Fragment, Init},
    hevc_configuration::{PacketInspection, RangeInspection},
    segplan::{
        companion::CopyCompanionVerifier,
        copy_cursor::CopyPlanCursor,
        observed_range::{ObservedRangeAssembler, ObservedUnit},
        packet_clock::{PacketClockParser, SourceFragmentClock, SourcePacketObservation},
        ranges::{RangeBoundaries, SourceTimestamp},
        IndexRow,
    },
    transcode::{CopyVideoOptions, VodSourceClock},
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

/// The inherited descriptor the pass's packet clock is written to. The
/// source is 3 and the sibling identities' outputs are 4 to 6.
pub(crate) const CLOCK_FD: i32 = 7;

/// How far the observer may fall behind the pass's readers before the ride is
/// abandoned: the bytes of everything handed to it and not yet taken.
const BACKLOG_BYTES: usize = 32 * 1024 * 1024;

/// The clock's header and first packet line, read before the origin exists.
const MAX_HEAD_CLOCK: usize = 64 * 1024;

/// What the pass's own probe of the held descriptor says about the clock:
/// the container's start time and the selected video stream's time base and
/// first presentation time, all on the source's own clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProbedCopyClock {
    format_start_micros: i64,
    time_base: (u32, u32),
    video_start_pts: i64,
}

impl ProbedCopyClock {
    /// From the index metadata probe's document. None when the container
    /// reports no start time or the first video stream no time base or start:
    /// the engine refuses a title whose format start is unknown, and so does
    /// the ride.
    pub(crate) fn from_probe(raw: &str) -> Option<Self> {
        let document: serde_json::Value = serde_json::from_str(raw).ok()?;
        let start = document.pointer("/format/start_time")?.as_str()?;
        let VodSourceClock::ReportedOrigin(format_start_micros) =
            VodSourceClock::from_probe_start_time(Some(start)).ok()?
        else {
            return None;
        };
        // The same stream the index map `0:v:0?` selects.
        let stream = document.get("streams")?.as_array()?.iter().find(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
        })?;
        let (num, den) = stream.get("time_base")?.as_str()?.split_once('/')?;
        let time_base = (
            num.parse::<u32>().ok().filter(|value| *value != 0)?,
            den.parse::<u32>().ok().filter(|value| *value != 0)?,
        );
        let video_start_pts = stream.get("start_pts")?.as_i64()?;
        Some(Self {
            format_start_micros,
            time_base,
            video_start_pts,
        })
    }

    /// The same clock claiming another first presentation time.
    #[cfg(test)]
    pub(crate) fn with_start_pts_for_test(mut self, video_start_pts: i64) -> Self {
        self.video_start_pts = video_start_pts;
        self
    }

    /// The source origin of a title whose first clock packet, on FFmpeg's
    /// input clock, is `first`.
    ///
    /// FFmpeg without `-copyts` adds `-start_time`, rescaled from
    /// microseconds, to every packet timestamp. Only a start that is a whole
    /// number of stream ticks is undone here, so no rounding rule is assumed;
    /// and the corrected first presentation time must be the one the probe
    /// reported for the stream, which FFmpeg never shifted. Either failing
    /// means this clock cannot be placed on the source's.
    fn source_origin(&self, first: &SourcePacketObservation) -> Result<SourceTimestamp, String> {
        let unknown = || "copy_clock_unknown".to_owned();
        if (first.time_base_num, first.time_base_den) != self.time_base {
            return Err(unknown());
        }
        let numerator = i128::from(self.format_start_micros) * i128::from(self.time_base.1);
        let denominator = 1_000_000_i128 * i128::from(self.time_base.0);
        if numerator % denominator != 0 {
            return Err(unknown());
        }
        let start = i64::try_from(numerator / denominator).map_err(|_| unknown())?;
        let dts = first.dts.checked_add(start).ok_or_else(unknown)?;
        let pts = first.pts.checked_add(start).ok_or_else(unknown)?;
        if pts != self.video_start_pts {
            return Err(unknown());
        }
        Ok(SourceTimestamp {
            dts,
            time_base_num: first.time_base_num,
            time_base_den: first.time_base_den,
        })
    }
}

/// One pass's decision to ride: the lead identity whose canonical stream is
/// observed, and the probed clock that places it.
pub(crate) struct CopyRidePlan {
    video: CopyVideoOptions,
    clock: ProbedCopyClock,
    backlog_bytes: usize,
    #[cfg(test)]
    stall_observer: bool,
}

impl CopyRidePlan {
    /// The ride of a pass whose lead output is `video`, or None when this
    /// title's clock cannot be placed: no probed clock, or a transport
    /// stream, whose discontinuities FFmpeg corrects without `-copyts` and
    /// the engine, with it, does not.
    pub(crate) fn for_pass(
        file: &MediaFile,
        video: CopyVideoOptions,
        clock: Option<ProbedCopyClock>,
    ) -> Option<Self> {
        if crate::subtitle_source::is_mpegts_container(file) {
            return None;
        }
        Some(Self {
            video,
            clock: clock?,
            backlog_bytes: BACKLOG_BYTES,
            #[cfg(test)]
            stall_observer: false,
        })
    }

    /// The clock output's argv, to follow the pass's index outputs.
    pub(crate) fn args(&self, file: &MediaFile) -> Vec<String> {
        plurx_core::transcode::copy_packet_clock_output_args(
            file,
            self.video,
            &format!("pipe:{CLOCK_FD}"),
        )
    }

    /// An observer that never takes anything, behind a backlog of
    /// `backlog_bytes`: the stalled consumer the pass must not wait on.
    #[cfg(test)]
    pub(crate) fn stalled_for_test(mut self, backlog_bytes: usize) -> Self {
        self.stall_observer = true;
        self.backlog_bytes = backlog_bytes;
        self
    }
}

enum RideEvent {
    Canonical(Vec<u8>),
    CanonicalEof,
    Clock(Vec<u8>),
    ClockEof,
    Packet(PacketInspection),
    TraceEnd(Result<RangeInspection, String>),
}

impl RideEvent {
    fn weight(&self) -> usize {
        match self {
            Self::Canonical(bytes) | Self::Clock(bytes) => bytes.len(),
            Self::Packet(packet) => {
                std::mem::size_of::<PacketInspection>() + packet.configuration_sha256.len()
            }
            Self::CanonicalEof | Self::ClockEof | Self::TraceEnd(_) => 0,
        }
    }
}

/// What the taps and the observer share. It holds no sender, so the
/// observer's inbox closes when the last tap is dropped.
struct TapShared {
    /// Bytes handed over and not yet taken by the observer.
    queued: AtomicUsize,
    limit: usize,
    abandoned: AtomicBool,
    /// Rows observed so far, for the pass's progress report.
    rows: AtomicUsize,
}

/// Rows a ride has observed so far, for the pass's progress report.
#[derive(Clone)]
pub(crate) struct CopyRideMeter(Arc<TapShared>);
impl CopyRideMeter {
    pub(crate) fn rows(&self) -> usize {
        self.0.rows.load(Ordering::Relaxed)
    }
}

/// Where the pass's readers hand the ride what they read. Nothing here ever
/// waits: an offer past the backlog bound abandons the ride instead. Every
/// tap is dropped by its reader when that reader ends, which is what tells
/// the observer that nothing more is coming.
#[derive(Clone)]
pub(crate) struct CopyRideTap {
    shared: Arc<TapShared>,
    sender: tokio::sync::mpsc::UnboundedSender<RideEvent>,
}

impl CopyRideTap {
    fn offer(&self, event: RideEvent) {
        if self.shared.abandoned.load(Ordering::Acquire) {
            return;
        }
        let weight = event.weight();
        let queued = self.shared.queued.fetch_add(weight, Ordering::AcqRel) + weight;
        if queued > self.shared.limit || self.sender.send(event).is_err() {
            self.shared.abandoned.store(true, Ordering::Release);
        }
    }
    /// Bytes the lead output's reader just read.
    pub(crate) fn canonical(&self, bytes: &[u8]) {
        self.offer(RideEvent::Canonical(bytes.to_vec()));
    }
    pub(crate) fn canonical_eof(&self) {
        self.offer(RideEvent::CanonicalEof);
    }
    /// One packet the header trace closed.
    pub(crate) fn packet(&self, packet: PacketInspection) {
        self.offer(RideEvent::Packet(packet));
    }
    /// The header trace reached the end of the child's stderr.
    pub(crate) fn trace_end(&self, end: Result<RangeInspection, String>) {
        self.offer(RideEvent::TraceEnd(end));
    }
}

/// The lead output's reader, also handing every byte it reads to the ride.
/// The index reads exactly what it would have read; the tap takes a copy and
/// never delays or fails the read.
pub(crate) struct RideTee<R> {
    inner: R,
    tap: CopyRideTap,
    ended: bool,
}

impl<R> RideTee<R> {
    pub(crate) fn new(inner: R, tap: CopyRideTap) -> Self {
        Self {
            inner,
            tap,
            ended: false,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for RideTee<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let wanted = buffer.remaining();
        let poll = std::pin::Pin::new(&mut self.inner).poll_read(context, buffer);
        if let std::task::Poll::Ready(Ok(())) = &poll {
            let read = &buffer.filled()[before..];
            if !read.is_empty() {
                self.tap.canonical(read);
            } else if wanted != 0 && !self.ended {
                self.ended = true;
                self.tap.canonical_eof();
            }
        }
        poll
    }
}

/// The pipe the child writes its packet clock to: the parent's reader,
/// registered with the runtime before the child exists, and the writer the
/// child inherits as descriptor [`CLOCK_FD`]. Both ends are close-on-exec.
pub(crate) fn clock_pipe(
) -> std::io::Result<(tokio::net::unix::pipe::Receiver, std::os::fd::OwnedFd)> {
    let (reader, writer) = std::io::pipe()?;
    let reader = tokio::net::unix::pipe::Receiver::from_owned_fd(reader.into())?;
    Ok((reader, writer.into()))
}

/// One running ride: the clock reader, the observer and the tap between the
/// pass's readers and it.
pub(crate) struct CopyRide {
    /// Handed to the lead output's reader and the header trace's, once each.
    tap: CopyRideTap,
    shared: Arc<TapShared>,
    video: CopyVideoOptions,
    file_id: i64,
    clock_reader: tokio::task::JoinHandle<()>,
    observer: tokio::task::JoinHandle<Result<AnalysisCopyRows, String>>,
}

impl CopyRide {
    /// Start reading `clock`, the parent's end of the child's descriptor 7.
    /// The reader runs to the end of the file whether or not anything is
    /// still listening, so the child never blocks on that descriptor.
    pub(crate) fn start(
        plan: CopyRidePlan,
        file: &MediaFile,
        clock: impl AsyncRead + Unpin + Send + 'static,
    ) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(TapShared {
            queued: AtomicUsize::new(0),
            limit: plan.backlog_bytes,
            abandoned: AtomicBool::new(false),
            rows: AtomicUsize::new(0),
        });
        let tap = CopyRideTap {
            shared: Arc::clone(&shared),
            sender,
        };
        let clock_reader = tokio::spawn(read_clock(clock, tap.clone()));
        let observer = CanonicalObserver::new(file.clone(), plan.video, plan.clock);
        #[cfg(test)]
        let stall = plan.stall_observer;
        #[cfg(not(test))]
        let stall = false;
        let observer = tokio::spawn(observe(receiver, Arc::clone(&shared), observer, stall));
        Self {
            tap,
            shared,
            video: plan.video,
            file_id: file.id,
            clock_reader,
            observer,
        }
    }

    pub(crate) fn tap(&self) -> CopyRideTap {
        self.tap.clone()
    }

    pub(crate) fn meter(&self) -> CopyRideMeter {
        CopyRideMeter(Arc::clone(&self.shared))
    }

    /// The whole-title rows, when the pass's lead output built and the ride
    /// saw the same stream through to its end.
    ///
    /// The caller has reaped the child, read every output and joined its
    /// stderr reader, so every tap but this ride's own is dropped and the
    /// clock's writers are closed. The clock reader is therefore at the end
    /// of its file, and the observer's inbox closes behind what it still
    /// holds: it ends after at most its bounded backlog, on no clock of its
    /// own. An abandoned ride, or one whose pass did not build, is stopped
    /// rather than waited for. Both tasks are joined before this returns.
    pub(crate) async fn finish(self, lead_built: bool) -> Option<PendingCopyRide> {
        let Self {
            tap,
            shared,
            video,
            file_id,
            clock_reader,
            observer,
        } = self;
        drop(tap);
        let abandoned = shared.abandoned.load(Ordering::Acquire);
        let _ = clock_reader.await;
        let observed = if lead_built && !abandoned {
            match observer.await {
                Ok(result) => result,
                Err(error) => Err(format!("copy ride observer failed: {error}")),
            }
        } else {
            observer.abort();
            let _ = observer.await;
            Err(if abandoned {
                "copy_ride_backlog"
            } else {
                "the pass's lead output did not build"
            }
            .to_owned())
        };
        match observed {
            Ok(rows) => {
                tracing::info!(
                    file_id,
                    rows = rows.rows.len(),
                    "the fragment-index pass also observed this file's whole copy index"
                );
                Some(PendingCopyRide { video, rows })
            }
            Err(reason) => {
                if lead_built {
                    tracing::info!(
                        file_id,
                        %reason,
                        "the fragment-index pass kept no copy index observation"
                    );
                }
                None
            }
        }
    }
}

/// Drain the child's packet clock to the end of the file. What is read goes
/// to the ride while it is still listening and nowhere once it is not.
async fn read_clock(mut clock: impl AsyncRead + Unpin, tap: CopyRideTap) {
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        match clock.read(&mut buffer).await {
            Ok(0) => {
                tap.offer(RideEvent::ClockEof);
                return;
            }
            Ok(read) => tap.offer(RideEvent::Clock(buffer[..read].to_vec())),
            // The descriptor is gone; there is nothing left to keep open.
            Err(_) => {
                tap.shared.abandoned.store(true, Ordering::Release);
                return;
            }
        }
    }
}

/// Take what the taps hand over until every stream has ended or every tap
/// is gone. It waits on nothing but its own inbox.
async fn observe(
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<RideEvent>,
    shared: Arc<TapShared>,
    mut observer: CanonicalObserver,
    stall: bool,
) -> Result<AnalysisCopyRows, String> {
    if stall {
        std::future::pending::<()>().await;
    }
    while let Some(event) = receiver.recv().await {
        shared.queued.fetch_sub(event.weight(), Ordering::AcqRel);
        if shared.abandoned.load(Ordering::Acquire) {
            return Err("copy_ride_backlog".into());
        }
        if let Err(reason) = observer.feed(event) {
            // Stop the pass's readers copying for an observer that is gone.
            shared.abandoned.store(true, Ordering::Release);
            return Err(reason);
        }
        shared.rows.store(observer.rows.len(), Ordering::Relaxed);
        if observer.ended() {
            break;
        }
    }
    observer.finish()
}

/// The canonical-only observer: the engine's observed mux without the AV
/// half. Media and clock bytes are paired by the engine's assembler, the
/// first fragment establishes the engine's normalization contract, and each
/// fragment is inspected, normalized and measured by the engine's own code.
struct CanonicalObserver {
    file: MediaFile,
    video: CopyVideoOptions,
    probed: ProbedCopyClock,
    inspect_hevc: bool,
    max_rows: usize,
    /// Clock bytes held until the first packet line names the origin.
    clock_head: Vec<u8>,
    source_origin: Option<SourceTimestamp>,
    assembler: Option<ObservedRangeAssembler>,
    pending_media: VecDeque<(Vec<u8>, usize)>,
    pending_clock: VecDeque<(Vec<u8>, usize)>,
    held_bytes: usize,
    raw_init: Option<Init>,
    established: Option<Established>,
    fragments: VecDeque<(Fragment, SourceFragmentClock)>,
    inspected: VecDeque<PacketInspection>,
    inspection: Option<RangeInspection>,
    rows: Vec<IndexRow>,
    row_inspections: Vec<CopyRowInspection>,
    media_eof: bool,
    clock_eof: bool,
    trace_eof: bool,
    media_marked: bool,
    clock_marked: bool,
}

/// What the first canonical fragment establishes for the rest of the title.
struct Established {
    contract: CopyNormalizationContract,
    normalizer: CanonicalNormalizer,
    verifier: CopyCompanionVerifier,
    cursor: CopyPlanCursor,
    pending_ticks: u64,
    max_ticks: u64,
}

impl CanonicalObserver {
    fn new(file: MediaFile, video: CopyVideoOptions, probed: ProbedCopyClock) -> Self {
        // The engine's own predicate (`copyrange::fill_copy_gap`).
        let inspect_hevc = file.video_codec.as_deref() == Some("hevc");
        Self {
            file,
            video,
            probed,
            inspect_hevc,
            max_rows: plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
            clock_head: Vec::new(),
            source_origin: None,
            assembler: None,
            pending_media: VecDeque::new(),
            pending_clock: VecDeque::new(),
            held_bytes: 0,
            raw_init: None,
            established: None,
            fragments: VecDeque::new(),
            inspected: VecDeque::new(),
            inspection: None,
            rows: Vec::new(),
            row_inspections: Vec::new(),
            media_eof: false,
            clock_eof: false,
            trace_eof: false,
            media_marked: false,
            clock_marked: false,
        }
    }

    /// Every stream this observer reads has ended.
    fn ended(&self) -> bool {
        self.media_eof && self.clock_eof && (self.trace_eof || !self.inspect_hevc)
    }

    fn hold(&mut self, bytes: usize) -> Result<(), String> {
        self.held_bytes = self
            .held_bytes
            .checked_add(bytes)
            .filter(|held| *held <= MAX_HELD)
            .ok_or("copy_range_input_limit")?;
        Ok(())
    }

    fn feed(&mut self, event: RideEvent) -> Result<(), String> {
        match event {
            RideEvent::Canonical(bytes) => {
                if self.media_eof {
                    return Err("copy_observation_closed".into());
                }
                self.hold(bytes.len())?;
                self.pending_media.push_back((bytes, 0));
            }
            RideEvent::CanonicalEof => self.media_eof = true,
            RideEvent::Clock(bytes) => {
                if self.clock_eof {
                    return Err("copy_observation_closed".into());
                }
                if self.assembler.is_some() {
                    self.hold(bytes.len())?;
                    self.pending_clock.push_back((bytes, 0));
                } else {
                    self.clock_head.extend_from_slice(&bytes);
                    self.establish_origin()?;
                }
            }
            RideEvent::ClockEof => {
                if self.assembler.is_none() {
                    return Err("copy_head_clock_missing".into());
                }
                self.clock_eof = true;
            }
            RideEvent::Packet(packet) => {
                if self.trace_eof {
                    return Err("copy_observation_closed".into());
                }
                if self.inspect_hevc {
                    if self.inspected.len() >= MAX_PACKETS {
                        return Err("copy_inspection_limit".into());
                    }
                    self.inspected.push_back(packet);
                }
            }
            RideEvent::TraceEnd(end) => {
                self.trace_eof = true;
                if self.inspect_hevc {
                    self.inspection =
                        Some(end.map_err(|_| "copy_hevc_inspection_failed".to_owned())?);
                }
            }
        }
        self.pump()?;
        self.measure()
    }

    /// Find the title's first clock packet in the held head of the clock
    /// output. It names both origins: FFmpeg's, which every later packet is
    /// measured from, and the source's, which the observation reports.
    fn establish_origin(&mut self) -> Result<(), String> {
        let Some(complete) = self.clock_head.iter().rposition(|byte| *byte == b'\n') else {
            if self.clock_head.len() > MAX_HEAD_CLOCK {
                return Err("copy_head_output_limit".into());
            }
            return Ok(());
        };
        let text = std::str::from_utf8(&self.clock_head[..=complete])
            .map_err(|_| "copy_head_clock_encoding".to_owned())?;
        let mut parser = PacketClockParser::new();
        let mut first = None;
        for line in text.lines() {
            if let Some(packet) = parser
                .push_line(line)
                .map_err(|_| "copy_head_clock_invalid".to_owned())?
            {
                first = Some(packet);
                break;
            }
        }
        let Some(first) = first else {
            if self.clock_head.len() > MAX_HEAD_CLOCK {
                return Err("copy_head_output_limit".into());
            }
            return Ok(());
        };
        let origin = head_origin(&first)?;
        self.source_origin = Some(self.probed.source_origin(&first)?);
        self.assembler = Some(
            ObservedRangeAssembler::new(origin, 0, MAX_PACKETS, MAX_HELD)
                .map_err(|_| "copy_observation_limit".to_owned())?,
        );
        // The assembler reads the clock from its first byte.
        let head = std::mem::take(&mut self.clock_head);
        self.hold(head.len())?;
        self.pending_clock.push_back((head, 0));
        Ok(())
    }

    fn observed(&mut self, unit: ObservedUnit) -> Result<(), String> {
        match unit {
            ObservedUnit::Init(init) => {
                if self.raw_init.replace(init).is_some() {
                    return Err("copy_init_contract_changed".into());
                }
            }
            ObservedUnit::Fragment { fragment, clock } => {
                self.hold(fragment.len())?;
                self.fragments.push_back((fragment, clock));
            }
        }
        Ok(())
    }

    /// The engine's media and clock pairing (`ObservedCopyMux`'s pump), with
    /// this observer's own held-byte bound.
    fn pump(&mut self) -> Result<(), String> {
        if self.assembler.is_none() {
            return Ok(());
        }
        loop {
            let mut progress = false;
            if let Some((bytes, at)) = self.pending_media.front() {
                let fed = self
                    .assembler
                    .as_mut()
                    .ok_or("copy_observation_closed")?
                    .feed_media(&bytes[*at..])
                    .map_err(observation_error)?;
                progress |= fed.consumed > 0 || fed.event.is_some();
                self.consumed(true, fed.consumed);
                if let Some(event) = fed.event {
                    self.observed(event)?;
                }
            }
            if let Some((bytes, at)) = self.pending_clock.front() {
                let fed = self
                    .assembler
                    .as_mut()
                    .ok_or("copy_observation_closed")?
                    .feed_clock(&bytes[*at..])
                    .map_err(observation_error)?;
                progress |= fed.consumed > 0 || fed.event.is_some();
                self.consumed(false, fed.consumed);
                if let Some(event) = fed.event {
                    self.observed(event)?;
                }
            }
            if !progress {
                break;
            }
        }
        // A held fragment can be released by the last clock line with no
        // media chunk after it; empty feeds drain that event.
        loop {
            let assembler = self.assembler.as_mut().ok_or("copy_observation_closed")?;
            let mut event = None;
            if !self.media_marked {
                event = assembler.feed_media(&[]).map_err(observation_error)?.event;
            }
            if event.is_none() && !self.clock_marked {
                event = assembler.feed_clock(&[]).map_err(observation_error)?.event;
            }
            match event {
                Some(event) => self.observed(event)?,
                None => break,
            }
        }
        let assembler = self.assembler.as_mut().ok_or("copy_observation_closed")?;
        if self.media_eof && self.pending_media.is_empty() && !self.media_marked {
            assembler.mark_media_eof().map_err(observation_error)?;
            self.media_marked = true;
        }
        if self.clock_eof && self.pending_clock.is_empty() && !self.clock_marked {
            assembler.mark_clock_eof().map_err(observation_error)?;
            self.clock_marked = true;
        }
        Ok(())
    }

    fn consumed(&mut self, media: bool, consumed: usize) {
        let queue = if media {
            &mut self.pending_media
        } else {
            &mut self.pending_clock
        };
        if let Some((bytes, at)) = queue.front_mut() {
            *at += consumed;
            if *at == bytes.len() {
                queue.pop_front();
            }
        }
        self.held_bytes = self.held_bytes.saturating_sub(consumed);
    }

    /// Turn every paired fragment whose packets have all been inspected into
    /// its row, exactly as the engine's mux does for the canonical half.
    fn measure(&mut self) -> Result<(), String> {
        while let Some((canonical, clock)) = self.fragments.front() {
            if self.established.is_none() {
                let raw_init = self.raw_init.clone().ok_or("copy_init_missing")?;
                let contract = CopyNormalizationContract::from_head(
                    &self.file, self.video, &raw_init, canonical,
                )
                .map_err(|error| error.to_string())?;
                let normalizer = contract
                    .begin_canonical(raw_init)
                    .map_err(|error| error.to_string())?;
                let served = normalizer.canonical_init().clone();
                let timescale = served.video().ok_or("copy_video_missing")?.timescale;
                // The index holds no AV. The canonical stream is its own
                // companion, so the verifier's row is the engine's row.
                let verifier = CopyCompanionVerifier::new(served.clone(), served)
                    .map_err(|error| format!("copy_kernel_init:{error:?}"))?;
                let recipe = CopyRecipe::playback_default(&self.file, self.video);
                self.established = Some(Established {
                    contract,
                    normalizer,
                    verifier,
                    cursor: CopyPlanCursor::new(
                        timescale,
                        copy_cut_policy(timescale),
                        recipe.tracks(&self.file),
                    ),
                    pending_ticks: 0,
                    max_ticks: u64::from(timescale) * u64::from(COPY_TARGET_SECONDS),
                });
            }
            let established = self.established.as_mut().ok_or("copy_init_missing")?;
            let video = established
                .normalizer
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
            let inspection = if self.inspect_hevc {
                Some(inspect_canonical_row(
                    canonical,
                    established.normalizer.canonical_init(),
                    clock,
                    first_sequence,
                    self.inspected.iter().take(packets),
                )?)
            } else {
                None
            };
            let (mut canonical, clock) = self.fragments.pop_front().ok_or("copy_pair_missing")?;
            self.held_bytes = self.held_bytes.saturating_sub(canonical.len());
            if self.inspect_hevc {
                self.inspected.drain(..packets);
            }
            established
                .normalizer
                .normalize(&mut canonical)
                .map_err(|error| error.to_string())?;
            let row = established
                .verifier
                .verify(&canonical, clock, &canonical)
                .map_err(|error| format!("copy_fragment_verification:{error:?}"))?;
            // What the engine's kernel refuses for any audio selection: a
            // row, or a segment of rows, longer than the copy target.
            if row.duration > established.max_ticks {
                return Err("copy_target_exceeded".into());
            }
            let completed = established
                .cursor
                .push(&row)
                .map_err(|error| format!("copy_fragment_verification:{error:?}"))?;
            let ticks = if completed.is_some() {
                0
            } else {
                established.pending_ticks
            };
            let ticks = ticks
                .checked_add(row.duration)
                .filter(|ticks| *ticks <= established.max_ticks)
                .ok_or("copy_target_exceeded")?;
            established.pending_ticks = ticks;
            if self.rows.len() >= self.max_rows {
                return Err("copy_range_row_limit".into());
            }
            self.rows.push(row);
            self.row_inspections.extend(inspection);
        }
        Ok(())
    }

    /// The whole-title observation, or why this read is not one.
    fn finish(mut self) -> Result<AnalysisCopyRows, String> {
        if !self.ended() {
            return Err("copy_output_incomplete".into());
        }
        self.pump()?;
        self.assembler
            .as_mut()
            .ok_or("copy_head_clock_missing")?
            .finish()
            .map_err(observation_error)?;
        self.measure()?;
        if !self.fragments.is_empty()
            || !self.pending_media.is_empty()
            || !self.pending_clock.is_empty()
            || !self.inspected.is_empty()
        {
            return Err("copy_pair_incomplete".into());
        }
        let established = self.established.ok_or("copy_fill_empty")?;
        let first = self.rows.first().ok_or("copy_fill_empty")?.dts;
        if self.inspect_hevc
            && (self.inspection.is_none() || self.row_inspections.len() != self.rows.len())
        {
            return Err("copy_inspection_incomplete".into());
        }
        Ok(AnalysisCopyRows {
            init: Arc::new(established.normalizer.canonical_init().clone()),
            source_origin: self.source_origin.ok_or("copy_clock_unknown")?,
            canonical_title_start: 0,
            rows: self.rows,
            boundaries: RangeBoundaries {
                begins_title: first == 0,
                eof: true,
            },
            inspection: self.inspection,
            row_inspections: self.row_inspections,
            promotion: established.contract.promotion().clone(),
        })
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
mod tests;
