//! The VOD serving runtime: sessions as handles on shared renditions.
//!
//! This is the object the transcode manager delegates every public HLS create
//! to. `presentation: "vod"` is explicit capability confirmation, while an
//! omitted presentation also means VOD; the removed live engine is never a
//! fallback. Everything M2 built dark is wired here:
//!
//! - **A `Rendition`** — one per (file identity, copy recipe) key, shared by
//!   every session on that recipe — owns a [`RenditionDir`], a [`Manifest`],
//!   the playlist bytes rendered *once* from the stored plan, the rendition's
//!   [`InitIdentity`], and a [`ProducerSlot`] driven by a per-rendition task.
//! - **Sessions are handles** (plan §2.5): auth, TTL, demand attribution, and
//!   a tombstone when they end for good. They own no media, no timeline, no
//!   playlist identity; reaping one deletes nothing.
//! - **The segment GET has exactly three outcomes** (plan §2.3): materialized
//!   bytes now; a bounded block on the [`WaitPool`]; or a typed refusal. An
//!   unknown name is the caller's 404 and nothing else's.
//! - **The producer driver** turns wait-pool and reader demand into
//!   [`prodsched::decide`](crate::prodsched::decide) →
//!   [`prodexec::next_step`](crate::prodexec::next_step) →
//!   [`ProducerSlot::perform`], and spawns real `ffmpeg` generations whose
//!   output [`vodgen::run`] cuts on the plan's own boundaries.
//!
//! The plan is normative (ledger D10): it is fetched from the store or
//! computed once with the same `CutPolicy` constants the generations cut
//! with, stored put-if-absent, and the stored copy is THE plan every node
//! serves. The playlist bytes derived from it are immutable for the life of
//! the rendition; a resurrected rendition serves the identical bytes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use plurx_core::domain::MediaFile;
use plurx_core::fmp4::{segment_name, CutPolicy, FragmentReader, Init, Unit};
use plurx_core::segplan::{
    AnnotationKind, FragmentIndex, PlanEntryKind, SegmentPlan, SourceIdentity, TrackDurations,
};
use plurx_core::store::Store;
use plurx_core::transcode::{
    copy_pipe_args_with_dolby_vision, CopyVideoOptions, Pacing, COPY_FIRST_SEGMENT_SECONDS,
    COPY_SEGMENT_MAX_BYTES, COPY_SEGMENT_MAX_SECS, COPY_SEGMENT_SECONDS,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{Mutex, Notify, Semaphore};

use crate::copyseg::sanitize_stale_dolby_brand;
use crate::ffmpeg::ffmpeg_bin;
use crate::prodexec::{next_step, yield_step, Contention, Producer, Step, Termination};
use crate::prodrun::{Performed, ProducerSlot};
use crate::prodsched::{decide, Action, Demand, Position, WorkingSet, AHEAD_HORIZON_SECONDS};
use crate::renditiondir::{InitIdentity, InitRefused, RenditionDir, INIT_NAME};
use crate::titlestore::{Budgets, Manifest, ReaderWindow, SegState};
use crate::transcode::{session_log_id, SessionKind, SessionRequest};
use crate::vodgen::{self, Failure, Generation, Outcome};
use crate::waitpool::{VodProducerKind, VodProducerTermination, WaitKey, WaitOutcome, WaitPool};

/// Sliding idle TTL for a session (plan §2.5's dormant reap, scoped to this
/// in-memory registry): a session none of whose authorized GETs have arrived
/// for this long simply vanishes from the map — no tombstone, because an
/// idle-reaped session is the one kind that may come back, and the durable
/// route machinery outside this module answers for it.
const SESSION_IDLE_TTL: Duration = Duration::from_secs(300);

/// Rolling live starts cannot notify the VOD registry. Recheck stopped
/// encoders within their five-second admission budget so they can yield.
const STOPPED_ENCODER_POLL: Duration = Duration::from_secs(1);

/// How long one refused prepared successor's request keeps its own viewer's
/// predecessor from producing. The successor refreshes it on every refused
/// retry (its driver polls every 250 ms while it waits), so it expires on its
/// own shortly after the successor either starts or goes away, and the
/// predecessor is never parked by a successor that no longer exists.
const HANDOFF_REQUEST_TTL: Duration = Duration::from_secs(2);

/// How long capacity a predecessor yielded stays reserved for its successor.
/// The successor is woken at the release and normally claims it within one
/// driver pass; the bound only matters when it has gone away.
const HANDOFF_RESERVATION_TTL: Duration = Duration::from_secs(2);

/// A prepared successor's request that this rendition's producer give its
/// encoder permit back. Bound to the exact staged successor incarnation the
/// predecessor's preparation slot names.
#[derive(Clone)]
struct HandoffRequest {
    until: Instant,
    incarnation: String,
    successor: Weak<Rendition>,
    wanted: crate::admission::TranscodeResourceEstimate,
}

/// How long a rendition with no attached sessions is kept warm before an
/// un-admitted one is purged. Admitted renditions are the completed cache and
/// are never purged here — cache retention is `cachekeep`'s job, not a TTL's.
const DORMANT_RENDITION_TTL: Duration = Duration::from_secs(1800);

/// Compact terminal owners remain available long enough for a lost 410 or
/// terminal-control acknowledgement to be retried exactly. Reader/producer/
/// media graphs are released by cleanup before this window begins; after the
/// window maintenance may evict the compact owner only when a fresh durable
/// read proves the capability is no longer active.
const TERMINAL_TOMBSTONE_RETENTION: Duration = Duration::from_secs(60);
const TERMINAL_ROUTE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one maintenance pass will wait on outstanding terminal cleanups
/// before proceeding without them.
///
/// Sized against the slowest legitimate cleanup — a reader detach that waits
/// on a subtitle ffmpeg to settle, plus a durable terminal commit — with room
/// to spare, because the cost of waiting slightly too long is one late
/// maintenance pass and the cost of not bounding it at all is a node whose
/// maintenance never runs again.
const TERMINAL_CLEANUP_MAINTENANCE_WAIT: Duration = Duration::from_secs(30);
const TERMINAL_ROUTE_CONFIRM_FANOUT: usize = 16;
/// Begin speculative marker work this many seconds of playback time before a
/// stored marker. The distance in film time is scaled by the client's current
/// playback rate, so 2x playback still gives the producer the same wall-clock
/// opportunity to warm the destination.
const MARKER_PREWARM_APPROACH_WALL_MS: i64 = 60_000;
/// One maintenance pass confirms at most one fanout wave. The cursor below
/// advances the window so a persistently unavailable route cannot starve
/// later tombstones while also preventing an unavailable Store from turning
/// one maintenance tick into minutes of serial waves.
const TERMINAL_ROUTE_CONFIRM_BATCH: usize = TERMINAL_ROUTE_CONFIRM_FANOUT;

/// A missing-init cache adoption must not own a per-key build forever merely
/// because ffmpeg never emits its muxer head. Termination and confirmed reap
/// are owned separately, so cancellation cannot orphan the child.
const HEAD_REGENERATION_TIMEOUT: Duration = Duration::from_secs(15);
/// Missing-init recovery is exceptional and process-heavy. Bound both the
/// number of ffmpeg heads alive node-wide and the bytes accepted before the
/// init atom; neither random cache damage nor a malformed muxer may create an
/// unbounded process or memory fan-out.
const HEAD_REGENERATION_CAPACITY: usize = 4;
const HEAD_REGENERATION_MAX_BYTES: usize = 8 * 1024 * 1024;

/// `Retry-After` on a deadline-expired blocking GET (plan §2.3's typed
/// `segment_pending`). The header stays on the wire even though hls.js reads
/// it only for 429 — AVPlayer and Media3 are unmeasured (§12.7).
const PENDING_RETRY_AFTER: Duration = Duration::from_secs(1);

/// Blocked GETs per session (plan §2.3, review B4). The cap is the wait
/// pool's; a refused wait answers a typed 503 immediately, no state.
const PER_SESSION_WAIT_CAP: usize = 4;

/// Blocked GETs across the node when the setting is absent.
///
/// Now the default rather than the law: `playback.vod_blocked_get_cap` sets
/// it, and `try_create` applies the current value, so an operator watching
/// `pool_full` refusals can size it for their own deployment without a
/// restart. The per-session cap stays a constant because it bounds one viewer
/// rather than the node.
const DEFAULT_GLOBAL_WAIT_CAP: usize = 64;

/// The rendition's persisted init identity, beside its `init.mp4`.
const IDENTITY_NAME: &str = "identity.json";
/// Process-generation metadata written only for encoded renditions. Copy
/// renditions deliberately have no marker, which lets startup reconcile the
/// non-reusable encoded keyspace without guessing from hashes or touching the
/// durable copy cache.
const ENCODED_PROCESS_NAME: &str = "encoded-process";
/// Bound exact Store deletes and recursive directory removals per maintenance
/// tick. Startup discovers the first batch; later ticks converge over the rest.
const ENCODED_RECONCILE_BATCH: usize = 128;
/// Sentinel in the per-entry watchdog map for the rendition init object.
const INIT_DEMAND_INDEX: u32 = u32::MAX;

// split: begin vod-discovery
#[path = "vod/discovery.rs"]
mod discovery;
use discovery::*;
// split: end vod-discovery

/// Settings snapshot the manager reads per-create.
#[derive(Debug, Clone)]
pub struct VodSettings {
    /// Node-wide byte budget for un-admitted VOD working sets. Never zero by
    /// the time it reaches here (settings validation refuses a parsed zero).
    pub working_set_bytes: u64,
    /// What `cache.max_gb` sizes the per-rendition admission threshold from.
    ///
    /// Not a bound on the completed cache. It reaches `Budgets` as
    /// `admission_sizing_bytes` and is only ever multiplied by the admission
    /// share to decide whether one rendition may be published; the node's
    /// total admitted bytes are counted and reported but never compared
    /// against anything. See `Budgets::admission_sizing_bytes`.
    pub completed_cache_bytes: u64,
    /// One hard deadline for a blocking segment GET.
    pub block_budget: Duration,
    /// From one segment's first blocked demand to bytes or typed failure.
    pub materialize_budget: Duration,
    /// Node-wide ceiling on blocked segment GETs, from
    /// `playback.vod_blocked_get_cap`.
    pub blocked_get_cap: usize,
}

/// Why a session ended for good. Every cause answers 410 and never resurrects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Deleted,
    Superseded,
    AdminStop,
    Revoked,
    Replaced,
}

