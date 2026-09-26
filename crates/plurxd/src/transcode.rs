//! On-the-fly HLS transcode sessions.
//!
//! When the decision engine says a file must be transcoded (HEVC/4K/HDR the
//! device can't take), we spawn one ffmpeg per session producing HLS segments
//! into a temp dir, and serve the playlist and segments over HTTP. Sessions
//! are reaped when idle. This is the session-based model; the deterministic
//! per-segment model that enables cluster failover is Phase 3's spike.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU64, AtomicUsize,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use plurx_core::domain::{
    CacheConsumerKind, CacheConsumerPin, OfflinePackage, PlaybackEvent, PretranscodeJob,
    PretranscodeWorkerCapabilities,
};
use plurx_core::error::StoreError;
use plurx_core::store::{keys, PublicationFence, PublicationStore, Store};
use plurx_core::transcode::{
    self, AttemptRestrictions, DecodeCacheIdentity, DecodeCapabilities,
    DecodeCapabilitySnapshotIdentity, DecodeCatalogMetadata, DecodeFacts, DecodePlanPolicy,
    DecodePolicySnapshot, DecodeSourceIdentity, EffectiveRateControl, Encoder, EncoderCaps,
    OutputGrade, Pacing, Pipeline, PipelineDigest, QualityRateControlValidation, QualityRc,
    RateMode, Recipe, ResolvedTranscode, SoftwareDecoder, ToneMap, TranscodeExecution,
    TranscodeMediaOptions, TranscodeOptions, TranscodeRequest, CACHE_RECIPE_VERSION,
};
use sha2::{Digest as _, Sha256};
use tokio::process::Child;
use tokio::sync::{Mutex, RwLock};

use crate::admission::Admission;
use crate::admission::TranscodeResourceEstimate;
use crate::admission::{
    Admissions, HwSlot, Priority, Workload, DEFAULT_MAX_HW_SESSIONS, QUEUE_WAIT,
};
use crate::copyseg;
use crate::ffmpeg::ffmpeg_bin;
use crate::ffmpeg::pacing_caps;
use crate::media_sessions::SessionSettlementGuard;
use crate::meter::Meter;

/// Namespace below the transcode work root which is owned by the Live TV
/// lifecycle rather than `TranscodeManager.sessions`.
pub(crate) const LIVE_TV_WORK_DIR_NAME: &str = "live-tv";

/// Idle timeout after which a session's ffmpeg is killed and its dir removed.
const SESSION_IDLE_SECS: u64 = crate::playback_control::ROLLING_LEASE_TIMEOUT_MS as u64 / 1_000;
/// Stable classification for a start that lost a bounded admission wait.
/// The HTTP layer maps only this class to 503; source, filesystem, and ffmpeg
/// failures remain server errors rather than being mislabeled as contention.
const RETRYABLE_CAPACITY_PREFIX: &str = "transcode capacity is temporarily unavailable: ";
/// The one capacity refusal a client may safely wait out by re-posting the
/// same request: this player's previous start has not let go of it yet.
///
/// A strict subset of the capacity class, and deliberately narrow. The other
/// producers of `capacity_error` are not all waits — one instructs the client
/// to ask for a smaller height, and one reports a scratch ceiling above the
/// configured budget, which no amount of retrying can satisfy. Those keep the
/// codeless 503 that no client retries.
const REPLACEMENT_WAIT_PREFIX: &str =
    "transcode capacity is temporarily unavailable: waiting for this player's previous start: ";
/// What a client should wait before re-posting. One cooperative window: by
/// then the previous start has either let go or been reclaimed.
pub(crate) const REPLACEMENT_WAIT_RETRY_AFTER_SECS: u64 = 3;
/// Longer than any legitimate hold of a player's key.
///
/// The key is held from provisional creation through the durable activation
/// verdict and predecessor settlement, so the ceiling has to clear the longest
/// declared start budget plus those windows — not the cooperative wait, which
/// is only how long an arrival is willing to queue. A holder past this has
/// exceeded every budget it asked for and is wedged by definition; reclaiming
/// on any shorter clock destroys healthy starts, which is what the review of
/// the first version of this fix found.
const CLUSTER_REPLACEMENT_HOLD_CEILING: Duration = Duration::from_secs(120);
/// How many times one acquisition will re-enter the registry after finding the
/// gate it waited on had been retired underneath it. Bounded because each pass
/// spends real time from the caller's own deadline.
const MAX_CLUSTER_REPLACEMENT_REENTRIES: usize = 4;
const SERVING_FENCE_PREFIX: &str = "media serving authority is unavailable: ";
const START_INFRASTRUCTURE_PREFIX: &str = "media session infrastructure is unavailable: ";
/// How long descriptor-bound fact collection may borrow from a producer's
/// deadline.
///
/// The bound probe is an improvement on the stored-probe plan, not a
/// precondition for producing anything. Left unbounded it takes the whole
/// production window from a title whose source probes slowly, and the producer
/// then makes nothing — every cycle, forever, for that title. Bounded, the
/// worst case is ten seconds and a plan built from stored facts, which is what
/// every other path already uses. Ten seconds also bounds S-08's descriptor-
/// bound `idet` verification for flagged sources. The elapsed time is added
/// back to the production deadline so observing costs the encode nothing.
const DECODE_PLAN_PROBE_BUDGET: Duration = Duration::from_secs(10);
static PROBE_CHANGED_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);

/// How long a mixed recovery waits for the CPU it newly needs.
///
/// Bounded rather than instantaneous because the usual reason the pool refuses
/// is a background producer holding a permit, and a background producer yields
/// — but only once a live waiter is registered, and only at its next
/// checkpoint. A single non-blocking try turns "wait two seconds" into
/// "destroy the session", because by this point the predecessor is already
/// terminated and its scratch already cleared. Bounded rather than unlimited
/// because §5 says to use the existing startup budget, not to invent a new
/// wait: a viewer is watching a stall while this runs.
const MIXED_RECOVERY_CAPACITY_WAIT: Duration = Duration::from_secs(5);

/// Give back what observation spent, so a bounded probe cannot shorten the
/// encode it was meant to inform.
fn retain_production_budget_after_planning(
    production_deadline: Instant,
    observation_duration: Duration,
) -> Instant {
    production_deadline
        .checked_add(observation_duration)
        .unwrap_or(production_deadline)
}

const ADMISSION_POLL: Duration = Duration::from_millis(250);
const SCRATCH_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
const SCRATCH_SAMPLE_MAX_AGE: Duration = Duration::from_secs(45);
const CACHE_OFFER_VERDICT_TTL: Duration = Duration::from_secs(30);
const MAX_CACHE_OFFER_VERDICTS: usize = 256;
const MAX_CLUSTER_REPLACEMENT_GATES: usize = 4_096;
const CLUSTER_REPLACEMENT_GATE_WAIT: Duration = Duration::from_secs(3);
const MARKER_AMBIGUITY_TTL: Duration = Duration::from_secs(60);
const MAX_MARKER_AMBIGUITIES: usize = 4_096;
#[derive(Debug, Default)]
struct RecentMarkerAmbiguityLedger {
    entries: HashMap<(String, i64, &'static str), Instant>,
    /// Saturation cannot discard a live retirement identity: doing so could
    /// let its delayed placeholder steal a VOD ledger. A single bounded
    /// global tombstone fails closed until every unrepresented retirement
    /// admitted during saturation has aged out.
    overflow_ambiguous_until: Option<Instant>,
}
type RecentMarkerAmbiguities = Arc<std::sync::Mutex<RecentMarkerAmbiguityLedger>>;
const SHARED_LOOKUP_PIN_MS: i64 = 30_000;
/// One retained owner per actor-managed generation is sufficient, but the
/// global bound also protects the process when many request futures vanish
/// while their first-media commands are still queued.
const MAX_FIRST_MEDIA_SETTLEMENT_OWNERS: usize = 256;
const ROLLING_SCRATCH_CLEANUP_ATTEMPT: Duration = Duration::from_secs(5);
const ROLLING_SCRATCH_CLEANUP_RETRY: Duration = Duration::from_secs(5);
const ROLLING_SCRATCH_CLEANUP_ATTEMPTS: usize = 3;

fn first_media_settlement_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_FIRST_MEDIA_SETTLEMENT_OWNERS,
        ))
    })
}

/// Stable non-secret correlation for bearer session capabilities. Raw UUIDs
/// authorize playback and therefore never belong in logs, traces, metrics, or
/// diagnostics even though they look like ordinary identifiers.
pub(crate) fn session_log_id(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    format!("s-{}", hex::encode(digest))
}

/// Remove the bearer capability anywhere a child-process diagnostic echoed
/// it. Structured fields already use [`session_log_id`], but ffmpeg repeats
/// paths and complete arguments in stderr; sanitizing the message body keeps
/// those unstructured surfaces under the same contract.
fn session_log_text(text: &str, session_id: &str) -> String {
    if session_id.is_empty() {
        return text.to_owned();
    }
    text.replace(session_id, &session_log_id(session_id))
}

fn ffmpeg_args_log_message(label: &str, args: &[String], session_id: &str) -> String {
    format!(
        "{label}: {}",
        session_log_text(&crate::scratch_put::redact(&args.join(" ")), session_id)
    )
}

fn log_ffmpeg_stderr(session_id: &str, encoder: &str, line: &str) {
    let line = session_log_text(&crate::scratch_put::redact(line), session_id);
    tracing::warn!(
        session = %session_log_id(session_id),
        encoder,
        "transcode ffmpeg: {line}"
    );
}

fn capacity_error(message: impl AsRef<str>) -> String {
    format!("{RETRYABLE_CAPACITY_PREFIX}{}", message.as_ref())
}

