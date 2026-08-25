//! HLS transcode endpoints. `start` creates a session (authenticated) and
//! returns the playlist URL; the playlist and segments are then fetched by
//! whatever HLS player the session ends up in.
//!
//! Playlist/segment requests authenticate by *capability*: the session id is
//! a v4 UUID (122 random bits) minted for an authenticated user, unguessable,
//! and short-lived (reaped on idle). No header requirement means dumb
//! fetchers can play the stream — Safari's native HLS, and crucially an
//! Apple TV during AirPlay, which fetches the URL itself with no way to
//! attach our bearer token. Same model Plex uses; also what Phase 4 wants,
//! since any cluster node can serve a session id without seeing the login.

use axum::body::Body;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use plurx_core::domain::{
    MediaFile, MediaSessionActivation, MediaSessionActivationOutcome, MediaSessionRequestClaim,
    MediaSessionRoute, SubtitleStream,
};
use plurx_core::playback::PlaybackMethod;
use plurx_core::tracks::is_native_text_subtitle;

use super::error::ApiError;
use super::extract::AuthUser;
use crate::media_pool::MediaOfferRequest;
use crate::media_sessions::{
    unix_ms, worker_session_request_is_valid, RelayHeaders, RelayRequest, RelayResource,
    RemoteAbortRequest, RemoteStartRequest, RemoteStartResponse, ACTIVATION_CONFIRMATION_WINDOW,
    ACTIVATION_FAST_RECONCILIATION, ACTIVATION_STORE_DEADLINE, LEASE_TTL_MS,
    OWNER_ASSIGNMENT_DEADLINE, REMOTE_ACTIVATION_CONFIRMATION_WINDOW, START_DEADLINE,
};
use crate::state::AppState;
use crate::transcode::{ClusterReplacementGuard, PlaylistError};

/// How much of an `init.mp4` this module will ever read into memory.
///
/// Both readers of an initialization segment — the playlist-time `hvcC` sniff
/// and the Apple High-tier rewrite in `segment` — are bounded by it, and both
/// hold their delivery tracker to the same figure, so a larger init reports a
/// skipped inspection rather than a truncated response.
const INIT_INSPECTION_LIMIT_BYTES: u64 = 1024 * 1024;
const MIN_RESOLVED_REPLAY_REMAINING_MS: i64 = 1_000;

#[derive(Deserialize)]
pub struct StartQuery {
    /// Target height (e.g. 1080, 720). Omitted means Auto (server-chosen).
    /// Ignored when `copy=1`.
    pub height: Option<i64>,
    /// Start offset in seconds (resume / seek).
    pub start: Option<f64>,
    /// Audio stream to use (`a:{audio}`); overrides the automatic pick.
    pub audio: Option<i64>,
    /// `copy=1` → a copy-video HLS session (repackage the source video into
    /// fMP4 HLS untouched, transcode audio only). For players that can't take a
    /// progressive fMP4 remux but decode HEVC/HDR natively via HLS (Safari).
    pub copy: Option<u8>,
    /// With `copy`: `aac=1` transcodes the audio to AAC (the codec the client
    /// can't take), `aac=0` copies it. The client knows which from `/decision`.
    pub aac: Option<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StartResponse {
    pub session_id: String,
    pub playlist_url: String,
    pub duration_ms: Option<i64>,
    pub start_seconds: f64,
    /// Source-timeline timestamp represented by player-local time zero.
    ///
    /// Additive for new clients. A copy session can start on the keyframe
    /// before `start_seconds`, while accurate transcodes start at the
    /// requested position. Old clients continue using `start_seconds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_origin_ms: Option<i64>,
    /// The normalized output height this request actually received. For a
    /// bound stall reopen this is the persisted one-rung-down answer, repeated
    /// unchanged on a transport replay of the same `request_id`.
    pub height: i64,
    pub encoder: String,
    /// The whole stream already exists on disk (a pre-transcode cache hit).
    /// The player treats it like direct play: seek by `currentTime`, and don't
    /// arm the stall watchdog's restart — see `StartInfo::vod`.
    pub vod: bool,
    /// The quality ladder for this source, top rung first — the rungs the
    /// menu and the Auto controller move between, so the client never
    /// hardcodes them (ADAPTIVE-QUALITY.md Phase 1).
    pub ladder: Vec<crate::transcode::Rung>,
    /// Node-local sustained-throughput prior used for this Auto start.
    /// Additive and absent while the opt-in feature is disabled or cold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_kbps: Option<u32>,
    /// What dynamic range the bytes of *this session* carry
    /// (`"dolby_vision" | "hdr10" | "hlg" | "sdr"`). It overrides the
    /// decision's answer the moment the session attaches, because a burn or
    /// a manually-picked rung forces a transcode the decision never promised
    /// (MEDIA-BADGES-PLAN §3.2). Absent when the source file vanished from
    /// the store mid-request: the client keeps whatever it had.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_dynamic_range: Option<String>,
}

/// Owns a published worker until the replicated activation has a definitive
/// outcome. Request futures are cancellation points at every store/network
/// await; tying cleanup to this value prevents a disconnected client from
/// leaving an encoder and its durable start claim behind.
struct StartedSessionGuard {
    cleanup: Option<StartedSessionCleanup>,
    _replacement: Option<ClusterReplacementGuard>,
}

struct StartedSessionCleanup {
    state: AppState,
    owner_node_id: String,
    incarnation_id: String,
    session_id: String,
    user_id: i64,
    request_id: String,
}

impl StartedSessionGuard {
    fn new(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self {
            cleanup: Some(StartedSessionCleanup {
                state,
                owner_node_id,
                incarnation_id,
                session_id,
                user_id,
                request_id,
            }),
            _replacement: replacement,
        }
    }

    fn disarm(&mut self) {
        self.cleanup = None;
        self._replacement = None;
    }
}

impl Drop for StartedSessionGuard {
    fn drop(&mut self) {
        let Some(cleanup) = self.cleanup.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            abort_started_session(
                &cleanup.state,
                &cleanup.owner_node_id,
                &cleanup.incarnation_id,
                &cleanup.session_id,
            )
            .await;
            let _ = cleanup
                .state
                .store
                .fail_media_session_request(
                    cleanup.user_id,
                    &cleanup.request_id,
                    &cleanup.incarnation_id,
                    unix_ms(),
                )
                .await;
        }));
    }
}

/// Everything a client must say to open a stream.
///
/// A body rather than a query string, and a POST rather than a GET, because
/// this call spawns a process and kills its predecessor. A GET that does that
/// is a trap: GET is idempotent by definition, so anything entitled to replay
/// one — a retry, a prefetch, an intermediary — could spawn a second encoder
/// and orphan the first.
#[derive(Deserialize)]
pub struct CreateSession {
    /// Stable for one player instance. Supersession is keyed by it, so two
    /// devices on one account no longer kill each other's streams.
    pub playback_id: String,
    /// Optional idempotency key for this one attempt.
    pub request_id: Option<String>,
    /// Exact predecessor for a typed recovery. Both fields are required for a
    /// stall reopen, which also requires `request_id`; ordinary seeks and
    /// track changes omit them.
    pub previous_session_id: Option<String>,
    pub reopen_reason: Option<crate::transcode::ReopenReason>,
    /// Target height for a transcode. Ignored when `copy` is set. Omitted
    /// means Auto, and Auto is the SERVER's decision: the rung depends on
    /// which encoder wins, and the player only learns that from the response
    /// to this request (see `TranscodeManager::auto_height`).
    pub height: Option<i64>,
    /// Whether the VIEWER chose Auto quality. This is what decides stall
    /// stickiness, and it is not the same question as "did this body carry a
    /// `height`".
    ///
    /// A client sends a height for reasons unrelated to the quality menu: a
    /// subtitle burn and Quality = Original both send the source's own height
    /// as a promise about the output. Android's `sessionHeight` answers the
    /// burn case *before* it consults quality, so an Auto viewer watching with
    /// a burned subtitle posts a height and would otherwise be permanently
    /// ineligible for the stall step-down — while the same viewer on Apple,
    /// which never names a height, would be eligible. Same intent, opposite
    /// recovery, decided by a field neither of them meant as a quality answer.
    ///
    /// Omitted keeps the old inference (`height` absent means Auto), so every
    /// shipped client behaves exactly as before. A client that can state the
    /// viewer's choice should send it, and then it is authoritative.
    pub quality_auto: Option<bool>,
    /// Subtitle stream to burn into the picture. Only for the ones a client
    /// cannot render itself — a bitmap track (PGS/VobSub) has no text to send,
    /// so the only way to show it is to draw it into the frames.
    pub subtitle_burn: Option<i64>,
    /// Explicit acknowledgement that the already-selected playback plan
    /// delivers SDR. This distinguishes a forced bitmap subtitle added to an
    /// existing HDR-to-SDR transcode from a subtitle that would silently
    /// downgrade an otherwise-HDR delivery. Absent stays fail-closed for old
    /// clients.
    pub subtitle_burn_sdr: Option<bool>,
    /// Advertise the source's lossless WebVTT-convertible tracks in an HLS
    /// master playlist. Native Apple clients opt in; older HLS consumers keep
    /// receiving the media playlist shape they already understand.
    pub native_subtitles: Option<bool>,
    /// Initially selected native rendition. This changes only HLS metadata,
    /// never the video recipe or the encoder choice.
    pub subtitle: Option<i64>,
    pub start: Option<f64>,
    pub audio: Option<i64>,
    /// Copy the source video into HLS rather than re-encoding it.
    pub copy: Option<bool>,
    /// With `copy`: re-encode the audio the client can't take.
    pub aac: Option<bool>,
    /// With `copy`: retain Dolby Vision signaling and dynamic metadata because
    /// the decision established that this client supports the source profile.
    pub preserve_dolby_vision: Option<bool>,
    /// For a transcode: this client decodes and presents HEVC Main10 PQ, so a
    /// Dolby Vision source it cannot decode may be re-encoded to HDR10 rather
    /// than tone-mapped to SDR. The create-body counterpart of `/decision`'s
    /// `hdr10t=1`, exactly as `preserve_dolby_vision` is the counterpart of
    /// `dv`.
    ///
    /// A request, never a verdict. The server refuses it for any source that
    /// does not prove a Dolby Vision RPU through the production renderer, for
    /// any rung other than the measured 1080p software/QSV or 2160p QSV
    /// points, and for any build whose boot probe did not clear the exact
    /// graph — every refusal lands on today's SDR ladder. Absent means no,
    /// which is what every client that predates it sends.
    pub hdr10: Option<bool>,
    /// Manual A/V correction for this playback attempt only. Positive delays
    /// audio. It is carried into every seek/reopen by the client and is never
    /// written back to the media file.
    pub audio_offset_ms: Option<i64>,
    /// The only accepted value is `"vod"`. Omitted also means VOD so clients
    /// predating this field cannot accidentally enter the removed growing
    /// live-HLS path. An explicit legacy value receives a typed refusal.
    pub presentation: Option<String>,
    /// With `presentation:"vod"`: the client's ceiling for one blocking
    /// segment fetch, in seconds. Clamped server-side.
    pub block_budget_secs: Option<f64>,
}

impl CreateSession {
    /// `height` is initially resolved by the caller — Auto answered, explicit
    /// rungs snapped, the source-height promise honored. A bound stall reopen
    /// is the one later normalization: `claim_request` replaces this value
    /// from the predecessor's resolved rung and persists it before supersede.
    fn into_request(self, file_id: i64, height: i64) -> crate::transcode::SessionRequest {
        use crate::transcode::SessionKind;
        // Viewer intent when the client states it; otherwise the old
        // wire-presence inference, which is right for every client that has
        // no separate quality answer to give.
        let automatic = self.quality_auto.unwrap_or(self.height.is_none());
        let kind = if self.copy == Some(true) {
            SessionKind::Copy {
                aac: self.aac == Some(true),
                preserve_dolby_vision: self.preserve_dolby_vision == Some(true),
            }
        } else {
            SessionKind::Transcode { height }
        };
        crate::transcode::SessionRequest {
            file_id,
            playback_id: self.playback_id,
            request_id: self.request_id,
            automatic,
            previous_session_id: self.previous_session_id,
            reopen_reason: self.reopen_reason,
            kind,
            start_seconds: self.start.unwrap_or(0.0).max(0.0),
            audio_index: self.audio.filter(|a| *a >= 0),
            subtitle_burn: self.subtitle_burn.filter(|s| *s >= 0),
            audio_offset_ms: self.audio_offset_ms.unwrap_or(0).clamp(-15_000, 15_000),
            hdr10: self.hdr10 == Some(true),
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: self.block_budget_secs.filter(|s| s.is_finite() && *s > 0.0),
        }
    }
}

const HDR_SUBTITLE_BURN_REFUSAL: &str =
    "That subtitle requires an SDR burn-in. HDR playback was kept unchanged.";

/// Old clients can still post `subtitle_burn` without first applying the
/// native clients' HDR guard. A known HDR source therefore fails closed unless
/// the client explicitly acknowledges that its selected plan already delivers
/// SDR. That acknowledgement is safe for an existing tone-map and remains
/// absent when the burn would replace DV, HDR10, or HLG with SDR.
fn hdr_subtitle_burn_is_refused(
    source: Option<&MediaFile>,
    subtitle_burn: Option<i64>,
    subtitle_burn_sdr: Option<bool>,
) -> bool {
    subtitle_burn.is_some_and(|index| index >= 0)
        && subtitle_burn_sdr != Some(true)
        && source.is_some_and(|file| {
            matches!(file.hdr.as_deref(), Some("dolby_vision" | "hdr10" | "hlg"))
        })
}

/// The dynamic range this session puts on the wire, read off the session it
/// actually built rather than the decision that suggested it — a burn or a
/// forced rung produces a transcode `/decision` never promised, and the
/// badge has to follow the session (MEDIA-BADGES-PLAN §3.2).
///
/// `None` only when the source row could not be loaded; there is nothing
/// honest to say about a file we cannot see.
fn session_delivered_dynamic_range(
    source: Option<&MediaFile>,
    kind: &crate::transcode::SessionKind,
    grade: plurx_core::transcode::OutputGrade,
) -> Option<&'static str> {
    use crate::transcode::SessionKind;
    let file = source?;
    // One helper for both wire fields, so the decision and the session can
    // never disagree about the same delivery.
    let (method, preserve) = match kind {
        SessionKind::Copy {
            preserve_dolby_vision,
            ..
        } => (PlaybackMethod::Remux, *preserve_dolby_vision),
        SessionKind::Transcode { .. } => (PlaybackMethod::Transcode, false),
    };
    Some(plurx_core::playback::delivered_dynamic_range(
        file, method, preserve, grade,
    ))
}