impl Terminal {
    pub(crate) const fn durable_reason(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Superseded => "superseded",
            Self::AdminStop => "admin_stop",
            Self::Revoked => "revoked",
            Self::Replaced => "replaced",
        }
    }

    pub(crate) const fn control_reason(self) -> &'static str {
        match self {
            Self::Deleted => "released by client",
            Self::Superseded => "superseded by cluster session",
            Self::AdminStop => "stopped by admin",
            Self::Revoked => "credentials revoked",
            Self::Replaced => "file replaced",
        }
    }

    pub(crate) fn from_durable_reason(reason: Option<&str>) -> Option<Self> {
        match reason {
            Some("deleted") => Some(Self::Deleted),
            Some("superseded") => Some(Self::Superseded),
            Some("admin_stop") => Some(Self::AdminStop),
            Some("revoked") => Some(Self::Revoked),
            Some("replaced") => Some(Self::Replaced),
            _ => None,
        }
    }

    pub(crate) fn from_control_reason(reason: &str) -> Option<Self> {
        [
            Self::Deleted,
            Self::Superseded,
            Self::AdminStop,
            Self::Revoked,
            Self::Replaced,
        ]
        .into_iter()
        .find(|terminal| terminal.control_reason() == reason)
    }
}

/// The create answer plus the request facts an idempotent replay echoes.
#[derive(Debug)]
pub struct RecoveredVod {
    pub start: VodStart,
    pub target_height: i64,
    pub kind: SessionKind,
}

