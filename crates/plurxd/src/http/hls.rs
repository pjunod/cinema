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

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{future::BoxFuture, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use plurx_core::domain::{
    MediaFile, MediaSessionActivation, MediaSessionActivationOutcome,
    MediaSessionActivationSettlement, MediaSessionProjectionCompletion, MediaSessionRequestClaim,
    MediaSessionRoute, MediaSessionTerminalAck, SubtitleStream, MEDIA_SESSION_PUBLICATION_BLOCKED,
};
use plurx_core::error::StoreError;
use plurx_core::playback::PlaybackMethod;
use plurx_core::tracks::is_native_text_subtitle;

use super::error::ApiError;
use super::extract::AuthUser;
use super::peer_transport::PeerTransportError;
use crate::media_pool::MediaOfferRequest;
use crate::media_sessions::{
    unix_ms, worker_session_request_is_valid, DurableRouteResolution, RelayHeaders, RelayRequest,
    RelayResource, ReleaseAdmission, ReleaseSettlement, RemoteAbortRequest, RemotePrepareRequest,
    RemoteStartRequest, RemoteStartResponse, ACTIVATION_STORE_DEADLINE, LEASE_TTL_MS,
    MAX_ADMITTED_MEDIA_BODY_LIFETIME, MEDIA_BODY_NO_PROGRESS_TIMEOUT, OWNER_ASSIGNMENT_DEADLINE,
    PREPARED_SUCCESSOR_PLAN_NOTE, REMOTE_ACTIVATION_CONFIRMATION_WINDOW, START_DEADLINE,
    TERMINAL_PROJECTION_SAFETY_WINDOW,
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
/// One absolute bound for the actor/manager portion of response publication.
/// Storage and network streaming have their own budgets; this prevents a live
/// HTTP request or detached EOF owner from waiting forever on control state.
const RESPONSE_PUBLICATION_LIFECYCLE_BUDGET: Duration = Duration::from_secs(5);
/// A segment request may legitimately spend up to the configured 30-second
/// VOD blocked-GET cap before response publication. The extra publication
/// budget is an outer request fence for lookup, resurrection and preparation;
/// actor admission still receives the shorter five-second sub-deadline.
const SEGMENT_REQUEST_LIFECYCLE_BUDGET: Duration = Duration::from_secs(35);
/// Completed streams retain one permit from pre-exposure admission through
/// exact EOF settlement. This bounds both active settlement ownership and the
/// detached tasks that can be alive at once.
const RESPONSE_COMPLETION_CAPACITY: usize = 256;
/// Public capability releases detach once admitted so request cancellation
/// cannot strand a committed tombstone without exact-owner cleanup. A fixed
/// settlement pool prevents slow durable storage from creating unbounded
/// detached work under random capability probes.
const SESSION_RELEASE_CAPACITY: usize = 128;
const REMOTE_RELEASE_ATTEMPTS: usize = 3;
const REMOTE_RELEASE_RETRY_DELAY: Duration = Duration::from_millis(100);
const REQUEST_CLAIM_SETTLEMENT_BUDGET: Duration = Duration::from_secs(5);
const REQUEST_CLAIM_SETTLEMENT_RETRY_DELAY: Duration = Duration::from_millis(100);
const PREDECESSOR_PROJECTION_FAST_WINDOW: Duration = Duration::from_secs(5);
const PREDECESSOR_PROJECTION_RETRY_DELAY: Duration = Duration::from_secs(1);

async fn pin_shared_session_for_local_start<F>(
    deadline: tokio::time::Instant,
    pin: F,
) -> Result<bool, ApiError>
where
    F: std::future::Future<Output = Result<bool, StoreError>>,
{
    super::internal_media_sessions::pin_shared_session_before_deadline(deadline, pin)
        .await
        .map_err(|error| {
            if crate::transcode::is_start_infrastructure_error(&error) {
                ApiError::ServiceUnavailable(error)
            } else {
                ApiError::Internal(error)
            }
        })
}

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
    /// The Dolby Vision profile *this session's* bytes carry, when it is
    /// known. Absent for a transcode, a strip, a source that never had Dolby
    /// Vision — and for a pre-M2 row whose label names no profile, which is
    /// why absence means "no answer" rather than "not Dolby Vision".
    /// `delivered_dynamic_range` beside it is the field that answers that.
    ///
    /// The one thing the field above cannot say. A Profile 7 title preserved
    /// for a device that enumerates 7 and the same title converted to 8.1 for
    /// a device that does not are both `"dolby_vision"`, and the badge that
    /// spells both `DV P7` is telling one of them something untrue about its
    /// own file (MEDIA-BADGES-PLAN §2.3). It overrides the decision's answer
    /// for the same reason the range does: a burn or a forced rung produces a
    /// session the decision never promised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_dolby_vision_profile: Option<u8>,
    /// Optional behavior-neutral v1 control capability. It is persisted with
    /// the idempotent create result so a setting change cannot mutate the wire
    /// contract of an already-open session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<crate::playback_control::ControlBootstrap>,
    /// Why this session's plan is not the one the client asked for, when it
    /// isn't (PLAYBACK-CAPS-V2-PLAN §4.5). Every entry is either a named
    /// override the client itself supplied or a `plan_mismatch:` line naming
    /// the field, the client's value and the server's.
    ///
    /// Empty and omitted on the wire for every agreeing create, which is the
    /// case this exists to make visible by its absence. The stats overlay
    /// shows these next to the badge: a viewer whose Dolby Vision title is
    /// playing as HDR10 can read *why* without an admin reading the log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan_notes: Vec<String>,
}

const PLANNING_CAPS_RESPONSE_FIELD: &str = "planning_caps";
const PLANNING_OVERRIDES_RESPONSE_FIELD: &str = "planning_overrides";
const PREPARATION_REASON_RESPONSE_FIELD: &str = "preparation_reason";

/// Read the create-time planning snapshot from the additive durable response.
///
/// The worker envelope intentionally denies unknown fields because it crosses
/// mixed-version nodes. `StartResponse` is already additive, so keeping this
/// server-owned sidecar beside its durable copy preserves legacy worker
/// activation while allowing a new owner to re-plan after takeover.
fn retained_planning_caps(response_json: &str) -> Option<plurx_core::playback::DeviceCaps> {
    serde_json::from_str::<serde_json::Value>(response_json)
        .ok()?
        .get(PLANNING_CAPS_RESPONSE_FIELD)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn retained_planning_overrides(response_json: &str) -> Option<CreateOverrides> {
    serde_json::from_str::<serde_json::Value>(response_json)
        .ok()?
        .get(PLANNING_OVERRIDES_RESPONSE_FIELD)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Owns a published worker until the replicated activation has a definitive
/// outcome. Request futures are cancellation points at every store/network
/// await; tying cleanup to this value prevents a disconnected client from
/// leaving an encoder and its durable start claim behind.
pub(super) struct StartedSessionGuard {
    cleanup: Option<StartedSessionCleanup>,
}

struct MediaSessionRequestGuard {
    cleanup: Option<(AppState, i64, String, String)>,
}

async fn settle_media_session_request_claim(
    state: &AppState,
    user_id: i64,
    request_id: &str,
    incarnation_id: &str,
) {
    let deadline = tokio::time::Instant::now() + REQUEST_CLAIM_SETTLEMENT_BUDGET;
    loop {
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .fail_media_session_request(user_id, request_id, incarnation_id, unix_ms()),
        )
        .await
        {
            // `false` is also settled: the exact claim already resolved or a
            // newer incarnation owns it, so this cleanup must not touch it.
            Ok(Ok(_)) => return,
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    user_id,
                    "media-session request cleanup is retrying"
                );
            }
            Err(_) => break,
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(REQUEST_CLAIM_SETTLEMENT_RETRY_DELAY.min(remaining)).await;
    }
    tracing::error!(
        user_id,
        retry_after_ms = 60_000,
        "media-session request cleanup exhausted its bound; claim expiry remains the durable fallback"
    );
}

impl MediaSessionRequestGuard {
    fn new(state: AppState, user_id: i64, request_id: String, incarnation_id: String) -> Self {
        Self {
            cleanup: Some((state, user_id, request_id, incarnation_id)),
        }
    }

    fn disarm(&mut self) {
        self.cleanup = None;
    }
}

impl Drop for MediaSessionRequestGuard {
    fn drop(&mut self) {
        let Some((state, user_id, request_id, incarnation_id)) = self.cleanup.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            settle_media_session_request_claim(&state, user_id, &request_id, &incarnation_id).await;
        }));
    }
}

struct StartedSessionCleanup {
    state: AppState,
    owner_node_id: String,
    incarnation_id: String,
    session_id: String,
    user_id: i64,
    request_id: String,
    owns_worker: bool,
    owns_request_claim: bool,
    _replacement: Option<ClusterReplacementGuard>,
    #[cfg(test)]
    test_settlement: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    )>,
}

impl StartedSessionGuard {
    pub(super) fn new(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            true,
            true,
        )
    }

    pub(super) fn recovered(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            false,
            true,
        )
    }

    /// Observe an idempotently replayed worker without acquiring cleanup
    /// ownership of either the worker or its original durable request claim.
    pub(super) fn replayed(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            false,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn worker_only(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            true,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_only(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            None,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_ownership(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
        owns_worker: bool,
        owns_request_claim: bool,
    ) -> Self {
        Self {
            cleanup: Some(StartedSessionCleanup {
                state,
                owner_node_id,
                incarnation_id,
                session_id,
                user_id,
                request_id,
                owns_worker,
                owns_request_claim,
                _replacement: replacement,
                #[cfg(test)]
                test_settlement: None,
            }),
        }
    }

    pub(super) fn disarm(&mut self) {
        self.cleanup = None;
    }

    #[cfg(test)]
    fn hold_cleanup_for_test(
        &mut self,
        settled: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
        released: tokio::sync::oneshot::Sender<()>,
    ) {
        self.cleanup
            .as_mut()
            .expect("armed guard cleanup")
            .test_settlement = Some((settled, release, released));
    }
}

impl crate::transcode::SessionAdoptionOwner for StartedSessionGuard {
    fn adopted_session_id(&mut self, durable_session_id: &str) {
        if let Some(cleanup) = self.cleanup.as_mut() {
            cleanup.session_id = durable_session_id.to_owned();
        }
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
        let StartedSessionCleanup {
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            owns_worker,
            owns_request_claim,
            _replacement,
            #[cfg(test)]
            test_settlement,
        } = cleanup;
        std::mem::drop(runtime.spawn(async move {
            if owns_worker {
                abort_started_session(&state, &owner_node_id, &incarnation_id, &session_id).await;
            }
            if owns_request_claim {
                settle_media_session_request_claim(&state, user_id, &request_id, &incarnation_id)
                    .await;
            }
            #[cfg(test)]
            if let Some((settled, release, released)) = test_settlement {
                let _ = settled.send(());
                let _ = release.await;
                drop(_replacement);
                let _ = released.send(());
                return;
            }
            drop(_replacement);
        }));
    }
}

/// Owns the gap between durable activation confirmation and publication of
/// the corresponding create response. Confirmation deliberately leaves the
/// request claim in `starting`; this guard races final response publication
/// with atomic abandonment, so a healthy retry can never recover a route whose
/// worker a delayed cleanup is about to stop.
struct ActivationPublicationGuard {
    cleanup: Option<(AppState, MediaSessionActivation)>,
}

impl ActivationPublicationGuard {
    fn new(state: AppState, activation: MediaSessionActivation) -> Self {
        Self {
            cleanup: Some((state, activation)),
        }
    }

    fn disarm(&mut self) {
        self.cleanup = None;
    }
}

impl Drop for ActivationPublicationGuard {
    fn drop(&mut self) {
        let Some((state, activation)) = self.cleanup.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            settle_activation_publication_cleanup(state, activation).await;
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
    /// The control exchange this restart was decided from, when the client has
    /// one. A seek storm asks for a session per destination; the sequence is
    /// what orders those asks against the destination the client has since
    /// settled on, so a restart the viewer has already scrolled past can be
    /// skipped rather than produced and thrown away.
    ///
    /// Absent means "do the work", which is what every client did before this
    /// field existed and what a first play still means -- there is no earlier
    /// exchange to be stale against.
    pub control_sequence: Option<u64>,
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
    /// The same capabilities document the client posted to `/decision`
    /// (PLAYBACK-CAPS-V2-PLAN §4.5).
    ///
    /// When present, create re-derives the plan from it and the two fields
    /// above become *assertions* rather than instructions — a disagreement is
    /// logged and counted, and the server's plan is used. When absent, the
    /// echo is trusted exactly as it always was, so every shipped build keeps
    /// working unchanged; that path is counted too, so the fleet can be
    /// watched for stragglers.
    pub caps: Option<plurx_core::playback::DeviceCaps>,
    /// The named reasons this body may legitimately differ from the plan the
    /// server derives from `caps`.
    pub overrides: Option<CreateOverrides>,
    /// The only accepted value is `"vod"`. Omitted also means VOD so clients
    /// predating this field cannot accidentally enter the removed growing
    /// live-HLS path. An explicit legacy value receives a typed refusal.
    pub presentation: Option<String>,
    /// With `presentation:"vod"`: the client's ceiling for one blocking
    /// segment fetch, in seconds. Clamped server-side.
    pub block_budget_secs: Option<f64>,
    /// What the viewer is asking for, as an orderable ask rather than as the
    /// flat scalars above.
    ///
    /// The scalars stay, and they still drive the pipeline — this does not
    /// replace them and a client that sends neither behaves exactly as before.
    /// What it adds is the one thing they cannot express: a complete normalized
    /// selection that keeps Auto, Original and Manual apart, carrying a
    /// revision that says which of two asks is later.
    ///
    /// It is on *create* and not only on control because the control protocol
    /// can be switched off, and a session created that way has no control
    /// channel at all. In that configuration the create body is the only thing
    /// the server ever learns about what the viewer wants, so an ask that
    /// arrives only through control is, there, an ask that never arrives.
    pub intent: Option<plurx_core::playback::MediaIntentEnvelope>,
    /// How this client will play and tear down the stream.
    ///
    /// `"hlsjs"` names the one audited teardown contract: the web player
    /// destroys its hls.js instance before it sends the release, so its
    /// retired objects can be dropped a segment after the DELETE instead of
    /// a whole advertised playlist later. `"native"`, an unrecognized value
    /// and an absent field all keep the original promise.
    ///
    /// The client must send what it actually selected, not a guess from the
    /// user agent, and must choose the conservative class whenever its
    /// transport could still fall back within the session.
    pub transport: Option<String>,
}

/// Test-only seam that freezes one create after it has recorded its viewer's
/// ask and before its activation runs.
///
/// The property it exists to prove cannot be observed any other way. `create`
/// carries the revision it *recorded* into the activation rather than one read
/// at activation time, and those two values are identical except in the window
/// between them — so a test that cannot stop inside that window cannot tell the
/// correct implementation from the broken one. Modelled on the replicated
/// store's `ACTIVATION_POINTER_READ_PAUSE`, which exists for the same reason.
///
/// One waiter at a time, taken rather than cloned, so a test that forgets to
/// arm it cannot accidentally inherit another test's pause.
#[cfg(test)]
type CreateAskRecordedPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[cfg(test)]
static CREATE_ASK_RECORDED_PAUSE: std::sync::LazyLock<
    std::sync::Mutex<Option<CreateAskRecordedPause>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

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
                // Never taken from the body. A client cannot ask to be handed
                // a conversion — whether one happens is decided from its caps
                // and the node's, and create overwrites this from the plan it
                // re-derives (`review_client_plan`). A wire field here would
                // be a claim the client has no way to be right about.
                convert_dolby_vision: false,
            }
        } else {
            SessionKind::Transcode { height }
        };
        // Refuse a malformed value rather than storing it; an unrecognized
        // but well-formed one is kept and simply reads as conservative.
        let transport = self
            .transport
            .filter(|transport| crate::transcode::session_transport_is_valid(transport));
        crate::transcode::SessionRequest {
            file_id,
            playback_id: self.playback_id,
            request_id: self.request_id,
            control_sequence: self.control_sequence,
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
            transport,
        }
    }
}

/// Process-lifetime counters for what create did with the plan it was handed.
///
/// The seam this closes (PLAYBACK-CAPS-V2-PLAN E4) was never a live bug — all
/// three shipped clients propagate `/decision`'s answer faithfully. It was a
/// *trust* relationship with no way to tell whether it still held. These four
/// numbers are that way: `legacy_trusted` falling to zero is how the fleet
/// proves every build has moved to caps v2, and `mismatched` rising is how a
/// client that started lying about itself gets noticed before a viewer files
/// a bug about a black screen.
pub mod plan_derivation {
    use std::sync::atomic::{AtomicU64, Ordering};

    static LEGACY_TRUSTED: AtomicU64 = AtomicU64::new(0);
    static UNUSABLE_CAPS: AtomicU64 = AtomicU64::new(0);
    static REDERIVED: AtomicU64 = AtomicU64::new(0);
    static MISMATCHED: AtomicU64 = AtomicU64::new(0);
    static OVERRIDDEN: AtomicU64 = AtomicU64::new(0);

    pub(super) fn count_legacy_trusted() {
        LEGACY_TRUSTED.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_unusable_caps() {
        UNUSABLE_CAPS.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_rederived() {
        REDERIVED.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_mismatched() {
        MISMATCHED.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_overridden() {
        OVERRIDDEN.fetch_add(1, Ordering::Relaxed);
    }

    /// `(legacy_trusted, unusable_caps, rederived, mismatched, overridden)`,
    /// this process.
    ///
    /// `rederived` is loaded FIRST and incremented BEFORE its own outcome
    /// counters, so a concurrent create can only ever make the total look
    /// larger than its parts — never `mismatched > rederived`, which reads as
    /// a bug in the server rather than in a client.
    pub fn snapshot() -> (u64, u64, u64, u64, u64) {
        let rederived = REDERIVED.load(Ordering::Relaxed);
        (
            LEGACY_TRUSTED.load(Ordering::Relaxed),
            UNUSABLE_CAPS.load(Ordering::Relaxed),
            rederived,
            MISMATCHED.load(Ordering::Relaxed),
            OVERRIDDEN.load(Ordering::Relaxed),
        )
    }
}

/// The named reasons a create body is *allowed* to disagree with the plan the
/// server derives from the same capabilities.
///
/// Named, because the alternative is a client that can silently opt out of
/// the server's verdict by echoing whatever it likes — which is the seam this
/// milestone exists to close. Each override that fires appends its own reason
/// to the session's notes, so the badge shows why the delivery is not the one
/// the caps alone would have produced.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CreateOverrides {
    /// Apple's `forceCompatibleHDRBase` retry (`PlayerController.swift:2373`):
    /// the client decoded the Dolby Vision stream, failed, and is asking for
    /// the HDR10-compatible base instead. A legitimate decline of a plan the
    /// server derived correctly from capabilities that were themselves
    /// correct — the device really does claim the profile, and really did
    /// fail on this title.
    #[serde(default)]
    pub compatible_hdr_base: Option<bool>,
    /// The quality menu: `"auto" | "original" | "transcode"`. It is fed into
    /// the re-derivation itself rather than compared against it, so a forced
    /// rung produces the server's plan *for that force* — but it still leaves
    /// a note, because a viewer who forced a transcode and then reads an SDR
    /// badge deserves to see the two facts next to each other.
    #[serde(default)]
    pub force: Option<String>,
}

/// What create decided about the plan the client echoed, and what to say
/// about it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanReview {
    /// The value create must use, whatever the body asked for.
    pub preserve_dolby_vision: bool,
    /// Whether the copy converts Profile 7 to 8.1 on the way through.
    ///
    /// Not clamped against anything the client asked for, because there is
    /// nothing to clamp: the conversion is not a wire field and a client has
    /// no way to be right or wrong about it. It follows
    /// `preserve_dolby_vision` instead — a client that declines Dolby Vision
    /// declines the converted kind too, and there is no such thing as
    /// converting RPUs a stripping filter has already removed.
    pub convert_dolby_vision: bool,
    /// Likewise for the HDR10 re-encode request. Still only a *request*:
    /// `TranscodeManager::hdr10_grade_for` refuses it for any source, rung, or
    /// build that did not prove the chain, and that refusal is unchanged.
    pub hdr10: bool,
    /// Reasons, in the order they were established. Empty means the client's
    /// echo and the server's derivation agreed and no override fired — the
    /// case that should be every case.
    pub notes: Vec<String>,
    /// A disagreement no override explained. Counted and logged; never a
    /// refusal (Paul, 2026-08-29: "I don't see a reason for it to prevent
    /// functionality").
    pub mismatched: bool,
}

/// Put the reconciled plan onto the request that will actually be built, and
/// hand back the notes that explain it.
///
/// Separate from `create` because this is the only place the server's own
/// derivation reaches the session, and every field it copies has a different
/// way of going wrong if it does not:
///
/// - `preserve_dolby_vision` reverts to the client's echo, which is the
///   pre-caps-v2 bug this milestone exists to close;
/// - `convert_dolby_vision` is never set at all — `into_request` leaves it
///   false, because a client has no way to ask for a conversion — so the whole
///   feature silently does nothing on every session;
/// - `hdr10` reverts to a claim the caps did not support.
fn apply_plan_review(
    request: &mut crate::transcode::SessionRequest,
    review: PlanReview,
) -> Vec<String> {
    if let crate::transcode::SessionKind::Copy {
        preserve_dolby_vision,
        convert_dolby_vision,
        ..
    } = &mut request.kind
    {
        *preserve_dolby_vision = review.preserve_dolby_vision;
        // The only place this is ever set. It is derived, never echoed.
        *convert_dolby_vision = review.convert_dolby_vision;
    }
    request.hdr10 = review.hdr10;
    review.notes
}

/// The plan for a build that sends no caps document: its echo, plus the one
/// field its echo cannot carry.
///
/// Everything a client can state, it states, and this trusts it — that is what
/// the legacy path *is*, and there is no capability document here to re-derive
/// from. The conversion is the exception, because no client has ever been able
/// to ask for one: `CreateSession` has no such field, so the echo is silent
/// and [`crate::transcode::SessionKind::Copy`] keeps the `false` it was built
/// with. Silence then reads as "no", which quietly overrides the *server's*
/// own answer — `/decision` says "Profile 7 converted to Profile 8.1 for this
/// device" and the session a second later preserves raw Profile 7.
///
/// That is the one delivery nothing plays: a dual-layer stream no consumer
/// decoder outside Blu-ray hardware takes. Observed in production as Safari
/// answering `stream_rejected ... browser refused the remux stream`, followed
/// by a fallback that tonemapped the title to SDR.
///
/// Only the file half of [`plurx_core::playback::dolby_vision_converts_to_p81`]
/// is asked here. Its client half ("takes 8 but not 7") exists to keep an
/// enhancement layer for a client that *enumerated* Profile 7, and it only
/// ever narrows who converts — a build sending no caps document enumerated
/// nothing, so there is no such claim to honor. Handing it the layer is not
/// the more conservative answer; it is the unplayable one.
///
/// The conversion still follows the preservation, for the same reason
/// [`review_client_plan`] ends by clamping it: a client that declined Dolby
/// Vision must not be handed a converted stream by a flag nobody looked at.
///
/// **And the preservation is not trusted all the way either.** Everything
/// above is about the conversion, which the echo cannot carry. This is about
/// the one thing the echo *does* carry that must still not be taken at face
/// value.
///
/// `preserve_dolby_vision: true` means "send me the Dolby Vision", and for
/// every single-layer profile that is a claim the client is entitled to make.
/// For a dual-layer source it is not a claim the client ever made: it echoed
/// a `true` the *server* produced, and the server produced it expecting to
/// convert. When the conversion then does not happen — an operator turned it
/// off, or the row is label-only and
/// [`plurx_core::playback::file_can_convert_to_p81`] refuses it for want of
/// the columns to build a record from — trusting the echo delivers raw
/// dual-layer to a build that enumerated nothing.
///
/// So the echo is clamped by the same rule
/// `DeviceProfile::decodes_dolby_vision_as_copied` applies to a document that
/// *did* enumerate: dual-layer needs either a conversion or an enumeration,
/// and a build sending no readable capabilities has neither.
///
/// Not counted as a `plan_mismatch`. That counter measures a client claiming
/// more than its own document supports, and there is no document here to
/// exceed; the note carries the explanation instead.
fn legacy_trusted_review(
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
) -> PlanReview {
    let convert_dolby_vision = asked_preserve_dolby_vision
        && node.dolby_vision_convert
        && plurx_core::playback::file_can_convert_to_p81(file);
    let dual_layer_stays_dual_layer =
        !convert_dolby_vision && plurx_core::playback::dolby_vision_is_dual_layer(file);
    let preserve_dolby_vision = asked_preserve_dolby_vision && !dual_layer_stays_dual_layer;
    let mut notes = Vec::new();
    if asked_preserve_dolby_vision && !preserve_dolby_vision {
        // Says what was *decided*, not what will be delivered. Whether the
        // base layer survives as HDR10, as HLG, or not at all is
        // `dv_handling`'s answer a layer down, and a note that guessed it
        // would be wrong for a Profile 7 over an SDR base.
        notes.push(
            "Dolby Vision declined: this build sent no capability document, and its \
             dual-layer stream is not one this copy can convert or any client that did \
             not name the profile can decode"
                .to_owned(),
        );
    }
    PlanReview {
        preserve_dolby_vision,
        convert_dolby_vision,
        hdr10: asked_hdr10,
        notes,
        mismatched: false,
    }
}

/// Re-derive the plan from the capabilities the client sent, and reconcile it
/// with what the client asked for.
///
/// **The server's plan is a ceiling, not a floor.** A client may always ask
/// for *less* than the derivation allows — that is a client declining
/// something it is entitled to decline, and the reason it must stay allowed
/// is Apple's `forceCompatibleHDRBase` retry (`PlayerController.swift:2373`):
/// the device really does take Profile 8, so the derivation really does say
/// "preserve", and forcing that answer back onto a retry hands the client the
/// exact stream it just failed on, forever. What the client may *not* do is
/// claim more than its own document supports — and that direction is the
/// whole of E4, because it is the one that ends in a black screen.
///
/// So a downward echo is honoured silently (a named override still says why,
/// when there is one), and an upward echo is clamped, logged, counted and
/// reported. No shipped build is affected either way: a client that sends no
/// `caps` never reaches this function.
///
/// The two fields are different *kinds* of claim and are reconciled
/// differently:
///
/// - `preserve_dolby_vision` is a **per-title** verdict. The derivation
///   answers it directly.
/// - `hdr10` is a **per-title request** for the HDR10 re-encode rung, and the
///   caps document only says whether the client could present one at all. So
///   the document is a *permission*, not an answer: the request stands when
///   the document backs it, and is refused when it does not. Deriving it from
///   the document alone would set `hdr10: true` on every create from a
///   PQ-capable client, including every SDR title — which would both change
///   the ladder ceiling (`capability_height_ceiling_for_request`) for requests
///   that never asked, and pin the §4.5 mismatch counter permanently off zero,
///   making it useless for the one thing it exists to detect.
pub(crate) fn review_client_plan(
    caps: &plurx_core::playback::DeviceCaps,
    overrides: Option<&CreateOverrides>,
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
    now_ms: i64,
) -> PlanReview {
    review_client_plan_inner(
        caps,
        overrides,
        file,
        node,
        asked_preserve_dolby_vision,
        asked_hdr10,
        now_ms,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn review_client_plan_inner(
    caps: &plurx_core::playback::DeviceCaps,
    overrides: Option<&CreateOverrides>,
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
    now_ms: i64,
    count_create_metrics: bool,
) -> PlanReview {
    use plurx_core::playback::{decide_forced, DeviceProfile, Force};

    if count_create_metrics {
        plan_derivation::count_rederived();
    }
    let mut profile = DeviceProfile::from_caps_v2(caps);
    // The same clock the decision was taken under. Without this a create
    // would apply a learned limit that `/decision` had already let expire,
    // and the two halves of one playback would disagree about the plan for
    // the reason the re-derivation exists to eliminate.
    profile.retain_applicable_learned_limits(now_ms);
    let named_force = overrides.and_then(|o| o.force.as_deref());
    let force = named_force.map(Force::parse).unwrap_or(Force::Auto);
    let derived = decide_forced(file, &profile, force, node);
    let mut review = PlanReview {
        preserve_dolby_vision: derived.preserve_dolby_vision,
        convert_dolby_vision: derived.convert_dolby_vision,
        hdr10: asked_hdr10 && profile.supports_hdr10_transcode,
        notes: Vec::new(),
        mismatched: false,
    };
    let mut overridden = false;

    if matches!(force, Force::Original | Force::Transcode) {
        review
            .notes
            .push(format!("override force={}", named_force.unwrap_or("auto")));
        overridden = true;
    }

    // The compatible-base retry only ever *lowers* the plan, and only for
    // Dolby Vision. It cannot be used to claim a profile the caps deny.
    let compatible_hdr_base = overrides
        .and_then(|o| o.compatible_hdr_base)
        .unwrap_or(false);
    if compatible_hdr_base {
        overridden = true;
        if review.preserve_dolby_vision {
            review.preserve_dolby_vision = false;
            review.notes.push(
                "override compatible_hdr_base: Dolby Vision declined by the client".to_owned(),
            );
        } else {
            // Harmless, and worth saying: a client retrying a title the server
            // was never going to send as Dolby Vision is a client chasing the
            // wrong failure.
            review.notes.push(
                "override compatible_hdr_base had nothing to decline: the plan was not \
                 Dolby Vision"
                    .to_owned(),
            );
        }
    }
    if overridden && count_create_metrics {
        plan_derivation::count_overridden();
    }

    // Clamp, then report. Only the upward direction is a mismatch: `asked`
    // above `derived` is a claim the client's own document does not support.
    for (field, asked, derived) in [
        (
            "preserve_dolby_vision",
            asked_preserve_dolby_vision,
            &mut review.preserve_dolby_vision,
        ),
        ("hdr10", asked_hdr10, &mut review.hdr10),
    ] {
        if asked && !*derived {
            review.mismatched = true;
            review.notes.push(format!(
                "plan_mismatch: client asked {field}=true, server derived false"
            ));
        } else if !asked && *derived {
            // The client declined something it could have had. Its answer
            // stands; a named override, if one fired, has already said why.
            *derived = false;
        }
    }
    // Whatever the clamps and the overrides settled, the conversion follows
    // the preservation. A `compatible_hdr_base` retry that declined Dolby
    // Vision, or a client that never asked for it, must not be handed a
    // converted stream by a flag nobody looked at.
    review.convert_dolby_vision &= review.preserve_dolby_vision;
    if review.mismatched && count_create_metrics {
        plan_derivation::count_mismatched();
    }
    review
}

/// Who the create was, for the three log lines the arms below emit.
///
/// A struct rather than three parameters because none of them changes what is
/// derived — they only say whose create it was — and a signature that mixes
/// the two invites a future reader to derive something from `user_id`.
pub(crate) struct ReviewContext<'a> {
    pub file_id: i64,
    pub user_id: i64,
    pub client_build: &'a str,
}

/// Which derivation a create with a source row gets, and the counter that
/// says so.
///
/// This is a function rather than a `match` inside `create` because the arm
/// *is* the behaviour. `legacy_trusted_review` has been correct since #842 and
/// is well covered; what shipped broken before #842, and what a future edit
/// can silently restore, is the arm returning `None` for a build that sends no
/// caps document — after which `apply_plan_review` never runs and
/// `SessionKind::Copy` keeps the `convert_dolby_vision: false` it was built
/// with, which is a raw Profile 7 stream no consumer decoder takes. A test
/// that hands `resolve_plan` a review it built itself cannot see that; a test
/// on this function can, and does
/// (`a_create_that_sends_no_caps_document_still_gets_a_review`).
///
/// `create` keeps the fourth case — no source row — because it is the one that
/// must not ask the node what it can render on the way to a 404.
///
/// The node's render caps are therefore resolved once, by the caller, for
/// every create that has a file. That is one extra cheap read on the
/// unusable-caps path, which is a straggler population on its way to zero, and
/// it buys a derivation that takes no `AppState`.
#[allow(clippy::too_many_arguments)] // one create's worth of inputs
fn plan_review_for(
    caps: Option<&plurx_core::playback::DeviceCaps>,
    overrides: Option<&CreateOverrides>,
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
    now_ms: i64,
    log: &ReviewContext<'_>,
) -> Option<PlanReview> {
    match caps {
        Some(caps) if caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty() => {
            let review = review_client_plan(
                caps,
                overrides,
                file,
                node,
                asked_preserve_dolby_vision,
                asked_hdr10,
                now_ms,
            );
            if review.mismatched {
                tracing::warn!(
                    file_id = log.file_id,
                    user_id = log.user_id,
                    client_build = %log.client_build,
                    asked_preserve_dolby_vision,
                    derived_preserve_dolby_vision = review.preserve_dolby_vision,
                    asked_hdr10,
                    derived_hdr10 = review.hdr10,
                    "plan_mismatch: the create body claims more than its own caps support; \
                     proceeding with the server's plan"
                );
            }
            Some(review)
        }
        // A document this server cannot read is worth exactly as much as no
        // document: its fields may not mean what v2's mean. It falls back to
        // the trust path rather than being refused, because a create is not
        // the place to fail a client over metadata — but it is counted
        // separately, because it is not the same population as a build that
        // predates caps v2 and it must not pollute the number that says the
        // migration is finished.
        //
        // "The trust path" is `legacy_trusted_review`, not the absence of a
        // review — and getting that wrong left this arm as the only create
        // path that could still build the delivery this milestone exists to
        // stop. `None` skips `apply_plan_review` entirely: the session keeps
        // the `convert_dolby_vision: false` `into_request` built it with *and*
        // the client's echoed `preserve_dolby_vision`, unclamped. For a
        // dual-layer source that is raw Profile 7 on the wire, reachable from
        // any client that posts a document this build cannot read beside
        // `preserve_dolby_vision: true` — a v1 body, an empty one, or every
        // client in the fleet at once on the day `DeviceCaps::VERSION` moves
        // to 3.
        //
        // The earlier reasoning for `None` was that an unreadable document
        // "may well have enumerated Profile 7", so deriving a conversion for
        // it overrides an answer its own document might have given. That reads
        // the risk backwards. A document that cannot be parsed enumerated
        // nothing *this server can act on*, which is the same position a build
        // sending no document at all is in; and the two outcomes are not
        // symmetric. Guessing "convert" costs a client that really did declare
        // 7 its enhancement layer, and it still plays. Guessing "preserve"
        // hands dual-layer to everyone else, and that does not.
        Some(caps) => {
            plan_derivation::count_unusable_caps();
            tracing::warn!(
                file_id = log.file_id,
                client_build = %log.client_build,
                caps_version = caps.v,
                caps_empty = caps.is_empty(),
                "create could not read the caps document; trusting the client's echo"
            );
            Some(legacy_trusted_review(
                file,
                node,
                asked_preserve_dolby_vision,
                asked_hdr10,
            ))
        }
        None => {
            plan_derivation::count_legacy_trusted();
            tracing::warn!(
                file_id = log.file_id,
                client_build = %log.client_build,
                "create trusted the client's plan echo: this build sends no caps document"
            );
            // Trusting the echo is right for everything the client can
            // actually state. It is wrong for the conversion, which no client
            // has ever been able to ask for: `CreateSession` carries no such
            // field, so the echo says nothing and `SessionKind::Copy` keeps
            // the `false` it was built with. That silently downgrades a
            // *server* decision — `/decision` answers "Profile 7 converted to
            // Profile 8.1 for this device" and the session one second later
            // preserves raw Profile 7, which is the one delivery no consumer
            // decoder outside Blu-ray hardware takes. Observed in production:
            // Safari answered `stream_rejected ... browser refused the remux
            // stream`, and the fallback tonemapped the title to SDR.
            //
            // So this arm derives the one field the echo cannot carry, and
            // nothing else.
            Some(legacy_trusted_review(
                file,
                node,
                asked_preserve_dolby_vision,
                asked_hdr10,
            ))
        }
    }
}

/// How to name the build in a create log line.
///
/// The v2 document names itself (`client.kind`/`client.build`), which is the
/// whole point of it carrying a `client` block at all. A body without one
/// leaves the User-Agent, truncated: these lines are for counting builds, not
/// for fingerprinting devices, and an unbounded header does not belong in a
/// log a support request gets pasted into.
fn client_build_label(
    caps: Option<&plurx_core::playback::DeviceCaps>,
    headers: &HeaderMap,
) -> String {
    if let Some(client) = caps.and_then(|caps| caps.client.as_ref()) {
        let named = match (bound(&client.kind), bound(&client.build)) {
            (None, None) => None,
            (None, Some(build)) => Some(build),
            (Some(kind), None) => Some(kind),
            (Some(kind), Some(build)) => Some(format!("{kind}/{build}")),
        };
        if let Some(named) = named {
            return named;
        }
    }
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .and_then(bound)
        .unwrap_or_else(|| "unknown".to_owned())
}

/// A log-safe rendering of one caller-supplied identity string.
///
/// Control characters go first, then the length. The order matters: a body
/// field is as attacker-controlled as a header, and a newline inside it is
/// how one log line becomes two forged ones. `None` for anything that had no
/// printable content to begin with, so the caller can fall through to its
/// next source rather than logging an empty name.
fn bound(value: &str) -> Option<String> {
    let cleaned: String = value.chars().filter(|c| !c.is_control()).take(48).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

const HDR_SUBTITLE_BURN_REFUSAL: &str =
    "That subtitle requires an SDR burn-in. HDR playback was kept unchanged.";

/// Would building this session with its requested burn cost the viewer the
/// dynamic range they would otherwise be getting?
///
/// This asks [`plurx_core::playback::burn_would_discard_hdr`] — the one guard
/// — about the grade this exact request resolves to **with no burn**, which
/// is the only grade the question has an honest answer against.
///
/// It replaces a predicate that keyed on the *source* `hdr` column and
/// demanded a `subtitle_burn_sdr` acknowledgement from the client. That
/// failed closed on sessions which were already being tone-mapped: a display
/// sending `hdr=0` gets an SDR transcode, drawing a forced PGS track into it
/// takes nothing away, and the server refused it anyway because the *file*
/// was HDR. The web client never sent the acknowledgement at all and Apple
/// computed it from the wrong range, so in practice the only way past the
/// guard was to not be an HDR title.
///
/// The grade comes from [`crate::transcode::TranscodeManager::grade_preview`]
/// rather than `Decision::transcode_grade`: that field ignores encoder proof
/// and can say HDR10 where `start` will deliver SDR, which would judge the
/// request by a different function than the one that delivers it.
async fn burn_would_discard_this_session_hdr(
    state: &AppState,
    source: Option<&MediaFile>,
    request: &crate::transcode::SessionRequest,
    hdr10_requested: bool,
    height: i64,
) -> bool {
    let Some(file) = source else {
        // No source row: this request is on its way to a 404, and there is
        // nothing honest to say about a file we cannot see.
        return false;
    };
    if !request.subtitle_burn.is_some_and(|index| index >= 0) {
        return false;
    }
    let base_grade = state
        .transcode
        .grade_preview(file, hdr10_requested, height, None)
        .await;
    let (method, preserve, _) = session_delivery_shape(&request.kind);
    let base_range =
        plurx_core::playback::delivered_dynamic_range(file, method, preserve, base_grade);
    plurx_core::playback::burn_would_discard_hdr(base_range, true)
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
    let file = source?;
    let (method, preserve, _) = session_delivery_shape(kind);
    Some(plurx_core::playback::delivered_dynamic_range(
        file, method, preserve, grade,
    ))
}

/// The Dolby Vision profile this session's bytes carry, read off the session
/// it actually built — same rule, same reason, as the range beside it.
fn session_delivered_dolby_vision_profile(
    source: Option<&MediaFile>,
    kind: &crate::transcode::SessionKind,
) -> Option<u8> {
    let file = source?;
    let (method, preserve, convert) = session_delivery_shape(kind);
    plurx_core::playback::delivered_dolby_vision_profile(file, method, preserve, convert)
}

/// What a session kind means to the two badge helpers.
///
/// One reading for both, so the range and the profile can never disagree
/// about the same delivery — a session badged `dolby_vision` with no profile,
/// or a profile on a stream whose range says HDR10, is a worse answer than
/// either field alone.
fn session_delivery_shape(kind: &crate::transcode::SessionKind) -> (PlaybackMethod, bool, bool) {
    use crate::transcode::SessionKind;
    match kind {
        SessionKind::Copy {
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } => (
            PlaybackMethod::Remux,
            *preserve_dolby_vision,
            *convert_dolby_vision,
        ),
        SessionKind::Transcode { .. } => (PlaybackMethod::Transcode, false, false),
    }
}

/// POST /api/v1/files/:id/hls/sessions — create a stream, or recover the one
/// an identical request already created.
/// Whether a restart has been superseded by the destination the client has
/// since settled on.
///
/// One expression rather than a predicate spread through the handler, because
/// the handler is not reachable from a test: it needs a store, a transcode
/// manager and an authenticated user. Everything that decides the outcome
/// lives here, so a test on this function pins the decision rather than
/// leaving the call site free to compute the anchor differently.
///
/// `false` for every absence. No `control_sequence` means a client that does
/// not send one, which is every client before the field existed and every
/// first play since — absent means *do the work*. No settled target means no
/// session, a retired actor, or a client that has not exchanged yet, and in
/// all three there is nothing to order against.
fn restart_is_superseded(
    settled: Option<crate::playback_control::SettledTarget>,
    control_sequence: Option<u64>,
    start_seconds: f64,
) -> bool {
    let (Some(settled), Some(control_sequence)) = (settled, control_sequence) else {
        return false;
    };
    // A non-finite start is not a destination. Treating it as the head is the
    // conservative reading: it can only make this look *less* superseded, so a
    // malformed body cannot cancel a session the viewer still wants. The float
    // cast saturates rather than wrapping, so an absurd start clamps instead of
    // becoming a small anchor that would compare equal to a real one.
    let requested_anchor_ms = if start_seconds.is_finite() {
        (start_seconds.max(0.0) * 1_000.0) as i64
    } else {
        0
    };
    settled.supersedes(control_sequence, requested_anchor_ms)
}

/// What an admitted restart carries into its activation.
#[derive(Debug)]
struct RestartAdmission {
    expected_predecessor_incarnation_id: Option<String>,
    fence_predecessor: bool,
}

/// Refuse a restart whose destination the viewer has already left, and say how
/// the activation that follows an admitted one is fenced.
///
/// Both halves live here because both are part of one claim M7's acceptance
/// makes: a storm starts work only for the settled target, and each successor
/// that does start takes the playback pointer from the exact predecessor it
/// replaces. `create` needs a store, a transcode manager and an authenticated
/// user, so neither half is reachable from a test through the handler — and a
/// predicate returning `bool` would let the acceptance assert about the
/// decision without ever seeing the refusal a client receives or the fence its
/// successor carries. This returns both.
fn admit_restart(
    settled: Option<crate::playback_control::SettledTarget>,
    control_sequence: Option<u64>,
    start_seconds: f64,
    predecessor_incarnation_id: Option<&str>,
) -> Result<RestartAdmission, ApiError> {
    if restart_is_superseded(settled, control_sequence, start_seconds) {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "playback_target_superseded",
            "a later seek replaced this destination, so no session was started for it",
        ));
    }
    Ok(RestartAdmission {
        expected_predecessor_incarnation_id: predecessor_incarnation_id.map(str::to_owned),
        // Every activation is a predecessor CAS, an ordinary start included:
        // `Some` requires the playback pointer to name that exact incarnation
        // and `None` requires it to be absent, so a storm's losing restart can
        // never take the pointer from the successor that already has it.
        fence_predecessor: true,
    })
}

/// The height a request resolves to, which is not always the one asked for.
///
/// Three arms and three different promises, which is why this is one function
/// rather than three lines at a call site. Splitting it is how one of them
/// gets lost:
///
/// * **Auto is never snapped.** The server's own choice already lands where it
///   means to, and snapping would re-decide policy — a 900p source
///   deliberately transcodes at 900, with no scaler in the chain at all. This
///   is also the only arm that reads the network prior or the HDR10 ask.
/// * **The source's own height is a promise**, not a request: it is the
///   Original/forced-burn path the player reads as `sessionHeight`, and it is
///   never snapped and never downgraded.
/// * **An explicit rung from a menu snaps onto the ladder**, so a stray number
///   lands where the encoder has rungs — while an above-ladder height passes
///   through as what it is.
///
/// The clamp bounds the result and nothing else. It never binds downward,
/// because the snap has already put a below-ladder ask on the lowest rung; it
/// is the only thing bounding an above-ladder one.
///
/// Shared by `resolve_plan` and by M6's candidate, because it is the one part
/// of resolving a recipe that needs the store, the ladder ceiling and the
/// network prior — see
/// [M6-CALLER-HANDOFF.md](../../../../docs/playback-control/M6-CALLER-HANDOFF.md) §3.3 on why
/// the rest of `resolve_plan` is not what a candidate wants.
pub(crate) async fn resolve_height(
    state: &AppState,
    source: Option<&MediaFile>,
    network_prior: Option<&plurx_core::domain::NetworkPrior>,
    hdr10_requested: bool,
    asked: Option<i64>,
) -> i64 {
    let source_height = source.and_then(|f| f.height);
    match asked {
        None => {
            state
                .transcode
                .auto_height_for_request(source, network_prior, hdr10_requested)
                .await
        }
        Some(h) if Some(h) == source_height => h,
        Some(h) => crate::transcode::snap_height(h),
    }
    .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT)
}

/// One resolved plan: the recipe a create body would produce right now.
///
/// M6's preparation decision has to ask *"if this client's selection were
/// honoured, what would we deliver?"* without creating anything, and that is
/// the same question `create` answers on its way to admission. Answered once,
/// here, rather than twice in two places that agree today —
/// [M6-CALLER-HANDOFF.md](../../../../docs/playback-control/M6-CALLER-HANDOFF.md) §3.2. **Do
/// not grow a second resolver.** The drift would be invisible, because both
/// sides would look correct in isolation.
pub(crate) struct ResolvedPlan {
    pub request: crate::transcode::SessionRequest,
    /// The height this plan resolved to, which is not always the one asked
    /// for.
    pub height: i64,
    /// The client's intent **as sent**, fingerprinted before the plan review
    /// is applied.
    ///
    /// Taken inside this function rather than by the caller, because the order
    /// is the contract: a transport retry that lands on a node running a
    /// different binary — or after a rescan changed the file's HDR facts —
    /// must fingerprint identically and get its own session back rather than a
    /// 409. A caller taking it afterwards would take a different fingerprint
    /// and have no way to notice.
    pub intent_fingerprint: String,
    pub plan_notes: Vec<String>,
    /// The client asked for a native text track rather than a burn, and which
    /// one. Resolved here because it is validated here — an index that names
    /// no track, or one that needs burning in, is refused before a recipe
    /// exists.
    pub native_subtitles: bool,
    pub native_subtitle: Option<i64>,
}

pub(crate) struct PlanInputs<'a> {
    pub state: &'a AppState,
    pub user_id: i64,
    pub file_id: i64,
    pub source: Option<&'a MediaFile>,
    pub network_prior: Option<&'a plurx_core::domain::NetworkPrior>,
}

/// Resolve a create body into the recipe it would produce.
///
/// `review` is a parameter rather than derived here, on purpose: deriving it
/// counts `plan_derivation` metrics, and those measure *what create did with
/// the plan it was handed*. A second caller counting them would stop the
/// number that says the caps-v2 migration is finished from meaning that.
///
/// Everything derivable *from* the review is derived here rather than passed
/// alongside it. `hdr10_requested` was a parameter for one revision, and that
/// was the drift seam this whole extraction exists to close: a second caller
/// passing the body's own `hdr10` where the review had said `false` would
/// resolve an Auto height against a ceiling `create` would never have used,
/// and nothing — not the type system, not a test — would notice.
///
/// This deliberately does **not** answer the delivered dynamic range. That
/// grade comes from the pipeline the session gets at start, so a recipe that
/// will never be built has none — which is why M6 reads the grade axis off the
/// request instead, as `GradeIntent`. See §3.2.1 of the caller handoff for the
/// claim that was withdrawn here.
pub(crate) async fn resolve_plan(
    inputs: PlanInputs<'_>,
    review: Option<PlanReview>,
    body: CreateSession,
) -> Result<ResolvedPlan, ApiError> {
    let PlanInputs {
        state,
        user_id,
        file_id,
        source,
        network_prior,
    } = inputs;
    let hdr10_requested = review
        .as_ref()
        .map(|review| review.hdr10)
        .unwrap_or(body.hdr10 == Some(true));
    let height = resolve_height(state, source, network_prior, hdr10_requested, body.height).await;
    let native_subtitles = body.native_subtitles == Some(true);
    let native_subtitle = body.subtitle.filter(|s| *s >= 0);
    if native_subtitles {
        if let Some(index) = native_subtitle {
            let track = source
                .and_then(|f| f.subtitle_streams.get(index as usize))
                .ok_or_else(|| ApiError::BadRequest("unknown native subtitle track".into()))?;
            if !is_native_text_subtitle(&track.codec) {
                return Err(ApiError::BadRequest(
                    "the selected subtitle requires burn-in".into(),
                ));
            }
        }
    }
    let mut request = body.into_request(file_id, height);
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
    // The fingerprint is the client's intent as sent, which is what makes a
    // retry of the same body recover the same session no matter which binary
    // answers it. Only after it is taken does the server's reconciliation
    // apply to the request that will actually be built.
    let fingerprint = request.durable_intent_fingerprint(user_id);
    let plan_notes = match review {
        Some(review) => apply_plan_review(&mut request, review),
        None => Vec::new(),
    };
    Ok(ResolvedPlan {
        request,
        height,
        intent_fingerprint: fingerprint,
        plan_notes,
        native_subtitles,
        native_subtitle,
    })
}

/// A copy-video request is already choosing HLS, but an explicit progressive
/// sample-entry constraint still has to prove that HLS was claimed. Do this
/// before durable request admission so an incompatible plan cannot allocate a
/// session and only then discover that the client has no safe transport.
async fn validate_hevc_copy_transport(
    state: &AppState,
    source: &MediaFile,
    caps: &plurx_core::playback::DeviceCaps,
    request: &crate::transcode::SessionRequest,
) -> Result<(), ApiError> {
    let crate::transcode::SessionKind::Copy {
        preserve_dolby_vision,
        ..
    } = request.kind
    else {
        return Ok(());
    };
    if caps.progressive_hevc_sample_entries.is_none() {
        return Ok(());
    }
    let probe_json = state.store.get_file_probe_json(source.id).await?;
    let promotes =
        plurx_core::transcode::hevc_parameter_set_promotion_required(source, probe_json.as_deref());
    let Some(actual) =
        super::stream::progressive_hevc_output_tag(source, preserve_dolby_vision, promotes)
    else {
        return Ok(());
    };
    if !caps.transports.iter().any(|transport| transport == "hls") {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "unsupported_hevc_delivery",
            format!("HEVC copy-HLS output uses {actual}, but HLS was not claimed by this client"),
        ));
    }
    super::stream::hevc_copy_requires_hls(source, caps, preserve_dolby_vision, promotes)?;
    Ok(())
}

pub async fn create(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
    headers: HeaderMap,
    super::network::RemoteAddress(remote): super::network::RemoteAddress,
    Json(req): Json<CreateSession>,
) -> Result<Json<StartResponse>, ApiError> {
    create_with_purpose(user, state, id, headers, remote, req, None).await
}

/// The finite-session application service used by Library channels after it
/// has authoritatively resolved an occurrence. The purpose is not a public
/// field on ordinary VOD creation: old servers must reject the dedicated
/// route instead of silently turning a channel tune into history-writing VOD.
pub(crate) async fn create_for_library_channel(
    user: plurx_core::domain::User,
    state: AppState,
    file_id: i64,
    headers: HeaderMap,
    remote: Option<std::net::SocketAddr>,
    req: CreateSession,
    purpose: crate::http::library_channels::LibraryChannelPlaybackPurpose,
) -> Result<StartResponse, ApiError> {
    create_with_purpose(user, state, file_id, headers, remote, req, Some(purpose))
        .await
        .map(|Json(response)| response)
}

async fn create_with_purpose(
    user: plurx_core::domain::User,
    state: AppState,
    id: i64,
    headers: HeaderMap,
    remote: Option<std::net::SocketAddr>,
    req: CreateSession,
    library_channel: Option<crate::http::library_channels::LibraryChannelPlaybackPurpose>,
) -> Result<Json<StartResponse>, ApiError> {
    if let Some(caps) = req.caps.as_ref() {
        super::stream::validate_device_caps(caps)?;
    }
    let planning_caps = req
        .caps
        .as_ref()
        .filter(|caps| caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty())
        .cloned();
    let planning_overrides = planning_caps.as_ref().and(req.overrides.as_ref()).cloned();
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
    // The ask is made durable before anything is built for it, and before this
    // request can be answered.
    //
    // Placed here, above every other decision, rather than beside the response:
    // a create that goes on to fail is still a viewer who asked, and the row
    // records the asking, not the outcome. The order that matters is the one
    // §1 names — canonical desired ownership persisted *before* an
    // intent-changing request is reported accepted — and answering first would
    // tell a client its selection was taken while nothing anywhere had recorded
    // it. A restart or an owner change in that window loses the ask entirely.
    //
    // A failed write refuses the create for the same reason the control
    // exchange refuses: a retry costs one request, and a phantom acceptance
    // costs a viewer their selection with nothing to point at.
    // `None` for a body that carried no ask: a playback with nothing recorded
    // must still activate, or the first play of every title would be refused.
    let mut recorded_ask_revision: Option<i64> = None;
    if let Some(intent) = req.intent.as_ref() {
        intent
            .validate()
            .map_err(|error| ApiError::BadRequest(format!("intent: {error}")))?;
        let selection = intent.selection;
        recorded_ask_revision = Some(
            state
                .store
                .record_desired_selection(
                    user.id,
                    &req.playback_id,
                    &intent.digest(),
                    &selection.canonical_form(),
                    unix_ms(),
                )
                .await
                .map_err(|error| {
                    tracing::warn!(
                        playback = %req.playback_id,
                        "recording the viewer's selection on create failed: {error}"
                    );
                    ApiError::ServiceUnavailable(
                        "this selection could not be recorded; retry shortly".to_owned(),
                    )
                })?
                .revision,
        );
        #[cfg(test)]
        {
            let pause = {
                let mut slot = CREATE_ASK_RECORDED_PAUSE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                slot.take()
            };
            if let Some((reached, release)) = pause {
                let _ = reached.send(());
                let _ = release.await;
            }
        }
    }
    let ingress_serving_authority = state.serving.authority();
    let ingress_serving_generation = ingress_serving_authority.admit().ok_or_else(|| {
        ApiError::ServiceUnavailable("the ingress node has no serving authority".to_owned())
    })?;
    // The source height answers three things now: Auto, the ladder in the
    // response, and the snap's source-height escape. One read, from the read
    // pool.
    let source = state
        .store
        .get_file(id)
        .await
        .map_err(|error| session_store_error("reading the source file", error))?;
    // The HDR subtitle-burn guard used to stand here, keyed on the source's
    // own `hdr` column. It now runs after `resolve_plan`, against the grade
    // this request actually resolves to — see
    // `burn_would_discard_this_session_hdr`. Nothing between here and there
    // opens an encoder or takes a durable admission row: the ladder ceiling,
    // the network prior, `resolve_plan` and `validate_hevc_copy_transport`
    // are reads, and `claim_media_session_request` is the first write, after
    // it. `plan_review_for` is the one exception and it is not a write
    // either — it bumps the process-lifetime caps-migration counters, so a
    // create that this guard now refuses is counted as a straggler where
    // before it was refused first. That is a metric inflating slightly, not
    // state being left behind.
    //
    // `subtitle_burn_sdr` stays on the wire for clients that still send it,
    // and is no longer load-bearing. It was an acknowledgement the web client
    // never sent and Apple computed from the wrong range, so consulting it
    // decided nothing except which clients could burn at all.
    if req.subtitle_burn_sdr.is_some() {
        tracing::debug!(
            file_id = id,
            subtitle_burn = req.subtitle_burn,
            subtitle_burn_sdr = req.subtitle_burn_sdr,
            "client sent the legacy SDR burn acknowledgement; the session's own grade decides"
        );
    }
    // Whose build this is, for every line below. The v2 document names
    // itself; a client that sends none leaves only its User-Agent, which is
    // exactly the population these lines exist to count down to zero.
    let client_build = client_build_label(req.caps.as_ref(), &headers);
    // E4: the client's echo stops being an instruction the moment it sends
    // the capabilities the plan was derived from. Everything below reads the
    // reconciled values, so there is exactly one decider again.
    // The reconciled plan, and the notes that explain it. `req` is left
    // untouched on purpose: the durable intent fingerprint below has to be a
    // pure function of the body as the client sent it, or a transport retry
    // that lands on a node running a different binary (or after a rescan
    // changed the file's HDR facts) fingerprints differently and gets a 409
    // where it should have got its own session back. The reconciled values
    // are applied to the built request afterwards.
    let review = match source.as_ref() {
        // No source row yet. This request is on its way to a 404; re-deriving
        // a plan for a file that is not there would say nothing, and counting
        // it would let any client hold the straggler metric off zero forever.
        // This is the one arm that stays here, because it is the only one that
        // must not ask the node what it can render.
        None => None,
        Some(file) => plan_review_for(
            req.caps.as_ref(),
            req.overrides.as_ref(),
            file,
            &super::stream::render_caps(&state).await,
            req.preserve_dolby_vision == Some(true),
            req.hdr10 == Some(true),
            unix_ms(),
            &ReviewContext {
                file_id: id,
                user_id: user.id,
                client_build: &client_build,
            },
        ),
    };
    let hdr10_requested = review
        .as_ref()
        .map(|review| review.hdr10)
        .unwrap_or(req.hdr10 == Some(true));
    let source_height = source.as_ref().and_then(|f| f.height);
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
    let network_prior = super::network::stored_prior(state.store.as_ref(), identity.as_ref())
        .await
        .map_err(|error| {
            ApiError::ServiceUnavailable(format!("reading the network prior: {error:?}"))
        })?;
    let resolved = resolve_plan(
        PlanInputs {
            state: &state,
            user_id: user.id,
            file_id: id,
            source: source.as_ref(),
            network_prior: network_prior.as_ref(),
        },
        review,
        req,
    )
    .await?;
    let request = resolved.request;
    if let (Some(source), Some(caps)) = (source.as_ref(), planning_caps.as_ref()) {
        validate_hevc_copy_transport(&state, source, caps, &request).await?;
    }
    let height = resolved.height;
    if burn_would_discard_this_session_hdr(
        &state,
        source.as_ref(),
        &request,
        hdr10_requested,
        height,
    )
    .await
    {
        return Err(ApiError::Unprocessable(serde_json::json!({
            "code": "hdr_subtitle_burn_refused",
            "error": HDR_SUBTITLE_BURN_REFUSAL,
        })));
    }
    let fingerprint = match library_channel.as_ref() {
        Some(purpose) => purpose.bind_session_fingerprint(&resolved.intent_fingerprint),
        None => resolved.intent_fingerprint,
    };
    let plan_notes = resolved.plan_notes;
    let native_subtitles = resolved.native_subtitles;
    let native_subtitle = resolved.native_subtitle;
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
        .await
        .map_err(|error| session_store_error("claiming the media-session request", error))?
    {
        MediaSessionRequestClaim::Acquired {
            incarnation_id: acquired,
        } => incarnation_id = acquired,
        MediaSessionRequestClaim::Resolved(route) if resolved_replay_is_live(&route, unix_ms()) => {
            let replay_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
            let _response_publication = ingress_serving_authority
                .commit_guard_before(ingress_serving_generation, replay_deadline.into_std())
                .await
                .ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        "the ingress lost serving authority before resolved replay".to_owned(),
                    )
                })?;
            let route = tokio::time::timeout_at(
                replay_deadline,
                state
                    .store
                    .media_session_route_by_incarnation(&route.incarnation_id),
            )
            .await
            .map_err(|_| {
                ApiError::ServiceUnavailable(
                    "resolved media-session replay freshness check timed out".to_owned(),
                )
            })?
            .map_err(|error| session_store_error("rechecking the resolved replay", error))?
            .filter(|current| {
                replay_route_identity_matches(current, &route)
                    && resolved_replay_is_live(current, unix_ms())
            })
            .ok_or_else(|| {
                ApiError::ServiceUnavailable(
                    "the resolved media-session replay is no longer current".to_owned(),
                )
            })?;
            let mut response = serde_json::from_str::<StartResponse>(&route.response_json)?;
            // Same-session owner takeover advances only the control epoch.
            // Replaying the persisted create must not hand a restarted client
            // the stale epoch embedded when owner 1 first activated.
            if let Some(control) = response.control.as_ref() {
                response.control =
                    control.refreshed(&route.session_id, &route.incarnation_id, route.owner_epoch);
            }
            return Ok(Json(response));
        }
        MediaSessionRequestClaim::Resolved(route)
            if route.state == "active" && route.publication_ready_at_ms != 0 =>
        {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "media_session_handoff_pending",
                "the predecessor owner has not completed session handoff yet; retry shortly",
            ));
        }
        MediaSessionRequestClaim::Resolved(_) => {
            return Err(ApiError::typed(
                StatusCode::GONE,
                "media_session_ended",
                "this idempotent session was already released",
            ));
        }
        MediaSessionRequestClaim::InFlight {
            incarnation_id: in_flight_incarnation,
            ..
        } => {
            let observed_at_ms = unix_ms();
            let route = tokio::time::timeout(
                ACTIVATION_STORE_DEADLINE,
                state
                    .store
                    .media_session_route_by_incarnation(&in_flight_incarnation),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .filter(|route| {
                route.user_id == user.id
                    && route.playback_id == request.playback_id
                    && route.request_fingerprint == fingerprint
                    && route.state == "active"
                    && route.publication_ready_at_ms == 0
                    && route.lease_expires_at_ms > observed_at_ms
            });
            if let Some(route) = route {
                let publication_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
                let _response_publication = ingress_serving_authority
                    .commit_guard_before(
                        ingress_serving_generation,
                        publication_deadline.into_std(),
                    )
                    .await
                    .ok_or_else(|| {
                        ApiError::ServiceUnavailable(
                            "the ingress lost serving authority before replay publication"
                                .to_owned(),
                        )
                    })?;
                let route = tokio::time::timeout_at(
                    publication_deadline,
                    state.store.publish_media_session_activation(
                        user.id,
                        &request_claim_id,
                        &in_flight_incarnation,
                        unix_ms(),
                    ),
                )
                .await
                .map_err(|_| {
                    ApiError::ServiceUnavailable(
                        "media-session replay publication exceeded its fixed deadline".to_owned(),
                    )
                })?
                .map_err(|error| session_store_error("publishing the session replay", error))?
                .filter(|published| replay_publication_matches(published, &route))
                .ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        "the media-session replay lost its exact activation".to_owned(),
                    )
                })?;
                state.media_sessions.cache_route(route.clone()).await;
                if route.owner_node_id == state.node_id {
                    state.media_sessions.seed_owned_lease(&route).await;
                }
                let mut response = serde_json::from_str::<StartResponse>(&route.response_json)?;
                if let Some(control) = response.control.as_ref() {
                    response.control = control.refreshed(
                        &route.session_id,
                        &route.incarnation_id,
                        route.owner_epoch,
                    );
                }
                return Ok(Json(response));
            }
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
    // From this point every early return must settle the exact durable claim.
    // Once worker placement returns, its StartedSessionGuard takes over the
    // same responsibility together with exact worker ownership.
    let mut request_guard = MediaSessionRequestGuard::new(
        state.clone(),
        user.id,
        request_claim_id.clone(),
        incarnation_id.clone(),
    );

    let advertise_control = plurx_core::store::stored_switch(
        state
            .store
            .get_setting(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1)
            .await
            .map_err(|error| session_store_error("reading the control protocol setting", error))?
            .as_deref(),
        true,
    );

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
        library_channel: library_channel.as_ref().map(|purpose| {
            serde_json::to_value(purpose).expect("bounded Library-channel purpose serializes")
        }),
        request: worker_request,
    };
    let recipe_json = serde_json::to_string(&remote_request)?;
    if library_channel.is_some()
        && !state
            .store
            .record_library_channel_session_recipe(
                user.id,
                &request_claim_id,
                &incarnation_id,
                &recipe_json,
                unix_ms(),
            )
            .await
            .map_err(|error| session_store_error("recording the channel session purpose", error))?
    {
        return Err(ApiError::ServiceUnavailable(
            "the channel session purpose could not be recorded; retry shortly".to_owned(),
        ));
    }
    let placement_deadline = super::peer_transport::deadline_after(START_DEADLINE);

    // Every activation is a predecessor CAS, including an ordinary start.
    // Capturing the exact route before worker placement gives a
    // commit-unknown reconciler the identity it must terminalize; allowing an
    // unfenced last-writer-wins activation loses that identity after the
    // pointer moves to the successor.
    let activation_predecessor = state
        .store
        .media_session_route_for_playback(user.id, &request.playback_id)
        .await
        .map_err(|error| session_store_error("reading the predecessor route", error))?;
    // One mint for this start, bound here rather than called twice.
    //
    // `recovery_epoch_for` is not a pure function: with no predecessor it
    // draws a fresh UUID, which is the whole point of it — a deliberate new
    // play is a new budget, and `recovery_epoch_for`'s own test asserts that
    // two calls with `None` differ. So calling it once for the session and
    // again for the activation would give a new play two epochs: the live
    // session would believe in one that exists in no durable row, and the
    // next continuation would inherit the other and find it unspent. That is
    // one automatic recovery per attempt against a file that cannot be
    // decoded, which is the loop the budget exists to close.
    //
    // Bound above placement because the session that carries it is built by
    // the task the placement loop spawns, 268 lines before the activation
    // literal is reached. The only input is the predecessor route, already
    // read above, so this costs one binding and no store read.
    let recovery_epoch = recovery_epoch_for(activation_predecessor.as_ref());
    if activation_predecessor
        .as_ref()
        .is_some_and(|route| route.state == "active" && route.publication_ready_at_ms != 0)
    {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "media_session_handoff_pending",
            "the current session is still completing its predecessor handoff; retry shortly",
        ));
    }
    // A seek storm asks for one session per destination. The control actor
    // already knows which destination the client settled on, and the sequence
    // on this request says where in that ordering this ask belongs -- so a
    // restart for a target the viewer has since scrolled past can be refused
    // before it spawns a producer.
    //
    // Refused, not deferred: waiting to see whether a newer exchange arrives
    // would add latency to every honest seek, and the ordering is already
    // decided. Only a strictly later exchange naming a different destination
    // supersedes, so the honest seek arriving next is never the one skipped.
    let settled_target = match activation_predecessor.as_ref() {
        // No predecessor is a first play: there is no ordering for it to be
        // stale against, and no session to ask.
        Some(predecessor) => {
            state
                .transcode
                .settled_target_for_session(&predecessor.session_id)
                .await
        }
        None => None,
    };
    let admission = admit_restart(
        settled_target,
        request.control_sequence,
        request.start_seconds,
        activation_predecessor
            .as_ref()
            .map(|route| route.incarnation_id.as_str()),
    )?;
    let RestartAdmission {
        expected_predecessor_incarnation_id,
        fence_predecessor,
    } = admission;
    let pinned_owner = if let Some(previous_session_id) = request
        .previous_session_id
        .as_deref()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
    {
        if let Some(route) = activation_predecessor.as_ref() {
            if route.session_id != previous_session_id
                || route.user_id != user.id
                || route.state != "active"
                || route.lease_expires_at_ms <= unix_ms()
            {
                return Err(ApiError::typed(
                    StatusCode::CONFLICT,
                    "media_session_superseded",
                    "the session being reopened is no longer current",
                ));
            }
            Some(route.owner_node_id.clone())
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
            if !ingress_serving_authority.is_current(ingress_serving_generation) {
                last_error = Some(ApiError::ServiceUnavailable(
                    "the local media worker has no serving authority".to_owned(),
                ));
                continue;
            }
            let admitted_serving_generation = ingress_serving_generation;
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
            let worker_recovery = crate::transcode::SessionRecoveryIdentity {
                user_id: user.id,
                incarnation_id: incarnation_id.clone(),
                recovery_epoch: recovery_epoch.clone(),
            };
            let worker_serving_authority = ingress_serving_authority.clone();
            let mut start_task = tokio::spawn(async move {
                let started = transcode
                    .create_cluster_session(
                        &worker_request,
                        &worker_recovery,
                        &user_name,
                        placement_deadline,
                        admitted_serving_generation,
                    )
                    .await?;
                let crate::transcode::ClusterSessionStart {
                    info,
                    replacement,
                    created,
                } = started;
                let session_id = info.session_id.clone();
                let response = RemoteStartResponse::from(info);
                let guard = Some(if created {
                    StartedSessionGuard::new(
                        guard_state,
                        guard_owner,
                        guard_incarnation,
                        session_id,
                        guard_user,
                        guard_request,
                        Some(replacement),
                    )
                } else {
                    StartedSessionGuard::recovered(
                        guard_state,
                        guard_owner,
                        guard_incarnation,
                        session_id,
                        guard_user,
                        guard_request,
                        Some(replacement),
                    )
                });
                if !worker_serving_authority.is_current(admitted_serving_generation) {
                    return Err(crate::transcode::serving_fence_error(
                        "the local worker lost authority before shared-cache pinning",
                    ));
                }
                Ok((response, guard, Some(admitted_serving_generation)))
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
            match state
                .media_sessions
                .start_remote(&candidate, &remote_request, placement_deadline)
                .await
            {
                Ok(started) => {
                    let target_generation = started.info.activation_generation;
                    let guard = match started.ownership {
                        crate::media_sessions::RemoteSessionStartOwnership::Created
                        | crate::media_sessions::RemoteSessionStartOwnership::Recovered => {
                            Some(StartedSessionGuard::claim_only(
                                state.clone(),
                                candidate.clone(),
                                incarnation_id.clone(),
                                started.info.session_id.clone(),
                                user.id,
                                request_claim_id.clone(),
                            ))
                        }
                        crate::media_sessions::RemoteSessionStartOwnership::LegacyAmbiguous => {
                            return Err(ApiError::ServiceUnavailable(format!(
                                "media worker {candidate} does not support safe activation ownership"
                            )));
                        }
                    };
                    let Some(target_generation) = target_generation else {
                        return Err(ApiError::ServiceUnavailable(format!(
                            "media worker {candidate} did not return an activation generation"
                        )));
                    };
                    Ok((started.info, guard, Some(target_generation)))
                }
                Err(error) => Err(ApiError::ServiceUnavailable(format!(
                    "media worker {candidate} could not start the session: {error:?}"
                ))),
            }
        };
        match result {
            Ok((info, guard, serving_generation)) if info.is_valid() => {
                started = Some((candidate, info, guard, serving_generation));
                break;
            }
            Ok((_info, _guard, _serving_generation)) => {
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
    let Some((owner_node_id, info, guard, local_serving_generation)) = started else {
        return Err(last_error.unwrap_or_else(|| {
            ApiError::ServiceUnavailable("no eligible media worker was available".to_owned())
        }));
    };
    request_guard.disarm();
    if !ingress_serving_authority.is_current(ingress_serving_generation) {
        return Err(ApiError::ServiceUnavailable(
            "the ingress node lost serving authority during worker placement".to_owned(),
        ));
    }
    if owner_node_id == state.node_id {
        let provisional_pin_ms =
            i64::try_from(REMOTE_ACTIVATION_CONFIRMATION_WINDOW.as_millis()).unwrap_or(i64::MAX);
        if !pin_shared_session_for_local_start(
            placement_deadline,
            state.transcode.pin_shared_session(
                &info.session_id,
                &incarnation_id,
                1,
                unix_ms().saturating_add(provisional_pin_ms),
            ),
        )
        .await?
        {
            return Err(ApiError::ServiceUnavailable(
                "shared cache generation changed before session activation".to_owned(),
            ));
        }
        if owner_node_id == state.node_id
            && local_serving_generation
                .is_some_and(|generation| !ingress_serving_authority.is_current(generation))
        {
            return Err(ApiError::ServiceUnavailable(
                "the local media worker lost serving authority before activation".to_owned(),
            ));
        }
    }
    if !ingress_serving_authority.is_current(ingress_serving_generation) {
        return Err(ApiError::ServiceUnavailable(
            "the ingress node lost serving authority before owner assignment".to_owned(),
        ));
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
            return Err(session_store_error(
                "assigning the media-session owner",
                error,
            ));
        }
        Err(_) => {
            return Err(ApiError::ServiceUnavailable(
                "session ownership assignment timed out".to_owned(),
            ));
        }
    }
    if !ingress_serving_authority.is_current(ingress_serving_generation) {
        return Err(ApiError::ServiceUnavailable(
            "the ingress node lost serving authority before session activation".to_owned(),
        ));
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
        delivered_dolby_vision_profile: session_delivered_dolby_vision_profile(
            source.as_ref(),
            &info.kind,
        ),
        control: advertise_control.then(|| {
            crate::playback_control::ControlBootstrap::new(
                &info.session_id,
                &incarnation_id,
                1,
                info.control_lease_timeout_ms,
            )
            .expect("new media-session owner epochs begin at one")
        }),
        plan_notes,
    };
    // `response_json` is the canonical durable session recipe/response. Keep
    // channel purpose in that row so restart and owner takeover cannot turn a
    // following session into ordinary VOD. `StartResponse` deliberately
    // ignores additive fields, preserving every existing recovery reader.
    let response_json =
        if library_channel.is_some() || planning_caps.is_some() || planning_overrides.is_some() {
            let mut value = serde_json::to_value(&response)?;
            let object = value
                .as_object_mut()
                .expect("StartResponse serializes as an object");
            if let Some(purpose) = library_channel.as_ref() {
                object.insert("library_channel".to_owned(), serde_json::to_value(purpose)?);
            }
            if let Some(caps) = planning_caps.as_ref() {
                object.insert(
                    PLANNING_CAPS_RESPONSE_FIELD.to_owned(),
                    serde_json::to_value(caps)?,
                );
            }
            if let Some(overrides) = planning_overrides.as_ref() {
                object.insert(
                    PLANNING_OVERRIDES_RESPONSE_FIELD.to_owned(),
                    serde_json::to_value(overrides)?,
                );
            }
            serde_json::to_string(&value)?
        } else {
            serde_json::to_string(&response)?
        };
    let activation_now_ms = unix_ms();
    let activation = MediaSessionActivation {
        // The one mint for this start, from the local bound above placement.
        //
        // This used to call `recovery_epoch_for` here, with a comment saying a
        // local copy could be believed after the store had ruled otherwise.
        // That reasoning is about a *read*: the store decides the row's epoch,
        // an activation that loses a race keeps whatever the winning row had,
        // and `activation_route_matches` does not compare this field — all
        // still true, and all reasons to trust the route the store hands back
        // rather than this value. It was never a reason to mint twice, and
        // minting twice is what calling it here a second time now does, since
        // the session start above needs the same epoch this row is about to
        // be given.
        recovery_epoch: recovery_epoch.clone(),
        // The revision this create recorded, not the one that is current now.
        //
        // Re-reading here would defeat the compare: a control exchange that
        // advanced the ask while this create was in flight would be read back
        // as the expectation, and the pointer would advance to a session built
        // for the selection the viewer has left. What this activation was
        // decided against is what create wrote at its very first step.
        expected_desired_revision: recorded_ask_revision,
        incarnation_id: incarnation_id.clone(),
        session_id: info.session_id.clone(),
        user_id: user.id,
        playback_id: request.playback_id.clone(),
        expected_predecessor_incarnation_id: expected_predecessor_incarnation_id.clone(),
        fence_predecessor,
        request_id: Some(request_claim_id.clone()),
        request_fingerprint: fingerprint,
        owner_node_id: owner_node_id.clone(),
        lease_expires_at_ms: activation_now_ms.saturating_add(LEASE_TTL_MS),
        recipe_json,
        response_json,
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: (info.media_origin_seconds * 1_000.0).round() as i64,
        now_ms: activation_now_ms,
    };
    let publication_activation = activation.clone();
    let mut publication_guard =
        ActivationPublicationGuard::new(state.clone(), publication_activation.clone());
    // The detached owner keeps both serving serialization and exact worker
    // cleanup until the route is publishable or its fixed activation lease
    // expires. The Store future itself may outlive its caller, so every first
    // commit is BLOCKED and cannot be renewed or taken over before this owner
    // confirms it.
    let owner_activation_generation = local_serving_generation.ok_or_else(|| {
        ApiError::ServiceUnavailable("media worker omitted its activation authority".to_owned())
    })?;
    let mut activation_task = if owner_node_id == state.node_id {
        let activation_state = state.clone();
        let activation_authority = ingress_serving_authority.clone();
        tokio::spawn(async move {
            activate_session_under_authority(
                activation_state,
                activation,
                activation_predecessor,
                expected_predecessor_incarnation_id,
                activation_authority,
                owner_activation_generation,
                guard,
            )
            .await
            .map(|_| ())
        })
    } else {
        let activation_state = state.clone();
        let activation_owner = owner_node_id.clone();
        tokio::spawn(async move {
            let request = crate::media_sessions::RemoteActivateRequest {
                target_generation: owner_activation_generation,
                activation,
            };
            let activation_deadline = tokio::time::Instant::now()
                + Duration::from_millis(u64::try_from(LEASE_TTL_MS).unwrap_or_default());
            activation_state
                .media_sessions
                .activate_remote(&activation_owner, &request, activation_deadline)
                .await
                .map_err(|error| {
                    ApiError::ServiceUnavailable(format!(
                        "media worker {activation_owner} could not activate the session: {error:?}"
                    ))
                })?;
            let mut guard = guard;
            let route = match wait_for_publishable_activation(
                &activation_state,
                &request.activation,
                activation_deadline,
            )
            .await
            {
                PublishableActivationWait::Ready(route) => *route,
                PublishableActivationWait::Pending => {
                    if let Some(guard) = guard.as_mut() {
                        guard.disarm();
                    }
                    return Err(ApiError::typed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "media_session_handoff_pending",
                        "the predecessor handoff remains durably owned; retry this request",
                    ));
                }
                PublishableActivationWait::Gone => return Err(ApiError::ServiceUnavailable(format!(
                    "media worker {activation_owner} did not confirm the session before its owner lease"
                ))),
            };
            activation_state.media_sessions.cache_route(route).await;
            if let Some(guard) = guard.as_mut() {
                guard.disarm();
            }
            Ok(())
        })
    };
    let activation_deadline = activation_lease_deadline(&publication_activation);
    match tokio::time::timeout_at(activation_deadline, &mut activation_task).await {
        Ok(Ok(Ok(_))) => {}
        Ok(Ok(Err(error))) => {
            if matches!(
                &error,
                ApiError::Typed {
                    code: "media_session_handoff_pending",
                    ..
                }
            ) {
                publication_guard.disarm();
            }
            return Err(error);
        }
        Ok(Err(error)) => {
            return Err(ApiError::Internal(format!(
                "media activation task failed: {error}"
            )))
        }
        Err(_) => {
            // The HTTP deadline never cancels either ownership decision. The
            // detached activation keeps its worker guard; this publication
            // guard abandons a completed-but-unpublished route, but disarms
            // when the durable predecessor handoff is intentionally pending.
            tokio::spawn(async move {
                match activation_task.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error))
                        if matches!(
                            &error,
                            ApiError::Typed {
                                code: "media_session_handoff_pending",
                                ..
                            }
                        ) =>
                    {
                        publication_guard.disarm();
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(?error, "detached media activation settled unsuccessfully");
                    }
                    Err(error) => {
                        tracing::error!(%error, "detached media activation task failed");
                    }
                }
            });
            return Err(ApiError::ServiceUnavailable(
                "session activation is still being reconciled".to_owned(),
            ));
        }
    }
    // The selected target owns and authority-fences the durable activation.
    // The ingress needs only to linearize publication of this successful
    // response against its own loss transition; retaining an ingress read
    // guard across remote RPC/polling would delay failover for the whole owner
    // lease. A retry through a healthy ingress can publish the starting claim if
    // this exact ingress loses authority before response publication.
    let publication_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
    let _response_publication = ingress_serving_authority
        .commit_guard_before(ingress_serving_generation, publication_deadline.into_std())
        .await
        .ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "the ingress lost serving authority before start response publication".to_owned(),
            )
        })?;
    let published_route = tokio::time::timeout_at(
        publication_deadline,
        state.store.publish_media_session_activation(
            user.id,
            &request_claim_id,
            &incarnation_id,
            unix_ms(),
        ),
    )
    .await
    .map_err(|_| {
        ApiError::ServiceUnavailable(
            "media-session response publication exceeded its fixed deadline".to_owned(),
        )
    })?
    .map_err(|error| session_store_error("publishing the media-session response", error))?
    .filter(|route| {
        route_matches_activation(route, &publication_activation)
            && route.publication_ready_at_ms == 0
    })
    .ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "media-session response publication lost its exact activation".to_owned(),
        )
    })?;
    state
        .media_sessions
        .cache_route(published_route.clone())
        .await;
    if published_route.owner_node_id == state.node_id {
        state
            .media_sessions
            .seed_owned_lease(&published_route)
            .await;
    }
    if library_channel.is_none() {
        crate::playstart::note_playback_started(
            &state,
            user.id,
            &user.username,
            id,
            method,
            Some(&request.playback_id),
        );
    }
    publication_guard.disarm();
    Ok(Json(response))
}

fn resolved_replay_is_live(route: &MediaSessionRoute, now_ms: i64) -> bool {
    route.state == "active"
        && route.publication_ready_at_ms == 0
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
        && route.owner_epoch == 1
        && route.state == "active"
        && route.lease_expires_at_ms > unix_ms()
        && route.recipe_json == activation.recipe_json
        && route.response_json == activation.response_json
}

fn replay_publication_matches(published: &MediaSessionRoute, observed: &MediaSessionRoute) -> bool {
    replay_route_identity_matches(published, observed)
        && published.state == "active"
        && published.publication_ready_at_ms == 0
        && published.lease_expires_at_ms > unix_ms()
}

fn replay_route_identity_matches(
    current: &MediaSessionRoute,
    observed: &MediaSessionRoute,
) -> bool {
    current.incarnation_id == observed.incarnation_id
        && current.session_id == observed.session_id
        && current.user_id == observed.user_id
        && current.playback_id == observed.playback_id
        && current.request_fingerprint == observed.request_fingerprint
        && current.recipe_json == observed.recipe_json
        && current.response_json == observed.response_json
        && current.media_origin_ms == observed.media_origin_ms
}

/// Lease time held back from a retried activation confirmation so the
/// committed-write recovery read always has budget.
///
/// The bounded idempotent-write retry can spend the entire remaining owner
/// lease on consensus timeouts. Spending it here is what
/// [`wait_for_confirmed_activation`] exists to survive, and that reader is
/// entered with the same lease deadline: without a reservation it observes
/// `now >= deadline`, returns `None` on its first statement, and abandons an
/// activation whose confirmation had in fact committed.
///
/// This is one complete consistent-read *attempt*, not a whole read: an
/// authority read that times out retries once more inside the Store, and
/// reserving that full envelope would leave the write itself under two
/// attempts. The reservation is therefore a target rather than a floor -- see
/// [`activation_confirmation_deadline`], which never takes more than half of
/// whatever lease is actually left, so a confirmation entered late keeps a
/// proportional share for recovery instead of losing the reservation entirely.
const ACTIVATION_CONFIRMATION_RECOVERY_MARGIN: Duration = ACTIVATION_STORE_DEADLINE;

const _: () = assert!(
    ACTIVATION_CONFIRMATION_RECOVERY_MARGIN.as_millis() * 2 <= LEASE_TTL_MS as u128,
    "the confirmation recovery reservation must leave most of the owner lease for the write"
);

/// Split the remaining owner lease between the confirmation write and the
/// recovery read that has to survive it.
///
/// Taking the margin only when the full margin fits is what an earlier cut of
/// this did, and it removed the reservation in exactly the case that needs it:
/// a preparation that ran long leaves under a margin of lease, the confirmation
/// takes all of it, and the recovery read is entered already expired. Half of
/// what is left is always available, so the reservation degrades instead.
fn activation_confirmation_deadline(lease_deadline: tokio::time::Instant) -> tokio::time::Instant {
    let remaining = lease_deadline.saturating_duration_since(tokio::time::Instant::now());
    lease_deadline - ACTIVATION_CONFIRMATION_RECOVERY_MARGIN.min(remaining / 2)
}

fn activation_lease_deadline(activation: &MediaSessionActivation) -> tokio::time::Instant {
    let remaining_ms = activation
        .lease_expires_at_ms
        .saturating_sub(activation.now_ms)
        .clamp(0, LEASE_TTL_MS);
    tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or_default())
}

fn spawn_activation_abandonment(state: AppState, activation: MediaSessionActivation) {
    tokio::spawn(async move {
        if let Err(error) = state
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Abandon,
                unix_ms(),
            )
            .await
        {
            tracing::warn!(%error, "provisional media activation abandonment failed");
        }
    });
}

async fn settle_activation_publication_cleanup(
    state: AppState,
    activation: MediaSessionActivation,
) {
    let remaining_ms = activation
        .lease_expires_at_ms
        .saturating_sub(unix_ms())
        .clamp(0, LEASE_TTL_MS);
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or_default());
    let mut settlement_attempt = 0_u64;
    loop {
        settlement_attempt = settlement_attempt.saturating_add(1);
        let now = tokio::time::Instant::now();
        let attempt_deadline = now + ACTIVATION_STORE_DEADLINE;
        match tokio::time::timeout_at(
            attempt_deadline,
            state.store.settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Abandon,
                unix_ms(),
            ),
        )
        .await
        {
            // Atomic response publication won the race. Preserve its worker;
            // an idempotent retry is now entitled to recover this route.
            Ok(Ok(Some(route)))
                if route.state == "active"
                    && route.publication_ready_at_ms == 0
                    && route.lease_expires_at_ms > unix_ms() =>
            {
                return;
            }
            // Abandonment won, the route disappeared, or ownership advanced.
            // Exact worker cleanup is idempotent in all three cases.
            Ok(Ok(_)) => {
                abort_started_session(
                    &state,
                    &activation.owner_node_id,
                    &activation.incarnation_id,
                    &activation.session_id,
                )
                .await;
                return;
            }
            Ok(Err(error)) => {
                if settlement_attempt == 1 || settlement_attempt.is_multiple_of(15) {
                    tracing::warn!(
                        %error,
                        settlement_attempt,
                        "activation response reaper is retrying exact settlement"
                    );
                }
            }
            Err(_) => {
                if settlement_attempt == 1 || settlement_attempt.is_multiple_of(15) {
                    tracing::warn!(
                        settlement_attempt,
                        "activation response reaper timed out exact settlement"
                    );
                }
            }
        }
        let retry_delay = if tokio::time::Instant::now() < deadline {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        };
        tokio::time::sleep(retry_delay).await;
    }
}

pub(super) async fn activate_session_under_authority(
    state: AppState,
    activation: MediaSessionActivation,
    reconciled_predecessor: Option<MediaSessionRoute>,
    predecessor_incarnation: Option<String>,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    guard: Option<StartedSessionGuard>,
) -> Result<MediaSessionActivationOutcome, ApiError> {
    let lease_deadline = activation_lease_deadline(&activation);
    let serving_transition = authority
        .commit_guard_before(admitted_generation, lease_deadline.into_std())
        .await
        .ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "the media owner lost serving authority before durable activation".to_owned(),
            )
        })?;
    let prepared = tokio::time::timeout_at(
        lease_deadline,
        state.store.activate_media_session(&activation),
    )
    .await;
    let mut outcome = match prepared {
        Ok(Ok(Some(outcome))) if route_matches_activation(&outcome.route, &activation) => outcome,
        Ok(Ok(Some(_))) | Ok(Ok(None)) => {
            spawn_activation_abandonment(state.clone(), activation.clone());
            drop(serving_transition);
            return Err(ApiError::ServiceUnavailable(
                "session ownership could not be prepared".to_owned(),
            ));
        }
        Ok(Err(store_error)) => {
            let error = session_store_error("preparing the media session", store_error);
            let Some(route) = wait_for_exact_activation(&state, &activation, lease_deadline).await
            else {
                spawn_activation_abandonment(state.clone(), activation.clone());
                drop(serving_transition);
                return Err(error);
            };
            MediaSessionActivationOutcome {
                route,
                predecessor: reconciled_predecessor,
            }
        }
        Err(_) => {
            // Prepare and Abandon are mutually excluding Store transactions.
            // If the late prepare wins it remains BLOCKED until Abandon
            // tombstones it; if Abandon wins the request predicate prevents
            // the late prepare from publishing any route.
            spawn_activation_abandonment(state.clone(), activation.clone());
            drop(serving_transition);
            return Err(ApiError::ServiceUnavailable(
                "session preparation exceeded its fixed owner lease".to_owned(),
            ));
        }
    };
    if tokio::time::Instant::now() >= lease_deadline {
        spawn_activation_abandonment(state.clone(), activation.clone());
        drop(serving_transition);
        return Err(ApiError::ServiceUnavailable(
            "session preparation exhausted its publication lease".to_owned(),
        ));
    }
    let confirmation_now_ms = unix_ms();
    let publication_ready_at_ms = predecessor_incarnation.as_ref().map_or(0, |_| {
        confirmation_now_ms.saturating_add(
            i64::try_from(TERMINAL_PROJECTION_SAFETY_WINDOW.as_millis()).unwrap_or(i64::MAX),
        )
    });
    // The confirmation write may retry inside the Store; the recovery read
    // below is the only path that can still observe a write that committed
    // behind a lost response, so it keeps its own reserved share of the lease.
    let confirmation_deadline = activation_confirmation_deadline(lease_deadline);
    let confirmation = tokio::time::timeout_at(
        confirmation_deadline,
        state.store.settle_media_session_activation(
            &activation,
            MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms,
            },
            confirmation_now_ms,
        ),
    )
    .await;
    let route = match confirmation {
        Ok(Ok(Some(route))) if route_matches_activation(&route, &activation) => route,
        Ok(Ok(Some(_))) | Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
            let confirmed =
                wait_for_confirmed_activation(&state, &activation, lease_deadline).await;
            let Some(route) = confirmed else {
                spawn_activation_abandonment(state.clone(), activation.clone());
                drop(serving_transition);
                return Err(ApiError::ServiceUnavailable(
                    "session confirmation did not settle before its fixed owner lease".to_owned(),
                ));
            };
            route
        }
    };
    outcome.route = route.clone();
    // Serving loss needs the write side of this transition, so the atomic
    // BLOCKED-to-ready/armed confirmation above linearizes before any loss.
    drop(serving_transition);
    state.media_sessions.cache_route(route.clone()).await;
    settle_activation_predecessor(
        &state,
        predecessor_incarnation,
        route,
        authority,
        admitted_generation,
        guard,
    )
    .await?;
    Ok(outcome)
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

pub(super) async fn wait_for_confirmed_activation(
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
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED =>
            {
                return Some(route)
            }
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED => {}
            Ok(Ok(Some(_))) => return None,
            Ok(Ok(None)) | Ok(Err(_)) => {}
            Err(_) => return None,
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250).min(remaining)).await;
    }
}

enum PublishableActivationWait {
    Ready(Box<MediaSessionRoute>),
    Pending,
    Gone,
}

async fn wait_for_publishable_activation(
    state: &AppState,
    activation: &MediaSessionActivation,
    deadline: tokio::time::Instant,
) -> PublishableActivationWait {
    let mut observed_pending_handoff = false;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return if observed_pending_handoff {
                PublishableActivationWait::Pending
            } else {
                PublishableActivationWait::Gone
            };
        }
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .media_session_route_by_incarnation(&activation.incarnation_id),
        )
        .await
        {
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms == 0 =>
            {
                return PublishableActivationWait::Ready(Box::new(route));
            }
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED =>
            {
                observed_pending_handoff = true;
            }
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED => {}
            Ok(Ok(Some(_))) => return PublishableActivationWait::Gone,
            Ok(Ok(None)) | Ok(Err(_)) => {}
            Err(_) => {
                return if observed_pending_handoff {
                    PublishableActivationWait::Pending
                } else {
                    PublishableActivationWait::Gone
                };
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return if observed_pending_handoff {
                PublishableActivationWait::Pending
            } else {
                PublishableActivationWait::Gone
            };
        }
        tokio::time::sleep(Duration::from_millis(250).min(remaining)).await;
    }
}

/// The recovery budget's identity for one activation.
///
/// Decided from the playback pointer, because that is the only thing that
/// knows whether this is a deliberate new play or a continuation of one
/// already in progress. A new play mints; every continuation — an automatic
/// reopen, a seek, a track change, an ownership handoff — carries its
/// predecessor's, and that is the entire difference between one automatic
/// decode recovery per playback and one per attempt.
///
/// What the epoch buys, stated exactly: under one `playback_id`, a new request
/// id, a new client session id, or a different owner node cannot present
/// themselves as a fresh playback and be granted a second budget. It does not
/// bound a client that varies `playback_id` itself — the ledger's key contains
/// `playback_id`, so a fresh one lands on a fresh row whatever the epoch is.
/// That client is bounded by the ordinary session caps (`MAX_CURRENT_PER_USER`
/// and the per-node active count), not by this.
///
/// An empty predecessor epoch is inherited as empty rather than replaced.
/// Those are sessions that started before the column existed and never had a
/// budget; minting here would hand a fresh allowance to every continuation of
/// one, which is exactly the failure this prevents, arriving through an
/// upgrade instead of through a client. They gain an epoch the next time
/// somebody deliberately presses play.
fn recovery_epoch_for(predecessor: Option<&MediaSessionRoute>) -> String {
    predecessor.map_or_else(
        || uuid::Uuid::new_v4().to_string(),
        |route| route.recovery_epoch.clone(),
    )
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
        if !state
            .transcode
            .stop_session_for_owner(incarnation_id, session_id, 1, "cluster start aborted")
            .await
        {
            state
                .transcode
                .stop_vod_session_for_request(incarnation_id, session_id, "cluster start aborted")
                .await;
        }
    } else if let Err(error) = state
        .media_sessions
        .abort_remote(
            owner_node_id,
            &RemoteAbortRequest {
                incarnation_id: incarnation_id.to_owned(),
                session_id: session_id.to_owned(),
                expected_owner_epoch: 1,
                reason: None,
            },
        )
        .await
    {
        tracing::warn!(error = ?error, "remote cluster-start abort did not settle");
    }
}

/// Prime one rolling prepared successor under its already-reserved durable
/// capability.
///
/// Ordinary cluster creation deliberately mints a process-local session id.
/// Preparation has already minted the public id in the Store, so the worker
/// is adopted under that id before its cancellation guard is disarmed. The
/// guard moves through the same registry transaction: cancellation before the
/// move cleans the provisional id, and cancellation after it cleans the
/// durable id. Only a successfully adopted worker backed by the reserved row
/// is left for the preparation deadline/abort owner to settle.
pub(super) async fn prime_live_prepared_session(
    state: &AppState,
    recipe: &RemoteStartRequest,
    durable_session_id: &str,
    recovery_epoch: &str,
    expected_owner_epoch: i64,
    deadline: tokio::time::Instant,
) -> bool {
    if recipe.request.presentation != crate::transcode::Presentation::Live
        || tokio::time::Instant::now() >= deadline
    {
        return false;
    }
    let Some(_restart_admission) = state.serving.try_restart_admission().await else {
        return false;
    };
    let authority = state.serving.authority();
    let Some(admitted_generation) = authority.admit() else {
        return false;
    };
    let user = match state.store.get_user(recipe.user_id).await {
        Ok(Some(user)) => user,
        _ => return false,
    };
    let recovery = crate::transcode::SessionRecoveryIdentity {
        user_id: recipe.user_id,
        incarnation_id: recipe.incarnation_id.clone(),
        recovery_epoch: recovery_epoch.to_owned(),
    };
    let started = match state
        .transcode
        .create_cluster_prepared_session(
            &recipe.request,
            &recovery,
            &user.username,
            deadline,
            admitted_generation,
        )
        .await
    {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(%error, "rolling prepared successor could not start");
            return false;
        }
    };
    let crate::transcode::ClusterSessionStart {
        info,
        replacement,
        created,
    } = started;
    let mut guard = if created {
        StartedSessionGuard::worker_only(
            state.clone(),
            state.node_id.clone(),
            recipe.incarnation_id.clone(),
            info.session_id.clone(),
            recipe.user_id,
            recipe.incarnation_id.clone(),
            Some(replacement),
        )
    } else {
        StartedSessionGuard::replayed(
            state.clone(),
            state.node_id.clone(),
            recipe.incarnation_id.clone(),
            info.session_id.clone(),
            recipe.user_id,
            recipe.incarnation_id.clone(),
            Some(replacement),
        )
    };
    if !authority.is_current(admitted_generation) {
        return false;
    }
    let expires_at_ms = unix_ms().saturating_add(
        i64::try_from(
            deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_millis(),
        )
        .unwrap_or(i64::MAX),
    );
    let pinned = super::internal_media_sessions::pin_shared_session_before_deadline(
        deadline,
        state.transcode.pin_shared_session(
            &info.session_id,
            &recipe.incarnation_id,
            expected_owner_epoch,
            expires_at_ms,
        ),
    )
    .await
    .unwrap_or(false);
    if !pinned || !authority.is_current(admitted_generation) {
        return false;
    }
    let Some(adoption) = state.transcode.session_adoption_token(durable_session_id) else {
        return false;
    };
    guard = match state
        .transcode
        .adopt_session_id_with_owner(&info.session_id, durable_session_id, adoption, guard)
        .await
    {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    if !authority.is_current(admitted_generation) {
        return false;
    }
    guard.disarm();
    true
}

async fn settle_activation_predecessor(
    state: &AppState,
    predecessor_incarnation: Option<String>,
    successor: MediaSessionRoute,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    mut guard: Option<StartedSessionGuard>,
) -> Result<(), ApiError> {
    // Before the early return, because a plain create for this playback is a
    // supersession too: a client that reopened without naming its predecessor
    // has still gone around the handoff, and the successor it abandoned would
    // otherwise run to the deadline. One call site rather than the two the plan
    // named — every activation reaches this function, and `keep` is what makes
    // the committed successor's own activation safe here.
    cancel_preparations_for_superseded_predecessor(
        &successor.playback_id,
        Some(successor.incarnation_id.as_str()),
    );
    let Some(predecessor_incarnation) = predecessor_incarnation else {
        if let Some(guard) = guard.as_mut() {
            guard.disarm();
        }
        return Ok(());
    };
    debug_assert_ne!(
        successor.publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    let fast_deadline = tokio::time::Instant::now() + PREDECESSOR_PROJECTION_FAST_WINDOW;
    if project_activation_predecessor_until(state, &predecessor_incarnation, fast_deadline).await {
        match complete_activation_handoff_until(
            state,
            &successor,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            &authority,
            admitted_generation,
            fast_deadline,
        )
        .await
        {
            ActivationHandoffVerdict::Ready => {
                if let Some(guard) = guard.as_mut() {
                    guard.disarm();
                }
                return Ok(());
            }
            ActivationHandoffVerdict::SuccessorGone => {
                return Err(media_session_ended());
            }
            ActivationHandoffVerdict::Pending => {}
            ActivationHandoffVerdict::AuthorityLost => {
                return Err(ApiError::ServiceUnavailable(
                    "the media owner lost serving authority during session handoff".to_owned(),
                ));
            }
        }
    }

    // Keep the activated successor's cleanup guard and replacement permit in
    // a cancellation-independent owner. The caller gets a typed retry while
    // this owner continues; publication cannot proceed until either the exact
    // predecessor acknowledges terminal control or every response admitted
    // by that generation has crossed the safety boundary.
    let projection_state = state.clone();
    tokio::spawn(async move {
        let settled = settle_armed_activation_handoff(
            &projection_state,
            predecessor_incarnation,
            successor,
            authority,
            admitted_generation,
        )
        .await;
        if settled {
            if let Some(guard) = guard.as_mut() {
                guard.disarm();
            }
        }
        // Otherwise the armed guard performs exact worker cleanup. A serving
        // generation which has been lost can never publish the finite route.
    });
    Err(ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "media_session_handoff_pending",
        "the predecessor handoff remains durably owned; retry this request",
    ))
}

async fn settle_armed_activation_handoff(
    state: &AppState,
    predecessor_incarnation: String,
    successor: MediaSessionRoute,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
) -> bool {
    let remaining_ms = successor
        .publication_ready_at_ms
        .saturating_sub(unix_ms())
        .max(0);
    let boundary_deadline = tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(u64::MAX));
    let acknowledged =
        project_activation_predecessor_until(state, &predecessor_incarnation, boundary_deadline)
            .await;
    if acknowledged
        && matches!(
            complete_activation_handoff_until(
                state,
                &successor,
                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                &authority,
                admitted_generation,
                boundary_deadline,
            )
            .await,
            ActivationHandoffVerdict::Ready | ActivationHandoffVerdict::SuccessorGone
        )
    {
        return true;
    }
    tracing::warn!(
        incarnation = %predecessor_incarnation,
        safety_window_seconds = TERMINAL_PROJECTION_SAFETY_WINDOW.as_secs(),
        "predecessor acknowledgement was unavailable through the response-lifetime safety boundary"
    );
    matches!(
        complete_activation_handoff_until(
            state,
            &successor,
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                expected_not_before_ms: successor.publication_ready_at_ms,
            },
            &authority,
            admitted_generation,
            tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE,
        )
        .await,
        ActivationHandoffVerdict::Ready | ActivationHandoffVerdict::SuccessorGone
    )
}

enum ActivationHandoffVerdict {
    Ready,
    SuccessorGone,
    Pending,
    AuthorityLost,
}

async fn complete_activation_handoff_until(
    state: &AppState,
    successor: &MediaSessionRoute,
    proof: MediaSessionProjectionCompletion,
    authority: &crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    deadline: tokio::time::Instant,
) -> ActivationHandoffVerdict {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return ActivationHandoffVerdict::Pending;
        }
        let attempt_deadline = (now + ACTIVATION_STORE_DEADLINE).min(deadline);
        let Some(serving_transition) = authority
            .commit_guard_before(admitted_generation, attempt_deadline.into_std())
            .await
        else {
            return ActivationHandoffVerdict::AuthorityLost;
        };
        match tokio::time::timeout_at(
            attempt_deadline,
            state.store.complete_media_session_handoff(
                &successor.incarnation_id,
                &successor.owner_node_id,
                successor.owner_epoch,
                proof,
                unix_ms(),
            ),
        )
        .await
        {
            Ok(Ok(Some(route))) => {
                drop(serving_transition);
                state.media_sessions.cache_route(route).await;
                return ActivationHandoffVerdict::Ready;
            }
            Ok(Ok(None)) => {
                drop(serving_transition);
                // A terminal or replaced successor no longer needs a
                // publication handoff; the armed cleanup guard may settle it.
                if let Ok(Ok(Some(route))) = tokio::time::timeout_at(
                    attempt_deadline,
                    state
                        .store
                        .media_session_route_by_incarnation(&successor.incarnation_id),
                )
                .await
                {
                    if route.state != "active"
                        || route.owner_node_id != successor.owner_node_id
                        || route.owner_epoch != successor.owner_epoch
                    {
                        return ActivationHandoffVerdict::SuccessorGone;
                    }
                }
            }
            Ok(Err(error)) => {
                drop(serving_transition);
                tracing::warn!(
                    error = ?error,
                    incarnation = %successor.incarnation_id,
                    "successor publication-fence completion is retrying"
                );
            }
            Err(_) => drop(serving_transition),
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return ActivationHandoffVerdict::Pending;
        }
        tokio::time::sleep(PREDECESSOR_PROJECTION_RETRY_DELAY.min(remaining)).await;
    }
}

async fn project_activation_predecessor_until(
    state: &AppState,
    predecessor_incarnation: &str,
    deadline: tokio::time::Instant,
) -> bool {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let read_deadline = (now + ACTIVATION_STORE_DEADLINE).min(deadline);
        let route = tokio::time::timeout_at(
            read_deadline,
            state
                .store
                .media_session_route_by_incarnation(predecessor_incarnation),
        )
        .await;
        if let Ok(Ok(Some(route))) = route {
            if route.incarnation_id == predecessor_incarnation && route.state == "ended" {
                let Some(terminal) = crate::vodserve::Terminal::from_durable_reason(
                    route.terminal_reason.as_deref(),
                ) else {
                    tracing::error!(
                        incarnation = %predecessor_incarnation,
                        terminal_reason = ?route.terminal_reason,
                        "ended activation predecessor has no valid durable terminal cause"
                    );
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return false;
                    }
                    tokio::time::sleep(PREDECESSOR_PROJECTION_RETRY_DELAY.min(remaining)).await;
                    continue;
                };
                let projection_deadline = (tokio::time::Instant::now()
                    + PREDECESSOR_PROJECTION_FAST_WINDOW)
                    .min(deadline);
                if matches!(
                    tokio::time::timeout_at(
                        projection_deadline,
                        stop_owned_session_because(
                            state,
                            &route,
                            terminal,
                            terminal.control_reason(),
                        ),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    if let Some(proof) = state
                        .media_sessions
                        .complete_terminal_projection(
                            &route,
                            MediaSessionProjectionCompletion::PredecessorAcknowledged,
                        )
                        .await
                        .filter(|proof| proof.terminal_projection_complete())
                    {
                        state.transcode.complete_session_release_durable(&proof);
                        return true;
                    }
                }
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::time::sleep(PREDECESSOR_PROJECTION_RETRY_DELAY.min(remaining)).await;
    }
}

/// Same teardown with an honest reason: a client DELETE is a release, not a
/// supersession, and the VOD tombstone cause keys off this string.
async fn stop_owned_session_because(
    state: &AppState,
    route: &MediaSessionRoute,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> Result<(), PeerTransportError> {
    if route.owner_node_id == state.node_id {
        state
            .transcode
            .begin_session_terminal(&route.session_id, terminal, reason)
            .await;
        state.transcode.complete_session_release(&route.session_id);
        Ok(())
    } else {
        state
            .media_sessions
            .abort_remote(
                &route.owner_node_id,
                &RemoteAbortRequest {
                    incarnation_id: route.incarnation_id.clone(),
                    session_id: route.session_id.clone(),
                    expected_owner_epoch: route.owner_epoch,
                    reason: Some(reason.to_owned()),
                },
            )
            .await
    }
}

/// The real mapper, reachable from the accounting regressions in
/// `transcode.rs` so a genuinely returned refusal can be asserted end to end
/// rather than a hand-built string that already carries the class.
#[cfg(test)]
pub(crate) fn session_start_error_status_for_test(file_id: i64, error: String) -> StatusCode {
    session_start_error(file_id, error).into_response().status()
}

/// What the durable recipe says about this session's client teardown.
///
/// The recipe is the serialized create request, so this is the transport the
/// client named at create and nothing else — never the user agent, never a
/// guess from the playlist shape. A recipe an older node wrote carries no
/// transport at all and reads as conservative, which is the whole point: a
/// mixed-version cluster shortens nothing it cannot account for.
fn exact_release_class(route: &MediaSessionRoute) -> crate::transcode::ReleaseClass {
    let transport =
        serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(&route.recipe_json)
            .ok()
            .and_then(|recipe| recipe.request.transport);
    crate::transcode::ReleaseClass::from_transport(transport.as_deref())
}

fn session_start_error(file_id: i64, error: String) -> ApiError {
    if error.contains("already used") {
        return ApiError::Conflict(error);
    }
    tracing::warn!(file = file_id, "session create failed: {error}");
    if crate::transcode::is_serving_fence_error(&error)
        || crate::transcode::is_start_infrastructure_error(&error)
        || crate::transcode::is_retryable_capacity_error(&error)
    {
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
            "vod_video_geometry_unknown" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_video_geometry_unknown",
            ),
            "vod_frame_cadence_unknown" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_frame_cadence_unknown",
            ),
            "vod_source_rescan_required" => (StatusCode::CONFLICT, "vod_source_rescan_required"),
            "vod_engine_unattested" => (StatusCode::SERVICE_UNAVAILABLE, "vod_engine_unattested"),
            "vod_audio_track_missing" => {
                (StatusCode::UNPROCESSABLE_ENTITY, "vod_audio_track_missing")
            }
            "vod_subtitle_track_missing" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_subtitle_track_missing",
            ),
            "vod_invalid_height" => (StatusCode::BAD_REQUEST, "vod_invalid_height"),
            "vod_recipe_unresolved" => (StatusCode::INTERNAL_SERVER_ERROR, "vod_recipe_unresolved"),
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

fn session_store_error(operation: &'static str, error: plurx_core::error::StoreError) -> ApiError {
    tracing::warn!(%error, operation, "media-session Store operation failed");
    ApiError::ServiceUnavailable(format!("{operation}: {error}"))
}

async fn relay_if_remote(
    state: &AppState,
    session_id: &str,
    resource: RelayResource,
    headers: RelayHeaders,
    request_deadline: Instant,
) -> Result<Option<Response>, ApiError> {
    let media_resource = !matches!(resource, RelayResource::Status | RelayResource::Delete);
    let resolution = if media_resource {
        state
            .media_sessions
            .media_route_resolution_before(session_id, &state.node_id, request_deadline)
            .await?
    } else {
        state
            .media_sessions
            .route_resolution_before(session_id, &state.node_id, request_deadline)
            .await?
    };
    let mut route = match resolution {
        DurableRouteResolution::Absent | DurableRouteResolution::ActiveLocal(_) => return Ok(None),
        DurableRouteResolution::OwnerTransition(_) | DurableRouteResolution::Terminal(_) => {
            let resolution = if media_resource {
                state
                    .media_sessions
                    .authoritative_media_route_resolution_before(
                        session_id,
                        &state.node_id,
                        request_deadline,
                    )
                    .await?
            } else {
                state
                    .media_sessions
                    .authoritative_route_resolution_before(
                        session_id,
                        &state.node_id,
                        request_deadline,
                    )
                    .await?
            };
            match resolution {
                DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
                DurableRouteResolution::ActiveLocal(_) => return Ok(None),
                DurableRouteResolution::ActiveRemote(route) => route,
                DurableRouteResolution::OwnerTransition(lost) => {
                    return Err(owner_transition_answer(&lost))
                }
                DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
            }
        }
        DurableRouteResolution::ActiveRemote(route) => route,
    };
    for attempt in 0..2 {
        let deadline_unix_ms =
            resource_deadline_unix_ms(request_deadline).ok_or_else(response_publication_timeout)?;
        let response = state
            .media_sessions
            .relay(
                &route.owner_node_id,
                &RelayRequest {
                    session_id: session_id.to_owned(),
                    resource: resource.clone(),
                    deadline_unix_ms,
                    // The relay envelope has no peer capability version.
                    // Until both ends negotiate validator-aware Range, every
                    // relayed request downgrades to a complete representation.
                    headers: headers.for_unversioned_peer(),
                },
            )
            .await
            .map_err(|error| {
                ApiError::ServiceUnavailable(format!("media worker relay unavailable: {error:?}"))
            })?;
        let peer_status = response.status();
        if !relay_status_requires_reclassification(peer_status) {
            return Ok(Some(response));
        }
        let resolution = if media_resource {
            state
                .media_sessions
                .authoritative_media_route_resolution_before(
                    session_id,
                    &state.node_id,
                    request_deadline,
                )
                .await?
        } else {
            state
                .media_sessions
                .authoritative_route_resolution_before(session_id, &state.node_id, request_deadline)
                .await?
        };
        match resolution {
            DurableRouteResolution::Absent if peer_status == StatusCode::NOT_FOUND => {
                return Ok(Some(response));
            }
            DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
            DurableRouteResolution::ActiveLocal(_) => return Ok(None),
            DurableRouteResolution::OwnerTransition(lost) => {
                return Err(owner_transition_answer(&lost))
            }
            DurableRouteResolution::Terminal(_) if peer_status == StatusCode::GONE => {
                return Ok(Some(response));
            }
            DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
            DurableRouteResolution::ActiveRemote(next_route) if attempt == 0 => {
                drop(response);
                route = next_route;
            }
            DurableRouteResolution::ActiveRemote(_) => return Err(media_owner_transition()),
        }
    }
    unreachable!("bounded relay reclassification returns on every branch")
}

fn relay_status_requires_reclassification(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::GONE
    )
}

/// Execute an authenticated relay on the owning worker without performing a
/// second route lookup that could proxy back to the ingress node.
pub(crate) async fn relay_local(state: &AppState, request: RelayRequest) -> Response {
    let Some(request_deadline) = inherited_resource_deadline(&request) else {
        return response_publication_timeout().into_response();
    };
    let (playlist_deadline, _) = playlist_request_deadlines_before(state, request_deadline);
    let result = match request.resource {
        RelayResource::Status => {
            status_local_before_with_relay(state, &request.session_id, request_deadline, false)
                .await
        }
        RelayResource::Playlist { native, subtitle } => {
            playlist_local_before(
                state,
                &request.session_id,
                PlaylistQuery {
                    native,
                    subtitle,
                    diagnostic: None,
                },
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::Master {
            subtitle,
            diagnostic,
        } => {
            master_playlist_response_local_before(
                state,
                &request.session_id,
                PlaylistQuery {
                    native: None,
                    subtitle,
                    diagnostic,
                },
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::VideoPlaylist => {
            video_playlist_local_before(
                state,
                &request.session_id,
                "video.m3u8",
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::SubtitlePlaylist { index } => {
            subtitle_playlist_local_before(
                state,
                &request.session_id,
                index,
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::SubtitleSegment { index, segment } => {
            subtitle_vtt_local_before(
                state,
                &request.session_id,
                index,
                &segment,
                request_deadline,
            )
            .await
        }
        RelayResource::Segment { segment } => {
            segment_local_before(
                state,
                &request.session_id,
                &segment,
                &request.headers,
                request_deadline,
            )
            .await
        }
        RelayResource::Delete => {
            // Mixed-version peers may still send this compatibility shape.
            // It must join the same durable first-writer transaction as the
            // public endpoint; a process-local 204 would allow the active row
            // to be takeover-claimed later.
            return release_with_slots(
                state.clone(),
                request.session_id.clone(),
                session_release_slots(),
                request_deadline,
                crate::vodserve::Terminal::Deleted,
                "released by client",
            )
            .await
            .into_response();
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
    let deadline = response_publication_deadline();
    delete_with_slots(state, session, session_release_slots(), deadline).await
}

/// Run an authenticated operator terminal through the same exact, durable
/// release coordinator used by public capability DELETE. The first admitted
/// terminal intent owns telemetry when concurrent callers join one settlement.
pub(super) async fn release_with_terminal(
    state: AppState,
    session: String,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> StatusCode {
    release_with_slots(
        state,
        session,
        session_release_slots(),
        response_publication_deadline(),
        terminal,
        reason,
    )
    .await
}

async fn delete_with_slots(
    state: AppState,
    session: String,
    slots: Arc<tokio::sync::Semaphore>,
    deadline: Instant,
) -> StatusCode {
    release_with_slots(
        state,
        session,
        slots,
        deadline,
        crate::vodserve::Terminal::Deleted,
        "released by client",
    )
    .await
}

async fn release_with_slots(
    state: AppState,
    session: String,
    slots: Arc<tokio::sync::Semaphore>,
    deadline: Instant,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> StatusCode {
    let release_settlement = match state
        .media_sessions
        .begin_release_reconciliation_with_intent(
            &session,
            crate::media_sessions::ReleaseIntent { terminal, reason },
        )
        .await
    {
        ReleaseAdmission::Won(settlement) => settlement,
        ReleaseAdmission::Joined(settlement) => {
            return tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                settlement.wait(),
            )
            .await
            .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
        }
        ReleaseAdmission::Full => return StatusCode::SERVICE_UNAVAILABLE,
    };
    // The task owns both the bounded settlement slot and the full durable
    // end/fence/cleanup transaction. Dropping the HTTP request or timing out
    // its wait only detaches this owner; it cannot cancel cleanup after the
    // Store has committed the terminal row. Ownership transfers immediately
    // after election: even cancellation while capacity is saturated cannot
    // strand an in-flight release fence.
    let settlement_task = tokio::spawn(async move {
        let permit = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            slots.acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) | Err(_) => {
                state
                    .media_sessions
                    .complete_release_settlement(
                        &session,
                        &release_settlement,
                        StatusCode::SERVICE_UNAVAILABLE,
                    )
                    .await;
                return StatusCode::SERVICE_UNAVAILABLE;
            }
        };
        let _permit = permit;
        let worker_state = state.clone();
        let worker_session = session.clone();
        let worker_settlement = Arc::clone(&release_settlement);
        match tokio::spawn(async move {
            release_session(
                worker_state,
                worker_session,
                worker_settlement,
                terminal,
                reason,
            )
            .await
        })
        .await
        {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(
                    %error,
                    session = %crate::transcode::session_log_id(&session),
                    "media-session release transaction panicked; transferred to lifecycle reconciliation"
                );
                state
                    .media_sessions
                    .defer_release_reconciliation(&session, &release_settlement)
                    .await;
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    });
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), settlement_task).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            tracing::warn!(%error, "media-session release settlement task failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
        // Dropping JoinHandle detaches the still-owned cleanup task.
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn session_release_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(SESSION_RELEASE_CAPACITY))),
    )
}

async fn release_session(
    state: AppState,
    session: String,
    settlement: Arc<ReleaseSettlement>,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> StatusCode {
    // Close publication before Store latency without choosing the local VOD
    // tombstone cause. The Store CAS below records the first writer; phase two
    // projects that exact durable winner.
    state
        .transcode
        .begin_session_publication_fence(&session)
        .await;
    #[cfg(test)]
    if let Some(pause) = release_pause_for(&session) {
        pause.wait().await;
        pause.wait().await;
    }
    // Never put an HTTP deadline around this mutation. SQLite blocking work
    // and a submitted Raft proposal may commit after caller cancellation, so
    // this detached capacity-owned task retains the attempt until the Store
    // returns a definitive result.
    let route = match end_media_session_for_release(&state, &session, terminal).await {
        Ok(route) => route,
        Err(error) => {
            tracing::warn!(
                %error,
                session = %crate::transcode::session_log_id(&session),
                "durable media-session release is commit-unknown; transferred to lifecycle reconciliation"
            );
            state
                .media_sessions
                .defer_release_reconciliation(&session, &settlement)
                .await;
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    match route {
        Some(route) => {
            // Publish the returned exact terminal owner before any local or
            // remote stop await. A negative cache entry would look absent to
            // ingress and reopen the still-live local fallback in this gap.
            let projected_terminal =
                crate::vodserve::Terminal::from_durable_reason(route.terminal_reason.as_deref())
                    .unwrap_or(terminal);
            let projected_reason = projected_terminal.control_reason();
            state
                .transcode
                .begin_session_terminal(&session, projected_terminal, projected_reason)
                .await;
            // R3. A viewer releasing their own stream is the one event that
            // tells the server nobody will read its retired objects again —
            // but only for a client whose teardown ordering has been audited.
            // The class comes from the durable recipe this coordinator just
            // resolved, so a remote owner, an idempotent replay and an owner
            // takeover all read the same answer the create wrote.
            //
            // Restricted to a client-initiated release: an admin stop, a
            // revocation or a supersession is not the viewer saying they are
            // finished with the bytes.
            if projected_terminal == crate::vodserve::Terminal::Deleted {
                let class = exact_release_class(&route);
                if let Some(deadline) = state
                    .transcode
                    .accept_exact_session_release(&session, class)
                    .await
                {
                    tracing::debug!(
                        session = %crate::transcode::session_log_id(&session),
                        class = class.label(),
                        in_ms = deadline.saturating_duration_since(Instant::now()).as_millis(),
                        "retired object promise shortened by an exact same-viewer release"
                    );
                }
            }
            let Some(durable_release) = state
                .media_sessions
                .complete_release_with_route(route.clone())
                .await
            else {
                state
                    .media_sessions
                    .defer_release_reconciliation(&session, &settlement)
                    .await;
                return StatusCode::SERVICE_UNAVAILABLE;
            };
            state
                .transcode
                .complete_session_release_durable(&durable_release);
            #[cfg(test)]
            if let Some(pause) = release_after_tombstone_pause_for(&session) {
                pause.wait().await;
                pause.wait().await;
            }
            // A remote worker must acknowledge its local terminal generation
            // before the shared DELETE settles. The acknowledgement is fast:
            // physical reap is already transferred to the worker's bounded
            // lifecycle owner. If transport is unavailable, retain the exact
            // owner route and settlement for lifecycle retry.
            if !durable_release.terminal_projection_complete()
                && route.owner_node_id != state.node_id
            {
                let mut projected = false;
                for attempt in 0..REMOTE_RELEASE_ATTEMPTS {
                    match stop_owned_session_because(
                        &state,
                        &route,
                        projected_terminal,
                        projected_reason,
                    )
                    .await
                    {
                        Ok(()) => {
                            projected = true;
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = ?error,
                                session = %crate::transcode::session_log_id(&session),
                                owner = %route.owner_node_id,
                                attempt = attempt + 1,
                                "exact owner terminal projection attempt failed"
                            );
                            if attempt + 1 < REMOTE_RELEASE_ATTEMPTS {
                                tokio::time::sleep(REMOTE_RELEASE_RETRY_DELAY).await;
                            }
                        }
                    }
                }
                if !projected {
                    state
                        .media_sessions
                        .defer_release_reconciliation(&session, &settlement)
                        .await;
                    return StatusCode::SERVICE_UNAVAILABLE;
                }
            }
            if !durable_release.terminal_projection_complete() {
                let Some(projected_release) = state
                    .media_sessions
                    .complete_terminal_projection(
                        &route,
                        MediaSessionProjectionCompletion::PredecessorAcknowledged,
                    )
                    .await
                    .filter(|proof| proof.terminal_projection_complete())
                else {
                    state
                        .media_sessions
                        .defer_release_reconciliation(&session, &settlement)
                        .await;
                    return StatusCode::SERVICE_UNAVAILABLE;
                };
                state
                    .transcode
                    .complete_session_release_durable(&projected_release);
            }
        }
        None => {
            // Confirmed durable absence is idempotent success. The elected
            // owner already tombstoned VOD or actor-fenced rolling and
            // transferred physical cleanup before submitting the Store
            // mutation, so lifting the temporary route fence cannot reopen a
            // process-local fallback here.
            state
                .transcode
                .begin_session_terminal(&session, terminal, reason)
                .await;
            let durable_release = state.media_sessions.complete_release_absent(&session).await;
            state
                .transcode
                .complete_session_release_durable(&durable_release);
        }
    }
    state
        .media_sessions
        .complete_release_settlement(&session, &settlement, StatusCode::NO_CONTENT)
        .await;
    StatusCode::NO_CONTENT
}

async fn end_media_session_for_release(
    state: &AppState,
    session: &str,
    terminal: crate::vodserve::Terminal,
) -> Result<Option<MediaSessionRoute>, StoreError> {
    #[cfg(test)]
    if take_injected_release_error(session) {
        return Err(StoreError::Database(
            "injected commit-unknown media-session release".to_owned(),
        ));
    }
    state
        .store
        .end_media_session(session, terminal.durable_reason(), unix_ms())
        .await
}

#[cfg(test)]
fn injected_release_errors() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static ERRORS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    ERRORS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
fn inject_release_error(session: &str) {
    injected_release_errors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session.to_owned());
}

#[cfg(test)]
fn take_injected_release_error(session: &str) -> bool {
    injected_release_errors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session)
}

#[cfg(test)]
fn release_pauses(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn release_pause_for(session: &str) -> Option<Arc<tokio::sync::Barrier>> {
    release_pauses()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session)
        .cloned()
}

#[cfg(test)]
fn release_after_tombstone_pauses(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn release_after_tombstone_pause_for(session: &str) -> Option<Arc<tokio::sync::Barrier>> {
    release_after_tombstone_pauses()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session)
        .cloned()
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
        intent: None,
        // No control exchange precedes a legacy start, so there is no
        // ordering for it to be stale against.
        control_sequence: None,
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
        // …and no capabilities document to re-derive from, so it lands on the
        // trust path with every other client that predates caps v2.
        caps: None,
        overrides: None,
        audio_offset_ms: None,
        presentation: None,
        block_budget_secs: None,
        transport: None,
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
    let request_deadline = response_publication_deadline();
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Status,
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    status_local_before(&state, &session, request_deadline).await
}

/// POST /api/v1/hls/:session/control — one bounded, capability-authenticated
/// playback-control exchange. The session UUID remains the bearer capability;
/// generation, epoch, client instance and sequence are mutation fences.
pub async fn control(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    body: Bytes,
) -> Response {
    let deadline_unix_ms = crate::media_sessions::unix_ms().saturating_add(
        i64::try_from(crate::playback_control::EXCHANGE_DEADLINE.as_millis()).unwrap_or(i64::MAX),
    );
    match tokio::time::timeout(
        crate::playback_control::EXCHANGE_DEADLINE,
        control_inner(state, session, body, deadline_unix_ms),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the playback control exchange exceeded its deadline",
                None,
                None,
                Some(500),
                None,
            )
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct RetainedTerminalResponse {
    platform: crate::playback_control::ClientPlatform,
    response: crate::playback_control::ControlResponseV1,
}

/// Resolve the selected native text track's cache state for the control
/// response.
///
/// Gated on `Native` before anything is read: `Off`, `Overlay` and `Burn` name
/// no sidecar, so the common case costs one enum comparison and no store hit.
/// Nothing here starts an extraction — a readiness probe that did would make
/// every control exchange a reason to spawn ffmpeg.
async fn subtitle_track_cache(
    state: &AppState,
    recipe: &RemoteStartRequest,
    selection: &crate::playback_control::SubtitleSelection,
    request: &crate::playback_control::ControlRequestV1,
    window_seconds: i64,
) -> Option<crate::playback_control::SubtitleTrackCache> {
    use crate::playback_control::{SubtitleMode, SubtitleTrackCache};

    if !matches!(selection.mode, SubtitleMode::Native) {
        return None;
    }
    let index = selection.track?;
    let file = state.store.get_file(recipe.request.file_id).await.ok()??;
    // A track that is not there, or is there as a bitmap, will never produce a
    // sidecar however long the client waits. Saying `unavailable` is the whole
    // point of distinguishing it from `warming`.
    let Some(track) = usize::try_from(index)
        .ok()
        .and_then(|i| file.subtitle_streams.get(i))
    else {
        return Some(SubtitleTrackCache::Unavailable);
    };
    if !plurx_core::tracks::is_native_text_subtitle(&track.codec) {
        return Some(SubtitleTrackCache::Unavailable);
    }
    // Control positions are already absolute film time. The segment path gets
    // the same value by adding its item-local segment start to
    // `media_origin_seconds`; snapping both through the shared grid is what
    // prevents readiness from reporting on a window the segment will not use.
    let demand_seconds = request.seek_target_ms.unwrap_or(request.position_ms).max(0) / 1_000;
    Some(
        match crate::subtitles::sidecar_state_for_demand(
            &state.subs_dir,
            &file,
            index,
            demand_seconds,
            window_seconds,
        )
        .await
        {
            crate::subtitles::SidecarState::Ready => SubtitleTrackCache::Ready,
            crate::subtitles::SidecarState::Failed => SubtitleTrackCache::Unavailable,
            crate::subtitles::SidecarState::Warming | crate::subtitles::SidecarState::Absent => {
                SubtitleTrackCache::Warming
            }
        },
    )
}

fn local_control_response(
    route: &MediaSessionRoute,
    start: &StartResponse,
    recipe: &RemoteStartRequest,
    request: &crate::playback_control::ControlRequestV1,
    result: &crate::playback_control::LocalControlResult,
    server_time_unix_ms: i64,
    subtitle_readiness: Option<String>,
) -> crate::playback_control::ControlResponseV1 {
    let owner_epoch = u64::try_from(route.owner_epoch).unwrap_or_default();
    let response = crate::playback_control::ControlResponseV1 {
        protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
        generation: route.incarnation_id.clone(),
        control_epoch: owner_epoch,
        accepted_sequence: result.accepted_sequence,
        server_time_unix_ms,
        lease: crate::playback_control::PlaybackLeaseView {
            state: result.lease_state.to_owned(),
            renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
            expires_at_unix_ms: if result.lease_state == "ended" {
                server_time_unix_ms
            } else {
                result.lease_expires_at_unix_ms
            },
        },
        delivery: crate::playback_control::DeliveryView::from_status(
            &result.status,
            request,
            &route.owner_node_id,
            owner_epoch,
            route.media_origin_ms,
            subtitle_readiness,
        ),
        effective_selection: crate::playback_control::EffectiveSelection::from_recipe(
            recipe,
            start.height,
            start.delivered_dynamic_range.clone(),
        ),
        action: crate::playback_control::ControlAction::None,
    };
    // The advisory hold is derived from the delivery this response is already
    // carrying, so the two can never disagree. Building the response first and
    // resolving after is what makes that guarantee structural rather than a
    // rule two call sites have to remember.
    let action =
        crate::playback_control::resolve_action(&result.action, &response.delivery, request);
    crate::playback_control::record_action(
        &action,
        &response.delivery,
        request,
        result.platform,
        result.action_suppressed,
    );
    crate::playback_control::ControlResponseV1 { action, ..response }
}

const PREPARATION_SETTLEMENT_CAPACITY: usize = 32;
const PREPARATION_SETTLEMENT_RETRY_BUDGET: Duration = Duration::from_secs(5);
const PREPARATION_SETTLEMENT_RETRY_MIN: Duration = Duration::from_millis(25);
const PREPARATION_SETTLEMENT_RETRY_MAX: Duration = Duration::from_millis(500);

#[derive(Clone)]
enum PreparationSettlement {
    Committed(Box<crate::playback_control::ControlResponseV1>),
    Aborted,
    Rejected,
    Unavailable,
}

#[derive(Clone)]
struct PreparationSettlementReceipt {
    result: tokio::sync::watch::Receiver<Option<PreparationSettlement>>,
}

impl PreparationSettlementReceipt {
    async fn wait_before(&self, deadline_unix_ms: i64) -> Option<PreparationSettlement> {
        let remaining = crate::playback_control::inherited_exchange_budget(
            deadline_unix_ms,
            crate::media_sessions::unix_ms(),
        )?;
        let mut result = self.result.clone();
        let wait = async move {
            loop {
                if let Some(outcome) = result.borrow().clone() {
                    return Some(outcome);
                }
                if result.changed().await.is_err() {
                    return None;
                }
            }
        };
        tokio::time::timeout(remaining, wait).await.ok().flatten()
    }
}

#[derive(Clone)]
struct ActivePreparationSettlement {
    request_fingerprint: String,
    receipt: PreparationSettlementReceipt,
    /// `None` while the owner is running; completed canonical responses stay
    /// joinable through the same durable replay window as their Store row.
    replay_until: Option<std::time::Instant>,
}

fn preparation_settlements(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ActivePreparationSettlement>> {
    static OPERATIONS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ActivePreparationSettlement>>,
    > = std::sync::OnceLock::new();
    OPERATIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn preparation_settlement_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(PREPARATION_SETTLEMENT_CAPACITY))),
    )
}

#[cfg(test)]
fn preparation_settlement_faults(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, usize>> {
    static FAULTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn fail_next_preparation_settlements(incarnation_id: &str, count: usize) {
    preparation_settlement_faults()
        .lock()
        .expect("preparation settlement faults")
        .insert(incarnation_id.to_owned(), count);
}

#[cfg(test)]
fn consume_preparation_settlement_fault(incarnation_id: &str) -> bool {
    let mut faults = preparation_settlement_faults()
        .lock()
        .expect("preparation settlement faults");
    let Some(remaining) = faults.get_mut(incarnation_id) else {
        return false;
    };
    if *remaining == 0 {
        faults.remove(incarnation_id);
        return false;
    }
    *remaining -= 1;
    true
}

struct DurablePreparationSettlementAdmission {
    state: AppState,
    gate: Arc<dyn crate::playback_control::PreparationGate>,
    route: MediaSessionRoute,
    start: StartResponse,
    recipe: RemoteStartRequest,
    request: crate::playback_control::ControlRequestV1,
    status: Option<crate::transcode::HlsSessionInfo>,
    reservation: std::sync::Mutex<Option<PreparationSettlementReservation>>,
    receipt: std::sync::Mutex<Option<PreparationSettlementReceipt>>,
}

struct PreparationSettlementReservation {
    _permit: tokio::sync::OwnedSemaphorePermit,
    _serving_commit: tokio::sync::OwnedRwLockReadGuard<()>,
}

async fn reserve_preparation_settlement(
    state: &AppState,
    deadline_unix_ms: i64,
    slots: Arc<tokio::sync::Semaphore>,
) -> Option<PreparationSettlementReservation> {
    let remaining = crate::playback_control::inherited_exchange_budget(
        deadline_unix_ms,
        crate::media_sessions::unix_ms(),
    )?;
    let deadline = tokio::time::Instant::now() + remaining.min(PREPARATION_SETTLEMENT_RETRY_BUDGET);
    let permit = tokio::time::timeout_at(deadline, slots.acquire_owned())
        .await
        .ok()?
        .ok()?;
    let authority = state.serving.authority();
    let serving_generation = authority.admit()?;
    let serving_commit = authority
        .commit_guard_before(serving_generation, deadline.into_std())
        .await?;
    Some(PreparationSettlementReservation {
        _permit: permit,
        _serving_commit: serving_commit,
    })
}

fn preparation_settlement_key(
    incarnation_id: &str,
    owner_epoch: i64,
    client_instance_id: &str,
    sequence: u64,
    staged_incarnation_id: &str,
) -> String {
    format!(
        "{incarnation_id}:{owner_epoch}:{client_instance_id}:{sequence}:{staged_incarnation_id}"
    )
}

impl DurablePreparationSettlementAdmission {
    fn receipt(&self) -> Option<PreparationSettlementReceipt> {
        self.receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl crate::playback_control::PreparationSettlementAdmission
    for DurablePreparationSettlementAdmission
{
    fn accepted(&self, outcome: crate::playback_control::PreparationControlOutcome) {
        let staged_incarnation_id = match &outcome.preparation_directive {
            crate::playback_control::PreparationDirective::Commit {
                staged_incarnation_id,
            }
            | crate::playback_control::PreparationDirective::Abort {
                staged_incarnation_id,
                ..
            } => staged_incarnation_id.clone(),
        };
        let Some(request_fingerprint) = self.request.fingerprint() else {
            return;
        };
        let key = preparation_settlement_key(
            &self.route.incarnation_id,
            self.route.owner_epoch,
            &self.request.client_instance_id,
            self.request.sequence,
            &staged_incarnation_id,
        );
        let mut operations = preparation_settlements()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = std::time::Instant::now();
        operations.retain(|_, operation| {
            operation
                .replay_until
                .is_none_or(|replay_until| replay_until > now)
        });
        if let Some(active) = operations.get(&key) {
            if active.request_fingerprint == request_fingerprint {
                *self
                    .receipt
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(active.receipt.clone());
            }
            return;
        }
        let Some(reservation) = self
            .reservation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            // Every request shape which can emit a directive reserves first.
            // Fail closed if a future actor path violates that invariant;
            // absence of a reservation is never evidence of durable cleanup.
            let (_sender, result) =
                tokio::sync::watch::channel(Some(PreparationSettlement::Unavailable));
            *self
                .receipt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(PreparationSettlementReceipt { result });
            return;
        };
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let receipt = PreparationSettlementReceipt { result: receiver };
        operations.insert(
            key.clone(),
            ActivePreparationSettlement {
                request_fingerprint,
                receipt: receipt.clone(),
                replay_until: None,
            },
        );
        *self
            .receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(receipt);
        drop(operations);

        let state = self.state.clone();
        let gate = Arc::clone(&self.gate);
        let route = self.route.clone();
        let start = self.start.clone();
        let recipe = self.recipe.clone();
        let request = self.request.clone();
        let status = self.status.clone();
        tokio::spawn(async move {
            let result = settle_preparation_control(
                &state,
                gate,
                &route,
                &start,
                &recipe,
                &request,
                status,
                reservation,
                outcome,
            )
            .await;
            let retain = !matches!(result, PreparationSettlement::Unavailable);
            sender.send_replace(Some(result));
            let mut operations = preparation_settlements()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if retain {
                if let Some(operation) = operations.get_mut(&key) {
                    operation.replay_until = Some(
                        std::time::Instant::now()
                            + Duration::from_millis(
                                crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS as u64,
                            ),
                    );
                }
            } else {
                operations.remove(&key);
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn settle_preparation_control(
    state: &AppState,
    gate: Arc<dyn crate::playback_control::PreparationGate>,
    route: &MediaSessionRoute,
    start: &StartResponse,
    recipe: &RemoteStartRequest,
    request: &crate::playback_control::ControlRequestV1,
    status: Option<crate::transcode::HlsSessionInfo>,
    _reservation: PreparationSettlementReservation,
    outcome: crate::playback_control::PreparationControlOutcome,
) -> PreparationSettlement {
    let executor = crate::playback_control::PreparationExecutor::new(
        Arc::clone(&state.store),
        gate,
        route.user_id,
        route.playback_id.clone(),
        route.owner_node_id.clone(),
        route.owner_epoch,
    );
    let now = tokio::time::Instant::now();
    let deadline = now + PREPARATION_SETTLEMENT_RETRY_BUDGET;
    #[cfg(test)]
    let settlement_delay = {
        preparation_settlement_delays()
            .lock()
            .expect("preparation settlement delays")
            .remove(&route.incarnation_id)
    };
    #[cfg(test)]
    if let Some(delay) = settlement_delay {
        tokio::time::sleep(delay).await;
    }
    let staged_incarnation_id = match &outcome.preparation_directive {
        crate::playback_control::PreparationDirective::Commit {
            staged_incarnation_id,
        }
        | crate::playback_control::PreparationDirective::Abort {
            staged_incarnation_id,
            ..
        } => staged_incarnation_id.as_str(),
    };
    // Take the one process-local owner before the deadline/disable/wait paths
    // can do so. From here this detached settlement task owns the exact row,
    // actor slot, and worker until it commits or explicitly aborts them.
    let active_preparation = take_active_preparation(staged_incarnation_id);
    let acknowledgement_rejected = matches!(
        &outcome.preparation_directive,
        crate::playback_control::PreparationDirective::Abort {
            acknowledgement_rejected: true,
            ..
        }
    );
    let commit_acknowledgement = if matches!(
        &outcome.preparation_directive,
        crate::playback_control::PreparationDirective::Commit { .. }
    ) {
        status.and_then(|status| {
            let response_time = unix_ms();
            let result = crate::playback_control::LocalControlResult {
                disposition: outcome.disposition,
                accepted_sequence: outcome.accepted_sequence,
                action: outcome.action.clone(),
                action_suppressed: outcome.action_suppressed,
                preparation_directive: Some(outcome.preparation_directive.clone()),
                lease_expires_at_unix_ms: outcome.lease_expires_at_unix_ms,
                lease_timeout_ms: outcome.lease_timeout_ms,
                lease_state: outcome.lease_state,
                status,
                platform: outcome.platform,
                terminal_handoff: None,
                terminal_commit: None,
                selection: outcome.selection.clone(),
            };
            let mut response =
                local_control_response(route, start, recipe, request, &result, response_time, None);
            // Answered in the receipt, not stamped on top of it afterwards. A
            // settled answer announces no successor — it carries no `Prepare`
            // action and its ask is consumed or aborted — and saying so here is
            // what makes the first answer and its own replay the same bytes.
            response.delivery.preparation = Some("none".to_owned());
            match (
                i64::try_from(request.sequence),
                request.fingerprint(),
                serde_json::to_string(&RetainedTerminalResponse {
                    platform: outcome.platform,
                    response,
                }),
            ) {
                (Ok(sequence), Some(request_fingerprint), Ok(response_json)) => {
                    Some(MediaSessionTerminalAck {
                        incarnation_id: route.incarnation_id.clone(),
                        session_id: route.session_id.clone(),
                        owner_node_id: route.owner_node_id.clone(),
                        owner_epoch: route.owner_epoch,
                        client_instance_id: request.client_instance_id.clone(),
                        sequence,
                        request_fingerprint,
                        response_json,
                        expires_at_ms: response_time
                            .saturating_add(crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS),
                        updated_at_ms: response_time,
                    })
                }
                _ => None,
            }
        })
    } else {
        None
    };
    let mut delay = PREPARATION_SETTLEMENT_RETRY_MIN;
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let settled: Result<PreparationSettlement, StoreError> = match &outcome
            .preparation_directive
        {
            crate::playback_control::PreparationDirective::Commit { .. } => {
                let planned_commit_guard =
                    match active_preparation.as_ref().map(|active| active.purpose) {
                        Some(PreparationPurpose::PlannedRelocation(fence)) => {
                            let Ok(guard) = state.serving.try_planned_outage_operation_guard()
                            else {
                                let wake = (tokio::time::Instant::now() + delay).min(deadline);
                                tokio::time::sleep_until(wake).await;
                                delay = delay
                                    .saturating_mul(2)
                                    .min(PREPARATION_SETTLEMENT_RETRY_MAX);
                                continue;
                            };
                            if !state.serving.planned_outage_is_current(fence).await {
                                if let Some(active) = active_preparation.clone() {
                                    settle_cancelled_preparation(
                                        active,
                                        "planned relocation expired before commit",
                                    )
                                    .await;
                                } else if executor
                                    .reject_commit(staged_incarnation_id, unix_ms())
                                    .await
                                    .is_ok_and(|rejected| rejected)
                                {
                                    retire_prepared_incarnation(
                                        state,
                                        staged_incarnation_id,
                                        "planned relocation expired before commit",
                                    )
                                    .await;
                                }
                                return PreparationSettlement::Rejected;
                            }
                            Some(guard)
                        }
                        _ => None,
                    };
                if active_preparation.is_none()
                    && !planned_preparation_still_current(state, staged_incarnation_id).await
                {
                    match executor
                        .reject_commit(staged_incarnation_id, unix_ms())
                        .await
                    {
                        Ok(rejected) => {
                            if rejected {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "planned relocation expired before commit",
                                )
                                .await;
                            }
                            return PreparationSettlement::Rejected;
                        }
                        Err(_) => return PreparationSettlement::Unavailable,
                    }
                }
                #[cfg(test)]
                if consume_preparation_settlement_fault(&route.incarnation_id) {
                    let wake = (tokio::time::Instant::now() + delay).min(deadline);
                    tokio::time::sleep_until(wake).await;
                    delay = delay
                        .saturating_mul(2)
                        .min(PREPARATION_SETTLEMENT_RETRY_MAX);
                    continue;
                }
                if let Some(acknowledgement) = commit_acknowledgement.clone() {
                    let committed = executor
                        .commit(
                            staged_incarnation_id,
                            unix_ms(),
                            outcome.lease_expires_at_unix_ms,
                            Some(acknowledgement),
                        )
                        .await;
                    // A cancellation/exit request needs this same gate, so it
                    // cannot clear the exact fence between the predicate above
                    // and the durable pointer CAS.
                    drop(planned_commit_guard);
                    match committed {
                        Ok(crate::playback_control::PreparationCommitOutcome::Committed(
                            commit,
                        ))
                        | Ok(crate::playback_control::PreparationCommitOutcome::Replayed(commit)) =>
                        {
                            settle_committed_preparation(state, &commit).await;
                            Ok(commit
                                .control_receipt
                                .filter(|receipt| {
                                    commit_acknowledgement.as_ref().is_some_and(|expected| {
                                        terminal_ack_matches(receipt, expected)
                                    })
                                })
                                .and_then(|receipt| {
                                    serde_json::from_str::<RetainedTerminalResponse>(
                                        &receipt.response_json,
                                    )
                                    .ok()
                                })
                                .map(|retained| {
                                    PreparationSettlement::Committed(Box::new(retained.response))
                                })
                                .unwrap_or(PreparationSettlement::Unavailable))
                        }
                        Ok(crate::playback_control::PreparationCommitOutcome::Refused) => {
                            if let Some(active) = active_preparation.as_ref() {
                                retire_prepared_worker(
                                    state,
                                    &active.preparation.owner_node_id,
                                    staged_incarnation_id,
                                    &active.preparation.session_id,
                                    "prepared successor commit refused",
                                )
                                .await;
                            } else {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "prepared successor commit refused",
                                )
                                .await;
                            }
                            Ok(PreparationSettlement::Rejected)
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    // A status snapshot is needed only to construct the exact
                    // successful response. If it disappeared after actor
                    // acceptance, convert the reserved commit to an abort and
                    // durably clean it up instead of leaving Committing stuck.
                    match executor
                        .reject_commit(staged_incarnation_id, unix_ms())
                        .await
                    {
                        Ok(rejected) => {
                            if rejected {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "prepared successor commit response unavailable",
                                )
                                .await;
                            }
                            Ok(PreparationSettlement::Unavailable)
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            crate::playback_control::PreparationDirective::Abort { .. } => {
                match executor.abort(staged_incarnation_id, unix_ms()).await {
                    Ok(aborted) => {
                        if aborted {
                            if let Some(active) = active_preparation.as_ref() {
                                retire_prepared_worker(
                                    state,
                                    &active.preparation.owner_node_id,
                                    staged_incarnation_id,
                                    &active.preparation.session_id,
                                    "prepared successor aborted",
                                )
                                .await;
                            } else {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "prepared successor aborted",
                                )
                                .await;
                            }
                        }
                        Ok(if !aborted {
                            PreparationSettlement::Unavailable
                        } else if acknowledgement_rejected {
                            PreparationSettlement::Rejected
                        } else {
                            PreparationSettlement::Aborted
                        })
                    }
                    Err(error) => Err(error),
                }
            }
        };
        match settled {
            Ok(result) => return result,
            Err(_) => {
                let wake = (tokio::time::Instant::now() + delay).min(deadline);
                tokio::time::sleep_until(wake).await;
                delay = delay
                    .saturating_mul(2)
                    .min(PREPARATION_SETTLEMENT_RETRY_MAX);
            }
        }
    }
    if matches!(
        &outcome.preparation_directive,
        crate::playback_control::PreparationDirective::Commit { .. }
    ) && executor
        .reject_commit(staged_incarnation_id, unix_ms())
        .await
        .is_ok_and(|rejected| rejected)
    {
        retire_prepared_incarnation(
            state,
            staged_incarnation_id,
            "prepared successor commit settlement expired",
        )
        .await;
    }
    if let Some(active) = active_preparation {
        settle_cancelled_preparation(active, "prepared successor settlement expired").await;
    }
    PreparationSettlement::Unavailable
}

async fn planned_preparation_still_current(state: &AppState, incarnation_id: &str) -> bool {
    let Ok(Some(route)) = state
        .store
        .media_session_route_by_incarnation(incarnation_id)
        .await
    else {
        return false;
    };
    let Ok(response) = serde_json::from_str::<serde_json::Value>(&route.response_json) else {
        return false;
    };
    let reason = response
        .get(PREPARATION_REASON_RESPONSE_FIELD)
        .and_then(serde_json::Value::as_str);
    let Some(reason) = reason else {
        return true;
    };
    state
        .serving
        .current_planned_outage()
        .await
        .is_some_and(|fence| reason == format!("planned_relocation:{}", fence.identity()))
}

async fn retire_prepared_incarnation(
    state: &AppState,
    staged_incarnation_id: &str,
    reason: &'static str,
) {
    let route = tokio::time::timeout(
        PREPARATION_STORE_BUDGET,
        state
            .store
            .media_session_route_by_incarnation(staged_incarnation_id),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .flatten()
    .filter(|route| route.state == "ended" && route.terminal_reason.as_deref() == Some("replaced"));
    if let Some(route) = route {
        retire_prepared_worker(
            state,
            &route.owner_node_id,
            &route.incarnation_id,
            &route.session_id,
            reason,
        )
        .await;
    }
}

/// Project the exact predecessor once its intentional drain retires it, then
/// clear a committed successor's control publication fence. If the fast proof
/// cannot be obtained, arm the existing full response-lifetime safety
/// boundary; the lease reconciler also resumes that durable state after a
/// crash.
async fn settle_committed_preparation(
    state: &AppState,
    commit: &plurx_core::domain::MediaSessionPreparationCommit,
) {
    let successor = commit.route.clone();
    state
        .transcode
        .promote_prepared_session(&successor.session_id)
        .await;
    state.media_sessions.cache_route(successor.clone()).await;
    if successor.owner_node_id == state.node_id {
        state.media_sessions.seed_owned_lease(&successor).await;
    }
    if successor.publication_ready_at_ms == 0 {
        return;
    }
    let authority = state.serving.authority();
    let Some(admitted_generation) = authority.admit() else {
        return;
    };
    let predecessor = commit
        .predecessor
        .as_ref()
        .map(|route| route.incarnation_id.clone());
    let state = state.clone();
    tokio::spawn(async move {
        publish_committed_preparation(
            state,
            successor,
            predecessor,
            authority,
            admitted_generation,
        )
        .await;
    });
}

/// Continue publication independently of the four-second control exchange.
/// The commit is already durable and replayable; making its response wait for
/// a draining predecessor would turn success into a false 503. Prepared media
/// remains authorized by its durable response marker and current pointer while
/// this task proves the control-plane publication boundary.
async fn publish_committed_preparation(
    state: AppState,
    successor: MediaSessionRoute,
    predecessor: Option<String>,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
) {
    if let Some(predecessor_incarnation) = predecessor.as_ref() {
        let deadline = tokio::time::Instant::now() + PREDECESSOR_PROJECTION_FAST_WINDOW;
        if project_activation_predecessor_until(&state, predecessor_incarnation, deadline).await
            && matches!(
                complete_activation_handoff_until(
                    &state,
                    &successor,
                    MediaSessionProjectionCompletion::PredecessorAcknowledged,
                    &authority,
                    admitted_generation,
                    deadline,
                )
                .await,
                ActivationHandoffVerdict::Ready | ActivationHandoffVerdict::SuccessorGone
            )
        {
            return;
        }
    }

    let now_ms = unix_ms();
    let not_before_ms = now_ms.saturating_add(
        i64::try_from(TERMINAL_PROJECTION_SAFETY_WINDOW.as_millis()).unwrap_or(i64::MAX),
    );
    let arm_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
    let Some(_guard) = authority
        .commit_guard_before(admitted_generation, arm_deadline.into_std())
        .await
    else {
        return;
    };
    let armed = tokio::time::timeout_at(
        arm_deadline,
        state.store.arm_media_session_handoff(
            &successor.incarnation_id,
            &successor.owner_node_id,
            successor.owner_epoch,
            not_before_ms,
            now_ms,
        ),
    )
    .await;
    let Ok(Ok(Some(armed))) = armed else {
        return;
    };
    drop(_guard);
    state.media_sessions.cache_route(armed.clone()).await;
    if let Some(predecessor_incarnation) = predecessor {
        let _ = settle_armed_activation_handoff(
            &state,
            predecessor_incarnation,
            armed,
            authority,
            admitted_generation,
        )
        .await;
    }
}

struct DurableTerminalCommitter {
    store: Arc<dyn plurx_core::store::Store>,
    route: MediaSessionRoute,
    start: StartResponse,
    recipe: RemoteStartRequest,
    request: crate::playback_control::ControlRequestV1,
    faults: Option<Arc<TerminalCommitFaults>>,
}

const TERMINAL_COMMIT_RETRY_BUDGET: Duration = Duration::from_secs(5);
const TERMINAL_COMMIT_RETRY_MIN: Duration = Duration::from_millis(25);
const TERMINAL_COMMIT_RETRY_MAX: Duration = Duration::from_millis(500);

fn terminal_ack_matches(
    stored: &MediaSessionTerminalAck,
    acknowledgement: &MediaSessionTerminalAck,
) -> bool {
    stored.incarnation_id == acknowledgement.incarnation_id
        && stored.session_id == acknowledgement.session_id
        && stored.owner_node_id == acknowledgement.owner_node_id
        && stored.owner_epoch == acknowledgement.owner_epoch
        && stored.client_instance_id == acknowledgement.client_instance_id
        && stored.sequence == acknowledgement.sequence
        && stored.request_fingerprint == acknowledgement.request_fingerprint
        && stored.response_json == acknowledgement.response_json
}

async fn terminal_io_before<T>(
    deadline: tokio::time::Instant,
    operation: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::time::timeout_at(deadline, operation).await.ok()
}

async fn terminal_ack_is_visible_before(
    store: &dyn plurx_core::store::Store,
    acknowledgement: &MediaSessionTerminalAck,
    deadline: tokio::time::Instant,
) -> bool {
    terminal_io_before(
        deadline,
        store.media_session_terminal_ack(&acknowledgement.session_id, unix_ms()),
    )
    .await
    .and_then(Result::ok)
    .flatten()
    .as_ref()
    .is_some_and(|stored| terminal_ack_matches(stored, acknowledgement))
}

#[derive(Default)]
struct TerminalCommitFaults {
    fail_before_commit: std::sync::atomic::AtomicUsize,
    fail_after_commit: std::sync::atomic::AtomicUsize,
}

impl TerminalCommitFaults {
    fn consume(counter: &std::sync::atomic::AtomicUsize) -> bool {
        counter
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
    }
}

async fn persist_terminal_ack(
    store: Arc<dyn plurx_core::store::Store>,
    acknowledgement: MediaSessionTerminalAck,
) -> bool {
    persist_terminal_ack_with_faults(store, acknowledgement, None).await
}

async fn persist_terminal_ack_with_faults(
    store: Arc<dyn plurx_core::store::Store>,
    acknowledgement: MediaSessionTerminalAck,
    faults: Option<&TerminalCommitFaults>,
) -> bool {
    let remaining_ms = acknowledgement.expires_at_ms.saturating_sub(unix_ms());
    let Some(remaining_ms) = u64::try_from(remaining_ms)
        .ok()
        .filter(|remaining| *remaining > 0)
    else {
        return false;
    };
    let now = tokio::time::Instant::now();
    let acknowledgement_deadline = now
        .checked_add(Duration::from_millis(remaining_ms))
        .unwrap_or(now);
    let deadline = (now + TERMINAL_COMMIT_RETRY_BUDGET).min(acknowledgement_deadline);
    let mut delay = TERMINAL_COMMIT_RETRY_MIN;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let fail_before =
            faults.is_some_and(|faults| TerminalCommitFaults::consume(&faults.fail_before_commit));
        let write = if fail_before {
            Some(Err(()))
        } else {
            let write = match terminal_io_before(
                deadline,
                store.record_media_session_terminal_ack(&acknowledgement),
            )
            .await
            {
                Some(Ok(persisted)) => Ok(persisted),
                Some(Err(_)) => Err(()),
                None => return false,
            };
            let fail_after = write == Ok(true)
                && faults
                    .is_some_and(|faults| TerminalCommitFaults::consume(&faults.fail_after_commit));
            Some(if fail_after { Err(()) } else { write })
        };
        match write {
            Some(Ok(true)) => return true,
            Some(Ok(false)) => {
                // `false` is normally a definitive route/identity conflict,
                // but first resolve an earlier unknown commit of these exact
                // immutable bytes.
                return terminal_ack_is_visible_before(store.as_ref(), &acknowledgement, deadline)
                    .await;
            }
            Some(Err(_))
                if terminal_ack_is_visible_before(store.as_ref(), &acknowledgement, deadline)
                    .await =>
            {
                // The write may have committed before its answer was lost.
                // Read-after-unknown prevents a second outcome from replacing
                // the accepted terminal response.
                return true;
            }
            Some(Err(_)) => {}
            None => unreachable!("write outcome is always classified"),
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let wake = now.checked_add(delay).unwrap_or(deadline).min(deadline);
        tokio::time::sleep_until(wake).await;
        delay = delay.saturating_mul(2).min(TERMINAL_COMMIT_RETRY_MAX);
    }
}

impl crate::playback_control::TerminalControlCommitter for DurableTerminalCommitter {
    fn start(
        &self,
        result: &crate::playback_control::LocalControlResult,
    ) -> crate::playback_control::TerminalCommitReceipt {
        let server_time_unix_ms = unix_ms();
        let mut response = local_control_response(
            &self.route,
            &self.start,
            &self.recipe,
            &self.request,
            result,
            server_time_unix_ms,
            // The session is ending. Subtitle readiness is a fact about a
            // stream that will keep serving segments, and this one will not;
            // absent is the honest answer, and this trait method cannot await
            // a probe anyway.
            None,
        );
        // Answered in the receipt, not stamped on top of it afterwards. A
        // settled answer announces no successor — it carries no `Prepare`
        // action and its ask is consumed or aborted — and saying so here is
        // what makes the first answer and its own replay the same bytes.
        response.delivery.preparation = Some("none".to_owned());
        // Persist the outcome before the commit machinery, because a terminal
        // is the thing most worth explaining after a restart and the atomics
        // that count it do not survive one. `durable_outcome` decides what
        // deserves a row; this only carries it to the store.
        //
        // Not awaited and not fatal: `emit` spawns, and a telemetry write that
        // fails must not turn a terminal the viewer's client is waiting on
        // into an error. Losing the row is worse than not having it only if
        // the alternative is losing the terminal.
        {
            let metrics = crate::playback_control::action_metrics(
                &response.action,
                &response.delivery,
                &self.request,
                result.action_suppressed,
            );
            if let Some(event) = crate::playback_control::durable_outcome(
                &response.action,
                &response.delivery,
                &self.request,
                &metrics,
                server_time_unix_ms,
            ) {
                crate::telemetry::emit(Arc::clone(&self.store), event);
            }
        }
        let response_json = serde_json::to_string(&RetainedTerminalResponse {
            platform: result.platform,
            response: response.clone(),
        });
        let identity = (
            i64::try_from(self.request.sequence),
            self.request.fingerprint(),
        );
        let handoff = result.terminal_handoff.clone();
        let (Ok(response_json), (Ok(sequence), Some(request_fingerprint))) =
            (response_json, identity)
        else {
            let (receipt, sender) = crate::playback_control::TerminalCommitReceipt::pending();
            if let Some(handoff) = handoff {
                handoff.complete();
            }
            let _ = sender.send(Some(Err(())));
            return receipt;
        };
        let acknowledgement = MediaSessionTerminalAck {
            incarnation_id: self.route.incarnation_id.clone(),
            session_id: self.route.session_id.clone(),
            owner_node_id: self.route.owner_node_id.clone(),
            owner_epoch: self.route.owner_epoch,
            client_instance_id: self.request.client_instance_id.clone(),
            sequence,
            request_fingerprint,
            response_json,
            expires_at_ms: server_time_unix_ms
                .saturating_add(crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS),
            updated_at_ms: server_time_unix_ms,
        };
        let store = Arc::clone(&self.store);
        let faults = self.faults.clone();
        // A terminal exchange on a *draining* predecessor is not receipted,
        // and that is the whole handling.
        //
        // The receipt table holds one row per `session_id`, and on a draining
        // predecessor that row is already the commit's: the prepared commit
        // runs on this route and retains its reply here. Until the drain, no
        // second receipted exchange could reach a predecessor, because the
        // commit ended it in the same transaction. Now the viewer closing the
        // player sends `demand: end` to a session that is still live, and a
        // write that loses to the commit receipt reads back as "not durably
        // committed" — a 503 the client retries twice a second for the rest
        // of the window.
        //
        // Ending the row is a better answer than a receipt anyway. The
        // retained reply exists so a lost terminal answer can be replayed;
        // once this row is `ended`, a client that missed the reply asks again
        // and gets `410 session_ended` from the row itself, which is the same
        // terminal by a shorter path. So the drain ends here, early, and the
        // commit keeps the receipt slot it needs for its own replay.
        //
        // `end_media_session` rather than the owner-scoped CAS: the lease
        // boundary on this route was read when the request arrived and the
        // owner's tick renews every three seconds, so an exact-boundary CAS
        // would lose a race it has no reason to enter. The pointer delete
        // inside it is guarded on this exact incarnation, and the pointer
        // names the successor, so ending the predecessor cannot take it.
        let draining = self.route.drain_deadline_ms.is_some();
        let session_id = self.route.session_id.clone();
        crate::playback_control::TerminalCommitReceipt::retryable_until(
            acknowledgement.expires_at_ms,
            move |attempt| {
                let store = Arc::clone(&store);
                let acknowledgement = acknowledgement.clone();
                let response = response.clone();
                let handoff = handoff.clone();
                let faults = faults.clone();
                let session_id = session_id.clone();
                if let Some(handoff) = &handoff {
                    handoff.restart();
                }
                tokio::spawn(async move {
                    let persisted = if draining {
                        matches!(
                            store
                                .end_media_session(&session_id, "superseded", unix_ms())
                                .await,
                            Ok(Some(_))
                        )
                    } else {
                        match faults {
                            Some(faults) => {
                                persist_terminal_ack_with_faults(
                                    store,
                                    acknowledgement,
                                    Some(faults.as_ref()),
                                )
                                .await
                            }
                            None => persist_terminal_ack(store, acknowledgement).await,
                        }
                    };
                    if let Some(handoff) = handoff {
                        handoff.complete();
                    }
                    attempt.complete(if persisted { Ok(response) } else { Err(()) });
                });
            },
        )
    }
}

pub(crate) struct TerminalAckReplay {
    response: crate::playback_control::ControlResponseV1,
    platform: Option<crate::playback_control::ClientPlatform>,
}

pub(crate) async fn terminal_ack_replay(
    state: &AppState,
    route: &MediaSessionRoute,
    request: &crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Result<Option<TerminalAckReplay>, ()> {
    if request.demand != crate::playback_control::PlaybackDemand::End {
        return Ok(None);
    }
    let Some(acknowledgement) = state
        .store
        .media_session_terminal_ack(&route.session_id, unix_ms())
        .await
        .map_err(|_| ())?
    else {
        return Ok(None);
    };
    if acknowledgement.incarnation_id != route.incarnation_id
        || acknowledgement.owner_node_id != route.owner_node_id
        || acknowledgement.owner_epoch != route.owner_epoch
        || acknowledgement.client_instance_id != request.client_instance_id
        || u64::try_from(acknowledgement.sequence).ok() != Some(request.sequence)
        || request.fingerprint().as_deref() != Some(acknowledgement.request_fingerprint.as_str())
    {
        return Ok(None);
    }
    let relay = crate::playback_control::ControlRelayRequest {
        session_id: route.session_id.clone(),
        generation: route.incarnation_id.clone(),
        expected_owner_node_id: route.owner_node_id.clone(),
        expected_owner_epoch: route.owner_epoch,
        deadline_unix_ms,
        control: request.clone(),
    };
    let retained = serde_json::from_str::<RetainedTerminalResponse>(&acknowledgement.response_json)
        .map(|retained| TerminalAckReplay {
            response: retained.response,
            platform: Some(retained.platform),
        })
        // Compatibility with terminal acknowledgements written by an earlier M3
        // build. New writes always retain platform independently of a retry's
        // optional capabilities.
        .or_else(|_| {
            serde_json::from_str::<crate::playback_control::ControlResponseV1>(
                &acknowledgement.response_json,
            )
            .map(|response| TerminalAckReplay {
                response,
                platform: request.capabilities.as_ref().map(|caps| caps.platform),
            })
        })
        .map_err(|_| ())?;
    retained
        .response
        .is_valid_for(&relay)
        .then_some(retained)
        .map(Some)
        .ok_or(())
}

pub(crate) async fn preparation_ack_replay(
    state: &AppState,
    route: &MediaSessionRoute,
    request: &crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Result<Option<TerminalAckReplay>, ()> {
    // Gated on the receipt, not on the route's state.
    //
    // The state check was the same fact by a different name — the commit
    // retires the predecessor in its own transaction, so `ended`/`superseded`
    // and "this commit already happened" were one condition. §4 has to break
    // that pairing: a predecessor a client may still be displaying cannot be
    // retired at commit, and the moment it is not, a client whose commit
    // response was lost would be answered by the live control plane instead of
    // by the exact durable answer — the one case §4 says must always replay.
    //
    // The receipt is a stricter fence than the state was. Its two writers are
    // the commit transaction and `record_media_session_terminal_ack`, which
    // ends the session in the same transaction, and every field of the
    // exchange is compared below: incarnation, owner, epoch, client instance,
    // sequence and request fingerprint. Only the byte-identical request that
    // produced a receipt can replay it, so a live serving session cannot have
    // a real exchange short-circuited.
    if !matches!(
        request.acknowledgement.as_ref().map(|ack| ack.state),
        Some(crate::playback_control::AcknowledgementState::Committed)
    ) {
        return Ok(None);
    }
    let Some(receipt) = state
        .store
        .media_session_terminal_ack(&route.session_id, unix_ms())
        .await
        .map_err(|_| ())?
    else {
        return Ok(None);
    };
    if receipt.incarnation_id != route.incarnation_id
        || receipt.owner_node_id != route.owner_node_id
        || receipt.owner_epoch != route.owner_epoch
        || receipt.client_instance_id != request.client_instance_id
        || u64::try_from(receipt.sequence).ok() != Some(request.sequence)
        || request.fingerprint().as_deref() != Some(receipt.request_fingerprint.as_str())
    {
        return Ok(None);
    }
    let retained =
        serde_json::from_str::<RetainedTerminalResponse>(&receipt.response_json).map_err(|_| ())?;
    let relay = crate::playback_control::ControlRelayRequest {
        session_id: route.session_id.clone(),
        generation: route.incarnation_id.clone(),
        expected_owner_node_id: route.owner_node_id.clone(),
        expected_owner_epoch: route.owner_epoch,
        deadline_unix_ms,
        control: request.clone(),
    };
    retained
        .response
        .is_valid_for(&relay)
        .then_some(TerminalAckReplay {
            response: retained.response,
            platform: Some(retained.platform),
        })
        .map(Some)
        .ok_or(())
}

pub(crate) fn terminal_ack_response(replay: TerminalAckReplay) -> Response {
    crate::playback_control::record(crate::playback_control::MetricOutcome::Replay);
    if let Some(platform) = replay.platform {
        crate::playback_control::record_platform(
            crate::playback_control::MetricOutcome::Replay,
            platform,
        );
    }
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(replay.response),
    )
        .into_response()
}

async fn control_inner(
    state: AppState,
    session: String,
    body: Bytes,
    deadline_unix_ms: i64,
) -> Response {
    if uuid::Uuid::parse_str(&session).is_err() {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
        return control_error(
            StatusCode::NOT_FOUND,
            "session_gone",
            "no active or durable media session has this capability",
            None,
            None,
            None,
            None,
        );
    }
    let request = match serde_json::from_slice::<crate::playback_control::ControlRequestV1>(&body) {
        Ok(request) => request,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Invalid);
            return control_error(
                StatusCode::BAD_REQUEST,
                "invalid_control",
                "the control body is not valid protocol v1 JSON",
                None,
                None,
                None,
                None,
            );
        }
    };
    let route = match state.media_sessions.control_route(&session).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "no active or durable media session has this capability",
                None,
                None,
                None,
                None,
            );
        }
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable media route is temporarily unavailable",
                None,
                None,
                Some(500),
                None,
            );
        }
    };
    if let Some(refusal) = library_channel_control_refusal(&state, &route).await {
        return refusal;
    }
    let owner_epoch = match u64::try_from(route.owner_epoch)
        .ok()
        .filter(|epoch| *epoch > 0)
    {
        Some(epoch) => epoch,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable media route has an invalid owner epoch",
                Some(route.incarnation_id),
                None,
                Some(500),
                None,
            );
        }
    };
    let start = match control_start_response(&route) {
        Some(start) => start,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "playback control was not advertised for this session",
                None,
                None,
                None,
                None,
            );
        }
    };
    let recipe = match serde_json::from_str::<RemoteStartRequest>(&route.recipe_json) {
        Ok(recipe) => recipe,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable delivery recipe is unreadable",
                Some(route.incarnation_id),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    };
    if let Err(field) = request.validate(
        start.duration_ms,
        crate::playback_control::target_duration_ms(&recipe),
    ) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Invalid);
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "a control field is outside the bounded v1 contract",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            Some(field),
        );
    }
    if request.generation != route.incarnation_id {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
        return control_error(
            StatusCode::CONFLICT,
            "stale_control",
            "the control generation is no longer current",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            None,
        );
    }
    if request.control_epoch != owner_epoch {
        crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
        return control_error(
            StatusCode::CONFLICT,
            "owner_changed",
            "the media session owner epoch changed",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            None,
        );
    }
    match terminal_ack_replay(&state, &route, &request, deadline_unix_ms).await {
        Ok(Some(replay)) => return terminal_ack_response(replay),
        Ok(None) => {}
        Err(()) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the terminal control acknowledgement is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    }
    match preparation_ack_replay(&state, &route, &request, deadline_unix_ms).await {
        Ok(Some(replay)) => return terminal_ack_response(replay),
        Ok(None) => {}
        Err(()) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the preparation acknowledgement replay is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    }
    if let Err(retry_after_ms) = state.media_sessions.admit_control(&session) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::RateLimited);
        return control_error(
            StatusCode::TOO_MANY_REQUESTS,
            "control_rate_limited",
            "the ingress control budget is exhausted",
            None,
            None,
            Some(retry_after_ms),
            None,
        );
    }
    if route.state != "active" {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
        return control_error(
            StatusCode::GONE,
            "session_ended",
            "this media session has ended or been superseded",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            None,
        );
    }
    if let Some(refusal) = control_owner_refusal(&route, Some(owner_epoch)) {
        return refusal;
    }
    if route.owner_node_id != state.node_id {
        let relay = crate::playback_control::ControlRelayRequest {
            session_id: session.clone(),
            generation: route.incarnation_id.clone(),
            expected_owner_node_id: route.owner_node_id.clone(),
            expected_owner_epoch: route.owner_epoch,
            deadline_unix_ms,
            control: request,
        };
        return match state
            .media_sessions
            .control(&route.owner_node_id, &relay)
            .await
        {
            Ok(response) => response,
            Err(_) => {
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "the owning media worker did not answer the control exchange",
                    Some(route.incarnation_id),
                    Some(owner_epoch),
                    Some(500),
                    None,
                )
            }
        };
    }
    control_local(&state, &route, request, deadline_unix_ms).await
}

/// Revalidate a durable following purpose at every normal control exchange.
/// The session UUID is still the bearer capability, but it cannot keep a
/// deleted, disabled, or newly-hidden channel alive. Ordinary VOD response
/// JSON has no `library_channel` member and pays only one object lookup.
async fn library_channel_control_refusal(
    state: &AppState,
    route: &MediaSessionRoute,
) -> Option<Response> {
    let response = match serde_json::from_str::<serde_json::Value>(&route.response_json) {
        Ok(response) => response,
        Err(_) => return None, // The ordinary response parser reports this below.
    };
    let raw_purpose = response.get("library_channel")?;
    let purpose = match serde_json::from_value::<
        crate::http::library_channels::LibraryChannelPlaybackPurpose,
    >(raw_purpose.clone())
    {
        Ok(purpose) => purpose,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return Some(control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable Library-channel purpose is unreadable",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                Some(500),
                None,
            ));
        }
    };
    let runtime_enabled = match state
        .store
        .get_setting(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED)
        .await
    {
        Ok(value) => plurx_core::store::stored_switch(value.as_deref(), false),
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return Some(control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "Library-channel authority is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                Some(500),
                None,
            ));
        }
    };
    if !runtime_enabled {
        return Some(control_error(
            StatusCode::GONE,
            "channel_unavailable",
            "Library-channel playback was disabled",
            Some(route.incarnation_id.clone()),
            u64::try_from(route.owner_epoch).ok(),
            None,
            None,
        ));
    }
    let user = match state.store.get_user(route.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return Some(control_error(
                StatusCode::GONE,
                "channel_unavailable",
                "the Library-channel viewer no longer exists",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                None,
                None,
            ));
        }
        Err(_) => {
            return Some(control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "Library-channel authorization is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                Some(500),
                None,
            ));
        }
    };
    match state
        .store
        .get_library_channel(user.id, false, &purpose.channel_id)
        .await
    {
        Ok(Some(channel)) if channel.enabled => None,
        Ok(_) => Some(control_error(
            StatusCode::GONE,
            "channel_unavailable",
            "the Library channel was deleted, disabled, or is no longer visible",
            Some(route.incarnation_id.clone()),
            u64::try_from(route.owner_epoch).ok(),
            None,
            None,
        )),
        Err(_) => Some(control_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_unavailable",
            "Library-channel authorization is temporarily unavailable",
            Some(route.incarnation_id.clone()),
            u64::try_from(route.owner_epoch).ok(),
            Some(500),
            None,
        )),
    }
}

fn control_start_response(route: &MediaSessionRoute) -> Option<StartResponse> {
    serde_json::from_str::<StartResponse>(&route.response_json)
        .ok()
        .filter(|response| response.control.is_some())
}

#[cfg(test)]
fn staged_read_faults() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static FAULTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
fn fail_next_staged_read(incarnation_id: &str) {
    staged_read_faults()
        .lock()
        .expect("staged read faults")
        .insert(incarnation_id.to_owned());
}

#[cfg(test)]
fn preparation_settlement_delays(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Duration>> {
    static DELAYS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Duration>>,
    > = std::sync::OnceLock::new();
    DELAYS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn delay_next_preparation_settlement(incarnation_id: &str, delay: Duration) {
    preparation_settlement_delays()
        .lock()
        .expect("preparation settlement delays")
        .insert(incarnation_id.to_owned(), delay);
}

/// Test-only seam at a preparation candidate's planning step.
///
/// `plan_preparation_candidate` is the last thing a candidate does before it
/// can reach either the ledger or the registry, so the window in which a
/// client's exchange finds neither is the one this widens far enough to drive
/// real exchanges through. The refusal half stands in for a candidate that
/// turns out to be unplannable, without needing a source row contrived to make
/// the planner fail for some unrelated reason.
#[cfg(test)]
fn preparation_planning_faults(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (Duration, bool)>> {
    static FAULTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, (Duration, bool)>>,
    > = std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn fault_preparation_planning(playback_id: &str, delay: Duration, refuse: bool) {
    preparation_planning_faults()
        .lock()
        .expect("preparation planning faults")
        .insert(playback_id.to_owned(), (delay, refuse));
}

#[cfg(test)]
fn take_preparation_planning_fault(playback_id: &str) -> Option<(Duration, bool)> {
    preparation_planning_faults()
        .lock()
        .expect("preparation planning faults")
        .remove(playback_id)
}

/// Test-only seam immediately before `register_active_preparation`.
///
/// Production has no suspension point between a staging task's last await and
/// that registration, which is exactly why the supersession flag is read once
/// more *after* it. A test cannot land a supersession inside a window that does
/// not exist, so this makes one: the task parks here, the ordinary
/// `cancel_preparations_for_superseded_predecessor` runs, and the read after
/// registration is then the only thing in the process that can still see it.
#[cfg(test)]
fn preparation_registration_delays(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Duration>> {
    static DELAYS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Duration>>,
    > = std::sync::OnceLock::new();
    DELAYS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn delay_preparation_registration(playback_id: &str, delay: Duration) {
    preparation_registration_delays()
        .lock()
        .expect("preparation registration delays")
        .insert(playback_id.to_owned(), delay);
}

#[cfg(test)]
fn take_preparation_registration_delay(playback_id: &str) -> Option<Duration> {
    preparation_registration_delays()
        .lock()
        .expect("preparation registration delays")
        .remove(playback_id)
}

/// Test-only seam between a dispatch and the answer that same exchange gives.
///
/// The first `staging` clause covers a race the emit rule has with the task it
/// just spawned: the candidate can finish, and its guard drop, before the emit
/// rule reads the pending map. In production that window is a few instructions
/// wide and cannot be widened from outside. Arming this makes the exchange wait
/// for exactly that to have happened, which is the only way the clause is under
/// test rather than merely present.
#[cfg(test)]
fn dispatch_settle_waits() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static WAITS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    WAITS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
fn wait_for_the_dispatched_candidate(playback_id: &str) {
    dispatch_settle_waits()
        .lock()
        .expect("dispatch settle waits")
        .insert(playback_id.to_owned());
}

#[cfg(test)]
fn take_dispatch_settle_wait(playback_id: &str) -> bool {
    dispatch_settle_waits()
        .lock()
        .expect("dispatch settle waits")
        .remove(playback_id)
}

/// Resolve the staged successor this predecessor may announce now.
///
/// The ledger is the wanted-work authority: a route at the publication
/// sentinel is not enough, because commit and abort both remove the ledger
/// row. The owner-local [`crate::playback_control::ControlState`] checks the
/// matching preparation slot once more when it accepts the exchange; this
/// read therefore proposes an action but cannot create a second authority.
async fn staged_successor_action(
    state: &AppState,
    predecessor: &MediaSessionRoute,
    staged: Option<plurx_core::domain::MediaSessionStagedGeneration>,
) -> Result<Option<crate::playback_control::PreparedSuccessorAction>, ()> {
    let Some(staged) = staged else {
        return Ok(None);
    };
    if staged.expected_predecessor_incarnation_id != predecessor.incarnation_id {
        tracing::warn!("staged playback generation names the wrong predecessor");
        return Err(());
    }
    if staged.deadline_ms <= unix_ms() {
        return Ok(None);
    }
    let successor = state
        .store
        .media_session_route_by_incarnation(&staged.staged_incarnation_id)
        .await
        .map_err(|error| {
            tracing::warn!(?error, "staged media-session route read failed");
        })?
        .ok_or_else(|| {
            tracing::warn!("staged playback generation has no media-session route");
        })?;
    if successor.incarnation_id != staged.staged_incarnation_id
        || successor.user_id != predecessor.user_id
        || successor.playback_id != predecessor.playback_id
        || successor.owner_node_id != state.node_id
        || successor.owner_epoch != 1
        || successor.state != "active"
        || successor.publication_ready_at_ms
            != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
    {
        tracing::warn!("staged media-session route failed its authority checks");
        return Err(());
    }
    let start = control_start_response(&successor).ok_or_else(|| {
        tracing::warn!("staged media-session response is unreadable");
    })?;
    let recipe =
        serde_json::from_str::<RemoteStartRequest>(&successor.recipe_json).map_err(|_| {
            tracing::warn!("staged media-session recipe is unreadable");
        })?;
    if !recipe.is_valid()
        || recipe.incarnation_id != successor.incarnation_id
        || recipe.user_id != successor.user_id
        || recipe.request.playback_id != successor.playback_id
        || start.session_id != successor.session_id
        || start.media_origin_ms != Some(successor.media_origin_ms)
        || !crate::playback_control::is_node_relative_playlist(
            &start.playlist_url,
            &successor.session_id,
        )
    {
        tracing::warn!("staged media-session payload failed its identity checks");
        return Err(());
    }
    let prepared = crate::playback_control::PreparedSuccessorAction {
        staged_incarnation_id: staged.staged_incarnation_id,
        deadline_ms: staged.deadline_ms,
        session_id: successor.session_id,
        playlist_url: start.playlist_url,
        media_origin_ms: successor.media_origin_ms,
        effective_selection: crate::playback_control::EffectiveSelection::from_recipe(
            &recipe,
            start.height,
            start.delivered_dynamic_range,
        ),
    };
    if !crate::playback_control::prepared_payload_is_valid(
        None,
        &prepared.session_id,
        &prepared.playlist_url,
        prepared.media_origin_ms,
        &prepared.effective_selection,
    ) {
        tracing::warn!("staged media-session action payload is invalid");
        return Err(());
    }
    Ok(Some(prepared))
}

pub(crate) fn control_error(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
    generation: Option<String>,
    control_epoch: Option<u64>,
    retry_after_ms: Option<u32>,
    invalid_field: Option<&'static str>,
) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(crate::playback_control::ControlErrorBody {
            code: code.to_owned(),
            message: message.into(),
            generation,
            control_epoch,
            retry_after_ms,
            invalid_field: invalid_field.map(str::to_owned),
        }),
    )
        .into_response()
}

/// Whether this route can authorize control at all, and the answer if not.
///
/// The *condition* lives here with the answer on purpose. Two of the three
/// gates that call this — public ingress and the owner side of the relay —
/// are separate call sites that could drift, and only the ingress one is
/// reachable from a test (the relay's needs a signed internal request and
/// there is no harness). Keeping the predicate here leaves those sites with
/// no logic of their own to get wrong: the remaining failure mode is deleting
/// the call, not answering differently from the plane next door.
pub(super) fn control_owner_refusal(
    route: &MediaSessionRoute,
    owner_epoch: Option<u64>,
) -> Option<Response> {
    let now_unix_ms = unix_ms();
    (route.publication_ready_at_ms != 0 || route.lease_expires_at_ms <= now_unix_ms)
        .then(|| control_owner_answer(route, owner_epoch, now_unix_ms))
}

/// The control-plane answer for a route that is no longer authorizing control
/// — its publication handoff is pending, or its owner lease has stopped being
/// renewed.
///
/// There are **three** gates: public ingress, the owner side of the relay in
/// `internal_media_sessions::control_inner`, and `verify_authority`'s re-read
/// at the owner. They must not diverge, because the earlier ones return
/// before the later ones run and would otherwise decide the answer on their
/// own. All three reach this function, and the classification is the media
/// plane's, so neither plane can answer one route differently from the other.
///
/// `now_unix_ms` comes from the caller's own liveness test rather than being
/// read again here, so the two cannot land on opposite sides of the lease
/// boundary.
pub(super) fn control_owner_answer(
    route: &MediaSessionRoute,
    owner_epoch: Option<u64>,
    now_unix_ms: i64,
) -> Response {
    match crate::playback_control::classify_control_owner(route, now_unix_ms) {
        crate::playback_control::ControlStateError::OwnerLost => {
            control_owner_lost(route, owner_epoch)
        }
        _ => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
            control_error(
                StatusCode::TOO_EARLY,
                "owner_transition",
                "the media owner is not currently authorizing control; takeover is not settled",
                Some(route.incarnation_id.clone()),
                owner_epoch,
                Some(500),
                None,
            )
        }
    }
}

/// No successor can answer this session's control exchange, so the client is
/// told to stop rather than to retry every 500 ms forever. The retry hint is
/// deliberately absent: there is nothing to come back to, and all three
/// reporters read 425-with-a-hint as an instruction to keep going.
///
/// The wording does not assert that a node died. On a single-node install
/// every session is untakeoverable by construction, and the common cause
/// there is the node's own store writes stalling past the lease rather than
/// the node being gone — so the message says what is true in both cases: this
/// session cannot be recovered, reopen.
pub(super) fn control_owner_lost(route: &MediaSessionRoute, owner_epoch: Option<u64>) -> Response {
    crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerLost);
    control_error(
        StatusCode::GONE,
        "owner_lost",
        "this media session's owner no longer holds it and nothing can take it over; \
         reopen playback",
        Some(route.incarnation_id.clone()),
        owner_epoch,
        None,
        None,
    )
}

/// Execute a control exchange after ingress (or the exact-write relay) has
/// proved the durable owner tuple. The manager repeats the tuple fence against
/// owner-local state before it can renew activity.
pub(crate) async fn control_local(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Response {
    // Public ingress and the internal relay both own the absolute exchange
    // deadline. Keep admission and every nonterminal mutation in that caller
    // future; only an already-accepted End receives a detached continuation
    // below for its durable acknowledgement.
    //
    // `switched` on a draining predecessor releases it, and it is released
    // *here* — after the exchange, on the owner — for three reasons, each of
    // which was a defect in the first placement:
    //
    // Only the owner reaches this function, and it reaches it on both paths:
    // public ingress that owns the route falls through to it, and a relayed
    // request arrives at `internal_media_sessions::control_authorized`, which
    // calls it directly. `control_inner` is on neither of those for a remote
    // route, so a release placed there ran only when the client happened to
    // hit the owning node.
    //
    // After the exchange, so the exchange is answered. Ending the row first
    // meant the very packet reporting the switch was answered `410
    // session_ended` — every successful early release counted as `Gone`, and
    // anything else riding that request was dropped.
    //
    // After the actor's fences. `control_inner` checks the generation, the
    // owner epoch and the route state; the client-instance and monotonic
    // sequence fences live in `ControlState::accept`, inside this call. The
    // drain exists to protect the instance that committed, so a packet from a
    // second instance — or a replayed one with a stale sequence — must not be
    // able to pull it out from under that instance.
    let releases_drain = route.drain_deadline_ms.is_some()
        && request.acknowledgement.as_ref().map(|ack| ack.state)
            == Some(crate::playback_control::AcknowledgementState::Switched);
    let response = control_local_inner(state, route, request, deadline_unix_ms).await;
    // Only on an accepted exchange. A refused one proves nothing about what
    // reached a screen.
    if releases_drain && response.status().is_success() {
        if let Err(error) = state
            .store
            .end_media_session(&route.session_id, "superseded", unix_ms())
            .await
        {
            // Not fatal. The deadline and the cross-node sweep are both still
            // behind this, so a failed early release costs the rest of the
            // window, not correctness.
            tracing::debug!(%error, "a switched acknowledgement could not release the drain");
        }
    }
    response
}

async fn control_local_inner(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Response {
    control_local_with_settlement_capacity(
        state,
        route,
        request,
        deadline_unix_ms,
        preparation_settlement_slots(),
    )
    .await
}

async fn control_local_with_settlement_capacity(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
    slots: Arc<tokio::sync::Semaphore>,
) -> Response {
    let owner_epoch = match u64::try_from(route.owner_epoch)
        .ok()
        .filter(|epoch| *epoch > 0)
    {
        Some(epoch) => epoch,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the local media route has an invalid owner epoch",
                Some(route.incarnation_id.clone()),
                None,
                Some(500),
                None,
            );
        }
    };
    let start = match control_start_response(route) {
        Some(start) => start,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "playback control was not advertised for this session",
                None,
                None,
                None,
                None,
            );
        }
    };
    let recipe = match serde_json::from_str::<RemoteStartRequest>(&route.recipe_json) {
        Ok(recipe) => recipe,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable delivery recipe is unreadable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    };
    if let Err(field) = request.validate(
        start.duration_ms,
        crate::playback_control::target_duration_ms(&recipe),
    ) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Invalid);
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "a control field is outside the bounded v1 contract",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            None,
            Some(field),
        );
    }
    if request.generation != route.incarnation_id {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
        return control_error(
            StatusCode::CONFLICT,
            "stale_control",
            "the control generation is no longer current",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            None,
            None,
        );
    }
    if request.control_epoch != owner_epoch {
        crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
        return control_error(
            StatusCode::CONFLICT,
            "owner_changed",
            "the media session owner epoch changed",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            None,
            None,
        );
    }
    let can_settle_preparation = request
        .accepts(crate::playback_control::PREPARE_REPLACEMENT_ACTION)
        || request.acknowledgement.is_some()
        || owner_epoch > 1
        || request.demand == crate::playback_control::PlaybackDemand::End;
    let staged_read = if !can_settle_preparation {
        Ok(None)
    } else {
        let read = state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await;
        #[cfg(test)]
        let read = if staged_read_faults()
            .lock()
            .expect("staged read faults")
            .remove(&route.incarnation_id)
        {
            Err(plurx_core::error::StoreError::Database(
                "injected staged ledger read failure".into(),
            ))
        } else {
            read
        };
        read.map_err(|error| {
            tracing::warn!(?error, "staged playback generation read failed");
        })
    };
    let ledger_unavailable = staged_read.is_err();
    let staged_generation = staged_read.ok().flatten();
    // Staging is detached from the exchange that requested it, so only a
    // later exchange can see the durable result. End never announces new
    // work: its owner-local transaction aborts any slot the session held.
    let prepared_successor = if request.demand == crate::playback_control::PlaybackDemand::End {
        crate::playback_control::PreparedSuccessorObservation::Inactive
    } else if !request.accepts(crate::playback_control::PREPARE_REPLACEMENT_ACTION) {
        crate::playback_control::PreparedSuccessorObservation::NotRequested
    } else if ledger_unavailable {
        crate::playback_control::PreparedSuccessorObservation::Unavailable
    } else {
        match staged_successor_action(state, route, staged_generation.clone()).await {
            Ok(Some(successor)) => {
                crate::playback_control::PreparedSuccessorObservation::Ready(successor)
            }
            Ok(None) => crate::playback_control::PreparedSuccessorObservation::Absent,
            Err(()) => crate::playback_control::PreparedSuccessorObservation::Unavailable,
        }
    };
    let terminal_committer =
        (request.demand == crate::playback_control::PlaybackDemand::End).then(|| {
            Arc::new(DurableTerminalCommitter {
                store: Arc::clone(&state.store),
                route: route.clone(),
                start: start.clone(),
                recipe: recipe.clone(),
                request: request.clone(),
                faults: None,
            }) as Arc<dyn crate::playback_control::TerminalControlCommitter>
        });
    // Resolve the engine-local gate before actor acceptance. It must remain
    // available even on a request without an acknowledgement: an owner-epoch
    // rollover can itself produce the abort directive that transfers an
    // inherited preparation away from the now-fenced old executor.
    let Some(preparation_gate) = state
        .transcode
        .session_preparation_gate(&route.session_id)
        .await
    else {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
        return control_error(
            StatusCode::TOO_EARLY,
            "owner_transition",
            "the durable route is active but its local worker is not yet available",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            Some(500),
            None,
        );
    };
    let preparation_status = state
        .transcode
        .hls_session_status_publication(&route.session_id)
        .await
        .and_then(|publication| publication.result.ok());
    // A missing ledger can mean maintenance already removed an Aborting
    // successor. Acknowledgements still need an owned executor to settle the
    // exact actor slot. Unknown reads must also reach frozen actor replay.
    let preparation_reservation = if staged_generation.is_some()
        || ledger_unavailable
        || request.acknowledgement.is_some()
        || owner_epoch > 1
        || request.demand == crate::playback_control::PlaybackDemand::End
    {
        let Some(reservation) =
            reserve_preparation_settlement(state, deadline_unix_ms, slots).await
        else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "preparation settlement capacity or serving authority is unavailable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        Some(reservation)
    } else {
        None
    };
    let preparation_admission = Some(Arc::new(DurablePreparationSettlementAdmission {
        state: state.clone(),
        gate: preparation_gate,
        route: route.clone(),
        start: start.clone(),
        recipe: recipe.clone(),
        request: request.clone(),
        status: preparation_status,
        reservation: std::sync::Mutex::new(preparation_reservation),
        receipt: std::sync::Mutex::new(None),
    }));
    let preparation_admission_trait = preparation_admission.as_ref().map(|admission| {
        Arc::clone(admission) as Arc<dyn crate::playback_control::PreparationSettlementAdmission>
    });
    let result = match state
        .transcode
        .hls_session_control_with_terminal(
            crate::playback_control::LocalControlRequest {
                session_id: &route.session_id,
                generation: &route.incarnation_id,
                owner_node_id: &route.owner_node_id,
                owner_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: crate::playback_control::PlaybackDemandSnapshot::from(&request),
                prepared_successor,
            },
            deadline_unix_ms,
            terminal_committer,
            preparation_admission_trait,
        )
        .await
    {
        Some(Ok(result)) => result,
        Some(Err(crate::playback_control::ControlStateError::OwnerChanged)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
            return control_error(
                StatusCode::CONFLICT,
                "owner_changed",
                "the owner-local control epoch changed",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::RateLimited(retry_after_ms))) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::RateLimited);
            return control_error(
                StatusCode::TOO_MANY_REQUESTS,
                "control_rate_limited",
                "the control sequence advanced faster than the per-session budget",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(retry_after_ms),
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::SessionEnded)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::GONE,
                "session_ended",
                "the durable media session ended before control could renew it",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::PauseExpired)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::GONE,
                "pause_grace_expired",
                "the paused rolling presentation reached its finite grace; resume may open one replacement at the saved position",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::OwnerTransition)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
            return control_error(
                StatusCode::TOO_EARLY,
                "owner_transition",
                "the media owner changed while control was being admitted",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::OwnerLost)) => {
            return control_owner_lost(route, Some(owner_epoch));
        }
        Some(Err(crate::playback_control::ControlStateError::Unavailable)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the owner could not revalidate durable control authority",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
        Some(Err(_)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
            return control_error(
                StatusCode::CONFLICT,
                "stale_control",
                "the generation, client instance, or sequence fence is stale",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
            return control_error(
                StatusCode::TOO_EARLY,
                "owner_transition",
                "the durable route is active but its local worker is not yet available",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    };
    let retained_preparation_response = if result.preparation_directive.is_some() {
        let settlement = match preparation_admission
            .as_ref()
            .and_then(|admission| admission.receipt())
        {
            Some(receipt) => receipt.wait_before(deadline_unix_ms).await,
            None => None,
        };
        match settlement {
            Some(PreparationSettlement::Committed(response)) => Some(*response),
            Some(PreparationSettlement::Aborted) => None,
            Some(PreparationSettlement::Rejected) => {
                crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
                return control_error(
                    StatusCode::CONFLICT,
                    "stale_control",
                    "the preparation acknowledgement lost its commit or deadline fence",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    None,
                    None,
                );
            }
            Some(PreparationSettlement::Unavailable) | None => {
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                return control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "the preparation acknowledgement was not durably settled",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    Some(500),
                    None,
                );
            }
        }
    } else {
        None
    };
    // A response that came back from a durable settlement is the byte-exact
    // body that was stored, and `preparation_ack_replay` / `terminal_ack_replay`
    // serve that stored JSON without ever reaching the stamp below. Anything
    // this exchange adds to it afterwards makes the first answer and its own
    // replay differ, which is the one property §4 says must always hold.
    let durable_receipt_response =
        retained_preparation_response.is_some() || result.lease_state == "ended";
    let mut response = if let Some(response) = retained_preparation_response {
        response
    } else if result.lease_state == "ended" {
        if request.demand != crate::playback_control::PlaybackDemand::End {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "a terminal control result did not match the requested demand",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
        let Some(commit) = &result.terminal_commit else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the terminal control continuation was not admitted",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        match commit.wait().await {
            Ok(response) => response,
            Err(()) => {
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                return control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "the terminal control acknowledgement was not durably committed",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    Some(500),
                    None,
                );
            }
        }
    } else {
        let subtitle_window_seconds = state.subtitle_window_seconds().await;
        let subtitle_cache = subtitle_track_cache(
            state,
            &recipe,
            &request.selection.subtitle,
            &request,
            subtitle_window_seconds,
        )
        .await;
        local_control_response(
            route,
            &start,
            &recipe,
            &request,
            &result,
            unix_ms(),
            crate::playback_control::subtitle_readiness_value(
                &request.selection.subtitle,
                subtitle_cache,
            ),
        )
    };
    // The ask becomes durable before this exchange is reported accepted.
    //
    // §1's rule, and the window it closes is narrow but real: a client told
    // its new selection was taken, with nothing durable saying so, leaves
    // every later admission decision comparing against an ask that never
    // landed — and a restart or an owner change in that window loses the
    // request entirely, with the session continuing to serve the old
    // selection and nothing anywhere recording that anything was asked.
    //
    // Only an exchange whose ask the row does not already name pays for this.
    // A heartbeat repeats the same selection, so it writes nothing and cannot
    // fail here; the cost falls on the exchange that actually changed
    // something, which is the one that has something to lose.
    //
    // A failed write refuses the exchange rather than answering it. That is
    // the whole point of "before reported accepted": answering anyway would
    // be reporting a request taken that nothing has recorded, which is worse
    // than a client retrying one exchange.
    if let Some(desired) = result.selection.persist_desired.clone() {
        match state
            .store
            .record_desired_selection(
                route.user_id,
                &route.playback_id,
                &desired.digest,
                &desired.canonical_form,
                unix_ms(),
            )
            .await
        {
            Ok(_) => {
                state
                    .transcode
                    .record_desired_persisted(&route.session_id, &desired.digest)
                    .await;
            }
            Err(error) => {
                tracing::warn!(
                    session = %crate::transcode::session_log_id(&route.session_id),
                    "recording the viewer's selection failed: {error}"
                );
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                return control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "this selection could not be recorded; retry shortly",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    Some(500),
                    None,
                );
            }
        }
    }
    let outcome = match result.disposition {
        crate::playback_control::ControlDisposition::Accepted => {
            crate::playback_control::MetricOutcome::Accepted
        }
        crate::playback_control::ControlDisposition::Replay => {
            crate::playback_control::MetricOutcome::Replay
        }
    };
    crate::playback_control::record(outcome);
    crate::playback_control::record_platform(outcome, result.platform);
    let incumbent_waiting = result.disposition
        == crate::playback_control::ControlDisposition::Accepted
        && request.demand == crate::playback_control::PlaybackDemand::Active
        && matches!(
            request.render_state,
            crate::playback_control::RenderState::Waiting
                | crate::playback_control::RenderState::Stalled
        );
    if incumbent_waiting {
        // Preparation is speculative. A current-player wait outranks it even
        // when the client still has loaded media: synchronously take the exact
        // registry entry and let its detached cancellation owner abort the row,
        // actor slot, and worker without extending this exchange's deadline.
        cancel_preparations_for_incumbent_wait(&route.playback_id);
    }
    // The replacement seam. A viewer's quality change arrives as a new session
    // that replaces the old one, so the transition to measure is
    // predecessor-delivered against this session's delivered — both already
    // resolved, which is why nothing here reads the store or spawns.
    //
    // `reopen_reason: Some(Stall)` is excluded deliberately: that is the
    // client adapting to a failure, not a viewer asking for something, and
    // §3.3's own rule is that M6 prepares for a change the *client asked for*.
    let replaced = if recipe.request.reopen_reason.is_none() {
        remember_delivered_selection(
            &recipe.request.playback_id,
            &route.session_id,
            recipe.request.file_id,
            &request.selection,
            &response.effective_selection,
            crate::playback_control::GradeIntent::from_request(&recipe.request),
        )
    } else {
        None
    };
    if let Some((previous, previous_grade)) = replaced {
        crate::playback_control::record_preparation_observation(
            crate::playback_control::PreparationSeam::Replacement,
        );
        let delivered_view = crate::playback_control::RecipeView {
            selection: &previous,
            grade: previous_grade,
        };
        let proposed_view = crate::playback_control::RecipeView {
            selection: &response.effective_selection,
            grade: crate::playback_control::GradeIntent::from_request(&recipe.request),
        };
        let conditions = crate::playback_control::PreparationConditions {
            observed_download_bps: request.observed_download_bps,
            delivered_bps: response.delivery.delivered_bps,
        };
        let capabilities = result.selection.capabilities.clone();
        crate::playback_control::record_preparation_decision(
            result.platform,
            crate::playback_control::decide_preparation(
                delivered_view,
                proposed_view,
                capabilities.as_ref(),
                conditions,
            ),
        );
        crate::playback_control::record_preparation_counterfactual(
            result.platform,
            crate::playback_control::decide_preparation_after_client_release(
                delivered_view,
                proposed_view,
                conditions,
            ),
        );
    }
    // Not `selection.changed`: an ask that arrived while the preparation slot
    // was busy is "changed" for exactly one exchange and then never again, so
    // the successor the viewer actually asked for was built once, refused, and
    // forgotten. This stays true until the ask has been dispatched.
    let planned_relocation = if route.owner_node_id == state.node_id {
        state.serving.current_planned_outage().await
    } else {
        None
    };
    // Computed once for both the claim below and the answer at the end of the
    // exchange: they must be the same string, or a client would be told
    // `staging` about a candidate for an ask it has already left.
    let desired_digest = request.selection.desired().digest();
    let preparation_purpose = (!incumbent_waiting)
        .then(|| {
            planned_relocation
                .map(PreparationPurpose::PlannedRelocation)
                .or_else(|| {
                    result
                        .selection
                        .dispatch_preparation
                        .then_some(PreparationPurpose::SelectionChange)
                })
        })
        .flatten();
    // Not on an exchange answering from a durable receipt. That body says
    // `none` and cannot say anything else — it is the stored answer, and its
    // replay has to be the same bytes — so dispatching here would tell a client
    // `none` on the very exchange that started its successor, which is the one
    // thing this whole seam exists to prevent: the client reopens immediately
    // and orphans a successor that then holds the slot until it expires. The
    // planned-relocation purpose consults no slot state, so it would otherwise
    // fire on a commit acknowledgement or an End during a drain. The next
    // exchange composes its own body and dispatches there.
    if let Some(purpose) = preparation_purpose.filter(|_| !durable_receipt_response) {
        crate::playback_control::record_preparation_observation(
            crate::playback_control::PreparationSeam::InSession,
        );
        // The film time this exchange accepted, bounded by the film itself.
        //
        // `validate` is deliberately looser than that: it allows the duration
        // plus one target duration of slack, so a client reporting the final
        // segment's *end* is not refused, and it falls back to
        // `MAX_MEDIA_MILLIS` when the duration is unknown. Neither is a place a
        // successor may begin. Staging past the end would hold this playback's
        // one preparation slot, a durable row and an admission slot for a full
        // `PREPARATION_DEADLINE_MS` on a resume no viewer can ever reach — and
        // this value is now client-reported rather than derived from what the
        // server observed itself delivering, so the seam is where it earns a
        // bound. `.max(0)` cannot fire on a validated envelope; it is here so a
        // future caller that has not validated cannot serialize a negative
        // `start_seconds`.
        let accepted_film_time_ms = {
            let asked = request.seek_target_ms.unwrap_or(request.position_ms).max(0);
            match start.duration_ms {
                Some(duration) if duration > 0 => asked.min(duration),
                _ => asked,
            }
        };
        // Claimed before the spawn, not inside it: a task that has not been
        // polled yet is still work this playback is doing, and an exchange
        // that raced in between would otherwise be told `none`.
        let pending = PendingCandidateGuard::begin(&route.playback_id, &desired_digest);
        // Spawned, never awaited: see the function's own doc. The exchange has
        // spent its deadline by here and the response is already built.
        tokio::spawn(process_preparation_candidate(
            state.clone(),
            pending,
            PreparationCandidateInputs {
                session_id: route.session_id.clone(),
                route: route.clone(),
                recipe: recipe.clone(),
                planning_caps: retained_planning_caps(&route.response_json),
                planning_overrides: retained_planning_overrides(&route.response_json),
                selection: request.selection.clone(),
                observed_download_bps: request.observed_download_bps,
                delivered: response.effective_selection.clone(),
                delivered_bps: response.delivery.delivered_bps,
                capabilities: result.selection.capabilities.clone(),
                // The exchange's own accepted platform, the same value
                // `record_platform` just used — not the retained capability
                // document's, which can be absent on a session this build did
                // not start and would then leave the measurement
                // unattributable.
                platform: result.platform,
                accepted_film_time_ms,
                purpose,
            },
        ));
    }
    // Answered last, because it is the only field that depends on the action
    // this exchange ended up carrying. `offered` is not a guess about the slot:
    // it is a restatement of what `action` already is, which is what keeps the
    // two from ever disagreeing.
    //
    // `staging` covers three different truths that the client must treat
    // identically — this exchange just dispatched a candidate, a candidate
    // spawned by an earlier exchange is still doing its Store reads, or a
    // successor is registered and priming. A client that could distinguish them
    // would have nothing different to do with the distinction.
    //
    // Every one of the three is measured against *this exchange's*
    // `desired_digest`, which is what keeps `staging` honest across a change of
    // mind: work still winding down for the selection the viewer just left is
    // not work being done for the ask they are waiting on, and saying `staging`
    // about it would hold them past the point where reopening was the better
    // answer — and then make them reopen anyway, several seconds later.
    //
    // The first clause is not redundant with the second, though it looks it.
    // The guard is claimed just above with exactly this digest, so the marker
    // does normally answer for the dispatching exchange — but the task it was
    // claimed for is already spawned and may run to completion on another
    // thread before this line reads the map. Whether the exchange that
    // dispatched says `staging` is a fact about that exchange, not a fact about
    // a map another thread owns, and a client told `none` on the very exchange
    // that started its successor reopens immediately and orphans it.
    #[cfg(test)]
    if take_dispatch_settle_wait(&route.playback_id) {
        while pending_candidate_for_playback(&route.playback_id).is_some() {
            tokio::task::yield_now().await;
        }
    }
    // Only on a body this exchange composed. On a durable-receipt body the
    // field stays absent, which is the tri-state's documented "not evaluated
    // here" — the honest answer for a response written before this exchange
    // evaluated the slot, and never a decline.
    if !durable_receipt_response {
        response.delivery.preparation = Some(
            if matches!(
                response.action,
                crate::playback_control::ControlAction::Prepare { .. }
            ) {
                "offered"
            } else if preparation_purpose.is_some()
                || pending_candidate_for_playback(&route.playback_id)
                    .is_some_and(|pending| pending == desired_digest)
                || has_active_preparation_for_ask(&route.playback_id, &desired_digest)
            {
                "staging"
            } else {
                "none"
            }
            .to_owned(),
        );
    }
    tracing::debug!(
        session = %crate::transcode::session_log_id(&route.session_id),
        owner_epoch,
        sequence = result.accepted_sequence,
        replay = result.disposition == crate::playback_control::ControlDisposition::Replay,
        lease_timeout_ms = result.lease_timeout_ms,
        "playback control exchange"
    );
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(response),
    )
        .into_response()
}

/// Detached preparation candidates in flight, bounding production fan-out.
///
/// The gate is *this selection differs from the last accepted one*, so a
/// client alternating between two selections trips it on every exchange —
/// four a second, against the server's own 250 ms floor. That is adversarial
/// rather than likely, but a detached task per exchange per client with a
/// store read inside it is not a shape to leave unbounded. Over the cap the
/// candidate fails safe and no successor is staged; the accepted exchange is
/// already complete and remains valid.
static PREPARATION_CANDIDATES_IN_FLIGHT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
/// Enough for every node in the fleet to evaluate several clients at once,
/// and far below the point where detached Store work becomes competing load.
const MAX_PREPARATION_CANDIDATES: usize = 32;

/// One process-local owner for every reserved but uncommitted successor.
///
/// The durable row remains the authority. This registry supplies the missing
/// cancellation edge: settings disable, incumbent wait, the fixed deadline,
/// and a planned-outage cancellation all name the same exact reservation and
/// only one of them can take cleanup ownership.
#[derive(Clone)]
struct ActivePreparedSuccessor {
    state: AppState,
    executor: crate::playback_control::PreparationExecutor,
    preparation: plurx_core::domain::MediaSessionPreparation,
    purpose: PreparationPurpose,
    cancelled: tokio_util::sync::CancellationToken,
}

fn active_prepared_successors(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ActivePreparedSuccessor>> {
    static ACTIVE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ActivePreparedSuccessor>>,
    > = std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn prepared_handoff_transition() -> Arc<tokio::sync::RwLock<()>> {
    static TRANSITION: std::sync::OnceLock<Arc<tokio::sync::RwLock<()>>> =
        std::sync::OnceLock::new();
    Arc::clone(TRANSITION.get_or_init(|| Arc::new(tokio::sync::RwLock::new(()))))
}

pub(super) async fn prepared_handoff_write_guard() -> tokio::sync::OwnedRwLockWriteGuard<()> {
    prepared_handoff_transition().write_owned().await
}

/// Every playback whose ask is currently being turned into a successor, from
/// the moment the exchange spawns the candidate until that task exits by any
/// path — refused, unplannable, disabled, cancelled, or registered as an
/// `ActivePreparedSuccessor`, at which point the registry above takes over.
///
/// This exists because the work between the dispatch exchange and registration
/// is otherwise invisible. `process_preparation_candidate` reads a setting, a
/// source row, and runs `plan_preparation_candidate` — several Store awaits —
/// before anything is registered. An exchange landing inside that window has
/// neither a new dispatch nor a registry entry, and a server that answered
/// `none` there would be telling a client that is doing exactly the right
/// thing to stop waiting and reopen, which is the reopen this whole milestone
/// exists to retire.
///
/// Keyed by playback id, because that is the identity that survives the
/// session change a directed quality ask produces. The value carries the
/// desired digest the candidate is for, so a superseding ask is visible as
/// "pending, but for a different ask"; a `claim` that only this task's guard
/// matches, so a guard dropping late cannot evict a *newer* candidate's entry;
/// and a cancelled flag, because removal is the wrong signal — the task must
/// still run its own teardown, and the entry must keep naming the playback
/// until it does.
struct PendingPreparationCandidate {
    claim: u64,
    desired_digest: String,
    cancelled: bool,
}

fn pending_preparation_candidates(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, PendingPreparationCandidate>> {
    static PENDING: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, PendingPreparationCandidate>>,
    > = std::sync::OnceLock::new();
    PENDING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

static NEXT_PENDING_CANDIDATE_CLAIM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// RAII: a destructor rather than a call at each `return`, because
/// `process_preparation_candidate` already has nine early exits and will grow
/// more. A guard also covers the two returns no `return` statement can —
/// a panic inside the detached task, and a runtime shutdown that drops it.
struct PendingCandidateGuard {
    playback_id: String,
    claim: u64,
}

impl PendingCandidateGuard {
    /// Claim the slot for `playback_id`. A newer ask deliberately overwrites an
    /// older one: there is one preparation slot per playback, so the older
    /// candidate is already doomed, and leaving its digest installed would make
    /// the emit rule answer `staging` about work nobody asked for any more.
    fn begin(playback_id: &str, desired_digest: &str) -> Self {
        let claim = NEXT_PENDING_CANDIDATE_CLAIM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        pending_preparation_candidates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                playback_id.to_owned(),
                PendingPreparationCandidate {
                    claim,
                    desired_digest: desired_digest.to_owned(),
                    cancelled: false,
                },
            );
        Self {
            playback_id: playback_id.to_owned(),
            claim,
        }
    }

    /// Whether this candidate has been superseded since it was spawned.
    ///
    /// A guard whose entry is no longer its own reads as cancelled too: the
    /// slot belongs to a newer ask, so continuing to build this one would
    /// spend an admission slot on a selection the viewer has already left.
    fn cancelled(&self) -> bool {
        pending_preparation_candidates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.playback_id)
            .is_none_or(|pending| pending.claim != self.claim || pending.cancelled)
    }
}

impl Drop for PendingCandidateGuard {
    fn drop(&mut self) {
        let mut pending = pending_preparation_candidates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending
            .get(&self.playback_id)
            .is_some_and(|entry| entry.claim == self.claim)
        {
            pending.remove(&self.playback_id);
        }
    }
}

/// The desired digest a pending candidate is being built for, if one is.
fn pending_candidate_for_playback(playback_id: &str) -> Option<String> {
    pending_preparation_candidates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(playback_id)
        .filter(|pending| !pending.cancelled)
        .map(|pending| pending.desired_digest.clone())
}

/// Mark a pending candidate superseded. The task observes this at its next
/// await boundary and exits through its own accounting; the entry stays until
/// its guard drops, so nothing can observe a gap in which neither this map nor
/// the active registry names the playback.
fn cancel_pending_candidate(playback_id: &str) {
    if let Some(pending) = pending_preparation_candidates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(playback_id)
    {
        pending.cancelled = true;
    }
}

/// Whether the pending candidate for this playback has been superseded, from
/// the point of view of a task building `desired_digest`.
///
/// Read once more immediately after registration: that is the one window the
/// guard's own checks cannot cover, because between a task's last await and
/// its `register_active_preparation` there is no suspension point at which it
/// could have noticed.
///
/// **Two ways to be superseded, and the flag is only one of them.** The other
/// is the ordinary one: a second quality tap on the same session activates
/// nothing, so `cancel_preparations_for_superseded_predecessor` never runs and
/// no flag is ever set — the next exchange simply dispatches again and
/// `PendingCandidateGuard::begin` *overwrites* the entry, with `cancelled`
/// false, for a different ask. A reader that looked only at the flag would let
/// that task register, arm its watch, reserve and prime a full encoder for a
/// selection the viewer had already left, and would leave two entries naming
/// one playback. This is the same "the slot is no longer mine" test
/// `PendingCandidateGuard::cancelled` makes, expressed against the digest
/// rather than the claim, because the caller here holds a `preparation` rather
/// than the guard.
///
/// An *absent* entry is not superseded: the test-only staging paths run with no
/// guard at all, and a task whose guard has already dropped has nothing left to
/// supersede. `None` for `desired_digest` is the staging path with no observed
/// selection — no evidence either way, so only the flag counts.
fn pending_candidate_superseded(playback_id: &str, desired_digest: Option<&str>) -> bool {
    pending_preparation_candidates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(playback_id)
        .is_some_and(|pending| {
            pending.cancelled || desired_digest.is_some_and(|asked| pending.desired_digest != asked)
        })
}

/// Whether a successor is registered and priming **for this ask**.
///
/// The digest comparison is the same one the pending-marker clause makes, and
/// for the same reason. Without it one leftover entry — a stale ask the viewer
/// left, a planned relocation, a successor whose commit never comes — answers
/// `staging` on every exchange for the rest of the playback, and a client that
/// believes it burns its whole wait and *then* reopens. That is strictly worse
/// than the `none` it would have been told without the entry: the reopen still
/// happens, just several seconds later.
///
/// An executor with no recorded ask matches. Those are the staging paths that
/// ran without an observed selection, and an absent record is no evidence
/// either way — reading it as a mismatch would answer `none` while a real
/// successor primes.
fn has_active_preparation_for_ask(playback_id: &str, desired_digest: &str) -> bool {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .any(|active| {
            active.preparation.playback_id == playback_id
                && active
                    .executor
                    .asked()
                    .is_none_or(|asked| asked == desired_digest)
        })
}

fn register_active_preparation(active: ActivePreparedSuccessor) {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(active.preparation.incarnation_id.clone(), active);
}

/// Register an `ActivePreparedSuccessor` without standing up a real encoder.
///
/// The stage-only path (`stage_prepared_successor`) deliberately does not
/// register: registration lives inside the prime branch, because cancellation
/// ownership only means anything once a worker exists to cancel. A test that
/// used the stage-only path to stand in for registration would be asserting
/// against an empty registry — the cancel would "succeed" because there was
/// nothing there, which is exactly the vacuous-guard shape this programme's
/// reviews keep finding. This builds the entry the real path builds and inserts
/// it the same way, so the edge under test is the real one.
#[cfg(test)]
fn register_test_preparation(
    state: AppState,
    executor: crate::playback_control::PreparationExecutor,
    preparation: plurx_core::domain::MediaSessionPreparation,
    purpose: PreparationPurpose,
) -> tokio_util::sync::CancellationToken {
    let cancelled = tokio_util::sync::CancellationToken::new();
    register_active_preparation(ActivePreparedSuccessor {
        state,
        executor,
        preparation,
        purpose,
        cancelled: cancelled.clone(),
    });
    cancelled
}

fn arm_preparation_foreground_watch(active: ActivePreparedSuccessor) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = active.cancelled.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(25)) => {}
            }
            if active_preparation(&active.preparation.incarnation_id).is_none() {
                return;
            }
            if active.state.transcode.foreground_media_waiting() {
                if let Some(active) = take_active_preparation(&active.preparation.incarnation_id) {
                    settle_cancelled_preparation(
                        active,
                        "foreground playback claimed prepared capacity",
                    )
                    .await;
                }
                return;
            }
        }
    });
}

fn active_preparation(staged_incarnation_id: &str) -> Option<ActivePreparedSuccessor> {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(staged_incarnation_id)
        .cloned()
}

fn take_active_preparation(staged_incarnation_id: &str) -> Option<ActivePreparedSuccessor> {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(staged_incarnation_id)
}

fn take_active_preparations_for_playback(playback_id: &str) -> Vec<ActivePreparedSuccessor> {
    let mut active = active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let incarnation_ids = active
        .iter()
        .filter(|(_, preparation)| preparation.preparation.playback_id == playback_id)
        .map(|(incarnation_id, _)| incarnation_id.clone())
        .collect::<Vec<_>>();
    incarnation_ids
        .into_iter()
        .filter_map(|incarnation_id| active.remove(&incarnation_id))
        .collect()
}

async fn settle_cancelled_preparation(active: ActivePreparedSuccessor, reason: &'static str) {
    let _ = take_active_preparation(&active.preparation.incarnation_id);
    crate::playback_control::record_preparation_cancelled(reason);
    active.cancelled.cancel();
    let deadline = tokio::time::Instant::now() + PREPARATION_SETTLEMENT_RETRY_BUDGET;
    let mut delay = PREPARATION_SETTLEMENT_RETRY_MIN;
    loop {
        let now_ms = unix_ms();
        let settled = match active
            .executor
            .abort(&active.preparation.incarnation_id, now_ms)
            .await
        {
            Ok(true) => true,
            Ok(false) => active
                .executor
                .discard_reserved(&active.preparation.incarnation_id, now_ms)
                .await
                .is_ok_and(|discarded| discarded),
            Err(_) => false,
        };
        if settled || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(delay).await;
        delay = delay
            .saturating_mul(2)
            .min(PREPARATION_SETTLEMENT_RETRY_MAX);
    }
    retire_prepared_worker(
        &active.state,
        &active.preparation.owner_node_id,
        &active.preparation.incarnation_id,
        &active.preparation.session_id,
        reason,
    )
    .await;
}

fn spawn_cancelled_preparation(active: ActivePreparedSuccessor, reason: &'static str) {
    active.cancelled.cancel();
    tokio::spawn(settle_cancelled_preparation(active, reason));
}

fn cancel_preparations_for_incumbent_wait(playback_id: &str) {
    for active in take_active_preparations_for_playback(playback_id) {
        spawn_cancelled_preparation(active, "incumbent playback needed prepared capacity");
    }
}

/// The client went around the handoff — it created a new session for this
/// playback instead of committing the one being prepared — so the successor
/// has no viewer. Free the worker now rather than at the 330 s deadline, and
/// mark a pending candidate that has not reached the registry yet.
///
/// Two outcomes were being paid for before this existed, both bad: on a node
/// with encoder headroom the orphan encodes for the full deadline for nobody;
/// on a node at its hardware session cap the reopen's own live create tears the
/// successor down as "foreground playback claimed prepared capacity" and counts
/// it as a refusal, which is what the fleet's `staged_total{refused}` has
/// actually been measuring.
///
/// `keep` is the activating route's incarnation id. The one successor that must
/// survive its own predecessor's retirement is the committed one. Commit
/// already removes that registry entry before this can run, so today the guard
/// is never the only thing standing between a committed successor and
/// cancellation — but ordering is not a contract, and the guard is what makes
/// it structural.
fn cancel_preparations_for_superseded_predecessor(playback_id: &str, keep: Option<&str>) {
    cancel_pending_candidate(playback_id);
    for active in take_active_preparations_for_playback(playback_id) {
        if keep.is_some_and(|keep| keep == active.preparation.incarnation_id) {
            register_active_preparation(active);
            continue;
        }
        spawn_cancelled_preparation(active, "predecessor superseded by a new session");
    }
}

pub(super) async fn abort_disabled_prepared_handoffs() {
    let active = {
        let mut active = active_prepared_successors()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active
            .drain()
            .map(|(_, preparation)| preparation)
            .collect::<Vec<_>>()
    };
    for preparation in active {
        settle_cancelled_preparation(preparation, "prepared handoff disabled").await;
    }
}

pub(super) fn cancel_prepared_handoff_work() {
    let active = {
        let mut active = active_prepared_successors()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active
            .drain()
            .map(|(_, preparation)| preparation)
            .collect::<Vec<_>>()
    };
    for preparation in active {
        spawn_cancelled_preparation(preparation, "prepared handoff disabled");
    }
}

struct ReservedPreparationGuard {
    active: Option<ActivePreparedSuccessor>,
}

impl ReservedPreparationGuard {
    fn new(active: ActivePreparedSuccessor) -> Self {
        Self {
            active: Some(active),
        }
    }

    fn active(&self) -> &ActivePreparedSuccessor {
        self.active.as_ref().expect("armed preparation guard")
    }

    fn disarm(mut self) -> ActivePreparedSuccessor {
        self.active.take().expect("armed preparation guard")
    }
}

impl Drop for ReservedPreparationGuard {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = take_active_preparation(&active.preparation.incarnation_id);
            spawn_cancelled_preparation(active, "prepared successor ownership was cancelled");
        }
    }
}

async fn reserve_preparation_before(
    active: ActivePreparedSuccessor,
) -> Option<ReservedPreparationGuard> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if active
            .executor
            .reserve(&active.preparation)
            .await
            .is_ok_and(|reserved| reserved)
        {
            let _ = send.send(ReservedPreparationGuard::new(active));
        } else {
            settle_cancelled_preparation(active, "prepared successor reservation refused").await;
        }
    });
    tokio::time::timeout(PREPARATION_STORE_BUDGET, receive)
        .await
        .ok()
        .and_then(Result::ok)
}

async fn activate_preparation_before(
    guard: ReservedPreparationGuard,
) -> Option<ReservedPreparationGuard> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let active = guard.active();
        if active
            .executor
            .activate_reserved(&active.preparation)
            .await
            .is_ok_and(|activated| activated)
        {
            let _ = send.send(guard);
        }
    });
    tokio::time::timeout(PREPARATION_STORE_BUDGET, receive)
        .await
        .ok()
        .and_then(Result::ok)
}

#[cfg(test)]
fn completed_preparation_candidates() -> &'static std::sync::Mutex<std::collections::HashSet<String>>
{
    static COMPLETED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    COMPLETED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
fn take_preparation_candidate_completion(incarnation_id: &str) -> bool {
    completed_preparation_candidates()
        .lock()
        .expect("preparation candidate completions")
        .remove(incarnation_id)
}

/// Last delivered selection of each *playback*, so a session that replaces
/// another can be measured against the one it replaced.
///
/// This exists because of what lab6 measured on 2026-09-02: 949 accepted
/// exchanges and one recorded decision. `ControlState::last_selection` sees a
/// selection change only *within* one session, and Apple does not change a
/// selection within a session — `PlayerController.selectQuality` calls
/// `reopen`, whose own comment says the replacement "intentionally removes the
/// older session". So a viewer's quality change arrives as a brand-new session
/// with no predecessor to differ from, and the gate is false by construction.
///
/// The seam M6 is actually about is therefore the replacement, not the
/// exchange — which is the whole point of the milestone: today a quality
/// change tears the session down, and M6 exists so that it does not.
///
/// **Keyed by playback id, not by `previous_session_id`.** That field is set
/// only on a stall reopen — `PlayerController.swift` builds it exclusively
/// from a `StallReopenTicket` — so a viewer's quality change carries none, and
/// a gate that required one alongside "not a stall" could never fire at all.
/// The playback id is what actually survives the replacement: the reopen
/// comment says the two sessions "share a playback ID".
static DELIVERED_SELECTIONS: std::sync::Mutex<Option<DeliveredSelections>> =
    std::sync::Mutex::new(None);

/// Bounded FIFO of `playback id -> the session serving it and what it was last
/// delivering`.
///
/// Process-local and best-effort by construction: a replacement served by a
/// different node measures nothing, which understates the count and never
/// misreports one. Bounded because a long-lived node serves unboundedly many
/// sessions; the oldest is dropped, and dropping one costs a measurement.
#[derive(Default)]
struct DeliveredSelections {
    by_playback: std::collections::HashMap<String, RememberedDelivery>,
    order: std::collections::VecDeque<String>,
}

/// What one playback was last seen delivering, and by which session.
#[derive(Clone)]
struct RememberedDelivery {
    session: String,
    /// The file the predecessor was serving.
    ///
    /// A playback id outlives the film. Apple's `PlayerController` mints one
    /// per `PlayerView` and autoplay-next is a `stop()`-then-`start()` on the
    /// same controller, so episode N and N+1 share it — and
    /// `EffectiveSelection` carries no file identity, so without this an
    /// episode boundary is indistinguishable from a viewer changing quality.
    /// On a series binge that would be the *dominant* source of replacement
    /// measurements, and a later slice acting on this seam would stage a
    /// "seamless handoff" across a film boundary.
    file_id: i64,
    /// What the viewer had asked for, as distinct from what was delivered.
    ///
    /// The client reopens for its own reasons that are not viewer intent: a
    /// Dolby Vision fallback to the HDR10 base or to a compatibility
    /// transcode, a burn-in retry, a failure retry, an out-of-window seek.
    /// Those change the *delivered* recipe on exactly the two axes whose
    /// counts are the evidence for or against widening `PREPARED_AXIS`, while
    /// the viewer asked for nothing. Only `reopen_reason: Some(Stall)` is
    /// declared on the wire, so the honest discriminator is this: M6 prepares
    /// for a change the client *asked for*, and an unchanged ask is not one.
    asked: crate::playback_control::ClientSelection,
    selection: crate::playback_control::EffectiveSelection,
    grade: crate::playback_control::GradeIntent,
}

/// Enough for every playback a node serves in the window a viewer might change
/// quality in, and small enough to be invisible.
const MAX_REMEMBERED_SELECTIONS: usize = 512;

/// Remember what this playback is delivering; answer what it was delivering
/// under the session this one replaced.
///
/// One lock, one pass. Answers `Some` exactly when the playback is already
/// known **and a different session is now serving it** — which is a
/// replacement, and is the only shape a viewer's quality change takes. Later
/// exchanges of the same session re-record and answer `None`, so one change is
/// counted once rather than for the life of the session.
fn remember_delivered_selection(
    playback: &str,
    session: &str,
    file_id: i64,
    asked: &crate::playback_control::ClientSelection,
    delivered: &crate::playback_control::EffectiveSelection,
    grade: crate::playback_control::GradeIntent,
) -> Option<(
    crate::playback_control::EffectiveSelection,
    crate::playback_control::GradeIntent,
)> {
    let Ok(mut guard) = DELIVERED_SELECTIONS.lock() else {
        // A poisoned lock costs measurements, never an exchange.
        return None;
    };
    let table = guard.get_or_insert_with(DeliveredSelections::default);
    let replaced = match table.by_playback.get(playback) {
        // A replacement worth measuring is a *different session*, serving the
        // *same film*, because the viewer *asked for something else*. Drop any
        // one of those three and the count fills with things no viewer did:
        // an episode boundary, or the client's own recovery reopen.
        Some(held)
            if held.session != session && held.file_id == file_id && &held.asked != asked =>
        {
            Some((held.selection.clone(), held.grade))
        }
        Some(_) => None,
        None => {
            table.order.push_back(playback.to_owned());
            None
        }
    };
    // Most-recently-touched, not first-seen. Insertion order would evict the
    // two-hour film first, and mid-film is exactly when a viewer changes
    // quality — the entry most worth keeping would be the first one dropped.
    table.order.retain(|held| held != playback);
    table.order.push_back(playback.to_owned());
    while table.order.len() > MAX_REMEMBERED_SELECTIONS {
        if let Some(evicted) = table.order.pop_front() {
            table.by_playback.remove(&evicted);
        }
    }
    table.by_playback.insert(
        playback.to_owned(),
        RememberedDelivery {
            session: session.to_owned(),
            file_id,
            asked: asked.clone(),
            selection: delivered.clone(),
            grade,
        },
    );
    replaced
}

/// Everything one exchange said, gathered for the detached preparation path.
///
/// A struct rather than eight parameters: these are all *one exchange's*
/// answer, they are always passed together, and the spawned task has no reason
/// to be able to take them from different exchanges.
#[derive(Clone, Copy, Debug)]
enum PreparationPurpose {
    SelectionChange,
    PlannedRelocation(crate::serving_fence::PlannedOutageFenceToken),
}

struct PreparationCandidateInputs {
    /// The live session this exchange belongs to. Staging needs it twice: to
    /// reach the actor that owns the one successor slot, and to name the
    /// predecessor the commit CAS will fence against.
    session_id: String,
    route: MediaSessionRoute,
    recipe: RemoteStartRequest,
    planning_caps: Option<plurx_core::playback::DeviceCaps>,
    planning_overrides: Option<CreateOverrides>,
    selection: crate::playback_control::ClientSelection,
    observed_download_bps: Option<u64>,
    delivered: crate::playback_control::EffectiveSelection,
    delivered_bps: Option<i64>,
    capabilities: Option<crate::playback_control::DynamicCapabilities>,
    platform: crate::playback_control::ClientPlatform,
    purpose: PreparationPurpose,
    /// Where the viewer actually is, in absolute film time, according to the
    /// envelope this exchange **accepted**.
    ///
    /// Captured here rather than read from the route because only the envelope
    /// knows it. A route carries `fetched_through_ms`, which is a high-water
    /// fetch frontier: it advances to the *end* of a segment the moment the
    /// client asks for it, clients prefetch and retry, and after a backward
    /// seek it stays far ahead of the playhead. Staging from it hands the
    /// viewer a successor that begins past film they have not watched.
    ///
    /// `seek_target_ms` wins over `position_ms` for the same reason
    /// [`ControlRequestV1::validate`] anchors the buffer on it: while a seek is
    /// in flight the reported position is still the old one, and the target is
    /// where this viewer is going.
    ///
    /// Already absolute — the control plane's positions are source-timeline
    /// values, not offsets into this session — so nothing downstream may add
    /// `media_origin_ms` to it.
    ///
    /// Bounded at the seam by the film's own duration, not by `validate`:
    /// validation allows a target duration of slack past the end so a client
    /// reporting the final segment's end is not refused, and that is not a
    /// place a successor may begin.
    accepted_film_time_ms: i64,
}

/// Re-run the same capability-aware planning used by ordinary create.
///
/// A prepared successor is a new playable recipe, not an edit to the current
/// encoder. In particular, Original may move a transcode back to direct/remux,
/// and a compound quality/track/subtitle request must be resolved once as a
/// whole. Recipes written before the durable response sidecar was retained use
/// the legacy edit path so an upgrade never guesses capabilities that the
/// client did not provide.
async fn plan_preparation_candidate(
    state: &AppState,
    predecessor: &RemoteStartRequest,
    planning_caps: Option<&plurx_core::playback::DeviceCaps>,
    planning_overrides: Option<&CreateOverrides>,
    selection: &crate::playback_control::ClientSelection,
    source: &MediaFile,
    delivered_height: i64,
) -> Result<crate::transcode::SessionRequest, ApiError> {
    if let Some(caps) = planning_caps {
        super::stream::validate_device_caps(caps)?;
    }
    let Some(caps) = planning_caps
        .filter(|caps| caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty())
    else {
        let height = match selection.quality {
            // A named Auto rung is an explicit ask to `resolve_height`, which
            // snaps it to the ladder and never binds above the capability
            // ceiling — the same treatment a manual rung gets. Unnamed Auto
            // still means "whatever is being delivered".
            crate::playback_control::QualitySelection::Auto {
                height: Some(height),
            } => {
                resolve_height(
                    state,
                    Some(source),
                    None,
                    predecessor.request.hdr10,
                    Some(height),
                )
                .await
            }
            crate::playback_control::QualitySelection::Auto { height: None } => delivered_height,
            crate::playback_control::QualitySelection::Original => {
                source.height.unwrap_or(delivered_height)
            }
            crate::playback_control::QualitySelection::Manual { height } => {
                resolve_height(
                    state,
                    Some(source),
                    None,
                    predecessor.request.hdr10,
                    Some(height),
                )
                .await
            }
        };
        // The legacy branch builds a candidate too, and it carries the
        // viewer's `subtitle_burn` just as the caps-v2 branch does — so it
        // needs the same guard. Without it a client that sends no caps
        // document (or one this build cannot parse) can still have a
        // successor staged as a burn that tone-maps the picture at the moment
        // it is committed, which is the failure this guard exists for.
        let candidate = crate::playback_control::candidate_request(
            &predecessor.request,
            selection,
            height,
            source.height,
        );
        if burn_would_discard_this_session_hdr(
            state,
            Some(source),
            &candidate,
            predecessor.request.hdr10,
            height,
        )
        .await
        {
            return Err(ApiError::Unprocessable(serde_json::json!({
                "code": "hdr_subtitle_burn_refused",
                "error": HDR_SUBTITLE_BURN_REFUSAL,
            })));
        }
        return Ok(candidate);
    };

    use plurx_core::playback::{DeviceProfile, Force, PlaybackMethod};
    use plurx_core::transcode::OutputGrade;

    let quality_force = match selection.quality {
        // Auto stays Auto with or without a named rung. The rung is carried on
        // the create body below, not by switching the server's own policy off.
        crate::playback_control::QualitySelection::Auto { .. } => Force::Auto,
        crate::playback_control::QualitySelection::Original => Force::Original,
        crate::playback_control::QualitySelection::Manual { .. } => Force::Transcode,
    };
    let unsupported = |message| {
        ApiError::typed(
            StatusCode::UNPROCESSABLE_ENTITY,
            "prepared_output_unsupported",
            message,
        )
    };
    let source_codec = source
        .video_codec
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let source_is_h264 = matches!(source_codec.as_str(), "h264" | "avc" | "avc1");
    let source_is_hevc = matches!(source_codec.as_str(), "hevc" | "h265" | "hev1" | "hvc1");
    let source_range = source.hdr.as_deref().unwrap_or("sdr");
    let codec_satisfied = match selection.codec {
        crate::playback_control::CodecPolicy::Auto => true,
        crate::playback_control::CodecPolicy::H264 => source_is_h264,
        crate::playback_control::CodecPolicy::Hevc => source_is_hevc,
        crate::playback_control::CodecPolicy::Av1 => {
            return Err(ApiError::typed(
                StatusCode::UNPROCESSABLE_ENTITY,
                "prepared_output_unsupported",
                "the ordinary planner has no AV1 successor output",
            ));
        }
    };
    let range_satisfied = match selection.dynamic_range {
        crate::playback_control::DynamicRangePolicy::Auto => true,
        crate::playback_control::DynamicRangePolicy::DolbyVision => source_range == "dolby_vision",
        crate::playback_control::DynamicRangePolicy::Hdr10 => source_range == "hdr10",
        crate::playback_control::DynamicRangePolicy::Hlg => source_range == "hlg",
        crate::playback_control::DynamicRangePolicy::Sdr => source_range == "sdr",
    };
    if matches!(
        (selection.codec, selection.dynamic_range),
        (
            crate::playback_control::CodecPolicy::H264,
            crate::playback_control::DynamicRangePolicy::Hdr10
                | crate::playback_control::DynamicRangePolicy::DolbyVision
                | crate::playback_control::DynamicRangePolicy::Hlg
        )
    ) {
        return Err(unsupported(
            "the ordinary planner cannot produce the requested HDR grade in H.264",
        ));
    }
    if matches!(
        selection.dynamic_range,
        crate::playback_control::DynamicRangePolicy::DolbyVision
            | crate::playback_control::DynamicRangePolicy::Hlg
    ) && (!range_satisfied
        || matches!(
            selection.quality,
            crate::playback_control::QualitySelection::Manual { .. }
        ))
    {
        return Err(unsupported(
            "the ordinary planner can preserve this requested grade but cannot synthesize it",
        ));
    }
    if matches!(
        selection.dynamic_range,
        crate::playback_control::DynamicRangePolicy::Hdr10
    ) && !range_satisfied
        && plurx_core::playback::hdr_route(source).is_none()
    {
        return Err(unsupported(
            "the ordinary planner cannot synthesize HDR10 from this source grade",
        ));
    }
    if matches!(selection.codec, crate::playback_control::CodecPolicy::Hevc)
        && !codec_satisfied
        && !matches!(
            selection.dynamic_range,
            crate::playback_control::DynamicRangePolicy::Hdr10
        )
    {
        return Err(unsupported(
            "the ordinary planner emits HEVC only for an HDR10 successor",
        ));
    }
    // Original controls the quality rung; it does not override an explicit
    // codec or grade request. A source that already satisfies the compound
    // policy may still copy/remux, while a mismatch goes through the ordinary
    // transcode planner and unsupported output was refused above.
    let force = if !codec_satisfied || !range_satisfied {
        Force::Transcode
    } else {
        quality_force
    };
    let force_name = match force {
        Force::Auto => "auto",
        Force::Original => "original",
        Force::Transcode => "transcode",
    };
    let mut overrides = planning_overrides.cloned().unwrap_or_default();
    overrides.force = Some(force_name.to_owned());
    let mut profile = DeviceProfile::from_caps_v2(caps);
    profile.retain_applicable_learned_limits(unix_ms());
    let node = super::stream::render_caps(state).await;
    let decision = plurx_core::playback::decide_forced(source, &profile, force, &node);
    let copy = decision.method != PlaybackMethod::Transcode;
    let requested_height = match selection.quality {
        crate::playback_control::QualitySelection::Auto { height } => height,
        crate::playback_control::QualitySelection::Original => source.height,
        crate::playback_control::QualitySelection::Manual { height } => Some(height),
    };
    let requested_hdr10 = decision.transcode_grade == OutputGrade::Hdr10
        && !matches!(
            selection.dynamic_range,
            crate::playback_control::DynamicRangePolicy::Sdr
        )
        && !matches!(selection.codec, crate::playback_control::CodecPolicy::H264);
    let delivered_codec_satisfied = match selection.codec {
        crate::playback_control::CodecPolicy::Auto => true,
        crate::playback_control::CodecPolicy::H264 => {
            if copy {
                source_is_h264
            } else {
                !requested_hdr10
            }
        }
        crate::playback_control::CodecPolicy::Hevc => {
            if copy {
                source_is_hevc
            } else {
                requested_hdr10
            }
        }
        crate::playback_control::CodecPolicy::Av1 => false,
    };
    let delivered_range_satisfied = match selection.dynamic_range {
        crate::playback_control::DynamicRangePolicy::Auto => true,
        crate::playback_control::DynamicRangePolicy::DolbyVision => {
            copy && decision.delivered_dynamic_range == "dolby_vision"
        }
        crate::playback_control::DynamicRangePolicy::Hdr10 => {
            (copy && decision.delivered_dynamic_range == "hdr10") || (!copy && requested_hdr10)
        }
        crate::playback_control::DynamicRangePolicy::Hlg => {
            copy && decision.delivered_dynamic_range == "hlg"
        }
        crate::playback_control::DynamicRangePolicy::Sdr => {
            decision.delivered_dynamic_range == "sdr" && !requested_hdr10
        }
    };
    if !delivered_codec_satisfied || !delivered_range_satisfied {
        return Err(unsupported(
            "the ordinary planner cannot satisfy the requested codec and dynamic range together",
        ));
    }
    let native_subtitle = matches!(
        selection.subtitle.mode,
        crate::playback_control::SubtitleMode::Native
    )
    .then_some(selection.subtitle.track)
    .flatten();
    let body = CreateSession {
        playback_id: predecessor.request.playback_id.clone(),
        request_id: None,
        control_sequence: None,
        previous_session_id: None,
        reopen_reason: None,
        height: requested_height,
        quality_auto: Some(matches!(
            selection.quality,
            crate::playback_control::QualitySelection::Auto { .. }
        )),
        subtitle_burn: matches!(
            selection.subtitle.mode,
            crate::playback_control::SubtitleMode::Burn
        )
        .then_some(selection.subtitle.track)
        .flatten(),
        // No acknowledgement. This path calls `resolve_plan` directly, so the
        // create handler's guard never ran on it and `Some(!requested_hdr10)`
        // had no reader at all — it was an answer to a question nobody asked.
        // The candidate is judged by the same guard below instead.
        subtitle_burn_sdr: None,
        native_subtitles: Some(native_subtitle.is_some()),
        subtitle: native_subtitle,
        start: Some(predecessor.request.start_seconds),
        audio: selection.audio_track,
        copy: Some(copy),
        aac: Some(decision.transcode_audio),
        preserve_dolby_vision: Some(decision.preserve_dolby_vision),
        hdr10: Some(requested_hdr10),
        audio_offset_ms: Some(selection.audio_offset_ms),
        caps: Some(caps.clone()),
        overrides: Some(overrides.clone()),
        presentation: Some("vod".to_owned()),
        block_budget_secs: predecessor.request.block_budget_secs,
        transport: predecessor.request.transport.clone(),
        intent: None,
    };
    let review = review_client_plan_inner(
        caps,
        Some(&overrides),
        source,
        &node,
        decision.preserve_dolby_vision,
        requested_hdr10,
        unix_ms(),
        false,
    );
    let plan = resolve_plan(
        PlanInputs {
            state,
            user_id: predecessor.user_id,
            file_id: predecessor.request.file_id,
            source: Some(source),
            network_prior: None,
        },
        Some(review),
        body,
    )
    .await?;
    let plan_height = plan.height;
    let mut resolved = plan.request;
    validate_hevc_copy_transport(state, source, caps, &resolved).await?;
    // The same guard ordinary create runs, on the same function. This path
    // reaches `resolve_plan` directly and therefore skipped it entirely: a
    // successor prepared for an HDR delivery could be staged as a burn that
    // silently tone-maps the picture at the moment it is committed, with the
    // viewer given no notice and no choice. Refusing the candidate is right
    // here — the incumbent keeps playing, which is what a refused preparation
    // means everywhere else.
    // `resolved.hdr10`, not the pre-review `requested_hdr10`: the review may
    // clamp the ask against the device profile, and the request this guard is
    // judging carries the clamped value. Create reads its post-review value
    // for the same reason.
    if burn_would_discard_this_session_hdr(
        state,
        Some(source),
        &resolved,
        resolved.hdr10,
        plan_height,
    )
    .await
    {
        return Err(ApiError::Unprocessable(serde_json::json!({
            "code": "hdr_subtitle_burn_refused",
            "error": HDR_SUBTITLE_BURN_REFUSAL,
        })));
    }
    // Planning chooses the codec/container recipe. It must not silently turn
    // a retained rolling fallback into VOD: the source prerequisite that made
    // the incumbent use rolling has not changed merely because its quality or
    // tracks did. VOD stays VOD and rolling stays rolling.
    resolved.presentation = predecessor.request.presentation;
    Ok(resolved)
}

async fn prepared_relocation_owner(
    state: &AppState,
    source: &MediaFile,
    candidate: &crate::transcode::SessionRequest,
    accepted_film_time_ms: i64,
) -> Option<String> {
    let target_height = match candidate.kind {
        crate::transcode::SessionKind::Transcode { height } => height,
        crate::transcode::SessionKind::Copy { .. } => source.height?,
    }
    .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT);
    let request = MediaOfferRequest::new(
        source,
        target_height,
        accepted_film_time_ms,
        candidate.audio_index,
        candidate.subtitle_burn,
        candidate.hdr10,
    )
    .ok()?;
    state
        .media_pool
        .offers(state, request)
        .await
        .offers
        .into_iter()
        .find(|offer| offer.eligible && offer.node_id != state.node_id)
        .map(|offer| offer.node_id)
}

/// Evaluate and, when admitted, durably stage this selection change.
///
/// The response never waits for this work: the accepted exchange has already
/// been built, and a later exchange observes the durable successor. The
/// decision is nevertheless production authority, not shadow measurement;
/// its actor slot and durable ledger are the only way this path can stage.
///
/// Called only when the engine reports the selection moved, and spawned rather
/// than awaited: the exchange is under an absolute deadline it has *already
/// spent* by this point, the response is fully built, and a store read that
/// ran long would turn a completed exchange into a 503 after the accepted
/// transaction had already been recorded.
///
/// Decided against **the response this exchange actually sent**: the client
/// was told a height and a rate, so staging from different values would build
/// a transition the client never requested.
async fn process_preparation_candidate(
    state: AppState,
    pending: PendingCandidateGuard,
    exchange: PreparationCandidateInputs,
) {
    let PreparationCandidateInputs {
        session_id,
        route,
        recipe,
        planning_caps,
        planning_overrides,
        selection,
        observed_download_bps,
        delivered,
        delivered_bps,
        capabilities,
        platform,
        accepted_film_time_ms,
        purpose,
    } = exchange;
    #[cfg(test)]
    struct Completion(String);
    #[cfg(test)]
    impl Drop for Completion {
        fn drop(&mut self) {
            completed_preparation_candidates()
                .lock()
                .expect("preparation candidate completions")
                .insert(self.0.clone());
        }
    }
    #[cfg(test)]
    let _completion = Completion(route.incarnation_id.clone());
    // Released on every exit below, including the early one.
    struct InFlight;
    impl Drop for InFlight {
        fn drop(&mut self) {
            PREPARATION_CANDIDATES_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if PREPARATION_CANDIDATES_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        >= MAX_PREPARATION_CANDIDATES
    {
        PREPARATION_CANDIDATES_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let _in_flight = InFlight;
    let enabled = match state
        .store
        .get_setting(plurx_core::store::keys::PREPARED_QUALITY_HANDOFF)
        .await
    {
        Ok(value) => plurx_core::store::stored_switch(value.as_deref(), true),
        Err(error) => {
            tracing::warn!(%error, "prepared-handoff setting could not be read");
            return;
        }
    };
    if !enabled {
        return;
    }
    if pending.cancelled() {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    let Ok(source) = state.store.get_file(recipe.request.file_id).await else {
        // Detached failure is fail-safe: no candidate is staged, and the
        // already-completed exchange remains valid.
        return;
    };
    let Some(source) = source.as_ref() else {
        return;
    };
    if pending.cancelled() {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    // Test-only. The refusal arm takes the same exit as the `Err` arm below,
    // so a forced refusal is indistinguishable from an unplannable candidate.
    #[cfg(test)]
    if let Some((delay, refuse)) = take_preparation_planning_fault(&route.playback_id) {
        tokio::time::sleep(delay).await;
        if refuse {
            crate::playback_control::record_preparation_staged(false);
            return;
        }
    }
    let candidate = match plan_preparation_candidate(
        &state,
        &recipe,
        planning_caps.as_ref(),
        planning_overrides.as_ref(),
        &selection,
        source,
        delivered.height,
    )
    .await
    {
        Ok(candidate) => candidate,
        Err(error) => {
            tracing::warn!(?error, "prepared successor could not be planned");
            crate::playback_control::record_preparation_staged(false);
            return;
        }
    };
    if pending.cancelled() {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    let proposed = crate::playback_control::EffectiveSelection::from_request(
        &candidate,
        match candidate.kind {
            crate::transcode::SessionKind::Transcode { height } => height,
            // A copy is not a rung, so it keeps the height it is already
            // delivering rather than one the ladder would have picked.
            crate::transcode::SessionKind::Copy { .. } => delivered.height,
        },
        None,
    );
    // Built once and passed to both decisions, so the two counters can never
    // disagree about what the transition was — only about the client gate.
    let delivered_view = crate::playback_control::RecipeView {
        selection: &delivered,
        grade: crate::playback_control::GradeIntent::from_request(&recipe.request),
    };
    let proposed_view = crate::playback_control::RecipeView {
        selection: &proposed,
        grade: crate::playback_control::GradeIntent::from_request(&candidate),
    };
    let conditions = crate::playback_control::PreparationConditions {
        observed_download_bps,
        delivered_bps,
    };
    let decision = crate::playback_control::decide_preparation(
        delivered_view,
        proposed_view,
        capabilities.as_ref(),
        conditions,
    );
    crate::playback_control::record_preparation_decision(platform, decision);
    // Keep the counterfactual beside the production decision as advisory
    // rollout telemetry. All three clients may advertise the explicit runtime
    // capability; neither a missing receipt nor a link estimate changes the
    // viewer's saved choice.
    crate::playback_control::record_preparation_counterfactual(
        platform,
        crate::playback_control::decide_preparation_after_client_release(
            delivered_view,
            proposed_view,
            conditions,
        ),
    );
    let successor_owner = match purpose {
        PreparationPurpose::SelectionChange => matches!(
            decision,
            crate::playback_control::PreparationDecision::Prepare { .. }
        )
        .then(|| state.node_id.clone()),
        PreparationPurpose::PlannedRelocation(fence)
            if capabilities
                .as_ref()
                .is_some_and(|caps| caps.dual_player_preparation)
                && state.serving.planned_outage_is_current(fence).await =>
        {
            prepared_relocation_owner(&state, source, &candidate, accepted_film_time_ms).await
        }
        PreparationPurpose::PlannedRelocation(_) => None,
    };
    if let Some(successor_owner) = successor_owner {
        #[cfg(test)]
        let _ = &successor_owner;
        #[cfg(not(test))]
        stage_and_prime_prepared_successor(
            &state,
            &session_id,
            &route,
            &recipe,
            &candidate,
            Some(source),
            &successor_owner,
            purpose,
            AcceptedAsk {
                film_time_ms: accepted_film_time_ms,
                desired_digest: Some(selection.desired().digest()),
            },
        )
        .await;
        // These decision-boundary tests use synthetic media rows and exercise
        // durable preparation semantics without launching ffmpeg. Production
        // always takes the reserve-and-prime call above.
        #[cfg(test)]
        stage_prepared_successor(
            &state,
            &session_id,
            &route,
            &recipe,
            &candidate,
            Some(source),
            AcceptedAsk {
                film_time_ms: accepted_film_time_ms,
                desired_digest: Some(selection.desired().digest()),
            },
        )
        .await;
    }
}

/// How long a staged successor may sit unclaimed.
///
/// Keyed off `VOD_LEASE_TIMEOUT_MS`, the longest control lease this can inherit.
/// A client is allowed that long between exchanges before it is even nominally
/// late, so a shorter window would expire VOD successors for clients behaving
/// exactly as the protocol permits. One longest lease plus a margin covers a
/// slow client without covering a gone one; rolling successors still advertise
/// and renew on their shorter rolling lease after commit.
///
/// **Correction, 2026-09-08: the store does enforce this bound, and this
/// paragraph used to say it did not.** It read *"This bound is enforced here,
/// not by the store. Maintenance retires an expired row only
/// `TAKEOVER_RECOVERY_MS` after its lease lapses…"*, and that was
/// **already false when it was committed**: `1d55c976`, "a preparation expires
/// on its own deadline", landed at 02:47:33 and `27e77824`, which added this
/// paragraph, at 04:14:45 the same morning — eighty-seven minutes later. The
/// belief was true when its author formed it; the tree moved underneath them
/// before they wrote it down, which is the ordinary way a comment is born
/// wrong rather than a way it goes stale.
///
/// `sqlite/sessions.rs` ends a staged row keyed on `deadline_ms` directly, and
/// `hiqlite_sessions.rs` mirrors it — *"A preparation expires on **its own
/// deadline**"* — and the lease loop refuses
/// to renew any incarnation carrying a preparation row, so the deadline cannot
/// be postponed.
///
/// So what the timer below is actually for is narrower than it looks: it frees
/// **the actor's in-memory slot**, which nothing in the store path settles,
/// and it shaves up to one maintenance tick (five minutes) off the durable
/// reap. Both are worth having. Neither is "the only thing enforcing the
/// bound", and building on that belief is how a reader concludes the durable
/// side is unprotected.
const PREPARATION_DEADLINE_MS: i64 = crate::playback_control::VOD_LEASE_TIMEOUT_MS as i64 + 30_000;
const PREPARATION_STORE_BUDGET: Duration = Duration::from_secs(5);
const PREPARATION_PRIME_BUDGET: Duration = Duration::from_secs(45);

/// M6 §3.4 — stage the successor the decision just admitted.
///
/// This joins stage with reserve-and-prime: the durable row is written first,
/// the VOD worker attaches behind its unpublished capability, and only then
/// may the actor expose its one successor slot. The pointer remains untouched
/// by construction — `prepare_media_session` is the entry point that neither
/// runs the supersession reap nor advances `media_playback_pointers`, which is
/// why a preparation cannot be built on `activate_media_session`.
///
/// Best-effort from the viewer's perspective. A staged successor that
/// fails to appear costs the viewer nothing: the fallback replacement they
/// would have taken before M6 is still exactly what happens. A staged
/// successor that appears when it should not costs a saturated user real
/// resources, which is why every refusal below returns rather than retries.
/// What the accepted control exchange said the viewer wants.
///
/// The two travel together because they are facts about the same exchange and
/// are both wrong if taken from different ones: staging at the film time from
/// one exchange under the ask from another builds a successor for a moment and
/// a selection that never coexisted.
struct AcceptedAsk {
    /// The absolute film time the accepted envelope settled on — the viewer's
    /// seek target where they asked for one, their playhead otherwise.
    film_time_ms: i64,
    /// The normalized ask, recorded on the preparation slot so an
    /// acknowledgement arriving later is judged against the ask that is
    /// current then. `None` where the caller stages without an observed
    /// selection.
    desired_digest: Option<String>,
}

#[cfg(test)]
async fn stage_prepared_successor(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
    accepted: AcceptedAsk,
) {
    stage_prepared_successor_with_prime(
        state,
        session_id,
        route,
        predecessor,
        candidate,
        source,
        &state.node_id,
        PreparationPurpose::SelectionChange,
        accepted,
        false,
    )
    .await;
}

#[cfg(not(test))]
#[allow(clippy::too_many_arguments)]
async fn stage_and_prime_prepared_successor(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
    successor_owner: &str,
    purpose: PreparationPurpose,
    accepted: AcceptedAsk,
) {
    stage_prepared_successor_with_prime(
        state,
        session_id,
        route,
        predecessor,
        candidate,
        source,
        successor_owner,
        purpose,
        accepted,
        true,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn stage_prepared_successor_with_prime(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
    successor_owner: &str,
    purpose: PreparationPurpose,
    accepted: AcceptedAsk,
    prime_worker: bool,
) {
    // Saving the switch off takes the write side, persists the choice, and
    // settles every registered successor before releasing it. Holding this
    // read side through the final setting read and stage closes the race where
    // an older detached candidate reserved after the administrator disabled
    // preparation.
    let _handoff_transition = prepared_handoff_transition().read_owned().await;
    let enabled = state
        .store
        .get_setting(plurx_core::store::keys::PREPARED_QUALITY_HANDOFF)
        .await
        .ok()
        .is_some_and(|value| plurx_core::store::stored_switch(value.as_deref(), true));
    if !enabled {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    let AcceptedAsk {
        film_time_ms: accepted_film_time_ms,
        desired_digest,
    } = accepted;
    if let PreparationPurpose::PlannedRelocation(fence) = purpose {
        if !state.serving.planned_outage_is_current(fence).await {
            crate::playback_control::record_preparation_staged(false);
            return;
        }
    }
    // The actor owns the slot. No live local worker means no slot to take, and
    // an expired, remote-owned or retired playback stages nothing.
    let Some(gate) = state.transcode.session_preparation_gate(session_id).await else {
        // Counted, not silent: a decision that never reached a gate is
        // exactly what the first version of this did on every production
        // session, and an uncounted return made that indistinguishable from
        // "nothing was admitted".
        crate::playback_control::record_preparation_staged(false);
        return;
    };
    // The source snapshot has to be the file's real one. `0/0` is this
    // codebase's documented "this placement never read the file" sentinel, and
    // `takeover_source_matches` refuses a takeover on it forever — so writing
    // it as a placeholder would poison the successor the moment it committed.
    let Some(source) = source else {
        return;
    };
    let now_ms = crate::media_sessions::unix_ms();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    let staged_session_id = uuid::Uuid::new_v4().to_string();
    // Where the successor actually begins: **where the viewer is**, not where
    // the predecessor was created and not how far it has fetched.
    //
    // `candidate` is the predecessor's request with only the selection fields
    // overwritten, so its `start_seconds` is the *original* start — committing
    // that would restart the film for a viewer forty minutes in. That much was
    // always true. What this previously did instead was
    // `media_origin_ms + fetched_through_ms`, and that is the other error:
    // the fetched frontier is a high-water mark, not a presentation time. It
    // reaches the end of a segment as soon as the client requests that segment,
    // it survives prefetch and retry, and after a backward seek it stays where
    // the client had already reached. Resuming there hands the viewer a
    // successor that starts past film they have never been shown — the very
    // discontinuity a prepared handoff exists to avoid. `OwnerLossResume`
    // documents the same hazard and pulls its frontier back by a whole segment
    // precisely because that path has no envelope to ask; this one does.
    //
    // `accepted_film_time_ms` is already absolute source-timeline film time, so
    // `media_origin_ms` must **not** be added back: the predecessor's origin is
    // baked into the position the client reported against it, and adding it
    // again would push a viewer thirty seconds into a film that started at
    // 00:01:30 out to 00:02:30.
    let resume_ms = accepted_film_time_ms;
    let staged_request = crate::transcode::SessionRequest {
        request_id: Some(staged_incarnation_id.clone()),
        start_seconds: resume_ms as f64 / 1_000.0,
        // A successor is its own generation, not a reopen of the one it
        // replaces. Carrying these across would stage a row claiming to be a
        // stall reopen of a session that is still playing.
        previous_session_id: None,
        reopen_reason: None,
        control_sequence: None,
        ..candidate.clone()
    };
    let staged_recipe = RemoteStartRequest {
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: staged_incarnation_id.clone(),
        user_id: route.user_id,
        source_size: source.size,
        source_mtime: source.mtime,
        // Read from the predecessor rather than assumed: this decides whether
        // anything may ever take the successor over.
        typeless_playlist: predecessor.typeless_playlist,
        library_channel: predecessor.library_channel.clone(),
        request: staged_request.clone(),
    };
    let Ok(recipe_json) = serde_json::to_string(&staged_recipe) else {
        return;
    };
    // A real `StartResponse`, because every reader of a route's
    // `response_json` parses it as one — and `control_start_response` filters
    // on the bootstrap being present, so a row without it answers 404
    // `session_gone` on the successor's first exchange after commit.
    let response = StartResponse {
        session_id: staged_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{staged_session_id}/index.m3u8"),
        duration_ms: source.duration_ms,
        start_seconds: resume_ms as f64 / 1_000.0,
        // VOD playlists retain the film's absolute zero origin. A rolling
        // successor instead begins its local timeline at this film boundary.
        // Copy may internally pull back to a keyframe, but the requested
        // boundary remains the client-visible handoff target just as it does
        // for an ordinary rolling create.
        media_origin_ms: Some(
            if staged_request.presentation == crate::transcode::Presentation::Vod {
                0
            } else {
                resume_ms
            },
        ),
        height: match staged_request.kind {
            crate::transcode::SessionKind::Transcode { height } => height,
            crate::transcode::SessionKind::Copy { .. } => source.height.unwrap_or_default(),
        },
        encoder: if staged_request.presentation == crate::transcode::Presentation::Vod {
            "vod"
        } else if matches!(
            staged_request.kind,
            crate::transcode::SessionKind::Copy { .. }
        ) {
            "copy"
        } else {
            "transcode"
        }
        .to_owned(),
        vod: staged_request.presentation == crate::transcode::Presentation::Vod,
        ladder: Vec::new(),
        prior_kbps: None,
        delivered_dynamic_range: None,
        delivered_dolby_vision_profile: None,
        control: crate::playback_control::ControlBootstrap::new(
            &staged_session_id,
            &staged_incarnation_id,
            1,
            if staged_request.presentation == crate::transcode::Presentation::Vod {
                crate::playback_control::VOD_LEASE_TIMEOUT_MS
            } else {
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS
            },
        ),
        plan_notes: vec![PREPARED_SUCCESSOR_PLAN_NOTE.to_owned()],
    };
    let mut response_value = match serde_json::to_value(response) {
        Ok(value) => value,
        Err(_) => return,
    };
    if let Some(caps) = retained_planning_caps(&route.response_json) {
        response_value
            .as_object_mut()
            .expect("StartResponse serializes as an object")
            .insert(
                PLANNING_CAPS_RESPONSE_FIELD.to_owned(),
                match serde_json::to_value(caps) {
                    Ok(value) => value,
                    Err(_) => return,
                },
            );
    }
    if let Some(overrides) = retained_planning_overrides(&route.response_json) {
        response_value
            .as_object_mut()
            .expect("StartResponse serializes as an object")
            .insert(
                PLANNING_OVERRIDES_RESPONSE_FIELD.to_owned(),
                match serde_json::to_value(overrides) {
                    Ok(value) => value,
                    Err(_) => return,
                },
            );
    }
    if let PreparationPurpose::PlannedRelocation(fence) = purpose {
        response_value
            .as_object_mut()
            .expect("StartResponse serializes as an object")
            .insert(
                PREPARATION_REASON_RESPONSE_FIELD.to_owned(),
                serde_json::Value::String(format!("planned_relocation:{}", fence.identity())),
            );
    }
    let Ok(response_json) = serde_json::to_string(&response_value) else {
        return;
    };
    let executor = crate::playback_control::PreparationExecutor::new(
        Arc::clone(&state.store),
        gate,
        route.user_id,
        route.playback_id.clone(),
        route.owner_node_id.clone(),
        route.owner_epoch,
    )
    // The ask this successor is being built for, recorded on the slot so an
    // acknowledgement arriving later is judged against the ask that is current
    // then rather than against the one that started the work.
    .asking(desired_digest.clone());
    let preparation = plurx_core::domain::MediaSessionPreparation {
        incarnation_id: staged_incarnation_id,
        session_id: staged_session_id,
        user_id: route.user_id,
        playback_id: route.playback_id.clone(),
        // Recorded now rather than read fresh at commit, so a lost CAS aborts
        // this successor and never reaps a newer player generation.
        expected_predecessor_incarnation_id: route.incarnation_id.clone(),
        expected_predecessor_owner_node_id: route.owner_node_id.clone(),
        expected_predecessor_owner_epoch: route.owner_epoch,
        // The successor's own intent, not the predecessor's: the
        // fingerprint encodes `kind` and `start_seconds`, both of which this
        // row deliberately changes.
        request_fingerprint: staged_request.durable_intent_fingerprint(route.user_id),
        owner_node_id: successor_owner.to_owned(),
        recipe_json,
        response_json,
        media_origin_ms: if staged_request.presentation == crate::transcode::Presentation::Vod {
            0
        } else {
            resume_ms
        },
        now_ms,
        // Read here rather than carried from the exchange that triggered this,
        // and re-compared inside the admission transaction. The awaits between
        // that exchange and this line — the source file read, the height
        // resolution — are exactly where a newer ask lands, and a value
        // captured before them would prove only that the ask had not changed
        // before the work started.
        expected_desired_revision: state
            .store
            .desired_selection(route.user_id, &route.playback_id)
            .await
            .ok()
            .flatten()
            .map(|desired| desired.revision),
        deadline_ms: now_ms.saturating_add(PREPARATION_DEADLINE_MS),
    };
    if prime_worker {
        if let PreparationPurpose::PlannedRelocation(fence) = purpose {
            if !state.serving.planned_outage_is_current(fence).await {
                crate::playback_control::record_preparation_staged(false);
                return;
            }
        }
        let active = ActivePreparedSuccessor {
            state: state.clone(),
            executor: executor.clone(),
            preparation: preparation.clone(),
            purpose,
            cancelled: tokio_util::sync::CancellationToken::new(),
        };
        // Test-only: see `preparation_registration_delays`. This is the only
        // way to put a supersession into the window the comment below names,
        // because in production that window contains no await at all.
        #[cfg(test)]
        if let Some(delay) = take_preparation_registration_delay(&preparation.playback_id) {
            tokio::time::sleep(delay).await;
        }
        // Publish cancellation ownership before the Store future is polled.
        // A wait or settings disable may then cancel an in-flight reservation;
        // the detached reservation owner reconciles a late commit exactly.
        register_active_preparation(active.clone());
        // Between this task's last await and the line above there is no
        // suspension point, so a supersession landing in that window is
        // invisible to the guard's own checks. Reading the flag once more here,
        // after the registry names the successor, is what closes it: from this
        // point on `cancel_preparations_for_superseded_predecessor` can see the
        // entry itself, and before it the guard could.
        if pending_candidate_superseded(&preparation.playback_id, desired_digest.as_deref()) {
            if let Some(active) = take_active_preparation(&preparation.incarnation_id) {
                spawn_cancelled_preparation(active, "predecessor superseded by a new session");
            }
            crate::playback_control::record_preparation_staged(false);
            return;
        }
        arm_preparation_foreground_watch(active.clone());
        let Some(reservation) = reserve_preparation_before(active.clone()).await else {
            crate::playback_control::record_preparation_staged(false);
            return;
        };
        if let PreparationPurpose::PlannedRelocation(fence) = purpose {
            if !state.serving.planned_outage_is_current(fence).await {
                crate::playback_control::record_preparation_staged(false);
                return;
            }
        }
        let prime_deadline = Instant::now() + PREPARATION_PRIME_BUDGET;
        let prime = async {
            if successor_owner == state.node_id {
                if staged_request.presentation == crate::transcode::Presentation::Live {
                    prime_live_prepared_session(
                        state,
                        &staged_recipe,
                        &preparation.session_id,
                        &route.recovery_epoch,
                        1,
                        prime_deadline.into(),
                    )
                    .await
                } else {
                    match state
                        .transcode
                        .session_adoption_token(&preparation.session_id)
                    {
                        Some(adoption) => {
                            state
                                .transcode
                                .vod_resurrect_before(
                                    &preparation.recipe_json,
                                    &preparation.session_id,
                                    preparation.user_id,
                                    adoption,
                                    prime_deadline,
                                    true,
                                )
                                .await
                        }
                        None => false,
                    }
                }
            } else {
                state
                    .media_sessions
                    .prepare_remote(
                        successor_owner,
                        &RemotePrepareRequest {
                            protocol_version: crate::media_pool::PROTOCOL_VERSION,
                            incarnation_id: preparation.incarnation_id.clone(),
                            session_id: preparation.session_id.clone(),
                            user_id: preparation.user_id,
                            expected_owner_epoch: 1,
                        },
                        prime_deadline.into(),
                    )
                    .await
                    .is_ok()
            }
        };
        let primed = tokio::select! {
            () = active.cancelled.cancelled() => false,
            primed = prime => primed,
        };
        if !primed {
            crate::playback_control::record_preparation_staged(false);
            return;
        }
        if let PreparationPurpose::PlannedRelocation(fence) = purpose {
            if !state.serving.planned_outage_is_current(fence).await {
                crate::playback_control::record_preparation_staged(false);
                return;
            }
        }
        let activated = tokio::select! {
            () = active.cancelled.cancelled() => None,
            activated = activate_preparation_before(reservation) => activated,
        };
        if let Some(guard) = activated {
            let active = guard.disarm();
            crate::playback_control::record_preparation_staged(true);
            arm_preparation_deadline(active);
        } else {
            crate::playback_control::record_preparation_staged(false);
        }
        return;
    }

    let staged = tokio::time::timeout(PREPARATION_STORE_BUDGET, executor.stage(&preparation)).await;
    match staged {
        Ok(Ok(true)) => {
            crate::playback_control::record_preparation_staged(true);
        }
        Ok(Ok(false)) | Ok(Err(_)) | Err(_) => {
            crate::playback_control::record_preparation_staged(false);
        }
    }
}

async fn retire_prepared_worker(
    state: &AppState,
    owner_node_id: &str,
    incarnation_id: &str,
    session_id: &str,
    reason: &'static str,
) {
    if owner_node_id == state.node_id {
        state
            .transcode
            .begin_session_terminal(session_id, crate::vodserve::Terminal::Replaced, reason)
            .await;
        state.transcode.complete_session_release(session_id);
    } else if let Err(error) = state
        .media_sessions
        .abort_remote(
            owner_node_id,
            &RemoteAbortRequest {
                incarnation_id: incarnation_id.to_owned(),
                session_id: session_id.to_owned(),
                expected_owner_epoch: 1,
                reason: None,
            },
        )
        .await
    {
        tracing::warn!(?error, owner = %owner_node_id, %reason, "remote prepared worker cleanup did not settle");
    }
}

/// Free the actor slot, durable row, and exact worker when nobody claims the
/// successor before its fixed deadline. The registry transfer means a commit,
/// explicit abort, settings disable, or incumbent wait can take ownership
/// first; the timer then observes no entry and cannot retire the winner.
fn arm_preparation_deadline(active: ActivePreparedSuccessor) {
    let deadline_ms = active.preparation.deadline_ms.saturating_sub(unix_ms());
    let staged_incarnation_id = active.preparation.incarnation_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(deadline_ms.max(0) as u64)).await;
        if let Some(active) = take_active_preparation(&staged_incarnation_id) {
            settle_cancelled_preparation(active, "prepared successor expired").await;
        }
    });
}

async fn status_local_before(
    state: &AppState,
    session: &str,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    status_local_before_with_relay(state, session, request_deadline, true).await
}

async fn status_local_before_with_relay(
    state: &AppState,
    session: &str,
    request_deadline: Instant,
    relay_new_owner: bool,
) -> Result<Response, ApiError> {
    // Durable authority is classified before touching process-local
    // telemetry. Random/ended/remote capabilities therefore cannot make a
    // lingering actor do work or leak whether it still exists.
    let resolution = state
        .media_sessions
        .authoritative_route_resolution_before(session, &state.node_id, request_deadline)
        .await
        .map_err(|_| response_publication_timeout())?;
    let mut route = match resolution {
        DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
        DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
        DurableRouteResolution::OwnerTransition(lost) => {
            return Err(owner_transition_answer(&lost))
        }
        DurableRouteResolution::ActiveRemote(_) if !relay_new_owner => {
            return Err(ApiError::Conflict(
                "media owner changed during status lookup".to_owned(),
            ))
        }
        DurableRouteResolution::ActiveRemote(route) => route,
        DurableRouteResolution::ActiveLocal(admitted_route) => {
            #[cfg(test)]
            record_status_telemetry_lookup(session);
            let local_publication = tokio::time::timeout_at(
                tokio::time::Instant::from_std(request_deadline),
                state.transcode.hls_session_status_publication(session),
            )
            .await
            .map_err(|_| response_publication_timeout())?;
            match state
                .media_sessions
                .authoritative_route_resolution_before(session, &state.node_id, request_deadline)
                .await
                .map_err(|_| response_publication_timeout())?
            {
                DurableRouteResolution::ActiveLocal(current)
                    if same_route_authority(&admitted_route, &current) =>
                {
                    let publication = local_publication.ok_or_else(media_owner_transition)?;
                    authorize_attempt_status(
                        state,
                        session,
                        &publication.owner,
                        "status",
                        None,
                        request_deadline,
                    )
                    .await?;
                    return publication
                        .result
                        .ok()
                        .map(|info| Json(info).into_response())
                        .ok_or_else(media_owner_transition);
                }
                DurableRouteResolution::ActiveRemote(route) if relay_new_owner => route,
                DurableRouteResolution::ActiveRemote(_) => {
                    return Err(ApiError::Conflict(
                        "media owner changed during status lookup".to_owned(),
                    ));
                }
                DurableRouteResolution::Absent => {
                    return Err(ApiError::NotFound("hls session"));
                }
                DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
                DurableRouteResolution::OwnerTransition(lost) => {
                    return Err(owner_transition_answer(&lost));
                }
                // Reached from inside the outer `ActiveLocal` branch after
                // `same_route_authority` failed: this node was already the
                // owner and its incarnation or epoch moved under the read.
                // That is a genuine transition and stays retryable.
                DurableRouteResolution::ActiveLocal(_) => {
                    return Err(media_owner_transition());
                }
            }
        }
    };

    for attempt in 0..2 {
        let deadline_unix_ms =
            resource_deadline_unix_ms(request_deadline).ok_or_else(response_publication_timeout)?;
        let response = state
            .media_sessions
            .relay(
                &route.owner_node_id,
                &RelayRequest {
                    session_id: session.to_owned(),
                    resource: RelayResource::Status,
                    deadline_unix_ms,
                    headers: RelayHeaders::default(),
                },
            )
            .await
            .map_err(|error| {
                ApiError::ServiceUnavailable(format!(
                    "media worker status relay unavailable: {error:?}"
                ))
            })?;
        let peer_status = response.status();
        if !relay_status_requires_reclassification(peer_status) {
            return Ok(response);
        }
        let resolution = state
            .media_sessions
            .authoritative_route_resolution_before(session, &state.node_id, request_deadline)
            .await
            .map_err(|_| response_publication_timeout())?;
        match resolution {
            DurableRouteResolution::Absent if peer_status == StatusCode::NOT_FOUND => {
                return Ok(response);
            }
            DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
            DurableRouteResolution::Terminal(_) if peer_status == StatusCode::GONE => {
                return Ok(response);
            }
            DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
            DurableRouteResolution::OwnerTransition(lost) => {
                return Err(owner_transition_answer(&lost))
            }
            // Reached from `ActiveRemote`: this node took ownership between
            // the relay and its reclassification. Also a genuine transition.
            DurableRouteResolution::ActiveLocal(_) => return Err(media_owner_transition()),
            DurableRouteResolution::ActiveRemote(next_route) if attempt == 0 => {
                drop(response);
                route = next_route;
            }
            DurableRouteResolution::ActiveRemote(_) => return Err(media_owner_transition()),
        }
    }
    unreachable!("bounded status relay reclassification returns on every branch")
}

fn same_route_authority(left: &MediaSessionRoute, right: &MediaSessionRoute) -> bool {
    left.incarnation_id == right.incarnation_id
        && left.owner_node_id == right.owner_node_id
        && left.owner_epoch == right.owner_epoch
}

#[cfg(test)]
fn status_telemetry_observers(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>>
{
    static OBSERVERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>>,
    > = std::sync::OnceLock::new();
    OBSERVERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn record_status_telemetry_lookup(session: &str) {
    if let Some(observer) = status_telemetry_observers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session)
        .cloned()
    {
        observer.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
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

/// Linearize one fully prepared response against the exact rolling attempt or
/// immutable VOD attachment that supplied it. Callers may prepare a buffered
/// response locally, but must not expose it or construct a streaming reader or
/// body until this succeeds.
async fn authorize_response_publication(
    state: &AppState,
    session: &str,
    owner: &crate::transcode::MediaResponseOwner,
    publication: crate::transcode::MediaResponsePublication,
    deadline: Instant,
) -> Result<crate::transcode::MediaResponseAuthorization, ApiError> {
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state
            .transcode
            .authorize_response_publication(session, owner, publication, deadline),
    )
    .await
    {
        Ok(Ok(authorization)) => Ok(authorization),
        Ok(Err(rejection)) => {
            Err(response_publication_rejection_before(state, session, rejection, deadline).await)
        }
        Err(_) => Err(response_publication_timeout()),
    }
}

/// Commit completion using the authorization issued for these exact response
/// bytes. The opaque token prevents EOF from reconstructing authority from a
/// reusable session id or an object name after a successor has taken over.
async fn commit_authorized_media(
    state: &AppState,
    session: &str,
    authorization: crate::transcode::MediaResponseAuthorization,
    complete_object: bool,
    deadline: Instant,
) -> Result<(), ApiError> {
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state
            .transcode
            .commit_authorized_media(authorization, complete_object, deadline),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(rejection)) => {
            Err(response_publication_rejection_before(state, session, rejection, deadline).await)
        }
        Err(_) => Err(response_publication_timeout()),
    }
}

/// Fence a bodyless status against the exact attempt that classified it.
/// Dropping the authorization is intentional: no media body completed, so the
/// response must not renew demand or advance a delivery frontier.
async fn authorize_attempt_status(
    state: &AppState,
    session: &str,
    owner: &crate::transcode::MediaResponseOwner,
    kind: &'static str,
    object_name: Option<&str>,
    deadline: Instant,
) -> Result<(), ApiError> {
    let _authorization = authorize_response_publication(
        state,
        session,
        owner,
        crate::transcode::MediaResponsePublication::attempt_status(kind, object_name),
        deadline,
    )
    .await?;
    Ok(())
}

fn response_publication_deadline() -> Instant {
    tokio::time::Instant::now().into_std() + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET
}

fn response_publication_deadline_before(request_deadline: Instant) -> Instant {
    response_publication_deadline().min(request_deadline)
}

fn playlist_request_deadlines(state: &AppState) -> (Instant, Instant) {
    let playlist_deadline = state.transcode.playlist_request_deadline();
    let request_deadline = playlist_deadline + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET;
    (playlist_deadline, request_deadline)
}

fn playlist_request_deadlines_before(
    state: &AppState,
    request_deadline: Instant,
) -> (Instant, Instant) {
    let reserved = request_deadline
        .checked_sub(RESPONSE_PUBLICATION_LIFECYCLE_BUDGET)
        .unwrap_or(request_deadline);
    (
        state.transcode.playlist_request_deadline().min(reserved),
        request_deadline,
    )
}

fn resource_deadline_unix_ms(deadline: Instant) -> Option<i64> {
    let now_unix_ms = unix_ms();
    let remaining = deadline.checked_duration_since(Instant::now())?;
    let remaining_ms = i64::try_from(remaining.as_millis())
        .ok()
        .filter(|ms| *ms > 0)?;
    Some(now_unix_ms.saturating_add(remaining_ms))
}

fn inherited_resource_deadline(request: &RelayRequest) -> Option<Instant> {
    let now = Instant::now();
    Some(now + request.owner_budget_at(unix_ms())?)
}

fn segment_request_deadline() -> Instant {
    tokio::time::Instant::now().into_std() + SEGMENT_REQUEST_LIFECYCLE_BUDGET
}

async fn vod_playlist_before(
    state: &AppState,
    session: &str,
    deadline: Instant,
) -> Result<Option<crate::transcode::VodResponsePublication<Vec<u8>>>, ApiError> {
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.transcode.vod_playlist(session),
    )
    .await
    .map_err(|_| response_publication_timeout())
}

async fn vod_segment_before(
    state: &AppState,
    session: &str,
    segment: &str,
    deadline: Instant,
) -> Result<
    Option<crate::transcode::VodResponsePublication<Option<crate::vodserve::SegmentReady>>>,
    ApiError,
> {
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.transcode.vod_segment(session, segment),
    )
    .await
    .map_err(|_| response_publication_timeout())
}

async fn admitted_vod_publication<T>(
    state: &AppState,
    session: &str,
    publication: crate::transcode::VodResponsePublication<T>,
    kind: &'static str,
    object_name: Option<&str>,
    deadline: Instant,
) -> Result<(T, crate::transcode::MediaResponseOwner), ApiError> {
    let crate::transcode::VodResponsePublication { result, owner } = publication;
    match result {
        Ok(value) => Ok((value, owner)),
        Err(error) => {
            authorize_attempt_status(state, session, &owner, kind, object_name, deadline).await?;
            Err(vod_error(session, error))
        }
    }
}

fn response_publication_rejection(
    rejection: crate::transcode::MediaResponsePublicationRejection,
) -> ApiError {
    match rejection {
        crate::transcode::MediaResponsePublicationRejection::OwnerGone => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_owner_transition",
            "the stream owner changed while the response was prepared; retry shortly",
        ),
        crate::transcode::MediaResponsePublicationRejection::StateChanged => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_state_changed",
            "the stream changed state while the response was prepared; retry shortly",
        ),
        crate::transcode::MediaResponsePublicationRejection::ProducerEnded(reason) => {
            let error = PlaylistError::ProducerEnded(reason);
            ApiError::typed(StatusCode::BAD_GATEWAY, error.code(), error.message())
        }
    }
}

async fn response_publication_rejection_before(
    state: &AppState,
    session: &str,
    rejection: crate::transcode::MediaResponsePublicationRejection,
    deadline: Instant,
) -> ApiError {
    if !matches!(
        &rejection,
        crate::transcode::MediaResponsePublicationRejection::OwnerGone
    ) {
        return response_publication_rejection(rejection);
    }
    match state
        .media_sessions
        .authoritative_route_resolution_before(session, &state.node_id, deadline)
        .await
    {
        Ok(DurableRouteResolution::Absent) => ApiError::NotFound("transcode session"),
        Ok(DurableRouteResolution::Terminal(_)) => ApiError::typed(
            StatusCode::GONE,
            "media_session_ended",
            "this media session is no longer active",
        ),
        Ok(DurableRouteResolution::OwnerTransition(lost)) => owner_transition_answer(&lost),
        Ok(DurableRouteResolution::ActiveLocal(_))
        | Ok(DurableRouteResolution::ActiveRemote(_)) => response_publication_rejection(rejection),
        Err(_) => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_owner_reclassification_unavailable",
            "the stream owner could not be reclassified before the request deadline; retry shortly",
        ),
    }
}

fn response_publication_timeout() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "response_publication_timeout",
        "response publication did not settle before its control deadline; retry shortly",
    )
}

fn response_publication_state_changed(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Typed {
            code: "response_state_changed",
            ..
        }
    )
}

fn response_completion_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RESPONSE_COMPLETION_CAPACITY))),
    )
}

async fn reserve_response_completion(
    deadline: Instant,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    reserve_response_completion_from(response_completion_slots(), deadline).await
}

async fn reserve_response_completion_from(
    slots: Arc<tokio::sync::Semaphore>,
    deadline: Instant,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        slots.acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) | Err(_) => Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_completion_capacity",
            "response completion capacity is full; retry shortly",
        )),
    }
}

/// Once storage has produced every advertised byte, completion owns its own
/// bounded task. The body consumer is allowed to stop polling immediately
/// after the final chunk; dropping that consumer must not discard an exact
/// EOF token or cancel it halfway through the actor/registry projection.
fn settle_streamed_response_completion(
    manager: Arc<crate::transcode::TranscodeManager>,
    session: String,
    authorization: crate::transcode::MediaResponseAuthorization,
    complete_object: bool,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let deadline = response_publication_deadline();
    tokio::spawn(async move {
        let _completion_permit = permit;
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            manager.commit_authorized_media(authorization, complete_object, deadline),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(rejection)) => tracing::debug!(
                session = %crate::transcode::session_log_id(&session),
                ?rejection,
                "discarded exact response completion after publication state changed"
            ),
            Err(_) => tracing::warn!(
                session = %crate::transcode::session_log_id(&session),
                "exact response completion exceeded its control deadline"
            ),
        }
    });
}

type StreamedResponseCompletion = (
    Arc<crate::transcode::TranscodeManager>,
    String,
    crate::transcode::MediaResponseAuthorization,
    bool,
    tokio::sync::OwnedSemaphorePermit,
);

const LOCAL_MEDIA_BODY_CHANNEL_CAPACITY: usize = 1;

struct DrivenLocalChunk {
    bytes: Bytes,
    accepted: tokio::sync::oneshot::Sender<()>,
}

#[derive(Clone)]
struct StreamedBodyTerminal {
    failure: Arc<std::sync::Mutex<Option<(std::io::ErrorKind, String)>>>,
    signal: tokio_util::sync::CancellationToken,
}

impl StreamedBodyTerminal {
    fn new() -> Self {
        Self {
            failure: Arc::new(std::sync::Mutex::new(None)),
            signal: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn fail(&self, kind: std::io::ErrorKind, message: String) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.is_none() {
            *failure = Some((kind, message));
        }
        drop(failure);
        self.signal.cancel();
    }

    fn take_error(&self) -> Option<std::io::Error> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(|(kind, message)| std::io::Error::new(kind, message))
    }
}

/// Build the public side of a driven local body. The producer task owns the
/// file, delivery tracker, authorization, and completion permit, so socket
/// backpressure cannot prevent either body deadline from advancing or retain
/// those resources after the receiver disappears.
fn driven_local_body(
    receiver: tokio::sync::mpsc::Receiver<DrivenLocalChunk>,
    terminal: StreamedBodyTerminal,
    body_deadline: tokio::time::Instant,
) -> Body {
    let stream = futures_util::stream::unfold(
        (receiver, terminal, body_deadline, false),
        |(mut receiver, terminal, body_deadline, finished)| async move {
            if finished {
                return None;
            }
            if let Some(error) = terminal.take_error() {
                return Some((Err(error), (receiver, terminal, body_deadline, true)));
            }
            if tokio::time::Instant::now() >= body_deadline {
                return Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    )),
                    (receiver, terminal, body_deadline, true),
                ));
            }
            let chunk = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "media response exceeded its maximum admitted body lifetime",
                        )),
                        (receiver, terminal, body_deadline, true),
                    ));
                }
                () = terminal.signal.cancelled() => {
                    let error = terminal
                        .take_error()
                        .unwrap_or_else(|| std::io::Error::other("media response producer failed"));
                    return Some((Err(error), (receiver, terminal, body_deadline, true)));
                }
                chunk = receiver.recv() => chunk,
            };
            let Some(chunk) = chunk else {
                if let Some(error) = terminal.take_error() {
                    return Some((Err(error), (receiver, terminal, body_deadline, true)));
                }
                return None;
            };
            // Recheck both fences after wakeup and before acknowledging this
            // exact chunk. If timeout/failure won concurrently with recv, the
            // ack sender drops, so the pump cannot count or commit the bytes.
            if tokio::time::Instant::now() >= body_deadline || terminal.signal.is_cancelled() {
                let error = terminal.take_error().unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    )
                });
                return Some((Err(error), (receiver, terminal, body_deadline, true)));
            }
            let _ = chunk.accepted.send(());
            Some((Ok(chunk.bytes), (receiver, terminal, body_deadline, false)))
        },
    );
    Body::from_stream(stream)
}

fn segment_publication_kind(segment: &str, requested_range: Option<(u64, u64)>) -> &'static str {
    if requested_range.is_some() {
        "segment-range"
    } else if crate::transcode::is_init_object(segment) {
        "init-segment"
    } else {
        "media-segment"
    }
}

/// Publish a response whose complete HTTP body has already been prepared in
/// memory. Constructing the value is not visibility; returning it is, so the
/// actor/registry fence and completion commit stay immediately before return.
fn bound_admitted_media_body(response: Response) -> Response {
    // A prepared playlist/init/subtitle body still needs a post-header owner:
    // without this wrapper an unpolled in-memory Body could survive forever,
    // invalidating the shared admitted-media lifetime used by handoff and
    // terminal fallback proofs.
    let (parts, body) = response.into_parts();
    let body_deadline = tokio::time::Instant::now() + MAX_ADMITTED_MEDIA_BODY_LIFETIME;
    let stream = futures_util::stream::unfold(
        (Box::pin(body.into_data_stream()), body_deadline, false),
        |(mut body, body_deadline, finished)| async move {
            if finished {
                return None;
            }
            let item = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "prepared media response exceeded its maximum admitted body lifetime",
                        )),
                        (body, body_deadline, true),
                    ));
                }
                item = body.next() => item,
            };
            match item {
                Some(Ok(bytes)) => Some((Ok(bytes), (body, body_deadline, false))),
                Some(Err(error)) => Some((
                    Err(std::io::Error::other(error.to_string())),
                    (body, body_deadline, true),
                )),
                None => None,
            }
        },
    );
    Response::from_parts(parts, Body::from_stream(stream))
}

async fn complete_buffered_response_before(
    state: &AppState,
    session: &str,
    owner: &crate::transcode::MediaResponseOwner,
    publication: crate::transcode::MediaResponsePublication,
    complete_object: bool,
    response: Response,
    deadline: Instant,
) -> Result<Response, ApiError> {
    let authorization =
        authorize_response_publication(state, session, owner, publication, deadline).await?;
    commit_authorized_media(state, session, authorization, complete_object, deadline).await?;
    Ok(bound_admitted_media_body(response))
}

async fn session_file(
    state: &AppState,
    session: &str,
    deadline: Instant,
) -> Result<
    (
        crate::transcode::HlsContext,
        MediaFile,
        crate::transcode::MediaResponseOwner,
    ),
    ApiError,
> {
    let mut resurrection_attempted = false;
    loop {
        match state
            .transcode
            .hls_presentation_before(session, deadline)
            .await
        {
            crate::transcode::HlsPresentationResolution::Ready(context, file, owner) => {
                return Ok((context, file, owner));
            }
            crate::transcode::HlsPresentationResolution::Failed(error) => {
                return match admitted_playlist_error(state, session, error, deadline).await {
                    Ok(error) => Err(error),
                    Err(()) => Err(response_publication_rejection(
                        crate::transcode::MediaResponsePublicationRejection::StateChanged,
                    )),
                };
            }
            crate::transcode::HlsPresentationResolution::StateChanged => {
                return Err(response_publication_rejection(
                    crate::transcode::MediaResponsePublicationRejection::StateChanged,
                ));
            }
            crate::transcode::HlsPresentationResolution::Gone if !resurrection_attempted => {
                resurrection_attempted = true;
                match vod_resurrected_before(state, session, deadline).await {
                    VodResurrection::Resurrected => continue,
                    VodResurrection::Absent => {
                        return Err(ApiError::NotFound("transcode session"));
                    }
                    VodResurrection::Ended => return Err(media_session_ended()),
                    VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                    VodResurrection::Unavailable => {
                        return Err(vod_resurrection_unavailable());
                    }
                }
            }
            crate::transcode::HlsPresentationResolution::Gone => {
                return Err(vod_resurrection_unavailable());
            }
        }
    }
}

/// Maximum frame rate from ffprobe's persisted source description.
///
/// Fractions are kept until the playlist is rendered so NTSC rates retain
/// their 24000/1001 or 30000/1001 meaning. `avg_frame_rate` is preferred;
/// `r_frame_rate` is the fallback for older probe output.
#[cfg(test)]
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
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Playlist {
            native: query.native,
            subtitle: query.subtitle,
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    playlist_local_before(&state, &session, query, playlist_deadline, request_deadline).await
}

async fn playlist_local_before(
    state: &AppState,
    session: &str,
    query: PlaylistQuery,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let initial_vod_deadline = response_publication_deadline_before(request_deadline);
    // A VOD session's child media playlist is the plan's immutable artifact.
    // The dedicated master path wraps it when native subtitles were requested;
    // the legacy `?native=1` bridge still needs that same wrapper.
    if let Some(answer) = vod_playlist_before(state, session, initial_vod_deadline).await? {
        let (bytes, playlist_owner) = admitted_vod_publication(
            state,
            session,
            answer,
            "vod-playlist",
            None,
            initial_vod_deadline,
        )
        .await?;
        if query.native == Some(1) {
            let (context, file, owner) = session_file(state, session, initial_vod_deadline).await?;
            let context =
                exact_hls_context_before(state, session, context, &owner, initial_vod_deadline)
                    .await?;
            let response =
                playlist_response(master_playlist(&file, query.subtitle, &context).into_bytes());
            return complete_buffered_response_before(
                state,
                session,
                &owner,
                crate::transcode::MediaResponsePublication::generation_metadata("master-playlist"),
                true,
                response,
                initial_vod_deadline,
            )
            .await;
        }
        let response = playlist_response(bytes);
        return complete_buffered_response_before(
            state,
            session,
            &playlist_owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "playlist",
                Some("index.m3u8"),
            ),
            true,
            response,
            initial_vod_deadline,
        )
        .await;
    }
    if query.native != Some(1) {
        return video_playlist_local_before(
            state,
            session,
            "index.m3u8",
            playlist_deadline,
            request_deadline,
        )
        .await;
    }
    let deadline = response_publication_deadline_before(request_deadline);
    let (context, file, owner) = session_file(state, session, deadline).await?;
    let context = exact_hls_context_before(state, session, context, &owner, deadline).await?;
    let response = playlist_response(master_playlist(&file, query.subtitle, &context).into_bytes());
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::generation_metadata("master-playlist"),
        true,
        response,
        deadline,
    )
    .await
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
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Master {
            subtitle: query.subtitle,
            diagnostic: query.diagnostic.clone(),
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    master_playlist_response_local_before(
        &state,
        &session,
        query,
        playlist_deadline,
        request_deadline,
    )
    .await
}

async fn master_playlist_response_local_before(
    state: &AppState,
    session: &str,
    query: PlaylistQuery,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let deadline = response_publication_deadline_before(request_deadline);
    let (context, file, owner) = session_file(state, session, deadline).await?;
    let context = exact_hls_context_before(state, session, context, &owner, deadline).await?;
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
        return video_playlist_local_before(
            state,
            session,
            "master.m3u8",
            playlist_deadline,
            request_deadline,
        )
        .await;
    }
    let response = playlist_response(
        master_playlist_diagnostic(&file, query.subtitle, &context, query.diagnostic.as_deref())
            .into_bytes(),
    );
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::generation_metadata("master-playlist"),
        true,
        response,
        deadline,
    )
    .await
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
        PlaylistError::ProducerExited(_)
        | PlaylistError::ProducerEnded(_)
        | PlaylistError::SessionFailed(_)
        | PlaylistError::InsufficientCapacity(_) => StatusCode::BAD_GATEWAY,
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

async fn admitted_playlist_error(
    state: &AppState,
    session: &str,
    err: crate::transcode::PlaylistPublicationError,
    deadline: Instant,
) -> Result<ApiError, ()> {
    if let Some(owner) = err.owner.as_ref() {
        match state
            .transcode
            .authorize_playlist_error_publication(session, owner, &err.error, deadline)
            .await
        {
            Ok(()) => {}
            Err(crate::transcode::MediaResponsePublicationRejection::StateChanged) => {
                // The same generation changed attempt/publication/decision
                // state during admission. Re-resolve instead of relabeling a
                // live producer as an anonymous fatal 404.
                return Err(());
            }
            Err(crate::transcode::MediaResponsePublicationRejection::OwnerGone) => {
                return Ok(response_publication_rejection_before(
                    state,
                    session,
                    crate::transcode::MediaResponsePublicationRejection::OwnerGone,
                    deadline,
                )
                .await);
            }
            Err(crate::transcode::MediaResponsePublicationRejection::ProducerEnded(reason)) => {
                return Ok(playlist_error(
                    session,
                    PlaylistError::ProducerEnded(reason),
                ));
            }
        }
    }
    Ok(playlist_error(session, err.error))
}

/// The video rendition referenced by the native-subtitle HLS master.
pub async fn video_playlist(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::VideoPlaylist,
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    video_playlist_local_before(
        &state,
        &session,
        "video.m3u8",
        playlist_deadline,
        request_deadline,
    )
    .await
}

async fn video_playlist_local_before(
    state: &AppState,
    session: &str,
    object_name: &'static str,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let initial_vod_deadline = response_publication_deadline_before(request_deadline);
    if let Some(answer) = vod_playlist_before(state, session, initial_vod_deadline).await? {
        let (bytes, owner) = admitted_vod_publication(
            state,
            session,
            answer,
            "vod-video-playlist",
            None,
            initial_vod_deadline,
        )
        .await?;
        let response = playlist_response(bytes);
        return complete_buffered_response_before(
            state,
            session,
            &owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "playlist",
                Some(object_name),
            ),
            true,
            response,
            initial_vod_deadline,
        )
        .await;
    }
    let mut publication_deadline = None;
    for reclassification in 0..=2 {
        match state
            .transcode
            .playlist_with_owner_before(session, playlist_deadline)
            .await
        {
            Ok((bytes, owner)) => {
                let response_deadline = *publication_deadline
                    .get_or_insert_with(|| response_publication_deadline_before(request_deadline));
                let response = playlist_response(bytes);
                let result = complete_buffered_response_before(
                    state,
                    session,
                    &owner,
                    crate::transcode::MediaResponsePublication::attempt_media(
                        "playlist",
                        Some(object_name),
                    ),
                    true,
                    response,
                    response_deadline,
                )
                .await;
                match result {
                    Err(error)
                        if response_publication_state_changed(&error)
                            && reclassification < 2
                            && tokio::time::Instant::now().into_std() < response_deadline =>
                    {
                        continue;
                    }
                    result => return result,
                }
            }
            Err(err) if matches!(&err.error, PlaylistError::SessionGone) => {
                match vod_resurrected_before(state, session, playlist_deadline).await {
                    VodResurrection::Absent => return Err(playlist_error(session, err.error)),
                    VodResurrection::Ended => return Err(media_session_ended()),
                    VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                    VodResurrection::Unavailable => return Err(vod_resurrection_unavailable()),
                    VodResurrection::Resurrected => {}
                }
                let answer = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(playlist_deadline),
                    state.transcode.vod_playlist(session),
                )
                .await
                .map_err(|_| vod_resurrection_unavailable())?
                .ok_or_else(vod_resurrection_unavailable)?;
                let response_deadline = *publication_deadline
                    .get_or_insert_with(|| response_publication_deadline_before(request_deadline));
                let (bytes, owner) = admitted_vod_publication(
                    state,
                    session,
                    answer,
                    "vod-video-playlist",
                    None,
                    response_deadline,
                )
                .await?;
                let response = playlist_response(bytes);
                return complete_buffered_response_before(
                    state,
                    session,
                    &owner,
                    crate::transcode::MediaResponsePublication::attempt_media(
                        "playlist",
                        Some(object_name),
                    ),
                    true,
                    response,
                    response_deadline,
                )
                .await;
            }
            Err(err) => match admitted_playlist_error(
                state,
                session,
                err,
                *publication_deadline
                    .get_or_insert_with(|| response_publication_deadline_before(request_deadline)),
            )
            .await
            {
                Ok(error) => return Err(error),
                Err(())
                    if reclassification < 2
                        && tokio::time::Instant::now().into_std()
                            < publication_deadline.expect("publication deadline initialized") =>
                {
                    continue;
                }
                Err(()) => {
                    return Err(ApiError::typed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "playlist_state_changed",
                        "the stream changed state while the playlist response was prepared; retry shortly",
                    ));
                }
            },
        }
    }
    unreachable!("bounded playlist reclassification loop returns on every terminal branch")
}

/// One native WebVTT rendition's media playlist. Its segments mirror the
/// video rendition so AVPlayer sees matching playlist types and timelines.
/// Every child resource is still cut from the one cached sidecar.
pub async fn subtitle_playlist(
    State(state): State<AppState>,
    AxPath((session, index)): AxPath<(String, i64)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::SubtitlePlaylist { index },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    subtitle_playlist_local_before(&state, &session, index, playlist_deadline, request_deadline)
        .await
}

#[cfg(test)]
async fn subtitle_playlist_local(
    state: &AppState,
    session: &str,
    index: i64,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(state);
    subtitle_playlist_local_before(state, session, index, playlist_deadline, request_deadline).await
}

async fn subtitle_playlist_local_before(
    state: &AppState,
    session: &str,
    index: i64,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let initial_vod_deadline = response_publication_deadline_before(request_deadline);
    // Resolve the typed rolling playlist verdict before asking for frozen
    // subtitle facts. A failed actor is stored before End; consulting the
    // live-only presentation facade first would erase that still-registered
    // failure into a 404 and bypass its exact 502 owner fence.
    if let Some(answer) = vod_playlist_before(state, session, initial_vod_deadline).await? {
        let (video, owner) = admitted_vod_publication(
            state,
            session,
            answer,
            "vod-subtitle-playlist",
            None,
            initial_vod_deadline,
        )
        .await?;
        return complete_subtitle_playlist_response(
            state,
            session,
            index,
            video,
            owner,
            initial_vod_deadline,
        )
        .await;
    }

    let mut publication_deadline = None;
    for reclassification in 0..=2 {
        let (video, owner) = match state
            .transcode
            .playlist_with_owner_before(session, playlist_deadline)
            .await
        {
            Ok(answer) => answer,
            Err(err) => {
                if matches!(&err.error, PlaylistError::SessionGone) {
                    match vod_resurrected_before(state, session, playlist_deadline).await {
                        VodResurrection::Absent => {}
                        VodResurrection::Ended => return Err(media_session_ended()),
                        VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                        VodResurrection::Unavailable => return Err(vod_resurrection_unavailable()),
                        VodResurrection::Resurrected => {
                            let answer = tokio::time::timeout_at(
                                tokio::time::Instant::from_std(playlist_deadline),
                                state.transcode.vod_playlist(session),
                            )
                            .await
                            .map_err(|_| vod_resurrection_unavailable())?
                            .ok_or_else(vod_resurrection_unavailable)?;
                            let response_deadline =
                                *publication_deadline.get_or_insert_with(|| {
                                    response_publication_deadline_before(request_deadline)
                                });
                            let (video, owner) = admitted_vod_publication(
                                state,
                                session,
                                answer,
                                "vod-subtitle-playlist",
                                None,
                                response_deadline,
                            )
                            .await?;
                            return complete_subtitle_playlist_response(
                                state,
                                session,
                                index,
                                video,
                                owner,
                                response_deadline,
                            )
                            .await;
                        }
                    }
                }
                match admitted_playlist_error(
                    state,
                    session,
                    err,
                    *publication_deadline.get_or_insert_with(|| {
                        response_publication_deadline_before(request_deadline)
                    }),
                )
                .await
                {
                    Ok(error) => return Err(error),
                    Err(())
                        if reclassification < 2
                            && tokio::time::Instant::now().into_std()
                                < publication_deadline
                                    .expect("publication deadline initialized") =>
                    {
                        continue;
                    }
                    Err(()) => {
                        return Err(ApiError::typed(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "playlist_state_changed",
                            "the stream changed state while the subtitle playlist was prepared; retry shortly",
                        ));
                    }
                }
            }
        };
        let result = complete_subtitle_playlist_response(
            state,
            session,
            index,
            video,
            owner,
            *publication_deadline
                .get_or_insert_with(|| response_publication_deadline_before(request_deadline)),
        )
        .await;
        match result {
            Err(error)
                if response_publication_state_changed(&error)
                    && reclassification < 2
                    && tokio::time::Instant::now().into_std()
                        < publication_deadline.expect("publication deadline initialized") =>
            {
                continue;
            }
            result => return result,
        }
    }
    unreachable!("bounded subtitle playlist reclassification loop always returns")
}

async fn complete_subtitle_playlist_response(
    state: &AppState,
    session: &str,
    index: i64,
    video: Vec<u8>,
    owner: crate::transcode::MediaResponseOwner,
    deadline: Instant,
) -> Result<Response, ApiError> {
    let (_, file) = match state
        .transcode
        .hls_presentation_for_owner_before(session, &owner, deadline)
        .await
    {
        Ok(presentation) => presentation,
        Err(rejection) => {
            return Err(
                response_publication_rejection_before(state, session, rejection, deadline).await,
            )
        }
    };
    let Some(track) = file.subtitle_streams.get(index as usize) else {
        authorize_attempt_status(state, session, &owner, "subtitle-playlist", None, deadline)
            .await?;
        return Err(ApiError::NotFound("subtitle track"));
    };
    if !is_native_text_subtitle(&track.codec) {
        authorize_attempt_status(state, session, &owner, "subtitle-playlist", None, deadline)
            .await?;
        return Err(ApiError::BadRequest(
            "this subtitle requires burn-in".into(),
        ));
    }
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        crate::subtitles::warm_vtt(&state.subs_dir, &file, index),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
    let response = playlist_response(subtitle_media_playlist(&video).into_bytes());
    #[cfg(test)]
    state
        .transcode
        .pause_subtitle_playlist_commit_for_test()
        .await;
    // Carry the owner resolved with the exact video bytes. A rolling wait may
    // span fallback, and a VOD attachment may be replaced under the same id;
    // a fresh lookup here would authorize the wrong incarnation in both cases.
    let object_name = format!("subs/{index}/index.m3u8");
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            "subtitle-playlist",
            Some(&object_name),
        ),
        true,
        response,
        deadline,
    )
    .await
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
    let request_deadline = response_publication_deadline();
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::SubtitleSegment {
            index,
            segment: segment.clone(),
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    subtitle_vtt_local_before(&state, &session, index, &segment, request_deadline).await
}

trait SubtitleSegmentSource: Send + Sync {
    fn read_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;

    fn read_window<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;

    fn warm_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, ()>;

    /// Start a bounded window for one playback.
    ///
    /// The session id and the control sequence it settled on are what let the
    /// subtitle owner answer M7's actual question — how many extractions does
    /// one viewer have running — rather than the key-shaped question of how
    /// many spans are in flight across the server.
    #[allow(clippy::too_many_arguments)]
    fn warm_window<'a>(
        &'a self,
        session: &'a str,
        sequence: Option<u64>,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool>;

    /// Is a producer for exactly this window span alive right now?
    ///
    /// Only asked after a `read_window` miss, and only to decide whether
    /// waiting a moment longer could turn this request's answer from an empty
    /// track into real cues. On the trait rather than called directly so the
    /// boundary fixture measures the same decision production makes.
    fn window_flight_is_live<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool>;

    /// What the whole-track sidecar for this track is doing. Observation
    /// only; it starts nothing.
    fn whole_track_state<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, crate::subtitles::SidecarState>;
}

/// How long a subtitle segment may wait for a window that is already being
/// extracted for it.
///
/// AVPlayer gives a subtitle segment about two seconds and stalls the muxed
/// video while it waits, so this is not a budget to spend freely — it exists
/// to win the *last* moment of a warm, which is the common case at a window
/// boundary once the previous segment's request kicked the next window. A
/// request that arrives at the start of an extraction still gets its empty
/// answer immediately and leaves the recovery to the client's readiness
/// retry; only a flight already in progress is worth standing still for.
const SUBTITLE_SEGMENT_PUBLICATION_WAIT: Duration = Duration::from_millis(1_500);

/// How often that wait re-reads the window path.
const SUBTITLE_SEGMENT_PUBLICATION_POLL: Duration = Duration::from_millis(100);

/// What the wait leaves of the response publication budget for the work that
/// still has to happen after it: the whole-track warm, the settled-target
/// read, the window kick and the response publication itself. Without it a
/// request that caught its cues at the deadline fails on the very next await.
const SUBTITLE_SEGMENT_PUBLICATION_RESERVE: Duration = Duration::from_millis(750);

struct ProductionSubtitleSegmentSource;

impl SubtitleSegmentSource for ProductionSubtitleSegmentSource {
    fn read_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(crate::subtitles::read_cached_vtt(dir, file, index))
    }

    fn read_window<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(crate::subtitles::read_cached_window(
            dir,
            file,
            index,
            anchor_seconds,
            window_seconds,
        ))
    }

    fn warm_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, ()> {
        Box::pin(crate::subtitles::warm_vtt(dir, file, index))
    }

    fn warm_window<'a>(
        &'a self,
        session: &'a str,
        sequence: Option<u64>,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(crate::subtitles::warm_vtt_window(
            session,
            sequence,
            dir,
            file,
            index,
            anchor_seconds,
            window_seconds,
        ))
    }

    fn window_flight_is_live<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(crate::subtitles::window_flight_is_live(
            dir,
            file,
            index,
            anchor_seconds,
            window_seconds,
        ))
    }

    fn whole_track_state<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, crate::subtitles::SidecarState> {
        Box::pin(crate::subtitles::sidecar_state(dir, file, index))
    }
}

/// Wait out the tail of a window extraction that is already running for this
/// exact span, and hand back its bytes if they land in time.
///
/// Bounded twice: by [`SUBTITLE_SEGMENT_PUBLICATION_WAIT`], which is the
/// engine's constraint, and by the response publication deadline, which is
/// the request's. Whichever is sooner wins, and missing both is not an error
/// — the caller's empty segment is still a correct answer.
async fn await_window_publication<S: SubtitleSegmentSource + ?Sized>(
    source: &S,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor: i64,
    window_seconds: i64,
    publication_deadline: Instant,
) -> Option<Vec<u8>> {
    // The handler still has to warm, settle and publish after this returns, so
    // the wait may not spend the whole remaining budget — a request that
    // caught its cues and then timed out on the work behind them has failed
    // *because* it succeeded. Leave the rest of the lifecycle its own slack.
    let reserved = publication_deadline.checked_sub(SUBTITLE_SEGMENT_PUBLICATION_RESERVE);
    let give_up = match reserved {
        Some(reserved) => (Instant::now() + SUBTITLE_SEGMENT_PUBLICATION_WAIT).min(reserved),
        // Already inside the reserve. Answer now.
        None => return None,
    };
    loop {
        // Read first: a window that published while this request was deciding
        // to wait is served without paying a poll interval for it, and the
        // loop cannot overshoot `give_up` by a whole sleep before noticing.
        if let Ok(Some(bytes)) = tokio::time::timeout_at(
            tokio::time::Instant::from_std(give_up),
            source.read_window(dir, file, index, anchor, window_seconds),
        )
        .await
        .unwrap_or(Ok(None))
        {
            return Some(bytes);
        }
        if Instant::now() + SUBTITLE_SEGMENT_PUBLICATION_POLL >= give_up {
            return None;
        }
        tokio::time::sleep(SUBTITLE_SEGMENT_PUBLICATION_POLL).await;
    }
}

async fn subtitle_vtt_local_before(
    state: &AppState,
    session: &str,
    index: i64,
    segment: &str,
    publication_deadline: Instant,
) -> Result<Response, ApiError> {
    subtitle_vtt_local_before_with_source(
        state,
        session,
        index,
        segment,
        publication_deadline,
        &ProductionSubtitleSegmentSource,
    )
    .await
}

async fn subtitle_vtt_local_before_with_source<S: SubtitleSegmentSource + ?Sized>(
    state: &AppState,
    session: &str,
    index: i64,
    segment: &str,
    publication_deadline: Instant,
    source: &S,
) -> Result<Response, ApiError> {
    let (context, file, owner) = session_file(state, session, publication_deadline).await?;
    let Some(track) = file.subtitle_streams.get(index as usize) else {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("subtitle track"));
    };
    if !is_native_text_subtitle(&track.codec) {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::BadRequest(
            "this subtitle requires burn-in".into(),
        ));
    }
    let sequence = segment
        .strip_prefix("seg")
        .and_then(|value| value.strip_suffix(".vtt"))
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|sequence| i64::try_from(sequence).ok());
    let Some(sequence) = sequence else {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("subtitle segment"));
    };
    let window = state
        .transcode
        .segment_window_for_owner_before(session, sequence, &owner, publication_deadline)
        .await;
    let window = match window {
        Ok(window) => window,
        Err(rejection) => {
            return Err(response_publication_rejection_before(
                state,
                session,
                rejection,
                publication_deadline,
            )
            .await)
        }
    };
    let Some((segment_start, segment_end)) = window else {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("subtitle segment"));
    };
    let subtitle_window_seconds = tokio::time::timeout_at(
        tokio::time::Instant::from_std(publication_deadline),
        state.subtitle_window_seconds(),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
    let (bytes, cache_control, slice_timeline) = match tokio::time::timeout_at(
        tokio::time::Instant::from_std(publication_deadline),
        source.read_whole(&state.subs_dir, &file, index),
    )
    .await
    .map_err(|_| response_publication_timeout())?
    {
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
            (bytes, "private, max-age=3600", true)
        }
        Ok(None) | Err(_) => {
            // The whole-track sidecar is not there yet. Before falling back to
            // an empty segment, ask whether a bounded window covering the
            // position this segment actually wants is available or worth
            // starting.
            //
            // Cue times in a sidecar are absolute source time and
            // `segment_start` is session-local, so the demand position is the
            // session's media origin plus this segment's start — the same
            // mapping `slice_webvtt` undoes below.
            let demand_seconds = (context.media_origin_seconds + segment_start).max(0.0) as i64;
            let anchor =
                crate::subtitles::window_anchor_seconds(demand_seconds, subtitle_window_seconds);
            let window_bytes = tokio::time::timeout_at(
                tokio::time::Instant::from_std(publication_deadline),
                source.read_window(
                    &state.subs_dir,
                    &file,
                    index,
                    anchor,
                    subtitle_window_seconds,
                ),
            )
            .await
            .map_err(|_| response_publication_timeout())?;
            // A window for this exact span may be seconds from publishing —
            // the common case at a window boundary, where the previous
            // segment's request already kicked this one. Standing still for
            // the tail of a flight that is *already running* turns an empty
            // segment into real cues without adding an extraction, and
            // without ever awaiting one that has not started.
            let window_bytes = match window_bytes {
                Ok(Some(bytes)) => Some(bytes),
                _ if source
                    .window_flight_is_live(
                        &state.subs_dir,
                        &file,
                        index,
                        anchor,
                        subtitle_window_seconds,
                    )
                    .await =>
                {
                    await_window_publication(
                        source,
                        &state.subs_dir,
                        &file,
                        index,
                        anchor,
                        subtitle_window_seconds,
                        publication_deadline,
                    )
                    .await
                }
                _ => None,
            };
            if let Some(bytes) = window_bytes {
                // Start the whole-track warm even though this request is
                // answered. A window is a bridge: it persists on disk across
                // restarts while the whole-track sidecar may not exist yet, so
                // returning here without warming would leave a viewer parked
                // past the first window served by a window forever, with the
                // authoritative extraction never kicked from this route.
                tokio::time::timeout_at(
                    tokio::time::Instant::from_std(publication_deadline),
                    source.warm_whole(&state.subs_dir, &file, index),
                )
                .await
                .map_err(|_| response_publication_timeout())?;
                tracing::info!(
                    session = %crate::transcode::session_log_id(session),
                    file_id = file.id,
                    index,
                    anchor,
                    "serving a windowed WebVTT subtitle while the whole track warms"
                );
                // `no-store`: a window covers this position and not the next
                // one, and the whole-track sidecar will supersede it. Letting
                // a player pin these bytes would pin a partial answer.
                (bytes, "no-store", true)
            } else {
                // AVPlayer gives a subtitle segment only about two seconds to
                // answer and blocks the muxed video while it waits. Extracting
                // an embedded text track is a full-source scan that can
                // legitimately take minutes on a large MKV over a NAS, so
                // awaiting `ensure_vtt` here turns healthy Dolby Vision, HDR
                // and H.264 streams into a black screen. Publish a
                // syntactically valid empty segment now and let the
                // deduplicated cache extraction finish independently.
                // `no-store` lets a player retry this window once the sidecar
                // is ready instead of pinning the temporary empty answer.
                //
                // Both warms are started, and neither is awaited. The window
                // is the bridge over the head of playback; the whole track is
                // what supersedes it and what every other consumer needs. The
                // window declines itself past the midpoint of the file, where
                // it would read the same bytes as the whole track for a
                // disposable result.
                // Read the whole track's state BEFORE warming it. `warm_vtt`
                // enlists the key in `warmups` synchronously and clears it
                // from a spawned task, and `sidecar_state` reports any
                // enlisted key as `Warming` — so asking afterwards races the
                // warm this very request just started, and a failed track
                // answers `Failed` or `Warming` depending on which task the
                // scheduler ran. A refusal that is a coin flip is worse than
                // no refusal.
                let whole_track = source
                    .whole_track_state(&state.subs_dir, &file, index)
                    .await;
                // The whole-track warm keeps its original contract, including
                // that a timeout here fails the request rather than being
                // swallowed: it is the path every other consumer depends on.
                // A track inside its failure memo is the one exception: the
                // warm cannot start an extraction while the memo stands, so
                // enlisting the key would buy nothing and would corrupt the
                // reading above for every request behind this one.
                if whole_track != crate::subtitles::SidecarState::Failed {
                    tokio::time::timeout_at(
                        tokio::time::Instant::from_std(publication_deadline),
                        source.warm_whole(&state.subs_dir, &file, index),
                    )
                    .await
                    .map_err(|_| response_publication_timeout())?;
                }
                // The destination is read here, immediately before the warm,
                // and not once at the top of the handler: a seek that lands
                // between the two reads is exactly the case M7 is about, and
                // the later read is the one that can still refuse the work.
                //
                // `None` is not staleness. It covers a session that is gone, an
                // actor that retired and a client that has not exchanged yet,
                // and in all three there is no ordering fact to be stale
                // against — so a first play still warms.
                let settled = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(publication_deadline),
                    state.transcode.settled_target_for_session(session),
                )
                .await;
                // The window is best effort by construction — it is a bridge,
                // and the empty segment below is already a correct answer — so
                // a timeout means "no window", not a failed request.
                let windowing = match settled {
                    Ok(Some(target))
                        if !target.covered_by_window(anchor, subtitle_window_seconds) =>
                    {
                        // The client has settled somewhere this window does not
                        // reach. Starting it would spend a full-source scan on
                        // a destination that is already history, which is the
                        // waste the seek-coalescing contract exists to refuse.
                        false
                    }
                    Ok(authority) => tokio::time::timeout_at(
                        tokio::time::Instant::from_std(publication_deadline),
                        source.warm_window(
                            session,
                            authority.map(|target| target.sequence),
                            &state.subs_dir,
                            &file,
                            index,
                            anchor,
                            subtitle_window_seconds,
                        ),
                    )
                    .await
                    .unwrap_or(false),
                    // Reading the authority did not fit inside the publication
                    // deadline. Starting an extraction this request could not
                    // justify is the failure mode being removed, so the empty
                    // segment answers alone.
                    Err(_) => false,
                };
                tracing::debug!(
                    session = %crate::transcode::session_log_id(session),
                    file_id = file.id,
                    index,
                    anchor,
                    windowing,
                    "serving an empty subtitle segment while its sidecar cache warms"
                );
                // An empty segment says "there are no cues here", which is
                // true while a sidecar is warming and a lie once it has
                // failed — and players keep the bytes in memory whatever
                // `no-store` says, so the lie is what a client is left with.
                // A refusal with `Retry-After` is the honest answer, and the
                // memo's own remaining time is the only moment a retry could
                // achieve anything.
                //
                // Behind an operator switch, and off by default, because the
                // cost of being honest here is not yet measured: AVPlayer
                // blocks the muxed video for about two seconds on a subtitle
                // segment, and whether each engine keeps playing video
                // through a subtitle 503 or stalls the picture has to be
                // observed per engine before this becomes the default. The
                // Developer tab reports what has been observed and does not
                // gate the switch on it.
                if whole_track == crate::subtitles::SidecarState::Failed
                    && state.subtitle_not_ready_503().await
                {
                    let retry_after =
                        crate::subtitles::failure_memo_remaining(&state.subs_dir, &file, index)
                            .await
                            .map(|remaining| remaining.as_secs().max(1))
                            .unwrap_or(1);
                    tracing::info!(
                        session = %crate::transcode::session_log_id(session),
                        file_id = file.id,
                        index,
                        retry_after,
                        "refusing a subtitle segment whose sidecar extraction failed"
                    );
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        "subtitle-segment",
                        None,
                        publication_deadline,
                    )
                    .await?;
                    return Ok((
                        StatusCode::SERVICE_UNAVAILABLE,
                        [(header::RETRY_AFTER, retry_after.to_string())],
                        [(header::CACHE_CONTROL, "no-store")],
                    )
                        .into_response());
                }
                (b"WEBVTT\n\n".to_vec(), "no-store", false)
            }
        }
    };
    let response = (
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
        if slice_timeline {
            slice_webvtt(
                &bytes,
                context.media_origin_seconds,
                segment_start,
                segment_end,
            )
        } else {
            // Keep the cold-cache fallback byte-for-byte minimal. It carries
            // no cues to shift, and this exact body is the acceptance handle
            // proving the request escaped before either producer finished.
            bytes
        },
    )
        .into_response();
    let object_name = format!("subs/{index}/{segment}");
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            "subtitle-segment",
            Some(&object_name),
        ),
        true,
        response,
        publication_deadline,
    )
    .await
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
async fn exact_hls_context_before(
    state: &AppState,
    session: &str,
    context: crate::transcode::HlsContext,
    owner: &crate::transcode::MediaResponseOwner,
    deadline: Instant,
) -> Result<crate::transcode::HlsContext, ApiError> {
    let started = Instant::now();
    let inspection_deadline = deadline
        .checked_sub(Duration::from_millis(25))
        .unwrap_or(deadline);
    let inspected = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        exact_hls_context_at(state, session, context, inspection_deadline),
    )
    .await;
    match inspected {
        Ok(Ok(context)) => {
            tracing::info!(
                session = %crate::transcode::session_log_id(session),
                phase = "init_inspection",
                outcome = "ready",
                elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                "resolved exact HLS initialization context"
            );
            Ok(context)
        }
        Ok(Err(error)) => {
            // The init lookup and parse can cross an owner replacement. A
            // predecessor's refusal must never be published against the
            // successor that reused this capability URL.
            authorize_attempt_status(
                state,
                session,
                owner,
                "master-playlist-init-inspection",
                None,
                deadline,
            )
            .await?;
            tracing::warn!(
                session = %crate::transcode::session_log_id(session),
                phase = "init_inspection",
                outcome = error.log_code(),
                elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                "HLS initialization inspection refused the playlist"
            );
            Err(error.into_api_error())
        }
        Err(_) => {
            tracing::warn!(
                session = %crate::transcode::session_log_id(session),
                phase = "init_inspection",
                outcome = "response_publication_timeout",
                elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                "HLS initialization inspection exhausted the response deadline"
            );
            Err(response_publication_timeout())
        }
    }
}

#[derive(Debug)]
enum HlsInitInspectionError {
    Response {
        status: StatusCode,
        code: &'static str,
        message: &'static str,
    },
    Api(ApiError),
}

impl HlsInitInspectionError {
    fn invalid() -> Self {
        Self::Response {
            status: StatusCode::BAD_GATEWAY,
            code: "hls_init_invalid",
            message: "the published HLS initialization media is malformed or incomplete",
        }
    }

    fn unsupported() -> Self {
        Self::Response {
            status: StatusCode::BAD_GATEWAY,
            code: "hls_init_unsupported",
            message: "the published HLS initialization media uses an unsupported codec layout",
        }
    }

    fn unavailable() -> Self {
        Self::Response {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "init_inspection_unavailable",
            message: "initialization media could not be inspected; retry shortly",
        }
    }

    fn pending() -> Self {
        Self::Response {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "startup_timeout",
            message: "initialization media is not ready yet; retry shortly",
        }
    }

    fn from_api_error(error: ApiError) -> Self {
        Self::Api(error)
    }

    fn log_code(&self) -> &'static str {
        match self {
            Self::Response { code, .. } => code,
            Self::Api(_) => "producer_or_session_failure",
        }
    }

    fn into_api_error(self) -> ApiError {
        match self {
            Self::Response {
                status,
                code,
                message,
            } => ApiError::typed(status, code, message),
            Self::Api(error) => error,
        }
    }
}

#[cfg(test)]
async fn exact_hls_context(
    state: &AppState,
    session: &str,
    context: crate::transcode::HlsContext,
) -> Result<crate::transcode::HlsContext, HlsInitInspectionError> {
    exact_hls_context_at(
        state,
        session,
        context,
        Instant::now() + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET,
    )
    .await
}

async fn exact_hls_context_at(
    state: &AppState,
    session: &str,
    mut context: crate::transcode::HlsContext,
    deadline: Instant,
) -> Result<crate::transcode::HlsContext, HlsInitInspectionError> {
    let fallback_video = context.codecs.split(',').next().unwrap_or_default();
    let Some(sample_entry) = ["hvc1", "hev1", "dvh1", "dvhe"]
        .into_iter()
        .find(|entry| fallback_video.starts_with(entry))
    else {
        return Ok(context);
    };
    // A fenced successor names its init after its ownership epoch, so the
    // object to probe comes from the session, not from a literal. Asking for
    // the wrong name does not fail fast: `segment` waits for a segment that
    // will never be produced, stalling every playlist request for the full
    // production wait before falling back to the scanner's guessed tier.
    let Some(init_object) = state.transcode.session_init_object(session).await else {
        return Err(HlsInitInspectionError::pending());
    };
    // Initialization segments are a few KiB. Bound malformed input so a
    // playlist request can never allocate without limit. VOD init bytes come
    // through their own registry; live bytes retain the internal delivery
    // tracker that keeps this probe out of player throughput telemetry.
    let mut init = Vec::new();
    match state
        .transcode
        .vod_segment_before(session, &init_object, deadline)
        .await
    {
        Some(crate::transcode::VodResponsePublication {
            result: Ok(Some(ready)),
            ..
        }) => {
            if ready.len > INIT_INSPECTION_LIMIT_BYTES {
                return Err(HlsInitInspectionError::invalid());
            }
            init.reserve(ready.len as usize);
            let mut reader = ready.file.take(ready.len);
            let read = reader
                .read_to_end(&mut init)
                .await
                .map_err(|_| HlsInitInspectionError::unavailable())?;
            if read as u64 != ready.len {
                return Err(HlsInitInspectionError::invalid());
            }
        }
        Some(crate::transcode::VodResponsePublication {
            result: Ok(None), ..
        }) => return Err(HlsInitInspectionError::invalid()),
        Some(crate::transcode::VodResponsePublication {
            result: Err(crate::vodserve::VodError::Pending { .. }),
            ..
        }) => return Err(HlsInitInspectionError::pending()),
        Some(crate::transcode::VodResponsePublication {
            result: Err(crate::vodserve::VodError::Busy(_) | crate::vodserve::VodError::Io(_)),
            ..
        }) => return Err(HlsInitInspectionError::unavailable()),
        Some(crate::transcode::VodResponsePublication {
            result: Err(error @ crate::vodserve::VodError::ProducerFailed(_)),
            ..
        })
        | Some(crate::transcode::VodResponsePublication {
            result: Err(error @ crate::vodserve::VodError::Gone(_)),
            ..
        }) => {
            return Err(HlsInitInspectionError::from_api_error(vod_error(
                session, error,
            )));
        }
        None => {
            let opened = match state
                .transcode
                .segment_for_publication_before(session, &init_object, deadline)
                .await
            {
                Ok(crate::transcode::SegmentPublication::Ready(opened)) => *opened,
                Ok(crate::transcode::SegmentPublication::Pending(_)) => {
                    return Err(HlsInitInspectionError::pending());
                }
                Ok(crate::transcode::SegmentPublication::Missing(Some(_))) => {
                    return Err(HlsInitInspectionError::invalid());
                }
                Ok(crate::transcode::SegmentPublication::Missing(None)) => {
                    return Err(HlsInitInspectionError::from_api_error(playlist_error(
                        session,
                        PlaylistError::SessionGone,
                    )));
                }
                Ok(crate::transcode::SegmentPublication::Unavailable(_)) => {
                    return Err(HlsInitInspectionError::unavailable());
                }
                Ok(crate::transcode::SegmentPublication::Failed(error)) => {
                    return Err(HlsInitInspectionError::from_api_error(playlist_error(
                        session,
                        error.error,
                    )));
                }
                Err(crate::transcode::SegmentOpenError::Capacity) => {
                    return Err(HlsInitInspectionError::unavailable());
                }
            };
            // This open is an internal capability check even when the size
            // alone rejects it. Settle the zero-body tracker explicitly so a
            // deliberate oversized refusal is not logged as an abandoned
            // client response.
            let mut delivery = opened.delivery.into_internal_probe();
            if opened.len > INIT_INSPECTION_LIMIT_BYTES {
                delivery.finish_without_body();
                return Err(HlsInitInspectionError::invalid());
            }
            init.reserve(opened.len as usize);
            // No response body exists here — this is the playlist generator
            // reading `hvcC` for itself.
            delivery.expect_at_most(opened.len);
            let started = Instant::now();
            let mut reader = opened.file.take(opened.len);
            match reader.read_to_end(&mut init).await {
                Ok(bytes) => {
                    delivery.note_read(bytes as u64, started.elapsed());
                    delivery.finish();
                    if bytes as u64 != opened.len {
                        return Err(HlsInitInspectionError::invalid());
                    }
                }
                Err(error) => {
                    delivery.fail(&error);
                    return Err(HlsInitInspectionError::unavailable());
                }
            }
        }
    }
    let mut reader = plurx_core::fmp4::FragmentReader::new();
    reader.push(&init);
    let parsed = match reader.next_unit() {
        Ok(Some(plurx_core::fmp4::Unit::Init(init))) => init,
        Ok(Some(_)) | Ok(None) | Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
            return Err(HlsInitInspectionError::invalid());
        }
        Err(
            plurx_core::fmp4::Fmp4Error::Unsupported(_)
            | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
        ) => return Err(HlsInitInspectionError::unsupported()),
    };
    let required_box = if matches!(sample_entry, "dvh1" | "dvhe") {
        init.windows(4)
            .any(|window| window == b"dvcC" || window == b"dvvC")
    } else {
        init.windows(4).any(|window| window == b"hvcC")
    };
    if !required_box {
        return Err(HlsInitInspectionError::invalid());
    }
    if matches!(sample_entry, "hvc1" | "dvh1") {
        match plurx_core::fmp4::hevc_parameter_sets_complete(&parsed) {
            Ok(true) => {}
            Ok(false) | Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
                return Err(HlsInitInspectionError::invalid());
            }
            Err(
                plurx_core::fmp4::Fmp4Error::Unsupported(_)
                | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
            ) => return Err(HlsInitInspectionError::unsupported()),
        }
    }
    match plurx_core::fmp4::validate_hevc_sample_entries(&parsed) {
        Ok(plurx_core::fmp4::HevcSampleEntryLayout::Single) => {}
        Ok(plurx_core::fmp4::HevcSampleEntryLayout::NotHevc)
        | Ok(plurx_core::fmp4::HevcSampleEntryLayout::Multiple { .. }) => {
            return Err(HlsInitInspectionError::unsupported());
        }
        Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
            return Err(HlsInitInspectionError::invalid());
        }
        Err(
            plurx_core::fmp4::Fmp4Error::Unsupported(_)
            | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
        ) => return Err(HlsInitInspectionError::unsupported()),
    }
    let derived = if matches!(sample_entry, "dvh1" | "dvhe") {
        dolby_vision_codec_from_init(&init, sample_entry)
    } else {
        hevc_codec_from_init(&init, sample_entry)
    };
    let video = derived.ok_or_else(HlsInitInspectionError::unsupported)?;
    context.codecs = match context.codecs.split_once(',') {
        Some((_, audio)) if !audio.trim().is_empty() => format!("{video},{}", audio.trim()),
        _ => video,
    };
    Ok(context)
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

/// Does this codec advertisement name Dolby Vision?
///
/// `dvh1`/`dvhe` are the Dolby Vision sample-entry spellings; a preserved
/// Profile 5 session carries one in `codecs` with no `SUPPLEMENTAL-CODECS`
/// beside it, which is why "no supplemental" alone is not the question.
fn advertises_dolby_vision(context: &crate::transcode::HlsContext) -> bool {
    let names_dv = |value: &str| value.contains("dvh1") || value.contains("dvhe");
    names_dv(&context.codecs) || context.supplemental_codecs.as_deref().is_some_and(names_dv)
}

/// Refuse to serve an initialization segment that declares Dolby Vision the
/// playlist does not advertise.
///
/// The invariant is the playlist's own claim: if neither `CODECS` nor
/// `SUPPLEMENTAL-CODECS` names Dolby Vision, an init carrying a `dvcC`/`dvvC`
/// record is describing a stream this session does not serve.
///
/// That pair is exactly what the legacy muxer path produces. A copy that
/// strips Dolby Vision removes the RPU and enhancement-layer NAL units with
/// `filter_units`, which works on NAL types and cannot see the DOVI side data
/// ffmpeg copied out of the source container — so on an ffmpeg without
/// `dovi_rpu` the muxer writes a Profile 7 record with `el_present_flag = 1`
/// over a stream carrying neither layer. The segmenting path removes it in
/// `copyseg`, and the VOD path through the promotion funnel; the legacy path
/// has ffmpeg's own HLS muxer write `init.mp4` straight to disk with no reader
/// in between, so this is where it is caught. Chrome ignores the box;
/// VideoToolbox honours it, and Safari answers 4K10 HEVC so labelled with a
/// software decode on hardware that has a dedicated block for it
/// (`docs/streaming/STUTTER-4K.md` §6).
///
/// Gated on what the playlist says rather than on how the session was built,
/// deliberately. The serve path has no copy options in hand, and the question
/// it can answer is better anyway: the playlist and the init must describe the
/// same stream, whatever produced them. A preserved or converted session
/// advertises Dolby Vision and keeps its record untouched.
///
/// The brand goes with it — `remove_dolby_vision_record` rewrites `dby1` in
/// the same call, because `dby1` over a sample entry with no record is a
/// contradictory init AVPlayer refuses outright rather than merely
/// software-decoding.
fn strip_unadvertised_dolby_vision(
    context: &crate::transcode::HlsContext,
    init: &mut Vec<u8>,
) -> bool {
    use plurx_core::fmp4::{FragmentReader, Unit};

    if advertises_dolby_vision(context) {
        return false;
    }
    let mut reader = FragmentReader::new();
    reader.push(init);
    let Ok(Some(Unit::Init(mut parsed))) = reader.next_unit() else {
        return false;
    };
    // Only when the parse round-trips: this rewrites a file a client is about
    // to play, and an init this reader models incompletely must be served as
    // it is rather than as this function's idea of it.
    if parsed.bytes != *init {
        return false;
    }
    match plurx_core::fmp4::remove_dolby_vision_record(&mut parsed) {
        Ok(true) => {
            *init = parsed.bytes;
            true
        }
        _ => false,
    }
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
    // Unconditional. None of these variants carries a caption track, and
    // Apple's authoring rules say a variant with no captions must say so.
    // Absent the attribute both AVFoundation and ExoPlayer are entitled to
    // synthesise a phantom CEA-608 option into the text group — which shifts
    // every option ordinal underneath it, so a client selecting "the first
    // subtitle rendition" by position gets whatever is now second. That is a
    // candidate cause of subtitles silently not enabling on both platforms,
    // and it is correct authoring either way, so it stops being a rung: an
    // experiment nobody can turn on is not evidence, and this one spent
    // weeks off.
    out.push_str(",CLOSED-CAPTIONS=NONE");
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
    let request_deadline = segment_request_deadline();
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Segment {
            segment: seg.clone(),
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    segment_local_before(
        &state,
        &session,
        &seg,
        &RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await
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

/// RFC 9110 If-Range is deliberately stricter than If-None-Match: only an
/// exact strong entity-tag authorizes a partial representation. Weak tags,
/// dates, malformed values and non-matches all ignore Range and return the
/// current complete representation as 200.
fn range_for_current_etag<'a>(headers: &'a RelayHeaders, etag: &str) -> Option<&'a str> {
    let range = headers.range.as_deref()?;
    match headers.if_range.as_deref() {
        None => Some(range),
        Some(candidate)
            if !etag.starts_with("W/")
                && !candidate.trim().starts_with("W/")
                && candidate.trim() == etag =>
        {
            Some(range)
        }
        Some(_) => None,
    }
}

/// Whether the resolved HTTP range carries every byte of the immutable
/// object. Open-ended and suffix ranges can cover the full object just as a
/// range-less 200 does; frontier semantics follow bytes, not status codes.
fn range_covers_object(range: Option<(u64, u64)>, len: u64) -> bool {
    match range {
        None => true,
        Some((0, end)) => len > 0 && end == len - 1,
        Some(_) => false,
    }
}

fn etag_matches(request: Option<&str>, etag: &str) -> bool {
    let representation = etag.strip_prefix("W/").unwrap_or(etag);
    request.is_some_and(|request| {
        request.split(',').map(str::trim).any(|candidate| {
            candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == representation
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VodResurrection {
    Absent,
    Ended,
    Resurrected,
    Unavailable,
    /// The durable route's owner is gone and no successor took it over
    /// (plan §10.3). A VOD handle is immutable and film-addressed, so a
    /// reopen on any node serves the same film from the same place — but it
    /// is a reopen, and the client is told so rather than left retrying.
    OwnerLost(crate::media_sessions::OwnerLossResume),
}

fn vod_resurrection_unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "vod_resurrection_unavailable",
        "the durable stream attachment could not be restored within this request; retry shortly",
    )
}

fn media_session_ended() -> ApiError {
    ApiError::typed(
        StatusCode::GONE,
        "media_session_ended",
        "this media session is no longer active",
    )
}

fn media_owner_transition() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "media_owner_transition",
        "the media owner is changing; retry shortly",
    )
}

/// The answer for a route classified as an owner transition (plan §10.3).
///
/// Both answers carry where a reopen should land, because the thing a viewer
/// loses when an owner dies is not only the stream but the knowledge of where
/// they were. Which of the two it is turns on the route's own recipe rather
/// than on a clock — see `classify_owner_loss` for why a deadline cannot be
/// made correct here.
fn owner_transition_answer(route: &MediaSessionRoute) -> ApiError {
    match crate::media_sessions::classify_owner_loss(route, unix_ms()) {
        crate::media_sessions::OwnerLoss::Transitioning(resume) => {
            media_owner_transition_with(resume)
        }
        crate::media_sessions::OwnerLoss::Unrecoverable(resume) => media_owner_lost(resume),
    }
}

/// A successor may still arrive, so this stays the retryable 503 it has always
/// been — but it now says where to reopen if the client stops waiting, and
/// that recovery will not be seamless. A recovered session is renumbered
/// across a `#EXT-X-DISCONTINUITY`, so `continuous: false` is true of the
/// retry path too, not only of the terminal one.
fn media_owner_transition_with(resume: crate::media_sessions::OwnerLossResume) -> ApiError {
    ApiError::typed_detail(
        StatusCode::SERVICE_UNAVAILABLE,
        "media_owner_transition",
        "the media owner is changing; retry shortly, or reopen playback from the position \
         in this response",
        owner_loss_detail(resume, false),
    )
}

/// The route can never be taken over, so no successor is coming and waiting
/// cannot change that.
///
/// 410 rather than another 503 on purpose: a client that understands no code
/// at all still reads "gone" and stops waiting, which is the correct
/// degradation for a session no node will serve again. `continuous: false` and
/// `reopen_required: true` are the machine-readable form of §10.3's "do not
/// guess transparency" — the seam is admitted in the body rather than papered
/// over by a status that invites the client to wait it out.
fn media_owner_lost(resume: crate::media_sessions::OwnerLossResume) -> ApiError {
    ApiError::typed_detail(
        StatusCode::GONE,
        "media_owner_lost",
        "the node serving this media session is gone and this session cannot be taken over; \
         reopen playback from the position in this response",
        owner_loss_detail(resume, true),
    )
}

fn owner_loss_detail(
    resume: crate::media_sessions::OwnerLossResume,
    reopen_required: bool,
) -> serde_json::Value {
    serde_json::json!({
        "film_position_ms": resume.film_position_ms,
        "film_frontier_ms": resume.film_frontier_ms,
        "reopen_required": reopen_required,
        "continuous": false,
    })
}

/// Try to resurrect a reaped VOD session from its durable route (plan §2.5)
/// without minting a second HTTP wait budget. Only an active, unexpired route
/// this node owns qualifies; Store/attachment uncertainty remains retryable
/// and must never be relabelled as authoritative absence.
async fn vod_resurrected_before(
    state: &AppState,
    session: &str,
    deadline: Instant,
) -> VodResurrection {
    if tokio::time::Instant::now().into_std() >= deadline {
        return VodResurrection::Unavailable;
    }
    // Capture the process-local release generation before the durable route
    // read. A DELETE may complete while that read is in flight; retaining the
    // exact token prevents the delayed request from minting a fresh permissive
    // gate and resurrecting a terminal capability.
    let Some(adoption) = state.transcode.session_adoption_token(session) else {
        tracing::warn!(
            session = %crate::transcode::session_log_id(session),
            "public VOD resurrection admission is full"
        );
        return VodResurrection::Unavailable;
    };
    // Lease loss also needs to classify this as VOD before the authoritative
    // route read returns; the marker and token cover the same full window.
    let _vod_preparing = state.transcode.begin_vod_preparation(session);
    let route = match state
        .media_sessions
        .authoritative_route_resolution_before(session, &state.node_id, deadline)
        .await
    {
        Ok(DurableRouteResolution::Absent) => return VodResurrection::Absent,
        Ok(DurableRouteResolution::Terminal(_)) => return VodResurrection::Ended,
        Ok(DurableRouteResolution::OwnerTransition(lost)) => {
            return match crate::media_sessions::classify_owner_loss(&lost, unix_ms()) {
                crate::media_sessions::OwnerLoss::Transitioning(_) => VodResurrection::Unavailable,
                crate::media_sessions::OwnerLoss::Unrecoverable(resume) => {
                    VodResurrection::OwnerLost(resume)
                }
            }
        }
        Ok(DurableRouteResolution::ActiveRemote(_)) => return VodResurrection::Unavailable,
        Ok(DurableRouteResolution::ActiveLocal(route)) => route,
        Err(_) => return VodResurrection::Unavailable,
    };
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.transcode.vod_resurrect_before(
            &route.recipe_json,
            session,
            route.user_id,
            adoption,
            deadline,
            false,
        ),
    )
    .await
    {
        Ok(true) => VodResurrection::Resurrected,
        Ok(false) | Err(_) => VodResurrection::Unavailable,
    }
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
        // Two caps, two answers. The client can act on the difference — one
        // says this player is asking for too much at once and should slow its
        // own requests, the other says the node has no parked-request capacity
        // left and retrying harder makes it worse — and an operator reading
        // these refusals needs them apart to tell one seek storm from a
        // ceiling sized for a smaller deployment. They were one code, so
        // neither could.
        VodError::Busy(crate::waitpool::WaitRefused::SessionBusy) => {
            log(
                "segment_wait_busy",
                "this session's blocked-GET cap reached",
            );
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_wait_busy",
                "too many blocked fetches for this session; retry shortly",
            )
        }
        VodError::Busy(crate::waitpool::WaitRefused::PoolFull) => {
            log("node_wait_capacity", "the node's blocked-GET cap reached");
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "node_wait_capacity",
                "this server is at its limit for waiting fetches; retry shortly",
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

async fn resolved_vod_segment_response(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    answer: crate::transcode::VodResponsePublication<Option<crate::vodserve::SegmentReady>>,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let publication_deadline = response_publication_deadline_before(request_deadline);
    let crate::transcode::VodResponsePublication { result, owner } = answer;
    match result {
        Ok(Some(ready)) => {
            vod_segment_response_before(
                state,
                session,
                seg,
                headers,
                ready,
                owner,
                request_deadline,
            )
            .await
        }
        Ok(None) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            Err(ApiError::NotFound("segment"))
        }
        Err(error) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            Err(vod_error(session, error))
        }
    }
}

/// Serve one VOD segment (or the init) with the immutable-cache headers the
/// plan's §2.1 URIs deserve. Range and conditional requests are honoured; the
/// Apple High-tier init rewrite is applied exactly as on the live path.
#[cfg(test)]
async fn vod_segment_response(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    ready: crate::vodserve::SegmentReady,
    owner: crate::transcode::MediaResponseOwner,
) -> Result<Response, ApiError> {
    vod_segment_response_before(
        state,
        session,
        seg,
        headers,
        ready,
        owner,
        segment_request_deadline(),
    )
    .await
}

async fn vod_segment_response_before(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    ready: crate::vodserve::SegmentReady,
    owner: crate::transcode::MediaResponseOwner,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let publication_deadline = response_publication_deadline_before(request_deadline);
    let mut ready = ready;
    if ready.len == 0 {
        authorize_attempt_status(
            state,
            session,
            &owner,
            segment_publication_kind(seg, None),
            Some(seg),
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("segment"));
    }
    let content_type = segment_content_type(seg);
    let artifact_etag = format!("\"{}\"", ready.etag);
    let buffered_init =
        crate::transcode::is_init_object(seg) && ready.len <= INIT_INSPECTION_LIMIT_BYTES;
    if !buffered_init && etag_matches(headers.if_none_match.as_deref(), &artifact_etag) {
        let response = (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, artifact_etag.clone()),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=3600, immutable".to_owned(),
                ),
            ],
        )
            .into_response();
        return complete_buffered_response_before(
            state,
            session,
            &owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "segment-not-modified",
                Some(seg),
            ),
            true,
            response,
            publication_deadline,
        )
        .await;
    }
    let requested_range =
        match requested_byte_range(range_for_current_etag(headers, &artifact_etag), ready.len) {
            Ok(range) => range,
            Err(()) if !buffered_init => {
                let response = (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [
                        (header::CONTENT_RANGE, format!("bytes */{}", ready.len)),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::ETAG, artifact_etag.clone()),
                    ],
                )
                    .into_response();
                authorize_attempt_status(
                    state,
                    session,
                    &owner,
                    "segment-range-not-satisfiable",
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Ok(response);
            }
            // A transformed initialization response has a distinct validator.
            // Delay its 416 until after the representation is known.
            Err(()) => None,
        };
    // Small objects — the init above all — are answered from memory so the
    // Apple rewrite can run; segments stream.
    if buffered_init {
        let mut init = Vec::with_capacity(ready.len.min(64 * 1024) as usize);
        let read = tokio::time::timeout_at(
            tokio::time::Instant::from_std(publication_deadline),
            ready.file.read_to_end(&mut init),
        )
        .await;
        match read {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                authorize_attempt_status(
                    state,
                    session,
                    &owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(ApiError::Internal(error.to_string()));
            }
            Err(_) => {
                authorize_attempt_status(
                    state,
                    session,
                    &owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(response_publication_timeout());
            }
        }
        if init.len() as u64 != ready.len {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(ApiError::Internal(format!(
                "VOD init ended after {} of {} advertised bytes",
                init.len(),
                ready.len
            )));
        }
        let mut transformed = false;
        if let Some(file) = state.transcode.vod_file_for_owner(&owner) {
            if normalize_high_tier_hevc_init(&file, &mut init) {
                transformed = true;
                tracing::info!(
                    session = %crate::transcode::session_log_id(session),
                    "translated the HEVC High-tier initialization record for Apple HLS"
                );
            }
        }
        let etag = if transformed {
            format!("\"{}-apple-high-tier-v1\"", ready.etag)
        } else {
            artifact_etag
        };
        if etag_matches(headers.if_none_match.as_deref(), &etag) {
            let response = (
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
                .into_response();
            return complete_buffered_response_before(
                state,
                session,
                &owner,
                crate::transcode::MediaResponsePublication::attempt_media(
                    "segment-not-modified",
                    Some(seg),
                ),
                true,
                response,
                publication_deadline,
            )
            .await;
        }
        let requested_range =
            match requested_byte_range(range_for_current_etag(headers, &etag), ready.len) {
                Ok(range) => range,
                Err(()) => {
                    let response = (
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        [
                            (header::CONTENT_RANGE, format!("bytes */{}", ready.len)),
                            (header::ACCEPT_RANGES, "bytes".to_owned()),
                            (header::ETAG, etag),
                        ],
                    )
                        .into_response();
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        "segment-range-not-satisfiable",
                        Some(seg),
                        publication_deadline,
                    )
                    .await?;
                    return Ok(response);
                }
            };
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
        return complete_buffered_response_before(
            state,
            session,
            &owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                segment_publication_kind(seg, requested_range),
                Some(seg),
            ),
            range_covers_object(requested_range, ready.len),
            response,
            publication_deadline,
        )
        .await;
    }
    let etag = artifact_etag;
    let (status, len, content_range) = match requested_range {
        Some((start, end)) => {
            use tokio::io::AsyncSeekExt;
            let seek = tokio::time::timeout_at(
                tokio::time::Instant::from_std(publication_deadline),
                ready.file.seek(std::io::SeekFrom::Start(start)),
            )
            .await;
            match seek {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        segment_publication_kind(seg, requested_range),
                        Some(seg),
                        publication_deadline,
                    )
                    .await?;
                    return Err(ApiError::Internal(error.to_string()));
                }
                Err(_) => {
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        segment_publication_kind(seg, requested_range),
                        Some(seg),
                        publication_deadline,
                    )
                    .await?;
                    return Err(response_publication_timeout());
                }
            }
            (
                StatusCode::PARTIAL_CONTENT,
                end - start + 1,
                Some(format!("bytes {start}-{end}/{}", ready.len)),
            )
        }
        None => (StatusCode::OK, ready.len, None),
    };
    let complete_object = range_covers_object(requested_range, ready.len);
    let completion_permit = match reserve_response_completion(publication_deadline).await {
        Ok(permit) => permit,
        Err(error) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, requested_range),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(error);
        }
    };
    let authorization = authorize_response_publication(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            segment_publication_kind(seg, requested_range),
            Some(seg),
        ),
        publication_deadline,
    )
    .await?;
    // Taken before `ready.file` is moved into the reader: the pump outlives
    // this function, and the session registry lock is long released by the
    // time it runs.
    let delivery = std::sync::Arc::clone(&ready.delivery);
    let reader = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(ready.file, len));
    let body_deadline = tokio::time::Instant::now() + MAX_ADMITTED_MEDIA_BODY_LIFETIME;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<DrivenLocalChunk>(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY);
    let terminal = StreamedBodyTerminal::new();
    let pump_terminal = terminal.clone();
    let pump_session = session.to_owned();
    let completion: StreamedResponseCompletion = (
        Arc::clone(&state.transcode),
        pump_session.clone(),
        authorization,
        complete_object,
        completion_permit,
    );
    tokio::spawn(async move {
        let mut reader = reader;
        let mut completion = Some(completion);
        let mut delivered = 0_u64;
        let fail = |kind, message: String| pump_terminal.fail(kind, message);
        loop {
            let progress_deadline =
                (tokio::time::Instant::now() + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
            let next = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime".to_owned(),
                    );
                    tracing::warn!(
                        session = %crate::transcode::session_log_id(&pump_session),
                        delivered_bytes = delivered,
                        expected_bytes = len,
                        "VOD response exceeded its maximum admitted body lifetime"
                    );
                    return;
                }
                () = sender.closed() => return,
                _ = tokio::time::sleep_until(progress_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response made no progress before its body deadline".to_owned(),
                    );
                    tracing::warn!(
                        session = %crate::transcode::session_log_id(&pump_session),
                        delivered_bytes = delivered,
                        expected_bytes = len,
                        "VOD response made no storage progress before its body deadline"
                    );
                    return;
                }
                next = reader.next() => next,
            };
            let bytes = match next {
                Some(Ok(bytes)) => bytes,
                Some(Err(error)) => {
                    fail(error.kind(), error.to_string());
                    return;
                }
                None if delivered == len => return,
                None => {
                    fail(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "VOD response reached EOF after {delivered} of {len} advertised bytes"
                        ),
                    );
                    tracing::warn!(
                        session = %crate::transcode::session_log_id(&pump_session),
                        delivered_bytes = delivered,
                        expected_bytes = len,
                        "VOD response reached EOF before its advertised length"
                    );
                    return;
                }
            };
            let bytes_len = bytes.len() as u64;
            let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
            let send = sender.send(DrivenLocalChunk {
                bytes,
                accepted: accepted_tx,
            });
            tokio::pin!(send);
            let sent = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime".to_owned(),
                    );
                    false
                }
                result = &mut send => result.is_ok(),
            };
            if !sent {
                return;
            }
            let downstream_deadline =
                (tokio::time::Instant::now() + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
            let accepted = tokio::select! {
                biased;
                Ok(()) = accepted_rx => true,
                _ = tokio::time::sleep_until(body_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime".to_owned(),
                    );
                    false
                }
                _ = tokio::time::sleep_until(downstream_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response made no downstream progress before its body deadline".to_owned(),
                    );
                    false
                }
                () = sender.closed() => false,
            };
            if !accepted {
                return;
            }
            // Counted here — where the bytes actually left. `accepted` is the
            // downstream acknowledgement, so nothing is credited to this
            // viewer's rate until the chunk has been taken. A meter advanced
            // at read time instead would measure the disk.
            //
            // The buffered init object is deliberately *not* counted. It is
            // handed to the response whole, so crediting it would date bytes
            // at handoff rather than at delivery and inflate the first window
            // of a session that has delivered nothing yet. One small object
            // missing from the total is the honest trade; an unmeasured
            // session correctly reports no rate at all rather than a fast
            // one, even though that advisory value no longer gates handoff.
            delivery.note(bytes_len);
            delivered = delivered.saturating_add(bytes_len);
            if delivered == len {
                if let Some((manager, session, authorization, complete_object, permit)) =
                    completion.take()
                {
                    settle_streamed_response_completion(
                        manager,
                        session,
                        authorization,
                        complete_object,
                        permit,
                    );
                }
                return;
            }
        }
    });
    let body = driven_local_body(receiver, terminal, body_deadline);
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

async fn segment_local_before(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    const APPLE_INIT_REWRITE_LIMIT_BYTES: u64 = INIT_INSPECTION_LIMIT_BYTES;

    // The VOD presentation's three-outcome contract dispatches first; `None`
    // falls through to the live path untouched. A session neither registry
    // knows may be a reaped VOD handle whose durable route is still live —
    // resurrect it and ask once more before giving up.
    if let Some(answer) = vod_segment_before(state, session, seg, request_deadline).await? {
        return resolved_vod_segment_response(
            state,
            session,
            seg,
            headers,
            answer,
            request_deadline,
        )
        .await;
    }

    let rolling = tokio::time::timeout_at(
        tokio::time::Instant::from_std(request_deadline),
        state.transcode.segment_for_publication(session, seg),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
    let mut opened = match rolling {
        Ok(crate::transcode::SegmentPublication::Ready(opened)) => *opened,
        Ok(crate::transcode::SegmentPublication::Missing(owner)) => {
            if let Some(owner) = owner.as_ref() {
                authorize_attempt_status(
                    state,
                    session,
                    owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    response_publication_deadline_before(request_deadline),
                )
                .await?;
            } else if crate::transcode::is_safe_segment(seg) {
                match vod_resurrected_before(state, session, request_deadline).await {
                    VodResurrection::Absent => {}
                    VodResurrection::Ended => return Err(media_session_ended()),
                    VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                    VodResurrection::Unavailable => return Err(vod_resurrection_unavailable()),
                    VodResurrection::Resurrected => {
                        let answer = vod_segment_before(state, session, seg, request_deadline)
                            .await?
                            .ok_or_else(vod_resurrection_unavailable)?;
                        return resolved_vod_segment_response(
                            state,
                            session,
                            seg,
                            headers,
                            answer,
                            request_deadline,
                        )
                        .await;
                    }
                }
            }
            return Err(ApiError::NotFound("segment"));
        }
        Ok(crate::transcode::SegmentPublication::Pending(owner)) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                response_publication_deadline_before(request_deadline),
            )
            .await?;
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_pending",
                "the segment is still being produced; retry shortly",
            ));
        }
        Ok(crate::transcode::SegmentPublication::Unavailable(owner)) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                response_publication_deadline_before(request_deadline),
            )
            .await?;
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_inspection_unavailable",
                "the segment exists but its storage metadata is temporarily unavailable",
            ));
        }
        Ok(crate::transcode::SegmentPublication::Failed(error)) => {
            return match admitted_playlist_error(
                state,
                session,
                error,
                response_publication_deadline_before(request_deadline),
            )
            .await
            {
                Ok(error) => Err(error),
                Err(()) => Err(response_publication_rejection(
                    crate::transcode::MediaResponsePublicationRejection::StateChanged,
                )),
            };
        }
        Err(crate::transcode::SegmentOpenError::Capacity) => {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "response_snapshot_capacity",
                "authenticated media response capacity is full; retry shortly",
            ));
        }
    };
    let response_owner = opened.response_owner();
    // A live media object is published only after bytes exist. Treat an empty
    // file as an incomplete/corrupt publication instead of advertising the
    // saturating `0..=0` calculation below as one byte and hanging the client.
    if opened.len == 0 {
        authorize_attempt_status(
            state,
            session,
            &response_owner,
            segment_publication_kind(seg, None),
            Some(seg),
            response_publication_deadline_before(request_deadline),
        )
        .await?;
        opened.delivery.finish_without_body();
        return Err(ApiError::NotFound("segment"));
    }
    let content_type = segment_content_type(seg);
    let etag = response_owner
        .rolling_etag(session, seg, opened.len)
        .expect("a live segment carries a rolling response owner");
    if etag_matches(headers.if_none_match.as_deref(), &etag) {
        let response = (
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
            .into_response();
        let publication_deadline = response_publication_deadline_before(request_deadline);
        let authorization = match authorize_response_publication(
            state,
            session,
            &response_owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "segment-not-modified",
                Some(seg),
            ),
            publication_deadline,
        )
        .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                opened.delivery.finish_without_body();
                return Err(error);
            }
        };
        if let Err(error) =
            commit_authorized_media(state, session, authorization, true, publication_deadline).await
        {
            opened.delivery.finish_without_body();
            return Err(error);
        }
        opened.delivery.finish_without_body();
        return Ok(response);
    }
    let requested_range =
        match requested_byte_range(range_for_current_etag(headers, &etag), opened.len) {
            Ok(range) => range,
            Err(()) => {
                let response = (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [
                        (header::CONTENT_RANGE, format!("bytes */{}", opened.len)),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::ETAG, etag),
                    ],
                )
                    .into_response();
                if let Err(error) = authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    "segment-range-not-satisfiable",
                    Some(seg),
                    response_publication_deadline_before(request_deadline),
                )
                .await
                {
                    opened.delivery.finish_without_body();
                    return Err(error);
                }
                opened.delivery.finish_without_body();
                return Ok(response);
            }
        };
    if crate::transcode::is_init_object(seg) && opened.len <= APPLE_INIT_REWRITE_LIMIT_BYTES {
        let publication_deadline = response_publication_deadline_before(request_deadline);
        let mut init = Vec::with_capacity(opened.len.min(64 * 1024) as usize);
        let mut delivery = opened.delivery;
        let started = Instant::now();
        let read_elapsed = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(publication_deadline),
            opened
                .file
                .take(APPLE_INIT_REWRITE_LIMIT_BYTES)
                .read_to_end(&mut init),
        )
        .await
        {
            Ok(Ok(_)) => started.elapsed(),
            Ok(Err(error)) => {
                delivery.fail(&error);
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(ApiError::Internal(error.to_string()));
            }
            Err(_) => {
                delivery.finish_without_body();
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(response_publication_timeout());
            }
        };
        if init.len() as u64 != opened.len {
            let error = std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "init ended after {} of {} advertised bytes",
                    init.len(),
                    opened.len
                ),
            );
            delivery.fail(&error);
            authorize_attempt_status(
                state,
                session,
                &response_owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(ApiError::Internal(error.to_string()));
        }
        if let Ok((context, file, _)) = session_file(state, session, publication_deadline).await {
            if strip_unadvertised_dolby_vision(&context, &mut init) {
                tracing::info!(
                    session = %crate::transcode::session_log_id(session),
                    "removed a Dolby Vision record the playlist does not advertise"
                );
            }
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
        let response_bytes = body.len() as u64;
        let response = response
            .body(Body::from(body))
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        let authorization = match authorize_response_publication(
            state,
            session,
            &response_owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                segment_publication_kind(seg, requested_range),
                Some(seg),
            ),
            publication_deadline,
        )
        .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                delivery.finish_without_body();
                return Err(error);
            }
        };
        if let Err(error) = commit_authorized_media(
            state,
            session,
            authorization,
            range_covers_object(requested_range, opened.len),
            publication_deadline,
        )
        .await
        {
            delivery.finish_without_body();
            return Err(error);
        }
        // The storage inspection reads the complete init so it can normalize
        // codec metadata, but client-delivery accounting follows only the
        // bytes placed in this response (especially for a Range request).
        delivery.expect_at_most(response_bytes);
        delivery.note_read(response_bytes, read_elapsed);
        delivery.finish();
        return Ok(bound_admitted_media_body(response));
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
    let publication_deadline = response_publication_deadline_before(request_deadline);
    if start > 0 {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(publication_deadline),
            opened.file.seek(std::io::SeekFrom::Start(start)),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                opened.delivery.fail(&error);
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, requested_range),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(ApiError::Internal(error.to_string()));
            }
            Err(_) => {
                opened.delivery.finish_without_body();
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, requested_range),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(response_publication_timeout());
            }
        }
    }
    let opened_len = end.saturating_sub(start).saturating_add(1);
    let total_len = opened.len;
    let complete_object = range_covers_object(requested_range, opened.len);
    let completion_permit = match reserve_response_completion(publication_deadline).await {
        Ok(permit) => permit,
        Err(error) => {
            opened.delivery.finish_without_body();
            authorize_attempt_status(
                state,
                session,
                &response_owner,
                segment_publication_kind(seg, requested_range),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(error);
        }
    };
    let authorization = match authorize_response_publication(
        state,
        session,
        &response_owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            segment_publication_kind(seg, requested_range),
            Some(seg),
        ),
        publication_deadline,
    )
    .await
    {
        Ok(authorization) => authorization,
        Err(error) => {
            opened.delivery.finish_without_body();
            return Err(error);
        }
    };
    // The object is open and its length is known. Pin it for the body's
    // lifetime: cleanup may unlink the name while these bytes are still going
    // out, and the directory scan that measures scratch cannot see an
    // unlinked-but-open file.
    let mut authorization = authorization;
    authorization.pin_scratch_object(total_len);
    let reader = tokio_util::io::ReaderStream::new(opened.file.take(opened_len));
    let mut delivery = opened.delivery;
    delivery.expect_at_most(opened_len);
    let body_deadline = tokio::time::Instant::now() + MAX_ADMITTED_MEDIA_BODY_LIFETIME;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<DrivenLocalChunk>(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY);
    let terminal = StreamedBodyTerminal::new();
    let pump_terminal = terminal.clone();
    let pump_session = session.to_owned();
    let completion: StreamedResponseCompletion = (
        Arc::clone(&state.transcode),
        pump_session.clone(),
        authorization,
        complete_object,
        completion_permit,
    );
    // This producer is the sole owner of the file, delivery tracker, and EOF
    // authorization after headers are exposed. Both deadlines keep advancing
    // even if downstream stops polling; receiver Drop ends it immediately.
    tokio::spawn(async move {
        let mut reader = reader;
        let mut delivery = delivery;
        let mut completion = Some(completion);
        let mut delivered = 0_u64;
        let fail = |kind, message: String| pump_terminal.fail(kind, message);
        loop {
            let started = Instant::now();
            let progress_deadline =
                (tokio::time::Instant::now() + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
            let next = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    );
                    delivery.fail_transport(&error, "body_lifetime_exceeded");
                    fail(error.kind(), error.to_string());
                    return;
                }
                () = sender.closed() => return,
                _ = tokio::time::sleep_until(progress_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response made no storage progress before its body deadline",
                    );
                    delivery.fail(&error);
                    fail(error.kind(), error.to_string());
                    return;
                }
                next = reader.next() => next,
            };
            let (bytes, read_elapsed) = match next {
                Some(Ok(bytes)) => (bytes, started.elapsed()),
                Some(Err(error)) => {
                    delivery.fail(&error);
                    fail(error.kind(), error.to_string());
                    return;
                }
                None if delivered == opened_len => return,
                None => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "media response reached EOF after {delivered} of {opened_len} advertised bytes"
                        ),
                    );
                    delivery.fail(&error);
                    fail(error.kind(), error.to_string());
                    return;
                }
            };
            let bytes_len = bytes.len() as u64;
            let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
            let send = sender.send(DrivenLocalChunk {
                bytes,
                accepted: accepted_tx,
            });
            tokio::pin!(send);
            let sent = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    );
                    delivery.fail_transport(&error, "body_lifetime_exceeded");
                    fail(error.kind(), error.to_string());
                    false
                }
                result = &mut send => result.is_ok(),
            };
            if !sent {
                return;
            }
            let downstream_deadline =
                (tokio::time::Instant::now() + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
            let accepted = tokio::select! {
                biased;
                Ok(()) = accepted_rx => true,
                _ = tokio::time::sleep_until(body_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    );
                    delivery.fail_transport(&error, "body_lifetime_exceeded");
                    fail(error.kind(), error.to_string());
                    false
                }
                _ = tokio::time::sleep_until(downstream_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response made no downstream progress before its body deadline",
                    );
                    delivery.fail_transport(&error, "downstream_no_progress");
                    fail(error.kind(), error.to_string());
                    false
                }
                () = sender.closed() => false,
            };
            if !accepted {
                return;
            }
            delivery.note_read(bytes_len, read_elapsed);
            delivered = delivered.saturating_add(bytes_len);
            if delivered == opened_len {
                if delivery.finish() {
                    if let Some((manager, session, authorization, complete_object, permit)) =
                        completion.take()
                    {
                        settle_streamed_response_completion(
                            manager,
                            session,
                            authorization,
                            complete_object,
                            permit,
                        );
                    }
                }
                return;
            }
        }
    });
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
        .body(driven_local_body(receiver, terminal, body_deadline))
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

// split: begin hls-tests
#[cfg(test)]
#[path = "hls/tests.rs"]
mod tests;
// split: end hls-tests
