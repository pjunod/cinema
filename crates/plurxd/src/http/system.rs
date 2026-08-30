//! Server identity, first-run setup, settings, and scan status.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{FromRef, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use plurx_core::auth;
#[cfg(test)]
use plurx_core::cluster::migration::status::DbSnapshotHistogram;
use plurx_core::cluster::migration::status::{
    DbSnapshotMetricsSnapshot, DB_SNAPSHOT_HISTOGRAM_BOUNDS_NANOS,
};
use plurx_core::domain::{PlaybackEvent, PlaybackEventQuery};
use plurx_core::metadata::genres::GenreBackfillReport;
use plurx_core::store::{keys, Store};
use serde::{Deserialize, Serialize};

use super::auth::LoginResponse;
use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use super::internal_activity::{ActivityDelivery, PeerActivityOutcome};
use crate::state::{AppState, IntegrationMetrics, ScanStatus, StoreMetricsCache, StoreMetricsView};

#[derive(Serialize)]
pub struct ServerInfo {
    pub name: String,
    /// Bare semver — clients compare this.
    pub version: &'static str,
    /// Git description of the exact build ("v0.1.0-14-gc0ffee"), for support.
    pub build: &'static str,
    /// Compile time, always present — the fallback when `build` is "unknown".
    pub built_at: &'static str,
    pub instance_id: String,
    /// Stable local identity used to distinguish this node's LAN records.
    pub node_id: String,
    /// True when discovery publishes one record per configured cluster node.
    pub cluster_advertisement: bool,
    pub uptime_seconds: u64,
    /// True when no users exist yet — the web app shows first-run setup.
    pub setup_required: bool,
    /// True when an Android APK is published (so the web UI shows the download
    /// link on Android). See `web::android_apk_path`.
    pub android_app: bool,
    /// Whether the web client's Auto controller may change rungs after the
    /// server's initial playback decision. Public because every signed-in web
    /// viewer needs the same node-wide playback policy.
    pub playback_auto_abr: bool,
}

/// GET /api/v1/server — public; drives the client's setup-vs-login decision.
///
/// It stays credential-free in both directions: a client probing an unknown
/// candidate must not attach a saved token, and this response must not carry
/// anything a stranger should not read. The cluster's other ingress origins
/// are therefore not here — see `GET /api/v1/cluster/ingress`.
pub async fn server_info(State(state): State<AppState>) -> Result<Json<ServerInfo>, ApiError> {
    let instance_id = state.store.instance_id().await?;
    let name = state
        .store
        .get_setting(keys::SERVER_NAME)
        .await?
        .unwrap_or_else(|| state.server_name.clone());
    let setup_required = state.store.count_users().await? == 0;
    let android_app = super::web::android_apk_path(&state.system.data_dir).is_some();
    let playback_auto_abr = state
        .store
        .get_setting(keys::PLAYBACK_AUTO_ABR)
        .await?
        .is_some_and(|value| value.trim() == "1");
    Ok(Json(ServerInfo {
        name,
        version: crate::version::SEMVER,
        build: crate::version::BUILD,
        built_at: crate::version::BUILT_AT,
        instance_id,
        node_id: state.node_id.clone(),
        cluster_advertisement: state.cluster_advertisement,
        uptime_seconds: state.started_at.elapsed().as_secs(),
        setup_required,
        android_app,
        playback_auto_abr,
    }))
}

#[derive(Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
}

/// POST /api/v1/setup — create the first (admin) user. Allowed only while no
/// users exist; auto-logs-in on success.
pub async fn setup(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    if state.store.count_users().await? > 0 {
        return Err(ApiError::Conflict("setup already completed".into()));
    }
    if req.username.trim().is_empty() || req.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "username required and password must be at least 8 characters".into(),
        ));
    }
    let hash = auth::hash_password(&req.password).map_err(|e| ApiError::Internal(e.to_string()))?;
    let user = state
        .store
        .create_user(req.username.trim(), &hash, true)
        .await?;

    let token = auth::generate_token().map_err(|e| ApiError::Internal(e.to_string()))?;
    let token_hash = auth::hash_token(&token);
    state
        .store
        .create_token(&token_hash, user.id, Some("setup"))
        .await?;
    Ok(Json(LoginResponse {
        token,
        user: user.into(),
    }))
}

#[derive(Serialize)]
pub struct SystemDto {
    pub name: String,
    pub version: &'static str,
    pub build: &'static str,
    pub built_at: &'static str,
    pub instance_id: String,
    pub uptime_seconds: u64,
    pub users: i64,
    pub libraries: usize,
    pub active_transcodes: usize,
    /// Backend and watch-state convergence, projected without membership data.
    pub replication: plurx_core::cluster::migration::status::ReplicationStatus,
    /// Hardware encoder slots in use, and the cap
    /// (`transcode.max_hw_sessions`). Reported as a pair because either number
    /// alone is unreadable: a start refused while this says 0 of 2 is a very
    /// different bug from one refused at 2 of 2.
    pub hw_slots_in_use: usize,
    pub hw_slots_max: usize,
    /// Recent targeted-scan requests from other applications, newest last.
    /// The one place an operator can see that monarr is actually talking to
    /// plurx — and what it asked for — without reading the log.
    pub scan_requests: Vec<crate::state::ScanRequestRecord>,
    /// The integration at a glance: has another application ever reached
    /// plurx, and when did it last.
    ///
    /// `scan_requests` above only holds requests that got as far as a
    /// library — a path-mapping mistake is rejected before one exists, so a
    /// server being called constantly and rejecting everything looks
    /// identical there to one nobody is calling. These counters tell those
    /// two apart, which is the difference between "fix monarr's path
    /// mapping" and "check monarr's URL and key".
    pub integration: IntegrationDto,
    /// What the libraries' storage reads at, last time anyone measured. The
    /// input side of the pipeline, and until this existed the only side that
    /// had never been measured — which is why a source the box could not read
    /// fast enough surfaced as a *client* stall and sent everyone to look at
    /// the encoder.
    pub storage: crate::storeprobe::StorageReport,
    /// What session create did with the plan each client handed it
    /// (PLAYBACK-CAPS-V2-PLAN §4.5).
    pub plan_derivation: PlanDerivationDto,
    #[serde(flatten)]
    pub info: crate::state::SystemInfo,
}

/// Create's reconciliation of the client's plan with the server's, counted
/// for this process.
///
/// Two numbers to watch, and they say opposite things. `legacy_trusted`
/// falling to zero is the migration finishing: every build on the fleet now
/// sends the capabilities its plan was derived from, so the server is the
/// only decider again. `mismatched` rising off zero is a client that started
/// disagreeing with a plan derived from its own claims — nothing breaks
/// (the server's plan is used, and a create is never refused over this), but
/// it is the first sign a shipped build's caps and its player have drifted
/// apart.
#[derive(Serialize)]
pub struct PlanDerivationDto {
    /// Creates whose body carried no caps document at all, so the client's
    /// `preserve_dolby_vision`/`hdr10` echo was trusted as before. This is
    /// the straggler count: zero means every build on the fleet has moved.
    pub legacy_trusted: u64,
    /// Creates that carried a caps document this server could not read — an
    /// unknown `v`, or one with nothing in it. Counted apart from
    /// `legacy_trusted` because it is a different problem with a different
    /// fix: a client that has adopted the document and is getting it wrong,
    /// rather than one that has not adopted it yet.
    pub unusable_caps: u64,
    /// Creates whose plan was re-derived from the caps in the body.
    pub rederived: u64,
    /// Of those, how many disagreed with the client, with no named override
    /// to explain it.
    pub mismatched: u64,
    /// Of those, how many carried a named override (`compatible_hdr_base`,
    /// `force`) — a legitimate, client-declared departure from the plan.
    pub overridden: u64,
}

#[derive(Serialize)]
pub struct IntegrationDto {
    /// Scan requests received from other applications, ever (this process).
    pub notifications_received: u64,
    /// Scans started, by what asked for one.
    pub scans_by_trigger: std::collections::BTreeMap<String, u64>,
    /// When the last targeted scan request arrived, and who said it was
    /// from. `None` means none has, this run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_notification_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_notification_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_correlation_id: Option<String>,
}

/// GET /api/v1/system (admin) — environment diagnostics for the settings
/// page: paths, ffmpeg, detected encoders, counts.
pub async fn system_info(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<SystemDto>, ApiError> {
    let requests = state.jobs.scan_requests().await;
    let last = requests.last();
    let (by_trigger, notifications) = state.jobs.metrics().snapshot();
    let (hw_in_use, hw_max) = state.transcode.hardware_slots().await;
    let replication = state.replication.status().await;
    let name = state
        .store
        .get_setting(keys::SERVER_NAME)
        .await?
        .unwrap_or_else(|| state.server_name.clone());
    Ok(Json(SystemDto {
        name,
        version: crate::version::SEMVER,
        build: crate::version::BUILD,
        built_at: crate::version::BUILT_AT,
        instance_id: state.store.instance_id().await?,
        uptime_seconds: state.started_at.elapsed().as_secs(),
        users: state.store.count_users().await?,
        libraries: state.catalogue.list_libraries().await?.len(),
        active_transcodes: state.transcode.active_sessions().await,
        replication,
        hw_slots_in_use: hw_in_use,
        hw_slots_max: hw_max,
        scan_requests: requests.clone(),
        integration: IntegrationDto {
            notifications_received: notifications,
            scans_by_trigger: by_trigger
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            last_notification_at: last.map(|r| r.at),
            last_notification_source: last.and_then(|r| r.source.clone()),
            last_correlation_id: last.and_then(|r| r.correlation_id.clone()),
        },
        storage: state.storage.read().await.clone(),
        plan_derivation: {
            let (legacy_trusted, unusable_caps, rederived, mismatched, overridden) =
                super::hls::plan_derivation::snapshot();
            PlanDerivationDto {
                legacy_trusted,
                unusable_caps,
                rederived,
                mismatched,
                overridden,
            }
        },
        info: (*state.system).clone(),
    }))
}

#[derive(Serialize)]
pub struct MediaShapeDto {
    pub probed: i64,
    pub unprobed: i64,
    pub hdr: Vec<(String, i64)>,
    pub hdr_4k: Vec<(String, i64)>,
    pub codecs: Vec<(String, i64)>,
    pub over_segmented_floor: i64,
    pub max_bitrate: Option<i64>,
}

/// GET /api/v1/system/library-shape (admin) — what the libraries actually hold.
///
/// Its own route rather than a field on `/system` because it is a table scan.
/// `/system` is polled by the settings page every few seconds; a census that
/// rides along with it would put a scan of every file behind a UI timer.
pub async fn library_shape(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<MediaShapeDto>, ApiError> {
    let s = state.catalogue.media_shape().await?;
    Ok(Json(MediaShapeDto {
        probed: s.probed,
        unprobed: s.unprobed,
        hdr: s.hdr,
        hdr_4k: s.hdr_4k,
        codecs: s.codecs,
        over_segmented_floor: s.over_segmented_floor,
        max_bitrate: s.max_bitrate,
    }))
}

/// Measure every library's storage and record the result on `state`.
///
/// Shared by the boot task and the admin re-run so there is one definition of
/// what "the storage numbers" are. Roots come from the library table rather
/// than from a config path: what matters is the storage plurx will actually
/// read media from, which is exactly the set of paths libraries point at.
pub async fn probe_storage(state: AppState, sustained_secs: f64) {
    let roots: Vec<std::path::PathBuf> = match state.store.list_libraries().await {
        Ok(libs) => libs.into_iter().flat_map(|l| l.paths).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "storage probe: could not list libraries");
            return;
        }
    };
    if roots.is_empty() {
        return;
    }
    let mut report = crate::storeprobe::probe(roots, sustained_secs).await;
    report.judge();
    for m in &report.mounts {
        // One line per mount, at info: this is the kind of fact someone reads
        // the log to find, and burying it at debug would mean it is only ever
        // seen by someone who already suspected storage.
        tracing::info!(
            roots = %m.roots.join(", "),
            read_mbps = m.read_bps.map(|b| b / 1e6),
            seek_ms = m.seek_ms,
            cache_suspect = m.cache_suspect,
            note = m.note.as_deref().unwrap_or(""),
            "storage probe"
        );
    }
    *state.storage.write().await = report;
}

#[derive(Deserialize, Default)]
pub struct StorageQuery {
    /// Seconds of continuous reading, per mount, on top of the quick probe.
    /// Absent or `0` is the quick probe alone. This costs real I/O — seconds
    /// × the read rate × the number of mounts — so it is opt-in and never
    /// happens at boot.
    #[serde(default)]
    pub sustained: Option<f64>,
}

/// The longest a sustained probe may be asked to run, per mount. A diagnostic
/// that can be told to read for an hour is a denial-of-service with a nice UI.
const SUSTAINED_MAX_SECS: f64 = 120.0;

/// POST /api/v1/system/storage (admin) — re-measure and return the new
/// numbers. Synchronous, because the caller asked for a measurement and an
/// immediate 200 with the *old* figures would be worse than a slow one.
pub async fn remeasure_storage(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(q): Query<StorageQuery>,
) -> Result<Json<crate::storeprobe::StorageReport>, ApiError> {
    let sustained = q.sustained.unwrap_or(0.0).clamp(0.0, SUSTAINED_MAX_SECS);
    probe_storage(state.clone(), sustained).await;
    Ok(Json(state.storage.read().await.clone()))
}

/// POST /api/v1/system/search-index/rebuild (admin) — recreate the derived
/// search index from cluster-authoritative item rows on every voter.
pub async fn rebuild_search_index(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let indexed_items = state.store.rebuild_search_index().await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "indexed_items": indexed_items
    })))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    /// Minimum severity to include ("error" … "trace"). Default: everything
    /// the server's log filter captured.
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_limit")]
    pub limit: usize,
    /// `general` is Settings → System; `cluster` is the isolated membership
    /// and Raft ring on Settings → Cluster.
    #[serde(default)]
    pub scope: String,
}

fn default_log_level() -> String {
    "trace".to_owned()
}
fn default_log_limit() -> usize {
    500
}

/// GET /api/v1/system/logs (admin) — recent log lines, oldest first.
pub async fn logs(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> Json<Vec<crate::logbuf::LogEntry>> {
    let buffer = if q.scope == "cluster" {
        &state.cluster_logs
    } else {
        &state.logs
    };
    Json(buffer.tail(&q.level, q.limit.min(2000)))
}

#[derive(Deserialize)]
pub struct PlaybackEventsQuery {
    pub since: Option<i64>,
    pub event: Option<String>,
    #[serde(default = "default_playback_event_limit")]
    pub limit: i64,
}

fn default_playback_event_limit() -> i64 {
    500
}

/// GET /api/v1/system/playback-events (admin) — newest node-local playback
/// observations first. The Store applies the final 2,000-row cap too, so a
/// future caller cannot bypass this handler's bound.
pub async fn playback_events(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<PlaybackEventsQuery>,
) -> Result<Json<Vec<PlaybackEvent>>, ApiError> {
    Ok(Json(
        state
            .store
            .playback_events(&bounded_playback_query(query))
            .await?,
    ))
}

fn bounded_playback_query(query: PlaybackEventsQuery) -> PlaybackEventQuery {
    PlaybackEventQuery {
        since_ms: query.since,
        event: query.event,
        limit: query.limit.clamp(1, 2_000),
    }
}

/// A client-side playback problem the browser reports back to the server.
///
/// Why this exists: when a browser refuses a stream — Safari rejecting a codec,
/// or a direct-play file it won't progressive-play — *nothing runs server-side
/// to fail*, so `Settings → Logs` stays empty and the failure is invisible
/// unless the user opens dev tools. Forwarding the browser's own error here puts
/// it in the same log the admin already reads. All fields optional so the client
/// can send only what's relevant; everything is length-clipped before logging.
/// Server telemetry the Apple client last polled before a freeze. The server
/// replaces these values with a live join when the named session still
/// exists; keeping the snapshot closes the race where recovery supersedes the
/// session before this best-effort beacon arrives.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ClientServerSnapshot {
    pub observed_age_ms: Option<i64>,
    pub recent_speed: Option<f64>,
    pub ahead_seconds: Option<i64>,
    pub ahead_bytes: Option<i64>,
    pub suspended: Option<bool>,
    pub hold_reason: Option<String>,
    pub delivered_bps: Option<i64>,
    pub delivered_idle_ms: Option<i64>,
    pub readrate: Option<f64>,
    pub suspend_count: Option<i64>,
    pub progress_idle_ms: Option<i64>,
    pub published_end_ms: Option<i64>,
    pub fetched_end_ms: Option<i64>,
    pub fetched_segment: Option<i64>,
    pub first_retained_segment: Option<i64>,
    pub playlist_shape: Option<String>,
    pub last_request: Option<String>,
    pub last_request_idle_ms: Option<i64>,
}

/// AVPlayer and access-log state captured at the same monotonic sample that
/// declared a stall. All fields are optional because direct files, older OS
/// releases, and a not-yet-started HLS item expose different subsets.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ClientPlaybackSnapshot {
    pub position_ms: Option<i64>,
    pub runway: Option<f64>,
    pub time_control_status: Option<String>,
    pub waiting_reason: Option<String>,
    pub playback_buffer_empty: Option<bool>,
    pub playback_likely_to_keep_up: Option<bool>,
    pub playback_buffer_full: Option<bool>,
    pub media_requests: Option<i64>,
    pub downloaded_duration: Option<f64>,
    pub bytes_transferred: Option<i64>,
    pub transfer_duration: Option<f64>,
    pub observed_bitrate_bps: Option<f64>,
    pub indicated_bitrate_bps: Option<f64>,
    pub access_stalls: Option<i64>,
    pub server: Option<ClientServerSnapshot>,
}

