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

use std::collections::HashMap;
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
use crate::prodexec::{next_step, Producer, Step, Termination};
use crate::prodrun::{Performed, ProducerSlot};
use crate::prodsched::{decide, Action, Demand, Position, WorkingSet, AHEAD_HORIZON_SECONDS};
use crate::renditiondir::{InitIdentity, InitRefused, RenditionDir, INIT_NAME};
use crate::titlestore::{Budgets, Manifest, ReaderWindow, SegState};
use crate::transcode::{session_log_id, SessionKind, SessionRequest};
use crate::vodgen::{self, Failure, Generation, Outcome};
use crate::waitpool::{WaitKey, WaitOutcome, WaitPool};

/// Sliding idle TTL for a session (plan §2.5's dormant reap, scoped to this
/// in-memory registry): a session none of whose authorized GETs have arrived
/// for this long simply vanishes from the map — no tombstone, because an
/// idle-reaped session is the one kind that may come back, and the durable
/// route machinery outside this module answers for it.
const SESSION_IDLE_TTL: Duration = Duration::from_secs(300);

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
/// Sentinel in the per-entry watchdog map for the rendition init object.
const INIT_DEMAND_INDEX: u32 = u32::MAX;

/// Settings snapshot the manager reads per-create.
#[derive(Debug, Clone)]
pub struct VodSettings {
    /// Node-wide byte budget for un-admitted VOD working sets. Never zero by
    /// the time it reaches here (settings validation refuses a parsed zero).
    pub working_set_bytes: u64,
    /// Budget for admitted (completed) renditions kept as cache.
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
// TODO(m3-wire): the allow comes out when the stall-reopen wiring lands.
#[allow(dead_code)]
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
    pub playlist_shape: &'static str,
    pub producer_state: &'static str,
    pub producer_hold: Option<&'static str>,
    pub producer_failed: Option<String>,
    /// The bounded class of that failure, in the same vocabulary rolling
    /// delivery publishes. `producer_failed` is the sentence an operator
    /// reads; this is the only part a client can act on, because
    /// `producer_state` flattens every failure to the word `failed`.
    pub producer_decision: Option<&'static str>,
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
    /// describes nothing that happened. Unknown must stay unknown — a
    /// substituted number would let `headroom_refusal` pass a client whose
    /// link was never measured.
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
    /// with. The delivery rate this feeds is what
    /// `PreparationConditions::headroom_refusal` reads; a body served against
    /// no meter would silently reintroduce `throughput_unreported`.
    pub delivery: Arc<crate::meter::Meter>,
}

/// The copy recipe one rendition serves, minus `start_seconds` — exactly the
/// cache's key discipline (plan §2.4).
#[derive(Debug, Clone)]
struct Recipe {
    file: MediaFile,
    audio_index: Option<i64>,
    aac: bool,
    video: CopyVideoOptions,
    /// Exact object version whose complete digest selected the cluster blob.
    /// None on the legacy node-local index path.
    source_object_version: Option<String>,
    /// Content/pipeline identity selected by the v2 catalog. This becomes part
    /// of the rendition directory key so weak legacy metadata cannot alias
    /// segments across an in-place source rewrite.
    cluster_cache_key: Option<String>,
}

/// One attached reader, in plan indexes.
#[derive(Debug, Clone)]
struct Reader {
    /// Current playback anchor, owned by accepted control once available.
    /// Before control arrives, successful media commits are the fallback.
    frontier: u32,
    control_sequence: Option<u64>,
    /// The last segment actually served. Telemetry only: the observed runway
    /// the status publishes is measured from here, while both the eviction
    /// window and the production target are anchored on `frontier`, which
    /// accepted control owns. Reading this as the playhead would put a
    /// viewer's protected range back where their bytes came from rather than
    /// where they are now.
    last_served: Option<u32>,
    /// Per-playback provenance for speculative marker production. This is
    /// deliberately separate from the ordinary reader frontier: moving the
    /// frontier to a marker would make speculative work outrank the playhead.
    marker_prewarm: Arc<StdMutex<MarkerPrewarmLedger>>,
}

impl Reader {
    fn new(frontier: u32) -> Self {
        Self {
            frontier,
            control_sequence: None,
            last_served: None,
            marker_prewarm: Arc::new(StdMutex::new(MarkerPrewarmLedger::default())),
        }
    }

    fn accept_control(&mut self, sequence: u64, index: u32) {
        // ControlState already accepted the complete owner epoch/sequence.
        // A new owner's sequence one must replace the old owner's sequence N.
        self.control_sequence = Some(sequence);
        self.frontier = index;
    }

    fn served(&mut self, index: u32) {
        self.last_served = Some(index);
        if self.control_sequence.is_none() {
            self.frontier = index;
        }
    }
}

/// Declared before a registered wait so cancellation first releases the wait
/// and its file pin, then wakes the scheduler to observe that exact removal.
struct DemandWake(Arc<Rendition>);

impl Drop for DemandWake {
    fn drop(&mut self) {
        self.0.kick();
    }
}

struct MaterializeClock {
    started: Instant,
    owners: usize,
    retry_pending: bool,
    watchdog: Option<tokio::task::AbortHandle>,
}

impl Drop for MaterializeClock {
    fn drop(&mut self) {
        if let Some(watchdog) = &self.watchdog {
            watchdog.abort();
        }
    }
}

/// HTTP timeout may transfer the deadline to a retry; cancellation may not.
/// The last cancelled owner removes the clock synchronously, fencing its
/// sleeping watchdog from a later request for the same entry.
struct MaterializeDemand {
    rendition: Arc<Rendition>,
    index: u32,
    started: Instant,
}

impl MaterializeDemand {
    fn retry_pending(&self) {
        let mut demands = self.rendition.demand_since.lock().expect("demand lock");
        if let Some(clock) = demands
            .get_mut(&self.index)
            .filter(|clock| clock.started == self.started)
        {
            clock.retry_pending = true;
        }
    }

    fn expired(&self) -> bool {
        self.started.elapsed() >= self.rendition.materialize_budget
    }
}

impl Drop for MaterializeDemand {
    fn drop(&mut self) {
        let mut demands = self.rendition.demand_since.lock().expect("demand lock");
        if let Some(clock) = demands
            .get_mut(&self.index)
            .filter(|clock| clock.started == self.started)
        {
            clock.owners -= 1;
            if clock.owners == 0 && !clock.retry_pending {
                demands.remove(&self.index);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerDestination {
    kind: AnnotationKind,
    start_ms: i64,
    end_ms: i64,
    target_entry: u32,
    window_end_entry: u32,
    eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrewarmedRange {
    first: u32,
    last: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrewarmedEntry {
    index: u32,
    publication: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkerPrewarmRecord {
    destination: MarkerDestination,
    requested_sequence: u64,
    /// Ledger-local identity. Unlike the protocol sequence, this never resets
    /// when control ownership changes, so an old dispatch cannot credit a
    /// replacement request that happens to reuse the same sequence number.
    nonce: u64,
    /// True only while the latest accepted snapshot is inside this marker's
    /// approach window. Historical provenance stays in the row after this is
    /// cleared, but it can no longer schedule work.
    schedulable: bool,
    active: bool,
    settled: bool,
    produced: Vec<PrewarmedEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerPrewarmRequest {
    destination: MarkerDestination,
    nonce: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerPendingSkip {
    request: MarkerPrewarmRequest,
    /// Exact prewarm publication present when the skip beacon was consumed.
    /// `None` is a durable miss: later production cannot upgrade it.
    publication_at_skip: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerAwaitingBeacon {
    request: MarkerPrewarmRequest,
    /// Exact publication observed at a post-marker Rendering snapshot. This
    /// lets a delayed beacon settle without treating old forward-buffer fetches
    /// as landing evidence.
    publication_at_landing: Option<u64>,
}

impl MarkerPrewarmRecord {
    fn credited(&self, entry: u32) -> bool {
        self.produced.iter().any(|produced| produced.index == entry)
    }

    fn credit(&mut self, entry: u32, publication: u64) {
        if entry > self.destination.window_end_entry {
            return;
        }
        if let Some(existing) = self
            .produced
            .iter_mut()
            .find(|produced| produced.index == entry)
        {
            existing.publication = publication;
            return;
        }
        self.produced.push(PrewarmedEntry {
            index: entry,
            publication,
        });
    }

    fn credited_publication(&self, entry: u32, publication: Option<u64>) -> bool {
        publication.is_some_and(|publication| {
            self.produced
                .iter()
                .any(|produced| produced.index == entry && produced.publication == publication)
        })
    }
}

/// Bounded provenance ledger for one playback. There can be at most one row
/// per stored annotation (the store itself caps that set), and rows survive
/// settlement so a later marker seek can prove who produced its landing
/// segment rather than consulting the ordinary forward buffer.
#[derive(Debug, Default)]
struct MarkerPrewarmLedger {
    enabled: bool,
    records: Vec<MarkerPrewarmRecord>,
    next_nonce: u64,
    /// A client marker beacon is the explicit skip intent. Control snapshots
    /// arm its exact stored destination while playback is inside the marker;
    /// the beacon moves it here until a served segment or later snapshot
    /// proves the landing.
    approach_skip: Option<MarkerPrewarmRequest>,
    armed_skip: Option<MarkerPrewarmRequest>,
    pending_skip: Option<MarkerPendingSkip>,
    awaiting_beacon: Option<MarkerAwaitingBeacon>,
    last_settled_skip: Option<MarkerPrewarmRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerPrewarmCandidate {
    target_entry: u32,
    window_end_entry: u32,
    target_materialized: bool,
}

struct MarkerPrewarmDecision {
    action: Action,
    candidate: Option<MarkerPrewarmCandidate>,
    owners: Vec<MarkerPrewarmOwner>,
}

#[derive(Clone)]
struct MarkerPrewarmOwner {
    ledger: Arc<StdMutex<MarkerPrewarmLedger>>,
    record_nonce: u64,
}

struct MarkerPrewarmDispatch {
    producer_epoch: u64,
    candidate: MarkerPrewarmCandidate,
    owners: Vec<MarkerPrewarmOwner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerPrewarmOutcome {
    hit: bool,
    destination: MarkerDestination,
    requested_sequence: Option<u64>,
    produced_range: Option<PrewarmedRange>,
}

#[derive(Debug)]
struct MarkerClientSkipResult {
    matched: bool,
    outcome: Option<MarkerPrewarmOutcome>,
}

struct MarkerPrewarmControl {
    rendition: Arc<Rendition>,
    snapshot: crate::playback_control::PlaybackDemandSnapshot,
    destinations: Vec<MarkerDestination>,
    sequence: u64,
    file_id: i64,
    kind: SessionKind,
}

impl MarkerPrewarmLedger {
    fn allocate_nonce(&mut self) -> u64 {
        self.next_nonce = self.next_nonce.saturating_add(1);
        self.next_nonce
    }

    fn request_for(&self, destination: MarkerDestination) -> Option<MarkerPrewarmRequest> {
        self.records
            .iter()
            .find(|record| record.destination == destination && !record.settled)
            .map(|record| MarkerPrewarmRequest {
                destination,
                nonce: record.nonce,
            })
    }

    fn update_control(
        &mut self,
        sequence: u64,
        snapshot: &crate::playback_control::PlaybackDemandSnapshot,
        destinations: &[MarkerDestination],
    ) {
        for record in &mut self.records {
            record.schedulable = false;
        }
        let scheduling_enabled = snapshot.demand == crate::playback_control::PlaybackDemand::Active
            && snapshot.render_state != crate::playback_control::RenderState::Seeking
            && snapshot.playback_rate > 0.0;
        self.enabled = scheduling_enabled;

        let approach_ms = (MARKER_PREWARM_APPROACH_WALL_MS as f64 * snapshot.playback_rate)
            .round()
            .clamp(0.0, i64::MAX as f64) as i64;
        let approach_end_ms = snapshot.position_ms.saturating_add(approach_ms);
        let approaching = destinations
            .iter()
            .copied()
            .filter(|destination| {
                destination.eligible
                    && snapshot.position_ms < destination.end_ms
                    && destination.start_ms <= approach_end_ms
            })
            .collect::<Vec<_>>();
        for destination in &approaching {
            let existing = self
                .records
                .iter()
                .position(|record| record.destination == *destination);
            match existing {
                Some(index) if self.records[index].settled => {
                    let nonce = self.allocate_nonce();
                    self.records[index] = MarkerPrewarmRecord {
                        destination: *destination,
                        requested_sequence: sequence,
                        nonce,
                        schedulable: scheduling_enabled,
                        active: false,
                        settled: false,
                        produced: Vec::new(),
                    };
                    self.last_settled_skip = None;
                }
                Some(index) => self.records[index].schedulable = scheduling_enabled,
                None => {
                    let nonce = self.allocate_nonce();
                    self.records.push(MarkerPrewarmRecord {
                        destination: *destination,
                        requested_sequence: sequence,
                        nonce,
                        schedulable: scheduling_enabled,
                        active: false,
                        settled: false,
                        produced: Vec::new(),
                    });
                }
            }
        }
        let mut approach_requests = approaching
            .iter()
            .filter_map(|destination| self.request_for(*destination));
        let approach_request = approach_requests.next();
        self.approach_skip = approach_requests
            .next()
            .is_none()
            .then_some(approach_request)
            .flatten();
        // A prior natural traversal can leave a landing waiting for a beacon.
        // Once playback is observed before that destination again, that
        // landing belongs to the old traversal and cannot prove a new skip.
        if self
            .awaiting_beacon
            .is_some_and(|awaiting| snapshot.position_ms < awaiting.request.destination.end_ms)
        {
            self.awaiting_beacon = None;
        }
        if self.approach_skip.is_some_and(|request| {
            self.last_settled_skip
                .is_some_and(|settled| settled != request)
        }) {
            self.last_settled_skip = None;
        }
        if self.approach_skip.is_some_and(|request| {
            self.pending_skip
                .is_some_and(|pending| pending.request != request)
        }) {
            self.pending_skip = None;
        }
        if self.approach_skip.is_some_and(|request| {
            self.awaiting_beacon
                .is_some_and(|awaiting| awaiting.request != request)
        }) {
            self.awaiting_beacon = None;
        }
        let inside = approaching.iter().copied().find(|destination| {
            snapshot.position_ms >= destination.start_ms
                && snapshot.position_ms < destination.end_ms
        });
        self.armed_skip = inside.and_then(|destination| self.request_for(destination));
        self.enabled &= !approaching.is_empty();
        if !self.enabled {
            self.deactivate();
        } else {
            for record in &mut self.records {
                if !record.schedulable {
                    record.active = false;
                }
            }
        }
    }

    fn candidate(&mut self, manifest: &Manifest) -> Option<MarkerPrewarmCandidate> {
        if !self.enabled {
            return None;
        }
        let mut candidate: Option<MarkerPrewarmCandidate> = None;
        for record in &mut self.records {
            if record.settled || !record.schedulable {
                continue;
            }
            let target_materialized = manifest
                .state(record.destination.target_entry)
                .is_some_and(SegState::is_materialized);
            if target_materialized && !record.credited(record.destination.target_entry) {
                // Ordinary playback or another reader won the race. It is
                // useful media, but it is not this prewarm's production.
                record.active = false;
                record.schedulable = false;
                continue;
            }
            if target_materialized {
                let has_window_gap = manifest
                    .next_gap(record.destination.target_entry)
                    .is_some_and(|gap| gap <= record.destination.window_end_entry);
                if !has_window_gap {
                    record.active = false;
                    record.schedulable = false;
                    continue;
                }
            }
            let proposed = MarkerPrewarmCandidate {
                target_entry: record.destination.target_entry,
                window_end_entry: record.destination.window_end_entry,
                target_materialized,
            };
            if candidate.is_none_or(|current| proposed.target_entry < current.target_entry) {
                candidate = Some(proposed);
            }
        }
        candidate
    }

    fn activate(&mut self, candidate: MarkerPrewarmCandidate) -> Vec<u64> {
        for record in &mut self.records {
            record.active = self.enabled
                && record.schedulable
                && !record.settled
                && record.destination.target_entry == candidate.target_entry
                && record.destination.window_end_entry == candidate.window_end_entry;
        }
        self.records
            .iter()
            .filter(|record| record.active)
            .map(|record| record.nonce)
            .collect()
    }

    fn deactivate(&mut self) {
        for record in &mut self.records {
            record.active = false;
        }
    }

    fn credit_dispatched(
        &mut self,
        candidate: MarkerPrewarmCandidate,
        record_nonce: u64,
        entry: u32,
        publication: u64,
    ) {
        for record in self.records.iter_mut().filter(|record| {
            !record.settled
                && record.nonce == record_nonce
                && record.destination.target_entry == candidate.target_entry
                && record.destination.window_end_entry == candidate.window_end_entry
        }) {
            record.credit(entry, publication);
        }
    }

    fn can_match_client_skip(&self) -> bool {
        self.approach_skip.is_some()
            || self.awaiting_beacon.is_some()
            || self.armed_skip.is_some()
            || self.pending_skip.is_some()
            || self.last_settled_skip.is_some()
    }

    fn credited_publication(
        &self,
        request: MarkerPrewarmRequest,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> Option<u64> {
        let publication = publications
            .get(request.destination.target_entry as usize)
            .copied()
            .flatten()?;
        (manifest
            .state(request.destination.target_entry)
            .is_some_and(SegState::is_materialized)
            && self.records.iter().any(|record| {
                record.nonce == request.nonce
                    && record
                        .credited_publication(request.destination.target_entry, Some(publication))
            }))
        .then_some(publication)
    }

    fn observe_control_landing(
        &mut self,
        position_ms: i64,
        landing_entry: u32,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> Option<MarkerPrewarmOutcome> {
        if let Some(outcome) = self.settle_pending_at(landing_entry, manifest, publications) {
            return Some(outcome);
        }
        let request = self.armed_skip.or(self.approach_skip).filter(|request| {
            position_ms >= request.destination.end_ms
                && landing_entry == request.destination.target_entry
        })?;
        self.awaiting_beacon = Some(MarkerAwaitingBeacon {
            request,
            publication_at_landing: self.credited_publication(request, manifest, publications),
        });
        None
    }

    fn note_client_skip(
        &mut self,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> MarkerClientSkipResult {
        if let Some(awaiting) = self.awaiting_beacon.take() {
            let current = self.credited_publication(awaiting.request, manifest, publications);
            let publication_at_skip = awaiting
                .publication_at_landing
                .filter(|publication| current == Some(*publication));
            self.last_settled_skip = Some(awaiting.request);
            return MarkerClientSkipResult {
                matched: true,
                outcome: Some(self.settle_skip(
                    awaiting.request,
                    publication_at_skip,
                    manifest,
                    publications,
                )),
            };
        }
        if self.pending_skip.is_some() {
            return MarkerClientSkipResult {
                matched: true,
                outcome: None,
            };
        }
        let request = self.armed_skip.take().or(self.approach_skip);
        let Some(request) = request else {
            return MarkerClientSkipResult {
                matched: self.last_settled_skip.is_some(),
                outcome: None,
            };
        };
        if self.approach_skip == Some(request) {
            self.approach_skip = None;
        }
        self.pending_skip = Some(MarkerPendingSkip {
            request,
            publication_at_skip: self.credited_publication(request, manifest, publications),
        });
        MarkerClientSkipResult {
            matched: true,
            outcome: None,
        }
    }

    fn settle_pending_at(
        &mut self,
        landing_entry: u32,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> Option<MarkerPrewarmOutcome> {
        let pending = self
            .pending_skip
            .filter(|pending| pending.request.destination.target_entry == landing_entry)?;
        self.pending_skip = None;
        if self.armed_skip == Some(pending.request) {
            self.armed_skip = None;
        }
        self.last_settled_skip = Some(pending.request);
        Some(self.settle_skip(
            pending.request,
            pending.publication_at_skip,
            manifest,
            publications,
        ))
    }

    fn settle_skip(
        &mut self,
        request: MarkerPrewarmRequest,
        publication_at_skip: Option<u64>,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> MarkerPrewarmOutcome {
        self.enabled = false;
        self.deactivate();
        self.approach_skip = None;
        self.armed_skip = None;
        self.awaiting_beacon = None;
        let current_publication = publications
            .get(request.destination.target_entry as usize)
            .copied()
            .flatten();
        let current_matches = publication_at_skip.is_some()
            && current_publication == publication_at_skip
            && manifest
                .state(request.destination.target_entry)
                .is_some_and(SegState::is_materialized);
        let record = self
            .records
            .iter_mut()
            .find(|record| record.nonce == request.nonce);
        let (requested_sequence, produced_range) = record.map_or((None, None), |record| {
            record.settled = true;
            record.schedulable = false;
            (
                Some(record.requested_sequence),
                (current_matches
                    && record.credited_publication(
                        request.destination.target_entry,
                        publication_at_skip,
                    ))
                .then_some(PrewarmedRange {
                    first: request.destination.target_entry,
                    last: request.destination.target_entry,
                }),
            )
        });
        MarkerPrewarmOutcome {
            hit: produced_range.is_some(),
            destination: request.destination,
            requested_sequence,
            produced_range,
        }
    }
}

/// The rendition's init identity and where it came from — `from_disk` marks a
/// resurrection adoption, which is the only case §5's mismatch arm may purge
/// and re-establish rather than fail typed.
#[derive(Debug, Default)]
struct IdentityState {
    identity: Option<InitIdentity>,
    from_disk: bool,
}

/// One live rendition: a directory, a manifest, a producer, and its readers.
struct Rendition {
    key: String,
    dir: RenditionDir,
    recipe: Recipe,
    /// One opened source object for the rendition's whole life. Every ffmpeg
    /// generation inherits this descriptor and every materialization/serve
    /// checks its object version, so pathname replacement or in-place mutation
    /// cannot mix two source revisions under one immutable playlist.
    source: Option<crate::fragment_index_cluster::SourceFence>,
    /// The stored plan — normative, immutable, the source of everything below.
    plan: SegmentPlan,
    /// Rendered ONCE from the plan at attach; identical for the rendition's
    /// whole life (plan §2.1).
    playlist: Vec<u8>,
    timescale: u32,
    seconds_per_segment: f64,
    index: FragmentIndex,
    policy: CutPolicy,
    working_set_budget: u64,
    completed_cache_budget: u64,
    materialize_budget: Duration,
    manifest: Mutex<Manifest>,
    identity: Mutex<IdentityState>,
    slot: ProducerSlot,
    readers: Mutex<HashMap<String, Reader>>,
    /// Monotonic identity of each successful segment publication. The ledger
    /// stores this beside an entry index so eviction followed by ordinary
    /// rematerialization cannot inherit stale prewarm credit.
    publication_serial: AtomicU64,
    publication_versions: StdMutex<Vec<Option<u64>>>,
    /// Attribution for work already dispatched to one producer generation.
    /// Control may disable future speculation while that generation has the
    /// landing fragment in flight; the dispatch survives just long enough to
    /// credit that publication to the playback that requested it.
    marker_prewarm_dispatch: StdMutex<Option<MarkerPrewarmDispatch>>,
    /// Keeps the ordinary producer publication path O(1) when no playback is
    /// currently attributing work to marker prewarm.
    active_marker_prewarms: AtomicU32,
    /// The generation that was started or repurposed for speculative marker
    /// work, encoded as epoch + 1 so zero means absent. Attribution may end as
    /// soon as the requested window publishes, but the producer can still
    /// have output queued; capacity work must fence that generation until it
    /// physically retires.
    marker_prewarm_generation: AtomicU64,
    /// A recorded producer failure: subsequent planned-segment GETs answer
    /// `ProducerFailed` until a new create replaces the rendition.
    ///
    /// The class travels with the prose. The prose is a sentence for an
    /// operator reading a log; the class is the only part a client can act on,
    /// because `producer_state` flattens every failure to the word `failed`
    /// and a client that cannot tell "try again" from "this will never work"
    /// guesses toward retry — reopening, getting the same verdict, reopening
    /// again.
    failed: StdMutex<Option<RenditionFailure>>,
    /// The capacity hold this rendition is under, if any, retained across the
    /// producer that was terminated for it.
    ///
    /// A hold that cannot clear on its own terminates the producer — a stopped
    /// one goes on holding everything a running one held — which leaves the
    /// belief `Absent` and takes the reason with it. So the status said
    /// `waiting` with no hold: the viewer's control plane saw a producer that
    /// had stopped and never saw *why*, which for a capacity stall is the only
    /// fact that explains why nothing is arriving. Recorded from each pass's
    /// own decision and cleared by the first pass that decides anything else,
    /// so it cannot outlive the condition.
    capacity_hold: StdMutex<Option<crate::prodsched::Hold>>,
    /// Woken when `init.mp4` lands, for GETs waiting on the identity.
    init_notify: Notify,
    /// The driver's kick: wait registration, segment GETs, attach/detach,
    /// maintain ticks.
    wake: Notify,
    /// Bumped whenever the driver kills or replaces the producer, so a
    /// generation that ends can tell "I died on my own" from "I was told to".
    gen_epoch: AtomicU64,
    /// The most recent generation child's pid, for diagnostics and tests.
    last_child_pid: AtomicU32,
    dormant_since: StdMutex<Option<Instant>>,
    /// Set when the rendition is purged or replaced; the driver exits and the
    /// sink refuses further writes as the quiet `NotFound` teardown.
    closed: AtomicBool,
    /// One admission-refusal line per fill, not one per materialize.
    warned_admission: AtomicBool,
    /// First blocked demand per plan entry, retained across HTTP 503 retries.
    demand_since: StdMutex<HashMap<u32, MaterializeClock>>,
}

impl Rendition {
    fn kick(&self) {
        self.wake.notify_one();
    }

    fn failure(&self) -> Option<RenditionFailure> {
        self.failed.lock().expect("failed lock").clone()
    }

    /// Just the operator-facing sentence, for the paths that answer a typed
    /// HTTP refusal whose body is prose.
    fn failure_cause(&self) -> Option<String> {
        self.failure().map(|failure| failure.cause)
    }

    fn identity_path(&self) -> PathBuf {
        self.dir.path().join(IDENTITY_NAME)
    }

    fn clear_demand(&self, index: u32) {
        self.demand_since
            .lock()
            .expect("demand lock")
            .remove(&index);
    }

    #[cfg(test)]
    async fn attach_reader(&self, session_id: &str, frontier: u32) {
        self.readers
            .lock()
            .await
            .insert(session_id.to_string(), Reader::new(frontier));
        *self.dormant_since.lock().expect("dormant lock") = None;
    }

    async fn detach_reader(&self, pool: &crate::waitpool::WaitPool, session_id: &str) {
        {
            let mut readers = self.readers.lock().await;
            readers.remove(session_id);
            if readers.is_empty() {
                *self.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
            }
        }
        // Retire this session's parked GETs with its reader, not after them.
        // `playback_demands` ranks a blocked request against the reader that
        // asked for it, and with no reader to rank against it falls back to
        // marking the session's oldest wait foreground — so a departed
        // viewer's abandoned request outranks a present viewer's and aims the
        // producer at media nobody is watching until its deadline expires.
        pool.retire_session(&self.key, session_id);
        // Every VOD session *ending* converges here — terminal and idle reap
        // alike — so this is the one place a departing viewer's subtitle
        // window is released. Reattachment is deliberately not one of them:
        // it is not an ending, the id it reuses belongs to a viewer who is
        // still watching, and releasing fences that id for half a minute. It
        // stops its own obsolete flight by name instead, without the fence,
        // which the comment here used to claim it did through this function.
        // The readers guard is deliberately
        // dropped first: releasing waits for a real ffmpeg to settle, and
        // holding a rendition-wide lifecycle lock across that await would let
        // one leaving viewer stall every other reader of the same rendition.
        crate::subtitles::release_session_window(session_id).await;
    }

    /// Eviction windows for every attached reader (plan §2.4's reader guard).
    async fn reader_windows(&self) -> Vec<ReaderWindow> {
        let readers = self.readers.lock().await;
        readers
            .values()
            .map(|reader| reader_window(reader, self.seconds_per_segment))
            .collect()
    }
}

struct TerminalCleanup {
    finished: AtomicBool,
    notify: Notify,
    completed_at: std::sync::OnceLock<tokio::time::Instant>,
    #[cfg(test)]
    wait_enabled_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl TerminalCleanup {
    fn new() -> Self {
        Self {
            finished: AtomicBool::new(false),
            notify: Notify::new(),
            completed_at: std::sync::OnceLock::new(),
            #[cfg(test)]
            wait_enabled_pause: StdMutex::new(None),
        }
    }

    fn complete(&self) {
        // The late-response window starts only after reader detach, lifecycle
        // emission, and any terminal commit settlement owned by the cleanup
        // task have actually finished. Starting it at tombstone creation made
        // a slow cleanup immediately evictable.
        let _ = self.completed_at.set(tokio::time::Instant::now());
        self.finished.store(true, Release);
        self.notify.notify_waiters();
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Acquire)
    }

    fn retention_expired(&self) -> bool {
        self.completed_at.get().is_some_and(|completed_at| {
            tokio::time::Instant::now() >= *completed_at + TERMINAL_TOMBSTONE_RETENTION
        })
    }

    async fn wait(&self) {
        while !self.is_finished() {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // `notify_waiters` stores no permit for a future that has only
            // been constructed. Register it before the second state read so
            // completion can occur on either side of that read without being
            // lost.
            notified.as_mut().enable();
            #[cfg(test)]
            {
                let pause = self
                    .wait_enabled_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(pause) = pause {
                    pause.wait().await;
                    pause.wait().await;
                }
            }
            if self.is_finished() {
                return;
            }
            notified.await;
        }
    }
}

struct TerminalCleanupGuard(Arc<TerminalCleanup>);

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        self.0.complete();
    }
}

/// One detached ffmpeg whose Drop path still owns SIGKILL plus a confirmed
/// wait. The reaper task is intentionally independent of the request future:
/// dropping a resurrection/build cannot drop the only process owner.
struct HeadChildOwner {
    child: Option<tokio::process::Child>,
    #[cfg(test)]
    reap_pause: Option<Arc<tokio::sync::Barrier>>,
}

impl HeadChildOwner {
    fn new(child: tokio::process::Child) -> Self {
        Self {
            child: Some(child),
            #[cfg(test)]
            reap_pause: None,
        }
    }

    #[cfg(test)]
    fn with_reap_pause(
        child: tokio::process::Child,
        reap_pause: Arc<tokio::sync::Barrier>,
    ) -> Self {
        Self {
            child: Some(child),
            reap_pause: Some(reap_pause),
        }
    }

    fn begin_reap(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let mut child = self.child.take()?;
        let _ = child.start_kill();
        #[cfg(test)]
        let reap_pause = self.reap_pause.take();
        Some(tokio::spawn(async move {
            #[cfg(test)]
            if let Some(pause) = reap_pause {
                pause.wait().await;
                pause.wait().await;
            }
            let _ = child.wait().await;
        }))
    }

    async fn terminate_and_reap(&mut self) {
        if let Some(reaper) = self.begin_reap() {
            // Cancelling this await only detaches the already-owned reaper.
            let _ = reaper.await;
        }
    }
}

impl Drop for HeadChildOwner {
    fn drop(&mut self) {
        let _ = self.begin_reap();
    }
}

#[derive(Debug)]
enum HeadRegenerationError {
    Busy,
    Oversize,
    Failed(String),
}

impl std::fmt::Display for HeadRegenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("head-regeneration capacity is full"),
            Self::Oversize => write!(
                formatter,
                "head regeneration exceeded the {} byte init limit",
                HEAD_REGENERATION_MAX_BYTES
            ),
            Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

fn deferred_terminal_commit(
    committer: Arc<dyn crate::playback_control::TerminalControlCommitter>,
    result: &mut crate::playback_control::LocalControlResult,
) -> crate::playback_control::TerminalCommitReceipt {
    // Install immutable prepared data before the outer receipt is attached.
    // Keeping that receipt out of the closure's captured result avoids the
    // outer -> closure -> prepared result -> outer Arc cycle.
    let prepared = Arc::new(StdMutex::new(Some(result.clone())));
    let inner = Arc::new(StdMutex::new(
        None::<crate::playback_control::TerminalCommitReceipt>,
    ));
    let receipt = crate::playback_control::TerminalCommitReceipt::deferred_retryable({
        let prepared = Arc::clone(&prepared);
        let inner = Arc::clone(&inner);
        move |attempt| {
            let (receipt, retry) = {
                let mut inner = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(receipt) = inner.as_ref() {
                    (receipt.clone(), true)
                } else {
                    let receipt = {
                        let prepared = prepared
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        committer.start(
                            prepared
                                .as_ref()
                                .expect("deferred VOD terminal result must be installed"),
                        )
                    };
                    *inner = Some(receipt.clone());
                    (receipt, false)
                }
            };
            if let Some(expires_at_unix_ms) = receipt.expires_at_unix_ms() {
                attempt.set_expires_at_unix_ms(expires_at_unix_ms);
            }
            if retry {
                receipt.retry();
            }
            tokio::spawn(async move {
                attempt.complete(receipt.wait().await);
            });
        }
    });
    result.terminal_commit = Some(receipt.clone());
    receipt
}

fn terminal_reason(cause: Terminal) -> &'static str {
    match cause {
        Terminal::Deleted => "client_released",
        Terminal::Superseded => "superseded",
        Terminal::AdminStop => "killed",
        Terminal::Revoked => "revoked",
        Terminal::Replaced => "file_replaced",
    }
}

/// One session handle (plan §2.5): auth attribution, sliding TTL, reader
/// window, and — once it ends for good — a tombstone.
struct Session {
    /// Cleared synchronously by the detached terminal cleanup owner before it
    /// publishes completion. The compact fields below retain exact 410/replay
    /// identity without retaining manifests, source handles, readers, or the
    /// producer graph until the next maintenance tick.
    rendition: Option<Arc<Rendition>>,
    rendition_key: String,
    file: Arc<MediaFile>,
    playback_id: String,
    user_name: String,
    item_title: String,
    started_unix: i64,
    /// Echoed on an idempotent create replay (`request_id` recovery).
    target_height: i64,
    /// Echoed on an idempotent create replay.
    kind: SessionKind,
    /// The same user scope the legacy supersession sweep filters by, so one
    /// viewer's `playback_id` can never end another viewer's session.
    supersession_user: String,
    block_budget: Duration,
    /// Serializes authority-checked control with terminal/reap transitions for
    /// this session only. Keeping the gate on the session avoids making an
    /// unrelated rolling or VOD control wait behind node-wide Store I/O.
    lifecycle: Arc<Mutex<()>>,
    /// Unique identity of this attachment while `lifecycle` is deliberately
    /// stable across same-id idle reap and resurrection.
    incarnation: Arc<()>,
    last_touch: StdMutex<Instant>,
    /// Bytes this viewer has actually been handed, over monotonic time.
    ///
    /// Per session rather than per rendition: two viewers of the same
    /// immutable rendition have two different links, and a rate that averaged
    /// them would describe neither. This is the VOD half of what
    /// `transcode::Session::delivery` already is for rolling delivery, and it
    /// exists for the same reason — `PreparationConditions::headroom_refusal`
    /// cannot judge headroom against a rate nobody measured, so on VOD, which
    /// is every public session, it refused `throughput_unreported` forever.
    ///
    /// `Arc` because the response body outlives the registry lock: bytes are
    /// counted on the pump task, long after `segment` has returned and the
    /// `sessions` map has been unlocked.
    delivery: Arc<crate::meter::Meter>,
    /// Owner-local sequence fence kept separate from media-object touches.
    control: StdMutex<crate::playback_control::ControlState>,
    /// Stored, source-fenced marker boundaries projected onto this rendition's
    /// immutable plan. No request-path probing or detector runs here.
    marker_destinations: Vec<MarkerDestination>,
    /// Latest accepted snapshot, used to correlate a session-less shipped
    /// marker beacon and to settle a seek whose transient state was coalesced.
    /// Replays never replace it or emit a second outcome.
    last_control_snapshot: Option<crate::playback_control::PlaybackDemandSnapshot>,
    /// Exact terminal acknowledgement retained after a client `demand=end`
    /// tombstones the attachment. Other lifecycle causes never populate it.
    control_end: Option<crate::playback_control::LocalControlResult>,
    /// Complete accepted End payload. Reusing its sequence with different
    /// observations is a conflict, not an idempotent terminal replay.
    control_end_snapshot: Option<crate::playback_control::PlaybackDemandSnapshot>,
    /// Session-owned, idempotent reader detach. It continues if the request
    /// that committed the tombstone is cancelled and is joined by every later
    /// replay/end/maintenance path.
    terminal_cleanup: Option<Arc<TerminalCleanup>>,
    tombstone: Option<Terminal>,
}

impl Session {
    /// Move any staged M6 successor to aborting, because this session is over.
    ///
    /// Called wherever a tombstone is written, under the same registry lock
    /// that writes it, so the slot and the session's liveness can never
    /// disagree. That is the rolling actor's shape — its `terminate` aborts
    /// the slot it is holding — and it is what lets
    /// `VodPreparationGate::may_commit_preparation` trust the slot instead of
    /// layering a second refusal on top of an untouched one.
    ///
    /// The durable row is not torn down here and is not this path's to reap:
    /// the executor's commit finds `may_commit` false and stops, and the
    /// preparation's own `deadline_ms` is the backstop for an owner that died
    /// holding a successor. What this does buy is that nothing afterwards
    /// believes the successor is still wanted.
    fn abort_staged_preparation(&self) {
        let mut control = self.control.lock().expect("control lock");
        if let Some(staged) = control.staged_incarnation_id().map(str::to_owned) {
            control.abort_preparation(&staged);
        }
    }

    fn live_rendition(&self) -> Option<&Arc<Rendition>> {
        self.rendition.as_ref().filter(|_| self.tombstone.is_none())
    }

    fn response_owner(&self) -> ResponseOwner {
        ResponseOwner {
            lifecycle: Arc::clone(&self.lifecycle),
            incarnation: Arc::clone(&self.incarnation),
            rendition: self.rendition.as_ref().map(Arc::clone),
            rendition_key: self.rendition_key.clone(),
            file: Arc::clone(&self.file),
            tombstone: self.tombstone,
        }
    }

    /// A terminal control response remains part of the exact-owner replay
    /// contract until its commit receipt expires, even if the independent
    /// cleanup-retention window has already elapsed.
    fn terminal_replay_expired(&self) -> bool {
        self.control_end
            .as_ref()
            .and_then(|result| result.terminal_commit.as_ref())
            .is_none_or(crate::playback_control::TerminalCommitReceipt::is_expired)
    }
}

/// This engine's half of [`crate::playback_control::PreparationGate`].
///
/// M6's preparation slot lives on `ControlState`, which both delivery engines
/// hold, precisely so it exists here — `into_request` sets `Presentation::Vod`
/// for every create, so this engine serves the sessions a staged successor is
/// actually for. What this type adds is the liveness half the slot cannot
/// answer for itself: a session that has vanished from the registry or carries
/// a tombstone takes no successor.
///
/// Holds `Arc<Shared>` and an id rather than a reference into the registry,
/// the same shape as [`VodPreparationGuard`], because `Session` lives inside
/// the `sessions` map by value and cannot be borrowed across an await. Each
/// call therefore takes the registry lock, holds it for one slot transition,
/// and releases it — never across durable I/O, which is what the executor's
/// three-phase order exists to keep true.
///
/// **The id alone is not the session.** A gate outlives a stage by however
/// long the successor takes to warm up, and in that window an idle reap can
/// remove this session — deliberately without a tombstone — and a reconnect
/// resurrect the same durable id with a fresh `ControlState`. The lifecycle
/// gate is stable across exactly that, on purpose, so it identifies nothing
/// here. So this pins `incarnation`, like every other operation in this file
/// that acts on a session it looked up earlier: without it a stale gate takes
/// the *replacement's* slot for a successor whose predecessor no longer holds
/// the pointer, leaving the store's CAS as the only thing between that and a
/// wrong commit — the second authority over one playback the executor exists
/// to prevent.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct VodPreparationGate {
    shared: Arc<Shared>,
    session_id: String,
    /// Weak so a held gate cannot keep a dead attachment's identity alive; a
    /// failed upgrade is simply a session that is gone.
    incarnation: Weak<()>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl VodPreparationGate {
    /// The live session this gate was made for, or `None` if it has been
    /// removed or replaced by a later incarnation.
    fn bound<'a>(&self, sessions: &'a mut HashMap<String, Session>) -> Option<&'a mut Session> {
        let mine = self.incarnation.upgrade()?;
        let session = sessions.get_mut(&self.session_id)?;
        Arc::ptr_eq(&session.incarnation, &mine).then_some(session)
    }
}

impl crate::playback_control::PreparationGate for VodPreparationGate {
    fn stage_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: String,
        predecessor_incarnation_id: String,
        deadline_ms: i64,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            // The liveness half, and the only place this engine asks it: a session
            // that has ended takes no successor. Past this point the slot answers
            // for itself, because `abort_staged_preparation` has already moved it
            // wherever a tombstone put it.
            if session.tombstone.is_some() {
                return false;
            }
            let staged = session
                .control
                .lock()
                .expect("control lock")
                .stage_preparation_for_owner(
                    staged_incarnation_id,
                    predecessor_incarnation_id,
                    deadline_ms,
                    expected_owner_epoch,
                );
            staged
        })
    }

    fn may_commit_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            // Deliberately **not** a second tombstone check. The rolling actor
            // enforces liveness here by mutating the slot at termination and then
            // letting the slot answer, and layering a refusal on top of an
            // untouched slot would leave the two disagreeing indefinitely — the
            // gate saying no while `ControlState` still said the successor was
            // committable. This engine terminalizes the same way: every path that
            // writes a tombstone calls `abort_staged_preparation` under the same
            // registry lock.
            let may = session
                .control
                .lock()
                .expect("control lock")
                .may_commit_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            may
        })
    }

    fn begin_abort_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            let reserved = session
                .control
                .lock()
                .expect("control lock")
                .begin_abort_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            reserved
        })
    }

    fn reject_preparation_commit_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            let rejected = session
                .control
                .lock()
                .expect("control lock")
                .reject_preparation_commit_for_owner(staged_incarnation_id, expected_owner_epoch);
            rejected
        })
    }

    fn settle_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        committed: bool,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                // A settle nobody can hear is not a failure. The session is gone,
                // so its slot is gone with it, and the durable outcome the caller
                // is reporting has already been written either way.
                return false;
            };
            let mut control = session.control.lock().expect("control lock");
            if !committed {
                control
                    .begin_abort_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            }
            let settled =
                control.settle_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            drop(control);
            settled
        })
    }
}

/// A prepared rendition plus the exact per-key build gate. The gate remains
/// held through the final reader/session registry transaction, so a dormant
/// purge cannot remove the handle between lookup and attachment. Slow build
/// ownership is cancellation-independent; if its requester disappears, the
/// detached owner finishes any child reap and drops this unused guard only at
/// the transaction boundary.
struct RenditionAttachment {
    rendition: Arc<Rendition>,
    _build_guard: tokio::sync::OwnedMutexGuard<()>,
}

struct TerminalEvictionCandidate {
    session_id: String,
    lifecycle: Arc<Mutex<()>>,
    incarnation: Arc<()>,
    cleanup: Arc<TerminalCleanup>,
}

/// Exact, cancellation-safe marker for a VOD capability whose rendition is
/// prepared outside the short final attachment transaction. Lease loss must
/// be able to terminalize this stable capability even before it appears in
/// `sessions`; otherwise a request admitted just before the loss can attach
/// after the owner has self-fenced.
pub(crate) struct VodPreparationGuard {
    shared: Arc<Shared>,
    session_id: String,
}

impl Drop for VodPreparationGuard {
    fn drop(&mut self) {
        let mut preparing = self
            .shared
            .preparing_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(holders) = preparing.get_mut(&self.session_id) {
            *holders = holders.saturating_sub(1);
            if *holders == 0 {
                preparing.remove(&self.session_id);
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TerminalRouteTestOutcome {
    Success,
    Timeout,
    Error,
}

fn spawn_cancellation_independent<T, F>(future: F) -> tokio::sync::oneshot::Receiver<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = future.await;
        let _ = result_tx.send(result);
    });
    result_rx
}

/// Everything the tasks share. `VodServe` is a thin handle over this so the
/// per-rendition driver and generation tasks can be `'static`.
struct Shared {
    base: PathBuf,
    store: Arc<dyn Store>,
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

impl VodServe {
    /// A preparation gate for one live session, or `None` when this engine
    /// has no live attachment under that id.
    ///
    /// **`None` does not mean "ask the rolling actor".** This engine tracks
    /// in-flight creates in `preparing_sessions` precisely because absence
    /// from `sessions` is not absence from this engine —
    /// [`VodServe::owns_or_preparing`] exists for that question and is the one
    /// a router must ask. `None` here means only that there is no live
    /// attachment to hold a slot *right now*; a session still being created
    /// will have one shortly. Returning a gate that always answered `false`
    /// would look like a full slot and hide that distinction.
    ///
    /// The liveness read below is not a guarantee — the gate outlives it, and
    /// re-checks on every call, pinned to the exact incarnation seen here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn preparation_gate(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Option<Arc<dyn crate::playback_control::PreparationGate>> {
        let sessions = self.shared.sessions.lock().await;
        let live = sessions
            .get(session_id)
            .filter(|session| session.tombstone.is_none())
            .map(|session| Arc::downgrade(&session.incarnation));
        drop(sessions);
        live.map(|incarnation| {
            Arc::new(VodPreparationGate {
                shared: Arc::clone(&self.shared),
                session_id: session_id.to_owned(),
                incarnation,
            }) as Arc<dyn crate::playback_control::PreparationGate>
        })
    }

    /// `base` is the renditions root directory (created lazily).
    pub fn new(base: PathBuf, store: Arc<dyn Store>) -> Arc<VodServe> {
        Self::new_configured(base, store, None, None, None)
    }

    /// Publish a producer-less VOD attachment for HTTP response-finalization
    /// tests. The fixture uses the real session/reader/incarnation machinery;
    /// only materialization and the producer driver are bypassed.
    #[cfg(test)]
    pub(crate) async fn install_http_test_session(
        &self,
        session_id: &str,
        file: MediaFile,
        base: &Path,
    ) {
        use plurx_core::fmp4::CutClass;
        use plurx_core::segplan::IndexRow;

        let mut rows = Vec::new();
        let mut dts = 0_u64;
        for index in 0..240 {
            let duration = if index % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes: 100_000,
                video_bytes: 99_400,
                class: CutClass::CleanIdr,
            });
            dts += duration;
        }
        let source_identity = SourceIdentity::new(
            u64::try_from(file.size).unwrap_or_default(),
            file.mtime,
            "http-test",
        );
        let index = FragmentIndex::new(16_000, rows, "http-test", source_identity);
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let duration_ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: duration_ms,
                audio_ms: duration_ms,
                audio_bits_per_second: 256_000,
            },
        );
        let dir = RenditionDir::new(base.join(format!("http-test-{}", uuid::Uuid::new_v4())));
        dir.create().await.expect("create HTTP VOD test rendition");
        let plan_len = plan.len();
        let rendition = Arc::new(Rendition {
            key: format!("http-test-{}", uuid::Uuid::new_v4()),
            dir,
            recipe: Recipe {
                file: file.clone(),
                audio_index: None,
                aac: true,
                video: CopyVideoOptions::new(false, false),
                source_object_version: None,
                cluster_cache_key: None,
            },
            source: None,
            playlist: plan.playlist().into_bytes(),
            timescale: plan.timescale,
            seconds_per_segment: plan.duration_ticks() as f64
                / f64::from(plan.timescale)
                / plan.len() as f64,
            index,
            policy,
            working_set_budget: 8 << 30,
            completed_cache_budget: 50 << 30,
            materialize_budget: Duration::from_secs(30),
            manifest: Mutex::new(Manifest::new(plan.clone())),
            plan,
            identity: Mutex::new(IdentityState::default()),
            slot: ProducerSlot::new(),
            readers: Mutex::new(HashMap::new()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            capacity_hold: StdMutex::new(None),
            init_notify: Notify::new(),
            wake: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            dormant_since: StdMutex::new(None),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        });

        let lifecycle = self.shared.session_lifecycle(session_id);
        let _lifecycle = lifecycle.lock().await;
        let previous = self
            .shared
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        if let Some(previous) = previous {
            previous.detach_reader(&self.shared.pool, session_id).await;
        }
        rendition.attach_reader(session_id, 0).await;
        self.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(file.clone()),
                playback_id: "http-vod-test".to_owned(),
                user_name: "test".to_owned(),
                item_title: "HTTP VOD fixture".to_owned(),
                started_unix: 1,
                target_height: file.height.unwrap_or(0),
                kind: SessionKind::Copy {
                    aac: true,
                    preserve_dolby_vision: false,
                    convert_dolby_vision: false,
                },
                supersession_user: "[\"user_id\",1]".to_owned(),
                block_budget: Duration::from_secs(1),
                lifecycle: Arc::clone(&lifecycle),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
    }

    #[cfg(test)]
    pub(crate) async fn last_touch_for_test(&self, session_id: &str) -> Option<Instant> {
        let sessions = self.shared.sessions.lock().await;
        let touch = sessions.get(session_id)?.last_touch.lock().ok()?;
        Some(*touch)
    }

    #[cfg(test)]
    pub(crate) fn set_terminal_detach_pause_for_test(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .shared
            .terminal_detach_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    #[cfg(test)]
    pub(crate) async fn has_attached_reader_for_test(&self, session_id: &str) -> bool {
        let rendition = self
            .shared
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        let Some(rendition) = rendition else {
            return false;
        };
        let attached = rendition.readers.lock().await.contains_key(session_id);
        attached
    }

    pub(crate) fn new_cluster(
        base: PathBuf,
        store: Arc<dyn Store>,
        node_id: String,
        cluster_index_root: PathBuf,
        membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        Self::new_configured(
            base,
            store,
            Some(node_id),
            Some(cluster_index_root),
            membership,
        )
    }

    fn new_configured(
        base: PathBuf,
        store: Arc<dyn Store>,
        cluster_node_id: Option<String>,
        cluster_index_root: Option<PathBuf>,
        cluster_membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        Arc::new(VodServe {
            shared: Arc::new(Shared {
                base,
                store,
                cluster_node_id,
                cluster_index_root,
                cluster_membership,
                sessions: Mutex::new(HashMap::new()),
                preparing_sessions: StdMutex::new(HashMap::new()),
                session_lifecycles: StdMutex::new(HashMap::new()),
                rendition_builds: StdMutex::new(HashMap::new()),
                renditions: Mutex::new(HashMap::new()),
                head_regeneration_slots: Arc::new(Semaphore::new(HEAD_REGENERATION_CAPACITY)),
                pool: WaitPool::new(DEFAULT_GLOBAL_WAIT_CAP, PER_SESSION_WAIT_CAP),
                working_set: AtomicU64::new(0),
                completed_cache: AtomicU64::new(0),
                terminal_eviction_cursor: AtomicU64::new(0),
                #[cfg(test)]
                terminal_replay_pause: StdMutex::new(None),
                #[cfg(test)]
                control_applied_pause: StdMutex::new(None),
                #[cfg(test)]
                segment_ready_pause: StdMutex::new(None),
                #[cfg(test)]
                terminal_detach_pause: StdMutex::new(None),
                #[cfg(test)]
                rendition_install_pause: StdMutex::new(None),
                #[cfg(test)]
                dormant_purge_pause: StdMutex::new(None),
                #[cfg(test)]
                terminal_route_test_outcomes: StdMutex::new(HashMap::new()),
            }),
        })
    }

    async fn try_cluster_fragment_index(
        &self,
        file: &MediaFile,
        video: CopyVideoOptions,
    ) -> Result<(FragmentIndex, String, String), String> {
        if video.preserves_dolby_vision() {
            return Err("the preserved Dolby Vision branch has no v2 artifact".to_owned());
        }
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return Err("the fragment-index engine changed; restart is required".to_owned());
        }
        let node_id = self
            .shared
            .cluster_node_id
            .as_deref()
            .ok_or_else(|| "this process has no cluster index identity".to_owned())?;
        let root = self
            .shared
            .cluster_index_root
            .as_deref()
            .ok_or_else(|| "this process has no cluster index cache root".to_owned())?;
        let object_version = crate::fragment_index_cluster::inspect_source(file).await?;
        let observation = self
            .shared
            .store
            .fragment_index_source(node_id, file.id, &object_version)
            .await
            .map_err(|error| format!("reading source attestation: {error}"))?
            .ok_or_else(|| "this node has not attested the current source object".to_owned())?;
        let engine = crate::ffmpeg::fragment_index_engine_digest().await;
        let pipeline = crate::fragment_index_cluster::pipeline_digest(file, &engine, video);
        let cache_key = plurx_core::store::cluster_fragment_index_key(
            file.id,
            file.size,
            file.mtime,
            &observation.source_sha256,
            &pipeline,
        )
        .ok_or_else(|| "source attestation contained an invalid digest".to_owned())?;
        let now = crate::fragment_index_cluster::unix_ms();
        let repair = plurx_core::store::NewClusterFragmentIndexJob {
            cache_key: cache_key.clone(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256: observation.source_sha256.clone(),
            pipeline_sha256: pipeline.clone(),
            priority: "foreground".to_owned(),
            trigger: "foreground".to_owned(),
            target_node_id: node_id.to_owned(),
            not_before_ms: now,
            created_at_ms: now,
        };
        let artifact = self
            .shared
            .store
            .cluster_fragment_index_artifact(&cache_key)
            .await
            .map_err(|error| format!("reading cluster index catalog: {error}"))?
            .filter(|artifact| {
                artifact.source_size == file.size
                    && artifact.source_sha256 == observation.source_sha256
                    && artifact.pipeline_sha256 == pipeline
            });
        let Some(artifact) = artifact else {
            let queued = self
                .shared
                .store
                .enqueue_cluster_fragment_index(&repair)
                .await
                .map_err(|error| format!("queueing the exact v2 artifact: {error}"))?;
            return Err(if queued {
                "the exact v2 artifact is queued".to_owned()
            } else {
                "the exact v2 artifact is awaiting queue admission".to_owned()
            });
        };
        let index = crate::fragment_index_cluster::hydrate(
            self.shared.store.as_ref(),
            self.shared.cluster_membership.as_ref(),
            node_id,
            root,
            &artifact,
        )
        .await?;
        let Some(index) = index else {
            let repair = repair_job_for_artifact(repair, &artifact);
            let _ = self
                .shared
                .store
                .requeue_cluster_fragment_index(&repair)
                .await;
            return Err("no verified holder could supply the v2 artifact".to_owned());
        };
        Ok((index, object_version, artifact.cache_key))
    }

    /// Keep VOD lifecycle telemetry on the same node-local event stream as
    /// every legacy presentation. The VOD registry is the only place every
    /// terminal path converges (client release, supersession, admin, revoked,
    /// replacement), so emitting here cannot miss one of those arms.
    fn emit_lifecycle(
        &self,
        session_id: &str,
        file_id: i64,
        height: i64,
        kind: SessionKind,
        event: &str,
        reason: Option<&str>,
    ) {
        crate::telemetry::emit(
            Arc::clone(&self.shared.store),
            plurx_core::domain::PlaybackEvent {
                at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                    .unwrap_or(0),
                session_id: Some(session_log_id(session_id)),
                file_id: Some(file_id),
                event: event.to_owned(),
                method: Some(
                    match kind {
                        SessionKind::Copy { .. } => "remux",
                        SessionKind::Transcode { .. } => "transcode",
                    }
                    .to_owned(),
                ),
                encoder: Some("vod".to_owned()),
                height: Some(height),
                suspended: Some(false),
                reason: reason.map(str::to_owned),
                extra: Some(r#"{"presentation":"vod"}"#.to_owned()),
                ..plurx_core::domain::PlaybackEvent::default()
            },
        );
    }

    fn emit_marker_prewarm(
        &self,
        session_id: &str,
        file_id: i64,
        kind: SessionKind,
        outcome: MarkerPrewarmOutcome,
    ) {
        let produced_range = outcome.produced_range.map(|range| {
            serde_json::json!({
                "first_entry": range.first,
                "last_entry": range.last,
            })
        });
        crate::telemetry::emit(
            Arc::clone(&self.shared.store),
            plurx_core::domain::PlaybackEvent {
                at_unix_ms: now_ms(),
                session_id: Some(session_log_id(session_id)),
                file_id: Some(file_id),
                event: "marker_prewarm".to_owned(),
                level: Some("info".to_owned()),
                method: Some(
                    match kind {
                        SessionKind::Copy { .. } => "remux",
                        SessionKind::Transcode { .. } => "transcode",
                    }
                    .to_owned(),
                ),
                encoder: Some("vod".to_owned()),
                detail: Some(if outcome.hit { "hit" } else { "miss" }.to_owned()),
                extra: Some(
                    serde_json::json!({
                        "presentation": "vod",
                        "marker": outcome.destination.kind.as_str(),
                        "destination_ms": outcome.destination.end_ms,
                        "destination_entry": outcome.destination.target_entry,
                        "requested_sequence": outcome.requested_sequence,
                        "produced_range": produced_range,
                    })
                    .to_string(),
                ),
                ..plurx_core::domain::PlaybackEvent::default()
            },
        );
    }

    /// The VOD arm of session create, called by the manager AFTER it has
    /// decided the request opts in (`presentation=="vod" && settings.enabled`).
    ///
    /// Registers a live session handle attached to a (created or resurrected)
    /// rendition and kicks its producer driver. A request that cannot be
    /// VOD-presented returns a stable refusal; it never changes presentation.
    /// `start_seconds` positions the first demand (the entry containing it),
    /// not the plan.
    pub async fn try_create(
        &self,
        req: &SessionRequest,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req,
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: None,
            },
        )
        .await
    }

    /// Cluster-only VOD creation. Unlike the legacy local entrypoint, this
    /// carries a serving admission captured before preparation and fences the
    /// final attachment against the corresponding quorum-loss transition.
    pub(crate) async fn try_create_cluster(
        &self,
        req: &SessionRequest,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        serving_admission: VodServingAdmission,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req,
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: Some(serving_admission),
            },
        )
        .await
    }

    /// Prepare a resurrection normally, then serialize only its final
    /// lifecycle/reader/registry attachment against public release. The
    /// release bit is checked while that exact transition is held, so slow
    /// Store/index/rendition work never delays a DELETE tombstone.
    pub(crate) async fn try_create_before_release(
        &self,
        req: &SessionRequest,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        release_fence: VodReleaseFence<'_>,
    ) -> Result<VodStart, String> {
        let _preparing = self.begin_preparing_session(&session_id);
        self.try_create_with_release_fence(
            req,
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: Some(release_fence),
                serving_admission: None,
            },
        )
        .await
    }

    pub(crate) fn begin_preparing_session(&self, session_id: &str) -> VodPreparationGuard {
        *self
            .shared
            .preparing_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_id.to_owned())
            .or_insert(0) += 1;
        VodPreparationGuard {
            shared: Arc::clone(&self.shared),
            session_id: session_id.to_owned(),
        }
    }

    pub(crate) async fn owns_or_preparing(&self, session_id: &str) -> bool {
        if self
            .shared
            .preparing_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(session_id)
        {
            return true;
        }
        self.shared.sessions.lock().await.contains_key(session_id)
    }

    pub(crate) async fn live_or_preparing_session_ids(&self) -> Vec<String> {
        let mut ids = self
            .shared
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| session.tombstone.is_none())
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            self.shared
                .preparing_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .cloned(),
        );
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    async fn try_create_with_release_fence(
        &self,
        req: &SessionRequest,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        fences: VodCreateFences<'_>,
    ) -> Result<VodStart, String> {
        // The one funnel every create passes through: the plain entry point,
        // the cluster one that every shipped caller actually uses, and the
        // resurrection of a session from its durable route. Applying the
        // ceiling here rather than at construction is what lets an operator
        // change it without a restart — a cap set only in `WaitPool::new`
        // would be frozen at whatever the node booted with — and applying it
        // at `try_create` alone reached no production path at all.
        self.shared.pool.set_global_cap(settings.blocked_get_cap);
        let SessionKind::Copy {
            aac,
            preserve_dolby_vision,
            convert_dolby_vision,
        } = req.kind
        else {
            return Err(crate::transcode::vod_refusal_error(
                "vod_transcode_unavailable",
                "transcode serving is gated on the D6 device measurement",
            ));
        };
        if req.subtitle_burn.is_some() {
            return Err(crate::transcode::vod_refusal_error(
                "vod_subtitle_burn_unavailable",
                "a subtitle burn changes the video pipeline",
            ));
        }
        // A NULL/unprobed duration cannot be described by a closed film-time
        // playlist. Refuse it honestly; the removed live presentation is not
        // a substitute.
        let Some(duration_ms) = file.duration_ms.filter(|ms| *ms > 0) else {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "the file has no probed duration, so no immutable plan can be built",
            ));
        };
        let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
        let probe_json = self
            .shared
            .store
            .get_file_probe_json(file.id)
            .await
            .map_err(|error| format!("reading the file probe: {error}"))?;
        let video = copy_video_pipeline(
            file,
            probe_json.as_deref(),
            have_dovi,
            preserve_dolby_vision,
            convert_dolby_vision,
        );
        let identity = crate::fragindex::identity_for(file, video);
        let cluster_cache_enabled = self
            .shared
            .store
            .get_setting(plurx_core::store::keys::VOD_INDEX_CLUSTER_CACHE)
            .await
            .map_err(|error| format!("reading the cluster index gate: {error}"))?
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let cluster_index = if cluster_cache_enabled {
            self.try_cluster_fragment_index(file, video).await.map(Some)
        } else {
            Ok(None)
        };
        let (index, source_object_version, cluster_cache_key) = match cluster_index {
            Ok(Some((index, object_version, cache_key))) => {
                (Some(index), Some(object_version), Some(cache_key))
            }
            Ok(None) => (
                self.shared
                    .store
                    .fragment_index(file.id, &identity)
                    .await
                    .map_err(|error| format!("reading the fragment index: {error}"))?,
                None,
                None,
            ),
            Err(reason) => {
                // The first rollout phase is write/shadow plus prefer-v2.
                // Per-key fallback preserves an already healthy v1 title
                // until this exact source/pipeline key is fully available.
                tracing::debug!(file_id = file.id, %reason, "v2 fragment index unavailable; using v1");
                (
                    self.shared
                        .store
                        .fragment_index(file.id, &identity)
                        .await
                        .map_err(|error| format!("reading the fragment index: {error}"))?,
                    None,
                    None,
                )
            }
        };
        let Some(index) = index else {
            return Err(crate::transcode::vod_refusal_error(
                "vod_index_pending",
                "no fragment index stored for the file's current identity",
            ));
        };
        // The §2 ruling: a single immutable init cannot describe a film whose
        // clean fragments carry varying parameter sets, so the verdict is a
        // scan-time fallback here, never a producer_failed mid-playback.
        if !index.parameter_sets_constant {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "its parameter sets vary mid-film (the §2 ruling)",
            ));
        }
        let recipe = Recipe {
            file: file.clone(),
            audio_index: req.audio_index,
            aac,
            video,
            source_object_version,
            cluster_cache_key,
        };
        let key = rendition_key(&recipe, &identity);
        let attachment = self
            .shared
            .attach_rendition(&key, &identity, index, recipe, duration_ms, settings)
            .await?
            .ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_source_unsupported",
                    "the fragment index produced an empty VOD plan",
                )
            })?;
        let rendition = Arc::clone(&attachment.rendition);

        let start_entry = entry_containing(&rendition.plan, req.start_seconds);
        let marker_destinations = stored_marker_destinations(
            self.shared.store.as_ref(),
            file,
            &rendition.plan,
            rendition.seconds_per_segment,
        )
        .await;
        let _release_transition = if let Some(release_fence) = fences.release_fence {
            let guard = release_fence.transition.lock_owned().await;
            if release_fence.released.load(Acquire) {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_session_released",
                    "the VOD session was released before attachment",
                ));
            }
            Some(guard)
        } else {
            None
        };
        let lifecycle = self.shared.session_lifecycle(&session_id);
        let _lifecycle = lifecycle.lock().await;
        let duration_ms = plan_duration_ms(&rendition.plan);
        let replacement = Session {
            rendition: Some(Arc::clone(&rendition)),
            rendition_key: rendition.key.clone(),
            file: Arc::new(rendition.recipe.file.clone()),
            playback_id: req.playback_id.clone(),
            user_name: attribution.user_name.to_owned(),
            item_title: attribution.item_title.to_owned(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
                .unwrap_or(0),
            target_height: file.height.unwrap_or(0),
            kind: req.kind,
            supersession_user: attribution.supersession_user.to_owned(),
            block_budget: settings.block_budget,
            lifecycle: Arc::clone(&lifecycle),
            incarnation: Arc::new(()),
            last_touch: StdMutex::new(Instant::now()),
            delivery: Arc::new(crate::meter::Meter::new()),
            control: StdMutex::new(crate::playback_control::ControlState::default()),
            marker_destinations,
            last_control_snapshot: None,
            control_end: None,
            control_end_snapshot: None,
            terminal_cleanup: None,
            tombstone: None,
        };

        // Acquire every async lock before changing either the reader graph or
        // the registry. Cancellation while resurrecting is therefore a clean
        // no-op; after the final lock is acquired the attachment replacement
        // is one synchronous transaction with no partial externally visible
        // state.
        let mut sessions = self.shared.sessions.lock().await;
        if sessions
            .get(&session_id)
            .is_some_and(|session| session.tombstone.is_some())
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_session_ended",
                "the VOD session already has a terminal tombstone",
            ));
        }
        let previous_rendition = sessions
            .get(&session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        let mut previous_readers = match previous_rendition.as_ref() {
            Some(previous) if !Arc::ptr_eq(previous, &rendition) => {
                Some(previous.readers.lock().await)
            }
            _ => None,
        };
        // Set only where this attachment actually moves the viewer off another
        // rendition. Stopping the flight has to happen after the guards below
        // are dropped — settlement waits on a real ffmpeg, and holding a
        // rendition-wide lock across that would let one viewer's recipe change
        // stall every other reader of the same rendition — so what crosses the
        // drop is the flight's identity, not the decision to stop it.
        let mut obsolete_window_flight: Option<u64> = None;
        let mut replacement_readers = rendition.readers.lock().await;
        let _serving_transition = if let Some(admission) = fences.serving_admission.as_ref() {
            Some(admission.commit_guard_before().await.ok_or_else(|| {
                crate::transcode::serving_fence_error(crate::serving_fence::SERVING_FENCED_MESSAGE)
            })?)
        } else {
            None
        };
        if let Some(previous_readers) = previous_readers.as_mut() {
            previous_readers.remove(&session_id);
            if previous_readers.is_empty() {
                if let Some(previous) = previous_rendition.as_ref() {
                    *previous.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
                }
            }
            // Reattachment removes the reader here rather than through
            // `detach_reader`, so it needs the same wait retirement: this
            // viewer has moved to another rendition, and any request still
            // parked on the one they left has no reader to be ranked against.
            // `playback_demands` would mark it foreground on exactly that
            // absence and keep the old rendition producing for somebody who
            // is no longer watching it.
            if let Some(previous) = previous_rendition.as_ref() {
                self.shared.pool.retire_session(&previous.key, &session_id);
            }
            // The third thing `detach_reader` does, which this path was
            // missing, and which it cannot do the same way. A subtitle window
            // is keyed by session id alone, so the flight this viewer left
            // behind is a live ffmpeg extracting a span for the recipe they
            // just moved off — worth stopping, and `detach_reader`'s comment
            // claims every ending converges here to stop it.
            //
            // It cannot call `release_session_window`, because releasing also
            // fences the id for half a minute and this id belongs to a viewer
            // who is still watching: the fence would refuse the first window
            // of the attachment replacing it and turn a recipe change into
            // half a minute without subtitles. It names the exact flight
            // instead, read here under the same guard that decides it is
            // obsolete, so a successor claiming the session between this read
            // and the stop is left alone.
            obsolete_window_flight = crate::subtitles::session_window_flight(&session_id);
        }
        replacement_readers.insert(session_id.clone(), Reader::new(start_entry));
        *rendition.dormant_since.lock().expect("dormant lock") = None;
        // A live entry with this id is replaced rather than refused, and the
        // replacement carries a fresh `ControlState` — so a staged M6
        // successor belonging to the outgoing attachment would simply vanish
        // while its durable row lived on. Abort it first, and note that the
        // replacement mints a new `incarnation`: a gate held for the outgoing
        // attachment is pinned to the old one and refuses everything from
        // here, rather than taking the newcomer's slot.
        if let Some(outgoing) = sessions.get(&session_id) {
            outgoing.abort_staged_preparation();
        }
        sessions.insert(session_id.clone(), replacement);
        // Both reader graphs are committed, so the rendition-wide guards have
        // no further work. They used to live to the end of the function, which
        // was free while nothing here awaited; the window stop below does, and
        // settlement waits on a real ffmpeg. Holding either guard across that
        // would let one viewer's recipe change stall every other reader of the
        // rendition they left or the one they joined.
        drop(replacement_readers);
        drop(previous_readers);
        drop(_serving_transition);
        drop(sessions);
        if let Some(flight) = obsolete_window_flight {
            crate::subtitles::abandon_session_window(&session_id, flight).await;
        }
        self.emit_lifecycle(
            &session_id,
            file.id,
            file.height.unwrap_or(0),
            req.kind,
            "session_start",
            None,
        );
        drop(_lifecycle);
        // The reader graph and registry are now one committed attachment.
        // Release does not wait for producer wakeup, tracing or lifecycle
        // event publication, and can tombstone this exact incarnation before
        // the caller attempts to acquire a response owner.
        drop(_release_transition);
        // Reader graph and registry now own the exact handle. Releasing the
        // per-key build gate before this point would let a dormant purge
        // remove it in the lookup/attach gap.
        drop(attachment);
        rendition.kick();
        tracing::info!(
            session = %session_log_id(&session_id),
            rendition = %rendition.key,
            file = file.id,
            "vod session attached (start entry {start_entry})"
        );
        Ok(VodStart {
            session_id,
            duration_ms,
        })
    }

    /// Every active VOD handle for the operator activity/session inventory.
    /// Tombstones remain addressable long enough to return their typed 410,
    /// but they are no longer deliveries and therefore stay out of this list.
    pub async fn delivery_infos(&self) -> Vec<VodDeliveryInfo> {
        self.delivery_infos_bounded(usize::MAX).await
    }

    /// Newest active VOD handles, bounded before they leave the registry.
    /// Cluster activity snapshots use this form so a node cannot make peer
    /// diagnostics enumerate more sessions than the wire contract can carry.
    pub async fn delivery_infos_bounded(&self, limit: usize) -> Vec<VodDeliveryInfo> {
        let sessions = self.shared.sessions.lock().await;
        let mut infos = sessions
            .iter()
            .filter(|(_, session)| session.tombstone.is_none())
            .filter_map(|(id, session)| {
                session.live_rendition()?;
                Some(VodDeliveryInfo {
                    id: id.clone(),
                    file_id: session.file.id,
                    item_id: session.file.item_id,
                    item_title: session.item_title.clone(),
                    user_name: session.user_name.clone(),
                    target_height: session.target_height,
                    started_unix: session.started_unix,
                    idle_seconds: session
                        .last_touch
                        .lock()
                        .expect("touch lock")
                        .elapsed()
                        .as_secs(),
                    delivered_bytes: session.delivery.total_bytes(),
                    delivered_bps: session.delivery.recent_bps().map(|bytes| bytes * 8),
                    delivered_idle_ms: session.delivery.idle_for_ms(),
                })
            })
            .collect::<Vec<_>>();
        infos.sort_by(|left, right| {
            right
                .started_unix
                .cmp(&left.started_unix)
                .then(left.id.cmp(&right.id))
        });
        infos.truncate(limit);
        infos
    }

    pub async fn active_sessions(&self) -> usize {
        self.shared
            .sessions
            .lock()
            .await
            .values()
            .filter(|session| session.tombstone.is_none())
            .count()
    }

    /// Consume the shipped clients' historical hard-coded marker miss only
    /// when it can be correlated to exactly one live VOD playback that was
    /// armed while inside a stored marker. The beacon is the durable skip
    /// intent that transient `Seeking` snapshots cannot provide: reporters
    /// may coalesce those away before the next control exchange.
    pub async fn consume_marker_prewarm_placeholder(
        &self,
        user_id: i64,
        file_id: i64,
        method: &str,
    ) -> bool {
        if !matches!(method, "remux" | "transcode") {
            return false;
        }
        let user_scope = serde_json::json!(["user_id", user_id]).to_string();
        let candidates = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| {
                    session.file.id == file_id && session.supersession_user == user_scope
                })
                .map(|(session_id, session)| {
                    (
                        session_id.clone(),
                        session.live_rendition().cloned(),
                        session.kind,
                    )
                })
                .collect::<Vec<_>>()
        };

        let [(session_id, rendition, kind)] = candidates.as_slice() else {
            // Ambiguity is a miss, never permission to steal another
            // playback's credit. Direct play and rolling HLS also land here.
            return false;
        };
        let Some(rendition) = rendition else {
            // A recent terminal VOD row can own a delayed beacon even though
            // its reader is already gone. Keep the miss rather than letting a
            // surviving same-file session steal it.
            return false;
        };
        if !matches!(
            (*kind, method),
            (SessionKind::Copy { .. }, "remux") | (SessionKind::Transcode { .. }, "transcode")
        ) {
            return false;
        }
        let reader_facts = {
            let readers = rendition.readers.lock().await;
            readers
                .get(session_id)
                .map(|reader| Arc::clone(&reader.marker_prewarm))
        };
        let Some(ledger) = reader_facts else {
            return false;
        };
        if !ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .can_match_client_skip()
        {
            return false;
        }

        let manifest = rendition.manifest.lock().await;
        let publications = rendition
            .publication_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = ledger.note_client_skip(&manifest, &publications);
        drop(ledger);
        drop(publications);
        drop(manifest);
        if let Some(outcome) = result.outcome {
            self.emit_marker_prewarm(session_id, file_id, *kind, outcome);
        }
        result.matched
    }

    /// Renew one immutable session only after its caller has a concrete
    /// playlist, subtitle, or media response ready. Kept separate from
    /// lookup so a vanished/tombstoned capability cannot be mistaken for a
    /// successful rolling-to-VOD fallthrough.
    pub async fn commit_resolved_media(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        segment_index: Option<u32>,
    ) -> bool {
        // End/reattach uses this same per-incarnation gate. Whichever enters
        // first finishes its whole transition before the other can inspect
        // the registry, so a served frontier cannot reappear after a
        // tombstone detached its reader.
        let _lifecycle = owner.lifecycle.lock().await;
        let mut sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return false;
        };
        if session.tombstone.is_some()
            || owner.tombstone.is_some()
            || !Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            || !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
        {
            return false;
        }
        let (Some(session_rendition), Some(owner_rendition)) =
            (session.rendition.as_ref(), owner.rendition.as_ref())
        else {
            return false;
        };
        if !Arc::ptr_eq(session_rendition, owner_rendition) {
            return false;
        }
        let mut readers = if segment_index.is_some() {
            Some(owner_rendition.readers.lock().await)
        } else {
            None
        };

        // Every await is above this line. Touch and frontier advance are one
        // cancellation-safe commit: EOF can never renew a session without
        // also recording the exact served frontier (or vice versa).
        *session.last_touch.lock().expect("touch lock") = Instant::now();
        if let (Some(index), Some(readers)) = (segment_index, readers.as_mut()) {
            if let Some(reader) = readers.get_mut(session_id) {
                reader.served(index);
            }
        }
        let marker_ledger = segment_index.and_then(|_| {
            readers
                .as_ref()?
                .get(session_id)
                .map(|reader| Arc::clone(&reader.marker_prewarm))
        });
        let marker_identity = (session.file.id, session.kind);
        drop(readers);
        drop(sessions);
        if let (Some(index), Some(ledger)) = (segment_index, marker_ledger) {
            let manifest = owner_rendition.manifest.lock().await;
            let publications = owner_rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let outcome = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settle_pending_at(index, &manifest, &publications);
            drop(publications);
            drop(manifest);
            if let Some(outcome) = outcome {
                self.emit_marker_prewarm(session_id, marker_identity.0, marker_identity.1, outcome);
            }
        }
        owner_rendition.kick();
        true
    }

    /// Confirm that a response token still names the live attachment without
    /// extending its lease. Streamed bodies use this immediately before
    /// publishing headers, then perform the mutating commit only after EOF.
    pub async fn response_owner_is_live(&self, session_id: &str, owner: &ResponseOwner) -> bool {
        let _lifecycle = owner.lifecycle.lock().await;
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session.tombstone.is_none()
            && owner.tombstone.is_none()
            && Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            && Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            && session.rendition.as_ref().is_some_and(|rendition| {
                owner
                    .rendition
                    .as_ref()
                    .is_some_and(|owner| Arc::ptr_eq(rendition, owner))
            })
    }

    /// Frozen source facts carried by this exact VOD response owner. HTTP may
    /// prepare a representation from them before final owner admission without
    /// consulting whichever attachment currently reuses the public id.
    pub(crate) fn response_owner_file(&self, owner: &ResponseOwner) -> MediaFile {
        owner.file.as_ref().clone()
    }

    /// Admit a typed VOD status against the exact live-or-terminal snapshot
    /// that produced it, without renewing the session or publishing media.
    pub async fn response_status_owner_is_current(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
    ) -> bool {
        let _lifecycle = owner.lifecycle.lock().await;
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session.tombstone == owner.tombstone
            && Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            && Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            && session.rendition_key == owner.rendition_key
            && (session.tombstone.is_some()
                || session.rendition.as_ref().is_some_and(|rendition| {
                    owner
                        .rendition
                        .as_ref()
                        .is_some_and(|owner| Arc::ptr_eq(rendition, owner))
                }))
    }

    /// Immutable playlist bytes: the same bytes for the session's whole life.
    pub async fn playlist(&self, session_id: &str) -> Option<VodPublication<Vec<u8>>> {
        let publication = self.session_rendition(session_id).await?;
        let (result, owner) = match publication.result {
            Ok((rendition, _, _)) => (Ok(rendition.playlist.clone()), publication.owner),
            Err(error) => (Err(error), publication.owner),
        };
        Some(VodPublication { result, owner })
    }

    /// The three-outcome segment GET (plan §2.3). The outer `None` means this
    /// is not a VOD session. `result: Ok(None)` is a genuine miss from one
    /// exact attachment; HTTP must retain the adjacent owner through its
    /// bodyless 404 admission instead of reconstructing identity from the
    /// reusable session id.
    pub async fn segment(
        &self,
        session_id: &str,
        name: &str,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        let publication = self.session_rendition(session_id).await?;
        let (rendition, budget, delivery) = match publication.result {
            Ok(found) => found,
            Err(error) => {
                return Some(VodPublication {
                    result: Err(error),
                    owner: publication.owner,
                })
            }
        };
        let owner = publication.owner;
        if name == INIT_NAME {
            return Some(VodPublication {
                result: self
                    .serve_init(&rendition, budget, delivery)
                    .await
                    .map(Some),
                owner,
            });
        }
        let Some(index) = planned_index(name) else {
            // Traversal names and everything else that is not `segNNNNN.m4s`
            // fail the same digit discipline `is_safe_segment` enforces.
            return Some(VodPublication {
                result: Ok(None),
                owner,
            });
        };
        if index as usize >= rendition.plan.len() {
            return Some(VodPublication {
                result: Ok(None),
                owner,
            });
        }
        Some(VodPublication {
            result: self
                .serve_segment(&rendition, session_id, index, budget, delivery)
                .await
                .map(Some),
            owner,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_terminal_cleanup(
        &self,
        session_id: String,
        cleanup: Arc<TerminalCleanup>,
        rendition: Arc<Rendition>,
        file_id: i64,
        height: i64,
        kind: SessionKind,
        cause: Terminal,
        terminal_commit: Option<Box<crate::playback_control::TerminalCommitReceipt>>,
    ) {
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let _completion = TerminalCleanupGuard(Arc::clone(&cleanup));
            #[cfg(test)]
            let terminal_detach_pause = shared
                .terminal_detach_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(pause) = terminal_detach_pause {
                pause.wait().await;
                pause.wait().await;
            }
            rendition.detach_reader(&shared.pool, &session_id).await;
            rendition.kick();
            let serve = VodServe { shared };
            serve.emit_lifecycle(
                &session_id,
                file_id,
                height,
                kind,
                "session_end",
                Some(terminal_reason(cause)),
            );
            tracing::info!(
                session = %session_log_id(&session_id),
                rendition = %rendition.key,
                "vod session ended for good: {cause:?}"
            );
            if let Some(terminal_commit) = terminal_commit {
                terminal_commit.retry();
                let _ = terminal_commit.wait().await;
            }
            // This task owns the exact cleanup identity. Compact before its
            // guard publishes completion so every waiter observing finished
            // cleanup also observes that the registry no longer retains the
            // rendition/media/process graph. A resurrected replacement has a
            // different cleanup pointer and is left untouched.
            let mut sessions = serve.shared.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                let exact_cleanup = session
                    .terminal_cleanup
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cleanup));
                let exact_rendition = session
                    .rendition
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &rendition));
                if session.tombstone.is_some() && exact_cleanup && exact_rendition {
                    session.rendition = None;
                }
            }
            drop(sessions);
            // `TerminalCleanupGuard` publishes completion on drop. Release
            // the task's own heavyweight captures first so a waiter that
            // observes completion cannot still race this future's teardown.
            drop(rendition);
            drop(serve);
        });
    }

    /// End one session for good with a cause; `true` if it was ours.
    /// Idempotent for authoritative causes. `Replaced` is also the fail-closed
    /// provisional cause used when lease reconciliation cannot yet read the
    /// durable winner; a later non-Replaced Store cause may refine only that
    /// placeholder, never another concrete first writer.
    async fn begin_end(&self, session_id: &str, cause: Terminal) -> Option<Arc<TerminalCleanup>> {
        let (lifecycle, incarnation) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            (
                Arc::clone(&session.lifecycle),
                Arc::clone(&session.incarnation),
            )
        };
        let _lifecycle = lifecycle.lock().await;
        let (cleanup, work) = {
            let mut sessions = self.shared.sessions.lock().await;
            let session = sessions.get_mut(session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle)
                || !Arc::ptr_eq(&session.incarnation, &incarnation)
            {
                return None;
            }
            let terminal_cause = match session.tombstone {
                Some(Terminal::Replaced) if cause != Terminal::Replaced => {
                    tracing::info!(
                        session = %session_log_id(session_id),
                        durable_cause = cause.durable_reason(),
                        "refined provisional VOD terminal cause from durable route"
                    );
                    session.tombstone = Some(cause);
                    cause
                }
                Some(existing) => existing,
                None => {
                    session.tombstone = Some(cause);
                    cause
                }
            };
            // Idempotent, and correct on the refinement branch too: the slot
            // was already aborted when the provisional tombstone was written.
            session.abort_staged_preparation();
            if let Some(cleanup) = session.terminal_cleanup.as_ref() {
                (Arc::clone(cleanup), None)
            } else {
                let cleanup = Arc::new(TerminalCleanup::new());
                session.terminal_cleanup = Some(Arc::clone(&cleanup));
                match session.rendition.as_ref().map(Arc::clone) {
                    Some(rendition) => {
                        let work = (
                            session_id.to_owned(),
                            rendition,
                            session.file.id,
                            session.target_height,
                            session.kind,
                            terminal_cause,
                        );
                        (cleanup, Some(work))
                    }
                    // A session whose rendition is already gone has nothing to
                    // detach, so this cleanup owns no work and has to say so
                    // here. Leaving it standing unfinished is far worse than
                    // the ending it is refusing: `maintain` waits on every
                    // unfinished cleanup belonging to a tombstoned session
                    // with nothing bounding the wait, so one of these stops
                    // the idle reap, the dormant purge, the tombstone eviction
                    // and every driver kick — node-wide, for good.
                    //
                    // Today the compaction that clears `rendition` writes the
                    // tombstone first under the same pointer identity, so this
                    // arm is unreachable and the assertion states that rather
                    // than trusting it. The completion is what a later change
                    // to that ordering lands on instead of a wedged node.
                    None => {
                        debug_assert!(
                            session.tombstone.is_some(),
                            "a VOD session lost its rendition without a tombstone"
                        );
                        cleanup.complete();
                        (cleanup, None)
                    }
                }
            }
        };
        if let Some((session_id, rendition, file_id, height, kind, cause)) = work {
            self.spawn_terminal_cleanup(
                session_id,
                Arc::clone(&cleanup),
                rendition,
                file_id,
                height,
                kind,
                cause,
                None,
            );
        }
        Some(cleanup)
    }

    pub async fn end(&self, session_id: &str, cause: Terminal) -> bool {
        let Some(cleanup) = self.begin_end(session_id, cause).await else {
            return false;
        };
        cleanup.wait().await;
        true
    }

    /// Install the exact terminal tombstone and transfer reader/event/commit
    /// cleanup to its detached owner without waiting for physical settlement.
    /// Public release uses this before submitting the durable Store End, so a
    /// slow reader detach cannot keep the replicated route active.
    pub(crate) async fn begin_end_detached(&self, session_id: &str, cause: Terminal) -> bool {
        self.begin_end(session_id, cause).await.is_some()
    }

    /// Supersession sweep: end every session with this viewer's
    /// `playback_id` except `keep` (cause [`Terminal::Superseded`]). Called
    /// by the manager on create — for a VOD create AND a legacy one, because
    /// a viewer switching presentations is still one player replacing its own
    /// stream. Scoped by the same user string the legacy sweep uses, so a
    /// colliding `playback_id` from another account ends nothing.
    #[cfg(test)]
    async fn supersede(&self, supersession_user: &str, playback_id: &str, keep: &str) -> usize {
        self.supersede_before(supersession_user, playback_id, keep, None)
            .await
            .expect("an unbounded VOD supersession cannot reach a deadline")
    }

    /// Transfer every matching predecessor to terminal cleanup ownership
    /// before returning. Every exact victim lifecycle is acquired within the
    /// deadline before the first tombstone, so timeout/cancellation is a whole
    /// no-op and the final registry mutation plus cleanup handoff is atomic.
    pub async fn supersede_before(
        &self,
        supersession_user: &str,
        playback_id: &str,
        keep: &str,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<usize, VodSupersedeError> {
        let collect = async {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(id, session)| {
                    session.playback_id == playback_id
                        && session.supersession_user == supersession_user
                        && id.as_str() != keep
                        && session.tombstone.is_none()
                })
                .map(|(id, session)| {
                    (
                        id.clone(),
                        Arc::clone(&session.lifecycle),
                        Arc::clone(&session.incarnation),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut victims = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, collect)
                .await
                .map_err(|_| VodSupersedeError::Deadline)?,
            None => collect.await,
        };
        victims.sort_by(|left, right| left.0.cmp(&right.0));
        // Own every exact victim lifecycle before the first mutation. A
        // deadline/cancellation can therefore only happen while the whole
        // operation is still a no-op; after all locks are held, tombstoning,
        // cleanup ownership and registry publication are synchronous.
        let mut lifecycle_guards = Vec::with_capacity(victims.len());
        for (_, lifecycle, _) in &victims {
            let lock = Arc::clone(lifecycle).lock_owned();
            let guard = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, lock)
                    .await
                    .map_err(|_| VodSupersedeError::Deadline)?,
                None => lock.await,
            };
            lifecycle_guards.push(guard);
        }
        let sessions_lock = self.shared.sessions.lock();
        let mut sessions = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, sessions_lock)
                .await
                .map_err(|_| VodSupersedeError::Deadline)?,
            None => sessions_lock.await,
        };
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(VodSupersedeError::Deadline);
        }
        let mut work = Vec::new();
        for (id, lifecycle, incarnation) in victims {
            let Some(session) = sessions.get_mut(&id) else {
                continue;
            };
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle)
                || !Arc::ptr_eq(&session.incarnation, &incarnation)
                || session.tombstone.is_some()
            {
                continue;
            }
            session.tombstone = Some(Terminal::Superseded);
            session.abort_staged_preparation();
            let cleanup = Arc::new(TerminalCleanup::new());
            session.terminal_cleanup = Some(Arc::clone(&cleanup));
            let Some(rendition) = session.rendition.as_ref().map(Arc::clone) else {
                // Same hazard as `begin_end`'s: this victim now carries a
                // tombstone and a cleanup, and skipping to the next one would
                // leave that cleanup unfinished forever, which is the one
                // thing `maintain` waits on without a bound. Nothing to
                // detach means finished, not abandoned.
                cleanup.complete();
                continue;
            };
            work.push((
                id,
                cleanup,
                rendition,
                session.file.id,
                session.target_height,
                session.kind,
            ));
        }
        drop(sessions);
        let ended = work.len();
        for (id, cleanup, rendition, file_id, height, kind) in work {
            self.spawn_terminal_cleanup(
                id,
                cleanup,
                rendition,
                file_id,
                height,
                kind,
                Terminal::Superseded,
                None,
            );
        }
        drop(lifecycle_guards);
        Ok(ended)
    }

    /// Blocked-GET admission counters for the operator surfaces.
    pub(crate) fn blocked_get_metrics_handle(&self) -> Arc<crate::waitpool::BlockedGetMetrics> {
        self.shared.pool.metrics_handle()
    }

    /// Whether this session id is (or ever was) a VOD session here.
    pub async fn owns(&self, session_id: &str) -> bool {
        self.shared.sessions.lock().await.contains_key(session_id)
    }

    /// Every registered session id, tombstoned included — "still addressed
    /// here" is the fact the caller needs, not "still playing".
    pub async fn session_ids(&self) -> Vec<String> {
        self.shared.sessions.lock().await.keys().cloned().collect()
    }

    /// Session ids still live (tombstoned excluded) — what the durable lease
    /// loop may renew.
    // TODO(m3-wire): the allow comes out when the lease loop wiring lands.
    #[allow(dead_code)]
    pub async fn live_session_ids(&self) -> Vec<String> {
        self.shared
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| session.tombstone.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// What a lease renewal reports for one live session: the film-time end,
    /// in ms, of the last segment it was served (0 before the first). `None`
    /// for unknown/tombstoned.
    // TODO(m3-wire): the allow comes out when the lease loop wiring lands.
    #[allow(dead_code)]
    pub async fn frontier_ms(&self, session_id: &str) -> Option<i64> {
        let rendition = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            session.live_rendition().map(Arc::clone)?
        };
        let last_served = {
            let readers = rendition.readers.lock().await;
            readers
                .get(session_id)
                .and_then(|reader| reader.last_served)
        };
        Some(match last_served {
            None => 0,
            Some(index) => rendition
                .plan
                .entry(index)
                .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
                .unwrap_or(0),
        })
    }

    /// VOD-native health for the web diagnostics. Like the live session
    /// status read, this does not touch the session TTL: looking at a panel is
    /// not an authorized media GET and must not keep an abandoned handle alive.
    pub async fn status(&self, session_id: &str) -> Option<VodSessionInfo> {
        self.status_publication(session_id)
            .await
            .and_then(|publication| publication.result.ok())
    }

    /// Status plus the exact VOD incarnation that produced it. HTTP performs
    /// final serving/release admission against this owner, so a reap and
    /// resurrection using the same public id cannot publish stale telemetry.
    pub(crate) async fn status_publication(
        &self,
        session_id: &str,
    ) -> Option<VodPublication<VodSessionInfo>> {
        let (rendition, target_height, owner, delivery) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            (
                session.live_rendition().map(Arc::clone)?,
                session.target_height,
                session.response_owner(),
                Arc::clone(&session.delivery),
            )
        };
        let last_served = rendition
            .readers
            .lock()
            .await
            .get(session_id)
            .and_then(|reader| reader.last_served);
        let (
            materialized_segments,
            planned_segments,
            materialized_bytes,
            planned_bytes,
            admitted,
            published_end_ms,
            ready_ahead_end_ms,
        ) = {
            let manifest = rendition.manifest.lock().await;
            let end_of_contiguous_run = |start: u32| {
                (start..manifest.len() as u32)
                    .take_while(|index| {
                        manifest
                            .state(*index)
                            .is_some_and(SegState::is_materialized)
                    })
                    .last()
                    .and_then(|index| rendition.plan.entry(index))
                    .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
            };
            // A far-seek may materialize one segment after a large hole. The
            // highest numbered file is not a publish frontier and must not be
            // reported as hours of runway; only contiguous bytes count.
            let published = end_of_contiguous_run(0);
            let ready_ahead = end_of_contiguous_run(last_served.unwrap_or(0));
            (
                manifest.materialized_count(),
                manifest.len(),
                manifest.materialized_bytes(),
                manifest.planned_bytes(),
                manifest.is_admitted(),
                published,
                ready_ahead,
            )
        };
        let failed = rendition.failure();
        let belief = rendition.slot.belief().await;
        let complete = planned_segments > 0 && materialized_segments == planned_segments;
        let (producer_state, producer_hold, suspended) = if failed.is_some() {
            ("failed", None, false)
        } else if complete {
            ("complete", None, false)
        } else {
            match belief {
                Producer::Running { .. } => ("running", None, false),
                Producer::Stopped { reason, .. } => (
                    "held",
                    Some(match reason {
                        crate::prodsched::Hold::Ahead { .. } => "ahead",
                        crate::prodsched::Hold::WorkingSetFull { .. } => "working_set",
                        crate::prodsched::Hold::NoRoom { .. } => "no_room",
                    }),
                    true,
                ),
                // A capacity hold outlives the producer it terminated, so a
                // rendition that wants to produce and cannot still says why.
                Producer::Absent { .. } => {
                    match *rendition.capacity_hold.lock().expect("capacity hold") {
                        Some(crate::prodsched::Hold::NoRoom { .. }) => {
                            ("waiting", Some("no_room"), false)
                        }
                        Some(crate::prodsched::Hold::WorkingSetFull { .. }) => {
                            ("waiting", Some("working_set"), false)
                        }
                        _ => ("waiting", None, false),
                    }
                }
            }
        };
        let fetched_end_ms = last_served
            .and_then(|index| rendition.plan.entry(index))
            .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
            .unwrap_or(0);
        Some(VodPublication {
            result: Ok(VodSessionInfo {
                id: session_id.to_owned(),
                file_id: rendition.recipe.file.id,
                target_height,
                encoder: "vod",
                playlist_shape: "vod",
                producer_state,
                producer_hold,
                producer_failed: failed.as_ref().map(|f| f.cause.clone()),
                producer_decision: failed.as_ref().map(|f| f.decision.status()),
                published_end_ms,
                ready_ahead_end_ms,
                fetched_end_ms,
                fetched_segment: last_served.map(i64::from),
                ahead_seconds: ready_ahead_end_ms.map(|end| (end - fetched_end_ms).max(0) / 1000),
                materialized_segments,
                planned_segments,
                materialized_bytes,
                planned_bytes,
                working_set_bytes: self.shared.working_set.load(Relaxed),
                working_set_budget_bytes: rendition.working_set_budget,
                completed_cache_bytes: self.shared.completed_cache.load(Relaxed),
                admitted,
                // Bytes per second becomes bits per second here, the same
                // conversion and the same units the rolling view publishes.
                delivered_bytes: delivery.total_bytes(),
                delivered_bps: delivery.recent_bps().map(|bytes| bytes * 8),
                delivered_idle_ms: delivery.idle_for_ms(),
                suspended,
                final_: complete,
            }),
            owner,
        })
    }

    /// Apply one fenced control exchange without conflating a replay or stale
    /// request with a media-object touch. Only a newly accepted non-terminal
    /// sequence moves the existing five-minute VOD activity clock. A fresh
    /// `demand=end` instead tombstones the attachment under this same lifecycle
    /// gate and retains its exact response for an idempotent retry.
    #[cfg(test)]
    pub(crate) async fn control(
        &self,
        control: crate::playback_control::LocalControlRequest<'_>,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        self.control_with_terminal(control, i64::MAX, None, None)
            .await
    }

    pub(crate) async fn control_with_terminal(
        &self,
        control: crate::playback_control::LocalControlRequest<'_>,
        deadline_unix_ms: i64,
        terminal_committer: Option<Arc<dyn crate::playback_control::TerminalControlCommitter>>,
        preparation_admission: Option<
            Arc<dyn crate::playback_control::PreparationSettlementAdmission>,
        >,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        // Resolve registry ownership before durable I/O, then keep this
        // session's gate held from the authority read through sequence
        // acceptance and any terminal detach. `end` and idle reap take the
        // same per-session gate, so neither can cross the linearization point.
        let lifecycle = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(control.session_id)?;
            Arc::clone(&session.lifecycle)
        };
        let lifecycle_guard = lifecycle.lock().await;

        // A client may lose the successful terminal response. The tombstone
        // closes every mutation except replay of the exact accepted identity
        // and sequence; no Store read is needed to recover that immutable
        // owner-local result.
        let terminal_replay = {
            let mut sessions = self.shared.sessions.lock().await;
            let session = sessions.get_mut(control.session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle) {
                return None;
            }
            if session.tombstone.is_some() {
                if session.control_end_snapshot.as_ref() != Some(&control.snapshot) {
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                }
                let replay = session.control.lock().expect("control lock").replay_exact(
                    control.generation,
                    control.owner_epoch,
                    control.client_instance_id,
                    control.sequence,
                    control.snapshot.platform(),
                );
                let Some((_, accepted_sequence, action, platform, action_suppressed)) = replay
                else {
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                };
                let Some(mut result) = session.control_end.clone() else {
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                };
                if result
                    .terminal_commit
                    .as_ref()
                    .is_some_and(|commit| commit.is_expired())
                {
                    // Preserve only the owner/sequence tombstone after the
                    // bounded response-recovery window. The retained Store
                    // handle, response, and retry closure are no longer useful.
                    session.control_end = None;
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                }
                debug_assert_eq!(result.accepted_sequence, accepted_sequence);
                debug_assert_eq!(result.action, action);
                debug_assert_eq!(result.platform, platform);
                debug_assert_eq!(result.action_suppressed, action_suppressed);
                result.disposition = crate::playback_control::ControlDisposition::Replay;
                // One viewer action is one measurement: a stored result
                // carries the observation of the exchange that produced it,
                // and replaying it must not replay that.
                result.selection = crate::playback_control::SelectionObservation::default();
                let Some(cleanup) = session.terminal_cleanup.as_ref().map(Arc::clone) else {
                    return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                };
                Some((result, cleanup))
            } else {
                None
            }
        };
        if let Some((result, cleanup)) = terminal_replay {
            #[cfg(test)]
            let terminal_replay_pause = self
                .shared
                .terminal_replay_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(pause) = terminal_replay_pause {
                pause.wait().await;
                pause.wait().await;
            }
            cleanup.wait().await;
            if let Some(commit) = &result.terminal_commit {
                // Reader detach is the visibility fence for VOD End. Every
                // retry, including one replacing a cancelled original HTTP
                // waiter, must cross the session-owned cleanup first.
                commit.retry();
            }
            return Some(Ok(result));
        }

        if let Err(error) = crate::playback_control::verify_authority(
            self.shared.store.as_ref(),
            control.session_id,
            control.generation,
            control.owner_node_id,
            control.owner_epoch,
        )
        .await
        {
            return Some(Err(error));
        }

        let ending_status =
            if control.snapshot.demand == crate::playback_control::PlaybackDemand::End {
                self.status(control.session_id).await
            } else {
                None
            };
        enum AppliedControl {
            End {
                result: crate::playback_control::LocalControlResult,
                terminal_commit: Option<Box<crate::playback_control::TerminalCommitReceipt>>,
                cleanup: Arc<TerminalCleanup>,
                rendition: Arc<Rendition>,
                file_id: i64,
                height: i64,
                kind: SessionKind,
            },
            Live {
                disposition: crate::playback_control::ControlDisposition,
                accepted_sequence: u64,
                action: crate::playback_control::ControlAction,
                action_suppressed: bool,
                preparation_directive: Option<Box<crate::playback_control::PreparationDirective>>,
                platform: crate::playback_control::ClientPlatform,
                selection: crate::playback_control::SelectionObservation,
                lease_expires_at_unix_ms: i64,
                marker_prewarm: Option<Box<MarkerPrewarmControl>>,
            },
        }
        let outcome = {
            let mut sessions = self.shared.sessions.lock().await;
            let session = sessions.get_mut(control.session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle) || session.tombstone.is_some() {
                return None;
            }
            // Acquire the async reader lock before the sequence fence. From
            // acceptance through anchor publication there is then no await:
            // cancelling an HTTP response cannot leave an accepted command
            // whose exact replay silently skips its playback destination.
            let control_rendition = session.live_rendition().map(Arc::clone)?;
            let mut readers = control_rendition.readers.lock().await;
            if control.snapshot.demand != crate::playback_control::PlaybackDemand::End
                && !readers.contains_key(control.session_id)
            {
                return Some(Err(crate::playback_control::ControlStateError::Unavailable));
            }
            if crate::media_sessions::unix_ms() >= deadline_unix_ms {
                return Some(Err(crate::playback_control::ControlStateError::Unavailable));
            }
            // Acceptance and M6's selection gate are one lock scope. They are
            // two reads of the same fence, and taking the lock twice would let
            // another exchange land between them and be measured against a
            // selection this one had already replaced.
            let (accepted, selection, preparation_directive) = {
                let mut fence = session.control.lock().expect("control lock");
                let accepted = fence.accept(
                    control.generation,
                    control.owner_epoch,
                    control.client_instance_id,
                    control.sequence,
                    crate::playback_control::ControlAcceptance::observed(
                        control.snapshot.platform(),
                        &control.prepared_successor,
                        control.snapshot.acknowledgement.as_ref(),
                        control.snapshot.request_fingerprint.as_deref(),
                    ),
                );
                // Only for an accepted exchange: a replay is the same exchange
                // arriving twice, and it changed the selection the first time
                // or not at all.
                let observation = if matches!(
                    &accepted,
                    Ok((crate::playback_control::ControlDisposition::Accepted, ..))
                ) {
                    fence.observe(
                        &control.snapshot.selection,
                        control.snapshot.capabilities.as_ref(),
                    )
                } else {
                    crate::playback_control::SelectionObservation::default()
                };
                let preparation_directive = fence.preparation_directive();
                (accepted, observation, preparation_directive)
            };
            let (disposition, accepted_sequence, action, platform, action_suppressed) =
                match accepted {
                    Ok(outcome) => outcome,
                    Err(error) => return Some(Err(error)),
                };
            if disposition == crate::playback_control::ControlDisposition::Accepted
                && control.snapshot.demand != crate::playback_control::PlaybackDemand::End
            {
                if let Some(reader) = readers.get_mut(control.session_id) {
                    reader.accept_control(
                        control.sequence,
                        entry_containing(
                            &control_rendition.plan,
                            control.snapshot.buffer_anchor_ms() as f64 / 1_000.0,
                        ),
                    );
                    // Recompute optional speculation only after the accepted
                    // intent is published. Cancellation before that async
                    // recompute leaves speculation disabled, never enabled
                    // for the previous seek, pause, or playback position.
                    let mut prewarm = reader.marker_prewarm.lock().expect("marker ledger lock");
                    prewarm.enabled = false;
                    prewarm.deactivate();
                }
                control_rendition.kick();
            }
            drop(readers);

            if disposition == crate::playback_control::ControlDisposition::Accepted
                && control.snapshot.demand == crate::playback_control::PlaybackDemand::End
            {
                let status = ending_status?;
                let rendition = session.rendition.as_ref().map(Arc::clone)?;
                let lease_expires_at_unix_ms = crate::media_sessions::unix_ms();
                if let (Some(admission), Some(directive)) = (
                    preparation_admission.as_ref(),
                    preparation_directive.clone(),
                ) {
                    // End tombstones the VOD session and synchronously clears
                    // its local slot. Transfer any rollover/acknowledgement
                    // cleanup to the durable owner first, while the retained
                    // gate can still settle that exact slot even if the HTTP
                    // waiter disappears after this critical section.
                    admission.accepted(crate::playback_control::PreparationControlOutcome {
                        disposition,
                        accepted_sequence,
                        action: action.clone(),
                        action_suppressed,
                        preparation_directive: directive,
                        platform,
                        lease_expires_at_unix_ms,
                        lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                        lease_state: "ended",
                        selection: selection.clone(),
                    });
                }
                let mut result = crate::playback_control::LocalControlResult {
                    disposition,
                    accepted_sequence,
                    action,
                    action_suppressed,
                    preparation_directive,
                    lease_expires_at_unix_ms,
                    lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                    lease_state: "ended",
                    status: crate::transcode::HlsSessionInfo::Vod(Box::new(status)),
                    platform,
                    terminal_handoff: None,
                    terminal_commit: None,
                    selection: selection.clone(),
                };
                let terminal_commit = terminal_committer
                    .as_ref()
                    .map(|committer| deferred_terminal_commit(Arc::clone(committer), &mut result));
                let cleanup = Arc::new(TerminalCleanup::new());
                session.terminal_cleanup = Some(Arc::clone(&cleanup));
                session.tombstone = Some(Terminal::Deleted);
                session.abort_staged_preparation();
                session.control_end = Some(result.clone());
                session.control_end_snapshot = Some(control.snapshot.clone());
                Ok::<_, crate::playback_control::ControlStateError>(AppliedControl::End {
                    terminal_commit: terminal_commit.map(Box::new),
                    result,
                    cleanup,
                    rendition,
                    file_id: session.file.id,
                    height: session.target_height,
                    kind: session.kind,
                })
            } else {
                let marker_prewarm = (disposition
                    == crate::playback_control::ControlDisposition::Accepted)
                    .then(|| MarkerPrewarmControl {
                        rendition: session
                            .live_rendition()
                            .map(Arc::clone)
                            .expect("a live VOD control has a rendition"),
                        snapshot: control.snapshot.clone(),
                        destinations: session.marker_destinations.clone(),
                        sequence: control.sequence,
                        file_id: session.file.id,
                        kind: session.kind,
                    });
                if disposition == crate::playback_control::ControlDisposition::Accepted {
                    session.last_control_snapshot = Some(control.snapshot.clone());
                }
                let mut last_touch = session.last_touch.lock().expect("touch lock");
                if disposition == crate::playback_control::ControlDisposition::Accepted {
                    *last_touch = Instant::now();
                }
                let remaining = SESSION_IDLE_TTL.saturating_sub(last_touch.elapsed());
                let remaining_ms = i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX);
                let lease_expires_at_unix_ms =
                    crate::media_sessions::unix_ms().saturating_add(remaining_ms);
                if let (Some(admission), Some(directive)) = (
                    preparation_admission.as_ref(),
                    preparation_directive.clone(),
                ) {
                    // Transfer durable ownership while the accepted sequence
                    // and its preparation directive are still under the VOD
                    // lifecycle/control fence. The HTTP future may disappear
                    // immediately after this scope without stranding the slot.
                    admission.accepted(crate::playback_control::PreparationControlOutcome {
                        disposition,
                        accepted_sequence,
                        action: action.clone(),
                        action_suppressed,
                        preparation_directive: directive,
                        platform,
                        lease_expires_at_unix_ms,
                        lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                        lease_state: "active",
                        selection: selection.clone(),
                    });
                }
                Ok(AppliedControl::Live {
                    disposition,
                    accepted_sequence,
                    action,
                    action_suppressed,
                    preparation_directive: preparation_directive.map(Box::new),
                    platform,
                    selection: selection.clone(),
                    lease_expires_at_unix_ms,
                    marker_prewarm: marker_prewarm.map(Box::new),
                })
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error)),
        };
        #[cfg(test)]
        let control_applied_pause = self
            .shared
            .control_applied_pause
            .lock()
            .expect("control pause lock")
            .clone();
        #[cfg(test)]
        if let Some(pause) = control_applied_pause {
            pause.wait().await;
            pause.wait().await;
        }
        let (
            disposition,
            accepted_sequence,
            action,
            action_suppressed,
            preparation_directive,
            platform,
            selection,
            lease_expires_at_unix_ms,
            marker_prewarm,
        ) = match outcome {
            AppliedControl::End {
                result,
                terminal_commit,
                cleanup,
                rendition,
                file_id,
                height,
                kind,
            } => {
                self.spawn_terminal_cleanup(
                    control.session_id.to_owned(),
                    Arc::clone(&cleanup),
                    rendition,
                    file_id,
                    height,
                    kind,
                    Terminal::Deleted,
                    terminal_commit,
                );
                cleanup.wait().await;
                return Some(Ok(result));
            }
            AppliedControl::Live {
                disposition,
                accepted_sequence,
                action,
                action_suppressed,
                preparation_directive,
                platform,
                selection,
                lease_expires_at_unix_ms,
                marker_prewarm,
            } => (
                disposition,
                accepted_sequence,
                action,
                action_suppressed,
                preparation_directive.map(|directive| *directive),
                platform,
                selection,
                lease_expires_at_unix_ms,
                marker_prewarm.map(|prewarm| *prewarm),
            ),
        };
        if let Some(marker_prewarm) = marker_prewarm {
            if let Some(outcome) = apply_marker_prewarm_control(
                &marker_prewarm.rendition,
                control.session_id,
                marker_prewarm.sequence,
                &marker_prewarm.snapshot,
                &marker_prewarm.destinations,
            )
            .await
            {
                self.emit_marker_prewarm(
                    control.session_id,
                    marker_prewarm.file_id,
                    marker_prewarm.kind,
                    outcome,
                );
            }
        }
        drop(lifecycle_guard);
        let status = self.status(control.session_id).await?;
        Some(Ok(crate::playback_control::LocalControlResult {
            disposition,
            accepted_sequence,
            action,
            action_suppressed,
            preparation_directive,
            lease_expires_at_unix_ms,
            lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            lease_state: "active",
            status: crate::transcode::HlsSessionInfo::Vod(Box::new(status)),
            platform,
            terminal_handoff: None,
            terminal_commit: None,
            selection,
        }))
    }

    /// The facts a stall-reopen's normalization checks against its
    /// predecessor. `None` for unknown/tombstoned.
    // TODO(m3-wire): the allow comes out when the stall-reopen wiring lands.
    #[allow(dead_code)]
    pub async fn reopen_facts(&self, session_id: &str) -> Option<ReopenFacts> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some() {
            return None;
        }
        Some(ReopenFacts {
            supersession_user: session.supersession_user.clone(),
            playback_id: session.playback_id.clone(),
            file_id: session.file.id,
        })
    }

    /// The file a live VOD session is serving, for callers that need source
    /// facts at response time (the Apple init-record rewrite).
    pub async fn session_file_id(&self, session_id: &str) -> Option<i64> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some() {
            return None;
        }
        session.live_rendition()?;
        Some(session.file.id)
    }

    /// Exact copy-recipe facts for native HLS wrappers. Lookup alone does not
    /// touch the sliding TTL; the HTTP response commit does that after every
    /// playlist and init-object check succeeds.
    pub async fn hls_facts(&self, session_id: &str) -> Option<VodHlsFacts> {
        let publication = self.session_rendition(session_id).await?;
        let rendition = match publication.result {
            Ok((rendition, _, _)) => rendition,
            _ => return None,
        };
        Some(VodHlsFacts {
            file: rendition.recipe.file.clone(),
            audio_index: rendition.recipe.audio_index,
            aac: rendition.recipe.aac,
            preserve_dolby_vision: rendition.recipe.video.preserves_dolby_vision(),
            convert_dolby_vision: rendition.recipe.video.converts_dolby_vision(),
            response_owner: publication.owner,
        })
    }

    /// One immutable plan entry's film-time window for WebVTT children.
    pub async fn segment_window(&self, session_id: &str, segment_index: i64) -> Option<(f64, f64)> {
        let index = u32::try_from(segment_index).ok()?;
        let publication = self.session_rendition(session_id).await?;
        let (rendition, _, _) = match publication.result {
            Ok(value) => value,
            _ => return None,
        };
        let entry = rendition.plan.entry(index)?;
        Some((
            entry.start_ticks as f64 / f64::from(rendition.timescale),
            entry.end_ticks() as f64 / f64::from(rendition.timescale),
        ))
    }

    /// Rebuild the create answer for an idempotent replay (`request_id`
    /// recovery). `None` for a session that is not ours or has ended — the
    /// caller then answers the way it always has.
    pub async fn recovered_start(&self, session_id: &str) -> Option<RecoveredVod> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some() {
            return None;
        }
        Some(RecoveredVod {
            start: VodStart {
                session_id: session_id.to_owned(),
                duration_ms: plan_duration_ms(&session.live_rendition()?.plan),
            },
            target_height: session.target_height,
            kind: session.kind,
        })
    }

    /// Periodic maintenance, called from a spawned interval task the manager
    /// owns: dormant-session reap (sliding TTL), dormant-rendition purge
    /// after TTL (un-admitted only), driver kicks.
    pub async fn maintain(&self) {
        let terminal_cleanups = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .values()
                .filter(|session| session.tombstone.is_some())
                .filter_map(|session| session.terminal_cleanup.as_ref().map(Arc::clone))
                .filter(|cleanup| !cleanup.is_finished())
                .collect::<Vec<_>>()
        };
        // Bounded, because every later step in this function is behind it and
        // this task is the node's only maintenance loop. A cleanup that never
        // completes — a task the runtime dropped mid-flight, a future ordering
        // change that installs one with no owner — would otherwise stop the
        // idle reap, the dormant purge, the tombstone eviction and every
        // driver kick on the node, permanently and silently.
        //
        // Giving up on the wait gives up nothing else: each step below re-reads
        // what it needs, and tombstone eviction independently requires
        // `is_finished()`, so a cleanup still legitimately running is simply
        // waited for on the next tick. The bound is generous against real
        // settlement — a terminal commit is the slow part — so reaching it is
        // a defect worth a loud line rather than a busy node.
        if tokio::time::timeout(
            TERMINAL_CLEANUP_MAINTENANCE_WAIT,
            futures_util::future::join_all(
                terminal_cleanups
                    .into_iter()
                    .map(|cleanup| async move { cleanup.wait().await }),
            ),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                "a VOD terminal cleanup outlived {:?} of maintenance waiting; \
                 continuing this pass without it",
                TERMINAL_CLEANUP_MAINTENANCE_WAIT
            );
        }

        // End tombstones outlive the response-recovery operation so late or
        // mismatched control cannot resurrect a detached reader. Once the
        // exact durable acknowledgement window closes, retain only that small
        // ownership fence and release the Store/response/retry graph.
        {
            let mut sessions = self.shared.sessions.lock().await;
            for session in sessions
                .values_mut()
                .filter(|session| session.tombstone.is_some())
            {
                let expired = session
                    .control_end
                    .as_ref()
                    .and_then(|result| result.terminal_commit.as_ref())
                    .is_some_and(|commit| commit.is_expired());
                if expired {
                    session.control_end = None;
                }
            }
        }

        // A terminal session retains only its compact exact response owner for
        // the bounded late-410 replay window. After that window, release it
        // only when a fresh durable read proves this capability cannot be active.
        // Store I/O stays outside both the per-id lifecycle gate and the
        // node-wide session registry lock.
        let mut terminal_eviction_candidates = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter_map(|(session_id, session)| {
                    let cleanup = session.terminal_cleanup.as_ref()?;
                    (session.tombstone.is_some()
                        && cleanup.is_finished()
                        && cleanup.retention_expired()
                        && session.terminal_replay_expired())
                    .then(|| TerminalEvictionCandidate {
                        session_id: session_id.clone(),
                        lifecycle: Arc::clone(&session.lifecycle),
                        incarnation: Arc::clone(&session.incarnation),
                        cleanup: Arc::clone(cleanup),
                    })
                })
                .collect::<Vec<_>>()
        };
        // Confirm one bounded, fair batch per tick. A Store timeout therefore
        // costs at most one timeout interval instead of one interval per
        // fanout wave, while rotating the start prevents a consistently bad
        // route from pinning every candidate behind it.
        if !terminal_eviction_candidates.is_empty() {
            terminal_eviction_candidates
                .sort_by(|left, right| left.session_id.cmp(&right.session_id));
            let start = self
                .shared
                .terminal_eviction_cursor
                .fetch_add(TERMINAL_ROUTE_CONFIRM_BATCH as u64, Relaxed)
                as usize
                % terminal_eviction_candidates.len();
            terminal_eviction_candidates.rotate_left(start);
            terminal_eviction_candidates.truncate(TERMINAL_ROUTE_CONFIRM_BATCH);
        }
        let shared = Arc::clone(&self.shared);
        let terminal_eviction_candidates =
            futures_util::stream::iter(terminal_eviction_candidates.into_iter().map(|candidate| {
                let shared = Arc::clone(&shared);
                async move {
                    let durable_non_live = shared
                        .terminal_route_durably_non_live(&candidate.session_id)
                        .await;
                    durable_non_live.then_some(candidate)
                }
            }))
            .buffer_unordered(TERMINAL_ROUTE_CONFIRM_FANOUT)
            .filter_map(std::future::ready)
            .collect::<Vec<_>>()
            .await;
        for candidate in terminal_eviction_candidates {
            let _lifecycle = candidate.lifecycle.lock().await;
            let removed = {
                let mut sessions = self.shared.sessions.lock().await;
                let exact_terminal = sessions.get(&candidate.session_id).is_some_and(|session| {
                    Arc::ptr_eq(&session.lifecycle, &candidate.lifecycle)
                        && Arc::ptr_eq(&session.incarnation, &candidate.incarnation)
                        && session.tombstone.is_some()
                        && session.terminal_replay_expired()
                        && session.terminal_cleanup.as_ref().is_some_and(|cleanup| {
                            Arc::ptr_eq(cleanup, &candidate.cleanup)
                                && cleanup.is_finished()
                                && cleanup.retention_expired()
                        })
                });
                if exact_terminal {
                    sessions.remove(&candidate.session_id)
                } else {
                    None
                }
            };
            if removed.is_some() {
                tracing::debug!(
                    session = %session_log_id(&candidate.session_id),
                    "released an expired VOD terminal response-owner tombstone"
                );
            }
        }

        let now = Instant::now();
        // Idle live sessions vanish — tombstone-free, because an idle reap is
        // the one ending a session may come back from (via the durable route
        // machinery outside this module).
        let expired: Vec<(String, Arc<Mutex<()>>)> = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| {
                    session.tombstone.is_none()
                        && now.duration_since(*session.last_touch.lock().expect("touch lock"))
                            > SESSION_IDLE_TTL
                })
                .map(|(id, session)| (id.clone(), Arc::clone(&session.lifecycle)))
                .collect()
        };
        for (id, lifecycle) in expired {
            let _lifecycle = lifecycle.lock().await;
            let rendition = {
                let mut sessions = self.shared.sessions.lock().await;
                let still_expired = sessions.get(&id).is_some_and(|session| {
                    Arc::ptr_eq(&session.lifecycle, &lifecycle)
                        && session.tombstone.is_none()
                        && now.duration_since(*session.last_touch.lock().expect("touch lock"))
                            > SESSION_IDLE_TTL
                });
                if still_expired {
                    sessions.remove(&id).and_then(|session| session.rendition)
                } else {
                    None
                }
            };
            if let Some(rendition) = rendition {
                // Keep the per-id gate through detach. A resurrection for the
                // same durable id must attach only after this old reader is
                // gone, never between registry removal and detach.
                rendition.detach_reader(&self.shared.pool, &id).await;
                tracing::info!(
                    session = %session_log_id(&id),
                    rendition = %rendition.key,
                    "vod session idle-reaped (sliding TTL)"
                );
            }
        }

        // Purge un-admitted renditions dormant past their TTL. The collection
        // is only a cheap pre-filter; `purge_if_dormant` takes the exact-key
        // build gate and re-checks everything before its short map commit.
        let dormant: Vec<String> = {
            let renditions = self.shared.renditions.lock().await;
            renditions
                .values()
                .filter(|rendition| {
                    rendition
                        .dormant_since
                        .lock()
                        .expect("dormant lock")
                        .is_some_and(|since| now.duration_since(since) > DORMANT_RENDITION_TTL)
                })
                .map(|rendition| rendition.key.clone())
                .collect()
        };
        for key in dormant {
            self.shared
                .purge_if_dormant(&key, DORMANT_RENDITION_TTL)
                .await;
        }

        // Kick every driver so holds and idle reclaims are re-examined.
        let renditions: Vec<Arc<Rendition>> = {
            self.shared
                .renditions
                .lock()
                .await
                .values()
                .map(Arc::clone)
                .collect()
        };
        for rendition in renditions {
            rendition.kick();
        }
        self.shared.prune_session_lifecycles();
        self.shared.prune_rendition_builds();
    }

    // ---- serving internals -------------------------------------------------

    /// The session's rendition and block budget, or the typed reason there
    /// isn't one. This is lookup only; response commit owns liveness.
    async fn session_rendition(
        &self,
        session_id: &str,
    ) -> Option<VodPublication<(Arc<Rendition>, Duration, Arc<crate::meter::Meter>)>> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        let owner = session.response_owner();
        Some(VodPublication {
            result: match session.tombstone {
                Some(cause) => Err(VodError::Gone(cause)),
                None => Ok((
                    session.live_rendition().map(Arc::clone)?,
                    session.block_budget,
                    Arc::clone(&session.delivery),
                )),
            },
            owner,
        })
    }

    async fn serve_init(
        &self,
        rendition: &Arc<Rendition>,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        if self.source_changed(rendition) {
            let cause = "source changed after the fragment index was selected".to_owned();
            record_failure(
                &self.shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(VodError::ProducerFailed(cause));
        }
        let demand = self
            .shared
            .arm_materialize_watchdog(rendition, INIT_DEMAND_INDEX);
        rendition.kick();
        let deadline = Instant::now() + budget;
        loop {
            // Arm the notification — created AND enabled — before checking
            // the disk. `Notified` registers with its `Notify` only on first
            // poll or `enable()`; without the explicit enable, a
            // `notify_waiters` firing between the disk check and the await
            // wakes nobody, and the first init fetch stalls a full budget for
            // bytes that are already there.
            let notified = rendition.init_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if rendition.dir.has_init().await {
                rendition.clear_demand(INIT_DEMAND_INDEX);
                let path = rendition.dir.path().join(INIT_NAME);
                let ready = open_ready(&path, &format!("{}-init", rendition.key), &delivery)
                    .await
                    .map_err(VodError::Io)?;
                if self.source_changed(rendition) {
                    let cause =
                        "source changed while the rendition init was being opened".to_owned();
                    record_failure(
                        &self.shared,
                        rendition,
                        crate::playback_control::ProducerDecisionReason::SourceChanged,
                        cause.clone(),
                    );
                    return Err(VodError::ProducerFailed(cause));
                }
                return Ok(ready);
            }
            if let Some(cause) = rendition.failure_cause() {
                return Err(VodError::ProducerFailed(cause));
            }
            if demand.expired() {
                return Err(VodError::ProducerFailed(
                    "materializing init.mp4 exceeded the producer deadline".to_owned(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || tokio::time::timeout(remaining, &mut notified)
                    .await
                    .is_err()
            {
                demand.retry_pending();
                return Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                });
            }
        }
    }

    async fn serve_segment(
        &self,
        rendition: &Arc<Rendition>,
        session_id: &str,
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        // Materialized → serve immediately: the overwhelmingly common case.
        if let Some(ready) = self.open_materialized(rendition, index, &delivery).await? {
            return Ok(ready);
        }
        if let Some(cause) = rendition.failure_cause() {
            return Err(VodError::ProducerFailed(cause));
        }
        self.blocked_wait(rendition, session_id, index, budget, delivery)
            .await
    }

    /// The blocking half of a segment GET: register on the wait pool, close
    /// the lost-wakeup window, and sleep until one of the four named ends.
    async fn blocked_wait(
        &self,
        rendition: &Arc<Rendition>,
        session_id: &str,
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        let deadline = tokio::time::Instant::now() + budget;
        let key = WaitKey {
            rendition: rendition.key.clone(),
            index,
        };
        let _wake = DemandWake(Arc::clone(rendition));
        // Reserve synchronously before any await: a seek storm must not park
        // unbounded unadmitted futures behind the manifest or a disk open.
        let (mut wait, demand) = {
            // The watchdog holds this same lock through fail_entry: an
            // admitted receiver is bound to its deadline before it is visible
            // to expiry, including on a multithread runtime.
            let mut clocks = rendition.demand_since.lock().expect("demand lock");
            let wait = self
                .shared
                .pool
                .register(key, session_id)
                // The class travels with the refusal. `blocked_wait` is the only
                // production caller of `register`, so this line is the whole
                // seam between the pool knowing which cap fired and a client
                // or an operator ever finding out.
                .map_err(VodError::Busy)?;
            let demand = self
                .shared
                .arm_materialize_watchdog_locked(rendition, index, &mut clocks);
            (wait, demand)
        };
        // Admission comes before any persistent demand or deadline state. A
        // refused GET is not work owed by this rendition.
        rendition.kick();
        // Register before rechecking bytes/failure to close the lost-wakeup
        // window. The registration stays alive through open_materialized,
        // protecting a just-published target from concurrent eviction.
        if let Some(ready) = self.open_materialized(rendition, index, &delivery).await? {
            return Ok(ready);
        }
        if let Some(cause) = rendition.failure_cause() {
            return Err(VodError::ProducerFailed(cause));
        }
        if demand.expired() {
            return Err(VodError::ProducerFailed(format!(
                "materializing {} exceeded the producer deadline",
                segment_name(u64::from(index))
            )));
        }
        #[cfg(test)]
        let waiting_pause = self
            .shared
            .segment_ready_pause
            .lock()
            .expect("segment pause lock")
            .clone();
        #[cfg(test)]
        if let Some(pause) = waiting_pause {
            // Phase one proves the post-registration recheck has finished;
            // the Ready arm supplies phases two and three around file open.
            pause.wait().await;
        }
        let outcome = wait
            .wait(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await;
        match outcome {
            WaitOutcome::Ready => {
                #[cfg(test)]
                let pause = self
                    .shared
                    .segment_ready_pause
                    .lock()
                    .expect("segment pause lock")
                    .clone();
                #[cfg(test)]
                if let Some(pause) = pause {
                    pause.wait().await;
                    pause.wait().await;
                }
                match self.open_materialized(rendition, index, &delivery).await? {
                    Some(ready) => Ok(ready),
                    // An eviction already committed before registration, or an
                    // external unlink, can still remove the file. Publication
                    // during this admitted wait is protected by its narrow pin.
                    None => Err(VodError::Pending {
                        retry_after: PENDING_RETRY_AFTER,
                    }),
                }
            }
            WaitOutcome::Deadline => {
                if demand.expired() {
                    return Err(VodError::ProducerFailed(format!(
                        "materializing {} exceeded the producer deadline",
                        segment_name(u64::from(index))
                    )));
                }
                demand.retry_pending();
                Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                })
            }
            WaitOutcome::ProducerFailed(cause) => Err(VodError::ProducerFailed(cause)),
            // The rendition went away under the wait; the session's own
            // tombstone (if any) is the more precise cause on the next GET.
            WaitOutcome::Gone => Err(VodError::Gone(Terminal::Deleted)),
        }
    }

    /// Open one materialized segment under the manifest lock, so eviction
    /// cannot unlink it between the check and the open.
    async fn open_materialized(
        &self,
        rendition: &Arc<Rendition>,
        index: u32,
        delivery: &Arc<crate::meter::Meter>,
    ) -> Result<Option<SegmentReady>, VodError> {
        if self.source_changed(rendition) {
            let cause = "source changed after the fragment index was selected".to_owned();
            record_failure(
                &self.shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(VodError::ProducerFailed(cause));
        }
        let manifest = rendition.manifest.lock().await;
        let Some(SegState::Materialized { at_ms, .. }) = manifest.state(index) else {
            return Ok(None);
        };
        let path = rendition.dir.path().join(segment_name(u64::from(index)));
        // The materialization instant is part of the etag: key-index-length
        // alone collides across an evict-and-regenerate whose bytes differ
        // while its length happens to match.
        match open_ready(
            &path,
            &format!("{}-{index}-{at_ms}", rendition.key),
            delivery,
        )
        .await
        {
            Ok(ready) => Ok(Some(ready)),
            // The manifest lied — treat as planned; reconcile repairs it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(VodError::Io(error)),
        }
    }

    fn source_changed(&self, rendition: &Rendition) -> bool {
        rendition
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
    }
}

impl Shared {
    async fn terminal_route_durably_non_live(&self, session_id: &str) -> bool {
        #[cfg(test)]
        if let Some(outcome) = self
            .terminal_route_test_outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .copied()
        {
            // Timeout and Store error are both fail-closed retention outcomes;
            // the distinct variants exist so the regression inventory proves
            // both paths without depending on SQLite scheduler timing.
            return match outcome {
                TerminalRouteTestOutcome::Success => true,
                TerminalRouteTestOutcome::Timeout | TerminalRouteTestOutcome::Error => false,
            };
        }
        tokio::time::timeout(
            TERMINAL_ROUTE_CONFIRM_TIMEOUT,
            self.store.media_session_route(session_id),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|route| route.as_ref().is_none_or(|route| route.state != "active"))
    }

    fn session_lifecycle(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut lifecycles = self
            .session_lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lifecycle) = lifecycles.get(session_id).and_then(Weak::upgrade) {
            return lifecycle;
        }
        let lifecycle = Arc::new(Mutex::new(()));
        lifecycles.insert(session_id.to_owned(), Arc::downgrade(&lifecycle));
        lifecycle
    }

    fn prune_session_lifecycles(&self) {
        self.session_lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, lifecycle| lifecycle.strong_count() > 0);
    }

    fn rendition_build_gate(&self, key: &str) -> Arc<Mutex<()>> {
        let mut builds = self
            .rendition_builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(build) = builds.get(key).and_then(Weak::upgrade) {
            return build;
        }
        let build = Arc::new(Mutex::new(()));
        builds.insert(key.to_owned(), Arc::downgrade(&build));
        build
    }

    fn prune_rendition_builds(&self) {
        self.rendition_builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, build| build.strong_count() > 0);
    }

    /// Arm ruling A3's producer deadline once per demanded plan entry. The
    /// timestamp survives shorter HTTP block deadlines and their 503 retries.
    fn arm_materialize_watchdog(
        self: &Arc<Self>,
        rendition: &Arc<Rendition>,
        index: u32,
    ) -> MaterializeDemand {
        let mut clocks = rendition.demand_since.lock().expect("demand lock");
        self.arm_materialize_watchdog_locked(rendition, index, &mut clocks)
    }

    fn arm_materialize_watchdog_locked(
        self: &Arc<Self>,
        rendition: &Arc<Rendition>,
        index: u32,
        demands: &mut HashMap<u32, MaterializeClock>,
    ) -> MaterializeDemand {
        let started = Instant::now();
        if let Some(clock) = demands.get_mut(&index) {
            clock.owners += 1;
            clock.retry_pending = false;
            return MaterializeDemand {
                rendition: Arc::clone(rendition),
                index,
                started: clock.started,
            };
        }
        demands.insert(
            index,
            MaterializeClock {
                started,
                owners: 1,
                retry_pending: false,
                watchdog: None,
            },
        );
        let owner = MaterializeDemand {
            rendition: Arc::clone(rendition),
            index,
            started,
        };
        let shared = Arc::clone(self);
        let rendition = Arc::clone(rendition);
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                started + rendition.materialize_budget,
            ))
            .await;
            if rendition.closed.load(Relaxed) || rendition.failure().is_some() {
                rendition.clear_demand(index);
                return;
            }
            let expired = if index == INIT_DEMAND_INDEX {
                if rendition.dir.has_init().await {
                    rendition.clear_demand(index);
                    false
                } else {
                    rendition
                        .demand_since
                        .lock()
                        .expect("demand lock")
                        .get(&index)
                        .is_some_and(|clock| clock.started == started)
                }
            } else {
                // The sink takes these locks in the same order and clears the
                // demand before releasing the manifest, closing the
                // deadline/materialization race.
                let manifest = rendition.manifest.lock().await;
                if manifest.state(index).is_some_and(SegState::is_materialized) {
                    rendition.clear_demand(index);
                    false
                } else {
                    rendition
                        .demand_since
                        .lock()
                        .expect("demand lock")
                        .get(&index)
                        .is_some_and(|clock| clock.started == started)
                }
            };
            if !expired || rendition.failure().is_some() {
                return;
            }
            if index != INIT_DEMAND_INDEX {
                // A slow, cancelled, or superseded segment request cannot
                // prove that this shared producer is unhealthy. Settle only
                // currently admitted waiters for this entry; if all callers
                // disappeared, this is a no-op. Real child failures still
                // use record_failure to answer the whole rendition.
                // Keep the episode fence through the synchronous wake. A
                // cancelled old watchdog must not wake a newly registered
                // request for this same index after it acquired a new clock.
                let clocks = rendition.demand_since.lock().expect("demand lock");
                if !clocks
                    .get(&index)
                    .is_some_and(|clock| clock.started == started)
                {
                    return;
                }
                shared.pool.fail_entry(
                    &rendition.key,
                    index,
                    &format!(
                        "materializing {} exceeded the {:.1}s producer deadline",
                        segment_name(u64::from(index)),
                        rendition.materialize_budget.as_secs_f64()
                    ),
                );
                drop(clocks);
                rendition.kick();
            } else {
                // Init waiters inspect their own captured clock after this wake.
                rendition.init_notify.notify_waiters();
            }
            // Preserve expiry long enough for a normal Retry-After cycle to
            // receive its typed failure, then forget an abandoned HTTP retry.
            // An old task never removes a newer episode for this same entry.
            tokio::time::sleep(PENDING_RETRY_AFTER.saturating_mul(2)).await;
            let mut demands = rendition.demand_since.lock().expect("demand lock");
            if demands
                .get(&index)
                .is_some_and(|clock| clock.started == started && clock.owners == 0)
            {
                demands.remove(&index);
            }
        });
        if let Some(clock) = demands
            .get_mut(&index)
            .filter(|clock| clock.started == started)
        {
            clock.watchdog = Some(watchdog.abort_handle());
        } else {
            watchdog.abort();
        }
        owner
    }

    /// Find or build the rendition for `key`, spawning its driver. `None`
    /// means the plan came out empty and the caller returns a typed refusal.
    async fn attach_rendition(
        self: &Arc<Shared>,
        key: &str,
        identity: &SourceIdentity,
        index: FragmentIndex,
        recipe: Recipe,
        duration_ms: i64,
        settings: &VodSettings,
    ) -> Result<Option<RenditionAttachment>, String> {
        let build_guard = self.rendition_build_gate(key).lock_owned().await;
        // Once exact-key admission succeeds, transfer the entire slow
        // Store/filesystem/head/build transaction to a detached owner before
        // the caller reaches another cancellation point. The successful
        // result returns the same guard for the final reader/session attach;
        // a cancelled caller merely drops the receiver, so the owner still
        // confirms child reap and then drops the unused attachment guard.
        let shared = Arc::clone(self);
        let key = key.to_owned();
        let identity = identity.clone();
        let settings = settings.clone();
        let result_rx = spawn_cancellation_independent(async move {
            shared
                .attach_rendition_owned(
                    key,
                    identity,
                    index,
                    recipe,
                    duration_ms,
                    settings,
                    build_guard,
                )
                .await
        });
        result_rx
            .await
            .unwrap_or_else(|_| Err("the rendition build owner exited unexpectedly".to_owned()))
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach_rendition_owned(
        self: &Arc<Shared>,
        key: String,
        identity: SourceIdentity,
        index: FragmentIndex,
        recipe: Recipe,
        duration_ms: i64,
        settings: VodSettings,
        build_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Option<RenditionAttachment>, String> {
        let key = key.as_str();
        // Single-flight only this key. No Store, filesystem, process, manifest,
        // or reader await below owns the node-wide registry mutex.
        let existing = {
            let renditions = self.renditions.lock().await;
            match renditions.get(key).map(Arc::clone) {
                Some(existing)
                    if existing.failure().is_none() && !existing.closed.load(Relaxed) =>
                {
                    return Ok(Some(RenditionAttachment {
                        rendition: existing,
                        _build_guard: build_guard,
                    }));
                }
                Some(existing) => {
                    // A failed (or closed) handle cannot answer new readers.
                    // Publish closed first, but leave the exact handle in the
                    // map until its accounting facts have been collected. If
                    // this request is cancelled during that await, the next
                    // key owner can resume the same removal transaction.
                    existing.closed.store(true, Relaxed);
                    existing.gen_epoch.fetch_add(1, Relaxed);
                    existing.kick();
                    self.pool.close(key);
                    Some(existing)
                }
                None => None,
            }
        };
        if let Some(stale) = existing {
            let (claimed, admitted) = {
                let manifest = stale.manifest.lock().await;
                (manifest.materialized_bytes(), manifest.is_admitted())
            };
            let removed = {
                let mut renditions = self.renditions.lock().await;
                let exact_stale = renditions
                    .get(key)
                    .is_some_and(|current| Arc::ptr_eq(current, &stale));
                if exact_stale {
                    renditions.remove(key)
                } else {
                    None
                }
            };
            if removed.is_some() {
                if admitted {
                    sub_saturating(&self.completed_cache, claimed);
                } else {
                    sub_saturating(&self.working_set, claimed);
                }
                tracing::info!(rendition = %key, "replacing a failed rendition on create");
            }
        }

        let plan = self
            .stored_plan(key, &identity, &index, &recipe, duration_ms)
            .await?;
        if plan.is_empty() {
            return Ok(None);
        }
        let rendition = self
            .build_rendition(key, index, recipe, plan, &settings)
            .await?;
        let adopted_bytes = rendition.manifest.lock().await.materialized_bytes();
        let (rendition, installed) = {
            let mut renditions = self.renditions.lock().await;
            if let Some(winner) = renditions
                .get(key)
                .filter(|winner| winner.failure().is_none() && !winner.closed.load(Relaxed))
                .map(Arc::clone)
            {
                (winner, false)
            } else {
                renditions.insert(key.to_string(), Arc::clone(&rendition));
                (rendition, true)
            }
        };
        if installed {
            if adopted_bytes > 0 {
                self.working_set.fetch_add(adopted_bytes, Relaxed);
            }
            spawn_driver(Arc::clone(self), Arc::clone(&rendition));
        }
        #[cfg(test)]
        if installed {
            let pause = self
                .rendition_install_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        // From this point cancellation leaves a registered, correctly
        // accounted dormant rendition. Admission may be retried by its driver
        // or maintenance; it can no longer leak counters or an untracked
        // directory if resurrection's request deadline wins.
        {
            let mut manifest = rendition.manifest.lock().await;
            if !manifest.is_empty() && manifest.next_gap(0).is_none() {
                self.try_admit(&rendition, &mut manifest).await;
            }
        }
        Ok(Some(RenditionAttachment {
            rendition,
            _build_guard: build_guard,
        }))
    }

    /// Plan acquisition (put-if-absent): the first plan written under a key
    /// is THE plan — on a lost race the stored one wins and is what every
    /// node serves.
    async fn stored_plan(
        &self,
        key: &str,
        identity: &SourceIdentity,
        index: &FragmentIndex,
        recipe: &Recipe,
        duration_ms: i64,
    ) -> Result<SegmentPlan, String> {
        if let Some(plan) = self
            .store
            .rendition_plan(key, identity)
            .await
            .map_err(|error| format!("reading the rendition plan: {error}"))?
        {
            return Ok(plan);
        }
        let policy = shipped_policy(index.timescale);
        let tracks = track_durations(index, recipe, duration_ms);
        let plan = plurx_core::segplan::plan_copy(index, &policy, &tracks);
        if plan.is_empty() {
            // Never stored: an empty plan under the key would poison it.
            return Ok(plan);
        }
        let stored = self
            .store
            .put_rendition_plan(key, recipe.file.id, &plan, identity)
            .await
            .map_err(|error| format!("storing the rendition plan: {error}"))?;
        if stored {
            return Ok(plan);
        }
        // Lost the put-if-absent race: re-get and serve the stored plan.
        match self
            .store
            .rendition_plan(key, identity)
            .await
            .map_err(|error| format!("re-reading the rendition plan: {error}"))?
        {
            Some(theirs) => Ok(theirs),
            None => {
                tracing::warn!(
                    rendition = %key,
                    "lost the plan race but the stored plan is gone; serving ours"
                );
                Ok(plan)
            }
        }
    }

    async fn build_rendition(
        self: &Arc<Shared>,
        key: &str,
        index: FragmentIndex,
        recipe: Recipe,
        plan: SegmentPlan,
        settings: &VodSettings,
    ) -> Result<Arc<Rendition>, String> {
        let source = crate::fragment_index_cluster::open_source_fence(
            &recipe.file,
            recipe.source_object_version.as_deref(),
        )
        .await?;
        let dir = RenditionDir::new(self.base.join(key));
        let existed = tokio::fs::metadata(dir.path()).await.is_ok();
        dir.create()
            .await
            .map_err(|error| format!("creating the rendition directory: {error}"))?;

        let mut manifest = Manifest::new(plan.clone());
        let mut identity_state = IdentityState::default();
        if existed {
            // Resurrection/adoption: make the manifest agree with the disk.
            let report = dir
                .reconcile(&mut manifest, now_ms())
                .await
                .map_err(|error| format!("reconciling the rendition directory: {error}"))?;
            if !report.adopted.is_empty() || !report.forgotten.is_empty() {
                tracing::info!(
                    rendition = %key,
                    adopted = report.adopted.len(),
                    forgotten = report.forgotten.len(),
                    "reconciled an adopted rendition directory"
                );
            }
            match load_identity(&dir.path().join(IDENTITY_NAME)).await {
                Some(identity) => {
                    if !report.init_present && manifest.materialized_count() > 0 {
                        // Handoff §5's missing-init arm, and it cannot wait
                        // for the next ordinary generation: a fully adopted
                        // manifest has no gap, so the scheduler answers Idle
                        // forever, no generation ever spawns to re-write the
                        // init, and every init GET pends to deadline for the
                        // session's life. Run a HEAD regeneration now — spawn
                        // the generation child, read only to its muxer init,
                        // verify against the stored digests.
                        match regenerate_init_head(
                            &recipe,
                            &source,
                            &identity,
                            &self.head_regeneration_slots,
                        )
                        .await
                        {
                            Ok(served) => {
                                // Match keeps every surviving segment.
                                dir.write_init(&served.bytes).await.map_err(|error| {
                                    format!("re-writing the regenerated init: {error}")
                                })?;
                                tracing::info!(
                                    rendition = %key,
                                    "head regeneration re-derived a missing init.mp4; \
                                     every adopted segment kept"
                                );
                                identity_state = IdentityState {
                                    identity: Some(identity),
                                    from_disk: true,
                                };
                            }
                            Err(HeadRegenerationError::Busy) => {
                                return Err(crate::transcode::vod_refusal_error(
                                    "vod_head_regeneration_busy",
                                    "missing-init recovery is at its node-wide process limit",
                                ));
                            }
                            Err(HeadRegenerationError::Oversize) => {
                                return Err(crate::transcode::vod_refusal_error(
                                    "vod_head_regeneration_oversize",
                                    "the regenerated muxer head exceeded its strict byte limit",
                                ));
                            }
                            Err(HeadRegenerationError::Failed(why)) => {
                                // Mismatch (or an unverifiable head): purge to
                                // planned-only and establish fresh — before
                                // the counters below, so nothing is adopted.
                                let freed = dir.purge(&mut manifest).await;
                                if let Some(error) = freed.error {
                                    tracing::warn!(
                                        rendition = %key,
                                        "purging after a failed head regeneration: {error}"
                                    );
                                }
                                let _ =
                                    tokio::fs::remove_file(dir.path().join(IDENTITY_NAME)).await;
                                tracing::info!(
                                    rendition = %key,
                                    "head regeneration could not verify the adopted \
                                     rendition; purged to planned-only: {why}"
                                );
                            }
                        }
                    } else {
                        // Init present (or nothing adopted): the stored
                        // digests let the FIRST ordinary generation verify
                        // (vodgen refuses InitDrift before any write) and
                        // re-write the init if it is the missing piece.
                        identity_state = IdentityState {
                            identity: Some(identity),
                            from_disk: true,
                        };
                    }
                }
                None => {
                    // No identity means nothing on disk is verifiable: purge
                    // to planned-only and establish fresh.
                    if manifest.materialized_count() > 0 || dir.has_init().await {
                        let freed = dir.purge(&mut manifest).await;
                        if let Some(error) = freed.error {
                            tracing::warn!(
                                rendition = %key,
                                "purging an unverifiable rendition: {error}"
                            );
                        }
                        tracing::info!(
                            rendition = %key,
                            "purged an adopted rendition with no stored identity"
                        );
                    }
                }
            }
        }

        let timescale = plan.timescale.max(1);
        let seconds_per_segment = if plan.is_empty() {
            1.0
        } else {
            plan.duration_ticks() as f64 / f64::from(timescale) / plan.len() as f64
        };
        let plan_len = plan.len();
        let rendition = Arc::new(Rendition {
            key: key.to_string(),
            dir,
            recipe,
            source: Some(source),
            playlist: plan.playlist().into_bytes(),
            plan,
            timescale,
            seconds_per_segment,
            index,
            policy: shipped_policy(timescale),
            working_set_budget: settings.working_set_bytes,
            completed_cache_budget: settings.completed_cache_bytes,
            materialize_budget: settings.materialize_budget,
            manifest: Mutex::new(manifest),
            identity: Mutex::new(identity_state),
            slot: ProducerSlot::new(),
            readers: Mutex::new(HashMap::new()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            capacity_hold: StdMutex::new(None),
            init_notify: Notify::new(),
            wake: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            // Creation can be cancelled after the rendition is installed but
            // before a session reader attaches. Start it as dormant so that
            // abandoned resurrection preparation remains reclaimable; the
            // atomic reader/session commit clears this on success.
            dormant_since: StdMutex::new(Some(Instant::now())),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        });
        Ok(rendition)
    }

    /// Completion → admission (plan §2.4): reserve, make durable, complete —
    /// in that order, because admission is the durability boundary and
    /// reversing it publishes a promise before the bytes behind it are real.
    async fn try_admit(self: &Arc<Shared>, rendition: &Rendition, manifest: &mut Manifest) {
        if manifest.is_admitted() {
            return;
        }
        let budgets = Budgets {
            working_set_bytes: rendition.working_set_budget,
            completed_cache_bytes: rendition.completed_cache_budget,
            admission_share: 0.5,
        };
        if let Err(refused) = manifest.reserve(&budgets) {
            if !rendition.warned_admission.swap(true, Relaxed) {
                tracing::info!(
                    rendition = %rendition.key,
                    "rendition stays working-set-only: {refused:?}"
                );
            }
            return;
        }
        // The identity file is part of the promise; sync it with the rest.
        if let Err(error) = sync_file(&rendition.identity_path()).await {
            tracing::warn!(
                rendition = %rendition.key,
                "could not sync identity.json before admission: {error}"
            );
            return;
        }
        if let Err(error) = rendition.dir.make_durable(manifest).await {
            tracing::warn!(
                rendition = %rendition.key,
                "make_durable refused, staying un-admitted: {error}"
            );
            return;
        }
        match manifest.complete(&budgets) {
            Ok(bytes) => {
                sub_saturating(&self.working_set, bytes);
                self.completed_cache.fetch_add(bytes, Relaxed);
                // The working set just shrank node-wide: producers on OTHER
                // renditions terminated for NoRoom are waiting on exactly
                // this, and the next maintain tick is a viewer's deadline
                // away.
                self.kick_all();
                tracing::info!(
                    rendition = %rendition.key,
                    bytes,
                    "rendition completed and admitted to the cache"
                );
            }
            Err(refused) => {
                if !rendition.warned_admission.swap(true, Relaxed) {
                    tracing::info!(
                        rendition = %rendition.key,
                        "completion refused, staying un-admitted: {refused:?}"
                    );
                }
            }
        }
    }

    /// Give a dormant, un-admitted rendition up whole — but only after
    /// serializing against creation for this exact key and re-verifying that
    /// it is still dormant.
    ///
    /// The re-check is the point: maintain's collect-then-purge scan races a
    /// create, and a session attached between the scan and the commit would
    /// be pinned to a closed rendition — its driver exited, its wait keys
    /// answering nothing, every GET pending forever. `attach_rendition` keeps
    /// the same per-key gate through session attach, while maintenance skips
    /// a key whose build/attach is active; different keys never wait for it.
    async fn purge_if_dormant(self: &Arc<Shared>, key: &str, ttl: Duration) {
        let Ok(build_guard) = self.rendition_build_gate(key).try_lock_owned() else {
            return;
        };
        let rendition = {
            let renditions = self.renditions.lock().await;
            renditions.get(key).map(Arc::clone)
        };
        let Some(rendition) = rendition else {
            return;
        };
        if !rendition.readers.lock().await.is_empty() {
            return;
        }
        let dormant = rendition
            .dormant_since
            .lock()
            .expect("dormant lock")
            .is_some_and(|since| since.elapsed() > ttl);
        if !dormant {
            return;
        }
        // Keep the manifest fence through the short exact map removal. That
        // makes admission and purge mutually exclusive without ever holding
        // the node-wide registry while awaiting a rendition-local lock.
        let manifest = rendition.manifest.lock().await;
        if manifest.is_admitted() {
            return;
        }
        let removed = {
            let mut renditions = self.renditions.lock().await;
            let exact = renditions
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &rendition));
            if exact {
                rendition.closed.store(true, Relaxed);
                rendition.gen_epoch.fetch_add(1, Relaxed);
                renditions.remove(key);
            }
            exact
        };
        drop(manifest);
        if !removed {
            return;
        }
        // The removal commit has no following request-owned await. Transfer
        // exact key authority, child termination, accounting, directory and
        // identity cleanup to one detached settlement owner first. Awaiting
        // its handle is only a convenience for maintenance/tests; cancellation
        // drops the handle, not the transaction or its build gate.
        let shared = Arc::clone(self);
        let settlement = tokio::spawn(async move {
            let _build_guard = build_guard;
            #[cfg(test)]
            let pause = shared
                .dormant_purge_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
            let _ = rendition
                .slot
                .perform(
                    Step::Terminate {
                        why: Termination::Idle,
                    },
                    || {},
                )
                .await;
            shared.pool.close(&rendition.key);
            {
                let mut manifest = rendition.manifest.lock().await;
                let freed = rendition.dir.purge(&mut manifest).await;
                sub_saturating(&shared.working_set, freed.bytes);
                // Whatever a failing unlink left both on disk and claimed is
                // no longer managed; subtract its final claim exactly once
                // while same-key rebuild is still excluded by the gate.
                sub_saturating(&shared.working_set, manifest.materialized_bytes());
                if let Some(error) = freed.error {
                    tracing::warn!(
                        rendition = %rendition.key,
                        "purging a dormant rendition: {error}"
                    );
                }
            }
            let _ = tokio::fs::remove_file(rendition.identity_path()).await;
            rendition.kick();
            // Freed bytes are node-wide news (see `try_admit`).
            shared.kick_all();
            tracing::info!(
                rendition = %rendition.key,
                "purged a dormant un-admitted rendition"
            );
        });
        let _ = settlement.await;
    }

    /// Kick EVERY rendition's driver — for events that change the node-wide
    /// working set. A producer terminated for `NoRoom` on another rendition
    /// is waiting on exactly this; without it, the hold stands until the next
    /// maintain tick while a blocked viewer's deadline burns.
    ///
    /// Spawned rather than inline so callers already holding the renditions
    /// lock (or a manifest lock ordered after it) cannot deadlock.
    fn kick_all(self: &Arc<Shared>) {
        let shared = Arc::clone(self);
        tokio::spawn(async move {
            let renditions: Vec<Arc<Rendition>> = shared
                .renditions
                .lock()
                .await
                .values()
                .map(Arc::clone)
                .collect();
            for rendition in renditions {
                rendition.kick();
            }
        });
    }
}

// ---------------------------------------------------------------------------
// the producer driver
// ---------------------------------------------------------------------------

fn spawn_driver(shared: Arc<Shared>, rendition: Arc<Rendition>) {
    tokio::spawn(async move {
        loop {
            rendition.wake.notified().await;
            if rendition.closed.load(Relaxed) {
                break;
            }
            driver_pass(&shared, &rendition).await;
        }
    });
}

/// Reclaim what a rendition that has already failed is still holding.
///
/// A recorded failure is first-wins and never cleared for the life of the
/// rendition, and `record_failure` answers every waiter — the pool, and the
/// init `Notify` — at the moment it lands. From that instant the producer is
/// serving nobody and no later pass can change that.
///
/// The bare early return that used to stand in `driver_pass` left two things
/// behind. The child was one. `Action::Idle` is what normally reclaims a
/// producer, and it is only ever reached through the pass that return skipped,
/// so what remained were the dormant purge — which refuses an admitted
/// rendition outright — and the last `Arc<Rendition>` drop, which an in-flight
/// generation task or a parked materialize watchdog can defer for as long as
/// they live. On an admitted rendition that combination reaps nothing: a live
/// ffmpeg, or a SIGSTOP'd one still sitting on its codec session, stayed on the
/// node until the process exited.
///
/// The capacity hold is the other, and it is worth being exact about what it
/// is not. `capacity_hold` is written in exactly one place — the pass below —
/// so the value from the last pass before the failure latched for the life of
/// the rendition. It changes nothing a client sees: `status` answers `failed`
/// ahead of every belief arm, so the stale hold was already shielded from the
/// wire. Clearing it here removes dead state that contradicts the rendition it
/// belongs to, so that the next reader of it — a status reordering, an
/// operator surface, a decision that consults it — is not the one that has to
/// discover it was never true.
///
/// `Termination::Idle` is the existing spelling for "reclaim it"; `after` reads
/// nothing from the `why` and no wire carries it, so this is not a claim that a
/// failed rendition is idle.
async fn retire_failed_rendition(rendition: &Arc<Rendition>) {
    *rendition.capacity_hold.lock().expect("capacity hold") = None;
    if matches!(rendition.slot.belief().await, Producer::Absent { .. }) {
        return;
    }
    // Ignored exactly as the dormant purge and generation-end terminations
    // ignore it: a slot that lost its child between the belief read and here
    // is the outcome this asked for.
    let _ = rendition
        .slot
        .perform(
            Step::Terminate {
                why: Termination::Idle,
            },
            || {},
        )
        .await;
}

/// One pass: demand → decide → step → carry it out.
async fn driver_pass(shared: &Arc<Shared>, rendition: &Arc<Rendition>) {
    if rendition.failure().is_some() {
        retire_failed_rendition(rendition).await;
        return;
    }
    let belief = rendition.slot.belief().await;
    let mut manifest = rendition.manifest.lock().await;
    let (demands, prewarm_ledgers) = {
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(&shared.pool, rendition, &readers, &manifest);
        let ledgers = readers
            .values()
            .map(|reader| Arc::clone(&reader.marker_prewarm))
            .collect::<Vec<_>>();
        (demands, ledgers)
    };
    let position = Position {
        produced_through: belief.produced_through(),
        positioned_at: belief.positioned_at(),
        seconds_per_segment: rendition.seconds_per_segment,
        working_set: WorkingSet {
            used_bytes: shared.working_set.load(Relaxed),
            budget_bytes: rendition.working_set_budget,
            held: matches!(belief, Producer::Stopped { .. }),
        },
    };
    // The eviction windows are read before the decision, not inside the sweep
    // it may ask for: whether anything can be given up is not answerable
    // without knowing what is protected, and answering it wrongly turns a
    // capacity stall into a producer that looks like it stopped making
    // progress.
    let windows = eviction_windows(shared, rendition).await;
    let decision =
        decide_with_marker_prewarm(&manifest, &demands, position, &windows, &prewarm_ledgers);
    // Recorded from this pass's own decision, before the step that acts on it.
    // A hold with no scheduled end terminates its producer, so the belief that
    // would otherwise carry the reason is gone by the time anyone reads the
    // status; a hold that clears on its own is still carried by the stopped
    // producer and needs nothing here. Any other decision clears it, so the
    // record cannot outlive the condition that produced it.
    *rendition.capacity_hold.lock().expect("capacity hold") = match decision.action {
        Action::Suspend { reason, .. } if !crate::prodexec::clears_on_its_own(reason) => {
            Some(reason)
        }
        _ => None,
    };
    let step = retire_completed_marker_prewarm(
        rendition,
        belief,
        &decision,
        next_step(belief, decision.action),
    );
    if fence_marker_prewarm_before_room(rendition, belief, step) {
        // Capacity belongs to blocked foreground demand. Fence and retire a
        // speculative generation before freeing bytes, so queued prewarm
        // output cannot consume the room between this pass and the next.
        let terminate = Step::Terminate {
            why: Termination::IndefiniteHold,
        };
        if let Err(error) = rendition.slot.perform(terminate, || {}).await {
            tracing::debug!(rendition = %rendition.key, "retiring prewarm producer: {error}");
        }
        rendition.kick();
        return;
    }
    update_marker_prewarm_dispatch(rendition, belief, step, &decision);
    match step {
        Step::Nothing => {}
        Step::Stop | Step::Resume => {
            // TODO(m3-wire): session progress clock — the manager's motion
            // clock replaces this no-op touch when it attaches.
            if let Err(error) = rendition.slot.perform(step, || {}).await {
                clear_marker_prewarm_dispatch(rendition);
                tracing::warn!(rendition = %rendition.key, "performing {step:?}: {error}");
            }
        }
        Step::Terminate { .. } => {
            rendition.gen_epoch.fetch_add(1, Relaxed);
            if let Err(error) = rendition.slot.perform(step, || {}).await {
                tracing::debug!(rendition = %rendition.key, "performing {step:?}: {error}");
            }
        }
        Step::Start { .. } | Step::Restart { .. } => {
            rendition.gen_epoch.fetch_add(1, Relaxed);
            match rendition.slot.perform(step, || {}).await {
                Ok(Performed::NeedsSpawn { at }) => spawn_generation(shared, rendition, at).await,
                Ok(Performed::Done) => {}
                Err(error) => {
                    clear_marker_prewarm_dispatch(rendition);
                    tracing::warn!(rendition = %rendition.key, "performing {step:?}: {error}");
                }
            }
        }
        Step::MakeRoom { wanted } => {
            match rendition
                .dir
                .make_room(&mut manifest, &windows, wanted)
                .await
            {
                Ok(freed) => {
                    sub_saturating(&shared.working_set, freed.bytes);
                    if let Some(error) = freed.error {
                        tracing::warn!(
                            rendition = %rendition.key,
                            freed = freed.bytes,
                            "eviction sweep stopped early: {error}"
                        );
                    }
                    // Room may now exist; decide again promptly — and the
                    // freed bytes are node-wide news, so every other
                    // rendition's driver re-examines its hold too.
                    if freed.bytes > 0 {
                        rendition.kick();
                        shared.kick_all();
                    } else if !matches!(belief, Producer::Absent { .. }) {
                        // Protected bytes make this an indefinite capacity
                        // hold. Fence queued writes and release the producer;
                        // leaving it running would immediately exceed the
                        // same bound that the zero-progress sweep proved.
                        rendition.gen_epoch.fetch_add(1, Relaxed);
                        if let Err(error) = rendition
                            .slot
                            .perform(
                                Step::Terminate {
                                    why: Termination::IndefiniteHold,
                                },
                                || {},
                            )
                            .await
                        {
                            tracing::warn!(rendition = %rendition.key, "terminating producer after a zero-progress capacity sweep: {error}");
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(rendition = %rendition.key, "make_room: {error}");
                }
            }
        }
        Step::Report { hold } => {
            tracing::warn!(rendition = %rendition.key, "producer stalled: {hold:?}");
        }
    }
}

/// Preserve all admitted obligations, using accepted control solely to rank
/// current playback ahead of requests outside its buffer window.
fn playback_demands(
    pool: &WaitPool,
    rendition: &Rendition,
    readers: &HashMap<String, Reader>,
    manifest: &Manifest,
) -> Vec<Demand> {
    let blocked = pool.demands(&rendition.key);
    let mut nearest = HashMap::new();
    let mut oldest = HashMap::new();
    for request in &blocked {
        // Publication precedes pool.satisfy. A newly materialized nearest
        // request must not hide the next missing GET during that interval.
        if !manifest
            .state(request.index)
            .is_some_and(|state| !state.is_materialized())
        {
            continue;
        }
        let first = oldest
            .entry(&request.session)
            .or_insert(request.arrival_order);
        *first = (*first).min(request.arrival_order);
        if let Some(reader) = readers.get(&request.session).filter(|reader| {
            reader.control_sequence.is_some()
                && reader_window(reader, rendition.seconds_per_segment).covers(request.index)
        }) {
            let distance = request.index.abs_diff(reader.frontier);
            let closest = nearest.entry(&request.session).or_insert(distance);
            *closest = (*closest).min(distance);
        }
    }
    let mut demands = blocked
        .iter()
        .map(|blocked| {
            let mut demand = Demand::waiting_on(blocked.index);
            demand.arrival_order = Some(blocked.arrival_order);
            demand.foreground = match (readers.get(&blocked.session), nearest.get(&blocked.session))
            {
                (Some(reader), Some(nearest)) => {
                    blocked.index.abs_diff(reader.frontier) == *nearest
                }
                _ => oldest.get(&blocked.session) == Some(&blocked.arrival_order),
            };
            demand
        })
        .collect::<Vec<_>>();
    demands.extend(
        readers
            .values()
            .map(|reader| Demand::idle_at(reader.frontier)),
    );
    demands
}

async fn eviction_windows(shared: &Shared, rendition: &Rendition) -> Vec<ReaderWindow> {
    let mut windows = rendition.reader_windows().await;
    windows.extend(
        shared
            .pool
            .retained(&rendition.key)
            .into_iter()
            .map(|index| ReaderWindow {
                back: 0,
                playhead: index,
                frontier: index,
                ahead: 0,
            }),
    );
    windows
}

/// A completed speculative window has no foreground reason to retain its
/// ffmpeg process. The ordinary scheduler would stop it at the ahead horizon;
/// retire it instead once attribution is exhausted, unless foreground demand
/// has explicitly taken ownership of the same generation.
fn retire_completed_marker_prewarm(
    rendition: &Rendition,
    belief: Producer,
    decision: &MarkerPrewarmDecision,
    step: Step,
) -> Step {
    let epoch = rendition.gen_epoch.load(Relaxed);
    let prewarm_generation =
        rendition.marker_prewarm_generation.load(Acquire) == epoch.saturating_add(1);
    let ahead_hold = matches!(
        decision.action,
        Action::Suspend {
            reason: crate::prodsched::Hold::Ahead { .. },
            ..
        }
    );
    if prewarm_generation
        && rendition.active_marker_prewarms.load(Acquire) == 0
        && decision.candidate.is_none()
        && ahead_hold
        && !matches!(belief, Producer::Absent { .. })
        && matches!(step, Step::Stop | Step::Nothing)
    {
        Step::Terminate {
            why: Termination::Idle,
        }
    } else {
        step
    }
}

/// Fence a live speculative producer before a foreground room-making pass.
/// Returns true when the caller must physically terminate the old generation
/// and retry scheduling before it may evict anything.
fn fence_marker_prewarm_before_room(rendition: &Rendition, belief: Producer, step: Step) -> bool {
    if !matches!(step, Step::MakeRoom { .. }) || matches!(belief, Producer::Absent { .. }) {
        return false;
    }
    let epoch = rendition.gen_epoch.load(Relaxed);
    let prewarm_generation =
        rendition.marker_prewarm_generation.load(Acquire) == epoch.saturating_add(1);
    if !prewarm_generation {
        return false;
    }
    clear_marker_prewarm_dispatch(rendition);
    rendition.marker_prewarm_generation.store(0, Release);
    rendition.gen_epoch.fetch_add(1, Relaxed);
    true
}

fn decide_with_marker_prewarm(
    manifest: &Manifest,
    demands: &[Demand],
    position: Position,
    readers: &[ReaderWindow],
    prewarm_ledgers: &[Arc<StdMutex<MarkerPrewarmLedger>>],
) -> MarkerPrewarmDecision {
    let foreground_action = decide(manifest, demands, position, readers);
    // Real reader demand is always decided first. Only an idle producer or
    // one that would otherwise stop at the ordinary ahead horizon may spend
    // work on a marker destination. Capacity holds never evict or make room
    // for speculative bytes.
    let selected_prewarm = if matches!(
        foreground_action,
        crate::prodsched::Action::Idle
            | crate::prodsched::Action::Suspend {
                reason: crate::prodsched::Hold::Ahead { .. },
                ..
            }
    ) {
        prewarm_ledgers
            .iter()
            .filter_map(|ledger| {
                ledger
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .candidate(manifest)
            })
            .min_by_key(|candidate| candidate.target_entry)
    } else {
        None
    };
    let action = selected_prewarm
        .map(|candidate| {
            let demand = if candidate.target_materialized {
                Demand::idle_at(candidate.target_entry)
            } else {
                // This internal demand gets the same repositioning decision a
                // blocked GET would, but only after the foreground decision
                // above proved the playhead has no work left.
                Demand::waiting_on(candidate.target_entry)
            };
            (candidate, decide(manifest, &[demand], position, readers))
        })
        .filter(|(_, action)| {
            matches!(
                action,
                crate::prodsched::Action::Produce { .. }
                    | crate::prodsched::Action::Reposition { .. }
            )
        });
    let mut owners = Vec::new();
    for ledger in prewarm_ledgers {
        let mut state = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((candidate, _)) = action {
            for record_nonce in state.activate(candidate) {
                owners.push(MarkerPrewarmOwner {
                    ledger: Arc::clone(ledger),
                    record_nonce,
                });
            }
        } else {
            state.deactivate();
        }
    }
    MarkerPrewarmDecision {
        action: action.map_or(foreground_action, |(_, action)| action),
        candidate: action.map(|(candidate, _)| candidate),
        owners,
    }
}

/// Bind speculative work to the exact producer generation that received it.
/// A later control snapshot may disable future prewarming before an in-flight
/// fragment publishes; in that case retain only the producer's immediate
/// reach. A restart for foreground work is a new cause and drops attribution.
fn update_marker_prewarm_dispatch(
    rendition: &Rendition,
    belief: Producer,
    step: Step,
    decision: &MarkerPrewarmDecision,
) {
    let current_epoch = rendition.gen_epoch.load(Relaxed);
    let mut dispatch = rendition
        .marker_prewarm_dispatch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(candidate) = decision.candidate.filter(|_| !decision.owners.is_empty()) {
        let producer_epoch = current_epoch.saturating_add(u64::from(matches!(
            step,
            Step::Start { .. } | Step::Restart { .. }
        )));
        *dispatch = Some(MarkerPrewarmDispatch {
            producer_epoch,
            candidate,
            owners: decision.owners.clone(),
        });
        rendition
            .marker_prewarm_generation
            .store(producer_epoch.saturating_add(1), Release);
    } else {
        let foreground_owns_generation = matches!(
            decision.action,
            Action::Produce { .. } | Action::Reposition { .. }
        );
        if foreground_owns_generation || matches!(step, Step::Terminate { .. }) {
            rendition.marker_prewarm_generation.store(0, Release);
        }
        if foreground_owns_generation {
            // The publication that follows is owed to foreground demand, even
            // when it reuses a process prewarm positioned. Keeping the old
            // owners would let foreground work manufacture a prewarm hit.
            *dispatch = None;
        } else {
            let immediate_reach = matches!(step, Step::Nothing | Step::Resume | Step::Stop)
                .then(|| {
                    belief
                        .produced_through()
                        .map(|through| through.saturating_add(1))
                        .or_else(|| belief.positioned_at())
                })
                .flatten();
            let retain = dispatch.as_mut().is_some_and(|existing| {
                if existing.producer_epoch != current_epoch {
                    return false;
                }
                let Some(reach) = immediate_reach else {
                    return false;
                };
                if reach < existing.candidate.target_entry {
                    return false;
                }
                existing.candidate.window_end_entry =
                    existing.candidate.window_end_entry.min(reach);
                true
            });
            if !retain {
                *dispatch = None;
            }
        }
    }
    rendition.active_marker_prewarms.store(
        dispatch.as_ref().map_or(0, |dispatch| {
            u32::try_from(dispatch.owners.len()).unwrap_or(u32::MAX)
        }),
        Release,
    );
}

/// Spawn a real generation positioned at plan entry `at` and hand its stdout
/// to [`run_generation`].
async fn spawn_generation(shared: &Arc<Shared>, rendition: &Arc<Rendition>, at: u32) {
    if rendition.recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        record_failure(
            shared,
            rendition,
            crate::playback_control::ProducerDecisionReason::EngineChanged,
            "the v2 fragment-index engine changed; restart is required".to_owned(),
        );
        return;
    }
    // A spawn position must be a VIDEO entry — the scheduler can name an
    // audio-tail index (a blocked GET on the tail is real demand), but the
    // generation that serves it starts at the last video boundary and its
    // `finish` produces the tail entries.
    let at = video_entry_at_or_before(&rendition.plan, at);
    let Some(entry) = rendition.plan.entry(at) else {
        tracing::warn!(rendition = %rendition.key, "no plan entry {at} to spawn at");
        return;
    };
    let start_seconds = entry.start_ticks as f64 / f64::from(rendition.timescale);
    let recipe = &rendition.recipe;
    let attested = attested_source_setup(rendition);
    // One ffmpeg, converting or not. The conversion happens on the far side of
    // the muxer now — `dvpipe` rewrites the RPUs inside the fragments this
    // process writes — so the producer is the producer it always was.
    let (mut child, stdout) = {
        let mut args = copy_pipe_args_with_dolby_vision(
            &recipe.file,
            start_seconds,
            recipe.audio_index,
            recipe.aac,
            Pacing::unpaced(),
            recipe.video,
        );
        if attested {
            replace_inputs_with_attested_descriptor(&mut args);
        }
        let mut command = tokio::process::Command::new(ffmpeg_bin());
        attach_attested_descriptor(&mut command, rendition.source.as_ref());
        let mut child = match command
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let cause = format!("spawning the producer: {error}");
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    cause,
                );
                return;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                "the producer started without a stdout".to_string(),
            );
            return;
        };
        (child, stdout)
    };
    // The rendition can be closed between the spawn above and the attach
    // below (a purge committing on the maintain task). Attaching would leave
    // a live ffmpeg in a slot whose driver has already exited — a child
    // nothing reaps until the Arc drops.
    //
    // A failure recorded in that same gap is the other half of the same
    // hazard, and it was not guarded. The driver does not exit on a failure,
    // it switches to reclaiming, and a reclaiming pass that has already read
    // an absent belief will not look again until something kicks it — so an
    // attach landing just behind it puts a live child in a slot whose only
    // remaining reader answers `ProducerFailed`. Refuse the attach instead,
    // here, where the child is still ours to kill.
    if rendition.closed.load(Relaxed) || rendition.failure().is_some() {
        let _ = child.kill().await;
        return;
    }
    rendition
        .last_child_pid
        .store(child.id().unwrap_or(0), Relaxed);
    rendition.slot.attach(child, at).await;
    let epoch = rendition.gen_epoch.load(Relaxed);
    let shared = Arc::clone(shared);
    let rendition = Arc::clone(rendition);
    tokio::spawn(async move {
        run_generation(shared, rendition, stdout, at, epoch).await;
    });
    tracing::info!(rendition = %rendition_key_field(at), "spawned a producer generation");
}

/// Whether this rendition's producers read the source through the attested
/// file descriptor rather than by name.
///
/// The attestation is what makes a session's video and its audio provably the
/// same bytes: a path can be replaced between two opens, a descriptor cannot.
fn attested_source_setup(rendition: &Rendition) -> bool {
    cfg!(unix) && rendition.source.is_some()
}

/// Point every `-i` at the inherited descriptor.
///
/// Every input in a copy argv is the same source — the video, and the second
/// open a copied audio track's A/V correction needs — so they all become fd 3.
#[allow(clippy::needless_range_loop)]
fn replace_inputs_with_attested_descriptor(args: &mut [String]) {
    for index in 0..args.len().saturating_sub(1) {
        if args[index] == "-i" {
            args[index + 1] = "/dev/fd/3".to_owned();
        }
    }
}

/// The video pipeline one copy session runs, from what that session asked for.
///
/// The conversion rides **in** the options rather than beside them, so it
/// reaches `copy_video_args` and therefore the argv fingerprint and the
/// fragment-index identity. A converted stream has different bytes and
/// different segment boundaries; sharing an identity with the unconverted one
/// would hand a session a playlist whose cut points describe different media,
/// and the session would fail its landing on every fragment.
///
/// Every later reader — the rendition's generation, its playlist facts, its
/// index pass — asks these options rather than carrying a second copy of the
/// answer, which is why this is the one place the two flags meet.
fn copy_video_pipeline(
    file: &MediaFile,
    probe_json: Option<&str>,
    have_dovi: bool,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
) -> CopyVideoOptions {
    CopyVideoOptions::from_probe(file, probe_json, have_dovi, preserve_dolby_vision)
        .with_dolby_vision_conversion(convert_dolby_vision)
}

/// Hand a child the attested source as fd 3, if there is one.
///
/// Factored out because two spawn sites need it — the producer and head
/// regeneration — and a child that opened the file by name rather than by the
/// attested descriptor could read a file that had been replaced since the
/// fence was taken, pairing a playlist with media from a different film.
fn attach_attested_descriptor(
    command: &mut tokio::process::Command,
    source: Option<&crate::fragment_index_cluster::SourceFence>,
) {
    #[cfg(unix)]
    if let Some(source) = source {
        use std::os::fd::AsRawFd;
        let source_fd = source.handle.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                let duplicate = libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 10);
                if duplicate == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(duplicate, 3) == -1 {
                    libc::close(duplicate);
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(duplicate);
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (command, source);
}

fn rendition_key_field(at: u32) -> String {
    format!("generation@{at}")
}

/// Read one generation to its end: establish or verify the init identity,
/// then let [`vodgen::run`] cut the plan's entries into the sink.
async fn run_generation(
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
    stdout: tokio::process::ChildStdout,
    at: u32,
    epoch: u64,
) {
    let mut stdout = stdout;
    let need_pre_read = {
        let identity = rendition.identity.lock().await;
        identity.identity.is_none() || !rendition.dir.has_init().await
    };
    let src: Box<dyn AsyncRead + Send + Unpin> = if need_pre_read {
        // The generation's own pipe carries the muxer init; read up to it,
        // establish or verify, write the served init — and then replay the
        // consumed bytes in front of the live pipe so vodgen sees the whole
        // stream (it verifies the init itself before a single write).
        match read_muxer_init(&mut stdout).await {
            Err(error) => {
                on_generation_end(
                    &shared,
                    &rendition,
                    Outcome::Failed(Failure::Stream(format!(
                        "reading the generation's init: {error}"
                    ))),
                    epoch,
                )
                .await;
                return;
            }
            Ok((consumed, muxer)) => {
                if !establish_or_verify(&shared, &rendition, &muxer, epoch).await {
                    return;
                }
                Box::new(std::io::Cursor::new(consumed).chain(stdout))
            }
        }
    } else {
        Box::new(stdout)
    };
    let identity = {
        let state = rendition.identity.lock().await;
        match &state.identity {
            Some(identity) => identity.clone(),
            // The pre-read established it, or the rendition already had it;
            // reaching here without one is the pre-read having purged and
            // bailed, which returns above.
            None => return,
        }
    };
    let generation = Generation {
        plan: rendition.plan.clone(),
        index: rendition.index.clone(),
        identity,
        start_entry: at,
        policy: rendition.policy,
        // The recipe's own answer, which is also the answer the index this
        // generation is matched against was built with — they share one
        // `CopyVideoOptions`, so they cannot disagree.
        convert_dolby_vision: rendition.recipe.video.converts_dolby_vision(),
    };
    let sink = RenditionSink {
        shared: Arc::clone(&shared),
        rendition: Arc::clone(&rendition),
        epoch,
    };
    let outcome = vodgen::run(src, generation, &sink, &rendition.key).await;
    on_generation_end(&shared, &rendition, outcome, epoch).await;
}

/// The identity half of a pre-read generation. `false` means the generation
/// is over (drift handled or failure recorded) and the caller must return.
async fn establish_or_verify(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    muxer: &Init,
    epoch: u64,
) -> bool {
    if rendition.recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        on_generation_end(
            shared,
            rendition,
            Outcome::Failed(Failure::EngineChanged(
                "the v2 fragment-index engine changed before init publication".to_owned(),
            )),
            epoch,
        )
        .await;
        return false;
    }
    let served = {
        let mut state = rendition.identity.lock().await;
        match &state.identity {
            Some(identity) => match identity.served_init_for(muxer) {
                Ok(served) => served,
                Err(refused) => {
                    if matches!(refused, InitRefused::PromotionDrift { .. }) {
                        tracing::error!(
                            rendition = %rendition.key,
                            "promotion is no longer a pure function of stored inputs: {refused}"
                        );
                    }
                    drop(state);
                    on_generation_end(
                        shared,
                        rendition,
                        Outcome::Failed(Failure::InitDrift(refused.to_string())),
                        epoch,
                    )
                    .await;
                    return false;
                }
            },
            None => {
                let identity =
                    match InitIdentity::establish(muxer, rendition.index.promotion.clone()) {
                        Ok(identity) => identity,
                        Err(error) => {
                            drop(state);
                            on_generation_end(
                                shared,
                                rendition,
                                Outcome::Failed(Failure::Stream(format!(
                                    "establishing the init identity: {error}"
                                ))),
                                epoch,
                            )
                            .await;
                            return false;
                        }
                    };
                let served = identity
                    .served_init_for(muxer)
                    .expect("an identity just established from this muxer init serves it");
                if let Err(error) = store_identity(&rendition.identity_path(), &identity).await {
                    tracing::warn!(
                        rendition = %rendition.key,
                        "persisting identity.json: {error}"
                    );
                }
                *state = IdentityState {
                    identity: Some(identity),
                    from_disk: false,
                };
                served
            }
        }
    };
    if let Err(error) = rendition.dir.write_init(&served.bytes).await {
        on_generation_end(
            shared,
            rendition,
            Outcome::Failed(Failure::Sink(error)),
            epoch,
        )
        .await;
        return false;
    }
    rendition.clear_demand(INIT_DEMAND_INDEX);
    rendition.init_notify.notify_waiters();
    true
}

/// What a generation's ending means for the rendition.
async fn on_generation_end(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    outcome: Outcome,
    epoch: u64,
) {
    if rendition.closed.load(Relaxed) {
        return;
    }
    if rendition.gen_epoch.load(Relaxed) != epoch {
        // The driver already killed or replaced this generation on purpose;
        // its ending carries no verdict.
        return;
    }
    let _ = rendition.marker_prewarm_generation.compare_exchange(
        epoch.saturating_add(1),
        0,
        AcqRel,
        Acquire,
    );
    // Reap the child so the belief goes honestly absent, keeping its progress.
    let _ = rendition
        .slot
        .perform(
            Step::Terminate {
                why: Termination::Idle,
            },
            || {},
        )
        .await;
    match outcome {
        Outcome::Failed(Failure::InitDrift(cause)) => {
            on_init_drift(shared, rendition, cause).await;
        }
        Outcome::Failed(failure) => {
            record_failure(
                shared,
                rendition,
                classify_failure(&failure),
                describe_failure(&failure),
            );
        }
        Outcome::Ran { produced_through } => {
            let last = rendition.plan.len().saturating_sub(1) as u32;
            let finished = produced_through == Some(last) || {
                let manifest = rendition.manifest.lock().await;
                !manifest.is_empty() && manifest.next_gap(0).is_none()
            };
            if !finished && shared.pool.blocked_on(&rendition.key).is_some() {
                // The producer died on its own with somebody mid-wait: the
                // typed refusal is the truth (plan §2.3 outcome 3).
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::PartialSuccessExit,
                    format!("producer exited early (produced through {produced_through:?})"),
                );
            } else {
                tracing::debug!(
                    rendition = %rendition.key,
                    "generation ended (produced through {produced_through:?})"
                );
            }
        }
    }
    rendition.kick();
}

/// The §5 mismatch arm: an adopted identity that this pipeline no longer
/// reproduces, with nothing admitted, purges to planned-only and establishes
/// fresh on the next spawn. Anything else is real drift and fails typed.
async fn on_init_drift(shared: &Arc<Shared>, rendition: &Arc<Rendition>, cause: String) {
    let from_disk = rendition.identity.lock().await.from_disk;
    let admitted = rendition.manifest.lock().await.is_admitted();
    if from_disk && !admitted {
        {
            let mut manifest = rendition.manifest.lock().await;
            let freed = rendition.dir.purge(&mut manifest).await;
            sub_saturating(&shared.working_set, freed.bytes);
            if let Some(error) = freed.error {
                tracing::warn!(
                    rendition = %rendition.key,
                    "purging after adopted-identity drift: {error}"
                );
            }
        }
        let _ = tokio::fs::remove_file(rendition.identity_path()).await;
        *rendition.identity.lock().await = IdentityState::default();
        tracing::info!(
            rendition = %rendition.key,
            "adopted identity no longer matches this pipeline; purged to \
             planned-only to establish fresh: {cause}"
        );
        return;
    }
    // Init drift is a *pipeline* change under a rendition a client already
    // holds a playlist for — vodgen says so in as many words — so it is filed
    // as the engine moving, not as the file on disk moving.
    record_failure(
        shared,
        rendition,
        crate::playback_control::ProducerDecisionReason::EngineChanged,
        cause,
    );
}

fn record_failure(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    decision: crate::playback_control::ProducerDecisionReason,
    cause: String,
) {
    tracing::warn!(
        rendition = %rendition.key,
        decision = decision.status(),
        "producer failed: {cause}"
    );
    {
        // First failure wins. A publication fence records the cause — the
        // source moved, the engine moved — and the generation it stops then
        // ends with a *consequence*: the sink refused the bytes, the pipe
        // closed. Overwriting would file the consequence as the diagnosis and
        // publish a class that names the wrong subsystem.
        let mut failed = rendition.failed.lock().expect("failed lock");
        if failed.is_none() {
            *failed = Some(RenditionFailure {
                decision,
                cause: cause.clone(),
            });
        }
    }
    shared.pool.fail(&rendition.key, &cause);
    // Init waiters block on their own Notify, not the wait pool — without
    // this, a GET waiting for `init.mp4` sleeps its whole budget to learn
    // what every segment waiter was told immediately.
    rendition.init_notify.notify_waiters();
    // And wake the driver, whose only remaining job for this rendition is to
    // reclaim the producer. Without it the first reclaiming pass waits for the
    // next maintenance tick's `kick_all`, so a failure recorded a moment after
    // one tick leaves a doomed ffmpeg holding a codec session for the whole
    // interval. This is a `notify_one` on the rendition's own wake, so it
    // cannot outlive the driver task or wake anything else.
    rendition.kick();
}

/// One recorded rendition failure: what a client can act on, and what an
/// operator reads.
#[derive(Debug, Clone)]
struct RenditionFailure {
    /// The bounded class, in the same vocabulary rolling delivery publishes.
    /// This is what becomes `DeliveryView::producer_decision`, and through it
    /// a `terminal` or `retry_resource` action the clients already handle.
    decision: crate::playback_control::ProducerDecisionReason,
    /// The sentence. Never parsed, only shown.
    cause: String,
}

/// Classify one generation outcome.
///
/// `Stream` is genuinely the reader failing on the producer's output, which is
/// what `ReaderFailed` already names on the rolling side; the other two have
/// no rolling equivalent, because nothing rolling lands fragments at planned
/// film times or writes through an immutable sink.
fn classify_failure(failure: &Failure) -> crate::playback_control::ProducerDecisionReason {
    use crate::playback_control::ProducerDecisionReason as Reason;
    match failure {
        // Handled before this point by `on_init_drift`; classified here so the
        // match stays exhaustive rather than defaulting a new variant, and
        // with the same class that path records.
        Failure::InitDrift(_) => Reason::EngineChanged,
        Failure::EngineChanged(_) => Reason::EngineChanged,
        Failure::Landing(_) => Reason::MediaLandingFailed,
        Failure::Stream(_) => Reason::ReaderFailed,
        Failure::Sink(_) => Reason::ProducerWriteFailed,
    }
}

fn describe_failure(failure: &Failure) -> String {
    match failure {
        Failure::InitDrift(why) => format!("init drift: {why}"),
        Failure::EngineChanged(why) => format!("engine changed: {why}"),
        Failure::Landing(why) => format!("landing failed: {why}"),
        Failure::Stream(why) => format!("stream failed: {why}"),
        Failure::Sink(error) => format!("sink refused: {error}"),
    }
}

/// The film-indexed sink one generation writes through: bytes to the
/// directory under the manifest lock, the node-wide counter, the wait pool,
/// the slot's progress, and the driver's wake — in that order.
struct RenditionSink {
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
    /// The generation epoch this sink was built for. A write from a stale
    /// epoch — a killed generation's queued materialize landing after a
    /// Restart — is refused under the manifest lock: letting it through would
    /// advance the NEW producer's belief with the OLD generation's progress
    /// (one spurious kill of the healthy replacement) and, worse, let a dead
    /// generation keep writing bytes under the replacement's feet.
    epoch: u64,
}

/// Assign the successful publication a monotonic identity and, only when the
/// scheduler currently attributes work to marker prewarm, credit that exact
/// identity to the active playback ledgers. The caller holds `manifest`, so a
/// landing observation cannot interleave between publication and provenance.
fn clear_marker_prewarm_dispatch(rendition: &Rendition) {
    *rendition
        .marker_prewarm_dispatch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    rendition.active_marker_prewarms.store(0, Release);
}

async fn credit_marker_prewarm_publication(
    rendition: &Rendition,
    producer_epoch: u64,
    entry: u32,
) -> u64 {
    let publication = rendition
        .publication_serial
        .fetch_add(1, Relaxed)
        .saturating_add(1);
    if let Some(version) = rendition
        .publication_versions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(entry as usize)
    {
        *version = Some(publication);
    }
    if rendition.active_marker_prewarms.load(Acquire) == 0 {
        return publication;
    }
    let attribution = {
        let mut dispatch = rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attribution = dispatch.as_ref().and_then(|dispatch| {
            (dispatch.producer_epoch == producer_epoch
                && entry >= dispatch.candidate.target_entry
                && entry <= dispatch.candidate.window_end_entry)
                .then(|| (dispatch.candidate, dispatch.owners.clone()))
        });
        if dispatch.as_ref().is_some_and(|dispatch| {
            dispatch.producer_epoch != producer_epoch
                || entry >= dispatch.candidate.window_end_entry
        }) {
            *dispatch = None;
            rendition.active_marker_prewarms.store(0, Release);
        }
        attribution
    };
    let Some((candidate, owners)) = attribution else {
        return publication;
    };
    for owner in owners {
        owner
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .credit_dispatched(candidate, owner.record_nonce, entry, publication);
    }
    publication
}

impl vodgen::Sink for RenditionSink {
    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> io::Result<()> {
        if self.rendition.closed.load(Relaxed) {
            // The quiet teardown: `NotFound` is how vodgen learns the session
            // ended normally rather than faulted.
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        // Both fences record their own class before returning. vodgen sees
        // only an `io::Error` and wraps it as `Failure::Sink`, which would
        // otherwise be classified `producer_write_failed` — a sink fault,
        // which is not what happened. `record_failure` is first-wins, so the
        // class recorded here survives the generation ending underneath it.
        if self
            .rendition
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
        {
            let cause = "source changed before fragment publication".to_owned();
            record_failure(
                &self.shared,
                &self.rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, cause));
        }
        if self.rendition.recipe.cluster_cache_key.is_some()
            && !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            let cause = "v2 fragment-index engine changed before fragment publication".to_owned();
            record_failure(
                &self.shared,
                &self.rendition,
                crate::playback_control::ProducerDecisionReason::EngineChanged,
                cause.clone(),
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, cause));
        }
        let len = bytes.len() as u64;
        {
            let mut manifest = self.rendition.manifest.lock().await;
            // Checked under the manifest lock, so a driver bumping the epoch
            // cannot interleave between the check and the write.
            if self.rendition.gen_epoch.load(Relaxed) != self.epoch {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            let before = manifest.state(entry).map(|s| s.bytes()).unwrap_or(0);
            self.rendition
                .dir
                .materialize(&mut manifest, entry, &bytes, now_ms())
                .await?;
            // Publication and provenance linearize under the same manifest
            // lock. A skip can therefore observe neither fact or both, never
            // real prewarm bytes with a missing credit.
            credit_marker_prewarm_publication(&self.rendition, self.epoch, entry).await;
            if !manifest.is_admitted() {
                sub_saturating(&self.shared.working_set, before);
                self.shared.working_set.fetch_add(len, Relaxed);
            }
            if manifest.next_gap(0).is_none() {
                self.shared.try_admit(&self.rendition, &mut manifest).await;
            }
            self.rendition.clear_demand(entry);
        }
        self.rendition.slot.produced(entry).await;
        self.shared.pool.satisfy(&self.rendition.key, entry);
        self.rendition.kick();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The CutPolicy the plans were derived from and every generation cuts with.
fn shipped_policy(timescale: u32) -> CutPolicy {
    CutPolicy::new(
        COPY_SEGMENT_SECONDS,
        COPY_FIRST_SEGMENT_SECONDS,
        COPY_SEGMENT_MAX_BYTES,
        COPY_SEGMENT_MAX_SECS,
        timescale,
    )
}

/// The rendition key: the copy recipe EXCLUDING `start_seconds` (the cache's
/// key discipline, plan §2.4), hashed into a directory-safe hex name.
fn rendition_key(recipe: &Recipe, identity: &SourceIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(recipe.file.id.to_le_bytes());
    hasher.update(identity.size.to_le_bytes());
    hasher.update(identity.mtime_ms.to_le_bytes());
    hasher.update(identity.argv_fingerprint.as_bytes());
    hasher.update(recipe.audio_index.unwrap_or(-1).to_le_bytes());
    hasher.update([
        u8::from(recipe.aac),
        u8::from(recipe.video.preserves_dolby_vision()),
        u8::from(recipe.video.promotes_parameter_sets()),
    ]);
    hasher.update(recipe.file.audio_offset_ms.to_le_bytes());
    match recipe.cluster_cache_key.as_deref() {
        Some(cache_key) => {
            hasher.update(b"cluster-v2\0");
            hasher.update(cache_key.as_bytes());
        }
        None => hasher.update(b"legacy-v1\0"),
    }
    hex::encode(hasher.finalize())
}

/// The per-track durations `plan_copy`'s audio tail (the c58a4307 rule) is
/// computed from — and the split matters more than either number:
///
/// - `video_ms` comes honestly from the **fragment index** (the video-only
///   pipe's own summed ticks), because the tail is `audio_ms - video_ms` and
///   the container duration is the max of both tracks. Using the container
///   number for video makes the tail identically zero on every file, so any
///   title whose audio outruns its video would emit trailing audio the plan
///   never named — an out-of-plan chunk that poisons the rendition at the end
///   of every complete watch.
/// - `audio_ms` is the container's probed duration: audio is the track that
///   outruns, and the probe's number is what the c58a4307 rule was written
///   against.
fn track_durations(index: &FragmentIndex, recipe: &Recipe, container_ms: i64) -> TrackDurations {
    TrackDurations {
        video_ms: index_video_ms(index),
        audio_ms: container_ms,
        audio_bits_per_second: audio_rate(recipe),
    }
}

/// The index's total video duration in ms — the sum of its fragment ticks on
/// its own timescale.
fn index_video_ms(index: &FragmentIndex) -> i64 {
    ticks_to_ms(index.ticks(), index.timescale)
}

fn ticks_to_ms(ticks: u64, timescale: u32) -> i64 {
    (ticks.saturating_mul(1000) / u64::from(timescale.max(1))) as i64
}

/// The audio rate the plan's byte headroom is computed from: what the
/// production pipe asks for (`-b:a`, mirrored from `copy_pipe_args`' branch)
/// when the audio is re-encoded, and a deliberately generous stand-in when it
/// is copied — `MediaFile` carries no per-stream audio rate, and `est_bytes`
/// feeds admission, never a refusal.
fn audio_rate(recipe: &Recipe) -> u32 {
    if recipe.aac {
        let channels = match recipe.audio_index {
            Some(index) => recipe
                .file
                .audio_streams
                .iter()
                .find(|stream| stream.index == index),
            None => recipe.file.audio_streams.first(),
        }
        .and_then(|stream| stream.channels);
        if channels == Some(6) {
            320_000
        } else {
            256_000
        }
    } else {
        640_000
    }
}

/// `segNNNNN.m4s` → its plan index; anything else — traversal included, by
/// the same digit discipline `is_safe_segment` enforces — is `None`.
fn planned_index(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("seg")?.strip_suffix(".m4s")?;
    if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The plan entry containing `start_seconds` — where the session's first
/// demand points, not where the plan starts. Always a VIDEO entry: a start
/// inside the audio tail positions at the last video entry instead, because
/// that is the generation that produces the tail.
fn entry_containing(plan: &SegmentPlan, start_seconds: f64) -> u32 {
    if start_seconds <= 0.0 {
        return video_entry_at_or_before(plan, 0);
    }
    let ticks = (start_seconds * f64::from(plan.timescale.max(1))) as u64;
    let mut at = 0u32;
    for entry in &plan.entries {
        if entry.start_ticks <= ticks {
            at = entry.index;
        } else {
            break;
        }
    }
    video_entry_at_or_before(plan, at)
}

/// Apply one accepted control snapshot to the playback's speculative ledger.
/// The accepted playback anchor has already committed with the control
/// sequence. Marker attribution uses only ranges credited by `RenditionSink`
/// and still materialized at this exact instant.
async fn apply_marker_prewarm_control(
    rendition: &Arc<Rendition>,
    session_id: &str,
    sequence: u64,
    snapshot: &crate::playback_control::PlaybackDemandSnapshot,
    destinations: &[MarkerDestination],
) -> Option<MarkerPrewarmOutcome> {
    let ledger = rendition
        .readers
        .lock()
        .await
        .get(session_id)
        .map(|reader| Arc::clone(&reader.marker_prewarm))?;
    let manifest = rendition.manifest.lock().await;
    let publications = rendition
        .publication_versions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ledger = ledger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // A landing observed before its fire-and-forget beacon is a one-control
    // reorder latch, not durable skip intent. Any later accepted control
    // proves the beacon did not accompany that landing; retaining it would
    // let a natural traversal manufacture a hit on a future replay.
    ledger.awaiting_beacon = None;

    let outcome = if snapshot.render_state == crate::playback_control::RenderState::Seeking {
        ledger.enabled = false;
        ledger.deactivate();
        None
    } else {
        let landing_entry =
            entry_containing(&rendition.plan, snapshot.position_ms as f64 / 1_000.0);
        let outcome = ledger.observe_control_landing(
            snapshot.position_ms,
            landing_entry,
            &manifest,
            &publications,
        );
        ledger.update_control(sequence, snapshot, destinations);
        outcome
    };
    drop(ledger);
    drop(publications);
    drop(manifest);
    rendition.kick();
    outcome
}

/// Read only the persisted annotation index and project each exact client
/// landing time onto the immutable VOD plan. A store miss is intentionally a
/// no-op: marker probing belongs to `/decision`, and prewarm must never add a
/// detector or source read to the control path.
async fn stored_marker_destinations(
    store: &dyn Store,
    file: &MediaFile,
    plan: &SegmentPlan,
    seconds_per_segment: f64,
) -> Vec<MarkerDestination> {
    let source = crate::http::stream::annotation_source_identity(file);
    let set = match store.timeline_annotation_set(file.id, &source).await {
        Ok(Some(set)) => set,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(
                file_id = file.id,
                %error,
                "could not read timeline annotations for marker prewarm"
            );
            return Vec::new();
        }
    };
    let per = if seconds_per_segment > 0.0 {
        seconds_per_segment
    } else {
        1.0
    };
    let window_entries = ((f64::from(AHEAD_HORIZON_SECONDS) / per).ceil() as u32).max(1);
    let last_entry = plan.entries.last().map_or(0, |entry| entry.index);
    let mut destinations = set
        .annotations
        .into_iter()
        .map(|annotation| {
            let target_entry = entry_containing(plan, annotation.end_ms as f64 / 1_000.0);
            MarkerDestination {
                kind: annotation.kind,
                start_ms: annotation.start_ms,
                end_ms: annotation.end_ms,
                target_entry,
                window_end_entry: target_entry.saturating_add(window_entries).min(last_entry),
                eligible: matches!(
                    annotation.kind,
                    AnnotationKind::Intro | AnnotationKind::Credits
                ),
            }
        })
        .collect::<Vec<_>>();
    destinations.sort_by_key(|destination| (destination.start_ms, destination.end_ms));
    destinations
}

/// The last VIDEO entry at or before `at` — the only kind a generation may be
/// positioned on. Audio-tail entries carry no video boundary of their own
/// ([`crate::titlestore::Manifest::is_audio_tail`]'s "a scheduler must not
/// chase them as if they were independently seekable" — this is that caller
/// arriving): `vodgen` categorically refuses a generation started inside the
/// tail, so an unclamped spawn there would answer `producer_failed` on every
/// seek to the end of an affected film. The tail entries are produced by this
/// generation's own `finish`.
fn video_entry_at_or_before(plan: &SegmentPlan, at: u32) -> u32 {
    let mut best = 0u32;
    for entry in &plan.entries {
        if entry.index > at {
            break;
        }
        if entry.kind == PlanEntryKind::Video {
            best = entry.index;
        }
    }
    best
}

fn plan_duration_ms(plan: &SegmentPlan) -> i64 {
    (plan.duration_ticks().saturating_mul(1000) / u64::from(plan.timescale.max(1))) as i64
}

/// The current playback window, never the interval between every position
/// visited during this session. Admitted GETs have independent narrow pins,
/// including the publication-to-open race, so seeking does not need to retain
/// the entire intervening film.
fn reader_window(reader: &Reader, seconds_per_segment: f64) -> ReaderWindow {
    let playhead = reader.frontier;
    let frontier = reader.frontier.saturating_add(1);
    let per = if seconds_per_segment > 0.0 {
        seconds_per_segment
    } else {
        1.0
    };
    // `ceil`, matching `Position::horizon_segments`, which is what bounds how
    // far the producer may run. Truncating made the protected range shorter
    // than the range the ahead-fill is allowed to reach, so a sweep under
    // pressure could evict the segment the producer was about to write again.
    let ahead = ((f64::from(AHEAD_HORIZON_SECONDS) / per).ceil() as u32).max(1);
    ReaderWindow {
        back: 2,
        playhead,
        frontier,
        ahead,
    }
}

async fn open_ready(
    path: &Path,
    etag_stem: &str,
    delivery: &Arc<crate::meter::Meter>,
) -> io::Result<SegmentReady> {
    let file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    Ok(SegmentReady {
        file,
        len,
        etag: format!("{etag_stem}-{len}"),
        delivery: Arc::clone(delivery),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

fn sub_saturating(counter: &AtomicU64, bytes: u64) {
    let _ = counter.fetch_update(Relaxed, Relaxed, |current| {
        Some(current.saturating_sub(bytes))
    });
}

/// Handoff §5's regenerate-and-verify: spawn the generation child at entry 0,
/// read its pipe only to the muxer init, kill the child, and answer the
/// served init the stored identity derives from it — or why it refused.
///
/// This is how a resurrected rendition with a complete (or gap-free-enough)
/// manifest gets its `init.mp4` back without producing a single segment: the
/// §2 ruling made the init reproducible independently of the segments, so a
/// verified head is proof enough to keep every adopted byte.
async fn regenerate_init_head(
    recipe: &Recipe,
    source: &crate::fragment_index_cluster::SourceFence,
    identity: &InitIdentity,
    slots: &Arc<Semaphore>,
) -> Result<Init, HeadRegenerationError> {
    let _permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| HeadRegenerationError::Busy)?;
    if recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        return Err(HeadRegenerationError::Failed(
            "the v2 fragment-index engine changed before head regeneration".to_owned(),
        ));
    }
    if !source.unchanged() {
        return Err(HeadRegenerationError::Failed(
            "source changed before head regeneration".to_owned(),
        ));
    }
    let mut args = copy_pipe_args_with_dolby_vision(
        &recipe.file,
        0.0,
        recipe.audio_index,
        recipe.aac,
        Pacing::unpaced(),
        recipe.video,
    );
    #[cfg(unix)]
    replace_inputs_with_attested_descriptor(&mut args);
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    attach_attested_descriptor(&mut command, Some(source));
    let mut child = command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            HeadRegenerationError::Failed(format!("spawning the head regeneration: {error}"))
        })?;
    let Some(stdout) = child.stdout.take() else {
        return Err(HeadRegenerationError::Failed(
            "the head regeneration started without a stdout".to_owned(),
        ));
    };
    let muxer = read_regenerated_head_before(
        child,
        stdout,
        HEAD_REGENERATION_TIMEOUT,
        HEAD_REGENERATION_MAX_BYTES,
    )
    .await?;
    if !source.unchanged() {
        return Err(HeadRegenerationError::Failed(
            "source changed during head regeneration".to_owned(),
        ));
    }
    if recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        return Err(HeadRegenerationError::Failed(
            "the v2 fragment-index engine changed during head regeneration".to_owned(),
        ));
    }
    identity
        .served_init_for(&muxer)
        .map_err(|refused| HeadRegenerationError::Failed(refused.to_string()))
}

/// Read exactly one regeneration child's muxer head within the supplied
/// budget, then always kill and confirm-reap the child before returning. The
/// owner also transfers reap to a detached task if this future is cancelled.
async fn read_regenerated_head_before(
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    budget: Duration,
    max_bytes: usize,
) -> Result<Init, HeadRegenerationError> {
    // The stdout arrives separately from the child because the owner below
    // takes the child by value, and the read needs the pipe after that move.
    let mut child = HeadChildOwner::new(child);
    let mut stdout = stdout;
    let head = tokio::time::timeout(budget, read_muxer_init_bounded(&mut stdout, max_bytes)).await;
    // Only the head is wanted; the rest of the pipe is not read.
    drop(stdout);
    child.terminate_and_reap().await;
    let (_consumed, muxer) = head
        .map_err(|_| {
            HeadRegenerationError::Failed(format!(
                "head regeneration exceeded its {:.1}s deadline",
                budget.as_secs_f64()
            ))
        })?
        .map_err(|error| {
            if error.kind() == io::ErrorKind::FileTooLarge {
                HeadRegenerationError::Oversize
            } else {
                HeadRegenerationError::Failed(format!(
                    "reading the head regeneration's init: {error}"
                ))
            }
        })?;
    Ok(muxer)
}

async fn read_muxer_init_bounded<R: AsyncRead + Unpin>(
    src: &mut R,
    max_bytes: usize,
) -> io::Result<(Vec<u8>, Init)> {
    let mut consumed = Vec::new();
    let mut reader = FragmentReader::new();
    let mut buf = vec![0u8; (256 * 1024).min(max_bytes.max(1))];
    loop {
        let remaining = max_bytes.saturating_sub(consumed.len());
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "the pipe did not emit its init within the byte limit",
            ));
        }
        let take = buf.len().min(remaining);
        let n = src.read(&mut buf[..take]).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the pipe ended before its moov arrived",
            ));
        }
        consumed.extend_from_slice(&buf[..n]);
        reader.push(&buf[..n]);
        loop {
            match reader.next_unit() {
                Ok(Some(Unit::Init(mut init))) => {
                    sanitize_stale_dolby_brand(&mut init);
                    return Ok((consumed, init));
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("parsing the generation's pipe: {error}"),
                    ));
                }
            }
        }
    }
}

/// Read a generation's pipe up to (and through) its muxer init, keeping every
/// consumed byte so the whole stream can be replayed in front of the live
/// pipe for [`vodgen::run`].
async fn read_muxer_init<R: AsyncRead + Unpin>(src: &mut R) -> io::Result<(Vec<u8>, Init)> {
    let mut consumed = Vec::new();
    let mut reader = FragmentReader::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the pipe ended before its moov arrived",
            ));
        }
        consumed.extend_from_slice(&buf[..n]);
        reader.push(&buf[..n]);
        loop {
            match reader.next_unit() {
                Ok(Some(Unit::Init(mut init))) => {
                    sanitize_stale_dolby_brand(&mut init);
                    return Ok((consumed, init));
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("parsing the generation's pipe: {error}"),
                    ));
                }
            }
        }
    }
}

/// The persisted form of [`InitIdentity`] — a serde mirror, because the type
/// itself does not derive `Serialize`.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredIdentity {
    muxer_init: String,
    served_init: String,
    promotion: plurx_core::fmp4::PromotionInputs,
}

async fn load_identity(path: &Path) -> Option<InitIdentity> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let stored: StoredIdentity = serde_json::from_slice(&bytes).ok()?;
    Some(InitIdentity {
        muxer_init: stored.muxer_init,
        served_init: stored.served_init,
        promotion: stored.promotion,
    })
}

/// Persist the identity tmp-then-rename, like every other rendition file:
/// absent or complete, never partial.
async fn store_identity(path: &Path, identity: &InitIdentity) -> io::Result<()> {
    let stored = StoredIdentity {
        muxer_init: identity.muxer_init.clone(),
        served_init: identity.served_init.clone(),
        promotion: identity.promotion.clone(),
    };
    let bytes = serde_json::to_vec(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

async fn sync_file(path: &Path) -> io::Result<()> {
    match tokio::fs::File::open(path).await {
        Ok(file) => file.sync_all().await,
        // No identity yet is a rendition that never generated — admissible
        // only in tests that fake materialization; nothing to sync.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::AcqRel};

    use plurx_core::store::{
        FragmentIndexStore, LibraryStore as _, MediaSessionStore as _, MediaStore as _,
        PlaybackTelemetryStore as _, SqliteStore, TimelineAnnotationStore as _,
    };
    use plurx_core::testfixtures;

    use crate::fragindex::IndexOutcome;

    struct CleanupObservingCommitter {
        rendition: Arc<Rendition>,
        session_id: String,
        started: AtomicBool,
        reader_detached: AtomicBool,
        attempts: Arc<AtomicUsize>,
        expires_at_unix_ms: i64,
    }

    #[derive(Default)]
    struct RecordingPreparationAdmission {
        outcome: std::sync::Mutex<Option<crate::playback_control::PreparationControlOutcome>>,
    }

    impl crate::playback_control::PreparationSettlementAdmission for RecordingPreparationAdmission {
        fn accepted(&self, outcome: crate::playback_control::PreparationControlOutcome) {
            *self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        }
    }

    impl crate::playback_control::TerminalControlCommitter for CleanupObservingCommitter {
        fn start(
            &self,
            result: &crate::playback_control::LocalControlResult,
        ) -> crate::playback_control::TerminalCommitReceipt {
            self.started.store(true, Release);
            let detached = self
                .rendition
                .readers
                .try_lock()
                .ok()
                .is_some_and(|readers| !readers.contains_key(&self.session_id));
            self.reader_detached.store(detached, Release);
            let response = crate::playback_control::terminal_response_for_test(result);
            let attempts = Arc::clone(&self.attempts);
            crate::playback_control::TerminalCommitReceipt::retryable_until(
                self.expires_at_unix_ms,
                move |attempt| {
                    let index = attempts.fetch_add(1, AcqRel);
                    attempt.complete(if index == 0 {
                        Err(())
                    } else {
                        Ok(response.clone())
                    });
                },
            )
        }
    }

    /// The conversion reaches the pipeline, and therefore the identity.
    ///
    /// This is the join between "the plan review said convert" and everything
    /// downstream: the generation, the playlist facts and the index pass all
    /// read these options rather than carrying their own copy of the answer,
    /// so the flag arriving here is what makes them agree — and the argv
    /// fingerprint is what stops a converting session ever landing on the
    /// unconverted stream's index.
    #[test]
    fn a_converting_session_gets_a_converting_pipeline_and_its_own_identity() {
        let mut file = fixture_file();
        file.hdr = Some("dolby_vision".into());
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);

        let converting = copy_video_pipeline(&file, None, true, true, true);
        assert!(converting.converts_dolby_vision());
        assert!(
            converting.preserves_dolby_vision(),
            "there is nothing to convert in a stream the filter removed"
        );

        let preserving = copy_video_pipeline(&file, None, true, true, false);
        assert!(!preserving.converts_dolby_vision());
        assert!(preserving.preserves_dolby_vision());

        let stripping = copy_video_pipeline(&file, None, true, false, false);
        assert!(!stripping.preserves_dolby_vision());

        // Three pipelines, three identities. Two of them sharing one would
        // hand a session a playlist whose cut points describe media it never
        // produces — the failure the whole third-identity design exists to
        // prevent.
        let fingerprints: std::collections::HashSet<_> = [converting, preserving, stripping]
            .into_iter()
            .map(|video| crate::fragindex::identity_for(&file, video).argv_fingerprint)
            .collect();
        assert_eq!(fingerprints.len(), 3, "{fingerprints:?}");
    }

    fn fixture_file() -> MediaFile {
        media_file_at(testfixtures::source("clean-cra"), 12_000)
    }

    fn media_file_at(path: PathBuf, duration_ms: i64) -> MediaFile {
        MediaFile {
            id: 1,
            item_id: 1,
            path,
            size: 1,
            mtime: 1,
            duration_ms: Some(duration_ms),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_profile: Some("Main".into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(1_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn request(playback_id: &str, start_seconds: f64) -> SessionRequest {
        SessionRequest {
            control_sequence: None,
            file_id: 1,
            playback_id: playback_id.to_string(),
            request_id: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
        }
    }

    fn settings() -> VodSettings {
        VodSettings {
            working_set_bytes: 8 << 30,
            completed_cache_bytes: 50 << 30,
            block_budget: Duration::from_secs(30),
            materialize_budget: Duration::from_secs(30),
            blocked_get_cap: DEFAULT_GLOBAL_WAIT_CAP,
        }
    }

    #[test]
    fn forced_generation_holder_repair_targets_the_resolved_artifact() {
        let logical_key = "a".repeat(64);
        let generation_key = "b".repeat(64);
        let repair = plurx_core::store::NewClusterFragmentIndexJob {
            cache_key: logical_key,
            file_id: 1,
            source_size: 10,
            source_mtime: 20,
            source_sha256: "c".repeat(64),
            pipeline_sha256: "d".repeat(64),
            priority: "foreground".to_owned(),
            trigger: "foreground".to_owned(),
            target_node_id: "node-a".to_owned(),
            not_before_ms: 30,
            created_at_ms: 30,
        };
        let artifact = plurx_core::store::ClusterFragmentIndexArtifact {
            cache_key: generation_key.clone(),
            file_id: 1,
            source_size: 10,
            source_mtime: 20,
            source_sha256: repair.source_sha256.clone(),
            pipeline_sha256: repair.pipeline_sha256.clone(),
            blob_sha256: "e".repeat(64),
            bytes: 40,
            built_by_node_id: "node-a".to_owned(),
            built_at_ms: 30,
        };

        let resolved = repair_job_for_artifact(repair, &artifact);

        assert_eq!(resolved.cache_key, generation_key);
        assert_eq!(resolved.source_sha256, artifact.source_sha256);
        assert_eq!(resolved.pipeline_sha256, artifact.pipeline_sha256);
    }

    /// A store holding the fixture's real fragment index, built by the real
    /// index pipe — the whole attach sequence starts from what a scan would
    /// have persisted.
    async fn store_with_index(file: &MediaFile) -> (Arc<dyn Store>, FragmentIndex) {
        testfixtures::require_ffmpeg();
        let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
        let runtime = crate::test_tempdir().expect("runtime cache dir");
        let outcome = crate::fragindex::build(
            file,
            CopyVideoOptions::new(have_dovi, false),
            runtime.path(),
            Duration::from_secs(120),
        )
        .await;
        let IndexOutcome::Built(index) = outcome else {
            panic!("the fixture must index: {outcome:?}");
        };
        let store = SqliteStore::open_in_memory().expect("store");
        store
            .put_fragment_index(file.id, &index)
            .await
            .expect("store the index");
        (Arc::new(store) as Arc<dyn Store>, *index)
    }

    async fn serve_on(base: &Path) -> (Arc<VodServe>, MediaFile) {
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        (VodServe::new(base.to_path_buf(), store), file)
    }

    /// A `VodServe` with an empty store, for tests that drive internals
    /// directly against a hand-built rendition.
    fn bare_serve(base: &Path) -> Arc<VodServe> {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        VodServe::new(base.to_path_buf(), store)
    }

    async fn activate_control_route(store: &SqliteStore, session_id: &str, generation: &str) {
        let now_ms = crate::media_sessions::unix_ms();
        let fingerprint = "a".repeat(64);
        let playback_id = format!("vod-control-{session_id}");
        store
            .claim_media_session_request(
                7,
                generation,
                &fingerprint,
                &playback_id,
                generation,
                now_ms,
                now_ms + 60_000,
            )
            .await
            .expect("claim route");
        assert!(store
            .assign_media_session_request_owner(7, generation, generation, "node-a", now_ms,)
            .await
            .expect("assign route owner"));
        let activation = plurx_core::domain::MediaSessionActivation {
            incarnation_id: generation.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id,
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: Some(generation.to_owned()),
            request_fingerprint: fingerprint,
            owner_node_id: "node-a".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms,
            lease_expires_at_ms: now_ms + 60_000,
        };
        store
            .activate_media_session(&activation)
            .await
            .expect("activate route")
            .expect("route accepted");
        store
            .settle_media_session_activation(
                &activation,
                plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now_ms,
            )
            .await
            .expect("confirm route")
            .expect("route confirmed");
        store
            .publish_media_session_activation(7, generation, generation, now_ms)
            .await
            .expect("publish route")
            .expect("route published");
    }

    async fn activate_ended_route(store: &SqliteStore, session_id: &str, generation: &str) {
        activate_control_route(store, session_id, generation).await;
        let ended = store
            .end_media_session(session_id, "deleted", crate::media_sessions::unix_ms())
            .await
            .expect("end durable VOD route")
            .expect("durable VOD route exists");
        assert_eq!(ended.state, "ended");
        assert_eq!(ended.incarnation_id, generation);
    }

    async fn insert_control_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
        touched: Instant,
    ) {
        rendition.attach_reader(session_id, 0).await;
        let rendition_key = rendition.key.clone();
        let file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition: Some(rendition),
                rendition_key,
                file,
                playback_id: "vod-control".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("vod-control", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle(session_id),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(touched),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
    }

    async fn insert_finished_terminal_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
    ) {
        let cleanup = Arc::new(TerminalCleanup::new());
        cleanup.complete();
        insert_terminal_session(serve, session_id, rendition, cleanup).await;
    }

    async fn insert_terminal_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
        cleanup: Arc<TerminalCleanup>,
    ) {
        let rendition_key = rendition.key.clone();
        let file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition: None,
                rendition_key,
                file,
                playback_id: "vod-terminal".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("vod-terminal", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle(session_id),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: Some(cleanup),
                tombstone: Some(Terminal::Deleted),
            },
        );
    }

    /// A synthetic ~7 s-per-segment index, prodsched's own fixture shape.
    fn synthetic_index(fragments: usize) -> FragmentIndex {
        use plurx_core::fmp4::CutClass;
        use plurx_core::segplan::IndexRow;
        let mut rows = Vec::new();
        let mut dts = 0u64;
        for i in 0..fragments {
            let duration = if i % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes: 100_000,
                video_bytes: 99_400,
                class: CutClass::CleanIdr,
            });
            dts += duration;
        }
        FragmentIndex::new(
            16_000,
            rows,
            "sha",
            SourceIdentity::new(1, 1, "fingerprint"),
        )
    }

    /// A rendition built by hand — no driver, no store, no producer — for
    /// tests that exercise one internal mechanism deterministically.
    async fn synthetic_rendition(base: &Path) -> Arc<Rendition> {
        let index = synthetic_index(240);
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        );
        let dir = RenditionDir::new(base.join("synthetic"));
        dir.create().await.expect("create rendition dir");
        let plan_len = plan.len();
        Arc::new(Rendition {
            key: "synthetic-rendition".to_string(),
            dir,
            recipe: Recipe {
                file: media_file_at(PathBuf::from("unused.mkv"), ms),
                audio_index: None,
                aac: true,
                video: CopyVideoOptions::new(false, false),
                source_object_version: None,
                cluster_cache_key: None,
            },
            source: None,
            playlist: plan.playlist().into_bytes(),
            timescale: plan.timescale,
            seconds_per_segment: plan.duration_ticks() as f64
                / f64::from(plan.timescale)
                / plan.len() as f64,
            index,
            policy,
            working_set_budget: 8 << 30,
            completed_cache_budget: 50 << 30,
            materialize_budget: Duration::from_secs(30),
            manifest: Mutex::new(Manifest::new(plan.clone())),
            plan,
            identity: Mutex::new(IdentityState::default()),
            slot: ProducerSlot::new(),
            readers: Mutex::new(HashMap::new()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            capacity_hold: StdMutex::new(None),
            init_notify: Notify::new(),
            wake: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            dormant_since: StdMutex::new(None),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        })
    }

    #[tokio::test]
    async fn stored_marker_prewarm_is_subordinate_and_hits_only_its_own_production() {
        use plurx_core::segplan::{
            AnnotationProvenance, TimelineAnnotation, TimelineAnnotationSet,
        };

        let base = crate::test_tempdir().expect("base");
        let rendition = synthetic_rendition(base.path()).await;
        let file = &rendition.recipe.file;
        let duration_ms = file.duration_ms.expect("synthetic duration");
        assert!(
            duration_ms > 400_000,
            "fixture must contain the credits marker"
        );
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&plurx_core::domain::NewLibrary {
                name: "Markers".to_owned(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: Vec::new(),
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&plurx_core::domain::NewItem {
                library_id: library.id,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "Marker fixture".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let stored_file_id = store
            .upsert_file(
                item,
                &file.path.to_string_lossy(),
                file.size,
                file.mtime,
                &plurx_core::domain::ProbeResult {
                    duration_ms: Some(duration_ms),
                    ..plurx_core::domain::ProbeResult::default()
                },
            )
            .await
            .expect("file");
        assert_eq!(stored_file_id, file.id);
        let annotations = TimelineAnnotationSet {
            source_identity: crate::http::stream::annotation_source_identity(file),
            generation_id: uuid::Uuid::new_v4().to_string(),
            version: 1,
            annotations: vec![TimelineAnnotation {
                kind: AnnotationKind::Credits,
                start_ticks: 200_000,
                end_ticks: 400_000,
                timescale: 1_000,
                start_ms: 200_000,
                end_ms: 400_000,
                provenance: AnnotationProvenance::Authored,
                confidence_millis: 1_000,
                detector_version: "stored-marker-fixture".to_owned(),
                manual_override_revision: None,
            }],
        };
        store
            .put_timeline_annotation_set(file.id, duration_ms, &annotations)
            .await
            .expect("store exact marker");
        let destinations = stored_marker_destinations(
            store.as_ref(),
            file,
            &rendition.plan,
            rendition.seconds_per_segment,
        )
        .await;
        assert_eq!(destinations.len(), 1);
        let destination = destinations[0];
        assert_eq!(destination.end_ms, 400_000);
        assert!(destination.eligible);

        let frontier = entry_containing(&rendition.plan, 150.0);
        assert!(destination.target_entry > frontier);
        rendition.attach_reader("prewarmed", frontier).await;
        let ledger = rendition.readers.lock().await["prewarmed"]
            .marker_prewarm
            .clone();
        let mut rendering = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        rendering.position_ms = 150_000;
        rendering.buffered_from_ms = Some(145_000);
        rendering.buffered_through_ms = 170_000;
        assert!(apply_marker_prewarm_control(
            &rendition,
            "prewarmed",
            7,
            &rendering,
            &destinations,
        )
        .await
        .is_none());
        assert_eq!(
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records
                .len(),
            1,
            "the approaching stored marker registers one bounded request"
        );

        let ledgers = vec![Arc::clone(&ledger)];
        let mut manifest = rendition.manifest.lock().await;
        let empty_position = Position {
            produced_through: None,
            positioned_at: None,
            seconds_per_segment: rendition.seconds_per_segment,
            working_set: WorkingSet::default(),
        };
        assert_eq!(
            decide(&manifest, &[Demand::idle_at(frontier)], empty_position, &[]),
            Action::Reposition { to: frontier },
            "the foreground fixture itself starts at the playhead window"
        );
        let foreground = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            empty_position,
            &[],
            &ledgers,
        );
        assert_eq!(
            foreground.action,
            Action::Reposition { to: frontier },
            "ordinary playhead fill wins before speculative work"
        );
        assert!(foreground.owners.is_empty());
        assert!(
            !ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records[0]
                .active
        );

        let horizon = ((f64::from(AHEAD_HORIZON_SECONDS) / rendition.seconds_per_segment).ceil()
            as u32)
            .max(1);
        let foreground_end = frontier.saturating_add(horizon);
        assert!(foreground_end < destination.target_entry);
        for entry in 0..=foreground_end {
            assert!(manifest.materialize(entry, 1_000, i64::from(entry)));
        }
        let filled_position = Position {
            produced_through: Some(foreground_end),
            positioned_at: Some(0),
            seconds_per_segment: rendition.seconds_per_segment,
            working_set: WorkingSet {
                used_bytes: 2,
                budget_bytes: 1,
                held: false,
            },
        };
        let pressured = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            filled_position,
            &[],
            &ledgers,
        );
        assert!(matches!(
            pressured.action,
            Action::Suspend {
                reason: crate::prodsched::Hold::Ahead { .. },
                ..
            }
        ));
        assert!(pressured.owners.is_empty());
        assert!(
            !ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records[0]
                .active,
            "prewarm never makes room under pressure"
        );

        let unpressured = Position {
            working_set: WorkingSet::default(),
            ..filled_position
        };
        let prewarm = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            unpressured,
            &[],
            &ledgers,
        );
        assert_eq!(
            prewarm.action,
            Action::Produce {
                next: foreground_end + 1
            },
            "the same bounded scheduler advances toward the target only after foreground is full"
        );
        assert_eq!(prewarm.owners.len(), 1);
        let producer = Producer::Running {
            produced_through: Some(foreground_end),
            positioned_at: 0,
        };
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, prewarm.action),
            &prewarm,
        );
        let mut prewarm_publication = None;
        for entry in destination.target_entry..=destination.window_end_entry {
            assert!(manifest.materialize(entry, 1_000, i64::from(entry)));
            let publication = credit_marker_prewarm_publication(
                &rendition,
                rendition.gen_epoch.load(Relaxed),
                entry,
            )
            .await;
            if entry == destination.target_entry {
                prewarm_publication = Some(publication);
            }
        }
        let prewarm_publication = prewarm_publication.expect("target publication");
        let completed = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            unpressured,
            &[],
            &ledgers,
        );
        assert!(
            completed.owners.is_empty(),
            "a complete prewarm window stops scheduling but remains correlatable"
        );
        let pending = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(
            pending.matched,
            "the marker beacon binds the unique approach request"
        );
        assert!(
            pending.outcome.is_none(),
            "the approach position is not a landing"
        );
        drop(manifest);

        let mut seeking = rendering.clone();
        seeking.position_ms = destination.end_ms;
        seeking.buffered_from_ms = Some(destination.end_ms);
        seeking.buffered_through_ms = destination.end_ms;
        seeking.render_state = crate::playback_control::RenderState::Seeking;
        seeking.seek_target_ms = Some(destination.end_ms);
        assert!(
            apply_marker_prewarm_control(&rendition, "prewarmed", 9, &seeking, &destinations,)
                .await
                .is_none(),
            "an arbitrary exact-target seek is not a marker skip"
        );

        let mut landed = rendering.clone();
        landed.position_ms = destination.end_ms;
        landed.buffered_from_ms = Some(destination.end_ms);
        landed.buffered_through_ms = destination.end_ms;
        let hit = apply_marker_prewarm_control(&rendition, "prewarmed", 10, &landed, &destinations)
            .await
            .expect("the first settled post-seek snapshot emits the result");
        assert!(hit.hit);
        assert_eq!(hit.requested_sequence, Some(7));
        assert!(hit
            .produced_range
            .is_some_and(|range| (range.first..=range.last).contains(&destination.target_entry)));
        let serve = VodServe::new(
            base.path().join("marker-telemetry"),
            Arc::clone(&store) as Arc<dyn Store>,
        );
        serve.emit_marker_prewarm(
            "prewarmed",
            file.id,
            SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            hit,
        );
        let expected_session = session_log_id("prewarmed");
        let mut emitted = None;
        for _ in 0..100 {
            let events = store
                .playback_events(&plurx_core::domain::PlaybackEventQuery {
                    event: Some("marker_prewarm".to_owned()),
                    limit: 10,
                    ..plurx_core::domain::PlaybackEventQuery::default()
                })
                .await
                .expect("read marker telemetry");
            emitted = events
                .into_iter()
                .find(|event| event.session_id.as_deref() == Some(expected_session.as_str()));
            if emitted.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let emitted = emitted.expect("server marker prewarm telemetry persisted");
        assert_eq!(emitted.detail.as_deref(), Some("hit"));
        let emitted_extra: serde_json::Value =
            serde_json::from_str(emitted.extra.as_deref().expect("prewarm proof"))
                .expect("valid prewarm proof JSON");
        assert_eq!(
            emitted_extra["destination_entry"].as_u64(),
            Some(u64::from(destination.target_entry))
        );
        assert_eq!(
            emitted_extra["produced_range"]["first_entry"].as_u64(),
            Some(u64::from(destination.target_entry))
        );
        let ratio = crate::telemetry::prometheus()
            .lines()
            .find_map(|line| {
                line.strip_prefix("plurx_playback_marker_prewarm_hit_ratio ")
                    .and_then(|value| value.parse::<f64>().ok())
            })
            .expect("marker prewarm ratio");
        assert!(ratio > 0.0, "a proven server hit moves the ratio off zero");
        let duplicate = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(duplicate.matched);
        assert!(duplicate.outcome.is_none(), "one skip emits only once");

        let mut inside_marker = rendering.clone();
        inside_marker.position_ms = 250_000;

        // The rendition now contains the landing segment, but it was produced
        // for another playback. This is the rejected ordinary-buffer
        // definition made adversarial: the second playback must still miss.
        rendition.attach_reader("ordinary-buffer", frontier).await;
        apply_marker_prewarm_control(&rendition, "ordinary-buffer", 1, &rendering, &destinations)
            .await;
        apply_marker_prewarm_control(
            &rendition,
            "ordinary-buffer",
            2,
            &inside_marker,
            &destinations,
        )
        .await;
        let ordinary_ledger = rendition.readers.lock().await["ordinary-buffer"]
            .marker_prewarm
            .clone();
        let miss = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut ledger = ordinary_ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let beacon = ledger.note_client_skip(&manifest, &publications);
            assert!(beacon.matched);
            assert!(beacon.outcome.is_none());
            ledger
                .settle_pending_at(destination.target_entry, &manifest, &publications)
                .expect("a later landing emits the uncredited miss")
        };
        assert!(!miss.hit);
        assert_eq!(miss.produced_range, None);
        let duplicate_inside = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ordinary_ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(duplicate_inside.matched);
        assert!(
            duplicate_inside.outcome.is_none(),
            "an inside-marker arm is consumed and cannot emit twice"
        );

        // Old credit cannot attach to a new ordinary publication of the same
        // entry after eviction.
        apply_marker_prewarm_control(&rendition, "prewarmed", 11, &inside_marker, &destinations)
            .await;
        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.evict(destination.target_entry));
        assert!(manifest.materialize(destination.target_entry, 2_000, 2));
        rendition.active_marker_prewarms.store(0, Release);
        let ordinary_publication = credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            destination.target_entry,
        )
        .await;
        assert_ne!(ordinary_publication, prewarm_publication);
        let rematerialized = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut ledger = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let beacon = ledger.note_client_skip(&manifest, &publications);
            assert!(beacon.matched);
            assert!(beacon.outcome.is_none());
            ledger
                .settle_pending_at(destination.target_entry, &manifest, &publications)
                .expect("the repeated explicit skip settles")
        };
        assert!(!rematerialized.hit, "ordinary regeneration is not prewarm");
        drop(manifest);

        // A disabled playback never receives another reader's shared
        // prewarm attribution.
        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.evict(destination.target_entry));
        drop(manifest);
        apply_marker_prewarm_control(&rendition, "prewarmed", 12, &rendering, &destinations).await;
        let mut held = rendering.clone();
        held.demand = crate::playback_control::PlaybackDemand::Hold;
        apply_marker_prewarm_control(&rendition, "prewarmed", 13, &held, &destinations).await;
        rendition.attach_reader("enabled", frontier).await;
        apply_marker_prewarm_control(&rendition, "enabled", 1, &rendering, &destinations).await;
        let enabled_ledger = rendition.readers.lock().await["enabled"]
            .marker_prewarm
            .clone();
        let mut manifest = rendition.manifest.lock().await;
        let enabled = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            unpressured,
            &[],
            &[Arc::clone(&ledger), Arc::clone(&enabled_ledger)],
        );
        assert_eq!(enabled.owners.len(), 1);
        assert!(!ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record.active));
        assert!(enabled_ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record.active));
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, enabled.action),
            &enabled,
        );
        assert!(manifest.materialize(destination.target_entry, 3_000, 3));
        let shared_publication = credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            destination.target_entry,
        )
        .await;
        let publications = rendition
            .publication_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            publications[destination.target_entry as usize],
            Some(shared_publication)
        );
        assert!(enabled_ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record
                .credited_publication(destination.target_entry, Some(shared_publication))));
        assert!(!ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record
                .credited_publication(destination.target_entry, Some(shared_publication))));
    }

    #[tokio::test]
    async fn marker_prewarm_skip_time_and_request_identity_are_immutable() {
        let base = crate::test_tempdir().expect("base");
        let rendition = synthetic_rendition(base.path()).await;
        let target_entry = entry_containing(&rendition.plan, 200.0);
        let destination = MarkerDestination {
            kind: AnnotationKind::Intro,
            start_ms: 100_000,
            end_ms: 200_000,
            target_entry,
            window_end_entry: target_entry.saturating_add(1),
            eligible: true,
        };
        let frontier = entry_containing(&rendition.plan, 50.0);
        let mut approach = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        approach.position_ms = 50_000;
        approach.buffered_from_ms = Some(45_000);
        approach.buffered_through_ms = 65_000;
        rendition.attach_reader("race", frontier).await;
        apply_marker_prewarm_control(&rendition, "race", 1, &approach, &[destination]).await;
        let ledger = rendition.readers.lock().await["race"]
            .marker_prewarm
            .clone();

        let manifest = rendition.manifest.lock().await;
        let candidate = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate(&manifest)
            .expect("approach candidate");
        let owners = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activate(candidate)
            .into_iter()
            .map(|record_nonce| MarkerPrewarmOwner {
                ledger: Arc::clone(&ledger),
                record_nonce,
            })
            .collect::<Vec<_>>();
        let decision = MarkerPrewarmDecision {
            action: Action::Reposition { to: target_entry },
            candidate: Some(candidate),
            owners,
        };
        let producer = Producer::Running {
            produced_through: None,
            positioned_at: target_entry,
        };
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, decision.action),
            &decision,
        );
        let before_publication = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(before_publication.matched);
        assert!(before_publication.outcome.is_none());
        drop(manifest);

        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.materialize(target_entry, 1_000, 1));
        credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            target_entry,
        )
        .await;
        let after_beacon = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settle_pending_at(target_entry, &manifest, &publications)
                .expect("landing settles the snapshotted miss")
        };
        assert!(
            !after_beacon.hit,
            "production after skip time cannot upgrade the miss"
        );

        // Reusing protocol sequence 1 after a settled request creates a new
        // ledger nonce. The old in-flight dispatch cannot credit that record.
        assert!(manifest.evict(target_entry));
        drop(manifest);
        apply_marker_prewarm_control(&rendition, "race", 1, &approach, &[destination]).await;
        let new_nonce = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .approach_skip
            .expect("replacement request")
            .nonce;
        assert_ne!(new_nonce, decision.owners[0].record_nonce);
        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.materialize(target_entry, 2_000, 2));
        credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            target_entry,
        )
        .await;
        let reused_sequence = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut state = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(state.note_client_skip(&manifest, &publications).matched);
            state
                .settle_pending_at(target_entry, &manifest, &publications)
                .expect("replacement landing")
        };
        assert!(
            !reused_sequence.hit,
            "an old dispatch cannot credit a nonce replacement"
        );
        assert!(manifest.evict(target_entry));
        drop(manifest);

        // The opposite network ordering is also deterministic: a Rendering
        // snapshot can prove landing before the fire-and-forget beacon arrives.
        rendition.attach_reader("late-beacon", frontier).await;
        apply_marker_prewarm_control(&rendition, "late-beacon", 1, &approach, &[destination]).await;
        let late = rendition.readers.lock().await["late-beacon"]
            .marker_prewarm
            .clone();
        let mut manifest = rendition.manifest.lock().await;
        let candidate = late
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate(&manifest)
            .expect("late candidate");
        let owners = late
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activate(candidate)
            .into_iter()
            .map(|record_nonce| MarkerPrewarmOwner {
                ledger: Arc::clone(&late),
                record_nonce,
            })
            .collect();
        let decision = MarkerPrewarmDecision {
            action: Action::Reposition { to: target_entry },
            candidate: Some(candidate),
            owners,
        };
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, decision.action),
            &decision,
        );
        assert!(manifest.materialize(target_entry, 3_000, 3));
        credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            target_entry,
        )
        .await;
        drop(manifest);
        let mut landed = approach.clone();
        landed.position_ms = destination.end_ms;
        landed.buffered_from_ms = Some(destination.end_ms);
        landed.buffered_through_ms = destination.end_ms;
        assert!(
            apply_marker_prewarm_control(&rendition, "late-beacon", 2, &landed, &[destination],)
                .await
                .is_none(),
            "landing waits for explicit skip intent"
        );
        let late_hit = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            late.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
                .outcome
                .expect("delayed beacon settles the latched landing")
        };
        assert!(late_hit.hit);

        // A natural pass can leave an exact landing waiting for a beacon.
        // Re-entering the same marker makes that old landing stale; a new
        // beacon, even if Seeking overtakes it on the control channel, must
        // wait for the new seek's own landing.
        {
            let mut manifest = rendition.manifest.lock().await;
            assert!(manifest.evict(target_entry));
        }
        rendition.attach_reader("rewind", frontier).await;
        apply_marker_prewarm_control(&rendition, "rewind", 1, &approach, &[destination]).await;
        let rewind = rendition.readers.lock().await["rewind"]
            .marker_prewarm
            .clone();
        {
            let mut manifest = rendition.manifest.lock().await;
            let candidate = rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .candidate(&manifest)
                .expect("rewind candidate");
            let owners = rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .activate(candidate)
                .into_iter()
                .map(|record_nonce| MarkerPrewarmOwner {
                    ledger: Arc::clone(&rewind),
                    record_nonce,
                })
                .collect();
            let rewind_decision = MarkerPrewarmDecision {
                action: Action::Reposition { to: target_entry },
                candidate: Some(candidate),
                owners,
            };
            update_marker_prewarm_dispatch(
                &rendition,
                producer,
                next_step(producer, rewind_decision.action),
                &rewind_decision,
            );
            assert!(manifest.materialize(target_entry, 4_000, 4));
            credit_marker_prewarm_publication(
                &rendition,
                rendition.gen_epoch.load(Relaxed),
                target_entry,
            )
            .await;
        }
        assert!(
            apply_marker_prewarm_control(&rendition, "rewind", 2, &landed, &[destination])
                .await
                .is_none(),
            "a natural traversal waits for an explicit marker beacon"
        );
        assert!(rewind
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .awaiting_beacon
            .is_some());
        assert!(
            apply_marker_prewarm_control(&rendition, "rewind", 3, &landed, &[destination])
                .await
                .is_none()
        );
        assert!(
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .awaiting_beacon
                .is_none(),
            "a landing-first reorder latch expires at the next accepted control"
        );

        let mut reentered = approach.clone();
        reentered.position_ms = destination.start_ms;
        apply_marker_prewarm_control(&rendition, "rewind", 4, &reentered, &[destination]).await;
        {
            let state = rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                state.awaiting_beacon.is_none(),
                "re-entry invalidates the old traversal's landing"
            );
            assert!(state.armed_skip.is_some());
        }
        let mut held_before_beacon = reentered.clone();
        held_before_beacon.demand = crate::playback_control::PlaybackDemand::Hold;
        held_before_beacon.playback_rate = 0.0;
        apply_marker_prewarm_control(&rendition, "rewind", 5, &held_before_beacon, &[destination])
            .await;
        assert!(
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .armed_skip
                .is_some(),
            "a pause disables speculation without erasing skip correlation"
        );

        let mut seeking_first = held_before_beacon.clone();
        seeking_first.render_state = crate::playback_control::RenderState::Seeking;
        seeking_first.seek_target_ms = Some(destination.end_ms);
        apply_marker_prewarm_control(&rendition, "rewind", 6, &seeking_first, &[destination]).await;
        let reordered_beacon = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(reordered_beacon.matched);
        assert!(
            reordered_beacon.outcome.is_none(),
            "Seeking cannot turn the previous traversal into an immediate hit"
        );
        let reordered_hit =
            apply_marker_prewarm_control(&rendition, "rewind", 7, &landed, &[destination])
                .await
                .expect("the new skip settles only at its new landing");
        assert!(reordered_hit.hit);

        apply_marker_prewarm_control(&rendition, "rewind", 8, &held_before_beacon, &[destination])
            .await;
        let held_replay = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(held_replay.matched);
        assert!(held_replay.outcome.is_none());
        let held_replay_miss =
            apply_marker_prewarm_control(&rendition, "rewind", 9, &landed, &[destination])
                .await
                .expect("a held replay still emits one authoritative result");
        assert!(
            !held_replay_miss.hit,
            "a replay that could not schedule new prewarm is a miss"
        );

        let second = MarkerDestination {
            kind: AnnotationKind::Credits,
            start_ms: 250_000,
            end_ms: 300_000,
            target_entry: entry_containing(&rendition.plan, 300.0),
            window_end_entry: entry_containing(&rendition.plan, 300.0).saturating_add(1),
            eligible: true,
        };
        let mut second_approach = approach.clone();
        second_approach.position_ms = 220_000;
        apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            3,
            &second_approach,
            &[destination, second],
        )
        .await;
        {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let second_beacon = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications);
            assert!(second_beacon.matched, "a later marker is not a duplicate");
            assert!(second_beacon.outcome.is_none());
        }
        let mut second_landing = second_approach.clone();
        second_landing.position_ms = second.end_ms;
        second_landing.buffered_from_ms = Some(second.end_ms);
        second_landing.buffered_through_ms = second.end_ms;
        let second_miss = apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            4,
            &second_landing,
            &[destination, second],
        )
        .await
        .expect("the second marker emits independently");
        assert!(!second_miss.hit);
        assert_eq!(second_miss.requested_sequence, Some(3));

        // An abandoned first seek cannot reserve the one sessionless beacon
        // slot forever. Reaching a different exact marker opportunity cancels
        // that stale pending request and lets the later marker settle.
        apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            5,
            &approach,
            &[destination, second],
        )
        .await;
        {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first_abandoned = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications);
            assert!(first_abandoned.matched);
            assert!(first_abandoned.outcome.is_none());
        }
        apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            6,
            &second_approach,
            &[destination, second],
        )
        .await;
        {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let later_beacon = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications);
            assert!(later_beacon.matched);
            assert!(
                later_beacon.outcome.is_none(),
                "marker B replaces marker A's abandoned pending seek"
            );
        }
        let later_landing = apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            7,
            &second_landing,
            &[destination, second],
        )
        .await
        .expect("marker B settles after marker A was abandoned");
        assert!(!later_landing.hit);
        assert_eq!(later_landing.requested_sequence, Some(6));

        // Publishing the speculative window endpoint exhausts attribution,
        // but the producer-origin fence survives until the old generation is
        // physically retired.
        {
            let mut manifest = rendition.manifest.lock().await;
            if manifest
                .state(destination.window_end_entry)
                .is_some_and(SegState::is_materialized)
            {
                assert!(manifest.evict(destination.window_end_entry));
            }
            assert!(manifest.materialize(destination.window_end_entry, 5_000, 5));
        }
        let old_epoch = rendition.gen_epoch.load(Relaxed);
        credit_marker_prewarm_publication(&rendition, old_epoch, destination.window_end_entry)
            .await;
        assert!(rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());
        assert_eq!(rendition.active_marker_prewarms.load(Acquire), 0);
        assert_eq!(
            rendition.marker_prewarm_generation.load(Acquire),
            old_epoch.saturating_add(1)
        );
        assert!(fence_marker_prewarm_before_room(
            &rendition,
            producer,
            Step::MakeRoom { wanted: 1 },
        ));
        assert_eq!(rendition.gen_epoch.load(Relaxed), old_epoch + 1);
        assert_eq!(rendition.active_marker_prewarms.load(Acquire), 0);
        assert!(rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());

        // Once real playhead demand adopts that same process, it is no longer
        // speculative and a later capacity pass must not kill it.
        let current_epoch = rendition.gen_epoch.load(Relaxed);
        rendition
            .marker_prewarm_generation
            .store(current_epoch.saturating_add(1), Release);
        let stale_owner = Arc::new(StdMutex::new(MarkerPrewarmLedger::default()));
        {
            let mut state = stale_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.records.push(MarkerPrewarmRecord {
                destination,
                requested_sequence: 10,
                nonce: 1,
                schedulable: false,
                active: true,
                settled: false,
                produced: Vec::new(),
            });
        }
        *rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(MarkerPrewarmDispatch {
            producer_epoch: current_epoch,
            candidate: MarkerPrewarmCandidate {
                target_entry,
                window_end_entry: target_entry,
                target_materialized: false,
            },
            owners: vec![MarkerPrewarmOwner {
                ledger: Arc::clone(&stale_owner),
                record_nonce: 1,
            }],
        });
        rendition.active_marker_prewarms.store(1, Release);
        let stopped = Producer::Stopped {
            produced_through: Some(target_entry.saturating_sub(1)),
            positioned_at: target_entry,
            reason: crate::prodsched::Hold::Ahead { horizon: 1 },
        };
        let foreground = MarkerPrewarmDecision {
            action: Action::Produce { next: target_entry },
            candidate: None,
            owners: Vec::new(),
        };
        let resume = next_step(stopped, foreground.action);
        assert_eq!(resume, Step::Resume);
        update_marker_prewarm_dispatch(&rendition, stopped, resume, &foreground);
        assert_eq!(rendition.marker_prewarm_generation.load(Acquire), 0);
        assert_eq!(rendition.active_marker_prewarms.load(Acquire), 0);
        assert!(rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());
        {
            let mut manifest = rendition.manifest.lock().await;
            if manifest
                .state(target_entry)
                .is_some_and(SegState::is_materialized)
            {
                assert!(manifest.evict(target_entry));
            }
            assert!(manifest.materialize(target_entry, 6_000, 6));
        }
        credit_marker_prewarm_publication(&rendition, current_epoch, target_entry).await;
        assert!(
            stale_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records[0]
                .produced
                .is_empty(),
            "foreground publication cannot inherit stale prewarm owners"
        );
        assert!(!fence_marker_prewarm_before_room(
            &rendition,
            Producer::Running {
                produced_through: Some(target_entry),
                positioned_at: target_entry,
            },
            Step::MakeRoom { wanted: 1 },
        ));

        // With no foreground owner and no remaining attribution, the same
        // ahead decision retires speculative ffmpeg instead of parking it.
        rendition
            .marker_prewarm_generation
            .store(current_epoch.saturating_add(1), Release);
        let running = Producer::Running {
            produced_through: Some(target_entry),
            positioned_at: target_entry,
        };
        let ahead = MarkerPrewarmDecision {
            action: Action::Suspend {
                produced_through: target_entry,
                reason: crate::prodsched::Hold::Ahead { horizon: 1 },
            },
            candidate: None,
            owners: Vec::new(),
        };
        assert!(matches!(
            retire_completed_marker_prewarm(
                &rendition,
                running,
                &ahead,
                next_step(running, ahead.action),
            ),
            Step::Terminate {
                why: Termination::Idle
            }
        ));
    }

    #[tokio::test]
    async fn marker_prewarm_placeholder_correlates_without_a_client_session_id() {
        let base = crate::test_tempdir().expect("base");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let serve = VodServe::new(base.path().join("serve"), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(
            &serve,
            "marker-client",
            Arc::clone(&rendition),
            Instant::now(),
        )
        .await;

        let target_entry = entry_containing(&rendition.plan, 200.0);
        let destination = MarkerDestination {
            kind: AnnotationKind::Intro,
            start_ms: 100_000,
            end_ms: 200_000,
            target_entry,
            window_end_entry: target_entry.saturating_add(1),
            eligible: true,
        };
        let mut inside = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        inside.position_ms = 50_000;
        inside.buffered_from_ms = Some(45_000);
        inside.buffered_through_ms = 65_000;
        apply_marker_prewarm_control(&rendition, "marker-client", 1, &inside, &[destination]).await;
        {
            let mut sessions = serve.shared.sessions.lock().await;
            let session = sessions.get_mut("marker-client").expect("session");
            session.marker_destinations = vec![destination];
            session.last_control_snapshot = Some(inside);
        }

        insert_control_session(
            &serve,
            "same-file-unarmed",
            Arc::clone(&rendition),
            Instant::now(),
        )
        .await;
        {
            let mut sessions = serve.shared.sessions.lock().await;
            let ended = sessions
                .get_mut("same-file-unarmed")
                .expect("second session");
            ended.kind = SessionKind::Transcode { height: 720 };
            ended.rendition = None;
            ended.tombstone = Some(Terminal::Deleted);
        }
        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "remux")
                .await,
            "method filtering cannot hide another same-file VOD candidate"
        );
        serve
            .shared
            .sessions
            .lock()
            .await
            .remove("same-file-unarmed");

        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "transcode",)
                .await,
            "a transcode beacon cannot consume a copy/remux ledger"
        );
        {
            let mut sessions = serve.shared.sessions.lock().await;
            sessions.get_mut("marker-client").expect("session").kind =
                SessionKind::Transcode { height: 720 };
        }
        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "remux")
                .await,
            "a remux beacon cannot consume a transcode ledger"
        );
        {
            let mut sessions = serve.shared.sessions.lock().await;
            sessions.get_mut("marker-client").expect("session").kind = SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            };
        }

        assert!(
            serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "remux")
                .await,
            "an early auto-skip beacon binds the unique approach request without a session id"
        );
        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "direct_play")
                .await,
            "direct play never consumes its client-owned miss"
        );
    }

    async fn create(
        serve: &Arc<VodServe>,
        file: &MediaFile,
        session_id: &str,
        playback_id: &str,
        settings: &VodSettings,
    ) -> VodStart {
        serve
            .try_create(
                &request(playback_id, 0.0),
                file,
                settings,
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                session_id.to_string(),
            )
            .await
            .expect("the fixture is VOD-presentable")
    }

    async fn fetch(serve: &Arc<VodServe>, session: &str, name: &str) -> SegmentReady {
        // A blocking GET may answer a typed Pending under a slow generation;
        // that is the protocol working, so retry the way a client would.
        for _ in 0..20 {
            match serve.segment(session, name).await {
                Some(VodPublication {
                    result: Ok(Some(ready)),
                    owner,
                }) => {
                    let index = planned_index(name);
                    assert!(serve.commit_resolved_media(session, &owner, index).await);
                    return ready;
                }
                Some(VodPublication {
                    result: Err(VodError::Pending { .. }),
                    ..
                }) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                other => panic!("fetching {name}: unexpected answer {:?}", describe(other)),
            }
        }
        panic!("fetching {name} never materialized");
    }

    fn describe(answer: Option<VodPublication<Option<SegmentReady>>>) -> String {
        match answer {
            None => "None".into(),
            Some(VodPublication {
                result: Ok(None), ..
            }) => "Ok(None)".into(),
            Some(VodPublication {
                result: Ok(Some(ready)),
                ..
            }) => format!("Ok(Some(len {}))", ready.len),
            Some(VodPublication {
                result: Err(error), ..
            }) => format!("Err({error:?})"),
        }
    }

    async fn read_ready(ready: SegmentReady) -> Vec<u8> {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let mut file = ready.file;
        file.read_to_end(&mut bytes).await.expect("read segment");
        bytes
    }

    async fn plan_len(serve: &Arc<VodServe>, session: &str) -> usize {
        let sessions = serve.shared.sessions.lock().await;
        sessions
            .get(session)
            .expect("session")
            .live_rendition()
            .expect("live rendition")
            .plan
            .len()
    }

    async fn rendition_of(serve: &Arc<VodServe>, session: &str) -> Arc<Rendition> {
        let sessions = serve.shared.sessions.lock().await;
        Arc::clone(
            sessions
                .get(session)
                .expect("session")
                .live_rendition()
                .expect("live rendition"),
        )
    }

    async fn wait_until(what: &str, deadline: Duration, mut check: impl AsyncFnMut() -> bool) {
        let started = Instant::now();
        while started.elapsed() < deadline {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{what} never happened within {deadline:?}");
    }

    #[tokio::test]
    async fn terminal_cleanup_completion_after_wait_registration_is_not_lost() {
        let cleanup = Arc::new(TerminalCleanup::new());
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *cleanup
            .wait_enabled_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let waiter = tokio::spawn({
            let cleanup = Arc::clone(&cleanup);
            async move { cleanup.wait().await }
        });

        // `wait` has enabled its Notified future but has not performed the
        // state re-check. `notify_waiters` in this exact gap used to vanish.
        pause.wait().await;
        cleanup.complete();
        pause.wait().await;
        tokio::time::timeout(Duration::from_millis(250), waiter)
            .await
            .expect("registered terminal waiter must observe completion")
            .expect("terminal waiter task");
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_retention_starts_when_cleanup_completes() {
        let cleanup = TerminalCleanup::new();
        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION + Duration::from_secs(1)).await;
        assert!(
            !cleanup.retention_expired(),
            "creation time must not consume the post-cleanup replay window"
        );

        cleanup.complete();
        assert!(!cleanup.retention_expired());
        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION - Duration::from_millis(1)).await;
        assert!(!cleanup.retention_expired());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(cleanup.retention_expired());
    }

    /// M6's preparation slot exists on the engine that serves the sessions.
    ///
    /// This is the whole point of moving it off the rolling actor. `create`
    /// sets `Presentation::Vod` for every session, so a slot only the actor
    /// held would have staged successors on a path viewers do not take — and
    /// M6 §3.4's acceptance would have passed while the feature fired on
    /// nothing, which is the defect class the replacement seam already had
    /// once.
    #[tokio::test]
    async fn a_vod_session_holds_a_preparation_slot() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;

        let gate = serve
            .preparation_gate(&session_id)
            .await
            .expect("a live VOD session has a gate");
        let successor = uuid::Uuid::new_v4().to_string();
        let predecessor = uuid::Uuid::new_v4().to_string();
        assert!(
            gate.stage_preparation(successor.clone(), predecessor.clone(), i64::MAX)
                .await
        );
        assert!(
            !gate.may_commit_preparation(&successor).await,
            "staging alone is not commit authority; a client acknowledgement must reserve it"
        );
        // One per playback, and the second ask is refused rather than
        // replacing the first: the store's primary key would reject it, and an
        // engine that believed in two could commit the wrong one.
        assert!(
            !gate
                .stage_preparation(uuid::Uuid::new_v4().to_string(), predecessor, i64::MAX)
                .await
        );
        assert!(gate.settle_preparation(&successor, true).await);
        assert!(
            !gate.may_commit_preparation(&successor).await,
            "a settled successor is no longer committable",
        );
    }

    /// An abandoned successor frees the slot, on this engine too.
    ///
    /// `settle_preparation(_, false)` aborts before it clears, and it is the
    /// only VOD-specific logic in the whole implementation — the rolling side
    /// pins the same pair, and the two must not drift.
    #[tokio::test]
    async fn an_abandoned_successor_frees_the_vod_slot() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let gate = serve.preparation_gate(&session_id).await.expect("gate");

        let abandoned = uuid::Uuid::new_v4().to_string();
        assert!(
            gate.stage_preparation(
                abandoned.clone(),
                uuid::Uuid::new_v4().to_string(),
                i64::MAX,
            )
            .await
        );
        assert!(gate.settle_preparation(&abandoned, false).await);
        assert!(
            !gate.may_commit_preparation(&abandoned).await,
            "an aborted successor is not committable",
        );
        // And the slot is free for the next one, which is the property that
        // distinguishes an abandon from a leak.
        assert!(
            gate.stage_preparation(
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                i64::MAX,
            )
            .await
        );
    }

    /// A gate outlives its attachment, and a session id does not identify one.
    ///
    /// An idle reap removes a VOD session deliberately *without* a tombstone,
    /// and a reconnect resurrects the same durable id sharing the same
    /// lifecycle gate — that gate is stable across exactly this, on purpose.
    /// So a gate that trusted the id would take the replacement's slot for a
    /// successor whose predecessor no longer holds the pointer, and the
    /// store's CAS would be the only thing left. The incarnation pin is what
    /// stops that.
    #[tokio::test]
    async fn a_gate_does_not_follow_its_session_id_to_a_new_incarnation() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let stale = serve.preparation_gate(&session_id).await.expect("gate");

        // The reap, then the resurrection under the same id.
        serve.shared.sessions.lock().await.remove(&session_id);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;

        assert!(
            !stale
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                )
                .await,
            "the stale gate must not take the replacement's slot",
        );
        // And the new attachment's own gate is unobstructed, which is what
        // that refusal is protecting.
        let fresh = serve.preparation_gate(&session_id).await.expect("gate");
        assert!(
            fresh
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                )
                .await
        );
    }

    /// Ending a session ends its staged successor, under the same lock.
    ///
    /// This is what lets `may_commit_preparation` trust the slot rather than
    /// layering a second refusal on it: the rolling actor's `terminate` aborts
    /// the slot it holds, and every VOD path that writes a tombstone now does
    /// the same. Without it the gate would say no while `ControlState` still
    /// said the successor was committable — two authorities over one slot.
    #[tokio::test]
    async fn ending_a_vod_session_ends_its_staged_successor() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let gate = serve.preparation_gate(&session_id).await.expect("gate");
        let staged = uuid::Uuid::new_v4().to_string();
        assert!(
            gate.stage_preparation(staged.clone(), uuid::Uuid::new_v4().to_string(), i64::MAX,)
                .await
        );

        serve.begin_end(&session_id, Terminal::Deleted).await;

        assert!(
            !gate.may_commit_preparation(&staged).await,
            "the slot itself refuses, not a check layered over it",
        );
        let sessions = serve.shared.sessions.lock().await;
        let session = sessions.get(&session_id).expect("session");
        assert!(
            !session
                .control
                .lock()
                .expect("control lock")
                .may_commit_preparation(&staged),
            "and the state agrees with the gate rather than contradicting it",
        );
    }

    /// Liveness is the engine's half, and it answers before the slot is
    /// touched: a session that has ended takes no successor, and one that is
    /// gone from the registry has no gate at all.
    #[tokio::test]
    async fn an_ended_vod_session_takes_no_successor() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let gate = serve.preparation_gate(&session_id).await.expect("gate");

        serve
            .shared
            .sessions
            .lock()
            .await
            .get_mut(&session_id)
            .expect("session")
            .tombstone = Some(Terminal::Deleted);
        assert!(
            !gate
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                )
                .await,
            "a tombstoned session takes no successor",
        );
        assert!(
            serve.preparation_gate(&session_id).await.is_none(),
            "and hands out no further gate",
        );

        serve.shared.sessions.lock().await.remove(&session_id);
        assert!(
            !gate
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                )
                .await,
            "nor does a gate outliving its session",
        );
        assert!(serve.preparation_gate(&session_id).await.is_none());
    }

    #[tokio::test]
    async fn terminal_cleanup_compacts_the_registry_before_publishing_completion() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        let weak = Arc::downgrade(&rendition);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        drop(rendition);

        assert!(serve.end(&session_id, Terminal::Deleted).await);
        let sessions = serve.shared.sessions.lock().await;
        let terminal = sessions.get(&session_id).expect("compact terminal owner");
        assert!(terminal
            .terminal_cleanup
            .as_ref()
            .is_some_and(|cleanup| cleanup.is_finished()));
        assert!(
            terminal.rendition.is_none(),
            "cleanup completion is not visible before the strong graph is released"
        );
        drop(sessions);
        assert!(
            weak.upgrade().is_none(),
            "the compact 410 owner retains no rendition/media/process graph"
        );
        assert!(matches!(
            serve
                .playlist(&session_id)
                .await
                .expect("compact terminal remains addressable")
                .result,
            Err(VodError::Gone(Terminal::Deleted))
        ));
    }

    #[tokio::test]
    async fn one_rendition_build_gate_does_not_block_other_keys_or_purge_maintenance() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        let key = rendition.key.clone();
        *rendition.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(key.clone(), Arc::clone(&rendition));

        let gate = serve.shared.rendition_build_gate(&key);
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let paused_build = tokio::spawn(async move {
            let _guard = gate.lock_owned().await;
            let _ = held_tx.send(());
            std::future::pending::<()>().await;
        });
        held_rx.await.expect("key A gate held");

        let other = serve.shared.rendition_build_gate("unrelated-key");
        let other_guard = tokio::time::timeout(Duration::from_millis(250), other.lock_owned())
            .await
            .expect("key B must not queue behind key A");
        drop(other_guard);
        let registry =
            tokio::time::timeout(Duration::from_millis(250), serve.shared.renditions.lock())
                .await
                .expect("a paused key build must not own the node-wide registry");
        drop(registry);
        tokio::time::timeout(
            Duration::from_millis(250),
            serve.shared.purge_if_dormant(&key, Duration::ZERO),
        )
        .await
        .expect("maintenance must skip an in-flight key rather than wait");
        assert!(serve.shared.renditions.lock().await.contains_key(&key));

        paused_build.abort();
        assert!(paused_build
            .await
            .expect_err("paused build is cancelled")
            .is_cancelled());
        let released = serve.shared.rendition_build_gate(&key);
        let released_guard =
            tokio::time::timeout(Duration::from_millis(250), released.lock_owned())
                .await
                .expect("cancellation releases exactly key A");
        drop(released_guard);
    }

    #[tokio::test]
    async fn cancelled_real_attach_is_accounted_reusable_and_purgeable() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let source_path = base.path().join("source.mkv");
        tokio::fs::write(&source_path, b"x")
            .await
            .expect("write source fence fixture");

        let index = synthetic_index(240);
        let duration_ms = index_video_ms(&index);
        let identity = SourceIdentity::new(1, 1, "fingerprint");
        let recipe = Recipe {
            file: media_file_at(source_path, duration_ms),
            audio_index: None,
            aac: true,
            video: CopyVideoOptions::new(false, false),
            source_object_version: None,
            cluster_cache_key: None,
        };
        let key = rendition_key(&recipe, &identity);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &shipped_policy(index.timescale),
            &track_durations(&index, &recipe, duration_ms),
        );

        // Plant one verifiable member so the real adoption path publishes a
        // non-zero working-set claim before the cancellation point.
        let dir = RenditionDir::new(base.path().join(&key));
        dir.create().await.expect("create adopted rendition");
        let mut planted = Manifest::new(plan);
        dir.materialize(&mut planted, 0, b"adopted-segment", now_ms())
            .await
            .expect("plant adopted segment");
        let adopted_bytes = planted.materialized_bytes();
        dir.write_init(b"fixture-init")
            .await
            .expect("plant adopted init");
        store_identity(
            &dir.path().join(IDENTITY_NAME),
            &InitIdentity {
                muxer_init: "fixture-muxer".to_owned(),
                served_init: "fixture-served".to_owned(),
                promotion: plurx_core::fmp4::PromotionInputs::default(),
            },
        )
        .await
        .expect("plant adopted identity");

        let install_pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .rendition_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&install_pause));
        let pending = {
            let shared = Arc::clone(&serve.shared);
            let key = key.clone();
            let identity = identity.clone();
            let index = index.clone();
            let recipe = recipe.clone();
            let settings = settings();
            tokio::spawn(async move {
                shared
                    .attach_rendition(&key, &identity, index, recipe, duration_ms, &settings)
                    .await
            })
        };
        install_pause.wait().await;

        let installed = serve
            .shared
            .renditions
            .lock()
            .await
            .get(&key)
            .map(Arc::clone)
            .expect("real attach installed the rendition");
        assert_eq!(serve.shared.working_set.load(Relaxed), adopted_bytes);
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(
            serve.shared.renditions.lock().await.contains_key(&key),
            "purge must skip the exact key while attach still owns its gate"
        );

        pending.abort();
        assert!(
            matches!(
                pending.await,
                Err(error) if error.is_cancelled()
            ),
            "attach is cancelled after publication"
        );
        install_pause.wait().await;
        *serve
            .shared
            .rendition_install_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

        let reused = serve
            .shared
            .attach_rendition(&key, &identity, index, recipe, duration_ms, &settings())
            .await
            .expect("same-key retry")
            .expect("non-empty rendition");
        assert!(Arc::ptr_eq(&installed, &reused.rendition));
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            adopted_bytes,
            "same-key reuse must not double-count adopted bytes"
        );
        drop(reused);

        tokio::time::sleep(Duration::from_millis(1)).await;
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(!serve.shared.renditions.lock().await.contains_key(&key));
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            0,
            "purge releases the exact installed rendition's byte claim"
        );
    }

    #[tokio::test]
    async fn cancelled_dormant_purge_keeps_key_and_accounting_owned_until_settlement() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"owned-dormant-bytes", now_ms())
                .await
                .expect("materialize dormant member");
        }
        let claimed = rendition.manifest.lock().await.materialized_bytes();
        serve.shared.working_set.fetch_add(claimed, Relaxed);
        tokio::fs::write(rendition.identity_path(), b"identity")
            .await
            .expect("write dormant identity");
        *rendition.dormant_since.lock().expect("dormant lock") =
            Some(Instant::now() - Duration::from_secs(1));
        let key = rendition.key.clone();
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(key.clone(), Arc::clone(&rendition));

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .dormant_purge_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let caller = tokio::spawn({
            let shared = Arc::clone(&serve.shared);
            let key = key.clone();
            async move { shared.purge_if_dormant(&key, Duration::ZERO).await }
        });
        pause.wait().await;
        assert!(!serve.shared.renditions.lock().await.contains_key(&key));
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            claimed,
            "accounting remains owned while detached cleanup is paused"
        );
        caller.abort();
        assert!(caller
            .await
            .expect_err("maintenance caller cancelled")
            .is_cancelled());

        let retry_gate = serve.shared.rendition_build_gate(&key);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), retry_gate.lock_owned())
                .await
                .is_err(),
            "same-key rebuild cannot overlap removed rendition settlement"
        );
        pause.wait().await;
        wait_until(
            "detached dormant settlement",
            Duration::from_secs(2),
            || {
                let serve = Arc::clone(&serve);
                let identity = rendition.identity_path();
                async move {
                    serve.shared.working_set.load(Relaxed) == 0
                        && tokio::fs::metadata(identity).await.is_err()
                }
            },
        )
        .await;
        let retry_gate = serve.shared.rendition_build_gate(&key);
        let retry = tokio::time::timeout(Duration::from_secs(2), retry_gate.lock_owned())
            .await
            .expect("same-key rebuild authority releases after exact settlement");
        drop(retry);
        *serve
            .shared
            .dormant_purge_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_head_regeneration_owner_kills_and_confirms_child_reap() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let regeneration = tokio::spawn(async move {
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .expect("deterministic head-regeneration child");
            let pid = child.id().expect("child pid");
            let owner = HeadChildOwner::new(child);
            let _ = started_tx.send(pid);
            std::future::pending::<()>().await;
            drop(owner);
        });
        let pid = started_rx.await.expect("child started");
        regeneration.abort();
        assert!(regeneration
            .await
            .expect_err("head regeneration is cancelled")
            .is_cancelled());

        wait_until(
            "cancelled head child confirmed reaped",
            Duration::from_secs(2),
            || async {
                let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
                result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            },
        )
        .await;
    }

    #[tokio::test]
    async fn head_regeneration_admission_and_pre_init_buffer_are_hard_bounded() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let slots = Arc::clone(&serve.shared.head_regeneration_slots);
        let all = Arc::clone(&slots)
            .acquire_many_owned(HEAD_REGENERATION_CAPACITY as u32)
            .await
            .expect("reserve every head slot");
        assert!(
            Arc::clone(&slots).try_acquire_owned().is_err(),
            "head-regeneration exhaustion is an immediate typed refusal point"
        );
        drop(all);
        let key_a = Arc::clone(&slots)
            .try_acquire_owned()
            .expect("key A head slot");
        let key_b = Arc::clone(&slots)
            .try_acquire_owned()
            .expect("key B is not serialized behind key A");
        drop((key_a, key_b));

        use tokio::io::AsyncWriteExt as _;
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let write = tokio::spawn(async move {
            let mut oversized = vec![0_u8; 4096];
            oversized[..4].copy_from_slice(&8192_u32.to_be_bytes());
            oversized[4..8].copy_from_slice(b"free");
            writer
                .write_all(&oversized)
                .await
                .expect("write malformed oversized head");
        });
        let error = read_muxer_init_bounded(&mut reader, 4096)
            .await
            .expect_err("an init absent at the exact byte ceiling is refused");
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        write.await.expect("bounded head writer");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_build_waiter_cannot_release_same_key_before_confirmed_head_reap() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let key = "cancelled-head-key";
        let gate = serve.shared.rendition_build_gate(key);
        let reap_pause = Arc::new(tokio::sync::Barrier::new(2));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let result = spawn_cancellation_independent({
            let reap_pause = Arc::clone(&reap_pause);
            async move {
                let _build_guard = gate.lock_owned().await;
                let child = tokio::process::Command::new("sleep")
                    .arg("60")
                    .kill_on_drop(true)
                    .spawn()
                    .expect("deterministic head child");
                let pid = child.id().expect("head child pid");
                let mut owner = HeadChildOwner::with_reap_pause(child, reap_pause);
                let _ = started_tx.send(pid);
                owner.terminate_and_reap().await;
            }
        });
        let pid = started_rx.await.expect("head child started");
        drop(result); // the request waiting for the build result is cancelled
        reap_pause.wait().await;

        let retry_gate = serve.shared.rendition_build_gate(key);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), retry_gate.lock_owned())
                .await
                .is_err(),
            "same-key retry cannot acquire spawn authority while reap is paused"
        );
        reap_pause.wait().await;
        let retry_gate = serve.shared.rendition_build_gate(key);
        let retry = tokio::time::timeout(Duration::from_secs(2), retry_gate.lock_owned())
            .await
            .expect("same-key authority releases after confirmed reap");
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        drop(retry);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn production_head_deadline_reaps_child_and_releases_build_gate() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let key = "head-timeout";
        let gate = serve.shared.rendition_build_gate(key);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let timed = tokio::spawn(async move {
            let _build_guard = gate.lock_owned().await;
            let mut child = tokio::process::Command::new("sleep")
                .arg("60")
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("deterministic head-regeneration child");
            let stdout = child.stdout.take().expect("child stdout");
            let pid = child.id().expect("child pid");
            let _ = started_tx.send(pid);
            read_regenerated_head_before(child, stdout, Duration::from_millis(25), 1024).await
        });
        let pid = started_rx
            .await
            .expect("head child started under build gate");
        let error = tokio::time::timeout(Duration::from_secs(2), timed)
            .await
            .expect("production head deadline must settle")
            .expect("head deadline task")
            .expect_err("a silent child cannot produce an init");
        assert!(
            error.to_string().contains("exceeded its 0.0s deadline"),
            "{error}"
        );
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "deadline return requires confirmed child reap"
        );

        let released = serve.shared.rendition_build_gate(key);
        let released_guard =
            tokio::time::timeout(Duration::from_millis(250), released.lock_owned())
                .await
                .expect("head timeout releases the exact build gate");
        drop(released_guard);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_tombstone_replays_410_for_the_retention_window_then_releases() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());
        let rendition = synthetic_rendition(base.path()).await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_ended_route(store.as_ref(), &session_id, &generation).await;
        insert_finished_terminal_session(&serve, &session_id, rendition).await;

        let publication = serve
            .playlist(&session_id)
            .await
            .expect("terminal owner retained");
        assert!(matches!(
            publication.result,
            Err(VodError::Gone(Terminal::Deleted))
        ));
        assert!(
            serve
                .response_status_owner_is_current(&session_id, &publication.owner)
                .await
        );

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION - Duration::from_millis(1)).await;
        serve.maintain().await;
        assert!(matches!(
            serve
                .playlist(&session_id)
                .await
                .expect("410 owner retained through the documented window")
                .result,
            Err(VodError::Gone(Terminal::Deleted))
        ));

        tokio::time::advance(Duration::from_millis(1)).await;
        serve.maintain().await;
        assert!(serve.playlist(&session_id).await.is_none());
        assert!(
            !serve
                .response_status_owner_is_current(&session_id, &publication.owner)
                .await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_graphs_compact_immediately_and_route_eviction_is_fair_and_fail_closed() {
        const ENDED_COUNT: usize = TERMINAL_ROUTE_CONFIRM_BATCH * 2 + 3;
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());
        let rendition = synthetic_rendition(base.path()).await;
        let mut ended_ids = Vec::new();
        for _ in 0..ENDED_COUNT {
            let session_id = uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            activate_ended_route(store.as_ref(), &session_id, &generation).await;
            insert_finished_terminal_session(&serve, &session_id, Arc::clone(&rendition)).await;
            ended_ids.push(session_id);
        }

        let active_id = uuid::Uuid::new_v4().to_string();
        activate_control_route(
            store.as_ref(),
            &active_id,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await;
        insert_finished_terminal_session(&serve, &active_id, Arc::clone(&rendition)).await;

        let timeout_id = uuid::Uuid::new_v4().to_string();
        activate_ended_route(
            store.as_ref(),
            &timeout_id,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await;
        let error_id = uuid::Uuid::new_v4().to_string();
        activate_ended_route(store.as_ref(), &error_id, &uuid::Uuid::new_v4().to_string()).await;
        insert_finished_terminal_session(&serve, &timeout_id, Arc::clone(&rendition)).await;
        insert_finished_terminal_session(&serve, &error_id, Arc::clone(&rendition)).await;
        {
            let mut outcomes = serve
                .shared
                .terminal_route_test_outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for session_id in &ended_ids {
                outcomes.insert(session_id.clone(), TerminalRouteTestOutcome::Success);
            }
            outcomes.insert(timeout_id.clone(), TerminalRouteTestOutcome::Timeout);
            outcomes.insert(error_id.clone(), TerminalRouteTestOutcome::Error);
        }

        let total = ENDED_COUNT + 3;
        let sessions = serve.shared.sessions.lock().await;
        assert_eq!(sessions.len(), total);
        assert_eq!(
            sessions
                .values()
                .filter(|session| session.rendition.is_some())
                .count(),
            0,
            "completed terminal cleanup retains no heavyweight graph even before maintenance"
        );
        drop(sessions);

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION).await;
        for _ in 0..8 {
            serve.maintain().await;
        }
        let sessions = serve.shared.sessions.lock().await;
        assert!(ended_ids.iter().all(|id| !sessions.contains_key(id)));
        assert!(
            sessions.contains_key(&active_id),
            "an active durable route is retained"
        );
        assert!(
            sessions.contains_key(&timeout_id),
            "Store timeout fails closed"
        );
        assert!(sessions.contains_key(&error_id), "Store error fails closed");
        assert_eq!(
            sessions.len(),
            3,
            "bounded rotating batches eventually reach every successful candidate"
        );
    }

    #[tokio::test]
    async fn the_playlist_bytes_are_identical_across_the_sessions_whole_life() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let first = serve
            .playlist("sess-a")
            .await
            .expect("a vod session")
            .expect("playlist bytes")
            .0;
        let text = String::from_utf8(first.clone()).expect("utf8 playlist");
        assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"), "{text}");
        assert!(text.ends_with("#EXT-X-ENDLIST\n"), "{text}");
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:0\n"), "{text}");

        // A blocking fetch in the middle of its life...
        fetch(&serve, "sess-a", "seg00000.m4s").await;

        // ...and the playlist has not moved a byte.
        let second = serve
            .playlist("sess-a")
            .await
            .expect("a vod session")
            .expect("playlist bytes")
            .0;
        assert_eq!(first, second, "the playlist is immutable (plan §2.1)");
    }

    /// A capacity stall says why, after the producer it terminated is gone.
    ///
    /// A hold with no scheduled end terminates its producer — a stopped one
    /// goes on holding everything a running one held — which leaves the belief
    /// `Absent` and takes the reason with it. The status then said `waiting`
    /// with no hold at all, so a viewer's control plane saw a producer stop
    /// and never saw that the working set was full. `hold_reason` is the only
    /// fact that distinguishes "nothing is arriving because there is no room"
    /// from "nothing is arriving".
    #[tokio::test]
    async fn a_capacity_stall_still_says_no_room_after_its_producer_is_gone() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");

        assert_eq!(
            serve.status("sess-a").await.expect("status").producer_hold,
            None,
            "a healthy rendition is not holding"
        );

        *rendition.capacity_hold.lock().expect("capacity hold") =
            Some(crate::prodsched::Hold::NoRoom { wanted: 4_096 });
        let held = serve.status("sess-a").await.expect("status");
        assert_eq!(
            held.producer_hold,
            Some("no_room"),
            "the reason outlives the producer it terminated"
        );

        // And it is cleared by the first pass that decides anything else,
        // rather than being latched for the life of the rendition.
        *rendition.capacity_hold.lock().expect("capacity hold") = None;
        assert_eq!(
            serve.status("sess-a").await.expect("status").producer_hold,
            None
        );
    }

    /// Reattachment removes a reader without going through `detach_reader`,
    /// so it needs the same wait retirement.
    ///
    /// A viewer who creates again lands on a different rendition; anything
    /// still parked on the one they left has no reader to be ranked against,
    /// and `playback_demands` marks a session's oldest wait foreground on
    /// exactly that absence — keeping the abandoned rendition producing for
    /// somebody who is no longer watching it.
    #[tokio::test]
    async fn reattaching_elsewhere_retires_the_waits_left_behind() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let first = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("the first rendition");

        let _parked = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: first.key.clone(),
                    index: 40,
                },
                "sess-a",
            )
            .expect("a parked request on the rendition being left");
        assert_eq!(serve.shared.pool.demands(&first.key).len(), 1);

        // Same session, a different recipe: a different rendition key, and the
        // reader moves to it.
        let mut moved = request("play-a", 0.0);
        moved.kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        serve
            .try_create(
                &moved,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-a".to_string(),
            )
            .await
            .expect("the replacement is VOD-presentable");
        let second = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("the replacement rendition");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the fixture must actually move the viewer to another rendition"
        );

        assert!(
            serve.shared.pool.demands(&first.key).is_empty(),
            "the request left behind goes with the reader that made it"
        );
    }

    /// Reattachment also stops the subtitle window the viewer left behind, and
    /// does it without fencing the id they are still watching under.
    ///
    /// A window is keyed by session id alone, so a viewer moving to another
    /// rendition leaves a live ffmpeg extracting a span for the recipe they
    /// moved off. `detach_reader` releases one on every real ending and its
    /// comment claimed reattachment converged there too; it did not, and it
    /// must not — releasing fences the id for half a minute, and this id
    /// belongs to somebody who is still watching, so the fence would refuse
    /// the first window of the attachment that just replaced it. The flight is
    /// stopped by name instead, which is what this pins from the serving side:
    /// gone, and the session still able to start the next one.
    #[tokio::test]
    async fn reattaching_elsewhere_stops_the_window_flight_it_left_behind() {
        // The window registry is process-global and keyed by session id
        // alone, so this fixture cannot use the shared `sess-a` literal: a
        // concurrent test releasing that id would fence or clear this one's
        // flight and the failure would read as a defect in the code.
        let session_id = &uuid::Uuid::new_v4().to_string();
        let base = crate::test_tempdir().expect("base");
        let (serve, mut file) = serve_on(base.path()).await;
        let subs = crate::test_tempdir().expect("subs");
        // Long enough that a window is a fraction of the runtime, which is the
        // only condition windowing asks about. The extraction itself is
        // injected, so nothing here reads the media.
        file.duration_ms = Some(4 * 60 * 60 * 1_000);
        create(&serve, &file, session_id, "play-a", &settings()).await;
        let first = rendition_of(&serve, session_id).await;

        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let hold = Arc::new(tokio::sync::Semaphore::new(0));
        let warm = |anchor: i64| {
            let started = Arc::clone(&started);
            let hold = Arc::clone(&hold);
            let subs_dir = subs.path().to_owned();
            let file = file.clone();
            async move {
                crate::subtitles::warm_vtt_window_with(
                    session_id,
                    Some(1),
                    &subs_dir,
                    &file,
                    0,
                    anchor,
                    30,
                    move |_tmp, _, _, _, _| async move {
                        started.add_permits(1);
                        hold.acquire().await.expect("hold").forget();
                        Ok(())
                    },
                )
                .await
            }
        };
        let running = || {
            let started = Arc::clone(&started);
            async move {
                tokio::time::timeout(Duration::from_secs(5), started.acquire())
                    .await
                    .expect("the injected extractor starts")
                    .expect("started semaphore remains open")
                    .forget();
            }
        };

        assert!(warm(600).await, "the viewer owns a window on the way in");
        running().await;
        assert!(crate::subtitles::owned_window_for_test(session_id).is_some());

        // Same session, a different recipe: a different rendition key, and the
        // reader moves to it.
        let mut moved = request("play-a", 0.0);
        moved.kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        serve
            .try_create(
                &moved,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                session_id.to_string(),
            )
            .await
            .expect("the replacement is VOD-presentable");
        assert!(
            !Arc::ptr_eq(&first, &rendition_of(&serve, session_id).await),
            "the fixture must actually move the viewer to another rendition"
        );

        assert!(
            crate::subtitles::owned_window_for_test(session_id).is_none(),
            "the flight for the recipe they left does not outlive the move"
        );
        assert!(
            warm(900).await,
            "and the viewer who is still watching is not fenced out of the next one"
        );
        running().await;
        assert_eq!(
            crate::subtitles::peak_window_flights_for_test(session_id),
            1,
            "one live flight per playback, across the move"
        );

        hold.add_permits(8);
        crate::subtitles::release_session_window(session_id).await;
    }

    /// The configured node cap reaches the pool, on create.
    ///
    /// Settings are re-read per create, so that is where the ceiling is
    /// applied — which is what lets an operator change it without a restart.
    /// A cap that lived only at construction would be frozen at whatever the
    /// node booted with, and the plan promised a setting.
    #[tokio::test]
    async fn the_configured_blocked_get_cap_reaches_the_pool() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        assert_eq!(
            serve.shared.pool.global_cap(),
            DEFAULT_GLOBAL_WAIT_CAP,
            "an unconfigured node still bounds its parked work"
        );

        let mut tuned = settings();
        tuned.blocked_get_cap = 3;
        create(&serve, &file, "sess-a", "play-a", &tuned).await;
        assert_eq!(
            serve.shared.pool.global_cap(),
            3,
            "the operator's number, applied where settings are read"
        );

        let mut raised = settings();
        raised.blocked_get_cap = 128;
        create(&serve, &file, "sess-b", "play-b", &raised).await;
        assert_eq!(
            serve.shared.pool.global_cap(),
            128,
            "and it tracks a later change rather than latching the first"
        );
    }

    /// A detached viewer stops being demand at the moment their reader goes,
    /// not when their abandoned request finally times out.
    ///
    /// `playback_demands` ranks a blocked request against the reader that
    /// asked for it. With the reader gone it falls back to marking that
    /// session's oldest wait foreground — so until the request's HTTP deadline
    /// expired, a viewer who had already left outranked one who was still
    /// watching and pointed the producer at media nobody wanted.
    #[tokio::test]
    async fn a_detached_viewer_stops_being_demand_with_its_reader() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");

        let _leaving = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 40,
                },
                "sess-a",
            )
            .expect("a parked request for the leaving viewer");
        let _staying = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 2,
                },
                "sess-b",
            )
            .expect("a parked request for the viewer who stays");
        assert_eq!(serve.shared.pool.demands(&rendition.key).len(), 2);

        rendition.detach_reader(&serve.shared.pool, "sess-a").await;

        let remaining = serve.shared.pool.demands(&rendition.key);
        assert_eq!(
            remaining.len(),
            1,
            "the departing viewer's parked request left with its reader"
        );
        assert_eq!(
            remaining[0].session, "sess-b",
            "and the viewer still watching keeps theirs"
        );
    }

    /// P1-2. A recorded failure carries its class to the session status.
    ///
    /// The prose was always there; the class is what a client can act on.
    /// Without it `DeliveryView::from_status` had nothing to publish, so a
    /// rendition that had genuinely failed still produced `action: none`.
    #[tokio::test]
    async fn a_recorded_failure_publishes_its_class_not_only_its_sentence() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let healthy = serve.status("sess-a").await.expect("status");
        assert_eq!(healthy.producer_decision, None, "nothing has failed");
        assert_eq!(healthy.producer_failed, None);

        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");
        record_failure(
            &serve.shared,
            &rendition,
            Reason::SourceChanged,
            "source changed after the fragment index was selected".to_owned(),
        );

        let failed = serve.status("sess-a").await.expect("status");
        assert_eq!(failed.producer_state, "failed");
        assert_eq!(
            failed.producer_decision,
            Some(Reason::SourceChanged.status()),
            "the class travels with the failure"
        );
        assert_eq!(
            failed.producer_failed.as_deref(),
            Some("source changed after the fragment index was selected"),
            "the operator's sentence is not replaced by the class"
        );
    }

    /// The first failure is the cause; a later one is its consequence.
    ///
    /// A publication fence records `source_changed` and then stops the
    /// generation, which ends by reporting that its sink refused the bytes.
    /// Overwriting would file the consequence as the diagnosis and publish a
    /// class naming the wrong subsystem — `producer_write_failed` for a source
    /// that moved. The sentence must not drift from the class either.
    #[tokio::test]
    async fn a_recorded_cause_is_not_overwritten_by_its_own_consequence() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = serve
            .shared
            .sessions
            .lock()
            .await
            .get("sess-a")
            .and_then(|session| session.live_rendition().map(Arc::clone))
            .expect("a live rendition");

        record_failure(
            &serve.shared,
            &rendition,
            Reason::SourceChanged,
            "source changed before fragment publication".to_owned(),
        );
        record_failure(
            &serve.shared,
            &rendition,
            Reason::ProducerWriteFailed,
            "sink refused: source changed before fragment publication".to_owned(),
        );

        let failure = rendition.failure().expect("a recorded failure");
        assert_eq!(
            failure.decision,
            Reason::SourceChanged,
            "the fence that stopped the generation is the diagnosis"
        );
        assert_eq!(
            failure.cause, "source changed before fragment publication",
            "the sentence stays with the class it was recorded beside"
        );
    }

    /// A source that moves under a rendition is published as a source change,
    /// end to end, not as whatever the generation happened to say on its way
    /// down.
    ///
    /// This is one of the classification choices that had no coverage: the
    /// class chosen at a `record_failure` site is invisible to a test that
    /// supplies its own.
    #[tokio::test]
    async fn a_source_that_moves_is_published_as_a_source_change() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        // Move the source under the fence the rendition took at create.
        let mut bytes = tokio::fs::read(&file.path).await.expect("source bytes");
        bytes.extend_from_slice(b"moved");
        tokio::fs::write(&file.path, &bytes)
            .await
            .expect("rewrite the source");

        let answer = serve.segment("sess-a", "seg00000.m4s").await;
        assert!(
            matches!(
                answer,
                Some(VodPublication {
                    result: Err(VodError::ProducerFailed(_)),
                    ..
                })
            ),
            "a moved source must refuse the GET, not serve stale media"
        );

        let status = serve.status("sess-a").await.expect("status");
        assert_eq!(status.producer_state, "failed");
        assert_eq!(
            status.producer_decision,
            Some(Reason::SourceChanged.status()),
            "the class names what actually changed"
        );
        assert!(
            !Reason::SourceChanged.is_permanent(),
            "a moved source is a statement about this plan: a fresh create \
             re-plans against the file that is there now"
        );
    }

    /// Every generation outcome has a class, and each names what happened.
    ///
    /// A `match` that fell through to one bucket would compile and would make
    /// the whole vocabulary decorative, so this pins each arm. `Stream` maps
    /// onto rolling's existing `ReaderFailed` because it is the same fact —
    /// reading the producer's output failed — while landing and sink faults
    /// have no rolling equivalent and get their own names rather than borrow
    /// one that would misdescribe them.
    #[test]
    fn every_generation_failure_names_what_actually_happened() {
        use crate::playback_control::ProducerDecisionReason as Reason;

        let cases = [
            (Failure::InitDrift("init".to_owned()), Reason::EngineChanged),
            (
                Failure::EngineChanged("engine".to_owned()),
                Reason::EngineChanged,
            ),
            (
                Failure::Landing("landing".to_owned()),
                Reason::MediaLandingFailed,
            ),
            (Failure::Stream("stream".to_owned()), Reason::ReaderFailed),
            (
                Failure::Sink(io::Error::other("sink")),
                Reason::ProducerWriteFailed,
            ),
        ];
        for (failure, expected) in cases {
            assert_eq!(
                classify_failure(&failure),
                expected,
                "{} must not be filed as something else",
                describe_failure(&failure)
            );
            // Permanence follows what a retry can change, not which subsystem
            // failed. A landing, stream or sink fault is a statement about
            // this attempt, and a fresh create can succeed. An engine change
            // is not: the engine baseline is established once per process, so
            // every reopen re-plans straight back into the same verdict.
            assert_eq!(
                classify_failure(&failure).is_permanent(),
                expected == Reason::EngineChanged,
                "{} is classified with the wrong permanence",
                describe_failure(&failure)
            );
        }
    }

    /// The seam the P1-3 correction consists of: the meter a segment answer
    /// carries is the meter its own session publishes.
    ///
    /// Every other proof of this change supplies its own meter, which exercises
    /// the pump and the control view in isolation and would keep passing if
    /// `session_rendition` handed out a fresh meter per request — every VOD
    /// session would silently return to `delivered_bps: None`, preparation
    /// would go back to refusing `throughput_unreported`, and the whole suite
    /// would stay green. This drives the production entry point, so nothing
    /// here chooses the meter, and reads the count back off the session's own
    /// status rather than off the answer.
    #[tokio::test]
    async fn a_segment_answer_carries_the_meter_its_own_session_publishes() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        create(&serve, &file, "sess-b", "play-b", &settings()).await;

        assert_eq!(
            serve
                .status("sess-a")
                .await
                .expect("status")
                .delivered_bytes,
            0,
            "nothing has been delivered yet"
        );

        let ready = fetch(&serve, "sess-a", "seg00000.m4s").await;
        // Exactly what the response pump does, against the meter production
        // chose rather than one this test made.
        ready.delivery.note(4_096);

        assert_eq!(
            serve
                .status("sess-a")
                .await
                .expect("status")
                .delivered_bytes,
            4_096,
            "the session publishes what its own answer counted"
        );
        assert_eq!(
            serve
                .status("sess-b")
                .await
                .expect("status")
                .delivered_bytes,
            0,
            "two viewers of the same immutable rendition are metered apart"
        );
    }

    #[tokio::test]
    async fn a_blocking_get_materializes_the_segment_and_a_re_get_serves_the_same_bytes() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let first = fetch(&serve, "sess-a", "seg00000.m4s").await;
        let etag = first.etag.clone();
        let bytes = read_ready(first).await;
        assert!(!bytes.is_empty());

        let again = fetch(&serve, "sess-a", "seg00000.m4s").await;
        assert_eq!(again.etag, etag, "a strong etag is stable across GETs");
        assert_eq!(read_ready(again).await, bytes, "same bytes on the re-GET");

        // The init the map names is served with its own strong etag.
        let init = match serve.segment("sess-a", "init.mp4").await {
            Some(VodPublication {
                result: Ok(Some(ready)),
                ..
            }) => ready,
            other => panic!("init.mp4: {}", describe(other)),
        };
        assert!(init.etag.contains("-init-"), "{}", init.etag);
        assert!(init.len > 0);
    }

    #[tokio::test]
    async fn deadline_expiry_answers_a_typed_pending() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        let tight = VodSettings {
            block_budget: Duration::from_millis(1),
            ..settings()
        };
        create(&serve, &file, "sess-a", "play-a", &tight).await;
        let last = plan_len(&serve, "sess-a").await - 1;
        let touched = *serve.shared.sessions.lock().await["sess-a"]
            .last_touch
            .lock()
            .expect("touch lock");

        // The producer cannot possibly reach the last entry inside a
        // millisecond, so the hard deadline fires and the answer is the typed
        // retryable pending — never an open-ended wait, never a bare 404.
        match serve
            .segment("sess-a", &segment_name(last as u64))
            .await
            .expect("a vod session")
            .result
        {
            Err(VodError::Pending { retry_after }) => {
                assert_eq!(retry_after, PENDING_RETRY_AFTER);
            }
            other => panic!("expected Pending, got {other:?}"),
        }
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "a timed-out VOD materialization must not renew the session",
        );
    }

    /// A rendition that has recorded a failure gives its producer back.
    ///
    /// The failure is permanent — first-wins, never cleared — and it answers
    /// every waiter at the moment it lands, so the child that is still running
    /// is producing for nobody. The driver used to return the instant it saw a
    /// failure, and `Action::Idle` is the only thing that reclaims a producer,
    /// so nothing was left to reap it: the dormant purge refuses an admitted
    /// rendition outright and the last `Arc` drop waits on whichever generation
    /// task or watchdog outlives it. This drives the case that costs the most —
    /// a SIGSTOP'd child, which goes on holding the codec session a running one
    /// held — and asserts the physical outcome, that the pid is gone and
    /// reaped, not merely that a belief was rewritten.
    ///
    /// The latched capacity hold is checked here too, and deliberately not
    /// dressed up as a wire defect. The first version of this test asserted
    /// that a failed rendition published `producer_hold: no_room`, and it does
    /// not: `status` answers `failed` ahead of every belief arm, so the stale
    /// hold was already shielded. What is asserted is what is true — the field
    /// is cleared rather than latched for the life of the rendition, so the
    /// next reader of it is not the one who discovers it was never true.
    #[tokio::test]
    async fn a_failed_rendition_reclaims_its_producer_and_drops_its_hold() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;

        wait_until("the producer spawned", Duration::from_secs(10), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.last_child_pid.load(Relaxed) != 0 }
        })
        .await;
        let pid = rendition.last_child_pid.load(Relaxed) as libc::pid_t;

        // The expensive shape: stopped, so it makes no progress, and alive, so
        // it still owns everything a running producer owned.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        *rendition.capacity_hold.lock().expect("capacity hold") =
            Some(crate::prodsched::Hold::NoRoom { wanted: 4_096 });
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the fixture needs a live child to reclaim"
        );

        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::EngineChanged,
            "the fixture recorded a permanent rendition failure".to_owned(),
        );

        wait_until(
            "the producer was reclaimed",
            Duration::from_secs(10),
            || {
                let rendition = Arc::clone(&rendition);
                async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
            },
        )
        .await;
        // Reaped, not merely signalled: `perform(Terminate)` is `start_kill`
        // then `wait`, so a pid that still answers here is a zombie this
        // process owns and never collected.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the failed rendition's child is gone, not left holding its codec session"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "gone because it was reaped, not because signalling was refused"
        );
        assert!(
            rendition
                .capacity_hold
                .lock()
                .expect("capacity hold")
                .is_none(),
            "and the hold is cleared rather than latched for the rendition's life"
        );
        let status = serve.status("sess-a").await.expect("status");
        assert_eq!(status.producer_state, "failed");
        assert_eq!(
            status.producer_hold, None,
            "which the status already answered ahead of the belief, and still does"
        );
    }

    /// One unfinished terminal cleanup cannot stop the node's maintenance.
    ///
    /// `maintain` waits on every unfinished cleanup belonging to a tombstoned
    /// session before it does anything else, and that wait had no bound. A
    /// single cleanup that never completes therefore stopped the idle reap,
    /// the dormant-rendition purge, the tombstone eviction and every driver
    /// kick — for the life of the process, with nothing said.
    ///
    /// Two arms could install one: `begin_end` and the supersession sweep both
    /// wrote the tombstone and the cleanup before reading a rendition that may
    /// be `None`, and both then returned without an owner for it. Both now
    /// complete it, so this fixture has to install one by hand — which is the
    /// point. The bound is what holds when the next change to that ordering
    /// gets it wrong.
    #[tokio::test(start_paused = true)]
    async fn one_unfinished_terminal_cleanup_cannot_wedge_node_maintenance() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());

        let wedged = Arc::new(TerminalCleanup::new());
        insert_terminal_session(
            &serve,
            "sess-wedged",
            synthetic_rendition(base.path()).await,
            Arc::clone(&wedged),
        )
        .await;
        let reapable = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_ended_route(store.as_ref(), &reapable, &generation).await;
        insert_finished_terminal_session(&serve, &reapable, synthetic_rendition(base.path()).await)
            .await;

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION + Duration::from_secs(1)).await;
        // Paused time auto-advances to the next timer whenever the runtime is
        // idle, so the elapsed span is exactly the bound the wait armed — which
        // is what makes this an assertion about the documented number rather
        // than about finiteness. Without the timeout there is no timer to
        // advance to and the pass never returns at all.
        let before = tokio::time::Instant::now();
        serve.maintain().await;
        assert_eq!(
            tokio::time::Instant::now() - before,
            TERMINAL_CLEANUP_MAINTENANCE_WAIT,
            "the pass gave up on the wedged cleanup after exactly the documented bound"
        );

        assert!(
            !wedged.is_finished(),
            "the fixture's whole point is a cleanup that never completes"
        );
        // And the pass ran to its end anyway, which is the thing that matters:
        // the work behind the wait still happened.
        assert!(
            serve.playlist(&reapable).await.is_none(),
            "maintenance past its retention window still evicted the other tombstone"
        );
    }

    #[tokio::test]
    async fn a_killed_producer_child_answers_every_waiter_producer_failed() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;

        // The driver spawns the generation for the session's own demand;
        // freeze it the moment its pid appears so nothing materializes.
        wait_until("the producer spawned", Duration::from_secs(10), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.last_child_pid.load(Relaxed) != 0 }
        })
        .await;
        let pid = rendition.last_child_pid.load(Relaxed);
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);

        let last = plan_len(&serve, "sess-a").await - 1;
        let waiter = {
            let serve = Arc::clone(&serve);
            let name = segment_name(last as u64);
            tokio::spawn(async move { serve.segment("sess-a", &name).await })
        };
        wait_until("the waiter registered", Duration::from_secs(10), || {
            let serve = Arc::clone(&serve);
            let key = rendition.key.clone();
            async move { serve.shared.pool.blocked_on(&key).is_some() }
        })
        .await;

        // Kill the child out from under the wait.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);

        match waiter
            .await
            .expect("waiter task")
            .expect("a vod session")
            .result
        {
            Err(VodError::ProducerFailed(cause)) => {
                assert!(!cause.is_empty());
            }
            other => panic!("a dead producer must answer typed, got {:?}", other),
        }
        // And the failure sticks: a later GET for a planned segment answers
        // the same typed refusal without waiting.
        match serve
            .segment("sess-a", &segment_name(last as u64))
            .await
            .expect("a vod session")
            .result
        {
            Err(VodError::ProducerFailed(_)) => {}
            other => panic!("expected ProducerFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_terminal_cause_answers_gone_and_supersession_spares_the_keeper() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let live_owner = serve.playlist("sess-a").await.expect("live owner").owner;

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(
            !serve
                .response_status_owner_is_current("sess-a", &live_owner)
                .await,
            "a live status snapshot cannot authorize after tombstoning"
        );
        let tombstone = serve.playlist("sess-a").await.expect("still ours");
        assert!(
            serve
                .response_status_owner_is_current("sess-a", &tombstone.owner)
                .await,
            "the exact tombstone snapshot authorizes its typed 410"
        );
        match tombstone.result {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("expected Gone(Deleted), got {other:?}"),
        }
        match serve
            .segment("sess-a", "seg00000.m4s")
            .await
            .expect("still ours")
            .result
        {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("expected Gone, got {other:?}"),
        }
        assert!(
            serve.end("sess-a", Terminal::AdminStop).await,
            "a second end is idempotent and keeps the first cause"
        );
        match serve.playlist("sess-a").await.expect("still ours").result {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("the first cause survives: {other:?}"),
        }
        assert!(serve.owns("sess-a").await, "tombstoned is still ours");
        assert!(!serve.end("sess-x", Terminal::Deleted).await, "not ours");

        // Supersession: two sessions of one playback, the newer one kept.
        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        create(&serve, &file, "sess-c", "play-b", &settings()).await;
        assert_eq!(
            serve.supersede("[\"user_id\",1]", "play-b", "sess-c").await,
            1
        );
        match serve.playlist("sess-b").await.expect("still ours").result {
            Err(VodError::Gone(Terminal::Superseded)) => {}
            other => panic!("expected Gone(Superseded), got {other:?}"),
        }
        assert!(
            serve.playlist("sess-c").await.expect("ours").is_ok(),
            "`keep` must survive the sweep"
        );

        let events = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = serve
                    .shared
                    .store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: None,
                        limit: 20,
                    })
                    .await
                    .expect("VOD lifecycle telemetry query");
                if events
                    .iter()
                    .filter(|event| matches!(event.event.as_str(), "session_start" | "session_end"))
                    .count()
                    >= 5
                {
                    break events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("VOD lifecycle telemetry persisted");
        let starts: Vec<_> = events
            .iter()
            .filter(|event| event.event == "session_start")
            .collect();
        let ends: Vec<_> = events
            .iter()
            .filter(|event| event.event == "session_end")
            .collect();
        assert_eq!(starts.len(), 3, "one start per attached VOD handle");
        assert_eq!(ends.len(), 2, "an idempotent second end emits nothing");
        assert!(starts.iter().all(|event| {
            event.method.as_deref() == Some("remux")
                && event.encoder.as_deref() == Some("vod")
                && event
                    .extra
                    .as_deref()
                    .is_some_and(|extra| extra.contains("\"presentation\":\"vod\""))
        }));
        assert!(ends
            .iter()
            .any(|event| event.reason.as_deref() == Some("client_released")));
        assert!(ends
            .iter()
            .any(|event| event.reason.as_deref() == Some("superseded")));
    }

    #[tokio::test]
    async fn durable_terminal_cause_refines_only_a_provisional_replaced_tombstone() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        assert!(serve.end("sess-a", Terminal::Replaced).await);
        assert!(serve.end("sess-a", Terminal::AdminStop).await);
        assert!(matches!(
            serve.playlist("sess-a").await.expect("still ours").result,
            Err(VodError::Gone(Terminal::AdminStop))
        ));

        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        assert!(serve.end("sess-b", Terminal::Deleted).await);
        assert!(serve.end("sess-b", Terminal::AdminStop).await);
        assert!(matches!(
            serve.playlist("sess-b").await.expect("still ours").result,
            Err(VodError::Gone(Terminal::Deleted))
        ));
    }

    /// Drive a whole small fixture to completion through real blocking GETs,
    /// then prove admission and that a second session on the same recipe
    /// rides the admitted rendition with no producer at all.
    #[tokio::test]
    async fn a_completed_rendition_admits_and_serves_a_second_session_without_a_producer() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let len = plan_len(&serve, "sess-a").await;
        for segment in 0..len {
            fetch(&serve, "sess-a", &segment_name(segment as u64)).await;
        }
        let rendition = rendition_of(&serve, "sess-a").await;
        wait_until("the rendition admitted", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.manifest.lock().await.is_admitted() }
        })
        .await;
        // The finished producer is reclaimed once the driver sees it idle
        // past every gap.
        wait_until("the producer reclaimed", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;

        // A second session on the same recipe attaches to the SAME rendition
        // and every segment is served instantly, no producer spawned.
        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        let second = rendition_of(&serve, "sess-b").await;
        assert!(
            Arc::ptr_eq(&rendition, &second),
            "one rendition per recipe, shared by its sessions"
        );
        for segment in 0..len {
            match serve
                .segment("sess-b", &segment_name(segment as u64))
                .await
                .expect("a vod session")
                .result
            {
                Ok(Some(ready)) => assert!(ready.len > 0),
                other => panic!("segment {segment} must serve instantly: {other:?}"),
            }
        }
        assert!(
            matches!(rendition.slot.belief().await, Producer::Absent { .. }),
            "an admitted rendition serves without spawning a producer"
        );
    }

    /// Resurrection: a new `VodServe` over the same base directory and store
    /// adopts the rendition — identical playlist bytes, materialized segments
    /// served without re-production.
    #[tokio::test]
    async fn a_new_vodserve_on_the_same_base_resurrects_the_rendition() {
        let base = crate::test_tempdir().expect("base");
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        let first = VodServe::new(base.path().to_path_buf(), Arc::clone(&store));
        create(&first, &file, "sess-a", "play-a", &settings()).await;
        let len = plan_len(&first, "sess-a").await;
        for segment in 0..len {
            fetch(&first, "sess-a", &segment_name(segment as u64)).await;
        }
        let playlist_before = first
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("bytes")
            .0;
        // Wait for the generation to finish so nothing writes under the
        // successor.
        let rendition = rendition_of(&first, "sess-a").await;
        wait_until("the first producer ended", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;
        drop(first);

        let second = VodServe::new(base.path().to_path_buf(), store);
        create(&second, &file, "sess-b", "play-b", &settings()).await;
        let playlist_after = second
            .playlist("sess-b")
            .await
            .expect("ours")
            .expect("bytes")
            .0;
        assert_eq!(
            playlist_before, playlist_after,
            "a resurrected rendition serves the identical playlist"
        );
        // Every materialized segment is adopted and served with no producer.
        for segment in 0..len {
            match second
                .segment("sess-b", &segment_name(segment as u64))
                .await
                .expect("a vod session")
                .result
            {
                Ok(Some(ready)) => assert!(ready.len > 0),
                other => panic!("adopted segment {segment} must serve instantly: {other:?}"),
            }
        }
        let adopted = rendition_of(&second, "sess-b").await;
        assert!(
            matches!(adopted.slot.belief().await, Producer::Absent { .. }),
            "adoption serves without re-production"
        );
    }

    #[tokio::test]
    async fn varying_parameter_sets_are_refused_instead_of_changing_presentation() {
        testfixtures::require_ffmpeg();
        let file = fixture_file();
        let (store, mut index) = store_with_index(&file).await;
        index.parameter_sets_constant = false;
        store
            .put_fragment_index(file.id, &index)
            .await
            .expect("replace the index");
        let base = crate::test_tempdir().expect("base");
        let serve = VodServe::new(base.path().to_path_buf(), store);

        let error = serve
            .try_create(
                &request("play-a", 0.0),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-a".into(),
            )
            .await
            .expect_err("varying parameter sets cannot make one immutable init");
        assert!(
            error.contains("vod_source_unsupported"),
            "the refusal must be typed: {error}"
        );
        assert!(!serve.owns("sess-a").await);
    }

    #[tokio::test]
    async fn serving_loss_racing_final_cluster_attachment_leaves_no_session() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let base = crate::test_tempdir().expect("cluster serving fence base");
        let (serve, file) = serve_on(base.path()).await;
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let authority = fence.authority();
        let generation = authority.admit().expect("initial serving authority");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        let create = tokio::spawn({
            let serve = Arc::clone(&serve);
            let file = file.clone();
            let pause = Arc::clone(&pause);
            async move {
                serve
                    .try_create_cluster(
                        &request("cluster-fenced", 0.0),
                        &file,
                        &settings(),
                        VodAttribution {
                            user_name: "paul",
                            item_title: "Fixture",
                            supersession_user: "[\"user_id\",1]",
                        },
                        "cluster-fenced-session".to_owned(),
                        VodServingAdmission::new(
                            authority,
                            generation,
                            Instant::now() + Duration::from_secs(30),
                        )
                        .with_pause_before_commit(pause),
                    )
                    .await
            }
        });

        pause.wait().await;
        fence.validation_set_ready(false).await;
        pause.wait().await;
        let error = create
            .await
            .expect("cluster create task")
            .expect_err("stale serving authority must refuse final attachment");
        assert!(crate::transcode::is_serving_fence_error(&error), "{error}");
        assert!(!serve.owns("cluster-fenced-session").await);

        fence.validation_set_ready(true).await;
        let recovered_authority = fence.authority();
        let recovered_generation = recovered_authority
            .admit()
            .expect("recovered serving authority");
        serve
            .try_create_cluster(
                &request("cluster-recovered", 0.0),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "cluster-recovered-session".to_owned(),
                VodServingAdmission::new(
                    recovered_authority,
                    recovered_generation,
                    Instant::now() + Duration::from_secs(30),
                ),
            )
            .await
            .expect("recovered authority may attach a new VOD session");
        assert!(serve.owns("cluster-recovered-session").await);
    }

    #[tokio::test]
    async fn requests_the_vod_presentation_cannot_serve_fail_typed() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;

        // A transcode recipe: gated on the D6 device measurement.
        let mut transcode = request("play-a", 0.0);
        transcode.kind = SessionKind::Transcode { height: 720 };
        let transcode_error = serve
            .try_create(
                &transcode,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-t".into(),
            )
            .await
            .expect_err("transcode VOD is not implemented");
        assert!(transcode_error.contains("vod_transcode_unavailable"));

        // A subtitle burn changes the video pipeline.
        let mut burn = request("play-a", 0.0);
        burn.subtitle_burn = Some(0);
        let burn_error = serve
            .try_create(
                &burn,
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-s".into(),
            )
            .await
            .expect_err("burn VOD is not implemented");
        assert!(burn_error.contains("vod_subtitle_burn_unavailable"));

        // No index stored for the current identity.
        let empty_store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let bare = VodServe::new(base.path().join("bare"), empty_store);
        let index_error = bare
            .try_create(
                &request("play-a", 0.0),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-n".into(),
            )
            .await
            .expect_err("an unindexed file must not change presentation");
        assert!(index_error.contains("vod_index_pending"));
    }

    #[tokio::test]
    async fn unknown_and_unsafe_segment_names_answer_a_404_shape() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let touched = *serve.shared.sessions.lock().await["sess-a"]
            .last_touch
            .lock()
            .expect("touch lock");

        for name in [
            "seg99999.m4s",  // past the plan's end
            "notes.txt",     // not a segment name at all
            "../etc/passwd", // traversal
            "seg../x.m4s",   // traversal dressed as a segment
            "seg00000.ts",   // the transcode extension is not this path's
            "seg.m4s",       // no digits
        ] {
            let publication = serve.segment("sess-a", name).await.expect("a vod session");
            match publication.result {
                Ok(None) => assert!(
                    serve
                        .response_owner_is_live("sess-a", &publication.owner)
                        .await,
                    "{name} must retain the exact attachment that classified its 404"
                ),
                other => panic!("{name} must be the caller's 404: {:?}", other),
            }
        }
        // A session this runtime never made is nobody's business here.
        assert!(serve.segment("sess-x", "seg00000.m4s").await.is_none());
        assert!(serve.playlist("sess-x").await.is_none());
        assert!(!serve.owns("sess-x").await);
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "invalid names and indexes never renew the VOD attachment",
        );
    }

    #[tokio::test]
    async fn status_describes_vod_without_touching_its_idle_lease() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("sess-a", 0).await;
        let touched = Instant::now();
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(rendition.recipe.file.clone()),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(touched),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );

        let status = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(status.id, "sess-a");
        assert_eq!(status.file_id, 1);
        assert_eq!(status.encoder, "vod");
        assert_eq!(status.playlist_shape, "vod");
        assert_eq!(status.producer_state, "waiting");
        assert_eq!(status.fetched_end_ms, 0);
        assert_eq!(status.materialized_segments, 0);
        assert!(status.planned_segments > 1);
        assert_eq!(status.working_set_budget_bytes, 8 << 30);
        assert!(matches!(
            serve.segment("sess-a", "notes.txt").await,
            Some(VodPublication {
                result: Ok(None),
                ..
            })
        ));
        let (_, owner) = serve
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("playlist");
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "diagnostics, lookups, and invalid objects must not keep an abandoned session alive",
        );
        assert!(serve.commit_resolved_media("sess-a", &owner, None).await);
        assert!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock")
                > touched,
            "only the resolved response commit renews the VOD session",
        );

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(
            serve.status("sess-a").await.is_none(),
            "a tombstone is not live"
        );
        assert!(serve.status("unknown").await.is_none());
    }

    #[tokio::test]
    async fn a_far_seek_reports_the_frontier_the_client_can_actually_fetch() {
        // The two frontiers answer different questions and a far seek pulls
        // them apart. `published_end_ms` is how much of the title exists,
        // counted from the start, and the activity page asks that. A client
        // parked past a hole asks a different one: is there anything ahead of
        // *me*. Reporting the first as the second leaves a wedged player being
        // told, correctly and uselessly, that the film begins.
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("sess-a", 20).await;
        {
            let mut manifest = rendition.manifest.lock().await;
            assert!(manifest.len() > 23, "the fixture needs a hole to seek past");
            for index in [0, 1, 2, 3, 4, 20, 21, 22] {
                assert!(manifest.materialize(index, 1_024, 0));
            }
        }
        rendition
            .readers
            .lock()
            .await
            .get_mut("sess-a")
            .expect("reader")
            .last_served = Some(20);
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(rendition.recipe.file.clone()),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
        let end_of = |index: u32| {
            rendition
                .plan
                .entry(index)
                .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
                .expect("planned segment")
        };

        let status = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(
            status.published_end_ms,
            Some(end_of(4)),
            "the run from the start stops at the hole",
        );
        assert_eq!(
            status.ready_ahead_end_ms,
            Some(end_of(22)),
            "the run from this client's own segment does not",
        );
        assert_eq!(status.fetched_end_ms, end_of(20));
        assert!(
            status.published_end_ms < Some(status.fetched_end_ms),
            "and the title's frontier is behind this client, which is the trap",
        );

        // Fill the hole: with nothing to seek past the two answers are the
        // same, which is why this is a correction and not a second policy.
        {
            let mut manifest = rendition.manifest.lock().await;
            for index in 5..20 {
                assert!(manifest.materialize(index, 1_024, 0));
            }
        }
        let filled = serve.status("sess-a").await.expect("live VOD status");
        assert_eq!(filled.published_end_ms, filled.ready_ahead_end_ms);
    }

    #[tokio::test]
    async fn resolved_vod_owner_cannot_commit_after_tombstone_or_reattachment() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("sess-a", 0).await;
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(Arc::clone(&rendition)),
                rendition_key: rendition.key.clone(),
                file: Arc::new(rendition.recipe.file.clone()),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );

        let (_, stale_owner) = serve
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("playlist");
        assert!(serve.response_owner_is_live("sess-a", &stale_owner).await);
        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(!serve.response_owner_is_live("sess-a", &stale_owner).await);
        assert!(
            !serve
                .commit_resolved_media("sess-a", &stale_owner, Some(0))
                .await
        );

        let replacement_touch = Instant::now() - Duration::from_secs(5);
        let replacement_key = rendition.key.clone();
        let replacement_file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(rendition),
                rendition_key: replacement_key,
                file: replacement_file,
                playback_id: "play-b".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 2,
                target_height: 360,
                kind: request("play-b", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(replacement_touch),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
        assert!(!serve.response_owner_is_live("sess-a", &stale_owner).await);
        assert!(
            !serve
                .commit_resolved_media("sess-a", &stale_owner, Some(0))
                .await
        );
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            replacement_touch,
            "an old incarnation cannot renew a new attachment under the same id",
        );
    }

    #[tokio::test]
    async fn idle_reap_and_same_id_resurrection_are_one_reader_transition() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;
        let (_, stale_owner) = serve
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("playlist");
        *serve.shared.sessions.lock().await["sess-a"]
            .last_touch
            .lock()
            .expect("touch lock") = Instant::now() - SESSION_IDLE_TTL - Duration::from_secs(1);

        // Stop the old reap exactly after registry removal but before reader
        // detach. Resurrection must wait on the stable per-id lifecycle gate,
        // not attach a successor that the old reap can then remove.
        let readers = rendition.readers.lock().await;
        let maintain = tokio::spawn({
            let serve = Arc::clone(&serve);
            async move { serve.maintain().await }
        });
        wait_until("idle registry removal", Duration::from_secs(2), || {
            let serve = Arc::clone(&serve);
            async move { !serve.shared.sessions.lock().await.contains_key("sess-a") }
        })
        .await;
        let resurrect = tokio::spawn({
            let serve = Arc::clone(&serve);
            let file = file.clone();
            async move { create(&serve, &file, "sess-a", "play-b", &settings()).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !resurrect.is_finished(),
            "resurrection waits until the old reader detach is complete"
        );

        drop(readers);
        maintain.await.expect("maintenance task");
        resurrect.await.expect("resurrection task");
        assert!(serve.shared.sessions.lock().await.contains_key("sess-a"));
        assert_eq!(
            rendition
                .readers
                .lock()
                .await
                .get("sess-a")
                .cloned()
                .expect("replacement reader")
                .last_served,
            None,
            "the replacement reader survives the predecessor's reap"
        );
        assert!(
            !serve
                .commit_resolved_media("sess-a", &stale_owner, Some(1))
                .await,
            "the old response incarnation cannot commit into the replacement"
        );
        assert_eq!(
            rendition.readers.lock().await["sess-a"].last_served,
            None,
            "the stale completion did not move the replacement frontier"
        );
    }

    #[tokio::test]
    async fn vod_control_renews_only_fresh_sequences_and_lifecycle_is_per_session() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = VodServe::new(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        let before = Instant::now() - Duration::from_secs(10);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), before).await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence, owner_epoch| crate::playback_control::LocalControlRequest {
            session_id: &session_id,
            generation: &generation,
            owner_node_id: "node-a",
            owner_epoch,
            client_instance_id: &client,
            sequence,
            snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Apple,
            ),
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };

        let accepted = serve
            .control(request(1, 1))
            .await
            .expect("VOD registry owner")
            .expect("accepted control");
        assert_eq!(
            accepted.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        let accepted_touch = {
            let sessions = serve.shared.sessions.lock().await;
            let last = sessions[&session_id].last_touch.lock().expect("touch lock");
            assert!(*last > before);
            *last
        };
        let replay = serve
            .control(request(1, 1))
            .await
            .expect("VOD registry owner")
            .expect("replay control");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(
            *serve.shared.sessions.lock().await[&session_id]
                .last_touch
                .lock()
                .expect("touch lock"),
            accepted_touch
        );
        assert!(matches!(
            serve.control(request(0, 1)).await,
            Some(Err(
                crate::playback_control::ControlStateError::StaleSequence
            ))
        ));
        assert!(matches!(
            serve.control(request(2, 2)).await,
            Some(Err(
                crate::playback_control::ControlStateError::OwnerChanged
            ))
        ));
        assert_eq!(
            *serve.shared.sessions.lock().await[&session_id]
                .last_touch
                .lock()
                .expect("touch lock"),
            accepted_touch
        );

        // Holding A's lifecycle cannot head-of-line block an unrelated VOD
        // terminal transition. A node-wide gate makes this timeout.
        let other_id = uuid::Uuid::new_v4().to_string();
        insert_control_session(&serve, &other_id, rendition, Instant::now()).await;
        let lifecycle = Arc::clone(&serve.shared.sessions.lock().await[&session_id].lifecycle);
        let lifecycle_guard = lifecycle.lock().await;
        assert!(tokio::time::timeout(
            Duration::from_millis(250),
            serve.end(&other_id, Terminal::Deleted),
        )
        .await
        .expect("unrelated lifecycle must not queue"));
        drop(lifecycle_guard);
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_vod_control_end_tombstones_detaches_and_replays_exactly() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = VodServe::new(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        let before = Instant::now() - Duration::from_secs(10);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), before).await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence| {
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Apple,
            );
            snapshot.demand = crate::playback_control::PlaybackDemand::End;
            snapshot.playback_rate = 0.0;
            snapshot.render_state = crate::playback_control::RenderState::Ended;
            crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: &client,
                sequence,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            }
        };

        let committer = Arc::new(CleanupObservingCommitter {
            rendition: Arc::clone(&rendition),
            session_id: session_id.clone(),
            started: AtomicBool::new(false),
            reader_detached: AtomicBool::new(false),
            attempts: Arc::new(AtomicUsize::new(0)),
            expires_at_unix_ms: crate::media_sessions::unix_ms().saturating_add(
                i64::try_from((TERMINAL_TOMBSTONE_RETENTION * 2).as_millis()).unwrap_or(i64::MAX),
            ),
        });
        let committer_weak = Arc::downgrade(&committer);
        let accepted = serve
            .control_with_terminal(request(1), i64::MAX, Some(committer.clone()), None)
            .await
            .expect("VOD registry owner")
            .expect("end accepted");
        assert_eq!(
            accepted.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        assert_eq!(accepted.lease_state, "ended");
        assert_eq!(
            *serve.shared.sessions.lock().await[&session_id]
                .last_touch
                .lock()
                .expect("touch lock"),
            before,
            "terminal control is not a five-minute lease renewal"
        );
        assert_eq!(
            serve.shared.sessions.lock().await[&session_id].tombstone,
            Some(Terminal::Deleted)
        );
        assert!(!rendition.readers.lock().await.contains_key(&session_id));
        assert!(serve.status(&session_id).await.is_none());
        assert!(committer.started.load(Acquire));
        assert!(
            committer.reader_detached.load(Acquire),
            "the durable terminal commit cannot start before VOD reader detach"
        );
        assert!(matches!(
            accepted
                .terminal_commit
                .as_ref()
                .expect("deferred terminal receipt")
                .wait()
                .await,
            Err(())
        ));
        assert_eq!(committer.attempts.load(Acquire), 1);

        let replay = serve
            .control(request(1))
            .await
            .expect("terminal VOD registry owner")
            .expect("exact terminal replay");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(replay.accepted_sequence, accepted.accepted_sequence);
        assert_eq!(replay.lease_state, "ended");
        let committed = replay
            .terminal_commit
            .as_ref()
            .expect("retried VOD terminal receipt")
            .wait()
            .await
            .expect("recovered VOD terminal response");
        assert_eq!(committed.server_time_unix_ms, 1);
        assert_eq!(committer.attempts.load(Acquire), 2);

        let active_same_sequence = crate::playback_control::LocalControlRequest {
            session_id: &session_id,
            generation: &generation,
            owner_node_id: "node-a",
            owner_epoch: 1,
            client_instance_id: &client,
            sequence: 1,
            snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Apple,
            ),
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };
        assert!(matches!(
            serve.control(active_same_sequence).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));
        assert!(matches!(
            serve.control(request(2)).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION).await;
        {
            let sessions = serve.shared.sessions.lock().await;
            let terminal = &sessions[&session_id];
            assert!(
                terminal
                    .terminal_cleanup
                    .as_ref()
                    .is_some_and(|cleanup| cleanup.retention_expired()),
                "cleanup retention has elapsed for this interleave"
            );
            assert!(
                !terminal.terminal_replay_expired(),
                "an unexpired terminal commit receipt independently fences eviction"
            );
        }

        let terminal_deadline = accepted
            .terminal_commit
            .as_ref()
            .expect("accepted terminal receipt")
            .deadline_for_test();
        tokio::time::advance(
            terminal_deadline
                .duration_since(tokio::time::Instant::now())
                .saturating_sub(Duration::from_millis(1)),
        )
        .await;
        assert_eq!(
            serve
                .control(request(1))
                .await
                .expect("VOD tombstone before expiry")
                .expect("exact response retained before expiry")
                .disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert!(serve.shared.sessions.lock().await[&session_id]
            .control_end
            .is_some());

        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(
            serve.control(request(1)).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));
        let sessions = serve.shared.sessions.lock().await;
        assert_eq!(sessions[&session_id].tombstone, Some(Terminal::Deleted));
        assert!(
            sessions[&session_id].control_end.is_none(),
            "expired VOD recovery drops the retained operation but keeps its ownership tombstone"
        );
        let cleanup_before_repeat = sessions[&session_id]
            .terminal_cleanup
            .as_ref()
            .map(Arc::clone)
            .expect("completed terminal cleanup marker survives expiry");
        assert!(cleanup_before_repeat.is_finished());
        drop(sessions);
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(
            serve.control(request(1)).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));

        assert!(
            serve.end(&session_id, Terminal::AdminStop).await,
            "a repeated non-control end remains idempotent after response expiry"
        );
        let cleanup_after_repeat = serve.shared.sessions.lock().await[&session_id]
            .terminal_cleanup
            .as_ref()
            .map(Arc::clone)
            .expect("idempotent terminal cleanup marker");
        assert!(
            Arc::ptr_eq(&cleanup_before_repeat, &cleanup_after_repeat),
            "expiry plus repeated end must not create a second cleanup or lifecycle-emission task"
        );

        serve.shared.sessions.lock().await.remove(&session_id);
        drop(accepted);
        drop(replay);
        drop(committer);
        assert!(
            committer_weak.upgrade().is_none(),
            "removing the VOD tombstone must release its deferred operation graph"
        );
    }

    #[tokio::test]
    async fn vod_end_transfers_a_rollover_abort_before_tombstoning() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = VodServe::new(base.path().to_path_buf(), store.clone());
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, rendition, Instant::now()).await;

        let epoch_one_client = uuid::Uuid::new_v4().to_string();
        serve
            .control(crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: &epoch_one_client,
                sequence: 1,
                snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Apple,
                ),
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            })
            .await
            .expect("VOD registry owner")
            .expect("epoch one control accepted");

        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        {
            let sessions = serve.shared.sessions.lock().await;
            assert!(sessions[&session_id]
                .control
                .lock()
                .expect("control lock")
                .stage_preparation_for_owner(
                    staged_incarnation_id.clone(),
                    generation.clone(),
                    i64::MAX,
                    1,
                ));
        }

        let route = store
            .media_session_route(&session_id)
            .await
            .expect("route read")
            .expect("active route");
        let takeover_at = route.lease_expires_at_ms.saturating_add(1);
        let transferred = store
            .claim_media_session_takeover(&plurx_core::domain::MediaSessionTakeover {
                incarnation_id: generation.clone(),
                expected_owner_node_id: "node-a".to_owned(),
                expected_owner_epoch: 1,
                next_owner_node_id: "node-b".to_owned(),
                now_ms: takeover_at,
                lease_expires_at_ms: takeover_at.saturating_add(60_000),
            })
            .await
            .expect("takeover")
            .expect("epoch two route");
        assert_eq!(transferred.owner_epoch, 2);

        let admission = Arc::new(RecordingPreparationAdmission::default());
        let epoch_two_client = uuid::Uuid::new_v4().to_string();
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        snapshot.demand = crate::playback_control::PlaybackDemand::End;
        snapshot.playback_rate = 0.0;
        snapshot.render_state = crate::playback_control::RenderState::Ended;
        let ended = serve
            .control_with_terminal(
                crate::playback_control::LocalControlRequest {
                    session_id: &session_id,
                    generation: &generation,
                    owner_node_id: "node-b",
                    owner_epoch: 2,
                    client_instance_id: &epoch_two_client,
                    sequence: 1,
                    snapshot,
                    prepared_successor:
                        crate::playback_control::PreparedSuccessorObservation::NotRequested,
                },
                i64::MAX,
                None,
                Some(admission.clone()),
            )
            .await
            .expect("VOD registry owner")
            .expect("epoch two End accepted");
        assert_eq!(ended.lease_state, "ended");
        assert_eq!(
            admission
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(|outcome| &outcome.preparation_directive),
            Some(&crate::playback_control::PreparationDirective::Abort {
                staged_incarnation_id,
                acknowledgement_rejected: false,
            }),
            "the durable abort owner must receive the inherited slot before End clears it"
        );
        assert_eq!(
            serve.shared.sessions.lock().await[&session_id].tombstone,
            Some(Terminal::Deleted)
        );
    }

    #[tokio::test]
    async fn cancelled_vod_end_response_cannot_cancel_terminal_reader_cleanup() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = VodServe::new(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let client = uuid::Uuid::new_v4().to_string();
        let committer = Arc::new(CleanupObservingCommitter {
            rendition: Arc::clone(&rendition),
            session_id: session_id.clone(),
            started: AtomicBool::new(false),
            reader_detached: AtomicBool::new(false),
            attempts: Arc::new(AtomicUsize::new(0)),
            expires_at_unix_ms: crate::media_sessions::unix_ms().saturating_add(60_000),
        });

        // Pause the session-owned task at the exact pre-detach point. Status
        // preparation is therefore free to inspect the reader registry before
        // End publishes its cleanup marker.
        let terminal_detach_pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .terminal_detach_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&terminal_detach_pause));
        let pending = {
            let serve = Arc::clone(&serve);
            let session_id = session_id.clone();
            let generation = generation.clone();
            let client = client.clone();
            let committer = committer.clone();
            tokio::spawn(async move {
                let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Apple,
                );
                snapshot.demand = crate::playback_control::PlaybackDemand::End;
                snapshot.playback_rate = 0.0;
                snapshot.render_state = crate::playback_control::RenderState::Ended;
                serve
                    .control_with_terminal(
                        crate::playback_control::LocalControlRequest {
                            session_id: &session_id,
                            generation: &generation,
                            owner_node_id: "node-a",
                            owner_epoch: 1,
                            client_instance_id: &client,
                            sequence: 1,
                            snapshot,
                            prepared_successor:
                                crate::playback_control::PreparedSuccessorObservation::NotRequested,
                        },
                        i64::MAX,
                        Some(committer),
                        None,
                    )
                    .await
            })
        };
        let cleanup = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let cleanup = serve.shared.sessions.lock().await[&session_id]
                    .terminal_cleanup
                    .as_ref()
                    .map(Arc::clone);
                if let Some(cleanup) = cleanup {
                    return cleanup;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal commit publishes session-owned cleanup");
        terminal_detach_pause.wait().await;
        assert!(!cleanup.is_finished());
        pending.abort();
        let cancellation = match pending.await {
            Err(error) => error,
            Ok(_) => panic!("request unexpectedly completed"),
        };
        assert!(cancellation.is_cancelled());

        let terminal_replay_pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .terminal_replay_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&terminal_replay_pause));
        let replay = {
            let serve = Arc::clone(&serve);
            let session_id = session_id.clone();
            let generation = generation.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Apple,
                );
                snapshot.demand = crate::playback_control::PlaybackDemand::End;
                snapshot.playback_rate = 0.0;
                snapshot.render_state = crate::playback_control::RenderState::Ended;
                serve
                    .control(crate::playback_control::LocalControlRequest {
                        session_id: &session_id,
                        generation: &generation,
                        owner_node_id: "node-a",
                        owner_epoch: 1,
                        client_instance_id: &client,
                        sequence: 1,
                        snapshot,
                        prepared_successor:
                            crate::playback_control::PreparedSuccessorObservation::NotRequested,
                    })
                    .await
            })
        };
        terminal_replay_pause.wait().await;
        assert!(
            !committer.started.load(Acquire),
            "a replacement waiter at the cleanup fence cannot expose terminal settlement while detach is pinned"
        );
        terminal_replay_pause.wait().await;
        assert!(rendition.readers.lock().await.contains_key(&session_id));
        terminal_detach_pause.wait().await;
        tokio::time::timeout(Duration::from_secs(1), cleanup.wait())
            .await
            .expect("detached cleanup survives request cancellation");
        assert!(!rendition.readers.lock().await.contains_key(&session_id));
        assert!(committer.started.load(Acquire));
        assert!(committer.reader_detached.load(Acquire));

        let replay = replay
            .await
            .expect("replacement replay task")
            .expect("terminal VOD session")
            .expect("terminal replay after cleanup");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert!(replay
            .terminal_commit
            .as_ref()
            .expect("replacement receipt")
            .wait()
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn accepted_seek_intent_controls_demand_and_retention_in_both_directions() {
        const VIEWER: &str = "00000000-0000-4000-8000-000000000001";
        const GENERATION: &str = "00000000-0000-4000-8000-000000000002";
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        activate_control_route(store.as_ref(), VIEWER, GENERATION).await;
        let serve = VodServe::new(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, VIEWER, Arc::clone(&rendition), Instant::now()).await;
        let control = |sequence, snapshot| crate::playback_control::LocalControlRequest {
            session_id: VIEWER,
            generation: GENERATION,
            owner_node_id: "node-a",
            owner_epoch: 1,
            client_instance_id: "00000000-0000-4000-8000-000000000005",
            sequence,
            snapshot,
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };
        let key = |index| WaitKey {
            rendition: rendition.key.clone(),
            index,
        };
        let old = serve
            .shared
            .pool
            .register(key(3), VIEWER)
            .expect("old GET admitted");
        let current = serve
            .shared
            .pool
            .register(key(45), VIEWER)
            .expect("seek GET admitted");
        let prefetch = serve
            .shared
            .pool
            .register(key(58), VIEWER)
            .expect("prefetch admitted");
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        snapshot.render_state = crate::playback_control::RenderState::Seeking;
        snapshot.seek_target_ms = Some(
            (rendition.plan.entry(45).expect("target").start_ticks * 1000
                / u64::from(rendition.timescale)) as i64
                + 1,
        );
        serve
            .control(control(1, snapshot.clone()))
            .await
            .expect("VOD session")
            .expect("accepted seek");
        let position = Position {
            produced_through: None,
            positioned_at: None,
            seconds_per_segment: rendition.seconds_per_segment,
            working_set: WorkingSet::default(),
        };
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        assert_eq!(
            decide(&*rendition.manifest.lock().await, &demands, position, &[]),
            Action::Reposition { to: 45 }
        );
        assert!(
            !reader_window(&readers[VIEWER], rendition.seconds_per_segment).covers(3),
            "a forward seek does not protect the entire intervening film"
        );
        drop(readers);

        snapshot.seek_target_ms = Some(
            (rendition.plan.entry(3).expect("rewind").start_ticks * 1000
                / u64::from(rendition.timescale)) as i64
                + 1,
        );
        let mut rewind = serve
            .control(control(2, snapshot.clone()))
            .await
            .expect("VOD session");
        if let Err(crate::playback_control::ControlStateError::RateLimited(ms)) = rewind {
            tokio::time::sleep(Duration::from_millis(u64::from(ms) + 1)).await;
            rewind = serve
                .control(control(2, snapshot))
                .await
                .expect("VOD session");
        }
        rewind.expect("accepted rewind");
        drop(prefetch);
        let near_prefetch = serve
            .shared
            .pool
            .register(key(10), VIEWER)
            .expect("nearby prefetch admitted");
        let mut readers = rendition.readers.lock().await;
        // An old high GET completing after the backward seek is delivery
        // evidence, not a new playback command.
        readers.get_mut(VIEWER).expect("reader").served(45);
        assert_eq!(readers[VIEWER].frontier, 3);
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        assert_eq!(
            decide(
                &*rendition.manifest.lock().await,
                &demands,
                Position {
                    positioned_at: Some(10),
                    ..position
                },
                &[]
            ),
            Action::Reposition { to: 3 }
        );
        assert!(!reader_window(&readers[VIEWER], rendition.seconds_per_segment).covers(45));
        drop(readers);
        assert_eq!(
            serve.shared.pool.retained(&rendition.key),
            vec![3, 10, 45],
            "every admitted request still has a narrow pin"
        );
        drop((current, near_prefetch));
        assert_eq!(serve.shared.pool.retained(&rendition.key), vec![3]);
        drop(old);
        assert!(serve.shared.pool.is_empty());
    }

    #[tokio::test]
    async fn cancelling_after_control_acceptance_keeps_the_anchor_and_exact_replay() {
        const VIEWER: &str = "00000000-0000-4000-8000-000000000003";
        const GENERATION: &str = "00000000-0000-4000-8000-000000000004";
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        activate_control_route(store.as_ref(), VIEWER, GENERATION).await;
        let serve = VodServe::new(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, VIEWER, Arc::clone(&rendition), Instant::now()).await;
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        snapshot.render_state = crate::playback_control::RenderState::Seeking;
        snapshot.seek_target_ms = Some(300_000);
        let target = entry_containing(&rendition.plan, 300.0);
        let ledger = Arc::clone(&rendition.readers.lock().await[VIEWER].marker_prewarm);
        ledger.lock().expect("ledger").enabled = true;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve
            .shared
            .control_applied_pause
            .lock()
            .expect("pause lock") = Some(Arc::clone(&pause));
        let accepted = {
            let serve = Arc::clone(&serve);
            let snapshot = snapshot.clone();
            tokio::spawn(async move {
                serve
                    .control(crate::playback_control::LocalControlRequest {
                        session_id: VIEWER,
                        generation: GENERATION,
                        owner_node_id: "node-a",
                        owner_epoch: 1,
                        client_instance_id: "00000000-0000-4000-8000-000000000006",
                        sequence: 1,
                        snapshot,
                        prepared_successor:
                            crate::playback_control::PreparedSuccessorObservation::NotRequested,
                    })
                    .await
            })
        };
        pause.wait().await;
        assert_eq!(
            rendition.readers.lock().await[VIEWER].frontier,
            target,
            "accepted sequence and playback anchor commit before any cancellable side effects"
        );
        assert!(!ledger.lock().expect("ledger").enabled, "accepted intent invalidates old speculation even when the later marker update is cancelled");
        accepted.abort();
        let _ = accepted.await;
        *serve
            .shared
            .control_applied_pause
            .lock()
            .expect("pause lock") = None;
        let replay = serve
            .control(crate::playback_control::LocalControlRequest {
                session_id: VIEWER,
                generation: GENERATION,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: "00000000-0000-4000-8000-000000000006",
                sequence: 1,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            })
            .await
            .expect("VOD session")
            .expect("exact replay");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(rendition.readers.lock().await[VIEWER].frontier, target);
    }

    #[tokio::test]
    async fn a_published_blocked_get_keeps_its_pin_through_the_response_file_open() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("viewer", 3).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *serve.shared.segment_ready_pause.lock().expect("pause lock") = Some(Arc::clone(&pause));
        let get = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        45,
                        Duration::from_secs(5),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        pause.wait().await;
        // Publication follows the production GET's post-registration check;
        // the oneshot remembers Ready even if it precedes the next poll.
        rendition
            .dir
            .materialize(
                &mut *rendition.manifest.lock().await,
                45,
                b"target bytes",
                now_ms(),
            )
            .await
            .expect("publish");
        serve.shared.pool.satisfy(&rendition.key, 45);
        pause.wait().await;
        let windows = eviction_windows(&serve.shared, &rendition).await;
        let mut manifest = rendition.manifest.lock().await;
        rendition
            .dir
            .make_room(&mut manifest, &windows, u64::MAX)
            .await
            .expect("pressure sweep");
        assert!(manifest.state(45).expect("target").is_materialized());
        drop(manifest);
        pause.wait().await;
        assert_eq!(get.await.expect("GET task").expect("response file").len, 12);
        assert!(serve.shared.pool.retained(&rendition.key).is_empty());
        let windows = eviction_windows(&serve.shared, &rendition).await;
        let mut manifest = rendition.manifest.lock().await;
        rendition
            .dir
            .make_room(&mut manifest, &windows, u64::MAX)
            .await
            .expect("later sweep");
        assert!(
            !manifest.state(45).expect("target").is_materialized(),
            "opening releases the narrow pin; old destinations do not become permanent retention"
        );
    }

    #[tokio::test]
    async fn legacy_and_controlled_readers_both_receive_fair_current_demand() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("legacy", 0).await;
        rendition.attach_reader("controlled", 45).await;
        rendition
            .readers
            .lock()
            .await
            .get_mut("controlled")
            .expect("reader")
            .accept_control(1, 45);
        let old = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 3,
                },
                "legacy",
            )
            .expect("legacy admitted");
        let newer = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 45,
                },
                "controlled",
            )
            .expect("controlled admitted");
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        let position = Position {
            produced_through: None,
            positioned_at: Some(45),
            seconds_per_segment: rendition.seconds_per_segment,
            working_set: WorkingSet::default(),
        };
        assert_eq!(
            decide(&*rendition.manifest.lock().await, &demands, position, &[]),
            Action::Reposition { to: 3 }
        );
        drop(readers);
        drop(old);
        let far = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 55,
                },
                "legacy",
            )
            .expect("far legacy seek admitted");
        drop(newer);
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        assert!(
            demands
                .iter()
                .any(|demand| demand.blocked_on == Some(55) && demand.foreground),
            "a legacy seek outside the last delivered window remains a fair candidate"
        );
        drop(far);
    }

    #[test]
    fn an_accepted_new_owner_sequence_one_replaces_the_old_owner_anchor() {
        let mut reader = Reader::new(3);
        reader.accept_control(90, 45);
        reader.accept_control(1, 3);
        assert_eq!(reader.frontier, 3);
        assert_eq!(reader.control_sequence, Some(1));
    }

    #[tokio::test]
    async fn a_published_nearest_request_cannot_hide_the_next_current_get() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("viewer", 45).await;
        rendition
            .readers
            .lock()
            .await
            .get_mut("viewer")
            .expect("reader")
            .accept_control(1, 45);
        let mut waits = Vec::new();
        for index in [3, 45, 46] {
            waits.push(
                serve
                    .shared
                    .pool
                    .register(
                        WaitKey {
                            rendition: rendition.key.clone(),
                            index,
                        },
                        "viewer",
                    )
                    .expect("admitted"),
            );
        }
        let mut manifest = rendition.manifest.lock().await;
        rendition
            .dir
            .materialize(&mut manifest, 45, b"published", now_ms())
            .await
            .expect("publish before satisfy");
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
        let position = Position {
            produced_through: Some(45),
            positioned_at: Some(45),
            seconds_per_segment: rendition.seconds_per_segment,
            working_set: WorkingSet::default(),
        };
        assert_eq!(
            decide(&manifest, &demands, position, &[]),
            Action::Produce { next: 46 },
            "old request3 must not win the publication-to-satisfy interval"
        );
        drop(waits);
    }

    #[tokio::test]
    async fn a_zero_progress_eviction_does_not_self_schedule_a_hot_loop() {
        use futures_util::FutureExt;
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .working_set_budget = 1;
        rendition.attach_reader("viewer", 0).await;
        rendition
            .dir
            .materialize(
                &mut *rendition.manifest.lock().await,
                0,
                b"protected bytes",
                now_ms(),
            )
            .await
            .expect("publish");
        serve.shared.working_set.store(15, Relaxed);
        let pending = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 1,
                },
                "viewer",
            )
            .expect("admitted");
        driver_pass(&serve.shared, &rendition).await;
        assert!(rendition.wake.notified().now_or_never().is_none(), "zero-byte sweep waits for demand/capacity/maintenance change instead of kicking itself forever");
        assert_eq!(serve.shared.working_set.load(Relaxed), 15);
        #[cfg(unix)]
        {
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .expect("fake running producer");
            rendition.slot.attach(child, 0).await;
            driver_pass(&serve.shared, &rendition).await;
            assert!(
                matches!(rendition.slot.belief().await, Producer::Absent { .. }),
                "zero-progress capacity hold releases a running producer"
            );
            assert_eq!(
                rendition.gen_epoch.load(Relaxed),
                1,
                "queued producer writes are fenced"
            );
            assert!(rendition.wake.notified().now_or_never().is_none());
        }
        drop(pending);
    }

    #[tokio::test]
    async fn cancelled_and_refused_gets_leave_no_watchdog_or_reader_frontier() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .materialize_budget = Duration::from_secs(5);
        rendition.attach_reader("viewer", 3).await;
        let mut waits = Vec::new();
        for index in 10..10 + PER_SESSION_WAIT_CAP as u32 {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            waits.push(tokio::spawn(async move {
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        index,
                        Duration::from_secs(1),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            }));
        }
        wait_until("admitted requests", Duration::from_secs(1), async || {
            serve.shared.pool.len() == PER_SESSION_WAIT_CAP
        })
        .await;
        assert!(matches!(
            serve
                .serve_segment(
                    &rendition,
                    "viewer",
                    50,
                    Duration::from_secs(1),
                    Arc::new(crate::meter::Meter::new())
                )
                .await,
            // One viewer at its own ceiling, on a node with room: the refusal
            // names the per-session cap, which is the answer that tells this
            // client to slow down rather than telling it the node is out.
            Err(VodError::Busy(crate::waitpool::WaitRefused::SessionBusy))
        ));
        assert!(!rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .contains_key(&50));
        assert_eq!(rendition.readers.lock().await["viewer"].frontier, 3);
        let watchdogs = rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .values()
            .filter_map(|clock| clock.watchdog.clone())
            .collect::<Vec<_>>();
        for wait in waits {
            wait.abort();
            let _ = wait.await;
        }
        assert!(serve.shared.pool.is_empty());
        assert!(rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .is_empty());
        wait_until(
            "cancelled watchdog tasks",
            Duration::from_secs(1),
            async || watchdogs.iter().all(tokio::task::AbortHandle::is_finished),
        )
        .await;
        assert!(
            rendition.failure().is_none(),
            "orphaned deadlines cannot poison a shared rendition"
        );
        assert!(
            matches!(
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        50,
                        Duration::from_millis(1),
                        Arc::new(crate::meter::Meter::new())
                    )
                    .await,
                Err(VodError::Pending { .. })
            ),
            "a later destination receives its own production budget"
        );
    }

    #[tokio::test]
    async fn a_cancelled_episode_cannot_expire_a_later_wait_for_the_same_entry() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .materialize_budget = Duration::from_secs(5);
        let old = serve.shared.arm_materialize_watchdog(&rendition, 10);
        let old_started = old.started;
        let old_task = rendition.demand_since.lock().expect("demand lock")[&10]
            .watchdog
            .as_ref()
            .expect("watchdog")
            .clone();
        drop(old);
        wait_until(
            "old watchdog cancelled",
            Duration::from_secs(1),
            async || old_task.is_finished(),
        )
        .await;
        let new = serve.shared.arm_materialize_watchdog(&rendition, 10);
        assert!(new.started > old_started);
        let request = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 10,
                },
                "viewer",
            )
            .expect("new request admitted");
        assert!(old_task.is_finished());
        assert_eq!(
            rendition
                .demand_since
                .lock()
                .expect("demand lock")
                .get(&10)
                .expect("new episode retained")
                .started,
            new.started
        );
        assert_eq!(
            serve.shared.pool.demands(&rendition.key).len(),
            1,
            "old watchdog cannot settle the new request"
        );
        drop((request, new));
    }

    #[tokio::test]
    async fn a_manifest_recheck_cannot_restart_the_http_block_budget() {
        // This production seam uses an absolute Tokio deadline; drive the
        // clock explicitly rather than depend on wall-clock scheduling.
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        tokio::time::pause();
        let manifest = rendition.manifest.lock().await;
        let get = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .blocked_wait(
                        &rendition,
                        "viewer",
                        10,
                        Duration::from_secs(5),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(serve.shared.pool.len(), 1);
        tokio::time::advance(Duration::from_secs(6)).await;
        drop(manifest);
        let result = tokio::time::timeout(Duration::from_millis(100), get)
            .await
            .expect("expired GET does not receive a fresh five-second wait")
            .expect("GET task");
        assert!(matches!(result, Err(VodError::Pending { .. })));
    }

    #[tokio::test]
    async fn cancelled_init_request_does_not_leave_a_rendition_failure() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .materialize_budget = Duration::from_secs(5);
        let pending = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_init(
                        &rendition,
                        Duration::from_secs(1),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        wait_until("init deadline owner", Duration::from_secs(1), async || {
            rendition
                .demand_since
                .lock()
                .expect("demand lock")
                .contains_key(&INIT_DEMAND_INDEX)
        })
        .await;
        let watchdog = rendition.demand_since.lock().expect("demand lock")[&INIT_DEMAND_INDEX]
            .watchdog
            .as_ref()
            .expect("init watchdog")
            .clone();
        pending.abort();
        let _ = pending.await;
        assert!(rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .is_empty());
        wait_until(
            "cancelled init watchdog",
            Duration::from_secs(1),
            async || watchdog.is_finished(),
        )
        .await;
        assert!(rendition.failure().is_none());
    }

    #[tokio::test]
    async fn materialize_watchdog_spans_http_retries_and_fails_typed() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared rendition")
            .materialize_budget = Duration::from_millis(25);
        rendition.attach_reader("sess-a", 0).await;
        let touched = Instant::now() - Duration::from_secs(5);
        let rendition_key = rendition.key.clone();
        let file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition: Some(rendition),
                rendition_key,
                file,
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_millis(1),
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(touched),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );

        assert!(matches!(
            serve.segment("sess-a", "seg00001.m4s").await,
            Some(VodPublication {
                result: Err(VodError::Pending { .. }),
                ..
            })
        ));
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "a pending response does not prove playback demand",
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        match serve.segment("sess-a", "seg00001.m4s").await {
            Some(VodPublication {
                result: Err(VodError::ProducerFailed(cause)),
                ..
            }) => {
                assert!(cause.contains("producer deadline"), "{cause}");
            }
            other => panic!("watchdog must settle retries typed: {}", describe(other)),
        }
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "a producer failure does not renew the attachment",
        );
    }

    /// Pure bookkeeping, faked materialization on purpose: the property under
    /// test is the driver's window construction feeding `make_room`, not the
    /// generation path (the 12 s fixture's plan is too small to leave
    /// anything outside a 180 s ahead window).
    #[tokio::test]
    async fn make_room_never_evicts_inside_a_live_readers_window() {
        let index = synthetic_index(240);
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        );
        let mut manifest = Manifest::new(plan.clone());
        let temp = crate::test_tempdir().expect("dir");
        let dir = RenditionDir::new(temp.path().join("r"));
        dir.create().await.expect("create");
        let count = manifest.len() as u32;
        assert!(count >= 40, "the synthetic plan must be big: {count}");
        for segment in 0..count {
            dir.materialize(&mut manifest, segment, b"0123456789", i64::from(segment))
                .await
                .expect("materialize");
        }

        // The reader just fetched segment 5 — exactly the window the driver
        // would build for it.
        let seconds_per_segment = plan.duration_ticks() as f64 / 16_000.0 / plan.len() as f64;
        let mut reader = Reader::new(6);
        reader.last_served = Some(5);
        let window = reader_window(&reader, seconds_per_segment);
        let freed = dir
            .make_room(&mut manifest, &[window], u64::MAX)
            .await
            .expect("make_room");
        assert!(freed.is_complete());
        assert!(freed.bytes > 0, "something outside the window was evicted");
        for segment in 0..count {
            let materialized = manifest.state(segment).expect("planned").is_materialized();
            if window.covers(segment) {
                assert!(
                    materialized,
                    "segment {segment} is inside the live reader's window"
                );
                assert!(dir.path().join(segment_name(u64::from(segment))).exists());
            }
        }
        assert!(
            manifest.state(5).expect("planned").is_materialized(),
            "the segment the reader just fetched must survive"
        );

        // A forward seek's blocked GET moves the frontier past the playhead;
        // the window's high edge must follow it, or `make_room` evicts the
        // just-materialized seek target before the waiter opens it.
        let mut seeked_reader = Reader::new(30);
        seeked_reader.last_served = Some(5);
        let seeked = reader_window(&seeked_reader, seconds_per_segment);
        assert!(
            seeked.covers(30),
            "the blocked seek target sits inside its own reader's window"
        );
    }

    /// Fix 1's derivation, as a unit: `video_ms` from the fragment index,
    /// `audio_ms` from the container — the split that makes the audio tail
    /// plannable at all. A container-for-both derivation makes the tail
    /// identically zero on every file.
    #[test]
    fn the_plan_derives_video_from_the_index_and_audio_from_the_container() {
        let index = synthetic_index(24);
        let recipe = Recipe {
            file: media_file_at(PathBuf::from("unused.mkv"), 0),
            audio_index: None,
            aac: true,
            video: CopyVideoOptions::new(false, false),
            source_object_version: None,
            cluster_cache_key: None,
        };
        let video_ms = index_video_ms(&index);
        assert!(video_ms > 0);

        // Audio outruns video by three seconds: the tail must be planned.
        let tracks = track_durations(&index, &recipe, video_ms + 3_000);
        assert_eq!(tracks.video_ms, video_ms, "video honestly from the index");
        assert_eq!(
            tracks.audio_ms,
            video_ms + 3_000,
            "audio from the container"
        );
        let plan = plurx_core::segplan::plan_copy(&index, &shipped_policy(16_000), &tracks);
        let last = plan.entries.last().expect("a planned entry");
        assert_eq!(
            last.kind,
            PlanEntryKind::AudioTail,
            "a 3 s overrun plans an audio tail"
        );

        // Tracks of equal length plan no tail.
        let flat = track_durations(&index, &recipe, video_ms);
        let plan = plurx_core::segplan::plan_copy(&index, &shipped_policy(16_000), &flat);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.kind == PlanEntryKind::Video),
            "no overrun, no tail"
        );
    }

    /// Fix 3, end to end on a real audiotail-shaped source: a seek into the
    /// audio tail positions the session (and every spawn) at the last VIDEO
    /// entry, and a blocked GET on the tail index materializes through that
    /// generation's own `finish` — never through a generation started inside
    /// the tail, which vodgen categorically refuses.
    #[tokio::test]
    async fn a_seek_into_the_audio_tail_spawns_at_the_last_video_entry_and_serves_the_tail() {
        testfixtures::require_ffmpeg();
        let temp = crate::test_tempdir().expect("temp");
        // Video 9 s, audio 12 s, no -shortest: an honest 3 s audio tail —
        // the c58a4307 shape the plan's tail entries exist for.
        let source = temp.path().join("audiotail.mkv");
        let mut cmd = std::process::Command::new(testfixtures::ffmpeg());
        cmd.args(["-y", "-v", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=24:duration=9",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=12",
            ])
            .args(["-c:v", "libx265", "-preset", "ultrafast"])
            .args([
                "-x265-params",
                "keyint=42:min-keyint=42:open-gop=1:bframes=0:scenecut=0:\
                 repeat-headers=1:log-level=none",
            ])
            .args([
                "-c:a", "aac", "-pix_fmt", "yuv420p", "-ac", "2", "-f", "matroska",
            ])
            .arg(&source);
        testfixtures::run(&mut cmd);
        // The index is built against the VIDEO duration (the video-only index
        // pipe can never cover the audio's extra three seconds — its coverage
        // check reads the duration it is handed); the CREATE carries the
        // container duration, which is what the probe reports and what the
        // audio tail is derived from.
        let (store, _) = store_with_index(&media_file_at(source.clone(), 9_000)).await;
        let file = media_file_at(source, 12_000);
        let serve = VodServe::new(temp.path().join("renditions"), store);

        // Seek into the tail (video ends ~9 s).
        serve
            .try_create(
                &request("play-a", 10.5),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-a".to_string(),
            )
            .await
            .expect("the audiotail source is VOD-presentable");
        let rendition = rendition_of(&serve, "sess-a").await;
        let tail = rendition
            .plan
            .entries
            .iter()
            .find(|entry| entry.kind == PlanEntryKind::AudioTail)
            .expect("the plan must carry an audio tail")
            .index;
        // The clamp itself: a start inside the tail is a video position.
        let positioned = entry_containing(&rendition.plan, 10.5);
        assert!(
            rendition.plan.entry(positioned).expect("entry").kind == PlanEntryKind::Video,
            "a seek into the tail positions at a video entry, got {positioned}"
        );
        assert!(positioned < tail);

        // The blocked GET on the tail index itself materializes and serves.
        let ready = fetch(&serve, "sess-a", &segment_name(u64::from(tail))).await;
        assert!(ready.len > 0);
        assert!(
            rendition.failure().is_none(),
            "the tail must be produced, not refused as an in-tail spawn"
        );
    }

    /// Fix 2: a resurrected rendition whose `init.mp4` is gone but whose
    /// manifest adopted every segment has no gap for the scheduler to fill —
    /// the head regeneration at attach is what puts the init back.
    #[tokio::test]
    async fn a_resurrection_missing_only_its_init_regenerates_the_head_and_keeps_the_segments() {
        let base = crate::test_tempdir().expect("base");
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        let first = VodServe::new(base.path().to_path_buf(), Arc::clone(&store));
        create(&first, &file, "sess-a", "play-a", &settings()).await;
        let len = plan_len(&first, "sess-a").await;
        for segment in 0..len {
            fetch(&first, "sess-a", &segment_name(segment as u64)).await;
        }
        let rendition = rendition_of(&first, "sess-a").await;
        wait_until("the first producer ended", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;
        let dir_path = rendition.dir.path().to_path_buf();
        drop(first);
        tokio::fs::remove_file(dir_path.join(INIT_NAME))
            .await
            .expect("take the init away");

        let second = VodServe::new(base.path().to_path_buf(), store);
        create(&second, &file, "sess-b", "play-b", &settings()).await;
        let adopted = rendition_of(&second, "sess-b").await;
        assert_eq!(
            adopted.manifest.lock().await.materialized_count(),
            len,
            "a verified head keeps every adopted segment"
        );
        assert!(
            adopted.dir.has_init().await,
            "the head regeneration re-derived init.mp4 at attach"
        );
        // And the init is servable without any producer having spawned.
        match second
            .segment("sess-b", INIT_NAME)
            .await
            .expect("ours")
            .result
        {
            Ok(Some(ready)) => assert!(ready.len > 0),
            other => panic!("init.mp4 must serve: {other:?}"),
        }
        assert!(
            matches!(adopted.slot.belief().await, Producer::Absent { .. }),
            "no generation was needed beyond the head"
        );
    }

    /// Fix 2's mismatch arm: an adopted identity the pipeline cannot
    /// reproduce purges to planned-only at attach and establishes fresh on
    /// the next real generation.
    #[tokio::test]
    async fn a_resurrection_whose_identity_cannot_be_verified_purges_and_reproduces() {
        let base = crate::test_tempdir().expect("base");
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        let first = VodServe::new(base.path().to_path_buf(), Arc::clone(&store));
        create(&first, &file, "sess-a", "play-a", &settings()).await;
        fetch(&first, "sess-a", "seg00000.m4s").await;
        let rendition = rendition_of(&first, "sess-a").await;
        let dir_path = rendition.dir.path().to_path_buf();
        wait_until("the first producer ended", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;
        drop(first);
        // A stored identity this pipeline never produced, and no init to
        // trust: the head regeneration must refuse and purge.
        let bogus = InitIdentity {
            muxer_init: "not-a-real-digest".to_string(),
            served_init: "not-a-real-digest-either".to_string(),
            promotion: plurx_core::fmp4::PromotionInputs::default(),
        };
        store_identity(&dir_path.join(IDENTITY_NAME), &bogus)
            .await
            .expect("plant the bogus identity");
        tokio::fs::remove_file(dir_path.join(INIT_NAME))
            .await
            .expect("take the init away");

        let second = VodServe::new(base.path().to_path_buf(), store);
        create(&second, &file, "sess-b", "play-b", &settings()).await;
        let adopted = rendition_of(&second, "sess-b").await;
        assert_eq!(
            adopted.manifest.lock().await.materialized_count(),
            0,
            "an unverifiable adoption is purged to planned-only"
        );
        // And the rendition is healthy: a fresh generation establishes a new
        // identity and serves.
        let ready = fetch(&second, "sess-b", "seg00000.m4s").await;
        assert!(ready.len > 0);
    }

    /// Fix 4: a stale generation's queued materialize — landing after the
    /// driver restarted the producer — is refused under the manifest lock and
    /// touches neither the manifest nor the counters.
    #[tokio::test]
    async fn a_stale_generations_write_is_refused() {
        use crate::vodgen::Sink;
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        let sink = RenditionSink {
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: rendition.gen_epoch.load(Relaxed),
        };
        // The driver kills and replaces the generation this sink belongs to.
        rendition.gen_epoch.fetch_add(1, Relaxed);
        let error = sink
            .materialize(0, b"stale bytes".to_vec())
            .await
            .expect_err("a stale epoch must be refused");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            !rendition
                .manifest
                .lock()
                .await
                .state(0)
                .expect("planned")
                .is_materialized(),
            "a refused write must not touch the manifest"
        );
        assert_eq!(serve.shared.working_set.load(Relaxed), 0);

        // The replacement's own sink — current epoch — writes normally.
        let fresh = RenditionSink {
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: rendition.gen_epoch.load(Relaxed),
        };
        fresh
            .materialize(0, b"fresh bytes".to_vec())
            .await
            .expect("the live generation writes");
        assert!(rendition
            .manifest
            .lock()
            .await
            .state(0)
            .expect("planned")
            .is_materialized());
    }

    /// Fix 5: replacing a failed rendition subtracts what its manifest still
    /// claimed, so the rebuild's adoption counts the same bytes exactly once.
    #[tokio::test]
    async fn replacing_a_failed_rendition_keeps_the_working_set_honest() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;
        // Freeze the producer before it materializes anything, so the fake
        // member below is the rendition's whole claim.
        wait_until("the producer spawned", Duration::from_secs(10), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.last_child_pid.load(Relaxed) != 0 }
        })
        .await;
        let pid = rendition.last_child_pid.load(Relaxed);
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);
        assert_eq!(
            rendition.manifest.lock().await.materialized_count(),
            0,
            "frozen before its first segment"
        );
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"0123456789", now_ms())
                .await
                .expect("inject a member");
        }
        serve.shared.working_set.fetch_add(10, Relaxed);
        assert_eq!(serve.shared.working_set.load(Relaxed), 10);
        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "boom".to_string(),
        );
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);

        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        let renewed = rendition_of(&serve, "sess-b").await;
        assert!(
            !Arc::ptr_eq(&rendition, &renewed),
            "a failed rendition is replaced, not reattached"
        );
        let claimed = renewed.manifest.lock().await.materialized_bytes();
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            claimed,
            "the stale rendition's bytes are subtracted exactly once"
        );
    }

    /// Fix 6: the purge re-checks dormancy while owning the exact build key,
    /// so a create that attached between maintain's scan and the purge
    /// committing keeps its rendition.
    #[tokio::test]
    async fn a_purge_never_takes_a_rendition_a_create_just_attached() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;
        let key = rendition.key.clone();

        // The interleave: maintain's scan saw the rendition dormant (simulate
        // by planting a dormant mark), but a session attached before the
        // purge could commit. The re-check must skip it.
        *rendition.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
        tokio::time::sleep(Duration::from_millis(5)).await;
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(
            serve.shared.renditions.lock().await.contains_key(&key),
            "a rendition with a live reader survives the purge"
        );
        assert!(!rendition.closed.load(Relaxed));
        assert!(
            serve.playlist("sess-a").await.expect("ours").is_ok(),
            "the attached session still serves"
        );

        // Genuinely dormant — no readers, past TTL — the purge commits.
        assert!(serve.end("sess-a", Terminal::Deleted).await);
        tokio::time::sleep(Duration::from_millis(5)).await;
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(
            !serve.shared.renditions.lock().await.contains_key(&key),
            "a dormant rendition purges"
        );
        assert!(rendition.closed.load(Relaxed));
    }

    /// Fix 8: a satisfy that fired between the materialized-miss and the wait
    /// registration woke nobody — the post-registration re-check serves the
    /// bytes instead of sleeping a whole budget on them.
    #[tokio::test]
    async fn a_satisfy_that_raced_registration_is_not_a_lost_wakeup() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        // The raced state: the bytes landed (their satisfy hit an empty
        // pool), and nothing will ever satisfy this index again.
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 5, b"already here", now_ms())
                .await
                .expect("materialize");
        }
        let served = tokio::time::timeout(
            Duration::from_secs(5),
            serve.blocked_wait(
                &rendition,
                "sess-x",
                5,
                Duration::from_secs(60),
                Arc::new(crate::meter::Meter::new()),
            ),
        )
        .await
        .expect("the re-check must answer without sleeping the budget")
        .expect("the materialized segment serves");
        assert!(served.len > 0);

        // Same window, failure flavor: a failure recorded in the gap answers
        // typed instead of sleeping.
        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "boom".to_string(),
        );
        match tokio::time::timeout(
            Duration::from_secs(5),
            serve.blocked_wait(
                &rendition,
                "sess-x",
                6,
                Duration::from_secs(60),
                Arc::new(crate::meter::Meter::new()),
            ),
        )
        .await
        .expect("the re-check must answer without sleeping the budget")
        {
            Err(VodError::ProducerFailed(cause)) => assert_eq!(cause, "boom"),
            other => panic!("expected ProducerFailed, got {other:?}"),
        }
    }

    /// Fix 9: both wake paths for a blocked init GET — the init landing, and
    /// a producer failure — answer promptly instead of sleeping the budget.
    #[tokio::test]
    async fn an_init_write_wakes_a_blocked_init_get() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        let waiter = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_init(
                        &rendition,
                        Duration::from_secs(30),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        rendition.dir.write_init(b"moov").await.expect("init");
        rendition.init_notify.notify_waiters();
        let ready = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the write must wake the waiter")
            .expect("waiter task")
            .expect("the init serves");
        assert!(ready.etag.contains("-init-"));
    }

    #[tokio::test]
    async fn a_failure_wakes_a_blocked_init_get() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        let waiter = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_init(
                        &rendition,
                        Duration::from_secs(30),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "boom".to_string(),
        );
        match tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the failure must wake the waiter")
            .expect("waiter task")
        {
            Err(VodError::ProducerFailed(cause)) => assert_eq!(cause, "boom"),
            other => panic!("expected ProducerFailed, got {other:?}"),
        }
    }

    /// Fix 10: the etag folds the materialization instant in, so an
    /// evict-and-regenerate whose length happens to match still changes it.
    #[tokio::test]
    async fn the_etag_changes_across_an_evict_and_regenerate() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"first bytes!", 1_000)
                .await
                .expect("materialize");
        }
        let first = serve
            .open_materialized(&rendition, 0, &Arc::new(crate::meter::Meter::new()))
            .await
            .expect("open")
            .expect("materialized");
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"other bytes!", 2_000)
                .await
                .expect("re-materialize");
        }
        let second = serve
            .open_materialized(&rendition, 0, &Arc::new(crate::meter::Meter::new()))
            .await
            .expect("open")
            .expect("materialized");
        assert_eq!(first.len, second.len, "the collision the instant breaks");
        assert_ne!(
            first.etag, second.etag,
            "same key, index and length must still not collide across a \
             regeneration"
        );
    }

    /// The three surfaces the integrator wires this round: the lease loop's
    /// live ids and frontier, and the stall-reopen's predecessor facts.
    #[tokio::test]
    async fn lease_and_reopen_surfaces_report_live_sessions_only() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        assert_eq!(serve.live_session_ids().await, vec!["sess-a".to_string()]);
        assert_eq!(
            serve.frontier_ms("sess-a").await,
            Some(0),
            "0 before the first served segment"
        );
        let facts = serve.reopen_facts("sess-a").await.expect("live facts");
        assert_eq!(facts.playback_id, "play-a");
        assert_eq!(facts.supersession_user, "[\"user_id\",1]");
        assert_eq!(facts.file_id, file.id);

        fetch(&serve, "sess-a", "seg00000.m4s").await;
        let rendition = rendition_of(&serve, "sess-a").await;
        let expected = ticks_to_ms(
            rendition.plan.entry(0).expect("entry 0").end_ticks(),
            rendition.timescale,
        );
        assert_eq!(
            serve.frontier_ms("sess-a").await,
            Some(expected),
            "the film-time end of the last served segment"
        );

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(serve.live_session_ids().await.is_empty());
        assert_eq!(serve.frontier_ms("sess-a").await, None);
        assert!(serve.reopen_facts("sess-a").await.is_none());
        assert!(serve.owns("sess-a").await, "tombstoned is still addressed");
        assert!(serve.frontier_ms("sess-x").await.is_none());
    }
}
