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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use plurx_core::dvr::{
    recording_basename, recording_folder, DvrEventInput, DvrInsertOutcome, DvrKeepMode, DvrOrigin,
    DvrRecording, DvrState, DvrStatePatch, DvrTransition, DVR_ATTEMPTS_MAX, DVR_EVENT_PRUNE_BATCH,
    DVR_EVENT_RETENTION_MS, DVR_MIN_USEFUL_S, DVR_SCHEDULE_DAYS_MAX,
};
use tokio_util::sync::CancellationToken;

use super::schedule;
use super::{
    collect_live_prefix, open_tuner_stream, pinned_url, unix_seconds, DvrConfig, GuideWindow,
    LiveTunerInput, LiveTvChannel, LiveTvConfig, LiveTvError, LiveTvManager, LiveTvMetrics,
    TUNER_READ_TIMEOUT,
};

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

/// One tuner GET, feeding every sink on its channel.
pub(crate) struct DvrTransport {
    pub(crate) channel: LiveTvChannel,
    /// The tuner configuration generation this transport opened under.
    /// `drain_before` compares it exactly as it does for a session.
    pub(crate) generation: i64,
    /// The serving-fence generation. Checked on every chunk, so a transport
    /// whose node has lost authority closes its files within one read timeout
    /// rather than writing into a file another owner may already be writing.
    pub(crate) owner_serving_generation: u64,
    pub(crate) cancel: CancellationToken,
    pub(crate) delivered: Arc<AtomicU64>,
    pub(crate) sinks: std::sync::Mutex<Vec<Arc<DvrSink>>>,
    pub(crate) worker: std::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), LiveTvError>>>>,
    /// What the prefix probe saw, for every sidecar this transport writes.
    pub(crate) source: std::sync::Mutex<Option<crate::live_tv_delivery::LiveSourceFacts>>,
}

impl DvrTransport {
    pub(crate) fn live_sinks(&self) -> Vec<Arc<DvrSink>> {
        self.sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|sink| !sink.cancel.is_cancelled())
            .cloned()
            .collect()
    }

    /// The furthest moment any sink still wants bytes. The transport closes
    /// when this passes, and a new sink may join while it has not.
    pub(crate) fn open_until(&self) -> i64 {
        self.live_sinks()
            .iter()
            .map(|sink| sink.window.1)
            .max()
            .unwrap_or(i64::MIN)
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
    let Ok(bytes) = u64::try_from(bytes) else {
        return false;
    };
    queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(bytes)
                .filter(|next| *next <= DVR_SINK_QUEUE_BYTES)
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
        let recording_transports = transports.len();
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
                } else if !ours || !live_tv.enabled || !dvr.enabled {
                    self.dvr_storage_free_bytes
                        .store(u64::MAX, Ordering::Relaxed);
                    // Not this node's work any more, or switched off. Close
                    // what we hold rather than leaving a tuner occupied by a
                    // feature the operator has turned off.
                    self.close_all_transports().await;
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

        // Decide about the tuner before touching the disk. The attempt file is
        // created with `O_EXCL`, so a file left behind by a refusal makes this
        // attempt number unusable for ever: the retry next tick would find its
        // own orphan and read it as a fenced predecessor's. Every fallible step
        // that does not need the file therefore happens first, and the two
        // that remain after it undo themselves.
        let joined = {
            let registry = self
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
                    if schedule::may_share_transport(transport.open_until(), row.capture_start) =>
                {
                    Some(transport)
                }
                _ => {
                    if !registry.may_open_transport(live_tv.max_sessions, dvr.tuner_reserve) {
                        return Err(LiveTvError::Capacity(
                            "no tuner is free for this recording".to_owned(),
                        ));
                    }
                    None
                }
            }
        };

        if let Some(parent) = base.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                LiveTvError::StreamFailed(format!("creating the recording folder: {error}"))
            })?;
        }
        // `create_new` is the whole guarantee against a fenced predecessor: an
        // attempt file that already exists means another process owns it, and
        // this attempt takes the next number rather than truncating it.
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(attempt_path(&base, attempt))
            .await
            .map_err(|error| {
                LiveTvError::StreamFailed(format!(
                    "opening attempt {attempt} of this recording: {error}"
                ))
            })?;
        let delivered = joined
            .as_ref()
            .map(|transport| Arc::clone(&transport.delivered))
            .unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
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
        start_sink_writer(&sink, Box::new(file), Arc::clone(&delivered));

        if let Some(transport) = joined {
            // Publish the sink only once its file is open, so the fan-out
            // never sees a sink it cannot write to.
            transport
                .sinks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Arc::clone(&sink));
            return Ok(());
        }

        let transport = Arc::new(DvrTransport {
            channel: channel.clone(),
            generation,
            owner_serving_generation: serving_generation,
            cancel: CancellationToken::new(),
            delivered,
            sinks: std::sync::Mutex::new(vec![Arc::clone(&sink)]),
            worker: std::sync::Mutex::new(None),
            source: std::sync::Mutex::new(None),
        });
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .transports
                .insert(row.channel_id.clone(), Arc::clone(&transport));
        }
        let manager = Arc::downgrade(self);
        let worker_transport = Arc::clone(&transport);
        let guide_number = channel.guide_number.clone();
        let scratch = self.scratch_root.join(format!("dvr-{}", channel.id));
        let serving = self.serving.clone();
        let worker = tokio::spawn(async move {
            let result = run_transport(
                client,
                address,
                guide_number,
                scratch,
                serving,
                Arc::clone(&worker_transport),
            )
            .await;
            if let Some(manager) = manager.upgrade() {
                let channel_id = worker_transport.channel.id.clone();
                // Cleanup must not await the JoinHandle of the task that is
                // currently executing this block. Hand it to a sibling task
                // so the worker can become joinable before close_transport
                // settles writers and collects it.
                tokio::spawn(async move {
                    manager.close_transport(&channel_id).await;
                });
            }
            result
        });
        *transport
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        Ok(())
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

    async fn close_finished_transports(&self) {
        let spent = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .transports
                .iter()
                .filter(|(_, transport)| transport.live_sinks().is_empty())
                .map(|(channel, _)| channel.clone())
                .collect::<Vec<_>>()
        };
        for channel in spent {
            self.close_transport(&channel).await;
        }
    }

    pub(crate) async fn close_transport(&self, channel_id: &str) {
        let transport = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.get(channel_id).cloned()
        };
        let Some(transport) = transport else {
            return;
        };
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
        if settling {
            return;
        }
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if registry
                .transports
                .get(channel_id)
                .is_some_and(|current| Arc::ptr_eq(current, &transport))
            {
                registry.transports.remove(channel_id);
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
    }

    pub(crate) async fn close_all_transports(&self) {
        let channels = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.keys().cloned().collect::<Vec<_>>()
        };
        for channel in channels {
            self.close_transport(&channel).await;
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
    address: std::net::Ipv4Addr,
    guide_number: String,
    scratch: PathBuf,
    serving: crate::serving_fence::ServingAuthority,
    transport: Arc<DvrTransport>,
) -> Result<(), LiveTvError> {
    let url = pinned_url(address, 5004, &format!("/auto/v{guide_number}"))?;
    let deadline = tokio::time::Instant::now() + super::STARTUP_TIMEOUT;
    let response = open_tuner_stream(&client, url, deadline).await?;
    let input = collect_live_prefix(response, &transport.cancel).await?;
    // The same prefix the live path probes, so every sidecar this transport
    // writes describes the same source facts a viewer's session would see.
    if let Err(error) = tokio::fs::create_dir_all(&scratch).await {
        tracing::warn!(%error, "could not create the capture scratch folder");
    }
    pump_tuner_fanout(input, serving, transport).await
}

