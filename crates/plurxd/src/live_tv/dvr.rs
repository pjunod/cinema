//! The recording engine: one transport per channel, one sink per recording.
//!
//! The live path opens exactly one tuner GET per session and pumps the
//! response into FFmpeg's stdin. A capture is the same open and the same
//! prefix probe with the pump's sink replaced by a fan-out to files — no
//! encoder, no FFmpeg process. HDHomeRun's `/auto/v{n}` is already a
//! single-programme MPEG-TS and the finite-media path already opens a `.ts`,
//! so the bytes the tuner sends are the bytes that land on disk.
//!
//! Three shapes carry the whole design:
//!
//! - A **transport** is one tuner GET on one channel. It counts against the
//!   tuner limit.
//! - A **sink** is one recording's file, with its own capture window. Several
//!   sinks share a transport when their windows meet, which is how the 8:30
//!   programme's tail padding and the 9:00 programme's head padding cost one
//!   tuner and produce two files. A sink costs no tuner.
//! - An **attempt** is one file: `<base>.a1.part`, `<base>.a2.part`. A capture
//!   that loses its worker — a restart, an owner handoff, a stalled tuner —
//!   resumes into a *new* file, so a fenced old process that is still draining
//!   can never write into the bytes the new attempt is writing. Finishing
//!   concatenates them in order and the gap is recorded rather than hidden.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use plurx_core::dvr::{
    recording_basename, recording_folder, DvrEventInput, DvrInsertOutcome, DvrKeepMode, DvrOrigin,
    DvrRecording, DvrState, DvrStatePatch, DvrTransition, DVR_ATTEMPTS_MAX, DVR_EVENT_PRUNE_BATCH,
    DVR_EVENT_RETENTION_MS, DVR_MIN_USEFUL_S, DVR_SCHEDULE_DAYS_MAX,
};
use tokio_util::sync::CancellationToken;

use super::schedule;
use super::{
    collect_live_prefix, open_tuner_stream, pinned_url, probe_live_source, unix_seconds, DvrConfig,
    GuideWindow, LiveTunerInput, LiveTvChannel, LiveTvConfig, LiveTvError, LiveTvManager,
    LiveTvMetrics, TUNER_READ_TIMEOUT,
};
use crate::live_tv_delivery::LiveSourceFacts;

/// How often the owner re-reads the world. Fast enough that a capture starts
/// within a quarter of a minute of its padding, slow enough that it is not a
/// poll loop on the replicated Store.
pub(crate) const DVR_TICK: std::time::Duration = std::time::Duration::from_secs(15);
/// How often a sink's byte count reaches the row, so the Activity page can
/// show a rising number without a write per chunk.
const DVR_PROGRESS_INTERVAL_MS: i64 = 30_000;
/// Retention is an hourly sweep, not a per-tick one: it reads every rule's
/// recordings and deletes files, and neither is work worth doing 240 times an
/// hour.
const DVR_RETENTION_INTERVAL_S: i64 = 3600;
/// How long a spent reminder is kept before it is swept. Long enough that a
/// viewer can still see what fired yesterday, short enough that it never
/// becomes the reason they cannot set another one.
const REMINDER_RETENTION_S: i64 = 7 * 24 * 3600;
/// How long after a capture finishes the library sweep keeps offering it to
/// the scanner. Past this, a row that still has no item is one the scan cannot
/// place, and repeating the request cannot change that.
const UNLINKED_SCAN_WINDOW_S: i64 = 24 * 3600;
const DVR_WRITE_RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const DVR_OBSERVATIONS_MAX: usize = 64;
/// About 3.3 seconds of a 19.4 Mbit/s ATSC 1.0 multiplex. One stalled disk
/// costs this much memory, then only that sink ends its current attempt.
const DVR_SINK_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
/// The byte bound is authoritative; this second bound stops a pathological
/// stream of tiny chunks from allocating an unbounded number of queue nodes.
const DVR_SINK_QUEUE_CHUNKS: usize = 512;

/// A viewer's queue: about 1.6 s of a 19.4 Mbit/s multiplex. Smaller than a
/// recording's because the consumer is FFmpeg reading its own stdin, the fast
/// path; a queue this deep that is still full means that FFmpeg has stopped,
/// and evicting it now is what keeps its siblings and the tuner read moving.
///
/// Measured from where the viewer started. A viewer attached when the fan-out
/// starts is handed the held opening prefix — up to `SOURCE_PREFIX_BYTES`,
/// about three seconds of a full-rate multiplex and more than this whole
/// queue — so its bound is this plus the bytes it was handed
/// (`ViewerConsumer::start_behind`). Without that, the opener of every
/// full-rate channel was evicted on its first chunk. Memory stays a number:
/// at most `MAX_CONSUMERS_PER_TRANSPORT` x (4 MiB + `SOURCE_PREFIX_BYTES`).
pub(crate) const VIEWER_QUEUE_BYTES: u64 = 4 * 1024 * 1024;
const VIEWER_QUEUE_CHUNKS: usize = 512;
/// Viewers one transport may feed. A household does not reach it; it exists
/// so the memory a transport can hold in viewer queues is a number
/// (16 x 4 MiB), not a function of how many starts arrive.
pub(crate) const MAX_CONSUMERS_PER_TRANSPORT: usize = 16;
/// Set in `DvrTransport::seats` once the fan-out has found nothing that wants
/// the transport: from then on no consumer may reserve a seat on it, which is
/// what makes "the last detach releases the tuner" race-free against a start
/// that is joining at the same moment.
const SEATS_RETIRED: usize = 1 << (usize::BITS - 1);

/// How far the transport's own probe has got. Viewers plan from it and
/// wait for it; recordings do not need it and never wait.
#[derive(Clone, Debug)]
pub(crate) enum TransportProbe {
    Pending,
    Ready(Box<LiveSourceFacts>),
    Failed(LiveTvError),
}

/// The newest probe of a transport's bytes (plan L-03 §2.4 D1, D5). The first
/// is of the opening prefix; each later one is of a sample the running
/// fan-out takes from the live edge because a viewer asked for newer facts.
#[derive(Clone, Debug)]
pub(crate) struct ProbePublication {
    /// Which sample these facts describe: 1 is the opening prefix, and each
    /// re-probe's sample takes the next number when its collection starts.
    pub(crate) epoch: u64,
    pub(crate) probe: TransportProbe,
    /// When these facts were published, for their age.
    pub(crate) at: tokio::time::Instant,
    /// The worker has ended: nothing will be published again.
    pub(crate) ended: bool,
}

/// Facts older than this are re-probed from the live edge before a viewer
/// plans from them. A broadcast changes format at programme boundaries and a
/// recording keeps its transport open for hours. The joiner's own input check
/// (`input_contradicts_plan` in the session) is what guarantees a plan matches
/// its bytes; this bound keeps that check from failing joins into a long
/// recording on facts from its first minute.
pub(crate) const TRANSPORT_FACTS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// Who asked for a transport first. Only used to count a recording's
/// transport against the recording reserve in the moment between its
/// reservation and its first sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransportOrigin {
    Viewer,
    Recording,
}

/// One tuner GET on one channel of one device, feeding every recording sink
/// and every live viewer on it (plan L-03 §3.1).
///
/// The name predates viewers sharing it. The plan renames it `LiveTransport`,
/// keyed by `(device_id, channel_id)`, in its own module; M1 kept the name,
/// the module and the `channel_id` key, and checks the device on join
/// (`same_tuner`). That is a recorded deviation from the plan, not something
/// it left optional.
pub(crate) struct DvrTransport {
    pub(crate) channel: LiveTvChannel,
    /// The tuner configuration generation this transport opened under.
    /// `drain_before` compares it exactly as it does for a session. A viewer
    /// may join across it (§2.4 D7, option (b)): the bytes do not depend on
    /// any generation-scoped setting except the device, which is checked
    /// separately below.
    pub(crate) generation: i64,
    /// The serving-fence generation. Checked on every chunk, so a transport
    /// whose node has lost authority closes its files within one read timeout
    /// rather than writing into a file another owner may already be writing.
    pub(crate) owner_serving_generation: u64,
    /// The device this GET went to: its reported id (empty when the opener
    /// did not know it) and the address it was opened on. A join requires
    /// both to match what the joiner would itself open.
    pub(crate) device_id: String,
    pub(crate) address: std::net::Ipv4Addr,
    pub(crate) origin: TransportOrigin,
    pub(crate) cancel: CancellationToken,
    pub(crate) delivered: Arc<AtomicU64>,
    pub(crate) sinks: std::sync::Mutex<Vec<Arc<DvrSink>>>,
    pub(crate) viewers: std::sync::Mutex<Vec<Arc<ViewerConsumer>>>,
    /// Consumers admitted but not yet attached (a viewer building its FFmpeg,
    /// a recording opening its file), plus `SEATS_RETIRED` once retired.
    seats: AtomicUsize,
    consumers_changed: tokio::sync::Notify,
    pub(crate) worker: std::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), LiveTvError>>>>,
    /// What the prefix probe saw, for every sidecar this transport writes.
    pub(crate) source: std::sync::Mutex<Option<LiveSourceFacts>>,
    probe: tokio::sync::watch::Sender<ProbePublication>,
    /// The highest sample epoch whose collection has started (1 is the
    /// opening prefix). Only the fan-out advances it.
    sampled: AtomicU64,
    /// A viewer needs facts from a sample numbered at least this, because the
    /// published ones failed, went stale or grew old. The fan-out starts a
    /// sample of the live edge when this exceeds `sampled`.
    reprobe_wanted: AtomicU64,
    /// Facts from samples numbered up to this no longer describe the mux: a
    /// viewer's FFmpeg saw the broadcast change format, or opened an input
    /// that contradicts the facts it planned from (§2.4 D5). Stale facts are
    /// never planned from and never offered; the next viewer re-probes.
    stale_through: AtomicU64,
    /// Why the worker ended, for the viewers whose feed it closed.
    terminal: std::sync::Mutex<Option<LiveTvError>>,
    /// Fired once the transport is out of the registry and its worker —
    /// which owns the tuner response — has been joined.
    closed: CancellationToken,
    /// Where the prefix probe writes its sample.
    pub(crate) scratch: PathBuf,
    metrics: Arc<LiveTvMetrics>,
    /// A warm opening (plan L-02 §3.3): the opener already planned from
    /// cached facts, so the fan-out feeds it from the first byte instead of
    /// holding a prefix, and probes the opening sample beside the feed as
    /// verification. Set before the worker starts; never changes after.
    warm_opening: AtomicBool,
    /// A warm opening's opener, by capability, until it has attached or given
    /// its seat back. Joiners wait for the opening sample's probe, and on a
    /// warm opening only the running fan-out collects that sample, so once
    /// the opener is gone without attaching the fan-out must run without it
    /// (see `await_first_consumer`).
    warm_opener: std::sync::Mutex<Option<String>>,
    /// The probe of the opening sample (epoch 1), kept apart from the newest
    /// publication so a warm opener reads the verdict on *this tune's first
    /// bytes* even if a later re-probe has already replaced it.
    opening: std::sync::Mutex<Option<Result<LiveSourceFacts, LiveTvError>>>,
}

pub(crate) struct TransportInit {
    pub(crate) channel: LiveTvChannel,
    pub(crate) generation: i64,
    pub(crate) owner_serving_generation: u64,
    pub(crate) device_id: String,
    pub(crate) address: std::net::Ipv4Addr,
    pub(crate) origin: TransportOrigin,
    pub(crate) scratch: PathBuf,
    pub(crate) metrics: Arc<LiveTvMetrics>,
    /// Seats reserved at creation: the opener's own.
    pub(crate) seats: usize,
}

impl DvrTransport {
    pub(crate) fn new(init: TransportInit) -> Arc<Self> {
        Arc::new(Self {
            channel: init.channel,
            generation: init.generation,
            owner_serving_generation: init.owner_serving_generation,
            device_id: init.device_id,
            address: init.address,
            origin: init.origin,
            cancel: CancellationToken::new(),
            delivered: Arc::new(AtomicU64::new(0)),
            sinks: std::sync::Mutex::new(Vec::new()),
            viewers: std::sync::Mutex::new(Vec::new()),
            seats: AtomicUsize::new(init.seats),
            consumers_changed: tokio::sync::Notify::new(),
            worker: std::sync::Mutex::new(None),
            source: std::sync::Mutex::new(None),
            probe: tokio::sync::watch::Sender::new(ProbePublication {
                epoch: 1,
                probe: TransportProbe::Pending,
                at: tokio::time::Instant::now(),
                ended: false,
            }),
            sampled: AtomicU64::new(1),
            reprobe_wanted: AtomicU64::new(0),
            stale_through: AtomicU64::new(0),
            terminal: std::sync::Mutex::new(None),
            closed: CancellationToken::new(),
            scratch: init.scratch,
            metrics: init.metrics,
            warm_opening: AtomicBool::new(false),
            warm_opener: std::sync::Mutex::new(None),
            opening: std::sync::Mutex::new(None),
        })
    }

    /// Mark this transport's opening as warm, opened by the session
    /// `opener`. Only its opener may call this, and only before
    /// `spawn_transport_worker`.
    pub(crate) fn open_warm(&self, opener: &str) {
        *self
            .warm_opener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(opener.to_owned());
        self.warm_opening.store(true, Ordering::Release);
    }

    /// The session `capability` has attached, or is giving its seat back.
    /// If it is this warm opening's opener, the opening stops waiting for it.
    /// Called before the seat is released, so an opener that attached is
    /// already in the viewer list when it is seen gone.
    pub(crate) fn warm_opener_done(&self, capability: &str) {
        let mut opener = self
            .warm_opener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if opener.as_deref() == Some(capability) {
            *opener = None;
        }
    }

    fn warm_opener_gone(&self) -> bool {
        self.is_warm_opening()
            && self
                .warm_opener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
    }

    fn is_warm_opening(&self) -> bool {
        self.warm_opening.load(Ordering::Acquire)
    }

