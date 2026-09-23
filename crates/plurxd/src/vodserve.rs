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

#[derive(Clone)]
struct ObsoleteEncodedGeneration {
    key: String,
    path: PathBuf,
}

struct EncodedGenerationScanner {
    base: PathBuf,
    entries: Option<std::fs::ReadDir>,
}

impl EncodedGenerationScanner {
    fn new(base: PathBuf) -> Self {
        Self {
            base,
            entries: None,
        }
    }

    fn discover(
        &mut self,
        current_process: &str,
        scan_limit: usize,
        candidate_limit: usize,
        excluded: &HashSet<String>,
    ) -> Vec<ObsoleteEncodedGeneration> {
        if scan_limit == 0 || candidate_limit == 0 {
            return Vec::new();
        }
        if self.entries.is_none() {
            self.entries = match std::fs::read_dir(&self.base) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
                Err(error) => {
                    tracing::warn!(path = %self.base.display(), %error, "cannot discover obsolete encoded VOD generations");
                    return Vec::new();
                }
            };
        }

        let mut examined = 0;
        let mut candidates = Vec::new();
        while examined < scan_limit && candidates.len() < candidate_limit {
            let next = self.entries.as_mut().and_then(Iterator::next);
            let entry = match next {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => {
                    examined += 1;
                    tracing::warn!(%error, "cannot inspect a VOD rendition directory entry");
                    continue;
                }
                None => {
                    // The following tick starts a fresh pass, which discovers
                    // encoded directories created after this iterator opened.
                    self.entries = None;
                    break;
                }
            };
            examined += 1;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let marker = entry.path().join(ENCODED_PROCESS_NAME);
            let Ok(owner) = std::fs::read_to_string(&marker) else {
                // No marker means copy rendition (or unrelated directory). It
                // is never safe to infer encoded ownership from a hashed name.
                continue;
            };
            if owner.trim() == current_process {
                continue;
            }
            let key = entry.file_name().to_string_lossy().into_owned();
            if excluded.contains(&key) {
                continue;
            }
            candidates.push(ObsoleteEncodedGeneration {
                key,
                path: entry.path(),
            });
        }
        candidates
    }
}

async fn publish_encoded_process_marker(path: &Path, process: &str) -> io::Result<()> {
    let marker = path.join(ENCODED_PROCESS_NAME);
    let temporary = path.join(format!("{ENCODED_PROCESS_NAME}.tmp"));
    tokio::fs::write(&temporary, format!("{process}\n")).await?;
    tokio::fs::rename(temporary, marker).await
}

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
    /// A frozen encoded strategy; None is the indexed compressed-copy path.
    encoding: Option<Arc<crate::vodencode::Encoding>>,
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
    index: Option<FragmentIndex>,
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
    /// Survives a yielded process so it cannot re-admit one segment after
    /// giving its permit to a live waiter.
    ahead_hold: AtomicBool,
    /// Woken when `init.mp4` lands, for GETs waiting on the identity.
    init_notify: Notify,
    /// The driver's kick: wait registration, segment GETs, attach/detach,
    /// maintain ticks.
    wake: Notify,
    /// Deterministic observation point for tests that must prove the driver's
    /// stopped-encoder poll, rather than a direct `driver_pass`, caused work.
    #[cfg(test)]
    stopped_poll_armed: Notify,
    #[cfg(test)]
    stopped_poll_fired: Notify,
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
    child_job: Option<crate::process_control::ChildJob>,
    permit: Option<crate::vodencode::EncodePermit>,
    #[cfg(test)]
    reap_pause: Option<Arc<tokio::sync::Barrier>>,
}

impl HeadChildOwner {
    fn new(child: tokio::process::Child) -> Self {
        Self {
            child: Some(child),
            child_job: None,
            permit: None,
            #[cfg(test)]
            reap_pause: None,
        }
    }

    fn new_job_owned(
        child: tokio::process::Child,
        child_job: crate::process_control::ChildJob,
    ) -> Self {
        Self {
            child: Some(child),
            child_job: Some(child_job),
            permit: None,
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
            child_job: None,
            permit: None,
            reap_pause: Some(reap_pause),
        }
    }