fn replacement_deadline_error() -> String {
    capacity_error("the replacement start expired before it could finish provisional work")
}

pub(crate) fn is_retryable_capacity_error(error: &str) -> bool {
    error.starts_with(RETRYABLE_CAPACITY_PREFIX)
}

fn replacement_wait_error(message: impl AsRef<str>) -> String {
    format!("{REPLACEMENT_WAIT_PREFIX}{}", message.as_ref())
}

/// A subset of [`is_retryable_capacity_error`], so every existing server-side
/// consumer of the capacity class keeps seeing these unchanged.
pub(crate) fn is_replacement_wait_error(error: &str) -> bool {
    error.starts_with(REPLACEMENT_WAIT_PREFIX)
}

pub(crate) fn serving_fence_error(message: impl AsRef<str>) -> String {
    format!("{SERVING_FENCE_PREFIX}{}", message.as_ref())
}

pub(crate) fn is_serving_fence_error(error: &str) -> bool {
    error.starts_with(SERVING_FENCE_PREFIX)
}

pub(crate) fn start_infrastructure_error(message: impl AsRef<str>) -> String {
    format!("{START_INFRASTRUCTURE_PREFIX}{}", message.as_ref())
}

pub(crate) fn is_start_infrastructure_error(error: &str) -> bool {
    error.starts_with(START_INFRASTRUCTURE_PREFIX)
}

/// Stable classification for a start this ffmpeg build cannot perform at all.
///
/// Separate from a capacity error because the answers differ: capacity says
/// "try again", this says "this will never work here, and here is what is
/// missing". Both exist because the alternative — an unclassified `String` —
/// reaches the client as `ApiError::Internal`, which deliberately drops its
/// message. A refusal nobody can read is the anonymous failure this change
/// exists to remove.
const UNSUPPORTED_BUILD_PREFIX: &str = "this server's media tools cannot do that: ";

fn unsupported_build_error(message: impl AsRef<str>) -> String {
    format!("{UNSUPPORTED_BUILD_PREFIX}{}", message.as_ref())
}

/// Strip the classification, leaving the sentence a viewer should read.
pub(crate) fn unsupported_build_reason(error: &str) -> Option<&str> {
    error.strip_prefix(UNSUPPORTED_BUILD_PREFIX)
}

/// Stable classification for a malformed or stale bound reopen. The HTTP
/// layer returns this as a client error without exposing unrelated internals.
const INVALID_REOPEN_PREFIX: &str = "invalid stall reopen: ";

fn invalid_reopen_error(reason: &str) -> String {
    format!("{INVALID_REOPEN_PREFIX}{reason}")
}

pub(crate) fn invalid_reopen_reason(error: &str) -> Option<&str> {
    error.strip_prefix(INVALID_REOPEN_PREFIX)
}

/// Stable classification for a request that cannot receive the immutable VOD
/// presentation. The live HLS presentation is not a recovery path: callers
/// must receive this verdict and either wait for the named prerequisite or
/// show the honest unsupported result.
const VOD_REFUSAL_PREFIX: &str = "vod presentation unavailable: ";
const CACHED_MEDIA_INTEGRITY_FAILURE: &str = "cached media failed an integrity check";

pub(crate) fn vod_refusal_error(code: &'static str, message: impl AsRef<str>) -> String {
    format!("{VOD_REFUSAL_PREFIX}{code}: {}", message.as_ref())
}

pub(crate) fn vod_refusal(error: &str) -> Option<(&str, &str)> {
    let classified = error.strip_prefix(VOD_REFUSAL_PREFIX)?;
    let (code, message) = classified.split_once(": ")?;
    if code.is_empty() || message.is_empty() {
        return None;
    }
    Some((code, message))
}

#[derive(Clone, Copy)]
enum BoundPlanCaller {
    Pretranscode,
    Vod,
}

/// The class of what `encoder_and_grade_for` starts: every caller of it is a
/// session start (VOD or streamed) or a peer's media offer for one.
const SESSION_START_CLASS: crate::process_control::ChildClass =
    crate::process_control::ChildClass::Realtime;

/// The held source probe a VOD start runs under `ENGINE_PROBE_TIMEOUT`. A
/// viewer waits on it and a miss answers `vod_source_rescan_required`, so it
/// is realtime (plan P-02 §3.2.2).
const VOD_START_HELD_PROBE: crate::process_control::ChildWork =
    crate::process_control::ChildWork::realtime("held source probe for a session start");

impl BoundPlanCaller {
    /// The class and purpose of the decode-fact probes this caller waits on.
    /// A VOD start waits up to `DECODE_PLAN_PROBE_BUDGET` with a viewer in
    /// front of it; the pre-transcode pass has nobody waiting.
    const fn decode_fact_work(self) -> crate::process_control::ChildWork {
        match self {
            Self::Pretranscode => crate::process_control::ChildWork::background(
                "decode-fact probe for the pre-transcode pass",
            ),
            Self::Vod => {
                crate::process_control::ChildWork::realtime("decode-fact probe for a session start")
            }
        }
    }

    fn decode_fact_source(
        self,
        handle: Arc<std::fs::File>,
        offset_gate: Arc<tokio::sync::Semaphore>,
    ) -> crate::decode_facts::DecodeFactSource {
        crate::decode_facts::DecodeFactSource::new(handle, offset_gate, self.decode_fact_work())
    }

    fn finish(
        self,
        result: Result<ResolvedTranscode, String>,
    ) -> Result<ResolvedTranscode, String> {
        match self {
            Self::Pretranscode => result,
            Self::Vod => {
                result.map_err(|error| vod_refusal_error("vod_decoder_plan_refused", error))
            }
        }
    }
}

/// What "Auto" resolves to — see [`TranscodeManager::auto_height`].
const AUTO_SOFTWARE_HEIGHT: i64 = 720;
/// Safe fallback when the source has no usable geometry, and the highest HDR
/// output this node's production probe has actually exercised. Known SDR
/// sources may follow validated hardware encoders to [`MAX_HEIGHT`].
const AUTO_HARDWARE_PROBED_HEIGHT: i64 = 1080;
/// Floor for any requested rung. Below this there is no picture worth the
/// session; the adaptive ladder itself bottoms out at 360p.
pub const MIN_HEIGHT: i64 = 144;
/// Ceiling for any requested or resolved rung. Hardware-backed SDR Auto and
/// explicit quality/source promises may reach it.
pub const MAX_HEIGHT: i64 = 2160;
/// How long a segment request waits for ffmpeg to produce a not-yet-written
/// segment before giving up.
const SEGMENT_WAIT: Duration = Duration::from_secs(20);
/// A live HLS segment that was named by a playlist should already exist.
/// Record any material wait so a client-side freeze can be joined to producer
/// starvation instead of being inferred from a later timeout.
const SEGMENT_WAIT_EVENT_MIN: Duration = Duration::from_millis(250);

/// Media requests parked in [`TranscodeManager::segment_for_publication_before`]
/// waiting for the producer to publish what they asked for — segments, and
/// the init object a playlist request resolves through the same loop (those
/// carry no segment index).
///
/// The rolling twin of the VOD wait pool's reading. The rolling status used to
/// report a constant zero here, on the claim that rolling delivery never parks
/// a response — but the publication loop does park, for up to [`SEGMENT_WAIT`],
/// and that wait is exactly what a client's "Buffering…" detail needs in order
/// to tell a starved producer from a player sitting on media it already has.
#[derive(Default)]
struct HttpWaitLedger {
    entries: std::sync::Mutex<HttpWaitEntries>,
}

#[derive(Default)]
struct HttpWaitEntries {
    next_id: u64,
    open: HashMap<u64, (Instant, Option<i64>)>,
}

/// One status sample of a [`HttpWaitLedger`]: how many requests are parked,
/// how long the oldest has been, and which segment it asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HttpWaitSnapshot {
    count: usize,
    oldest_ms: Option<i64>,
    oldest_segment: Option<i64>,
}

impl HttpWaitLedger {
    fn enter(&self, segment: Option<i64>) -> u64 {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = entries.next_id;
        entries.next_id = entries.next_id.wrapping_add(1);
        entries.open.insert(id, (Instant::now(), segment));
        id
    }

    fn leave(&self, id: u64) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .open
            .remove(&id);
    }

    fn snapshot(&self) -> HttpWaitSnapshot {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let oldest = entries.open.values().min_by_key(|(since, _)| *since);
        HttpWaitSnapshot {
            count: entries.open.len(),
            oldest_ms: oldest
                .map(|(since, _)| since.elapsed().as_millis().min(i64::MAX as u128) as i64),
            oldest_segment: oldest.and_then(|(_, segment)| *segment),
        }
    }
}

/// Holds one entry in a session's [`HttpWaitLedger`] for as long as a segment
/// request is parked. Dropping it is the only way the entry leaves — on a
/// served segment, on every early return, and when the client disconnects and
/// the request future is dropped mid-sleep — so the count cannot leak.
struct HttpWaitGuard {
    session: Arc<Session>,
    id: u64,
}

impl HttpWaitGuard {
    fn enter(session: &Arc<Session>, segment: Option<i64>) -> Self {
        Self {
            id: session.http_waits.enter(segment),
            session: Arc::clone(session),
        }
    }
}

impl Drop for HttpWaitGuard {
    fn drop(&mut self) {
        self.session.http_waits.leave(self.id);
    }
}

/// Below this sustained storage rate, a material segment read is a stall.
/// Normalizing by bytes keeps the signal comparable when body read buffers
/// change size.
const SEGMENT_STALL_BYTES_PER_SECOND: f64 = (1024 * 1024) as f64;

