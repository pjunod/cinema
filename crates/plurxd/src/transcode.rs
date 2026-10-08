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
use tokio::sync::Mutex;

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
/// Physical heavy-background admission, shared by all durable media workers.
pub(crate) const BACKGROUND_HEAVY_LIMIT: usize = 1;
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

// split: begin planning
#[path = "transcode/planning.rs"]
mod planning;
#[cfg(test)]
pub(crate) use planning::unverified_hevc_copy_enabled;
use planning::*;
// split: end planning

pub(crate) const ADMISSION_POLL: Duration = Duration::from_millis(250);
const SCRATCH_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
const SCRATCH_SAMPLE_MAX_AGE: Duration = Duration::from_secs(45);
const CACHE_OFFER_VERDICT_TTL: Duration = Duration::from_secs(30);
const MAX_CACHE_OFFER_VERDICTS: usize = 256;
const MAX_CLUSTER_REPLACEMENT_GATES: usize = 4_096;
const CLUSTER_REPLACEMENT_GATE_WAIT: Duration = Duration::from_secs(3);
const MARKER_AMBIGUITY_TTL: Duration = Duration::from_secs(60);
const MAX_MARKER_AMBIGUITIES: usize = 4_096;
type RecentMarkerAmbiguities = Arc<std::sync::Mutex<RecentMarkerAmbiguityLedger>>;
const SHARED_LOOKUP_PIN_MS: i64 = 30_000;
/// One retained owner per actor-managed generation is sufficient, but the
/// global bound also protects the process when many request futures vanish
/// while their first-media commands are still queued.
const MAX_FIRST_MEDIA_SETTLEMENT_OWNERS: usize = 256;
const ROLLING_SCRATCH_CLEANUP_ATTEMPT: Duration = Duration::from_secs(5);
const ROLLING_SCRATCH_CLEANUP_RETRY: Duration = Duration::from_secs(5);
const ROLLING_SCRATCH_CLEANUP_ATTEMPTS: usize = 3;

// split: begin session-log
#[path = "transcode/session_log.rs"]
mod session_log;
pub(crate) use session_log::*;
// split: end session-log

// split: begin errors
#[path = "transcode/errors.rs"]
mod errors;
pub(crate) use errors::*;
// split: end errors

/// Stable classification for a start this ffmpeg build cannot perform at all.
///
/// Separate from a capacity error because the answers differ: capacity says
/// "try again", this says "this will never work here, and here is what is
/// missing". Both exist because the alternative — an unclassified `String` —
/// reaches the client as `ApiError::Internal`, which deliberately drops its
/// message. A refusal nobody can read is the anonymous failure this change
/// exists to remove.
const UNSUPPORTED_BUILD_PREFIX: &str = "this server's media tools cannot do that: ";

/// Stable classification for a malformed or stale bound reopen. The HTTP
/// layer returns this as a client error without exposing unrelated internals.
const INVALID_REOPEN_PREFIX: &str = "invalid stall reopen: ";

/// Stable classification for a request that cannot receive the immutable VOD
/// presentation. The live HLS presentation is not a recovery path: callers
/// must receive this verdict and either wait for the named prerequisite or
/// show the honest unsupported result.
const VOD_REFUSAL_PREFIX: &str = "vod presentation unavailable: ";
const CACHED_MEDIA_INTEGRITY_FAILURE: &str = "cached media failed an integrity check";

/// The class of what `encoder_and_grade_for` starts: every caller of it is a
/// session start (VOD or streamed) or a peer's media offer for one.
const SESSION_START_CLASS: crate::process_control::ChildClass =
    crate::process_control::ChildClass::Realtime;

/// The held source probe a VOD start runs under `ENGINE_PROBE_TIMEOUT`. A
/// viewer waits on it and a miss answers `vod_source_rescan_required`, so it
/// is realtime (plan P-02 §3.2.2).
const VOD_START_HELD_PROBE: crate::process_control::ChildWork =
    crate::process_control::ChildWork::realtime("held source probe for a session start");

/// What "Auto" resolves to — see [`TranscodeManager::auto_height`].
const AUTO_SOFTWARE_HEIGHT: i64 = 720;
/// Safe fallback when the source has no usable geometry, and the highest HDR
/// output this node's production probe has actually exercised. Known SDR
/// sources may follow validated hardware encoders to [`MAX_HEIGHT`].
const AUTO_HARDWARE_PROBED_HEIGHT: i64 = 1080;
/// Floor for any requested rung. Below this there is no picture worth the
/// session; the adaptive ladder itself bottoms out here.
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

// split: begin http-wait
#[path = "transcode/rolling/http_wait.rs"]
mod http_wait;
use http_wait::*;
// split: end http-wait

/// Below this sustained storage rate, a material segment read is a stall.
/// Normalizing by bytes keeps the signal comparable when body read buffers
/// change size.
const SEGMENT_STALL_BYTES_PER_SECOND: f64 = (1024 * 1024) as f64;

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

// split: begin live-recovery
#[path = "transcode/live_recovery.rs"]
mod live_recovery;
pub(crate) use live_recovery::*;
// split: end live-recovery

/// The reason labels, in `live_recovery_snapshot()` order.
pub(crate) const LIVE_RECOVERY_LABELS: [&str; 5] = LiveRecoveryReason::LABELS;

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

// split: begin progress
#[path = "transcode/producer/progress.rs"]
mod progress;
pub use progress::*;
// split: end progress

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

// split: begin media-origin
#[path = "transcode/media_origin.rs"]
mod media_origin;
pub(crate) use media_origin::*;
// split: end media-origin

// split: begin hls-codecs
#[path = "transcode/hls_codecs.rs"]
mod hls_codecs;
use hls_codecs::*;
// split: end hls-codecs