/// Last control snapshot the server accepted before a legacy client recovery.
/// M2 keeps the legacy recovery authoritative, but preserving this join makes
/// shadow comparisons possible without inferring client state after the fact.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ClientControlSnapshot {
    pub generation: Option<String>,
    pub control_epoch: Option<u64>,
    pub sequence: Option<u64>,
    pub demand: Option<String>,
    pub render_state: Option<String>,
    pub position_ms: Option<i64>,
    pub buffered_through_ms: Option<i64>,
    pub observed_download_bps: Option<u64>,
    pub observation: Option<ClientControlObservation>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ClientControlObservation {
    pub dropped_frames: Option<u64>,
    pub decoder_state: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ClientLog {
    /// "error" | "warn" — anything but "error" logs at WARN.
    pub level: String,
    /// Short machine tag: "playback_failed" | "stream_rejected" | "hls_fatal" |
    /// "stall" | "stall_recovery".
    pub event: String,
    /// Human-readable summary (e.g. "format not supported by this browser").
    pub message: String,
    /// Delivery path in play at the time: "direct_play" | "remux" | "transcode".
    pub method: Option<String>,
    /// `HTMLMediaElement.error.code` (1..=4), when the failure is a media error.
    pub code: Option<i64>,
    /// Title being played, for cross-referencing with the library.
    pub title: Option<String>,
    /// File id being played.
    pub file_id: Option<i64>,
    /// Source video codec the decision picked, e.g. "hevc" — the usual Safari culprit.
    pub vcodec: Option<String>,
    /// Stream URL (query/token stripped by the client).
    pub src: Option<String>,
    /// Extra detail (hls.js error type, stall verdict, …).
    pub detail: Option<String>,
    /// Browser label the client computed ("Safari" | "Chrome" | …).
    pub ua: Option<String>,
    /// Whether this browser will decode this stream in hardware, as reported
    /// by `navigator.mediaCapabilities` for the real codec, resolution and
    /// bitrate — not for a codec string alone.
    ///
    /// The one thing that separates two failures every other field here
    /// renders identically: a full buffer with late frames because the GPU is
    /// doing the work and something upstream hiccuped, versus a full buffer
    /// with late frames because a CPU is software-decoding 4K. `null` from a
    /// browser without the API is honest; `false` is the finding.
    pub decode_hw: Option<bool>,
    /// The browser's own guess at whether it can keep up. `false` alongside
    /// `decode_hw: false` is the browser saying so before it even started.
    pub decode_smooth: Option<bool>,
    // -- playback measurements (M0) ------------------------------------------
    // These are the point of the beacons. Without them the log records THAT a
    // stream stalled and not the one number that says why, which is how much
    // was buffered when it did. Unknown fields are dropped by serde, so a
    // measurement the client sends and this struct doesn't name is a
    // measurement nobody ever sees — the reason these are listed explicitly.
    /// Which playback attempt this belongs to (one per stream start/restart),
    /// so a seek's numbers never contaminate a cold start's.
    pub attempt: Option<String>,
    /// Why that attempt began: cold-start · resume · seek · audio · quality ·
    /// fallback.
    pub reason: Option<String>,
    /// Seconds of decoded video ahead of the playhead at the moment reported.
    pub runway: Option<f64>,
    /// Time from click to first frame, in ms (the `ttff` event).
    pub ms: Option<i64>,
    /// hls.js's bandwidth estimate, in kb/s.
    pub bandwidth: Option<i64>,
    /// Target height of the stream in play.
    pub height: Option<i64>,
    /// Encoder the server reported for this session.
    pub encoder: Option<String>,
    /// Live HLS session to join with node-local server telemetry. Web clients
    /// historically call this `session`; the alias keeps that field additive.
    #[serde(alias = "session")]
    pub session_id: Option<String>,
    /// Correlated AVPlayer and last-polled server state for stall attribution.
    pub snapshot: Option<ClientPlaybackSnapshot>,
    /// Client-reported last accepted protocol state preceding this event.
    /// It is evidence, not an authoritative server join.
    pub control: Option<ClientControlSnapshot>,
    /// Current trigger state sampled immediately before legacy recovery.
    pub control_trigger: Option<ClientControlSnapshot>,
}

/// Sustained rate and burst allowance for `/client-log`, in reports per minute.
///
/// The log ring holds 2000 lines, so a browser that hits an error in a loop can
/// erase every other line in it in seconds — destroying precisely the history an
/// operator opened the page to read. That is not hypothetical: a stranded
/// hls.js instance polling a playlist that never ends fires a fatal error on a
/// timer, and there was no bound on how many of those could pile up.
const CLIENT_LOG_USER_PER_MIN: u32 = 240;
const CLIENT_LOG_GLOBAL_PER_MIN: u32 = 1_000;
const CLIENT_LOG_USER_BUCKETS_MAX: usize = 4_096;

/// One bucket in the two-tier limiter. Per-user buckets prevent one viewer
/// silencing another; the global bucket bounds total pressure on the log ring.
struct LogBucket {
    tokens: f64,
    per_minute: u32,
    /// `None` until the first report — `Instant` has no const constructor.
    last: Option<std::time::Instant>,
    /// Reports dropped since the last admitted one, so the gap is reported
    /// rather than silently swallowed.
    suppressed: u64,
}

impl LogBucket {
    /// Take a token. `Some(n)` means record this report and mention that `n`
    /// were dropped before it; `None` means drop it.
    fn admit(&mut self, now: std::time::Instant) -> Option<u64> {
        let elapsed = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f64())
            .unwrap_or(0.0);
        self.last = Some(now);
        self.tokens =
            (self.tokens + elapsed * (self.per_minute as f64 / 60.0)).min(self.per_minute as f64);
        if self.tokens < 1.0 {
            self.suppressed = self.suppressed.saturating_add(1);
            return None;
        }
        self.tokens -= 1.0;
        Some(std::mem::take(&mut self.suppressed))
    }
}

impl LogBucket {
    fn new(per_minute: u32) -> Self {
        Self {
            tokens: per_minute as f64,
            per_minute,
            last: None,
            suppressed: 0,
        }
    }
}

struct ClientLogLimiter {
    global: LogBucket,
    users: HashMap<i64, LogBucket>,
}

impl ClientLogLimiter {
    fn new() -> Self {
        Self {
            global: LogBucket::new(CLIENT_LOG_GLOBAL_PER_MIN),
            users: HashMap::new(),
        }
    }

    fn admit(&mut self, user_id: i64, now: std::time::Instant) -> Option<u64> {
        if !self.users.contains_key(&user_id) && self.users.len() >= CLIENT_LOG_USER_BUCKETS_MAX {
            let oldest_id = self
                .users
                .iter()
                .min_by_key(|(_, bucket)| bucket.last)
                .map(|(id, _)| *id);
            if let Some(id) = oldest_id {
                if let Some(evicted) = self.users.remove(&id) {
                    self.global.suppressed =
                        self.global.suppressed.saturating_add(evicted.suppressed);
                }
            }
        }
        let user_suppressed = self
            .users
            .entry(user_id)
            .or_insert_with(|| LogBucket::new(CLIENT_LOG_USER_PER_MIN))
            .admit(now)?;
        match self.global.admit(now) {
            Some(global_suppressed) => Some(user_suppressed.saturating_add(global_suppressed)),
            None => {
                // The global bucket counted this report. Preserve any older
                // per-user gap that was just collected, so the next admitted
                // report still prints every suppressed event exactly once.
                if let Some(bucket) = self.users.get_mut(&user_id) {
                    bucket.suppressed = bucket.suppressed.saturating_add(user_suppressed);
                }
                None
            }
        }
    }
}

static CLIENT_LOG_LIMITER: std::sync::LazyLock<std::sync::Mutex<ClientLogLimiter>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(ClientLogLimiter::new()));

#[cfg(test)]
struct ClientLogCaptureHook {
    message: &'static str,
    captured: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
static CLIENT_LOG_CAPTURE_HOOK: std::sync::Mutex<Option<ClientLogCaptureHook>> =
    std::sync::Mutex::new(None);

/// Pause one client-log request after its authenticated network identity has
/// been captured but before the detached telemetry work is started.
#[cfg(test)]
pub(super) fn pause_next_client_log_after_capture(
    message: &'static str,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (captured_tx, captured_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    *CLIENT_LOG_CAPTURE_HOOK
        .lock()
        .expect("client-log hook lock") = Some(ClientLogCaptureHook {
        message,
        captured: captured_tx,
        release: release_rx,
    });
    (captured_rx, release_tx)
}

/// POST /api/v1/client-log — any signed-in user. Records one browser playback
/// error into the server log ring so it surfaces in `Settings → Logs`. Bounded
/// by per-field clipping and by a global rate limit (this is diagnostics, not an
/// audit trail), and tagged with the `plurxd::client` target so it's visibly a
/// client report.
pub async fn client_log(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    super::network::RemoteAddress(remote): super::network::RemoteAddress,
    Json(ev): Json<ClientLog>,
) -> StatusCode {
    let suppressed = match CLIENT_LOG_LIMITER.lock() {
        Ok(mut limiter) => limiter.admit(user.id, std::time::Instant::now()),
        // Fail open: a poisoned lock must not silence diagnostics.
        Err(_) => Some(0),
    };
    // Still 204 when dropped. The client is reporting, not asking, and an error
    // response would only give it something new to report about.
    let Some(suppressed) = suppressed else {
        return StatusCode::NO_CONTENT;
    };
    let line = client_log_line(&ev, suppressed);

    // Both WARN and ERROR clear the default `info` filter, so either shows in
    // the admin log without the operator touching PLURX_LOG.
    if ev.level.eq_ignore_ascii_case("error") {
        tracing::error!(target: "plurxd::client", "{line}");
    } else {
        tracing::warn!(target: "plurxd::client", "{line}");
    }

    let event = client_playback_event(&ev, user.id);
    // Deliberately not `ev.ua`: the class must come from the same input the
    // read paths use, or the prior is written under a key nothing reads.
    let mut network = super::network::identity(&headers, remote);
    if let Some(ref mut id) = network {
        id.user_id = Some(user.id);
        id.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
            user.id,
            user.created_at,
            &user.password_hash,
        ));
    }
    #[cfg(test)]
    let capture_hook = {
        let mut hook = CLIENT_LOG_CAPTURE_HOOK
            .lock()
            .expect("client-log hook lock");
        if hook.as_ref().is_some_and(|hook| hook.message == ev.message) {
            hook.take()
        } else {
            None
        }
    };
    #[cfg(test)]
    if let Some(hook) = capture_hook {
        let _ = hook.captured.send(());
        let _ = hook.release.await;
    }
    let transcode = Arc::clone(&state.transcode);
    let store = Arc::clone(&state.store);
    tokio::spawn(async move {
        let session_id = event.session_id.clone();
        let info = match session_id.as_deref() {
            Some(session_id) => transcode.session_status(session_id).await,
            None => None,
        };
        emit_client_playback_event(store, event, info.as_ref(), network);
    });
    StatusCode::NO_CONTENT
}

fn join_session_truth(event: &mut PlaybackEvent, info: &crate::transcode::SessionInfo) {
    event.file_id = Some(info.file_id);
    event.encoder = Some(info.encoder.to_owned());
    event.height = Some(info.target_height);
    event.speed_recent = info.recent_speed;
    event.ahead_seconds = info.ahead_seconds;
    event.suspended = Some(info.suspended);
    event.hold_reason = info.hold_reason.map(|reason| match reason {
        crate::transcode::AheadHoldReason::Demand => "demand".to_owned(),
        crate::transcode::AheadHoldReason::Time => "time".to_owned(),
        crate::transcode::AheadHoldReason::Bytes => "bytes".to_owned(),
        crate::transcode::AheadHoldReason::Global => "global".to_owned(),
    });
    event.delivered_bps = info.delivered_bps;
    event.readrate = Some(info.readrate);
    let mut extra = event
        .extra
        .as_deref()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    extra.insert(
        "server".to_owned(),
        serde_json::json!({
            "recent_speed": info.recent_speed,
            "ahead_seconds": info.ahead_seconds,
            "ahead_bytes": info.ahead_bytes,
            "suspended": info.suspended,
            "hold_reason": info.hold_reason,
            "delivered_bps": info.delivered_bps,
            "delivered_idle_ms": info.delivered_idle_ms,
            "readrate": info.readrate,
            "suspend_count": info.suspend_count,
            "progress_idle_ms": info.progress_idle_ms,
            "producer_state": info.producer_state,
            "producer_exit_success": info.producer_exit_success,
            "producer_exit_code": info.producer_exit_code,
            "producer_exit_signal": info.producer_exit_signal,
            "producer_exit_idle_ms": info.producer_exit_idle_ms,
            "producer_attempt": info.producer_attempt,
            "playlist_ready": info.playlist_ready,
            "published_segment": info.published_segment,
            "published_end_ms": info.published_end_ms,
            "next_media_sequence": info.next_media_sequence,
            "fetched_end_ms": info.fetched_end_ms,
            "fetched_segment": info.fetched_segment,
            "pending_fetched_segment": info.pending_fetched_segment,
            "first_retained_segment": info.first_retained_segment,
            "playlist_shape": info.playlist_shape,
            "last_request": info.last_request,
            "lease_mode": info.lease_mode,
            "lease_state": info.lease_state,
            "lease_timeout_ms": info.lease_timeout_ms,
            "control_demand": info.control_demand,
            "reported_position_ms": info.reported_position_ms,
            "client_runway_ms": info.client_runway_ms,
            "render_state": info.render_state,
            "production_policy": info.production_policy,
            "production_ahead_seconds": info.production_ahead_seconds,
            "production_target_seconds": info.production_target_seconds,
            "producer_control": info.producer_control,
            "last_request_idle_ms": i64::try_from(info.idle_seconds)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000)
        }),
    );
    event.extra = Some(serde_json::Value::Object(extra).to_string());
}

fn emit_client_playback_event(
    store: Arc<dyn Store>,
    mut event: PlaybackEvent,
    info: Option<&crate::transcode::SessionInfo>,
    network: Option<crate::telemetry::NetworkIdentity>,
) {
    if let Some(info) = info {
        join_session_truth(&mut event, info);
    }
    event.session_id = event
        .session_id
        .as_deref()
        .map(crate::transcode::session_log_id);
    crate::telemetry::emit_with_network(store, event, network);
}

fn client_playback_event(ev: &ClientLog, user_id: i64) -> PlaybackEvent {
    fn clipped(value: &Option<String>, limit: usize) -> Option<String> {
        let value = value.as_deref()?.trim();
        if value.is_empty() {
            return None;
        }
        match value.char_indices().nth(limit) {
            Some((index, _)) => Some(value[..index].to_owned()),
            None => Some(value.to_owned()),
        }
    }
    fn control_context(value: &ClientControlSnapshot) -> Option<serde_json::Value> {
        const MAX_MEDIA_MS: i64 = 366 * 24 * 60 * 60 * 1_000;
        const MAX_DOWNLOAD_BPS: u64 = 10_000_000_000_000;
        let mut context = serde_json::Map::new();
        if let Some(generation) = clipped(&value.generation, 64)
            .filter(|generation| uuid::Uuid::parse_str(generation).is_ok())
        {
            let hash = crate::transcode::session_log_id(&generation);
            context.insert(
                "generation".to_owned(),
                format!("g-{}", hash.trim_start_matches("s-")).into(),
            );
        }
        for (key, number) in [
            (
                "control_epoch",
                value.control_epoch.filter(|number| *number > 0),
            ),
            ("sequence", value.sequence.filter(|number| *number > 0)),
        ] {
            if let Some(number) = number {
                context.insert(key.to_owned(), number.into());
            }
        }
        if let Some(demand) = clipped(&value.demand, 16)
            .filter(|demand| matches!(demand.as_str(), "active" | "hold" | "end"))
        {
            context.insert("demand".to_owned(), demand.into());
        }
        if let Some(render) = clipped(&value.render_state, 16).filter(|render| {
            matches!(
                render.as_str(),
                "starting" | "rendering" | "waiting" | "stalled" | "seeking" | "ended" | "failed"
            )
        }) {
            context.insert("render_state".to_owned(), render.into());
        }
        let position = value
            .position_ms
            .filter(|number| (0..=MAX_MEDIA_MS).contains(number));
        if let Some(number) = position {
            context.insert("position_ms".to_owned(), number.into());
        }
        if let Some(number) = value.buffered_through_ms.filter(|number| {
            (0..=MAX_MEDIA_MS).contains(number)
                && position.is_none_or(|position| *number >= position)
        }) {
            context.insert("buffered_through_ms".to_owned(), number.into());
        }
        if let Some(number) = value
            .observed_download_bps
            .filter(|number| *number <= MAX_DOWNLOAD_BPS)
        {
            context.insert("observed_download_bps".to_owned(), number.into());
        }
        if let Some(value) = value.observation.as_ref() {
            let mut observation = serde_json::Map::new();
            if let Some(number) = value
                .dropped_frames
                .filter(|number| *number <= 1_000_000_000)
            {
                observation.insert("dropped_frames".to_owned(), number.into());
            }
            if let Some(state) = clipped(&value.decoder_state, 16).filter(|state| {
                matches!(state.as_str(), "unknown" | "ready" | "starved" | "failed")
            }) {
                observation.insert("decoder_state".to_owned(), state.into());
            }
            let error_code = clipped(&value.error_code, 16).filter(|code| {
                matches!(
                    code.as_str(),
                    "network" | "manifest" | "media" | "decoder" | "drm" | "unknown"
                )
            });
            if let Some(code) = error_code {
                observation.insert("error_code".to_owned(), code.into());
                if let Some(detail) = clipped(&value.error_detail, 120).filter(|detail| {
                    !detail
                        .bytes()
                        .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
                }) {
                    observation.insert("error_detail".to_owned(), detail.into());
                }
            }
            if !observation.is_empty() {
                context.insert("observation".to_owned(), observation.into());
            }
        }
        (!context.is_empty()).then_some(serde_json::Value::Object(context))
    }
    let mut extra = serde_json::Map::new();
    for (key, value) in [
        ("message", clipped(&Some(ev.message.clone()), 200)),
        ("title", clipped(&ev.title, 120)),
        ("vcodec", clipped(&ev.vcodec, 16)),
        ("src", clipped(&ev.src, 160)),
    ] {
        if let Some(value) = value {
            extra.insert(key.to_owned(), value.into());
        }
    }
    if let Some(code) = ev.code {
        extra.insert("code".into(), code.into());
    }
    if let Some(value) = ev.decode_hw {
        extra.insert("decode_hw".into(), value.into());
    }
    if let Some(value) = ev.decode_smooth {
        extra.insert("decode_smooth".into(), value.into());
    }
    let snapshot = ev.snapshot.as_ref();
    let server = snapshot.and_then(|snapshot| snapshot.server.as_ref());
    if let Some(snapshot) = snapshot {
        let finite = |value: Option<f64>| value.filter(|value| value.is_finite() && *value >= 0.0);
        let mut client = serde_json::Map::new();
        for (key, value) in [
            ("position_ms", snapshot.position_ms),
            ("media_requests", snapshot.media_requests),
            ("bytes_transferred", snapshot.bytes_transferred),
            ("access_stalls", snapshot.access_stalls),
        ] {
            if let Some(value) = value.filter(|value| *value >= 0) {
                client.insert(key.to_owned(), value.into());
            }
        }
        for (key, value) in [
            ("runway", finite(snapshot.runway)),
            ("downloaded_duration", finite(snapshot.downloaded_duration)),
            ("transfer_duration", finite(snapshot.transfer_duration)),
            (
                "observed_bitrate_bps",
                finite(snapshot.observed_bitrate_bps),
            ),
            (
                "indicated_bitrate_bps",
                finite(snapshot.indicated_bitrate_bps),
            ),
        ] {
            if let Some(value) = value {
                client.insert(key.to_owned(), value.into());
            }
        }
        for (key, value) in [
            (
                "time_control_status",
                clipped(&snapshot.time_control_status, 48),
            ),
            ("waiting_reason", clipped(&snapshot.waiting_reason, 80)),
        ] {
            if let Some(value) = value {
                client.insert(key.to_owned(), value.into());
            }
        }
        for (key, value) in [
            ("playback_buffer_empty", snapshot.playback_buffer_empty),
            (
                "playback_likely_to_keep_up",
                snapshot.playback_likely_to_keep_up,
            ),
            ("playback_buffer_full", snapshot.playback_buffer_full),
        ] {
            if let Some(value) = value {
                client.insert(key.to_owned(), value.into());
            }
        }
        if let Some(server) = server {
            let mut status = serde_json::Map::new();
            for (key, value) in [
                ("observed_age_ms", server.observed_age_ms),
                ("ahead_seconds", server.ahead_seconds),
                ("ahead_bytes", server.ahead_bytes),
                ("delivered_bps", server.delivered_bps),
                ("delivered_idle_ms", server.delivered_idle_ms),
                ("suspend_count", server.suspend_count),
                ("progress_idle_ms", server.progress_idle_ms),
                ("published_end_ms", server.published_end_ms),
                ("fetched_end_ms", server.fetched_end_ms),
                ("fetched_segment", server.fetched_segment),
                ("first_retained_segment", server.first_retained_segment),
                ("last_request_idle_ms", server.last_request_idle_ms),
            ] {
                if let Some(value) = value.filter(|value| *value >= 0) {
                    status.insert(key.to_owned(), value.into());
                }
            }
            for (key, value) in [
                ("recent_speed", server.recent_speed),
                ("readrate", server.readrate),
            ] {
                if let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) {
                    status.insert(key.to_owned(), value.into());
                }
            }
            if let Some(value) = server.suspended {
                status.insert("suspended".to_owned(), value.into());
            }
            for (key, value) in [
                ("hold_reason", clipped(&server.hold_reason, 16)),
                ("playlist_shape", clipped(&server.playlist_shape, 16)),
                ("last_request", clipped(&server.last_request, 24)),
            ] {
                if let Some(value) = value {
                    status.insert(key.to_owned(), value.into());
                }
            }
            if !status.is_empty() {
                client.insert("server".to_owned(), status.into());
            }
        }
        if !client.is_empty() {
            extra.insert("client".to_owned(), client.into());
        }
    }
    if let Some(control) = ev.control.as_ref().and_then(control_context) {
        extra.insert("control_accepted_client".to_owned(), control);
    }
    if let Some(control) = ev.control_trigger.as_ref().and_then(control_context) {
        extra.insert("control_trigger_client".to_owned(), control);
    }
    let runway = ev
        .runway
        .or_else(|| snapshot.and_then(|snapshot| snapshot.runway));
    let bandwidth = ev.bandwidth.or_else(|| {
        snapshot
            .and_then(|snapshot| snapshot.observed_bitrate_bps)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| (value / 1_000.0).round().min(i64::MAX as f64) as i64)
    });
    PlaybackEvent {
        at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0),
        user_id: Some(user_id),
        session_id: clipped(&ev.session_id, 80),
        file_id: ev.file_id,
        event: clipped(&Some(ev.event.clone()), 40).unwrap_or_else(|| "event".to_owned()),
        level: clipped(&Some(ev.level.clone()), 16),
        method: clipped(&ev.method, 16),
        encoder: clipped(&ev.encoder, 32),
        height: ev.height.filter(|value| *value > 0),
        ms: ev.ms.filter(|value| *value >= 0),
        runway_ds: runway
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| (value * 10.0).round().min(i64::MAX as f64) as i64),
        bandwidth_kbps: bandwidth.filter(|value| *value > 0),
        speed_recent: server
            .and_then(|server| server.recent_speed)
            .filter(|value| value.is_finite() && *value >= 0.0),
        ahead_seconds: server
            .and_then(|server| server.ahead_seconds)
            .filter(|value| *value >= 0),
        suspended: server.and_then(|server| server.suspended),
        hold_reason: server.and_then(|server| clipped(&server.hold_reason, 16)),
        delivered_bps: server
            .and_then(|server| server.delivered_bps)
            .filter(|value| *value >= 0),
        readrate: server
            .and_then(|server| server.readrate)
            .filter(|value| value.is_finite() && *value >= 0.0),
        detail: clipped(&ev.detail, 200),
        attempt: clipped(&ev.attempt, 24),
        reason: clipped(&ev.reason, 24),
        ua: clipped(&ev.ua, 24),
        extra: (!extra.is_empty()).then(|| serde_json::Value::Object(extra).to_string()),
        ..PlaybackEvent::default()
    }
}