/// POST /api/v1/files/:id/hls/sessions — create a stream, or recover the one
/// an identical request already created.
pub async fn create(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
    headers: HeaderMap,
    super::network::RemoteAddress(remote): super::network::RemoteAddress,
    Json(req): Json<CreateSession>,
) -> Result<Json<StartResponse>, ApiError> {
    if !valid_playback_id(&req.playback_id) {
        return Err(ApiError::BadRequest(
            "playback_id must contain 1 to 128 safe characters".into(),
        ));
    }
    if req
        .presentation
        .as_deref()
        .is_some_and(|presentation| presentation != "vod")
    {
        return Err(ApiError::typed(
            StatusCode::GONE,
            "live_presentation_removed",
            "the growing live HLS presentation has been removed; request VOD",
        ));
    }
    // The source height answers three things now: Auto, the ladder in the
    // response, and the snap's source-height escape. One read, from the read
    // pool.
    let source = state.store.get_file(id).await?;
    if hdr_subtitle_burn_is_refused(source.as_ref(), req.subtitle_burn, req.subtitle_burn_sdr) {
        return Err(ApiError::Unprocessable(serde_json::json!({
            "code": "hdr_subtitle_burn_refused",
            "error": HDR_SUBTITLE_BURN_REFUSAL,
        })));
    }
    let source_height = source.as_ref().and_then(|f| f.height);
    let hdr10_requested = req.hdr10 == Some(true);
    let ladder_ceiling = state
        .transcode
        .capability_height_ceiling_for_request(source.as_ref(), hdr10_requested)
        .await;
    let mut identity = super::network::identity(&headers, remote);
    if let Some(ref mut id) = identity {
        id.user_id = Some(user.id);
        id.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
            user.id,
            user.created_at,
            &user.password_hash,
        ));
    }
    let network_prior =
        super::network::stored_prior(state.store.as_ref(), identity.as_ref()).await?;
    let height = match req.height {
        // Auto: the server's own choice already lands where it means to —
        // snapping it would re-decide policy (a 900p source deliberately
        // transcodes at 900: no scaler in the chain at all).
        None => {
            state
                .transcode
                .auto_height_for_request(source.as_ref(), network_prior.as_ref(), hdr10_requested)
                .await
        }
        // The source's own height is the Original/forced-burn promise
        // (see the player's sessionHeight): never snapped, never downgraded.
        Some(h) if Some(h) == source_height => h,
        // An explicit rung from a menu: snap strays onto the ladder;
        // above-ladder heights pass through as what they are.
        Some(h) => crate::transcode::snap_height(h),
    }
    .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT);
    let native_subtitles = req.native_subtitles == Some(true);
    let native_subtitle = req.subtitle.filter(|s| *s >= 0);
    if native_subtitles {
        if let Some(index) = native_subtitle {
            let track = source
                .as_ref()
                .and_then(|f| f.subtitle_streams.get(index as usize))
                .ok_or_else(|| ApiError::BadRequest("unknown native subtitle track".into()))?;
            if !is_native_text_subtitle(&track.codec) {
                return Err(ApiError::BadRequest(
                    "the selected subtitle requires burn-in".into(),
                ));
            }
        }
    }
    let request = req.into_request(id, height);
    if request
        .request_id
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 128)
    {
        return Err(ApiError::BadRequest(
            "request_id must contain 1 to 128 characters".to_owned(),
        ));
    }
    if !worker_session_request_is_valid(&request) {
        return Err(ApiError::BadRequest(
            "media session request exceeds the supported cluster contract".to_owned(),
        ));
    }
    let fingerprint = request.durable_intent_fingerprint(user.id);
    let now_ms = unix_ms();
    let mut incarnation_id = uuid::Uuid::new_v4().to_string();
    // Every create occupies a durable admission row. A caller-supplied key
    // keeps public idempotency; an ordinary create uses its unguessable
    // incarnation internally so omitting request_id cannot bypass the
    // in-flight cap or allocate encoders before admission.
    let request_claim_id = request
        .request_id
        .clone()
        .unwrap_or_else(|| incarnation_id.clone());
    match state
        .store
        .claim_media_session_request(
            user.id,
            &request_claim_id,
            &fingerprint,
            &request.playback_id,
            &incarnation_id,
            now_ms,
            now_ms.saturating_add(60_000),
        )
        .await?
    {
        MediaSessionRequestClaim::Acquired {
            incarnation_id: acquired,
        } => incarnation_id = acquired,
        MediaSessionRequestClaim::Resolved(route) if resolved_replay_is_live(&route, unix_ms()) => {
            return serde_json::from_str::<StartResponse>(&route.response_json)
                .map(Json)
                .map_err(ApiError::from);
        }
        MediaSessionRequestClaim::Resolved(_) => {
            return Err(ApiError::typed(
                StatusCode::GONE,
                "media_session_ended",
                "this idempotent session was already released",
            ));
        }
        MediaSessionRequestClaim::InFlight { .. } => {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "media_session_starting",
                "an identical session request is still starting; retry shortly",
            ));
        }
        MediaSessionRequestClaim::Conflict => {
            return Err(ApiError::Conflict(
                "request_id was already used for a different session intent".to_owned(),
            ));
        }
        MediaSessionRequestClaim::Overloaded => {
            return Err(ApiError::ServiceUnavailable(
                "too many session starts are already active for this user".to_owned(),
            ));
        }
    }

    let mut worker_request = request.clone();
    worker_request.request_id = Some(incarnation_id.clone());
    let remote_request = RemoteStartRequest {
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: incarnation_id.clone(),
        user_id: user.id,
        // The source snapshot a later takeover must match exactly (§7.3). A
        // row we could not read records an impossible snapshot rather than a
        // plausible one, so takeover refuses instead of reproducing a session
        // against a file it never verified.
        source_size: source.as_ref().map_or(0, |f| f.size),
        source_mtime: source.as_ref().map_or(0, |f| f.mtime),
        // Recorded from the same decision the worker will make, so a later
        // takeover can tell whether this URL was ever serving a shape a
        // successor is allowed to continue.
        typeless_playlist: state.transcode.cluster_playlist_is_typeless().await,
        request: worker_request,
    };
    let recipe_json = serde_json::to_string(&remote_request)?;
    let placement_deadline = super::peer_transport::deadline_after(START_DEADLINE);

    // A stall reopen must stay with its existing worker in P5: the normalized
    // predecessor state is process-local and automatic migration does not
    // become legal until the fenced P7 takeover protocol exists.
    let mut expected_predecessor_incarnation_id = None;
    let mut fence_predecessor = false;
    let pinned_owner = if let Some(previous_session_id) = request
        .previous_session_id
        .as_deref()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
    {
        // Bypass the positive route cache: an ended predecessor may remain
        // cached briefly, but a reopen must bind to the durable pointer state.
        let durable = state.store.media_session_route(previous_session_id).await?;
        if let Some(route) = durable {
            if route.user_id != user.id
                || route.state != "active"
                || route.lease_expires_at_ms <= unix_ms()
            {
                let _ = state
                    .store
                    .fail_media_session_request(
                        user.id,
                        &request_claim_id,
                        &incarnation_id,
                        unix_ms(),
                    )
                    .await;
                return Err(ApiError::typed(
                    StatusCode::CONFLICT,
                    "media_session_superseded",
                    "the session being reopened is no longer current",
                ));
            }
            fence_predecessor = true;
            expected_predecessor_incarnation_id = Some(route.incarnation_id);
            Some(route.owner_node_id)
        } else if state
            .transcode
            .active_session_ids()
            .await
            .iter()
            .any(|session_id| session_id == previous_session_id)
        {
            // A session created by the pre-P5 binary has no durable route.
            // Its reopen state is nevertheless process-local, so pin that
            // rolling-upgrade legacy predecessor to this node rather than
            // ranking a peer that cannot possess it.
            fence_predecessor = true;
            Some(state.node_id.clone())
        } else {
            None
        }
    } else {
        None
    };
    let mut owner_candidates = if let Some(owner) = pinned_owner {
        vec![owner]
    } else if !state.media_pool.remote_placement_ready(&state).await {
        vec![state.node_id.clone()]
    } else {
        match MediaOfferRequest::new(
            source.as_ref().ok_or(ApiError::NotFound("file"))?,
            height,
            (request.start_seconds * 1_000.0).round().max(0.0) as i64,
            request.audio_index,
            request.subtitle_burn,
            request.hdr10,
        ) {
            Ok(offer_request) => state
                .media_pool
                .offers(&state, offer_request)
                .await
                .offers
                .into_iter()
                .filter(|offer| offer.eligible)
                .fold(Vec::new(), |mut candidates, offer| {
                    if !candidates.contains(&offer.node_id) {
                        candidates.push(offer.node_id);
                    }
                    candidates
                }),
            Err(_) => Vec::new(),
        }
    };
    // Cold snapshots and one-node recovery retain the established local
    // behavior. A non-empty ranked list is authoritative: an ineligible local
    // offer must not bypass an eligible peer or capacity refusal.
    if owner_candidates.is_empty() {
        owner_candidates.push(state.node_id.clone());
    }

    let mut started = None;
    let mut last_error = None;
    for candidate in owner_candidates {
        if tokio::time::Instant::now() >= placement_deadline {
            last_error = Some(ApiError::ServiceUnavailable(
                "media worker placement exceeded its common deadline".to_owned(),
            ));
            break;
        }
        let result = if candidate == state.node_id {
            // Session creation owns a child process before publishing the map
            // entry. An owned task reaches a verdict even if this request is
            // cancelled, and its returned guard cleans the exact late worker
            // if nobody receives it.
            let transcode = Arc::clone(&state.transcode);
            let worker_request = remote_request.request.clone();
            let user_name = user.username.clone();
            let guard_state = state.clone();
            let guard_owner = candidate.clone();
            let guard_incarnation = incarnation_id.clone();
            let guard_request = request_claim_id.clone();
            let guard_user = user.id;
            let mut start_task = tokio::spawn(async move {
                transcode
                    .create_cluster_session(
                        &worker_request,
                        guard_user,
                        &user_name,
                        placement_deadline,
                    )
                    .await
                    .map(|started| {
                        let info = started.info;
                        let session_id = info.session_id.clone();
                        let response = RemoteStartResponse::from(info);
                        let guard = StartedSessionGuard::new(
                            guard_state,
                            guard_owner,
                            guard_incarnation,
                            session_id,
                            guard_user,
                            guard_request,
                            Some(started.replacement),
                        );
                        (response, guard)
                    })
            });
            match tokio::time::timeout_at(placement_deadline, &mut start_task).await {
                Ok(Ok(result)) => result.map_err(|error| session_start_error(id, error)),
                Ok(Err(error)) => Err(ApiError::Internal(format!(
                    "local media worker task failed: {error}"
                ))),
                Err(_) => {
                    tokio::spawn(async move {
                        // Dropping a successful output drops its armed guard.
                        let _ = start_task.await;
                    });
                    Err(ApiError::ServiceUnavailable(
                        "local media worker exceeded the placement deadline".to_owned(),
                    ))
                }
            }
        } else {
            state
                .media_sessions
                .start_remote(&candidate, &remote_request, placement_deadline)
                .await
                .map(|info| {
                    let guard = StartedSessionGuard::new(
                        state.clone(),
                        candidate.clone(),
                        incarnation_id.clone(),
                        info.session_id.clone(),
                        user.id,
                        request_claim_id.clone(),
                        None,
                    );
                    (info, guard)
                })
                .map_err(|error| {
                    ApiError::ServiceUnavailable(format!(
                        "media worker {candidate} could not start the session: {error:?}"
                    ))
                })
        };
        match result {
            Ok((info, guard)) if info.is_valid() => {
                started = Some((candidate, info, guard));
                break;
            }
            Ok((_info, _guard)) => {
                // The armed guard aborts an invalid local result just as the
                // peer transport rejects and aborts an invalid remote result.
                tracing::warn!(
                    owner_node_id = %candidate,
                    "media worker returned an invalid start contract"
                );
                last_error = Some(ApiError::ServiceUnavailable(format!(
                    "media worker {candidate} returned an invalid start contract"
                )));
            }
            Err(error) => {
                tracing::debug!(owner_node_id = %candidate, "media worker start attempt failed");
                last_error = Some(error);
            }
        }
    }
    let Some((owner_node_id, info, guard)) = started else {
        let _ = state
            .store
            .fail_media_session_request(user.id, &request_claim_id, &incarnation_id, unix_ms())
            .await;
        return Err(last_error.unwrap_or_else(|| {
            ApiError::ServiceUnavailable("no eligible media worker was available".to_owned())
        }));
    };
    if owner_node_id == state.node_id {
        let provisional_pin_ms =
            i64::try_from(REMOTE_ACTIVATION_CONFIRMATION_WINDOW.as_millis()).unwrap_or(i64::MAX);
        if !state
            .transcode
            .pin_shared_session(
                &info.session_id,
                &incarnation_id,
                1,
                unix_ms().saturating_add(provisional_pin_ms),
            )
            .await?
        {
            return Err(ApiError::ServiceUnavailable(
                "shared cache generation changed before session activation".to_owned(),
            ));
        }
    }
    match tokio::time::timeout(
        OWNER_ASSIGNMENT_DEADLINE,
        state.store.assign_media_session_request_owner(
            user.id,
            &request_claim_id,
            &incarnation_id,
            &owner_node_id,
            unix_ms(),
        ),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            // The armed guard aborts the exact worker and fails this claim.
            return Err(ApiError::ServiceUnavailable(
                "session ownership changed while placement was being committed".to_owned(),
            ));
        }
        Ok(Err(error)) => {
            return Err(error.into());
        }
        Err(_) => {
            return Err(ApiError::ServiceUnavailable(
                "session ownership assignment timed out".to_owned(),
            ));
        }
    }
    // The grade the session actually built, not the one the body asked for:
    // the server refuses the HDR10 rung for a source or a rung that cannot
    // prove it, and the badge has to follow the encoder.
    let delivered = session_delivered_dynamic_range(source.as_ref(), &info.kind, info.grade);
    // A session being created is playback beginning — the honest moment for
    // the scrobble that used to fire from `/decision`. Read the normalized
    // route from the result: a bound Auto stall may have changed a copy request
    // into a lower transcode before the session was spawned.
    let method = match info.kind {
        crate::transcode::SessionKind::Copy { .. } => crate::delivery::Method::HlsCopy,
        crate::transcode::SessionKind::Transcode { .. } => crate::delivery::Method::Transcode,
    };
    let playlist_url = if native_subtitles {
        // Give the multivariant playlist its own path. AVPlayer caches HLS
        // resources by URL and can otherwise conflate `index.m3u8?native=1`
        // with the child `index.m3u8` media playlist referenced by that
        // master. Keep the query form in `playlist` for clients holding an
        // older session response, but new sessions use the unambiguous path.
        let master = format!("/api/v1/hls/{}/master.m3u8", info.session_id);
        match native_subtitle {
            Some(index) => format!("{master}?subtitle={index}"),
            None => master,
        }
    } else {
        info.playlist_url
    };
    let response = StartResponse {
        session_id: info.session_id.clone(),
        playlist_url,
        duration_ms: info.duration_ms,
        start_seconds: info.start_seconds,
        media_origin_ms: Some((info.media_origin_seconds * 1000.0).round() as i64),
        height: info.target_height,
        encoder: info.encoder.clone(),
        vod: info.vod,
        // Capped at what this node can actually serve for this file, not just
        // at the source height: an advertised rung is a promise, and the web
        // ABR controller upgrades into any rung the ladder lists. See
        // `capability_height_ceiling`.
        ladder: crate::transcode::advertised_ladder(source_height, ladder_ceiling),
        prior_kbps: network_prior.and_then(|prior| prior.sustained_kbps),
        delivered_dynamic_range: delivered.map(str::to_owned),
    };
    let response_json = serde_json::to_string(&response)?;
    let activation_now_ms = unix_ms();
    let activation = MediaSessionActivation {
        incarnation_id: incarnation_id.clone(),
        session_id: info.session_id.clone(),
        user_id: user.id,
        playback_id: request.playback_id.clone(),
        expected_predecessor_incarnation_id,
        fence_predecessor,
        request_id: Some(request_claim_id.clone()),
        request_fingerprint: fingerprint,
        owner_node_id: owner_node_id.clone(),
        lease_expires_at_ms: activation_now_ms.saturating_add(LEASE_TTL_MS),
        recipe_json,
        response_json,
        media_origin_ms: (info.media_origin_seconds * 1_000.0).round() as i64,
        now_ms: activation_now_ms,
    };
    // Once activation begins, this owned task also owns the cleanup guard.
    // A disconnected HTTP client cannot interrupt the commit-unknown
    // reconciliation and accidentally kill an activated winner (or leak an
    // unactivated worker).
    let activation_state = state.clone();
    tokio::spawn(async move {
        let mut guard = guard;
        match tokio::time::timeout(
            ACTIVATION_STORE_DEADLINE,
            activation_state.store.activate_media_session(&activation),
        )
        .await
        {
            Ok(Ok(Some(outcome))) => {
                activation_state
                    .media_sessions
                    .cache_route(outcome.route.clone())
                    .await;
                if outcome.route.owner_node_id == activation_state.node_id {
                    activation_state
                        .media_sessions
                        .seed_owned_lease(&outcome.route)
                        .await;
                }
                guard.disarm();
                if let Some(predecessor) = outcome.predecessor.as_ref() {
                    stop_owned_session(&activation_state, predecessor).await;
                }
                Ok(outcome)
            }
            Ok(Ok(None)) => Err(ApiError::ServiceUnavailable(
                "session ownership could not be activated".to_owned(),
            )),
            uncertain => {
                let error = match uncertain {
                    Ok(Err(error)) => error.into(),
                    Err(_) => ApiError::ServiceUnavailable(
                        "session activation outcome is still being reconciled".to_owned(),
                    ),
                    Ok(Ok(_)) => unreachable!("definitive activation outcomes returned above"),
                };
                let reconcile_deadline =
                    tokio::time::Instant::now() + ACTIVATION_FAST_RECONCILIATION;
                if let Some(route) =
                    wait_for_exact_activation(&activation_state, &activation, reconcile_deadline)
                        .await
                {
                    activation_state
                        .media_sessions
                        .cache_route(route.clone())
                        .await;
                    if route.owner_node_id == activation_state.node_id {
                        activation_state
                            .media_sessions
                            .seed_owned_lease(&route)
                            .await;
                    }
                    guard.disarm();
                    Ok(MediaSessionActivationOutcome {
                        route,
                        predecessor: None,
                    })
                } else {
                    // A timed-out replicated transaction may still commit
                    // after the caller loses its response. Transfer ownership
                    // to the bounded reconciler before disarming this guard.
                    let reconcile_state = activation_state.clone();
                    let reconcile_activation = activation.clone();
                    tokio::spawn(async move {
                        reconcile_or_abort_activation(reconcile_state, reconcile_activation, guard)
                            .await;
                    });
                    Err(error)
                }
            }
        }
    })
    .await
    .map_err(|error| ApiError::Internal(format!("media activation task failed: {error}")))??;
    crate::playstart::note_playback_started(
        &state,
        user.id,
        &user.username,
        id,
        method,
        Some(&request.playback_id),
    );
    Ok(Json(response))
}

fn resolved_replay_is_live(route: &MediaSessionRoute, now_ms: i64) -> bool {
    route.state == "active"
        && route.lease_expires_at_ms > now_ms.saturating_add(MIN_RESOLVED_REPLAY_REMAINING_MS)
}

fn route_matches_activation(
    route: &MediaSessionRoute,
    activation: &MediaSessionActivation,
) -> bool {
    route.incarnation_id == activation.incarnation_id
        && route.session_id == activation.session_id
        && route.user_id == activation.user_id
        && route.playback_id == activation.playback_id
        && route.request_fingerprint == activation.request_fingerprint
        && route.owner_node_id == activation.owner_node_id
        && route.state == "active"
        && route.lease_expires_at_ms > unix_ms()
        && route.recipe_json == activation.recipe_json
        && route.response_json == activation.response_json
}

async fn wait_for_exact_activation(
    state: &AppState,
    activation: &MediaSessionActivation,
    deadline: tokio::time::Instant,
) -> Option<MediaSessionRoute> {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return None;
        }
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .media_session_route_by_incarnation(&activation.incarnation_id),
        )
        .await
        {
            Ok(Ok(Some(route))) if route_matches_activation(&route, activation) => {
                return Some(route)
            }
            Ok(Ok(Some(_))) => return None,
            Ok(Ok(None)) | Ok(Err(_)) => {}
            Err(_) => return None,
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250).min(remaining)).await;
    }
}

async fn reconcile_or_abort_activation(
    state: AppState,
    activation: MediaSessionActivation,
    mut guard: StartedSessionGuard,
) {
    let deadline = tokio::time::Instant::now() + ACTIVATION_CONFIRMATION_WINDOW;
    if let Some(route) = wait_for_exact_activation(&state, &activation, deadline).await {
        state.media_sessions.cache_route(route.clone()).await;
        if route.owner_node_id == state.node_id {
            state.media_sessions.seed_owned_lease(&route).await;
        }
        guard.disarm();
    }
    // Dropping the armed guard aborts the exact worker, fails its exact claim,
    // and releases the replacement gate only after the full bounded verdict.
}

fn valid_playback_id(playback_id: &str) -> bool {
    !playback_id.trim().is_empty()
        && playback_id.len() <= 128
        && !playback_id
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0))
}

async fn abort_started_session(
    state: &AppState,
    owner_node_id: &str,
    incarnation_id: &str,
    session_id: &str,
) {
    if owner_node_id == state.node_id {
        state
            .transcode
            .stop_session_for_request(incarnation_id, session_id, "cluster start aborted")
            .await;
    } else {
        state
            .media_sessions
            .abort_remote(
                owner_node_id,
                &RemoteAbortRequest {
                    incarnation_id: incarnation_id.to_owned(),
                    session_id: session_id.to_owned(),
                },
            )
            .await;
    }
}

async fn stop_owned_session(state: &AppState, route: &MediaSessionRoute) {
    stop_owned_session_because(state, route, "superseded by cluster session").await
}

/// Same teardown with an honest reason: a client DELETE is a release, not a
/// supersession, and the VOD tombstone cause keys off this string.
async fn stop_owned_session_because(
    state: &AppState,
    route: &MediaSessionRoute,
    reason: &'static str,
) {
    state.media_sessions.cache_miss(&route.session_id).await;
    if route.owner_node_id == state.node_id {
        state
            .transcode
            .stop_session_for_request(&route.incarnation_id, &route.session_id, reason)
            .await;
    } else {
        state
            .media_sessions
            .abort_remote(
                &route.owner_node_id,
                &RemoteAbortRequest {
                    incarnation_id: route.incarnation_id.clone(),
                    session_id: route.session_id.clone(),
                },
            )
            .await;
    }
}

fn session_start_error(file_id: i64, error: String) -> ApiError {
    if error.contains("already used") {
        return ApiError::Conflict(error);
    }
    tracing::warn!(file = file_id, "session create failed: {error}");
    if crate::transcode::is_retryable_capacity_error(&error) {
        return ApiError::ServiceUnavailable(error);
    }
    if let Some(reason) = crate::transcode::invalid_reopen_reason(&error) {
        return ApiError::BadRequest(reason.to_owned());
    }
    if let Some((code, reason)) = crate::transcode::vod_refusal(&error) {
        let (status, code) = match code {
            "vod_disabled" => (StatusCode::SERVICE_UNAVAILABLE, "vod_disabled"),
            "vod_index_pending" => (StatusCode::SERVICE_UNAVAILABLE, "vod_index_pending"),
            "vod_transcode_unavailable" => {
                (StatusCode::NOT_IMPLEMENTED, "vod_transcode_unavailable")
            }
            "vod_subtitle_burn_unavailable" => {
                (StatusCode::NOT_IMPLEMENTED, "vod_subtitle_burn_unavailable")
            }
            "vod_source_unsupported" => {
                (StatusCode::UNPROCESSABLE_ENTITY, "vod_source_unsupported")
            }
            "vod_reopen_required" => (StatusCode::CONFLICT, "vod_reopen_required"),
            "live_presentation_removed" => (StatusCode::GONE, "live_presentation_removed"),
            _ => return ApiError::Internal(error),
        };
        return ApiError::typed(status, code, reason);
    }
    // A build that lacks a filter is not a server fault to be swallowed as
    // "internal server error" — it is a fact about this install that the
    // person pressing play can act on (pick a different subtitle track, or
    // tell whoever runs the box to install a full ffmpeg). 501, because the
    // request is well-formed and this server simply cannot implement it.
    if let Some(reason) = crate::transcode::unsupported_build_reason(&error) {
        return ApiError::typed(StatusCode::NOT_IMPLEMENTED, "unsupported_build", reason);
    }
    ApiError::Internal(error)
}