    /// The verdict of the opening sample's probe: `None` while it is still
    /// being collected or probed; the transport's own end if it stopped first.
    pub(crate) fn opening_facts(&self) -> Option<Result<LiveSourceFacts, LiveTvError>> {
        if let Some(opening) = self
            .opening
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Some(opening);
        }
        let ended = self.probe.borrow().ended;
        ended.then(|| {
            Err(self.terminal_error().unwrap_or_else(|| {
                LiveTvError::StreamFailed(
                    "the tuner transport stopped before its source was observed".into(),
                )
            }))
        })
    }

    pub(crate) fn live_sinks(&self) -> Vec<Arc<DvrSink>> {
        self.sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|sink| !sink.cancel.is_cancelled())
            .cloned()
            .collect()
    }

    /// Viewers that are still reading: not cancelled, not evicted.
    pub(crate) fn live_viewers(&self) -> Vec<Arc<ViewerConsumer>> {
        self.viewers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|viewer| viewer.is_live())
            .cloned()
            .collect()
    }

    pub(crate) fn pending_seats(&self) -> usize {
        self.seats.load(Ordering::Acquire) & !SEATS_RETIRED
    }

    /// Viewer seats in use: attached viewers plus admitted ones on their way.
    pub(crate) fn viewer_seats(&self) -> usize {
        self.live_viewers().len() + self.pending_seats()
    }

    /// A recording transport counts against the recording reserve: it has a
    /// live sink, or it was opened for one that is still opening its file.
    pub(crate) fn is_recording(&self) -> bool {
        !self.live_sinks().is_empty()
            || (self.origin == TransportOrigin::Recording
                && self
                    .sinks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
                && self.pending_seats() > 0)
    }

    /// Closed, closing, or retired: nothing may join it.
    pub(crate) fn is_closing(&self) -> bool {
        self.cancel.is_cancelled() || self.seats.load(Ordering::Acquire) & SEATS_RETIRED != 0
    }

    /// Whether any consumer, attached or admitted, still wants its bytes.
    pub(crate) fn wanted(&self) -> bool {
        !self.live_sinks().is_empty() || !self.live_viewers().is_empty() || self.pending_seats() > 0
    }

    /// Whether this transport is the tuner GET a joiner would itself open.
    pub(crate) fn same_tuner(&self, device_id: &str, address: std::net::Ipv4Addr) -> bool {
        self.address == address
            && (self.device_id.is_empty() || device_id.is_empty() || self.device_id == device_id)
    }

    /// Reserve a seat for a consumer that will attach shortly. Refused once
    /// the transport is closing or retired.
    pub(crate) fn reserve_seat(&self) -> bool {
        if self.cancel.is_cancelled() {
            return false;
        }
        self.seats
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |seats| {
                (seats & SEATS_RETIRED == 0).then_some(seats + 1)
            })
            .is_ok()
    }

    /// Give back a seat that was reserved and not (or no longer) needed.
    pub(crate) fn release_seat(&self) {
        let _ = self
            .seats
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |seats| {
                (seats & !SEATS_RETIRED > 0).then(|| seats - 1)
            });
        self.consumers_changed.notify_waiters();
    }

    /// Retire the seat book when nothing wants the transport. After this no
    /// consumer can reserve a seat, so the caller may end the transport
    /// knowing nobody is on their way to it.
    ///
    /// Both consumer lists stay locked from the check through the swap. A
    /// consumer on its way holds a seat, so the swap from zero fails; one
    /// that has attached is in its list, so the check fails. An attach pushes
    /// under its list's lock and only then gives its seat back, so it cannot
    /// land between the two — which it could when the lists were read and
    /// released before the swap, retiring a transport with a consumer on it.
    fn try_retire(&self) -> bool {
        let sinks = self
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let viewers = self
            .viewers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sinks.iter().any(|sink| !sink.cancel.is_cancelled())
            || viewers.iter().any(|viewer| viewer.is_live())
        {
            return false;
        }
        match self
            .seats
            .compare_exchange(0, SEATS_RETIRED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(seats) => seats & SEATS_RETIRED != 0,
        }
    }

    /// Attach a viewer that holds a reserved seat. The caller releases that
    /// seat afterwards, so the transport is never seen with neither the seat
    /// nor the viewer. Refused if the transport is closing: `close_transport`
    /// cancels before it takes the viewer list, and this checks under that
    /// list's lock, so a viewer is either seen by the close or refused here.
    pub(crate) fn attach_viewer(&self, viewer: Arc<ViewerConsumer>) -> bool {
        let mut viewers = self
            .viewers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.cancel.is_cancelled() {
            return false;
        }
        viewers.retain(|existing| existing.is_live());
        viewers.push(viewer);
        drop(viewers);
        self.consumers_changed.notify_waiters();
        true
    }

    /// The same guarantee for a recording's sink.
    fn attach_sink(&self, sink: Arc<DvrSink>) -> bool {
        let mut sinks = self
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.cancel.is_cancelled() {
            return false;
        }
        sinks.push(sink);
        drop(sinks);
        self.consumers_changed.notify_waiters();
        true
    }

    /// Detach a viewer. No reference is kept: its queue is dropped with it.
    pub(crate) fn detach_viewer(&self, capability: &str) {
        let removed = {
            let mut viewers = self
                .viewers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let before = viewers.len();
            viewers.retain(|viewer| viewer.capability != capability);
            before != viewers.len()
        };
        if removed {
            self.consumers_changed.notify_waiters();
        }
    }

    /// The facts published so far no longer describe the mux (§2.4 D5).
    /// Every sample begun before now is stale, including one being collected
    /// or probed; the next viewer re-probes. The transport itself, and every
    /// recording and viewer on it, carries on.
    pub(crate) fn mark_source_stale(&self) {
        self.stale_through
            .fetch_max(self.sampled.load(Ordering::Acquire), Ordering::AcqRel);
    }

    fn is_stale_epoch(&self, epoch: u64) -> bool {
        epoch <= self.stale_through.load(Ordering::Acquire)
    }

    /// Whether the newest published facts are stale.
    #[cfg(test)]
    pub(crate) fn source_stale(&self) -> bool {
        self.is_stale_epoch(self.probe.borrow().epoch)
    }

    /// Whether this transport's format is known: its newest probe succeeded
    /// and is not stale. A pending, failed or stale transport is not offered
    /// as "watch instead" (§3.4) — a viewer sent there could not be told what
    /// it would get.
    pub(crate) fn source_known(&self) -> bool {
        let publication = self.probe.borrow();
        matches!(publication.probe, TransportProbe::Ready(_))
            && !self.is_stale_epoch(publication.epoch)
    }

    /// Ask the running fan-out for facts newer than the sample `seen`.
    fn request_reprobe(&self, seen: u64) {
        self.reprobe_wanted
            .fetch_max(seen.saturating_add(1), Ordering::AcqRel);
    }

    /// The fan-out's side: the epoch of a sample to start now, if a viewer
    /// wants newer facts than any sample begun so far. Viewers that ask while
    /// a sample is being collected or probed wait for that one.
    fn begin_reprobe_sample(&self) -> Option<u64> {
        let sampled = self.sampled.load(Ordering::Acquire);
        (self.reprobe_wanted.load(Ordering::Acquire) > sampled)
            .then(|| self.sampled.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// Publish the probe of sample `epoch`. Never replaces newer facts, and
    /// nothing is published once the worker has ended.
    fn publish_source(&self, epoch: u64, result: Result<LiveSourceFacts, LiveTvError>) {
        if epoch == 1 {
            self.opening
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert_with(|| result.clone());
        }
        let probe = match result {
            Ok(facts) => {
                // A recording's sidecar describes the first facts this
                // transport established; a later re-probe is for viewers.
                self.source
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_or_insert_with(|| facts.clone());
                TransportProbe::Ready(Box::new(facts))
            }
            Err(error) => TransportProbe::Failed(error),
        };
        self.probe.send_if_modified(|current| {
            if current.ended || epoch < current.epoch {
                return false;
            }
            *current = ProbePublication {
                epoch,
                probe,
                at: tokio::time::Instant::now(),
                ended: false,
            };
            true
        });
    }

    /// The facts a viewer plans from (plan L-03 §2.4 D1, D5).
    ///
    /// Waits for the opening probe. If the newest facts failed, went stale or
    /// are older than `TRANSPORT_FACTS_MAX_AGE`, asks the running fan-out for
    /// a fresh sample of the live edge and waits for that instead: one start's
    /// probe never decides every later viewer. A re-probe this viewer asked
    /// for that fails fails this viewer only, and the next viewer asks again.
    /// Bounded by the viewer's own start deadline and cancellation.
    pub(crate) async fn source_for_viewer(
        &self,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<LiveSourceFacts, LiveTvError> {
        let mut probe = self.probe.subscribe();
        let mut asked_after: Option<u64> = None;
        loop {
            let current = probe.borrow_and_update().clone();
            let answered = !matches!(current.probe, TransportProbe::Pending)
                && asked_after.is_none_or(|seen| current.epoch > seen);
            if answered {
                match current.probe {
                    TransportProbe::Ready(facts)
                        if !self.is_stale_epoch(current.epoch)
                            && current.at.elapsed() < TRANSPORT_FACTS_MAX_AGE =>
                    {
                        return Ok(*facts);
                    }
                    TransportProbe::Failed(error) if current.ended || asked_after.is_some() => {
                        return Err(error);
                    }
                    _ => {}
                }
            }
            if current.ended {
                return Err(self.terminal_error().unwrap_or_else(|| {
                    LiveTvError::StreamFailed(
                        "the tuner transport stopped before its source was observed".into(),
                    )
                }));
            }
            if answered {
                self.request_reprobe(current.epoch);
                asked_after = Some(current.epoch);
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Err(LiveTvError::CapabilityExpired(
                        "the live-TV start was cancelled while its tuner was being observed".into(),
                    ));
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(LiveTvError::StartupTimeout(
                        "the tuner's source could not be observed before the start deadline".into(),
                    ));
                }
                changed = probe.changed() => changed
                    .map_err(|_| LiveTvError::StreamFailed("the tuner transport stopped".into()))?,
            }
        }
    }

    /// Record how the worker ended and close every viewer's feed, so each
    /// viewer's pump ends and its session reports this error.
    fn finish(&self, result: &Result<(), LiveTvError>) {
        let error = result.as_ref().err().cloned().unwrap_or_else(|| {
            LiveTvError::StreamFailed("the shared tuner connection closed".into())
        });
        {
            let mut terminal = self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            terminal.get_or_insert(error.clone());
        }
        self.probe.send_modify(|current| {
            if matches!(current.probe, TransportProbe::Pending) {
                current.probe = TransportProbe::Failed(error);
            }
            current.ended = true;
        });
        self.close_viewer_feeds();
    }

    fn close_viewer_feeds(&self) {
        for viewer in self
            .viewers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            viewer.close_feed();
        }
        self.consumers_changed.notify_waiters();
    }

    pub(crate) fn terminal_error(&self) -> Option<LiveTvError> {
        self.terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Resolves once this transport has left the registry and its worker —
    /// the owner of the tuner response — has been joined.
    pub(crate) async fn closed(&self) {
        self.closed.cancelled().await;
    }

    #[cfg(test)]
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// The furthest moment anything still wants bytes. A recording may join
    /// while it has not passed; a live viewer wants bytes now.
    pub(crate) fn open_until(&self) -> i64 {
        let sinks = self
            .live_sinks()
            .iter()
            .map(|sink| sink.window.1)
            .max()
            .unwrap_or(i64::MIN);
        if !self.live_viewers().is_empty() || self.pending_seats() > 0 {
            sinks.max(unix_seconds())
        } else {
            sinks
        }
    }

    /// Hold the prefix until the first consumer is attached, so the opener's
    /// FFmpeg starts from the bytes its plan was made from. Returns false if
    /// nothing will ever attach (every admitted consumer gave up) or the
    /// transport was cancelled.
    ///
    /// A warm opening also stops waiting once its opener has gone without
    /// attaching while another consumer still holds a seat (plan L-02 §3.3).
    /// That consumer is a joiner waiting for the opening sample's probe, and
    /// on a warm opening the sample is collected by the running fan-out, not
    /// before it; waiting on for an attach that the joiner is itself waiting
    /// behind would hold both until the joiner's start deadline. The fan-out
    /// then collects and probes the sample from the tuner's first byte with
    /// nobody attached, exactly as a cold transport probes its prefix before
    /// anyone attaches, and the joiner plans from it.
    async fn await_first_consumer(&self) -> bool {
        loop {
            let changed = self.consumers_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            // Read before the consumer lists: an opener that attached pushed
            // its viewer before it was marked done, so gone-and-unattached
            // below means it really left without attaching.
            let opener_gone = self.warm_opener_gone();
            if !self.live_sinks().is_empty() || !self.live_viewers().is_empty() {
                return true;
            }
            if self.try_retire() {
                return false;
            }
            if opener_gone {
                return true;
            }
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return false,
                _ = &mut changed => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        }
    }
}

/// One viewer's seat on a transport: a bounded queue its own pump drains into
/// its own FFmpeg. The transport authorises nothing; this carries no
/// capability beyond the name used to detach it.
pub(crate) struct ViewerConsumer {
    pub(crate) capability: String,
    /// The session's own token: a viewer detaches on its own cancel.
    cancel: CancellationToken,
    queue: std::sync::Mutex<Option<SinkQueue>>,
    evicted: std::sync::Mutex<Option<&'static str>>,
    /// Bytes of the held opening prefix this viewer was handed: it started
    /// that far behind the live edge, and its queue bound counts from there.
    started_behind: AtomicU64,
}

/// The receiving half of a viewer's queue.
pub(crate) struct ViewerFeed {
    pub(crate) rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    pub(crate) queued_bytes: Arc<AtomicU64>,
}

impl ViewerConsumer {
    pub(crate) fn new(capability: String, cancel: CancellationToken) -> (Arc<Self>, ViewerFeed) {
        let (tx, rx) = tokio::sync::mpsc::channel(VIEWER_QUEUE_CHUNKS);
        let queued_bytes = Arc::new(AtomicU64::new(0));
        (
            Arc::new(Self {
                capability,
                cancel,
                queue: std::sync::Mutex::new(Some(SinkQueue {
                    tx,
                    queued_bytes: Arc::clone(&queued_bytes),
                })),
                evicted: std::sync::Mutex::new(None),
                started_behind: AtomicU64::new(0),
            }),
            ViewerFeed { rx, queued_bytes },
        )
    }

    fn is_live(&self) -> bool {
        !self.cancel.is_cancelled()
            && self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
    }

    /// This viewer is being handed the held opening prefix, `bytes` long.
    fn start_behind(&self, bytes: u64) {
        self.started_behind.fetch_max(bytes, Ordering::AcqRel);
    }

    /// How far behind the live edge this viewer may fall before it is
    /// evicted: `VIEWER_QUEUE_BYTES` from where it started.
    fn queue_limit(&self) -> u64 {
        VIEWER_QUEUE_BYTES.saturating_add(self.started_behind.load(Ordering::Acquire))
    }

    /// Why the transport stopped feeding this viewer, if it chose to.
    pub(crate) fn evicted(&self) -> Option<&'static str> {
        *self
            .evicted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn evict(&self, reason: &'static str, metrics: &LiveTvMetrics) {
        let first = {
            let mut evicted = self
                .evicted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = evicted.is_none();
            evicted.get_or_insert(reason);
            first
        };
        if first {
            metrics.observe_consumer_eviction("viewer", reason);
        }
        self.close_feed();
    }

    fn close_feed(&self) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

/// One recording's file within a transport.
pub(crate) struct DvrSink {
    pub(crate) recording_id: String,
    pub(crate) channel_id: String,
    pub(crate) airing_start: i64,
    pub(crate) title: String,
    pub(crate) attempt: i64,
    /// `[capture_start, capture_end)`. Bytes outside it belong to a different
    /// programme and are simply not written here.
    pub(crate) window: (i64, i64),
    pub(crate) base: PathBuf,
    /// The fan-out owns only this bounded queue. The writer task is the sole
    /// owner of the file, so a slow filesystem can never stall the tuner read.
    writer: std::sync::Mutex<Option<SinkQueue>>,
    /// Kept until it settles. A timed-out stop puts the handle back so the
    /// next DVR tick can join it before reading the attempt file.
    writer_task: std::sync::Mutex<Option<tokio::task::JoinHandle<SinkWriterOutcome>>>,
    pub(crate) bytes: AtomicU64,
    pub(crate) prior_attempt_bytes: Option<u64>,
    observation: std::sync::Mutex<DvrSinkObservation>,
    metrics: Arc<LiveTvMetrics>,
    pub(crate) cancel: CancellationToken,
}

#[derive(Clone)]
struct SinkQueue {
    tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    queued_bytes: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SinkWriterOutcome {
    Closed { bytes: u64 },
    Failed { reason: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SinkStopResult {
    NotFound,
    Settled,
    Settling,
}

/// Production needs `sync_all`; tests need an in-memory `AsyncWrite`. Keeping
/// the durability operation on this object-safe seam avoids a test-only field
/// on the shipping sink while preserving the file contract.
trait SinkWriter: tokio::io::AsyncWrite + Send + Unpin {
    fn sync_all<'a>(&'a mut self)
        -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>>;
}

impl SinkWriter for tokio::fs::File {
    fn sync_all<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
        Box::pin(tokio::fs::File::sync_all(self))
    }
}

#[cfg(test)]
impl SinkWriter for tokio::io::DuplexStream {
    fn sync_all<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
        Box::pin(async { tokio::io::AsyncWriteExt::flush(self).await })
    }
}

#[derive(Debug)]
struct DvrSinkObservation {
    phase: &'static str,
    first_write_at_ms: Option<i64>,
    last_write_at: Option<std::time::Instant>,
    rate_samples: VecDeque<(std::time::Instant, u64)>,
    reason_code: Option<String>,
    pending_events: VecDeque<LatchedDvrEvent>,
}

#[derive(Clone, Debug)]
struct LatchedDvrEvent {
    event_id: String,
    kind: &'static str,
    occurred_at_ms: i64,
    reason_code: Option<String>,
    actionable: bool,
}

impl DvrSinkObservation {
    fn new(attempt: i64) -> Self {
        Self {
            phase: if attempt > 1 {
                "reconnecting"
            } else {
                "starting"
            },
            first_write_at_ms: None,
            last_write_at: None,
            rate_samples: VecDeque::new(),
            reason_code: None,
            pending_events: VecDeque::new(),
        }
    }

    fn successful_write(&mut self, now: std::time::Instant, at_ms: i64, total: u64) {
        if !matches!(self.phase, "finishing" | "settling") {
            self.phase = "writing";
        }
        if self.first_write_at_ms.is_none() {
            self.first_write_at_ms = Some(at_ms);
            self.pending_events.push_back(LatchedDvrEvent {
                event_id: uuid::Uuid::new_v4().to_string(),
                kind: "first_bytes_written",
                occurred_at_ms: at_ms,
                reason_code: None,
                actionable: false,
            });
        }
        self.last_write_at = Some(now);
        self.rate_samples.push_back((now, total));
        while self
            .rate_samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > DVR_WRITE_RATE_WINDOW)
        {
            self.rate_samples.pop_front();
        }
    }

    fn begin_finishing(&mut self, at_ms: i64) {
        if self.phase == "finishing" {
            return;
        }
        self.phase = "finishing";
        self.pending_events.push_back(LatchedDvrEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            kind: "finishing",
            occurred_at_ms: at_ms,
            reason_code: None,
            actionable: false,
        });
    }

    fn interrupted(&mut self, reason: &'static str) -> bool {
        let first = self.reason_code.is_none();
        if first {
            self.pending_events.push_back(LatchedDvrEvent {
                event_id: uuid::Uuid::new_v4().to_string(),
                kind: "capture_interrupted",
                occurred_at_ms: unix_millis(),
                reason_code: Some(reason.to_owned()),
                actionable: true,
            });
            self.reason_code = Some(reason.to_owned());
        }
        first
    }

    fn settling(&mut self) {
        self.phase = "settling";
    }

    fn write_bps(&self) -> Option<u64> {
        let (first_at, first_bytes) = self.rate_samples.front()?;
        let (last_at, last_bytes) = self.rate_samples.back()?;
        let elapsed_ms = last_at.duration_since(*first_at).as_millis() as u64;
        (elapsed_ms > 0 && last_bytes > first_bytes).then(|| {
            last_bytes
                .saturating_sub(*first_bytes)
                .saturating_mul(1_000)
                / elapsed_ms
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrCaptureObservation {
    pub(crate) recording_id: String,
    pub(crate) channel_id: String,
    pub(crate) airing_start: i64,
    pub(crate) owner_node_id: String,
    pub(crate) config_generation: i64,
    pub(crate) serving_generation: u64,
    pub(crate) attempt: i64,
    pub(crate) phase: String,
    pub(crate) observation_age_ms: u64,
    pub(crate) last_write_age_ms: Option<u64>,
    pub(crate) first_write_at_ms: Option<i64>,
    pub(crate) attempt_bytes_written: u64,
    pub(crate) prior_attempt_bytes: Option<u64>,
    pub(crate) write_bps: Option<u64>,
    pub(crate) reason_code: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrObservationSnapshot {
    pub(crate) observed_sink_count: usize,
    pub(crate) truncated: bool,
    pub(crate) observations: Vec<DvrCaptureObservation>,
    pub(crate) recording_transports: usize,
    #[serde(default)]
    pub(crate) storage_free_bytes: Option<u64>,
}

impl DvrSink {
    fn final_path(&self) -> PathBuf {
        final_path(&self.base)
    }
}

fn reserve_sink_queue_bytes(queued: &AtomicU64, bytes: usize) -> bool {
    reserve_queue_bytes(queued, bytes, DVR_SINK_QUEUE_BYTES)
}

fn reserve_queue_bytes(queued: &AtomicU64, bytes: usize, limit: u64) -> bool {
    let Ok(bytes) = u64::try_from(bytes) else {
        return false;
    };
    queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(bytes).filter(|next| *next <= limit)
        })
        .is_ok()
}

fn interrupt_sink(sink: &Arc<DvrSink>, reason: &'static str) {
    let first = sink
        .observation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .interrupted(reason);
    if first {
        sink.metrics.observe_dvr_sink_failure(reason);
    }
    sink.cancel.cancel();
    sink.writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
}

fn start_sink_writer(sink: &Arc<DvrSink>, writer: Box<dyn SinkWriter>, delivered: Arc<AtomicU64>) {
    let (tx, rx) = tokio::sync::mpsc::channel(DVR_SINK_QUEUE_CHUNKS);
    let queued_bytes = Arc::new(AtomicU64::new(0));
    *sink
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(SinkQueue {
        tx,
        queued_bytes: Arc::clone(&queued_bytes),
    });
    let weak = Arc::downgrade(sink);
    let task =
        tokio::spawn(
            async move { sink_writer_loop(weak, writer, rx, queued_bytes, delivered).await },
        );
    *sink
        .writer_task
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
}

async fn sink_writer_loop(
    sink: std::sync::Weak<DvrSink>,
    mut writer: Box<dyn SinkWriter>,
    mut rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    queued_bytes: Arc<AtomicU64>,
    delivered: Arc<AtomicU64>,
) -> SinkWriterOutcome {
    use tokio::io::AsyncWriteExt as _;

    while let Some(bytes) = rx.recv().await {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        match tokio::time::timeout(TUNER_READ_TIMEOUT, writer.write_all(&bytes)).await {
            Ok(Ok(())) => {
                queued_bytes.fetch_sub(len, Ordering::AcqRel);
                let Some(sink) = sink.upgrade() else {
                    return SinkWriterOutcome::Closed { bytes: 0 };
                };
                let total = sink
                    .bytes
                    .fetch_add(len, Ordering::AcqRel)
                    .saturating_add(len);
                sink.observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .successful_write(std::time::Instant::now(), unix_millis(), total);
                delivered.fetch_add(len, Ordering::Release);
            }
            Ok(Err(error)) => {
                queued_bytes.store(0, Ordering::Release);
                if let Some(sink) = sink.upgrade() {
                    tracing::warn!(
                        recording = %sink.recording_id,
                        %error,
                        "a recording's file could not be written; that capture is stopping"
                    );
                    interrupt_sink(&sink, "disk_write_failed");
                }
                return SinkWriterOutcome::Failed {
                    reason: "disk_write_failed",
                };
            }
            Err(_) => {
                queued_bytes.store(0, Ordering::Release);
                if let Some(sink) = sink.upgrade() {
                    tracing::warn!(
                        recording = %sink.recording_id,
                        "a recording's file write stalled; that capture is stopping"
                    );
                    interrupt_sink(&sink, "disk_write_timeout");
                }
                return SinkWriterOutcome::Failed {
                    reason: "disk_write_timeout",
                };
            }
        }
    }

    let close = async {
        writer.flush().await?;
        writer.sync_all().await
    };
    let failure = match tokio::time::timeout(TUNER_READ_TIMEOUT, close).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => {
            if let Some(sink) = sink.upgrade() {
                tracing::warn!(recording = %sink.recording_id, %error, "closing a recording file");
            }
            Some("disk_write_failed")
        }
        Err(_) => Some("disk_write_timeout"),
    };
    if let Some(reason) = failure {
        if let Some(sink) = sink.upgrade() {
            interrupt_sink(&sink, reason);
        }
        SinkWriterOutcome::Failed { reason }
    } else {
        SinkWriterOutcome::Closed {
            bytes: sink
                .upgrade()
                .map(|sink| sink.bytes.load(Ordering::Acquire))
                .unwrap_or_default(),
        }
    }
}

async fn settle_sink(sink: &Arc<DvrSink>) -> SinkStopResult {
    sink.cancel.cancel();
    sink.writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let task = sink
        .writer_task
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(mut task) = task else {
        return SinkStopResult::Settled;
    };
    match tokio::time::timeout(super::SESSION_DRAIN_TIMEOUT, &mut task).await {
        Ok(Ok(_)) => SinkStopResult::Settled,
        Ok(Err(error)) => {
            tracing::warn!(%error, recording = %sink.recording_id, "capture writer stopped unexpectedly");
            SinkStopResult::Settled
        }
        Err(_) => {
            sink.observation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settling();
            *sink
                .writer_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
            SinkStopResult::Settling
        }
    }
}

/// Every path a recording owns, built by appending to the basename rather than
/// through `Path::with_extension`.
///
/// `with_extension` replaces everything after the *last* dot in the file name,
/// and a recording's basename ends `… - 7.1 - abcdef01`. It would therefore
/// truncate at the dot inside the channel number, throwing away the
/// sub-channel and the id — which are precisely what makes the name unique.
/// Two sub-channels carrying a programme called `News` at six o'clock would
/// have collided on one file.
fn final_path(base: &std::path::Path) -> PathBuf {
    sibling(base, ".ts")
}

fn sidecar_path(base: &std::path::Path) -> PathBuf {
    sibling(base, ".json")
}

fn attempt_path(base: &std::path::Path, attempt: i64) -> PathBuf {
    sibling(base, &format!(".a{attempt}.part"))
}

fn sibling(base: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = base.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

enum SinkAdmission {
    Join(Arc<DvrTransport>),
    Open(Arc<DvrTransport>),
    Replace(Arc<DvrTransport>),
}

/// What `tuner_capacity` tells a refused viewer: which channels are held, and
/// by what, so the client can offer to stop a recording rather than leaving
/// the viewer to guess.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrHolder {
    pub(crate) channel_id: String,
    pub(crate) guide_number: String,
    pub(crate) channel_name: String,
    pub(crate) sinks: Vec<DvrHolderSink>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrHolderSink {
    pub(crate) recording_id: String,
    pub(crate) title: String,
    pub(crate) ends_at: i64,
}

/// One capture, as the Activity page sees it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrActivity {
    pub(crate) recording_id: String,
    pub(crate) title: String,
    pub(crate) channel_number: String,
    pub(crate) channel_name: String,
    pub(crate) owner_node_id: String,
    pub(crate) rule_name: Option<String>,
    pub(crate) bytes: u64,
    pub(crate) seconds_left: i64,
    pub(crate) percent: Option<u8>,
    pub(crate) path: Option<String>,
}

impl LiveTvManager {
    /// Both halves of the configuration from one settings read. Two reads
    /// would be two round trips on the replicated backend and could see two
    /// different commits.
    pub(crate) async fn dvr_configs(&self) -> Result<(LiveTvConfig, DvrConfig), LiveTvError> {
        let snapshot = self.store.settings_snapshot().await.map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}"))
        })?;
        let live_tv = LiveTvConfig::from_snapshot(&snapshot, &self.node_id);
        self.observe_config(&live_tv);
        Ok((live_tv, DvrConfig::from_snapshot(&snapshot)))
    }

    /// Channels currently held by captures, for the capacity refusal body.
    ///
    /// Only transports with a live recording: a transport only viewers hold
    /// is not a recording the client can offer to stop (plan L-03 §2.4 D3).
    pub(crate) fn transport_holders(&self) -> Vec<DvrHolder> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        transports
            .into_iter()
            .filter(|transport| !transport.live_sinks().is_empty())
            .map(|transport| DvrHolder {
                channel_id: transport.channel.id.clone(),
                guide_number: transport.channel.guide_number.clone(),
                channel_name: transport.channel.guide_name.clone(),
                sinks: transport
                    .live_sinks()
                    .into_iter()
                    .map(|sink| DvrHolderSink {
                        recording_id: sink.recording_id.clone(),
                        title: sink.title.clone(),
                        ends_at: sink.window.1,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Channels a refused viewer could watch instead: transports someone is
    /// already watching or recording that would take one more viewer, so a
    /// `tuner_capacity` refusal can offer "watch 2.1 instead" (plan L-03
    /// §3.4). The same row shape as the holders; `sinks` names any recording
    /// on it.
    pub(crate) fn watchable_channels(&self) -> Vec<DvrHolder> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let mut rows = transports
            .into_iter()
            .filter(|transport| {
                !transport.is_closing()
                    && transport.source_known()
                    && self.serving.is_current(transport.owner_serving_generation)
                    && (!transport.live_viewers().is_empty() || !transport.live_sinks().is_empty())
                    && transport.viewer_seats() < MAX_CONSUMERS_PER_TRANSPORT
            })
            .map(|transport| DvrHolder {
                channel_id: transport.channel.id.clone(),
                guide_number: transport.channel.guide_number.clone(),
                channel_name: transport.channel.guide_name.clone(),
                sinks: transport
                    .live_sinks()
                    .into_iter()
                    .map(|sink| DvrHolderSink {
                        recording_id: sink.recording_id.clone(),
                        title: sink.title.clone(),
                        ends_at: sink.window.1,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.guide_number.cmp(&right.guide_number));
        rows
    }

    /// Live captures, for the Activity page.
    ///
    /// Anything on this server that holds a tuner and writes to a disk has to
    /// be attributable from inside the product: what it is, why it chose that
    /// work, and a way to stop it. These rows are that, for recording.
    pub(crate) fn recording_activities(&self) -> Vec<DvrActivity> {
        let now = unix_seconds();
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let mut out = Vec::new();
        for transport in transports {
            for sink in transport.live_sinks() {
                let (start, end) = sink.window;
                let span = (end - start).max(1);
                let elapsed = (now - start).clamp(0, span);
                out.push(DvrActivity {
                    recording_id: sink.recording_id.clone(),
                    title: sink.title.clone(),
                    channel_number: transport.channel.guide_number.clone(),
                    channel_name: transport.channel.guide_name.clone(),
                    owner_node_id: self.node_id.clone(),
                    rule_name: None,
                    bytes: sink.bytes.load(Ordering::Relaxed),
                    seconds_left: (end - now).max(0),
                    percent: u8::try_from(elapsed.saturating_mul(100) / span).ok(),
                    path: sink.final_path().to_str().map(str::to_owned),
                });
            }
        }
        out.sort_by(|left, right| {
            left.channel_number
                .cmp(&right.channel_number)
                .then(left.title.cmp(&right.title))
        });
        out
    }

    /// Current owner-only capture evidence. Every age is derived from a
    /// monotonic clock, and every byte/rate sample was recorded only after a
    /// successful `write_all`; this never stats a file or touches the Store.
    pub(crate) fn capture_observation_snapshot(&self) -> DvrObservationSnapshot {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let recording_transports = transports
            .iter()
            .filter(|transport| !transport.live_sinks().is_empty())
            .count();
        let now = std::time::Instant::now();
        let mut observations = Vec::new();
        for transport in transports {
            if !self.serving.is_current(transport.owner_serving_generation) {
                continue;
            }
            let sinks = transport
                .sinks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            for sink in sinks {
                let sample = sink
                    .observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if sink.cancel.is_cancelled() && sample.phase != "finishing" {
                    continue;
                }
                let last_write_age_ms = sample
                    .last_write_at
                    .map(|at| now.duration_since(at).as_millis() as u64);
                observations.push(DvrCaptureObservation {
                    recording_id: sink.recording_id.clone(),
                    channel_id: sink.channel_id.clone(),
                    airing_start: sink.airing_start,
                    owner_node_id: self.node_id.clone(),
                    config_generation: transport.generation,
                    serving_generation: transport.owner_serving_generation,
                    attempt: sink.attempt,
                    phase: sample.phase.to_owned(),
                    // The registry was inspected synchronously for this
                    // response. Write age is separate: no bytes for 15 s can
                    // be a fresh, unhealthy observation rather than stale
                    // owner evidence.
                    observation_age_ms: 0,
                    last_write_age_ms,
                    first_write_at_ms: sample.first_write_at_ms,
                    attempt_bytes_written: sink.bytes.load(Ordering::Acquire),
                    prior_attempt_bytes: sink.prior_attempt_bytes,
                    write_bps: sample.write_bps(),
                    reason_code: sample.reason_code.clone(),
                });
            }
        }
        observations.sort_by(|left, right| {
            left.recording_id
                .cmp(&right.recording_id)
                .then(left.attempt.cmp(&right.attempt))
        });
        let observed_sink_count = observations.len();
        let truncated = observations.len() > DVR_OBSERVATIONS_MAX;
        observations.truncate(DVR_OBSERVATIONS_MAX);
        DvrObservationSnapshot {
            observed_sink_count,
            truncated,
            observations,
            recording_transports,
            storage_free_bytes: match self.dvr_storage_free_bytes.load(Ordering::Relaxed) {
                u64::MAX => None,
                value => Some(value),
            },
        }
    }

    /// Persist latched sink facts away from the tuner pump. A failed Store
    /// write leaves the event at the front of the queue with the same UUID so
    /// the next tick retries idempotently.
    async fn drain_dvr_observation_events(&self) -> Result<(), LiveTvError> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        for transport in transports {
            let sinks = transport
                .sinks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            for sink in sinks {
                loop {
                    let pending = sink
                        .observation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pending_events
                        .front()
                        .cloned();
                    let Some(pending) = pending else { break };
                    let event = DvrEventInput {
                        event_id: pending.event_id.clone(),
                        kind: pending.kind.to_owned(),
                        occurred_at_ms: pending.occurred_at_ms,
                        attempt: Some(sink.attempt),
                        actor_user_id: None,
                        reason_code: pending.reason_code.clone(),
                        facts_json: serde_json::json!({
                            "attempt_bytes_written": sink.bytes.load(Ordering::Acquire)
                        })
                        .to_string(),
                        actionable: pending.actionable,
                    };
                    self.store
                        .append_dvr_observation_event(
                            &sink.recording_id,
                            &self.node_id,
                            sink.attempt,
                            &event,
                        )
                        .await
                        .map_err(store_error)?;
                    let mut observation = sink
                        .observation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if observation
                        .pending_events
                        .front()
                        .is_some_and(|event| event.event_id == pending.event_id)
                    {
                        observation.pending_events.pop_front();
                    }
                }
            }
        }
        Ok(())
    }

    /// The owner's recording loop.
    ///
    /// Owner-only by construction, like the guide loop, and holding no job
    /// lease: the tuner configuration already names one owner, and a lease
    /// would be a second authority over the same resource that could disagree
    /// with it.
    pub(crate) async fn dvr_loop(
        self: Arc<Self>,
        events: super::webhook::DvrEventSink,
        shutdown: CancellationToken,
    ) {
        let mut last_retention = 0_i64;
        loop {
            if let Ok((live_tv, dvr)) = self.dvr_configs().await {
                let ours = live_tv.owner_node_id == self.node_id;
                if ours && live_tv.enabled && dvr.enabled && self.serving.admit().is_some() {
                    self.dvr_storage_free_bytes.store(
                        crate::live_tv::free_space_bytes(&dvr.root).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    if let Err(error) = self
                        .dvr_tick(&live_tv, &dvr, &events, &mut last_retention)
                        .await
                    {
                        tracing::warn!(
                            code = error.code(),
                            "the recording tick did not complete; it will be retried"
                        );
                    }
                } else if !ours || !live_tv.enabled {
                    self.dvr_storage_free_bytes
                        .store(u64::MAX, Ordering::Relaxed);
                    // Not this node's work any more, or Live TV switched off.
                    // Close what we hold rather than leaving a tuner occupied
                    // by a feature the operator has turned off.
                    self.close_all_transports().await;
                } else if !dvr.enabled {
                    self.dvr_storage_free_bytes
                        .store(u64::MAX, Ordering::Relaxed);
                    // Recording switched off while Live TV stays on: stop the
                    // captures, and leave the transports viewers are watching
                    // (they are not the DVR's to close).
                    self.close_recordings().await;
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => {
                    self.close_all_transports().await;
                    return;
                }
                _ = tokio::time::sleep(DVR_TICK) => {}
            }
        }
    }

    /// One pass. The order matters and is the whole tick:
    ///
    /// recover before scheduling, so a restarted owner resumes what it was
    /// already doing before it plans anything new; terminalise before
    /// allocating, so a tuner is never planned for an airing that can no
    /// longer usefully start; and start last, so a start always acts on a plan
    /// made in this same pass.
    async fn dvr_tick(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        events: &super::webhook::DvrEventSink,
        last_retention: &mut i64,
    ) -> Result<(), LiveTvError> {
        // Read once at the top and carry it through every conditional write:
        // a settings save mid-tick must land no rows from the configuration it
        // replaced. Same discipline as `registry.min_generation` on the live
        // path.
        let generation = live_tv.generation;
        let now = unix_seconds();

        if let Err(error) = self.drain_dvr_observation_events().await {
            tracing::warn!(%error, "could not persist pending DVR observations");
        }
        self.dvr_recover(live_tv, dvr, events, generation, now)
            .await?;
        self.dvr_expand(live_tv, dvr, now).await?;
        self.dvr_reconcile(live_tv, generation, now).await?;
        self.dvr_consume_stops(events, generation).await?;
        self.dvr_terminalise(generation, now).await?;
        self.dvr_allocate(live_tv, dvr, generation, now).await?;
        self.dvr_start(live_tv, dvr, events, generation, now)
            .await?;
        if let Err(error) = self.drain_dvr_observation_events().await {
            tracing::warn!(%error, "could not persist pending DVR observations");
        }
        self.dvr_finish(events, generation, now).await?;
        self.dvr_purge_deleted().await?;
        if now - *last_retention >= DVR_RETENTION_INTERVAL_S {
            *last_retention = now;
            self.dvr_retain(now).await?;
            self.store
                .prune_dvr_events(
                    now.saturating_mul(1000)
                        .saturating_sub(DVR_EVENT_RETENTION_MS),
                    DVR_EVENT_PRUNE_BATCH,
                )
                .await
                .map_err(store_error)?;
        }
        Ok(())
    }

    /// Step 1 — resume what this owner was doing before it stopped being able
    /// to do it.
    ///
    /// A row in `recording` with no live sink is a capture whose worker is
    /// gone: the process restarted, the owner changed, or the transport
    /// failed. While there is still a useful amount of programme left, open
    /// the next attempt — a new file, so the old process draining into the
    /// previous one cannot corrupt it — and record the gap. Otherwise finish
    /// what is on disk honestly.
    async fn dvr_recover(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        events: &super::webhook::DvrEventSink,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let live = self.live_recording_ids();
        let rows = self.dvr_rows(&[DvrState::Recording]).await?;
        for row in rows {
            if live.contains(&row.id) {
                continue;
            }
            if self
                .sink_interruption_reason(&row.id, row.attempt)
                .is_none()
            {
                let interruption = DvrEventInput {
                    // Stable across retries: an owner that could not recover on
                    // this tick must not add the same interruption every 15 s.
                    event_id: format!("worker-lost:{}:{}", row.id, row.attempt),
                    kind: "capture_interrupted".to_owned(),
                    occurred_at_ms: now.saturating_mul(1_000),
                    attempt: Some(row.attempt),
                    actor_user_id: None,
                    reason_code: Some("worker_lost".to_owned()),
                    facts_json: serde_json::json!({}).to_string(),
                    actionable: true,
                };
                if let Err(error) = self
                    .store
                    .append_dvr_observation_event(
                        &row.id,
                        &self.node_id,
                        row.attempt,
                        &interruption,
                    )
                    .await
                {
                    tracing::warn!(recording = %row.id, %error, "could not persist worker-loss event");
                }
            }
            if let Err(error) = self
                .store
                .mark_dvr_history_gap(&row.id, &self.node_id, row.attempt)
                .await
            {
                // Provenance is advisory to the recovery path. A database
                // hiccup must not consume the remaining capture window.
                tracing::warn!(recording = %row.id, %error, "could not mark recovered DVR history incomplete");
            }
            // `started_at_ms`, never `created_at_ms`: the row was inserted by
            // expansion, possibly a fortnight before the capture opened, and a
            // capture that failed before its first 30-second progress write
            // would otherwise report a two-week gap.
            let last_progress = row
                .last_progress_ms
                .or(row.started_at_ms)
                .unwrap_or(row.created_at_ms)
                / 1000;
            let gap = (now - last_progress).max(0);
            if now < row.capture_end - DVR_MIN_USEFUL_S && row.attempt < DVR_ATTEMPTS_MAX {
                let attempt = row.attempt + 1;
                match self
                    .attach_sink(live_tv, dvr, &row, attempt, generation)
                    .await
                {
                    Ok(()) => {
                        self.transition(
                            &row.id,
                            &[DvrState::Recording],
                            DvrState::Recording,
                            Some("resumed after the capture lost its worker"),
                            DvrStatePatch::Reattempt {
                                attempt,
                                gap_s: row.gap_s + gap,
                            },
                            Some(generation),
                        )
                        .await?;
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(
                            recording = %row.id,
                            code = error.code(),
                            "could not resume a capture; it will be retried next tick"
                        );
                        continue;
                    }
                }
            }
            let reason = if row.attempt >= DVR_ATTEMPTS_MAX {
                "attempts exhausted"
            } else {
                "worker lost"
            };
            self.finish_row(&row, events, now, gap, None, reason, generation)
                .await?;
        }
        Ok(())
    }

    /// Step 2 — materialise the airings every enabled rule matches.
    ///
    /// Insert-if-absent, always. A rule that keeps matching an airing the
    /// viewer skipped must not bring it back, and this is the only place that
    /// could.
    async fn dvr_expand(
        &self,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let rules = self.store.list_dvr_rules().await.map_err(store_error)?;
        if rules.is_empty() || !live_tv.guide_fetches() {
            return Ok(());
        }
        let Some(guide) = self.local_guide_view(live_tv).await else {
            return Ok(());
        };
        let window = GuideWindow {
            start: now,
            end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
        };
        let lineup = self.cached_lineup(live_tv).await;
        let at_ms = now.saturating_mul(1000);
        for channel in &guide.guide.channels {
            let name = lineup
                .iter()
                .find(|entry| entry.id == channel.id)
                .map(|entry| entry.guide_name.clone())
                .unwrap_or_else(|| channel.guide_number.clone());
            for programme in channel.programmes_in(&window) {
                if programme.start <= now {
                    continue;
                }
                // Highest-priority matching rule wins the row it creates; the
                // rest are simply also true, and reconciliation keeps the
                // pointer current if priorities change later.
                let Some(rule) = rules
                    .iter()
                    .filter(|rule| crate::http::dvr::rule_matches(rule, &channel.id, programme))
                    .min_by_key(|rule| rule.priority)
                else {
                    continue;
                };
                let row = crate::http::dvr::recording_from_programme(
                    &crate::http::dvr::ResolvedChannel {
                        id: channel.id.clone(),
                        guide_number: channel.guide_number.clone(),
                        name: name.clone(),
                    },
                    programme,
                    DvrOrigin::Rule,
                    Some(rule.id.clone()),
                    Some(rule.owner_user_id),
                    rule.pad_start_s,
                    rule.pad_end_s,
                    at_ms,
                );
                let event = DvrEventInput {
                    event_id: uuid::Uuid::new_v4().to_string(),
                    kind: "scheduled".to_owned(),
                    occurred_at_ms: at_ms,
                    attempt: None,
                    actor_user_id: Some(rule.owner_user_id),
                    reason_code: None,
                    facts_json: serde_json::json!({"origin": "rule", "rule_id": rule.id})
                        .to_string(),
                    actionable: false,
                };
                match self
                    .store
                    .insert_dvr_airing_with_event(&row, &event)
                    .await
                    .map_err(store_error)?
                {
                    DvrInsertOutcome::Inserted => tracing::info!(
                        recording = %row.id,
                        rule = %rule.name,
                        title = %row.title,
                        "scheduled a recording from a rule"
                    ),
                    DvrInsertOutcome::Exists(_) => {}
                }
                let _ = dvr;
            }
        }
        Ok(())
    }

    /// Step 3 — keep every pending row pointing at a rule that still wants it,
    /// and surface the ones nothing does.
    async fn dvr_reconcile(
        &self,
        live_tv: &LiveTvConfig,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let rules = self.store.list_dvr_rules().await.map_err(store_error)?;
        let rows = self.dvr_rows(DvrState::PENDING).await?;
        if rows.is_empty() {
            return Ok(());
        }
        // An empty guide is what the cache returns while it is cold, for a
        // generation that has just changed, and past the stale window — none
        // of which is evidence that a programme moved. Reconciling against one
        // would withdraw every rule row and, worse, mark every manual row
        // `stale`, from which nothing brings it back. A settings save would
        // have silently killed every recording a person had asked for.
        let window = GuideWindow {
            start: now - 3600,
            end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
        };
        let guide = if live_tv.guide_fetches() {
            self.local_guide_view(live_tv)
                .await
                .filter(|view| !view.guide.channels.is_empty())
        } else {
            None
        };
        let mut repoint = Vec::new();
        for row in rows {
            // Does the guide still carry this exact airing, under this title?
            let still_listed = guide.as_ref().map(|guide| {
                guide.guide.channels.iter().any(|channel| {
                    channel.id == row.channel_id
                        && channel.programmes_in(&window).iter().any(|programme| {
                            programme.start == row.airing_start && programme.title == row.title
                        })
                })
            });
            if still_listed == Some(false) && row.airing_start > now {
                match row.origin {
                    DvrOrigin::Rule => {
                        self.transition(
                            &row.id,
                            DvrState::PENDING,
                            DvrState::Withdrawn,
                            Some("the guide no longer lists this programme at that time"),
                            DvrStatePatch::None,
                            Some(generation),
                        )
                        .await?;
                    }
                    // A person asked for this one, so it is theirs to
                    // re-decide. Surfaced, never withdrawn.
                    DvrOrigin::Manual => {
                        if row.state != DvrState::Stale {
                            self.transition(
                                &row.id,
                                DvrState::PENDING,
                                DvrState::Stale,
                                Some("the programme moved or was renamed"),
                                DvrStatePatch::None,
                                Some(generation),
                            )
                            .await?;
                        }
                    }
                }
                continue;
            }
            if row.origin != DvrOrigin::Rule {
                // A manual row marked `stale` because its programme vanished
                // is scheduled again the moment the guide lists it once more.
                // Without this, `stale` is a one-way door: nothing else moves
                // a row out of it, and the viewer cannot even re-create the
                // recording, because the airing is already spoken for.
                if row.state == DvrState::Stale && still_listed == Some(true) {
                    self.transition(
                        &row.id,
                        &[DvrState::Stale],
                        DvrState::Scheduled,
                        None,
                        DvrStatePatch::None,
                        Some(generation),
                    )
                    .await?;
                }
                continue;
            }
            let owner = guide.as_ref().and_then(|guide| {
                let programme = guide.guide.channels.iter().find_map(|channel| {
                    (channel.id == row.channel_id)
                        .then(|| {
                            channel
                                .programmes_in(&window)
                                .iter()
                                .find(|programme| programme.start == row.airing_start)
                        })
                        .flatten()
                })?;
                rules
                    .iter()
                    .filter(|rule| crate::http::dvr::rule_matches(rule, &row.channel_id, programme))
                    .min_by_key(|rule| rule.priority)
                    .map(|rule| rule.id.clone())
            });
            match owner {
                Some(rule_id) => {
                    if row.rule_id.as_deref() != Some(rule_id.as_str()) {
                        repoint.push((row.id.clone(), Some(rule_id)));
                    }
                    // A row that was withdrawn and now matches again comes
                    // back: withdrawal is the absence of a rule, not a
                    // decision anybody made.
                    if row.state == DvrState::Withdrawn {
                        self.transition(
                            &row.id,
                            &[DvrState::Withdrawn],
                            DvrState::Scheduled,
                            None,
                            DvrStatePatch::None,
                            Some(generation),
                        )
                        .await?;
                    }
                }
                None if guide.is_some() && row.state != DvrState::Withdrawn => {
                    self.transition(
                        &row.id,
                        DvrState::PENDING,
                        DvrState::Withdrawn,
                        Some("no enabled rule matches this programme any more"),
                        DvrStatePatch::None,
                        Some(generation),
                    )
                    .await?;
                }
                None => {}
            }
        }
        if !repoint.is_empty() {
            self.store
                .repoint_dvr_rule_rows(&repoint, now.saturating_mul(1000))
                .await
                .map_err(store_error)?;
        }
        Ok(())
    }

    /// Step 4 — act on the Stop a viewer pressed, possibly on another node.
    async fn dvr_consume_stops(
        &self,
        events: &super::webhook::DvrEventSink,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        let rows = self.dvr_rows(&[DvrState::Recording]).await?;
        for row in rows {
            if row.stop_requested_at_ms.is_none() {
                continue;
            }
            let now = unix_seconds();
            // Read the probe's facts before the transport closes: `stop_sink`
            // may take the last sink with it, and a sidecar without the source
            // codec is the one thing a stopped recording would otherwise lack
            // that a completed one has.
            let facts = self.transport_source_facts(&row.channel_id);
            self.begin_finishing(&row.id, row.attempt);
            if let Err(error) = self.drain_dvr_observation_events().await {
                tracing::warn!(recording = %row.id, %error, "could not persist finishing event");
                self.store
                    .mark_dvr_history_gap(&row.id, &self.node_id, row.attempt)
                    .await
                    .map_err(store_error)?;
            }
            if self.stop_sink(&row.id, row.attempt).await == SinkStopResult::Settling {
                tracing::warn!(
                    recording = %row.id,
                    "capture file is still settling; stop will finish on the next DVR tick"
                );
                continue;
            }
            self.finish_row_with_facts(
                &row,
                facts,
                events,
                now,
                0,
                row.stop_requested_by_user_id,
                "stopped by a viewer",
                generation,
            )
            .await?;
            self.close_finished_transports().await;
        }
        Ok(())
    }

    /// Step 5 — anything that can no longer usefully start never starts.
    ///
    /// Without this a restarted owner would find an airing whose window closed
    /// hours ago still marked `scheduled`, tune its channel, and record
    /// whatever happens to be on now under last night's title.
    async fn dvr_terminalise(&self, generation: i64, now: i64) -> Result<(), LiveTvError> {
        for row in self.dvr_rows(DvrState::PENDING).await? {
            if now < row.capture_end - DVR_MIN_USEFUL_S {
                continue;
            }
            let reason = match row.state {
                DvrState::Conflict => row
                    .state_reason
                    .clone()
                    .unwrap_or_else(|| "no tuner was free".to_owned()),
                DvrState::Withdrawn | DvrState::Stale => {
                    "the programme was no longer scheduled".to_owned()
                }
                _ => "the server was not recording when it should have started".to_owned(),
            };
            self.transition(
                &row.id,
                DvrState::PENDING,
                DvrState::Missed,
                Some(&reason),
                DvrStatePatch::None,
                Some(generation),
            )
            .await?;
        }
        Ok(())
    }

    /// Step 6 — recompute which airings have a tuner.
    async fn dvr_allocate(
        &self,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let rows = self
            .dvr_rows(&[DvrState::Scheduled, DvrState::Conflict, DvrState::Recording])
            .await?
            .into_iter()
            .filter(|row| row.capture_start <= now + DVR_SCHEDULE_DAYS_MAX * 86_400)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Ok(());
        }
        let priorities = self
            .store
            .list_dvr_rules()
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|rule| (rule.id, rule.priority))
            .collect::<BTreeMap<_, _>>();
        let floor_met = super::free_space_bytes(&dvr.root).is_none_or(|free| {
            free >= (dvr.free_floor_gb.max(0) as u64).saturating_mul(1_000_000_000)
        });
        let plan = schedule::allocate(
            &rows,
            dvr.recording_slots(live_tv.max_sessions),
            floor_met,
            &|rule_id| priorities.get(rule_id).copied().unwrap_or(i64::MAX),
        );
        for entry in plan {
            let current = rows.iter().find(|row| row.id == entry.id);
            let unchanged = current.is_some_and(|row| {
                row.state == entry.state && row.state_reason.as_deref() == entry.reason.as_deref()
            });
            if unchanged {
                continue;
            }
            self.transition(
                &entry.id,
                &[DvrState::Scheduled, DvrState::Conflict],
                entry.state,
                entry.reason.as_deref(),
                DvrStatePatch::None,
                Some(generation),
            )
            .await?;
        }
        Ok(())
    }

    /// Step 7 — open the captures whose moment has come.
    async fn dvr_start(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        events: &super::webhook::DvrEventSink,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let live = self.live_recording_ids();
        for row in self.dvr_rows(&[DvrState::Scheduled]).await? {
            if row.capture_start > now || now >= row.capture_end - DVR_MIN_USEFUL_S {
                continue;
            }
            if live.contains(&row.id) {
                continue;
            }
            if let Err(error) = self.attach_sink(live_tv, dvr, &row, 1, generation).await {
                tracing::warn!(
                    recording = %row.id,
                    code = error.code(),
                    "could not start a capture; it will be retried next tick"
                );
                continue;
            }
            let late = (now - row.capture_start).max(0);
            // If the row moved under us — a viewer cancelled it, or the
            // configuration generation changed — the capture that is already
            // opened has no row to belong to. Close it rather than letting a
            // file grow for a recording nobody asked for.
            let claimed = self
                .transition(
                    &row.id,
                    &[DvrState::Scheduled],
                    DvrState::Recording,
                    (late > 0).then_some("started late"),
                    DvrStatePatch::Started {
                        attempt: 1,
                        tuner_owner_node_id: self.node_id.clone(),
                        started_at_ms: now.saturating_mul(1000),
                        late_start_s: late,
                        path: self
                            .recording_final_path(dvr, &row)
                            .to_string_lossy()
                            .into_owned(),
                    },
                    Some(generation),
                )
                .await?;
            if !claimed {
                tracing::info!(
                    recording = %row.id,
                    "the recording changed under its own start; closing the capture"
                );
                let _ = self.stop_sink(&row.id, 1).await;
                self.close_finished_transports().await;
                continue;
            }
            tracing::info!(
                recording = %row.id,
                title = %row.title,
                channel = %row.guide_number,
                "recording started"
            );
            events.enqueue(super::webhook::DvrEvent::RecordingStarted {
                recording_id: row.id.clone(),
                channel_id: row.channel_id.clone(),
                guide_number: row.guide_number.clone(),
                title: row.title.clone(),
                airing_start: row.airing_start,
                capture_end: row.capture_end,
            });
        }
        Ok(())
    }

    /// Step 8 — close the captures whose window has passed.
    async fn dvr_finish(
        &self,
        events: &super::webhook::DvrEventSink,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let mut finished = HashSet::new();
        for transport in &transports {
            for sink in transport.live_sinks() {
                if now < sink.window.1 {
                    continue;
                }
                finished.insert(sink.recording_id.clone());
            }
        }
        for row in self.dvr_rows(&[DvrState::Recording]).await? {
            if !finished.contains(&row.id) && now < row.capture_end {
                continue;
            }
            // Always through `stop_sink`, which flushes and takes the file.
            // Cancelling alone leaves a buffered `tokio::fs::File` whose last
            // writes have not reached the kernel, and the concatenation below
            // would then copy a short part and unlink it — losing exactly the
            // end of every recording that finished normally.
            self.begin_finishing(&row.id, row.attempt);
            if let Err(error) = self.drain_dvr_observation_events().await {
                tracing::warn!(recording = %row.id, %error, "could not persist finishing event");
                self.store
                    .mark_dvr_history_gap(&row.id, &self.node_id, row.attempt)
                    .await
                    .map_err(store_error)?;
            }
            if self.stop_sink(&row.id, row.attempt).await == SinkStopResult::Settling {
                tracing::warn!(
                    recording = %row.id,
                    "capture file is still settling; assembly waits for the next DVR tick"
                );
                continue;
            }
            self.finish_row(&row, events, now, 0, None, "capture complete", generation)
                .await?;
        }
        self.close_finished_transports().await;
        Ok(())
    }

    /// Remove the files of recordings a person deleted.
    ///
    /// The route writes the decision and cannot act on it: a viewer may press
    /// Delete on any node, and only the owner has the DVR root. So a `deleted`
    /// row that still names a path is a standing instruction, consumed here
    /// and then cleared — which is also what stops this running twice on the
    /// same file for ever.
    async fn dvr_purge_deleted(&self) -> Result<(), LiveTvError> {
        for row in self.dvr_rows(&[DvrState::Deleted]).await? {
            if row.path.is_none() {
                continue;
            }
            self.delete_recording_files(&row).await;
            self.transition(
                &row.id,
                &[DvrState::Deleted],
                DvrState::Deleted,
                row.state_reason.as_deref(),
                DvrStatePatch::Purged,
                None,
            )
            .await?;
        }
        Ok(())
    }

    /// Step 9 — apply each rule's keep policy, hourly.
    async fn dvr_retain(&self, now: i64) -> Result<(), LiveTvError> {
        let rules = self.store.list_dvr_rules().await.map_err(store_error)?;
        let rows = self.dvr_rows(&[DvrState::Done, DvrState::Partial]).await?;
        for rule in rules {
            let mut mine = rows
                .iter()
                .filter(|row| row.rule_id.as_deref() == Some(rule.id.as_str()))
                .collect::<Vec<_>>();
            mine.sort_by_key(|row| std::cmp::Reverse(row.airing_start));
            let doomed: Vec<&&DvrRecording> = match rule.keep_mode {
                DvrKeepMode::All => Vec::new(),
                DvrKeepMode::LastN => mine.iter().skip(rule.keep_value.max(0) as usize).collect(),
                DvrKeepMode::Days => mine
                    .iter()
                    .filter(|row| now - row.airing_start > rule.keep_value.max(0) * 86_400)
                    .collect(),
                DvrKeepMode::UntilWatched => {
                    // "Until watched" means the rule's owner watched it — not
                    // anyone in the house. A recording deleted because a
                    // different viewer finished it is the failure this policy
                    // exists to avoid. A row the scan has not linked yet has
                    // no item to have been watched, and is therefore kept.
                    let mut watched = Vec::new();
                    for row in &mine {
                        let Some(item_id) = row.item_id else {
                            continue;
                        };
                        let seen = self
                            .store
                            .watch_state(rule.owner_user_id, item_id)
                            .await
                            .map_err(store_error)?
                            .is_some_and(|state| state.watched);
                        if seen {
                            watched.push(*row);
                        }
                    }
                    mine.iter().filter(|row| watched.contains(row)).collect()
                }
            };
            for row in doomed {
                self.delete_recording_files(row).await;
                self.transition(
                    &row.id,
                    &[DvrState::Done, DvrState::Partial],
                    DvrState::Deleted,
                    Some(&format!(
                        "removed by the rule's keep policy ({})",
                        rule.name
                    )),
                    DvrStatePatch::None,
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    // ---- capture plumbing -------------------------------------------------

    /// Join the transport already tuned to this channel, or open a new one.
    ///
    /// Opening needs a tuner slot; joining does not, because the bytes are
    /// already being received and a second file costs no hardware.
    pub(crate) async fn attach_sink(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        row: &DvrRecording,
        attempt: i64,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        if dvr.root.trim().is_empty() {
            return Err(LiveTvError::InvalidConfig(
                "no DVR root is configured, so a recording has nowhere to go".to_owned(),
            ));
        }
        let channel = self
            .cached_lineup(live_tv)
            .await
            .into_iter()
            .find(|channel| channel.id == row.channel_id)
            .ok_or_else(|| {
                LiveTvError::ChannelNotFound(
                    "the channel this recording names is not in the lineup".to_owned(),
                )
            })?;
        if channel.drm {
            return Err(LiveTvError::DrmUnsupported(
                "this channel is protected and cannot be recorded".to_owned(),
            ));
        }
        let base = self.recording_base_path(dvr, row);
        let address = live_tv.device_ipv4.ok_or_else(|| {
            LiveTvError::InvalidConfig("an HDHomeRun IPv4 address is required".to_owned())
        })?;
        let client = self
            .client
            .as_ref()
            .map_err(|error| {
                LiveTvError::DeviceUnavailable(format!("the HTTP client is unavailable: {error}"))
            })?
            .clone();
        let serving_generation = self.serving.admit().ok_or_else(|| {
            LiveTvError::OwnerUnavailable(crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned())
        })?;

        let device_id = self.cached_device_id(live_tv).await.unwrap_or_default();

        // Decide about the tuner before touching the disk. The attempt file is
        // created with `O_EXCL`, so a file left behind by a refusal makes this
        // attempt number unusable for ever: the retry next tick would find its
        // own orphan and read it as a fenced predecessor's. Every fallible step
        // that does not need the file therefore happens first, and the two
        // that remain after it undo themselves.
        //
        // The decision and the reservation are one registry lock hold (plan
        // L-03 §2.4 D2): a new transport is inserted here, before the file is
        // opened, holding the seat this sink will take; a joined transport
        // has that seat reserved on it. Nothing is ever inserted over a live
        // entry.
        let mut retried = false;
        let (transport, opened) = loop {
            let decision = {
                let mut registry = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if registry.closing {
                    return Err(LiveTvError::OwnerUnavailable(
                        "this node is shutting down".to_owned(),
                    ));
                }
                match registry.transports.get(&row.channel_id).cloned() {
                    Some(transport)
                        if !transport.is_closing()
                            && transport.same_tuner(&device_id, address)
                            && transport.owner_serving_generation == serving_generation
                            && schedule::may_share_transport(
                                transport.open_until(),
                                row.capture_start,
                            ) =>
                    {
                        // A recording joining a transport only viewers hold
                        // needs no tuner, but it turns that transport into a
                        // recording transport, so the reserve is asked now
                        // (plan §3.4).
                        if !transport.is_recording()
                            && !registry.may_add_recording(live_tv.max_sessions, dvr.tuner_reserve)
                        {
                            return Err(LiveTvError::Capacity(
                                "no recording slot is free for this channel's tuner".to_owned(),
                            ));
                        }
                        if transport.reserve_seat() {
                            SinkAdmission::Join(transport)
                        } else {
                            SinkAdmission::Replace(transport)
                        }
                    }
                    Some(transport) => SinkAdmission::Replace(transport),
                    None => {
                        if !registry.may_open_transport(live_tv.max_sessions, dvr.tuner_reserve) {
                            return Err(LiveTvError::Capacity(
                                "no tuner is free for this recording".to_owned(),
                            ));
                        }
                        let transport = DvrTransport::new(TransportInit {
                            channel: channel.clone(),
                            generation,
                            owner_serving_generation: serving_generation,
                            device_id: device_id.clone(),
                            address,
                            origin: TransportOrigin::Recording,
                            scratch: self.transport_scratch(),
                            metrics: Arc::clone(&self.metrics),
                            seats: 1,
                        });
                        registry
                            .transports
                            .insert(row.channel_id.clone(), Arc::clone(&transport));
                        SinkAdmission::Open(transport)
                    }
                }
            };
            match decision {
                SinkAdmission::Join(transport) => break (transport, false),
                SinkAdmission::Open(transport) => break (transport, true),
                SinkAdmission::Replace(existing) => {
                    // An entry that cannot take this sink: closing, fenced,
                    // another device, or its windows have passed. Viewers
                    // still watching it are not the DVR's to end.
                    if !existing.is_closing() && !existing.live_viewers().is_empty() {
                        return Err(LiveTvError::Capacity(
                            "this channel's tuner is held by viewers on a connection a recording cannot share"
                                .to_owned(),
                        ));
                    }
                    if retried {
                        return Err(LiveTvError::Capacity(
                            "this channel's previous tuner connection is still closing".to_owned(),
                        ));
                    }
                    retried = true;
                    self.close_transport_arc(existing).await;
                }
            }
        };

        let opened_file = async {
            if let Some(parent) = base.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    LiveTvError::StreamFailed(format!("creating the recording folder: {error}"))
                })?;
            }
            // `create_new` is the whole guarantee against a fenced predecessor:
            // an attempt file that already exists means another process owns
            // it, and this attempt takes the next number rather than
            // truncating it.
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(attempt_path(&base, attempt))
                .await
                .map_err(|error| {
                    LiveTvError::StreamFailed(format!(
                        "opening attempt {attempt} of this recording: {error}"
                    ))
                })
        }
        .await;
        let file = match opened_file {
            Ok(file) => file,
            Err(error) => {
                transport.release_seat();
                if opened {
                    self.abandon_unstarted_transport(&transport);
                }
                return Err(error);
            }
        };
        let sink = Arc::new(DvrSink {
            recording_id: row.id.clone(),
            channel_id: row.channel_id.clone(),
            airing_start: row.airing_start,
            title: row.title.clone(),
            attempt,
            window: (row.capture_start, row.capture_end),
            base: base.clone(),
            writer: std::sync::Mutex::new(None),
            writer_task: std::sync::Mutex::new(None),
            bytes: AtomicU64::new(0),
            prior_attempt_bytes: prior_attempt_bytes(&base, attempt).await,
            observation: std::sync::Mutex::new(DvrSinkObservation::new(attempt)),
            metrics: Arc::clone(&self.metrics),
            cancel: CancellationToken::new(),
        });
        start_sink_writer(&sink, Box::new(file), Arc::clone(&transport.delivered));

        // Publish the sink only once its file is open, so the fan-out never
        // sees a sink it cannot write to; then give back the seat that kept
        // the transport from retiring while the file was opened.
        let attached = transport.attach_sink(Arc::clone(&sink));
        transport.release_seat();
        if !attached {
            let _ = settle_sink(&sink).await;
            if opened {
                self.abandon_unstarted_transport(&transport);
            }
            return Err(LiveTvError::StreamFailed(
                "this channel's tuner connection closed while the recording file was opened"
                    .to_owned(),
            ));
        }
        if opened {
            self.spawn_transport_worker(&transport, client);
        }
        Ok(())
    }

    /// Forget a transport that was reserved and never started: it holds no
    /// tuner, so there is nothing to join; it only has to leave the registry
    /// and let anyone waiting on it proceed.
    pub(crate) fn abandon_unstarted_transport(&self, transport: &Arc<DvrTransport>) {
        transport.cancel.cancel();
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = transport.channel.id.clone();
            if registry
                .transports
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, transport))
            {
                registry.transports.remove(&key);
            }
        }
        transport.finish(&Err(LiveTvError::StreamFailed(
            "the tuner connection was never opened".into(),
        )));
        transport.closed.cancel();
    }

    /// Start the one task that owns a transport's tuner response.
    pub(crate) fn spawn_transport_worker(
        self: &Arc<Self>,
        transport: &Arc<DvrTransport>,
        client: reqwest::Client,
    ) {
        let manager = Arc::downgrade(self);
        let worker_transport = Arc::clone(transport);
        let serving = self.serving.clone();
        let worker = tokio::spawn(async move {
            let result = run_transport(
                client,
                manager.clone(),
                serving,
                Arc::clone(&worker_transport),
            )
            .await;
            worker_transport.finish(&result);
            if let Some(manager) = manager.upgrade() {
                // Cleanup must not await the JoinHandle of the task that is
                // currently executing this block. Hand it to a sibling task
                // so the worker can become joinable before close_transport
                // settles writers and collects it. The exact transport, not
                // whatever the channel's entry is by then.
                let transport = Arc::clone(&worker_transport);
                tokio::spawn(async move {
                    manager.close_transport_arc(transport).await;
                });
            }
            result
        });
        *transport
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
    }

    /// Cancel every writer for one recording through the row's exact attempt.
    /// The transport lives on while any other recording still wants bytes —
    /// stopping one of two recordings on a channel frees no tuner, and
    /// pretending otherwise is what would make the capacity dialog lie.
    async fn stop_sink(&self, recording_id: &str, attempt: i64) -> SinkStopResult {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let sinks = transports
            .iter()
            .flat_map(|transport| {
                transport
                    .sinks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .filter(|sink| sink.recording_id == recording_id)
            .collect::<Vec<_>>();
        if sinks.iter().any(|sink| sink.attempt > attempt) {
            // The row this finalizer read is stale: recovery has already
            // attached a later attempt. Do not cancel that writer and, more
            // importantly, do not let the caller assemble/unlink its part as
            // though the older row snapshot were authoritative.
            return SinkStopResult::Settling;
        }
        if sinks.is_empty() {
            return SinkStopResult::NotFound;
        }
        let mut settling = false;
        for sink in sinks {
            // Assembly reads every part through the row's current attempt, so
            // every matching writer through that attempt must be joined first.
            settling |= settle_sink(&sink).await == SinkStopResult::Settling;
        }
        if settling {
            SinkStopResult::Settling
        } else {
            SinkStopResult::Settled
        }
    }

    /// Publish the closure phase before removing a sink from the write fanout.
    /// Its event is drained before file assembly starts, while the observation
    /// remains in the registry until the terminal row is committed.
    fn begin_finishing(&self, recording_id: &str, attempt: i64) -> bool {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let sinks = transports
            .iter()
            .flat_map(|transport| {
                transport
                    .sinks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .filter(|sink| sink.recording_id == recording_id)
            .collect::<Vec<_>>();
        if sinks.iter().any(|sink| sink.attempt > attempt) {
            return false;
        }
        let mut found = false;
        for sink in sinks.into_iter().filter(|sink| sink.attempt == attempt) {
            sink.observation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .begin_finishing(unix_seconds().saturating_mul(1_000));
            found = true;
        }
        found
    }

    fn live_recording_ids(&self) -> HashSet<String> {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry
            .transports
            .values()
            .flat_map(|transport| {
                transport
                    .live_sinks()
                    .into_iter()
                    .map(|sink| sink.recording_id.clone())
            })
            .collect()
    }

    /// A cancelled sink can still explain why the worker disappeared. The
    /// observation remains on the attempt after its durable event drains, so
    /// recovery can preserve the disk/backlog reason instead of appending a
    /// second actionable `worker_lost` diagnosis for the same interruption.
    fn sink_interruption_reason(&self, recording_id: &str, attempt: i64) -> Option<String> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        transports.into_iter().find_map(|transport| {
            transport
                .sinks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .find(|sink| sink.recording_id == recording_id && sink.attempt == attempt)
                .and_then(|sink| {
                    sink.observation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .reason_code
                        .clone()
                })
        })
    }

    /// Close every transport nothing wants any more. "Nothing" includes
    /// viewers and admitted-but-unattached consumers: a transport only viewers
    /// hold is not finished because no recording is on it (§2.4 D3), and the
    /// retirement is atomic against a start reserving a seat.
    async fn close_finished_transports(&self) {
        let spent = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .transports
                .values()
                .filter(|transport| transport.try_retire())
                .cloned()
                .collect::<Vec<_>>()
        };
        for transport in spent {
            self.close_transport_arc(transport).await;
        }
    }

    /// Recording switched off: stop every capture, and close only the
    /// transports that nothing else then wants.
    async fn close_recordings(&self) {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        for transport in transports {
            for sink in transport.live_sinks() {
                let _ = settle_sink(&sink).await;
            }
            if transport.try_retire() {
                self.close_transport_arc(transport).await;
            }
        }
    }

    /// Close whatever transport is registered for this channel now.
    pub(crate) async fn close_transport(&self, channel_id: &str) {
        let transport = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.get(channel_id).cloned()
        };
        if let Some(transport) = transport {
            self.close_transport_arc(transport).await;
        }
    }

    /// Close exactly this transport: cancel it, settle its sinks, close its
    /// viewers' feeds, take it out of the registry if it is still the entry
    /// there, join the worker that owns the tuner response, then say so.
    ///
    /// The slot is free at the removal; the tuner is released at the join,
    /// up to `SESSION_DRAIN_TIMEOUT` later (§2.4 D4). A start that needs this
    /// transport gone waits on `closed()`, which fires after the join.
    pub(crate) async fn close_transport_arc(&self, transport: Arc<DvrTransport>) {
        transport.cancel.cancel();
        let sinks = transport
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut settling = false;
        for sink in sinks {
            settling |= settle_sink(&sink).await == SinkStopResult::Settling;
        }
        transport.close_viewer_feeds();
        if settling {
            return;
        }
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = transport.channel.id.clone();
            if registry
                .transports
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &transport))
            {
                registry.transports.remove(&key);
            }
        }
        let worker = transport
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = tokio::time::timeout(super::SESSION_DRAIN_TIMEOUT, worker).await;
        }
        // Absent is common (the probe never wrote); anything left behind is
        // the orphan sweeper's once this transport is out of the registry.
        let _ = tokio::fs::remove_dir_all(&transport.scratch).await;
        transport.closed.cancel();
    }

    pub(crate) async fn close_all_transports(&self) {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        for transport in transports {
            self.close_transport_arc(transport).await;
        }
    }

    // ---- rows and files ---------------------------------------------------

    async fn dvr_rows(&self, states: &[DvrState]) -> Result<Vec<DvrRecording>, LiveTvError> {
        self.store
            .list_dvr_recordings_in(states)
            .await
            .map_err(store_error)
    }

    async fn transition(
        &self,
        id: &str,
        from: &[DvrState],
        to: DvrState,
        reason: Option<&str>,
        patch: DvrStatePatch,
        fence_generation: Option<i64>,
    ) -> Result<bool, LiveTvError> {
        let now_ms = unix_seconds().saturating_mul(1000);
        let (kind, actionable) = match (&patch, to) {
            (DvrStatePatch::Started { .. }, _) => ("attempt_started", false),
            // A successful reattach resolves an interruption. The earlier
            // actionable event remains reviewable in historical attention;
            // the recovery itself must not create a new current problem.
            (DvrStatePatch::Reattempt { .. }, _) => ("retry_started", false),
            (_, DvrState::Conflict) => ("conflict", true),
            (_, DvrState::Withdrawn) => ("withdrawn", true),
            (_, DvrState::Cancelled) => ("cancelled", false),
            (_, DvrState::Done) => ("finished", false),
            (
                DvrStatePatch::Finished {
                    stopped_by_user_id: Some(_),
                    ..
                },
                DvrState::Partial,
            ) => ("finished", false),
            (_, DvrState::Partial) => ("finished", true),
            (_, DvrState::Failed) => ("failed", true),
            (_, DvrState::Missed) => ("missed", true),
            (_, DvrState::Deleted) => ("deleted", false),
            _ => ("schedule_changed", false),
        };
        let facts = match &patch {
            DvrStatePatch::Finished { bytes, gap_s, .. } => {
                serde_json::json!({"bytes": bytes, "gap_s": gap_s})
            }
            DvrStatePatch::Reattempt { attempt, gap_s } => {
                serde_json::json!({"attempt": attempt, "gap_s": gap_s})
            }
            _ => serde_json::json!({}),
        };
        let event = DvrEventInput {
            event_id: uuid::Uuid::new_v4().to_string(),
            kind: kind.to_owned(),
            occurred_at_ms: now_ms,
            attempt: match &patch {
                DvrStatePatch::Started { attempt, .. }
                | DvrStatePatch::Reattempt { attempt, .. } => Some(*attempt),
                _ => None,
            },
            actor_user_id: match &patch {
                DvrStatePatch::Finished {
                    stopped_by_user_id, ..
                } => *stopped_by_user_id,
                _ => None,
            },
            reason_code: reason.map(str::to_owned),
            facts_json: facts.to_string(),
            actionable,
        };
        self.store
            .transition_dvr_recording_with_event(
                &DvrTransition {
                    id,
                    from,
                    to,
                    reason,
                    patch,
                    fence_generation,
                    now_ms,
                },
                &event,
            )
            .await
            .map_err(store_error)
    }

    fn recording_base_path(&self, dvr: &DvrConfig, row: &DvrRecording) -> PathBuf {
        PathBuf::from(&dvr.root)
            .join(recording_folder(&row.title))
            .join(recording_basename(
                &row.title,
                row.airing_start,
                &row.guide_number,
                &row.id,
            ))
    }

    fn recording_final_path(&self, dvr: &DvrConfig, row: &DvrRecording) -> PathBuf {
        final_path(&self.recording_base_path(dvr, row))
    }

    /// Close a capture out: join its attempts into one file, write the
    /// sidecar, and record honestly what it managed to get.
    #[allow(clippy::too_many_arguments)]
    async fn finish_row(
        &self,
        row: &DvrRecording,
        events: &super::webhook::DvrEventSink,
        now: i64,
        extra_gap: i64,
        stopped_by: Option<i64>,
        reason: &str,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        let facts = self.transport_source_facts(&row.channel_id);
        self.finish_row_with_facts(
            row, facts, events, now, extra_gap, stopped_by, reason, generation,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_row_with_facts(
        &self,
        row: &DvrRecording,
        facts: Option<crate::live_tv_delivery::LiveSourceFacts>,
        events: &super::webhook::DvrEventSink,
        now: i64,
        extra_gap: i64,
        stopped_by: Option<i64>,
        reason: &str,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        let (_, dvr) = self.dvr_configs().await?;
        let base = self.recording_base_path(&dvr, row);
        let gap = row.gap_s + extra_gap;
        let bytes = match concatenate_attempts(&base, row.attempt).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    recording = %row.id,
                    %error,
                    "could not assemble a recording's attempts"
                );
                0
            }
        };
        let state = if bytes == 0 {
            DvrState::Failed
        } else if gap > 0 || row.late_start_s > 0 {
            DvrState::Partial
        } else {
            DvrState::Done
        };
        let reason = if state == DvrState::Partial {
            format!("{reason}; {gap}s not captured, {}s late", row.late_start_s)
        } else {
            reason.to_owned()
        };
        if bytes > 0 {
            write_sidecar(&base, row, &facts, state, bytes, gap, now).await;
        }
        self.transition(
            &row.id,
            &[DvrState::Recording],
            state,
            Some(&reason),
            DvrStatePatch::Finished {
                finished_at_ms: now.saturating_mul(1000),
                bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
                gap_s: gap,
                path: final_path(&base).to_str().map(str::to_owned),
                stopped_by_user_id: stopped_by,
            },
            Some(generation),
        )
        .await?;
        tracing::info!(
            recording = %row.id,
            title = %row.title,
            state = state.as_str(),
            bytes,
            gap_s = gap,
            "recording finished"
        );
        // Enqueued, never sent from here: a tick must not wait on a network
        // call it cannot bound.
        events.enqueue(if state == DvrState::Failed {
            super::webhook::DvrEvent::RecordingFailed {
                recording_id: row.id.clone(),
                title: row.title.clone(),
                reason: reason.clone(),
            }
        } else {
            super::webhook::DvrEvent::RecordingFinished {
                recording_id: row.id.clone(),
                title: row.title.clone(),
                state: state.as_str().to_owned(),
                bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
                gap_s: gap,
                path: final_path(&base).to_str().map(str::to_owned),
            }
        });
        Ok(())
    }

    fn transport_source_facts(
        &self,
        channel_id: &str,
    ) -> Option<crate::live_tv_delivery::LiveSourceFacts> {
        // Clone the transport out before releasing the registry lock: the
        // source facts live behind a second lock, and holding both at once is
        // the shape that eventually deadlocks.
        let transport = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.get(channel_id).cloned()
        }?;
        let facts = transport
            .source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        facts
    }

    async fn delete_recording_files(&self, row: &DvrRecording) {
        let Some(path) = row.path.as_deref() else {
            return;
        };
        let base = PathBuf::from(path);
        // The row stores the final `.ts` path, so the sidecar is found by
        // dropping that one suffix — never by `with_extension`, which would
        // cut the name at the dot inside the channel number.
        let sidecar = base
            .to_str()
            .and_then(|path| path.strip_suffix(".ts"))
            .map(|stem| PathBuf::from(format!("{stem}.json")));
        for candidate in std::iter::once(base.clone()).chain(sidecar) {
            if let Err(error) = tokio::fs::remove_file(&candidate).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %candidate.display(), "could not remove a recording");
                }
            }
        }
    }
}