fn storage_read_is_slow(bytes: u64, elapsed: Duration) -> bool {
    elapsed >= SEGMENT_WAIT_EVENT_MIN
        && bytes as f64 / elapsed.as_secs_f64() < SEGMENT_STALL_BYTES_PER_SECOND
}

/// Hold the first live transcode playlist until it has both two complete
/// segments and this much published media. The first playlist used to expose
/// one ~2 s segment while ffmpeg was already writing the rest; hls.js reached
/// that edge at the same instant it scheduled its first EVENT reload and
/// visibly stalled even when the encoder had finished the entire title.
///
/// Two nominal segments are the smallest useful head start. Media duration is
/// the actual contract rather than `TARGETDURATION` (a ceiling, not inventory),
/// and requiring two entries prevents one unusually long opening segment from
/// recreating the same live-edge race. Finished short titles escape through
/// `ENDLIST` rather than waiting for media that can never exist.
const TRANSCODE_START_CUSHION_MS: i64 = transcode::SEGMENT_SECONDS as i64 * 2 * 1_000;
/// How often a held playlist request re-asks. Naming the cadence keeps the
/// start cushion inside the same failure/cancellation surface: no detached task
/// survives a dropped HTTP request and a failed session still exits
/// immediately.
const PLAYLIST_WAIT_POLL: Duration = Duration::from_millis(100);

async fn wait_for_playlist_poll_before(deadline: Instant) {
    let now = tokio::time::Instant::now().into_std();
    if now >= deadline {
        return;
    }
    tokio::time::sleep_until(tokio::time::Instant::from_std(
        (now + PLAYLIST_WAIT_POLL).min(deadline),
    ))
    .await;
}
/// Compatibility mirror of the actor's bounded hardware startup budget. It
/// sizes only the HTTP playlist wait; it is not a timer or recovery owner.
/// The prepublication actor exclusively commits the 12-second verdict.
const ACTOR_HARDWARE_STARTUP_BUDGET: Duration = Duration::from_secs(12);
/// Compatibility mirror of the actor's bounded software startup/retry budget.
/// Like the hardware mirror, it sizes HTTP patience and owns no timer.
const ACTOR_SOFTWARE_STARTUP_BUDGET: Duration = Duration::from_secs(30);
/// How long ffmpeg's output timestamp may sit still. It is the actor's
/// progress budget for every actor-managed rolling producer.
/// How long a flow evaluation waits for the actor to order its desire.
///
/// This bounds a mailbox round trip, not a producer: it selects no recipe,
/// publishes no response and starts no process. A caller that cannot be
/// ordered within it simply does not signal, and the next evaluation tries
/// again.
const FLOW_INTENTION_BUDGET: Duration = Duration::from_secs(5);

const PROGRESS_STALL: Duration = Duration::from_secs(10);
/// One typed copy-reader fact is a bounded actor ingress operation. This is
/// separate from the actor's own five-second two-fact rendezvous, which starts
/// only after the first exact-attempt completion/exit fact is accepted.
const COPY_READER_INGRESS_TIMEOUT: Duration = Duration::from_secs(5);
/// Repair cadence for a producer that has already been terminalized but whose
/// process reap could not yet be confirmed. This owner never makes playback
/// policy or replacement decisions; it only retains resources until physical
/// cleanup converges.
const PREPUBLICATION_REAP_RETRY: Duration = Duration::from_secs(5);
/// One supervisor terminate/reap request may not monopolize lifecycle
/// serialization. Timeout retains the child and permits for the next repair
/// attempt; it never treats an unconfirmed reap as success.
const PREPUBLICATION_REAP_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
/// Slack on top of both exact actor startup budgets for scheduler latency,
/// predecessor reap, successor spawn/install, and the request's 100 ms file
/// observation cadence. It is deliberately independent of every retained
/// compatibility polling interval.
const PLAYLIST_WAIT_SLACK: Duration = Duration::from_secs(13);
/// How long a playlist request holds a still-starting session before giving up.
///
/// **Derived from the server's own startup-recovery budget, not chosen.** This
/// used to be an independent 30 s, sized against the copy path's publish gate
/// (`COPY_PUBLISH_GATE_SECS`) and never against the recovery it now has to
/// outlive. A hardware start gets `ACTOR_HARDWARE_STARTUP_BUDGET` before it
/// receives its one immutable retry and then the actor's software budget to
/// produce real output — 42 s of *legitimate, still-working* startup, all of
/// it past a 30 s wait. So a session the control transaction was in the middle
/// of rescuing had its playlist 404'd, and hls.js escalated that to a fatal
/// `levelLoadError`: the viewer was told the stream had permanently failed
/// while the server was still successfully starting it. A mid-film bitmap
/// subtitle burn is the likeliest session to need exactly that fallback — a
/// full re-encode plus a subtitle composite from a non-zero `-ss`, with no
/// direct-play or remux escape.
///
/// Holding rather than erroring is safe because the client's patience is
/// asymmetric (see [`TranscodeManager::playlist`]), and it costs nothing that
/// a *failed* session would pay: every terminal verdict still returns
/// immediately, so this bound is only ever spent on a session that is
/// genuinely still starting.
const PLAYLIST_WAIT_BUDGET: Duration = Duration::from_secs(
    ACTOR_HARDWARE_STARTUP_BUDGET.as_secs()
        + ACTOR_SOFTWARE_STARTUP_BUDGET.as_secs()
        + PLAYLIST_WAIT_SLACK.as_secs(),
);
/// How far behind the *download frontier* media is kept on disk, and why that
/// is not the same as "behind the playhead".
///
/// An HLS session's playlist grows for its whole life (event type), so without
/// pruning a full watch accumulates every segment — cheap at 720p, ~17 GB for
/// a 4K copy. The subtlety is what to measure from. The server knows the
/// highest segment a client has *fetched*, and a client fetches its whole
/// forward buffer ahead of what it is showing. A physical iPad running
/// AVPlayer fetched about 120 seconds ahead even with
/// `preferredForwardBufferDuration = 60`; that preference is not a hard cap.
/// Pruning at a fixed distance behind the frontier therefore deletes media the
/// viewer is about to watch — or is watching. Retention covers the observed
/// fetch lead, the back buffer the client keeps for scrubbing, and an
/// allowance for a retry or a playlist reload landing on something older.
const CLIENT_FORWARD_FETCH_SECS: i64 = 120;
const CLIENT_BACK_BUFFER_SECS: i64 = 30;
const RETRY_ALLOWANCE_SECS: i64 = 30;
const RETENTION_SECS: i64 =
    CLIENT_FORWARD_FETCH_SECS + CLIENT_BACK_BUFFER_SECS + RETRY_ALLOWANCE_SECS;
/// Maximum metadata handoffs performed while retention owns the producer-path
/// transition. Payload deletion happens detached after these bounded renames.
const RETENTION_HANDOFF_BATCH: usize = 32;
/// Why the retained live-HLS engine served a session, since this process
/// started. Indexed by `LiveRecoveryReason`.
///
/// This exists because the engine spends real encode time on real hardware and
/// nothing in the product said so. A `tracing::warn!` in a log file is not
/// attribution: an operator watching a GPU work could not find out what was
/// running, why it chose that work, or that a switch existed to stop it. The
/// count is exported as `plurx_live_hls_recovery_sessions_total{reason}` and
/// read back by the Developer readiness route, which names the switch.
static LIVE_RECOVERY_SESSIONS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// What made a session enter the retained engine rather than immutable VOD.
///
/// `RequestedLive` is the one worth watching. It is not a fallback decision at
/// all: it is a `SessionRequest` that arrived already naming the live
/// presentation, which today means the peer takeover path, whose recipe
/// validation requires it. That path consults no setting, so a node can serve
/// live-HLS sessions with the fallback switched off — visible here and
/// nowhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveRecoveryReason {
    RequestedLive,
    IndexPending,
    TranscodeUnavailable,
    SubtitleBurnUnavailable,
    SourceUnsupported,
}

impl LiveRecoveryReason {
    fn index(self) -> usize {
        match self {
            Self::RequestedLive => 0,
            Self::IndexPending => 1,
            Self::TranscodeUnavailable => 2,
            Self::SubtitleBurnUnavailable => 3,
            Self::SourceUnsupported => 4,
        }
    }

    pub(crate) const LABELS: [&'static str; 5] = [
        "requested_live",
        "vod_index_pending",
        "vod_transcode_unavailable",
        "vod_subtitle_burn_unavailable",
        "vod_source_unsupported",
    ];

    fn label(self) -> &'static str {
        Self::LABELS[self.index()]
    }

    fn from_refusal(code: &str) -> Option<Self> {
        match code {
            "vod_index_pending" => Some(Self::IndexPending),
            "vod_transcode_unavailable" => Some(Self::TranscodeUnavailable),
            "vod_subtitle_burn_unavailable" => Some(Self::SubtitleBurnUnavailable),
            "vod_source_unsupported" => Some(Self::SourceUnsupported),
            _ => None,
        }
    }
}

/// The reason labels, in `live_recovery_snapshot()` order.
pub(crate) async fn unverified_hevc_copy_enabled(store: &dyn Store) -> Result<bool, String> {
    let value = store
        .get_setting(plurx_core::store::keys::HEVC_UNVERIFIED_COPY)
        .await
        .map_err(|error| format!("reading HEVC copy preference: {error}"))?;
    Ok(plurx_core::store::stored_switch(value.as_deref(), false))
}

pub(crate) const LIVE_RECOVERY_LABELS: [&str; 5] = LiveRecoveryReason::LABELS;