async fn relay_if_remote(
    state: &AppState,
    session_id: &str,
    resource: RelayResource,
    headers: RelayHeaders,
) -> Result<Option<Response>, ApiError> {
    let Some(route) = state.media_sessions.route(session_id).await? else {
        // A process upgraded with already-live sessions has no durable row;
        // keep the established node-local capability behavior until reaping.
        return Ok(None);
    };
    if route.state != "active" || route.lease_expires_at_ms <= unix_ms() {
        return Err(ApiError::typed(
            StatusCode::GONE,
            "media_session_ended",
            "this media session is no longer active",
        ));
    }
    if route.owner_node_id == state.node_id {
        return Ok(None);
    }
    state
        .media_sessions
        .relay(
            &route.owner_node_id,
            &RelayRequest {
                session_id: session_id.to_owned(),
                resource,
                headers,
            },
        )
        .await
        .map(Some)
        .map_err(|error| {
            ApiError::ServiceUnavailable(format!("media worker relay unavailable: {error:?}"))
        })
}

/// Execute an authenticated relay on the owning worker without performing a
/// second route lookup that could proxy back to the ingress node.
pub(crate) async fn relay_local(state: &AppState, request: RelayRequest) -> Response {
    let result = match request.resource {
        RelayResource::Status => status_local(state, &request.session_id)
            .await
            .map(IntoResponse::into_response),
        RelayResource::Playlist { native, subtitle } => {
            playlist_local(
                state,
                &request.session_id,
                PlaylistQuery {
                    native,
                    subtitle,
                    diagnostic: None,
                },
            )
            .await
        }
        RelayResource::Master {
            subtitle,
            diagnostic,
        } => {
            master_playlist_response_local(
                state,
                &request.session_id,
                PlaylistQuery {
                    native: None,
                    subtitle,
                    diagnostic,
                },
            )
            .await
        }
        RelayResource::VideoPlaylist => video_playlist_local(state, &request.session_id).await,
        RelayResource::SubtitlePlaylist { index } => {
            subtitle_playlist_local(state, &request.session_id, index).await
        }
        RelayResource::SubtitleSegment { index, segment } => {
            subtitle_vtt_local(state, &request.session_id, index, &segment).await
        }
        RelayResource::Segment { segment } => {
            segment_local(state, &request.session_id, &segment, &request.headers).await
        }
        RelayResource::Delete => {
            state.media_sessions.cache_miss(&request.session_id).await;
            state
                .transcode
                .stop_session(&request.session_id, "released through cluster relay")
                .await;
            return StatusCode::NO_CONTENT.into_response();
        }
    };
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// DELETE /api/v1/hls/:session — the player is done with this stream.
///
/// Capability auth, like the playlist and segment routes: the session id *is*
/// the credential, and requiring a header would stop the browser sending this
/// with `keepalive` as a tab closes — which is the whole point. Without it a
/// finished stream keeps its encoder for the 60-second idle timeout plus a
/// reaper tick, holding a hardware slot nobody is watching.
///
/// Idempotent: deleting a session that has already gone is a success, because
/// the caller's intent ("this must not be running") is satisfied either way.
pub async fn delete(State(state): State<AppState>, AxPath(session): AxPath<String>) -> StatusCode {
    match state.media_sessions.route(&session).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            state
                .transcode
                .stop_session(&session, "released by client")
                .await;
            return StatusCode::NO_CONTENT;
        }
        Err(error) => {
            tracing::warn!(%error, "durable media-session route unavailable");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }
    let route = match state.store.end_media_session(&session, unix_ms()).await {
        Ok(route) => route,
        Err(error) => {
            tracing::warn!(%error, "durable media-session release unavailable");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    match route {
        Some(route) => {
            state.media_sessions.cache_route(route.clone()).await;
            stop_owned_session_because(&state, &route, "released by client").await;
        }
        None => {
            state.media_sessions.cache_miss(&session).await;
            state
                .transcode
                .stop_session(&session, "released by client")
                .await;
        }
    }
    StatusCode::NO_CONTENT
}

/// GET /api/v1/files/:id/hls/start — **deprecated**; use `POST …/hls/sessions`.
///
/// Kept as a bridge for clients that predate the POST route, and implemented
/// over the same creation path so it cannot bypass identity or cleanup. Its
/// one concession: with no `playback_id` to key supersession by, it
/// synthesises the old (viewer, file) key, so its behaviour is exactly what it
/// was — including the two-devices-one-account collision the new route fixes.
pub async fn start(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
    Query(q): Query<StartQuery>,
    headers: HeaderMap,
    super::network::RemoteAddress(remote): super::network::RemoteAddress,
) -> Result<Json<StartResponse>, ApiError> {
    let legacy = CreateSession {
        playback_id: format!("legacy:{}:{id}", user.username),
        request_id: None,
        previous_session_id: None,
        reopen_reason: None,
        height: q.height,
        // The GET bridge has no quality-intent parameter, so it keeps the
        // wire-presence inference it has always had.
        quality_auto: None,
        subtitle_burn: None, // the deprecated GET bridge never offered a burn
        subtitle_burn_sdr: None,
        native_subtitles: None,
        subtitle: None,
        start: q.start,
        audio: q.audio,
        copy: Some(q.copy == Some(1)),
        aac: Some(q.aac == Some(1)),
        preserve_dolby_vision: Some(false),
        // The deprecated GET bridge has no capability parameters at all, so
        // it keeps the SDR ladder it has always had.
        hdr10: None,
        audio_offset_ms: None,
        presentation: None,
        block_budget_secs: None,
    };
    create(
        AuthUser(user),
        State(state),
        AxPath(id),
        headers,
        super::network::RemoteAddress(remote),
        Json(legacy),
    )
    .await
}

/// GET /api/v1/hls/:session/status — how the session is actually doing.
///
/// Capability auth like its siblings (the session id is the credential), and
/// deliberately outside the `{segment}` route: a static path segment wins over
/// a parameter in the router, and `status` is not a valid segment name anyway.
///
/// This exists because the first question every buffering report asks —
/// *is the server keeping up?* — had no answer anywhere in the product. The
/// player puts `speed` and `ahead_seconds` in the stats overlay, so "why did
/// it stutter" resolves to a number instead of a theory.
pub async fn status(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Status,
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    status_local(&state, &session)
        .await
        .map(IntoResponse::into_response)
}

async fn status_local(
    state: &AppState,
    session: &str,
) -> Result<Json<crate::transcode::HlsSessionInfo>, ApiError> {
    state
        .transcode
        .hls_session_status(session)
        .await
        .map(Json)
        .ok_or(ApiError::NotFound("hls session"))
}

#[derive(Default, Deserialize)]
pub struct PlaylistQuery {
    pub native: Option<u8>,
    pub subtitle: Option<i64>,
    /// Temporary physical-device isolation mode for the Apple HDR master.
    /// The session UUID remains the capability; these modes only remove
    /// declarations from the playlist and never expose another resource.
    pub diagnostic: Option<String>,
}

fn playlist_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.apple.mpegurl".to_owned(),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

async fn session_file(
    state: &AppState,
    session: &str,
) -> Result<(crate::transcode::HlsContext, MediaFile), ApiError> {
    let mut context = state
        .transcode
        .hls_context(session)
        .await
        .ok_or(ApiError::NotFound("transcode session"))?;
    let file = state
        .store
        .get_file(context.file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    context.frame_rate = state
        .store
        .get_file_probe_json(context.file_id)
        .await?
        .as_deref()
        .and_then(video_frame_rate);
    Ok((context, file))
}

/// Maximum frame rate from ffprobe's persisted source description.
///
/// Fractions are kept until the playlist is rendered so NTSC rates retain
/// their 24000/1001 or 30000/1001 meaning. `avg_frame_rate` is preferred;
/// `r_frame_rate` is the fallback for older probe output.
fn video_frame_rate(probe_json: &str) -> Option<f64> {
    fn fraction(raw: &str) -> Option<f64> {
        let (numerator, denominator) = raw.split_once('/')?;
        let numerator = numerator.parse::<f64>().ok()?;
        let denominator = denominator.parse::<f64>().ok()?;
        let rate = numerator / denominator;
        (rate.is_finite() && rate > 0.0).then_some(rate)
    }

    let probe: serde_json::Value = serde_json::from_str(probe_json).ok()?;
    let stream = probe
        .get("streams")?
        .as_array()?
        .iter()
        .find(|stream| stream.get("codec_type").and_then(|v| v.as_str()) == Some("video"))?;
    ["avg_frame_rate", "r_frame_rate"]
        .into_iter()
        .find_map(|key| stream.get(key).and_then(|v| v.as_str()).and_then(fraction))
}

/// GET /api/v1/hls/:session/index.m3u8 — capability auth (see module docs).
///
/// Existing clients receive the historical media playlist. The `?native=1`
/// form remains as a compatibility bridge for Apple sessions created before
/// the dedicated master-playlist path existed.
pub async fn playlist(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    Query(query): Query<PlaylistQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Playlist {
            native: query.native,
            subtitle: query.subtitle,
        },
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    playlist_local(&state, &session, query).await
}

async fn playlist_local(
    state: &AppState,
    session: &str,
    query: PlaylistQuery,
) -> Result<Response, ApiError> {
    // A VOD session's child media playlist is the plan's immutable artifact.
    // The dedicated master path wraps it when native subtitles were requested;
    // the legacy `?native=1` bridge still needs that same wrapper.
    if let Some(answer) = state.transcode.vod_playlist(session).await {
        let bytes = answer.map_err(|err| vod_error(session, err))?;
        if query.native == Some(1) {
            let (context, file) = session_file(state, session).await?;
            let context = exact_hls_context(state, session, context).await;
            return Ok(playlist_response(
                master_playlist(&file, query.subtitle, &context).into_bytes(),
            ));
        }
        return Ok(playlist_response(bytes));
    }
    if query.native != Some(1) {
        return video_playlist_local(state, session).await;
    }
    let (context, file) = session_file(state, session).await?;
    let context = exact_hls_context(state, session, context).await;
    Ok(playlist_response(
        master_playlist(&file, query.subtitle, &context).into_bytes(),
    ))
}

/// The multivariant playlist used by Apple clients for native subtitles and
/// HDR variant signaling.
///
/// It has a distinct path from the child media playlist so AVPlayer cannot
/// collapse the two resources when applying URL-query cache normalization.
pub async fn master_playlist_response(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    Query(query): Query<PlaylistQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Master {
            subtitle: query.subtitle,
            diagnostic: query.diagnostic.clone(),
        },
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    master_playlist_response_local(&state, &session, query).await
}

async fn master_playlist_response_local(
    state: &AppState,
    session: &str,
    query: PlaylistQuery,
) -> Result<Response, ApiError> {
    let (context, file) = session_file(state, session).await?;
    let context = exact_hls_context(state, session, context).await;
    // Apple's multivariant eligibility check rejects UHD Blu-ray-style HEVC
    // High-tier declarations before VideoToolbox sees bytes it can decode.
    // With no native text renditions, the wrapper buys this session nothing:
    // serve the same proven fMP4 media playlist from this capability URL. The
    // initialization-segment response translates the tier declaration into
    // the Apple eligibility envelope without changing the coded picture.
    // Bitmap/styled subtitles already use burn-in, so they do not depend on a
    // multivariant subtitle group.
    if query.diagnostic.is_none() && should_serve_high_tier_media_playlist(&file, &context) {
        tracing::info!(
            session = %crate::transcode::session_log_id(session),
            codecs = %context.codecs,
            "serving high-tier HEVC through the direct media-playlist envelope"
        );
        return video_playlist_local(state, session).await;
    }
    Ok(playlist_response(
        master_playlist_diagnostic(&file, query.subtitle, &context, query.diagnostic.as_deref())
            .into_bytes(),
    ))
}

/// Translate a session's own verdict into the response the client acts on.
///
/// Every one of these used to be `ApiError::NotFound("transcode session")` — a
/// bare 404 that hls.js escalates to a fatal `levelLoadError` whatever caused
/// it, so a session still inside the server's startup recovery was reported to
/// the viewer as permanently broken and a session that had genuinely failed
/// was reported as nothing at all. The status separates the three cases the
/// client can actually act on differently, and the typed body carries the
/// reason a person can read.
///
/// - 404: the session is gone. Re-open, or accept that it is over.
/// - 503: still starting. **Retryable**, and hls.js's own level-load retry is
///   the right response — the stream is being built right now.
/// - 502: the producer or the session failed. Terminal; report it.
fn playlist_error(session: &str, err: PlaylistError) -> ApiError {
    let status = match &err {
        PlaylistError::SessionGone => StatusCode::NOT_FOUND,
        PlaylistError::StartupTimedOut(_) => StatusCode::SERVICE_UNAVAILABLE,
        PlaylistError::ProducerExited(_) | PlaylistError::SessionFailed(_) => {
            StatusCode::BAD_GATEWAY
        }
    };
    tracing::warn!(
        session = %crate::transcode::session_log_id(session),
        code = err.code(),
        retryable = err.retryable(),
        "HLS playlist request refused: {}",
        err.message()
    );
    ApiError::typed(status, err.code(), err.message())
}

/// The video rendition referenced by the native-subtitle HLS master.
pub async fn video_playlist(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::VideoPlaylist,
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    video_playlist_local(&state, &session).await
}

async fn video_playlist_local(state: &AppState, session: &str) -> Result<Response, ApiError> {
    if let Some(answer) = state.transcode.vod_playlist(session).await {
        let bytes = answer.map_err(|err| vod_error(session, err))?;
        return Ok(playlist_response(bytes));
    }
    match state.transcode.playlist(session).await {
        Ok(bytes) => Ok(playlist_response(bytes)),
        Err(PlaylistError::SessionGone) if vod_resurrected(state, session).await => {
            match state.transcode.vod_playlist(session).await {
                Some(answer) => {
                    let bytes = answer.map_err(|err| vod_error(session, err))?;
                    Ok(playlist_response(bytes))
                }
                None => Err(playlist_error(session, PlaylistError::SessionGone)),
            }
        }
        Err(err) => Err(playlist_error(session, err)),
    }
}

/// One native WebVTT rendition's media playlist. Its segments mirror the
/// video rendition so AVPlayer sees matching playlist types and timelines.
/// Every child resource is still cut from the one cached sidecar.
pub async fn subtitle_playlist(
    State(state): State<AppState>,
    AxPath((session, index)): AxPath<(String, i64)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::SubtitlePlaylist { index },
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    subtitle_playlist_local(&state, &session, index).await
}

async fn subtitle_playlist_local(
    state: &AppState,
    session: &str,
    index: i64,
) -> Result<Response, ApiError> {
    let (_, file) = session_file(state, session).await?;
    let track = file
        .subtitle_streams
        .get(index as usize)
        .ok_or(ApiError::NotFound("subtitle track"))?;
    if !is_native_text_subtitle(&track.codec) {
        return Err(ApiError::BadRequest(
            "this subtitle requires burn-in".into(),
        ));
    }
    crate::subtitles::warm_vtt(&state.subs_dir, &file, index).await;
    let video = match state.transcode.vod_playlist(session).await {
        Some(answer) => answer.map_err(|err| vod_error(session, err))?,
        None => state
            .transcode
            .playlist(session)
            .await
            .map_err(|err| playlist_error(session, err))?,
    };
    Ok(playlist_response(
        subtitle_media_playlist(&video).into_bytes(),
    ))
}

/// Capability-authenticated VTT data for AVPlayer's autonomous child fetch.
/// Each resource mirrors one video segment's time window. Cues are shifted
/// onto the session-relative video timeline, so a session opened at a
/// resume/seek offset still presents captions at the right frame.
pub async fn subtitle_vtt(
    State(state): State<AppState>,
    AxPath((session, index, segment)): AxPath<(String, i64, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::SubtitleSegment {
            index,
            segment: segment.clone(),
        },
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    subtitle_vtt_local(&state, &session, index, &segment).await
}

async fn subtitle_vtt_local(
    state: &AppState,
    session: &str,
    index: i64,
    segment: &str,
) -> Result<Response, ApiError> {
    let (context, file) = session_file(state, session).await?;
    let track = file
        .subtitle_streams
        .get(index as usize)
        .ok_or(ApiError::NotFound("subtitle track"))?;
    if !is_native_text_subtitle(&track.codec) {
        return Err(ApiError::BadRequest(
            "this subtitle requires burn-in".into(),
        ));
    }
    let sequence = segment
        .strip_prefix("seg")
        .and_then(|value| value.strip_suffix(".vtt"))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(ApiError::NotFound("subtitle segment"))?;
    let sequence = i64::try_from(sequence).map_err(|_| ApiError::NotFound("subtitle segment"))?;
    let (segment_start, segment_end) = state
        .transcode
        .segment_window(session, sequence)
        .await
        .ok_or(ApiError::NotFound("subtitle segment"))?;
    let (bytes, cache_control) =
        match crate::subtitles::read_cached_vtt(&state.subs_dir, &file, index).await {
            Ok(Some(bytes)) => {
                tracing::info!(
                    session = %crate::transcode::session_log_id(session),
                    file_id = file.id,
                    index,
                    codec = %track.codec,
                    language = track.language.as_deref().unwrap_or("und"),
                    title = track.title.as_deref().unwrap_or(""),
                    start_seconds = context.start_seconds,
                    "serving native HLS WebVTT subtitle"
                );
                (bytes, "private, max-age=3600")
            }
            Ok(None) | Err(_) => {
                // AVPlayer gives a subtitle segment only about two seconds to
                // answer and blocks the muxed video while it waits. Extracting an
                // embedded text track is a full-source scan that can legitimately
                // take minutes on a large MKV over a NAS, so awaiting `ensure_vtt`
                // here turns healthy Dolby Vision, HDR and H.264 streams into a
                // black screen. Publish a syntactically valid empty segment now
                // and let the deduplicated cache extraction finish independently.
                // `no-store` lets a player retry this window once the sidecar is
                // ready instead of pinning the temporary empty answer.
                crate::subtitles::warm_vtt(&state.subs_dir, &file, index).await;
                tracing::debug!(
                    session = %crate::transcode::session_log_id(session),
                    file_id = file.id,
                    index,
                    "serving an empty subtitle segment while its sidecar cache warms"
                );
                (b"WEBVTT\n\n".to_vec(), "no-store")
            }
        };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            (header::CACHE_CONTROL, cache_control),
        ],
        // The session's MEDIA origin, not the offset that was requested. A
        // copy session seeks with `-noaccurate_seek`, so its timeline begins
        // at the keyframe before the requested start — shifting cues by the
        // request made them lead the picture by up to a whole GOP (1–6 s on a
        // 4K film) on every resumed or seeked copy session, which is the
        // flagship Apple path.
        slice_webvtt(
            &bytes,
            context.media_origin_seconds,
            segment_start,
            segment_end,
        ),
    )
        .into_response())
}

fn quoted(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '\r' | '\n' => ' ',
            other => other,
        })
        .collect()
}

/// BCP-47 for the `LANGUAGE=` attribute. One line, because the knowledge of
/// which spellings mean the same language belongs to `plurx_core::tracks` —
/// this module used to keep a ten-language copy of it, which silently passed
/// "dut"/"cze"/"gre" through as non-BCP-47 and defeated viewer-language
/// matching for every language the copy had not learned.
fn language_tag(raw: Option<&str>) -> &str {
    plurx_core::tracks::bcp47_tag(raw)
}