/// One client report as a log line.
///
/// Pure, and separate from the handler for the reason every other pure
/// decision in this codebase is: the thing worth checking here is the *naming*
/// of the measurements, and naming mistakes read perfectly and produce wrong
/// data. This one printed every event's `ms` as `ttff_ms` — so a stall's
/// duration entered the start-time series, and a grep that looked exactly
/// right returned numbers that were part start and part stall.
fn client_log_line(ev: &ClientLog, suppressed: u64) -> String {
    /// Trim and cap one field so a client can't spam oversized log lines.
    fn clip(s: &str, n: usize) -> String {
        let s = s.trim();
        match s.char_indices().nth(n) {
            Some((i, _)) => format!("{}…", &s[..i]),
            None => s.to_owned(),
        }
    }
    fn field(v: &Option<String>, n: usize) -> Option<String> {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| clip(s, n))
    }

    let event = {
        let e = clip(&ev.event, 40);
        if e.is_empty() {
            "event".to_owned()
        } else {
            e
        }
    };
    let mut line = match field(&ev.ua, 24) {
        Some(ua) => format!("client[{ua}] {event}"),
        None => format!("client {event}"),
    };
    if let Some(m) = field(&ev.method, 16) {
        line.push_str(&format!(" method={m}"));
    }
    if let Some(v) = field(&ev.vcodec, 16) {
        line.push_str(&format!(" vcodec={v}"));
    }
    if let Some(c) = ev.code {
        line.push_str(&format!(" code={c}"));
    }
    if let Some(h) = ev.height.filter(|h| *h > 0) {
        line.push_str(&format!(" height={h}"));
    }
    if let Some(e) = field(&ev.encoder, 32) {
        line.push_str(&format!(" encoder={e}"));
    }
    // Printed before the message so it sits with the other facts about the
    // client rather than in the trailing measurements: it describes the
    // machine, not this event.
    if let Some(hw) = ev.decode_hw {
        line.push_str(if hw { " decode=hw" } else { " decode=SOFTWARE" });
        if ev.decode_smooth == Some(false) {
            line.push_str("/not-smooth");
        }
    }
    let msg = clip(&ev.message, 200);
    if !msg.is_empty() {
        line.push_str(&format!(": {msg}"));
    }
    // The measurements, in a fixed order so a grep over the log ring produces
    // a column-alignable series rather than prose.
    //
    // `ms` is named for what the EVENT measured, not for the field it arrived
    // in. It used to be printed as `ttff_ms=` on every event that carried one,
    // which meant a stall's *duration* was logged as a time-to-first-frame —
    // so `grep ttff_ms` returned a distribution that was part start times and
    // part stall lengths, and the tail of it was entirely stalls. That is the
    // one number M0 exists to produce, and it was wrong in the direction that
    // makes the system look worse than it is.
    if let Some(ms) = ev.ms.filter(|v| *v >= 0) {
        let name = match ev.event.as_str() {
            "ttff" => "ttff_ms",
            "stall" => "stall_ms",
            _ => "ms",
        };
        line.push_str(&format!(" {name}={ms}"));
    }
    let snapshot = ev.snapshot.as_ref();
    let runway = ev
        .runway
        .or_else(|| snapshot.and_then(|snapshot| snapshot.runway));
    let bandwidth = ev.bandwidth.or_else(|| {
        snapshot
            .and_then(|snapshot| snapshot.observed_bitrate_bps)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| (value / 1_000.0).round().min(i64::MAX as f64) as i64)
    });
    if let Some(r) = runway.filter(|v| v.is_finite() && *v >= 0.0) {
        line.push_str(&format!(" runway={r:.1}s"));
    }
    if let Some(b) = bandwidth.filter(|v| *v > 0) {
        line.push_str(&format!(" bw={b}kbps"));
    }
    if let Some(t) = field(&ev.title, 120) {
        line.push_str(&format!(" — {t}"));
    }
    if let Some(id) = ev.file_id {
        line.push_str(&format!(" file={id}"));
    }
    if let Some(s) = field(&ev.src, 160) {
        line.push_str(&format!(" src={s}"));
    }
    if let Some(d) = field(&ev.detail, 200) {
        line.push_str(&format!(" [{d}]"));
    }
    // Attempt identity last: it's what you group by when reading back, and
    // putting it at the end keeps the front of every line comparable.
    match (field(&ev.attempt, 24), field(&ev.reason, 24)) {
        (Some(a), Some(r)) => line.push_str(&format!(" attempt={a}/{r}")),
        (Some(a), None) => line.push_str(&format!(" attempt={a}")),
        (None, Some(r)) => line.push_str(&format!(" reason={r}")),
        (None, None) => {}
    }
    if suppressed > 0 {
        line.push_str(&format!(" (+{suppressed} suppressed)"));
    }
    line
}

#[derive(Serialize)]
pub struct SettingsDto {
    /// Replicated logical name shared by every voter.
    pub server_name: String,
    pub tmdb_configured: bool,
    /// The stored TMDB key itself. This endpoint is admin-only and the key is
    /// low-sensitivity (read-only metadata), so the admin who set it can see
    /// and copy it back — the web UI masks it until clicked. Empty when unset.
    pub tmdb_api_key: String,
    pub omdb_configured: bool,
    /// The stored OMDb key (Rotten Tomatoes / Metacritic / IMDb ratings). Same
    /// admin-only, mask-until-clicked treatment as the TMDB key.
    pub omdb_api_key: String,
    /// Trakt app credentials (the admin's own API app), same treatment.
    pub trakt_configured: bool,
    pub trakt_client_id: String,
    pub trakt_client_secret: String,
    /// Where monarr lives, and the key plurxd reads its calendar with — the
    /// coming-soon rail (plan §11.2). Server-side only: plurxd proxies the
    /// call, so this key never reaches a browser. Same admin-only,
    /// mask-until-clicked treatment as the others.
    pub monarr_configured: bool,
    pub monarr_url: String,
    pub monarr_api_key: String,
    /// Push watch state to monarr. Off by default, and separate from the
    /// pair above on purpose: reading monarr's calendar and sending it your
    /// household's viewing history are very different consents.
    pub monarr_watched_sync: bool,
    /// Playback language defaults (docs/FEATURES.md §7): ISO 639 codes and the
    /// subtitle mode "auto" | "always" | "off".
    pub default_audio_lang: String,
    pub default_sub_lang: String,
    pub sub_mode: String,
    /// How fast a remux may be delivered, as a multiple of real time. "0" means
    /// unpaced — which lets a single stream take the whole link.
    pub stream_readrate: String,
    /// Requested N1 rate control. The production-effective value may be VBR
    /// when a family refuses quality mode; `/system` capabilities and boot
    /// logs carry that validation result.
    pub transcode_rate_mode: String,
    /// `None` means use the validated family-tuned default.
    pub transcode_quality: Option<u8>,
    /// How an HLS session (transcode or copy-video) is paced: the multiple of
    /// real time it settles at, how many seconds it may deliver flat-out first
    /// (that burst IS the viewer's opening buffer), and how far ahead of the
    /// playhead it may write before being suspended.
    pub hls_readrate: String,
    pub hls_burst_secs: String,
    pub hls_ahead_max_secs: String,
    /// The ahead-window's other two bounds, in bytes: one session's share and
    /// the ceiling across all of them. Safety limits rather than tuning knobs
    /// — they have no dropdown, because the answer is "big enough that you
    /// never meet it, small enough that a runaway stream cannot fill the
    /// disk" and that is a property of the disk, not a preference.
    pub hls_ahead_max_bytes: String,
    pub hls_scratch_max_bytes: String,
    /// Opt-in physical-device experiment: serve new live sessions as typeless
    /// sliding playlists from their first response. Off by default.
    pub hls_typeless_sliding: bool,
    /// VOD availability kill switch. On by default; turning it off refuses
    /// HLS session creation and never restores the removed live presentation.
    pub vod_presentation: bool,
    /// Temporary growing-HLS fallback for typed VOD prerequisite failures.
    /// On by default while the recovery feature is compiled in.
    pub vod_live_recovery: bool,
    /// Additive, behavior-neutral playback-control v1 advertisement. Off by
    /// default until clients ship passive reporters.
    pub playback_control_protocol_v1: bool,
    /// Node-wide byte budget for un-admitted VOD working sets. Empty = the
    /// built-in default. Never zero — "no working set" is not a configuration
    /// this accepts (M3 handoff §6).
    pub vod_working_set_bytes: String,
    /// Server ceiling on one blocking VOD segment fetch, seconds.
    pub vod_block_budget_secs: String,
    /// Producer watchdog from first blocked demand to bytes or typed failure.
    pub vod_materialize_budget_secs: String,
    /// Node-local fragment-index pass interval. Defaults to 15 minutes; 0 is
    /// an explicit pause, in which case an unindexed title is refused.
    pub vod_index_mins: i64,
    /// Default-off content-addressed cluster queue and peer hydration for VOD
    /// indexes. The cadence above remains the operator's I/O budget.
    pub vod_index_cluster_cache: bool,
    /// Cluster-wide opt-in for placing new HLS workers on another voter. The
    /// readiness bit is true only while the replicated flag is enabled and
    /// every committed voter publishes the current media protocol.
    pub cluster_media_pool_enabled: bool,
    pub cluster_media_pool_ready: bool,
    /// Opt-in replacement of expired HLS owners. This remains independently
    /// gated after remote placement is enabled so operators can stage rollout.
    pub cluster_session_takeover_enabled: bool,
    /// Server-wide scheduled maintenance, in minutes; 0 is off (the default).
    /// Per-library scan/refresh intervals are on the library, not here.
    pub probe_retry_mins: i64,
    /// The exception to "0 is the default": on by default, because it repairs
    /// artwork the server itself failed to fetch rather than adding a habit
    /// nobody asked for. See `keys::ARTWORK_RETRY_DEFAULT_MINS`.
    pub artwork_retry_mins: i64,
    pub transcode_cleanup_mins: i64,
    /// How often the pre-transcode producer looks for something worth making,
    /// and how much disk what it makes may occupy. Both off/0 by default: the
    /// producer competes with live playback for the encoder, so it is a thing
    /// an operator turns on, not a thing an upgrade turns on for them.
    pub cache_produce_mins: i64,
    pub cache_max_gb: i64,
    /// What the cache currently holds on this node, in bytes — the number that
    /// makes the budget above mean something.
    pub cache_used_bytes: i64,
    /// Node-local playback event retention. Default 30 days; 0 is fully off.
    pub telemetry_retain_days: i64,
    /// Use coarse, node-local playback history to seed Auto quality.
    /// Explicit opt-in; missing is false.
    pub playback_network_priors: bool,
    /// Let the web client's Auto controller change rungs after playback starts.
    /// Explicit opt-in; missing is false.
    pub playback_auto_abr: bool,
    /// App-managed offline preparation has a separate reservation budget from
    /// the opportunistic playback cache above.
    pub offline_enabled: bool,
    pub offline_max_gb: i64,
    pub offline_max_gb_per_user: i64,
    pub offline_max_rows_per_user: i64,
    /// Scan every library once, ~30s after the server starts.
    pub scan_on_startup: bool,
    /// Is the one-off genre backfill armed? It disarms itself when it reaches
    /// the end of the catalogue, so this reads `false` again afterwards.
    ///
    /// Off by default and opt-in, unlike the artwork retry: it re-hits the
    /// provider once per title because nothing stored can produce a genre
    /// (see migration v13), and an upgrade that started that on its own is
    /// the failure v9 documents.
    pub genre_backfill: bool,
    /// What the last backfill pass did, or `None` if none has run since boot.
    /// Reported here rather than in the per-library scan status because the
    /// backfill walks item ids, not libraries — and because this page is
    /// where an operator armed it, so it is where they will look for whether
    /// it worked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre_backfill_last: Option<GenreBackfillReport>,
}

async fn settings_dto(state: &AppState) -> Result<SettingsDto, ApiError> {
    // This page renders the settings as one administrative snapshot. On the
    // clustered backend, reading each field independently turns one response
    // into dozens of leader barriers; it can also mix values from different
    // commits. `cache_bytes` below is one additional authoritative aggregate
    // for this node's cache ownership when a cache location is configured.
    let settings = state.store.settings_snapshot().await?;
    let setting = |key: &str| settings.get(key).cloned();
    let server_name = setting(keys::SERVER_NAME).unwrap_or_else(|| state.server_name.clone());
    let tmdb_api_key = setting(keys::TMDB_API_KEY).unwrap_or_default();
    let omdb_api_key = setting(keys::OMDB_API_KEY).unwrap_or_default();
    let monarr_url = setting(keys::MONARR_URL).unwrap_or_default();
    let monarr_api_key = setting(keys::MONARR_API_KEY).unwrap_or_default();
    let trakt_client_id = setting(keys::TRAKT_CLIENT_ID).unwrap_or_default();
    let trakt_client_secret = setting(keys::TRAKT_CLIENT_SECRET).unwrap_or_default();
    let mut prefs = plurx_core::tracks::LangPrefs::default();
    if let Some(value) = setting(keys::AUDIO_LANG).filter(|value| !value.trim().is_empty()) {
        prefs.audio_lang = value.trim().to_owned();
    }
    if let Some(value) = setting(keys::SUB_LANG).filter(|value| !value.trim().is_empty()) {
        prefs.sub_lang = value.trim().to_owned();
    }
    if let Some(value) = setting(keys::SUB_MODE) {
        prefs.sub_mode = plurx_core::tracks::SubMode::parse(value.trim());
    }
    let stream_readrate = setting(keys::STREAM_READRATE)
        .unwrap_or_else(|| crate::http::stream::READRATE_DEFAULT.to_string());
    let transcode_rate_mode = setting(keys::TRANSCODE_RATE_MODE);
    let transcode_quality = setting(keys::TRANSCODE_QUALITY);
    let (transcode_rate_mode, transcode_quality, _) =
        crate::transcode::normalize_rate_control_request(
            transcode_rate_mode.as_deref(),
            transcode_quality.as_deref(),
        );
    let transcode_rate_mode = transcode_rate_mode.as_str().to_owned();
    let text = |v: Option<String>, default: &str| -> String {
        v.map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_owned())
    };
    let hls_readrate = text(
        setting(keys::HLS_READRATE),
        &crate::transcode::HLS_READRATE_DEFAULT.to_string(),
    );
    let hls_burst_secs = text(
        setting(keys::HLS_BURST_SECS),
        &crate::transcode::HLS_BURST_SECS_DEFAULT.to_string(),
    );
    let hls_ahead_max_secs = text(
        setting(keys::HLS_AHEAD_MAX_SECS),
        &crate::transcode::HLS_AHEAD_MAX_SECS_DEFAULT.to_string(),
    );
    let hls_ahead_max_bytes = text(
        setting(keys::HLS_AHEAD_MAX_BYTES),
        &crate::transcode::HLS_AHEAD_MAX_BYTES_DEFAULT.to_string(),
    );
    let hls_scratch_max_bytes = text(
        setting(keys::HLS_SCRATCH_MAX_BYTES),
        &crate::transcode::HLS_SCRATCH_MAX_BYTES_DEFAULT.to_string(),
    );
    let mins = |v: Option<String>| -> i64 {
        v.and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(0)
            .max(0)
    };
    let probe_retry_mins = mins(setting(keys::JOB_PROBE_RETRY_MINS));
    // Absent means the default here, not 0 — the settings page must show the
    // interval that is actually in force, or an admin reading "0" would
    // reasonably conclude nothing is retrying their artwork.
    let artwork_retry_mins = setting(keys::JOB_ARTWORK_RETRY_MINS)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(keys::ARTWORK_RETRY_DEFAULT_MINS)
        .max(0);
    let transcode_cleanup_mins = mins(setting(keys::JOB_TRANSCODE_CLEANUP_MINS));
    let cache_produce_mins = mins(setting(keys::JOB_CACHE_PRODUCE_MINS));
    let cache_max_gb = setting(keys::CACHE_MAX_GB)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(crate::cachekeep::DEFAULT_MAX_GB)
        .max(0);
    let cache_used_bytes = match state.transcode.cache_location() {
        Some((_, node)) => state.store.cache_bytes(node).await.unwrap_or(0),
        None => 0,
    };
    let telemetry_retain_days = setting(keys::TELEMETRY_RETAIN_DAYS)
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(keys::TELEMETRY_RETAIN_DEFAULT_DAYS)
        .max(0);
    let playback_network_priors =
        setting(keys::PLAYBACK_NETWORK_PRIORS).is_some_and(|value| value.trim() == "1");
    let playback_auto_abr =
        setting(keys::PLAYBACK_AUTO_ABR).is_some_and(|value| value.trim() == "1");
    let offline_enabled = !matches!(
        setting(keys::OFFLINE_ENABLED).as_deref(),
        Some("0" | "false" | "off" | "no")
    );
    let offline_integer = |value: Option<String>, default: i64| {
        value
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(default)
            .max(0)
    };
    let offline_max_gb = offline_integer(
        setting(keys::OFFLINE_MAX_GB),
        super::offline::DEFAULT_GLOBAL_GB,
    );
    let offline_max_gb_per_user = offline_integer(
        setting(keys::OFFLINE_MAX_GB_PER_USER),
        super::offline::DEFAULT_USER_GB,
    );
    let offline_max_rows_per_user = offline_integer(
        setting(keys::OFFLINE_MAX_ROWS_PER_USER),
        super::offline::DEFAULT_USER_ROWS,
    );
    let scan_on_startup = setting(keys::JOB_SCAN_ON_STARTUP).is_some_and(|v| v.trim() == "1");
    let genre_backfill = setting(keys::GENRE_BACKFILL).is_some_and(|v| v.trim() == "1");
    let cluster_media_pool_enabled =
        setting(keys::CLUSTER_MEDIA_POOL_ENABLED).as_deref() == Some("1");
    let cluster_media_pool_ready =
        cluster_media_pool_enabled && state.media_pool.remote_rollout_ready().await;
    let cluster_session_takeover_enabled =
        setting(keys::CLUSTER_SESSION_TAKEOVER_ENABLED).as_deref() == Some("1");
    Ok(SettingsDto {
        server_name,
        tmdb_configured: !tmdb_api_key.is_empty(),
        tmdb_api_key,
        omdb_configured: !omdb_api_key.is_empty(),
        omdb_api_key,
        monarr_configured: !monarr_url.is_empty() && !monarr_api_key.is_empty(),
        monarr_url,
        monarr_api_key,
        monarr_watched_sync: setting(keys::MONARR_WATCHED_SYNC).unwrap_or_default() == "1",
        trakt_configured: !trakt_client_id.is_empty() && !trakt_client_secret.is_empty(),
        trakt_client_id,
        trakt_client_secret,
        default_audio_lang: prefs.audio_lang,
        default_sub_lang: prefs.sub_lang,
        sub_mode: prefs.sub_mode.as_str().to_owned(),
        stream_readrate,
        transcode_rate_mode,
        transcode_quality,
        hls_readrate,
        hls_burst_secs,
        hls_ahead_max_secs,
        hls_ahead_max_bytes,
        hls_scratch_max_bytes,
        hls_typeless_sliding: setting(keys::HLS_TYPELESS_SLIDING)
            .is_some_and(|value| value.trim() == "1"),
        vod_presentation: setting(keys::VOD_PRESENTATION).as_deref() != Some("0"),
        vod_live_recovery: setting(keys::VOD_LIVE_RECOVERY).as_deref() != Some("0"),
        playback_control_protocol_v1: setting(keys::PLAYBACK_CONTROL_PROTOCOL_V1).as_deref()
            == Some("1"),
        vod_working_set_bytes: setting(keys::VOD_WORKING_SET_BYTES).unwrap_or_default(),
        vod_block_budget_secs: setting(keys::VOD_BLOCK_BUDGET_SECS).unwrap_or_default(),
        vod_materialize_budget_secs: setting(keys::VOD_MATERIALIZE_BUDGET_SECS).unwrap_or_default(),
        vod_index_mins: setting(keys::VOD_INDEX_MINS).map_or(15, |value| mins(Some(value))),
        vod_index_cluster_cache: setting(keys::VOD_INDEX_CLUSTER_CACHE)
            .is_some_and(|value| value.trim() == "1"),
        cluster_media_pool_enabled,
        cluster_media_pool_ready,
        cluster_session_takeover_enabled,
        probe_retry_mins,
        artwork_retry_mins,
        transcode_cleanup_mins,
        cache_produce_mins,
        cache_max_gb,
        cache_used_bytes,
        telemetry_retain_days,
        playback_network_priors,
        playback_auto_abr,
        offline_enabled,
        offline_max_gb,
        offline_max_gb_per_user,
        offline_max_rows_per_user,
        scan_on_startup,
        genre_backfill,
        genre_backfill_last: state.jobs.last_genre_backfill().await,
    })
}

