//! The copy index rides the content-analysis pass.
//!
//! The fragment-index pass already reads the whole file once, and its lead
//! output's reader already parses the canonical video-only stream that a v3
//! copy-index row is defined on. This module turns the same read into the
//! title's whole-title copy observation, so the Analysis worker can seal and
//! publish the v3 index without a second pass over the video. The standalone
//! form of the same job is a row-less demand filled by
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
//! - **No second read, parse or conversion.** The lead reader hands over
//!   each fragment it has parsed, measured and (for a converting identity)
//!   converted, by move. The ride pairs it with its packets through the
//!   engine's `associate_fragment_clock`, checks it against the engine's
//!   head normalization contract, and takes its row from the engine's
//!   `CopyCompanionVerifier` (the stream standing as its own companion: the
//!   index holds no AV) and its inspection from `inspect_canonical_row`.
//!   That inspection walks the fragment's NAL headers for HDR10 SEI; it is
//!   the one thing done again, and it reads headers, not payload.
//!
//! The ride has no task of its own beyond the clock reader. Its state is fed
//! in place by the three readers the pass already runs (the lead output, the
//! header trace on stderr, the clock), each doing a few microseconds of
//! pairing under one lock. It can only ever add an answer: the clock reader
//! drains descriptor 7 to the end of the file whatever happens, so FFmpeg
//! never waits on it, and anything the engine would not accept, or more
//! unpaired media than the engine itself would hold, abandons the ride and
//! leaves the pass to build its index as before. An abandoned ride is
//! counted, logged, shown on the pass's Analysis row and remembered, so the
//! file's next pass does not ride.
use crate::copy_analysis::{
    record_ride, AnalysisCopyRows, CopyRecipe, CopyRowInspection, PendingCopyRide, RideOutcome,
};
use crate::copy_normalization::{CanonicalNormalizer, CopyNormalizationContract};
use crate::copyrange::{
    copy_cut_policy, head_origin, inspect_canonical_row, COPY_TARGET_SECONDS, MAX_HELD, MAX_PACKETS,
};
use crate::fragment_index_cluster::SourceStamp;
use plurx_core::{
    domain::MediaFile,
    fmp4::{Fragment, Init, TrackFragment},
    hevc_configuration::{PacketInspection, RangeInspection},
    segplan::{
        companion::CopyCompanionVerifier,
        copy_cursor::CopyPlanCursor,
        packet_clock::{
            associate_fragment_clock, PacketClockParser, SourcePacketObservation,
            MAX_PACKET_CLOCK_LINE_BYTES,
        },
        ranges::{RangeBoundaries, SourceTimestamp},
        IndexRow,
    },
    transcode::{CopyVideoOptions, VodSourceClock},
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};

/// The inherited descriptor the pass's packet clock is written to. The
/// source is 3 and the sibling identities' outputs are 4 to 6.
pub(crate) const CLOCK_FD: i32 = 7;

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

/// The containers a ride is taken on: Matroska and ISO base media. Their
/// demuxers apply no discontinuity correction, so FFmpeg's input clock
/// without `-copyts` differs from the source's by the format start alone.
/// The scanner records the lowercased extension. Every other container, and
/// a file with none, is left to the standalone form.
fn rides_container(file: &MediaFile) -> bool {
    matches!(
        file.container.as_deref(),
        Some("mkv" | "webm" | "mp4" | "m4v" | "mov")
    )
}

/// Files whose last riding pass abandoned its ride or did not build, with
/// the source identity that pass read. The next pass on the same source does
/// not ride, so a ride can never be why a title's pass keeps failing, and a
/// title the ride cannot observe is not observed again on every pass. In
/// memory, as the subtitle ride-along's is: a restart is a new build, which
/// is when the question deserves asking again.
static FAILED_RIDES: Mutex<BTreeMap<i64, SourceStamp>> = Mutex::new(BTreeMap::new());