/// Replace the scanner's best-effort HEVC tier with the exact declaration
/// carried by the HLS initialization segment.
///
/// ffprobe records Main/Main10 and level in the library row but commonly
/// omits the tier. Guessing Main tier then advertises `L150` for a High-tier
/// `hvcC` record (`H150`), and AVPlayer treats the only HDR variant as
/// unsupported. The init segment is the canonical description of the bytes
/// this session actually publishes, including any Dolby Vision stripping.
///
/// A `dvh1`/`dvhe` sample entry is NOT described by the HEVC form. Dolby
/// Vision's HLS identifier is `dvh1.PP.LL` — two-digit profile, two-digit
/// level — and the `hvcC` beside it describes only the base layer, so reading
/// the tier out of it and emitting `dvh1.2.4.L150.90` produces a string no
/// player can resolve. AVPlayer rejected exactly that on a Profile 5 title
/// while the same session's media playlist, which carries no CODECS at all,
/// played. Dolby Vision therefore reads its own `dvcC`/`dvvC` configuration
/// record, which is also the canonical answer when the library row's probe
/// carried no DOVI side data and the advertised codec came through bare.
async fn exact_hls_context(
    state: &AppState,
    session: &str,
    mut context: crate::transcode::HlsContext,
) -> crate::transcode::HlsContext {
    let fallback_video = context.codecs.split(',').next().unwrap_or_default();
    let Some(sample_entry) = ["hvc1", "hev1", "dvh1", "dvhe"]
        .into_iter()
        .find(|entry| fallback_video.starts_with(entry))
    else {
        return context;
    };
    // A fenced successor names its init after its ownership epoch, so the
    // object to probe comes from the session, not from a literal. Asking for
    // the wrong name does not fail fast: `segment` waits for a segment that
    // will never be produced, stalling every playlist request for the full
    // production wait before falling back to the scanner's guessed tier.
    let Some(init_object) = state.transcode.session_init_object(session).await else {
        return context;
    };
    // Initialization segments are a few KiB. Bound malformed input so a
    // playlist request can never allocate without limit. VOD init bytes come
    // through their own registry; live bytes retain the internal delivery
    // tracker that keeps this probe out of player throughput telemetry.
    let mut init = Vec::new();
    match state.transcode.vod_segment(session, &init_object).await {
        Some(Ok(Some(ready))) => {
            init.reserve(ready.len.min(INIT_INSPECTION_LIMIT_BYTES) as usize);
            let mut reader = ready.file.take(INIT_INSPECTION_LIMIT_BYTES);
            if reader.read_to_end(&mut init).await.is_err() {
                return context;
            }
        }
        Some(_) => return context,
        None => {
            let Ok(Some(opened)) = state.transcode.segment(session, &init_object).await else {
                return context;
            };
            init.reserve(opened.len.min(INIT_INSPECTION_LIMIT_BYTES) as usize);
            // No response body exists here — this is the playlist generator
            // reading `hvcC` for itself.
            let mut delivery = opened.delivery.into_internal_probe();
            delivery.expect_at_most(INIT_INSPECTION_LIMIT_BYTES);
            let started = Instant::now();
            let mut reader = opened.file.take(INIT_INSPECTION_LIMIT_BYTES);
            match reader.read_to_end(&mut init).await {
                Ok(bytes) => {
                    delivery.note_read(bytes as u64, started.elapsed());
                    delivery.finish();
                }
                Err(error) => {
                    delivery.fail(&error);
                    return context;
                }
            }
        }
    }
    let derived = if matches!(sample_entry, "dvh1" | "dvhe") {
        dolby_vision_codec_from_init(&init, sample_entry)
    } else {
        hevc_codec_from_init(&init, sample_entry)
    };
    let Some(video) = derived else {
        return context;
    };
    context.codecs = match context.codecs.split_once(',') {
        Some((_, audio)) if !audio.trim().is_empty() => format!("{video},{}", audio.trim()),
        _ => video,
    };
    context
}

/// Apple's Dolby Vision HLS identifier from the `DOVIDecoderConfigurationRecord`.
///
/// The record is carried by `dvcC` (profiles up to 7) or `dvvC` (profiles 8
/// and 9) inside the sample entry. Its payload begins with two version bytes
/// and then packs the two fields the playlist needs across two more:
///
/// ```text
/// byte 2: dv_profile (7 bits) | dv_level high bit
/// byte 3: dv_level low 5 bits | rpu_present | el_present | bl_present
/// ```
///
/// Apple's form is `dvh1.PP.LL` with both fields zero-padded to two digits —
/// no tier letter, no constraint bytes, and nothing from `hvcC`.
fn dolby_vision_codec_from_init(init: &[u8], sample_entry: &str) -> Option<String> {
    let type_at = [b"dvcC", b"dvvC"]
        .into_iter()
        .find_map(|name| init.windows(4).position(|window| window == name))?;
    let box_start = type_at.checked_sub(4)?;
    let box_size = u32::from_be_bytes(init.get(box_start..type_at)?.try_into().ok()?) as usize;
    let box_end = box_start.checked_add(box_size)?;
    // 8-byte header plus the 24-byte record.
    if box_size < 32 || box_end > init.len() {
        return None;
    }
    let payload = init.get(type_at + 4..box_end)?;
    if payload.len() < 4 {
        return None;
    }
    let profile = payload[2] >> 1;
    let level = ((payload[2] & 0x01) << 5) | (payload[3] >> 3);
    // Profile 0 is not assigned and level 0 is not a real declaration; either
    // means the record was not what this parser assumed.
    if profile == 0 || level == 0 {
        return None;
    }
    Some(format!("{sample_entry}.{profile:02}.{level:02}"))
}

/// RFC 6381 identifier from the HEVCDecoderConfigurationRecord in `hvcC`.
fn hevc_codec_from_init(init: &[u8], sample_entry: &str) -> Option<String> {
    let type_at = init.windows(4).position(|window| window == b"hvcC")?;
    let box_start = type_at.checked_sub(4)?;
    let box_size = u32::from_be_bytes(init.get(box_start..type_at)?.try_into().ok()?) as usize;
    let box_end = box_start.checked_add(box_size)?;
    if box_size < 21 || box_end > init.len() {
        return None;
    }
    let payload = init.get(type_at + 4..box_end)?;
    if payload.len() < 13 || payload[0] != 1 {
        return None;
    }

    let profile_byte = payload[1];
    let profile_space = match profile_byte >> 6 {
        0 => "",
        1 => "A",
        2 => "B",
        3 => "C",
        _ => return None,
    };
    let profile = profile_byte & 0x1f;
    let tier = if profile_byte & 0x20 == 0 { 'L' } else { 'H' };
    let compatibility = u32::from_be_bytes(payload.get(2..6)?.try_into().ok()?).reverse_bits();
    let level = payload[12];
    let constraints = payload.get(6..12)?;
    let constraints_len = constraints
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |last| last + 1);
    let constraints = constraints[..constraints_len]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();

    let mut codec =
        format!("{sample_entry}.{profile_space}{profile}.{compatibility:X}.{tier}{level}");
    if !constraints.is_empty() {
        codec.push('.');
        codec.push_str(&constraints);
    }
    Some(codec)
}

/// Whether an Apple HDR master should collapse to its media rendition.
///
/// This is deliberately narrower than "HDR": Main-tier HDR/Dolby Vision and
/// every session with native text renditions keep the authored multivariant
/// playlist. The workaround applies only to the High-tier codec declaration
/// AVPlayer rejects and only when removing the wrapper loses no rendition.
fn should_serve_high_tier_media_playlist(
    file: &MediaFile,
    context: &crate::transcode::HlsContext,
) -> bool {
    let has_native_subtitles = file
        .subtitle_streams
        .iter()
        .any(|track| is_native_text_subtitle(&track.codec));
    if file.hdr.is_none() || has_native_subtitles {
        return false;
    }
    let video = context.codecs.split(',').next().unwrap_or_default();
    let mut fields = video.split('.');
    let sample_entry = fields.next().unwrap_or_default();
    matches!(sample_entry, "hvc1" | "hev1" | "dvh1" | "dvhe")
        && fields.any(|field| {
            field.strip_prefix('H').is_some_and(|level| {
                !level.is_empty() && level.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
}

/// Clear the HEVC High-tier declaration AVPlayer rejects for otherwise
/// hardware-decodable HDR copy sessions.
///
/// Tier changes decoder throughput limits, not the coded picture or decoder
/// tools. Relabeling only the generated initialization record lets the bytes
/// reach VideoToolbox instead of failing AVPlayer's HLS preparation step. This
/// is deliberately limited to the same HDR/no-native-rendition sessions whose
/// master is collapsed above.
fn normalize_high_tier_hevc_init(file: &MediaFile, init: &mut [u8]) -> bool {
    if file.hdr.is_none()
        || file
            .subtitle_streams
            .iter()
            .any(|track| is_native_text_subtitle(&track.codec))
    {
        return false;
    }
    let Some(type_at) = init.windows(4).position(|window| window == b"hvcC") else {
        return false;
    };
    let Some(box_start) = type_at.checked_sub(4) else {
        return false;
    };
    let Some(size_bytes) = init.get(box_start..type_at) else {
        return false;
    };
    let Ok(size_bytes) = size_bytes.try_into() else {
        return false;
    };
    let box_size = u32::from_be_bytes(size_bytes) as usize;
    let Some(box_end) = box_start.checked_add(box_size) else {
        return false;
    };
    let Some(payload) = init.get_mut(type_at + 4..box_end) else {
        return false;
    };
    if box_size < 21 || payload.len() < 13 || payload[0] != 1 || payload[1] & 0x20 == 0 {
        return false;
    }
    payload[1] &= !0x20;
    true
}

/// The human half of a rendition's `NAME`. Display names are a presentation
/// concern, so they live here rather than in core — but the set is kept in
/// step with the alias table `language_tag` reads, so a language that matches
/// never renders as a bare three-letter code.
fn language_name(raw: Option<&str>) -> &str {
    match language_tag(raw) {
        "en" => "English",
        "it" => "Italian",
        "ja" => "Japanese",
        "es" => "Spanish",
        "fr" => "French",
        "de" => "German",
        "pt" => "Portuguese",
        "ko" => "Korean",
        "zh" => "Chinese",
        "ru" => "Russian",
        "hi" => "Hindi",
        "ar" => "Arabic",
        "nl" => "Dutch",
        "sv" => "Swedish",
        "pl" => "Polish",
        "no" => "Norwegian",
        "da" => "Danish",
        "fi" => "Finnish",
        "tr" => "Turkish",
        "th" => "Thai",
        "vi" => "Vietnamese",
        "uk" => "Ukrainian",
        "cs" => "Czech",
        "el" => "Greek",
        "he" => "Hebrew",
        "hu" => "Hungarian",
        "ro" => "Romanian",
        other => other,
    }
}

fn subtitle_name(track: &SubtitleStream, ordinal: usize) -> String {
    let language = language_name(track.language.as_deref());
    match track
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(title) if language != "und" => format!("{language} · {title}"),
        Some(title) => title.to_owned(),
        None if language != "und" => language.to_owned(),
        None => format!("Subtitle {}", ordinal + 1),
    }
}

/// Does a track *title* declare the track forced?
///
/// Title-based detection is contract, not cleanup fodder: file 5615's forced
/// Italian track carries `disposition.forced = false` and the title "Forced",
/// and that regression case is why this exists at all (handoff §3.4).
///
/// But a substring test is too eager. "Non-Forced" and "Unforced" are real
/// titles, and classifying them FORCED=YES hides an ordinary subtitle track
/// from Apple's subtitle menu entirely — a forced rendition is only offered
/// when the presentation language matches. So: match "forced" on word
/// boundaries, and reject it when the preceding word negates it. "Unforced"
/// falls out for free, because the `n` in front of it is not a boundary.
fn title_marks_forced(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut consumed = 0usize;
    while let Some(at) = rest.find("forced") {
        let start = consumed + at;
        let end = start + "forced".len();
        let before = lower[..start].chars().next_back();
        let after = lower[end..].chars().next();
        let bounded =
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric);
        if bounded && !negated_before(&lower[..start]) {
            return true;
        }
        consumed = end;
        rest = &lower[end..];
    }
    false
}

/// The word immediately before a "forced" occurrence, when it turns the claim
/// around. Separators are skipped, so "non-forced", "non forced" and
/// "not forced" are all caught by the same rule.
fn negated_before(prefix: &str) -> bool {
    let trimmed = prefix.trim_end_matches(|c: char| !c.is_alphanumeric());
    let word = trimmed
        .rsplit(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("");
    matches!(word, "non" | "not" | "no" | "never")
}

fn track_is_forced(track: &SubtitleStream) -> bool {
    track.forced || track.title.as_deref().is_some_and(title_marks_forced)
}

/// The Apple accessibility `CHARACTERISTICS` for an SDH / hard-of-hearing
/// rendition — the tag that lets a viewer who needs captions find them by
/// what they *do* rather than by what someone happened to name them.
///
/// The container's own `hearing_impaired` disposition answers first: it is
/// the authored fact, and it is right even for a track called "English 2".
/// The title sniff stays as the fallback, because a library probed before
/// plurx read that disposition has nothing else to go on and re-probing is
/// manual — so the naming convention keeps working exactly as it did.
fn subtitle_characteristics(track: &SubtitleStream) -> Option<&'static str> {
    const ACCESSIBILITY: &str =
        "public.accessibility.transcribes-spoken-dialog,public.accessibility.describes-music-and-sound";
    if track.hearing_impaired {
        return Some(ACCESSIBILITY);
    }
    let title = track.title.as_deref()?.to_ascii_lowercase();
    [
        "sdh",
        "closed caption",
        "closed-caption",
        "hard of hearing",
        "non udenti",
    ]
    .iter()
    .any(|marker| title.contains(marker))
    .then_some(ACCESSIBILITY)
}

/// Rendition `NAME`s, made unique.
///
/// RFC 8216 §4.3.4.1 makes NAME a MUST-unique quoted string within a group,
/// and two same-language untitled tracks otherwise both render as "English".
/// AVFoundation is entitled to merge them, and the client resolves options by
/// name — so a duplicate is not a cosmetic wart, it is the client selecting
/// the wrong track or none at all. Disambiguate by occurrence, leaving the
/// first one alone so the common single-track case reads naturally.
fn unique_subtitle_names(native: &[(usize, &SubtitleStream)]) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    native
        .iter()
        .map(|(index, track)| {
            let base = subtitle_name(track, *index);
            let count = seen.entry(base.clone()).or_insert(0);
            *count += 1;
            match *count {
                1 => base,
                n => format!("{base} ({n})"),
            }
        })
        .collect()
}

/// Candidate master-playlist changes from the §5.4 ladder, each off by
/// default and enabled one at a time for one deploy.
///
/// Every master regression in this arc passed unit tests and failed on the
/// physical device, so the device is the only oracle and the ladder exists to
/// keep exactly one variable moving per deploy. These are compiled in but
/// inert until an operator sets the variable, which is what lets a rung be
/// tried, observed on Bedroom, and either kept or dropped without another
/// build. Once a rung is accepted, delete the flag and make it unconditional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct MasterRungs {
    /// `CLOSED-CAPTIONS=NONE` on the variant. Apple's authoring rules ask for
    /// it, and it also stops AVFoundation synthesising a phantom
    /// closed-caption option into the `.legible` group.
    closed_captions_none: bool,
    /// `AUTOSELECT=YES` on forced renditions, which Apple's authoring rules
    /// require and this master currently withholds when two forced tracks
    /// share a language.
    forced_autoselect: bool,
}

impl MasterRungs {
    fn enabled(name: &str) -> bool {
        std::env::var(name).is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    }

    /// Read once: an operator sets these before plurxd starts, and a master
    /// that could change shape between two fetches of the same session is a
    /// worse problem than any rung solves.
    fn active() -> Self {
        static ACTIVE: std::sync::OnceLock<MasterRungs> = std::sync::OnceLock::new();
        *ACTIVE.get_or_init(|| MasterRungs {
            closed_captions_none: Self::enabled("PLURX_HLS_CLOSED_CAPTIONS_NONE"),
            forced_autoselect: Self::enabled("PLURX_HLS_FORCED_AUTOSELECT"),
        })
    }
}

