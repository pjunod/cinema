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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use plurx_core::domain::MediaFile;
use plurx_core::fmp4::{segment_name, CutPolicy, FragmentReader, Init, Unit};
use plurx_core::segplan::{
    FragmentIndex, PlanEntryKind, SegmentPlan, SourceIdentity, TrackDurations,
};
use plurx_core::store::Store;
use plurx_core::transcode::{
    copy_pipe_args_with_dolby_vision, Pacing, COPY_FIRST_SEGMENT_SECONDS, COPY_SEGMENT_MAX_BYTES,
    COPY_SEGMENT_MAX_SECS, COPY_SEGMENT_SECONDS,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{Mutex, Notify};

use crate::copyseg::sanitize_stale_dolby_brand;
use crate::ffmpeg::ffmpeg_bin;
use crate::prodexec::{next_step, Producer, Step, Termination};
use crate::prodrun::{Performed, ProducerSlot};
use crate::prodsched::{decide, Demand, Position, WorkingSet, AHEAD_HORIZON_SECONDS};
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

/// `Retry-After` on a deadline-expired blocking GET (plan §2.3's typed
/// `segment_pending`). The header stays on the wire even though hls.js reads
/// it only for 429 — AVPlayer and Media3 are unmeasured (§12.7).
const PENDING_RETRY_AFTER: Duration = Duration::from_secs(1);

/// Blocked GETs per session (plan §2.3, review B4). The cap is the wait
/// pool's; a refused wait answers a typed 503 immediately, no state.
const PER_SESSION_WAIT_CAP: usize = 4;

/// Blocked GETs across the node. TODO(m3-wire): becomes a setting when the
/// manager exposes the wait caps through `/api/v1/settings`.
const GLOBAL_WAIT_CAP: usize = 64;

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
    pub published_end_ms: Option<i64>,
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
    /// Wait-pool caps hit → typed 503, no wait.
    Busy,
    /// 500.
    Io(std::io::Error),
}

/// An open, verified segment ready to stream.
#[derive(Debug)]
pub struct SegmentReady {
    pub file: tokio::fs::File,
    pub len: u64,
    /// Strong: rendition key + plan index + materialization instant + length.
    pub etag: String,
}

/// The copy recipe one rendition serves, minus `start_seconds` — exactly the
/// cache's key discipline (plan §2.4).
#[derive(Debug, Clone)]
struct Recipe {
    file: MediaFile,
    audio_index: Option<i64>,
    aac: bool,
    preserve_dolby_vision: bool,
    have_dovi: bool,
    /// Exact object version whose complete digest selected the cluster blob.
    /// None on the legacy node-local index path.
    source_object_version: Option<String>,
    /// Content/pipeline identity selected by the v2 catalog. This becomes part
    /// of the rendition directory key so weak legacy metadata cannot alias
    /// segments across an in-place source rewrite.
    cluster_cache_key: Option<String>,
}

/// One attached reader, in plan indexes.
#[derive(Debug, Clone, Copy)]
struct Reader {
    /// The furthest segment this session has asked for.
    frontier: u32,
    /// The last segment actually served — the eviction window's playhead.
    last_served: Option<u32>,
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
    /// A recorded producer failure: subsequent planned-segment GETs answer
    /// `ProducerFailed` until a new create replaces the rendition.
    failed: StdMutex<Option<String>>,
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
    demand_since: StdMutex<HashMap<u32, Instant>>,
}

impl Rendition {
    fn kick(&self) {
        self.wake.notify_one();
    }