/// The facts a stall-reopen's normalization checks against its predecessor.
#[derive(Debug, Clone)]
pub struct ReopenFacts {
    pub supersession_user: String,
    pub playback_id: String,
    pub file_id: i64,
}

/// Source facts needed to wrap a VOD media playlist in the existing native
/// subtitle master without pretending it is a live transcode session.
#[derive(Debug, Clone)]
pub struct VodHlsFacts {
    pub file: MediaFile,
    pub audio_index: Option<i64>,
    pub aac: bool,
    pub preserve_dolby_vision: bool,
    /// Whether this session's copy rewrites Profile 7 RPUs to 8.1. Carried
    /// beside the preservation because the playlist has to describe what the
    /// copy produces — `hvc1` plus `SUPPLEMENTAL-CODECS: dvh1.08.LL` — and
    /// the source's own Dolby Vision record says profile 7.
    pub convert_dolby_vision: bool,
    pub(crate) encoding: Option<Arc<crate::vodencode::Encoding>>,
    pub(crate) response_owner: ResponseOwner,
}

/// Opaque identity of one VOD session incarnation. A response resolved from
/// an old rendition cannot commit liveness or frontier state to a resurrected
/// session that reused the durable session id.
#[derive(Clone)]
pub(crate) struct ResponseOwner {
    lifecycle: Arc<Mutex<()>>,
    incarnation: Arc<()>,
    /// Present only for a live media owner. Terminal owners deliberately do
    /// not keep the heavyweight rendition graph alive after reader/commit
    /// cleanup has finished.
    rendition: Option<Arc<Rendition>>,
    rendition_key: String,
    file: Arc<MediaFile>,
    /// Terminal snapshot at resolution. Status publication compares this
    /// exact value so a live error cannot be admitted after tombstoning and a
    /// tombstone from one incarnation cannot describe its replacement.
    tombstone: Option<Terminal>,
}