fn master_playlist(
    file: &MediaFile,
    selected: Option<i64>,
    context: &crate::transcode::HlsContext,
) -> String {
    master_playlist_with_shape(
        file,
        selected,
        context,
        MasterRungs::active(),
        MasterShape::default(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MasterShape {
    subtitles: bool,
    codecs: bool,
    video_range: bool,
}

impl Default for MasterShape {
    fn default() -> Self {
        Self {
            subtitles: true,
            codecs: true,
            video_range: true,
        }
    }
}

fn master_playlist_diagnostic(
    file: &MediaFile,
    selected: Option<i64>,
    context: &crate::transcode::HlsContext,
    diagnostic: Option<&str>,
) -> String {
    let shape = match diagnostic {
        Some("video-only") => MasterShape {
            subtitles: false,
            codecs: false,
            video_range: false,
        },
        Some("video-only-codecs") => MasterShape {
            subtitles: false,
            codecs: true,
            video_range: false,
        },
        Some("video-only-range") => MasterShape {
            subtitles: false,
            codecs: false,
            video_range: true,
        },
        Some("video-only-hdr") => MasterShape {
            subtitles: false,
            codecs: true,
            video_range: true,
        },
        _ => MasterShape::default(),
    };
    master_playlist_with_shape(file, selected, context, MasterRungs::active(), shape)
}

#[cfg(test)]
fn master_playlist_with(
    file: &MediaFile,
    selected: Option<i64>,
    context: &crate::transcode::HlsContext,
    rungs: MasterRungs,
) -> String {
    master_playlist_with_shape(file, selected, context, rungs, MasterShape::default())
}

fn master_playlist_with_shape(
    file: &MediaFile,
    selected: Option<i64>,
    context: &crate::transcode::HlsContext,
    rungs: MasterRungs,
    shape: MasterShape,
) -> String {
    let native: Vec<(usize, &SubtitleStream)> = file
        .subtitle_streams
        .iter()
        .enumerate()
        .filter(|(_, track)| is_native_text_subtitle(&track.codec))
        .collect();
    let names = unique_subtitle_names(&native);
    // Copy/remux sessions can contain open GOPs, so the video rendition does
    // not promise independently decodable segments. The master must not make
    // that stronger claim on its behalf: AVPlayer acts on it at a resume
    // boundary and can reject an otherwise playable copied HEVC/DV stream.
    // SUPPLEMENTAL-CODECS was introduced at HLS compatibility version 10.
    // Advertising it from a version-7 master makes AVPlayer reject the
    // otherwise valid Profile 8.1/8.4 rendition during item preparation, and
    // the Apple client then takes its final H.264/SDR compatibility fallback.
    // Keep ordinary masters at version 7; only the enhanced-codec declaration
    // needs the newer contract.
    let compatibility_version = if shape.codecs && context.supplemental_codecs.is_some() {
        10
    } else {
        7
    };
    let mut out = format!("#EXTM3U\n#EXT-X-VERSION:{compatibility_version}\n");
    for (ordinal, (index, track)) in native.iter().enumerate() {
        if !shape.subtitles {
            break;
        }
        // The query describes this player's selection. No selected index is
        // an explicit Off, not permission to resurrect a foreign-language
        // container default behind the client's back.
        let forced = track_is_forced(track);
        // A forced rendition is a narrow set of cues the player may use when
        // its language matches the presentation. Apple's authoring examples
        // deliberately keep those renditions DEFAULT=NO: DEFAULT=YES means
        // "play this whole selection absent any user choice", which is not
        // the same thing as FORCED=YES and makes AVPlayer reject or repeatedly
        // reload some subtitle groups. The Apple client explicitly selects
        // its preferred media option once the item exposes the legible group.
        let default = !forced && selected.is_some_and(|pick| pick == *index as i64);
        let characteristics = subtitle_characteristics(track);
        // RFC 8216 requires every AUTOSELECT=YES member of a rendition group
        // to have a unique LANGUAGE/ASSOC-LANGUAGE/FORCED/CHARACTERISTICS
        // combination. AVPlayer rejects the entire master when two ordinary
        // tracks share that tuple, so exact duplicates stay manually
        // selectable. DEFAULT=YES itself requires AUTOSELECT=YES.
        let tuple_copies = native
            .iter()
            .filter(|(_, other)| {
                language_tag(other.language.as_deref()) == language_tag(track.language.as_deref())
                    && track_is_forced(other) == forced
                    && subtitle_characteristics(other) == characteristics
            })
            .count();
        // Ladder rung (P2-11): Apple's authoring rules say a forced rendition
        // is AUTOSELECT=YES — the player is meant to reach it on its own when
        // the presentation language matches. Today two forced tracks sharing
        // a language both come out AUTOSELECT=NO, because RFC 8216's
        // uniqueness rule is written in terms of AUTOSELECT=YES members and
        // AVPlayer has been observed rejecting a whole master over it. The
        // two rules genuinely conflict; only the device settles which one it
        // enforces, so this is a rung and not a fix.
        let autoselect = default || tuple_copies == 1 || (rungs.forced_autoselect && forced);
        let characteristics = characteristics
            .map(|value| format!(",CHARACTERISTICS=\"{value}\""))
            .unwrap_or_default();
        out.push_str(&format!(
            "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"{}\",LANGUAGE=\"{}\",DEFAULT={},AUTOSELECT={},FORCED={}{},URI=\"subs/{index}/index.m3u8\"\n",
            quoted(&names[ordinal]),
            quoted(language_tag(track.language.as_deref())),
            if default { "YES" } else { "NO" },
            if autoselect { "YES" } else { "NO" },
            if forced { "YES" } else { "NO" },
            characteristics,
        ));
    }
    let bandwidth = file.bitrate.unwrap_or(25_000_000).max(128_000);
    out.push_str(&format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},AVERAGE-BANDWIDTH={bandwidth}"
    ));
    if let (Some(width), Some(height)) = (file.width, file.height) {
        if width > 0 && height > 0 {
            out.push_str(&format!(",RESOLUTION={width}x{height}"));
        }
    }
    if let Some(frame_rate) = context
        .frame_rate
        .filter(|rate| rate.is_finite() && *rate > 0.0)
    {
        out.push_str(&format!(",FRAME-RATE={frame_rate:.3}"));
    }
    // HDR is not self-describing at HLS's variant-selection layer. Apple
    // requires VIDEO-RANGE before it opens the HEVC init segment, and CODECS
    // must name the exact Main10 profile/level carried by this session. The
    // source flag alone is not sufficient: an H.264 session from the same HDR
    // source has already been tone-mapped, so its master must remain SDR.
    let video_codec = context.codecs.split(',').next().unwrap_or_default();
    let hevc = ["hvc1", "hev1", "dvh1", "dvhe"]
        .iter()
        .any(|prefix| video_codec.starts_with(prefix));
    let video_range = if hevc {
        match file.hdr.as_deref() {
            Some("dolby_vision" | "hdr10" | "hdr10_plus") => Some("PQ"),
            Some("hlg") => Some("HLG"),
            _ => None,
        }
    } else {
        None
    };
    if let Some(video_range) = video_range {
        if shape.video_range {
            out.push_str(&format!(",VIDEO-RANGE={video_range}"));
        }
        if shape.codecs {
            out.push_str(&format!(",CODECS=\"{}\"", quoted(&context.codecs)));
        }
        if shape.codecs {
            if let Some(supplemental) = context.supplemental_codecs.as_deref() {
                out.push_str(&format!(
                    ",SUPPLEMENTAL-CODECS=\"{}\"",
                    quoted(supplemental)
                ));
            }
        }
    }
    // Ladder rung: the variant carries no CLOSED-CAPTIONS attribute, and
    // Apple's authoring rules say a variant with no captions must say so.
    // Absent it, AVFoundation is entitled to synthesise a phantom
    // closed-caption option into the `.legible` group — which shifts every
    // option ordinal underneath it. Correct HLS authoring, and untested on
    // the device, which is exactly what a rung is.
    if rungs.closed_captions_none {
        out.push_str(",CLOSED-CAPTIONS=NONE");
    }
    if shape.subtitles && !native.is_empty() {
        out.push_str(",SUBTITLES=\"subs\"");
    }
    out.push('\n');
    // Resolve to the historical media-playlist URL. The master itself lives
    // at `master.m3u8`, so this child path is unambiguous without relying on
    // query-string distinctions in AVPlayer's HLS resource cache.
    out.push_str("index.m3u8\n");
    out
}

#[derive(Debug, Clone, PartialEq)]
struct SubtitleWindow {
    sequence: u64,
    start_seconds: f64,
    end_seconds: f64,
    duration: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct SubtitleTimeline {
    target_duration: u64,
    media_sequence: u64,
    playlist_type: Option<String>,
    endlist: bool,
    segments: Vec<SubtitleWindow>,
}

fn subtitle_timeline(video_playlist: &[u8]) -> SubtitleTimeline {
    let text = String::from_utf8_lossy(video_playlist);
    let mut target_duration = 1;
    let mut media_sequence = 0;
    let mut playlist_type = None;
    let mut pending_duration = None;
    let mut durations = Vec::new();
    let mut endlist = false;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = value.parse().unwrap_or(1).max(1);
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = value.parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            playlist_type = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            pending_duration = value
                .split_once(',')
                .map_or(value, |(duration, _)| duration)
                .parse::<f64>()
                .ok();
        } else if line == "#EXT-X-ENDLIST" {
            endlist = true;
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some(duration) = pending_duration.take().filter(|value| *value > 0.0) {
                durations.push(duration);
            }
        }
    }

    let mut start_seconds = 0.0;
    let segments = durations
        .into_iter()
        .enumerate()
        .map(|(ordinal, duration)| {
            let end_seconds = start_seconds + duration;
            let window = SubtitleWindow {
                sequence: media_sequence + ordinal as u64,
                start_seconds,
                end_seconds,
                duration,
            };
            start_seconds = end_seconds;
            window
        })
        .collect();
    SubtitleTimeline {
        target_duration,
        media_sequence,
        playlist_type,
        endlist,
        segments,
    }
}

fn subtitle_media_playlist(video_playlist: &[u8]) -> String {
    let timeline = subtitle_timeline(video_playlist);
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
        timeline.target_duration, timeline.media_sequence
    );
    if let Some(kind) = &timeline.playlist_type {
        out.push_str(&format!("#EXT-X-PLAYLIST-TYPE:{kind}\n"));
    }
    for window in &timeline.segments {
        out.push_str(&format!(
            "#EXTINF:{:.6},\nseg{:05}.vtt\n",
            window.duration, window.sequence
        ));
    }
    if timeline.endlist {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

fn parse_vtt_timestamp(raw: &str) -> Option<f64> {
    let fields: Vec<&str> = raw.trim().split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => (
            0.0,
            minutes.parse::<f64>().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        [hours, minutes, seconds] => (
            hours.parse::<f64>().ok()?,
            minutes.parse::<f64>().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        _ => return None,
    };
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn format_vtt_timestamp(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = millis / 3_600_000;
    let minutes = millis / 60_000 % 60;
    let seconds = millis / 1000 % 60;
    let millis = millis % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn slice_webvtt(
    bytes: &[u8],
    offset_seconds: f64,
    segment_start: f64,
    segment_end: f64,
) -> Vec<u8> {
    let normalized = String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut shifted = Vec::new();
    for block in normalized.split("\n\n") {
        let mut lines: Vec<String> = block.lines().map(str::to_owned).collect();
        let Some((line_index, start, end, settings)) =
            lines.iter().enumerate().find_map(|(line_index, line)| {
                let (left, right) = line.split_once("-->")?;
                let right = right.trim_start();
                let end_token = right.split_whitespace().next()?;
                let start = parse_vtt_timestamp(left)?;
                let end = parse_vtt_timestamp(end_token)?;
                let settings = right[end_token.len()..].trim_start().to_owned();
                Some((line_index, start, end, settings))
            })
        else {
            shifted.push(block.to_owned());
            continue;
        };
        let start = start - offset_seconds;
        let end = end - offset_seconds;
        if end <= segment_start || start >= segment_end {
            continue;
        }
        let settings = if settings.is_empty() {
            String::new()
        } else {
            format!(" {settings}")
        };
        // A cue that outlives this segment keeps its AUTHORED end time. It
        // used to be clipped to the segment boundary and re-emitted, clipped
        // again, into the next one — so a line of dialogue spanning a 6 s
        // boundary was torn in half and flickered at the seam, and its
        // authored duration was destroyed in both halves. The cue is now
        // emitted whole in every segment window it intersects; a player that
        // sees the same cue twice reconciles it by identity, and worst case
        // draws the same text over the same interval twice, which is
        // invisible. Only the leading edge is still clamped, and only because
        // it has to be: cue times in this scheme are segment-local and WebVTT
        // has no way to spell a negative one. (Carrying session-absolute cue
        // times with X-TIMESTAMP-MAP anchored at 0 would remove that last
        // clamp — it is a change to a wire shape the device has already
        // validated, so it belongs on the §5.4 ladder, not here.)
        lines[line_index] = format!(
            "{} --> {}{}",
            format_vtt_timestamp(start.max(segment_start) - segment_start),
            format_vtt_timestamp(end - segment_start),
            settings
        );
        shifted.push(lines.join("\n"));
    }
    if let Some(header) = shifted
        .first_mut()
        .filter(|header| header.starts_with("WEBVTT"))
    {
        let timestamp = ((segment_start.max(0.0) * 90_000.0).round() as u64) % (1_u64 << 33);
        let mut lines: Vec<String> = header
            .lines()
            .filter(|line| !line.starts_with("X-TIMESTAMP-MAP="))
            .map(str::to_owned)
            .collect();
        lines.push(format!(
            "X-TIMESTAMP-MAP=MPEGTS:{timestamp},LOCAL:00:00:00.000"
        ));
        *header = lines.join("\n");
    }
    let mut out = shifted.join("\n\n");
    out.push_str("\n\n");
    out.into_bytes()
}

/// GET /api/v1/hls/:session/:segment — capability auth (see module docs).
pub async fn segment(
    State(state): State<AppState>,
    AxPath((session, seg)): AxPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Segment {
            segment: seg.clone(),
        },
        RelayHeaders::from_http(&headers),
    )
    .await?
    {
        return Ok(response);
    }
    segment_local(&state, &session, &seg, &RelayHeaders::from_http(&headers)).await
}

fn requested_byte_range(value: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let raw = raw.strip_prefix("bytes=").ok_or(())?;
    if raw.contains(',') || len == 0 {
        return Err(());
    }
    let (start, end) = raw.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (len.saturating_sub(suffix.min(len)), len - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= len {
            return Err(());
        }
        let end = if end.is_empty() {
            len - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(len - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(Some((start, end)))
}

fn segment_etag(session: &str, segment: &str, len: u64) -> String {
    format!("\"{session}-{segment}-{len:x}\"")
}

fn etag_matches(request: Option<&str>, etag: &str) -> bool {
    request.is_some_and(|request| {
        request
            .split(',')
            .map(str::trim)
            .any(|candidate| candidate == "*" || candidate == etag)
    })
}

/// Try to resurrect a reaped VOD session from its durable route (plan §2.5).
/// Only an active, unexpired route this node owns qualifies — a released
/// route (DELETE, supersession) stays dead, which is what keeps every
/// terminal cause terminal.
async fn vod_resurrected(state: &AppState, session: &str) -> bool {
    let Ok(Some(route)) = state.media_sessions.route(session).await else {
        return false;
    };
    if route.owner_node_id != state.node_id
        || route.state != "active"
        || route.lease_expires_at_ms <= unix_ms()
    {
        return false;
    }
    state
        .transcode
        .vod_resurrect(&route.recipe_json, session, route.user_id)
        .await
}

/// Map a VOD serving refusal to its typed response (plan §2.3).
fn vod_error(session: &str, err: crate::vodserve::VodError) -> ApiError {
    use crate::vodserve::VodError;
    let log = |code: &str, message: &str| {
        tracing::warn!(
            session = %crate::transcode::session_log_id(session),
            code,
            "vod request refused: {message}"
        );
    };
    match err {
        // The third outcome: the deadline passed with the segment still
        // unproduced. Retryable by contract, and says so.
        VodError::Pending { retry_after } => {
            let secs = retry_after.as_secs().max(1);
            log("segment_pending", "the segment is not materialized yet");
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_pending",
                format!("the segment is being produced; retry in {secs}s"),
            )
        }
        VodError::Busy => {
            log("segment_wait_busy", "blocked-GET caps reached");
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_wait_busy",
                "too many blocked fetches for this session; retry shortly",
            )
        }
        VodError::ProducerFailed(reason) => {
            log("producer_failed", &reason);
            ApiError::typed(StatusCode::BAD_GATEWAY, "producer_failed", &reason)
        }
        VodError::Gone(cause) => {
            log("media_session_ended", &format!("{cause:?}"));
            ApiError::typed(
                StatusCode::GONE,
                "media_session_ended",
                "this session has ended and will not resume",
            )
        }
        VodError::Io(error) => ApiError::Internal(error.to_string()),
    }
}

/// Serve one VOD segment (or the init) with the immutable-cache headers the
/// plan's §2.1 URIs deserve. Range and conditional requests are honoured; the
/// Apple High-tier init rewrite is applied exactly as on the live path.
async fn vod_segment_response(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    ready: crate::vodserve::SegmentReady,
) -> Result<Response, ApiError> {
    let mut ready = ready;
    if ready.len == 0 {
        return Err(ApiError::NotFound("segment"));
    }
    let content_type = segment_content_type(seg);
    let etag = format!("\"{}\"", ready.etag);
    if etag_matches(headers.if_none_match.as_deref(), &etag) {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=3600, immutable".to_owned(),
                ),
            ],
        )
            .into_response());
    }
    let requested_range = match requested_byte_range(headers.range.as_deref(), ready.len) {
        Ok(range) => range,
        Err(()) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_RANGE, format!("bytes */{}", ready.len)),
                    (header::ACCEPT_RANGES, "bytes".to_owned()),
                    (header::ETAG, etag),
                ],
            )
                .into_response());
        }
    };
    // Small objects — the init above all — are answered from memory so the
    // Apple rewrite can run; segments stream.
    if crate::transcode::is_init_object(seg) && ready.len <= INIT_INSPECTION_LIMIT_BYTES {
        let mut init = Vec::with_capacity(ready.len.min(64 * 1024) as usize);
        ready
            .file
            .read_to_end(&mut init)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        if let Some(file_id) = state.transcode.vod_session_file_id(session).await {
            if let Ok(Some(file)) = state.store.get_file(file_id).await {
                if normalize_high_tier_hevc_init(&file, &mut init) {
                    tracing::info!(
                        session = %crate::transcode::session_log_id(session),
                        "translated the HEVC High-tier initialization record for Apple HLS"
                    );
                }
            }
        }
        let (status, body, content_range) = match requested_range {
            Some((start, end)) => {
                let start = usize::try_from(start).map_err(|_| ApiError::NotFound("segment"))?;
                let end = usize::try_from(end).map_err(|_| ApiError::NotFound("segment"))?;
                if end >= init.len() {
                    return Err(ApiError::NotFound("segment"));
                }
                (
                    StatusCode::PARTIAL_CONTENT,
                    init[start..=end].to_vec(),
                    Some(format!("bytes {start}-{end}/{}", init.len())),
                )
            }
            None => (StatusCode::OK, init, None),
        };
        let mut response = (StatusCode::OK, body).into_response();
        *response.status_mut() = status;
        let headers_mut = response.headers_mut();
        headers_mut.insert(header::CONTENT_TYPE, content_type.parse().expect("mime"));
        headers_mut.insert(header::ETAG, etag.parse().expect("etag"));
        headers_mut.insert(header::ACCEPT_RANGES, "bytes".parse().expect("ranges"));
        headers_mut.insert(
            header::CACHE_CONTROL,
            "private, max-age=3600, immutable".parse().expect("cache"),
        );
        if let Some(range) = content_range {
            headers_mut.insert(header::CONTENT_RANGE, range.parse().expect("range"));
        }
        return Ok(response);
    }
    let (status, len, content_range) = match requested_range {
        Some((start, end)) => {
            use tokio::io::AsyncSeekExt;
            ready
                .file
                .seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|error| ApiError::Internal(error.to_string()))?;
            (
                StatusCode::PARTIAL_CONTENT,
                end - start + 1,
                Some(format!("bytes {start}-{end}/{}", ready.len)),
            )
        }
        None => (StatusCode::OK, ready.len, None),
    };
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(
        tokio::io::AsyncReadExt::take(ready.file, len),
    ));
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers_mut = response.headers_mut();
    headers_mut.insert(header::CONTENT_TYPE, content_type.parse().expect("mime"));
    headers_mut.insert(header::CONTENT_LENGTH, len.into());
    headers_mut.insert(header::ETAG, etag.parse().expect("etag"));
    headers_mut.insert(header::ACCEPT_RANGES, "bytes".parse().expect("ranges"));
    headers_mut.insert(
        header::CACHE_CONTROL,
        "private, max-age=3600, immutable".parse().expect("cache"),
    );
    if let Some(range) = content_range {
        headers_mut.insert(header::CONTENT_RANGE, range.parse().expect("range"));
    }
    Ok(response)
}