#[allow(dead_code)] // Finite owned actor API; ordinary Shared ingress remains closed.
#[path = "transcode/source_actor.rs"]
pub(crate) mod source_actor;
pub(crate) mod source_preparation;
mod source_subtitles;

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
    #[allow(dead_code)] // The private Source HTTP actor consumer is being integrated.
    source_workers: source_actor::SourceWorkerRegistry,
    pub(crate) source_http_starts: crate::http::shared_source_playback::SourceStartRegistry,
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
    /// Saved operator choice; captured with one node-local compatibility
    /// report for each new immutable plan, never used as a probe gate.
    macos_video_processing_enabled: Arc<AtomicBool>,
    /// Independent operator choice for newly negotiated HEVC output.
    macos_hevc_output_enabled: AtomicBool,
    /// Serialize durable preference writes and replicated reload publication;
    /// a stale read cannot overwrite a just-saved hot value.
    macos_video_preference_update: Mutex<()>,
    macos_video_probe: Arc<crate::macos_video::MacosVideoProbe>,
    /// The manager's test points (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8), in
    /// every build. Production never fills the slot, so every point reads
    /// [`NoopTranscodeManagerHooks`] (Decision D-M8-I).
    hooks: crate::seam_hooks::HookSlot<dyn TranscodeManagerHooks>,
    /// Descriptor-bound per-source decode facts, scoped to the configured
    /// FFprobe build rather than to a mutable pathname.
    decode_facts: crate::decode_facts::DecodeFactCache,
    decode_probe_identity: Option<crate::decode_facts::DecodeProbeIdentity>,
    candidate_production_proofs: Arc<crate::vodencode::CandidateProductionProofs>,
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
    /// Saved node choice, activated with its tone-map graph at startup.
    encoder_override: Option<String>,
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
    /// Monotonic counterpart to `serving_ready`: the generation the fence
    /// loop last resolved. Recovery may reopen the process, but registrations
    /// admitted under an older generation still refuse themselves.
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
    /// Shared by every heavy durable worker, independently of CPU/GPU cost.
    /// The owned guard follows the physical child through cancellation/join.
    background_heavy: Arc<tokio::sync::Semaphore>,
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
    hdr10_passthrough_vaapi: bool,
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
    /// Complete-output queue publications handed off by VOD starts. Create
    /// never waits on the catalog rebuild, source fences or replicated
    /// enqueue; [`TranscodeManager::output_enqueue_loop`] owns them.
    output_enqueue: OutputEnqueueQueue,
}

/// Starts that may hand off before the owned worker drains. Beyond this a
/// start does not queue its preparation; the title's next start offers it.
const OUTPUT_ENQUEUE_CAPACITY: usize = 64;

/// One complete-output queue publication handed off by a VOD start.
pub(crate) struct OutputEnqueue {
    request: SessionRequest,
    file: plurx_core::domain::MediaFile,
    settings: crate::vodserve::VodSettings,
    encoding: Option<Arc<crate::vodencode::Encoding>>,
    session_id: String,
    queued_at: Instant,
}

/// Bounded hand-off from session create to the single owned enqueue worker.
pub(crate) struct OutputEnqueueQueue {
    sender: tokio::sync::mpsc::Sender<OutputEnqueue>,
    receiver: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<OutputEnqueue>>>,
}

impl OutputEnqueueQueue {
    fn new() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(OUTPUT_ENQUEUE_CAPACITY);
        Self {
            sender,
            receiver: std::sync::Mutex::new(Some(receiver)),
        }
    }
}

// split: begin manager-hooks
#[path = "transcode/manager_hooks.rs"]
mod manager_hooks;
pub(crate) use manager_hooks::*;
// split: end manager-hooks

// split: begin metrics
#[path = "transcode/metrics.rs"]
mod metrics;
pub(crate) use metrics::*;
// split: end metrics

const QUALIFICATION_ENCODERS: [Encoder; 5] = [
    Encoder::Software,
    Encoder::Nvenc,
    Encoder::Qsv,
    Encoder::Vaapi,
    Encoder::VideoToolbox,
];
const QUALIFICATION_GRADES: [OutputGrade; 2] = [OutputGrade::Sdr, OutputGrade::Hdr10];
const QUALIFICATION_PIPELINES: [Pipeline; 12] = [
    Pipeline::VppQsv,
    Pipeline::TonemapVaapi,
    Pipeline::Libplacebo,
    Pipeline::TonemapOpencl,
    Pipeline::DoviTonemapx,
    Pipeline::DoviPassthrough,
    Pipeline::Hdr10Passthrough,
    Pipeline::Cpu,
    Pipeline::LibplaceboVaapi,
    Pipeline::TonemapCuda,
    Pipeline::VtScaleSdr,
    Pipeline::VtToneMapMetal,
];

// split: begin terminal-admission
#[path = "transcode/rolling/terminal_admission.rs"]
mod terminal_admission;
use terminal_admission::*;
// split: end terminal-admission

/// Rolling retention as this node sees it, for the Developer card.
pub(crate) struct RollingRetentionFacts {
    pub(crate) same_filesystem: Option<bool>,
    pub(crate) slack: Option<i64>,
    pub(crate) admits: Result<(), &'static str>,
    pub(crate) rows: Vec<crate::vodserve::retained::RetainedOutputRow>,
    pub(crate) cleanup_pending: usize,
}

// split: begin manager
#[path = "transcode/manager/cache.rs"]
mod manager_cache;
#[path = "transcode/manager/candidates.rs"]
mod manager_candidates;
pub(crate) use manager_candidates::QUALITY_PLANNING_KEYS;
#[path = "transcode/content_encoding.rs"]
mod content_encoding;
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
#[path = "transcode/manager/rolling_retained.rs"]
mod manager_rolling_retained;
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