fn store_error(error: plurx_core::error::StoreError) -> LiveTvError {
    LiveTvError::DeviceUnavailable(format!("reading recording state: {error}"))
}

/// Open the tuner, keep the same prefix the live path keeps, probe it, and
/// then write every chunk to every sink whose window contains this moment.
async fn run_transport(
    client: reqwest::Client,
    manager: std::sync::Weak<LiveTvManager>,
    serving: crate::serving_fence::ServingAuthority,
    transport: Arc<DvrTransport>,
) -> Result<(), LiveTvError> {
    let guide_number = transport.channel.guide_number.clone();
    let url = pinned_url(transport.address, 5004, &format!("/auto/v{guide_number}"))?;
    let deadline = tokio::time::Instant::now() + super::STARTUP_TIMEOUT;
    let response = tokio::select! {
        biased;
        _ = transport.cancel.cancelled() => return Ok(()),
        response = open_tuner_stream(&client, url, deadline) => response?,
    };
    // A warm opening keeps no prefix: its opener's FFmpeg was planned from
    // cached facts and is fed from the first byte, while the fan-out probes
    // those same first bytes as the opening sample (plan L-02 §3.3).
    let warm = transport.is_warm_opening();
    let input = if warm {
        LiveTunerInput {
            prefix: bytes::Bytes::new(),
            queued: None,
            remainder: futures_util::StreamExt::boxed(response.bytes_stream()),
        }
    } else {
        collect_live_prefix(response, &transport.cancel).await?
    };
    // Probe the prefix here, for every consumer (plan L-03 §2.4 D1): the
    // opener plans from these facts and every sidecar this transport writes
    // describes them. Before this the transport never probed, so `source`
    // was always empty. Later viewers may ask the fan-out to re-probe the
    // live edge (`source_for_viewer`).
    let (system, source) = match manager.upgrade() {
        Some(manager) => {
            {
                // The orphan sweeper takes this gate across its snapshot of
                // owned directories and its walk; creating the directory
                // under it means the sweeper either sees this transport as
                // the owner or finishes before the directory exists.
                let _gate = manager.scratch_sweep_gate.lock().await;
                if let Err(error) = tokio::fs::create_dir_all(&transport.scratch).await {
                    tracing::warn!(%error, "could not create the transport's probe folder");
                }
            }
            let system = Arc::clone(&manager.system);
            drop(manager);
            if warm {
                return pump_tuner_fanout(input, serving, transport, Some(system)).await;
            }
            let source = probe_live_source(&system, &transport.scratch, &input.prefix).await;
            (Some(system), source)
        }
        None => (
            None,
            Err(LiveTvError::StreamFailed("live-TV manager stopped".into())),
        ),
    };
    if let Err(error) = &source {
        if !transport.is_recording() {
            // Nothing but viewers wants these bytes: the transport ends with
            // the probe's error, as D1 specifies. The viewers waiting on it
            // report that error, and the next start opens a fresh tuner GET
            // and probes again — as each start did before sharing.
            tracing::warn!(
                code = error.code(),
                channel = %guide_number,
                "the shared tuner transport could not observe its source; closing it"
            );
            return Err(error.clone());
        }
        // A recording holds the tuner and needs no plan, so the transport
        // stays up; the next viewer asks the fan-out to re-probe.
        tracing::warn!(
            code = error.code(),
            channel = %guide_number,
            "the shared tuner transport could not observe its source; the recording continues and the next viewer re-probes"
        );
    }
    transport.publish_source(1, source);
    pump_tuner_fanout(input, serving, transport, system).await
}