async fn segment_local(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
) -> Result<Response, ApiError> {
    const APPLE_INIT_REWRITE_LIMIT_BYTES: u64 = INIT_INSPECTION_LIMIT_BYTES;

    // The VOD presentation's three-outcome contract dispatches first; `None`
    // falls through to the live path untouched. A session neither registry
    // knows may be a reaped VOD handle whose durable route is still live —
    // resurrect it and ask once more before giving up.
    let mut vod_answer = state.transcode.vod_segment(session, seg).await;
    if vod_answer.is_none()
        && state.transcode.session_status(session).await.is_none()
        && vod_resurrected(state, session).await
    {
        vod_answer = state.transcode.vod_segment(session, seg).await;
    }
    if let Some(answer) = vod_answer {
        return match answer {
            Ok(Some(ready)) => vod_segment_response(state, session, seg, headers, ready).await,
            Ok(None) => Err(ApiError::NotFound("segment")),
            Err(err) => Err(vod_error(session, err)),
        };
    }

    let mut opened = match state.transcode.segment(session, seg).await {
        Ok(Some(opened)) => opened,
        Ok(None) => return Err(ApiError::NotFound("segment")),
        Err(crate::transcode::SegmentOpenError::Capacity) => {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "response_snapshot_capacity",
                "authenticated media response capacity is full; retry shortly",
            ));
        }
    };
    // A live media object is published only after bytes exist. Treat an empty
    // file as an incomplete/corrupt publication instead of advertising the
    // saturating `0..=0` calculation below as one byte and hanging the client.
    if opened.len == 0 {
        return Err(ApiError::NotFound("segment"));
    }
    let content_type = segment_content_type(seg);
    let etag = segment_etag(session, seg, opened.len);
    if etag_matches(headers.if_none_match.as_deref(), &etag) {
        opened.delivery.finish_without_body();
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=3600, immutable".to_owned(),
                ),
            ],
        )
            .into_response());
    }
    let requested_range = match requested_byte_range(headers.range.as_deref(), opened.len) {
        Ok(range) => range,
        Err(()) => {
            opened.delivery.finish_without_body();
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_RANGE, format!("bytes */{}", opened.len)),
                    (header::ACCEPT_RANGES, "bytes".to_owned()),
                    (header::ETAG, etag),
                ],
            )
                .into_response());
        }
    };
    if crate::transcode::is_init_object(seg) && opened.len <= APPLE_INIT_REWRITE_LIMIT_BYTES {
        let mut init = Vec::with_capacity(opened.len.min(64 * 1024) as usize);
        let mut delivery = opened.delivery;
        let started = Instant::now();
        let read_elapsed = match opened
            .file
            .take(APPLE_INIT_REWRITE_LIMIT_BYTES)
            .read_to_end(&mut init)
            .await
        {
            Ok(_) => started.elapsed(),
            Err(error) => {
                delivery.fail(&error);
                return Err(ApiError::Internal(error.to_string()));
            }
        };
        if let Ok((_, file)) = session_file(state, session).await {
            if normalize_high_tier_hevc_init(&file, &mut init) {
                tracing::info!(
                    session = %crate::transcode::session_log_id(session),
                    "translated the HEVC High-tier initialization record for Apple HLS"
                );
            }
        }
        let (status, body, content_range) = match requested_range {
            Some((start, end)) => {
                let start = usize::try_from(start).map_err(|_| ApiError::NotFound("segment"))?;
                let end = usize::try_from(end).map_err(|_| ApiError::NotFound("segment"))?;
                (
                    StatusCode::PARTIAL_CONTENT,
                    init[start..=end].to_vec(),
                    Some(format!("bytes {start}-{end}/{}", init.len())),
                )
            }
            None => (StatusCode::OK, init, None),
        };
        // The storage inspection reads the complete init so it can normalize
        // codec metadata, but client-delivery accounting follows only the
        // bytes placed in this response (especially for a Range request).
        delivery.expect_at_most(body.len() as u64);
        delivery.note_read(body.len() as u64, read_elapsed);
        delivery.finish();
        let mut response = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, body.len())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "private, max-age=3600, immutable");
        if let Some(content_range) = content_range {
            response = response.header(header::CONTENT_RANGE, content_range);
        }
        return response
            .body(Body::from(body))
            .map_err(|error| ApiError::Internal(error.to_string()));
    }
    if crate::transcode::is_init_object(seg) && opened.len > APPLE_INIT_REWRITE_LIMIT_BYTES {
        tracing::warn!(
            session = %crate::transcode::session_log_id(session),
            init_bytes = opened.len,
            limit_bytes = APPLE_INIT_REWRITE_LIMIT_BYTES,
            "skipped Apple HEVC tier normalization because init.mp4 exceeds the inspection bound"
        );
    }
    let (status, start, end) = requested_range
        .map(|(start, end)| (StatusCode::PARTIAL_CONTENT, start, end))
        .unwrap_or((StatusCode::OK, 0, opened.len.saturating_sub(1)));
    if start > 0 {
        if let Err(error) = opened.file.seek(std::io::SeekFrom::Start(start)).await {
            opened.delivery.fail(&error);
            return Err(ApiError::Internal(error.to_string()));
        }
    }
    let opened_len = end.saturating_sub(start).saturating_add(1);
    let total_len = opened.len;
    let reader = tokio_util::io::ReaderStream::new(opened.file.take(opened_len));
    let mut delivery = opened.delivery;
    delivery.expect_at_most(opened_len);
    // The tracker rides the stream state rather than the handler, so it is
    // dropped whether the body completes, errors, or is abandoned mid-flight —
    // an abandoned body is the `response_dropped` case, and it is the only one
    // nothing else observes.
    let stream = futures_util::stream::unfold(
        (Some(reader), delivery),
        |(reader, mut delivery)| async move {
            // `None` means a previous poll already reported a storage error.
            // Re-polling a reader that just failed has no defined meaning, so
            // the error is the last thing this body yields.
            let mut reader = reader?;
            let started = Instant::now();
            match reader.next().await {
                Some(Ok(bytes)) => {
                    delivery.note_read(bytes.len() as u64, started.elapsed());
                    Some((Ok(bytes), (Some(reader), delivery)))
                }
                Some(Err(error)) => {
                    delivery.fail(&error);
                    Some((Err(error), (None, delivery)))
                }
                None => {
                    delivery.finish();
                    None
                }
            }
        },
    );
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, opened_len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        // A finished segment never changes: ffmpeg writes `.tmp` and
        // renames. The URI carries a capability-scoped session id.
        .header(header::CACHE_CONTROL, "private, max-age=3600, immutable");
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total_len}"),
        );
    }
    response
        // Streamed rather than buffered: a 4K copy segment is ~35 MB, and
        // reading it into memory before the first byte goes out is an
        // allocation and a copy per request for data on its way to a socket.
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::Internal(error.to_string()))
}