fn record_live_recovery(reason: LiveRecoveryReason) {
    LIVE_RECOVERY_SESSIONS[reason.index()].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Sessions the retained engine served since start, by reason.
pub(crate) fn live_recovery_snapshot() -> [u64; 5] {
    std::array::from_fn(|index| {
        LIVE_RECOVERY_SESSIONS[index].load(std::sync::atomic::Ordering::Relaxed)
    })
}

pub(crate) fn live_recovery_prometheus() -> String {
    let counts = live_recovery_snapshot();
    let mut output = String::from(
        "# HELP plurx_live_hls_recovery_sessions_total Sessions served by the retained live-HLS engine, by why it was chosen.\n\
         # TYPE plurx_live_hls_recovery_sessions_total counter\n",
    );
    for (index, label) in LiveRecoveryReason::LABELS.iter().enumerate() {
        output.push_str(&format!(
            "plurx_live_hls_recovery_sessions_total{{reason=\"{label}\"}} {}\n",
            counts[index]
        ));
    }
    output
}

static RETENTION_SWEEP_ID: AtomicU64 = AtomicU64::new(0);
/// Default pace for an HLS session's input, as a multiple of realtime, and how
/// many seconds it may deliver flat-out first. Admin-overridable (see
/// [`keys::HLS_READRATE`] / [`keys::HLS_BURST_SECS`]).
pub(crate) const HLS_READRATE_DEFAULT: f64 = 2.0;
pub(crate) const HLS_BURST_SECS_DEFAULT: f64 = 90.0;
/// How far ahead of the download frontier a session may produce before it is
/// suspended — in seconds of published media, and in bytes on disk.
///
/// Both, because neither alone is a bound. 180 seconds is a few hundred
/// megabytes at a transcode rung and over a gigabyte of 4K copy, so a
/// time-only limit is not a disk contract; and a byte-only limit would starve
/// a low-bitrate stream of the reserve it could afford.
pub(crate) const HLS_AHEAD_MAX_SECS_DEFAULT: i64 = 180;
pub(crate) const HLS_AHEAD_MAX_BYTES_DEFAULT: i64 = 2 * 1024 * 1024 * 1024;
/// Media a still-starting session may always produce, whatever the client's
/// demand says.
///
/// **A starting client cannot report `active`.** The demand vocabulary is
/// derived from the player: the web client sends `hold` whenever
/// `video.paused` is true, and a `<video>` that has never received a byte is
/// paused. Apple and Android report the same shape from the same state. So
/// the first control of every session says `hold` — before playback, not
/// against it.
///
/// Honouring that hold before anything is published is a deadlock with no
/// exit. Production stops at zero, so no playlist is ever written; the
/// playlist request the client is blocked on spends the whole
/// [`PLAYLIST_WAIT_BUDGET`] and 503s; the client reports `manifestLoadTimeOut`
/// and shows "the server couldn't build the stream". The client cannot say
/// `active` until it plays, and it cannot play until this produces. Every
/// fallback to a fresh session — the transcode a refused remux escalates to,
/// most of all — died here, which turned one recoverable delivery fault into
/// a terminal one.
///
/// Startup admission is now actor-owned presentation state. Publication
/// volume cannot end the protection: only accepted Rendering progress does,
/// and the actor's non-renewing deadline bounds an abandoned client. The byte
/// limits remain active throughout because disk is a hard bound.
/// Ceiling on scratch across *all* live sessions. A per-session cap bounds one
/// runaway; it does nothing about four healthy 4K sessions filling the disk
/// between them.
pub(crate) const HLS_SCRATCH_MAX_BYTES_DEFAULT: i64 = 8 * 1024 * 1024 * 1024;
/// How long a snapshot of the ahead-window limits may serve flow control
/// before the settings are consulted again. The bound on how stale an
/// admin's change can look, and the whole cost of caching it.
const AHEAD_LIMITS_TTL: Duration = Duration::from_secs(2);
/// Repair cadence when no client request triggers flow control first. At the
/// default 2× pace this permits at most 30 seconds of additional production
/// between observations; keep the arithmetic pinned below.
const FLOW_CONTROL_REPAIR_INTERVAL: Duration = Duration::from_secs(15);

/// Live encode telemetry for one session, fed by ffmpeg's `-progress` stream.
///
/// Without it, "slow" and "stalled" are the same observation. The only signal
/// the session machinery had was whether a finished segment was listed yet —
/// a yes/no answer to a question that needs a rate. A 4K HDR session
/// tone-mapping at 0.7x and a session whose GPU has wedged look identical for
/// the first twelve seconds, and the watchdog killed both, restarting the
/// merely-slow one on software that is slower still. The actor now consumes
/// these measurements directly and owns the only progress deadline.
#[derive(Debug)]
pub struct Progress {
    /// Monotonic zero point for `moved_at_ms`.
    started: Instant,
    /// Which spawn attempt owns these numbers.
    ///
    /// A killed attempt's stdout reader does not stop the instant the process
    /// does — it can still be draining buffered lines while the replacement is
    /// already running. Without a generation, one of those late lines lands on
    /// the new attempt's telemetry, and a stale `out_time` from a process that
    /// got further along reads as "produced, then frozen": a healthy new
    /// encoder declared stalled by its dead predecessor.
    generation: AtomicU64,
    /// Content produced so far, in ms of output timeline; `-1` before the
    /// first block. Relative to the session's own `-ss`, because an input seek
    /// restarts output timestamps at zero.
    ///
    /// This is ENCODER progress, not published media: it includes frames muxed
    /// into the in-progress `.tmp` segment, which no client can fetch. The
    /// producer actor wants exactly that (proof of motion); pacing must not
    /// use it (that is what the segment index is for).
    out_time_ms: AtomicI64,
    /// Cumulative encode rate x1000 (`speed=1.85x` -> 1850); `-1` when unknown.
    speed_milli: AtomicI64,
    /// `started.elapsed()` when `out_time_ms` last *moved*. Staleness is
    /// measured from here rather than from the last block received: a stuck
    /// ffmpeg keeps emitting blocks, it just stops advancing.
    moved_at_ms: AtomicI64,
    /// Baseline for the recent-rate delta: wall clock and output time at the
    /// last usable sample, or [`SAMPLE_UNSET`] before there is one.
    sample_wall_ms: AtomicI64,
    sample_out_ms: AtomicI64,
    /// Smoothed recent rate x1000; `-1` until two usable samples exist.
    recent_milli: AtomicI64,
}

/// "No baseline yet". A distinct sentinel rather than `-1`, because these two
/// fields are *subtracted* — a sentinel inside the arithmetic's own range is a
/// value that can be silently differenced against a real one.
const SAMPLE_UNSET: i64 = i64::MIN;

/// A sample separated by more than this much wall clock spans a suspend or a
/// stall; dividing content by that gap reports a slowdown that never happened,
/// so such a sample re-baselines instead of contributing.
const RECENT_SAMPLE_MAX_GAP_MS: i64 = 5_000;
/// Samples closer together than this are noise -- ffmpeg emits progress about
/// twice a second and adjacent blocks carry real jitter.
const RECENT_SAMPLE_MIN_MS: i64 = 400;

/// One exponential-moving-average step over a progress sample, or `None` when
/// the sample should not move the average at all.
///
/// Pure, because the two rejection rules are the whole subtlety and they are
/// invisible in a test that has to sleep to produce a sample: a gap longer
/// than the cutoff spans a suspend (dividing its content by the stopped time
/// invents a slowdown the moment a held session resumes), and a gap shorter
/// than the floor is two adjacent ffmpeg blocks whose jitter is larger than
/// the signal.
fn recent_rate_step(prev_ewma: i64, d_wall_ms: i64, d_out_ms: i64) -> Option<i64> {
    if !(RECENT_SAMPLE_MIN_MS..=RECENT_SAMPLE_MAX_GAP_MS).contains(&d_wall_ms) || d_out_ms < 0 {
        return None;
    }
    let instant = (d_out_ms * 1000) / d_wall_ms;
    Some(if prev_ewma < 0 {
        instant
    } else {
        // Weighted toward history: a single slow segment on a variable-bitrate
        // film should bend the number, not spike it.
        (prev_ewma * 7 + instant * 3) / 10
    })
}

impl Progress {
    pub fn new() -> Progress {
        Progress {
            started: Instant::now(),
            generation: AtomicU64::new(0),
            out_time_ms: AtomicI64::new(-1),
            speed_milli: AtomicI64::new(-1),
            moved_at_ms: AtomicI64::new(0),
            sample_wall_ms: AtomicI64::new(SAMPLE_UNSET),
            sample_out_ms: AtomicI64::new(SAMPLE_UNSET),
            recent_milli: AtomicI64::new(-1),
        }
    }

    /// Start a new attempt: forget everything measured so far and return the
    /// generation the new process's reader must quote to be believed.
    ///
    /// Called when a session respawns (the hardware->software fallback). The
    /// replacement writes its own timeline from the same seek point, so
    /// carrying the dead process's numbers would make its actor progress
    /// deadline look expired -- and the generation bump is what stops the dead
    /// process's reader from putting them back.
    pub fn begin_attempt(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Relaxed) + 1;
        self.reset_attempt(generation);
        generation
    }

    /// Reset the compatibility telemetry to an attempt allocated by the
    /// rolling actor. Offline producers still use [`Self::begin_attempt`]; a
    /// live session must share the actor's attempt fence with publication and
    /// response commits so a predecessor cannot mutate its successor.
    fn begin_fenced_attempt(&self, generation: u64) {
        self.generation.store(generation, Relaxed);
        self.reset_attempt(generation);
    }

    fn reset_attempt(&self, generation: u64) {
        debug_assert!(generation > 0);
        self.out_time_ms.store(-1, Relaxed);
        self.speed_milli.store(-1, Relaxed);
        self.recent_milli.store(-1, Relaxed);
        self.sample_wall_ms.store(SAMPLE_UNSET, Relaxed);
        self.sample_out_ms.store(SAMPLE_UNSET, Relaxed);
        self.moved_at_ms
            .store(self.started.elapsed().as_millis() as i64, Relaxed);
    }

    fn generation(&self) -> u64 {
        self.generation.load(Relaxed)
    }

    fn note_out_time(&self, ms: i64) {
        let now = self.started.elapsed().as_millis() as i64;
        if self.out_time_ms.swap(ms, Relaxed) == ms {
            return; // a repeated timestamp is not progress
        }
        self.moved_at_ms.store(now, Relaxed);

        // Recent rate: content produced per second of wall clock, smoothed.
        // ffmpeg's own `speed=` is cumulative over the whole session, which
        // hides a slowdown behind a fast start and reads as nonsense across a
        // suspend -- and it is the recent number that predicts whether the
        // viewer's reserve is about to drain.
        let last_wall = self.sample_wall_ms.swap(now, Relaxed);
        let last_out = self.sample_out_ms.swap(ms, Relaxed);
        if last_wall == SAMPLE_UNSET || last_out == SAMPLE_UNSET {
            return; // the first sample only establishes a baseline
        }
        let prev = self.recent_milli.load(Relaxed);
        if let Some(next) = recent_rate_step(prev, now - last_wall, ms - last_out) {
            self.recent_milli.store(next, Relaxed);
        }
    }

    /// How long output has not advanced. Also the answer before the first
    /// block ever arrives, which is what makes a session that never opened its
    /// input measurable by the same rule as one that died halfway.
    fn stalled_for(&self) -> Duration {
        let now = self.started.elapsed().as_millis() as i64;
        Duration::from_millis((now - self.moved_at_ms.load(Relaxed)).max(0) as u64)
    }

    pub fn out_time_ms(&self) -> Option<i64> {
        Some(self.out_time_ms.load(Relaxed)).filter(|v| *v >= 0)
    }

    pub fn speed(&self) -> Option<f64> {
        self.speed_milli().map(|v| v as f64 / 1000.0)
    }

    fn speed_milli(&self) -> Option<i64> {
        Some(self.speed_milli.load(Relaxed)).filter(|v| *v >= 0)
    }

    /// The rate over the last few seconds, which is the one that predicts a
    /// stall. Falls back to nothing rather than to the cumulative figure --
    /// reporting a session's lifetime average as "recent" is how a slowdown
    /// stays invisible.
    pub fn recent_speed(&self) -> Option<f64> {
        self.recent_speed_milli().map(|v| v as f64 / 1000.0)
    }

    fn recent_speed_milli(&self) -> Option<i64> {
        Some(self.recent_milli.load(Relaxed)).filter(|v| *v >= 0)
    }

    /// Restart the motion clock without touching anything measured.
    ///
    /// Called when a suspended session is resumed. `moved_at_ms` stopped
    /// advancing the moment the SIGSTOP landed — correctly, nothing was
    /// moving — so the first stall check after SIGCONT would otherwise read
    /// the whole suspension as "output has not advanced for minutes" and fail
    /// a healthy session that simply had not emitted its first post-resume
    /// progress block yet. The recent-rate sampler needs no equivalent: a
    /// sample spanning the suspension is already rejected by
    /// [`RECENT_SAMPLE_MAX_GAP_MS`].
    fn touch(&self) {
        self.moved_at_ms
            .store(self.started.elapsed().as_millis() as i64, Relaxed);
    }
}