/// Latched files kept. Past it the lowest file id is forgotten.
const MAX_FAILED_RIDES: usize = 4_096;

fn failed_rides() -> std::sync::MutexGuard<'static, BTreeMap<i64, SourceStamp>> {
    FAILED_RIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn record_failed_ride(file_id: i64, stamp: SourceStamp) {
    let mut failed = failed_rides();
    if failed.len() >= MAX_FAILED_RIDES && !failed.contains_key(&file_id) {
        failed.pop_first();
    }
    failed.insert(file_id, stamp);
}

/// One pass's decision to ride: the lead identity whose canonical stream is
/// observed, and the probed clock that places it.
pub(crate) struct CopyRidePlan {
    video: CopyVideoOptions,
    clock: ProbedCopyClock,
    stamp: SourceStamp,
    held_limit: usize,
    #[cfg(test)]
    deaf_clock: bool,
}

impl CopyRidePlan {
    /// The ride of a pass over `stamp` whose lead output is `video`, or None
    /// when this title does not ride: a container outside the allow-list, no
    /// probed clock, or a source whose last riding pass failed.
    pub(crate) fn for_pass(
        file: &MediaFile,
        video: CopyVideoOptions,
        clock: Option<ProbedCopyClock>,
        stamp: SourceStamp,
    ) -> Option<Self> {
        if !rides_container(file) {
            return None;
        }
        let clock = clock?;
        {
            let mut failed = failed_rides();
            match failed.get(&file.id) {
                Some(last) if *last == stamp => {
                    tracing::debug!(
                        file_id = file.id,
                        "the last riding pass on this source failed; no copy ride"
                    );
                    return None;
                }
                // Another source identity is another question.
                Some(_) => {
                    failed.remove(&file.id);
                }
                None => {}
            }
        }
        Some(Self {
            video,
            clock,
            stamp,
            held_limit: MAX_HELD,
            #[cfg(test)]
            deaf_clock: false,
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

    /// A ride whose clock is drained but never heard, holding at most
    /// `held_limit` bytes of unpaired media: the ride that falls behind.
    #[cfg(test)]
    pub(crate) fn without_clock_for_test(mut self, held_limit: usize) -> Self {
        self.deaf_clock = true;
        self.held_limit = held_limit;
        self
    }
}

enum RideState {
    Observing(Box<CanonicalObserver>),
    /// The ride ended early, for a reason already counted and logged. The
    /// pass goes on without it.
    Abandoned,
    Finished,
}

/// What the pass's three readers and the ride share.
struct RideShared {
    file_id: i64,
    state: Mutex<RideState>,
    /// Set with `RideState::Abandoned`, so a reader can skip the lock.
    abandoned: AtomicBool,
    /// Rows observed so far, for the pass's progress report.
    rows: AtomicUsize,
}

impl RideShared {
    fn state(&self) -> std::sync::MutexGuard<'_, RideState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Hand the observer one thing a reader read. It is taken in place: a
    /// few sample-table comparisons, never a wait. A refusal ends the ride.
    fn feed(&self, feed: impl FnOnce(&mut CanonicalObserver) -> Result<(), String>) {
        if self.abandoned.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state();
        let RideState::Observing(observer) = &mut *state else {
            return;
        };
        match feed(observer) {
            Ok(()) => self.rows.store(observer.rows.len(), Ordering::Relaxed),
            Err(reason) => {
                *state = RideState::Abandoned;
                drop(state);
                self.abandoned_notice(&reason);
            }
        }
    }

    /// End the ride for a reason no observation gave.
    fn abandon(&self, reason: &str) {
        let mut state = self.state();
        if matches!(&*state, RideState::Observing(_)) {
            *state = RideState::Abandoned;
            drop(state);
            self.abandoned_notice(reason);
        }
    }

    /// Once per ride: counted, and logged with the observer's own reason.
    fn abandoned_notice(&self, reason: &str) {
        self.abandoned.store(true, Ordering::Release);
        record_ride(RideOutcome::Abandoned);
        tracing::warn!(
            file_id = self.file_id,
            reason,
            "the fragment-index pass abandoned its copy index observation; the pass itself is unaffected"
        );
    }
}

/// Rows a ride has observed so far and whether it was abandoned, for the
/// pass's progress report.
#[derive(Clone)]
pub(crate) struct CopyRideMeter(Arc<RideShared>);
impl CopyRideMeter {
    pub(crate) fn rows(&self) -> usize {
        self.0.rows.load(Ordering::Relaxed)
    }
    pub(crate) fn abandoned(&self) -> bool {
        self.0.abandoned.load(Ordering::Acquire)
    }
}

/// Where the pass's readers hand the ride what they read.
#[derive(Clone)]
pub(crate) struct CopyRideTap(Arc<RideShared>);

impl CopyRideTap {
    /// One packet the header trace closed.
    pub(crate) fn packet(&self, packet: PacketInspection) {
        self.0.feed(|observer| observer.packet(packet));
    }
    /// The header trace reached the end of the child's stderr.
    pub(crate) fn trace_end(&self, end: Result<RangeInspection, String>) {
        self.0.feed(|observer| observer.trace_end(end));
    }
}

impl crate::fragindex::LeadFragments for CopyRideTap {
    fn wants_fragments(&self) -> bool {
        !self.0.abandoned.load(Ordering::Acquire)
    }
    fn init(&self, init: &Init) {
        self.0.feed(|observer| observer.init(init));
    }
    fn fragment(&self, unconverted: Option<Vec<TrackFragment>>, fragment: Fragment) {
        self.0
            .feed(|observer| observer.fragment(unconverted, fragment));
    }
    fn end(&self) {
        self.0.feed(CanonicalObserver::lead_end);
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

/// One running ride: its shared state and the clock reader.
pub(crate) struct CopyRide {
    shared: Arc<RideShared>,
    video: CopyVideoOptions,
    stamp: SourceStamp,
    clock_reader: tokio::task::JoinHandle<()>,
}

impl CopyRide {
    /// Start reading `clock`, the parent's end of the child's descriptor 7.
    /// The reader runs to the end of the file whether or not the ride is
    /// still observing, so the child never blocks on that descriptor.
    pub(crate) fn start(
        plan: CopyRidePlan,
        file: &MediaFile,
        clock: impl AsyncRead + Unpin + Send + 'static,
    ) -> Self {
        let observer =
            CanonicalObserver::new(file.clone(), plan.video, plan.clock, plan.held_limit);
        let shared = Arc::new(RideShared {
            file_id: file.id,
            state: Mutex::new(RideState::Observing(Box::new(observer))),
            abandoned: AtomicBool::new(false),
            rows: AtomicUsize::new(0),
        });
        #[cfg(test)]
        let deaf = plan.deaf_clock;
        #[cfg(not(test))]
        let deaf = false;
        let clock_reader = tokio::spawn(read_clock(clock, Arc::clone(&shared), deaf));
        Self {
            shared,
            video: plan.video,
            stamp: plan.stamp,
            clock_reader,
        }
    }

    pub(crate) fn tap(&self) -> CopyRideTap {
        CopyRideTap(Arc::clone(&self.shared))
    }

    pub(crate) fn meter(&self) -> CopyRideMeter {
        CopyRideMeter(Arc::clone(&self.shared))
    }

    /// The whole-title rows, when the pass's lead output built and the ride
    /// saw the same stream through to its end.
    ///
    /// The clock reader is joined only when that is certain to end: the lead
    /// built, so the child exited successfully and closed the clock, and the
    /// ride is still observing. In every other case (a child that failed,
    /// was killed or could not be waited for, or a ride already abandoned)
    /// the reader is stopped instead; nothing here waits without a bound.
    /// A ride that was abandoned, or whose pass did not build, is remembered
    /// so this source's next pass does not ride.
    pub(crate) async fn finish(self, lead_built: bool) -> Option<PendingCopyRide> {
        let Self {
            shared,
            video,
            stamp,
            clock_reader,
        } = self;
        if !lead_built || shared.abandoned.load(Ordering::Acquire) {
            clock_reader.abort();
        }
        let _ = clock_reader.await;
        let state = std::mem::replace(&mut *shared.state(), RideState::Finished);
        let observed = match state {
            RideState::Observing(observer) if lead_built => match observer.finish() {
                Ok(rows) => Some(rows),
                Err(reason) => {
                    shared.abandoned_notice(&reason);
                    None
                }
            },
            // The pass did not build: there was nothing to observe to the end.
            RideState::Observing(_) => {
                record_ride(RideOutcome::NotBuilt);
                None
            }
            RideState::Abandoned | RideState::Finished => None,
        };
        match observed {
            Some(rows) => {
                record_ride(RideOutcome::Observed);
                tracing::info!(
                    file_id = shared.file_id,
                    rows = rows.rows.len(),
                    "the fragment-index pass also observed this file's whole copy index"
                );
                Some(PendingCopyRide { video, rows })
            }
            None => {
                tracing::warn!(
                    file_id = shared.file_id,
                    built = lead_built,
                    "a riding fragment-index pass kept no copy index observation; this file's next pass will not ride"
                );
                record_failed_ride(shared.file_id, stamp);
                None
            }
        }
    }
}

/// Drain the child's packet clock to the end of the file. What is read goes
/// to the ride while it is still observing and nowhere once it is not.
///
/// A read error ends the ride, never the read: the descriptor stays open and
/// is tried again, so the child is never handed a broken pipe. The pass
/// stops this task once its child is gone.
async fn read_clock(mut clock: impl AsyncRead + Unpin, shared: Arc<RideShared>, deaf: bool) {
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        match clock.read(&mut buffer).await {
            Ok(0) => {
                shared.feed(CanonicalObserver::clock_end);
                return;
            }
            Ok(read) => {
                if !deaf {
                    shared.feed(|observer| observer.clock(&buffer[..read]));
                }
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::Interrupted {
                    shared.abandon("copy_clock_read_failed");
                }
                tokio::task::yield_now().await;
            }
        }
    }
}

/// One fragment the lead reader handed over, not yet paired with its packets.
struct HeldFragment {
    /// The sample tables before conversion, as a media-less fragment, when
    /// the lead converted this fragment: packet sizes are of unconverted
    /// samples. None when the fragment is as the muxer wrote it.
    timing: Option<Fragment>,
    /// The fragment as the index measured it.
    fragment: Fragment,
}

/// The canonical-only observer: the engine's observed mux without the AV
/// half and without a parser of its own. Fragments arrive parsed from the
/// lead reader, packets from the clock, inspections from the header trace;
/// each fragment is paired, checked, inspected and measured by the engine's
/// own functions as soon as its packets are all here.
struct CanonicalObserver {
    file: MediaFile,
    video: CopyVideoOptions,
    probed: ProbedCopyClock,
    inspect_hevc: bool,
    max_rows: usize,
    held_limit: usize,
    clock_line: Vec<u8>,
    clocks: PacketClockParser,
    packets: VecDeque<SourcePacketObservation>,
    /// The first packet on FFmpeg's input clock, which every later packet
    /// is measured from, and the same instant on the source's clock, which
    /// the observation reports.
    origin: Option<SourceTimestamp>,
    source_origin: Option<SourceTimestamp>,
    next_sequence: u64,
    raw_init: Option<Init>,
    established: Option<Established>,
    fragments: VecDeque<HeldFragment>,
    held_bytes: usize,
    inspected: VecDeque<PacketInspection>,
    inspection: Option<RangeInspection>,
    rows: Vec<IndexRow>,
    row_inspections: Vec<CopyRowInspection>,
    lead_eof: bool,
    clock_eof: bool,
    trace_eof: bool,
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
    fn new(
        file: MediaFile,
        video: CopyVideoOptions,
        probed: ProbedCopyClock,
        held_limit: usize,
    ) -> Self {
        // The engine's own predicate (`copyrange::fill_copy_gap`).
        let inspect_hevc = file.video_codec.as_deref() == Some("hevc");
        Self {
            file,
            video,
            probed,
            inspect_hevc,
            max_rows: plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
            held_limit,
            clock_line: Vec::new(),
            clocks: PacketClockParser::new(),
            packets: VecDeque::new(),
            origin: None,
            source_origin: None,
            next_sequence: 0,
            raw_init: None,
            established: None,
            fragments: VecDeque::new(),
            held_bytes: 0,
            inspected: VecDeque::new(),
            inspection: None,
            rows: Vec::new(),
            row_inspections: Vec::new(),
            lead_eof: false,
            clock_eof: false,
            trace_eof: false,
        }
    }

    fn init(&mut self, init: &Init) -> Result<(), String> {
        if init.tracks.len() != 1 || init.video().is_none() {
            return Err("copy_canonical_observation_failed".into());
        }
        if self.raw_init.replace(init.clone()).is_some() {
            return Err("copy_init_contract_changed".into());
        }
        Ok(())
    }

    /// One fragment the lead reader has measured. `unconverted` is its
    /// sample tables before a Dolby Vision conversion, when there was one.
    fn fragment(
        &mut self,
        unconverted: Option<Vec<TrackFragment>>,
        fragment: Fragment,
    ) -> Result<(), String> {
        if self.lead_eof {
            return Err("copy_observation_closed".into());
        }
        // Unpaired media beyond what the engine's own mux would hold.
        self.held_bytes = self
            .held_bytes
            .checked_add(fragment.len())
            .filter(|held| *held <= self.held_limit)
            .ok_or("copy_range_input_limit")?;
        self.fragments.push_back(HeldFragment {
            timing: unconverted.map(|tracks| Fragment {
                bytes: Vec::new(),
                mdat_payload: 0..0,
                tracks,
            }),
            fragment,
        });
        self.measure()
    }

    fn lead_end(&mut self) -> Result<(), String> {
        self.lead_eof = true;
        self.measure()
    }

    /// Clock bytes, a line at a time through the engine's packet parser. The
    /// first packet names both origins.
    fn clock(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.clock_eof {
            return Err("copy_observation_closed".into());
        }
        for byte in bytes {
            if *byte != b'\n' {
                if self.clock_line.len() == MAX_PACKET_CLOCK_LINE_BYTES {
                    return Err("copy_clock_unknown".into());
                }
                self.clock_line.push(*byte);
                continue;
            }
            let line = std::mem::take(&mut self.clock_line);
            let line = std::str::from_utf8(&line).map_err(|_| "copy_clock_unknown".to_owned())?;
            let Some(packet) = self
                .clocks
                .push_line(line)
                .map_err(|_| "copy_clock_unknown".to_owned())?
            else {
                continue;
            };
            if self.origin.is_none() {
                self.origin = Some(head_origin(&packet)?);
                self.source_origin = Some(self.probed.source_origin(&packet)?);
            }
            if self.packets.len() == MAX_PACKETS {
                return Err("copy_observation_limit".into());
            }
            self.packets.push_back(packet);
        }
        self.measure()
    }

    fn clock_end(&mut self) -> Result<(), String> {
        if !self.clock_line.is_empty() || self.clocks.finish().is_err() {
            return Err("copy_clock_unknown".into());
        }
        self.clock_eof = true;
        self.measure()
    }

    fn packet(&mut self, packet: PacketInspection) -> Result<(), String> {
        if self.trace_eof {
            return Err("copy_observation_closed".into());
        }
        if self.inspect_hevc {
            if self.inspected.len() >= MAX_PACKETS {
                return Err("copy_inspection_limit".into());
            }
            self.inspected.push_back(packet);
        }
        self.measure()
    }

    fn trace_end(&mut self, end: Result<RangeInspection, String>) -> Result<(), String> {
        self.trace_eof = true;
        if self.inspect_hevc {
            self.inspection = Some(end.map_err(|_| "copy_hevc_inspection_failed".to_owned())?);
        }
        self.measure()
    }

    /// Turn every held fragment whose packets and inspections are all here
    /// into its row, as the engine's mux does for the canonical half.
    fn measure(&mut self) -> Result<(), String> {
        loop {
            let (Some(held), Some(raw_init), Some(origin)) =
                (self.fragments.front(), self.raw_init.as_ref(), self.origin)
            else {
                return Ok(());
            };
            let video = raw_init.video().ok_or("copy_video_missing")?;
            let packets = held
                .timing
                .as_ref()
                .unwrap_or(&held.fragment)
                .track(video.id)
                .ok_or("copy_video_missing")?
                .sample_count();
            if packets == 0 {
                return Err("copy_canonical_observation_failed".into());
            }
            if packets > MAX_PACKETS {
                return Err("copy_observation_limit".into());
            }
            if self.packets.len() < packets || (self.inspect_hevc && self.inspected.len() < packets)
            {
                return Ok(());
            }
            let held = self.fragments.pop_front().ok_or("copy_pair_missing")?;
            self.held_bytes = self.held_bytes.saturating_sub(held.fragment.len());
            // The engine's association of a fragment with its packets, on
            // the sample tables the packets were cut from.
            let clock = associate_fragment_clock(
                held.timing.as_ref().unwrap_or(&held.fragment),
                raw_init,
                &self.packets.make_contiguous()[..packets],
                self.next_sequence,
                origin,
                0,
            )
            .map_err(|_| "copy_clock_unknown".to_owned())?;
            self.packets.drain(..packets);
            self.next_sequence = clock.next_packet_sequence;
            if self.established.is_none() {
                // Promotion is read from the head's parameter sets and SEI,
                // which a conversion (it rewrites RPU NAL units only) leaves
                // as the muxer wrote them.
                let contract = CopyNormalizationContract::from_head(
                    &self.file,
                    self.video,
                    raw_init,
                    &held.fragment,
                )
                .map_err(|error| error.to_string())?;
                let normalizer = contract
                    .begin_canonical(raw_init.clone())
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
            let first_sequence = clock
                .next_packet_sequence
                .checked_sub(packets as u64)
                .ok_or("copy_inspection_sequence")?;
            let inspection = if self.inspect_hevc {
                let inspection = inspect_canonical_row(
                    &held.fragment,
                    established.normalizer.canonical_init(),
                    &clock,
                    first_sequence,
                    self.inspected.iter().take(packets),
                )?;
                self.inspected.drain(..packets);
                Some(inspection)
            } else {
                None
            };
            // The lead reader's conversion is the engine's (the same
            // converter over the same track); what remains of the engine's
            // normalization is its parameter-set check.
            established
                .normalizer
                .admit(&held.fragment)
                .map_err(|error| error.to_string())?;
            let row = established
                .verifier
                .verify(&held.fragment, clock, &held.fragment)
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
            established.pending_ticks = ticks
                .checked_add(row.duration)
                .filter(|ticks| *ticks <= established.max_ticks)
                .ok_or("copy_target_exceeded")?;
            if self.rows.len() >= self.max_rows {
                return Err("copy_range_row_limit".into());
            }
            self.rows.push(row);
            self.row_inspections.extend(inspection);
        }
    }

    /// The whole-title observation, or why this read is not one.
    fn finish(mut self) -> Result<AnalysisCopyRows, String> {
        if !self.lead_eof || !self.clock_eof || (self.inspect_hevc && !self.trace_eof) {
            return Err("copy_output_incomplete".into());
        }
        self.measure()?;
        if !self.fragments.is_empty() || !self.packets.is_empty() || !self.inspected.is_empty() {
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

#[cfg(test)]
mod tests;