/// A re-probe in flight on the fan-out: the sample's epoch and its facts.
type ProbeInFlight =
    Pin<Box<dyn Future<Output = (u64, Result<LiveSourceFacts, LiveTvError>)> + Send>>;

/// The live-edge bytes collected for a re-probe, bounded exactly as the
/// opening prefix is (`SOURCE_PREFIX_BYTES` or `SOURCE_PREFIX_TIME`).
struct ReprobeSample {
    epoch: u64,
    bytes: Vec<u8>,
    started: tokio::time::Instant,
}

/// Resolves when the re-probe in flight, if any, has its answer.
async fn reprobe_outcome(
    probing: &mut Option<ProbeInFlight>,
) -> (u64, Result<LiveSourceFacts, LiveTvError>) {
    match probing {
        Some(probe) => probe.as_mut().await,
        None => std::future::pending().await,
    }
}

/// The fan-out itself: one task owns the response, so cancelling and joining
/// it releases the tuner connection rather than leaving a detached reader
/// behind. Per chunk it checks the serving fence and then hands the chunk to
/// every live consumer with a `try_send` — it never waits on one. A consumer
/// whose bounded queue is full is evicted; nothing else is (plan §3.2).
async fn pump_tuner_fanout(
    input: LiveTunerInput,
    serving: crate::serving_fence::ServingAuthority,
    transport: Arc<DvrTransport>,
    prober: Option<Arc<super::SystemInfo>>,
) -> Result<(), LiveTvError> {
    use futures_util::StreamExt as _;

    let mut stream = input.remainder;
    let mut pending = std::iter::once(input.prefix)
        .chain(input.queued)
        .filter(|held| !held.is_empty())
        .collect::<std::collections::VecDeque<_>>();
    // A viewer handed the held prefix starts that far behind the live edge.
    let held_bytes = pending
        .iter()
        .map(|held| u64::try_from(held.len()).unwrap_or(u64::MAX))
        .fold(0_u64, u64::saturating_add);
    // The prefix is held for the first consumer: the opener's viewer attaches
    // once its FFmpeg exists, and its first bytes must be the bytes its plan
    // was made from. A later joiner starts at the live edge.
    if !transport.await_first_consumer().await {
        return Ok(());
    }
    // A warm opening's sample is epoch 1, begun with the first byte its
    // opener's FFmpeg is fed; its probe is the opener's verification.
    let mut sample: Option<ReprobeSample> = transport.is_warm_opening().then(|| ReprobeSample {
        epoch: 1,
        bytes: Vec::new(),
        started: tokio::time::Instant::now(),
    });
    let mut probing: Option<ProbeInFlight> = None;
    loop {
        let (bytes, held) = match pending.pop_front() {
            Some(bytes) => (bytes, true),
            None => {
                let next = tokio::select! {
                    biased;
                    _ = transport.cancel.cancelled() => return Ok(()),
                    (epoch, result) = reprobe_outcome(&mut probing) => {
                        probing = None;
                        transport.publish_source(epoch, result);
                        continue;
                    }
                    next = tokio::time::timeout(TUNER_READ_TIMEOUT, stream.next()) => next,
                }
                .map_err(|_| {
                    LiveTvError::StreamFailed("the HDHomeRun stream stopped producing bytes".into())
                })?;
                let Some(bytes) = next else {
                    return Err(LiveTvError::StreamFailed(
                        "the HDHomeRun stream ended".into(),
                    ));
                };
                let bytes = bytes.map_err(|_| {
                    LiveTvError::StreamFailed("the HDHomeRun stream body failed".into())
                })?;
                (bytes, false)
            }
        };
        // Checked per chunk, exactly as a session's fence is. A node that
        // has lost serving authority must stop writing within one read
        // timeout: the replacement owner is about to open its own attempt,
        // and two processes writing one recording is the corruption the
        // attempt files exist to make impossible.
        if !serving.is_current(transport.owner_serving_generation) {
            for sink in transport.live_sinks() {
                transport
                    .metrics
                    .observe_consumer_eviction("recording", "fenced");
                sink.cancel.cancel();
            }
            for viewer in transport.live_viewers() {
                viewer.evict("fenced", &transport.metrics);
            }
            return Err(LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
            ));
        }
        // A viewer wants newer facts than any sample so far (§2.4 D1, D5):
        // collect the next bytes of the live edge — the bytes a joiner
        // attaching now starts near — and probe them beside this loop, which
        // keeps feeding every consumer while the probe runs.
        if sample.is_none() && probing.is_none() {
            if let Some(epoch) = transport.begin_reprobe_sample() {
                sample = Some(ReprobeSample {
                    epoch,
                    bytes: Vec::new(),
                    started: tokio::time::Instant::now(),
                });
            }
        }
        if let Some(collecting) = sample.as_mut() {
            let room = super::SOURCE_PREFIX_BYTES.saturating_sub(collecting.bytes.len());
            collecting
                .bytes
                .extend_from_slice(&bytes[..bytes.len().min(room)]);
            if collecting.bytes.len() >= super::SOURCE_PREFIX_BYTES
                || collecting.started.elapsed() >= super::SOURCE_PREFIX_TIME
            {
                if let Some(ReprobeSample {
                    epoch,
                    bytes: collected,
                    ..
                }) = sample.take()
                {
                    let directory = transport.scratch.clone();
                    let system = prober.clone();
                    probing = Some(Box::pin(async move {
                        let result = match system {
                            Some(system) => {
                                probe_live_source(&system, &directory, &collected).await
                            }
                            None => {
                                Err(LiveTvError::StreamFailed("live-TV manager stopped".into()))
                            }
                        };
                        (epoch, result)
                    }));
                }
            }
        }
        let sinks = transport.live_sinks();
        let viewers = transport.live_viewers();
        if sinks.is_empty() && viewers.is_empty() {
            // Nothing attached. Retire unless a consumer is admitted and on
            // its way; in that case the live edge waits for nobody, and the
            // chunk is dropped.
            if transport.try_retire() {
                return Ok(());
            }
            continue;
        }
        let now = unix_seconds();
        for sink in sinks {
            // A sink takes only the bytes inside its own window. The pads of
            // two adjacent programmes overlap deliberately, and in that
            // overlap both files get the same bytes — which is what makes a
            // shared transport correct rather than merely cheap.
            if now < sink.window.0 || now >= sink.window.1 {
                continue;
            }
            let queue = sink
                .writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let Some(queue) = queue else {
                continue;
            };
            if !reserve_sink_queue_bytes(&queue.queued_bytes, bytes.len()) {
                tracing::warn!(
                    recording = %sink.recording_id,
                    queued_bytes = queue.queued_bytes.load(Ordering::Acquire),
                    "a recording's disk queue filled; that capture is stopping"
                );
                transport
                    .metrics
                    .observe_consumer_eviction("recording", "backlog");
                interrupt_sink(&sink, "disk_write_backlog");
                continue;
            }
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            match queue.tx.try_send(bytes.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    queue.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                    transport
                        .metrics
                        .observe_consumer_eviction("recording", "backlog");
                    interrupt_sink(&sink, "disk_write_backlog");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    queue.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                    interrupt_sink(&sink, "disk_write_failed");
                }
            }
        }
        for viewer in viewers {
            if held {
                viewer.start_behind(held_bytes);
            }
            let queue = viewer
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let Some(queue) = queue else {
                continue;
            };
            if !reserve_queue_bytes(&queue.queued_bytes, bytes.len(), viewer.queue_limit()) {
                tracing::warn!(
                    queued_bytes = queue.queued_bytes.load(Ordering::Acquire),
                    "a live viewer's FFmpeg stopped reading; evicting that viewer only"
                );
                viewer.evict("backlog", &transport.metrics);
                continue;
            }
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            match queue.tx.try_send(bytes.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    queue.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                    viewer.evict("backlog", &transport.metrics);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    // Its pump is gone; the session is ending on its own.
                    queue.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                    viewer.close_feed();
                }
            }
        }
    }
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