// split: begin segment-index
#[path = "transcode/rolling/segment_index.rs"]
mod segment_index;
use segment_index::*;
// split: end segment-index

// split: begin flow
#[path = "transcode/rolling/flow.rs"]
mod flow;
pub use flow::*;
// split: end flow

// split: begin spawn
#[path = "transcode/producer/spawn.rs"]
mod spawn;
pub use spawn::*;
// split: end spawn

/// Remove the (empty/partial) HLS output so a restarted ffmpeg starts clean.
async fn clear_session_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await.map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("could not enumerate {}: {error}", dir.display()),
        )
    })?;
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("could not enumerate {}: {error}", dir.display()),
        )
    })? {
        let path = entry.path();
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!("could not remove {}: {error}", path.display()),
                ));
            }
        }
    }
    drop(entries);

    // The child/path transition excludes a successor writer, but detached
    // retention cleanup can race this scan. Verify the directory is actually
    // empty before ownership is allowed to reopen under the next attempt.
    let mut remaining = tokio::fs::read_dir(dir).await?;
    if let Some(entry) = remaining.next_entry().await? {
        return Err(std::io::Error::other(format!(
            "{} remained after predecessor cleanup",
            entry.path().display()
        )));
    }
    Ok(())
}

// split: begin prepublication
#[path = "transcode/rolling/prepublication.rs"]
mod prepublication;
use prepublication::*;
// split: end prepublication

// split: begin retirement
#[path = "transcode/rolling/retirement.rs"]
mod retirement;
use retirement::*;
// split: end retirement

// split: begin prepublication-executor
#[path = "transcode/producer/prepublication_executor.rs"]
mod prepublication_executor;
use prepublication_executor::*;
// split: end prepublication-executor

/// Fail in terms of the dependency that is actually missing.
///
/// Several tests here and in [`crate::http`] drive the real spawn path. They
/// don't need ffmpeg to *succeed* — the fixtures are placeholder bytes, so it
/// always exits with an error — they need it to *start*, because what they
/// assert on is the session bookkeeping that only exists once there is a child
/// process to track. Absent ffmpeg that arrives as `No such file or directory
/// (os error 2)` under twenty frames of tokio, or, having been through the HTTP
/// layer first, as nothing more informative than `left: 500, right: 200`.
///
/// plurxd shells out to ffmpeg at runtime, so this is a dependency to install
/// rather than a test to skip: skipping would let CI report green on the
/// transcode paths without having run any of them.
#[cfg(test)]
pub(crate) fn require_ffmpeg() {
    let bin = ffmpeg_bin();
    if let Err(err) = std::process::Command::new(&bin)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        panic!(
            "this test needs ffmpeg, and running `{bin}` failed: {err}\n\
             install it (`apt-get install ffmpeg`, `brew install ffmpeg`) or point \
             PLURX_FFMPEG at a build — plurxd requires it at runtime too"
        );
    }
}

fn tone_map_pref() -> ToneMap {
    match std::env::var("PLURX_TONEMAP").as_deref() {
        Ok("libplacebo") => ToneMap::Libplacebo,
        Ok("off" | "none" | "passthrough") => ToneMap::None,
        _ => ToneMap::Zscale,
    }
}

// split: begin attempt-child
#[path = "transcode/producer/attempt_child.rs"]
mod attempt_child;
use attempt_child::*;
// split: end attempt-child

// split: begin publication
#[path = "transcode/rolling/publication.rs"]
mod publication;
use publication::*;
// split: end publication

// split: begin session
#[path = "transcode/rolling/session.rs"]
mod session;
use session::*;
// split: end session

// split: begin response
#[path = "transcode/response.rs"]
mod response;
pub use response::*;
// split: end response

/// How long a media-origin probe may take before the session gives up on it.
///
/// This runs on the session-creation path, so it is in front of the viewer.
/// A seek plus four packets is milliseconds on any healthy source; a source
/// that cannot answer that quickly is a source whose session is about to have
/// much larger problems, and the fallback (the requested start) is exactly
/// what this code used unconditionally before.
const MEDIA_ORIGIN_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a copy session's media actually begins in the source.
///
/// See [`plurx_core::transcode::keyframe_probe_args`] for why the requested
/// start is not the answer. Every failure path returns `start_seconds`, which
/// is both the old behaviour and the correct answer whenever the requested
/// offset happens to land on a keyframe.
pub(crate) async fn probe_media_origin(source_path: &std::path::Path, start_seconds: f64) -> f64 {
    if start_seconds <= 0.0 {
        return 0.0;
    }
    #[cfg(windows)]
    let source = {
        let path = source_path.to_owned();
        let opened = tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&path)
        })
        .await;
        let Ok(Ok(source)) = opened else {
            tracing::warn!(start_seconds, "media-origin source could not be held");
            return start_seconds;
        };
        source
    };
    #[cfg(windows)]
    let input = match crate::ffmpeg::windows_source_path(&source) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(start_seconds, %error, "media-origin source path could not be resolved");
            return start_seconds;
        }
    };
    #[cfg(not(windows))]
    let input = source_path.to_owned();
    let args = plurx_core::transcode::keyframe_probe_args(&input.to_string_lossy(), start_seconds);
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    if let Err(error) = crate::ffmpeg::verify_windows_source_path(&source, &input) {
        tracing::warn!(start_seconds, %error, "media-origin source changed before probe");
        return start_seconds;
    }
    let probe = crate::process_control::output_job_owned(
        &mut command,
        crate::process_control::ChildWork::realtime("playback start media probe"),
    );
    let Ok(Ok(out)) = tokio::time::timeout(MEDIA_ORIGIN_PROBE_TIMEOUT, probe).await else {
        tracing::warn!(
            start_seconds,
            "media-origin probe did not answer; subtitle cues fall back to the requested start"
        );
        return start_seconds;
    };
    if !out.status.success() {
        tracing::warn!(
            start_seconds,
            "media-origin probe failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return start_seconds;
    }
    let origin =
        plurx_core::transcode::parse_keyframe_origin(&String::from_utf8_lossy(&out.stdout))
            // A keyframe *after* the requested start would mean the demuxer
            // seeked forward, which `-noaccurate_seek` does not do. Treat it as a
            // probe that misread rather than moving cues the wrong way.
            .filter(|origin| *origin <= start_seconds + 0.001)
            .unwrap_or(start_seconds);
    if (origin - start_seconds).abs() > 0.05 {
        tracing::info!(
            start_seconds,
            origin,
            lead_seconds = start_seconds - origin,
            "copy session begins at the preceding keyframe; subtitle cues shift by the origin"
        );
    }
    origin
}