impl std::fmt::Debug for ResponseOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponseOwner")
            .field("incarnation", &Arc::as_ptr(&self.incarnation))
            .field("rendition_key", &self.rendition_key)
            .field("tombstone", &self.tombstone)
            .finish_non_exhaustive()
    }
}

/// One VOD lookup result bound to the exact session incarnation that produced
/// it. Errors deliberately carry the same owner as bytes and misses; HTTP must
/// authorize this snapshot before publishing any status.
#[derive(Debug)]
pub struct VodPublication<T> {
    pub result: Result<T, VodError>,
    pub(crate) owner: ResponseOwner,
}

#[cfg(test)]
impl<T> VodPublication<T> {
    fn is_ok(&self) -> bool {
        self.result.is_ok()
    }

    fn expect(self, message: &str) -> (T, ResponseOwner) {
        (self.result.expect(message), self.owner)
    }
}

/// Live diagnostics for one VOD session. This is intentionally not the live
/// transcode [`SessionInfo`](crate::transcode::SessionInfo): an immutable VOD
/// playlist has no encode speed or sliding publish gate, and inventing those
/// numbers would make the player diagnose the wrong system.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VodSessionInfo {
    pub id: String,
    pub file_id: i64,
    pub target_height: i64,
    pub encoder: &'static str,
    /// Explicit CPU tone-map peak and its source, absent for copy and GPU
    /// renditions. `default` is policy provenance, not source metadata.
    pub tone_map_peak_nits: Option<u32>,
    pub tone_map_peak_source: Option<&'static str>,
    pub playlist_shape: &'static str,
    pub producer_state: &'static str,
    pub producer_hold: Option<&'static str>,
    pub producer_failed: Option<String>,
    /// The bounded class of that failure, in the same vocabulary rolling
    /// delivery publishes. `producer_failed` is the sentence an operator
    /// reads; this is the only part a client can act on, because
    /// `producer_state` flattens every failure to the word `failed`.
    pub producer_decision: Option<&'static str>,
    pub control_demand: Option<&'static str>,
    pub reported_position_ms: Option<i64>,
    pub client_runway_ms: Option<i64>,
    pub render_state: Option<&'static str>,
    /// Playhead-anchored, contiguous materialized media. This is deliberately
    /// separate from the fetch-anchored compatibility frontier below.
    pub server_ready_state: &'static str,
    pub server_ready_anchor_ms: Option<i64>,
    pub server_ready_end_ms: Option<i64>,
    pub server_ready_seconds: Option<f64>,
    pub server_next_ready_start_ms: Option<i64>,
    pub server_next_ready_end_ms: Option<i64>,
    pub published_end_ms: Option<i64>,
    /// The end of the contiguous materialized run measured from the segment
    /// this client was last served, rather than from segment 0.
    ///
    /// A far seek materializes segments past a hole, so `published_end_ms` can
    /// sit behind the playhead while there is real media ahead of the client.
    /// This is the frontier a stalled client could actually fetch from, and it
    /// is what the control plane reads. `published_end_ms` keeps its own
    /// meaning — how much of the title exists — because the activity page asks
    /// that question, and it is a different one.
    pub ready_ahead_end_ms: Option<i64>,
    pub fetched_end_ms: i64,
    pub fetched_segment: Option<i64>,
    pub ahead_seconds: Option<i64>,
    pub materialized_segments: usize,
    pub planned_segments: usize,
    pub materialized_bytes: u64,
    pub planned_bytes: u64,
    pub working_set_bytes: u64,
    pub working_set_budget_bytes: u64,
    pub completed_cache_bytes: u64,
    pub admitted: bool,
    /// Everything this viewer has actually been handed, and how fast.
    ///
    /// `delivered_bps` is `None` until a measurement window has closed, and
    /// stays `None` rather than being filled from the encoder's configured
    /// target: what an encoder was *asked* to produce is not what a link
    /// carried, and on VOD the presentation is a stream copy, so the target
    /// describes nothing that happened. Unknown must stay unknown so advisory
    /// fleet evidence never reports a fabricated link measurement.
    pub delivered_bytes: i64,
    /// Bits per second, to match the rolling view's units.
    pub delivered_bps: Option<i64>,
    /// How long since the rate was last recomputed, which is what tells a
    /// stale rate from a dead link.
    ///
    /// **Deliberately not serialized.** This struct is the body of
    /// `/hls/{session}/status`, and Apple's `DeliveryStarvationDetector`
    /// refuses to fire unless `delivered_idle_ms` is present and at least
    /// sixteen seconds. VOD has never carried the key, so that detector has
    /// never been armed on the primary presentation; publishing it here would
    /// turn on an automatic session reopen for every VOD viewer as a side
    /// effect of adding a measurement. That may well be the right thing — the
    /// detector was written for copy-HLS — but it is a client-behaviour change
    /// that needs its own evidence on hardware, not a rider on this one. The
    /// field is read in-process by `DeliveryView::from_status`, which is what
    /// needs it.
    #[serde(skip)]
    pub delivered_idle_ms: i64,
    pub http_wait_count: usize,
    pub http_wait_oldest_ms: Option<i64>,
    pub http_wait_segment: Option<i64>,
    pub status_generated_unix_ms: i64,
    pub suspended: bool,
    #[serde(rename = "final")]
    pub final_: bool,
}