async fn prior_attempt_bytes(base: &std::path::Path, attempt: i64) -> Option<u64> {
    if attempt <= 1 {
        return Some(0);
    }
    let mut total = 0_u64;
    for prior in 1..attempt {
        let metadata = tokio::fs::metadata(attempt_path(base, prior)).await.ok()?;
        total = total.checked_add(metadata.len())?;
    }
    Some(total)
}

/// Join `<base>.a1.part`, `<base>.a2.part`, … into `<base>.ts` and remove the
/// parts. MPEG-TS packets concatenate, so the join is a byte copy; the
/// discontinuity between attempts is a gap, which the row and the sidecar name
/// rather than paper over.
async fn concatenate_attempts(base: &std::path::Path, attempts: i64) -> std::io::Result<u64> {
    use tokio::io::AsyncWriteExt as _;

    let final_path = final_path(base);
    let mut parts = Vec::new();
    for attempt in 1..=attempts.max(1) {
        let part = attempt_path(base, attempt);
        if tokio::fs::metadata(&part).await.is_ok() {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return Ok(tokio::fs::metadata(&final_path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0));
    }
    // One attempt is the common case, and a rename beats copying gigabytes.
    if parts.len() == 1 {
        tokio::fs::rename(&parts[0], &final_path).await?;
        return Ok(tokio::fs::metadata(&final_path).await?.len());
    }
    let mut out = tokio::fs::File::create(&final_path).await?;
    for part in &parts {
        let mut input = tokio::fs::File::open(part).await?;
        tokio::io::copy(&mut input, &mut out).await?;
    }
    out.flush().await?;
    drop(out);
    for part in &parts {
        let _ = tokio::fs::remove_file(part).await;
    }
    Ok(tokio::fs::metadata(&final_path).await?.len())
}

/// Everything the library scan needs, beside the file.
///
/// Written at finish rather than at start, because half of it — how many
/// attempts it took, what was missed, how big it is — is only true at the end.
async fn write_sidecar(
    base: &std::path::Path,
    row: &DvrRecording,
    facts: &Option<crate::live_tv_delivery::LiveSourceFacts>,
    state: DvrState,
    bytes: u64,
    gap_s: i64,
    now: i64,
) {
    let document = serde_json::json!({
        "plurx_dvr": 1,
        "recording_id": row.id,
        "channel": {
            "id": row.channel_id,
            "guide_number": row.guide_number,
            "name": row.channel_name,
        },
        "airing": { "start": row.airing_start, "end": row.airing_end },
        "capture": {
            "start": row.capture_start,
            "end": row.capture_end,
            "actual_end": now,
        },
        "attempts": row.attempt,
        "gap_s": gap_s,
        "late_start_s": row.late_start_s,
        "programme": {
            "title": row.title,
            "episode_title": row.episode_title,
            "episode": row.episode,
            "synopsis": row.synopsis,
            "image_url": row.image_url,
            "original_air_date": row.original_air_date,
            "series_id": row.series_id,
            "programme_id": row.programme_id,
        },
        "source": facts,
        "rule": row.rule_id.as_ref().map(|id| serde_json::json!({ "id": id })),
        "state": state.as_str(),
        "bytes": bytes,
    });
    let path = sidecar_path(base);
    let body = match serde_json::to_vec_pretty(&document) {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "could not serialise a recording sidecar");
            return;
        }
    };
    if let Err(error) = tokio::fs::write(&path, body).await {
        tracing::warn!(%error, path = %path.display(), "could not write a recording sidecar");
    }
}

/// Byte counts reaching the Store, so the Activity row's number rises without
/// a write per chunk.
pub(crate) async fn dvr_progress_loop(manager: Arc<LiveTvManager>, shutdown: CancellationToken) {
    let mut last: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_millis(
                DVR_PROGRESS_INTERVAL_MS as u64,
            )) => {}
        }
        let activities = manager.recording_activities();
        let now_ms = unix_seconds().saturating_mul(1000);
        for activity in activities {
            if last.get(&activity.recording_id) == Some(&activity.bytes) {
                continue;
            }
            last.insert(activity.recording_id.clone(), activity.bytes);
            if let Err(error) = manager
                .store
                .progress_dvr_recording(
                    &activity.recording_id,
                    i64::try_from(activity.bytes).unwrap_or(i64::MAX),
                    now_ms,
                )
                .await
            {
                tracing::warn!(%error, "could not record a capture's progress");
            }
        }
        last.retain(|id, _| {
            manager
                .recording_activities()
                .iter()
                .any(|activity| &activity.recording_id == id)
        });
    }
}

/// The reminder loop.
///
/// Gated on being the owner and being admitted to serve, and on nothing else.
/// A reminder is a row with a time in it: it needs no tuner, no disk and no
/// DVR root, so it fires with recording switched off and with Live TV itself
/// switched off. Making it wait on either would be a gate on a feature that
/// does not use them — and the person who set the reminder would simply not be
/// told, with nothing anywhere saying why.
///
/// It consults the guide only to notice that an airing has moved, and only
/// when a guide exists at all.
pub(crate) async fn reminder_loop(
    manager: Arc<LiveTvManager>,
    events: super::webhook::DvrEventSink,
    shutdown: CancellationToken,
) {
    loop {
        let ours_and_serving = match manager.dvr_configs().await {
            Ok((live_tv, _)) => {
                live_tv.owner_node_id == manager.node_id && manager.serving.admit().is_some()
            }
            Err(_) => false,
        };
        if ours_and_serving {
            if let Err(error) = reminder_tick(&manager, &events).await {
                tracing::warn!(
                    code = error.code(),
                    "the reminder tick did not complete; it will be retried"
                );
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(DVR_TICK) => {}
        }
    }
}

async fn reminder_tick(
    manager: &Arc<LiveTvManager>,
    events: &super::webhook::DvrEventSink,
) -> Result<(), LiveTvError> {
    reminder_tick_at(manager, events, unix_seconds()).await
}

/// The reminder pass with an explicit clock. Production supplies wall time;
/// the oversized-guide regression supplies the guide's deterministic epoch so
/// it can prove the late programme survives reconciliation rather than merely
/// testing whatever happens to fall within fourteen days of the test run.
async fn reminder_tick_at(
    manager: &Arc<LiveTvManager>,
    events: &super::webhook::DvrEventSink,
    now: i64,
) -> Result<(), LiveTvError> {
    let fired = manager
        .store
        .transition_dvr_reminders(now, now.saturating_mul(1000))
        .await
        .map_err(store_error)?;
    for reminder in &fired {
        tracing::info!(
            reminder = %reminder.id,
            title = %reminder.title,
            "a reminder fired"
        );
        events.enqueue(super::webhook::DvrEvent::Reminder {
            reminder_id: reminder.id.clone(),
            user_id: reminder.user_id,
            channel_id: reminder.channel_id.clone(),
            guide_number: reminder.guide_number.clone(),
            title: reminder.title.clone(),
            airing_start: reminder.airing_start,
            lead_s: reminder.lead_s,
        });
    }

    // A programme that moved is worth saying so about: the reminder names a
    // time that no longer has that programme in it, and firing it would send
    // someone to the wrong thing.
    let (live_tv, _) = manager.dvr_configs().await?;
    if !live_tv.guide_fetches() {
        return Ok(());
    }
    let armed = manager
        .store
        .list_dvr_reminders_in(plurx_core::dvr::DvrReminderState::Armed)
        .await
        .map_err(store_error)?;
    if armed.is_empty() {
        return Ok(());
    }
    let Some(guide) = manager.local_guide_view(&live_tv).await else {
        return Ok(());
    };
    if guide.guide.channels.is_empty() {
        // No guide is not evidence that a programme moved.
        return Ok(());
    }
    // Spent reminders are history, and history is not a quota. Without this
    // sweep a long-lived account eventually cannot set another one.
    for reminder in manager
        .store
        .list_dvr_reminders_in(plurx_core::dvr::DvrReminderState::Acked)
        .await
        .map_err(store_error)?
        .into_iter()
        .chain(
            manager
                .store
                .list_dvr_reminders_in(plurx_core::dvr::DvrReminderState::Expired)
                .await
                .map_err(store_error)?,
        )
        .chain(
            manager
                .store
                .list_dvr_reminders_in(plurx_core::dvr::DvrReminderState::Moved)
                .await
                .map_err(store_error)?,
        )
    {
        if now - reminder.airing_end > REMINDER_RETENTION_S {
            // Best-effort retention cleanup: a failure leaves an inert moved
            // reminder for the next maintenance pass to delete.
            crate::store_result::observe(
                crate::store_result::Operation::DeleteExpiredDvrReminder,
                crate::store_result::Discard::BestEffort,
                manager
                    .store
                    .delete_dvr_reminder(reminder.user_id, &reminder.id)
                    .await,
            );
        }
    }

    for reminder in armed {
        let window = GuideWindow {
            start: now,
            end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
        };
        let listed = guide.guide.channels.iter().any(|channel| {
            channel.id == reminder.channel_id
                && channel.programmes_in(&window).iter().any(|programme| {
                    programme.start == reminder.airing_start && programme.title == reminder.title
                })
        });
        if listed {
            continue;
        }
        manager
            .store
            .set_dvr_reminder_state(
                None,
                &reminder.id,
                plurx_core::dvr::DvrReminderState::Armed,
                plurx_core::dvr::DvrReminderState::Moved,
                now.saturating_mul(1000),
            )
            .await
            .map_err(store_error)?;
    }
    Ok(())
}

/// Keep the recordings library in step with what the engine wrote.
///
/// Two jobs, both deliberately done by polling rather than by a signal from
/// the capture path:
///
/// 1. Create the `recordings` library the first time a DVR root is set, so an
///    operator who turns recording on does not also have to know they must
///    add a library pointed at the same folder.
/// 2. Ask for a targeted scan of every finished capture that is not yet in
///    the library, so a recording becomes playable within a minute of ending.
///
/// Polling, because the alternative is a signal that can be missed: a capture
/// that finishes while the scanner is busy, or while this node is restarting,
/// still has to reach the shelf. A row with no `item_id` is a standing request
/// that survives both.
pub(crate) async fn dvr_library_loop(state: crate::state::AppState, shutdown: CancellationToken) {
    const SWEEP: std::time::Duration = std::time::Duration::from_secs(60);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(SWEEP) => {}
        }
        if let Err(error) = sweep_recordings_library(&state).await {
            tracing::warn!(%error, "could not reconcile the recordings library");
        }
    }
}