    fn begin_reap(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let mut child = self.child.take()?;
        let child_job = self.child_job.take();
        let permit = self.permit.take();
        let _ = child.start_kill();
        #[cfg(test)]
        let reap_pause = self.reap_pause.take();
        Some(tokio::spawn(async move {
            let _child_job = child_job;
            let _permit = permit;
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

impl From<tokio::process::Child> for HeadChildOwner {
    fn from(child: tokio::process::Child) -> Self {
        Self::new(child)
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
    /// `transcode::Session::delivery` already is for rolling delivery. The
    /// measurement remains useful for fleet advice and incident diagnosis,
    /// while missing or low values no longer veto prepared handoff.
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
        desired_digest: Option<String>,
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
                    desired_digest,
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

impl VodServe {
    /// Convert an attached prepared VOD rendition to ordinary foreground
    /// admission after its durable pointer commit. A copy rendition has no
    /// encoder and therefore needs no transition.
    pub(crate) async fn promote_prepared_session(&self, session_id: &str) -> bool {
        let rendition = self
            .shared
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|session| session.rendition.clone());
        let Some(rendition) = rendition else {
            return false;
        };
        if let Some(encoding) = rendition.recipe.encoding.as_ref() {
            encoding.promote();
        }
        true
    }

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
        let runtime_cache = base
            .parent()
            .unwrap_or(base.as_path())
            .join(".runtime-cache");
        Self::new_configured(base, runtime_cache, store, None, None, None)
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
                encoding: None,
            },
            source: None,
            playlist: plan.playlist().into_bytes(),
            timescale: plan.timescale,
            seconds_per_segment: plan.duration_ticks() as f64
                / f64::from(plan.timescale)
                / plan.len() as f64,
            index: Some(index),
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
            ahead_hold: AtomicBool::new(false),
            init_notify: Notify::new(),
            wake: Notify::new(),
            #[cfg(test)]
            stopped_poll_armed: Notify::new(),
            #[cfg(test)]
            stopped_poll_fired: Notify::new(),
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new_cluster(
        base: PathBuf,
        store: Arc<dyn Store>,
        node_id: String,
        cluster_index_root: PathBuf,
        membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        let runtime_cache = cluster_index_root
            .parent()
            .unwrap_or(cluster_index_root.as_path())
            .to_owned();
        Self::new_cluster_with_runtime(
            base,
            runtime_cache,
            store,
            node_id,
            cluster_index_root,
            membership,
        )
    }

    pub(crate) fn new_cluster_with_runtime(
        base: PathBuf,
        runtime_cache: PathBuf,
        store: Arc<dyn Store>,
        node_id: String,
        cluster_index_root: PathBuf,
        membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        Self::new_configured(
            base,
            runtime_cache,
            store,
            Some(node_id),
            Some(cluster_index_root),
            membership,
        )
    }

    fn new_configured(
        base: PathBuf,
        runtime_cache: PathBuf,
        store: Arc<dyn Store>,
        cluster_node_id: Option<String>,
        cluster_index_root: Option<PathBuf>,
        cluster_membership: Option<plurx_core::cluster::membership::MembershipManager>,
    ) -> Arc<VodServe> {
        let mut encoded_generation_scanner = EncodedGenerationScanner::new(base.clone());
        let obsolete_encoded_generations = encoded_generation_scanner.discover(
            crate::ffmpeg::encoded_process_identity(),
            ENCODED_RECONCILE_BATCH,
            ENCODED_RECONCILE_BATCH,
            &HashSet::new(),
        );
        Arc::new(VodServe {
            shared: Arc::new(Shared {
                base,
                runtime_cache,
                store,
                obsolete_encoded_generations: Mutex::new(VecDeque::from(
                    obsolete_encoded_generations,
                )),
                encoded_generation_scanner: Arc::new(StdMutex::new(encoded_generation_scanner)),
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

    /// None means this node has no source attestation yet. The caller may
    /// still use a local index; only a complete index miss queues analysis.
    async fn try_cluster_fragment_index(
        &self,
        file: &MediaFile,
        video: CopyVideoOptions,
    ) -> Result<Option<(FragmentIndex, String, String)>, String> {
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
        let Some(observation) = self
            .shared
            .store
            .fragment_index_source(node_id, file.id, &object_version)
            .await
            .map_err(|error| format!("reading source attestation: {error}"))?
        else {
            return Ok(None);
        };
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
            // Lost work: without this transition the exact artifact remains
            // permanently complete even though no verified holder can serve it.
            crate::store_result::observe(
                crate::store_result::Operation::RequeueFragmentIndexNoHolder,
                crate::store_result::Discard::LostWork,
                plurx_core::store::requeue_cluster_fragment_index_after_no_holder(
                    self.shared.store.as_ref(),
                    &repair,
                )
                .await,
            );
            return Err("no verified holder could supply the v2 artifact".to_owned());
        };
        Ok(Some((index, object_version, artifact.cache_key)))
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
    pub(crate) async fn try_create<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req.into(),
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
    pub(crate) async fn try_create_cluster<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        serving_admission: VodServingAdmission,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req.into(),
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
    pub(crate) async fn try_create_before_release<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        release_fence: VodReleaseFence<'_>,
    ) -> Result<VodStart, String> {
        let _preparing = self.begin_preparing_session(&session_id);
        self.try_create_with_release_fence(
            req.into(),
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
        prepared: VodRecipeRequest<'_>,
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
        let req = prepared.request;
        let (aac, preserve_dolby_vision, convert_dolby_vision) = match req.kind {
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
            } if prepared.encoding.is_none() => (aac, preserve_dolby_vision, convert_dolby_vision),
            _ if prepared.encoding.is_some() => (true, false, false),
            _ => {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_recipe_unresolved",
                    "the encoded VOD request has no resolved encoder recipe",
                ))
            }
        };
        if req.subtitle_burn.is_some() && prepared.encoding.is_none() {
            return Err(crate::transcode::vod_refusal_error(
                "vod_recipe_unresolved",
                "a subtitle burn requires a resolved encoded VOD recipe",
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
        let identity = match prepared.encoding.as_ref() {
            Some(encoding) => encoding.identity(file, duration_ms as f64 / 1_000.0),
            None => crate::fragindex::identity_for(file, video),
        };
        let cluster_cache_enabled = prepared.encoding.is_none()
            && self
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
        let cluster_index = if prepared.encoding.is_some() {
            Ok(None)
        } else if cluster_cache_enabled {
            self.try_cluster_fragment_index(file, video).await
        } else {
            Ok(None)
        };
        let needs_attestation = cluster_cache_enabled && matches!(&cluster_index, Ok(None));
        let unavailable_reason = cluster_index.as_ref().err().cloned();
        let (index, source_object_version, cluster_cache_key) = match cluster_index {
            Ok(Some((index, object_version, cache_key))) => {
                (Some(index), Some(object_version), Some(cache_key))
            }
            Ok(None) if prepared.encoding.is_some() => (
                None,
                prepared
                    .encoding
                    .as_ref()
                    .map(|encoding| encoding.source_object_version.clone()),
                None,
            ),
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
        if index.is_none() && prepared.encoding.is_none() {
            let reason = if needs_attestation {
                match self.shared.cluster_node_id.as_deref() {
                    Some(node_id) => match crate::state::enqueue_copy_preparation(
                        self.shared.store.as_ref(),
                        node_id,
                        file,
                        video,
                    )
                    .await
                    {
                        Ok(request) => format!(
                            "exact copy preparation is {}{}",
                            request.state,
                            if request.last_error_code.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", request.last_error_code)
                            }
                        ),
                        Err(error) => {
                            format!("exact copy preparation could not be queued: {error}")
                        }
                    },
                    None => "this process has no cluster index identity".to_owned(),
                }
            } else {
                unavailable_reason.unwrap_or_else(|| {
                    "shared preparation is disabled; no matching local index exists".to_owned()
                })
            };
            // This is a prerequisite refusal, not a claim that a worker is
            // active. The durable analysis row and reason carry its actual
            // queued/running/failed state; the caller keeps rolling first play.
            return Err(crate::transcode::vod_refusal_error(
                "vod_index_pending",
                reason,
            ));
        }
        // The §2 ruling: a single immutable init cannot describe a film whose
        // clean fragments carry varying parameter sets, so the verdict is a
        // scan-time fallback here, never a producer_failed mid-playback.
        if index
            .as_ref()
            .is_some_and(|index| !index.parameter_sets_constant)
        {
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
            encoding: prepared.encoding,
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
            target_height: rendition
                .recipe
                .encoding
                .as_ref()
                .map_or(file.height.unwrap_or(0), |encoding| {
                    encoding.options.target_height
                }),
            kind: rendition
                .recipe
                .encoding
                .as_ref()
                .map_or(req.kind, |encoding| SessionKind::Transcode {
                    height: encoding.options.target_height,
                }),
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
                    method: match &session.kind {
                        SessionKind::Copy { .. } => crate::delivery::Method::HlsCopy,
                        SessionKind::Transcode { .. } => crate::delivery::Method::Transcode,
                    },
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
        self.segment_before(session_id, name, None).await
    }

    /// Resolve a segment without allowing its blocked-GET allowance to run
    /// past a caller's response-publication deadline.
    pub(crate) async fn segment_before(
        &self,
        session_id: &str,
        name: &str,
        deadline: Option<Instant>,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        let publication = self.session_rendition(session_id).await?;
        let (rendition, mut budget, delivery) = match publication.result {
            Ok(found) => found,
            Err(error) => {
                return Some(VodPublication {
                    result: Err(error),
                    owner: publication.owner,
                })
            }
        };
        if let Some(deadline) = deadline {
            budget = budget.min(deadline.saturating_duration_since(Instant::now()));
        }
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

    /// Record that the durable row now names this ask, if this engine owns
    /// the session. `false` means it does not, and the caller should ask the
    /// other one.
    pub(crate) async fn record_desired_persisted(&self, session_id: &str, digest: &str) -> bool {
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session
            .control
            .lock()
            .expect("control lock")
            .record_desired_persisted(digest);
        true
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
        let (rendition, target_height, owner, delivery, control_snapshot) = {
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
                session.last_control_snapshot.clone(),
            )
        };
        let init_present = rendition.dir.has_init().await;
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
            server_ready_state,
            server_ready_anchor_ms,
            server_ready_end_ms,
            server_ready_seconds,
            server_next_ready_start_ms,
            server_next_ready_end_ms,
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
            let ready_anchor = control_snapshot
                .as_ref()
                .map(crate::playback_control::PlaybackDemandSnapshot::buffer_anchor_ms);
            let anchor_index = ready_anchor.and_then(|anchor| {
                let index = entry_containing(&rendition.plan, anchor as f64 / 1_000.0);
                let entry = rendition.plan.entry(index)?;
                let start_ms = ticks_to_ms(entry.start_ticks, rendition.timescale);
                let end_ms = ticks_to_ms(entry.end_ticks(), rendition.timescale);
                (start_ms <= anchor && anchor < end_ms).then_some(index)
            });
            let anchored_end = anchor_index
                .filter(|index| {
                    init_present
                        && manifest
                            .state(*index)
                            .is_some_and(SegState::is_materialized)
                })
                .and_then(end_of_contiguous_run);
            let ready_state = if ready_anchor.is_none() {
                "unavailable"
            } else if anchored_end.is_some() {
                "ready"
            } else {
                "missing"
            };
            let later_floor = anchored_end.or(ready_anchor);
            let next_index = init_present
                .then(|| {
                    (0..manifest.len() as u32).find(|index| {
                        let Some(entry) = rendition.plan.entry(*index) else {
                            return false;
                        };
                        let start_ms = ticks_to_ms(entry.start_ticks, rendition.timescale);
                        manifest
                            .state(*index)
                            .is_some_and(SegState::is_materialized)
                            && later_floor.is_some_and(|floor| start_ms > floor)
                    })
                })
                .flatten();
            let next_interval = next_index.and_then(|index| {
                let start = rendition.plan.entry(index)?;
                Some((
                    ticks_to_ms(start.start_ticks, rendition.timescale),
                    end_of_contiguous_run(index)?,
                ))
            });
            (
                manifest.materialized_count(),
                manifest.len(),
                manifest.materialized_bytes(),
                manifest.planned_bytes(),
                manifest.is_admitted(),
                published,
                ready_ahead,
                ready_state,
                ready_anchor,
                anchored_end
                    .or_else(|| (ready_state == "missing").then_some(ready_anchor).flatten()),
                anchored_end
                    .zip(ready_anchor)
                    .map(|(end, anchor)| end.saturating_sub(anchor) as f64 / 1_000.0)
                    .or_else(|| (ready_state == "missing").then_some(0.0)),
                next_interval.map(|(start, _)| start),
                next_interval.map(|(_, end)| end),
            )
        };
        let wait = self.shared.pool.session_snapshot(session_id);
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
        let control_demand = control_snapshot
            .as_ref()
            .map(|snapshot| match snapshot.demand {
                crate::playback_control::PlaybackDemand::Active => "active",
                crate::playback_control::PlaybackDemand::Hold => "hold",
                crate::playback_control::PlaybackDemand::End => "end",
            });
        let render_state = control_snapshot
            .as_ref()
            .map(|snapshot| match snapshot.render_state {
                crate::playback_control::RenderState::Starting => "starting",
                crate::playback_control::RenderState::Rendering => "rendering",
                crate::playback_control::RenderState::Waiting => "waiting",
                crate::playback_control::RenderState::Stalled => "stalled",
                crate::playback_control::RenderState::Seeking => "seeking",
                crate::playback_control::RenderState::Ended => "ended",
                crate::playback_control::RenderState::Failed => "failed",
            });
        Some(VodPublication {
            result: Ok(VodSessionInfo {
                id: session_id.to_owned(),
                file_id: rendition.recipe.file.id,
                target_height,
                encoder: "vod",
                tone_map_peak_nits: rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .filter(|encoding| {
                        encoding.plan.options().tone_map == plurx_core::transcode::ToneMap::Zscale
                    })
                    .map(|encoding| encoding.plan.options().tone_map_peak_nits),
                tone_map_peak_source: rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .filter(|encoding| {
                        encoding.plan.options().tone_map == plurx_core::transcode::ToneMap::Zscale
                    })
                    .map(|encoding| encoding.plan.options().tone_map_peak_source.name()),
                playlist_shape: "vod",
                producer_state,
                producer_hold,
                producer_failed: failed.as_ref().map(|f| f.cause.clone()),
                producer_decision: failed.as_ref().map(|f| f.decision.status()),
                control_demand,
                reported_position_ms: control_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.position_ms),
                client_runway_ms: control_snapshot
                    .as_ref()
                    .map(crate::playback_control::PlaybackDemandSnapshot::runway_ms),
                render_state,
                server_ready_state,
                server_ready_anchor_ms,
                server_ready_end_ms,
                server_ready_seconds,
                server_next_ready_start_ms,
                server_next_ready_end_ms,
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
                http_wait_count: wait.count,
                http_wait_oldest_ms: wait.oldest_ms,
                http_wait_segment: wait.oldest_segment.map(i64::from),
                status_generated_unix_ms: crate::media_sessions::unix_ms(),
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
                        &control.snapshot.selection,
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
            encoding: rendition.recipe.encoding.clone(),
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
        // Startup seeds the first bounded batch; every live maintenance tick
        // discovers the next one. Plan deletion precedes directory deletion,
        // so a crash cannot erase the only durable key needed to collect the
        // node-local database row.
        self.shared.reconcile_obsolete_encoded_generations().await;

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
        // Materialized → serve immediately: the overwhelmingly common case,
        // and until now the invisible one. It never reaches the wait pool, so
        // nothing counted it: a node serving a hundred concurrent cache hits
        // and one serving none reported the same thing, and "is this node
        // busy" was a question only `ps` could answer.
        //
        // The guard, not a counter pair, because the request future is dropped
        // when a client goes away and a decrement on the success path leaks on
        // exactly the disconnects worth seeing.
        let _serving = self.shared.pool.metrics_handle().serving();
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
    async fn reconcile_obsolete_encoded_generations(&self) -> usize {
        // This lock belongs only to the cleanup loop. Holding it across the
        // exact Store delete and filesystem removal makes cancellation safe:
        // the front candidate remains queued unless both steps finish.
        let mut pending = self.obsolete_encoded_generations.lock().await;
        let available = ENCODED_RECONCILE_BATCH.saturating_sub(pending.len());
        if available > 0 {
            let excluded = pending
                .iter()
                .map(|candidate| candidate.key.clone())
                .collect::<HashSet<_>>();
            let scanner = Arc::clone(&self.encoded_generation_scanner);
            let process = crate::ffmpeg::encoded_process_identity().to_owned();
            match tokio::task::spawn_blocking(move || {
                scanner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .discover(&process, ENCODED_RECONCILE_BATCH, available, &excluded)
            })
            .await
            {
                Ok(discovered) => pending.extend(discovered),
                Err(error) => tracing::warn!(%error, "encoded VOD generation discovery failed"),
            }
        }

        let mut removed = 0;
        let attempts = pending.len().min(ENCODED_RECONCILE_BATCH);
        for _ in 0..attempts {
            let Some(candidate) = pending.front().cloned() else {
                break;
            };
            if let Err(error) = self.store.forget_rendition_plan(&candidate.key).await {
                tracing::warn!(
                    rendition = %candidate.key,
                    %error,
                    "cannot forget an obsolete encoded rendition plan"
                );
                // Store availability is shared by the batch. Keep every
                // directory, and retry from this exact candidate next tick.
                break;
            }
            let path = candidate.path.clone();
            let removal = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await;
            let gone = match removal {
                Ok(Ok(())) => true,
                Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => true,
                Ok(Err(error)) => {
                    tracing::warn!(
                        path = %candidate.path.display(),
                        %error,
                        "cannot remove an obsolete encoded VOD generation"
                    );
                    false
                }
                Err(error) => {
                    tracing::warn!(%error, "encoded VOD generation removal failed");
                    false
                }
            };
            if gone {
                pending.pop_front();
                removed += 1;
            } else if let Some(candidate) = pending.pop_front() {
                // One unreadable directory cannot pin every later obsolete
                // generation. Its already-deleted plan makes a retry cheap.
                pending.push_back(candidate);
            }
        }
        if removed > 0 {
            tracing::info!(removed, "reconciled obsolete encoded VOD generations");
        }
        removed
    }

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
        index: Option<FragmentIndex>,
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
        index: Option<FragmentIndex>,
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
            let _driver = spawn_driver(Arc::clone(self), Arc::clone(&rendition));
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
        index: &Option<FragmentIndex>,
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
        let plan = if let Some(encoding) = &recipe.encoding {
            encoding.grid.plan(
                duration_ms,
                (encoding.options.video_bitrate_kbps + encoding.options.audio_bitrate_kbps)
                    .saturating_mul(1000)
                    .into(),
            )
        } else {
            let index = index.as_ref().expect("copy recipe has a fragment index");
            let policy = shipped_policy(index.timescale);
            let tracks = track_durations(index, recipe, duration_ms);
            plurx_core::segplan::plan_copy(index, &policy, &tracks)
        };
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
        index: Option<FragmentIndex>,
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
        let mut existed = tokio::fs::metadata(dir.path()).await.is_ok();
        let encoded_process = recipe
            .encoding
            .as_ref()
            .map(|encoding| encoding.engine.process_identity().to_owned());
        if existed {
            if let Some(process) = encoded_process.as_deref() {
                match tokio::fs::read_to_string(dir.path().join(ENCODED_PROCESS_NAME)).await {
                    Ok(owner) if owner.trim() == process => {}
                    Ok(_) => {
                        tokio::fs::remove_dir_all(dir.path())
                            .await
                            .map_err(|error| {
                                format!("removing an obsolete encoded rendition directory: {error}")
                            })?;
                        existed = false;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        // An encoded directory without its generation marker
                        // is unverifiable. Never adopt it under a new marker.
                        tokio::fs::remove_dir_all(dir.path())
                            .await
                            .map_err(|error| {
                                format!("removing an unmarked encoded rendition directory: {error}")
                            })?;
                        existed = false;
                    }
                    Err(error) => {
                        return Err(format!(
                            "reading the encoded rendition process marker: {error}"
                        ));
                    }
                }
            }
        }
        dir.create()
            .await
            .map_err(|error| format!("creating the rendition directory: {error}"))?;
        if !existed {
            if let Some(process) = encoded_process.as_deref() {
                if let Err(error) = publish_encoded_process_marker(dir.path(), process).await {
                    let _ = tokio::fs::remove_dir_all(dir.path()).await;
                    return Err(format!(
                        "publishing the encoded rendition process marker: {error}"
                    ));
                }
            }
        }

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
                            &self.runtime_cache,
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
            ahead_hold: AtomicBool::new(false),
            init_notify: Notify::new(),
            wake: Notify::new(),
            #[cfg(test)]
            stopped_poll_armed: Notify::new(),
            #[cfg(test)]
            stopped_poll_fired: Notify::new(),
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
            admission_sizing_bytes: rendition.completed_cache_budget,
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
            let _ = perform_driver_step(
                &shared,
                &rendition,
                Step::Terminate {
                    why: Termination::Idle,
                },
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

fn spawn_driver(shared: Arc<Shared>, rendition: Arc<Rendition>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let encoded_waiting = rendition
                .recipe
                .encoding
                .as_ref()
                .is_some_and(|encoding| encoding.is_waiting());
            let stopped_encoder = rendition.recipe.encoding.is_some()
                && matches!(rendition.slot.belief().await, Producer::Stopped { .. });
            if encoded_waiting || stopped_encoder {
                // One owned driver, not one retry task per GET. Pool releases
                // outside VOD cannot notify this registry. A queued rendition
                // retries admission quickly; a stopped encoder polls slowly
                // enough to yield inside the live start's five-second budget.
                let poll = if encoded_waiting {
                    Duration::from_millis(250)
                } else {
                    #[cfg(test)]
                    rendition.stopped_poll_armed.notify_one();
                    STOPPED_ENCODER_POLL
                };
                tokio::select! {
                    _ = rendition.wake.notified() => {},
                    _ = tokio::time::sleep(poll) => {},
                }
                #[cfg(test)]
                if stopped_encoder && !encoded_waiting {
                    rendition.stopped_poll_fired.notify_one();
                }
            } else {
                rendition.wake.notified().await;
            }
            if rendition.closed.load(Relaxed) {
                if let Some(encoding) = &rendition.recipe.encoding {
                    encoding.cancel_wait();
                }
                rendition.gen_epoch.fetch_add(1, Relaxed);
                let _ = perform_driver_step(
                    &shared,
                    &rendition,
                    Step::Terminate {
                        why: Termination::Idle,
                    },
                )
                .await;
                break;
            }
            driver_pass(&shared, &rendition).await;
        }
    })
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
async fn retire_failed_rendition(shared: &Shared, rendition: &Arc<Rendition>) {
    *rendition.capacity_hold.lock().expect("capacity hold") = None;
    if matches!(rendition.slot.belief().await, Producer::Absent { .. }) {
        return;
    }
    // Ignored exactly as the dormant purge and generation-end terminations
    // ignore it: a slot that lost its child between the belief read and here
    // is the outcome this asked for.
    let _ = perform_driver_step(
        shared,
        rendition,
        Step::Terminate {
            why: Termination::Idle,
        },
    )
    .await;
}

fn record_performed_step(shared: &Shared, rendition: &Rendition, step: Step) {
    match step {
        Step::Stop => rendition.ahead_hold.store(true, Release),
        Step::Resume | Step::Start { .. } | Step::Restart { .. } => {
            rendition.ahead_hold.store(false, Release);
        }
        Step::Terminate {
            why: Termination::YieldToWaiter,
        } => rendition.ahead_hold.store(true, Release),
        _ => {}
    }
    let why = match step {
        Step::Terminate {
            why: Termination::Idle,
        } => Some(VodProducerTermination::Idle),
        Step::Terminate {
            why: Termination::IndefiniteHold,
        } => Some(VodProducerTermination::IndefiniteHold),
        Step::Terminate {
            why: Termination::YieldToWaiter,
        } => Some(VodProducerTermination::YieldToWaiter),
        Step::Restart { .. } => Some(VodProducerTermination::Restart),
        _ => None,
    };
    if let Some(why) = why {
        shared.pool.metrics_handle().count_producer_termination(why);
    }
}

fn notify_new_vod_live_wait(
    shared: &Arc<Shared>,
    encoding: &crate::vodencode::Encoding,
    was_waiting: bool,
    admitted: bool,
) {
    if !admitted && !was_waiting && encoding.has_live_wait() {
        // Registration is node-wide news: a stopped encoded rendition may be
        // holding exactly this permit. Policy-read retries are deliberately
        // excluded because they registered no capacity waiter.
        shared.kick_all();
    }
}

async fn perform_driver_step(
    shared: &Shared,
    rendition: &Rendition,
    step: Step,
) -> std::io::Result<Performed> {
    let performed = rendition.slot.perform(step, || {}).await?;
    record_performed_step(shared, rendition, step);
    Ok(performed)
}

/// One pass: demand → decide → step → carry it out.
async fn driver_pass(shared: &Arc<Shared>, rendition: &Arc<Rendition>) {
    let mut prepared_permit = None;
    loop {
        if rendition.closed.load(Relaxed) {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
            }
            return;
        }
        if rendition.failure().is_some() {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
            }
            rendition.gen_epoch.fetch_add(1, Relaxed);
            clear_marker_prewarm_dispatch(rendition);
            retire_failed_rendition(shared, rendition).await;
            return;
        }
        let belief = rendition.slot.belief().await;
        // Reader windows may require asynchronous state reads. Take that
        // snapshot before the manifest lock so cached GET publication never
        // waits behind policy or admission I/O.
        let windows = eviction_windows(shared, rendition).await;
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
            ahead_held: rendition.ahead_hold.load(Acquire),
            working_set: WorkingSet {
                used_bytes: shared.working_set.load(Relaxed),
                budget_bytes: rendition.working_set_budget,
                held: matches!(
                    belief,
                    Producer::Stopped {
                        reason: crate::prodsched::Hold::WorkingSetFull { .. }
                            | crate::prodsched::Hold::NoRoom { .. },
                        ..
                    }
                ),
            },
        };
        let decision =
            decide_with_marker_prewarm(&manifest, &demands, position, &windows, &prewarm_ledgers);
        // Record only this pass's decision. A later non-capacity decision
        // clears the reason, so the status cannot outlive the condition that
        // produced it.
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
        let contention = rendition.recipe.encoding.as_ref().map_or(
            Contention {
                live_waiting: false,
                holds_permit: false,
            },
            |encoding| Contention {
                live_waiting: encoding.admissions.live_is_waiting(),
                holds_permit: true,
            },
        );
        let step = yield_step(belief, step, contention);
        if matches!(step, Step::Start { .. } | Step::Restart { .. }) && prepared_permit.is_none() {
            if let Some(encoding) = &rendition.recipe.encoding {
                // The old child may own this pool's only permit. Retire it before
                // admission, but never hold the manifest over process or Store I/O:
                // already-materialized GETs and in-flight publication must drain.
                if matches!(step, Step::Restart { .. }) {
                    rendition.gen_epoch.fetch_add(1, Relaxed);
                    clear_marker_prewarm_dispatch(rendition);
                }
                drop(manifest);
                if matches!(step, Step::Restart { .. }) {
                    match perform_driver_step(shared, rendition, step).await {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(rendition = %rendition.key, "retiring encoder before admission: {error}");
                            return;
                        }
                    }
                }
                let was_waiting = encoding.has_live_wait();
                prepared_permit = encoding.try_permit().await;
                notify_new_vod_live_wait(shared, encoding, was_waiting, prepared_permit.is_some());
                if prepared_permit.is_none() {
                    return;
                }
                // Admission is not permission to execute the old decision. A
                // seek/cancellation/publication may have changed it while waiting.
                // Re-read producer belief and current admitted/accepted demand.
                continue;
            }
        }
        if fence_marker_prewarm_before_room(rendition, belief, step) {
            // Capacity belongs to blocked foreground demand. Fence and retire a
            // speculative generation before freeing bytes, so queued prewarm
            // output cannot consume the room between this pass and the next.
            let terminate = Step::Terminate {
                why: Termination::IndefiniteHold,
            };
            match perform_driver_step(shared, rendition, terminate).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(rendition = %rendition.key, "retiring prewarm producer: {error}");
                }
            }
            rendition.kick();
            return;
        }
        update_marker_prewarm_dispatch(rendition, belief, step, &decision);
        if !matches!(step, Step::Start { .. } | Step::Restart { .. }) {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
            }
        }
        match step {
            Step::Nothing => {}
            Step::Stop | Step::Resume => {
                // TODO(m3-wire): session progress clock — the manager's motion
                // clock replaces this no-op touch when it attaches.
                match perform_driver_step(shared, rendition, step).await {
                    Ok(_) => {}
                    Err(error) => {
                        clear_marker_prewarm_dispatch(rendition);
                        tracing::warn!(rendition = %rendition.key, "performing {step:?}: {error}");
                    }
                }
            }
            Step::Terminate { .. } => {
                rendition.gen_epoch.fetch_add(1, Relaxed);
                match perform_driver_step(shared, rendition, step).await {
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(rendition = %rendition.key, "performing {step:?}: {error}");
                    }
                }
            }
            Step::Start { .. } | Step::Restart { .. } => {
                rendition.gen_epoch.fetch_add(1, Relaxed);
                drop(manifest);
                match perform_driver_step(shared, rendition, step).await {
                    Ok(Performed::NeedsSpawn { at }) => {
                        spawn_generation(shared, rendition, at, prepared_permit.take()).await;
                    }
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
                            match perform_driver_step(
                                shared,
                                rendition,
                                Step::Terminate {
                                    why: Termination::IndefiniteHold,
                                },
                            )
                            .await
                            {
                                Ok(_) => {}
                                Err(error) => {
                                    tracing::warn!(rendition = %rendition.key, "terminating producer after a zero-progress capacity sweep: {error}");
                                }
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
        break;
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
async fn spawn_generation(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    at: u32,
    permit: Option<crate::vodencode::EncodePermit>,
) {
    if !recipe_engine_is_current(&rendition.recipe).await {
        record_failure(
            shared,
            rendition,
            crate::playback_control::ProducerDecisionReason::EngineChanged,
            "the immutable media engine changed; restart is required".to_owned(),
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
    debug_assert_eq!(recipe.encoding.is_some(), permit.is_some());
    let attested = attested_source_setup(rendition);
    let audio_source = match reopen_encoded_audio(rendition.source.as_ref(), recipe).await {
        Ok(source) => source,
        Err(cause) => {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause,
            );
            return;
        }
    };
    // One ffmpeg, converting or not. The conversion happens on the far side of
    // the muxer now — `dvpipe` rewrites the RPUs inside the fragments this
    // process writes — so the producer is the producer it always was.
    let (mut child, child_job, stdout, stderr) = {
        let args = recipe_pipe_args(recipe, start_seconds, attested);
        #[cfg(unix)]
        let descriptors = crate::producer_spawn::Descriptors::from_files(
            rendition.source.as_ref().map(|source| &source.handle),
            audio_source.as_ref().map(|audio| &audio.handle),
            recipe
                .encoding
                .as_ref()
                .and_then(|encoding| encoding.subtitle.as_deref()),
            false,
        );
        #[cfg(windows)]
        let descriptors = crate::producer_spawn::Descriptors::default();
        let program = recipe_program(recipe);
        let spawned = match crate::producer_spawn::spawn(
            &program,
            &args,
            crate::producer_spawn::SpawnOptions {
                runtime_cache: &shared.runtime_cache,
                progress: crate::producer_spawn::Progress::None,
                descriptors,
                env: &[],
            },
        ) {
            Ok(spawned) => spawned,
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
        (
            spawned.child,
            spawned.child_job,
            spawned.stdout,
            spawned.stderr,
        )
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
    rendition
        .slot
        .attach_job_owned(
            child,
            child_job,
            at,
            permit.map(|permit| Box::new(permit) as Box<dyn Send>),
        )
        .await;
    shared.pool.metrics_handle().count_producer_generation(
        if rendition.recipe.encoding.is_some() {
            VodProducerKind::Encoded
        } else {
            VodProducerKind::Copy
        },
    );
    let epoch = rendition.gen_epoch.load(Relaxed);
    let shared = Arc::clone(shared);
    let rendition = Arc::clone(rendition);
    tokio::spawn(async move {
        let key = rendition.key.clone();
        let (_, diagnostic) = tokio::join!(
            run_generation(shared, rendition, stdout, at, epoch),
            crate::ffmpeg::drain_diagnostics(stderr),
        );
        if !diagnostic.trim().is_empty() {
            tracing::warn!(rendition = %key, generation = epoch, %diagnostic, "VOD producer diagnostic");
        }
    });
    tracing::info!(rendition = %rendition_key_field(at), "spawned a producer generation");
}

async fn recipe_engine_is_current(recipe: &Recipe) -> bool {
    if let Some(encoding) = recipe.encoding.as_ref() {
        // One blocking batch for the whole encoding attestation. The
        // executable used to be statted inline here, ahead of the batch and
        // short-circuiting it, so every producer launch and every segment
        // materialisation paid a synchronous `metadata` on a runtime worker.
        if !encoding
            .engine
            .is_current_with_executable(&encoding.executable)
            .await
        {
            return false;
        }
    }
    recipe.cluster_cache_key.is_none() || crate::ffmpeg::fragment_index_engine_is_current().await
}

fn recipe_program(recipe: &Recipe) -> std::path::PathBuf {
    recipe.encoding.as_ref().map_or_else(
        || ffmpeg_bin().into(),
        |encoding| encoding.executable.path.clone(),
    )
}

fn recipe_pipe_args(recipe: &Recipe, start_seconds: f64, attested: bool) -> Vec<String> {
    if let Some(encoding) = &recipe.encoding {
        let mut file = recipe.file.clone();
        if attested {
            file.path = "/dev/fd/3".into();
        }
        // Plan duration is rounded to a complete output frame, just like the
        // terminal -t. Neither audio padding nor the source's final VFR gap
        // may turn a short final entry into an unplanned audio-only tail.
        let plan = encoding.grid.plan(file.duration_ms.unwrap_or(0), 0);
        let end = plan
            .entries
            .last()
            .map_or(0, |entry| entry.start_ticks + entry.duration_ticks);
        let mut args = encoding.args(&file, start_seconds, end as f64 / f64::from(plan.timescale));
        if attested && !file.audio_streams.is_empty() {
            let mut inputs = 0;
            for index in 0..args.len().saturating_sub(1) {
                if args[index] == "-i" {
                    inputs += 1;
                    if inputs == 2 {
                        args[index + 1] = "/dev/fd/4".into();
                        break;
                    }
                }
            }
        }
        args
    } else {
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
        args
    }
}

async fn reopen_encoded_audio(
    source: Option<&crate::fragment_index_cluster::SourceFence>,
    recipe: &Recipe,
) -> Result<Option<crate::fragment_index_cluster::SourceFence>, String> {
    if recipe.encoding.is_some() && !recipe.file.audio_streams.is_empty() {
        if let Some(source) = source {
            return source.reopen(&recipe.file).await.map(Some);
        }
    }
    Ok(None)
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
        encoded_audio_anchor: rendition.recipe.encoding.as_ref().map(|_| {
            let start = rendition.plan.entry(at).expect("spawn entry").start_ticks as f64
                / f64::from(rendition.timescale);
            plurx_core::transcode::vod_audio_anchor(start)
        }),
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
    if !recipe_engine_is_current(&rendition.recipe).await {
        on_generation_end(
            shared,
            rendition,
            Outcome::Failed(Failure::EngineChanged(
                "the immutable media engine changed before init publication".to_owned(),
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
                let identity = match InitIdentity::establish(
                    muxer,
                    rendition
                        .index
                        .as_ref()
                        .map(|index| index.promotion.clone())
                        .unwrap_or_default(),
                ) {
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
    let _ = perform_driver_step(
        shared,
        rendition,
        Step::Terminate {
            why: Termination::Idle,
        },
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
    // The mismatch is local to this immutable rendition. It refuses further
    // publication under the playlist identity the client already holds, but
    // it is not evidence that the process-wide engine baseline moved.
    record_failure(
        shared,
        rendition,
        crate::playback_control::ProducerDecisionReason::RenditionInitChanged,
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
        // with the same local class that path records.
        Failure::InitDrift(_) => Reason::RenditionInitChanged,
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
        if let Some(drift) = self
            .rendition
            .source
            .as_ref()
            .and_then(|source| source.drift())
        {
            // The sentence names what was observed, not what was concluded.
            // "source changed" alone was a confident claim about a thing that
            // may not have happened: the same refusal covers a rewritten
            // source, an inode whose metadata moved without its bytes, and an
            // `fstat` that failed outright. A viewer sees a refusal either
            // way; an operator now sees which one, and a failing CI lane says
            // which field moved instead of leaving it to be guessed at.
            let cause = format!("source changed before fragment publication: {drift}");
            record_failure(
                &self.shared,
                &self.rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, cause));
        }
        if !recipe_engine_is_current(&self.rendition.recipe).await {
            let cause = "immutable media engine changed before fragment publication".to_owned();
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
    runtime_cache: &Path,
) -> Result<Init, HeadRegenerationError> {
    let _permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| HeadRegenerationError::Busy)?;
    if !recipe_engine_is_current(recipe).await {
        return Err(HeadRegenerationError::Failed(
            "the immutable media engine changed before head regeneration".to_owned(),
        ));
    }
    if !source.unchanged() {
        return Err(HeadRegenerationError::Failed(
            "source changed before head regeneration".to_owned(),
        ));
    }
    let permit = if let Some(encoding) = &recipe.encoding {
        let permit = encoding.try_permit().await;
        encoding.cancel_wait();
        Some(permit.ok_or(HeadRegenerationError::Busy)?)
    } else {
        None
    };
    let args = recipe_pipe_args(recipe, 0.0, cfg!(unix));
    let audio_source = reopen_encoded_audio(Some(source), recipe)
        .await
        .map_err(HeadRegenerationError::Failed)?;
    #[cfg(unix)]
    let descriptors = crate::producer_spawn::Descriptors::from_files(
        Some(&source.handle),
        audio_source.as_ref().map(|audio| &audio.handle),
        recipe
            .encoding
            .as_ref()
            .and_then(|encoding| encoding.subtitle.as_deref()),
        false,
    );
    #[cfg(windows)]
    let descriptors = crate::producer_spawn::Descriptors::default();
    let program = recipe_program(recipe);
    let spawned = crate::producer_spawn::spawn(
        &program,
        &args,
        crate::producer_spawn::SpawnOptions {
            runtime_cache,
            progress: crate::producer_spawn::Progress::None,
            descriptors,
            env: &[],
        },
    )
    .map_err(|error| {
        HeadRegenerationError::Failed(format!("spawning the head regeneration: {error}"))
    })?;
    let mut owner = HeadChildOwner::new_job_owned(spawned.child, spawned.child_job);
    owner.permit = permit;
    let stdout = spawned.stdout;
    let stderr = spawned.stderr;
    let (muxer, diagnostic) = tokio::join!(
        read_regenerated_head_before(
            owner,
            stdout,
            HEAD_REGENERATION_TIMEOUT,
            HEAD_REGENERATION_MAX_BYTES,
        ),
        crate::ffmpeg::drain_diagnostics(stderr)
    );
    if !diagnostic.trim().is_empty() {
        tracing::warn!(%diagnostic, "VOD head regeneration diagnostic");
    }
    let muxer = muxer?;
    if !source.unchanged() {
        return Err(HeadRegenerationError::Failed(
            "source changed during head regeneration".to_owned(),
        ));
    }
    if !recipe_engine_is_current(recipe).await {
        return Err(HeadRegenerationError::Failed(
            "the immutable media engine changed during head regeneration".to_owned(),
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
    child: impl Into<HeadChildOwner>,
    stdout: tokio::process::ChildStdout,
    budget: Duration,
    max_bytes: usize,
) -> Result<Init, HeadRegenerationError> {
    // The stdout arrives separately from the child because the owner below
    // takes the child by value, and the read needs the pipe after that move.
    let mut child = child.into();
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

// split: begin vod-tests
#[cfg(test)]
#[path = "vod/tests.rs"]
mod tests;
// split: end vod-tests