/// A copy session must use the preceding keyframe reported on stdout, not the
/// requested seek time. Generate exactly two keyframes so the expected origin
/// is a property of this fixture rather than of an installed media file.
#[cfg(test)]
#[tokio::test]
#[ignore = "needs ffmpeg"]
async fn probe_media_origin_reads_the_preceding_keyframe() {
    plurx_core::testfixtures::require_ffmpeg();
    let directory = crate::test_tempdir().expect("media-origin fixture");
    let source = directory.path().join("two-keyframes.mp4");
    let output = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=15:duration=2",
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "15",
            "-keyint_min",
            "15",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source)
        .output()
        .expect("generate media-origin fixture");
    assert!(
        output.status.success(),
        "fixture encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requested = 1.5;
    let origin = probe_media_origin(&source, requested).await;
    assert!(
        (origin - 1.0).abs() < 0.05,
        "expected the preceding 1.0 s keyframe, got {origin}"
    );
    assert_ne!(origin, requested, "probe fell back to the requested seek");
}

// split: begin hls-codecs
#[path = "transcode/hls_codecs.rs"]
mod hls_codecs;
use hls_codecs::*;
// split: end hls-codecs

pub struct StartInfo {
    pub session_id: String,
    pub playlist_url: String,
    pub duration_ms: Option<i64>,
    pub start_seconds: f64,
    /// The source timestamp represented by player-local time zero.
    ///
    /// Copy sessions may begin at the keyframe before `start_seconds`; an
    /// accurate transcode begins at the requested position, and a cached VOD
    /// begins at zero. Clients use this value for progress and timed overlays.
    pub media_origin_seconds: f64,
    /// The resolved output height. This is the server's persisted answer for
    /// an Auto request and is repeated unchanged on an idempotent recovery.
    pub target_height: i64,
    /// The normalized route that actually created the session. A stall reopen
    /// may turn an Auto copy session into a lower transcode rung, so callers
    /// must report this answer rather than the pre-normalization request.
    pub kind: SessionKind,
    pub encoder: &'static str,
    /// The dynamic range this session's bytes carry. `StartResponse` turns it
    /// into `delivered_dynamic_range`, overriding whatever `/decision` said —
    /// a burn, a forced rung, or a refused HDR10 grade all produce a session
    /// the decision never promised.
    pub grade: OutputGrade,
    /// Served from the cache: every segment already exists, so this is a VOD
    /// asset rather than a stream being written.
    ///
    /// The player needs to know, because the difference is visible. A live
    /// session seeks by restarting the encoder somewhere else; a finished one
    /// seeks by moving `currentTime`, like direct play, in well under a second.
    /// No rolling producer recovery applies here — a segment that is late was
    /// never going to be produced faster.
    pub vod: bool,
    /// Legacy activity lifetime enforced by the registry that owns this
    /// session. A finished transcode-cache hit is seekable VOD to the client
    /// but still belongs to the 60-second rolling registry.
    pub control_lease_timeout_ms: u32,
}

// split: begin cluster-adoption
#[path = "transcode/cluster_adoption.rs"]
mod cluster_adoption;
pub(crate) use cluster_adoption::*;
// split: end cluster-adoption

// split: begin session-request
#[path = "transcode/session_request.rs"]
mod session_request;
pub use session_request::*;
// split: end session-request

// split: begin requests
#[path = "transcode/requests.rs"]
mod requests;
use requests::*;
// split: end requests

// split: begin pretranscode-source
#[path = "transcode/pretranscode/source.rs"]
mod pretranscode_source;
pub use pretranscode_source::*;
// split: end pretranscode-source

// split: begin pretranscode-renewal-tests
#[cfg(test)]
#[path = "transcode/tests/pretranscode_renewal.rs"]
mod pretranscode_renewal_tests;
// split: end pretranscode-renewal-tests

// split: begin pretranscode-parts
#[path = "transcode/pretranscode/parts.rs"]
mod pretranscode_parts;
pub use pretranscode_parts::*;
// split: end pretranscode-parts

// split: begin rate-control
#[path = "transcode/rate_control.rs"]
mod rate_control;
pub use rate_control::*;
// split: end rate-control