/// MIME types from Apple's HLS authoring profile. An initialization section
/// is an MP4 file, while each `.m4s` resource is an ISO BMFF media segment.
/// Labeling both as `video/mp4` makes the bytes decodable in isolation but can
/// cause AVPlayer's multivariant validator to reject the rendition before it
/// ever opens the decoder.
fn segment_content_type(name: &str) -> &'static str {
    if name.ends_with(".ts") {
        "video/mp2t"
    } else if name.ends_with(".m4s") {
        "video/iso.segment"
    } else {
        "video/mp4"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcode::HlsDeliveryFixture;

    #[test]
    fn public_playback_ids_and_segment_ranges_are_bounded() {
        assert!(valid_playback_id("player-a"));
        assert!(!valid_playback_id("   "));
        assert!(!valid_playback_id("player\r\nforged"));
        assert!(!valid_playback_id(&"p".repeat(129)));

        assert_eq!(requested_byte_range(None, 100), Ok(None));
        assert_eq!(
            requested_byte_range(Some("bytes=10-19"), 100),
            Ok(Some((10, 19)))
        );
        assert_eq!(
            requested_byte_range(Some("bytes=90-"), 100),
            Ok(Some((90, 99)))
        );
        assert_eq!(
            requested_byte_range(Some("bytes=-10"), 100),
            Ok(Some((90, 99)))
        );
        assert_eq!(
            requested_byte_range(Some("bytes=-200"), 100),
            Ok(Some((0, 99)))
        );
        assert_eq!(requested_byte_range(Some("bytes=10-9"), 100), Err(()));
        assert_eq!(requested_byte_range(Some("bytes=100-"), 100), Err(()));
        assert_eq!(requested_byte_range(Some("bytes=0-1,3-4"), 100), Err(()));
    }

    #[test]
    fn delayed_resolved_replay_rechecks_the_current_lease_boundary() {
        let mut route = MediaSessionRoute {
            incarnation_id: "00000000-0000-4000-8000-0000000000d1".to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000d2".to_owned(),
            user_id: 7,
            playback_id: "replay-player".to_owned(),
            request_fingerprint: "d".repeat(64),
            owner_node_id: "node-d".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: 2_001,
            state: "active".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: 1,
        };
        assert!(resolved_replay_is_live(&route, 1_000));
        assert!(!resolved_replay_is_live(&route, 1_001));
        route.lease_expires_at_ms = 1_000;
        assert!(
            !resolved_replay_is_live(&route, 1_000),
            "a read that returns after exact expiry must never answer a replay"
        );
    }

    // ---- segment delivery, through the real response ------------------------
    //
    // These drive the handler, not `SegmentDelivery`. The correction under
    // test is *where* bytes are counted — at open, or as the response body
    // drains — and a test that calls `note_read` by hand cannot tell those
    // apart.

    /// The correction itself: a segment's bytes are counted as the response
    /// body drains, not when the file opens.
    ///
    /// Counting at open credits a client that fetched a header and then
    /// stalled with a whole segment's worth of throughput, which is precisely
    /// the reading that makes a delivery freeze look like a healthy transfer.
    /// Both halves are pinned: the meter is still at zero when the handler
    /// returns, and reaches the segment's length only once the body is read.
    #[tokio::test]
    async fn segment_bytes_are_counted_as_the_body_drains_not_at_open() {
        use futures_util::StreamExt;

        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "drain").await;
        let body = vec![7_u8; 12 * 1024];
        tokio::fs::write(dir.path().join("seg00001.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("drain".to_owned(), "seg00001.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(body.len().to_string().as_str())
        );
        assert_eq!(
            fixture.delivered_bytes(),
            0,
            "opening a segment delivers nothing: the client has only been handed a body"
        );

        let mut stream = response.into_body().into_data_stream();
        let mut read = 0_usize;
        while let Some(chunk) = stream.next().await {
            read += chunk.expect("segment chunk").len();
        }
        assert_eq!(read, body.len(), "the whole segment reached the client");
        assert_eq!(
            fixture.delivered_bytes(),
            body.len() as i64,
            "the meter tracks bytes actually read out of the segment"
        );
        assert!(
            fixture.settle().await.is_empty(),
            "a complete delivery is not an incident"
        );
    }

    #[tokio::test]
    async fn range_and_bodyless_segment_responses_keep_delivery_truth() {
        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "range").await;
        let body = vec![5_u8; 16 * 1024];
        tokio::fs::write(dir.path().join("seg00004.m4s"), &body)
            .await
            .expect("segment bytes");

        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=1024-2047".parse().expect("range"));
        let response = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            headers,
        )
        .await
        .expect("partial response");
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 2_048)
                .await
                .expect("partial body")
                .len(),
            1_024
        );
        assert_eq!(fixture.delivered_bytes(), 1_024);

        let mut conditional = HeaderMap::new();
        conditional.insert(
            header::IF_NONE_MATCH,
            segment_etag("range", "seg00004.m4s", body.len() as u64)
                .parse()
                .expect("etag"),
        );
        let not_modified = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            conditional,
        )
        .await
        .expect("conditional response");
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

        let mut unsatisfiable = HeaderMap::new();
        unsatisfiable.insert(
            header::RANGE,
            "bytes=999999-".parse().expect("invalid range value"),
        );
        let rejected = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            unsatisfiable,
        )
        .await
        .expect("range response");
        assert_eq!(rejected.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(fixture.delivered_bytes(), 1_024);
        assert!(
            fixture.settle().await.is_empty(),
            "valid partial and intentionally bodyless responses are not incomplete deliveries"
        );

        let init_dir = tempfile::tempdir().expect("init directory");
        let init_fixture = HlsDeliveryFixture::publish(init_dir.path(), "init-range").await;
        tokio::fs::write(init_dir.path().join("init.mp4"), vec![9_u8; 4_096])
            .await
            .expect("init bytes");
        let mut init_headers = HeaderMap::new();
        init_headers.insert(header::RANGE, "bytes=0-3".parse().expect("init range"));
        let init_response = segment(
            State(init_fixture.state.clone()),
            AxPath(("init-range".to_owned(), "init.mp4".to_owned())),
            init_headers,
        )
        .await
        .expect("init partial response");
        assert_eq!(
            axum::body::to_bytes(init_response.into_body(), 16)
                .await
                .expect("init body")
                .len(),
            4
        );
        assert_eq!(init_fixture.delivered_bytes(), 4);
        assert!(init_fixture.settle().await.is_empty());
    }

    /// A client that walks away mid-segment is the case nothing else observes:
    /// the handler has already returned, the stream never reaches EOF, and no
    /// error is raised. Only `Drop` can name it, which also makes it the
    /// easiest classification to lose to a later refactor.
    #[tokio::test]
    async fn an_abandoned_segment_body_is_recorded_as_response_dropped() {
        use futures_util::StreamExt;

        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "abandoned").await;
        let body = vec![3_u8; 64 * 1024];
        tokio::fs::write(dir.path().join("seg00002.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("abandoned".to_owned(), "seg00002.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");
        let mut stream = response.into_body().into_data_stream();
        let first = stream
            .next()
            .await
            .expect("a first chunk")
            .expect("a readable first chunk");
        assert!(
            first.len() < body.len(),
            "the fixture must leave the body partially consumed"
        );
        drop(stream);

        let events = fixture.delivery_events(1).await;
        let dropped = events
            .iter()
            .find(|event| event.reason.as_deref() == Some("response_dropped"))
            .expect("an abandoned body is attributed to the client, not to storage");
        assert_eq!(dropped.event, "segment_delivery_incomplete");
        let extra = dropped.extra.as_deref().unwrap_or_default();
        assert!(
            extra.contains("\"segment\":\"seg00002.m4s\"")
                && extra.contains(&format!("\"expected_bytes\":{}", body.len()))
                && extra.contains(&format!("\"delivered_bytes\":{}", first.len())),
            "the event names the segment, what was owed, and what arrived: {extra}"
        );
        assert_eq!(
            fixture.delivered_bytes(),
            first.len() as i64,
            "only the bytes the client actually took are delivery"
        );
    }

    /// A storage error mid-body is its own classification, separate from an
    /// abandoned response and from a short one. Nothing reached `fail()`
    /// before this test, so `storage_read_error` could have stopped being
    /// emitted with no check noticing.
    #[tokio::test]
    async fn an_unreadable_segment_body_is_recorded_as_storage_read_error() {
        use futures_util::StreamExt;

        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unreadable").await;
        // A directory opens like a file and reports a length, then fails its
        // first read with EISDIR — a storage failure the handler meets only
        // after the response headers are already on the wire.
        tokio::fs::create_dir(dir.path().join("seg00003.m4s"))
            .await
            .expect("unreadable segment");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("unreadable".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");
        let mut stream = response.into_body().into_data_stream();
        assert!(
            stream.next().await.is_some_and(|chunk| chunk.is_err()),
            "the body surfaces the storage error to the client"
        );
        // The `unfold` stops rather than re-polling a failed reader. That is
        // belt-and-braces — `ReaderStream` already drops its reader on error —
        // so this pins the reachable contract (the body ends at the error)
        // rather than the guard itself.
        assert!(
            stream.next().await.is_none(),
            "the body ends at the storage error"
        );

        let events = fixture.delivery_events(1).await;
        let failed = events
            .iter()
            .find(|event| event.reason.as_deref() == Some("storage_read_error"))
            .expect("a failed storage read is reported as one");
        assert_eq!(failed.event, "segment_delivery_incomplete");
        assert!(
            failed
                .extra
                .as_deref()
                .is_some_and(|extra| extra.contains("\"error\"")),
            "the operator gets the underlying error text"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "segment_delivery_incomplete")
                .count(),
            1,
            "one response produces one terminal classification"
        );
    }

    /// `exact_hls_context` opens `init.mp4` for the playlist generator, not
    /// for a client: there is no response body on that path. Its bytes must
    /// not move the session's delivery meter, and — because the read is
    /// bounded well below a large init's real length — a bounded read that
    /// returned everything it asked for must not be reported as a response
    /// that ended early.
    #[tokio::test]
    async fn a_playlist_time_init_probe_is_not_client_delivery_and_never_a_short_response() {
        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "probe").await;
        // Past the inspection bound, so the read stops short of the file's
        // advertised length by design.
        let oversized = vec![0_u8; (INIT_INSPECTION_LIMIT_BYTES + 4_096) as usize];
        tokio::fs::write(dir.path().join("init.mp4"), &oversized)
            .await
            .expect("oversized init");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "hvc1.2.4.L150.B0,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "probe", context.clone()).await;
        assert_eq!(
            resolved.codecs, context.codecs,
            "an init with no hvcC leaves the advertised codecs alone"
        );
        assert_eq!(
            fixture.delivered_bytes(),
            0,
            "a playlist-time codec sniff delivers nothing to anyone"
        );
        assert!(
            fixture.settle().await.is_empty(),
            "a bounded read that returned every byte it asked for is not a truncated response"
        );
    }

    /// When a server-internal read *does* fail, the event still has to be
    /// readable as internal. An untagged `segment_delivery_incomplete` for
    /// `init.mp4` is indistinguishable from a client fetch that broke, which
    /// is the availability/delivery conflation this telemetry exists to end.
    #[tokio::test]
    async fn a_failed_init_probe_is_tagged_as_internal_rather_than_a_client_fetch() {
        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "probe-error").await;
        tokio::fs::create_dir(dir.path().join("init.mp4"))
            .await
            .expect("unreadable init");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "hvc1.2.4.L150.B0,mp4a.40.2".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        exact_hls_context(&fixture.state, "probe-error", context).await;

        let events = fixture.delivery_events(1).await;
        let failed = events
            .iter()
            .find(|event| event.reason.as_deref() == Some("storage_read_error"))
            .expect("a failed internal read is still reported");
        assert!(
            failed
                .extra
                .as_deref()
                .is_some_and(|extra| extra.contains("\"purpose\":\"internal_probe\"")),
            "an operator reading a freeze can tell a codec sniff from a segment fetch: {:?}",
            failed.extra
        );
        assert_eq!(
            fixture.delivered_bytes(),
            0,
            "a failed internal read is not a partial delivery"
        );
    }

    #[test]
    fn bounded_admission_failure_is_a_retryable_503() {
        let capacity =
            "transcode capacity is temporarily unavailable: background encoding did not yield";
        let response = session_start_error(42, capacity.into()).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(matches!(
            session_start_error(42, "ffmpeg failed".into()),
            ApiError::Internal(_)
        ));
    }

    #[test]
    fn omitted_public_presentation_means_vod_never_live() {
        let create: CreateSession = serde_json::from_value(serde_json::json!({
            "playback_id": "older-native-client",
            "copy": true
        }))
        .expect("create body");
        assert_eq!(
            create.into_request(42, 1080).presentation,
            crate::transcode::Presentation::Vod
        );
    }

    #[test]
    fn every_vod_ineligibility_reaches_the_wire_as_a_typed_refusal() {
        let cases = [
            ("vod_disabled", StatusCode::SERVICE_UNAVAILABLE),
            ("vod_index_pending", StatusCode::SERVICE_UNAVAILABLE),
            ("vod_transcode_unavailable", StatusCode::NOT_IMPLEMENTED),
            ("vod_subtitle_burn_unavailable", StatusCode::NOT_IMPLEMENTED),
            ("vod_source_unsupported", StatusCode::UNPROCESSABLE_ENTITY),
            ("vod_reopen_required", StatusCode::CONFLICT),
            ("live_presentation_removed", StatusCode::GONE),
        ];
        for (code, status) in cases {
            let error = crate::transcode::vod_refusal_error(code, "named reason");
            match session_start_error(42, error) {
                ApiError::Typed {
                    status: actual_status,
                    code: actual_code,
                    message,
                } => {
                    assert_eq!(actual_status, status, "{code}");
                    assert_eq!(actual_code, code, "{code}");
                    assert_eq!(message, "named reason", "{code}");
                }
                other => panic!("{code} was not typed: {other:?}"),
            }
        }
    }

    /// §7.3 requirement 3 is about VIEWER intent, not wire presence. Android's
    /// `sessionHeight` answers a subtitle burn with the source height before it
    /// consults quality, so an Auto viewer with a burned subtitle posts a
    /// height; inferring stickiness from that field alone makes the heaviest
    /// session type there is permanently unsteppable on Android while the
    /// identical Apple session steps. `quality_auto` is the intent itself.
    #[test]
    fn stall_stickiness_follows_stated_quality_intent_not_a_posted_height() {
        let burn_under_auto = serde_json::json!({
            "playback_id": "android-player",
            // The Original/forced-burn source-height promise, not a rung pick.
            "height": 2160,
            "subtitle_burn": 3,
            "quality_auto": true,
        });
        let create: CreateSession = serde_json::from_value(burn_under_auto).expect("create body");
        let request = create.into_request(17, 2160);
        assert!(
            request.automatic,
            "a burn's source-height promise is not a manual quality pick"
        );
        assert_eq!(
            request.kind,
            crate::transcode::SessionKind::Transcode { height: 2160 }
        );

        // The other direction is just as explicit: a viewer who picked a rung
        // stays on it even though the body would otherwise read as Auto.
        let manual_without_height = serde_json::json!({
            "playback_id": "android-player",
            "quality_auto": false,
        });
        let create: CreateSession =
            serde_json::from_value(manual_without_height).expect("create body");
        assert!(!create.into_request(17, 720).automatic);

        // Every shipped client omits the field, so the old inference has to
        // survive untouched in both of its arms.
        let silent_auto = serde_json::json!({ "playback_id": "apple-player" });
        let create: CreateSession = serde_json::from_value(silent_auto).expect("create body");
        assert!(create.into_request(17, 720).automatic);

        let silent_manual = serde_json::json!({ "playback_id": "apple-player", "height": 480 });
        let create: CreateSession = serde_json::from_value(silent_manual).expect("create body");
        assert!(!create.into_request(17, 480).automatic);
    }

    #[test]
    fn a_typed_stall_reopen_reaches_the_claim_unchanged() {
        let body = serde_json::json!({
            "playback_id": "native-player",
            "request_id": "stall-attempt",
            "previous_session_id": "previous-session",
            "reopen_reason": "stall",
            "start": 42.5,
            "audio": 2,
            "subtitle_burn": 5
        });
        let create: CreateSession = serde_json::from_value(body).expect("typed create body");
        let request = create.into_request(17, 1080);

        assert!(request.automatic);
        assert_eq!(
            request.previous_session_id.as_deref(),
            Some("previous-session")
        );
        assert_eq!(
            request.reopen_reason,
            Some(crate::transcode::ReopenReason::Stall)
        );
        assert_eq!(request.start_seconds, 42.5);
        assert_eq!(request.audio_index, Some(2));
        assert_eq!(request.subtitle_burn, Some(5));

        let unknown = serde_json::json!({
            "playback_id": "native-player",
            "request_id": "future-attempt",
            "previous_session_id": "previous-session",
            "reopen_reason": "network_changed"
        });
        assert!(serde_json::from_value::<CreateSession>(unknown).is_err());
    }

    #[test]
    fn an_invalid_bound_reopen_is_a_400_not_an_idempotency_conflict() {
        let response = session_start_error(
            42,
            "invalid stall reopen: the previous session does not belong to this user, playback, and file"
                .into(),
        )
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A build that cannot burn is a fact the viewer can act on, not an
    /// "internal server error" whose message is deliberately dropped.
    #[test]
    fn a_build_that_cannot_burn_says_so_instead_of_hiding_behind_a_500() {
        let error = "this server's media tools cannot do that: this server's ffmpeg build has no \
             subtitles filter, which burning text subtitles into the picture requires";
        match session_start_error(42, error.into()) {
            ApiError::Typed {
                status,
                code,
                message,
            } => {
                assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
                assert_eq!(code, "unsupported_build");
                assert!(
                    message.contains("subtitles filter") && !message.contains("cannot do that:"),
                    "the viewer reads the reason, not the classification: {message}"
                );
            }
            other => panic!("expected a typed refusal, got {other:?}"),
        }
    }

    /// Every playlist refusal, from the session's verdict to the wire.
    ///
    /// All four used to be `ApiError::NotFound("transcode session")` — one
    /// anonymous 404 that hls.js escalates to a fatal `levelLoadError`
    /// whatever caused it. The status now separates what the client can do
    /// about it, and the typed body carries the sentence a person reads.
    #[tokio::test]
    async fn each_playlist_refusal_reaches_the_client_as_itself() {
        use axum::body::to_bytes;

        let rows = [
            (
                PlaylistError::SessionGone,
                StatusCode::NOT_FOUND,
                "session_gone",
                "no longer running",
            ),
            (
                PlaylistError::ProducerExited("exit status: 1".into()),
                StatusCode::BAD_GATEWAY,
                "producer_failed",
                "exit status: 1",
            ),
            (
                PlaylistError::SessionFailed("the encoder never produced any video".into()),
                StatusCode::BAD_GATEWAY,
                "session_failed",
                "never produced any video",
            ),
            (
                // The #263 case: still inside the server's own recovery.
                // 503, not 404, and hls.js's level-load retry is the right
                // response to it.
                PlaylistError::StartupTimedOut(std::time::Duration::from_secs(45)),
                StatusCode::SERVICE_UNAVAILABLE,
                "startup_timeout",
                "still preparing",
            ),
        ];
        for (err, status, code, fragment) in rows {
            let response = playlist_error("sess-1", err.clone()).into_response();
            assert_eq!(response.status(), status, "{code}");
            let body = to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("body");
            let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
            assert_eq!(json["code"], code);
            let message = json["message"].as_str().expect("message");
            assert!(
                message.to_lowercase().contains(fragment),
                "{code}: \"{message}\" should contain \"{fragment}\""
            );
            // The sentence the old overlay showed sent people to a log file on
            // a box they were not sitting at. Nothing here may do that.
            assert!(!message.contains("Settings"), "{code}: {message}");
        }
    }

    #[test]
    fn fmp4_media_segments_use_the_iso_segment_mime_type() {
        assert_eq!(segment_content_type("init.mp4"), "video/mp4");
        assert_eq!(segment_content_type("seg00000.m4s"), "video/iso.segment");
        assert_eq!(segment_content_type("seg00000.ts"), "video/mp2t");
    }

    fn hls_file(subtitle_streams: Vec<SubtitleStream>) -> MediaFile {
        MediaFile {
            id: 5615,
            item_id: 1,
            path: "/media/Scary Movie.mkv".into(),
            size: 19_000_000_000,
            mtime: 1,
            duration_ms: Some(120_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_profile: None,
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: Some("dolby_vision".into()),
            hdr_format: Some("Dolby Vision".into()),
            bitrate: Some(40_000_000),
            audio_streams: vec![],
            subtitle_streams,
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
        }
    }

    fn hls_context(
        codecs: &str,
        supplemental_codecs: Option<&str>,
    ) -> crate::transcode::HlsContext {
        crate::transcode::HlsContext {
            file_id: 5615,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: codecs.into(),
            supplemental_codecs: supplemental_codecs.map(str::to_owned),
            frame_rate: Some(24_000.0 / 1_001.0),
        }
    }

    fn sdr_context() -> crate::transcode::HlsContext {
        hls_context("avc1.640034,mp4a.40.2", None)
    }

    #[test]
    fn source_probe_preserves_fractional_video_frame_rate() {
        let probe = r#"{
            "streams": [
                {"codec_type":"audio", "avg_frame_rate":"0/0"},
                {"codec_type":"video", "avg_frame_rate":"24000/1001", "r_frame_rate":"24/1"}
            ]
        }"#;
        let rate = video_frame_rate(probe).expect("frame rate");
        assert!((rate - 23.976).abs() < 0.001, "{rate}");
        assert!(video_frame_rate("not json").is_none());
    }

    fn sub(
        codec: &str,
        language: &str,
        title: &str,
        default: bool,
        forced: bool,
    ) -> SubtitleStream {
        SubtitleStream {
            index: 0,
            codec: codec.into(),
            language: Some(language.into()),
            title: Some(title.into()),
            default,
            forced,
            hearing_impaired: false,
        }
    }

    #[test]
    fn playback_audio_offset_is_bounded_and_carried_by_the_session() {
        let request = CreateSession {
            playback_id: "player".into(),
            request_id: Some("attempt".into()),
            previous_session_id: None,
            reopen_reason: None,
            height: None,
            quality_auto: None,
            subtitle_burn: None,
            subtitle_burn_sdr: None,
            native_subtitles: None,
            subtitle: None,
            start: Some(12.0),
            audio: None,
            copy: Some(false),
            aac: None,
            preserve_dolby_vision: None,
            hdr10: None,
            audio_offset_ms: Some(20_000),
            presentation: None,
            block_budget_secs: None,
        }
        .into_request(7, 1080);

        assert_eq!(request.audio_offset_ms, 15_000);
        assert_eq!(request.file_id, 7);
    }

    #[test]
    fn bitmap_fallback_still_carries_an_explicit_burn_request() {
        let request = CreateSession {
            playback_id: "apple-bitmap".into(),
            request_id: None,
            previous_session_id: None,
            reopen_reason: None,
            height: Some(2160),
            quality_auto: None,
            subtitle_burn: Some(5),
            subtitle_burn_sdr: None,
            native_subtitles: Some(true),
            subtitle: None,
            start: None,
            audio: None,
            copy: None,
            aac: None,
            preserve_dolby_vision: None,
            hdr10: None,
            audio_offset_ms: None,
            presentation: None,
            block_budget_secs: None,
        }
        .into_request(5615, 2160);
        assert_eq!(request.subtitle_burn, Some(5));
        assert!(matches!(
            request.kind,
            crate::transcode::SessionKind::Transcode { height: 2160 }
        ));
    }

    #[test]
    fn server_refuses_hdr_burns_unless_the_plan_is_already_sdr() {
        let mut file = hls_file(vec![]);
        for hdr in ["dolby_vision", "hdr10", "hlg"] {
            file.hdr = Some(hdr.into());
            assert!(hdr_subtitle_burn_is_refused(Some(&file), Some(5), None));
            assert!(hdr_subtitle_burn_is_refused(
                Some(&file),
                Some(5),
                Some(false)
            ));
            assert!(
                !hdr_subtitle_burn_is_refused(Some(&file), Some(5), Some(true)),
                "an existing HDR-to-SDR plan may keep its forced subtitle"
            );
        }

        file.hdr = None;
        assert!(!hdr_subtitle_burn_is_refused(Some(&file), Some(5), None));
        assert!(!hdr_subtitle_burn_is_refused(Some(&file), Some(-1), None));
        assert!(!hdr_subtitle_burn_is_refused(Some(&file), None, None));
        assert!(!hdr_subtitle_burn_is_refused(None, Some(5), None));
    }

    /// The session's answer is the one that wins once playback attaches, so
    /// it has to be read off the session that was built — not off the
    /// decision that suggested one. A DV remux the client can take delivers
    /// Dolby Vision; the same file behind a rung or a burn delivers SDR,
    /// because the encoder tone-maps it.
    #[test]
    fn a_session_reports_the_dynamic_range_of_the_stream_it_just_built() {
        let file = hls_file(vec![]);
        let copy = |preserve: bool| crate::transcode::SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: preserve,
        };
        use plurx_core::transcode::OutputGrade;
        assert_eq!(
            session_delivered_dynamic_range(Some(&file), &copy(true), OutputGrade::Sdr),
            Some("dolby_vision")
        );
        // Stripped: what reaches the client is the compatible base layer.
        let mut base = file.clone();
        base.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        assert_eq!(
            session_delivered_dynamic_range(Some(&base), &copy(false), OutputGrade::Sdr),
            Some("hdr10")
        );
        assert_eq!(
            session_delivered_dynamic_range(
                Some(&file),
                &crate::transcode::SessionKind::Transcode { height: 1080 },
                OutputGrade::Sdr,
            ),
            Some("sdr"),
            "an SDR-grade transcode is H.264 8-bit, whatever the source carried"
        );
        // …and the HDR10 rung of the same source says so, on the same helper.
        // The session's grade is what decides it, so a rung the server refused
        // cannot report HDR10 by having been asked for.
        assert_eq!(
            session_delivered_dynamic_range(
                Some(&file),
                &crate::transcode::SessionKind::Transcode { height: 1080 },
                OutputGrade::Hdr10,
            ),
            Some("hdr10"),
            "the Dolby Vision → HDR10 rung is not SDR"
        );
        // A file that vanished from the store mid-request says nothing at
        // all rather than guessing; the client keeps what it had.
        assert_eq!(
            session_delivered_dynamic_range(None, &copy(true), OutputGrade::Sdr),
            None
        );
    }

    #[test]
    fn native_hls_master_advertises_selection_language_names_and_forced_metadata() {
        let file = hls_file(vec![
            sub("subrip", "ita", "Forced", true, true),
            sub("subrip", "ita", "Regular", false, false),
            sub("subrip", "eng", "Forced", false, false),
            sub("subrip", "eng", "Regular", false, false),
            sub("webvtt", "eng", "SDH", false, false),
        ]);
        let master = master_playlist(&file, Some(2), &sdr_context());

        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
        assert!(master.contains(
            "#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,\
             RESOLUTION=3840x2160,FRAME-RATE=23.976,SUBTITLES=\"subs\""
        ));
        assert!(!master.contains("CODECS="));
        assert!(!master.contains("#EXT-X-INDEPENDENT-SEGMENTS"));
        assert!(master.contains("NAME=\"English · Forced\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,FORCED=YES,URI=\"subs/2/index.m3u8\""));
        assert!(master.contains(
            "NAME=\"Italian · Forced\",LANGUAGE=\"it\",DEFAULT=NO,AUTOSELECT=YES,FORCED=YES"
        ));
        assert!(master.contains("NAME=\"English · Regular\",LANGUAGE=\"en\",DEFAULT=NO"));
        assert!(master.contains("NAME=\"English · SDH\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,FORCED=NO,CHARACTERISTICS=\"public.accessibility.transcribes-spoken-dialog,public.accessibility.describes-music-and-sound\""));
        assert!(master.ends_with("index.m3u8\n"));
    }

    #[test]
    fn hdr_master_declares_the_range_and_exact_session_codecs() {
        let file = hls_file(vec![]);
        let stripped = hls_context("hvc1.2.4.L150.B0,mp4a.40.2", None);
        let master = master_playlist(&file, None, &stripped);
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
        assert!(master.contains(
            "#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,\
             RESOLUTION=3840x2160,FRAME-RATE=23.976,VIDEO-RANGE=PQ,\
             CODECS=\"hvc1.2.4.L150.B0,mp4a.40.2\""
        ));

        let compatible_dv = hls_context("hvc1.2.4.L150.B0,ec-3", Some("dvh1.08.10/db1p"));
        let master = master_playlist(&file, None, &compatible_dv);
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:10\n"));
        assert!(master.contains("VIDEO-RANGE=PQ"));
        assert!(master.contains("CODECS=\"hvc1.2.4.L150.B0,ec-3\""));
        assert!(master.contains("SUPPLEMENTAL-CODECS=\"dvh1.08.10/db1p\""));

        // A tone-mapped session keeps its source's HDR library metadata, but
        // its H.264 bytes and master are SDR.
        let transcoded = hls_context("avc1.640034,mp4a.40.2", None);
        let master = master_playlist(&file, None, &transcoded);
        assert!(!master.contains("VIDEO-RANGE="));
        assert!(!master.contains("CODECS="));
    }

    #[test]
    fn hdr_diagnostics_isolate_master_attributes_without_changing_the_child() {
        let file = hls_file(vec![sub("subrip", "eng", "Forced", true, false)]);
        let context = hls_context("hvc1.2.4.L150.B0,ec-3", None);

        let minimal = master_playlist_diagnostic(&file, None, &context, Some("video-only"));
        assert_eq!(
            minimal,
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,RESOLUTION=3840x2160,FRAME-RATE=23.976\nindex.m3u8\n"
        );

        let range = master_playlist_diagnostic(&file, None, &context, Some("video-only-range"));
        assert!(range.contains("FRAME-RATE=23.976,VIDEO-RANGE=PQ\n"));
        assert!(!range.contains("CODECS="));
        assert!(!range.contains("SUBTITLES="));

        let codecs = master_playlist_diagnostic(&file, None, &context, Some("video-only-codecs"));
        assert!(codecs.contains("CODECS=\"hvc1.2.4.L150.B0,ec-3\""));
        assert!(!codecs.contains("VIDEO-RANGE="));

        let hdr = master_playlist_diagnostic(&file, None, &context, Some("video-only-hdr"));
        assert!(hdr.contains("VIDEO-RANGE=PQ"));
        assert!(hdr.contains("CODECS=\"hvc1.2.4.L150.B0,ec-3\""));
        assert!(hdr.ends_with("index.m3u8\n"));
    }

    /// The HDR10 rung's master, built from the codec string the session
    /// actually advertises.
    ///
    /// The point is that **no new attribute logic was written for it**. The
    /// existing rule — an `hvc1`/`hev1`/`dvh1`/`dvhe` prefix plus an HDR
    /// source column — already emits `VIDEO-RANGE=PQ` and `CODECS`, and a
    /// re-encoded HDR10 rung satisfies both. A second implementation of that
    /// rule is a second thing to keep in step.
    #[test]
    fn the_hdr10_rungs_master_gets_pq_and_hevc_codecs_from_the_existing_rule() {
        let file = hls_file(vec![]);
        // Exactly what the 1080p branch of
        // `transcode::transcoded_hls_codecs(OutputGrade::Hdr10, height)` puts
        // on the session.
        let context = hls_context("hvc1.2.4.H120.90,mp4a.40.2", None);
        let master = master_playlist(&file, None, &context);
        assert!(master.contains("VIDEO-RANGE=PQ"), "{master}");
        assert!(
            master.contains("CODECS=\"hvc1.2.4.H120.90,mp4a.40.2\""),
            "{master}"
        );
        // No SUPPLEMENTAL-CODECS: the RPU is consumed by the reshape, so
        // there is no Dolby Vision enhancement left to declare, and the
        // master stays at compatibility version 7.
        assert!(!master.contains("SUPPLEMENTAL-CODECS"), "{master}");
        assert!(master.contains("#EXT-X-VERSION:7"), "{master}");

        // The SDR rung of the same source is H.264, and its master must stay
        // SDR — the source's `hdr` column alone is not permission to claim PQ
        // for a tone-mapped picture.
        let sdr = master_playlist(&file, None, &hls_context("avc1.640034,mp4a.40.2", None));
        assert!(!sdr.contains("VIDEO-RANGE="), "{sdr}");
        assert!(!sdr.contains("CODECS="), "{sdr}");
    }

    #[test]
    fn high_tier_hevc_without_native_subtitles_uses_the_media_playlist() {
        let bitmap_only = hls_file(vec![sub(
            "hdmv_pgs_subtitle",
            "eng",
            "English",
            false,
            false,
        )]);
        let michael = hls_context("hvc1.2.4.H153.90,mp4a.40.2", None);
        assert!(should_serve_high_tier_media_playlist(
            &bitmap_only,
            &michael
        ));

        let main_tier = hls_context("hvc1.2.4.L153.B0,mp4a.40.2", None);
        assert!(!should_serve_high_tier_media_playlist(
            &bitmap_only,
            &main_tier
        ));

        let mut sdr_high_tier = bitmap_only.clone();
        sdr_high_tier.hdr = None;
        assert!(!should_serve_high_tier_media_playlist(
            &sdr_high_tier,
            &michael
        ));

        let native_text = hls_file(vec![sub("subrip", "eng", "English", false, false)]);
        assert!(!should_serve_high_tier_media_playlist(
            &native_text,
            &michael
        ));

        let high_profile_h264 = hls_context("avc1.640034,mp4a.40.2", None);
        assert!(!should_serve_high_tier_media_playlist(
            &bitmap_only,
            &high_profile_h264
        ));
    }

    #[test]
    fn high_tier_hdr_init_is_relabelled_for_apple_hls() {
        let file = hls_file(vec![sub(
            "hdmv_pgs_subtitle",
            "eng",
            "English",
            false,
            false,
        )]);
        // hvcC version 1, Main 10, High tier, compatibility 4, level 153.
        let mut init = vec![0, 0, 0, 21];
        init.extend_from_slice(b"hvcC");
        init.extend_from_slice(&[1, 0x22, 0x20, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 153]);
        assert_eq!(
            hevc_codec_from_init(&init, "hvc1").as_deref(),
            Some("hvc1.2.4.H153.90")
        );
        assert!(normalize_high_tier_hevc_init(&file, &mut init));
        assert_eq!(
            hevc_codec_from_init(&init, "hvc1").as_deref(),
            Some("hvc1.2.4.L153.90")
        );
        assert!(!normalize_high_tier_hevc_init(&file, &mut init));

        let mut sdr = file;
        sdr.hdr = None;
        init[9] |= 0x20;
        assert!(!normalize_high_tier_hevc_init(&sdr, &mut init));
    }

    #[test]
    fn hevc_codec_uses_the_published_profile_tier_level_and_constraints() {
        // hvcC version 1, Main 10, High tier, compatibility 0x20000000
        // (RFC 6381 reverse-bit value 4), constraint B0, level 150. This is
        // the header carried by the live HDR regression file's init segment.
        let mut init = vec![0, 0, 0, 21];
        init.extend_from_slice(b"hvcC");
        init.extend_from_slice(&[1, 0x22, 0x20, 0, 0, 0, 0xB0, 0, 0, 0, 0, 0, 150]);

        assert_eq!(
            hevc_codec_from_init(&init, "hvc1").as_deref(),
            Some("hvc1.2.4.H150.B0")
        );
        assert!(hevc_codec_from_init(&init[..12], "hvc1").is_none());
    }

    /// A `dvcC` record laid out the way the Dexter Profile 5 title's init
    /// segment carries it: version 1.0, profile 5, level 6, RPU and BL
    /// present, no enhancement layer, compatibility id 0.
    fn dolby_vision_init(profile: u8, level: u8) -> Vec<u8> {
        let mut init = vec![0, 0, 0, 32];
        init.extend_from_slice(b"dvcC");
        init.extend_from_slice(&[
            1,
            0,
            (profile << 1) | (level >> 5),
            ((level & 0x1f) << 3) | 0b101,
            0,
        ]);
        init.extend_from_slice(&[0; 19]);
        init
    }

    #[test]
    fn dolby_vision_codec_is_read_from_its_own_configuration_record() {
        let init = dolby_vision_init(5, 6);
        assert_eq!(
            dolby_vision_codec_from_init(&init, "dvh1").as_deref(),
            Some("dvh1.05.06")
        );
        assert_eq!(
            dolby_vision_codec_from_init(&init, "dvhe").as_deref(),
            Some("dvhe.05.06")
        );
        // Profile 8 level 10 exercises the level's high bit, which lives in
        // the profile's byte.
        assert_eq!(
            dolby_vision_codec_from_init(&dolby_vision_init(8, 10), "dvh1").as_deref(),
            Some("dvh1.08.10")
        );
        assert_eq!(
            dolby_vision_codec_from_init(&dolby_vision_init(7, 33), "dvh1").as_deref(),
            Some("dvh1.07.33")
        );
        // Truncated, absent and empty records decline rather than guess.
        assert!(dolby_vision_codec_from_init(&init[..16], "dvh1").is_none());
        assert!(dolby_vision_codec_from_init(b"nothing here at all", "dvh1").is_none());
        assert!(dolby_vision_codec_from_init(&dolby_vision_init(0, 6), "dvh1").is_none());
        assert!(dolby_vision_codec_from_init(&dolby_vision_init(5, 0), "dvh1").is_none());
    }

    /// The regression this milestone exists for. A preserved Profile 5 init
    /// carries `dvh1` + `hvcC` + `dvcC`; reading the tier out of `hvcC` and
    /// publishing `dvh1.2.4.L150.90` is what AVPlayer refused with CoreMedia
    /// -15517 while the same session's CODECS-free media playlist played.
    #[tokio::test]
    async fn a_preserved_dolby_vision_master_keeps_its_dolby_vision_identifier() {
        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "dv").await;
        // The sample entry's `hvcC` describes the base layer only: Main 10,
        // High tier, level 150 — the record the HEVC reader would have used.
        let mut init = vec![0, 0, 0, 21];
        init.extend_from_slice(b"hvcC");
        init.extend_from_slice(&[1, 0x22, 0x20, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 150]);
        init.extend_from_slice(&dolby_vision_init(5, 6));
        tokio::fs::write(dir.path().join("init.mp4"), &init)
            .await
            .expect("dolby vision init");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "dvh1.05.06,ec-3".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "dv", context).await;
        assert_eq!(
            resolved.codecs, "dvh1.05.06,ec-3",
            "a Dolby Vision master must not be rewritten into HEVC shape"
        );
    }

    /// The same probe also heals the other half of the failure: when the
    /// library row carried no DOVI side data, `copied_hls_codecs` advertises a
    /// bare `dvh1`, which the code's own comment calls fatal during asset
    /// preparation. The init knows the answer.
    #[tokio::test]
    async fn a_bare_dolby_vision_declaration_is_completed_from_the_init() {
        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "dv-bare").await;
        tokio::fs::write(dir.path().join("init.mp4"), dolby_vision_init(5, 6))
            .await
            .expect("dolby vision init");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "dvh1,ec-3".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "dv-bare", context).await;
        assert_eq!(resolved.codecs, "dvh1.05.06,ec-3");
    }

    /// And a Dolby Vision init with no configuration record at all leaves the
    /// advertised string alone rather than falling back to the HEVC reader.
    #[tokio::test]
    async fn a_dolby_vision_init_without_a_configuration_record_changes_nothing() {
        let dir = tempfile::tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "dv-nodvcc").await;
        let mut init = vec![0, 0, 0, 21];
        init.extend_from_slice(b"hvcC");
        init.extend_from_slice(&[1, 0x22, 0x20, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 150]);
        tokio::fs::write(dir.path().join("init.mp4"), &init)
            .await
            .expect("init without dvcC");

        let context = crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "dvh1.05.06,ec-3".to_owned(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let resolved = exact_hls_context(&fixture.state, "dv-nodvcc", context).await;
        assert_eq!(resolved.codecs, "dvh1.05.06,ec-3");
    }

    #[test]
    fn a_profile_5_master_declares_dolby_vision_without_a_supplemental_codec() {
        let file = hls_file(vec![]);
        let profile5 = hls_context("dvh1.05.06,ec-3", None);
        let master = master_playlist(&file, None, &profile5);

        // Profile 5 has no compatible base layer to declare, so the master
        // stays at version 7 — SUPPLEMENTAL-CODECS would drag it to 10, which
        // this code already documents as rejection-prone.
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
        assert!(master.contains("VIDEO-RANGE=PQ"));
        assert!(master.contains("CODECS=\"dvh1.05.06,ec-3\""));
        assert!(!master.contains("SUPPLEMENTAL-CODECS"));
    }

    /// Who gets the accessibility tag: the muxer's answer first, the track's
    /// name only as a fallback. The fallback is not decoration — files
    /// probed before plurx read the disposition carry `false` for it, and
    /// re-probing is a manual scan, so the naming convention is still the
    /// only signal an existing library has.
    #[test]
    fn sdh_renditions_are_tagged_from_the_disposition_before_the_title() {
        let accessibility = "CHARACTERISTICS=\"public.accessibility.transcribes-spoken-dialog,public.accessibility.describes-music-and-sound\"";
        let flagged = SubtitleStream {
            hearing_impaired: true,
            ..sub("subrip", "eng", "English", false, false)
        };
        assert!(subtitle_characteristics(&flagged).is_some());

        let file = hls_file(vec![
            flagged,
            sub("subrip", "eng", "English SDH", false, false),
            sub("subrip", "eng", "Regular", false, false),
        ]);
        let master = master_playlist(&file, None, &sdr_context());
        assert_eq!(
            master.matches(accessibility).count(),
            2,
            "the flagged track and the named one, and only those: {master}"
        );
        let line = |name: &str| {
            master
                .lines()
                .find(|line| line.contains(&format!("NAME=\"{name}\"")))
                .unwrap_or_else(|| panic!("no rendition named {name} in {master}"))
        };
        assert!(
            line("English · English").contains(accessibility),
            "the disposition tags a track its title says nothing about"
        );
        assert!(line("English · English SDH").contains(accessibility));
        assert!(!line("English · Regular").contains(accessibility));

        // A track with no title at all is not an accessibility track by
        // default — silence is not a claim.
        let untitled = SubtitleStream {
            title: None,
            ..sub("subrip", "eng", "", false, false)
        };
        assert_eq!(subtitle_characteristics(&untitled), None);
    }

    #[test]
    fn duplicate_manual_renditions_do_not_violate_hls_autoselect_uniqueness() {
        let file = hls_file(vec![
            sub("subrip", "eng", "Regular", false, false),
            sub("webvtt", "eng", "Alternate", false, false),
        ]);
        let master = master_playlist(&file, None, &sdr_context());
        assert!(!master.contains("CODECS="));
        assert_eq!(master.matches("AUTOSELECT=NO").count(), 2);

        let selected = master_playlist(&file, Some(1), &sdr_context());
        assert!(selected
            .contains("NAME=\"English · Alternate\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES"));
    }

    #[test]
    fn bitmap_and_styled_subtitles_stay_out_of_native_renditions() {
        let file = hls_file(vec![
            sub("subrip", "eng", "Regular", false, false),
            sub("hdmv_pgs_subtitle", "eng", "PGS", false, false),
            sub("dvd_subtitle", "eng", "VobSub", false, false),
            sub("ass", "eng", "Styled Signs", false, false),
            sub("ssa", "eng", "Styled Dialogue", false, false),
            // The MP4 case, and the common one: every WEB-DL carries these.
            // `SubTrackDto.text` says true of them (they do have text to
            // extract), which is exactly why `native` exists — the master
            // must not carry a rendition this path cannot slice.
            sub("mov_text", "eng", "MP4 Timed Text", false, false),
        ]);
        let master = master_playlist(&file, None, &sdr_context());
        assert!(master.contains("subs/0/index.m3u8"));
        for index in 1..=5 {
            assert!(!master.contains(&format!("subs/{index}/index.m3u8")));
        }
    }

    #[test]
    fn subtitle_playlist_and_vtt_mirror_video_segments_at_resume_timeline() {
        let video = b"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:4.000000,\nseg00000.m4s\n#EXTINF:6.000000,\nseg00001.m4s\n";
        let playlist = subtitle_media_playlist(video);
        assert!(playlist.contains("#EXT-X-TARGETDURATION:6"));
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));
        assert!(playlist.contains("#EXTINF:4.000000,\nseg00000.vtt"));
        assert!(playlist.contains("#EXTINF:6.000000,\nseg00001.vtt"));
        assert!(!playlist.contains("#EXT-X-ENDLIST"));

        let sliding = subtitle_media_playlist(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:2\n#EXTINF:6.0,\nseg00002.m4s\n",
        );
        assert!(sliding.contains("#EXT-X-MEDIA-SEQUENCE:2"), "{sliding}");
        assert!(sliding.contains("seg00002.vtt"), "{sliding}");
        assert!(
            !sliding.contains("#EXT-X-PLAYLIST-TYPE:EVENT"),
            "the subtitle rendition mirrors the video's sliding shape: {sliding}"
        );

        let source = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\npast\n\ncue-id\n00:00:09.000 --> 00:00:11.000 align:start\ncrossing\n\n00:00:15.250 --> 00:00:16.500\nfuture\n";
        let first = String::from_utf8(slice_webvtt(source, 10.0, 0.0, 4.0)).expect("utf8");
        assert!(first.contains("X-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000"));
        assert!(!first.contains("past"));
        assert!(first.contains("00:00:00.000 --> 00:00:01.000 align:start"));
        assert!(!first.contains("future"));

        let second = String::from_utf8(slice_webvtt(source, 10.0, 4.0, 10.0)).expect("utf8");
        assert!(second.contains("X-TIMESTAMP-MAP=MPEGTS:360000,LOCAL:00:00:00.000"));
        assert!(!second.contains("crossing"));
        assert!(second.contains("00:00:01.250 --> 00:00:02.500"));

        let finished = subtitle_media_playlist(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4.0,\nseg00000.m4s\n#EXT-X-ENDLIST\n",
        );
        assert!(finished.ends_with("seg00000.vtt\n#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn forced_detection_reads_words_not_substrings() {
        // The contract case: disposition says nothing, the title says Forced.
        assert!(track_is_forced(&sub(
            "subrip", "ita", "Forced", false, false
        )));
        assert!(track_is_forced(&sub(
            "subrip",
            "eng",
            "English (Forced)",
            false,
            false
        )));
        assert!(track_is_forced(&sub(
            "subrip",
            "eng",
            "forced signs",
            false,
            false
        )));
        // Disposition alone is still enough.
        assert!(track_is_forced(&sub(
            "subrip", "eng", "Regular", false, true
        )));

        // The bug: a substring test hid these tracks from Apple's subtitle
        // menu entirely, because a forced rendition is only offered when the
        // presentation language matches.
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "Non-Forced",
            false,
            false
        )));
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "non forced",
            false,
            false
        )));
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "Not Forced",
            false,
            false
        )));
        assert!(!track_is_forced(&sub(
            "subrip", "eng", "Unforced", false, false
        )));
        assert!(!track_is_forced(&sub(
            "subrip",
            "eng",
            "Reinforced Audio",
            false,
            false
        )));
    }

    #[test]
    fn language_tags_come_from_the_shared_alias_table() {
        // The ten this module used to know.
        assert_eq!(language_tag(Some("eng")), "en");
        assert_eq!(language_tag(Some("zho")), "zh");
        // And the ones it did not, which used to reach AVPlayer as
        // non-BCP-47 codes no viewer preference could ever match.
        assert_eq!(language_tag(Some("dut")), "nl");
        assert_eq!(language_tag(Some("cze")), "cs");
        assert_eq!(language_tag(Some("gre")), "el");
        assert_eq!(language_tag(Some("rum")), "ro");
        assert_eq!(language_name(Some("cze")), "Czech");
        // Unknown stays unknown rather than becoming a guess.
        assert_eq!(language_tag(Some("xyz")), "xyz");
        assert_eq!(language_tag(None), "und");
    }

    #[test]
    fn rendition_names_are_unique_within_the_group() {
        // Two untitled English tracks: RFC 8216 §4.3.4.1 makes NAME
        // MUST-unique, and the client resolves options by name — so a
        // duplicate is the client picking the wrong track, not a wart.
        let file = hls_file(vec![
            SubtitleStream {
                index: 0,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: None,
                default: false,
                forced: false,
                hearing_impaired: false,
            },
            SubtitleStream {
                index: 1,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: None,
                default: false,
                forced: false,
                hearing_impaired: false,
            },
        ]);
        let master = master_playlist_with(&file, None, &sdr_context(), MasterRungs::default());
        assert_eq!(master.matches("NAME=\"English\"").count(), 1, "{master}");
        assert!(master.contains("NAME=\"English (2)\""), "{master}");
    }

    #[test]
    fn rendition_attributes_survive_hostile_titles() {
        // A quoted-string attribute has no escape, so a quote, a comma or a
        // line break in a title is not a formatting problem — it is a
        // playlist that no longer parses.
        let file = hls_file(vec![sub(
            "subrip",
            "eng",
            "The \"Good\" One, v2\r\nsecond line",
            false,
            false,
        )]);
        let master = master_playlist_with(&file, None, &sdr_context(), MasterRungs::default());
        let line = master
            .lines()
            .find(|line| line.starts_with("#EXT-X-MEDIA:"))
            .expect("a rendition line");
        assert_eq!(
            line.matches('"').count() % 2,
            0,
            "unbalanced quotes: {line}"
        );
        assert!(!line.contains("\"Good\""), "{line}");
        assert!(line.contains("URI=\"subs/0/index.m3u8\""), "{line}");
        assert_eq!(
            master
                .lines()
                .filter(|l| l.starts_with("#EXT-X-MEDIA:"))
                .count(),
            1
        );
    }

    #[test]
    fn ladder_rungs_are_inert_until_an_operator_lights_them() {
        let file = hls_file(vec![
            sub("subrip", "ita", "Forced", false, true),
            sub("subrip", "ita", "Forced Signs", false, true),
        ]);

        // Default: the shape that plays on the device today. Two forced
        // tracks share a language, so RFC 8216's uniqueness rule keeps them
        // manually selectable.
        let shipped = master_playlist_with(&file, None, &sdr_context(), MasterRungs::default());
        assert!(!shipped.contains("CLOSED-CAPTIONS"), "{shipped}");
        assert_eq!(shipped.matches("AUTOSELECT=NO").count(), 2, "{shipped}");
        assert!(!shipped.contains("CODECS="), "{shipped}");

        // Rung 1, alone.
        let captions = master_playlist_with(
            &file,
            None,
            &sdr_context(),
            MasterRungs {
                closed_captions_none: true,
                ..MasterRungs::default()
            },
        );
        assert!(
            captions.contains(
                "#EXT-X-STREAM-INF:BANDWIDTH=40000000,AVERAGE-BANDWIDTH=40000000,\
                 RESOLUTION=3840x2160,FRAME-RATE=23.976,CLOSED-CAPTIONS=NONE,\
                 SUBTITLES=\"subs\""
            ),
            "{captions}"
        );
        assert_eq!(captions.matches("AUTOSELECT=NO").count(), 2, "{captions}");

        // Rung 2, alone.
        let forced = master_playlist_with(
            &file,
            None,
            &sdr_context(),
            MasterRungs {
                forced_autoselect: true,
                ..MasterRungs::default()
            },
        );
        assert!(!forced.contains("CLOSED-CAPTIONS"), "{forced}");
        assert_eq!(forced.matches("AUTOSELECT=YES").count(), 2, "{forced}");
    }

    #[test]
    fn a_cue_spanning_a_segment_boundary_keeps_its_authored_end() {
        // 6 s windows; the cue runs 5.0 → 8.0, straddling the boundary.
        let source = b"WEBVTT\n\nspan\n00:00:05.000 --> 00:00:08.000\ncrossing\n";
        let first = String::from_utf8(slice_webvtt(source, 0.0, 0.0, 6.0)).expect("utf8");
        let second = String::from_utf8(slice_webvtt(source, 0.0, 6.0, 12.0)).expect("utf8");

        // It appears in both windows it intersects...
        assert!(first.contains("crossing"), "{first}");
        assert!(second.contains("crossing"), "{second}");
        // ...and the copy in the first window runs to its AUTHORED end rather
        // than being cut off at the boundary. Clipping it there is what tore
        // a line of dialogue in half and flickered at every 6 s seam.
        assert!(first.contains("00:00:05.000 --> 00:00:08.000"), "{first}");
        // The trailing copy still starts at the window edge, because cue
        // times in this scheme are segment-local and WebVTT cannot spell a
        // negative one. Its identifier is what lets a player reconcile the
        // two.
        assert!(second.contains("00:00:00.000 --> 00:00:02.000"), "{second}");
        assert!(second.contains("span"), "{second}");
    }

    #[test]
    fn cue_shifting_is_relative_to_the_media_origin_not_the_request() {
        // The P0-2 shape, as the slicer sees it: a copy session asked to
        // start at 12.3 s whose media actually begins at the 10 s keyframe.
        // A cue authored at 14.0 s belongs 4.0 s into the session, not 1.7.
        let source = b"WEBVTT\n\n00:00:14.000 --> 00:00:16.000\nline\n";
        let correct = String::from_utf8(slice_webvtt(source, 10.0, 0.0, 6.0)).expect("utf8");
        assert!(
            correct.contains("00:00:04.000 --> 00:00:06.000"),
            "{correct}"
        );

        let by_request = String::from_utf8(slice_webvtt(source, 12.3, 0.0, 6.0)).expect("utf8");
        assert!(by_request.contains("00:00:01.700"), "{by_request}");
        assert!(
            !by_request.contains("00:00:04.000 -->"),
            "shifting by the request leads the picture by the seek's distance from its keyframe"
        );
    }
}