/// The fan-out itself. Mirrors `pump_tuner_stream`: one task owns the response,
/// so cancelling and joining it releases the tuner connection rather than
/// leaving a detached reader behind.
async fn pump_tuner_fanout(
    input: LiveTunerInput,
    serving: crate::serving_fence::ServingAuthority,
    transport: Arc<DvrTransport>,
) -> Result<(), LiveTvError> {
    use futures_util::StreamExt as _;

    let mut stream = input.remainder;
    let mut pending = std::iter::once(input.prefix)
        .chain(input.queued)
        .collect::<std::collections::VecDeque<_>>();
    loop {
        let bytes = match pending.pop_front() {
            Some(bytes) => bytes,
            None => {
                let next = tokio::select! {
                    biased;
                    _ = transport.cancel.cancelled() => return Ok(()),
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
                bytes.map_err(|_| {
                    LiveTvError::StreamFailed("the HDHomeRun stream body failed".into())
                })?
            }
        };
        // Checked per chunk, exactly as a session's fence is. A node that
        // has lost serving authority must stop writing within one read
        // timeout: the replacement owner is about to open its own attempt,
        // and two processes writing one recording is the corruption the
        // attempt files exist to make impossible.
        if !serving.is_current(transport.owner_serving_generation) {
            for sink in transport.live_sinks() {
                sink.cancel.cancel();
            }
            return Err(LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
            ));
        }
        let sinks = transport.live_sinks();
        if sinks.is_empty() {
            return Ok(());
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
                interrupt_sink(&sink, "disk_write_backlog");
                continue;
            }
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            match queue.tx.try_send(bytes.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    queue.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                    interrupt_sink(&sink, "disk_write_backlog");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    queue.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                    interrupt_sink(&sink, "disk_write_failed");
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
        delivered: Arc<AtomicU64>,
        serving_generation: u64,
    ) -> Arc<DvrTransport> {
        Arc::new(DvrTransport {
            channel: LiveTvChannel {
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
            },
            generation: 1,
            owner_serving_generation: serving_generation,
            cancel: CancellationToken::new(),
            delivered,
            sinks: std::sync::Mutex::new(sinks),
            worker: std::sync::Mutex::new(None),
            source: std::sync::Mutex::new(None),
        })
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
        )
        .await;
        assert!(matches!(result, Err(LiveTvError::StreamFailed(_))));
        assert_eq!(settle_sink(&left).await, SinkStopResult::Settled);
        assert_eq!(settle_sink(&right).await, SinkStopResult::Settled);
        assert_eq!(*left_bytes.lock().expect("left bytes"), expected);
        assert_eq!(*right_bytes.lock().expect("right bytes"), expected);
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