/// GET /api/v1/settings (admin)
pub async fn get_settings(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<SettingsDto>, ApiError> {
    Ok(Json(settings_dto(&state).await?))
}

#[derive(Deserialize)]
pub struct UpdateSettings {
    /// Rename the logical server on every voter. Configuration is only the
    /// bootstrap seed and is not edited by this operation.
    pub server_name: Option<String>,
    /// Set the TMDB API key. Empty string clears it. Absent leaves it as-is.
    pub tmdb_api_key: Option<String>,
    /// Set the OMDb API key. Empty string clears it. Absent leaves it as-is.
    pub omdb_api_key: Option<String>,
    /// Trakt app credentials; same empty-clears semantics.
    pub trakt_client_id: Option<String>,
    pub trakt_client_secret: Option<String>,
    /// monarr pairing for the coming-soon rail; same empty-clears semantics.
    pub monarr_url: Option<String>,
    pub monarr_api_key: Option<String>,
    pub monarr_watched_sync: Option<bool>,
    /// VOD session-creation kill switch, serving budgets, and index cadence;
    /// absent leaves each as-is. Disabling refuses HLS rather than restoring
    /// the removed live engine. `vod_working_set_bytes` refuses 0.
    pub vod_presentation: Option<bool>,
    pub vod_live_recovery: Option<bool>,
    pub playback_control_protocol_v1: Option<bool>,
    pub vod_working_set_bytes: Option<String>,
    pub vod_block_budget_secs: Option<String>,
    pub vod_materialize_budget_secs: Option<String>,
    pub vod_index_mins: Option<i64>,
    pub vod_index_cluster_cache: Option<bool>,
    /// Playback language defaults. ISO 639 codes ("eng"); mode is
    /// "auto" | "always" | "off".
    pub default_audio_lang: Option<String>,
    pub default_sub_lang: Option<String>,
    pub sub_mode: Option<String>,
    /// Remux delivery pace, a multiple of real time; "0" disables the limit.
    pub stream_readrate: Option<String>,
    /// N1 requested rate-control family. Whenever either rate-control field is
    /// sent, both are required so a replicated update is one complete pair.
    /// A quality request is behavior-probed before the effective snapshot
    /// changes; a refused driver remains VBR.
    pub transcode_rate_mode: Option<String>,
    /// JSON null clears the override back to the family-tuned default.
    #[serde(default, deserialize_with = "deserialize_nullable")]
    pub transcode_quality: Option<Option<u8>>,
    /// HLS session pacing: rate (x real time, "0" unpaced), opening burst in
    /// seconds, and the ahead-of-playhead window in seconds ("0" unbounded).
    pub hls_readrate: Option<String>,
    pub hls_burst_secs: Option<String>,
    pub hls_ahead_max_secs: Option<String>,
    pub hls_ahead_max_bytes: Option<String>,
    pub hls_scratch_max_bytes: Option<String>,
    pub hls_typeless_sliding: Option<bool>,
    /// Enable remote live-session placement cluster-wide. Enabling is refused
    /// until every committed voter is freshly publishing this protocol;
    /// disabling always succeeds.
    pub cluster_media_pool_enabled: Option<bool>,
    /// Replace an expired remote HLS owner while retaining the public session
    /// id. Requires remote placement to remain enabled and rollout-ready.
    pub cluster_session_takeover_enabled: Option<bool>,
    /// Server-wide job intervals in minutes; 0 turns one off.
    pub probe_retry_mins: Option<i64>,
    pub artwork_retry_mins: Option<i64>,
    pub transcode_cleanup_mins: Option<i64>,
    pub cache_produce_mins: Option<i64>,
    pub cache_max_gb: Option<i64>,
    pub telemetry_retain_days: Option<i64>,
    pub playback_network_priors: Option<bool>,
    pub playback_auto_abr: Option<bool>,
    pub offline_enabled: Option<bool>,
    pub offline_max_gb: Option<i64>,
    pub offline_max_gb_per_user: Option<i64>,
    pub offline_max_rows_per_user: Option<i64>,
    pub scan_on_startup: Option<bool>,
    /// Arm or disarm the one-off genre backfill.
    pub genre_backfill: Option<bool>,
}

/// Preserve the distinction between an absent PATCH-style field and an
/// explicit JSON null. Serde's ordinary `Option<Option<T>>` collapses both;
/// the harness needs null to restore an originally-unset quality override.
fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// PUT /api/v1/settings (admin)
pub async fn update_settings(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<UpdateSettings>,
) -> Result<Json<SettingsDto>, ApiError> {
    if let Some(name) = &req.server_name {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::BadRequest("server_name must not be empty".into()));
        }
        state.store.put_setting(keys::SERVER_NAME, name).await?;
    }
    if req.cluster_media_pool_enabled == Some(true)
        && !state.media_pool.remote_rollout_ready().await
    {
        return Err(ApiError::Conflict(
            "cluster media placement cannot be enabled until every committed voter is reachable and publishing the current media protocol".into(),
        ));
    }
    if req.cluster_session_takeover_enabled == Some(true) {
        let media_pool_enabled = match req.cluster_media_pool_enabled {
            Some(enabled) => enabled,
            None => {
                state
                    .store
                    .get_setting(keys::CLUSTER_MEDIA_POOL_ENABLED)
                    .await?
                    .as_deref()
                    == Some("1")
            }
        };
        if !media_pool_enabled || !state.media_pool.remote_rollout_ready().await {
            return Err(ApiError::Conflict(
                "cluster media session takeover requires remote placement to be enabled and every committed voter to publish the current media protocol".into(),
            ));
        }
    }
    match (&req.transcode_rate_mode, req.transcode_quality) {
        (None, None) => {}
        (Some(requested_mode), Some(quality)) => {
            let mode = plurx_core::transcode::RateMode::parse(requested_mode).ok_or_else(|| {
                ApiError::BadRequest("transcode_rate_mode must be bitrate or quality".into())
            })?;
            match state
                .transcode
                .apply_rate_control_settings(mode, quality)
                .await
            {
                Ok(()) => {}
                Err(crate::transcode::ApplyRateControlError::Store(error)) => {
                    return Err(error.into())
                }
                Err(crate::transcode::ApplyRateControlError::Busy) => {
                    return Err(ApiError::Conflict(
                        "rate-control validation deferred while playback, offline/speculative encoding, or encoder capacity is active; retry after the node is idle".into(),
                    ))
                }
            }
        }
        _ => {
            return Err(ApiError::BadRequest(
                "transcode_rate_mode and transcode_quality must be provided together".into(),
            ));
        }
    }
    let pairs: [(&str, &Option<String>); 8] = [
        (keys::TMDB_API_KEY, &req.tmdb_api_key),
        (keys::OMDB_API_KEY, &req.omdb_api_key),
        (keys::TRAKT_CLIENT_ID, &req.trakt_client_id),
        (keys::TRAKT_CLIENT_SECRET, &req.trakt_client_secret),
        (keys::MONARR_URL, &req.monarr_url),
        (keys::MONARR_API_KEY, &req.monarr_api_key),
        (keys::AUDIO_LANG, &req.default_audio_lang),
        (keys::SUB_LANG, &req.default_sub_lang),
    ];
    for (key, value) in pairs {
        if let Some(value) = value {
            // The monarr URL is canonicalized on the way in, not at each use
            // site: a bare `monarr:7676` or `host.docker.internal` is what a
            // person types, and reqwest answers a schemeless address with
            // "builder error" — a message about our HTTP client rather than
            // about their setting. Storing the completed form means every
            // consumer agrees on it and the settings screen shows back
            // exactly what plurx will dial.
            let value = if key == keys::MONARR_URL {
                super::comingsoon::normalize_monarr_url(value)
            } else {
                value.trim().to_owned()
            };
            state.store.put_setting(key, &value).await?;
        }
    }
    if let Some(on) = req.monarr_watched_sync {
        state
            .store
            .put_setting(keys::MONARR_WATCHED_SYNC, if on { "1" } else { "0" })
            .await?;
    }
    if let Some(on) = req.hls_typeless_sliding {
        state
            .store
            .put_setting(keys::HLS_TYPELESS_SLIDING, if on { "1" } else { "0" })
            .await?;
    }
    if let Some(on) = req.vod_presentation {
        state
            .store
            .put_setting(keys::VOD_PRESENTATION, if on { "1" } else { "0" })
            .await?;
    }
    if let Some(on) = req.vod_live_recovery {
        state
            .store
            .put_setting(keys::VOD_LIVE_RECOVERY, if on { "1" } else { "0" })
            .await?;
    }
    if let Some(on) = req.playback_control_protocol_v1 {
        state
            .store
            .put_setting(
                keys::PLAYBACK_CONTROL_PROTOCOL_V1,
                if on { "1" } else { "0" },
            )
            .await?;
    }
    if let Some(on) = req.vod_index_cluster_cache {
        state
            .store
            .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, if on { "1" } else { "0" })
            .await?;
    }
    if let Some(raw) = req
        .vod_working_set_bytes
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    {
        let parsed: u64 = raw
            .trim()
            .parse()
            .map_err(|_| ApiError::BadRequest("vod_working_set_bytes must be a number".into()))?;
        // A parsed zero is refused rather than stored: 0 means "not
        // configured" to the serving layer, and an operator who typed it
        // meant "no working set" — an answer this presentation cannot run
        // with. Offer the honest alternatives instead of silently keeping a
        // default (M3 handoff §6).
        if parsed == 0 {
            return Err(ApiError::BadRequest(
                "vod_working_set_bytes cannot be 0: the smallest accepted working set is \
                 268435456 (256 MiB); disabling VOD refuses HLS playback and does not restore \
                 the removed live presentation"
                    .into(),
            ));
        }
        if parsed < 256 * 1024 * 1024 {
            return Err(ApiError::BadRequest(
                "vod_working_set_bytes must be at least 268435456 (256 MiB)".into(),
            ));
        }
        state
            .store
            .put_setting(keys::VOD_WORKING_SET_BYTES, &parsed.to_string())
            .await?;
    }
    if req
        .vod_working_set_bytes
        .as_deref()
        .is_some_and(|raw| raw.trim().is_empty())
    {
        // Empty resets to the built-in default — this is the one numeric
        // setting that refuses 0, so it needs an explicit way back.
        state
            .store
            .put_setting(keys::VOD_WORKING_SET_BYTES, "")
            .await?;
    }
    if let Some(raw) = req
        .vod_block_budget_secs
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    {
        let parsed: f64 = raw
            .trim()
            .parse()
            .map_err(|_| ApiError::BadRequest("vod_block_budget_secs must be a number".into()))?;
        if !(1.0..=30.0).contains(&parsed) {
            return Err(ApiError::BadRequest(
                "vod_block_budget_secs must be between 1 and 30".into(),
            ));
        }
        state
            .store
            .put_setting(keys::VOD_BLOCK_BUDGET_SECS, &parsed.to_string())
            .await?;
    }
    if req
        .vod_block_budget_secs
        .as_deref()
        .is_some_and(|raw| raw.trim().is_empty())
    {
        state
            .store
            .put_setting(keys::VOD_BLOCK_BUDGET_SECS, "")
            .await?;
    }
    if let Some(raw) = req
        .vod_materialize_budget_secs
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    {
        let parsed: f64 = raw.trim().parse().map_err(|_| {
            ApiError::BadRequest("vod_materialize_budget_secs must be a number".into())
        })?;
        if !(10.0..=300.0).contains(&parsed) {
            return Err(ApiError::BadRequest(
                "vod_materialize_budget_secs must be between 10 and 300".into(),
            ));
        }
        state
            .store
            .put_setting(keys::VOD_MATERIALIZE_BUDGET_SECS, &parsed.to_string())
            .await?;
    }
    if req
        .vod_materialize_budget_secs
        .as_deref()
        .is_some_and(|raw| raw.trim().is_empty())
    {
        state
            .store
            .put_setting(keys::VOD_MATERIALIZE_BUDGET_SECS, "")
            .await?;
    }
    if let Some(on) = req.cluster_media_pool_enabled {
        state
            .store
            .put_setting(keys::CLUSTER_MEDIA_POOL_ENABLED, if on { "1" } else { "0" })
            .await?;
        if !on {
            state
                .store
                .put_setting(keys::CLUSTER_SESSION_TAKEOVER_ENABLED, "0")
                .await?;
        }
    }
    if let Some(on) = req.cluster_session_takeover_enabled {
        state
            .store
            .put_setting(
                keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
                if on { "1" } else { "0" },
            )
            .await?;
    }
    if let Some(mode) = &req.sub_mode {
        // Normalize through the parser so only valid modes are stored.
        let mode = plurx_core::tracks::SubMode::parse(mode.trim()).as_str();
        state.store.put_setting(keys::SUB_MODE, mode).await?;
    }
    if let Some(rate) = &req.stream_readrate {
        // Store only something the streamer can act on. A garbled value would
        // otherwise fall back to the default silently and leave the settings
        // page showing a number that isn't in force.
        let parsed: f64 = rate
            .trim()
            .parse()
            .map_err(|_| ApiError::BadRequest("stream_readrate must be a number".into()))?;
        if !(0.0..=1000.0).contains(&parsed) {
            return Err(ApiError::BadRequest(
                "stream_readrate must be between 0 and 1000".into(),
            ));
        }
        // Below real time the client can never buffer and playback stalls by
        // construction; refuse rather than let someone quietly break streaming.
        if parsed > 0.0 && parsed < 1.0 {
            return Err(ApiError::BadRequest(
                "stream_readrate below 1.0 cannot keep up with playback; use 0 to disable pacing"
                    .into(),
            ));
        }
        state
            .store
            .put_setting(keys::STREAM_READRATE, &parsed.to_string())
            .await?;
    }
    // HLS pacing. Same "store only what the streamer can act on" rule as the
    // remux rate, with per-key bounds: a rate below real time cannot keep up
    // by construction, a burst is seconds of content (an hour of it is not a
    // burst), and the ahead-window is what stops a 4K session filling the
    // disk — an enormous one is the same as none, so say so rather than
    // silently accept it.
    for (key, label, value, max) in [
        (
            keys::HLS_READRATE,
            "hls_readrate",
            &req.hls_readrate,
            1000.0,
        ),
        (
            keys::HLS_BURST_SECS,
            "hls_burst_secs",
            &req.hls_burst_secs,
            600.0,
        ),
        (
            keys::HLS_AHEAD_MAX_SECS,
            "hls_ahead_max_secs",
            &req.hls_ahead_max_secs,
            3600.0,
        ),
        // Bytes. The ceiling is generous on purpose: this is a guard against a
        // runaway stream, not a quota, and refusing a large disk would be the
        // setting telling the operator they are wrong about their own hardware.
        (
            keys::HLS_AHEAD_MAX_BYTES,
            "hls_ahead_max_bytes",
            &req.hls_ahead_max_bytes,
            1024.0 * 1024.0 * 1024.0 * 1024.0,
        ),
        (
            keys::HLS_SCRATCH_MAX_BYTES,
            "hls_scratch_max_bytes",
            &req.hls_scratch_max_bytes,
            1024.0 * 1024.0 * 1024.0 * 1024.0,
        ),
    ] {
        let Some(raw) = value else { continue };
        let parsed: f64 = raw
            .trim()
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("{label} must be a number")))?;
        if !(0.0..=max).contains(&parsed) {
            return Err(ApiError::BadRequest(format!(
                "{label} must be between 0 and {max:.0}"
            )));
        }
        if key == keys::HLS_READRATE && parsed > 0.0 && parsed < 1.0 {
            return Err(ApiError::BadRequest(
                "hls_readrate below 1.0 cannot keep up with playback; use 0 to disable pacing"
                    .into(),
            ));
        }
        state.store.put_setting(key, &parsed.to_string()).await?;
    }
    // Job intervals: 0 = off, otherwise a floor of 15 minutes, matching the
    // per-library schedule. The scheduler ticks once a minute, so anything
    // shorter would be a lie dressed as a setting.
    for (key, label, value) in [
        (
            keys::JOB_PROBE_RETRY_MINS,
            "probe_retry_mins",
            req.probe_retry_mins,
        ),
        (
            keys::JOB_ARTWORK_RETRY_MINS,
            "artwork_retry_mins",
            req.artwork_retry_mins,
        ),
        (
            keys::JOB_TRANSCODE_CLEANUP_MINS,
            "transcode_cleanup_mins",
            req.transcode_cleanup_mins,
        ),
        (
            keys::JOB_CACHE_PRODUCE_MINS,
            "cache_produce_mins",
            req.cache_produce_mins,
        ),
        (keys::VOD_INDEX_MINS, "vod_index_mins", req.vod_index_mins),
    ] {
        if let Some(value) = value {
            if value < 0 || (value > 0 && value < 15) {
                return Err(ApiError::BadRequest(format!(
                    "{label} must be 0 (off) or at least 15 minutes"
                )));
            }
            state.store.put_setting(key, &value.to_string()).await?;
        }
    }
    if let Some(gb) = req.cache_max_gb {
        // Ten terabytes is not a policy so much as a typo guard: the field is
        // in gigabytes, and somebody entering bytes would set a budget no disk
        // can reach — which reads as eviction being broken.
        if !(0..=10_240).contains(&gb) {
            return Err(ApiError::BadRequest(
                "cache_max_gb must be between 0 (off) and 10240".into(),
            ));
        }
        state
            .store
            .put_setting(keys::CACHE_MAX_GB, &gb.to_string())
            .await?;
    }
    if let Some(days) = req.telemetry_retain_days {
        if !(0..=3650).contains(&days) {
            return Err(ApiError::BadRequest(
                "telemetry_retain_days must be between 0 (off) and 3650".into(),
            ));
        }
        state
            .store
            .put_setting(keys::TELEMETRY_RETAIN_DAYS, &days.to_string())
            .await?;
    }
    if let Some(enabled) = req.playback_network_priors {
        state
            .store
            .put_setting(
                keys::PLAYBACK_NETWORK_PRIORS,
                if enabled { "1" } else { "0" },
            )
            .await?;
    }
    if let Some(enabled) = req.playback_auto_abr {
        state
            .store
            .put_setting(keys::PLAYBACK_AUTO_ABR, if enabled { "1" } else { "0" })
            .await?;
    }
    for (key, label, value) in [
        (keys::OFFLINE_MAX_GB, "offline_max_gb", req.offline_max_gb),
        (
            keys::OFFLINE_MAX_GB_PER_USER,
            "offline_max_gb_per_user",
            req.offline_max_gb_per_user,
        ),
    ] {
        if let Some(gb) = value {
            if !(0..=10_240).contains(&gb) {
                return Err(ApiError::BadRequest(format!(
                    "{label} must be between 0 (disables offline admission) and 10240"
                )));
            }
            state.store.put_setting(key, &gb.to_string()).await?;
        }
    }
    if let Some(rows) = req.offline_max_rows_per_user {
        if !(0..=10_000).contains(&rows) {
            return Err(ApiError::BadRequest(
                "offline_max_rows_per_user must be between 0 (disables offline admission) and 10000"
                    .into(),
            ));
        }
        state
            .store
            .put_setting(keys::OFFLINE_MAX_ROWS_PER_USER, &rows.to_string())
            .await?;
    }
    if let Some(on) = req.offline_enabled {
        state
            .store
            .put_setting(keys::OFFLINE_ENABLED, if on { "1" } else { "0" })
            .await?;
        if !on {
            state.offline.cancel_all().await;
        }
    }
    if let Some(on) = req.scan_on_startup {
        state
            .store
            .put_setting(keys::JOB_SCAN_ON_STARTUP, if on { "1" } else { "0" })
            .await?;
    }
    if let Some(on) = req.genre_backfill {
        // Arming rewinds the cursor. A pass that finished left it at 0 and
        // disarmed itself, so this normally changes nothing; it matters for
        // the operator who disarms a run half way and arms it again later,
        // expecting the titles it already failed on to be retried rather than
        // skipped forever because the cursor is past them.
        if on {
            state
                .store
                .put_setting(keys::GENRE_BACKFILL_CURSOR, "0")
                .await?;
        }
        state
            .store
            .put_setting(keys::GENRE_BACKFILL, if on { "1" } else { "0" })
            .await?;
        tracing::info!(armed = on, "genre backfill");
    }
    Ok(Json(settings_dto(&state).await?))
}