async fn sweep_recordings_library(
    state: &crate::state::AppState,
) -> Result<(), plurx_core::error::StoreError> {
    let settings = state.store.settings_snapshot().await?;
    let live_tv = LiveTvConfig::from_snapshot(&settings, &state.node_id);
    let dvr = DvrConfig::from_snapshot(&settings);
    // The owner is the node that wrote the files; every other node would be
    // racing it to index the same paths.
    if !dvr.enabled || dvr.root.is_empty() || live_tv.owner_node_id != state.node_id {
        return Ok(());
    }
    let root = std::path::PathBuf::from(&dvr.root);
    let library = match ensure_recordings_library(state, &root).await? {
        Some(library) => library,
        None => return Ok(()),
    };
    // Recent finishes only. A recording whose sidecar could not be written —
    // a full disk, a permission change — will never link, and asking the
    // scanner about it every sixty seconds for the life of the server is a
    // load with no possible outcome.
    let now = crate::live_tv::unix_seconds();
    let unlinked = state
        .store
        .list_dvr_recordings_in(&[DvrState::Done, DvrState::Partial])
        .await?
        .into_iter()
        .filter(|row| row.item_id.is_none())
        .filter(|row| now - row.finished_at_ms.unwrap_or(0) / 1000 < UNLINKED_SCAN_WINDOW_S)
        .filter_map(|row| row.path.clone())
        .collect::<Vec<_>>();
    for path in unlinked {
        let path = std::path::PathBuf::from(path);
        if tokio::fs::metadata(&path).await.is_err() {
            continue;
        }
        let request = crate::state::ScanRequest {
            id: uuid::Uuid::new_v4().to_string(),
            library_id: library.id,
            path,
            ids: None,
            book: None,
            correlation_id: None,
            source: Some("dvr".to_owned()),
        };
        // Queued rather than dropped when a scan is already running, which is
        // what makes a burst of finishes all reach the shelf.
        if let Err(error) = state.jobs.request_scan(request).await {
            tracing::warn!(%error, "could not ask for a scan of a finished recording");
            break;
        }
    }
    Ok(())
}