    fn failure(&self) -> Option<String> {
        self.failed.lock().expect("failed lock").clone()
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

    async fn attach_reader(&self, session_id: &str, frontier: u32) {
        self.readers.lock().await.insert(
            session_id.to_string(),
            Reader {
                frontier,
                last_served: None,
            },
        );
        *self.dormant_since.lock().expect("dormant lock") = None;
    }

    async fn detach_reader(&self, session_id: &str) {
        let mut readers = self.readers.lock().await;
        readers.remove(session_id);
        if readers.is_empty() {
            *self.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
        }
    }

    /// Eviction windows for every attached reader (plan §2.4's reader guard).
    async fn reader_windows(&self) -> Vec<ReaderWindow> {
        let readers = self.readers.lock().await;
        readers
            .values()
            .map(|reader| reader_window(*reader, self.seconds_per_segment))
            .collect()
    }
}

/// One session handle (plan §2.5): auth attribution, sliding TTL, reader
/// window, and — once it ends for good — a tombstone.
struct Session {
    rendition: Arc<Rendition>,
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
    last_touch: StdMutex<Instant>,
    /// Owner-local sequence fence kept separate from media-object touches.
    control: StdMutex<crate::playback_control::ControlState>,
    tombstone: Option<Terminal>,
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
    renditions: Mutex<HashMap<String, Arc<Rendition>>>,
    pool: WaitPool,
    /// Node-wide un-admitted materialized bytes — `prodsched`'s working set.
    working_set: AtomicU64,
    /// Bytes of admitted renditions, moved here from the working set at
    /// completion.
    completed_cache: AtomicU64,
}

pub struct VodServe {
    shared: Arc<Shared>,
}

impl VodServe {
    /// `base` is the renditions root directory (created lazily).
    pub fn new(base: PathBuf, store: Arc<dyn Store>) -> Arc<VodServe> {
        Self::new_configured(base, store, None, None, None)
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
                renditions: Mutex::new(HashMap::new()),
                pool: WaitPool::new(GLOBAL_WAIT_CAP, PER_SESSION_WAIT_CAP),
                working_set: AtomicU64::new(0),
                completed_cache: AtomicU64::new(0),
            }),
        })
    }

    async fn try_cluster_fragment_index(
        &self,
        file: &MediaFile,
        have_dovi: bool,
        preserve_dolby_vision: bool,
    ) -> Result<(FragmentIndex, String, String), String> {
        if preserve_dolby_vision {
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
        let pipeline = crate::fragment_index_cluster::pipeline_digest(file, &engine, have_dovi);
        let cache_key =
            plurx_core::store::cluster_fragment_index_key(&observation.source_sha256, &pipeline)
                .ok_or_else(|| "source attestation contained an invalid digest".to_owned())?;
        let now = crate::fragment_index_cluster::unix_ms();
        let repair = plurx_core::store::NewClusterFragmentIndexJob {
            cache_key: cache_key.clone(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256: observation.source_sha256.clone(),
            pipeline_sha256: pipeline.clone(),
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
        let SessionKind::Copy {
            aac,
            preserve_dolby_vision,
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
        let identity = crate::fragindex::identity_for(file, have_dovi, preserve_dolby_vision);
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
            self.try_cluster_fragment_index(file, have_dovi, preserve_dolby_vision)
                .await
                .map(Some)
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
            preserve_dolby_vision,
            have_dovi,
            source_object_version,
            cluster_cache_key,
        };
        let key = rendition_key(&recipe, &identity);
        let rendition = self
            .shared
            .attach_rendition(&key, &identity, index, recipe, duration_ms, settings)
            .await?
            .ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_source_unsupported",
                    "the fragment index produced an empty VOD plan",
                )
            })?;

        let start_entry = entry_containing(&rendition.plan, req.start_seconds);
        rendition.attach_reader(&session_id, start_entry).await;
        let duration_ms = plan_duration_ms(&rendition.plan);
        self.shared.sessions.lock().await.insert(
            session_id.clone(),
            Session {
                rendition: Arc::clone(&rendition),
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
                lifecycle: Arc::new(Mutex::new(())),
                last_touch: StdMutex::new(Instant::now()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                tombstone: None,
            },
        );
        rendition.kick();
        tracing::info!(
            session = %session_log_id(&session_id),
            rendition = %rendition.key,
            file = file.id,
            "vod session attached (start entry {start_entry})"
        );
        self.emit_lifecycle(
            &session_id,
            file.id,
            file.height.unwrap_or(0),
            req.kind,
            "session_start",
            None,
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
            .map(|(id, session)| VodDeliveryInfo {
                id: id.clone(),
                file_id: session.rendition.recipe.file.id,
                item_id: session.rendition.recipe.file.item_id,
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

    /// Immutable playlist bytes: the same bytes for the session's whole life.
    pub async fn playlist(&self, session_id: &str) -> Option<Result<Vec<u8>, VodError>> {
        let rendition = match self.session_rendition(session_id).await? {
            Ok((rendition, _)) => rendition,
            Err(error) => return Some(Err(error)),
        };
        Some(Ok(rendition.playlist.clone()))
    }

    /// The three-outcome segment GET (plan §2.3). `None` = not a VOD session;
    /// `Ok(None)` = genuinely unknown name → the caller 404s.
    pub async fn segment(
        &self,
        session_id: &str,
        name: &str,
    ) -> Option<Result<Option<SegmentReady>, VodError>> {
        let (rendition, budget) = match self.session_rendition(session_id).await? {
            Ok(found) => found,
            Err(error) => return Some(Err(error)),
        };
        if name == INIT_NAME {
            return Some(self.serve_init(&rendition, budget).await.map(Some));
        }
        let Some(index) = planned_index(name) else {
            // Traversal names and everything else that is not `segNNNNN.m4s`
            // fail the same digit discipline `is_safe_segment` enforces.
            return Some(Ok(None));
        };
        if index as usize >= rendition.plan.len() {
            return Some(Ok(None));
        }
        Some(
            self.serve_segment(&rendition, session_id, index, budget)
                .await
                .map(Some),
        )
    }

    /// End one session for good with a cause; `true` if it was ours.
    /// Idempotent — the first call writes the tombstone and detaches, every
    /// later one only confirms ownership.
    pub async fn end(&self, session_id: &str, cause: Terminal) -> bool {
        let lifecycle = {
            let sessions = self.shared.sessions.lock().await;
            let Some(session) = sessions.get(session_id) else {
                return false;
            };
            Arc::clone(&session.lifecycle)
        };
        let _lifecycle = lifecycle.lock().await;
        let (rendition, file_id, height, kind) = {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = sessions.get_mut(session_id) else {
                return false;
            };
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle) {
                return false;
            }
            if session.tombstone.is_some() {
                return true;
            }
            session.tombstone = Some(cause);
            (
                Arc::clone(&session.rendition),
                session.rendition.recipe.file.id,
                session.target_height,
                session.kind,
            )
        };
        rendition.detach_reader(session_id).await;
        rendition.kick();
        self.emit_lifecycle(
            session_id,
            file_id,
            height,
            kind,
            "session_end",
            Some(match cause {
                Terminal::Deleted => "client_released",
                Terminal::Superseded => "superseded",
                Terminal::AdminStop => "killed",
                Terminal::Revoked => "revoked",
                Terminal::Replaced => "file_replaced",
            }),
        );
        tracing::info!(
            session = %session_log_id(session_id),
            rendition = %rendition.key,
            "vod session ended for good: {cause:?}"
        );
        true
    }

    /// Supersession sweep: end every session with this viewer's
    /// `playback_id` except `keep` (cause [`Terminal::Superseded`]). Called
    /// by the manager on create — for a VOD create AND a legacy one, because
    /// a viewer switching presentations is still one player replacing its own
    /// stream. Scoped by the same user string the legacy sweep uses, so a
    /// colliding `playback_id` from another account ends nothing.
    pub async fn supersede(&self, supersession_user: &str, playback_id: &str, keep: &str) -> usize {
        let victims: Vec<String> = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(id, session)| {
                    session.playback_id == playback_id
                        && session.supersession_user == supersession_user
                        && id.as_str() != keep
                        && session.tombstone.is_none()
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut ended = 0;
        for id in victims {
            if self.end(&id, Terminal::Superseded).await {
                ended += 1;
            }
        }
        ended
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
            Arc::clone(&session.rendition)
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
        let (rendition, target_height) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            (Arc::clone(&session.rendition), session.target_height)
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
                Producer::Absent { .. } => ("waiting", None, false),
            }
        };
        let fetched_end_ms = last_served
            .and_then(|index| rendition.plan.entry(index))
            .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
            .unwrap_or(0);
        Some(VodSessionInfo {
            id: session_id.to_owned(),
            file_id: rendition.recipe.file.id,
            target_height,
            encoder: "vod",
            playlist_shape: "vod",
            producer_state,
            producer_hold,
            producer_failed: failed,
            published_end_ms,
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
            suspended,
            final_: complete,
        })
    }

    /// Apply one fenced control exchange without conflating a replay or stale
    /// request with a media-object touch. Only a newly accepted sequence moves
    /// the existing five-minute VOD activity clock.
    pub(crate) async fn control(
        &self,
        control: crate::playback_control::LocalControlRequest<'_>,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        // Resolve registry ownership before durable I/O, then keep this
        // session's gate held from the authority read through sequence
        // acceptance and the legacy-clock touch. `end` and idle reap take the
        // same per-session gate, so neither can cross the linearization point.
        let lifecycle = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(control.session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            Arc::clone(&session.lifecycle)
        };
        let lifecycle_guard = lifecycle.lock().await;
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
        let outcome = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(control.session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle) || session.tombstone.is_some() {
                return None;
            }
            let accepted = session.control.lock().expect("control lock").accept(
                control.generation,
                control.owner_epoch,
                control.client_instance_id,
                control.sequence,
                control.snapshot.platform(),
            );
            let (disposition, accepted_sequence, action, platform) = match accepted {
                Ok(outcome) => outcome,
                Err(error) => return Some(Err(error)),
            };
            let mut last_touch = session.last_touch.lock().expect("touch lock");
            if disposition == crate::playback_control::ControlDisposition::Accepted {
                *last_touch = Instant::now();
            }
            let remaining = SESSION_IDLE_TTL.saturating_sub(last_touch.elapsed());
            let remaining_ms = i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX);
            Ok::<_, crate::playback_control::ControlStateError>((
                disposition,
                accepted_sequence,
                action,
                platform,
                crate::media_sessions::unix_ms().saturating_add(remaining_ms),
            ))
        };
        let (disposition, accepted_sequence, action, platform, lease_expires_at_unix_ms) =
            match outcome {
                Ok(outcome) => outcome,
                Err(error) => return Some(Err(error)),
            };
        drop(lifecycle_guard);
        let status = self.status(control.session_id).await?;
        Some(Ok(crate::playback_control::LocalControlResult {
            disposition,
            accepted_sequence,
            action,
            lease_expires_at_unix_ms,
            lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            status: crate::transcode::HlsSessionInfo::Vod(Box::new(status)),
            platform,
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
            file_id: session.rendition.recipe.file.id,
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
        Some(session.rendition.recipe.file.id)
    }

    /// Exact copy-recipe facts for native HLS wrappers. This is a media
    /// capability read, so it touches the same sliding TTL as playlist and
    /// segment GETs.
    pub async fn hls_facts(&self, session_id: &str) -> Option<VodHlsFacts> {
        let (rendition, _) = match self.session_rendition(session_id).await {
            Some(Ok(value)) => value,
            _ => return None,
        };
        Some(VodHlsFacts {
            file: rendition.recipe.file.clone(),
            audio_index: rendition.recipe.audio_index,
            aac: rendition.recipe.aac,
            preserve_dolby_vision: rendition.recipe.preserve_dolby_vision,
        })
    }

    /// One immutable plan entry's film-time window for WebVTT children.
    pub async fn segment_window(&self, session_id: &str, segment_index: i64) -> Option<(f64, f64)> {
        let index = u32::try_from(segment_index).ok()?;
        let (rendition, _) = match self.session_rendition(session_id).await {
            Some(Ok(value)) => value,
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
                duration_ms: plan_duration_ms(&session.rendition.plan),
            },
            target_height: session.target_height,
            kind: session.kind,
        })
    }

    /// Periodic maintenance, called from a spawned interval task the manager
    /// owns: dormant-session reap (sliding TTL), dormant-rendition purge
    /// after TTL (un-admitted only), driver kicks.
    pub async fn maintain(&self) {
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
        let mut reaped = Vec::new();
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
                    sessions
                        .remove(&id)
                        .map(|session| Arc::clone(&session.rendition))
                } else {
                    None
                }
            };
            if let Some(rendition) = rendition {
                reaped.push((id, rendition));
            }
        }
        for (id, rendition) in &reaped {
            rendition.detach_reader(id).await;
            tracing::info!(
                session = %session_log_id(id),
                rendition = %rendition.key,
                "vod session idle-reaped (sliding TTL)"
            );
        }

        // Purge un-admitted renditions dormant past their TTL. The collection
        // is only a cheap pre-filter; `purge_if_dormant` re-checks everything
        // under the renditions lock, because a create can attach between this
        // scan and the purge committing.
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
    }

    // ---- serving internals -------------------------------------------------

    /// The session's rendition and block budget, or the typed reason there
    /// isn't one. Touches the sliding TTL — every authorized GET is life.
    async fn session_rendition(
        &self,
        session_id: &str,
    ) -> Option<Result<(Arc<Rendition>, Duration), VodError>> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if let Some(cause) = session.tombstone {
            return Some(Err(VodError::Gone(cause)));
        }
        *session.last_touch.lock().expect("touch lock") = Instant::now();
        Some(Ok((Arc::clone(&session.rendition), session.block_budget)))
    }

    async fn serve_init(
        &self,
        rendition: &Arc<Rendition>,
        budget: Duration,
    ) -> Result<SegmentReady, VodError> {
        if self.source_changed(rendition) {
            let cause = "source changed after the fragment index was selected".to_owned();
            record_failure(&self.shared, rendition, cause.clone());
            return Err(VodError::ProducerFailed(cause));
        }
        self.shared
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
                let ready = open_ready(&path, &format!("{}-init", rendition.key))
                    .await
                    .map_err(VodError::Io)?;
                if self.source_changed(rendition) {
                    let cause =
                        "source changed while the rendition init was being opened".to_owned();
                    record_failure(&self.shared, rendition, cause.clone());
                    return Err(VodError::ProducerFailed(cause));
                }
                return Ok(ready);
            }
            if let Some(cause) = rendition.failure() {
                return Err(VodError::ProducerFailed(cause));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || tokio::time::timeout(remaining, &mut notified)
                    .await
                    .is_err()
            {
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
    ) -> Result<SegmentReady, VodError> {
        // Materialized → serve immediately: the overwhelmingly common case.
        if let Some(ready) = self.open_materialized(rendition, index).await? {
            self.note_served(rendition, session_id, index).await;
            return Ok(ready);
        }
        if let Some(cause) = rendition.failure() {
            return Err(VodError::ProducerFailed(cause));
        }
        self.shared.arm_materialize_watchdog(rendition, index);
        // Planned → register demand and block. The frontier moves before the
        // wait so the driver sees this reader's reach even while registration
        // is still in flight.
        {
            let mut readers = rendition.readers.lock().await;
            if let Some(reader) = readers.get_mut(session_id) {
                reader.frontier = reader.frontier.max(index);
            }
        }
        rendition.kick();
        self.blocked_wait(rendition, session_id, index, budget)
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
    ) -> Result<SegmentReady, VodError> {
        let key = WaitKey {
            rendition: rendition.key.clone(),
            index,
        };
        let mut wait = Box::pin(self.shared.pool.wait(key, session_id, budget));
        // One poll registers the waiter; kick the driver again once it is
        // really in the pool so `blocked_on` reflects it this pass.
        let first = std::future::poll_fn(|cx| {
            use std::future::Future;
            std::task::Poll::Ready(wait.as_mut().poll(cx))
        })
        .await;
        let outcome = match first {
            std::task::Poll::Ready(outcome) => outcome,
            std::task::Poll::Pending => {
                rendition.kick();
                // The lost-wakeup window: a materialize (or a failure) that
                // landed between the caller's materialized-miss and the
                // registration above fired its satisfy/fail against an empty
                // pool — and the driver then sees the index as not owed, so
                // nothing would ever satisfy it again. One re-check after
                // registering, before sleeping, closes it; dropping `wait`
                // deregisters synchronously.
                if let Some(ready) = self.open_materialized(rendition, index).await? {
                    self.note_served(rendition, session_id, index).await;
                    return Ok(ready);
                }
                if let Some(cause) = rendition.failure() {
                    return Err(VodError::ProducerFailed(cause));
                }
                wait.await
            }
        };
        match outcome {
            Err(_refused) => Err(VodError::Busy),
            Ok(WaitOutcome::Ready) => match self.open_materialized(rendition, index).await? {
                Some(ready) => {
                    self.note_served(rendition, session_id, index).await;
                    Ok(ready)
                }
                // Evicted in the instant between satisfy and open — rare, and
                // a retryable pending is the honest answer.
                None => Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                }),
            },
            Ok(WaitOutcome::Deadline) => Err(VodError::Pending {
                retry_after: PENDING_RETRY_AFTER,
            }),
            Ok(WaitOutcome::ProducerFailed(cause)) => Err(VodError::ProducerFailed(cause)),
            // The rendition went away under the wait; the session's own
            // tombstone (if any) is the more precise cause on the next GET.
            Ok(WaitOutcome::Gone) => Err(VodError::Gone(Terminal::Deleted)),
        }
    }

    /// Open one materialized segment under the manifest lock, so eviction
    /// cannot unlink it between the check and the open.
    async fn open_materialized(
        &self,
        rendition: &Arc<Rendition>,
        index: u32,
    ) -> Result<Option<SegmentReady>, VodError> {
        if self.source_changed(rendition) {
            let cause = "source changed after the fragment index was selected".to_owned();
            record_failure(&self.shared, rendition, cause.clone());
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
        match open_ready(&path, &format!("{}-{index}-{at_ms}", rendition.key)).await {
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

    /// Serving updates the session's reader window and kicks the driver.
    async fn note_served(&self, rendition: &Arc<Rendition>, session_id: &str, index: u32) {
        {
            let mut readers = rendition.readers.lock().await;
            if let Some(reader) = readers.get_mut(session_id) {
                reader.last_served = Some(index);
                reader.frontier = reader.frontier.max(index);
            }
        }
        rendition.kick();
    }
}

impl Shared {
    /// Arm ruling A3's producer deadline once per demanded plan entry. The
    /// timestamp survives shorter HTTP block deadlines and their 503 retries.
    fn arm_materialize_watchdog(self: &Arc<Self>, rendition: &Arc<Rendition>, index: u32) {
        let started = Instant::now();
        {
            let mut demands = rendition.demand_since.lock().expect("demand lock");
            if demands.contains_key(&index) {
                return;
            }
            demands.insert(index, started);
        }
        let shared = Arc::clone(self);
        let rendition = Arc::clone(rendition);
        tokio::spawn(async move {
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
                        .remove(&index)
                        .is_some_and(|since| since == started)
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
                        .remove(&index)
                        .is_some_and(|since| since == started)
                }
            };
            if !expired || rendition.failure().is_some() {
                return;
            }
            rendition.gen_epoch.fetch_add(1, Relaxed);
            if !matches!(rendition.slot.belief().await, Producer::Absent { .. }) {
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
            let object = if index == INIT_DEMAND_INDEX {
                "init.mp4".to_owned()
            } else {
                segment_name(u64::from(index))
            };
            record_failure(
                &shared,
                &rendition,
                format!(
                    "materializing {object} exceeded the {:.1}s producer deadline",
                    rendition.materialize_budget.as_secs_f64()
                ),
            );
        });
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
    ) -> Result<Option<Arc<Rendition>>, String> {
        let mut renditions = self.renditions.lock().await;
        if let Some(existing) = renditions.get(key) {
            // A closed rendition is one a purge already committed against —
            // its driver has exited and its wait keys answer nothing, so a
            // session pinned to it would pend forever. Rebuild instead.
            if existing.failure().is_none() && !existing.closed.load(Relaxed) {
                *existing.dormant_since.lock().expect("dormant lock") = None;
                return Ok(Some(Arc::clone(existing)));
            }
            // A failed (or closed) rendition is replaced by the next create:
            // close the old handle (its waiters were already answered typed)
            // and adopt whatever its directory still holds.
            existing.closed.store(true, Relaxed);
            existing.gen_epoch.fetch_add(1, Relaxed);
            existing.kick();
            self.pool.close(key);
            let stale = renditions.remove(key).expect("the entry just looked up");
            // The node-wide counters stop claiming what the detached handle
            // claimed; the rebuild re-adds exactly what it adopts. Skipping
            // this double-counts the same bytes forever, and the working-set
            // budget becomes fiction that stalls every producer on the node.
            {
                let manifest = stale.manifest.lock().await;
                let claimed = manifest.materialized_bytes();
                if manifest.is_admitted() {
                    sub_saturating(&self.completed_cache, claimed);
                } else {
                    sub_saturating(&self.working_set, claimed);
                }
            }
            tracing::info!(rendition = %key, "replacing a failed rendition on create");
        }

        let plan = self
            .stored_plan(key, identity, &index, &recipe, duration_ms)
            .await?;
        if plan.is_empty() {
            return Ok(None);
        }
        let rendition = self
            .build_rendition(key, index, recipe, plan, settings)
            .await?;
        renditions.insert(key.to_string(), Arc::clone(&rendition));
        drop(renditions);
        spawn_driver(Arc::clone(self), Arc::clone(&rendition));
        Ok(Some(rendition))
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
                        match regenerate_init_head(&recipe, &source, &identity).await {
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
                            Err(why) => {
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
        let adopted_bytes = manifest.materialized_bytes();
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
            failed: StdMutex::new(None),
            init_notify: Notify::new(),
            wake: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            dormant_since: StdMutex::new(None),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        });
        if adopted_bytes > 0 {
            self.working_set.fetch_add(adopted_bytes, Relaxed);
        }
        // An adopted rendition that came back complete may admit right away.
        {
            let mut manifest = rendition.manifest.lock().await;
            if !manifest.is_empty() && manifest.next_gap(0).is_none() {
                self.try_admit(&rendition, &mut manifest).await;
            }
        }
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
    /// re-verifying, under the renditions lock, that it is still dormant.
    ///
    /// The re-check is the point: maintain's collect-then-purge scan races a
    /// create, and a session attached between the scan and the commit would
    /// be pinned to a closed rendition — its driver exited, its wait keys
    /// answering nothing, every GET pending forever. `attach_rendition`
    /// clears `dormant_since` while holding the same lock, so the two cannot
    /// interleave.
    async fn purge_if_dormant(self: &Arc<Shared>, key: &str, ttl: Duration) {
        let rendition = {
            let mut renditions = self.renditions.lock().await;
            let Some(rendition) = renditions.get(key).map(Arc::clone) else {
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
            if rendition.manifest.lock().await.is_admitted() {
                return;
            }
            renditions.remove(key);
            rendition
        };
        // Committed: from here the map no longer answers this key, so no new
        // session can attach to the handle being torn down.
        rendition.closed.store(true, Relaxed);
        rendition.gen_epoch.fetch_add(1, Relaxed);
        let _ = rendition
            .slot
            .perform(
                Step::Terminate {
                    why: Termination::Idle,
                },
                || {},
            )
            .await;
        self.pool.close(&rendition.key);
        {
            let mut manifest = rendition.manifest.lock().await;
            let freed = rendition.dir.purge(&mut manifest).await;
            sub_saturating(&self.working_set, freed.bytes);
            // Whatever a failing unlink left both on disk and claimed is no
            // longer managed by anything; keeping it in the counter would
            // hold budget nothing can ever release.
            sub_saturating(&self.working_set, manifest.materialized_bytes());
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
        self.kick_all();
        tracing::info!(
            rendition = %rendition.key,
            "purged a dormant un-admitted rendition"
        );
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

/// One pass: demand → decide → step → carry it out.
async fn driver_pass(shared: &Arc<Shared>, rendition: &Arc<Rendition>) {
    if rendition.failure().is_some() {
        return;
    }
    let mut demands: Vec<Demand> = Vec::new();
    if let Some(blocked) = shared.pool.blocked_on(&rendition.key) {
        demands.push(Demand::waiting_on(blocked));
    }
    {
        let readers = rendition.readers.lock().await;
        for reader in readers.values() {
            demands.push(Demand::idle_at(reader.frontier));
        }
    }
    let belief = rendition.slot.belief().await;
    let mut manifest = rendition.manifest.lock().await;
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
    let action = decide(&manifest, &demands, position);
    let step = next_step(belief, action);
    match step {
        Step::Nothing => {}
        Step::Stop | Step::Resume => {
            // TODO(m3-wire): session progress clock — the manager's motion
            // clock replaces this no-op touch when it attaches.
            if let Err(error) = rendition.slot.perform(step, || {}).await {
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
                    tracing::warn!(rendition = %rendition.key, "performing {step:?}: {error}");
                }
            }
        }
        Step::MakeRoom { wanted } => {
            let windows = rendition.reader_windows().await;
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
                    rendition.kick();
                    if freed.bytes > 0 {
                        shared.kick_all();
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

/// Spawn a real generation positioned at plan entry `at` and hand its stdout
/// to [`run_generation`].
async fn spawn_generation(shared: &Arc<Shared>, rendition: &Arc<Rendition>, at: u32) {
    if rendition.recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        record_failure(
            shared,
            rendition,
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
    let mut args = copy_pipe_args_with_dolby_vision(
        &recipe.file,
        start_seconds,
        recipe.audio_index,
        recipe.aac,
        Pacing::unpaced(),
        recipe.have_dovi,
        recipe.preserve_dolby_vision,
    );
    #[cfg(unix)]
    if rendition.source.is_some() {
        for index in 0..args.len().saturating_sub(1) {
            if args[index] == "-i" {
                args[index + 1] = "/dev/fd/3".to_owned();
            }
        }
    }
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    #[cfg(unix)]
    if let Some(source) = rendition.source.as_ref() {
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
            record_failure(shared, rendition, cause);
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        record_failure(
            shared,
            rendition,
            "the producer started without a stdout".to_string(),
        );
        return;
    };
    // The rendition can be closed between the spawn above and the attach
    // below (a purge committing on the maintain task). Attaching would leave
    // a live ffmpeg in a slot whose driver has already exited — a child
    // nothing reaps until the Arc drops.
    if rendition.closed.load(Relaxed) {
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
            Outcome::Failed(Failure::Stream(
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
            record_failure(shared, rendition, describe_failure(&failure));
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
    record_failure(shared, rendition, cause);
}

fn record_failure(shared: &Arc<Shared>, rendition: &Arc<Rendition>, cause: String) {
    tracing::warn!(rendition = %rendition.key, "producer failed: {cause}");
    *rendition.failed.lock().expect("failed lock") = Some(cause.clone());
    shared.pool.fail(&rendition.key, &cause);
    // Init waiters block on their own Notify, not the wait pool — without
    // this, a GET waiting for `init.mp4` sleeps its whole budget to learn
    // what every segment waiter was told immediately.
    rendition.init_notify.notify_waiters();
}

fn describe_failure(failure: &Failure) -> String {
    match failure {
        Failure::InitDrift(why) => format!("init drift: {why}"),
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

impl vodgen::Sink for RenditionSink {
    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> io::Result<()> {
        if self.rendition.closed.load(Relaxed) {
            // The quiet teardown: `NotFound` is how vodgen learns the session
            // ended normally rather than faulted.
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        if self
            .rendition
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source changed before fragment publication",
            ));
        }
        if self.rendition.recipe.cluster_cache_key.is_some()
            && !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "v2 fragment-index engine changed before fragment publication",
            ));
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
    hasher.update([u8::from(recipe.aac), u8::from(recipe.preserve_dolby_vision)]);
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

/// A reader's eviction guard: back two segments from the playhead, ahead a
/// horizon's worth from the frontier.
///
/// The high edge covers `max(last_served + 1, frontier)`, not last_served
/// alone: a forward seek's blocked GET moves the frontier past the playhead,
/// and a window that forgot it would let `make_room` evict the just-
/// materialized seek target before the waiter opens it — a Pending → produce
/// → evict livelock under working-set pressure.
fn reader_window(reader: Reader, seconds_per_segment: f64) -> ReaderWindow {
    let playhead = reader.last_served.unwrap_or(reader.frontier);
    let frontier = reader
        .last_served
        .map(|served| served.saturating_add(1))
        .unwrap_or(reader.frontier)
        .max(reader.frontier);
    let per = if seconds_per_segment > 0.0 {
        seconds_per_segment
    } else {
        1.0
    };
    let ahead = ((f64::from(AHEAD_HORIZON_SECONDS) / per) as u32).max(1);
    ReaderWindow {
        back: 2,
        playhead,
        frontier,
        ahead,
    }
}

async fn open_ready(path: &Path, etag_stem: &str) -> io::Result<SegmentReady> {
    let file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    Ok(SegmentReady {
        file,
        len,
        etag: format!("{etag_stem}-{len}"),
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
) -> Result<Init, String> {
    if recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        return Err("the v2 fragment-index engine changed before head regeneration".to_owned());
    }
    if !source.unchanged() {
        return Err("source changed before head regeneration".to_owned());
    }
    let mut args = copy_pipe_args_with_dolby_vision(
        &recipe.file,
        0.0,
        recipe.audio_index,
        recipe.aac,
        Pacing::unpaced(),
        recipe.have_dovi,
        recipe.preserve_dolby_vision,
    );
    #[cfg(unix)]
    for index in 0..args.len().saturating_sub(1) {
        if args[index] == "-i" {
            args[index + 1] = "/dev/fd/3".to_owned();
        }
    }
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    #[cfg(unix)]
    {
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
    let mut child = command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawning the head regeneration: {error}"))?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return Err("the head regeneration started without a stdout".to_string());
    };
    let head = read_muxer_init(&mut stdout).await;
    // Only the head is wanted; the rest of the pipe is not read.
    let _ = child.kill().await;
    let (_consumed, muxer) =
        head.map_err(|error| format!("reading the head regeneration's init: {error}"))?;
    if !source.unchanged() {
        return Err("source changed during head regeneration".to_owned());
    }
    if recipe.cluster_cache_key.is_some()
        && !crate::ffmpeg::fragment_index_engine_is_current().await
    {
        return Err("the v2 fragment-index engine changed during head regeneration".to_owned());
    }
    identity
        .served_init_for(&muxer)
        .map_err(|refused| refused.to_string())
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

    use plurx_core::store::{FragmentIndexStore, MediaSessionStore as _, SqliteStore};
    use plurx_core::testfixtures;

    use crate::fragindex::IndexOutcome;

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
        }
    }

    fn request(playback_id: &str, start_seconds: f64) -> SessionRequest {
        SessionRequest {
            file_id: 1,
            playback_id: playback_id.to_string(),
            request_id: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
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
        }
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
            have_dovi,
            false,
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
        store
            .claim_media_session_request(
                7,
                generation,
                &fingerprint,
                "vod-control",
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
        store
            .activate_media_session(&plurx_core::domain::MediaSessionActivation {
                incarnation_id: generation.to_owned(),
                session_id: session_id.to_owned(),
                user_id: 7,
                playback_id: "vod-control".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: Some(generation.to_owned()),
                request_fingerprint: fingerprint,
                owner_node_id: "node-a".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms,
                lease_expires_at_ms: now_ms + 60_000,
            })
            .await
            .expect("activate route")
            .expect("route accepted");
    }

    async fn insert_control_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
        touched: Instant,
    ) {
        rendition.attach_reader(session_id, 0).await;
        serve.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition,
                playback_id: "vod-control".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("vod-control", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: Arc::new(Mutex::new(())),
                last_touch: StdMutex::new(touched),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                tombstone: None,
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
        Arc::new(Rendition {
            key: "synthetic-rendition".to_string(),
            dir,
            recipe: Recipe {
                file: media_file_at(PathBuf::from("unused.mkv"), ms),
                audio_index: None,
                aac: true,
                preserve_dolby_vision: false,
                have_dovi: false,
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
            failed: StdMutex::new(None),
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
                Some(Ok(Some(ready))) => return ready,
                Some(Err(VodError::Pending { .. })) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                other => panic!("fetching {name}: unexpected answer {:?}", describe(other)),
            }
        }
        panic!("fetching {name} never materialized");
    }

    fn describe(answer: Option<Result<Option<SegmentReady>, VodError>>) -> String {
        match answer {
            None => "None".into(),
            Some(Ok(None)) => "Ok(None)".into(),
            Some(Ok(Some(ready))) => format!("Ok(Some(len {}))", ready.len),
            Some(Err(error)) => format!("Err({error:?})"),
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
        sessions.get(session).expect("session").rendition.plan.len()
    }

    async fn rendition_of(serve: &Arc<VodServe>, session: &str) -> Arc<Rendition> {
        let sessions = serve.shared.sessions.lock().await;
        Arc::clone(&sessions.get(session).expect("session").rendition)
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
    async fn the_playlist_bytes_are_identical_across_the_sessions_whole_life() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        let first = serve
            .playlist("sess-a")
            .await
            .expect("a vod session")
            .expect("playlist bytes");
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
            .expect("playlist bytes");
        assert_eq!(first, second, "the playlist is immutable (plan §2.1)");
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
            Some(Ok(Some(ready))) => ready,
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

        // The producer cannot possibly reach the last entry inside a
        // millisecond, so the hard deadline fires and the answer is the typed
        // retryable pending — never an open-ended wait, never a bare 404.
        match serve
            .segment("sess-a", &segment_name(last as u64))
            .await
            .expect("a vod session")
        {
            Err(VodError::Pending { retry_after }) => {
                assert_eq!(retry_after, PENDING_RETRY_AFTER);
            }
            other => panic!("expected Pending, got {:?}", describe(Some(other))),
        }
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

        match waiter.await.expect("waiter task").expect("a vod session") {
            Err(VodError::ProducerFailed(cause)) => {
                assert!(!cause.is_empty());
            }
            other => panic!(
                "a dead producer must answer typed, got {:?}",
                describe(Some(other))
            ),
        }
        // And the failure sticks: a later GET for a planned segment answers
        // the same typed refusal without waiting.
        match serve
            .segment("sess-a", &segment_name(last as u64))
            .await
            .expect("a vod session")
        {
            Err(VodError::ProducerFailed(_)) => {}
            other => panic!("expected ProducerFailed, got {:?}", describe(Some(other))),
        }
    }

    #[tokio::test]
    async fn every_terminal_cause_answers_gone_and_supersession_spares_the_keeper() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        match serve.playlist("sess-a").await.expect("still ours") {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("expected Gone(Deleted), got {other:?}"),
        }
        match serve
            .segment("sess-a", "seg00000.m4s")
            .await
            .expect("still ours")
        {
            Err(VodError::Gone(Terminal::Deleted)) => {}
            other => panic!("expected Gone, got {:?}", describe(Some(other))),
        }
        assert!(
            serve.end("sess-a", Terminal::AdminStop).await,
            "a second end is idempotent and keeps the first cause"
        );
        match serve.playlist("sess-a").await.expect("still ours") {
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
        match serve.playlist("sess-b").await.expect("still ours") {
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
            {
                Ok(Some(ready)) => assert!(ready.len > 0),
                other => panic!(
                    "segment {segment} must serve instantly: {:?}",
                    describe(Some(other))
                ),
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
            .expect("bytes");
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
            .expect("bytes");
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
            {
                Ok(Some(ready)) => assert!(ready.len > 0),
                other => panic!(
                    "adopted segment {segment} must serve instantly: {:?}",
                    describe(Some(other))
                ),
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

        for name in [
            "seg99999.m4s",  // past the plan's end
            "notes.txt",     // not a segment name at all
            "../etc/passwd", // traversal
            "seg../x.m4s",   // traversal dressed as a segment
            "seg00000.ts",   // the transcode extension is not this path's
            "seg.m4s",       // no digits
        ] {
            match serve.segment("sess-a", name).await.expect("a vod session") {
                Ok(None) => {}
                other => panic!(
                    "{name} must be the caller's 404: {:?}",
                    describe(Some(other))
                ),
            }
        }
        // A session this runtime never made is nobody's business here.
        assert!(serve.segment("sess-x", "seg00000.m4s").await.is_none());
        assert!(serve.playlist("sess-x").await.is_none());
        assert!(!serve.owns("sess-x").await);
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
                rendition: Arc::clone(&rendition),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: Arc::new(Mutex::new(())),
                last_touch: StdMutex::new(touched),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
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
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "diagnostic reads must not keep an abandoned session alive",
        );

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(
            serve.status("sess-a").await.is_none(),
            "a tombstone is not live"
        );
        assert!(serve.status("unknown").await.is_none());
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

    #[tokio::test]
    async fn materialize_watchdog_spans_http_retries_and_fails_typed() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared rendition")
            .materialize_budget = Duration::from_millis(25);
        rendition.attach_reader("sess-a", 0).await;
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                rendition,
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_millis(1),
                lifecycle: Arc::new(Mutex::new(())),
                last_touch: StdMutex::new(Instant::now()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                tombstone: None,
            },
        );

        assert!(matches!(
            serve.segment("sess-a", "seg00001.m4s").await,
            Some(Err(VodError::Pending { .. }))
        ));
        tokio::time::sleep(Duration::from_millis(40)).await;
        match serve.segment("sess-a", "seg00001.m4s").await {
            Some(Err(VodError::ProducerFailed(cause))) => {
                assert!(cause.contains("producer deadline"), "{cause}");
            }
            other => panic!("watchdog must settle retries typed: {}", describe(other)),
        }
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
        let window = reader_window(
            Reader {
                frontier: 6,
                last_served: Some(5),
            },
            seconds_per_segment,
        );
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
        let seeked = reader_window(
            Reader {
                frontier: 30,
                last_served: Some(5),
            },
            seconds_per_segment,
        );
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
            preserve_dolby_vision: false,
            have_dovi: false,
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
        match second.segment("sess-b", INIT_NAME).await.expect("ours") {
            Ok(Some(ready)) => assert!(ready.len > 0),
            other => panic!("init.mp4 must serve: {:?}", describe(Some(other))),
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
        record_failure(&serve.shared, &rendition, "boom".to_string());
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

    /// Fix 6: the purge re-checks dormancy under the renditions lock, so a
    /// create that attached between maintain's scan and the purge committing
    /// keeps its rendition.
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
            serve.blocked_wait(&rendition, "sess-x", 5, Duration::from_secs(60)),
        )
        .await
        .expect("the re-check must answer without sleeping the budget")
        .expect("the materialized segment serves");
        assert!(served.len > 0);

        // Same window, failure flavor: a failure recorded in the gap answers
        // typed instead of sleeping.
        record_failure(&serve.shared, &rendition, "boom".to_string());
        match tokio::time::timeout(
            Duration::from_secs(5),
            serve.blocked_wait(&rendition, "sess-x", 6, Duration::from_secs(60)),
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
            tokio::spawn(async move { serve.serve_init(&rendition, Duration::from_secs(30)).await })
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
            tokio::spawn(async move { serve.serve_init(&rendition, Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        record_failure(&serve.shared, &rendition, "boom".to_string());
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
            .open_materialized(&rendition, 0)
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
            .open_materialized(&rendition, 0)
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