/// GET /api/v1/scan/status — per-library scan status (keyed by library id).
/// Any authenticated user may look; scans aren't a secret, but strangers
/// shouldn't see filesystem paths in problem messages.
pub async fn scan_status(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Json<HashMap<i64, ScanStatus>> {
    Json(state.jobs.all_statuses().await)
}

/// One thing the server is doing right now. Deliberately generic — future
/// task kinds (file moves, renames, backups) reuse the same shape and the
/// same global indicator in every client.
#[derive(Serialize)]
pub struct Activity {
    /// Machine-readable kind: scan, enrich, stream, or offline work.
    pub kind: &'static str,
    /// Short human label, e.g. "Scanning Movies".
    pub label: String,
    /// Optional detail, e.g. "412 of 3801 files".
    pub detail: Option<String>,
    /// 0–100 when a meaningful percentage exists.
    pub percent: Option<u8>,
}

/// Result of the optional clustered half of an activity read. Ordinary
/// SQLite and never-joined installs never construct this work, while an
/// opted-in cluster keeps transport failures as data instead of turning the
/// whole page into an error.
enum PeerActivityRead {
    LocalOnly,
    Peers(Vec<(String, PeerActivityOutcome)>),
    DirectoryUnavailable,
}

#[derive(Serialize)]
struct ActivityNodeStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<String>,
    status: &'static str,
}

#[derive(Serialize)]
struct ClusterDelivery {
    method: String,
    presentation: Option<String>,
    user: String,
    file_id: i64,
    item_id: i64,
    title: String,
    started_unix: i64,
    idle_seconds: u64,
    session_id: Option<String>,
    delivered_bytes: Option<i64>,
    delivered_bps: Option<i64>,
    node_id: String,
}

impl ClusterDelivery {
    fn local(delivery: Delivery, node_id: &str) -> Self {
        Self {
            method: delivery.method.to_owned(),
            presentation: delivery.presentation.map(str::to_owned),
            user: delivery.user,
            file_id: delivery.file_id,
            item_id: delivery.item_id,
            title: delivery.title,
            started_unix: delivery.started_unix,
            idle_seconds: delivery.idle_seconds,
            session_id: delivery.session_id,
            delivered_bytes: delivery.delivered_bytes,
            delivered_bps: delivery.delivered_bps,
            node_id: node_id.to_owned(),
        }
    }

    fn peer(delivery: ActivityDelivery, node_id: String) -> Self {
        Self {
            method: delivery.method,
            presentation: delivery.presentation,
            user: delivery.user,
            file_id: delivery.file_id,
            item_id: delivery.item_id,
            title: delivery.title,
            started_unix: delivery.started_unix,
            idle_seconds: delivery.idle_seconds,
            session_id: None,
            delivered_bytes: delivery.delivered_bytes,
            delivered_bps: delivery.delivered_bps,
            node_id,
        }
    }
}

/// The roster's machine names, for a reader allowed to see them.
///
/// Admin-only, matching `GET /api/v1/cluster/nodes`: machine names are an
/// operator fact, and the activity page must not become the one place an
/// ordinary household member can read the fleet's hostnames. A household
/// member keeps the node id they are already shown.
///
/// A roster read that fails costs the page its labels, never the page.
async fn node_hostnames(state: &AppState, is_admin: bool) -> BTreeMap<String, String> {
    if !is_admin {
        return BTreeMap::new();
    }
    match state.membership.node_hostnames().await {
        Ok(hostnames) => hostnames,
        Err(error) => {
            tracing::warn!(?error, "node hostnames unavailable for activity");
            BTreeMap::new()
        }
    }
}

async fn peer_activity(state: &AppState) -> PeerActivityRead {
    // `advertise_host` is the explicit first step toward peer membership.
    // Keeping this guard outside `snapshots()` means the overwhelmingly common
    // SQLite and never-joined paths do not even read the peer directory.
    if !state.cluster_advertisement || !state.membership.is_replicated() {
        return PeerActivityRead::LocalOnly;
    }
    match state.peer_activity.snapshots().await {
        Ok(peers) if peers.is_empty() => PeerActivityRead::LocalOnly,
        Ok(peers) => PeerActivityRead::Peers(peers),
        Err(_) => PeerActivityRead::DirectoryUnavailable,
    }
}

fn peer_status(outcome: &PeerActivityOutcome) -> &'static str {
    match outcome {
        PeerActivityOutcome::Answered(_) => "answered",
        PeerActivityOutcome::Unhealthy => "unhealthy",
        PeerActivityOutcome::Unreachable => "unreachable",
        PeerActivityOutcome::TimedOut => "timed_out",
        PeerActivityOutcome::InvalidResponse => "invalid_response",
    }
}

fn activity_nodes(local_node_id: &str, peers: &PeerActivityRead) -> Vec<ActivityNodeStatus> {
    let mut nodes = vec![ActivityNodeStatus {
        node_id: Some(local_node_id.to_owned()),
        status: "answered",
    }];
    match peers {
        PeerActivityRead::Peers(outcomes) => nodes.extend(outcomes.iter().map(
            |(node_id, outcome)| ActivityNodeStatus {
                node_id: Some(node_id.clone()),
                status: peer_status(outcome),
            },
        )),
        PeerActivityRead::DirectoryUnavailable => nodes.push(ActivityNodeStatus {
            node_id: None,
            status: "unavailable",
        }),
        PeerActivityRead::LocalOnly => {}
    }
    nodes
}

fn clustered_deliveries(
    local_node_id: &str,
    local: Vec<Delivery>,
    peers: &PeerActivityRead,
) -> Vec<ClusterDelivery> {
    let mut out = local
        .into_iter()
        .map(|delivery| ClusterDelivery::local(delivery, local_node_id))
        .collect::<Vec<_>>();
    if let PeerActivityRead::Peers(outcomes) = peers {
        for (node_id, outcome) in outcomes {
            let PeerActivityOutcome::Answered(snapshot) = outcome else {
                continue;
            };
            out.extend(
                snapshot
                    .deliveries
                    .iter()
                    .cloned()
                    .map(|delivery| ClusterDelivery::peer(delivery, node_id.clone())),
            );
        }
    }
    out.sort_by(|left, right| {
        right
            .started_unix
            .cmp(&left.started_unix)
            .then(left.method.cmp(&right.method))
            .then(left.file_id.cmp(&right.file_id))
            .then(left.user.cmp(&right.user))
            .then(left.node_id.cmp(&right.node_id))
    });
    out
}

fn missing_activity_summary(peers: &PeerActivityRead) -> Option<String> {
    match peers {
        PeerActivityRead::LocalOnly => None,
        PeerActivityRead::DirectoryUnavailable => {
            Some("cluster peer directory did not answer".to_owned())
        }
        PeerActivityRead::Peers(outcomes) => {
            let missing = outcomes
                .iter()
                .filter(|(_, outcome)| !matches!(outcome, PeerActivityOutcome::Answered(_)))
                .collect::<Vec<_>>();
            if missing.is_empty() {
                return None;
            }
            let shown = missing
                .iter()
                .take(3)
                .map(|(node_id, outcome)| format!("{node_id} ({})", peer_status(outcome)))
                .collect::<Vec<_>>()
                .join(", ");
            let remainder = missing.len().saturating_sub(3);
            Some(if remainder == 0 {
                format!("{shown} did not answer")
            } else {
                format!("{shown}, and {remainder} more did not answer")
            })
        }
    }
}

#[derive(Clone, Serialize)]
struct OfflineWork {
    id: String,
    /// `prepare` or `send`; neither is a playback session.
    kind: &'static str,
    user: String,
    file_id: i64,
    item_id: Option<i64>,
    title: String,
    state: String,
    phase: String,
    target_height: i64,
    percent: Option<u8>,
    bytes_sent: Option<u64>,
    bytes_total: i64,
    started_unix: i64,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

async fn offline_work(state: &AppState) -> Result<Vec<OfflineWork>, ApiError> {
    let now = now_unix();
    let rows = state
        .store
        // Lease touches are deliberately throttled to one SQLite write per
        // minute. Fetch a 65-second candidate window, then use the exact
        // process-local response meter below to decide whether it is sending
        // now; this preserves the write bound without a false 40-second gap.
        .offline_activity_packages(&state.node_id, now, now.saturating_sub(65), 50)
        .await?;
    let rows: Vec<_> = rows
        .into_iter()
        .filter_map(|row| {
            let transfer_bytes = row
                .lease_active
                .then(|| state.offline.transfer_bytes(&row.package.id))
                .flatten();
            (row.package.state != "ready" || transfer_bytes.is_some())
                .then_some((row, transfer_bytes))
        })
        .collect();
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut work = Vec::with_capacity(rows.len());
    for (row, transfer_bytes) in rows {
        let item_id = row.item_id;
        let title = row.title;
        let user = row.user_name;
        let package = row.package;
        work.push(OfflineWork {
            id: package.id.clone(),
            kind: if transfer_bytes.is_some() {
                "send"
            } else {
                "prepare"
            },
            user,
            file_id: package.file_id,
            item_id,
            title,
            state: package.state,
            phase: package.phase,
            target_height: package.target_height,
            percent: (package.progress_millis > 0)
                .then_some(((package.progress_millis.clamp(0, 1_000) * 100) / 1_000) as u8),
            bytes_sent: transfer_bytes,
            bytes_total: package.actual_bytes.unwrap_or(package.estimated_bytes),
            started_unix: package.created_at,
        });
    }
    Ok(work)
}

fn activity_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MB", bytes.div_ceil(MIB))
    }
}