/// The identity/lifetime slice needed by the shared activity inventory.
/// Producer diagnostics stay in [`VodSessionInfo`]; this shape deliberately
/// contains only facts that can be read without taking a rendition manifest
/// lock for every open activity page.
#[derive(Debug, Clone)]
pub struct VodDeliveryInfo {
    pub id: String,
    pub method: crate::delivery::Method,
    pub file_id: i64,
    pub item_id: i64,
    pub item_title: String,
    pub user_name: String,
    pub target_height: i64,
    pub started_unix: i64,
    pub idle_seconds: u64,
    /// What this delivery has actually handed the viewer.
    ///
    /// Carried so the activity view can say what a VOD session is costing.
    /// It previously reported a hard-coded zero with a delivery idle age
    /// derived from the session's last touch, which reads as a measurement
    /// and is not one — a handle that had been touched a second ago looked
    /// busy whether or not a single byte had moved.
    pub delivered_bytes: i64,
    pub delivered_bps: Option<i64>,
    pub delivered_idle_ms: i64,
}

/// What [`VodServe::try_create`] answers when the VOD presentation can serve
/// this request.
#[derive(Debug)]
pub struct VodStart {
    pub session_id: String,
    pub duration_ms: i64,
}

/// The durable request plus an already resolved encoder recipe. Plain copy
/// callers need no encoder preparation and convert from their request alone.
pub(crate) struct VodRecipeRequest<'a> {
    pub request: &'a SessionRequest,
    pub encoding: Option<Arc<crate::vodencode::Encoding>>,
}

impl<'a> From<&'a SessionRequest> for VodRecipeRequest<'a> {
    fn from(request: &'a SessionRequest) -> Self {
        Self {
            request,
            encoding: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VodAttribution<'a> {
    pub user_name: &'a str,
    pub item_title: &'a str,
    pub supersession_user: &'a str,
}

/// A cluster start's synchronous serving admission. The generation is
/// captured before any slow VOD preparation begins and is checked again at
/// the final reader/registry attachment, so a loss followed by recovery
/// cannot let an old admission publish new media.
pub(crate) struct VodServingAdmission {
    authority: crate::serving_fence::ServingAuthority,
    generation: u64,
    deadline: Instant,
    #[cfg(test)]
    pause_before_commit: Option<Arc<tokio::sync::Barrier>>,
}

struct VodCreateFences<'a> {
    release_fence: Option<VodReleaseFence<'a>>,
    serving_admission: Option<VodServingAdmission>,
}