pub struct TranscodeManager {
    store: Arc<dyn Store>,
    work_dir: PathBuf,
    /// The VOD presentation's serving runtime (plan §2). Sessions created
    /// under `presentation:"vod"` live here rather than in `sessions`; every
    /// serving entry point dispatches to it first and falls through when the
    /// id is not one of its own.
    vod: Arc<crate::vodserve::VodServe>,
    /// Writable XDG cache inherited by ffmpeg and libraries such as fontconfig.
    runtime_cache: PathBuf,
    /// Extracted text subtitles shared with the WebVTT endpoint.
    subtitle_cache: PathBuf,
    subtitle_membership: Option<plurx_core::cluster::membership::MembershipManager>,
    subtitle_jobs: Option<Arc<crate::state::JobManager>>,
    caps: EncoderCaps,
    /// Portable decoder names inventoried from this exact ffmpeg at boot.
    decoders: Vec<String>,
    /// The diagnostic contracts this process resolved for its own FFmpeg.
    ///
    /// Handed to the manager rather than reached for, so the readiness this
    /// node publishes is computed from a value someone had to supply — and so
    /// a test can supply one. The default is the empty policy, which is the
    /// honest answer for a manager nobody told anything.
    diagnostic_policy: std::sync::Arc<crate::decoder_health::DiagnosticPolicy>,
    /// Which decoder this build was measured to select, per codec.
    ///
    /// Empty until the boot probe runs, and empty forever on a build whose
    /// codecs cannot be probed. Read only by qualified planning: naming a
    /// decoder changes an artifact's identity, and doing that for every node
    /// on the strength of a boot probe would rotate an unqualified fleet's
    /// cache for a value nothing yet enforces.
    measured_decoders: plurx_core::transcode::decoder_inventory::MeasuredDecoders,
    /// Live operator authorization for the one-shot decoder fallback. Unlike
    /// artifact qualification this changes no cache identity, so the Settings
    /// checkbox can apply immediately to attempts created after the write.
    automatic_decoder_recovery: AtomicBool,
    /// How many generation manifests this manager has published.
    ///
    /// The only durable trace a manifest leaves on a refused generation is the
    /// generation itself, and a refusal quarantines that — so without this
    /// counter no test can tell a manifest that was written and thrown away
    /// from one that was never written, and the milestone's central change
    /// would revert green.
    #[cfg(test)]
    manifests_published: std::sync::atomic::AtomicUsize,
    /// Tests that exercise receipt enforcement without installing a real
    /// diagnostic policy can explicitly force the old all-plan identity. The
    /// production path is always contract-scoped.
    #[cfg(test)]
    force_artifact_qualification: std::sync::atomic::AtomicBool,
    /// Deterministic offline coordinator outcomes. Production always enters
    /// `produce_normalized`; tests use this only to prove the durable
    /// primary-to-alternate transition without depending on host hardware.
    #[cfg(test)]
    offline_produce_script: std::sync::Mutex<std::collections::VecDeque<OfflineProduceOutcome>>,
    #[cfg(test)]
    offline_produced_recipes: std::sync::Mutex<Vec<String>>,
    /// Inject the authority-write failure at the exact primary-fault boundary.
    #[cfg(test)]
    fail_next_offline_recovery_begin: std::sync::atomic::AtomicBool,
    /// Descriptor-bound per-source decode facts, scoped to the configured
    /// FFprobe build rather than to a mutable pathname.
    decode_facts: crate::decode_facts::DecodeFactCache,
    decode_probe_identity: Option<crate::decode_facts::DecodeProbeIdentity>,
    #[cfg(test)]
    decode_source_final_identity_delay: Duration,
    /// Validated hot rate-control state. Published only after every usable
    /// family has completed its production-argument probe.
    rate_control: std::sync::RwLock<RateControlSnapshot>,
    /// The published request and its measured path coverage.
    ///
    /// Published rather than read per plan, for the same reason the rate
    /// control is: changing which paths use a different artifact name between
    /// claim and settlement would settle one identity's bytes under another's
    /// key. Each plan then intersects this stable request with its own exact
    /// measured diagnostic contract; uncovered paths keep their old identity.
    artifact_qualification: std::sync::RwLock<ArtifactQualificationReadiness>,
    /// Serializes probe → durable settings → publication. Without this, two
    /// concurrent admin PUTs can leave the store describing one request and
    /// the in-memory effective snapshot describing the other.
    rate_control_update: Mutex<()>,
    /// The tone-map graph this node proved it can run, or [`Pipeline::Cpu`]
    /// when nothing did. A per-session decision still filters it — see
    /// [`Pipeline::for_session`] — because a proven graph is a claim about the
    /// box, not about the session.
    pipeline: Pipeline,
    /// The hardware budget, and what this box has learned about its own speed.
    admissions: Admissions,
    /// Where finished transcodes live, and what identifies them here. `None`
    /// until configured, which is a real state rather than a placeholder: a
    /// node with no cache root simply always misses, and every path below is
    /// written so that a miss is the ordinary case.
    cache: Option<CacheConfig>,
    shared_cache: Option<Arc<crate::shared_cache::SharedCacheCoordinator>>,
    /// Last completed cache-filesystem capacity sample. Request and scheduler
    /// paths read only this atomic projection: `statvfs` can block forever on
    /// a hard network mount and therefore belongs to one non-accumulating
    /// background worker, never the async media-serving runtime.
    scratch_bytes_free: AtomicI64,
    /// Wall-clock completion time of the free-space sample. A previously
    /// positive sample fails closed once it is too old, including while a
    /// later `statvfs` call remains stuck on a hard mount.
    scratch_sampled_at_unix_ms: AtomicI64,
    /// Even outside publication, odd while the two sample atomics change.
    /// Readers accept bytes only when both generation reads match.
    scratch_sample_generation: AtomicU64,
    /// One authoritative charge per rolling scratch incarnation. Admission,
    /// growth, retirement conversion and release all linearize here, so a
    /// session moving between the live and retired registries can no longer
    /// be counted twice — registry membership is not part of the charge.
    scratch_ledger: Arc<crate::scratch_ledger::ScratchLedger>,
    /// The configured global ceiling, published for the writers that have to
    /// consult it outside an `async` settings read — the native copy writer
    /// asks it before every object it publishes.
    scratch_cap: Arc<AtomicI64>,
    /// Rung by a scratch writer when its allocation starts or stops waiting
    /// on a refused grant. The reaper answers with a flow evaluation.
    scratch_starved: Arc<tokio::sync::Notify>,
    /// Shared with cache housekeeping. A row can say bytes exist, but only
    /// this registry can say an HTTP session on this node is using them now.
    cache_readers: crate::cachekeep::ActiveCacheReaders,
    /// Fresh, byte-verified cache facts used by speculative placement. A hard
    /// cache mount may strand one verifier, but the semaphore and pending
    /// entry ensure offers never submit a second filesystem operation behind
    /// it. Offers fail closed until the background verdict arrives.
    cache_offer_verdicts: Arc<std::sync::Mutex<HashMap<String, CacheOfferVerdict>>>,
    cache_offer_verifier: Arc<tokio::sync::Semaphore>,
    /// Arc-owned so an accepted retirement can remove its exact Session after
    /// the initiating request, reaper, or takeover future has disappeared.
    /// The detached owner never needs to retain the complete manager merely
    /// to converge one registry entry.
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    /// Read-only object owners moved here after producer retirement. They
    /// authorize only already-promised segment/init bytes and disappear at
    /// the finite RFC removal deadline; no control or flow path reads them.
    retired_presentations: RetiredPresentations,
    /// Recently retired rolling presentations remain ambiguous for delayed
    /// fire-and-forget marker beacons after their live registry row is gone.
    recent_marker_ambiguities: RecentMarkerAmbiguities,
    /// Bounded terminal operations outlive rolling-session cleanup so an
    /// exact End can retry the original immutable durable acknowledgement
    /// after one Store-attempt window expires.
    terminal_controls: std::sync::Mutex<HashMap<String, RollingTerminalOperation>>,
    cluster_replacement_gates: Arc<ClusterReplacementGates>,
    /// Exact per-capability exclusion between late registration/resurrection
    /// and public release. Active releases retain a strong gate; an operation
    /// that began earlier retains the same gate through its final insertion,
    /// so completing the durable End cannot erase the generation change out
    /// from under a delayed adopter.
    session_release_gates: Arc<SessionReleaseGates>,
    /// Authoritative synchronous serving projection shared with AppState.
    /// Response publication never waits for the teardown watch to mirror it.
    serving_authority: crate::serving_fence::ServingAuthority,
    /// Process-local quorum serving authority. The router rejects ordinary
    /// starts before they reach the manager; this second edge closes the
    /// transition race between that check and publishing a spawned child.
    serving_ready: AtomicBool,
    /// Monotonic counterpart to `serving_ready`. Recovery may reopen the
    /// process, but it cannot erase a loss observed by a session admitted
    /// under an older generation.
    serving_loss_generation: AtomicU64,
    /// Lock-free projection for Prometheus. The session map remains the
    /// authority; every production insert/removal publishes its resulting
    /// length while holding that map's lock.
    active_session_count: Arc<AtomicUsize>,
    /// Process-local, closed-cardinality inventory of the encoder contract
    /// selected by successful session starts. These counters deliberately
    /// live beside the boot-probed caps: `/metrics` can compare what this
    /// process proved with what playback actually selected without a Store
    /// read or a request-derived label.
    codec_qualification: Arc<CodecQualificationMetrics>,
    /// Creation requests by `request_id` — reserved *before* work starts, so
    /// two concurrent creates with the same id cannot both pass the check and
    /// spawn two encoders (the check-then-act race this map used to have).
    /// Each value carries client intent, the normalized stall target, and the
    /// create state. Small by construction — a `Ready`
    /// entry is dropped as soon as its session is gone, an `InFlight` one the
    /// moment its create resolves or is abandoned, and one player instance
    /// has one in flight at a time.
    ///
    /// A `std` mutex rather than tokio's, never held across an await: the
    /// abandoned-create cleanup runs in a `Drop` impl, and `Drop` cannot
    /// await.
    requests: std::sync::Mutex<HashMap<String, RequestEntry>>,
    /// See [`ProducerTuning`]. Always the default outside tests.
    producer: ProducerTuning,
    /// Scheduled and user-requested cache producers share one writer. A queued
    /// offline request asks speculative work to stop at its next published
    /// segment boundary, then takes this gate before resuming its own claim.
    background_producer: Mutex<()>,
    offline_waiting: AtomicBool,
    /// Whether this daemon's ffmpeg can strip a Dolby Vision configuration —
    /// probed at boot ([`crate::ffmpeg::has_dovi_rpu`]).
    ///
    /// Its own field because the copy path used to derive this from the
    /// *cache config*, which happens to carry a copy of the ffmpeg version
    /// string: a node with no cache configured would answer "no dovi_rpu"
    /// whatever ffmpeg it was running, leave the DV configuration in every
    /// remux, and have browsers that cannot decode DV refuse the stream.
    dv_strippable: bool,
    /// Whether this node converts Dolby Vision Profile 7 to 8.1 in the copy
    /// pipe. Not a probe — the conversion is plurx's own code — but an
    /// operator switch, and it sits beside `dv_strippable` because the two
    /// answer the same shape of question about the same pipeline.
    dv_convertible: bool,
    /// Whether boot proved the exact software/tonemapx/software renderer used
    /// for non-backward-compatible Dolby Vision.
    dovi_reshape: bool,
    /// Whether this daemon's ffmpeg proved the HDR10 passthrough renderer at
    /// boot ([`crate::ffmpeg::has_dovi_passthrough`]). Its own field, and its
    /// own probe: the SDR reshape graph proves nothing about a PQ output.
    dovi_passthrough: bool,
    /// Whether boot also proved the P010 upload → QSV Main10 encode half.
    /// This is the measured route that makes the 2160p HDR10 rung realtime.
    dovi_passthrough_qsv: bool,
    hdr10_passthrough: bool,
    hdr10_passthrough_qsv: bool,
    dovi_proofs: std::sync::Mutex<HashMap<String, bool>>,
    /// The ahead-window limits, snapshotted ([`AHEAD_LIMITS_TTL`]).
    ///
    /// Flow control consults the limits on every segment publish and
    /// frontier advance — which used to mean three SQLite round-trips a
    /// time, through the store's one serialized connection, on the hottest
    /// control path the server has (review §2.6). The keys are API-settable
    /// with no UI knob, so a two-second staleness bound is invisible to an
    /// operator and removes the reads from the path entirely.
    cached_limits: std::sync::RwLock<Option<(Instant, AheadLimits)>>,
    /// Shortens [`PLAYLIST_WAIT_BUDGET`] for tests that need the deadline to
    /// actually elapse. Zero (always, in production) means the real budget —
    /// see [`TranscodeManager::playlist_wait`].
    playlist_wait_override_ms: std::sync::atomic::AtomicU64,
    /// Test-only rendezvous after subtitle playlist bytes and their exact
    /// owner resolve, before the composite response commits.
    #[cfg(test)]
    subtitle_playlist_commit_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only pause after authoritative publication admission but before a
    /// VOD owner await, proving loss need not reach the teardown watch first.
    #[cfg(test)]
    vod_publication_admission_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

/// Store-free, lock-free projection used by the Prometheus handler.
#[derive(Clone)]
pub(crate) struct TranscodeMetrics {
    active_sessions: Arc<AtomicUsize>,
    active_cache: crate::cachekeep::ActiveCacheMetrics,
    decode_facts: Arc<crate::decode_facts::DecodeFactMetrics>,
    caps: EncoderCaps,
    codec_qualification: Arc<CodecQualificationMetrics>,
}

const QUALIFICATION_ENCODERS: [Encoder; 5] = [
    Encoder::Software,
    Encoder::Nvenc,
    Encoder::Qsv,
    Encoder::Vaapi,
    Encoder::VideoToolbox,
];
const QUALIFICATION_GRADES: [OutputGrade; 2] = [OutputGrade::Sdr, OutputGrade::Hdr10];
const QUALIFICATION_PIPELINES: [Pipeline; 8] = [
    Pipeline::VppQsv,
    Pipeline::TonemapVaapi,
    Pipeline::Libplacebo,
    Pipeline::TonemapOpencl,
    Pipeline::DoviTonemapx,
    Pipeline::DoviPassthrough,
    Pipeline::Hdr10Passthrough,
    Pipeline::Cpu,
];

struct CodecQualificationMetrics {
    encoder_sessions: [[AtomicU64; QUALIFICATION_GRADES.len()]; QUALIFICATION_ENCODERS.len()],
    pipeline_sessions: [AtomicU64; QUALIFICATION_PIPELINES.len()],
}

impl Default for CodecQualificationMetrics {
    fn default() -> Self {
        Self {
            encoder_sessions: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            pipeline_sessions: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl CodecQualificationMetrics {
    fn encoder_slot(encoder: Encoder) -> usize {
        match encoder {
            Encoder::Software => 0,
            Encoder::Nvenc => 1,
            Encoder::Qsv => 2,
            Encoder::Vaapi => 3,
            Encoder::VideoToolbox => 4,
        }
    }

    fn grade_slot(grade: OutputGrade) -> usize {
        match grade {
            OutputGrade::Sdr => 0,
            OutputGrade::Hdr10 => 1,
        }
    }

    fn pipeline_slot(pipeline: Pipeline) -> usize {
        match pipeline {
            Pipeline::VppQsv => 0,
            Pipeline::TonemapVaapi => 1,
            Pipeline::Libplacebo => 2,
            Pipeline::TonemapOpencl => 3,
            Pipeline::DoviTonemapx => 4,
            Pipeline::DoviPassthrough => 5,
            Pipeline::Hdr10Passthrough => 6,
            Pipeline::Cpu => 7,
        }
    }

    fn record_encoder(&self, encoder: Encoder, grade: OutputGrade) {
        self.encoder_sessions[Self::encoder_slot(encoder)][Self::grade_slot(grade)]
            .fetch_add(1, Relaxed);
    }

    fn record_pipeline(&self, pipeline: Pipeline) {
        self.pipeline_sessions[Self::pipeline_slot(pipeline)].fetch_add(1, Relaxed);
    }

    fn prometheus(&self, caps: &EncoderCaps) -> String {
        let mut out = String::from(
            "# HELP plurx_encoder_available Whether boot validation test-encoded through this family.\n\
             # TYPE plurx_encoder_available gauge\n",
        );
        for encoder in QUALIFICATION_ENCODERS {
            out.push_str(&format!(
                "plurx_encoder_available{{family=\"{}\"}} {}\n",
                encoder.family_name(),
                u8::from(caps.available(encoder)),
            ));
        }
        out.push_str(
            "# HELP plurx_encoder_sessions_total Accepted encoding starts after their serving owner is registered, by family and output grade. Process-local; use reset-aware increase() or uninterrupted uptime for interval totals.\n\
             # TYPE plurx_encoder_sessions_total counter\n",
        );
        for encoder in QUALIFICATION_ENCODERS {
            for grade in QUALIFICATION_GRADES {
                out.push_str(&format!(
                    "plurx_encoder_sessions_total{{family=\"{}\",grade=\"{}\"}} {}\n",
                    encoder.family_name(),
                    grade.name(),
                    self.encoder_sessions[Self::encoder_slot(encoder)][Self::grade_slot(grade)]
                        .load(Relaxed),
                ));
            }
        }
        out.push_str(
            "# HELP plurx_tone_map_pipeline_sessions_total Accepted encoding starts after their serving owner is registered, by resolved video pipeline. Process-local; use reset-aware increase() or uninterrupted uptime for interval totals.\n\
             # TYPE plurx_tone_map_pipeline_sessions_total counter\n",
        );
        for pipeline in QUALIFICATION_PIPELINES {
            out.push_str(&format!(
                "plurx_tone_map_pipeline_sessions_total{{pipeline=\"{}\"}} {}\n",
                pipeline.name(),
                self.pipeline_sessions[Self::pipeline_slot(pipeline)].load(Relaxed),
            ));
        }
        out
    }
}

/// Bounded local facts published in the cluster media snapshot. None of these
/// fields require reading a library source path.
pub(crate) struct MediaNodeRuntime {
    pub(crate) scratch_bytes_free: u64,
    pub(crate) scratch_target_bytes: u64,
    pub(crate) active_sessions: usize,
    pub(crate) session_pressure_limit: usize,
    pub(crate) encoder_families: Vec<String>,
    pub(crate) max_target_height: i64,
    pub(crate) decoders: Vec<String>,
    pub(crate) tone_map_pipelines: Vec<String>,
    pub(crate) hardware_slots_used: usize,
    pub(crate) hardware_slots_max: usize,
    pub(crate) software_threads_used: usize,
    pub(crate) software_threads_max: usize,
    pub(crate) live_waiting: bool,
    pub(crate) background_active: bool,
}

/// Node-local answer to a diagnostics-only offer request. Calculating it does
/// not reserve capacity or open the media source.
pub(crate) struct MediaOfferProbe {
    pub(crate) active_sessions: usize,
    pub(crate) session_pressure_limit: usize,
    pub(crate) scratch_bytes_free: u64,
    pub(crate) decoder_supported: bool,
    pub(crate) target_supported: bool,
    pub(crate) cache_hit: bool,
    pub(crate) free_hardware_slots: usize,
    pub(crate) free_software_threads: usize,
    pub(crate) encoder: String,
    pub(crate) pipeline: String,
    pub(crate) recent_speed: Option<f64>,
}

impl TranscodeMetrics {
    pub(crate) fn snapshot(&self) -> (usize, usize) {
        (
            self.active_sessions.load(Relaxed),
            self.active_cache.active_entries(),
        )
    }

    pub(crate) fn decode_facts_prometheus(&self) -> String {
        self.decode_facts.prometheus()
    }

    pub(crate) fn codec_qualification_prometheus(&self) -> String {
        self.codec_qualification.prometheus(&self.caps)
    }
}

struct RollingTerminalAdmission {
    manager: Arc<TranscodeManager>,
    session_id: String,
    session: Arc<Session>,
    identity: RollingTerminalIdentity,
    terminal_committer: Option<Arc<dyn crate::playback_control::TerminalControlCommitter>>,
    #[cfg(test)]
    control_pause: Option<Arc<tokio::sync::Barrier>>,
}

impl crate::playback_control::RollingTerminalAdmission for RollingTerminalAdmission {
    fn accepted(&self, outcome: crate::playback_control::RollingControlOutcome) {
        if outcome.lease.terminal != Some(crate::playback_control::RollingTerminalCause::End) {
            return;
        }
        let (receipt, sender) = RollingTerminalResultReceipt::pending();
        let expires_at_unix_ms = Arc::new(AtomicI64::new(0));
        let exact_commit = Arc::new(std::sync::Mutex::new(None));
        let operation = RollingTerminalOperation {
            identity: self.identity.clone(),
            result: receipt,
            expires_at_unix_ms: Arc::clone(&expires_at_unix_ms),
            exact_commit: Arc::clone(&exact_commit),
        };
        let handoff = {
            let mut shared = self
                .session
                .terminal_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if shared.is_some() {
                return;
            }
            let handoff = crate::playback_control::TerminalResponseHandoff::new(Arc::clone(
                &self.session.terminal_response_pending,
            ));
            *shared = Some(operation.clone());
            handoff
        };
        self.manager
            .terminal_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(self.session_id.clone(), operation);
        let manager = Arc::clone(&self.manager);
        let session_id = self.session_id.clone();
        let session = Arc::clone(&self.session);
        let terminal_committer = self.terminal_committer.clone();
        #[cfg(test)]
        let control_pause = self.control_pause.clone();
        tokio::spawn(async move {
            let result = manager
                .finish_hls_session_control(
                    session_id,
                    session,
                    outcome.disposition,
                    outcome.accepted_sequence,
                    outcome.action,
                    outcome.action_suppressed,
                    outcome.preparation_directive,
                    outcome.platform,
                    outcome.selection,
                    outcome.lease.expires_at_unix_ms(),
                    outcome.lease.timeout_ms(),
                    outcome.flow_ticket,
                    outcome.lease.terminal.map_or(
                        "active",
                        crate::playback_control::RollingTerminalCause::status,
                    ),
                    true,
                    Some(handoff),
                    terminal_committer,
                    #[cfg(test)]
                    control_pause,
                )
                .await;
            let prepared_commit = result
                .as_ref()
                .ok()
                .and_then(|result| result.terminal_commit.as_ref())
                .cloned();
            let exact_expiry = prepared_commit
                .as_ref()
                .and_then(|commit| commit.expires_at_unix_ms())
                // Tests and embedders may intentionally omit durable commit;
                // still start their bound after result preparation, never at
                // actor admission before the immutable result exists.
                .unwrap_or_else(|| {
                    crate::media_sessions::unix_ms()
                        .saturating_add(crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS)
                });
            *exact_commit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = prepared_commit;
            expires_at_unix_ms.store(exact_expiry, Release);
            let _ = sender.send(Some(result));
        });
    }
}

// split: begin manager
#[path = "transcode/manager/cache.rs"]
mod manager_cache;
#[path = "transcode/manager/construct.rs"]
mod manager_construct;
#[path = "transcode/manager/control.rs"]
mod manager_control;
#[path = "transcode/manager/create.rs"]
mod manager_create;
#[path = "transcode/manager/describe.rs"]
mod manager_describe;
#[path = "transcode/manager/maintenance.rs"]
mod manager_maintenance;
#[path = "transcode/manager/plan.rs"]
mod manager_plan;
#[path = "transcode/manager/produce.rs"]
mod manager_produce;
#[path = "transcode/manager/publication.rs"]
mod manager_publication;
#[path = "transcode/manager/start.rs"]
mod manager_start;
// split: end manager

// split: begin retention
#[path = "transcode/rolling/retention.rs"]
mod retention;
pub(crate) use retention::*;
// split: end retention

// split: begin ladder
#[path = "transcode/ladder.rs"]
mod ladder;
pub use ladder::*;
// split: end ladder

// split: begin test-support
#[cfg(test)]
#[path = "transcode/test_support.rs"]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;
// split: end test-support

// split: begin transcode-tests
#[cfg(test)]
#[path = "transcode/tests.rs"]
pub(crate) mod tests;
// split: end transcode-tests