/// Find the recordings library for this root, creating it once if there is
/// none. Never creates a second: an operator who deleted it deliberately gets
/// it back, and an operator who renamed it keeps their name.
async fn ensure_recordings_library(
    state: &crate::state::AppState,
    root: &std::path::Path,
) -> Result<Option<plurx_core::domain::Library>, plurx_core::error::StoreError> {
    use plurx_core::domain::{LibraryKind, NewLibrary};

    let libraries = state.store.list_libraries().await?;
    if let Some(existing) = libraries.iter().find(|library| {
        library.kind == LibraryKind::Recordings && library.paths.iter().any(|path| path == root)
    }) {
        return Ok(Some(existing.clone()));
    }
    // `name` is unique, so a library called "Recordings" pointed somewhere
    // else must not make this fail for ever.
    let taken = libraries
        .iter()
        .any(|library| library.name.eq_ignore_ascii_case("Recordings"));
    let name = if taken {
        format!("Recordings ({})", root.display())
    } else {
        "Recordings".to_owned()
    };
    let created = state
        .store
        .create_library(&NewLibrary {
            name,
            kind: LibraryKind::Recordings,
            paths: vec![root.to_path_buf()],
            anime: false,
        })
        .await?;
    tracing::info!(
        library = created.id,
        root = %root.display(),
        "created the recordings library for the configured DVR root"
    );
    Ok(Some(created))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    async fn oversized_scheduler_fixture() -> (
        Arc<LiveTvManager>,
        LiveTvConfig,
        DvrConfig,
        i64,
        i64,
        tempfile::TempDir,
    ) {
        use plurx_core::store::{SqliteStore, UserStore as _};
        use plurx_core::transcode::{EncoderCaps, Pipeline};

        let root = tempfile::tempdir().expect("temporary scheduler root");
        let root_path = root.path();
        let sqlite = Arc::new(SqliteStore::open_in_memory().expect("scheduler store"));
        let user = sqlite
            .create_user("scheduler", "test-only", true)
            .await
            .expect("scheduler user");
        let store: Arc<dyn plurx_core::store::Store> = sqlite;
        let transcode = Arc::new(crate::transcode::TranscodeManager::new(
            Arc::clone(&store),
            root_path.join("finite"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let manager = LiveTvManager::new(
            Arc::clone(&store),
            Arc::new(super::super::SystemInfo::default()),
            transcode,
            crate::serving_fence::ServingAuthority::always_ready(),
            "node-a".to_owned(),
            root_path.join("live-tv"),
            root_path.join("guide"),
        );
        let now = 1_800_000_000;
        let target_start = now + 13 * 86_400;
        let target_title = "late programme the public clip drops";
        let guide = super::super::LiveTvGuide {
            source: "hdhomerun".to_owned(),
            freshness: super::super::GuideFreshness::Fresh,
            age_seconds: 0,
            fetched_at: Some(now),
            window: GuideWindow {
                start: now,
                end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
            },
            refresh_error: None,
            next_refresh_at: None,
            matched_channels: 64,
            lineup_channels: 64,
            channels: (0..64)
                .map(|channel| super::super::LiveTvGuideChannel {
                    id: format!("{channel}.1"),
                    guide_number: format!("{channel}.1"),
                    affiliate: None,
                    image_url: None,
                    programmes: (0..700)
                        .map(|slot| {
                            let start = now + i64::from(slot) * 1_800;
                            super::super::LiveTvProgramme {
                                start,
                                end: start + 1_800,
                                title: if channel == 63 && start == target_start {
                                    target_title.to_owned()
                                } else {
                                    format!(
                                        "ordinary programme {channel:02}-{slot:03} {}",
                                        "x".repeat(180)
                                    )
                                },
                                episode_title: None,
                                episode: None,
                                synopsis: None,
                                image_url: None,
                                original_air_date: None,
                                series_id: None,
                                programme_id: None,
                                is_new: None,
                                filters: Vec::new(),
                            }
                        })
                        .collect(),
                })
                .collect(),
        };
        manager.guide_cache.store(7, 1, &Ok(guide)).await;

        let mut live_tv = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
        live_tv.enabled = true;
        live_tv.owner_node_id = "node-a".to_owned();
        live_tv.guide_source = super::super::GuideSource::HdHomeRun;
        live_tv.guide_hours = 336;
        live_tv.generation = 7;
        let dvr = DvrConfig {
            enabled: true,
            root: root_path.join("dvr").to_string_lossy().into_owned(),
            free_floor_gb: 0,
            tuner_reserve: 0,
            pad_start_s: 0,
            pad_end_s: 0,
            reminder_lead_s: 0,
            webhook_url: String::new(),
        };
        store
            .put_dvr_rule(&plurx_core::dvr::DvrRule {
                id: "late-rule".to_owned(),
                owner_user_id: user.id,
                priority: 0,
                name: "late rule".to_owned(),
                match_mode: plurx_core::dvr::DvrMatchMode::Title,
                match_value: target_title.to_owned(),
                channel_id: Some("63.1".to_owned()),
                new_only: false,
                keep_mode: DvrKeepMode::All,
                keep_value: 0,
                pad_start_s: 0,
                pad_end_s: 0,
                enabled: true,
                created_at_ms: now * 1_000,
                updated_at_ms: now * 1_000,
            })
            .await
            .expect("store late rule");
        (manager, live_tv, dvr, now, target_start, root)
    }

    #[tokio::test]
    async fn the_scheduler_sees_an_airing_the_public_clip_drops() {
        let (manager, live_tv, dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
        manager
            .dvr_expand(&live_tv, &dvr, now)
            .await
            .expect("expand late rule");
        let row = manager
            .store
            .get_dvr_recording_for_airing("63.1", target_start)
            .await
            .expect("read scheduled row")
            .expect("late programme scheduled from full view");
        assert_eq!(row.title, "late programme the public clip drops");

        let public = manager
            .local_guide(
                &live_tv,
                GuideWindow {
                    start: now,
                    end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
                },
            )
            .await;
        assert!(
            public.channels.iter().all(|channel| channel
                .programmes
                .iter()
                .all(|programme| programme.title != row.title)),
            "the fixture must prove the scheduler used data omitted by the 2 MiB response"
        );
    }

    #[tokio::test]
    async fn already_scheduled_late_rows_survive_reconciliation_of_an_oversized_guide() {
        let (manager, live_tv, dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
        manager
            .dvr_expand(&live_tv, &dvr, now)
            .await
            .expect("expand late rule");
        manager
            .dvr_reconcile(&live_tv, live_tv.generation, now)
            .await
            .expect("reconcile late row");
        let row = manager
            .store
            .get_dvr_recording_for_airing("63.1", target_start)
            .await
            .expect("read reconciled row")
            .expect("late row retained");
        assert_eq!(row.state, DvrState::Scheduled);
    }

    #[tokio::test]
    async fn a_late_reminder_is_not_moved_when_only_the_full_guide_contains_it() {
        let (manager, live_tv, _dvr, now, target_start, _root) =
            oversized_scheduler_fixture().await;
        use plurx_core::store::keys;
        manager
            .store
            .put_settings(&[
                (keys::LIVE_TV_ENABLED, "1"),
                (keys::LIVE_TV_OWNER_NODE_ID, "node-a"),
                (keys::LIVE_TV_GUIDE_SOURCE, "hdhomerun"),
                (keys::LIVE_TV_GUIDE_HOURS, "336"),
                (keys::LIVE_TV_CONFIG_GENERATION, "7"),
            ])
            .await
            .expect("configure the reminder guide consumer");
        let owner = manager
            .store
            .list_dvr_rules()
            .await
            .expect("read fixture rule")
            .into_iter()
            .next()
            .expect("fixture rule owner")
            .owner_user_id;
        let reminder = plurx_core::dvr::DvrReminder {
            id: "late-reminder".to_owned(),
            user_id: owner,
            channel_id: "63.1".to_owned(),
            guide_number: "63.1".to_owned(),
            airing_start: target_start,
            airing_end: target_start + 1_800,
            title: "late programme the public clip drops".to_owned(),
            lead_s: 0,
            state: plurx_core::dvr::DvrReminderState::Armed,
            fired_at_ms: None,
            acked_at_ms: None,
            created_at_ms: now.saturating_mul(1_000),
            updated_at_ms: now.saturating_mul(1_000),
        };
        assert!(manager
            .store
            .put_dvr_reminder(&reminder)
            .await
            .expect("store late reminder"));

        let (events, _queue) = super::super::webhook::channel();
        reminder_tick_at(&manager, &events, now)
            .await
            .expect("reconcile reminder against full guide");

        let rows = manager
            .store
            .list_dvr_reminders(owner, None)
            .await
            .expect("read reconciled reminder");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, plurx_core::dvr::DvrReminderState::Armed);
        assert_eq!(
            live_tv.generation, 7,
            "fixture and persisted cache generation agree"
        );
    }

    #[derive(Clone, Default)]
    struct MemoryWriter {
        bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl tokio::io::AsyncWrite for MemoryWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.bytes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(bytes);
            std::task::Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl SinkWriter for MemoryWriter {
        fn sync_all<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FailingWriter;

    impl tokio::io::AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::other("injected disk failure")))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl SinkWriter for FailingWriter {
        fn sync_all<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn test_sink(
        id: &str,
        writer: Box<dyn SinkWriter>,
        delivered: Arc<AtomicU64>,
        metrics: Arc<LiveTvMetrics>,
    ) -> Arc<DvrSink> {
        test_sink_at(
            id,
            PathBuf::from(format!("/unused/{id}")),
            writer,
            delivered,
            metrics,
        )
    }

    fn test_sink_at(
        id: &str,
        base: PathBuf,
        writer: Box<dyn SinkWriter>,
        delivered: Arc<AtomicU64>,
        metrics: Arc<LiveTvMetrics>,
    ) -> Arc<DvrSink> {
        test_sink_attempt_at(id, 1, base, writer, delivered, metrics)
    }

    fn test_sink_attempt_at(
        id: &str,
        attempt: i64,
        base: PathBuf,
        writer: Box<dyn SinkWriter>,
        delivered: Arc<AtomicU64>,
        metrics: Arc<LiveTvMetrics>,
    ) -> Arc<DvrSink> {
        let now = unix_seconds();
        let sink = Arc::new(DvrSink {
            recording_id: id.to_owned(),
            channel_id: "7.1".to_owned(),
            airing_start: now,
            title: id.to_owned(),
            attempt,
            window: (now - 60, now + 60),
            base,
            writer: std::sync::Mutex::new(None),
            writer_task: std::sync::Mutex::new(None),
            bytes: AtomicU64::new(0),
            prior_attempt_bytes: None,
            observation: std::sync::Mutex::new(DvrSinkObservation::new(attempt)),
            metrics,
            cancel: CancellationToken::new(),
        });
        start_sink_writer(&sink, writer, delivered);
        sink
    }

    fn test_transport(
        sinks: Vec<Arc<DvrSink>>,
        _delivered: Arc<AtomicU64>,
        serving_generation: u64,
    ) -> Arc<DvrTransport> {
        test_transport_with(
            sinks,
            serving_generation,
            Arc::new(LiveTvMetrics::default()),
            0,
        )
    }

    fn test_channel() -> LiveTvChannel {
        LiveTvChannel {
            id: "7.1".to_owned(),
            guide_number: "7.1".to_owned(),
            guide_name: "Test".to_owned(),
            favorite: false,
            drm: false,
            support: super::super::LiveTvChannelSupport::Ready,
            hd: Some(true),
            video_codec: None,
            audio_codec: None,
            source_format: None,
        }
    }

    fn test_transport_with(
        sinks: Vec<Arc<DvrSink>>,
        serving_generation: u64,
        metrics: Arc<LiveTvMetrics>,
        seats: usize,
    ) -> Arc<DvrTransport> {
        let transport = DvrTransport::new(TransportInit {
            channel: test_channel(),
            generation: 1,
            owner_serving_generation: serving_generation,
            device_id: "fixture-device".to_owned(),
            address: std::net::Ipv4Addr::new(10, 42, 1, 20),
            origin: TransportOrigin::Recording,
            scratch: PathBuf::from("/unused/transport"),
            metrics,
            seats,
        });
        transport
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(sinks);
        transport
    }

    fn tuner_input(
        chunks: Vec<bytes::Bytes>,
        pulled_at: Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
    ) -> LiveTunerInput {
        use futures_util::StreamExt as _;

        let prefix = chunks[0].clone();
        let queued = chunks.get(1).cloned();
        let remainder = chunks.into_iter().skip(2).collect::<Vec<_>>();
        let stream = futures_util::stream::unfold(
            (remainder, 0usize, pulled_at),
            |(chunks, index, pulled_at)| async move {
                if index >= chunks.len() {
                    return None;
                }
                tokio::task::yield_now().await;
                pulled_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(std::time::Instant::now());
                Some((
                    Ok::<_, reqwest::Error>(chunks[index].clone()),
                    (chunks, index + 1, pulled_at),
                ))
            },
        )
        .boxed();
        LiveTunerInput {
            prefix,
            queued,
            remainder: stream,
        }
    }

    #[tokio::test]
    async fn a_slow_sink_never_delays_the_tuner_reader_or_its_sibling() {
        let metrics = Arc::new(LiveTvMetrics::default());
        let delivered = Arc::new(AtomicU64::new(0));
        let fast_writer = MemoryWriter::default();
        let fast_bytes = Arc::clone(&fast_writer.bytes);
        let fast = test_sink(
            "fast",
            Box::new(fast_writer),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let (stalled_writer, stalled_reader) = tokio::io::duplex(1);
        let slow = test_sink(
            "slow",
            Box::new(stalled_writer),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let slow_queue_bytes = slow
            .writer
            .lock()
            .expect("slow writer queue")
            .as_ref()
            .expect("slow writer")
            .queued_bytes
            .clone();
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport(
            vec![Arc::clone(&fast), Arc::clone(&slow)],
            delivered,
            serving.admit().expect("serving generation"),
        );
        let chunk_count = 160usize;
        let chunk_bytes = 64 * 1024usize;
        let chunks = (0..chunk_count)
            .map(|index| bytes::Bytes::from(vec![(index % 251) as u8; chunk_bytes]))
            .collect::<Vec<_>>();
        let pulled_at = Arc::new(std::sync::Mutex::new(Vec::new()));

        let started = std::time::Instant::now();
        let result = pump_tuner_fanout(
            tuner_input(chunks, Arc::clone(&pulled_at)),
            serving,
            transport,
            None,
        )
        .await;
        assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(slow.cancel.is_cancelled());
        assert!(!fast.cancel.is_cancelled());
        assert!(slow_queue_bytes.load(Ordering::Acquire) <= DVR_SINK_QUEUE_BYTES);
        assert_eq!(
            slow.observation
                .lock()
                .expect("slow observation")
                .reason_code
                .as_deref(),
            Some("disk_write_backlog")
        );
        let max_gap = {
            let pulls = pulled_at.lock().expect("pull timings");
            pulls
                .windows(2)
                .map(|pair| pair[1].duration_since(pair[0]))
                .max()
                .unwrap_or_default()
        };
        assert!(max_gap < std::time::Duration::from_millis(100));

        assert_eq!(settle_sink(&fast).await, SinkStopResult::Settled);
        assert_eq!(
            fast_bytes.lock().expect("fast bytes").len(),
            chunk_count * chunk_bytes
        );
        drop(stalled_reader);
        assert_eq!(settle_sink(&slow).await, SinkStopResult::Settled);
        assert!(metrics
            .dvr_sink_failures_prometheus()
            .contains("reason=\"disk_write_backlog\"} 1"));
    }

    #[tokio::test]
    async fn a_failing_sink_records_one_named_interruption() {
        let metrics = Arc::new(LiveTvMetrics::default());
        let delivered = Arc::new(AtomicU64::new(0));
        let sink = test_sink(
            "failing",
            Box::new(FailingWriter),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport(
            vec![Arc::clone(&sink)],
            delivered,
            serving.admit().expect("serving generation"),
        );
        let pulled_at = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = pump_tuner_fanout(
            tuner_input(
                vec![
                    bytes::Bytes::from_static(b"first"),
                    bytes::Bytes::from_static(b"second"),
                    bytes::Bytes::from_static(b"third"),
                ],
                pulled_at,
            ),
            serving,
            transport,
            None,
        )
        .await;
        assert!(
            result.is_ok(),
            "a sink-local disk failure must not become a tuner failure"
        );
        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
        let observation = sink.observation.lock().expect("failure observation");
        assert_eq!(
            observation.reason_code.as_deref(),
            Some("disk_write_failed")
        );
        assert_eq!(
            observation
                .pending_events
                .iter()
                .filter(|event| event.kind == "capture_interrupted")
                .count(),
            1
        );
        drop(observation);
        assert!(metrics
            .dvr_sink_failures_prometheus()
            .contains("reason=\"disk_write_failed\"} 1"));
    }

    #[tokio::test]
    async fn recovery_preserves_one_durable_sink_failure_without_worker_lost() {
        let (manager, live_tv, dvr, now, target_start, _root) = oversized_scheduler_fixture().await;
        manager
            .dvr_expand(&live_tv, &dvr, now)
            .await
            .expect("materialize recovery row");
        let row = manager
            .store
            .get_dvr_recording_for_airing("63.1", target_start)
            .await
            .expect("read recovery row")
            .expect("recovery row");
        assert!(manager
            .transition(
                &row.id,
                &[DvrState::Scheduled],
                DvrState::Recording,
                Some("test capture started"),
                DvrStatePatch::Started {
                    attempt: 1,
                    tuner_owner_node_id: manager.node_id.clone(),
                    started_at_ms: now.saturating_mul(1_000),
                    late_start_s: 0,
                    path: "/unused/recovery.ts".to_owned(),
                },
                None,
            )
            .await
            .expect("claim recovery row"));

        let metrics = Arc::new(LiveTvMetrics::default());
        let delivered = Arc::new(AtomicU64::new(0));
        let failing = test_sink_attempt_at(
            &row.id,
            1,
            PathBuf::from(format!("/unused/{}", row.id)),
            Box::new(FailingWriter),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let sibling = test_sink(
            "sibling-keeps-transport",
            Box::new(MemoryWriter::default()),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport(
            vec![Arc::clone(&failing), Arc::clone(&sibling)],
            delivered,
            serving.admit().expect("serving generation"),
        );
        manager
            .registry
            .lock()
            .expect("registry")
            .transports
            .insert("7.1".to_owned(), Arc::clone(&transport));
        let _ = pump_tuner_fanout(
            tuner_input(
                vec![
                    bytes::Bytes::from_static(b"first"),
                    bytes::Bytes::from_static(b"second"),
                    bytes::Bytes::from_static(b"third"),
                ],
                Arc::new(std::sync::Mutex::new(Vec::new())),
            ),
            serving,
            transport,
            None,
        )
        .await;
        assert!(failing.cancel.is_cancelled());

        manager
            .drain_dvr_observation_events()
            .await
            .expect("persist precise sink failure");
        let (events, _queue) = super::super::webhook::channel();
        manager
            .dvr_recover(&live_tv, &dvr, &events, live_tv.generation, now)
            .await
            .expect("recover failed sink");

        let history = manager
            .store
            .list_dvr_events(&row.id, None, None, 100)
            .await
            .expect("read recovery history");
        let interruptions = history
            .rows
            .iter()
            .filter(|event| event.kind == "capture_interrupted")
            .collect::<Vec<_>>();
        assert_eq!(interruptions.len(), 1);
        assert_eq!(
            interruptions[0].reason_code.as_deref(),
            Some("disk_write_failed")
        );
        assert!(metrics
            .dvr_sink_failures_prometheus()
            .contains("reason=\"disk_write_failed\"} 1"));
        assert_eq!(settle_sink(&failing).await, SinkStopResult::Settled);
        assert_eq!(settle_sink(&sibling).await, SinkStopResult::Settled);
    }

    #[tokio::test]
    async fn two_overlapping_recordings_get_identical_bytes_in_their_overlap() {
        let metrics = Arc::new(LiveTvMetrics::default());
        let delivered = Arc::new(AtomicU64::new(0));
        let left_writer = MemoryWriter::default();
        let left_bytes = Arc::clone(&left_writer.bytes);
        let right_writer = MemoryWriter::default();
        let right_bytes = Arc::clone(&right_writer.bytes);
        let left = test_sink(
            "left",
            Box::new(left_writer),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let right = test_sink(
            "right",
            Box::new(right_writer),
            Arc::clone(&delivered),
            metrics,
        );
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport(
            vec![Arc::clone(&left), Arc::clone(&right)],
            delivered,
            serving.admit().expect("serving generation"),
        );
        let chunks = vec![
            bytes::Bytes::from_static(b"overlap-a"),
            bytes::Bytes::from_static(b"overlap-b"),
            bytes::Bytes::from_static(b"overlap-c"),
        ];
        let expected = chunks.concat();
        let result = pump_tuner_fanout(
            tuner_input(chunks, Arc::new(std::sync::Mutex::new(Vec::new()))),
            serving,
            transport,
            None,
        )
        .await;
        assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
        assert_eq!(settle_sink(&left).await, SinkStopResult::Settled);
        assert_eq!(settle_sink(&right).await, SinkStopResult::Settled);
        assert_eq!(*left_bytes.lock().expect("left bytes"), expected);
        assert_eq!(*right_bytes.lock().expect("right bytes"), expected);
    }

    // ---- shared transport (plan L-03 M1) ------------------------------------

    fn transport_test_manager(root: &std::path::Path) -> Arc<LiveTvManager> {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::{EncoderCaps, Pipeline};

        let store: Arc<dyn plurx_core::store::Store> =
            Arc::new(SqliteStore::open_in_memory().expect("transport store"));
        let transcode = Arc::new(crate::transcode::TranscodeManager::new(
            Arc::clone(&store),
            root.join("finite"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        LiveTvManager::new(
            store,
            Arc::new(super::super::SystemInfo::default()),
            transcode,
            crate::serving_fence::ServingAuthority::always_ready(),
            "node-a".to_owned(),
            root.join("live-tv"),
            root.join("guide"),
        )
    }

    /// A viewer attached the way a session attaches: a seat reserved, the
    /// consumer pushed, the seat given back.
    fn attach_test_viewer(
        transport: &Arc<DvrTransport>,
        name: &str,
    ) -> (Arc<ViewerConsumer>, ViewerFeed, CancellationToken) {
        let cancel = CancellationToken::new();
        let (viewer, feed) = ViewerConsumer::new(name.to_owned(), cancel.clone());
        assert!(transport.reserve_seat());
        assert!(transport.attach_viewer(Arc::clone(&viewer)));
        transport.release_seat();
        (viewer, feed, cancel)
    }

    fn seat_for(channel_id: &str) -> super::super::TransportSeat<'_> {
        super::super::TransportSeat {
            channel_id,
            device_id: "fixture-device",
            address: std::net::Ipv4Addr::new(10, 42, 1, 20),
            serving_generation: 0,
        }
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    /// A tuner that never stops sending, and says when its response is gone.
    fn endless_tuner(dropped: Arc<AtomicBool>) -> LiveTunerInput {
        use futures_util::StreamExt as _;

        let packet = bytes::Bytes::from_static(&[0x47; 188]);
        let chunk = packet.clone();
        let stream = futures_util::stream::unfold(DropFlag(dropped), move |guard| {
            let chunk = chunk.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                Some((Ok::<_, reqwest::Error>(chunk), guard))
            }
        })
        .boxed();
        LiveTunerInput {
            prefix: packet,
            queued: None,
            remainder: stream,
        }
    }

    fn register(manager: &LiveTvManager, key: &str, transport: &Arc<DvrTransport>) {
        manager
            .registry
            .lock()
            .expect("registry")
            .transports
            .insert(key.to_owned(), Arc::clone(transport));
    }

    #[tokio::test]
    async fn a_viewer_joins_a_recording_transport_without_a_slot() {
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let metrics = Arc::new(LiveTvMetrics::default());
        let sink = test_sink(
            "recording",
            Box::new(MemoryWriter::default()),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&metrics),
        );
        let recording = test_transport_with(vec![Arc::clone(&sink)], 0, metrics, 0);
        register(&manager, "7.1", &recording);
        let now = tokio::time::Instant::now();
        let decide = |channel: &str| {
            manager.registry.lock().expect("registry").viewer_admission(
                &seat_for(channel),
                7,
                now,
                1,
            )
        };

        match decide("7.1") {
            super::super::ViewerAdmission::Join(joined) => {
                assert!(Arc::ptr_eq(&joined, &recording));
            }
            _ => panic!("a viewer on the recorded channel takes no tuner of its own"),
        }
        assert!(
            matches!(
                decide("4.1"),
                super::super::ViewerAdmission::Refuse(LiveTvError::Capacity(_))
            ),
            "another channel needs the one tuner the recording holds"
        );
        let holders = manager.transport_holders();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].channel_id, "7.1");
        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
    }

    #[tokio::test]
    async fn a_second_start_joins_a_transport_whose_opener_has_not_attached_yet() {
        // §2.4 D2: the opener reserves its transport in the admission's own
        // lock hold, so a second start on the channel in the window before
        // the opener's FFmpeg exists joins it. It never opens a second GET
        // and never replaces the first in the registry.
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let opening = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
        register(&manager, "7.1", &opening);
        let decision = manager.registry.lock().expect("registry").viewer_admission(
            &seat_for("7.1"),
            8,
            tokio::time::Instant::now(),
            2,
        );
        match decision {
            super::super::ViewerAdmission::Join(joined) => assert!(Arc::ptr_eq(&joined, &opening)),
            _ => panic!("a reserved transport is joined, not replaced"),
        }
    }

    #[tokio::test]
    async fn a_join_crosses_a_settings_generation_but_not_a_device_or_serving_change() {
        // The D7 decision: a viewer at configuration generation 4 joins a
        // transport opened at 3, because the bytes do not depend on it. The
        // device (id and address) and the serving generation are what the
        // bytes do depend on, and neither may differ. Stale source facts do
        // not refuse a join: the joiner re-probes before it plans.
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let transport = DvrTransport::new(TransportInit {
            channel: test_channel(),
            generation: 3,
            owner_serving_generation: 0,
            device_id: "fixture-device".to_owned(),
            address: std::net::Ipv4Addr::new(10, 42, 1, 20),
            origin: TransportOrigin::Viewer,
            scratch: PathBuf::from("/unused/transport"),
            metrics: Arc::new(LiveTvMetrics::default()),
            seats: 0,
        });
        let (_viewer, _feed, _cancel) = attach_test_viewer(&transport, "watching");
        register(&manager, "7.1", &transport);
        let now = tokio::time::Instant::now();
        let decide = |seat: super::super::TransportSeat<'_>| {
            manager
                .registry
                .lock()
                .expect("registry")
                .viewer_admission(&seat, 7, now, 2)
        };
        assert!(matches!(
            decide(seat_for("7.1")),
            super::super::ViewerAdmission::Join(_)
        ));
        let mut other_address = seat_for("7.1");
        other_address.address = std::net::Ipv4Addr::new(10, 42, 1, 21);
        let mut other_device = seat_for("7.1");
        other_device.device_id = "another-device";
        let mut other_authority = seat_for("7.1");
        other_authority.serving_generation = 1;
        for seat in [other_address, other_device, other_authority] {
            assert!(
                matches!(
                    decide(seat),
                    super::super::ViewerAdmission::Refuse(LiveTvError::Capacity(_))
                ),
                "a different tuner is never joined, and never opened beside a live one"
            );
        }
        transport.mark_source_stale();
        assert!(
            matches!(
                decide(seat_for("7.1")),
                super::super::ViewerAdmission::Join(_)
            ),
            "one viewer's format change does not lock the channel for the rest of the recording"
        );
    }

    #[tokio::test]
    async fn a_stalled_viewer_is_evicted_and_its_sibling_continues() {
        let metrics = Arc::new(LiveTvMetrics::default());
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport_with(
            Vec::new(),
            serving.admit().expect("serving generation"),
            Arc::clone(&metrics),
            0,
        );
        let (reading, mut feed, _reading_cancel) = attach_test_viewer(&transport, "reading");
        let (stalled, _stalled_feed, _stalled_cancel) = attach_test_viewer(&transport, "stalled");
        let drained = tokio::spawn(async move {
            let mut total = 0usize;
            while let Some(bytes) = feed.rx.recv().await {
                feed.queued_bytes
                    .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
                total += bytes.len();
            }
            total
        });
        let chunk_count = 160usize;
        let chunk_bytes = 64 * 1024usize;
        let chunks = (0..chunk_count)
            .map(|index| bytes::Bytes::from(vec![(index % 251) as u8; chunk_bytes]))
            .collect::<Vec<_>>();
        let pulled_at = Arc::new(std::sync::Mutex::new(Vec::new()));

        let started = std::time::Instant::now();
        let result = pump_tuner_fanout(
            tuner_input(chunks, Arc::clone(&pulled_at)),
            serving,
            Arc::clone(&transport),
            None,
        )
        .await;
        assert!(
            matches!(result, Err(LiveTvError::StreamFailed(_))),
            "the transport ends with its tuner stream, not with an eviction"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(stalled.evicted(), Some("backlog"));
        assert_eq!(reading.evicted(), None);
        assert!(!transport.cancel.is_cancelled());
        let max_gap = {
            let pulls = pulled_at.lock().expect("pull timings");
            pulls
                .windows(2)
                .map(|pair| pair[1].duration_since(pair[0]))
                .max()
                .unwrap_or_default()
        };
        assert!(max_gap < std::time::Duration::from_millis(100));
        transport.finish(&result);
        assert_eq!(
            drained.await.expect("drain task"),
            chunk_count * chunk_bytes,
            "the sibling received every byte the stalled viewer missed"
        );
        let series = super::super::LiveTvMetrics::transports_prometheus(
            &super::super::LiveTvRegistry::default(),
            &metrics.consumer_evictions,
        );
        assert!(
            series.contains(
                "plurx_live_tv_consumer_evictions_total{kind=\"viewer\",reason=\"backlog\"} 1"
            ),
            "{series}"
        );
    }

    #[tokio::test]
    async fn the_last_detach_closes_the_transport_and_releases_the_tuner() {
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport_with(
            Vec::new(),
            serving.admit().expect("serving generation"),
            Arc::new(LiveTvMetrics::default()),
            0,
        );
        let (_first, _first_feed, first) = attach_test_viewer(&transport, "first");
        let (_second, _second_feed, second) = attach_test_viewer(&transport, "second");
        register(&manager, "7.1", &transport);
        let dropped = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(pump_tuner_fanout(
            endless_tuner(Arc::clone(&dropped)),
            serving,
            Arc::clone(&transport),
            None,
        ));
        assert!(
            transport.open_until() >= unix_seconds(),
            "a live viewer wants bytes now"
        );

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        first.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !dropped.load(Ordering::Acquire),
            "one viewer left; the other still watches"
        );
        assert!(!worker.is_finished());

        second.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), worker)
            .await
            .expect("the last detach ends the reader promptly")
            .expect("reader task");
        assert!(result.is_ok());
        assert!(
            dropped.load(Ordering::Acquire),
            "the reader owned the tuner response, so its end is the tuner's release"
        );
        assert!(
            !transport.reserve_seat(),
            "a retired transport admits nobody, however late they arrive"
        );
        manager.close_transport_arc(Arc::clone(&transport)).await;
        assert!(transport.is_closed());
        assert_eq!(manager.registry.lock().expect("registry").held(), 0);
    }

    #[tokio::test]
    async fn a_recording_keeps_the_transport_open_past_the_last_viewer() {
        let metrics = Arc::new(LiveTvMetrics::default());
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let writer = MemoryWriter::default();
        let written = Arc::clone(&writer.bytes);
        let sink = test_sink(
            "recording",
            Box::new(writer),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&metrics),
        );
        let transport = test_transport_with(
            vec![Arc::clone(&sink)],
            serving.admit().expect("serving generation"),
            metrics,
            0,
        );
        let (_viewer, _feed, viewer) = attach_test_viewer(&transport, "viewer");
        let dropped = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(pump_tuner_fanout(
            endless_tuner(Arc::clone(&dropped)),
            serving,
            Arc::clone(&transport),
            None,
        ));
        viewer.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !worker.is_finished(),
            "the recording's window is still open"
        );
        assert!(!dropped.load(Ordering::Acquire));
        assert_eq!(transport.open_until(), sink.window.1);

        sink.cancel.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), worker)
            .await
            .expect("the window's end closes the transport")
            .expect("reader task");
        assert!(result.is_ok());
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
        assert!(!written.lock().expect("written").is_empty());
    }

    #[tokio::test]
    async fn a_drain_closes_a_shared_transport_and_ends_every_viewer() {
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = DvrTransport::new(TransportInit {
            channel: test_channel(),
            generation: 3,
            owner_serving_generation: serving.admit().expect("serving generation"),
            device_id: "fixture-device".to_owned(),
            address: std::net::Ipv4Addr::new(10, 42, 1, 20),
            origin: TransportOrigin::Viewer,
            scratch: root.path().join("transport-probe"),
            metrics: Arc::new(LiveTvMetrics::default()),
            seats: 0,
        });
        let (_a, mut a_feed, _a_cancel) = attach_test_viewer(&transport, "a");
        let (_b, mut b_feed, _b_cancel) = attach_test_viewer(&transport, "b");
        register(&manager, "7.1", &transport);
        let dropped = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(pump_tuner_fanout(
            endless_tuner(Arc::clone(&dropped)),
            serving,
            Arc::clone(&transport),
            None,
        ));
        *transport.worker.lock().expect("worker slot") = Some(worker);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        manager.drain_before(4).await.expect("drain");
        assert!(transport.is_closed());
        assert_eq!(manager.registry.lock().expect("registry").held(), 0);
        assert!(
            dropped.load(Ordering::Acquire),
            "the drain joined the tuner's reader"
        );
        for feed in [&mut a_feed, &mut b_feed] {
            let ended = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while feed.rx.recv().await.is_some() {}
            })
            .await;
            assert!(ended.is_ok(), "every viewer's feed ends with the transport");
        }
    }

    #[tokio::test]
    async fn a_recording_joining_a_viewer_transport_respects_the_reserve() {
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let metrics = Arc::new(LiveTvMetrics::default());
        let viewers = test_transport_with(Vec::new(), 0, Arc::clone(&metrics), 0);
        let (_viewer, _feed, _cancel) = attach_test_viewer(&viewers, "viewer");
        register(&manager, "2.1", &viewers);
        {
            let registry = manager.registry.lock().expect("registry");
            assert!(
                registry.may_add_recording(2, 1),
                "no recording yet: one may join the viewers' transport"
            );
        }
        let sink = test_sink(
            "recording",
            Box::new(MemoryWriter::default()),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&metrics),
        );
        let recording = test_transport_with(vec![Arc::clone(&sink)], 0, metrics, 0);
        register(&manager, "4.1", &recording);
        {
            let registry = manager.registry.lock().expect("registry");
            assert!(
                !registry.may_add_recording(2, 1),
                "a second recording transport on two tuners with one reserved is refused, \
                 even though joining needs no tuner"
            );
            assert!(!registry.may_open_transport(2, 1));
        }
        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
    }

    #[tokio::test]
    async fn the_recording_tick_leaves_a_transport_only_viewers_hold() {
        // §2.4 D3: both closers keyed on sinks, so the DVR tick closed every
        // transport without a recording — including one people are watching.
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let transport = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 0);
        let (_viewer, _feed, viewer) = attach_test_viewer(&transport, "viewer");
        transport.publish_source(1, Ok(LiveSourceFacts::default()));
        register(&manager, "7.1", &transport);

        manager.close_finished_transports().await;
        manager.close_recordings().await;
        assert!(!transport.cancel.is_cancelled());
        assert_eq!(manager.registry.lock().expect("registry").held(), 1);
        assert!(
            manager.transport_holders().is_empty(),
            "nothing a client could stop"
        );
        let watchable = manager.watchable_channels();
        assert_eq!(
            watchable.len(),
            1,
            "but a channel a refused viewer could join"
        );
        assert_eq!(watchable[0].channel_id, "7.1");
        let metrics = manager.metrics.prometheus();
        assert!(
            metrics.contains("plurx_live_tv_transports 1\n"),
            "{metrics}"
        );
        assert!(
            metrics.contains("plurx_live_tv_transport_consumers{kind=\"viewer\"} 1\n"),
            "{metrics}"
        );

        viewer.cancel();
        manager.close_finished_transports().await;
        assert!(transport.is_closed());
        assert_eq!(manager.registry.lock().expect("registry").held(), 0);
    }

    #[tokio::test]
    async fn a_viewer_waiting_on_a_transport_gets_its_probe_or_its_error() {
        // §2.4 D1: the transport's probe is what every viewer plans from and
        // what every sidecar records. A tuner the device refused reaches the
        // viewer as the device's own error.
        let refused = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
        let waiting = {
            let refused = Arc::clone(&refused);
            tokio::spawn(async move {
                refused
                    .source_for_viewer(
                        &CancellationToken::new(),
                        tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        refused.finish(&Err(LiveTvError::TunerUnavailable("no tuner".into())));
        assert!(matches!(
            waiting.await.expect("waiter"),
            Err(LiveTvError::TunerUnavailable(_))
        ));

        let probed = test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
        let facts = LiveSourceFacts {
            video_codec: Some("mpeg2video".into()),
            width: Some(1920),
            height: Some(1080),
            ..LiveSourceFacts::default()
        };
        probed.publish_source(1, Ok(facts.clone()));
        let seen = probed
            .source_for_viewer(
                &CancellationToken::new(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .await
            .expect("probed facts");
        assert_eq!(seen, facts);
        assert_eq!(
            probed.source.lock().expect("source").clone(),
            Some(facts),
            "the facts a recording's sidecar reads are the same ones"
        );
    }

    /// A recording writer that keeps nothing, for tests that do not read the
    /// recorded bytes.
    struct DiscardWriter;

    impl tokio::io::AsyncWrite for DiscardWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl SinkWriter for DiscardWriter {
        fn sync_all<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A tuner sending 16 KiB a millisecond, so a re-probe's sample reaches
    /// `SOURCE_PREFIX_BYTES` in well under `SOURCE_PREFIX_TIME`.
    fn busy_tuner(dropped: Arc<AtomicBool>) -> LiveTunerInput {
        use futures_util::StreamExt as _;

        let chunk = bytes::Bytes::from(vec![0x47_u8; 16 * 1024]);
        let prefix = chunk.clone();
        let stream = futures_util::stream::unfold(DropFlag(dropped), move |guard| {
            let chunk = chunk.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                Some((Ok::<_, reqwest::Error>(chunk), guard))
            }
        })
        .boxed();
        LiveTunerInput {
            prefix,
            queued: None,
            remainder: stream,
        }
    }

    /// An FFprobe that prints `<root>/probe.json`, and fails while that file
    /// is absent, so a test decides what each probe sees.
    #[cfg(unix)]
    fn scripted_ffprobe(root: &std::path::Path) -> Arc<super::super::SystemInfo> {
        use std::os::unix::fs::PermissionsExt;

        let script = root.join("fake-ffprobe");
        let answer = root.join("probe.json");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ -f '{0}' ] || exit 1\nexec /bin/cat '{0}'\n",
                answer.display()
            ),
        )
        .expect("fake FFprobe");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("executable fake FFprobe");
        Arc::new(super::super::SystemInfo {
            ffprobe: script.to_string_lossy().into_owned(),
            ..super::super::SystemInfo::default()
        })
    }

    fn probe_answer(height: u16) -> String {
        let width = if height >= 720 { 1920 } else { 720 };
        format!(
            r#"{{"streams":[{{"codec_type":"video","codec_name":"mpeg2video","width":{width},"height":{height},"field_order":"tt"}},{{"codec_type":"audio","codec_name":"ac3","channels":2}}]}}"#
        )
    }

    async fn viewer_facts(transport: &DvrTransport) -> Result<LiveSourceFacts, LiveTvError> {
        transport
            .source_for_viewer(
                &CancellationToken::new(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            )
            .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_or_stale_probe_is_reprobed_for_the_next_viewer_of_a_recording() {
        // Review of #482, finding 1, built from the reviewer's reproduction: a
        // recording's transport whose opening probe failed, with a tuner
        // free. Every later start joined it and failed at once on the stored
        // failure, and the channel was offered as "watch instead". Now each
        // viewer that finds failed, stale or old facts has the running
        // fan-out re-probe the live edge; a failed re-probe fails that viewer
        // alone; the channel is offered only while its format is known; and a
        // format change (D5) is re-probed instead of refusing every viewer
        // until the recording ends. The recording is never cut.
        let root = tempfile::tempdir().expect("root");
        let manager = transport_test_manager(root.path());
        let metrics = Arc::new(LiveTvMetrics::default());
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let scratch = root.path().join("transport");
        std::fs::create_dir_all(&scratch).expect("transport scratch");
        let transport = DvrTransport::new(TransportInit {
            channel: test_channel(),
            generation: 1,
            owner_serving_generation: serving.admit().expect("serving generation"),
            device_id: "fixture-device".to_owned(),
            address: std::net::Ipv4Addr::new(10, 42, 1, 20),
            origin: TransportOrigin::Recording,
            scratch,
            metrics: Arc::clone(&metrics),
            seats: 0,
        });
        let sink = test_sink(
            "recording",
            Box::new(DiscardWriter),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&metrics),
        );
        transport
            .sinks
            .lock()
            .expect("sinks")
            .push(Arc::clone(&sink));
        register(&manager, "7.1", &transport);
        let dropped = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(pump_tuner_fanout(
            busy_tuner(Arc::clone(&dropped)),
            serving,
            Arc::clone(&transport),
            Some(scripted_ffprobe(root.path())),
        ));
        transport.publish_source(
            1,
            Err(LiveTvError::StartupTimeout(
                "source_probe_incomplete: FFprobe exceeded its two-second budget".into(),
            )),
        );
        assert!(
            manager.watchable_channels().is_empty(),
            "a channel whose format is unknown is not offered as watch-instead"
        );
        let join = || {
            let decision = manager.registry.lock().expect("registry").viewer_admission(
                &seat_for("7.1"),
                7,
                tokio::time::Instant::now(),
                2,
            );
            match decision {
                super::super::ViewerAdmission::Join(joined) => {
                    assert!(Arc::ptr_eq(&joined, &transport));
                }
                _ => panic!("a viewer joins the recording's transport"),
            }
        };

        // The first viewer's re-probe fails as well: it gets that probe's own
        // error, not the stored timeout.
        join();
        let failed = viewer_facts(&transport).await;
        assert!(
            matches!(failed, Err(LiveTvError::InvalidResponse(_))),
            "{failed:?}"
        );

        // The next viewer asks again, and this time the probe succeeds.
        std::fs::write(root.path().join("probe.json"), probe_answer(1080)).expect("probe answer");
        join();
        let facts = viewer_facts(&transport)
            .await
            .expect("a re-probe of the live edge");
        assert_eq!(facts.height, Some(1080));
        assert_eq!(
            manager.watchable_channels().len(),
            1,
            "a known format is offered"
        );

        // Fresh facts are shared without probing again.
        std::fs::remove_file(root.path().join("probe.json")).expect("probe answer removed");
        join();
        let shared = std::time::Instant::now();
        assert_eq!(
            viewer_facts(&transport).await.expect("fresh facts").height,
            Some(1080)
        );
        assert!(shared.elapsed() < std::time::Duration::from_millis(100));

        // D5: one viewer saw the format change. The channel is neither
        // offered nor locked: the next viewer re-probes and plans from what
        // the broadcast now is.
        transport.mark_source_stale();
        assert!(manager.watchable_channels().is_empty());
        std::fs::write(root.path().join("probe.json"), probe_answer(480)).expect("probe answer");
        join();
        assert_eq!(
            viewer_facts(&transport)
                .await
                .expect("a re-probe after the change")
                .height,
            Some(480)
        );
        assert_eq!(manager.watchable_channels().len(), 1);

        assert!(!transport.cancel.is_cancelled());
        assert!(!sink.cancel.is_cancelled(), "the recording was never cut");
        assert!(!dropped.load(Ordering::Acquire));
        transport.cancel.cancel();
        assert!(worker.await.expect("fan-out").is_ok());
        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
    }

    #[tokio::test]
    async fn the_opener_of_a_full_rate_channel_takes_its_whole_prefix() {
        // Found while building the test for review finding 5. The held
        // prefix is about three seconds of the multiplex (7 MB at 19.4
        // Mbit/s) and went to viewers against a 4 MiB bound, so every opener
        // of a full-rate channel was evicted on its first chunk, however fast
        // its FFmpeg read. A reading opener takes the prefix and the live
        // bytes behind it, and a viewer that stops reading is still evicted.
        let metrics = Arc::new(LiveTvMetrics::default());
        let serving = crate::serving_fence::ServingAuthority::always_ready();
        let transport = test_transport_with(
            Vec::new(),
            serving.admit().expect("serving generation"),
            Arc::clone(&metrics),
            0,
        );
        let (opener, mut feed, _opener_cancel) = attach_test_viewer(&transport, "opener");
        let (stalled, _stalled_feed, _stalled_cancel) = attach_test_viewer(&transport, "stalled");
        let prefix_bytes = 7 * 1024 * 1024;
        let live_chunks = 96;
        let chunk_bytes = 64 * 1024;
        let chunks = std::iter::once(bytes::Bytes::from(vec![0x47_u8; prefix_bytes]))
            .chain((0..live_chunks).map(|_| bytes::Bytes::from(vec![0x47_u8; chunk_bytes])))
            .collect::<Vec<_>>();
        let reader = tokio::spawn(async move {
            let mut total = 0usize;
            while let Some(bytes) = feed.rx.recv().await {
                feed.queued_bytes
                    .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
                total += bytes.len();
            }
            total
        });
        let result = pump_tuner_fanout(
            tuner_input(chunks, Arc::new(std::sync::Mutex::new(Vec::new()))),
            serving,
            Arc::clone(&transport),
            None,
        )
        .await;
        assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
        assert_eq!(opener.evicted(), None, "the opener is fed its prefix");
        assert_eq!(
            stalled.evicted(),
            Some("backlog"),
            "the bound still holds past the prefix"
        );
        transport.finish(&result);
        assert_eq!(
            reader.await.expect("reader"),
            prefix_bytes + live_chunks * chunk_bytes
        );
    }

    #[test]
    fn retirement_is_atomic_against_a_consumer_attaching() {
        // Review of #482, finding 3. A consumer attaches by pushing itself
        // under its list's lock and then giving its seat back. The retirement
        // read the lists and swapped the seat count separately, so an attach
        // and release landing between the two retired a transport that had
        // just gained a viewer — and the fan-out then returned, cutting it.
        // Race the two, many times over, and count retirements with a live
        // viewer attached.
        let rounds = 20_000;
        let mut retired_under_a_viewer = 0;
        for round in 0..rounds {
            let transport =
                test_transport_with(Vec::new(), 0, Arc::new(LiveTvMetrics::default()), 1);
            let start = Arc::new(std::sync::Barrier::new(2));
            let attached = Arc::new(AtomicBool::new(false));
            let attacher = {
                let transport = Arc::clone(&transport);
                let start = Arc::clone(&start);
                let attached = Arc::clone(&attached);
                std::thread::spawn(move || {
                    let (viewer, feed) =
                        ViewerConsumer::new(format!("viewer-{round}"), CancellationToken::new());
                    start.wait();
                    assert!(transport.attach_viewer(viewer));
                    transport.release_seat();
                    attached.store(true, Ordering::Release);
                    feed
                })
            };
            start.wait();
            let mut retired = false;
            loop {
                let done = attached.load(Ordering::Acquire);
                if transport.try_retire() {
                    retired = true;
                    break;
                }
                if done {
                    break;
                }
            }
            let _feed = attacher.join().expect("attacher");
            if retired && !transport.live_viewers().is_empty() {
                retired_under_a_viewer += 1;
            }
        }
        assert_eq!(
            retired_under_a_viewer, 0,
            "a transport was retired with a viewer attached in {retired_under_a_viewer} of {rounds} rounds"
        );
    }

    #[tokio::test]
    async fn a_fenced_transport_cancels_every_sink_before_exiting() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let metrics = Arc::new(LiveTvMetrics::default());
        let delivered = Arc::new(AtomicU64::new(0));
        let left_writer = MemoryWriter::default();
        let left_bytes = Arc::clone(&left_writer.bytes);
        let right_writer = MemoryWriter::default();
        let right_bytes = Arc::clone(&right_writer.bytes);
        let left = test_sink(
            "fenced-left",
            Box::new(left_writer),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let right = test_sink(
            "fenced-right",
            Box::new(right_writer),
            Arc::clone(&delivered),
            metrics,
        );
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let serving = fence.authority();
        let generation = serving.admit().expect("serving generation");
        let transport = test_transport(
            vec![Arc::clone(&left), Arc::clone(&right)],
            delivered,
            generation,
        );
        fence.validation_set_ready(false).await;

        let result = pump_tuner_fanout(
            tuner_input(
                vec![
                    bytes::Bytes::from_static(b"must-not-land"),
                    bytes::Bytes::from_static(b"nor-this"),
                ],
                Arc::new(std::sync::Mutex::new(Vec::new())),
            ),
            serving,
            transport,
            None,
        )
        .await;
        assert!(matches!(result, Err(LiveTvError::OwnerUnavailable(_))));
        assert!(left.cancel.is_cancelled());
        assert!(right.cancel.is_cancelled());
        assert_eq!(settle_sink(&left).await, SinkStopResult::Settled);
        assert_eq!(settle_sink(&right).await, SinkStopResult::Settled);
        assert!(left_bytes.lock().expect("left bytes").is_empty());
        assert!(right_bytes.lock().expect("right bytes").is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stop_waits_for_the_writer_and_never_concatenates_an_unsettled_attempt() {
        let root = tempfile::tempdir().expect("temporary recording root");
        let base = root.path().join("stalled-recording");
        tokio::fs::write(attempt_path(&base, 1), b"settling")
            .await
            .expect("attempt fixture");
        let (writer, reader) = tokio::io::duplex(1);
        let sink = test_sink_at(
            "settling",
            base.clone(),
            Box::new(writer),
            Arc::new(AtomicU64::new(0)),
            Arc::new(LiveTvMetrics::default()),
        );
        let queue = sink
            .writer
            .lock()
            .expect("writer queue")
            .as_ref()
            .expect("active writer")
            .clone();
        assert!(reserve_sink_queue_bytes(&queue.queued_bytes, 64 * 1024));
        queue
            .tx
            .try_send(bytes::Bytes::from(vec![0_u8; 64 * 1024]))
            .expect("queue stalled write");
        tokio::task::yield_now().await;

        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settling);
        assert_eq!(
            sink.observation.lock().expect("settling observation").phase,
            "settling"
        );
        assert!(attempt_path(&base, 1).exists());
        assert!(!final_path(&base).exists());

        drop(reader);
        assert_eq!(settle_sink(&sink).await, SinkStopResult::Settled);
    }

    #[tokio::test(start_paused = true)]
    async fn finalization_fences_a_later_retry_and_joins_every_attempt_before_assembly() {
        let (manager, _live_tv, _dvr, _now, _target_start, root) =
            oversized_scheduler_fixture().await;
        let base = root.path().join("retried-recording");
        tokio::fs::write(attempt_path(&base, 1), b"attempt-one")
            .await
            .expect("first attempt fixture");
        tokio::fs::write(attempt_path(&base, 2), b"attempt-two")
            .await
            .expect("second attempt fixture");
        let delivered = Arc::new(AtomicU64::new(0));
        let metrics = Arc::new(LiveTvMetrics::default());
        let first = test_sink_attempt_at(
            "retried",
            1,
            base.clone(),
            Box::new(MemoryWriter::default()),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        assert_eq!(settle_sink(&first).await, SinkStopResult::Settled);
        let sibling = test_sink(
            "sibling",
            Box::new(MemoryWriter::default()),
            Arc::clone(&delivered),
            Arc::clone(&metrics),
        );
        let (blocked_writer, blocked_reader) = tokio::io::duplex(1);
        let second = test_sink_attempt_at(
            "retried",
            2,
            base.clone(),
            Box::new(blocked_writer),
            Arc::clone(&delivered),
            metrics,
        );
        let queue = second
            .writer
            .lock()
            .expect("second writer queue")
            .as_ref()
            .expect("active second writer")
            .clone();
        assert!(reserve_sink_queue_bytes(&queue.queued_bytes, 64 * 1024));
        queue
            .tx
            .try_send(bytes::Bytes::from(vec![0_u8; 64 * 1024]))
            .expect("queue blocked retry write");
        tokio::task::yield_now().await;
        let transport = test_transport(
            vec![
                Arc::clone(&first),
                Arc::clone(&sibling),
                Arc::clone(&second),
            ],
            delivered,
            crate::serving_fence::ServingAuthority::always_ready()
                .admit()
                .expect("serving generation"),
        );
        manager
            .registry
            .lock()
            .expect("registry")
            .transports
            .insert("7.1".to_owned(), transport);

        assert!(!manager.begin_finishing("retried", 1));
        assert_eq!(
            manager.stop_sink("retried", 1).await,
            SinkStopResult::Settling,
            "a stale row may neither cancel nor assemble the later attempt"
        );
        assert!(!second.cancel.is_cancelled());
        assert!(!final_path(&base).exists());

        assert!(manager.begin_finishing("retried", 2));
        assert_eq!(
            manager.stop_sink("retried", 2).await,
            SinkStopResult::Settling,
            "the current attempt stays unassemblable while its writer lives"
        );
        assert!(attempt_path(&base, 1).exists());
        assert!(attempt_path(&base, 2).exists());
        assert!(!final_path(&base).exists());

        drop(blocked_reader);
        assert_eq!(
            manager.stop_sink("retried", 2).await,
            SinkStopResult::Settled
        );
        assert_eq!(
            concatenate_attempts(&base, 2)
                .await
                .expect("assemble settled attempts"),
            b"attempt-oneattempt-two".len() as u64
        );
        assert_eq!(
            tokio::fs::read(final_path(&base))
                .await
                .expect("final recording"),
            b"attempt-oneattempt-two"
        );
        assert!(!attempt_path(&base, 1).exists());
        assert!(!attempt_path(&base, 2).exists());
        assert_eq!(settle_sink(&sibling).await, SinkStopResult::Settled);
    }

    #[test]
    fn queue_byte_reservations_never_exceed_the_bound() {
        let queued = AtomicU64::new(0);
        for _ in 0..DVR_SINK_QUEUE_CHUNKS * 2 {
            let _ = reserve_sink_queue_bytes(&queued, 64 * 1024);
            assert!(queued.load(Ordering::Acquire) <= DVR_SINK_QUEUE_BYTES);
        }
        assert_eq!(queued.load(Ordering::Acquire), DVR_SINK_QUEUE_BYTES);
        assert!(!reserve_sink_queue_bytes(&queued, 1));
    }

    #[test]
    fn write_rate_uses_successful_samples_from_one_five_second_window() {
        let mut sample = DvrSinkObservation::new(1);
        let start = std::time::Instant::now();
        sample.successful_write(start, 1_000, 100);
        assert_eq!(sample.write_bps(), None, "one write is not a rate");
        sample.successful_write(start + std::time::Duration::from_secs(1), 2_000, 1_100);
        assert_eq!(sample.write_bps(), Some(1_000));
        sample.successful_write(start + std::time::Duration::from_secs(7), 8_000, 2_100);
        assert_eq!(
            sample.write_bps(),
            None,
            "a new attempt window never reuses a sample older than five seconds"
        );
        assert_eq!(sample.first_write_at_ms, Some(1_000));
    }

    #[test]
    fn finishing_is_latched_before_late_writes_can_relabel_the_sink() {
        let mut sample = DvrSinkObservation::new(2);
        assert_eq!(sample.phase, "reconnecting");
        sample.begin_finishing(2_000);
        sample.successful_write(std::time::Instant::now(), 2_001, 512);
        assert_eq!(sample.phase, "finishing");
        assert_eq!(
            sample.pending_events.front().map(|event| event.kind),
            Some("finishing")
        );

        sample.settling();
        sample.successful_write(std::time::Instant::now(), 2_002, 1_024);
        assert_eq!(sample.phase, "settling");
    }

    fn base(root: &str, title: &str, start: i64, number: &str, id: &str) -> PathBuf {
        PathBuf::from(root)
            .join(recording_folder(title))
            .join(recording_basename(title, start, number, id))
    }

    /// The bug this test exists for: `Path::with_extension` replaces
    /// everything after the *last* dot, and a recording's basename ends
    /// `… - 7.1 - abcdef01`. Building paths that way truncated at the dot
    /// inside the channel number, throwing away the sub-channel and the id —
    /// which are exactly what makes the name unique. Two sub-channels showing
    /// a programme called `News` at six o'clock collided on one file.
    #[test]
    fn a_channel_number_is_not_mistaken_for_a_file_extension() {
        let seven_one = base("/20t/dvr", "News", 1_789_000_800, "7.1", "aaaaaaaa1111");
        let seven_two = base("/20t/dvr", "News", 1_789_000_800, "7.2", "bbbbbbbb2222");

        assert!(
            final_path(&seven_one)
                .to_string_lossy()
                .ends_with("7.1 - aaaaaaaa.ts"),
            "the sub-channel and the id survive: {}",
            final_path(&seven_one).display()
        );
        assert_ne!(
            final_path(&seven_one),
            final_path(&seven_two),
            "two sub-channels, one programme title, one instant"
        );
        assert_ne!(attempt_path(&seven_one, 1), attempt_path(&seven_two, 1));
        assert_ne!(sidecar_path(&seven_one), sidecar_path(&seven_two));
        assert_eq!(
            attempt_path(&seven_one, 2),
            PathBuf::from(format!("{}.a2.part", seven_one.display())),
            "an attempt is a suffix on the whole basename, never an extension swap"
        );
    }

    /// Every path a recording owns is the same basename plus one suffix, so
    /// the sidecar is always found beside the file the row names.
    #[test]
    fn a_recordings_paths_are_all_siblings_of_one_basename() {
        let base = base(
            "/20t/dvr",
            "Kitchen Table",
            1_789_000_800,
            "7.1",
            "abcdef012345",
        );
        let stem = base.to_string_lossy().into_owned();
        assert_eq!(final_path(&base).to_string_lossy(), format!("{stem}.ts"));
        assert_eq!(
            sidecar_path(&base).to_string_lossy(),
            format!("{stem}.json")
        );
        assert_eq!(
            attempt_path(&base, 1).to_string_lossy(),
            format!("{stem}.a1.part")
        );
    }

    /// A single attempt is renamed rather than copied, and several are joined
    /// in order. MPEG-TS packets concatenate, so the join is a byte copy; the
    /// discontinuity is a gap the row and the sidecar name rather than hide.
    #[tokio::test]
    async fn attempts_are_joined_in_order_and_their_parts_removed() {
        let root = tempfile::tempdir().expect("temp root");
        let base = root
            .path()
            .join("Kitchen Table - 2026-09-10 0040 - 7.1 - abcdef01");

        tokio::fs::write(attempt_path(&base, 1), b"first")
            .await
            .expect("a1");
        let bytes = concatenate_attempts(&base, 1).await.expect("join one");
        assert_eq!(bytes, 5);
        assert_eq!(
            tokio::fs::read(final_path(&base)).await.expect("final"),
            b"first",
            "one attempt is a rename, not a copy"
        );
        assert!(!attempt_path(&base, 1).exists());

        // A second recovery: the final file from the first join is stale and
        // the parts are what count.
        tokio::fs::remove_file(final_path(&base))
            .await
            .expect("reset");
        tokio::fs::write(attempt_path(&base, 1), b"first")
            .await
            .expect("a1");
        tokio::fs::write(attempt_path(&base, 2), b"-second")
            .await
            .expect("a2");
        let bytes = concatenate_attempts(&base, 2).await.expect("join two");
        assert_eq!(bytes, 12);
        assert_eq!(
            tokio::fs::read(final_path(&base)).await.expect("final"),
            b"first-second",
            "attempts join in order, so the recording plays in order"
        );
        assert!(!attempt_path(&base, 1).exists());
        assert!(!attempt_path(&base, 2).exists());
    }

    /// A capture that never opened a file must not be reported as a file.
    #[tokio::test]
    async fn a_capture_that_wrote_nothing_joins_to_nothing() {
        let root = tempfile::tempdir().expect("temp root");
        let base = root
            .path()
            .join("Nothing - 2026-09-10 0040 - 7.1 - abcdef01");
        assert_eq!(
            concatenate_attempts(&base, 3).await.expect("join none"),
            0,
            "zero bytes is what makes `finish` call it failed rather than done"
        );
        assert!(!final_path(&base).exists());
    }

    /// The sidecar is what the library scan reads, so it has to carry the
    /// facts the guide supplied — and say plainly when a capture is partial.
    #[tokio::test]
    async fn a_sidecar_says_what_the_capture_actually_got() {
        let root = tempfile::tempdir().expect("temp root");
        let base = root
            .path()
            .join("Kitchen Table - 2026-09-10 0040 - 7.1 - abcdef01");
        let mut row = crate::http::dvr::recording_from_programme(
            &crate::http::dvr::ResolvedChannel {
                id: "7.1".into(),
                guide_number: "7.1".into(),
                name: "WABC".into(),
            },
            &super::super::LiveTvProgramme {
                start: 1_789_000_800,
                end: 1_789_002_600,
                title: "Kitchen Table".into(),
                episode_title: Some("The Chase".into()),
                episode: Some("S3E14".into()),
                synopsis: Some("A synopsis.".into()),
                image_url: None,
                original_air_date: Some("2026-09-10".into()),
                series_id: Some("EP01234567".into()),
                programme_id: Some("EP012345670023".into()),
                is_new: Some(true),
                filters: Vec::new(),
            },
            plurx_core::dvr::DvrOrigin::Rule,
            Some("rule-a".into()),
            Some(1),
            60,
            120,
            0,
        );
        row.attempt = 2;

        write_sidecar(
            &base,
            &row,
            &None,
            DvrState::Partial,
            2_048,
            41,
            1_789_002_700,
        )
        .await;
        let parsed = plurx_core::scan::recordings::read_sidecar(&final_path(&base))
            .expect("the scan must be able to read what the engine wrote");
        let programme = parsed.programme.expect("programme");
        assert_eq!(programme.title.as_deref(), Some("Kitchen Table"));
        assert_eq!(programme.episode.as_deref(), Some("S3E14"));
        assert_eq!(parsed.state.as_deref(), Some("partial"));
        assert_eq!(parsed.gap_s, Some(41));
        assert_eq!(parsed.recording_id.as_deref(), Some(row.id.as_str()));
    }

    /// The webhook queue drops its *oldest* entry. A recording that just
    /// started is the event someone is waiting on; an hour-old reminder
    /// nobody could deliver is not.
    #[test]
    fn a_full_webhook_queue_drops_the_oldest_event() {
        let (sink, queue) = super::super::webhook::channel();
        let event = |id: usize| super::super::webhook::DvrEvent::RecordingFailed {
            recording_id: format!("rec-{id}"),
            title: "Kitchen Table".into(),
            reason: "test".into(),
        };
        for index in 0..plurx_core::dvr::DVR_WEBHOOK_QUEUE + 3 {
            sink.enqueue(event(index));
        }
        let mut seen = Vec::new();
        while let Some(super::super::webhook::DvrEvent::RecordingFailed { recording_id, .. }) =
            queue.take()
        {
            seen.push(recording_id);
        }
        assert_eq!(seen.len(), plurx_core::dvr::DVR_WEBHOOK_QUEUE);
        assert_eq!(
            seen.first().map(String::as_str),
            Some("rec-3"),
            "the three oldest went, not the three newest"
        );
    }
}