impl VodServingAdmission {
    pub(crate) fn new(
        authority: crate::serving_fence::ServingAuthority,
        generation: u64,
        deadline: Instant,
    ) -> Self {
        Self {
            authority,
            generation,
            deadline,
            #[cfg(test)]
            pause_before_commit: None,
        }
    }

    #[cfg(test)]
    fn with_pause_before_commit(mut self, pause: Arc<tokio::sync::Barrier>) -> Self {
        self.pause_before_commit = Some(pause);
        self
    }

    async fn commit_guard_before(&self) -> Option<tokio::sync::OwnedRwLockReadGuard<()>> {
        #[cfg(test)]
        if let Some(pause) = self.pause_before_commit.as_ref() {
            pause.wait().await;
            pause.wait().await;
        }
        self.authority
            .commit_guard_before(self.generation, self.deadline)
            .await
    }
}

/// The exact public-release generation that must remain closed across a slow
/// VOD resurrection's final registry attachment.
pub(crate) struct VodReleaseFence<'a> {
    transition: Arc<tokio::sync::Mutex<()>>,
    released: &'a AtomicBool,
}

impl<'a> VodReleaseFence<'a> {
    pub(crate) fn new(transition: Arc<tokio::sync::Mutex<()>>, released: &'a AtomicBool) -> Self {
        Self {
            transition,
            released,
        }
    }
}