/// GET /api/v1/activity — everything in flight, for the always-visible
/// indicator in the app header. Empty array = the server is idle.
pub async fn activity(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Activity>>, ApiError> {
    if !state.cluster_advertisement || !state.membership.is_replicated() {
        return Ok(Json(local_activity(&state).await?));
    }

    // Local Store work and the bounded peer round overlap. A healthy peer can
    // never add another sequential wave to the page, and the common two-second
    // peer deadline remains the only added clustered wait.
    let (activities, peers) = tokio::join!(local_activity(&state), peer_activity(&state));
    let mut activities = activities?;
    if matches!(peers, PeerActivityRead::LocalOnly) {
        return Ok(Json(activities));
    }

    let remote = match &peers {
        PeerActivityRead::Peers(outcomes) => outcomes
            .iter()
            .filter_map(|(_, outcome)| match outcome {
                PeerActivityOutcome::Answered(snapshot) => Some(snapshot.deliveries.len()),
                _ => None,
            })
            .sum::<usize>(),
        PeerActivityRead::LocalOnly | PeerActivityRead::DirectoryUnavailable => 0,
    };
    let local = state.transcode.active_sessions().await
        + state.streams.list().len()
        + state.direct_plays.list().len();
    let streams = local.saturating_add(remote);

    // The historical local HLS-only summary would double count clustered
    // streams. Replace it with the complete direct/remux/HLS total while
    // retaining the established scan -> stream -> background-work ordering.
    activities.retain(|activity| activity.kind != "stream");
    if streams > 0 {
        let insert_at = activities
            .iter()
            .take_while(|activity| matches!(activity.kind, "scan" | "enrich"))
            .count();
        activities.insert(
            insert_at,
            Activity {
                kind: "stream",
                label: if streams == 1 {
                    "1 active stream".to_owned()
                } else {
                    format!("{streams} active streams")
                },
                detail: None,
                percent: None,
            },
        );
    }
    if let Some(detail) = missing_activity_summary(&peers) {
        activities.insert(
            0,
            Activity {
                kind: "cluster_degraded",
                label: "Activity incomplete".to_owned(),
                detail: Some(detail),
                percent: None,
            },
        );
    }
    Ok(Json(activities))
}

async fn local_activity(state: &AppState) -> Result<Vec<Activity>, ApiError> {
    let mut activities = Vec::new();

    let mut statuses: Vec<_> = state
        .jobs
        .all_statuses()
        .await
        .into_iter()
        .filter(|(_, s)| s.running)
        .collect();
    statuses.sort_by_key(|(id, _)| *id);
    let names: HashMap<i64, String> = if statuses.is_empty() {
        HashMap::new()
    } else {
        state
            .store
            .list_libraries()
            .await?
            .into_iter()
            .map(|l| (l.id, l.name))
            .collect()
    };
    for (id, status) in statuses {
        let name = names.get(&id).cloned().unwrap_or_else(|| format!("#{id}"));
        let enriching = status.phase.as_deref() == Some("enriching");
        let (kind, label) = if enriching {
            ("enrich", format!("Fetching metadata for {name}"))
        } else {
            ("scan", format!("Scanning {name}"))
        };
        let (detail, percent) = match status.progress.filter(|_| !enriching) {
            Some(p) if p.found > 0 => (
                Some(format!("{} of {} files", p.processed, p.found)),
                Some(((p.processed * 100 / p.found).min(100)) as u8),
            ),
            _ => (None, None),
        };
        activities.push(Activity {
            kind,
            label,
            detail,
            percent,
        });
    }

    let streams = state.transcode.active_sessions().await;
    if streams > 0 {
        activities.push(Activity {
            kind: "stream",
            label: if streams == 1 {
                "1 active stream".to_owned()
            } else {
                format!("{streams} active streams")
            },
            detail: None,
            percent: None,
        });
    }

    // The pre-transcode pass. It is the only background job that holds an
    // encoder for hours, and it was the only one that never said so: an admin
    // who found a busy ffmpeg had no way, from inside plurx, to learn what it
    // was or why it had chosen that file.
    if let Some(p) = state.jobs.producing_now().await {
        activities.push(Activity {
            kind: "produce",
            label: format!("Pre-transcoding {}", p.title),
            detail: Some(format!("{} · {} of {}", p.reason, p.index, p.total)),
            // Titles done, not percent of this encode — the encoder does not
            // report a percentage and inventing one would be a lie that reads
            // like a measurement.
            percent: None,
        });
    }

    for work in offline_work(state).await? {
        let sending = work.kind == "send";
        activities.push(Activity {
            kind: if sending {
                "offline_send"
            } else {
                "offline_prepare"
            },
            label: if sending {
                format!("Sending offline · {}", work.title)
            } else {
                format!("Preparing offline · {}", work.title)
            },
            detail: if sending {
                Some(format!(
                    "{} / {}",
                    activity_bytes(work.bytes_sent.unwrap_or(0)),
                    activity_bytes(work.bytes_total.max(0) as u64)
                ))
            } else if work.state == "queued" {
                Some(format!("Waiting for encoder · {}p", work.target_height))
            } else {
                Some(format!(
                    "{} · {}p",
                    work.phase.replace('_', " "),
                    work.target_height
                ))
            },
            percent: (!sending).then_some(work.percent).flatten(),
        });
    }

    if let Some((label, detail)) = state.trakt.activity().await {
        activities.push(Activity {
            kind: "trakt",
            label,
            detail,
            percent: None,
        });
    }

    Ok(activities)
}

/// One live delivery, whatever route it takes to the screen.
///
/// A new array rather than fields grafted onto `sessions`: that array means
/// "HLS sessions" to every native client parsing it today, and two of the four
/// rows here are not sessions at all.
#[derive(Serialize)]
pub struct Delivery {
    /// `direct` · `remux` · `hls-copy` · `transcode`.
    pub method: &'static str,
    /// Present for HLS: `vod` or `live-recovery`.
    pub presentation: Option<&'static str>,
    /// Who is watching. Exactly what `sessions[].user_name` has always
    /// carried on this endpoint — see the handler's note on who may look. This
    /// array names no one a `sessions` row would not have named.
    pub user: String,
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
    pub started_unix: i64,
    /// Seconds since this delivery last showed a sign of life: `last_access`
    /// for a session, the last byte handed over for a remux, and for a direct
    /// play the last range request or progress beacon — which is the clock the
    /// idle expiry runs on.
    pub idle_seconds: u64,
    /// One-way correlation for the `sessions` row this stream belongs to.
    /// The raw HLS capability is never exposed through diagnostics.
    pub session_id: Option<String>,
    /// Bytes handed to this client, where anything counts them. Direct play
    /// counts nothing: `serve_file_range` hands a file to axum and never sees
    /// the bytes leave, and metering it would mean wrapping every 206 body.
    pub delivered_bytes: Option<i64>,
    pub delivered_bps: Option<i64>,
}

/// Everything currently being delivered, from the three places that know.
///
/// Deliberately a join at read time rather than one registry. The two session
/// registries own exact lifetimes — a reap, a `StreamGuard` drop — and their
/// locks disagree by necessity: `TranscodeManager` uses an async mutex because
/// its holders await, `progressive::Streams` a sync one because a `Drop`
/// deregisters and a `Drop` cannot await. Merging them would have forced one
/// answer onto the other. Here, in an async handler, both can simply be read.
async fn deliveries(state: &AppState) -> (Vec<crate::transcode::SessionInfo>, Vec<Delivery>) {
    let sessions = state.transcode.list_deliveries().await;
    let mut out: Vec<Delivery> = sessions
        .iter()
        .map(|(s, method)| Delivery {
            method: method.as_str(),
            presentation: Some(s.presentation),
            user: s.user_name.clone(),
            file_id: s.file_id,
            item_id: s.item_id,
            title: s.item_title.clone(),
            started_unix: s.started_unix,
            idle_seconds: s.idle_seconds,
            session_id: Some(crate::transcode::session_log_id(&s.id)),
            delivered_bytes: Some(s.delivered_bytes),
            delivered_bps: s.delivered_bps,
        })
        .collect();

    // The two routes that hold no session. A remux carries the item id already
    // loaded at stream start, while titles are resolved here so they do not go
    // stale against a rename.
    let mut titles: HashMap<i64, String> = HashMap::new();
    for stream in state.streams.list() {
        out.push(Delivery {
            method: crate::delivery::Method::Remux.as_str(),
            presentation: None,
            user: stream.user_name,
            file_id: stream.file_id,
            item_id: stream.item_id,
            title: title_of(state, stream.item_id, &mut titles).await,
            started_unix: stream.started_unix,
            // A remux is one pipe with no `last_access` of its own; how long
            // since a byte left it is the same question.
            idle_seconds: (stream.delivered_idle_ms.max(0) / 1_000) as u64,
            session_id: None,
            delivered_bytes: Some(stream.delivered_bytes),
            delivered_bps: stream.delivered_bps,
        });
    }
    for play in state.direct_plays.list() {
        out.push(Delivery {
            method: crate::delivery::Method::Direct.as_str(),
            presentation: None,
            user: play.user_name,
            file_id: play.file_id,
            item_id: play.item_id,
            title: title_of(state, play.item_id, &mut titles).await,
            started_unix: play.started_unix,
            idle_seconds: play.idle_seconds,
            session_id: None,
            delivered_bytes: None,
            delivered_bps: None,
        });
    }
    // Newest first, with a total order so a two-second poll does not reshuffle
    // rows that started in the same second.
    out.sort_by(|a, b| {
        b.started_unix
            .cmp(&a.started_unix)
            .then(a.method.cmp(b.method))
            .then(a.file_id.cmp(&b.file_id))
            .then(a.user.cmp(&b.user))
    });
    let sessions = sessions
        .into_iter()
        .map(|(mut session, _)| {
            session.id = crate::transcode::session_log_id(&session.id);
            session
        })
        .collect();
    (sessions, out)
}

/// An item's title, read once per request however many rows want it.
async fn title_of(state: &AppState, item_id: i64, seen: &mut HashMap<i64, String>) -> String {
    if let Some(title) = seen.get(&item_id) {
        return title.clone();
    }
    let title = state
        .store
        .get_item(item_id)
        .await
        .ok()
        .flatten()
        .map(|i| i.title)
        // A file whose item was deleted mid-play is still a delivery in
        // progress; the row belongs on the page with an honest gap in it
        // rather than being dropped for want of a name.
        .unwrap_or_default();
    seen.insert(item_id, title.clone());
    title
}

/// GET /api/v1/activity/detail — the activity page: live playback sessions,
/// per-library scan state, and the Trakt sync story, all in one shape. Any
/// authenticated user may look (it's their household server); the stop action
/// below is admin-only.
pub async fn activity_detail(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // `sessions` is untouched — native clients parse it — and `deliveries` is
    // the superset beside it: the same HLS sessions plus the two routes that
    // were never listed at all.
    //
    // The roster's machine names ride in the same wave rather than following
    // it. This page polls every few seconds; a name read that waits for the
    // peer fan-out to finish would add its latency to every poll for nothing.
    let replicated = state.cluster_advertisement && state.membership.is_replicated();
    let ((sessions, deliveries), peers, hostnames) = if replicated {
        let (local, peers, hostnames) = tokio::join!(
            deliveries(&state),
            peer_activity(&state),
            node_hostnames(&state, user.0.is_admin)
        );
        (local, peers, hostnames)
    } else {
        (
            deliveries(&state).await,
            PeerActivityRead::LocalOnly,
            BTreeMap::new(),
        )
    };
    let clustered = !matches!(peers, PeerActivityRead::LocalOnly);
    let deliveries = if clustered {
        serde_json::to_value(clustered_deliveries(&state.node_id, deliveries, &peers))
            .map_err(|error| ApiError::Internal(error.to_string()))?
    } else {
        serde_json::to_value(deliveries).map_err(|error| ApiError::Internal(error.to_string()))?
    };
    let offline = offline_work(&state).await?;
    let statuses = state.jobs.all_statuses().await;
    let names: HashMap<i64, String> = if statuses.is_empty() {
        HashMap::new()
    } else {
        state
            .store
            .list_libraries()
            .await?
            .into_iter()
            .map(|l| (l.id, l.name))
            .collect()
    };
    let scans: Vec<serde_json::Value> = statuses
        .into_iter()
        .map(|(id, st)| {
            serde_json::json!({
                "library_id": id,
                "library": names.get(&id).cloned().unwrap_or_else(|| format!("#{id}")),
                "status": st,
            })
        })
        .collect();
    let trakt = state.trakt.status(0).await; // page shows server-wide state
    let linked = state
        .store
        .list_trakt_auth()
        .await?
        .into_iter()
        .next()
        .map(|a| {
            serde_json::json!({
                "trakt_username": a.trakt_username,
                "last_sync_at": (a.last_sync_at > 0).then_some(a.last_sync_at),
            })
        });
    let mut response = serde_json::json!({
        "sessions": sessions,
        "deliveries": deliveries,
        "offline": offline,
        "scans": scans,
        "producing": state.jobs.producing_now().await,
        "trakt": {
            "configured": trakt.configured,
            "linked": linked,
            "syncing": trakt.syncing,
            "note": trakt.note,
        },
    });
    if clustered {
        response["activity_nodes"] = serde_json::to_value(activity_nodes(&state.node_id, &peers))
            .map_err(|error| ApiError::Internal(error.to_string()))?;
    }
    // Node ids are stable but name nothing: an operator reading this page
    // cannot tell which machine `5deeeebc-…` is. The roster already knows every
    // node's short hostname, so send the id -> hostname map once per response
    // and let the page label its rows from it.
    //
    // Present for every clustered admin read even when it is empty, so the
    // field's presence answers "may this reader see machine names" and nothing
    // else. Making an empty roster look identical to a refused one would leave
    // the gate untestable from the wire.
    if clustered && user.0.is_admin {
        response["node_hostnames"] = serde_json::to_value(&hostnames)
            .map_err(|error| ApiError::Internal(error.to_string()))?;
    }
    // Analysis is an operator concern: keep it out of ordinary household
    // responses, but make it a first-class part of the admin Activity page.
    // This is folded into the existing page read rather than making the web
    // client add a second polling loop beside /activity/detail.
    if user.0.is_admin {
        response["analysis"] = match super::analysis::activity_summary(&state).await {
            Ok(summary) => summary,
            Err(error) => {
                tracing::warn!(?error, "analysis summary unavailable for activity");
                serde_json::json!({
                    "available": false,
                    "enabled": state.jobs.analysis_queue_enabled().await,
                })
            }
        };
    }
    Ok(Json(response))
}

/// DELETE /api/v1/activity/producer (admin) — stop the pre-transcode pass.
///
/// It stops after the title it is on rather than mid-encode: the producer
/// resumes from published segment boundaries, so a clean stop keeps the part
/// it has already made and a kill throws it away. The next scheduled pass
/// picks up from there.
pub async fn stop_producer(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.jobs.stop_producing() {
        return Err(ApiError::NotFound("producer"));
    }
    Ok(Json(
        serde_json::json!({ "ok": true, "note": "stopping after the current title" }),
    ))
}

/// DELETE /api/v1/activity/sessions/:id (admin) — stop a transcode session.
pub async fn stop_session(
    _admin: AdminUser,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut candidates = state.transcode.active_session_ids().await;
    // VOD sessions live in their own registry; without them here the
    // Terminal::AdminStop arm is unreachable from its only intended caller.
    candidates.extend(state.transcode.vod_live_session_ids().await);
    let session_id = candidates
        .into_iter()
        .find(|session_id| session_id == &id || crate::transcode::session_log_id(session_id) == id);
    let Some(session_id) = session_id else {
        return Err(ApiError::NotFound("session"));
    };
    let status = super::hls::release_with_terminal(
        state,
        session_id,
        crate::vodserve::Terminal::AdminStop,
        "stopped by admin",
    )
    .await;
    if status != StatusCode::NO_CONTENT {
        return Err(if status == StatusCode::SERVICE_UNAVAILABLE {
            ApiError::ServiceUnavailable(
                "the session stop is still being durably reconciled; retry shortly".to_owned(),
            )
        } else {
            ApiError::Internal(format!("unexpected session release status {status}"))
        });
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// DELETE /api/v1/activity/offline/:id (admin) — cancel one visible package.
pub async fn stop_offline_package(
    _admin: AdminUser,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = now_unix();
    let package = state
        .store
        .offline_activity_packages(&state.node_id, now, now.saturating_sub(65), 50)
        .await?
        .into_iter()
        .map(|row| row.package)
        .find(|package| package.id == id)
        .ok_or(ApiError::NotFound("offline package"))?;
    state.offline.cancel(&id).await;
    if !state
        .store
        .delete_offline_package(&id, package.user_id)
        .await?
    {
        return Err(ApiError::NotFound("offline package"));
    }
    state.offline.record_cancellation(&package);
    state.offline.forget_transfer(&id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Store-free substate available to the unauthenticated Prometheus handler.
/// Its fields expose only process-local counters and the background sample;
/// the handler cannot reach `AppState::store` through this type.
#[derive(Clone)]
pub(crate) struct MetricsState {
    started_at: Instant,
    transcode: crate::transcode::TranscodeMetrics,
    integration: Arc<IntegrationMetrics>,
    offline: Arc<crate::offline::OfflineMetrics>,
    store_metrics: StoreMetricsCache,
    passive_raft: plurx_core::cluster::migration::status::PassiveRaftMetrics,
    passive_membership: plurx_core::cluster::membership::PassiveMembershipMetrics,
}

impl FromRef<AppState> for MetricsState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            started_at: state.started_at,
            transcode: state.transcode.metrics_handle(),
            integration: state.jobs.metrics_handle(),
            offline: state.offline.metrics_handle(),
            store_metrics: state.store_metrics.clone(),
            passive_raft: state.replication.metrics_handle(),
            passive_membership: state.membership.metrics_handle(),
        }
    }
}

fn render_passive_membership_metrics(
    view: plurx_core::cluster::membership::PassiveMembershipMetricsView,
) -> String {
    let mut out = format!(
        "# HELP plurx_cluster_replicated Whether this process is configured as a replicated cluster node.\n\
         # TYPE plurx_cluster_replicated gauge\n\
         plurx_cluster_replicated {}\n\
         # HELP plurx_cluster_membership_sample_valid Whether the cached membership and heartbeat sample is present and fresh.\n\
         # TYPE plurx_cluster_membership_sample_valid gauge\n\
         plurx_cluster_membership_sample_valid {}\n\
         # HELP plurx_cluster_membership_sample_errors_total Failed membership sample or local heartbeat attempts.\n\
         # TYPE plurx_cluster_membership_sample_errors_total counter\n\
         plurx_cluster_membership_sample_errors_total {}\n",
        u8::from(view.replicated),
        u8::from(view.valid),
        view.errors,
    );
    if let Some(age) = view.age_seconds {
        out.push_str(&format!(
            "# HELP plurx_cluster_membership_sample_age_seconds Age of the last complete membership sample.\n\
             # TYPE plurx_cluster_membership_sample_age_seconds gauge\n\
             plurx_cluster_membership_sample_age_seconds {age}\n"
        ));
    }
    let Some(sample) = view.sample else {
        return out;
    };
    let quorum_required = sample.voters / 2 + 1;
    let heartbeat_quorum_available = sample.heartbeat_fresh_voters >= quorum_required;
    out.push_str(&format!(
        "# HELP plurx_cluster_nodes Committed cluster nodes by role and heartbeat freshness.\n\
         # TYPE plurx_cluster_nodes gauge\n\
         plurx_cluster_nodes{{role=\"voter\",heartbeat=\"fresh\"}} {}\n\
         plurx_cluster_nodes{{role=\"voter\",heartbeat=\"stale\"}} {}\n\
         plurx_cluster_nodes{{role=\"learner\",heartbeat=\"all\"}} {}\n\
         # HELP plurx_cluster_quorum_required Voters required to form a Raft majority.\n\
         # TYPE plurx_cluster_quorum_required gauge\n\
         plurx_cluster_quorum_required {quorum_required}\n\
         # HELP plurx_cluster_heartbeat_quorum_available Whether heartbeat-fresh voters currently meet the majority count.\n\
         # TYPE plurx_cluster_heartbeat_quorum_available gauge\n\
         plurx_cluster_heartbeat_quorum_available {}\n\
         # HELP plurx_cluster_local_is_voter Whether this process is in the committed voter set.\n\
         # TYPE plurx_cluster_local_is_voter gauge\n\
         plurx_cluster_local_is_voter {}\n\
         # HELP plurx_cluster_removals_pending Durable membership-removal fences awaiting resolution.\n\
         # TYPE plurx_cluster_removals_pending gauge\n\
         plurx_cluster_removals_pending {}\n",
        sample.heartbeat_fresh_voters,
        sample.heartbeat_stale_voters,
        sample.learners,
        u8::from(heartbeat_quorum_available),
        u8::from(sample.local_is_voter),
        sample.removals_pending,
    ));
    out
}

fn render_passive_raft_metrics(
    view: plurx_core::cluster::migration::status::PassiveRaftMetricsView,
) -> String {
    let mut out = format!(
        "# HELP plurx_raft_metric_sample_valid Whether the named Raft sample is present and fresh.\n\
         # TYPE plurx_raft_metric_sample_valid gauge\n\
         plurx_raft_metric_sample_valid{{source=\"local\"}} {}\n\
         plurx_raft_metric_sample_valid{{source=\"watermark\"}} {}\n\
         # HELP plurx_raft_metric_sample_errors_total Rejected or closed samples from the named Raft source.\n\
         # TYPE plurx_raft_metric_sample_errors_total counter\n\
         plurx_raft_metric_sample_errors_total{{source=\"local\"}} {}\n\
         plurx_raft_metric_sample_errors_total{{source=\"watermark\"}} {}\n\
         # HELP plurx_raft_leader_changes_total Distinct known-leader changes observed by this process.\n\
         # TYPE plurx_raft_leader_changes_total counter\n\
         plurx_raft_leader_changes_total {}\n\
         # HELP plurx_raft_local_read_protocol_supported Whether the watermark source supports this binary's bounded local-read protocol.\n\
         # TYPE plurx_raft_local_read_protocol_supported gauge\n\
         plurx_raft_local_read_protocol_supported {}\n",
        u8::from(view.valid),
        u8::from(view.watermark_valid),
        view.errors,
        view.watermark_errors,
        view.leader_changes,
        u8::from(view.watermark_local_reads_supported),
    );
    if view.age_seconds.is_some() || view.watermark_age_millis.is_some() {
        out.push_str(
            "# HELP plurx_raft_metric_sample_age_seconds Age of the last successful sample from the named Raft source.\n\
             # TYPE plurx_raft_metric_sample_age_seconds gauge\n",
        );
        if let Some(age) = view.age_seconds {
            out.push_str(&format!(
                "plurx_raft_metric_sample_age_seconds{{source=\"local\"}} {age}\n"
            ));
        }
        if let Some(age_millis) = view.watermark_age_millis {
            out.push_str(&format!(
                "plurx_raft_metric_sample_age_seconds{{source=\"watermark\"}} {}.{:03}\n",
                age_millis / 1_000,
                age_millis % 1_000,
            ));
        }
    }
    if let Some(sample) = view.sample {
        out.push_str(&format!(
            "# HELP plurx_raft_current_term Current term observed from the local Raft watch.\n\
             # TYPE plurx_raft_current_term gauge\n\
             plurx_raft_current_term {}\n\
             # HELP plurx_raft_leader_known Whether the local Raft watch currently identifies a leader.\n\
             # TYPE plurx_raft_leader_known gauge\n\
             plurx_raft_leader_known {}\n\
             # HELP plurx_raft_is_leader Whether this process is the leader in the observed term.\n\
             # TYPE plurx_raft_is_leader gauge\n\
             plurx_raft_is_leader {}\n",
            sample.current_term,
            u8::from(sample.leader_known),
            u8::from(sample.is_leader),
        ));
        if let Some(index) = sample.last_applied_index {
            out.push_str(&format!(
                "# HELP plurx_raft_applied_index Last log index applied by this local state machine.\n\
                 # TYPE plurx_raft_applied_index gauge\n\
                 plurx_raft_applied_index {index}\n"
            ));
        }
    }
    if let Some(watermark) = view.watermark {
        out.push_str(&format!(
            "# HELP plurx_raft_commit_index Most recent quorum-confirmed database commit watermark.\n\
             # TYPE plurx_raft_commit_index gauge\n\
             plurx_raft_commit_index {}\n",
            watermark.committed_index,
        ));
        if let Some(lag) = watermark.apply_lag_entries {
            out.push_str(&format!(
                "# HELP plurx_raft_apply_lag_entries Quorum watermark minus the local applied index.\n\
                 # TYPE plurx_raft_apply_lag_entries gauge\n\
                 plurx_raft_apply_lag_entries {lag}\n"
            ));
        }
    }
    if let Some(snapshot) = view.snapshot_metrics {
        render_snapshot_metrics(&mut out, snapshot);
    }
    out
}

fn render_snapshot_metrics(out: &mut String, snapshot: DbSnapshotMetricsSnapshot) {
    out.push_str(
        "# HELP plurx_raft_snapshot_seconds Database Raft snapshot build and install duration.\n\
         # TYPE plurx_raft_snapshot_seconds histogram\n",
    );
    for (operation, outcome, histogram) in [
        ("build", "ok", snapshot.build_ok),
        ("build", "error", snapshot.build_error),
        ("install", "ok", snapshot.install_ok),
        ("install", "error", snapshot.install_error),
    ] {
        for (bound_nanos, count) in DB_SNAPSHOT_HISTOGRAM_BOUNDS_NANOS
            .iter()
            .zip(histogram.cumulative_buckets)
        {
            let label = prometheus_seconds_label(*bound_nanos);
            out.push_str(&format!(
                "plurx_raft_snapshot_seconds_bucket{{operation=\"{operation}\",outcome=\"{outcome}\",le=\"{label}\"}} {count}\n"
            ));
        }
        out.push_str(&format!(
            "plurx_raft_snapshot_seconds_bucket{{operation=\"{operation}\",outcome=\"{outcome}\",le=\"+Inf\"}} {}\n\
             plurx_raft_snapshot_seconds_sum{{operation=\"{operation}\",outcome=\"{outcome}\"}} {}.{:09}\n\
             plurx_raft_snapshot_seconds_count{{operation=\"{operation}\",outcome=\"{outcome}\"}} {}\n",
            histogram.count,
            histogram.sum_nanos / 1_000_000_000,
            histogram.sum_nanos % 1_000_000_000,
            histogram.count,
        ));
    }
}

fn prometheus_seconds_label(nanos: u64) -> String {
    let whole = nanos / 1_000_000_000;
    let remainder = nanos % 1_000_000_000;
    if remainder == 0 {
        return whole.to_string();
    }
    let mut fraction = format!("{remainder:09}");
    while fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole}.{fraction}")
}

fn render_store_metrics(view: StoreMetricsView) -> String {
    let mut out = format!(
        "# HELP plurx_store_metrics_sample_valid Whether the Store-backed gauge sample is present and fresh.\n\
         # TYPE plurx_store_metrics_sample_valid gauge\n\
         plurx_store_metrics_sample_valid {}\n\
         # HELP plurx_store_metrics_sample_errors_total Failed Store-backed gauge samples.\n\
         # TYPE plurx_store_metrics_sample_errors_total counter\n\
         plurx_store_metrics_sample_errors_total {}\n",
        u8::from(view.valid),
        view.errors,
    );
    if let Some(age) = view.age_seconds {
        out.push_str(&format!(
            "# HELP plurx_store_metrics_sample_age_seconds Age of the last complete Store-backed gauge sample.\n\
             # TYPE plurx_store_metrics_sample_age_seconds gauge\n\
             plurx_store_metrics_sample_age_seconds {age}\n"
        ));
    }
    let Some(sample) = view.sample else {
        return out;
    };
    let offline = sample.offline;
    let (pending, ok, failed) = sample.watched_outbox;
    out.push_str(&format!(
        "# HELP plurx_libraries_total Configured libraries.\n\
         # TYPE plurx_libraries_total gauge\n\
         plurx_libraries_total {}\n\
         # HELP plurx_users_total Registered users.\n\
         # TYPE plurx_users_total gauge\n\
         plurx_users_total {}\n\
         # HELP plurx_offline_packages Durable offline packages by state.\n\
         # TYPE plurx_offline_packages gauge\n\
         plurx_offline_packages{{state=\"queued\"}} {}\n\
         plurx_offline_packages{{state=\"preparing\"}} {}\n\
         plurx_offline_packages{{state=\"ready\"}} {}\n\
         plurx_offline_packages{{state=\"failed\"}} {}\n\
         # HELP plurx_offline_bytes Package bytes by state, actual when ready and reserved otherwise.\n\
         # TYPE plurx_offline_bytes gauge\n\
         plurx_offline_bytes{{state=\"queued\"}} {}\n\
         plurx_offline_bytes{{state=\"preparing\"}} {}\n\
         plurx_offline_bytes{{state=\"ready\"}} {}\n\
         plurx_offline_bytes{{state=\"failed\"}} {}\n\
         # HELP plurx_offline_active_leases Unexpired leases for ready offline packages.\n\
         # TYPE plurx_offline_active_leases gauge\n\
         plurx_offline_active_leases {}\n\
         # HELP plurx_cache_pinned_bytes Completed cache bytes protected by offline packages.\n\
         # TYPE plurx_cache_pinned_bytes gauge\n\
         plurx_cache_pinned_bytes{{reason=\"offline\"}} {}\n\
         # HELP plurx_watched_outbox Watched notifications queued for monarr, by state.\n\
         # TYPE plurx_watched_outbox gauge\n\
         plurx_watched_outbox{{status=\"pending\"}} {pending}\n\
         plurx_watched_outbox{{status=\"ok\"}} {ok}\n\
         plurx_watched_outbox{{status=\"failed\"}} {failed}\n",
        sample.libraries,
        sample.users,
        offline.queued,
        offline.preparing,
        offline.ready,
        offline.failed,
        offline.queued_bytes,
        offline.preparing_bytes,
        offline.ready_bytes,
        offline.failed_bytes,
        offline.active_leases,
        offline.pinned_bytes,
    ));
    out
}

/// GET /metrics — Prometheus text exposition (unauthenticated; counts only).
pub(crate) async fn metrics(
    State(state): State<MetricsState>,
) -> impl axum::response::IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let (sessions, active_cache_entries) = state.transcode.snapshot();
    let store_metrics = render_store_metrics(state.store_metrics.snapshot());
    let raft_metrics = render_passive_raft_metrics(state.passive_raft.snapshot());
    let membership_metrics = render_passive_membership_metrics(state.passive_membership.snapshot());
    let process_metrics = format!(
        "# HELP plurx_cache_protected_entries Cache entries protected from housekeeping by active playback.\n\
         # TYPE plurx_cache_protected_entries gauge\n\
         plurx_cache_protected_entries{{reason=\"active_playback\"}} {active_cache_entries}\n{}{}{}",
        state.offline.prometheus(),
        plurx_core::store::prometheus_store_operations(),
        // Stays at zero on a healthy node and on an unclustered one. It moves
        // only when this process declined the cluster's singleton work because
        // it could not read committed membership — a state nothing else in
        // this exposition would show.
        plurx_core::cluster::membership::prometheus_cluster_job_authority(),
    );

    // Integration counters (plan P6). Scans by what asked for them, and how
    // many times another application has called in at all — the pair that
    // answers "is the fast path actually being used, or is the scheduled
    // sweep quietly carrying everything?".
    let (by_trigger, notifications) = state.integration.snapshot();
    let mut scans = String::from(
        "# HELP plurx_scan_total Library scans started, by what asked for one.\n\
         # TYPE plurx_scan_total counter\n",
    );
    for (trigger, count) in by_trigger {
        scans.push_str(&format!(
            "plurx_scan_total{{trigger=\"{trigger}\"}} {count}\n"
        ));
    }
    scans.push_str(&format!(
        "# HELP plurx_notify_received_total Scan requests received from other applications.\n\
         # TYPE plurx_notify_received_total counter\n\
         plurx_notify_received_total {notifications}\n"
    ));

    let body = format!(
        "# HELP plurx_build_info Build information.\n\
         # TYPE plurx_build_info gauge\n\
         plurx_build_info{{version=\"{version}\",build=\"{build}\"}} 1\n\
         # HELP plurx_uptime_seconds Seconds since this node started.\n\
         # TYPE plurx_uptime_seconds gauge\n\
         plurx_uptime_seconds {uptime}\n\
         # HELP plurx_transcode_sessions_active Live transcode sessions.\n\
         # TYPE plurx_transcode_sessions_active gauge\n\
         plurx_transcode_sessions_active {sessions}\n\
         {scans}{store_metrics}{membership_metrics}{raft_metrics}{process_metrics}{takeover_metrics}{control_metrics}{playback_metrics}",
        version = crate::version::SEMVER,
        build = crate::version::BUILD,
        takeover_metrics = crate::media_sessions::prometheus(),
        control_metrics = crate::playback_control::prometheus(),
        playback_metrics = crate::telemetry::prometheus(),
    );
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn test_delivery(method: &'static str, started_unix: i64) -> Delivery {
        Delivery {
            method,
            presentation: Some("live-recovery"),
            user: "paul".to_owned(),
            file_id: started_unix,
            item_id: started_unix + 100,
            title: format!("Title {started_unix}"),
            started_unix,
            idle_seconds: 2,
            session_id: Some(format!("session-{started_unix}")),
            delivered_bytes: Some(4_096),
            delivered_bps: Some(8_000),
        }
    }

    #[test]
    fn clustered_activity_attributes_answered_rows_and_names_missing_nodes() {
        let peers = PeerActivityRead::Peers(vec![
            (
                "node-b".to_owned(),
                PeerActivityOutcome::Answered(crate::http::internal_activity::ActivitySnapshot {
                    node_id: "node-b".to_owned(),
                    deliveries: vec![ActivityDelivery {
                        method: "direct".to_owned(),
                        presentation: None,
                        user: "viewer".to_owned(),
                        file_id: 2,
                        item_id: 102,
                        title: "Remote title".to_owned(),
                        started_unix: 2,
                        idle_seconds: 1,
                        delivered_bytes: None,
                        delivered_bps: None,
                    }],
                }),
            ),
            ("node-c".to_owned(), PeerActivityOutcome::TimedOut),
            ("node-d".to_owned(), PeerActivityOutcome::Unhealthy),
        ]);

        let rows = serde_json::to_value(clustered_deliveries(
            "node-a",
            vec![test_delivery("transcode", 1)],
            &peers,
        ))
        .expect("cluster deliveries serialize");
        let rows = rows.as_array().expect("delivery array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["node_id"], "node-b");
        assert_eq!(rows[0]["method"], "direct");
        assert!(rows[0]["session_id"].is_null());
        assert_eq!(rows[1]["node_id"], "node-a");
        assert_eq!(rows[1]["session_id"], "session-1");

        let nodes =
            serde_json::to_value(activity_nodes("node-a", &peers)).expect("node status serializes");
        assert_eq!(nodes[0]["status"], "answered");
        assert_eq!(nodes[1]["node_id"], "node-b");
        assert_eq!(nodes[1]["status"], "answered");
        assert_eq!(nodes[2]["status"], "timed_out");
        assert_eq!(nodes[3]["status"], "unhealthy");

        let missing = missing_activity_summary(&peers).expect("missing-node summary");
        assert!(missing.contains("node-c (timed_out)"), "{missing}");
        assert!(missing.contains("node-d (unhealthy)"), "{missing}");
        assert!(missing.ends_with("did not answer"), "{missing}");
    }

    #[test]
    fn failed_peer_directory_is_visible_without_exposing_an_address() {
        let peers = PeerActivityRead::DirectoryUnavailable;
        let nodes =
            serde_json::to_value(activity_nodes("node-a", &peers)).expect("node status serializes");
        assert_eq!(nodes[1]["status"], "unavailable");
        assert!(nodes[1].get("node_id").is_none());
        let summary = missing_activity_summary(&peers).expect("directory failure summary");
        assert_eq!(summary, "cluster peer directory did not answer");
        assert!(!summary.contains("http"));
    }

    #[test]
    fn snapshot_histogram_labels_are_derived_from_every_exported_bound() {
        let labels = DB_SNAPSHOT_HISTOGRAM_BOUNDS_NANOS.map(prometheus_seconds_label);
        assert_eq!(
            labels,
            [
                "0.001", "0.0025", "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1",
                "5", "10", "30", "60", "120", "300",
            ]
        );
    }

    #[test]
    fn a_roster_read_costs_the_page_its_labels_and_never_the_page() {
        let source = include_str!("system.rs");
        let reader = source
            .split_once("async fn node_hostnames(state: &AppState")
            .expect("the roster reader")
            .1
            .split_once("\nasync fn peer_activity(")
            .expect("function after node_hostnames")
            .0;
        // A household member is refused before the roster is touched at all.
        let refused = reader.find("if !is_admin {").expect("the admin gate");
        let read = reader
            .find("state.membership.node_hostnames().await")
            .expect("the roster read");
        assert!(refused < read);
        // No `?`: a roster that will not answer must not turn the page into an
        // error, and must not be reported as an empty roster either.
        assert!(!reader.contains("node_hostnames().await?"));
        assert!(reader.contains("Err(error) =>"));
        assert!(reader.contains("tracing::warn!"));
        // The cheap accessor, not the full membership status.
        assert!(!reader.contains("membership.status()"));
    }

    #[test]
    fn machine_names_reach_the_activity_page_only_for_an_admin() {
        let source = include_str!("system.rs");
        let handler = source
            .split_once("pub async fn activity_detail(")
            .expect("activity handler")
            .1
            .split_once("\n/// DELETE /api/v1/activity/producer")
            .expect("handler after activity_detail")
            .0;
        // `GET /api/v1/cluster/nodes` is `AdminUser`. This page is `AuthUser`,
        // so an ungated map here would make the activity page the one place an
        // ordinary household member can read the fleet's hostnames.
        //
        // Asserted as "the branch this line is inside", not "a gate appears
        // somewhere above": an ungated `if clustered {` added immediately
        // before the publish would satisfy the weaker form while leaking.
        assert_eq!(
            handler.matches("response[\"node_hostnames\"]").count(),
            1,
            "one publication site, or this test reasons about the wrong one"
        );
        let published = handler
            .find("response[\"node_hostnames\"]")
            .expect("the activity page is sent the roster's machine names");
        let enclosing = handler[..published]
            .rfind("if clustered")
            .expect("the publication is inside a clustered branch");
        assert!(
            handler[enclosing..].starts_with("if clustered && user.0.is_admin {"),
            "the branch the map is published from is not the admin branch"
        );
    }

    #[test]
    fn prometheus_scrape_has_no_store_operation() {
        let source = include_str!("system.rs");
        let handler = source
            .split_once("pub(crate) async fn metrics")
            .expect("metrics handler")
            .1
            .split_once("#[cfg(test)]")
            .expect("metrics test module")
            .0;
        let compact = handler
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(compact.starts_with("(State(state):State<MetricsState>"));
        assert!(!compact.contains("AppState"));
        assert!(!compact.contains(".store."));
        assert!(!compact.contains("Store>"));
        assert!(!compact.contains("Client"));
        assert!(!compact.contains("ReplicationMonitor"));
        assert!(!compact.contains("LocalDbRaft"));

        let substate = source
            .split_once("pub(crate) struct MetricsState")
            .expect("metrics substate")
            .1
            .split_once("impl FromRef<AppState> for MetricsState")
            .expect("metrics substate conversion")
            .0;
        assert!(!substate.contains("Manager"));
        assert!(!substate.contains("Arc<dyn Store>"));
        assert!(!substate.contains("Client"));
        assert!(!substate.contains("ReplicationMonitor"));
        assert!(!substate.contains("LocalDbRaft"));
    }

    #[test]
    fn absent_and_stale_store_samples_are_explicit_in_exposition() {
        let absent = render_store_metrics(StoreMetricsView {
            sample: None,
            age_seconds: None,
            valid: false,
            errors: 2,
        });
        assert!(absent.contains("plurx_store_metrics_sample_valid 0"));
        assert!(absent.contains("plurx_store_metrics_sample_errors_total 2"));
        assert!(!absent.contains("plurx_store_metrics_sample_age_seconds "));
        assert!(!absent.contains("plurx_libraries_total"));

        let stale = render_store_metrics(StoreMetricsView {
            sample: Some(plurx_core::store::PrometheusStoreSnapshot {
                libraries: 3,
                users: 4,
                ..Default::default()
            }),
            age_seconds: Some(121),
            valid: false,
            errors: 3,
        });
        assert!(stale.contains("plurx_store_metrics_sample_valid 0"));
        assert!(stale.contains("plurx_store_metrics_sample_age_seconds 121"));
        assert!(stale.contains("plurx_libraries_total 3"));
        assert!(stale.contains("plurx_users_total 4"));
    }

    #[test]
    fn membership_exposition_reports_quorum_without_node_identity_labels() {
        use plurx_core::cluster::membership::{
            MembershipMetricsSample, PassiveMembershipMetricsView,
        };

        let rendered = render_passive_membership_metrics(PassiveMembershipMetricsView {
            replicated: true,
            valid: true,
            age_seconds: Some(3),
            errors: 2,
            sample: Some(MembershipMetricsSample {
                voters: 4,
                learners: 1,
                heartbeat_fresh_voters: 3,
                heartbeat_stale_voters: 1,
                removals_pending: 1,
                local_is_voter: true,
            }),
        });
        assert!(rendered.contains("plurx_cluster_replicated 1"));
        assert!(rendered.contains("plurx_cluster_membership_sample_valid 1"));
        assert!(rendered.contains("plurx_cluster_membership_sample_age_seconds 3"));
        assert!(rendered.contains("plurx_cluster_membership_sample_errors_total 2"));
        assert!(rendered.contains("plurx_cluster_nodes{role=\"voter\",heartbeat=\"fresh\"} 3"));
        assert!(rendered.contains("plurx_cluster_nodes{role=\"voter\",heartbeat=\"stale\"} 1"));
        assert!(rendered.contains("plurx_cluster_quorum_required 3"));
        assert!(rendered.contains("plurx_cluster_heartbeat_quorum_available 1"));
        assert!(rendered.contains("plurx_cluster_local_is_voter 1"));
        assert!(rendered.contains("plurx_cluster_removals_pending 1"));
        assert!(!rendered.contains("node_id"));
        assert!(!rendered.contains("hostname"));
    }

    #[test]
    fn raft_exposition_is_fixed_and_privacy_safe() {
        use plurx_core::cluster::migration::status::{
            PassiveRaftMetricsView, PassiveRaftSample, QuorumWatermarkSample,
        };
        let zero_snapshot_histogram = DbSnapshotHistogram {
            count: 0,
            sum_nanos: 0,
            cumulative_buckets: [0; 16],
        };

        let rendered = render_passive_raft_metrics(PassiveRaftMetricsView {
            local_source: true,
            sample: Some(PassiveRaftSample {
                node_id: 1,
                current_term: 7,
                leader_id: Some(1),
                last_applied_index: Some(42),
                leader_known: true,
                is_leader: true,
            }),
            age_seconds: Some(2),
            valid: true,
            errors: 3,
            leader_changes: 4,
            watermark_source: true,
            watermark_requires_local_binding: true,
            watermark: Some(QuorumWatermarkSample {
                committed_index: 45,
                apply_lag_entries: Some(3),
            }),
            watermark_age_millis: Some(250),
            watermark_valid: true,
            watermark_local_reads_supported: true,
            watermark_errors: 5,
            snapshot_metrics: Some(DbSnapshotMetricsSnapshot {
                build_ok: DbSnapshotHistogram {
                    count: 2,
                    sum_nanos: 1_250_000_000,
                    cumulative_buckets: [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2],
                },
                build_error: zero_snapshot_histogram,
                install_ok: zero_snapshot_histogram,
                install_error: zero_snapshot_histogram,
                last_build: None,
                last_install: None,
            }),
        });

        assert!(rendered.contains("plurx_raft_metric_sample_valid{source=\"local\"} 1"));
        assert!(rendered.contains("plurx_raft_metric_sample_age_seconds{source=\"local\"} 2"));
        assert!(rendered.contains("plurx_raft_metric_sample_errors_total{source=\"local\"} 3"));
        assert!(rendered.contains("plurx_raft_current_term 7"));
        assert!(rendered.contains("plurx_raft_applied_index 42"));
        assert!(rendered.contains("plurx_raft_leader_known 1"));
        assert!(rendered.contains("plurx_raft_is_leader 1"));
        assert!(rendered.contains("plurx_raft_leader_changes_total 4"));
        assert!(rendered.contains("plurx_raft_metric_sample_valid{source=\"watermark\"} 1"));
        assert!(
            rendered.contains("plurx_raft_metric_sample_age_seconds{source=\"watermark\"} 0.250")
        );
        assert!(rendered.contains("plurx_raft_metric_sample_errors_total{source=\"watermark\"} 5"));
        assert!(rendered.contains("plurx_raft_commit_index 45"));
        assert!(rendered.contains("plurx_raft_apply_lag_entries 3"));
        assert!(rendered.contains("plurx_raft_local_read_protocol_supported 1"));
        assert!(rendered.contains(
            "plurx_raft_snapshot_seconds_bucket{operation=\"build\",outcome=\"ok\",le=\"0.1\"} 1"
        ));
        assert!(rendered.contains(
            "plurx_raft_snapshot_seconds_bucket{operation=\"build\",outcome=\"ok\",le=\"+Inf\"} 2"
        ));
        assert!(rendered.contains(
            "plurx_raft_snapshot_seconds_sum{operation=\"build\",outcome=\"ok\"} 1.250000000"
        ));
        assert!(!rendered.contains("node_id"));
        assert!(!rendered.contains("leader_id"));

        let stale = render_passive_raft_metrics(PassiveRaftMetricsView {
            watermark_valid: false,
            watermark_age_millis: Some(1_001),
            watermark: Some(QuorumWatermarkSample {
                committed_index: 45,
                apply_lag_entries: None,
            }),
            ..PassiveRaftMetricsView {
                local_source: true,
                sample: Some(PassiveRaftSample {
                    node_id: 2,
                    current_term: 7,
                    leader_id: Some(1),
                    last_applied_index: Some(42),
                    leader_known: true,
                    is_leader: false,
                }),
                age_seconds: Some(1),
                valid: true,
                errors: 0,
                leader_changes: 0,
                watermark_source: true,
                watermark_requires_local_binding: true,
                watermark: None,
                watermark_age_millis: None,
                watermark_valid: false,
                watermark_local_reads_supported: true,
                watermark_errors: 1,
                snapshot_metrics: None,
            }
        });
        assert!(stale.contains("plurx_raft_metric_sample_valid{source=\"watermark\"} 0"));
        assert!(stale.contains("plurx_raft_commit_index 45"));
        assert!(!stale.contains("plurx_raft_apply_lag_entries"));

        let absent = render_passive_raft_metrics(PassiveRaftMetricsView {
            local_source: false,
            sample: None,
            age_seconds: None,
            valid: false,
            errors: 0,
            leader_changes: 0,
            watermark_source: false,
            watermark_requires_local_binding: false,
            watermark: None,
            watermark_age_millis: None,
            watermark_valid: false,
            watermark_local_reads_supported: false,
            watermark_errors: 0,
            snapshot_metrics: None,
        });
        assert!(absent.contains("plurx_raft_metric_sample_valid{source=\"local\"} 0"));
        assert!(!absent.contains("plurx_raft_metric_sample_age_seconds"));
        assert!(!absent.contains("plurx_raft_applied_index"));
        assert!(absent.contains("plurx_raft_metric_sample_valid{source=\"watermark\"} 0"));
        assert!(!absent.contains("plurx_raft_commit_index"));
        assert!(!absent.contains("plurx_raft_apply_lag_entries"));
    }

    fn beacon(event: &str, ms: i64) -> ClientLog {
        ClientLog {
            level: "warn".into(),
            event: event.into(),
            message: "x".into(),
            method: Some("remux".into()),
            code: None,
            title: None,
            file_id: None,
            vcodec: None,
            src: None,
            detail: None,
            ua: None,
            attempt: None,
            reason: None,
            runway: None,
            ms: Some(ms),
            bandwidth: None,
            height: None,
            encoder: None,
            decode_hw: None,
            decode_smooth: None,
            session_id: None,
            snapshot: None,
            control: None,
            control_trigger: None,
        }
    }

    /// The decoder verdict rides with the client facts, and says which one it
    /// is loudly enough to notice in a wall of log lines. `null` (a browser
    /// without mediaCapabilities) prints nothing rather than guessing.
    #[test]
    fn a_software_decode_is_named_in_the_beacon() {
        let mut ev = beacon("stall", 900);
        assert!(
            !client_log_line(&ev, 0).contains("decode="),
            "silent when unknown"
        );

        ev.decode_hw = Some(true);
        assert!(client_log_line(&ev, 0).contains(" decode=hw"));

        ev.decode_hw = Some(false);
        ev.decode_smooth = Some(false);
        let line = client_log_line(&ev, 0);
        assert!(line.contains(" decode=SOFTWARE/not-smooth"), "{line}");
    }

    #[test]
    fn session_identity_does_not_change_the_human_log_line() {
        let mut event = beacon("ttff", 684);
        let before = client_log_line(&event, 0);
        event.session_id = Some("session-a".into());
        assert_eq!(client_log_line(&event, 0), before);
    }

    #[test]
    fn telemetry_retention_does_not_change_the_human_log_surface() {
        let mut event = beacon("disabled_telemetry_proof", 0);
        event.message = "still logs".into();
        assert_eq!(
            client_log_line(&event, 0),
            "client disabled_telemetry_proof method=remux: still logs ms=0"
        );
    }

    /// A measurement is named for what it measured.
    ///
    /// `ms` carries a start time on a `ttff` event and a stall's *duration* on
    /// a `stall` one, and both were printed as `ttff_ms`. So a grep for
    /// `ttff_ms` over the log ring returned a series that was part start times
    /// and part stall lengths — with the stalls, being longer, owning the whole
    /// tail. That is the one number M0 exists to produce, wrong in the
    /// direction that makes the server look worse than it is: on nynuc a real
    /// p90 of 1.5 s read as 4.6 s.
    #[test]
    fn a_stalls_duration_is_never_reported_as_a_start_time() {
        let start = client_log_line(&beacon("ttff", 684), 0);
        assert!(start.contains("ttff_ms=684"), "{start}");

        let stall = client_log_line(&beacon("stall", 4584), 0);
        assert!(stall.contains("stall_ms=4584"), "{stall}");
        assert!(
            !stall.contains("ttff_ms"),
            "a stall's duration entered the start-time series: {stall}"
        );

        // An event nobody has taught this about says the neutral thing rather
        // than borrowing whichever name is nearest.
        let other = client_log_line(&beacon("hls_fatal", 12), 0);
        assert!(other.contains(" ms=12"), "{other}");
        assert!(
            !other.contains("ttff_ms") && !other.contains("stall_ms"),
            "{other}"
        );
    }

    /// The log ring must survive a client stuck in an error loop: the burst gets
    /// through, the flood does not, and the gap is accounted for rather than
    /// silently dropped.
    #[test]
    fn client_log_bucket_bounds_a_flood_and_counts_the_gap() {
        let t0 = Instant::now();
        let mut b = LogBucket::new(30);

        // The full burst is admitted, nothing suppressed yet.
        for _ in 0..30 {
            assert_eq!(b.admit(t0), Some(0));
        }
        // The next 500 in the same instant are dropped.
        for _ in 0..500 {
            assert_eq!(b.admit(t0), None);
        }

        // Two seconds later one token has refilled (30/min), and the report that
        // spends it carries the count of everything dropped meanwhile.
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(b.admit(t1), Some(500));
        // That count resets once reported.
        let t2 = t1 + Duration::from_secs(2);
        assert_eq!(b.admit(t2), Some(0));

        // A long quiet period refills to the burst cap and no further: an idle
        // client can't bank hours of credit and then dump it.
        let t3 = t2 + Duration::from_secs(3600);
        for _ in 0..30 {
            assert_eq!(b.admit(t3), Some(0));
        }
        assert_eq!(b.admit(t3), None);
    }

    /// A steady trickle below the limit is never throttled — normal playback
    /// errors keep flowing.
    #[test]
    fn client_log_bucket_passes_a_normal_trickle() {
        let mut now = Instant::now();
        let mut b = LogBucket::new(30);
        // One report every 5s for an hour: well under 30/min.
        for _ in 0..720 {
            assert_eq!(b.admit(now), Some(0));
            now += Duration::from_secs(5);
        }
    }

    #[test]
    fn client_log_limits_are_isolated_per_user() {
        let now = Instant::now();
        let mut limiter = ClientLogLimiter::new();
        for _ in 0..CLIENT_LOG_USER_PER_MIN {
            assert_eq!(limiter.admit(1, now), Some(0));
        }
        assert_eq!(limiter.admit(1, now), None);
        assert_eq!(
            limiter.admit(2, now),
            Some(0),
            "one user's flood must not silence another user"
        );
    }

    #[test]
    fn client_log_user_bucket_registry_is_bounded() {
        let now = Instant::now();
        let mut limiter = ClientLogLimiter::new();
        for user_id in 0..=CLIENT_LOG_USER_BUCKETS_MAX as i64 {
            let _ = limiter.admit(user_id, now + Duration::from_nanos(user_id as u64));
        }
        assert_eq!(limiter.users.len(), CLIENT_LOG_USER_BUCKETS_MAX);
        assert!(limiter
            .users
            .contains_key(&(CLIENT_LOG_USER_BUCKETS_MAX as i64)));
        assert!(
            !limiter.users.contains_key(&0),
            "the oldest idle bucket was evicted"
        );
    }

    #[test]
    fn client_log_suppression_counts_survive_the_global_ceiling() {
        let now = Instant::now();
        let mut limiter = ClientLogLimiter {
            global: LogBucket::new(1),
            users: HashMap::from([(1, LogBucket::new(1)), (2, LogBucket::new(1))]),
        };
        assert_eq!(limiter.admit(1, now), Some(0));
        assert_eq!(limiter.admit(1, now), None, "user bucket counts one drop");

        let minute = now + Duration::from_secs(60);
        assert_eq!(
            limiter.admit(2, minute),
            Some(0),
            "another user consumes the refilled global token"
        );
        assert_eq!(
            limiter.admit(1, minute),
            None,
            "user one's recovered token meets the global ceiling"
        );

        assert_eq!(
            limiter.admit(1, minute + Duration::from_secs(60)),
            Some(2),
            "one per-user and one global drop are both reported"
        );
    }

    #[tokio::test]
    async fn client_event_joins_and_persists_every_live_session_measurement() {
        let store: Arc<dyn Store> = Arc::new(
            plurx_core::store::SqliteStore::open_in_memory().expect("telemetry test store"),
        );
        let mut beacon = beacon("stall", 900);
        beacon.session_id = Some("session-a".into());
        let event = client_playback_event(&beacon, 7);
        let info = crate::transcode::SessionInfo {
            id: "session-a".into(),
            presentation: "live-recovery",
            file_id: 42,
            item_id: 4,
            item_title: "not persisted".into(),
            user_name: "not persisted".into(),
            target_height: 1080,
            encoder: "qsv",
            started_unix: 0,
            idle_seconds: 0,
            last_request: "segment",
            lease_mode: "explicit",
            lease_state: "active",
            lease_timeout_ms: Some(30_000),
            control_demand: Some("active"),
            reported_position_ms: Some(9_000),
            client_runway_ms: Some(20_000),
            render_state: Some("stalled"),
            production_policy: "explicit_demand",
            production_ahead_seconds: Some(35),
            production_target_seconds: Some(50),
            producer_control: Some(
                crate::playback_control::RollingProducerOperationalSnapshot {
                    phase: "held",
                    deadline_attempt: None,
                    deadline_mode: None,
                    deadline_remaining_ms: None,
                    due_attempt: None,
                    due_mode: None,
                    due_overdue_ms: None,
                    process_exit_attempt: None,
                    process_exit_due: false,
                    physical_flow: "held",
                    last_flow_applied_sequence: 11,
                    last_applied_sequence: 17,
                    observation_only: true,
                    action_owner: "legacy_compatibility",
                    startup_kind: None,
                    presentation_contract_fingerprint: None,
                    metadata_response_authorized: false,
                    producer_media_published: false,
                    retry_state: "legacy_compatibility",
                    decision_sequence: None,
                    decision_reason: None,
                    executor_state: "registered",
                    executor_pending_decision_age_ms: None,
                    executor_last_observed_sequence: 0,
                    executor_last_action_failure: None,
                    executor_registered: true,
                    decision_applied_sequence: None,
                    decision_installed_attempt: None,
                    pending_probe_sequence: None,
                    pending_probe_attempt: None,
                    pending_probe_deadline_remaining_ms: None,
                    last_probe_outcome: "none",
                    producer_ended_with_proposal: false,
                    proposal: None,
                    completion: "incomplete",
                    completion_attempt: None,
                    completion_final_segment: None,
                    completion_final_end_ms: None,
                },
            ),
            producer_state: "held",
            producer_attempt: Some(3),
            playlist_ready: Some(true),
            published_segment: Some(5),
            next_media_sequence: Some(6),
            pending_fetched_segment: None,
            speed: Some(2.0),
            recent_speed: Some(1.7),
            out_time_ms: Some(10_000),
            progress_idle_ms: 15,
            producer_exit_success: Some(false),
            producer_exit_code: Some(23),
            producer_exit_signal: None,
            producer_exit_idle_ms: Some(7),
            published_end_ms: Some(44_000),
            fetched_end_ms: 10_000,
            fetched_segment: Some(4),
            first_retained_segment: Some(1),
            playlist_shape: "sliding",
            ahead_seconds: Some(34),
            hold_reason: Some(crate::transcode::AheadHoldReason::Time),
            resume_below_seconds: Some(30),
            resume_below_bytes: None,
            ahead_bytes: Some(123),
            delivered_bytes: 456,
            delivered_bps: Some(8_000_000),
            delivered_idle_ms: 25,
            readrate: 2.0,
            suspended: true,
            suspend_count: 1,
        };
        emit_client_playback_event(Arc::clone(&store), event, Some(&info), None);
        let row = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(row) = store
                    .playback_events(&PlaybackEventQuery {
                        event: Some("stall".into()),
                        limit: 10,
                        ..PlaybackEventQuery::default()
                    })
                    .await
                    .expect("query joined client event")
                    .into_iter()
                    .next()
                {
                    return row;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("joined client event persisted");
        let correlation = crate::transcode::session_log_id("session-a");
        assert_eq!(row.session_id.as_deref(), Some(correlation.as_str()));
        assert_eq!(row.file_id, Some(42));
        assert_eq!(row.speed_recent, Some(1.7));
        assert_eq!(row.ahead_seconds, Some(34));
        assert_eq!(row.suspended, Some(true));
        assert_eq!(row.hold_reason.as_deref(), Some("time"));
        assert_eq!(row.delivered_bps, Some(8_000_000));
        assert_eq!(row.readrate, Some(2.0));
        let extra: serde_json::Value =
            serde_json::from_str(row.extra.as_deref().expect("joined status JSON"))
                .expect("valid joined status JSON");
        assert_eq!(extra["server"]["progress_idle_ms"], 15);
        assert_eq!(extra["server"]["producer_state"], "held");
        assert_eq!(extra["server"]["producer_exit_success"], false);
        assert_eq!(extra["server"]["producer_exit_code"], 23);
        assert_eq!(
            extra["server"]["producer_exit_signal"],
            serde_json::Value::Null
        );
        assert_eq!(extra["server"]["producer_exit_idle_ms"], 7);
        assert_eq!(extra["server"]["producer_attempt"], 3);
        assert_eq!(extra["server"]["playlist_ready"], true);
        assert_eq!(extra["server"]["published_segment"], 5);
        assert_eq!(extra["server"]["published_end_ms"], 44_000);
        assert_eq!(extra["server"]["next_media_sequence"], 6);
        assert_eq!(extra["server"]["fetched_segment"], 4);
        assert_eq!(extra["server"]["playlist_shape"], "sliding");
        assert_eq!(extra["server"]["last_request"], "segment");
        assert_eq!(extra["server"]["lease_mode"], "explicit");
        assert_eq!(extra["server"]["lease_state"], "active");
        assert_eq!(extra["server"]["lease_timeout_ms"], 30_000);
        assert_eq!(extra["server"]["control_demand"], "active");
        assert_eq!(extra["server"]["production_policy"], "explicit_demand");
        assert_eq!(extra["server"]["production_ahead_seconds"], 35);
        assert_eq!(extra["server"]["production_target_seconds"], 50);
        assert_eq!(extra["server"]["producer_control"]["phase"], "held");
        assert_eq!(
            extra["server"]["producer_control"]["last_applied_sequence"],
            17
        );
    }

    #[test]
    fn client_stall_snapshot_survives_a_superseded_session() {
        let mut event = beacon("stall", 12_000);
        event.session_id = Some("gone-session".into());
        event.snapshot = Some(ClientPlaybackSnapshot {
            position_ms: Some(90_000),
            runway: Some(0.4),
            time_control_status: Some("waiting".into()),
            waiting_reason: Some("AVPlayerWaitingToMinimizeStallsReason".into()),
            playback_buffer_empty: Some(true),
            media_requests: Some(17),
            bytes_transferred: Some(8_192),
            observed_bitrate_bps: Some(12_345_000.0),
            server: Some(ClientServerSnapshot {
                observed_age_ms: Some(2_345),
                recent_speed: Some(0.7),
                ahead_seconds: Some(4),
                delivered_bps: Some(6_000_000),
                readrate: Some(0.8),
                progress_idle_ms: Some(11_000),
                published_end_ms: Some(125_000),
                fetched_end_ms: Some(121_000),
                playlist_shape: Some("sliding".into()),
                last_request: Some("segment".into()),
                ..ClientServerSnapshot::default()
            }),
            ..ClientPlaybackSnapshot::default()
        });

        let persisted = client_playback_event(&event, 7);
        assert_eq!(persisted.runway_ds, Some(4));
        assert_eq!(persisted.bandwidth_kbps, Some(12_345));
        assert_eq!(persisted.speed_recent, Some(0.7));
        assert_eq!(persisted.ahead_seconds, Some(4));
        assert_eq!(persisted.delivered_bps, Some(6_000_000));
        assert_eq!(persisted.readrate, Some(0.8));
        let extra: serde_json::Value =
            serde_json::from_str(persisted.extra.as_deref().expect("client snapshot JSON"))
                .expect("valid client snapshot JSON");
        assert_eq!(extra["client"]["position_ms"], 90_000);
        assert_eq!(extra["client"]["time_control_status"], "waiting");
        assert_eq!(extra["client"]["media_requests"], 17);
        assert_eq!(extra["client"]["server"]["observed_age_ms"], 2_345);
        assert_eq!(extra["client"]["server"]["progress_idle_ms"], 11_000);
        assert_eq!(extra["client"]["server"]["playlist_shape"], "sliding");

        let line = client_log_line(&event, 0);
        assert!(line.contains("runway=0.4s"), "{line}");
        assert!(line.contains("bw=12345kbps"), "{line}");
    }

    #[test]
    fn legacy_recovery_keeps_the_preceding_control_snapshot() {
        let mut event = beacon("stall_recovery", 8_000);
        event.control = Some(ClientControlSnapshot {
            generation: Some("11111111-1111-4111-8111-111111111111".into()),
            control_epoch: Some(17),
            sequence: Some(23),
            demand: Some("active".into()),
            render_state: Some("stalled".into()),
            position_ms: Some(90_000),
            buffered_through_ms: Some(90_400),
            observed_download_bps: Some(1_500_000),
            observation: Some(ClientControlObservation {
                dropped_frames: Some(3),
                decoder_state: Some("starved".into()),
                error_code: None,
                error_detail: None,
            }),
        });
        event.control_trigger = Some(ClientControlSnapshot {
            generation: Some("11111111-1111-4111-8111-111111111111".into()),
            control_epoch: Some(17),
            sequence: None,
            demand: Some("active".into()),
            render_state: Some("failed".into()),
            position_ms: Some(90_000),
            buffered_through_ms: Some(90_400),
            observed_download_bps: Some(1_500_000),
            observation: Some(ClientControlObservation {
                dropped_frames: Some(4),
                decoder_state: Some("failed".into()),
                error_code: Some("decoder".into()),
                error_detail: Some("persistent_decode_stall".into()),
            }),
        });

        let persisted = client_playback_event(&event, 7);
        let extra: serde_json::Value =
            serde_json::from_str(persisted.extra.as_deref().expect("control snapshot JSON"))
                .expect("valid control snapshot JSON");
        let accepted = &extra["control_accepted_client"];
        assert_eq!(accepted["sequence"], 23);
        assert_eq!(accepted["render_state"], "stalled");
        assert_eq!(accepted["buffered_through_ms"], 90_400);
        assert_eq!(accepted["observed_download_bps"], 1_500_000);
        assert_eq!(accepted["observation"]["decoder_state"], "starved");
        assert!(accepted["generation"]
            .as_str()
            .is_some_and(|value| value.starts_with("g-")));
        assert!(!persisted
            .extra
            .as_deref()
            .unwrap_or_default()
            .contains("11111111-1111-4111-8111-111111111111"));
        assert_eq!(extra["control_trigger_client"]["render_state"], "failed");
        assert_eq!(
            extra["control_trigger_client"]["observation"]["error_code"],
            "decoder"
        );
        assert_eq!(
            extra["control_trigger_client"]["observation"]["error_detail"],
            "persistent_decode_stall"
        );
        assert!(extra["control_trigger_client"].get("sequence").is_none());
    }

    #[test]
    fn playback_event_reader_caps_the_requested_row_count() {
        assert_eq!(
            bounded_playback_query(PlaybackEventsQuery {
                since: None,
                event: None,
                limit: i64::MAX,
            })
            .limit,
            2_000
        );
    }
}