/// Playlist / segment answers. `None` from any method = "not a VOD session,
/// fall through to the legacy path" — the dispatch contract.
#[derive(Debug)]
pub enum VodError {
    /// Typed `segment_pending` 503.
    Pending { retry_after: Duration },
    /// Typed `producer_failed` 502.
    ProducerFailed(String),
    /// 410 — the session ended for good.
    Gone(Terminal),
    /// A wait-pool cap was hit → typed 503, no wait.
    ///
    /// It carries *which* cap. The two mean different things to the client
    /// holding the refusal — one says this player is asking for too much at
    /// once, the other says the node is out of parked-request capacity — and
    /// collapsing them here is what left an operator unable to tell a single
    /// seek storm from a node whose ceiling is sized for a smaller deployment.
    Busy(crate::waitpool::WaitRefused),
    /// 500.
    Io(std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VodSupersedeError {
    Deadline,
}

/// An open, verified segment ready to stream.
#[derive(Debug)]
pub struct SegmentReady {
    pub file: tokio::fs::File,
    pub len: u64,
    /// Strong: rendition key + plan index + materialization instant + length.
    pub etag: String,
    /// The meter of the session this answer was opened for.
    ///
    /// Carried on the answer rather than looked up by the response, and not
    /// optional, so a segment cannot be served without something to count it
    /// with. The delivery rate remains advisory preparation and fleet
    /// telemetry; a body served against no meter would silently lose it.
    pub delivery: Arc<crate::meter::Meter>,
}

// split: begin vod-reader
#[path = "vod/reader.rs"]
mod reader;
use reader::*;
// split: end vod-reader

// split: begin vod-marker-prewarm
#[path = "vod/marker_prewarm.rs"]
mod marker_prewarm;
use marker_prewarm::*;
// split: end vod-marker-prewarm

// split: begin vod-session
#[path = "vod/session.rs"]
mod session;
pub(crate) use session::*;
// split: end vod-session

/// Everything the tasks share. `VodServe` is a thin handle over this so the
/// per-rendition driver and generation tasks can be `'static`.
struct Shared {
    base: PathBuf,
    runtime_cache: PathBuf,
    store: Arc<dyn Store>,
    /// Startup-discovered obsolete encoded generations. The directory remains
    /// present until its exact plan row is deleted, so cancellation or Store
    /// failure remains rediscoverable after either a later tick or restart.
    obsolete_encoded_generations: Mutex<VecDeque<ObsoleteEncodedGeneration>>,
    /// A live directory iterator makes discovery itself bounded, not merely
    /// deletion. It advances across ticks and begins a fresh pass at EOF.
    encoded_generation_scanner: Arc<StdMutex<EncodedGenerationScanner>>,
    cluster_node_id: Option<String>,
    cluster_index_root: Option<PathBuf>,
    cluster_membership: Option<plurx_core::cluster::membership::MembershipManager>,
    sessions: Mutex<HashMap<String, Session>>,
    /// Bounded by in-flight VOD request admission and cleaned by exact RAII.
    /// This closes lease loss against a slow resurrection before attachment.
    preparing_sessions: StdMutex<HashMap<String, usize>>,
    /// Per-session-id transition gates survive the remove/reattach gap via a
    /// weak registry. A reaper holds the strong gate through reader detach;
    /// resurrection upgrades the same gate before it can attach a successor.
    session_lifecycles: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    /// Per-rendition single-flight gates. Slow Store/filesystem/ffmpeg build
    /// work owns only its key; the node-wide rendition registry is held solely
    /// for short lookup/install/remove transactions.
    rendition_builds: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    renditions: Mutex<HashMap<String, Arc<Rendition>>>,
    head_regeneration_slots: Arc<Semaphore>,
    pool: WaitPool,
    /// Node-wide un-admitted materialized bytes — `prodsched`'s working set.
    working_set: AtomicU64,
    /// Bytes of admitted renditions, moved here from the working set at
    /// completion.
    completed_cache: AtomicU64,
    /// Fair starting point for the bounded terminal route-confirmation batch.
    terminal_eviction_cursor: AtomicU64,
    /// Test-only rendezvous immediately before an exact terminal replay joins
    /// the session-owned detach fence.
    #[cfg(test)]
    terminal_replay_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    #[cfg(test)]
    control_applied_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    #[cfg(test)]
    segment_ready_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous immediately before terminal reader detach.
    #[cfg(test)]
    terminal_detach_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after a newly built rendition is installed and
    /// accounted, while its exact build gate is still held.
    #[cfg(test)]
    rendition_install_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after exact dormant map removal has transferred
    /// every cleanup resource to its detached settlement owner.
    #[cfg(test)]
    dormant_purge_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    #[cfg(test)]
    terminal_route_test_outcomes: StdMutex<HashMap<String, TerminalRouteTestOutcome>>,
}

pub struct VodServe {
    shared: Arc<Shared>,
}

fn repair_job_for_artifact(
    mut repair: plurx_core::store::NewClusterFragmentIndexJob,
    artifact: &plurx_core::store::ClusterFragmentIndexArtifact,
) -> plurx_core::store::NewClusterFragmentIndexJob {
    // Serving resolves a logical key through the durable head. Repair must
    // rebuild that resolved immutable generation, not the pre-resolution
    // logical key, or the head would continue pointing at an unavailable blob.
    repair.cache_key.clone_from(&artifact.cache_key);
    repair
}

// split: begin vod-serve
#[path = "vod/serve/construct.rs"]
mod vod_serve_construct;
#[path = "vod/serve/control.rs"]
mod vod_serve_control;
#[path = "vod/serve/create.rs"]
mod vod_serve_create;
#[path = "vod/serve/delivery.rs"]
mod vod_serve_delivery;
#[path = "vod/serve/end.rs"]
mod vod_serve_end;
#[path = "vod/serve/serve.rs"]
mod vod_serve_serve;
// split: end vod-serve

// split: begin vod-shared
#[path = "vod/shared.rs"]
mod shared;
// split: end vod-shared

// split: begin vod-driver
#[path = "vod/driver.rs"]
mod driver;
use driver::*;
// split: end vod-driver

// split: begin vod-marker-dispatch
#[path = "vod/marker_dispatch.rs"]
mod marker_dispatch;
use marker_dispatch::*;
// split: end vod-marker-dispatch

// split: begin vod-generation
#[path = "vod/generation.rs"]
mod generation;
use generation::*;
// split: end vod-generation

// split: begin vod-plan
#[path = "vod/plan.rs"]
mod plan;
use plan::*;
// split: end vod-plan

// split: begin vod-init
#[path = "vod/init.rs"]
mod init;
use init::*;
// split: end vod-init

// split: begin vod-tests
#[cfg(test)]
#[path = "vod/tests.rs"]
mod tests;
// split: end vod-tests
