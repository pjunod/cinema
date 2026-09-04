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
    RelayResource, ReleaseAdmission, ReleaseSettlement, RemoteAbortRequest, RemoteStartRequest,
    RemoteStartResponse, ACTIVATION_STORE_DEADLINE, LEASE_TTL_MS, MAX_ADMITTED_MEDIA_BODY_LIFETIME,
    MEDIA_BODY_NO_PROGRESS_TIMEOUT, OWNER_ASSIGNMENT_DEADLINE,
    REMOTE_ACTIVATION_CONFIRMATION_WINDOW, START_DEADLINE, TERMINAL_PROJECTION_SAFETY_WINDOW,
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
#[derive(Debug, Clone, Default, Deserialize)]
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
    use plurx_core::playback::{decide_forced, DeviceProfile, Force};

    plan_derivation::count_rederived();
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
    if overridden {
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
    if review.mismatched {
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
/// [M6-CALLER-HANDOFF.md](../../../../docs/M6-CALLER-HANDOFF.md) §3.3 on why
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
/// [M6-CALLER-HANDOFF.md](../../../../docs/M6-CALLER-HANDOFF.md) §3.2. **Do
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
    if hdr_subtitle_burn_is_refused(source.as_ref(), req.subtitle_burn, req.subtitle_burn_sdr) {
        return Err(ApiError::Unprocessable(serde_json::json!({
            "code": "hdr_subtitle_burn_refused",
            "error": HDR_SUBTITLE_BURN_REFUSAL,
        })));
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
    let height = resolved.height;
    let fingerprint = resolved.intent_fingerprint;
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

    let advertise_control = state
        .store
        .get_setting(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1)
        .await
        .map_err(|error| session_store_error("reading the control protocol gate", error))?
        .as_deref()
        == Some("1");

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
            let worker_serving_authority = ingress_serving_authority.clone();
            let mut start_task = tokio::spawn(async move {
                let started = transcode
                    .create_cluster_session(
                        &worker_request,
                        guard_user,
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
    let response_json = serde_json::to_string(&response)?;
    let activation_now_ms = unix_ms();
    let activation = MediaSessionActivation {
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
    crate::playstart::note_playback_started(
        &state,
        user.id,
        &user.username,
        id,
        method,
        Some(&request.playback_id),
    );
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

async fn settle_activation_predecessor(
    state: &AppState,
    predecessor_incarnation: Option<String>,
    successor: MediaSessionRoute,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    mut guard: Option<StartedSessionGuard>,
) -> Result<(), ApiError> {
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
    let mut route = match state
        .media_sessions
        .route_resolution_before(session_id, &state.node_id, request_deadline)
        .await?
    {
        DurableRouteResolution::Absent | DurableRouteResolution::ActiveLocal(_) => return Ok(None),
        DurableRouteResolution::OwnerTransition(_) | DurableRouteResolution::Terminal(_) => {
            match state
                .media_sessions
                .authoritative_route_resolution_before(session_id, &state.node_id, request_deadline)
                .await?
            {
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
        let resolution = state
            .media_sessions
            .authoritative_route_resolution_before(session_id, &state.node_id, request_deadline)
            .await?;
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
        let response = local_control_response(
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
        crate::playback_control::TerminalCommitReceipt::retryable_until(
            acknowledgement.expires_at_ms,
            move |attempt| {
                let store = Arc::clone(&store);
                let acknowledgement = acknowledgement.clone();
                let response = response.clone();
                let handoff = handoff.clone();
                let faults = faults.clone();
                if let Some(handoff) = &handoff {
                    handoff.restart();
                }
                tokio::spawn(async move {
                    let persisted = match faults {
                        Some(faults) => {
                            persist_terminal_ack_with_faults(
                                store,
                                acknowledgement,
                                Some(faults.as_ref()),
                            )
                            .await
                        }
                        None => persist_terminal_ack(store, acknowledgement).await,
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
) -> Result<Option<crate::playback_control::PreparedSuccessorAction>, ()> {
    #[cfg(test)]
    if staged_read_faults()
        .lock()
        .expect("staged read faults")
        .remove(&predecessor.incarnation_id)
    {
        return Err(());
    }
    let Some(staged) = state
        .store
        .staged_media_session_for_playback(predecessor.user_id, &predecessor.playback_id)
        .await
        .map_err(|error| {
            tracing::warn!(?error, "staged playback generation read failed");
        })?
    else {
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
    control_local_inner(state, route, request, deadline_unix_ms).await
}

async fn control_local_inner(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
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
    // Staging is detached from the exchange that requested it, so only a
    // later exchange can see the durable result. End never announces new
    // work: its owner-local transaction aborts any slot the session held.
    let prepared_successor = if request.demand == crate::playback_control::PlaybackDemand::End {
        crate::playback_control::PreparedSuccessorObservation::Inactive
    } else if !request.accepts(crate::playback_control::PREPARE_REPLACEMENT_ACTION) {
        crate::playback_control::PreparedSuccessorObservation::NotRequested
    } else {
        match staged_successor_action(state, route).await {
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
    if let Some(directive) = result.preparation_directive.clone() {
        let Some(gate) = state
            .transcode
            .session_preparation_gate(&route.session_id)
            .await
        else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the preparation owner disappeared before acknowledgement settlement",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        let authority = state.serving.authority();
        let Some(serving_generation) = authority.admit() else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the node is not accepting a preparation acknowledgement",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        let Some(remaining) = crate::playback_control::inherited_exchange_budget(
            deadline_unix_ms,
            crate::media_sessions::unix_ms(),
        ) else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the preparation acknowledgement exhausted its exchange deadline",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        let deadline = Instant::now()
            .checked_add(remaining)
            .unwrap_or_else(Instant::now);
        let Some(_serving_commit) = authority
            .commit_guard_before(serving_generation, deadline)
            .await
        else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "serving authority changed before preparation acknowledgement settlement",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        let executor = crate::playback_control::PreparationExecutor::new(
            Arc::clone(&state.store),
            gate,
            route.user_id,
            route.playback_id.clone(),
        );
        let settled = match directive {
            crate::playback_control::PreparationDirective::Commit {
                staged_incarnation_id,
            } => tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                executor.commit(
                    &staged_incarnation_id,
                    crate::media_sessions::unix_ms(),
                    result.lease_expires_at_unix_ms,
                ),
            )
            .await
            .map(|outcome| outcome.map(|_| ())),
            crate::playback_control::PreparationDirective::Abort {
                staged_incarnation_id,
            } => {
                tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    executor.abort(&staged_incarnation_id, crate::media_sessions::unix_ms()),
                )
                .await
            }
        };
        if !matches!(settled, Ok(Ok(()))) {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
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
    let response = if result.lease_state == "ended" {
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
    if result.selection.changed {
        crate::playback_control::record_preparation_observation(
            crate::playback_control::PreparationSeam::InSession,
        );
        // Spawned, never awaited: see the function's own doc. The exchange has
        // spent its deadline by here and the response is already built.
        tokio::spawn(process_preparation_candidate(
            state.clone(),
            PreparationCandidateInputs {
                session_id: route.session_id.clone(),
                route: route.clone(),
                recipe: recipe.clone(),
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
            },
        ));
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
/// This exists because of what m6 measured on 2026-09-02: 949 accepted
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
struct PreparationCandidateInputs {
    /// The live session this exchange belongs to. Staging needs it twice: to
    /// reach the actor that owns the one successor slot, and to name the
    /// predecessor the commit CAS will fence against.
    session_id: String,
    route: MediaSessionRoute,
    recipe: RemoteStartRequest,
    selection: crate::playback_control::ClientSelection,
    observed_download_bps: Option<u64>,
    delivered: crate::playback_control::EffectiveSelection,
    delivered_bps: Option<i64>,
    capabilities: Option<crate::playback_control::DynamicCapabilities>,
    platform: crate::playback_control::ClientPlatform,
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
async fn process_preparation_candidate(state: AppState, exchange: PreparationCandidateInputs) {
    let PreparationCandidateInputs {
        session_id,
        route,
        recipe,
        selection,
        observed_download_bps,
        delivered,
        delivered_bps,
        capabilities,
        platform,
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
    let Ok(source) = state.store.get_file(recipe.request.file_id).await else {
        // Detached failure is fail-safe: no candidate is staged, and the
        // already-completed exchange remains valid.
        return;
    };
    // Only a *manual* ask needs resolving, and this is the whole reason the
    // network prior is not read here.
    //
    // `resolve_height`'s Auto arm is the only one that consults the prior, and
    // Auto is not a client-driven height change: the ladder moving because the
    // link moved is the server's own adaptation, which M6 is explicitly not
    // for. So an Auto selection keeps the height it is already being served,
    // and every case still decides the same way — Auto → Auto crosses nothing,
    // and anything to or from Auto crosses `ResolutionOrBitrate` on
    // `quality_auto` regardless of the number. Resolving it instead would
    // invent a server-driven change and attribute it to the viewer.
    let height = match selection.quality {
        crate::playback_control::QualitySelection::Auto => delivered.height,
        crate::playback_control::QualitySelection::Manual { height } => {
            resolve_height(
                &state,
                source.as_ref(),
                None,
                recipe.request.hdr10,
                Some(height),
            )
            .await
        }
    };
    let candidate = crate::playback_control::candidate_request(
        &recipe.request,
        &selection,
        height,
        source.as_ref().and_then(|file| file.height),
    );
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
    // Apple now advertises the measured capability; web and Android remain
    // false. Keep the counterfactual beside the production decision so the
    // remaining rollout cost can still be read without pretending those
    // clients can safely hold two pipelines.
    //
    // Expect `throughput_unreported` to dominate it at first, and read that as
    // a statement about the *inputs*: the native clients send no
    // `observed_download_bps` and VOD sessions carry no `delivered_bps`. That
    // is a finding about instrumentation, not about links.
    crate::playback_control::record_preparation_counterfactual(
        platform,
        crate::playback_control::decide_preparation_after_client_release(
            delivered_view,
            proposed_view,
            conditions,
        ),
    );
    if let crate::playback_control::PreparationDecision::Prepare { .. } = decision {
        stage_prepared_successor(
            &state,
            &session_id,
            &route,
            &recipe,
            &candidate,
            source.as_ref(),
        )
        .await;
    }
}

/// How long a staged successor may sit unclaimed.
///
/// Keyed off `VOD_LEASE_TIMEOUT_MS`, because every session this fires on is
/// VOD: a client is allowed that long between exchanges before it is even
/// nominally late, so a shorter window would expire successors for clients
/// behaving exactly as the protocol permits. One lease plus a margin covers a
/// slow client without covering a gone one.
///
/// **This bound is enforced here, not by the store.** Maintenance retires an
/// expired row only `TAKEOVER_RECOVERY_MS` after its lease lapses and ticks
/// every five minutes, so waiting for it would hold a successor's admission
/// slot and the playback's one preparation slot for several minutes past the
/// deadline — and would never free the actor's slot at all, because nothing
/// in that path settles it. The timer armed at stage time does both.
const PREPARATION_DEADLINE_MS: i64 = crate::playback_control::VOD_LEASE_TIMEOUT_MS as i64 + 30_000;

/// M6 §3.4 — stage the successor the decision just admitted.
///
/// **Stage only.** This is the second of §5.1's eight phases — propose ·
/// stage · reserve and prime · prepare · commit · commit durability · retire ·
/// abort — and it is: a durable row and the
/// actor's one successor slot, nothing produced yet. The pointer is untouched
/// by construction — `prepare_media_session` is the entry point that neither
/// runs the supersession reap nor advances `media_playback_pointers`, which is
/// why a preparation cannot be built on `activate_media_session`.
///
/// Best-effort, like the measurement it runs beside. A staged successor that
/// fails to appear costs the viewer nothing: the fallback replacement they
/// would have taken before M6 is still exactly what happens. A staged
/// successor that appears when it should not costs a saturated user real
/// admission headroom, which is why every refusal below returns rather than
/// retries.
async fn stage_prepared_successor(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
) {
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
    // Where the successor actually begins: the frontier the predecessor has
    // reached, not the position it was created at. `candidate` is the
    // predecessor's request with the selection fields overwritten, so its
    // `start_seconds` is the *original* start — committing that would restart
    // the film for a viewer forty minutes in.
    let resume_ms = route
        .media_origin_ms
        .saturating_add(route.fetched_through_ms.max(0));
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
    let Ok(recipe_json) = serde_json::to_string(&RemoteStartRequest {
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: staged_incarnation_id.clone(),
        user_id: route.user_id,
        source_size: source.size,
        source_mtime: source.mtime,
        // Read from the predecessor rather than assumed: this decides whether
        // anything may ever take the successor over.
        typeless_playlist: predecessor.typeless_playlist,
        request: staged_request.clone(),
    }) else {
        return;
    };
    // A real `StartResponse`, because every reader of a route's
    // `response_json` parses it as one — and `control_start_response` filters
    // on the bootstrap being present, so a row without it answers 404
    // `session_gone` on the successor's first exchange after commit.
    let Ok(response_json) = serde_json::to_string(&StartResponse {
        session_id: staged_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{staged_session_id}/index.m3u8"),
        duration_ms: source.duration_ms,
        start_seconds: resume_ms as f64 / 1_000.0,
        media_origin_ms: Some(resume_ms),
        height: match staged_request.kind {
            crate::transcode::SessionKind::Transcode { height } => height,
            crate::transcode::SessionKind::Copy { .. } => source.height.unwrap_or_default(),
        },
        encoder: "staged".to_owned(),
        vod: staged_request.presentation == crate::transcode::Presentation::Vod,
        ladder: Vec::new(),
        prior_kbps: None,
        delivered_dynamic_range: None,
        delivered_dolby_vision_profile: None,
        control: crate::playback_control::ControlBootstrap::new(
            &staged_session_id,
            &staged_incarnation_id,
            1,
            crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        ),
        plan_notes: Vec::new(),
    }) else {
        return;
    };
    let executor = crate::playback_control::PreparationExecutor::new(
        Arc::clone(&state.store),
        gate,
        route.user_id,
        route.playback_id.clone(),
    );
    let preparation = plurx_core::domain::MediaSessionPreparation {
        incarnation_id: staged_incarnation_id,
        session_id: staged_session_id,
        user_id: route.user_id,
        playback_id: route.playback_id.clone(),
        // Recorded now rather than read fresh at commit, so a lost CAS aborts
        // this successor and never reaps a newer player generation.
        expected_predecessor_incarnation_id: route.incarnation_id.clone(),
        // The successor's own intent, not the predecessor's: the
        // fingerprint encodes `kind` and `start_seconds`, both of which this
        // row deliberately changes.
        request_fingerprint: staged_request.durable_intent_fingerprint(route.user_id),
        owner_node_id: state.node_id.clone(),
        recipe_json,
        response_json,
        media_origin_ms: resume_ms,
        now_ms,
        deadline_ms: now_ms.saturating_add(PREPARATION_DEADLINE_MS),
    };
    match executor.stage(&preparation).await {
        Ok(true) => {
            crate::playback_control::record_preparation_staged(true);
            arm_preparation_deadline(
                executor,
                preparation.incarnation_id,
                PREPARATION_DEADLINE_MS,
            );
        }
        Ok(false) | Err(_) => crate::playback_control::record_preparation_staged(false),
    }
}

/// Free the slot and the row when nobody claims the successor.
///
/// Without this the first successful stage is the last one for the life of the
/// session: `ControlState::stage_preparation` refuses a non-empty slot, and
/// the slot is cleared only by a commit or an abort — neither of which has a
/// caller until §3.5. Every later selection change would then pay a durable
/// create-and-abort round trip for a successor that can never exist.
///
/// The roadmap states the invariant this keeps: *"after the preparation lease
/// expires the actor aborts the successor and keeps the current generation if
/// healthy."* Aborting is idempotent against a successor that has already
/// committed — the store's abort names an incarnation that is no longer
/// staged and changes nothing — so the timer does not race the commit path
/// §3.5 will add.
fn arm_preparation_deadline(
    executor: crate::playback_control::PreparationExecutor,
    staged_incarnation_id: String,
    deadline_ms: i64,
) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(deadline_ms.max(0) as u64)).await;
        let _ = executor
            .abort(&staged_incarnation_id, crate::media_sessions::unix_ms())
            .await;
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
            let context = tokio::time::timeout_at(
                tokio::time::Instant::from_std(initial_vod_deadline),
                exact_hls_context(state, session, context),
            )
            .await
            .map_err(|_| response_publication_timeout())?;
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
    let context = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        exact_hls_context(state, session, context),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
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
    let context = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        exact_hls_context(state, session, context),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
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
        | PlaylistError::SessionFailed(_) => StatusCode::BAD_GATEWAY,
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
}

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
            if let Ok(Some(bytes)) = window_bytes {
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
                // The whole-track warm keeps its original contract, including
                // that a timeout here fails the request rather than being
                // swallowed: it is the path every other consumer depends on.
                tokio::time::timeout_at(
                    tokio::time::Instant::from_std(publication_deadline),
                    source.warm_whole(&state.subs_dir, &file, index),
                )
                .await
                .map_err(|_| response_publication_timeout())?;
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
        Some(crate::transcode::VodResponsePublication {
            result: Ok(Some(ready)),
            ..
        }) => {
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
/// (`docs/STUTTER-4K.md` §6).
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
        Ok(crate::transcode::SegmentPublication::Ready(opened)) => opened,
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

#[cfg(test)]
mod tests {

    /// The reservation is a control-flow property of one `async fn` that needs
    /// a store, a transcode manager and an authenticated owner to reach, so
    /// nothing executes it. Pin it where it lives instead: reverting the
    /// confirmation to the bare lease deadline, or handing the recovery read
    /// the reserved deadline it must never be given, fails here.
    #[test]
    fn the_confirmation_reserves_recovery_budget_inside_the_owner_lease() {
        let source = include_str!("hls.rs");
        let reservation = source
            .split_once("    let confirmation_deadline = activation_confirmation_deadline(")
            .expect("the confirmation must be awaited on its own reserved deadline")
            .1
            .split_once("    let route = match confirmation {")
            .expect("confirmation settlement boundary")
            .0;
        assert!(
            reservation.contains("tokio::time::timeout_at(")
                && reservation.contains("confirmation_deadline,")
                && reservation.contains("settle_media_session_activation("),
            "the reserved deadline must bound the confirmation write itself"
        );
        assert!(
            !reservation.contains("timeout_at(lease_deadline"),
            "the confirmation must not be awaited on the raw owner lease"
        );
        // The recovery read is the beneficiary; giving it the reserved
        // deadline would hand back the very budget the reservation bought.
        assert!(
            source.contains("wait_for_confirmed_activation(&state, &activation, lease_deadline)"),
            "the committed-write recovery read must keep the full owner lease"
        );
        // A reservation that is taken only when it fits whole is the cut that
        // vanished in the case that needed it.
        let split = source
            .split_once("fn activation_confirmation_deadline(")
            .expect("the lease split must be one named function")
            .1;
        // Measuring anything other than the lease actually left would make
        // the split degrade against a constant, which is not a split at all.
        const REMAINING: &str =
            "lease_deadline.saturating_duration_since(tokio::time::Instant::now())";
        assert!(
            split.contains(REMAINING)
                && split.contains("ACTIVATION_CONFIRMATION_RECOVERY_MARGIN.min(remaining / 2)"),
            "the reservation must degrade with the remaining lease, never be skipped"
        );
    }

    /// M3's acceptance is that a seek storm starts production for exactly one
    /// target. The latch decides which; this decides whether a restart that
    /// arrives for a superseded one is refused. The handler cannot be reached
    /// from a test -- it needs a store, a transcode manager and an
    /// authenticated user -- so the decision lives in one function and this
    /// pins it.
    mod restart_supersession {
        use super::super::restart_is_superseded;
        use crate::playback_control::SettledTarget;

        fn settled(sequence: u64, anchor_ms: i64) -> Option<SettledTarget> {
            Some(SettledTarget {
                sequence,
                anchor_ms,
            })
        }

        #[test]
        fn a_client_that_sends_no_sequence_is_never_refused() {
            // Every client before the field existed, and Apple and Android
            // until they send it. Absent means do the work.
            assert!(!restart_is_superseded(settled(90, 1_800_000), None, 5.0));
        }

        #[test]
        fn a_playback_with_no_settled_target_is_never_refused() {
            // No session, a retired actor, or a client that has not exchanged
            // yet. Nothing to order against, so do the work.
            assert!(!restart_is_superseded(None, Some(3), 5.0));
        }

        #[test]
        fn a_restart_for_a_destination_the_client_left_is_refused() {
            // Sequence 3 asked for 5 s; the client has since settled on 1,800 s
            // at sequence 90. That session is waste before it spawns.
            assert!(restart_is_superseded(settled(90, 1_800_000), Some(3), 5.0));
        }

        #[test]
        fn the_honest_seek_arriving_next_is_never_the_one_refused() {
            // The create can reach the server before its own snapshot does, so
            // only a strictly later exchange supersedes. Equal and later both
            // proceed -- refusing either is the failure this milestone exists
            // to prevent.
            assert!(!restart_is_superseded(
                settled(90, 1_800_000),
                Some(90),
                5.0
            ));
            assert!(!restart_is_superseded(
                settled(90, 1_800_000),
                Some(91),
                5.0
            ));
        }

        #[test]
        fn a_seek_that_lands_beside_its_target_is_not_refused() {
            // 1,799.64 s against a settled 1,800 s is the same destination;
            // seeking is not exact, and refusing this would make every honest
            // seek cancel its own session.
            assert!(!restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                1_799.64
            ));
        }

        #[test]
        fn a_malformed_start_cannot_cancel_a_session_the_viewer_wants() {
            // NaN and a negative both read as the head. That can only make a
            // restart look *less* superseded, which is the safe direction: a
            // malformed body must not be able to refuse work.
            assert!(!restart_is_superseded(settled(90, 0), Some(3), f64::NAN));
            assert!(!restart_is_superseded(settled(90, 0), Some(3), -12.0));
            // ... and it is still refused when the settled target really is
            // somewhere else, so the clamp does not become a bypass.
            assert!(restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                f64::NAN
            ));
        }

        #[test]
        fn an_absurd_start_saturates_instead_of_wrapping_into_a_real_anchor() {
            // The float-to-int cast saturates. Were it to wrap, a huge start
            // could land on a small anchor and compare equal to a destination
            // the viewer actually wants.
            assert!(restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                f64::MAX
            ));
            assert!(restart_is_superseded(
                settled(90, 1_800_000),
                Some(3),
                f64::INFINITY
            ));
        }
    }
    use super::*;

    /// The replacement seam, which is where a viewer's quality change actually
    /// arrives.
    ///
    /// m6 recorded one decision against 949 accepted exchanges because
    /// `ControlState::last_selection` only sees a change *within* a session,
    /// and Apple's `selectQuality` replaces the session instead. Keyed by
    /// playback id rather than by `previous_session_id`, which the Apple
    /// client sets only on a stall reopen — a gate needing that field on a
    /// non-stall reopen matches nothing, which is exactly what the first
    /// attempt at this measured.
    #[test]
    fn a_replacement_is_measured_against_the_session_it_replaced() {
        let selection = |height: i64| crate::playback_control::EffectiveSelection {
            height,
            ..sample_effective_selection()
        };
        let playback = format!("seam-playback-{}", std::process::id());
        let first = format!("{playback}-session-1");
        let second = format!("{playback}-session-2");

        // The playback's first session has nothing to be measured against.
        assert_eq!(
            remember(&playback, &first, &selection(2160), &asked_1080()),
            None,
        );
        // Its later exchanges answer nothing, so a session is never measured
        // against itself.
        assert_eq!(
            remember(&playback, &first, &selection(2160), &asked_1080()),
            None,
        );
        // A new session for the same playback is the viewer's quality change,
        // and it is measured against what the old session was delivering.
        let (previous, previous_grade) =
            remember(&playback, &second, &selection(1080), &asked_720())
                .expect("the replaced session is remembered");
        assert_eq!(previous.height, 2160);
        assert_eq!(previous_grade, seam_grade());
        // Exactly once: the replacement's later exchanges answer nothing, or
        // one change would be counted for the life of the session.
        assert_eq!(
            remember(&playback, &second, &selection(1080), &asked_720()),
            None,
        );
        // And a change back is its own transition.
        let (back, _) = remember(
            &playback,
            &format!("{playback}-session-3"),
            &selection(2160),
            &asked_1080(),
        )
        .expect("the second session is remembered too");
        assert_eq!(back.height, 1080);
        // A playback this node has never served is not a measurement.
        assert_eq!(
            remember(
                &format!("seam-other-{}", std::process::id()),
                "some-session",
                &selection(720),
                &asked_720(),
            ),
            None,
        );
    }

    /// Three gates, and the count fills with things no viewer did if any one
    /// of them is dropped.
    ///
    /// Found by review, all three confirmed against the shipped clients: a
    /// playback id outlives the film (Apple mints one per `PlayerView` and
    /// autoplay-next is `stop()`-then-`start()` on the same controller), and
    /// the client reopens for its own reasons — a Dolby Vision fallback to the
    /// HDR10 base or to a compatibility transcode, a burn-in retry, a failure
    /// retry, an out-of-window seek — none of which declare a `reopen_reason`
    /// and all of which move the delivered recipe on exactly the two axes
    /// whose counts are the evidence about `PREPARED_AXIS`.
    #[test]
    fn a_replacement_the_viewer_did_not_ask_for_is_not_measured() {
        let selection = |height: i64| crate::playback_control::EffectiveSelection {
            height,
            ..sample_effective_selection()
        };

        // An episode boundary: same playback, new session, different film.
        let binge = format!("seam-binge-{}", std::process::id());
        assert_eq!(
            remember(&binge, "episode-1", &selection(2160), &asked_1080()),
            None,
        );
        assert_eq!(
            remember_delivered_selection(
                &binge,
                "episode-2",
                SEAM_FILE + 1,
                &asked_720(),
                &selection(1080),
                seam_grade(),
            ),
            None,
            "a new film is not a viewer changing quality",
        );

        // The client's own recovery: same film, new session, unchanged ask.
        let fallback = format!("seam-fallback-{}", std::process::id());
        assert_eq!(
            remember(&fallback, "before", &selection(2160), &asked_1080()),
            None,
        );
        assert_eq!(
            remember(&fallback, "after", &selection(1080), &asked_1080()),
            None,
            "a Dolby Vision fallback changes what is delivered, not what was asked",
        );

        // And the real thing still measures, so the gates are not simply off.
        let real = format!("seam-real-{}", std::process::id());
        assert_eq!(
            remember(&real, "one", &selection(2160), &asked_1080()),
            None
        );
        assert!(
            remember(&real, "two", &selection(1080), &asked_720()).is_some(),
            "same film, new session, and the viewer asked for something else",
        );
    }

    /// The table is bounded, because a node serves unboundedly many playbacks.
    #[test]
    fn the_remembered_selections_are_bounded() {
        let tag = format!("bound-{}", std::process::id());
        let first = format!("{tag}-0");
        let kept = format!("{tag}-kept");
        remember(&first, "s", &sample_effective_selection(), &asked_1080());
        remember(&kept, "s", &sample_effective_selection(), &asked_1080());
        for index in 1..=MAX_REMEMBERED_SELECTIONS {
            remember(
                &format!("{tag}-{index}"),
                "s",
                &sample_effective_selection(),
                &asked_1080(),
            );
            // Touched throughout, and therefore kept: eviction is by last
            // touch, not by first sight. Insertion order would drop the
            // two-hour film first, and mid-film is exactly when a viewer
            // changes quality.
            remember(&kept, "s", &sample_effective_selection(), &asked_1080());
        }
        let guard = DELIVERED_SELECTIONS.lock().expect("lock");
        let table = guard.as_ref().expect("table");
        assert!(table.by_playback.len() <= MAX_REMEMBERED_SELECTIONS);
        assert_eq!(table.by_playback.len(), table.order.len());
        assert!(
            !table.by_playback.contains_key(&first),
            "the least recently touched entry is evicted",
        );
        assert!(
            table.by_playback.contains_key(&kept),
            "and one touched throughout survives however many arrive after it",
        );
    }

    /// One film, one grade — the seam's other two gates held constant so a
    /// test that means to vary the session varies only the session.
    const SEAM_FILE: i64 = 4_242;

    fn seam_grade() -> crate::playback_control::GradeIntent {
        crate::playback_control::GradeIntent {
            hdr10: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        }
    }

    fn remember(
        playback: &str,
        session: &str,
        delivered: &crate::playback_control::EffectiveSelection,
        asked: &crate::playback_control::ClientSelection,
    ) -> Option<(
        crate::playback_control::EffectiveSelection,
        crate::playback_control::GradeIntent,
    )> {
        remember_delivered_selection(playback, session, SEAM_FILE, asked, delivered, seam_grade())
    }

    fn asked_at(
        quality: crate::playback_control::QualitySelection,
    ) -> crate::playback_control::ClientSelection {
        crate::playback_control::ClientSelection {
            quality,
            audio_track: Some(0),
            subtitle: crate::playback_control::SubtitleSelection {
                mode: crate::playback_control::SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: crate::playback_control::CodecPolicy::Auto,
            dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
        }
    }

    fn asked_1080() -> crate::playback_control::ClientSelection {
        asked_at(crate::playback_control::QualitySelection::Manual { height: 1080 })
    }

    fn asked_720() -> crate::playback_control::ClientSelection {
        asked_at(crate::playback_control::QualitySelection::Manual { height: 720 })
    }

    /// A minimal delivered selection; only `height` matters to these tests.
    fn sample_effective_selection() -> crate::playback_control::EffectiveSelection {
        crate::playback_control::EffectiveSelection {
            height: 1080,
            quality_auto: false,
            codec: "server_selected".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
            audio_track: None,
            audio_offset_ms: 0,
            subtitle_burn: None,
        }
    }

    /// A fixed clock for the plan-review tests. `review_client_plan` reads it
    /// to decide which of the client's learned limits still apply, so a test
    /// on the real clock would rot the moment a fixture aged out.
    const NOW_MS: i64 = 1_756_400_000_000;
    use crate::transcode::HlsDeliveryFixture;
    use http_body_util::BodyExt;
    use std::time::Duration;

    async fn activate_ready(
        store: &Arc<dyn plurx_core::store::Store>,
        mut activation: MediaSessionActivation,
    ) -> MediaSessionRoute {
        activation.publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED;
        store
            .activate_media_session(&activation)
            .await
            .expect("prepare media route")
            .expect("media route preparation accepted");
        let publication_ready_at_ms = activation
            .expected_predecessor_incarnation_id
            .as_ref()
            .map_or(0, |_| {
                activation
                    .now_ms
                    .saturating_add(plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS)
            });
        store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms,
                },
                activation.now_ms,
            )
            .await
            .expect("confirm media route")
            .expect("media route confirmation accepted")
    }

    #[tokio::test]
    async fn driven_local_body_rejects_queued_data_after_terminal_failure() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let (accepted, rejected) = tokio::sync::oneshot::channel();
        sender
            .send(DrivenLocalChunk {
                bytes: Bytes::from_static(b"stale-chunk"),
                accepted,
            })
            .await
            .expect("body receiver");
        let terminal = StreamedBodyTerminal::new();
        terminal.fail(
            std::io::ErrorKind::TimedOut,
            "body deadline expired".to_owned(),
        );
        drop(sender);

        let mut body = driven_local_body(
            receiver,
            terminal,
            tokio::time::Instant::now() + Duration::from_secs(60),
        );
        let error = body
            .frame()
            .await
            .expect("terminal error frame")
            .expect_err("the driven body must expose the producer failure");
        assert!(error.to_string().contains("body deadline expired"));
        assert!(body.frame().await.is_none());
        assert!(
            rejected.await.is_err(),
            "stale bytes must not be acknowledged"
        );
    }

    #[test]
    fn same_incarnation_publication_rejection_is_retryable_not_not_found() {
        let error = response_publication_rejection(
            crate::transcode::MediaResponsePublicationRejection::StateChanged,
        );
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "response_state_changed",
                ..
            }
        ));
    }

    #[test]
    fn beyond_frontier_publication_rejection_is_typed_producer_ended() {
        let error = response_publication_rejection(
            crate::transcode::MediaResponsePublicationRejection::ProducerEnded(
                "process_exit".to_owned(),
            ),
        );
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::BAD_GATEWAY,
                code: "producer_ended",
                ref message,
            } if message.contains("process_exit")
        ));
    }

    #[test]
    fn if_none_match_uses_http_weak_comparison() {
        assert!(etag_matches(Some("W/\"artifact\""), "\"artifact\""));
        assert!(etag_matches(
            Some("\"other\", W/\"artifact\""),
            "\"artifact\""
        ));
        assert!(etag_matches(Some("*"), "\"artifact\""));
        assert!(!etag_matches(Some("W/\"other\""), "\"artifact\""));
    }

    #[test]
    fn vod_resurrection_uncertainty_is_retryable_not_not_found() {
        let error = vod_resurrection_unavailable();
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "vod_resurrection_unavailable",
                ..
            }
        ));
    }

    #[test]
    fn publication_subdeadline_never_outlives_its_request() {
        let request_deadline = Instant::now();
        assert_eq!(
            response_publication_deadline_before(request_deadline),
            request_deadline
        );
    }

    #[tokio::test(start_paused = true)]
    async fn prepared_body_rejects_buffered_bytes_after_absolute_lifetime() {
        let response = bound_admitted_media_body(Response::new(Body::from("late bytes")));
        tokio::time::advance(MAX_ADMITTED_MEDIA_BODY_LIFETIME).await;
        let error = response
            .into_body()
            .into_data_stream()
            .next()
            .await
            .expect("expired prepared body terminal item")
            .expect_err("expired prepared bytes must not be exposed");
        assert!(error.to_string().contains("maximum admitted body lifetime"));
    }

    #[tokio::test(start_paused = true)]
    async fn streamed_response_settlement_capacity_is_bounded_before_visibility() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&slots)
            .acquire_owned()
            .await
            .expect("first settlement permit");
        let deadline = tokio::time::Instant::now().into_std() + Duration::from_millis(1);
        let error = reserve_response_completion_from(slots, deadline)
            .await
            .expect_err("a second streamed response must not exceed settlement capacity");
        assert!(matches!(
            error,
            ApiError::Typed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "response_completion_capacity",
                ..
            }
        ));
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_store_wait_uses_the_absolute_attempt_deadline() {
        let deadline = tokio::time::Instant::now() + TERMINAL_COMMIT_RETRY_BUDGET;
        let stalled = tokio::spawn(async move {
            terminal_io_before(deadline, std::future::pending::<()>()).await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(TERMINAL_COMMIT_RETRY_BUDGET).await;
        assert_eq!(
            stalled.await.expect("bounded Store wait task"),
            None,
            "a Store operation cannot outlive the terminal attempt budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn local_start_pin_timeout_is_retryable_service_unavailable() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let pending = pin_shared_session_for_local_start(
            deadline,
            std::future::pending::<Result<bool, StoreError>>(),
        );
        tokio::pin!(pending);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(matches!(
            pending.await,
            Err(ApiError::ServiceUnavailable(message))
                if message == crate::transcode::start_infrastructure_error(
                    "shared cache pin exceeded the start deadline"
                )
        ));
    }

    #[tokio::test]
    async fn active_durable_route_without_local_worker_maps_to_owner_transition() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-worker").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: generation.clone(),
            user_id: 7,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: true,
            request: crate::transcode::SessionRequest {
                control_sequence: None,
                file_id: 1,
                playback_id: "control-transition".to_owned(),
                request_id: Some(generation.clone()),
                automatic: true,
                previous_session_id: None,
                reopen_reason: None,
                kind: crate::transcode::SessionKind::Transcode { height: 720 },
                start_seconds: 0.0,
                audio_index: None,
                subtitle_burn: None,
                audio_offset_ms: 0,
                hdr10: false,
                presentation: crate::transcode::Presentation::Vod,
                block_budget_secs: None,
            },
        };
        let start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(60_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 720,
            encoder: "software".to_owned(),
            vod: false,
            ladder: vec![],
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &generation,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let route = MediaSessionRoute {
            incarnation_id: generation.clone(),
            session_id,
            user_id: 7,
            playback_id: "control-transition".to_owned(),
            request_fingerprint: "a".repeat(64),
            owner_node_id: "test-node".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: unix_ms() + 60_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: serde_json::to_string(&recipe).expect("recipe"),
            response_json: serde_json::to_string(&start).expect("response"),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: unix_ms(),
        };
        let request = crate::playback_control::ControlRequestV1 {
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation,
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: crate::playback_control::PlaybackDemand::Active,
            position_ms: 1_000,
            buffered_from_ms: Some(0),
            buffered_through_ms: 10_000,
            playback_rate: 1.0,
            render_state: crate::playback_control::RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: crate::playback_control::ClientSelection {
                quality: crate::playback_control::QualitySelection::Auto,
                audio_track: None,
                subtitle: crate::playback_control::SubtitleSelection {
                    mode: crate::playback_control::SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: crate::playback_control::CodecPolicy::Auto,
                dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
            },
            capabilities: Some(crate::playback_control::DynamicCapabilities {
                platform: crate::playback_control::ClientPlatform::Web,
                max_height: 1080,
                codecs: vec![crate::playback_control::CodecPolicy::H264],
                dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: None,
        };

        let response = control_local(&fixture.state, &route, request, i64::MAX).await;
        assert_eq!(response.status(), StatusCode::TOO_EARLY);
    }

    #[tokio::test]
    async fn public_delete_tombstones_expired_route_but_reports_unreachable_exact_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-delete-worker").await;
        let user = fixture
            .store
            .create_user("delete-transition", "hash", false)
            .await
            .expect("delete user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id,
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "delete-transition".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "former-owner".to_owned(),
                // The row is explicitly active but already outside its owner
                // lease at current wall time: routing must call this a
                // transition, while DELETE must still tombstone it.
                lease_expires_at_ms: 2,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: 1,
            },
        )
        .await;
        let mut stale_cached_local = route.clone();
        stale_cached_local.owner_node_id = fixture.state.node_id.clone();
        fixture
            .state
            .media_sessions
            .cache_route(stale_cached_local)
            .await;

        let Err(transition) = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await
        else {
            panic!("expired active route is not local status absence")
        };
        // The recipe here is `{}`, which no successor can act on, so the
        // expired owner is loss rather than a transition. DELETE below must
        // tombstone it either way.
        assert!(matches!(
            transition,
            ApiError::TypedDetail {
                status: StatusCode::GONE,
                code: "media_owner_lost",
                ..
            }
        ));

        assert_eq!(
            delete(State(fixture.state.clone()), AxPath(session_id.clone()),).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "durable termination is not a claim that unreachable owner cleanup settled"
        );
        let ended = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("ended route lookup")
            .expect("ended route");
        assert_eq!(ended.state, "ended");
        assert_eq!(ended.owner_node_id, route.owner_node_id);
        assert_eq!(ended.incarnation_id, route.incarnation_id);
        let Err(terminal) = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await
        else {
            panic!("terminal status must not return a response")
        };
        assert!(matches!(
            terminal,
            ApiError::Typed {
                status: StatusCode::GONE,
                code: "media_session_ended",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn public_delete_confirmed_absence_still_releases_a_local_attachment() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        assert!(fixture.worker_is_registered(&session_id).await);

        assert_eq!(
            delete(State(fixture.state.clone()), AxPath(session_id.clone()),).await,
            StatusCode::NO_CONTENT
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture.worker_is_registered(&session_id).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("confirmed durable absence eventually releases the rolling attachment");
    }

    #[tokio::test]
    async fn public_delete_tombstones_and_stops_the_exact_local_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "delete-exact-unrelated").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let _owner = install_vod_http_session(&fixture, dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("delete-exact", "hash", false)
            .await
            .expect("delete exact user");
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id: incarnation_id.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "delete-exact".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;

        assert_eq!(
            delete(State(fixture.state.clone()), AxPath(session_id.clone()),).await,
            StatusCode::NO_CONTENT
        );
        let ended = fixture
            .store
            .media_session_route(&session_id)
            .await
            .expect("ended exact route lookup")
            .expect("ended exact route");
        assert_eq!(ended.state, "ended");
        assert_eq!(ended.incarnation_id, incarnation_id);
        assert_eq!(ended.owner_node_id, route.owner_node_id);
        assert_eq!(ended.terminal_reason.as_deref(), Some("deleted"));
        assert_eq!(
            ended.publication_ready_at_ms, 0,
            "204 requires durable exact-owner projection completion"
        );
        assert!(
            fixture
                .state
                .transcode
                .hls_session_status(&session_id)
                .await
                .is_none(),
            "204 means the exact local VOD attachment has settled, not only that its route was tombstoned"
        );
    }

    #[tokio::test]
    async fn durable_delete_tombstone_blocks_media_before_local_stop() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "delete-gap-unrelated").await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let _owner = install_vod_http_session(&fixture, dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("delete-gap", "hash", false)
            .await
            .expect("delete gap user");
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id,
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "delete-gap".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        release_after_tombstone_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), Arc::clone(&pause));
        let detach_pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture
            .state
            .transcode
            .set_vod_terminal_detach_pause_for_test(Arc::clone(&detach_pause));
        let deletion = tokio::spawn({
            let state = fixture.state.clone();
            let session_id = session_id.clone();
            async move { delete(State(state), AxPath(session_id)).await }
        });
        tokio::time::timeout(Duration::from_secs(5), detach_pause.wait())
            .await
            .expect("VOD cleanup reached its pre-detach seam");
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("release reached its post-tombstone seam");
        assert!(
            fixture
                .state
                .transcode
                .vod_has_attached_reader_for_test(&session_id)
                .await,
            "the barrier is specifically inside the tombstone-to-VOD-detach gap"
        );
        let media = playlist(
            State(fixture.state.clone()),
            AxPath(session_id.clone()),
            Query(PlaylistQuery::default()),
            HeaderMap::new(),
        )
        .await;
        assert!(
            matches!(
                media,
                Err(ApiError::Typed {
                    status: StatusCode::GONE,
                    code: "media_session_ended",
                    ..
                })
            ),
            "the cached durable tombstone must refuse media before actor cleanup"
        );

        tokio::time::timeout(Duration::from_secs(5), detach_pause.wait())
            .await
            .expect("release VOD detach");
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("release durable deletion");
        assert_eq!(
            deletion.await.expect("delete gap task"),
            StatusCode::NO_CONTENT
        );
        release_after_tombstone_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    #[tokio::test]
    async fn lingering_local_status_cannot_publish_after_durable_owner_moves_remote() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        assert!(fixture.worker_is_registered(&session_id).await);
        let user = fixture
            .store
            .create_user("status-owner-moved", "hash", false)
            .await
            .expect("status user");
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "status-owner-moved".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "new-owner".to_owned(),
                lease_expires_at_ms: unix_ms() + 60_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;

        let result = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await;
        assert!(
            matches!(result, Err(ApiError::Conflict(_))),
            "a lingering predecessor actor cannot authorize stale status"
        );
        let rerouted = tokio::time::timeout(
            Duration::from_millis(250),
            status_local_before_with_relay(
                &fixture.state,
                &session_id,
                Instant::now() + Duration::from_millis(200),
                true,
            ),
        )
        .await
        .expect("status reroute is bounded by the inherited deadline");
        assert!(matches!(rerouted, Err(ApiError::ServiceUnavailable(_))));
    }

    #[tokio::test]
    async fn status_classifies_durable_absence_before_local_telemetry() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        assert!(fixture.worker_is_registered(&session_id).await);
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        status_telemetry_observers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), Arc::clone(&lookups));

        let result = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotFound("hls session"))));
        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "an unowned capability must not query a lingering local actor"
        );
        status_telemetry_observers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    #[tokio::test]
    async fn cancelling_public_delete_does_not_cancel_its_admitted_cleanup_owner() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        release_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.clone(), Arc::clone(&pause));

        let request = tokio::spawn({
            let state = fixture.state.clone();
            let session_id = session_id.clone();
            async move { delete(State(state), AxPath(session_id)).await }
        });
        pause.wait().await;
        request.abort();
        assert!(request
            .await
            .expect_err("public DELETE request cancelled")
            .is_cancelled());
        pause.wait().await;

        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture.worker_is_registered(&session_id).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached DELETE cleanup survives caller cancellation");
        release_pauses()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    #[tokio::test]
    async fn commit_unknown_delete_stays_fenced_until_definitive_reconciliation() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        inject_release_error(&session_id);
        let settlement = match fixture
            .state
            .media_sessions
            .begin_release_reconciliation(&session_id)
            .await
        {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
        };

        assert_eq!(
            release_session(
                fixture.state.clone(),
                session_id.clone(),
                Arc::clone(&settlement),
                crate::vodserve::Terminal::Deleted,
                "released by client",
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            fixture
                .state
                .media_sessions
                .route_resolution_before(
                    &session_id,
                    &fixture.state.node_id,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .is_err(),
            "a commit-unknown End retains its fail-closed publication fence"
        );

        let definitive = fixture
            .store
            .end_media_session(&session_id, "deleted", unix_ms())
            .await
            .expect("idempotent release reconciliation");
        assert!(definitive.is_none(), "fixture has no durable route");
        fixture
            .state
            .media_sessions
            .complete_release_absent(&session_id)
            .await;
        fixture
            .state
            .transcode
            .complete_session_release(&session_id);
        fixture
            .state
            .media_sessions
            .complete_release_settlement(&session_id, &settlement, StatusCode::NO_CONTENT)
            .await;
        assert!(matches!(
            fixture
                .state
                .media_sessions
                .route_resolution_before(
                    &session_id,
                    &fixture.state.node_id,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("definitive absence after reconciliation"),
            DurableRouteResolution::Absent
        ));
    }

    #[tokio::test]
    async fn public_delete_refuses_visibility_when_settlement_slots_are_saturated() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "delete-slot-unrelated").await;
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&slots)
            .acquire_owned()
            .await
            .expect("hold only DELETE settlement slot");
        let status = delete_with_slots(
            fixture.state.clone(),
            uuid::Uuid::new_v4().to_string(),
            slots,
            Instant::now() + Duration::from_millis(10),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        drop(held);
    }

    #[tokio::test]
    async fn terminal_control_cancellation_and_reaper_preserve_one_durable_reply() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        let user = fixture
            .store
            .create_user("terminal-cancellation", "hash", false)
            .await
            .expect("terminal cancellation user");
        let recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: generation.clone(),
            user_id: user.id,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: true,
            request: crate::transcode::SessionRequest {
                control_sequence: None,
                file_id: fixture.file_id(),
                playback_id: "terminal-cancellation".to_owned(),
                request_id: Some(generation.clone()),
                automatic: true,
                previous_session_id: None,
                reopen_reason: None,
                kind: crate::transcode::SessionKind::Transcode { height: 720 },
                start_seconds: 0.0,
                audio_index: None,
                subtitle_burn: None,
                audio_offset_ms: 0,
                hdr10: false,
                presentation: crate::transcode::Presentation::Live,
                block_budget_secs: None,
            },
        };
        let start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(60_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 720,
            encoder: "software".to_owned(),
            vod: false,
            ladder: vec![],
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &generation,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id: generation.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "terminal-cancellation".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: now_ms.saturating_add(60_000),
                recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                response_json: serde_json::to_string(&start).expect("start response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms,
            },
        )
        .await;
        let request = crate::playback_control::ControlRequestV1 {
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation: generation.clone(),
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: crate::playback_control::PlaybackDemand::End,
            position_ms: 1_000,
            buffered_from_ms: Some(0),
            buffered_through_ms: 10_000,
            playback_rate: 0.0,
            render_state: crate::playback_control::RenderState::Ended,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: crate::playback_control::ClientSelection {
                quality: crate::playback_control::QualitySelection::Auto,
                audio_track: None,
                subtitle: crate::playback_control::SubtitleSelection {
                    mode: crate::playback_control::SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: crate::playback_control::CodecPolicy::Auto,
                dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
            },
            capabilities: Some(crate::playback_control::DynamicCapabilities {
                platform: crate::playback_control::ClientPlatform::Apple,
                max_height: 1080,
                codecs: vec![crate::playback_control::CodecPolicy::H264],
                dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: None,
        };

        // A future discarded before owner-local admission must not enqueue or
        // mutate anything later.
        drop(control_local(
            &fixture.state,
            &route,
            request.clone(),
            i64::MAX,
        ));
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("route after unpolled control")
                .map(|route| route.state),
            Some("active".to_owned())
        );
        assert!(fixture
            .store
            .media_session_terminal_ack(&session_id, unix_ms())
            .await
            .expect("ack after unpolled control")
            .is_none());

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture.pause_control_after_acceptance(Arc::clone(&pause));
        let control = tokio::spawn({
            let state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move { control_local(&state, &route, request, i64::MAX).await }
        });
        pause.wait().await;

        let retry_a = tokio::spawn({
            let state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move { control_local(&state, &route, request, i64::MAX).await }
        });
        let retry_b = tokio::spawn({
            let state = fixture.state.clone();
            let route = route.clone();
            let request = request.clone();
            async move { control_local(&state, &route, request, i64::MAX).await }
        });
        tokio::task::yield_now().await;

        // Run the production reaper verdict after actor End but before the
        // final status join, with two exact retries also attached. All three
        // waiters must share the one actor-installed continuation, and the
        // reaper must not mistake the retired actor for abandoned cleanup.
        assert!(fixture.reaper_pass_keeps_worker(&session_id).await);
        assert!(fixture.worker_is_registered(&session_id).await);
        control.abort();
        assert!(matches!(control.await, Err(error) if error.is_cancelled()));
        pause.wait().await;

        let retry_a = retry_a.await.expect("first retry task");
        let retry_b = retry_b.await.expect("second retry task");
        assert_eq!(retry_a.status(), StatusCode::OK);
        assert_eq!(retry_b.status(), StatusCode::OK);
        let retry_a = axum::body::to_bytes(retry_a.into_body(), 64 * 1024)
            .await
            .expect("first retry body");
        let retry_b = axum::body::to_bytes(retry_b.into_body(), 64 * 1024)
            .await
            .expect("second retry body");
        assert_eq!(retry_a, retry_b, "exact retries share one terminal result");

        let acknowledgement = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(acknowledgement) = fixture
                    .store
                    .media_session_terminal_ack(&session_id, unix_ms())
                    .await
                    .expect("terminal acknowledgement")
                {
                    break acknowledgement;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("accepted End continuation must commit after HTTP cancellation");
        assert_eq!(acknowledgement.sequence, 1);
        assert_eq!(
            fixture
                .store
                .media_session_route(&session_id)
                .await
                .expect("settled terminal route")
                .map(|route| route.state),
            Some("ended".to_owned())
        );
    }

    #[tokio::test]
    async fn settled_rolling_and_vod_routes_replay_the_durable_terminal_ack() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unrelated-worker").await;
        let user = fixture
            .store
            .create_user("terminal-replay", "hash", false)
            .await
            .expect("terminal replay user");

        for (label, presentation, lease_timeout_ms, producer_state, admitted) in [
            (
                "rolling",
                crate::transcode::Presentation::Live,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
                "exited",
                None,
            ),
            (
                "vod",
                crate::transcode::Presentation::Vod,
                crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                "complete",
                Some(true),
            ),
        ] {
            let session_id = uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client_instance_id = uuid::Uuid::new_v4().to_string();
            let recipe = RemoteStartRequest {
                protocol_version: crate::media_pool::PROTOCOL_VERSION,
                incarnation_id: generation.clone(),
                user_id: user.id,
                source_size: 1,
                source_mtime: 1,
                typeless_playlist: true,
                request: crate::transcode::SessionRequest {
                    control_sequence: None,
                    file_id: fixture.file_id(),
                    playback_id: format!("terminal-{label}"),
                    request_id: Some(generation.clone()),
                    automatic: true,
                    previous_session_id: None,
                    reopen_reason: None,
                    kind: crate::transcode::SessionKind::Transcode { height: 720 },
                    start_seconds: 0.0,
                    audio_index: None,
                    subtitle_burn: None,
                    audio_offset_ms: 0,
                    hdr10: false,
                    presentation,
                    block_budget_secs: None,
                },
            };
            let start = StartResponse {
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
                duration_ms: Some(60_000),
                start_seconds: 0.0,
                media_origin_ms: Some(0),
                height: 720,
                encoder: "software".to_owned(),
                vod: label == "vod",
                ladder: vec![],
                prior_kbps: None,
                delivered_dynamic_range: Some("sdr".to_owned()),
                delivered_dolby_vision_profile: None,
                control: crate::playback_control::ControlBootstrap::new(
                    &session_id,
                    &generation,
                    1,
                    lease_timeout_ms,
                ),
                plan_notes: Vec::new(),
            };
            let now_ms = unix_ms();
            let route = activate_ready(
                &fixture.store,
                MediaSessionActivation {
                    incarnation_id: generation.clone(),
                    session_id: session_id.clone(),
                    user_id: user.id,
                    playback_id: format!("terminal-{label}"),
                    expected_predecessor_incarnation_id: None,
                    fence_predecessor: false,
                    request_id: None,
                    request_fingerprint: "a".repeat(64),
                    owner_node_id: fixture.state.node_id.clone(),
                    lease_expires_at_ms: now_ms.saturating_add(60_000),
                    recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                    response_json: serde_json::to_string(&start).expect("start response"),
                    publication_ready_at_ms: 0,
                    media_origin_ms: 0,
                    now_ms,
                },
            )
            .await;
            let request = crate::playback_control::ControlRequestV1 {
                protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
                generation: generation.clone(),
                control_epoch: 1,
                client_instance_id: client_instance_id.clone(),
                sequence: 7,
                demand: crate::playback_control::PlaybackDemand::End,
                position_ms: 1_000,
                buffered_from_ms: Some(0),
                buffered_through_ms: 10_000,
                playback_rate: 0.0,
                render_state: crate::playback_control::RenderState::Ended,
                seek_target_ms: None,
                observed_download_bps: None,
                selection: crate::playback_control::ClientSelection {
                    quality: crate::playback_control::QualitySelection::Auto,
                    audio_track: None,
                    subtitle: crate::playback_control::SubtitleSelection {
                        mode: crate::playback_control::SubtitleMode::Off,
                        track: None,
                    },
                    audio_offset_ms: 0,
                    codec: crate::playback_control::CodecPolicy::Auto,
                    dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
                },
                // Sequence > 1 retries may omit capabilities; replay
                // telemetry must come from the retained accepted result.
                capabilities: None,
                observation: None,
                acknowledgement: None,
                supported_actions: None,
            };
            let terminal_time_ms = unix_ms();
            let terminal = crate::playback_control::ControlResponseV1 {
                protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
                generation: generation.clone(),
                control_epoch: 1,
                accepted_sequence: request.sequence,
                server_time_unix_ms: terminal_time_ms,
                lease: crate::playback_control::PlaybackLeaseView {
                    state: "ended".to_owned(),
                    renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
                    expires_at_unix_ms: terminal_time_ms,
                },
                delivery: crate::playback_control::DeliveryView {
                    presentation: if label == "vod" {
                        "vod".to_owned()
                    } else {
                        "live-recovery".to_owned()
                    },
                    producer_state: producer_state.to_owned(),
                    produced_through_ms: Some(60_000),
                    fetched_through_ms: 10_000,
                    delivered_bps: None,
                    delivered_idle_ms: None,
                    recent_producer_speed: None,
                    client_runway_ms: 9_000,
                    admitted,
                    producer_decision: None,
                    hold_reason: None,
                    subtitle_readiness: None,
                    owner_node_hash: "n-0123456789abcdef".to_owned(),
                    owner_epoch: 1,
                },
                effective_selection: crate::playback_control::EffectiveSelection {
                    quality_auto: true,
                    height: 720,
                    audio_track: None,
                    subtitle_burn: None,
                    audio_offset_ms: 0,
                    codec: "server_selected".to_owned(),
                    dynamic_range: Some("sdr".to_owned()),
                },
                action: crate::playback_control::ControlAction::None,
            };
            let acknowledgement = MediaSessionTerminalAck {
                incarnation_id: generation.clone(),
                session_id: session_id.clone(),
                owner_node_id: route.owner_node_id.clone(),
                owner_epoch: route.owner_epoch,
                client_instance_id,
                sequence: i64::try_from(request.sequence).expect("bounded sequence"),
                request_fingerprint: request
                    .fingerprint()
                    .expect("valid terminal request fingerprint"),
                response_json: serde_json::to_string(&RetainedTerminalResponse {
                    platform: crate::playback_control::ClientPlatform::Apple,
                    response: terminal.clone(),
                })
                .expect("terminal response"),
                expires_at_ms: terminal_time_ms.saturating_add(60_000),
                updated_at_ms: terminal_time_ms,
            };
            let faults = TerminalCommitFaults::default();
            let injected = if label == "rolling" {
                &faults.fail_before_commit
            } else {
                &faults.fail_after_commit
            };
            injected.store(1, std::sync::atomic::Ordering::Release);
            let terminal_store: Arc<dyn plurx_core::store::Store> = fixture.store.clone();
            assert!(
                persist_terminal_ack_with_faults(terminal_store, acknowledgement, Some(&faults),)
                    .await,
                "{label} terminal acknowledgement resolves the injected Store failure"
            );
            assert_eq!(
                injected.load(std::sync::atomic::Ordering::Acquire),
                0,
                "{label} consumed its injected before/after-commit failure"
            );
            let retained = terminal_ack_replay(
                &fixture.state,
                &route,
                &request,
                unix_ms().saturating_add(4_000),
            )
            .await
            .expect("terminal acknowledgement lookup")
            .expect("exact terminal acknowledgement");
            assert_eq!(
                retained.platform,
                Some(crate::playback_control::ClientPlatform::Apple),
                "{label} replay must retain the originally accepted platform"
            );
            let body = Bytes::from(serde_json::to_vec(&request).expect("terminal request"));
            let response = control_inner(
                fixture.state.clone(),
                session_id,
                body,
                unix_ms().saturating_add(4_000),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{label} replay");
            let response_body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("bounded terminal replay body");
            assert_eq!(
                serde_json::from_slice::<crate::playback_control::ControlResponseV1>(
                    &response_body,
                )
                .expect("decode terminal replay"),
                terminal,
                "{label} replay must return the exact durable acknowledgement"
            );
            let mut changed_payload = request.clone();
            changed_payload.position_ms += 1;
            let changed_response = control_inner(
                fixture.state.clone(),
                route.session_id.clone(),
                Bytes::from(
                    serde_json::to_vec(&changed_payload).expect("changed terminal request"),
                ),
                unix_ms().saturating_add(4_000),
            )
            .await;
            assert_eq!(
                changed_response.status(),
                StatusCode::GONE,
                "{label} cannot reuse the terminal sequence for a changed payload"
            );
        }
    }

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
        assert!(range_covers_object(None, 100));
        assert!(range_covers_object(
            requested_byte_range(Some("bytes=0-"), 100).expect("open range"),
            100
        ));
        assert!(range_covers_object(
            requested_byte_range(Some("bytes=-200"), 100).expect("full suffix"),
            100
        ));
        assert!(!range_covers_object(Some((0, 98)), 100));
        assert!(!range_covers_object(Some((1, 99)), 100));

        let mut headers = RelayHeaders {
            range: Some("bytes=10-19".to_owned()),
            if_range: None,
            ..RelayHeaders::default()
        };
        assert_eq!(
            range_for_current_etag(&headers, "\"current\""),
            Some("bytes=10-19")
        );
        headers.if_range = Some("\"current\"".to_owned());
        assert_eq!(
            range_for_current_etag(&headers, "\"current\""),
            Some("bytes=10-19"),
            "an exact strong validator preserves Range"
        );
        for non_match in [
            "W/\"current\"",
            "\"previous\"",
            "Sun, 23 Aug 2026 08:00:00 GMT",
            "",
        ] {
            headers.if_range = Some(non_match.to_owned());
            assert_eq!(
                range_for_current_etag(&headers, "\"current\""),
                None,
                "{non_match:?} must fall back to the complete representation"
            );
        }
    }

    #[test]
    fn mixed_rollout_peer_terminal_statuses_require_fresh_route_agreement() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::GONE,
        ] {
            assert!(
                relay_status_requires_reclassification(status),
                "{status} from one peer is not authoritative during ownership handoff"
            );
        }
        assert!(!relay_status_requires_reclassification(StatusCode::OK));
        assert!(!relay_status_requires_reclassification(
            StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    async fn add_http_text_subtitle(fixture: &mut HlsDeliveryFixture, session_id: &str) {
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("fixture file lookup")
            .expect("fixture file");
        let probe = plurx_core::domain::ProbeResult {
            duration_ms: file.duration_ms,
            container: file.container.clone(),
            video_codec: file.video_codec.clone(),
            video_profile: file.video_profile.clone(),
            width: file.width,
            height: file.height,
            bit_depth: file.bit_depth,
            hdr: file.hdr.clone(),
            dolby_vision: Default::default(),
            hdr_format: file.hdr_format.clone(),
            bitrate: file.bitrate,
            audio_streams: file.audio_streams.clone(),
            subtitle_streams: vec![SubtitleStream {
                index: 0,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: Some("English".into()),
                default: true,
                forced: false,
                hearing_impaired: false,
            }],
            raw_json: None,
            creation_time: None,
        };
        fixture
            .store
            .upsert_file(
                file.item_id,
                file.path.to_str().expect("fixture path"),
                file.size,
                file.mtime,
                &probe,
            )
            .await
            .expect("install text subtitle");
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("updated fixture lookup")
            .expect("updated fixture");
        fixture
            .refresh_frozen_presentation_from_store(session_id)
            .await;
        tokio::fs::create_dir_all(&fixture.state.subs_dir)
            .await
            .expect("subtitle cache");
        tokio::fs::write(
            crate::subtitles::vtt_path(&fixture.state.subs_dir, &file, 0),
            b"WEBVTT\n\n",
        )
        .await
        .expect("published VTT sidecar");
    }

    struct WindowFixtureSubtitleSource {
        whole_runs: Arc<std::sync::atomic::AtomicUsize>,
        whole_started: Arc<tokio::sync::Semaphore>,
        whole_release: Arc<tokio::sync::Semaphore>,
        runs: Arc<std::sync::atomic::AtomicUsize>,
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
        /// Window producers running *right now*.
        ///
        /// Counted with a drop guard rather than by subtracting completions,
        /// because the case this exists to measure is the producer that never
        /// completes: a superseded extraction is dropped mid-body, and a
        /// counter incremented at the end of the body would never learn it had
        /// gone. The guard runs whether the future finishes or is dropped, so
        /// this is the physical number of live extractions.
        live: Arc<std::sync::atomic::AtomicUsize>,
        /// The highest `live` ever reached.
        ///
        /// Sampling `live` from the test task can only observe instants the
        /// test task is awake for, and after an await that waited for
        /// settlement those instants are the ones where an overlap is least
        /// likely. This is recorded by the producers themselves, so an overlap
        /// that existed for one poll is still visible afterwards.
        peak_live: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// Decrements the live-producer count however its producer ends.
    struct LiveProducer(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for LiveProducer {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl WindowFixtureSubtitleSource {
        fn counting() -> Self {
            Self {
                whole_runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                whole_started: Arc::new(tokio::sync::Semaphore::new(0)),
                whole_release: Arc::new(tokio::sync::Semaphore::new(0)),
                runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                started: Arc::new(tokio::sync::Semaphore::new(0)),
                release: Arc::new(tokio::sync::Semaphore::new(0)),
                live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                peak_live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn window_runs(&self) -> usize {
            self.runs.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn live_window_producers(&self) -> usize {
            self.live.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn peak_window_producers(&self) -> usize {
            self.peak_live.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl SubtitleSegmentSource for WindowFixtureSubtitleSource {
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
            let runs = Arc::clone(&self.whole_runs);
            let started = Arc::clone(&self.whole_started);
            let whole_release = Arc::clone(&self.whole_release);
            Box::pin(crate::subtitles::warm_vtt_with(
                dir,
                file,
                index,
                move |tmp, _, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    started.add_permits(1);
                    whole_release
                        .acquire()
                        .await
                        .expect("release whole producer")
                        .forget();
                    tokio::fs::write(tmp, b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nwhole cue\n")
                        .await
                        .map_err(|error| error.to_string())
                },
            ))
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
            let runs = Arc::clone(&self.runs);
            let started = Arc::clone(&self.started);
            let release = Arc::clone(&self.release);
            let live = Arc::clone(&self.live);
            let peak_live = Arc::clone(&self.peak_live);
            Box::pin(crate::subtitles::warm_vtt_window_with(
                session,
                sequence,
                dir,
                file,
                index,
                anchor_seconds,
                window_seconds,
                move |tmp, _, _, _, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let now = live.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    peak_live.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    let _live = LiveProducer(live);
                    // The partial write happens BEFORE the park, so a producer
                    // that is superseded has a temp file on disk when it dies.
                    // A real ffmpeg is the same shape — it is writing while it
                    // runs — and without it "an abort unlinks its own temp
                    // file" would be a claim about a file that never existed.
                    tokio::fs::write(&tmp, b"WEBVTT\n\n")
                        .await
                        .map_err(|error| error.to_string())?;
                    started.add_permits(1);
                    // A superseded producer is dropped exactly here, which is
                    // what makes `live` fall again without this body ever
                    // reaching its end.
                    // `forget` rather than holding the guard: a permit that
                    // returned to the semaphore when one producer finished
                    // would silently release the next one, and this fixture
                    // exists to decide exactly which producers get to run.
                    release
                        .acquire()
                        .await
                        .expect("release window producer")
                        .forget();
                    tokio::fs::write(tmp, b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nready cue\n")
                        .await
                        .map_err(|error| error.to_string())
                },
            ))
        }
    }

    /// M7 R-M2 B7: exercise the real subtitle-segment handler boundary while
    /// replacing only the producer. The first requests must not wait for the
    /// held extraction, concurrent first touches must share its production
    /// flight, and the same advertised segment must expose its cues after the
    /// window publishes. A segment the video playlist never advertised stays
    /// a 404 throughout.
    #[tokio::test]
    async fn subtitles_cold_segment_is_empty_nonblocking_then_serves_one_window_flight() {
        let dir = crate::test_tempdir().expect("session directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "subtitle-window").await;
        add_http_text_subtitle(&mut fixture, "subtitle-window").await;
        let file = fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("fixture file lookup")
            .expect("fixture file");
        tokio::fs::remove_file(crate::subtitles::vtt_path(
            &fixture.state.subs_dir,
            &file,
            0,
        ))
        .await
        .expect("return fixture to a cold cache");
        tokio::fs::write(dir.path().join("seg00000.ts"), b"video")
            .await
            .expect("video segment");
        tokio::fs::write(
            dir.path().join("index.m3u8"),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.000,\nseg00000.ts\n",
        )
        .await
        .expect("video playlist");
        assert_eq!(
            fixture
                .state
                .transcode
                .segment_window("subtitle-window", 0)
                .await,
            Some((0.0, 4.0)),
            "the fixture advertises exactly one subtitle interval"
        );

        let source = Arc::new(WindowFixtureSubtitleSource::counting());
        let request = |state: AppState, source: Arc<WindowFixtureSubtitleSource>| async move {
            subtitle_vtt_local_before_with_source(
                &state,
                "subtitle-window",
                0,
                "seg00000.vtt",
                Instant::now() + Duration::from_secs(5),
                source.as_ref(),
            )
            .await
            .expect("advertised subtitle segment")
        };
        let first = tokio::spawn(request(fixture.state.clone(), Arc::clone(&source)));
        let second = tokio::spawn(request(fixture.state.clone(), Arc::clone(&source)));

        let _started = tokio::time::timeout(Duration::from_secs(2), source.started.acquire())
            .await
            .expect("window producer starts")
            .expect("started semaphore remains open");
        let _whole_started =
            tokio::time::timeout(Duration::from_secs(2), source.whole_started.acquire())
                .await
                .expect("whole-track producer starts")
                .expect("whole started semaphore remains open");
        let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
            (
                first.await.expect("first request task"),
                second.await.expect("second request task"),
            )
        })
        .await
        .expect("cold requests return without waiting for the held producer");
        assert_eq!(
            source.runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "concurrent first touches share exactly one window producer"
        );
        assert_eq!(
            source.whole_runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the artificially slow whole-track producer is also single-flight"
        );
        for response in [first, second] {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&axum::http::HeaderValue::from_static("no-store"))
            );
            let body = response
                .into_body()
                .collect()
                .await
                .expect("empty VTT body")
                .to_bytes();
            assert_eq!(body.as_ref(), b"WEBVTT\n\n");
        }

        let missing = subtitle_vtt_local_before_with_source(
            &fixture.state,
            "subtitle-window",
            0,
            "seg00001.vtt",
            Instant::now() + Duration::from_secs(5),
            source.as_ref(),
        )
        .await;
        assert!(matches!(
            missing,
            Err(ApiError::NotFound("subtitle segment"))
        ));

        source.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        0,
                        200,
                    )
                    .await,
                    Ok(Some(_))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("window publishes");

        let ready = request(fixture.state.clone(), Arc::clone(&source)).await;
        assert_eq!(ready.status(), StatusCode::OK);
        assert_eq!(
            ready.headers().get(header::CACHE_CONTROL),
            Some(&axum::http::HeaderValue::from_static("no-store"))
        );
        let body = ready
            .into_body()
            .collect()
            .await
            .expect("ready VTT body")
            .to_bytes();
        let body = String::from_utf8(body.to_vec()).expect("utf8 VTT");
        assert!(body.contains("ready cue"), "{body}");
        assert_eq!(
            source.runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "serving the published window does not relaunch production"
        );

        source.whole_release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("slow whole-track producer settles before fixture cleanup");
    }

    /// M7 R-M3: seek coalescing at the production boundary.
    ///
    /// The acceptance language is that "a 20-seek storm starts work only for
    /// the settled target". Nineteen logical `supersedes` verdicts do not say
    /// that — they say the latch can rank two numbers. What says it is a storm
    /// driven through real control exchanges and the real subtitle-segment
    /// handler, with the producer counted as it spawns and as it dies.
    mod seek_coalescing {
        use super::*;

        /// One segment per window anchor, so a storm walks a real grid rather
        /// than re-requesting one key. Sixty-second segments sit under the
        /// hundred-and-twenty-second retention cap, and pinning the operator's
        /// span to its thirty-second minimum makes every segment start its own
        /// anchor. The fixture film is 6,000 s and a window declines past its
        /// midpoint, so every segment used here stays well below 3,000 s.
        const SEGMENT_SECONDS: i64 = 60;
        const SEGMENTS: i64 = 15;
        const WINDOW_SECONDS: i64 = 30;

        /// A subtitle-capable session with a cold cache and a long playlist.
        async fn cold_windowed_fixture(
            dir: &std::path::Path,
            session_id: &str,
        ) -> (HlsDeliveryFixture, MediaFile) {
            let mut fixture = HlsDeliveryFixture::publish(dir, session_id).await;
            fixture
                .store
                .put_setting(
                    plurx_core::store::keys::SUBTITLE_WINDOW_SECS,
                    &WINDOW_SECONDS.to_string(),
                )
                .await
                .expect("pin the operator's window span");
            add_http_text_subtitle(&mut fixture, session_id).await;
            let file = fixture
                .store
                .get_file(fixture.file_id())
                .await
                .expect("fixture file lookup")
                .expect("fixture file");
            tokio::fs::remove_file(crate::subtitles::vtt_path(
                &fixture.state.subs_dir,
                &file,
                0,
            ))
            .await
            .expect("return the fixture to a cold cache");
            let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:60\n");
            for index in 0..SEGMENTS {
                let name = format!("seg{index:05}.ts");
                tokio::fs::write(dir.join(&name), b"video")
                    .await
                    .expect("video segment");
                playlist.push_str(&format!("#EXTINF:60.000,\n{name}\n"));
            }
            tokio::fs::write(dir.join("index.m3u8"), playlist.as_bytes())
                .await
                .expect("video playlist");
            // The segment index is built by the publication flow, not by the
            // file on disk, so ask for the last interval once. It both primes
            // the index every subtitle request reads and proves the fixture
            // really advertises the grid this storm walks.
            assert_eq!(
                fixture
                    .state
                    .transcode
                    .segment_window(session_id, SEGMENTS - 1)
                    .await,
                Some((
                    ((SEGMENTS - 1) * SEGMENT_SECONDS) as f64,
                    (SEGMENTS * SEGMENT_SECONDS) as f64
                )),
                "the fixture advertises every segment the storm asks for"
            );
            (fixture, file)
        }

        /// Wait until the producer a request just started is actually running.
        ///
        /// `warm_window` returns as soon as the flight is owned; the extraction
        /// itself begins one poll later. Counting live producers before that
        /// happens would measure the scheduler, not the contract.
        async fn producer_started(source: &WindowFixtureSubtitleSource) {
            match tokio::time::timeout(Duration::from_secs(5), source.started.acquire()).await {
                Ok(permit) => permit.expect("started semaphore remains open").forget(),
                Err(_) => panic!(
                    "no window producer started: runs={} live={}",
                    source.window_runs(),
                    source.live_window_producers()
                ),
            }
        }

        async fn subtitle_segment(
            state: &AppState,
            session: &str,
            segment: i64,
            source: &WindowFixtureSubtitleSource,
        ) -> Response {
            subtitle_vtt_local_before_with_source(
                state,
                session,
                0,
                &format!("seg{segment:05}.vtt"),
                Instant::now() + Duration::from_secs(5),
                source,
            )
            .await
            .expect("an advertised subtitle segment")
        }

        /// One accepted control exchange naming where the client now is.
        async fn settle_on(
            fixture: &HlsDeliveryFixture,
            session_id: &str,
            generation: &str,
            client: &str,
            sequence: u64,
            target_seconds: i64,
        ) -> Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        > {
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Web,
            );
            let target_ms = target_seconds.saturating_mul(1_000);
            // A storm is a scrub bar being dragged, so the snapshots that drive
            // it are seeks: the client is not where it was, and `position_ms`
            // still names the old place until the seek lands.
            snapshot.render_state = crate::playback_control::RenderState::Seeking;
            snapshot.position_ms = target_ms.saturating_sub(4_000).max(0);
            snapshot.seek_target_ms = Some(target_ms);
            snapshot.buffered_from_ms = Some(target_ms);
            snapshot.buffered_through_ms = target_ms.saturating_add(15_000);
            fixture
                .state
                .transcode
                .hls_session_control(crate::playback_control::LocalControlRequest {
                    session_id,
                    generation,
                    owner_node_id: "test-node",
                    owner_epoch: 1,
                    client_instance_id: client,
                    sequence,
                    snapshot,
                    prepared_successor:
                        crate::playback_control::PreparedSuccessorObservation::NotRequested,
                })
                .await
                .expect("the storm session is local")
        }

        /// The twenty destinations. It walks forward and scrubs back, because
        /// a real storm is a scrub bar being dragged, not a monotonic ramp —
        /// and a revisited anchor proves an aborted flight left no memo behind
        /// to suppress its own retry.
        const STORM: [i64; 20] = [
            0, 3, 1, 6, 2, 9, 4, 11, 5, 13, 8, 14, 7, 12, 10, 6, 9, 3, 11, 7,
        ];

        #[tokio::test]
        async fn a_twenty_seek_storm_starts_work_only_for_the_settled_target() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client = uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &generation,
                "test-node",
            )
            .await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());
            let window_seconds = fixture.state.subtitle_window_seconds().await;

            // Fact 6 needs a window that really published, and it has to
            // publish before the storm moves on, so the first destination is
            // driven to completion on its own.
            let first =
                subtitle_segment(&fixture.state, session_id, STORM[0], source.as_ref()).await;
            assert_eq!(first.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;
            source.release.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        STORM[0] * SEGMENT_SECONDS,
                        window_seconds,
                    )
                    .await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the first window publishes");

            let mut sequence = 0_u64;
            // A create that the storm has already passed, held back so it
            // reaches admission after a later snapshot was accepted.
            let mut pending_create: Option<(u64, f64)> = None;
            let mut refusals = 0_usize;
            for segment in STORM {
                sequence += 1;
                let target_seconds = segment * SEGMENT_SECONDS;

                let outcome = settle_on(
                    &fixture,
                    session_id,
                    &generation,
                    &client,
                    sequence,
                    target_seconds,
                )
                .await
                .expect("the storm's exchanges are accepted");
                assert_eq!(
                    outcome.disposition,
                    crate::playback_control::ControlDisposition::Accepted
                );

                // Fact 1, with the ordering the acceptance names: the snapshot
                // for this destination has just been accepted, and only now
                // does the create for the *previous* one reach admission. The
                // settled target it is judged against is therefore read from
                // the live actor after the later exchange landed, which is the
                // race a create loses in production.
                //
                // `create` itself needs a store, a transcode manager and an
                // authenticated user, so the acceptance reaches the decision
                // where the handler reaches it — through `admit_restart`, which
                // returns the same `ApiError` the client receives and sits
                // above every producer and activation in `create`'s body.
                if let Some((stale_sequence, stale_start)) = pending_create.take() {
                    let settled = fixture
                        .state
                        .transcode
                        .settled_target_for_session(session_id)
                        .await;
                    match admit_restart(settled, Some(stale_sequence), stale_start, None) {
                        Err(ApiError::Typed { status, code, .. }) => {
                            assert_eq!(status, StatusCode::CONFLICT);
                            assert_eq!(code, "playback_target_superseded");
                            refusals += 1;
                        }
                        other => panic!("a create for a destination the storm left: {other:?}"),
                    }
                }

                // Fact 2: the ordering rules the storm relies on are unchanged.
                assert!(matches!(
                    settle_on(
                        &fixture,
                        session_id,
                        &generation,
                        &client,
                        sequence,
                        target_seconds
                    )
                    .await,
                    Ok(crate::playback_control::LocalControlResult {
                        disposition: crate::playback_control::ControlDisposition::Replay,
                        ..
                    })
                ));
                assert!(matches!(
                    settle_on(
                        &fixture,
                        session_id,
                        &generation,
                        &client,
                        sequence - 1,
                        target_seconds
                    )
                    .await,
                    Err(crate::playback_control::ControlStateError::StaleSequence)
                ));

                let response =
                    subtitle_segment(&fixture.state, session_id, segment, source.as_ref()).await;
                assert_eq!(response.status(), StatusCode::OK);
                if segment != STORM[0] {
                    // Every destination but the first is cold, so every one of
                    // them spawns. The first is already published and correctly
                    // starts nothing at all.
                    producer_started(source.as_ref()).await;
                }

                // Fact 4, asserted at every step rather than at the end: one
                // playback never has more than one live window producer.
                assert!(
                    source.live_window_producers() <= 1,
                    "sequence {sequence} left {} window producers alive",
                    source.live_window_producers()
                );

                // Hold back this destination's create so the next iteration
                // presents it after a later snapshot has landed.
                pending_create = Some((sequence, target_seconds as f64));

                // Above the protocol's 250 ms exchange floor, which is what
                // makes the next sequence an acceptance rather than a
                // rate-limit rejection.
                tokio::time::sleep(Duration::from_millis(260)).await;
            }

            assert_eq!(
                refusals,
                STORM.len() - 1,
                "every destination but the first is superseded before its own create lands"
            );

            // Fact 8: exactly one producer survives, and it carries the target
            // the client actually settled on.
            let settled = fixture
                .state
                .transcode
                .settled_target_for_session(session_id)
                .await
                .expect("the storm settled somewhere");
            let final_anchor =
                crate::subtitles::window_anchor_seconds(settled.anchor_ms / 1_000, window_seconds);
            let owned = crate::subtitles::owned_window_for_test(session_id)
                .expect("the settled destination is still being extracted");
            assert_eq!(
                owned.0, final_anchor,
                "the survivor carries the final target"
            );
            assert_eq!(owned.1, window_seconds);
            assert_eq!(owned.2, Some(sequence));
            assert_eq!(
                source.live_window_producers(),
                1,
                "the storm settles on exactly one live producer"
            );
            assert_eq!(
                source.peak_window_producers(),
                1,
                "and never had two extractors running at once during it"
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1,
                "nor two flights, which is the wider span the settlement wait \
                 exists to keep from overlapping — a displaced flight is still \
                 alive while it kills its child and clears the registries"
            );

            // Release every held producer before judging what published.
            //
            // Without this the storm's abandoned anchors are absent for a
            // reason that says nothing about supersession: the fixture never
            // let any of them past its gate. Opening the gate now means a
            // producer that was merely *parked* would wake and publish, while
            // one that was actually aborted is gone and cannot. Waiting for the
            // survivor's own window to appear proves the permits really flowed.
            source.release.add_permits(STORM.len());
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        final_anchor,
                        window_seconds,
                    )
                    .await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the surviving producer publishes once it is released");

            // Fact 5: every destination the storm passed through published
            // nothing. Only the first, which was deliberately driven to
            // completion, is on disk.
            for segment in STORM {
                if segment == STORM[0] || segment * SEGMENT_SECONDS == final_anchor {
                    continue;
                }
                assert!(
                    matches!(
                        crate::subtitles::read_cached_window(
                            &fixture.state.subs_dir,
                            &file,
                            0,
                            segment * SEGMENT_SECONDS,
                            window_seconds,
                        )
                        .await,
                        Ok(None)
                    ),
                    "an abandoned window at {segment} published bytes"
                );
            }

            // Fact 6: the window that did publish is untouched by every
            // supersession that followed it.
            assert!(matches!(
                crate::subtitles::read_cached_window(
                    &fixture.state.subs_dir,
                    &file,
                    0,
                    STORM[0] * SEGMENT_SECONDS,
                    window_seconds,
                )
                .await,
                Ok(Some(_))
            ));

            // Nothing was left half-written. An abort unlinks its own temp
            // file and only its own temp file, so inverting that unlink to the
            // cache name — the one way a cancellation could reach a published
            // sidecar — shows up here as a `.tmp-` entry that never goes away.
            //
            // Waited for rather than sampled once: the survivor's publication is
            // what proved the gate opened, and `read_cached_window` starts
            // answering at the atomic write, one unlink before that producer is
            // finished. Sampling on that edge measures how fast the runner is.
            // The wait still fails on the defect, because a temp file nothing
            // will ever unlink outlasts any timeout.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let mut stray = None;
                    let mut entries = tokio::fs::read_dir(&fixture.state.subs_dir)
                        .await
                        .expect("subtitle cache directory");
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with(".tmp-") {
                            stray = Some(name);
                            break;
                        }
                    }
                    match stray {
                        None => return,
                        Some(_) => tokio::task::yield_now().await,
                    }
                }
            })
            .await
            .expect("every abandoned window unlinked its own temp file");

            // Release the whole-track producer last: publishing it prunes the
            // matching windows, which would erase the evidence above.
            source.whole_release.add_permits(4);
            crate::subtitles::release_session_window(session_id).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }

        /// Fact 3, first half: what admission hands the activation.
        ///
        /// This half is cheap and shallow on purpose — it pins that admission
        /// names the predecessor it saw and asks for a compare-and-swap rather
        /// than a last-writer-wins overwrite. The half that matters, that the
        /// durable pointer actually refuses the loser, is the test below it.
        #[test]
        fn an_admitted_successor_fences_the_predecessor_it_replaces() {
            let admitted = admit_restart(
                Some(crate::playback_control::SettledTarget {
                    sequence: 4,
                    anchor_ms: 1_800_000,
                }),
                Some(9),
                1_800.0,
                Some("incarnation-4"),
            )
            .expect("a create carrying the newest sequence is admitted");
            assert!(admitted.fence_predecessor);
            assert_eq!(
                admitted.expected_predecessor_incarnation_id.as_deref(),
                Some("incarnation-4")
            );

            let first_play = admit_restart(None, Some(1), 0.0, None)
                .expect("a first play has no ordering to be stale against");
            assert!(
                first_play.fence_predecessor,
                "an ordinary start is a CAS against the pointer being absent"
            );
            assert_eq!(first_play.expected_predecessor_incarnation_id, None);
        }

        /// Fact 3, second half: the fence admission asks for is real.
        ///
        /// `fence_predecessor` is a constant in the code, so asserting on it
        /// proves only that the constant is still there. What the storm
        /// actually depends on is that the durable pointer honours it — that a
        /// losing restart naming the predecessor cannot take the pointer back
        /// once the winner has moved it. That is a Store contract, so it is
        /// asserted against the Store.
        #[tokio::test]
        async fn the_pointer_refuses_a_loser_that_still_names_the_old_incarnation() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let predecessor = uuid::Uuid::new_v4().to_string();
            let fixture = HlsDeliveryFixture::publish(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &predecessor,
                "test-node",
            )
            .await;

            let winner = uuid::Uuid::new_v4().to_string();
            let admitted = admit_restart(None, Some(9), 1_800.0, Some(predecessor.as_str()))
                .expect("the newest sequence is admitted");
            let fingerprint = "b".repeat(64);
            let claim = |incarnation: String, fingerprint: String| {
                let store = Arc::clone(&fixture.store);
                async move {
                    let now_ms = crate::media_sessions::unix_ms();
                    store
                        .claim_media_session_request(
                            7,
                            &incarnation,
                            &fingerprint,
                            "player-control",
                            &incarnation,
                            now_ms,
                            now_ms + 60_000,
                        )
                        .await
                        .expect("claim the restart's request");
                    assert!(store
                        .assign_media_session_request_owner(
                            7,
                            &incarnation,
                            &incarnation,
                            "test-node",
                            now_ms,
                        )
                        .await
                        .expect("assign the request owner"));
                }
            };
            let activation = |incarnation: &str, admitted: &RestartAdmission| {
                let now_ms = crate::media_sessions::unix_ms();
                plurx_core::domain::MediaSessionActivation {
                    incarnation_id: incarnation.to_owned(),
                    // A restart is a new session taking over one playback, so
                    // the session id moves and the playback id is what the
                    // pointer is keyed by.
                    session_id: uuid::Uuid::new_v4().to_string(),
                    user_id: 7,
                    playback_id: "player-control".to_owned(),
                    expected_predecessor_incarnation_id: admitted
                        .expected_predecessor_incarnation_id
                        .clone(),
                    fence_predecessor: admitted.fence_predecessor,
                    request_id: Some(incarnation.to_owned()),
                    request_fingerprint: fingerprint.clone(),
                    owner_node_id: "test-node".to_owned(),
                    recipe_json: "{}".to_owned(),
                    response_json: "{}".to_owned(),
                    publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                    media_origin_ms: 0,
                    now_ms,
                    lease_expires_at_ms: now_ms + 60_000,
                }
            };

            claim(winner.clone(), fingerprint.clone()).await;
            assert!(
                fixture
                    .store
                    .activate_media_session(&activation(&winner, &admitted))
                    .await
                    .expect("winner activation")
                    .is_some(),
                "the winner names the incarnation the pointer holds, so its CAS succeeds"
            );

            // The loser was admitted against the same predecessor — it lost the
            // race, not the ordering check — and still asks to take the pointer
            // from an incarnation that no longer holds it.
            let loser = uuid::Uuid::new_v4().to_string();
            claim(loser.clone(), fingerprint.clone()).await;
            assert!(
                fixture
                    .store
                    .activate_media_session(&activation(&loser, &admitted))
                    .await
                    .expect("loser activation")
                    .is_none(),
                "a storm's loser cannot take the pointer back from the successor"
            );
        }

        /// The ownership latch's own arms, driven directly.
        ///
        /// Through the handler these are unreachable: the settled-destination
        /// gate answers first, so a request for a different anchor is refused
        /// before the latch ever sees it. The latch still has to be right —
        /// the two guards mask each other, and a bug in either would hide
        /// behind the other — so this drives `warm_vtt_window_with` itself.
        #[tokio::test]
        async fn the_owner_latch_joins_refuses_and_displaces_by_authority() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let started = Arc::new(tokio::sync::Semaphore::new(0));
            let hold = Arc::new(tokio::sync::Semaphore::new(0));

            let warm = |sequence: Option<u64>, anchor: i64| {
                let runs = Arc::clone(&runs);
                let started = Arc::clone(&started);
                let hold = Arc::clone(&hold);
                let subs_dir = fixture.state.subs_dir.clone();
                let file = file.clone();
                let session_id = session_id.clone();
                async move {
                    crate::subtitles::warm_vtt_window_with(
                        &session_id,
                        sequence,
                        &subs_dir,
                        &file,
                        0,
                        anchor,
                        WINDOW_SECONDS,
                        move |_tmp, _, _, _, _| async move {
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            started.add_permits(1);
                            hold.acquire().await.expect("hold").forget();
                            Ok(())
                        },
                    )
                    .await
                }
            };
            let count = || runs.load(std::sync::atomic::Ordering::SeqCst);
            let running = || {
                let started = Arc::clone(&started);
                async move {
                    tokio::time::timeout(Duration::from_secs(5), started.acquire())
                        .await
                        .expect("a producer starts")
                        .expect("started semaphore remains open")
                        .forget();
                }
            };

            assert!(warm(Some(5), 600).await, "a first claim starts a flight");
            running().await;
            assert_eq!(count(), 1);

            assert!(
                warm(Some(5), 600).await,
                "the same destination joins the flight already running"
            );
            assert_eq!(count(), 1, "and never spawns a second extractor for it");

            assert!(
                !warm(Some(5), 900).await,
                "an equal sequence is not a newer ordering fact"
            );
            assert!(
                !warm(Some(4), 900).await,
                "and an older one is evidence in the wrong direction"
            );
            assert!(
                !warm(None, 900).await,
                "nor does the absence of one displace a flight that has it"
            );
            assert_eq!(count(), 1, "none of those started anything");
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the flight is kept")
                    .0,
                600
            );

            assert!(warm(Some(6), 900).await, "a newer sequence displaces");
            running().await;
            assert_eq!(count(), 2);
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the successor owns the slot")
                    .0,
                900
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1,
                "the predecessor settled before the successor started"
            );

            // Session end fences the id: a request already on its way cannot
            // leave an extraction behind a playback that is gone.
            crate::subtitles::release_session_window(session_id).await;
            assert!(crate::subtitles::owned_window_for_test(session_id).is_none());
            assert!(
                !warm(Some(9), 1_200).await,
                "a released session starts nothing, however new its authority"
            );
            assert_eq!(count(), 2);

            crate::subtitles::clear_release_fence_for_test(session_id);
            assert!(
                warm(Some(9), 1_200).await,
                "and gets its bridge back once the fence lapses"
            );
            running().await;
            assert_eq!(count(), 3);

            hold.add_permits(8);
            crate::subtitles::release_session_window(session_id).await;
        }

        /// A first play with no authority is displaced by the first request
        /// that has one — a settled destination is a fact, and the anchor a
        /// session happened to open on is not.
        #[tokio::test]
        async fn an_ordering_fact_displaces_a_flight_started_without_one() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hold = Arc::new(tokio::sync::Semaphore::new(0));
            let warm = |sequence: Option<u64>, anchor: i64| {
                let runs = Arc::clone(&runs);
                let hold = Arc::clone(&hold);
                let subs_dir = fixture.state.subs_dir.clone();
                let file = file.clone();
                let session_id = session_id.clone();
                async move {
                    crate::subtitles::warm_vtt_window_with(
                        &session_id,
                        sequence,
                        &subs_dir,
                        &file,
                        0,
                        anchor,
                        WINDOW_SECONDS,
                        move |_tmp, _, _, _, _| async move {
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            hold.acquire().await.expect("hold").forget();
                            Ok(())
                        },
                    )
                    .await
                }
            };

            assert!(warm(None, 300).await);
            assert!(warm(Some(1), 600).await, "an authority outranks none");
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the successor owns the slot")
                    .0,
                600
            );
            assert_eq!(
                crate::subtitles::peak_window_flights_for_test(session_id),
                1
            );

            hold.add_permits(4);
            crate::subtitles::release_session_window(session_id).await;
        }

        /// Terminal retirement releases the owner through the production path,
        /// not through a test calling the release function by hand.
        #[tokio::test]
        async fn rolling_retirement_releases_the_window_a_playback_owned() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, _file) = cold_windowed_fixture(dir.path(), session_id).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            let response = subtitle_segment(&fixture.state, session_id, 3, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;
            assert!(crate::subtitles::owned_window_for_test(session_id).is_some());

            assert!(
                fixture
                    .state
                    .transcode
                    .stop_session(session_id, "admin")
                    .await,
                "the fixture session retires"
            );
            assert!(
                crate::subtitles::owned_window_for_test(session_id).is_none(),
                "terminal retirement released the window owner"
            );
            assert_eq!(
                source.live_window_producers(),
                0,
                "and waited for its extraction to settle"
            );

            source.whole_release.add_permits(4);
        }

        /// The coverage rule, at its edges.
        ///
        /// A window is refused when the viewer has left it behind or when it is
        /// too far ahead to be a buffer head. Between those, including the
        /// window immediately ahead of the playhead, it is exactly the bridge
        /// this feature exists to build.
        #[test]
        fn coverage_admits_the_look_ahead_and_refuses_both_ends() {
            let settled = crate::playback_control::SettledTarget {
                sequence: 4,
                anchor_ms: 190_000,
            };
            assert!(
                settled.covered_by_window(0, 200),
                "the window holding the playhead"
            );
            assert!(
                settled.covered_by_window(200, 200),
                "and the one immediately ahead of it, which is what a player is fetching"
            );
            assert!(
                !settled.covered_by_window(1_800, 200),
                "a window half an hour ahead is a destination the viewer left, not a buffer"
            );
            assert!(
                !settled.covered_by_window(0, 30),
                "a short span behind the playhead has nothing left to give it"
            );
            assert!(
                settled.covered_by_window(180, 30),
                "while the short span holding it still does"
            );
            assert!(
                settled.covered_by_window(300, 30),
                "and a short span keeps the measured client lead, not its own length"
            );
            let head = crate::playback_control::SettledTarget {
                sequence: 1,
                anchor_ms: 0,
            };
            assert!(head.covered_by_window(0, 200), "a first play at the head");
        }

        /// Fact 7. `None` is the absence of an ordering fact, not evidence of
        /// staleness, so a client that has never exchanged still gets its
        /// bridge.
        #[tokio::test]
        async fn a_first_play_with_no_settled_target_still_warms() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());
            assert!(
                fixture
                    .state
                    .transcode
                    .settled_target_for_session(session_id)
                    .await
                    .is_none(),
                "the fixture session has never exchanged"
            );

            let response = subtitle_segment(&fixture.state, session_id, 2, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            producer_started(source.as_ref()).await;
            assert_eq!(source.window_runs(), 1);
            let owned = crate::subtitles::owned_window_for_test(session_id)
                .expect("the first play owns its flight");
            assert_eq!(owned.2, None, "with no ordering fact behind it");

            source.release.add_permits(2);
            source.whole_release.add_permits(2);
            crate::subtitles::release_session_window(session_id).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }

        /// The deterministic core: three anchors traversed, a same-anchor
        /// request joining rather than spawning, a different anchor with no
        /// newer authority refused, and the slot gone at session end.
        #[tokio::test]
        async fn one_session_traverses_anchors_joins_refuses_and_releases() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client = uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &generation,
                "test-node",
            )
            .await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());
            let window_seconds = fixture.state.subtitle_window_seconds().await;

            let mut sequence = 0;
            for segment in [1_i64, 4, 9] {
                sequence += 1;
                settle_on(
                    &fixture,
                    session_id,
                    &generation,
                    &client,
                    sequence,
                    segment * SEGMENT_SECONDS,
                )
                .await
                .expect("accepted exchange");
                let response =
                    subtitle_segment(&fixture.state, session_id, segment, source.as_ref()).await;
                assert_eq!(response.status(), StatusCode::OK);
                producer_started(source.as_ref()).await;
                let owned = crate::subtitles::owned_window_for_test(session_id)
                    .expect("the current destination is owned");
                assert_eq!(owned.0, segment * SEGMENT_SECONDS);
                assert_eq!(source.live_window_producers(), 1);
                assert_eq!(source.window_runs() as u64, sequence);
                tokio::time::sleep(Duration::from_millis(260)).await;
            }

            // Same anchor, same destination: joined, never respawned.
            let response = subtitle_segment(&fixture.state, session_id, 9, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                source.window_runs(),
                3,
                "a second request for the live destination joins it"
            );

            // A different anchor with no newer exchange behind it: the live
            // flight is not evidence-free to kill, so nothing starts.
            let response = subtitle_segment(&fixture.state, session_id, 2, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                source.window_runs(),
                3,
                "a different anchor without newer authority starts nothing"
            );
            assert_eq!(
                crate::subtitles::owned_window_for_test(session_id)
                    .expect("the live flight is kept")
                    .0,
                9 * SEGMENT_SECONDS
            );

            // Session end releases the owner, and does it by waiting for the
            // real producer rather than by dropping a handle.
            crate::subtitles::release_session_window(session_id).await;
            assert!(crate::subtitles::owned_window_for_test(session_id).is_none());
            assert_eq!(
                source.live_window_producers(),
                0,
                "release waits for the extraction to settle"
            );
            assert!(
                matches!(
                    crate::subtitles::read_cached_window(
                        &fixture.state.subs_dir,
                        &file,
                        0,
                        9 * SEGMENT_SECONDS,
                        window_seconds,
                    )
                    .await,
                    Ok(None)
                ),
                "a released flight publishes nothing"
            );

            source.whole_release.add_permits(2);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }

        /// The window a settled client cannot reach is never started at all —
        /// the refusal happens in the handler, before the owner is consulted.
        #[tokio::test]
        async fn a_window_that_misses_the_settled_target_starts_nothing() {
            let dir = crate::test_tempdir().expect("session directory");
            let session_id = &uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            let client = uuid::Uuid::new_v4().to_string();
            let (fixture, file) = cold_windowed_fixture(dir.path(), session_id).await;
            crate::transcode::tests::activate_control_route(
                fixture.store.as_ref(),
                session_id,
                &generation,
                "test-node",
            )
            .await;
            let source = Arc::new(WindowFixtureSubtitleSource::counting());

            settle_on(
                &fixture,
                session_id,
                &generation,
                &client,
                1,
                12 * SEGMENT_SECONDS,
            )
            .await
            .expect("accepted exchange");
            let response = subtitle_segment(&fixture.state, session_id, 1, source.as_ref()).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = response
                .into_body()
                .collect()
                .await
                .expect("empty VTT body")
                .to_bytes();
            assert_eq!(
                body.as_ref(),
                b"WEBVTT\n\n",
                "the unchanged empty fallback still answers"
            );
            assert_eq!(
                source.window_runs(),
                0,
                "no window is started for a destination the client already left"
            );
            assert!(crate::subtitles::owned_window_for_test(session_id).is_none());

            source.whole_release.add_permits(2);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    crate::subtitles::read_cached_vtt(&fixture.state.subs_dir, &file, 0).await,
                    Ok(Some(_))
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the whole-track producer settles before cleanup");
        }
    }

    #[tokio::test]
    async fn real_subtitle_playlist_rebinds_after_video_attempt_handoff() {
        let dir = crate::test_tempdir().expect("session directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "subtitle-handoff").await;
        add_http_text_subtitle(&mut fixture, "subtitle-handoff").await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture
            .state
            .transcode
            .set_subtitle_playlist_commit_pause(Arc::clone(&pause));
        let owner_pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture.pause_playlist_publication(Arc::clone(&owner_pause));
        tokio::fs::write(dir.path().join("seg00000.ts"), b"old-zero")
            .await
            .expect("predecessor segment zero");
        tokio::fs::write(dir.path().join("seg00001.ts"), b"old-one")
            .await
            .expect("predecessor segment one");
        tokio::fs::write(
            dir.path().join("index.m3u8"),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000,\nseg00000.ts\n#EXTINF:2.000,\nseg00001.ts\n",
        )
        .await
        .expect("predecessor playlist");
        let state = fixture.state.clone();
        let waiting =
            tokio::spawn(
                async move { subtitle_playlist_local(&state, "subtitle-handoff", 0).await },
            );
        tokio::time::timeout(Duration::from_secs(5), owner_pause.wait())
            .await
            .expect("subtitle request read predecessor playlist");

        assert_eq!(fixture.begin_producer_attempt().await, Ok(1));
        tokio::fs::write(dir.path().join("seg00000.ts"), b"zero")
            .await
            .expect("segment zero");
        tokio::fs::write(dir.path().join("seg00001.ts"), b"one")
            .await
            .expect("segment one");
        tokio::fs::write(dir.path().join("seg00002.ts"), b"two")
            .await
            .expect("segment two");
        tokio::fs::write(
            dir.path().join("index.m3u8"),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000,\nseg00000.ts\n#EXTINF:2.000,\nseg00001.ts\n#EXTINF:2.000,\nseg00002.ts\n",
        )
        .await
        .expect("successor playlist");
        tokio::time::timeout(Duration::from_secs(5), owner_pause.wait())
            .await
            .expect("release predecessor playlist publication");
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("subtitle response reached the exact-owner commit seam");
        assert!(
            !waiting.is_finished(),
            "subtitle response reached the exact-owner commit seam"
        );
        tokio::time::timeout(Duration::from_secs(5), pause.wait())
            .await
            .expect("release subtitle response commit");
        let response = waiting
            .await
            .expect("subtitle task")
            .expect("subtitle response commits against successor owner");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("subtitle playlist body");
        let text = String::from_utf8(body.to_vec()).expect("subtitle playlist text");
        assert!(
            text.lines().any(|line| line == "seg00002.vtt"),
            "subtitle child playlist must rebind to successor-relative segment URIs: {text}"
        );
        assert_eq!(fixture.last_renewal_kind().await, "subtitle-playlist");
    }

    #[tokio::test]
    async fn real_subtitle_playlist_cannot_commit_after_vod_same_id_reattachment() {
        let dir = crate::test_tempdir().expect("VOD subtitle directory");
        let mut fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        add_http_text_subtitle(&mut fixture, "rolling-unused").await;
        let session_id = "vod-subtitle-replaced";
        let _predecessor = install_vod_http_session(&fixture, dir.path(), session_id).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture
            .state
            .transcode
            .set_subtitle_playlist_commit_pause(Arc::clone(&pause));
        let state = fixture.state.clone();
        let pending =
            tokio::spawn(async move { subtitle_playlist_local(&state, session_id, 0).await });
        pause.wait().await;

        let _successor = install_vod_http_session(&fixture, dir.path(), session_id).await;
        let successor_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(session_id)
            .await
            .expect("successor touch");
        pause.wait().await;

        assert!(
            pending.await.expect("subtitle task").is_err(),
            "predecessor bytes must fail their exact-owner commit"
        );
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(session_id)
                .await,
            Some(successor_touch),
            "stale subtitle bytes cannot renew the same-id successor"
        );
    }

    /// A route whose durable recipe the takeover path accepts. The fixture is
    /// `media_sessions`' own, so "eligible" cannot mean two things.
    fn eligible_owner_loss_route() -> MediaSessionRoute {
        let mut route = crate::media_sessions::takeover_eligible_route(
            "00000000-0000-4000-8000-0000000000e2",
            "00000000-0000-4000-8000-0000000000e1",
        );
        route.owner_node_id = "node-gone".to_owned();
        route.owner_epoch = 3;
        route.lease_expires_at_ms = NOW_MS;
        route.updated_at_ms = NOW_MS;
        route.media_origin_ms = 90_000;
        route.fetched_through_ms = 60_000;
        route.produced_playable_through_ms = 120_000;
        route
    }

    /// The same route with a recipe nothing will ever take over.
    fn untakeoverable_owner_loss_route() -> MediaSessionRoute {
        let mut route = eligible_owner_loss_route();
        route.recipe_json = route
            .recipe_json
            .replace("\"typeless_playlist\":true", "\"typeless_playlist\":false");
        assert!(
            route.recipe_json.contains("\"typeless_playlist\":false"),
            "the EVENT fixture must actually clear the flag"
        );
        route
    }

    async fn error_body(error: ApiError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let http_status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect error body")
            .to_bytes();
        (
            http_status,
            serde_json::from_slice(&bytes).expect("typed errors are JSON objects"),
        )
    }

    /// A route a successor can still take over keeps the retryable 503 it has
    /// always had — but it now says where to reopen if the client stops
    /// waiting, and that recovery carries a discontinuity either way.
    #[tokio::test]
    async fn an_eligible_route_stays_retryable_and_still_says_where_to_reopen() {
        let route = eligible_owner_loss_route();
        let (status, body) = error_body(owner_transition_answer(&route)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "media_owner_transition");
        assert_eq!(
            body["reopen_required"], false,
            "a client that is willing to wait should keep waiting"
        );
        assert_eq!(body["continuous"], false);
        assert!(body["film_position_ms"].is_i64());
    }

    /// Hard owner loss the takeover path can never answer (plan §10.3). The
    /// client is told the session is gone rather than to wait for an event
    /// that cannot happen, and is handed where to reopen.
    #[tokio::test]
    async fn an_untakeoverable_route_answers_gone_with_the_resume_and_no_continuity_claim() {
        // The EVENT case: nothing will ever replace this session.
        let route = untakeoverable_owner_loss_route();
        let (status, body) = error_body(owner_transition_answer(&route)).await;

        assert_eq!(
            status,
            StatusCode::GONE,
            "a session no node will serve again is gone, not temporarily unavailable"
        );
        assert_eq!(body["code"], "media_owner_lost");
        assert_eq!(body["reopen_required"], true);
        assert_eq!(
            body["continuous"], false,
            "§10.3 forbids guessing transparency, and the client reads the field, not the prose"
        );
        // 60 s fetched, pulled back by one transcode segment plus the
        // overlap margin, on a session whose origin is 90 s into the film.
        let overlap_ms = i64::from(plurx_core::transcode::SEGMENT_SECONDS) * 1_000 + 2_000;
        assert_eq!(body["film_position_ms"], 90_000 + 60_000 - overlap_ms);
        assert_eq!(body["film_frontier_ms"], 90_000 + 120_000);
    }

    /// The wiring, not the helper. Every public media path that answers an
    /// owner transition must go through the classifier — a call site left on
    /// the old unconditional 503 would make this fail while every unit test
    /// on the helper still passed.
    #[tokio::test]
    async fn a_public_status_read_of_an_untakeoverable_route_answers_gone() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "owner-loss-unrelated").await;
        let user = fixture
            .store
            .create_user("owner-loss", "hash", false)
            .await
            .expect("owner-loss user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id: incarnation_id.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "owner-loss".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "former-owner".to_owned(),
                // Active, but outside its owner lease: routing calls this a
                // transition, and the recipe decides which kind.
                lease_expires_at_ms: 2,
                // An EVENT session — refused by `attempt_takeover` forever.
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: 1,
            },
        )
        .await;

        let Err(answer) = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await
        else {
            panic!("an expired active route is not local status absence")
        };
        assert!(
            matches!(
                answer,
                ApiError::TypedDetail {
                    status: StatusCode::GONE,
                    code: "media_owner_lost",
                    ..
                }
            ),
            "the public status path must classify rather than always answer 503"
        );
    }

    /// The EVENT half of §4's non-expansion rule, driven through the function
    /// that owns it rather than through the flag it reads.
    #[tokio::test]
    async fn attempt_takeover_refuses_an_event_session() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "event-refusal-unrelated").await;
        let mut route = untakeoverable_owner_loss_route();
        route.owner_node_id = "some-other-node".to_owned();

        let refusal = crate::media_sessions::attempt_takeover_for_test(&fixture.state, route)
            .await
            .expect_err("an EVENT session is never taken over");
        assert!(
            refusal.contains("EVENT playlist"),
            "the EVENT gate must fire before any later refusal: {refusal}"
        );
    }

    async fn control_body(response: Response) -> (StatusCode, serde_json::Value) {
        let http_status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect control body")
            .to_bytes();
        (
            http_status,
            serde_json::from_slice(&bytes).expect("control errors are JSON objects"),
        )
    }

    fn control_request(generation: String) -> crate::playback_control::ControlRequestV1 {
        crate::playback_control::ControlRequestV1 {
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation,
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: crate::playback_control::PlaybackDemand::Active,
            position_ms: 1_000,
            buffered_from_ms: Some(0),
            buffered_through_ms: 10_000,
            playback_rate: 1.0,
            render_state: crate::playback_control::RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: crate::playback_control::ClientSelection {
                quality: crate::playback_control::QualitySelection::Auto,
                audio_track: None,
                subtitle: crate::playback_control::SubtitleSelection {
                    mode: crate::playback_control::SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: crate::playback_control::CodecPolicy::Auto,
                dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
            },
            capabilities: Some(crate::playback_control::DynamicCapabilities {
                platform: crate::playback_control::ClientPlatform::Web,
                max_height: 1080,
                codecs: vec![crate::playback_control::CodecPolicy::H264],
                dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
            supported_actions: None,
        }
    }

    /// A live local worker and a durable route naming it — both halves are
    /// needed, because staging takes the actor's slot *and* writes a row
    /// fenced on the route's current generation.
    async fn staging_fixture(
        dir: &std::path::Path,
    ) -> (HlsDeliveryFixture, String, MediaSessionRoute) {
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir, &session_id).await;
        let user = fixture
            .store
            .create_user("stage-on-prepare", "hash", false)
            .await
            .expect("staging user");
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let predecessor_request = crate::transcode::SessionRequest {
            control_sequence: None,
            file_id: fixture.file_id(),
            playback_id: "stage-player".to_owned(),
            request_id: Some(incarnation_id.clone()),
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: crate::transcode::SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
        };
        let predecessor_recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation_id.clone(),
            user_id: user.id,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: false,
            request: predecessor_request.clone(),
        };
        let predecessor_start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(6_000_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 2160,
            encoder: "test".to_owned(),
            vod: false,
            ladder: Vec::new(),
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &incarnation_id,
                1,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let route = activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id,
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "stage-player".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: predecessor_request.durable_intent_fingerprint(user.id),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: unix_ms().saturating_add(900_000),
                recipe_json: serde_json::to_string(&predecessor_recipe).expect("recipe"),
                response_json: serde_json::to_string(&predecessor_start).expect("response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: unix_ms(),
            },
        )
        .await;
        (fixture, session_id, route)
    }

    /// M6 §3.4's acceptance, stated in the handoff: *"with a test client
    /// reporting `dual_player_preparation: true` on a resolution-and-delivery-
    /// method change with headroom, the ledger holds a staged successor and
    /// the pointer still names the predecessor."*
    ///
    /// Both halves matter and the second is the one that would go wrong
    /// quietly. `activate_media_session` — the call every other durable
    /// successor in this codebase goes through — reaps the predecessor and
    /// advances the pointer unconditionally, so a preparation built on it
    /// would retire the stream the viewer is still watching. This asserts the
    /// pointer is untouched, which is what says `prepare_media_session` is the
    /// entry point actually being used.
    #[tokio::test]
    async fn a_prepared_transition_stages_a_successor_and_leaves_the_pointer_alone() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;

        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        assert_eq!(
            staged.expected_predecessor_incarnation_id, route.incarnation_id,
            "the commit CAS is fenced on the generation that was current when it staged"
        );
        assert_ne!(staged.staged_incarnation_id, route.incarnation_id);

        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("route while staged")
                .expect("the playback still has a pointer")
                .incarnation_id,
            route.incarnation_id,
            "staging must not advance the pointer — `activate_media_session` would have",
        );
    }

    /// M6 §3.4c's acceptance at the actual control seam. The stage happens
    /// outside the exchange that requested it; the next exchange reads the
    /// durable row, has the same owner-local slot authorize it, and returns a
    /// transaction whose identity survives an exact replay.
    #[tokio::test]
    async fn a_staged_successor_is_announced_without_moving_the_pointer_and_replays_exactly() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        let staged_route = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged.staged_incarnation_id)
            .await
            .expect("staged route read")
            .expect("the ledger names a route");

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let first = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("control response");
        assert_eq!(
            first.delivery.presentation, "vod",
            "the acceptance must run through the engine public sessions use",
        );
        let crate::playback_control::ControlAction::Prepare {
            action_id,
            session_id: announced_session_id,
            playlist_url,
            media_origin_ms,
            effective_selection,
        } = &first.action
        else {
            panic!("the next exchange must announce the staged successor");
        };
        assert!(uuid::Uuid::parse_str(action_id).is_ok());
        assert_eq!(announced_session_id, &staged_route.session_id);
        assert_eq!(media_origin_ms, &staged_route.media_origin_ms);
        let start = control_start_response(&staged_route).expect("staged response");
        let recipe = serde_json::from_str::<RemoteStartRequest>(&staged_route.recipe_json)
            .expect("staged recipe");
        assert_eq!(playlist_url, &start.playlist_url);
        assert_eq!(
            effective_selection,
            &crate::playback_control::EffectiveSelection::from_recipe(
                &recipe,
                start.height,
                start.delivered_dynamic_range,
            ),
        );
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("the predecessor remains current")
                .incarnation_id,
            route.incarnation_id,
            "announcing a preparation must not advance the pointer",
        );

        let mut replay_request = request;
        replay_request.supported_actions = None;
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                replay_request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");
    }

    #[tokio::test]
    async fn a_local_prepare_rejects_an_unsafe_durable_playlist_url() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        let gate = fixture
            .state
            .transcode
            .session_preparation_gate(&session_id)
            .await
            .expect("live predecessor gate");
        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        let staged_session_id = uuid::Uuid::new_v4().to_string();
        let mut staged_request = staged_candidate_request();
        staged_request.file_id = fixture.file_id();
        staged_request.request_id = Some(staged_incarnation_id.clone());
        let response = StartResponse {
            session_id: staged_session_id.clone(),
            playlist_url: format!("//attacker.invalid/{staged_session_id}/index.m3u8"),
            duration_ms: Some(6_000_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 1080,
            encoder: "staged".to_owned(),
            vod: true,
            ladder: Vec::new(),
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &staged_session_id,
                &staged_incarnation_id,
                1,
                crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        let preparation = plurx_core::domain::MediaSessionPreparation {
            incarnation_id: staged_incarnation_id.clone(),
            session_id: staged_session_id,
            user_id: route.user_id,
            playback_id: route.playback_id.clone(),
            expected_predecessor_incarnation_id: route.incarnation_id.clone(),
            request_fingerprint: staged_request.durable_intent_fingerprint(route.user_id),
            owner_node_id: fixture.state.node_id.clone(),
            recipe_json: serde_json::to_string(&RemoteStartRequest {
                protocol_version: crate::media_pool::PROTOCOL_VERSION,
                incarnation_id: staged_incarnation_id,
                user_id: route.user_id,
                source_size: 1,
                source_mtime: 1,
                typeless_playlist: false,
                request: staged_request,
            })
            .expect("recipe"),
            response_json: serde_json::to_string(&response).expect("response"),
            media_origin_ms: 0,
            now_ms,
            deadline_ms: now_ms.saturating_add(30_000),
        };
        let executor = crate::playback_control::PreparationExecutor::new(
            Arc::clone(&fixture.state.store),
            gate,
            route.user_id,
            route.playback_id.clone(),
        );
        assert!(executor
            .stage(&preparation)
            .await
            .expect("stage hostile row"));

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");
    }

    #[tokio::test]
    async fn a_transient_staged_read_failure_does_not_advance_or_rotate_the_action() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let first = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("first response");
        assert!(matches!(
            first.action,
            crate::playback_control::ControlAction::Prepare { .. }
        ));

        fail_next_staged_read(&route.incarnation_id);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let replay = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("cached exact replay");
        assert_eq!(replay.accepted_sequence, first.accepted_sequence);
        assert_eq!(replay.action, first.action);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        fail_next_staged_read(&route.incarnation_id);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");

        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let recovered = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("recovered response");
        assert_eq!(recovered.accepted_sequence, 2);
        assert_eq!(recovered.action, first.action);
    }

    #[tokio::test]
    async fn a_client_without_prepare_vocabulary_skips_the_staged_store_read() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;

        let mut request = control_request(route.incarnation_id.clone());
        fail_next_staged_read(&route.incarnation_id);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let passive = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("passive response");
        assert_eq!(passive.action, crate::playback_control::ControlAction::None);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");

        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let prepared = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("prepared response");
        assert!(matches!(
            prepared.action,
            crate::playback_control::ControlAction::Prepare { .. }
        ));
    }

    /// The detached decision boundary stages an admitted candidate and leaves
    /// the predecessor pointer alone. The production control-to-boundary
    /// wiring is covered separately below.
    #[tokio::test]
    async fn an_admitted_preparation_candidate_reaches_the_ledger() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        // The seam reads the source, and staging refuses without a snapshot.
        // Its 2160 height is also load-bearing: it is what makes the viewer's
        // 1080 ask cross the delivery method as well as the resolution.
        let source = staging_source(&fixture).await;

        // A 2160p copy being delivered, and the viewer asks for 1080p: the
        // copy becomes a transcode, so height and delivery method move
        // together — the one pair with a hardware receipt.
        let mut recipe = staged_candidate_request();
        recipe.file_id = source.id;
        recipe.kind = crate::transcode::SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        // 2160 is what is being *delivered*; the source row's own height is
        // deliberately not required. `candidate_request` turns a copy into a
        // transcode whenever the asked height is not the source's, and an
        // unprobed source is `None`, so the copy → transcode arm fires either
        // way — which is the delivery-method half of the transition.
        let delivered =
            crate::playback_control::EffectiveSelection::from_request(&recipe, 2160, None);
        assert_eq!(delivered.codec, "source", "the predecessor direct-plays");
        let asked_height = 1080;

        process_preparation_candidate(
            fixture.state.clone(),
            PreparationCandidateInputs {
                session_id: session_id.clone(),
                route: route.clone(),
                recipe: RemoteStartRequest {
                    protocol_version: crate::media_pool::PROTOCOL_VERSION,
                    incarnation_id: route.incarnation_id.clone(),
                    user_id: route.user_id,
                    source_size: 0,
                    source_mtime: 0,
                    typeless_playlist: false,
                    request: recipe,
                },
                selection: crate::playback_control::ClientSelection {
                    quality: crate::playback_control::QualitySelection::Manual {
                        height: asked_height,
                    },
                    audio_track: None,
                    subtitle: crate::playback_control::SubtitleSelection {
                        mode: crate::playback_control::SubtitleMode::Off,
                        track: None,
                    },
                    audio_offset_ms: 0,
                    codec: crate::playback_control::CodecPolicy::Auto,
                    dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
                },
                // Ten times the delivered rate: well clear of the floor the
                // 2026-09-03 run had to be re-run to satisfy.
                observed_download_bps: Some(100_000_000),
                delivered: delivered.clone(),
                delivered_bps: Some(10_000_000),
                capabilities: Some(crate::playback_control::DynamicCapabilities {
                    platform: crate::playback_control::ClientPlatform::Apple,
                    max_height: 2160,
                    codecs: vec![crate::playback_control::CodecPolicy::H264],
                    dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                    // The retained field, read rather than re-derived.
                    dual_player_preparation: true,
                }),
                platform: crate::playback_control::ClientPlatform::Apple,
            },
        )
        .await;

        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("the seam staged a successor");
        assert_eq!(
            staged.expected_predecessor_incarnation_id,
            route.incarnation_id
        );
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("route while staged")
                .expect("the playback still has a pointer")
                .incarnation_id,
            route.incarnation_id,
            "and the viewer's own generation is still the current one",
        );
    }

    /// The complete production seam: an accepted selection change schedules
    /// the detached stage, and the next accepted sequence announces the row.
    /// Capabilities are omitted after sequence one exactly as shipped clients
    /// do, proving the actor-retained document is the one used.
    #[tokio::test]
    async fn an_accepted_selection_change_is_staged_and_announced_on_the_next_exchange() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut first = control_request(route.incarnation_id.clone());
        first.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        first.observed_download_bps = Some(100_000_000);
        first.capabilities = Some(crate::playback_control::DynamicCapabilities {
            platform: crate::playback_control::ClientPlatform::Web,
            max_height: 2160,
            codecs: vec![crate::playback_control::CodecPolicy::H264],
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dual_player_preparation: true,
        });
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                first.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        let mut changed = first.clone();
        changed.sequence = 2;
        changed.capabilities = None;
        changed.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                changed.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let changed_response =
            serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
                .expect("selection-change response");
        assert_eq!(
            changed_response.action,
            crate::playback_control::ControlAction::None
        );

        let staged = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(staged) = fixture
                    .state
                    .store
                    .staged_media_session_for_playback(route.user_id, &route.playback_id)
                    .await
                    .expect("ledger read")
                {
                    break staged;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached production seam staged the successor");

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        changed.sequence = 3;
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                changed,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let announced = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("announcement response");
        let staged_route = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged.staged_incarnation_id)
            .await
            .expect("staged route read")
            .expect("staged route");
        assert!(matches!(
            announced.action,
            crate::playback_control::ControlAction::Prepare { ref session_id, .. }
                if staged_route.session_id == *session_id
        ));
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("predecessor remains current")
                .incarnation_id,
            route.incarnation_id,
        );
    }

    /// The production seam with a client that cannot safely hold two players.
    /// Nothing stages, which proves the retained capability reaches the
    /// detached decision rather than being re-derived from the platform.
    #[tokio::test]
    async fn a_client_that_cannot_prepare_stages_nothing_at_the_seam() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut first = control_request(route.incarnation_id.clone());
        first.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        first.observed_download_bps = Some(100_000_000);
        first.capabilities = Some(crate::playback_control::DynamicCapabilities {
            platform: crate::playback_control::ClientPlatform::Web,
            max_height: 2160,
            codecs: vec![crate::playback_control::CodecPolicy::H264],
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dual_player_preparation: false,
        });
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                first.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        first.sequence = 2;
        first.capabilities = None;
        first.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                first,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // The candidate runs detached. Wait for the completion marker keyed
        // to this exact predecessor, rather than inferring completion from a
        // process-global in-flight count that may still be zero before the
        // spawned future receives its first poll.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if take_preparation_candidate_completion(&route.incarnation_id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached preparation decision completed");

        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "a client that cannot hold two pipelines is not given a successor"
        );
    }

    /// A second `Prepare` on the same playback finds the actor's one slot
    /// already taken. It is refused rather than queued, and refusing is
    /// counted separately from deciding, because the two answer different
    /// questions and only the second says whether the ledger moved.
    #[tokio::test]
    async fn a_second_prepared_transition_is_refused_by_the_one_successor_slot() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        let first = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("the first staged")
            .staged_incarnation_id;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        assert_eq!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read")
                .expect("still exactly one")
                .staged_incarnation_id,
            first,
            "the second is refused rather than replacing the first"
        );
    }

    /// A playback with no live local worker has no slot to take. Staging a row
    /// the actor never holds would leave it for the maintenance backstop while
    /// it counted against the user's admission cap the whole time.
    #[tokio::test]
    async fn a_playback_with_no_live_worker_stages_nothing() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &uuid::Uuid::new_v4().to_string(),
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "the ledger is untouched"
        );
    }

    /// The staged row has to be **usable**, not merely present. Every reader of
    /// a route's `response_json` parses it as a `StartResponse`, and
    /// `control_start_response` filters on the bootstrap being present — so a
    /// row without one answers 404 `session_gone` on the successor's first
    /// exchange the moment §3.5 commits it. A first version of this change
    /// wrote three loose fields and no test noticed, because the staged-row
    /// read carries no recipe or response at all. This commits the successor
    /// and reads what it became.
    #[tokio::test]
    async fn a_staged_successor_commits_into_a_usable_route() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        // The predecessor was itself opened as a stall reopen. `candidate` is
        // its request with only the selection overwritten, so without an
        // explicit clear the successor would stage claiming to be a reopen of
        // a session that is still playing.
        let candidate = crate::transcode::SessionRequest {
            previous_session_id: Some(uuid::Uuid::new_v4().to_string()),
            reopen_reason: Some(crate::transcode::ReopenReason::Stall),
            ..staged_candidate_request()
        };
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &candidate,
            Some(&staged_source_file()),
        )
        .await;
        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (status, prepare_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(prepare_body["action"]["type"], "prepare");
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            first_frame_unix_ms: Some(unix_ms()),
        });
        let (status, committed_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(committed_body["action"]["type"], "none");

        let committed = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("committed route")
            .expect("the successor is now current");
        assert_eq!(committed.incarnation_id, staged.staged_incarnation_id);
        let retired_predecessor = fixture
            .state
            .store
            .media_session_route(&session_id)
            .await
            .expect("predecessor route after acknowledgement")
            .expect("the predecessor remains as a terminal route");
        assert_ne!(
            retired_predecessor.state, "active",
            "pointer advancement retires the predecessor"
        );
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read after acknowledgement")
                .is_none(),
            "commit removes the preparation ledger row"
        );

        let response = serde_json::from_str::<StartResponse>(&committed.response_json)
            .expect("the staged response must parse as the type every reader uses");
        assert!(
            response.control.is_some(),
            "without a bootstrap the successor's first control exchange is a 404"
        );
        assert_eq!(response.session_id, committed.session_id);

        let recipe = serde_json::from_str::<RemoteStartRequest>(&committed.recipe_json)
            .expect("the staged recipe must parse");
        let file = staged_source_file();
        assert_eq!(
            (recipe.source_size, recipe.source_mtime),
            (file.size, file.mtime),
            "`0/0` is the sentinel that refuses this successor a takeover forever"
        );
        assert_eq!(
            recipe.request.request_id.as_deref(),
            Some(recipe.incarnation_id.as_str()),
            "the envelope validator requires exactly this"
        );
        assert!(
            recipe.request.previous_session_id.is_none() && recipe.request.reopen_reason.is_none(),
            "a successor is its own generation, not a reopen of the one it replaces"
        );
    }

    #[tokio::test]
    async fn a_failed_preparation_acknowledgement_aborts_and_frees_the_slot() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        let first_staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged")
            .staged_incarnation_id;

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (status, prepare_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Failed,
            buffered_through_ms: None,
            first_frame_unix_ms: None,
        });
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["action"]["type"], "none");
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("the predecessor stays current")
                .incarnation_id,
            route.incarnation_id
        );
        assert!(fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger after abort")
            .is_none());

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        let replacement = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("replacement ledger read")
            .expect("the settled slot accepts another preparation");
        assert_ne!(replacement.staged_incarnation_id, first_staged);
    }

    /// The successor resumes where the viewer is, not where the predecessor
    /// started. `candidate_request` clones the predecessor's request and
    /// overwrites only the selection, so its `start_seconds` is the *original*
    /// start — committing that restarts the film for a viewer forty minutes
    /// in.
    #[tokio::test]
    async fn a_staged_successor_resumes_at_the_frontier_not_the_original_start() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, mut route) = staging_fixture(dir.path()).await;
        route.media_origin_ms = 90_000;
        route.fetched_through_ms = 2_400_000;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
        )
        .await;
        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        let gate = fixture
            .state
            .transcode
            .session_preparation_gate(&session_id)
            .await
            .expect("gate");
        crate::playback_control::PreparationExecutor::new(
            Arc::clone(&fixture.state.store),
            gate,
            route.user_id,
            route.playback_id.clone(),
        )
        .commit(
            &staged.staged_incarnation_id,
            unix_ms(),
            unix_ms() + 900_000,
        )
        .await
        .expect("commit");

        let committed = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("committed route")
            .expect("current");
        assert_eq!(
            committed.media_origin_ms, 2_490_000,
            "origin is the predecessor's origin plus what the client fetched"
        );
        let recipe =
            serde_json::from_str::<RemoteStartRequest>(&committed.recipe_json).expect("recipe");
        assert!(
            (recipe.request.start_seconds - 2_490.0).abs() < 0.001,
            "the successor starts at the frontier, not at {}",
            staged_candidate_request().start_seconds,
        );
    }

    /// A VOD session — which is what every public create actually is — has a
    /// gate. The first version of this change resolved only the rolling
    /// registry, so it staged nothing in production while its own tests
    /// passed against a registry viewers never enter.
    #[tokio::test]
    async fn a_vod_session_resolves_a_preparation_gate() {
        let dir = crate::test_tempdir().expect("state dir");
        let (_app, state) = super::super::tests::test_app_with_state();
        let session_id = uuid::Uuid::new_v4().to_string();
        assert!(
            state
                .transcode
                .session_preparation_gate(&session_id)
                .await
                .is_none(),
            "a session neither engine knows has no slot to take"
        );

        state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        assert!(
            state
                .transcode
                .session_preparation_gate(&session_id)
                .await
                .is_some(),
            "the VOD engine serves every public session and must answer here"
        );
    }

    /// The fixture's own source row. The seam reads it, and staging refuses
    /// without a snapshot — `0/0` is the sentinel that would refuse the
    /// successor a takeover forever.
    async fn staging_source(fixture: &HlsDeliveryFixture) -> plurx_core::domain::MediaFile {
        fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("fixture file lookup")
            .expect("the fixture has a source")
    }

    fn staged_predecessor_recipe(route: &MediaSessionRoute) -> RemoteStartRequest {
        RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: route.incarnation_id.clone(),
            user_id: route.user_id,
            source_size: 4_096,
            source_mtime: 1_700_000_000,
            typeless_playlist: false,
            request: staged_candidate_request(),
        }
    }

    /// The real source snapshot the seam reads. `0/0` is the codebase's
    /// "this placement never saw the file" sentinel and would refuse the
    /// successor a takeover forever, so the fixture carries a readable one.
    fn staged_source_file() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            id: 11,
            item_id: 1,
            path: std::path::PathBuf::from("/library/staged.mkv"),
            size: 4_096,
            mtime: 1_700_000_000,
            duration_ms: Some(3_600_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_profile: None,
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(18_183_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn staged_candidate_request() -> crate::transcode::SessionRequest {
        crate::transcode::SessionRequest {
            control_sequence: None,
            file_id: 11,
            playback_id: "stage-player".to_owned(),
            request_id: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: crate::transcode::SessionKind::Transcode { height: 1080 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
        }
    }

    /// Drive the **ingress** control gate, not the helper behind it.
    ///
    /// That gate returns before `verify_authority` ever runs, so a version of
    /// this test that called the classifier directly would pass with the gate
    /// reverted to its old unconditional 425 — which is the one fact this
    /// change turns on. An adversarial review found exactly that mutation
    /// surviving, so this posts a real exchange at a durable route instead.
    #[tokio::test]
    async fn the_ingress_control_gate_answers_a_lost_owner_gone() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "control-loss-unrelated").await;
        let user = fixture
            .store
            .create_user("control-loss", "hash", false)
            .await
            .expect("control-loss user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                incarnation_id: incarnation_id.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "control-loss".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "former-owner".to_owned(),
                // Active, long outside its lease, and serving EVENT — a
                // readable recipe that no takeover can ever act on, which is
                // the dominant real case rather than a malformed one. The
                // response is a real one too, because control admission needs
                // a bootstrap before it reaches the gate under test.
                lease_expires_at_ms: 2,
                recipe_json: crate::media_sessions::takeover_eligible_route(
                    &session_id,
                    &incarnation_id,
                )
                .recipe_json
                .replace("\"typeless_playlist\":true", "\"typeless_playlist\":false")
                .replace("\"user_id\":7", &format!("\"user_id\":{}", user.id)),
                response_json: serde_json::to_string(&StartResponse {
                    session_id: session_id.clone(),
                    playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
                    duration_ms: Some(60_000),
                    start_seconds: 0.0,
                    media_origin_ms: Some(0),
                    height: 720,
                    encoder: "software".to_owned(),
                    vod: false,
                    ladder: vec![],
                    prior_kbps: None,
                    delivered_dynamic_range: Some("sdr".to_owned()),
                    delivered_dolby_vision_profile: None,
                    control: crate::playback_control::ControlBootstrap::new(
                        &session_id,
                        &incarnation_id,
                        1,
                        crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
                    ),
                    plan_notes: Vec::new(),
                })
                .expect("start response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: 1,
            },
        )
        .await;

        let body = Bytes::from(
            serde_json::to_vec(&control_request(incarnation_id.clone())).expect("control request"),
        );
        let response = control_inner(
            fixture.state.clone(),
            session_id,
            body,
            unix_ms().saturating_add(4_000),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::GONE,
            "a control exchange against an owner nothing can replace must not ask for a retry"
        );
        let (_, body) = control_body(response).await;
        assert_eq!(body["code"], "owner_lost");
        assert!(
            body.get("retry_after_ms").is_none(),
            "a hint is what all three reporters read as an instruction to keep going"
        );
        assert_eq!(body["generation"], incarnation_id);
    }

    /// Its counterpart, through the same gate: a session a survivor can still
    /// claim keeps the retryable answer and its hint.
    #[tokio::test]
    async fn the_ingress_control_gate_keeps_a_claimable_owner_retryable() {
        let route = {
            let mut route = eligible_owner_loss_route();
            route.lease_expires_at_ms = 1;
            route
        };
        let (status, body) = control_body(control_owner_answer(&route, Some(3), NOW_MS)).await;
        assert_eq!(status, StatusCode::TOO_EARLY);
        assert_eq!(body["code"], "owner_transition");
        assert_eq!(body["retry_after_ms"], 500);
        assert_eq!(
            body["control_epoch"], 3,
            "the client still needs the epoch it was fenced against"
        );
    }

    /// And the loss answer carries it too, when the caller has one.
    #[tokio::test]
    async fn the_loss_answer_reports_the_epoch_its_caller_was_fenced_against() {
        let mut route = untakeoverable_owner_loss_route();
        route.lease_expires_at_ms = 1;

        let (status, body) = control_body(control_owner_answer(&route, Some(3), NOW_MS)).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["code"], "owner_lost");
        assert_eq!(body["control_epoch"], 3);
        assert_eq!(body["generation"], route.incarnation_id);
    }

    /// The relay passes `owner_epoch` as an `Option`, and a peer's answer is
    /// validated against a strict `(status, code)` allowlist before it reaches
    /// a client — so the owner-side shape has to be a legal control body too.
    #[tokio::test]
    async fn the_relay_shaped_loss_answer_is_a_legal_control_body() {
        let mut route = untakeoverable_owner_loss_route();
        route.lease_expires_at_ms = 1;

        let (status, body) = control_body(control_owner_answer(&route, None, NOW_MS)).await;
        assert_eq!(status, StatusCode::GONE);
        let parsed: crate::playback_control::ControlErrorBody =
            serde_json::from_value(body).expect("the relay deserializes this exact shape");
        assert!(
            parsed.is_valid_for_status(status.as_u16()),
            "a body the relay would refuse never reaches the client at all"
        );
        assert_eq!(parsed.code, "owner_lost");
        assert!(parsed.retry_after_ms.is_none());
        assert!(
            parsed.control_epoch.is_none(),
            "the relay has no epoch to report and must not invent one"
        );
    }

    /// A committed replacement is pending publication with a live lease, so it
    /// is a transition on this plane too — and its recipe, being a VOD
    /// handle's, is one no takeover would ever accept. Classifying on the
    /// recipe alone would end a session whose successor is alive.
    #[tokio::test]
    async fn a_publication_fence_with_a_live_lease_stays_retryable_on_the_control_plane() {
        let mut route = untakeoverable_owner_loss_route();
        route.lease_expires_at_ms = NOW_MS + 1;
        route.publication_ready_at_ms =
            NOW_MS + plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS;

        let (status, body) = control_body(control_owner_answer(&route, Some(3), NOW_MS)).await;
        assert_eq!(status, StatusCode::TOO_EARLY);
        assert_eq!(body["code"], "owner_transition");
    }

    /// The detail object is data. A route that somehow carried a `code` field
    /// must not be able to rename the error a client dispatches on.
    #[tokio::test]
    async fn typed_detail_cannot_shadow_the_code_a_client_dispatches_on() {
        let error = ApiError::typed_detail(
            StatusCode::GONE,
            "media_owner_lost",
            "message",
            serde_json::json!({ "code": "not_found", "message": "spoofed", "extra": 1 }),
        );
        let (status, body) = error_body(error).await;

        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["code"], "media_owner_lost");
        assert_eq!(body["message"], "message");
        assert_eq!(body["extra"], 1);
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
            terminal_reason: None,
            publication_ready_at_ms: 0,
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
        route.publication_ready_at_ms = 1_001;
        assert!(
            !resolved_replay_is_live(&route, 1_000),
            "a durable predecessor-handoff fence blocks idempotent replay"
        );
        route.publication_ready_at_ms = 0;
        assert!(!resolved_replay_is_live(&route, 1_001));
        route.lease_expires_at_ms = 1_000;
        assert!(
            !resolved_replay_is_live(&route, 1_000),
            "a read that returns after exact expiry must never answer a replay"
        );
    }

    #[test]
    fn activation_confirmation_rejects_a_takeover_epoch_with_reused_ids() {
        let incarnation_id = "00000000-0000-4000-8000-0000000000e1";
        let session_id = "00000000-0000-4000-8000-0000000000e2";
        let fingerprint = "e".repeat(64);
        let recipe_json = "{}".to_owned();
        let response_json = r#"{"session":"confirmed"}"#.to_owned();
        let activation = MediaSessionActivation {
            incarnation_id: incarnation_id.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id: "activation-confirmation".to_owned(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: fingerprint.clone(),
            owner_node_id: "node-e".to_owned(),
            recipe_json: recipe_json.clone(),
            response_json: response_json.clone(),
            publication_ready_at_ms: 0,
            media_origin_ms: 0,
            now_ms: unix_ms(),
            lease_expires_at_ms: unix_ms() + 60_000,
        };
        let mut route = MediaSessionRoute {
            incarnation_id: incarnation_id.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id: "activation-confirmation".to_owned(),
            request_fingerprint: fingerprint,
            owner_node_id: "node-e".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: unix_ms() + 60_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json,
            response_json,
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: unix_ms(),
        };
        assert!(route_matches_activation(&route, &activation));
        route.owner_epoch = 2;
        assert!(
            !route_matches_activation(&route, &activation),
            "a delayed epoch-one confirmation cannot adopt a same-id takeover"
        );
    }

    #[test]
    fn idempotent_publication_replay_accepts_the_current_takeover_epoch() {
        let now_ms = unix_ms();
        let observed = MediaSessionRoute {
            incarnation_id: "00000000-0000-4000-8000-0000000000e3".to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000e4".to_owned(),
            user_id: 7,
            playback_id: "publication-replay".to_owned(),
            request_fingerprint: "f".repeat(64),
            owner_node_id: "departed-owner".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: now_ms + 30_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: r#"{"recipe":1}"#.to_owned(),
            response_json: r#"{"session":"ready"}"#.to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: now_ms,
        };
        let mut published = observed.clone();
        published.owner_node_id = "surviving-owner".to_owned();
        published.owner_epoch = 2;
        published.lease_expires_at_ms = now_ms + 60_000;
        published.discontinuity_sequence = 1;
        published.updated_at_ms = now_ms + 1;
        assert!(
            replay_publication_matches(&published, &observed),
            "takeover may advance only mutable ownership/progress coordinates"
        );
        published.response_json = r#"{"session":"different"}"#.to_owned();
        assert!(
            !replay_publication_matches(&published, &observed),
            "replay still requires the exact persisted response identity"
        );
    }

    #[tokio::test]
    async fn delayed_start_abort_cannot_reap_takeover_or_unmapped_vod() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let fixture =
            HlsDeliveryFixture::publish_takeover(dir.path(), &session_id, &incarnation_id, 2).await;

        abort_started_session(
            &fixture.state,
            &fixture.state.node_id,
            &incarnation_id,
            &session_id,
        )
        .await;
        assert!(
            fixture.worker_is_registered(&session_id).await,
            "the epoch-one call-site composition must leave a same-id epoch-two worker alive"
        );
        assert!(
            fixture
                .state
                .transcode
                .stop_session_for_owner(&incarnation_id, &session_id, 2, "test cleanup",)
                .await
        );

        let vod_session = uuid::Uuid::new_v4().to_string();
        let unmapped_incarnation = uuid::Uuid::new_v4().to_string();
        let _vod_owner = install_vod_http_session(&fixture, dir.path(), &vod_session).await;
        assert!(
            fixture
                .state
                .transcode
                .vod_owns_or_preparing(&vod_session)
                .await
        );
        assert!(
            !fixture
                .state
                .transcode
                .stop_vod_session_for_request(
                    &unmapped_incarnation,
                    &vod_session,
                    "delayed start abort",
                )
                .await,
            "a real VOD capability without the exact request mapping cannot be reaped"
        );
        assert!(
            fixture
                .state
                .transcode
                .vod_owns_or_preparing(&vod_session)
                .await,
            "the rejected VOD cleanup leaves the capability intact"
        );
        fixture
            .state
            .transcode
            .begin_session_terminal(
                &vod_session,
                crate::vodserve::Terminal::Replaced,
                "test cleanup",
            )
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn started_session_guard_holds_replacement_gate_until_cleanup_settles() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "guard-lifetime").await;
        let request = crate::transcode::SessionRequest {
            control_sequence: None,
            file_id: 1,
            playback_id: "guard-lifetime-player".to_owned(),
            request_id: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: crate::transcode::SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Live,
            block_budget_secs: None,
        };
        let replacement = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("replacement gate");
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (released_tx, released_rx) = tokio::sync::oneshot::channel();
        let mut guard = StartedSessionGuard::new(
            fixture.state.clone(),
            fixture.state.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
            "guard-lifetime".to_owned(),
            7,
            "guard-lifetime-request".to_owned(),
            Some(replacement),
        );
        guard.hold_cleanup_for_test(settled_tx, release_rx, released_tx);
        drop(guard);
        settled_rx
            .await
            .expect("cleanup reached its settlement seam");

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let blocked = tokio::spawn({
            let state = fixture.state.clone();
            let request = request.clone();
            async move {
                let blocked = tokio::time::timeout(
                    Duration::from_secs(1),
                    state.transcode.acquire_cluster_takeover_replacement(
                        &request,
                        7,
                        tokio::time::Instant::now() + Duration::from_secs(10),
                    ),
                );
                tokio::pin!(blocked);
                let mut entered_tx = Some(entered_tx);
                std::future::poll_fn(|context| {
                    let result = std::future::Future::poll(blocked.as_mut(), context);
                    if result.is_pending() {
                        if let Some(entered_tx) = entered_tx.take() {
                            let _ = entered_tx.send(());
                        }
                    }
                    result
                })
                .await
            }
        });
        entered_rx
            .await
            .expect("replacement waiter registered behind the cleanup-owned gate");
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            blocked.await.expect("replacement waiter task").is_err(),
            "cleanup must retain the replacement gate"
        );

        release_tx.send(()).expect("cleanup release");
        released_rx
            .await
            .expect("replacement guard was dropped after cleanup settlement");
        let reacquired = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("cleanup settlement releases the replacement gate");
        drop(reacquired);

        let replacement = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("replacement gate for disarm");
        let mut disarmed = StartedSessionGuard::new(
            fixture.state.clone(),
            fixture.state.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
            "guard-lifetime".to_owned(),
            7,
            "guard-disarm-request".to_owned(),
            Some(replacement),
        );
        disarmed.disarm();
        let reacquired = fixture
            .state
            .transcode
            .acquire_cluster_takeover_replacement(
                &request,
                7,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("disarm releases the replacement gate synchronously");
        drop(reacquired);
    }

    #[tokio::test]
    async fn durable_activation_commit_cannot_straddle_serving_loss() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let authority = fence.authority();
        let generation = authority.admit().expect("initial authority");
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut commit = tokio::spawn({
            let authority = authority.clone();
            let entered = Arc::clone(&entered);
            async move {
                let transition = authority
                    .commit_guard_before(
                        generation,
                        std::time::Instant::now() + Duration::from_secs(5),
                    )
                    .await
                    .ok_or_else(|| {
                        ApiError::ServiceUnavailable(
                            "authority admitted commit could not acquire transition".to_owned(),
                        )
                    })?;
                entered.wait().await;
                release_rx.await.expect("release activation commit");
                Ok::<_, ApiError>((7_u8, transition))
            }
        });
        entered.wait().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut commit)
                .await
                .is_err(),
            "the HTTP wait may expire without cancelling the commit owner"
        );
        let loss = tokio::spawn(async move { fence.validation_set_ready(false).await });
        tokio::task::yield_now().await;
        assert!(
            !loss.is_finished(),
            "serving loss waits until the bounded activation commit ends"
        );
        release_tx.send(()).expect("release commit");
        let (result, transition) = commit
            .await
            .expect("commit task")
            .expect("authority admitted commit");
        assert_eq!(result, 7);
        drop(transition);
        loss.await.expect("serving loss task");

        let stale = authority
            .commit_guard_before(
                generation,
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .await;
        assert!(
            stale.is_none(),
            "a stale activation cannot acquire a commit guard"
        );
    }

    #[tokio::test]
    async fn replayed_start_guard_owns_neither_worker_nor_original_claim() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = "recovered-start-worker";
        let fixture = HlsDeliveryFixture::publish(dir.path(), session_id).await;
        let user = fixture
            .state
            .store
            .create_user("replayed-guard", "hash", false)
            .await
            .expect("create guard user");
        let request_id = "replayed-guard-request";
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let fingerprint = "a".repeat(64);
        let now_ms = unix_ms();
        assert!(matches!(
            fixture
                .state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "replayed-guard-player",
                    &incarnation_id,
                    now_ms,
                    now_ms.saturating_add(60_000),
                )
                .await
                .expect("claim original request"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        let guard = StartedSessionGuard::replayed(
            fixture.state.clone(),
            fixture.state.node_id.clone(),
            incarnation_id.clone(),
            session_id.to_owned(),
            user.id,
            request_id.to_owned(),
            None,
        );

        drop(guard);
        tokio::task::yield_now().await;
        assert!(
            fixture.worker_is_registered(session_id).await,
            "a duplicate start never owns the recovered worker"
        );
        let retry_incarnation = uuid::Uuid::new_v4().to_string();
        let retry_now_ms = unix_ms();
        assert!(matches!(
            fixture
                .state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "replayed-guard-player",
                    &retry_incarnation,
                    retry_now_ms,
                    retry_now_ms.saturating_add(60_000),
                )
                .await
                .expect("inspect original claim"),
            MediaSessionRequestClaim::InFlight {
                incarnation_id: active,
                ..
            } if active == incarnation_id
        ));
        assert!(fixture
            .state
            .store
            .fail_media_session_request(user.id, request_id, &incarnation_id, unix_ms())
            .await
            .expect("settle original claim"));
        assert!(
            fixture
                .state
                .transcode
                .stop_session(session_id, "test")
                .await
        );
    }

    #[tokio::test]
    async fn pre_worker_request_guard_releases_an_owned_claim_for_immediate_retry() {
        let (_app, state) = super::super::tests::test_app_with_state();
        let user = state
            .store
            .create_user("request-guard", "hash", false)
            .await
            .expect("create request-guard user");
        let request_id = "request-guard-attempt";
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let retry_incarnation = uuid::Uuid::new_v4().to_string();
        let fingerprint = "b".repeat(64);
        let now_ms = unix_ms();
        assert!(matches!(
            state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "request-guard-player",
                    &incarnation_id,
                    now_ms,
                    now_ms.saturating_add(60_000),
                )
                .await
                .expect("claim guarded request"),
            MediaSessionRequestClaim::Acquired { .. }
        ));

        drop(MediaSessionRequestGuard::new(
            state.clone(),
            user.id,
            request_id.to_owned(),
            incarnation_id,
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let retry_now_ms = unix_ms();
            match state
                .store
                .claim_media_session_request(
                    user.id,
                    request_id,
                    &fingerprint,
                    "request-guard-player",
                    &retry_incarnation,
                    retry_now_ms,
                    retry_now_ms.saturating_add(60_000),
                )
                .await
                .expect("retry guarded request")
            {
                MediaSessionRequestClaim::Acquired { incarnation_id }
                    if incarnation_id == retry_incarnation =>
                {
                    break;
                }
                MediaSessionRequestClaim::InFlight { .. }
                    if tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                other => panic!("guarded claim did not become retryable: {other:?}"),
            }
        }
        assert!(state
            .store
            .fail_media_session_request(user.id, request_id, &retry_incarnation, unix_ms(),)
            .await
            .expect("settle retry claim"));
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

        let dir = crate::test_tempdir().expect("segment directory");
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
    async fn completed_segment_eof_does_not_wait_for_a_blocked_producer_transition() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "nonblocking-eof").await;
        let body = vec![11_u8; 12 * 1024];
        tokio::fs::write(dir.path().join("seg00001.m4s"), &body)
            .await
            .expect("segment bytes");
        let response = segment(
            State(fixture.state.clone()),
            AxPath(("nonblocking-eof".to_owned(), "seg00001.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("segment response");

        // Model an encoder replacement or hold/resume transition that owns
        // the physical signal gate. EOF may commit lease/frontier state and
        // queue flow work, but must not hold END_STREAM behind this gate.
        let transition = fixture.hold_child_transition().await;
        let delivered = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            axum::body::to_bytes(response.into_body(), body.len() + 1),
        )
        .await
        .expect("response EOF is independent of producer signaling")
        .expect("segment body");
        assert_eq!(delivered.len(), body.len());
        drop(transition);
    }

    async fn install_vod_http_session(
        fixture: &HlsDeliveryFixture,
        base: &std::path::Path,
        session_id: &str,
    ) -> crate::transcode::MediaResponseOwner {
        fixture
            .state
            .transcode
            .install_vod_http_test_session(session_id, fixture.file_id(), base)
            .await;
        let publication = fixture
            .state
            .transcode
            .vod_playlist(session_id)
            .await
            .expect("VOD fixture ownership");
        let _playlist = publication.result.expect("VOD fixture playlist");
        publication.owner
    }

    async fn vod_ready(
        path: &std::path::Path,
        advertised_len: u64,
    ) -> crate::vodserve::SegmentReady {
        crate::vodserve::SegmentReady {
            file: tokio::fs::File::open(path)
                .await
                .expect("open VOD response object"),
            len: advertised_len,
            etag: format!("http-test-{advertised_len}"),
        }
    }

    async fn vod_fetched_segment(fixture: &HlsDeliveryFixture, session_id: &str) -> Option<i64> {
        let crate::transcode::HlsSessionInfo::Vod(status) = fixture
            .state
            .transcode
            .hls_session_status(session_id)
            .await
            .expect("VOD fixture status")
        else {
            panic!("fixture was not VOD");
        };
        status.fetched_segment
    }

    #[tokio::test]
    async fn vod_stream_finalizer_commits_only_exact_live_response_bodies() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("VOD HTTP directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "rolling-unused").await;
        let headers = RelayHeaders::default();

        // Exact EOF: lease and the segment frontier both commit.
        let full_id = "vod-full";
        let full_owner = install_vod_http_session(&fixture, dir.path(), full_id).await;
        let full_path = dir.path().join("full.m4s");
        let full_bytes = vec![1_u8; 24 * 1024];
        tokio::fs::write(&full_path, &full_bytes)
            .await
            .expect("full VOD object");
        let full = vod_segment_response(
            &fixture.state,
            full_id,
            "seg00003.m4s",
            &headers,
            vod_ready(&full_path, full_bytes.len() as u64).await,
            full_owner,
        )
        .await
        .expect("full VOD response");
        assert_eq!(
            axum::body::to_bytes(full.into_body(), full_bytes.len() + 1)
                .await
                .expect("full VOD body")
                .len(),
            full_bytes.len()
        );
        assert_eq!(vod_fetched_segment(&fixture, full_id).await, Some(3));

        // A strict subset Range proves demand and renews the lease, but does
        // not claim that the client owns the complete immutable segment.
        let range_id = "vod-range";
        let range_owner = install_vod_http_session(&fixture, dir.path(), range_id).await;
        let range_path = dir.path().join("range.m4s");
        let range_bytes = vec![2_u8; 16 * 1024];
        tokio::fs::write(&range_path, &range_bytes)
            .await
            .expect("range VOD object");
        let touched_before = fixture
            .state
            .transcode
            .vod_last_touch_for_test(range_id)
            .await
            .expect("range touch before");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let range_headers = RelayHeaders {
            range: Some("bytes=1024-2047".to_owned()),
            if_range: Some(format!("\"http-test-{}\"", range_bytes.len())),
            ..RelayHeaders::default()
        };
        let range = vod_segment_response(
            &fixture.state,
            range_id,
            "seg00004.m4s",
            &range_headers,
            vod_ready(&range_path, range_bytes.len() as u64).await,
            range_owner,
        )
        .await
        .expect("range VOD response");
        assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            axum::body::to_bytes(range.into_body(), 2_048)
                .await
                .expect("range VOD body")
                .len(),
            1_024
        );
        assert!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(range_id)
                .await
                .expect("range touch after")
                > touched_before
        );
        assert_eq!(vod_fetched_segment(&fixture, range_id).await, None);

        // Dropping the body before EOF cannot renew or move the frontier.
        let drop_id = "vod-drop";
        let drop_owner = install_vod_http_session(&fixture, dir.path(), drop_id).await;
        let drop_path = dir.path().join("drop.m4s");
        let drop_bytes = vec![3_u8; 64 * 1024];
        tokio::fs::write(&drop_path, &drop_bytes)
            .await
            .expect("drop VOD object");
        let drop_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(drop_id)
            .await
            .expect("drop touch");
        let dropped = vod_segment_response(
            &fixture.state,
            drop_id,
            "seg00005.m4s",
            &headers,
            vod_ready(&drop_path, drop_bytes.len() as u64).await,
            drop_owner,
        )
        .await
        .expect("droppable VOD response");
        let mut dropped = dropped.into_body().into_data_stream();
        assert!(dropped.next().await.is_some_and(|chunk| chunk.is_ok()));
        drop(dropped);
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(drop_id)
                .await,
            Some(drop_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, drop_id).await, None);

        // A short object reaches storage EOF but not the promised response
        // length, so it is not successful media delivery.
        let short_id = "vod-short";
        let short_owner = install_vod_http_session(&fixture, dir.path(), short_id).await;
        let short_path = dir.path().join("short.m4s");
        tokio::fs::write(&short_path, vec![4_u8; 1_024])
            .await
            .expect("short VOD object");
        let short_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(short_id)
            .await
            .expect("short touch");
        let short = vod_segment_response(
            &fixture.state,
            short_id,
            "seg00006.m4s",
            &headers,
            vod_ready(&short_path, 2_048).await,
            short_owner,
        )
        .await
        .expect("short VOD response");
        let mut short = short.into_body().into_data_stream();
        assert_eq!(
            short
                .next()
                .await
                .expect("short VOD data")
                .expect("readable short prefix")
                .len(),
            1_024
        );
        assert!(
            short.next().await.is_some_and(|chunk| chunk.is_err()),
            "advertised short read must terminate the HTTP body with an error"
        );
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(short_id)
                .await,
            Some(short_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, short_id).await, None);

        // A storage error terminates the body and discards the completion.
        let error_id = "vod-error";
        let error_owner = install_vod_http_session(&fixture, dir.path(), error_id).await;
        let error_path = dir.path().join("unreadable-vod.m4s");
        tokio::fs::create_dir(&error_path)
            .await
            .expect("unreadable VOD object");
        let error_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(error_id)
            .await
            .expect("error touch");
        let error = vod_segment_response(
            &fixture.state,
            error_id,
            "seg00007.m4s",
            &headers,
            vod_ready(&error_path, 1).await,
            error_owner,
        )
        .await
        .expect("error VOD response");
        let mut error = error.into_body().into_data_stream();
        assert!(error.next().await.is_some_and(|chunk| chunk.is_err()));
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(error_id)
                .await,
            Some(error_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, error_id).await, None);

        // Resolution before same-id reattachment carries the old incarnation;
        // even exact EOF cannot touch the successor or its reader frontier.
        let replaced_id = "vod-replaced";
        let stale_owner = install_vod_http_session(&fixture, dir.path(), replaced_id).await;
        let replaced_path = dir.path().join("replaced.m4s");
        let replaced_bytes = vec![5_u8; 8 * 1024];
        tokio::fs::write(&replaced_path, &replaced_bytes)
            .await
            .expect("replaced VOD object");
        let stale_response = vod_segment_response(
            &fixture.state,
            replaced_id,
            "seg00008.m4s",
            &headers,
            vod_ready(&replaced_path, replaced_bytes.len() as u64).await,
            stale_owner,
        )
        .await
        .expect("stale VOD response");
        let _successor_owner = install_vod_http_session(&fixture, dir.path(), replaced_id).await;
        let successor_touch = fixture
            .state
            .transcode
            .vod_last_touch_for_test(replaced_id)
            .await
            .expect("successor touch");
        assert_eq!(
            axum::body::to_bytes(stale_response.into_body(), replaced_bytes.len() + 1)
                .await
                .expect("stale body remains readable")
                .len(),
            replaced_bytes.len()
        );
        assert_eq!(
            fixture
                .state
                .transcode
                .vod_last_touch_for_test(replaced_id)
                .await,
            Some(successor_touch)
        );
        assert_eq!(vod_fetched_segment(&fixture, replaced_id).await, None);
    }

    #[tokio::test]
    async fn range_and_bodyless_segment_responses_keep_delivery_truth() {
        let dir = crate::test_tempdir().expect("segment directory");
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
        let partial_delivery = fixture
            .wait_for_delivery_projection("segment-range", None)
            .await;
        assert_eq!(fixture.delivered_bytes(), 1_024);
        assert_eq!(fixture.last_renewal_kind().await, "segment-range");
        assert_eq!(
            fixture.fetched_segment(),
            -1,
            "a completed byte range proves demand but not a complete segment"
        );
        assert_eq!(partial_delivery.fetched_segment, None);

        let mut full_span = HeaderMap::new();
        full_span.insert(header::RANGE, "bytes=0-".parse().expect("full range"));
        let full_span = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            full_span,
        )
        .await
        .expect("full-span range response");
        assert_eq!(full_span.status(), StatusCode::PARTIAL_CONTENT);
        let full_span_etag = full_span
            .headers()
            .get(header::ETAG)
            .cloned()
            .expect("rolling response ETag");
        assert_eq!(
            axum::body::to_bytes(full_span.into_body(), body.len() + 1)
                .await
                .expect("full-span body")
                .len(),
            body.len()
        );
        let actor_delivery = fixture
            .wait_for_delivery_projection("segment-range", Some(4))
            .await;
        assert_eq!(
            fixture.fetched_segment(),
            4,
            "a Range response that contains every byte advances the frontier"
        );
        assert_eq!(actor_delivery.fetched_segment, Some(4));
        assert_eq!(actor_delivery.pending_fetched_segment, Some(4));
        let mut stale_if_range = HeaderMap::new();
        stale_if_range.insert(
            header::RANGE,
            "bytes=1024-2047".parse().expect("stale conditional range"),
        );
        stale_if_range.insert(
            header::IF_RANGE,
            "W/\"stale-generation\"".parse().expect("weak If-Range"),
        );
        let complete = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            stale_if_range,
        )
        .await
        .expect("stale If-Range response");
        assert_eq!(complete.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(complete.into_body(), body.len() + 1)
                .await
                .expect("complete fallback body")
                .len(),
            body.len(),
            "weak or stale If-Range must ignore Range"
        );
        let delivered_after_full_span = fixture.delivered_bytes();

        let mut conditional = HeaderMap::new();
        conditional.insert(header::IF_NONE_MATCH, full_span_etag);
        let not_modified = segment(
            State(fixture.state.clone()),
            AxPath(("range".to_owned(), "seg00004.m4s".to_owned())),
            conditional,
        )
        .await
        .expect("conditional response");
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            fixture.fetched_segment(),
            4,
            "the client has the cached object"
        );
        assert_eq!(fixture.actor_delivery().await.fetched_segment, Some(4));
        let renewal_before_rejection = fixture.last_renewal_kind().await;
        let frontier_before_rejection = fixture.fetched_segment();

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
        assert_eq!(fixture.delivered_bytes(), delivered_after_full_span);
        assert_eq!(fixture.last_renewal_kind().await, renewal_before_rejection);
        assert_eq!(fixture.fetched_segment(), frontier_before_rejection);
        assert!(
            fixture.settle().await.is_empty(),
            "valid partial and intentionally bodyless responses are not incomplete deliveries"
        );

        let init_dir = crate::test_tempdir().expect("init directory");
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

        let mut stale_init_headers = HeaderMap::new();
        stale_init_headers.insert(header::RANGE, "bytes=0-3".parse().expect("init range"));
        stale_init_headers.insert(
            header::IF_RANGE,
            "\"different-representation\""
                .parse()
                .expect("init If-Range"),
        );
        let complete_init = segment(
            State(init_fixture.state.clone()),
            AxPath(("init-range".to_owned(), "init.mp4".to_owned())),
            stale_init_headers,
        )
        .await
        .expect("init If-Range fallback");
        assert_eq!(complete_init.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(complete_init.into_body(), 4_097)
                .await
                .expect("complete init body")
                .len(),
            4_096
        );
        assert_eq!(init_fixture.delivered_bytes(), 4_100);
        assert!(init_fixture.settle().await.is_empty());
    }

    /// A client that walks away mid-segment is the case nothing else observes:
    /// the handler has already returned, the stream never reaches EOF, and no
    /// error is raised. Only `Drop` can name it, which also makes it the
    /// easiest classification to lose to a later refactor.
    #[tokio::test]
    async fn an_abandoned_segment_body_is_recorded_as_response_dropped() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "abandoned").await;
        let renewal_before = fixture.last_renewal_kind().await;
        let frontier_before = fixture.fetched_segment();
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
        assert_eq!(
            fixture.last_renewal_kind().await,
            renewal_before,
            "a dropped response cannot renew the playback lease"
        );
        assert_eq!(
            fixture.fetched_segment(),
            frontier_before,
            "a dropped response cannot advance the consumed frontier"
        );
    }

    #[tokio::test]
    async fn a_stream_resolved_before_retirement_cannot_commit_after_eof() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "retired-body").await;
        let body = vec![7_u8; 32 * 1024];
        tokio::fs::write(dir.path().join("seg00003.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("retired-body".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("resolved response");
        assert!(
            fixture
                .state
                .transcode
                .stop_session("retired-body", "test-retirement")
                .await
        );
        let renewal_after_retirement = fixture.last_renewal_kind().await;
        let frontier_after_retirement = fixture.fetched_segment();

        assert_eq!(
            axum::body::to_bytes(response.into_body(), body.len() + 1)
                .await
                .expect("already-authorized bytes")
                .len(),
            body.len()
        );
        assert_eq!(
            fixture.last_renewal_kind().await,
            renewal_after_retirement,
            "EOF from an obsolete incarnation cannot renew it"
        );
        assert_eq!(
            fixture.fetched_segment(),
            frontier_after_retirement,
            "EOF from an obsolete incarnation cannot move its frontier"
        );
    }

    #[tokio::test]
    async fn a_stream_from_an_old_producer_attempt_cannot_advance_its_successor() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "old-attempt-body").await;
        let body = vec![9_u8; 32 * 1024];
        tokio::fs::write(dir.path().join("seg00003.m4s"), &body)
            .await
            .expect("segment bytes");

        let response = segment(
            State(fixture.state.clone()),
            AxPath(("old-attempt-body".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("response opened on attempt zero");
        assert_eq!(fixture.begin_producer_attempt().await, Ok(1));
        let renewal_after_replacement = fixture.last_renewal_kind().await;

        assert_eq!(
            axum::body::to_bytes(response.into_body(), body.len() + 1)
                .await
                .expect("already-open predecessor bytes")
                .len(),
            body.len()
        );
        assert_eq!(
            fixture.last_renewal_kind().await,
            renewal_after_replacement,
            "predecessor EOF cannot renew the successor attempt"
        );
        assert_eq!(fixture.fetched_segment(), -1);
        assert_eq!(fixture.actor_delivery().await.fetched_segment, None);
    }

    #[tokio::test]
    async fn accepted_predecessor_eof_cannot_project_after_successor_reset() {
        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "projection-race").await;
        let body = vec![5_u8; 32 * 1024];
        tokio::fs::write(dir.path().join("seg00003.m4s"), &body)
            .await
            .expect("segment bytes");
        let response = segment(
            State(fixture.state.clone()),
            AxPath(("projection-race".to_owned(), "seg00003.m4s".to_owned())),
            HeaderMap::new(),
        )
        .await
        .expect("predecessor response");

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        fixture.pause_response_projection(Arc::clone(&pause));
        let body_len = body.len();
        let drain = tokio::spawn(async move {
            axum::body::to_bytes(response.into_body(), body_len + 1)
                .await
                .expect("predecessor body")
        });
        pause.wait().await;
        assert_eq!(
            fixture.begin_producer_attempt().await,
            Ok(1),
            "successor admission resets the compatibility projection"
        );
        pause.wait().await;
        assert_eq!(drain.await.expect("body task").len(), body_len);
        assert_eq!(fixture.fetched_segment(), -1);
        assert_eq!(fixture.actor_delivery().await.fetched_segment, None);
    }

    /// A storage error mid-body is its own classification, separate from an
    /// abandoned response and from a short one. Nothing reached `fail()`
    /// before this test, so `storage_read_error` could have stopped being
    /// emitted with no check noticing.
    #[tokio::test]
    async fn an_unreadable_segment_body_is_recorded_as_storage_read_error() {
        use futures_util::StreamExt;

        let dir = crate::test_tempdir().expect("segment directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unreadable").await;
        let renewal_before = fixture.last_renewal_kind().await;
        let frontier_before = fixture.fetched_segment();
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
        assert_eq!(fixture.last_renewal_kind().await, renewal_before);
        assert_eq!(fixture.fetched_segment(), frontier_before);
    }

    #[tokio::test]
    async fn an_unreadable_small_init_never_commits_lease_or_frontier() {
        let dir = crate::test_tempdir().expect("init directory");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "unreadable-init").await;
        tokio::fs::create_dir(dir.path().join("init.mp4"))
            .await
            .expect("unreadable init");
        let renewal_before = fixture.last_renewal_kind().await;
        let frontier_before = fixture.fetched_segment();

        assert!(
            segment(
                State(fixture.state.clone()),
                AxPath(("unreadable-init".to_owned(), "init.mp4".to_owned())),
                HeaderMap::new(),
            )
            .await
            .is_err(),
            "the buffered init read must fail before a response is committed"
        );
        assert_eq!(fixture.last_renewal_kind().await, renewal_before);
        assert_eq!(fixture.fetched_segment(), frontier_before);
        assert!(fixture
            .delivery_events(1)
            .await
            .iter()
            .any(|event| event.reason.as_deref() == Some("storage_read_error")));
    }

    /// `exact_hls_context` opens `init.mp4` for the playlist generator, not
    /// for a client: there is no response body on that path. Its bytes must
    /// not move the session's delivery meter, and — because the read is
    /// bounded well below a large init's real length — a bounded read that
    /// returned everything it asked for must not be reported as a response
    /// that ended early.
    #[tokio::test]
    async fn a_playlist_time_init_probe_is_not_client_delivery_and_never_a_short_response() {
        let dir = crate::test_tempdir().expect("segment directory");
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
        let dir = crate::test_tempdir().expect("segment directory");
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
    /// All five used to be `ApiError::NotFound("transcode session")` — one
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
                PlaylistError::ProducerEnded("progress deadline elapsed".into()),
                StatusCode::BAD_GATEWAY,
                "producer_ended",
                "already listed remains available",
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
            assert_eq!(err.retryable(), code == "startup_timeout", "{code}");
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
            dolby_vision: Default::default(),
        }
    }

    /// A node that can do everything, so the review's answers are the
    /// client's answers and nothing is confounded by what this server proved
    /// at boot.
    fn capable_node() -> plurx_core::playback::RenderCaps {
        plurx_core::playback::RenderCaps::proven(true)
    }

    /// `hls_file` carries a bare "Dolby Vision" label — deliberately, because
    /// the HLS master tests exist to prove a stream with no configuration
    /// record still gets a coherent playlist. The plan review needs the
    /// opposite: a title whose profile is actually knowable, or every
    /// derivation below answers "no Dolby Vision" for a reason that has
    /// nothing to do with the client.
    fn dolby_vision_p8_file() -> MediaFile {
        let mut file = hls_file(Vec::new());
        file.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".into());
        file
    }

    fn caps_v2(json: &str) -> plurx_core::playback::DeviceCaps {
        serde_json::from_str(json).expect("the v2 document parses")
    }

    /// A client that decodes Profile 8 Dolby Vision over HLS. The plan for
    /// `hls_file` (a P8-labelled 2160p HEVC title) is to preserve it.
    fn dolby_vision_client() -> plurx_core::playback::DeviceCaps {
        caps_v2(
            r#"{"v":2,
                "video":[{"codec":"hevc","present":["sdr","pq"],"dv_profiles":[5,8],
                          "max_height":2160}],
                "audio":["aac"],"containers":["mkv","mp4"],
                "dv_transport":"hls",
                "display":{"hdr":true,"dolby_vision":true}}"#,
        )
    }

    /// The same hardware minus the Dolby Vision decoder — Chrome, in other
    /// words, which is the client the whole plan was written for.
    fn no_dolby_vision_client() -> plurx_core::playback::DeviceCaps {
        caps_v2(
            r#"{"v":2,
                "video":[{"codec":"hevc","present":["sdr"],"dv_profiles":[],
                          "max_height":2160}],
                "audio":["aac"],"containers":["mkv","mp4"],
                "display":{"hdr":false,"dolby_vision":false}}"#,
        )
    }

    /// The acceptance case from PLAYBACK-CAPS-V2-PLAN M3: a create whose body
    /// claims Dolby Vision for a client whose caps enumerate none.
    ///
    /// It **succeeds**, with the server's plan, and says so. Paul's ruling
    /// (2026-08-29): "I don't see a reason for it to prevent functionality."
    /// A refusal here would turn a client bug into an unplayable title, which
    /// is strictly worse than the tone-mapped stream the viewer would
    /// otherwise have got anyway.
    #[test]
    fn a_create_that_claims_more_than_its_caps_gets_the_servers_plan_and_a_note() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,  // the body says preserve_dolby_vision
            false, // …and asks for no HDR10 rung
            NOW_MS,
        );

        assert!(
            !review.preserve_dolby_vision,
            "the caps enumerate no Dolby Vision profile, so the plan cannot preserve it"
        );
        assert!(review.mismatched);
        assert_eq!(
            review.notes,
            vec!["plan_mismatch: client asked preserve_dolby_vision=true, \
                 server derived false"
                .to_owned()],
            "the note names the field, the claim, and the answer — it is what the \
             stats overlay shows a viewer asking why the badge changed"
        );
    }

    /// The case that must stay silent: the client and the server agree.
    ///
    /// This is the one that should be every create once the fleet has moved,
    /// and the absence of notes is the signal. A review that leaves a note on
    /// an agreeing create makes `plan_notes` useless, because a field that is
    /// always populated is a field nobody reads.
    #[test]
    fn an_agreeing_create_leaves_no_note_at_all() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            true,
            NOW_MS,
        );
        assert!(review.preserve_dolby_vision);
        assert!(review.hdr10, "the caps present PQ on hevc");
        assert!(!review.mismatched);
        assert!(review.notes.is_empty(), "{:?}", review.notes);
    }

    /// And when the conversion does *not* happen, the echo is not trusted
    /// either.
    ///
    /// This is the half the test below does not reach. Deriving the conversion
    /// fixed the case where it could be derived; it left the cases where it
    /// comes back `false` delivering exactly what the original regression
    /// delivered — raw dual-layer to a build that enumerated nothing. There
    /// are two of them and both are ordinary: an operator who turned the
    /// conversion off, and a row the M2 backfill has not reached, which has a
    /// Profile 7 label and no columns to build a record from.
    ///
    /// `preserve_dolby_vision: true` from such a build is not a claim about
    /// dual-layer. It is the server's own answer, echoed back — and the server
    /// only answered `true` because it expected to convert.
    #[test]
    fn a_build_with_no_caps_document_is_never_handed_unconverted_dual_layer() {
        let p7 = dolby_vision_p7_file();

        let mut off = capable_node();
        off.dolby_vision_convert = false;
        let refused = legacy_trusted_review(&p7, &off, true, false);
        assert!(!refused.convert_dolby_vision);
        assert!(
            !refused.preserve_dolby_vision,
            "keeping Profile 7 for a build that enumerated nothing is the black \
             screen this milestone exists to stop delivering"
        );
        assert!(
            refused
                .notes
                .iter()
                .any(|note| note.contains("Dolby Vision declined")),
            "and the session says why: {:?}",
            refused.notes
        );
        assert!(
            !refused.mismatched,
            "not a mismatch: there is no document here for the client to have \
             exceeded, and the counter that measures that must stay meaningful"
        );

        // A row with a Profile 7 label and no columns is the same answer by a
        // different route: it cannot convert, so it cannot preserve.
        let mut label_only = p7.clone();
        label_only.dolby_vision = Default::default();
        let node = capable_node();
        assert!(!legacy_trusted_review(&label_only, &node, true, false).preserve_dolby_vision);

        // Everything else is unchanged. A converting create still preserves,
        // because the conversion needs the RPUs to survive the filter…
        let converting = legacy_trusted_review(&p7, &node, true, false);
        assert!(converting.convert_dolby_vision && converting.preserve_dolby_vision);
        assert!(converting.notes.is_empty(), "{:?}", converting.notes);

        // …and a single-layer source is trusted exactly as it always was, on
        // the same node that refused the dual-layer one.
        let p8 = dolby_vision_p8_file();
        let single_layer = legacy_trusted_review(&p8, &off, true, false);
        assert!(
            single_layer.preserve_dolby_vision,
            "the rule is about dual-layer, not about Dolby Vision"
        );
        assert!(single_layer.notes.is_empty(), "{:?}", single_layer.notes);
    }

    /// A build that sends no caps document still gets the conversion.
    ///
    /// The regression this exists for shipped and reached production: the
    /// legacy-trusted arm returned no review at all, so `apply_plan_review`
    /// never ran, so `convert_dolby_vision` kept the `false`
    /// `SessionKind::Copy` was built with — and the client's *silence* on a
    /// field it has no way to speak about outvoted the server's own decision.
    /// `/decision` logged "Profile 7 converted to Profile 8.1 for this
    /// device"; the session created a second later delivered raw Profile 7;
    /// Safari answered `stream_rejected ... browser refused the remux
    /// stream`; the fallback tonemapped the title to SDR. Nothing in the
    /// cluster had ever run a conversion.
    ///
    /// Restored on `main` after #850 removed it. The derivation it guards
    /// survived that PR; only the proof went, and without the proof reverting
    /// the one-line derivation to `false` goes green again. This effort's line
    /// kept its copy throughout, so the merge has one test and one history.
    #[test]
    fn a_build_with_no_caps_document_still_converts_profile_7() {
        let mut p7 = dolby_vision_p8_file();
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);

        let node = capable_node();
        let review = legacy_trusted_review(&p7, &node, true, false);
        assert!(
            review.convert_dolby_vision,
            "the echo cannot carry this field, so trusting it means deriving it"
        );
        assert!(
            review.preserve_dolby_vision,
            "and the echo is still trusted"
        );
        assert!(!review.hdr10, "for every field the echo *can* carry");

        // The conversion follows the preservation, exactly as
        // `review_client_plan` clamps it: a client that declined Dolby Vision
        // is not handed a converted stream by a flag nobody looked at.
        assert!(
            !legacy_trusted_review(&p7, &node, false, false).convert_dolby_vision,
            "declining Dolby Vision declines the conversion with it"
        );

        // An operator switch still wins.
        let mut off = capable_node();
        off.dolby_vision_convert = false;
        assert!(!legacy_trusted_review(&p7, &off, true, false).convert_dolby_vision);

        // And a source the conversion cannot be built for keeps the delivery
        // it has always had, rather than being routed to an index that could
        // never exist.
        let p8 = dolby_vision_p8_file();
        assert!(
            !legacy_trusted_review(&p8, &node, true, false).convert_dolby_vision,
            "there is nothing to convert a Profile 8 source into"
        );
        let mut label_only = p7.clone();
        label_only.dolby_vision.level = None;
        assert!(
            !legacy_trusted_review(&label_only, &node, true, false).convert_dolby_vision,
            "no columns, no conversion — the record could not be built"
        );
    }

    /// A Profile 7 title with the columns the conversion needs — the source
    /// shape the whole of this milestone is about.
    fn dolby_vision_p7_file() -> MediaFile {
        let mut p7 = dolby_vision_p8_file();
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);
        p7
    }

    fn review_log() -> ReviewContext<'static> {
        ReviewContext {
            file_id: 70,
            user_id: 1,
            client_build: "safari/test",
        }
    }

    /// The *arm*, not the review it returns.
    ///
    /// `a_build_with_no_caps_document_still_converts_profile_7` above proves
    /// `legacy_trusted_review` derives the conversion. It cannot prove that a
    /// create ever reaches it — and reaching it is precisely what was broken
    /// before #842: the no-caps arm returned `None`, `apply_plan_review` never
    /// ran, and the session kept the `convert_dolby_vision: false` it was
    /// built with. Restoring `None` here (the mutation) leaves every review
    /// test green and puts raw Profile 7 back on the wire, so this is the test
    /// that has to fail.
    ///
    /// The three shapes are asserted together because the counters are one
    /// population split three ways: a create that lands in two of them, or in
    /// none, is a migration metric that can never reach zero.
    #[test]
    fn a_create_that_sends_no_caps_document_still_gets_a_review() {
        let p7 = dolby_vision_p7_file();
        let node = capable_node();
        let before = plan_derivation::snapshot();

        // The straggler: no caps document at all. It gets a review, and the
        // review carries the one field the echo could not.
        let legacy = plan_review_for(None, None, &p7, &node, true, false, NOW_MS, &review_log())
            .expect("a build that sends no caps document must still get a review");
        assert!(
            legacy.convert_dolby_vision,
            "the arm that trusts the echo has to derive the conversion, or the \
             session preserves raw Profile 7 — the pre-#842 delivery no \
             consumer decoder takes"
        );
        assert!(legacy.preserve_dolby_vision);
        assert!(!legacy.mismatched);

        // A readable v2 document is re-derived instead, from the client's own
        // enumeration. This client takes 8 and not 7, which is the population
        // the conversion exists for, so it converts too — by a different
        // route, which is the point of asserting both.
        let rederived = plan_review_for(
            Some(&dolby_vision_client()),
            None,
            &p7,
            &node,
            true,
            false,
            NOW_MS,
            &review_log(),
        )
        .expect("a v2 document is re-derived, not discarded");
        assert!(rederived.convert_dolby_vision);
        assert!(!rederived.mismatched);

        // A document this build cannot read is not a document, and it lands on
        // the same trust path a build sending none takes. Not `None`: that
        // skips `apply_plan_review` altogether, which leaves the echoed
        // `preserve_dolby_vision` unclamped beside a `convert_dolby_vision`
        // nobody set — raw Profile 7, from any client that can post a `"v":1`
        // body.
        let unusable = caps_v2(r#"{"v":1,"video":[],"audio":[],"containers":[]}"#);
        let unreadable = plan_review_for(
            Some(&unusable),
            None,
            &p7,
            &node,
            true,
            false,
            NOW_MS,
            &review_log(),
        )
        .expect(
            "an unreadable caps document falls through to the client's echo, which is a review",
        );
        assert!(
            unreadable.convert_dolby_vision,
            "and the echo it falls through to derives the conversion, exactly as \
             the no-caps arm does"
        );

        // One create, one counter — asserted as a lower bound, because the
        // counters are process-global `AtomicU64`s and nineteen other tests in
        // this binary call `review_client_plan` on other threads. An exact
        // delta here is a test that fails on an unrelated change, under
        // another test's name; a lower bound still goes red under every
        // mutation of the arms above, which is the property being pinned.
        let after = plan_derivation::snapshot();
        assert!(
            after.0 - before.0 >= 1,
            "the no-caps arm counts a legacy_trusted create"
        );
        assert!(
            after.1 - before.1 >= 1,
            "the unreadable-document arm counts an unusable_caps create"
        );
        assert!(
            after.2 - before.2 >= 1,
            "the v2 arm counts a rederived create"
        );
    }

    /// The session's own answer for both badge fields, for every kind of
    /// session that can carry Dolby Vision.
    ///
    /// Read off the session that was built rather than the decision that
    /// suggested one, because a burn or a forced rung produces a delivery
    /// `/decision` never promised. The two fields are asserted together
    /// because they are read together: a session badged `dolby_vision` whose
    /// profile is absent, or a profile on a session whose range says HDR10,
    /// is a worse answer than either field alone would be.
    #[test]
    fn a_sessions_badge_names_the_range_and_the_profile_it_actually_carries() {
        use crate::transcode::SessionKind;
        use plurx_core::transcode::OutputGrade;

        let mut p7 = dolby_vision_p8_file();
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);

        let copy = |preserve: bool, convert: bool| SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: preserve,
            convert_dolby_vision: convert,
        };
        let badge = |kind: &SessionKind| {
            (
                session_delivered_dynamic_range(Some(&p7), kind, OutputGrade::Sdr),
                session_delivered_dolby_vision_profile(Some(&p7), kind),
            )
        };

        assert_eq!(
            badge(&copy(true, true)),
            (Some("dolby_vision"), Some(8)),
            "a converting session delivers Dolby Vision, and the profile is the \
             one the conversion made — not the 7 the source row says"
        );
        assert_eq!(
            badge(&copy(true, false)),
            (Some("dolby_vision"), Some(7)),
            "the same range as the converting answer, which is why the profile \
             has to be on the wire at all"
        );
        assert_eq!(
            badge(&copy(false, false)),
            (Some("hdr10"), None),
            "a stripped stream carries no Dolby Vision to name"
        );
        assert_eq!(
            badge(&SessionKind::Transcode { height: 1080 }),
            (Some("sdr"), None),
            "no plurx encode rung produces Dolby Vision"
        );

        // A source the store could not load says nothing rather than guessing.
        assert_eq!(
            session_delivered_dolby_vision_profile(None, &copy(true, true)),
            None
        );
    }

    /// A create body with nothing set, to be spread over.
    fn bare_create() -> CreateSession {
        CreateSession {
            control_sequence: None,
            playback_id: String::new(),
            request_id: None,
            previous_session_id: None,
            reopen_reason: None,
            height: None,
            quality_auto: None,
            subtitle_burn: None,
            subtitle_burn_sdr: None,
            native_subtitles: None,
            subtitle: None,
            start: None,
            audio: None,
            copy: None,
            aac: None,
            preserve_dolby_vision: None,
            hdr10: None,
            caps: None,
            overrides: None,
            audio_offset_ms: None,
            presentation: None,
            block_budget_secs: None,
        }
    }

    fn resolver_state() -> AppState {
        let root = crate::test_temp_path(format!("plurx-resolve-plan-{}", uuid::Uuid::new_v4()));
        AppState::new(
            "test".to_owned(),
            std::sync::Arc::new(
                plurx_core::store::SqliteStore::open_in_memory().expect("resolver store"),
            ),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "test-node".to_owned(),
            Default::default(),
            Default::default(),
            std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
        )
    }

    async fn resolved_height(state: &AppState, source: &MediaFile, asked: Option<i64>) -> i64 {
        let body = CreateSession {
            playback_id: "player-a".into(),
            height: asked,
            ..bare_create()
        };
        resolve_plan(
            PlanInputs {
                state,
                user_id: 7,
                file_id: source.id,
                source: Some(source),
                network_prior: None,
            },
            None,
            body,
        )
        .await
        .expect("resolve")
        .height
    }

    /// The three arms of the height resolution — the largest thing
    /// `resolve_plan` extracted that was not already a named function. (The
    /// native-subtitle validation and the `request_id` length check are also
    /// inline; both are covered through the real handler by
    /// `hls_create_rejects_native_subtitle_indices_it_cannot_serve`.)
    ///
    /// They are policy, not arithmetic, and each is a different promise:
    ///
    /// * **the source's own height is never snapped and never downgraded** —
    ///   it is the Original/forced-burn promise the player reads as
    ///   `sessionHeight`;
    /// * **an explicit rung from a menu snaps onto the ladder**, so a stray
    ///   number lands somewhere the encoder has rungs for;
    /// * **an above-ladder height passes through as what it is**, rather than
    ///   being pulled down to the nearest rung.
    ///
    /// Splitting the match is how one of the three gets lost, so this test
    /// exists to make that loud.
    #[tokio::test]
    async fn the_height_resolution_keeps_its_three_promises() {
        let state = resolver_state();
        let mut odd_source = hls_file(Vec::new());
        odd_source.height = Some(900);

        assert_eq!(
            resolved_height(&state, &odd_source, Some(900)).await,
            900,
            "the source's own height is a promise, and is neither snapped nor \
             downgraded",
        );
        assert_eq!(
            resolved_height(&state, &odd_source, Some(1079)).await,
            crate::transcode::snap_height(1079),
            "a stray rung from a menu snaps onto the ladder",
        );
        let above = crate::transcode::MAX_HEIGHT;
        assert_eq!(
            resolved_height(&state, &odd_source, Some(above)).await,
            above,
            "an above-ladder height passes through as what it is",
        );
        // A below-ladder ask lands on the ladder's lowest rung, not on
        // `MIN_HEIGHT`: the snap runs first and the clamp only bounds what
        // comes out of it, so for a menu rung the clamp never binds downward
        // at all.
        let floor = resolved_height(&state, &odd_source, Some(1)).await;
        assert_eq!(floor, crate::transcode::snap_height(1));
        assert!(
            floor > crate::transcode::MIN_HEIGHT,
            "the clamp is a bound, not the policy: {floor}",
        );
        // Upward it does bind, and it is the only thing that does: an
        // above-ladder height passes the snap through untouched, so without
        // the clamp a body asking for 100000 builds a transcode at 100000.
        assert_eq!(
            resolved_height(&state, &odd_source, Some(crate::transcode::MAX_HEIGHT + 1)).await,
            crate::transcode::MAX_HEIGHT,
        );
    }

    /// Auto is the arm every client that does not pin a quality takes, and the
    /// only one that reads the stored network prior or the HDR10 ask. Both
    /// reach `auto_height_for_request` or neither does — and a resolver that
    /// dropped either would still return a plausible height, which is why this
    /// compares against the same call rather than against a constant.
    #[tokio::test]
    async fn auto_asks_the_ladder_with_everything_it_was_given() {
        let state = resolver_state();
        let mut source = hls_file(Vec::new());
        source.height = Some(2160);
        let prior = plurx_core::domain::NetworkPrior {
            credential_generation: Default::default(),
            client_class: "web".to_owned(),
            network_fingerprint: "fingerprint".to_owned(),
            // A measured slow link, which is the whole reason Auto reads the
            // prior at all.
            sustained_kbps: Some(4_000),
            worst_rung_height: Some(1080),
            starved_at_ms: Some(1),
            sample_count: 12,
            updated_at_ms: 1,
        };

        for hdr10 in [false, true] {
            let body = CreateSession {
                playback_id: "player-a".into(),
                height: None,
                hdr10: Some(hdr10),
                ..bare_create()
            };
            let resolved = resolve_plan(
                PlanInputs {
                    state: &state,
                    user_id: 7,
                    file_id: source.id,
                    source: Some(&source),
                    network_prior: Some(&prior),
                },
                None,
                body,
            )
            .await
            .expect("resolve auto")
            .height;
            let expected = state
                .transcode
                .auto_height_for_request(Some(&source), Some(&prior), hdr10)
                .await
                .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT);
            assert_eq!(
                resolved, expected,
                "auto must carry the source, the prior and hdr10={hdr10}",
            );
        }
    }

    /// The fingerprint is taken from the intent as sent, before the review is
    /// applied — so a retry of the same body recovers the same session even on
    /// a node whose review would decide differently.
    #[tokio::test]
    async fn the_intent_fingerprint_ignores_the_review() {
        let state = resolver_state();
        let source = dolby_vision_p8_file();
        // Built twice rather than cloned: `CreateSession` is a wire type and
        // deriving `Clone` on it for a test would be the test changing the
        // contract to suit itself.
        let body = || CreateSession {
            playback_id: "player-a".into(),
            copy: Some(true),
            preserve_dolby_vision: Some(false),
            hdr10: Some(false),
            ..bare_create()
        };
        let inputs = || PlanInputs {
            state: &state,
            user_id: 7,
            file_id: source.id,
            source: Some(&source),
            network_prior: None,
        };
        let plain = resolve_plan(inputs(), None, body())
            .await
            .expect("resolve without a review");
        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &source,
            &capable_node(),
            true,
            true,
            NOW_MS,
        );
        let reviewed = resolve_plan(inputs(), Some(review), body())
            .await
            .expect("resolve with a review");

        assert_eq!(
            plain.intent_fingerprint, reviewed.intent_fingerprint,
            "the review must not move the fingerprint, or a transport retry \
             on a differently-deciding binary gets a 409 instead of its own \
             session",
        );
        assert_ne!(
            plain.request.kind, reviewed.request.kind,
            "and the review must still reach the request",
        );
        assert_eq!(
            plain.plan_notes,
            Vec::<String>::new(),
            "no review, nothing to note",
        );
    }

    /// A review's notes reach the resolved plan. They are what the create
    /// response shows a client about a plan it did not get, so dropping them
    /// on the floor between `apply_plan_review` and `ResolvedPlan` would be
    /// silent.
    #[tokio::test]
    async fn a_reviews_notes_reach_the_resolved_plan() {
        let state = resolver_state();
        let source = dolby_vision_p8_file();
        // A body that claims more than its own caps support: the review
        // overrides it and says so. Same fixture as
        // `a_create_that_claims_more_than_its_caps_gets_the_servers_plan_and_a_note`,
        // because a note is what that case exists to produce.
        let review = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &source,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(!review.notes.is_empty(), "fixture must produce notes");
        let expected = review.notes.clone();
        let resolved = resolve_plan(
            PlanInputs {
                state: &state,
                user_id: 7,
                file_id: source.id,
                source: Some(&source),
                network_prior: None,
            },
            Some(review),
            CreateSession {
                playback_id: "player-a".into(),
                copy: Some(true),
                preserve_dolby_vision: Some(true),
                ..bare_create()
            },
        )
        .await
        .expect("resolve with notes");
        assert_eq!(resolved.plan_notes, expected);
    }

    /// The server's re-derivation reaches the session, and the body never
    /// does.
    ///
    /// `review_client_plan` is well covered; this is the wire between it and
    /// the request that gets built, and each field it carries fails
    /// differently if the wire is cut. `preserve_dolby_vision` reverts to the
    /// client's own echo — the pre-caps-v2 bug where a blanket `dv=1` got
    /// Safari a preserved Profile 7 it could not decode. `hdr10` reverts to a
    /// claim the caps did not support. And `convert_dolby_vision` is never set
    /// at all: `into_request` leaves it false because a client has no way to
    /// ask for a conversion, so this assignment is the *only* one, and without
    /// it the whole milestone is dead code that ships and does nothing.
    #[test]
    fn the_reconciled_plan_reaches_the_request_and_the_body_cannot() {
        let body = CreateSession {
            playback_id: "p".into(),
            copy: Some(true),
            // The client asks for the opposite of everything the review says.
            preserve_dolby_vision: Some(false),
            hdr10: Some(false),
            ..bare_create()
        };
        let mut request = body.into_request(1, 0);
        let asked = request.kind;
        assert!(
            matches!(
                asked,
                crate::transcode::SessionKind::Copy {
                    convert_dolby_vision: false,
                    ..
                }
            ),
            "a client cannot ask to be handed a conversion: {asked:?}"
        );

        // …including a client that asks for everything adjacent to one. The
        // conversion is not a wire field, so no combination of body values can
        // produce it — which is what makes the assignment below the only one.
        let eager = CreateSession {
            playback_id: "p".into(),
            copy: Some(true),
            preserve_dolby_vision: Some(true),
            hdr10: Some(true),
            ..bare_create()
        }
        .into_request(1, 0);
        assert!(
            matches!(
                eager.kind,
                crate::transcode::SessionKind::Copy {
                    convert_dolby_vision: false,
                    preserve_dolby_vision: true,
                    ..
                }
            ),
            "asking to preserve is not asking to convert: {:?}",
            eager.kind
        );

        let notes = apply_plan_review(
            &mut request,
            PlanReview {
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                hdr10: true,
                notes: vec!["a note".to_owned()],
                mismatched: false,
            },
        );
        let crate::transcode::SessionKind::Copy {
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } = request.kind
        else {
            panic!("a copy request stays a copy request");
        };
        assert!(preserve_dolby_vision, "the server's answer, not the body's");
        assert!(
            convert_dolby_vision,
            "the only assignment there is — without it the conversion never runs"
        );
        assert!(request.hdr10, "and the same for the HDR10 request");
        assert_eq!(notes, vec!["a note".to_owned()]);

        // A transcode request has no Dolby Vision fields to carry, and must
        // still take the notes and the HDR10 answer.
        let mut transcode = CreateSession {
            playback_id: "p".into(),
            height: Some(1080),
            ..bare_create()
        }
        .into_request(1, 1080);
        apply_plan_review(
            &mut transcode,
            PlanReview {
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                hdr10: true,
                notes: Vec::new(),
                mismatched: false,
            },
        );
        assert!(matches!(
            transcode.kind,
            crate::transcode::SessionKind::Transcode { .. }
        ));
        assert!(transcode.hdr10);
    }

    /// A Profile 7 title reaches a Profile-8 client as a conversion, and the
    /// conversion follows the preservation wherever that goes.
    ///
    /// The two flags are set together and clamped together. `convert` is not a
    /// wire field — a client has no way to be right or wrong about it — so it
    /// is never compared against anything the body asked for; it follows
    /// `preserve_dolby_vision`, and the `compatible_hdr_base` retry is the
    /// case that makes it matter. A client that decoded Dolby Vision, failed
    /// on this title, and asked for the plain HDR10 base must not be handed a
    /// *converted* Dolby Vision stream instead: that is the same stream it
    /// just failed on, wearing a different profile number.
    #[test]
    fn a_declined_dolby_vision_plan_declines_the_converted_kind_too() {
        let mut file = dolby_vision_p8_file();
        file.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);

        // The ordinary answer for a client that takes 8 and not 7.
        let converted = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(converted.convert_dolby_vision);
        assert!(
            converted.preserve_dolby_vision,
            "there is nothing to convert in a stream the filter removed"
        );

        // The client declines Dolby Vision by not asking for it. The
        // conversion goes with it.
        let declined = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false,
            false,
            NOW_MS,
        );
        assert!(!declined.preserve_dolby_vision);
        assert!(
            !declined.convert_dolby_vision,
            "a client that declined Dolby Vision declined the converted kind"
        );

        // And the same through the named override, which is the path Apple's
        // retry actually takes.
        let overridden = review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                ..Default::default()
            }),
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(!overridden.preserve_dolby_vision);
        assert!(!overridden.convert_dolby_vision);
    }

    /// Apple's `forceCompatibleHDRBase` retry, and why it needs a name.
    ///
    /// The client decoded the Dolby Vision stream, failed on this title, and
    /// is asking for the HDR10-compatible base. Its caps are still correct —
    /// the device really does take Profile 8 — so the re-derivation says
    /// "preserve", and without a named override the create would hand it back
    /// exactly the stream it just failed on, forever. The override lowers the
    /// plan and leaves its own reason, and it is *not* counted as a mismatch:
    /// nothing disagreed, the client asked for something legitimate.
    #[test]
    fn the_compatible_base_override_lowers_the_plan_without_being_a_mismatch() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                force: None,
            }),
            &file,
            &capable_node(),
            false, // the retry's body echoes the lowered plan
            true,
            NOW_MS,
        );

        assert!(!review.preserve_dolby_vision);
        assert!(
            !review.mismatched,
            "a named override is an explanation, not a disagreement"
        );
        assert_eq!(
            review.notes,
            vec!["override compatible_hdr_base: Dolby Vision declined by the client".to_owned()]
        );
    }

    /// The same override on a title the plan was never going to send as
    /// Dolby Vision.
    ///
    /// Harmless — the plan is already what the client is asking for — but
    /// worth a line, because a client retrying the compatible base on an
    /// HDR10 title is a client chasing a failure that came from somewhere
    /// else, and the next person debugging it should not have to guess that.
    #[test]
    fn the_compatible_base_override_says_so_when_it_had_nothing_to_decline() {
        let mut file = hls_file(Vec::new());
        file.hdr = Some("hdr10".into());
        file.hdr_format = Some("HDR10".into());
        let review = review_client_plan(
            &no_dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                force: None,
            }),
            &file,
            &capable_node(),
            false,
            false,
            NOW_MS,
        );
        assert!(!review.preserve_dolby_vision);
        assert!(!review.mismatched);
        assert_eq!(
            review.notes,
            vec![
                "override compatible_hdr_base had nothing to decline: the plan was \
                 not Dolby Vision"
                    .to_owned()
            ]
        );
    }

    /// The quality menu is fed *into* the derivation, not compared against
    /// it.
    ///
    /// A forced transcode is a viewer's answer to a different question — how
    /// big should this stream be — and the plan it produces is still the
    /// server's. It leaves a note anyway: a viewer who forced a transcode and
    /// then read an SDR badge deserves those two facts next to each other
    /// rather than a support thread.
    #[test]
    fn a_forced_rung_produces_the_servers_plan_for_that_force_and_says_which() {
        let file = dolby_vision_p8_file();
        let review = review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: None,
                force: Some("transcode".to_owned()),
            }),
            &file,
            &capable_node(),
            false,
            true,
            NOW_MS,
        );
        assert!(
            !review.preserve_dolby_vision,
            "a transcode cannot preserve Dolby Vision; it re-encodes"
        );
        assert!(!review.mismatched);
        assert_eq!(review.notes, vec!["override force=transcode".to_owned()]);
    }

    /// The `hdr10` field is a claim about the CLIENT, and the review checks
    /// it against the client's own document — not against what this node or
    /// this source can do.
    ///
    /// Keeping those separate matters: `hdr10_grade_for` still refuses the
    /// rung for any source, height or build that did not prove the chain, and
    /// folding that refusal in here would report a "mismatch" every time a
    /// perfectly honest client asked for a rung this particular title cannot
    /// have.
    #[test]
    fn the_hdr10_claim_is_checked_against_the_document_not_against_the_node() {
        let file = dolby_vision_p8_file();
        // A node that proved nothing at all: no RPU render, no HDR10 chain.
        let bare = plurx_core::playback::RenderCaps::proven(false);
        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &bare,
            false,
            true,
            NOW_MS,
        );
        assert!(
            review.hdr10,
            "the document presents PQ on hevc, so the claim stands; whether this \
             node can honour it is `hdr10_grade_for`'s question, asked later"
        );

        // …and a client whose document does not present PQ cannot claim it,
        // however capable the node.
        let review = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false,
            true,
            NOW_MS,
        );
        assert!(!review.hdr10);
        assert!(review.mismatched);
        assert!(
            review
                .notes
                .iter()
                .any(|note| note.contains("client asked hdr10=true, server derived false")),
            "{:?}",
            review.notes
        );
    }

    /// The build label is what every create log line is keyed by, so it has
    /// to be right for the population it is counting — and it is assembled
    /// from strings a caller chose.
    #[test]
    fn the_build_label_prefers_the_document_and_bounds_every_source() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            axum::http::HeaderValue::from_str(&"M".repeat(400)).expect("ascii"),
        );

        let named = caps_v2(
            r#"{"v":2,"client":{"kind":"ios","build":"86"},
                "video":[],"audio":[],"containers":[]}"#,
        );
        assert_eq!(client_build_label(Some(&named), &headers), "ios/86");

        // A document with a client block that says nothing falls through to
        // the header, same as no document at all.
        let anonymous = caps_v2(r#"{"v":2,"client":{},"video":[],"audio":[],"containers":[]}"#);
        for caps in [Some(&anonymous), None] {
            let label = client_build_label(caps, &headers);
            assert_eq!(
                label.chars().count(),
                48,
                "an unbounded header does not belong in a log line: {label}"
            );
        }

        assert_eq!(client_build_label(None, &HeaderMap::new()), "unknown");

        // The body is exactly as caller-controlled as the header, and a
        // newline inside it is how one log line becomes two forged ones. The
        // early return for a named client must not skip the bounding.
        let hostile = caps_v2(
            "{\"v\":2,\"client\":{\"kind\":\"ios\\n2026-08-30 WARN plan_mismatch: forged\",\
             \"build\":\"86\"},\"video\":[],\"audio\":[],\"containers\":[]}",
        );
        let label = client_build_label(Some(&hostile), &headers);
        assert!(
            !label.contains('\n') && !label.chars().any(char::is_control),
            "{label:?}"
        );
        assert!(label.chars().count() <= 48 * 2 + 1, "{label:?}");

        // A client block that is nothing but control characters has no
        // printable content, so it falls through rather than logging a blank.
        let blank = caps_v2(
            "{\"v\":2,\"client\":{\"kind\":\"\\n\\t\",\"build\":\"\"},\
             \"video\":[],\"audio\":[],\"containers\":[]}",
        );
        assert_eq!(
            client_build_label(Some(&blank), &HeaderMap::new()),
            "unknown"
        );
    }

    /// The client may always ask for LESS than the plan allows.
    ///
    /// This is the direction that keeps Apple's compatible-base retry from
    /// becoming an infinite loop even before that client learns to send
    /// `overrides.compatible_hdr_base` — and it is not a hole in E4, because
    /// the direction E4 exists to close is the client claiming MORE than its
    /// own document supports. A viewer declining Dolby Vision gets a stream
    /// that plays; a client handed Dolby Vision it cannot decode gets black.
    #[test]
    fn a_client_may_decline_the_plan_but_not_exceed_it() {
        let file = dolby_vision_p8_file();
        let declined = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false, // "don't preserve Dolby Vision, I just failed on it"
            false,
            NOW_MS,
        );
        assert!(
            !declined.preserve_dolby_vision,
            "the retry must not be handed back the stream it failed on"
        );
        assert!(!declined.mismatched, "declining is not disagreeing");
        assert!(declined.notes.is_empty(), "{:?}", declined.notes);

        // The other direction is still clamped, logged and reported.
        let exceeded = review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        assert!(!exceeded.preserve_dolby_vision);
        assert!(exceeded.mismatched);
    }

    /// The shape every real create has, and the one the counters have to stay
    /// quiet for.
    ///
    /// `hdr10` is a per-TITLE request — the web sets it only when the
    /// decision it is acting on said `transcode` + `hdr10`. Deriving it from
    /// the caps document alone would set it true on every create from any
    /// PQ-capable client, including every SDR title: the ladder ceiling would
    /// move for requests that never asked, and the mismatch counter would sit
    /// permanently off zero, which is the one thing that would make it
    /// useless.
    #[test]
    fn an_sdr_title_from_a_pq_capable_client_is_not_a_mismatch() {
        let mut file = dolby_vision_p8_file();
        file.hdr = None;
        file.hdr_format = None;

        let review = review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            false, // the body carries no preserve_dolby_vision…
            false, // …and no hdr10, because there is nothing to ask for
            NOW_MS,
        );
        assert!(!review.hdr10, "the client asked for no HDR10 rung");
        assert!(!review.preserve_dolby_vision);
        assert!(
            !review.mismatched,
            "an ordinary SDR create must not report a plan mismatch"
        );
        assert!(review.notes.is_empty(), "{:?}", review.notes);

        // The same client asking for the rung on a title that has one is
        // granted it, because its document backs the claim.
        let mut hdr10 = dolby_vision_p8_file();
        hdr10.hdr = Some("hdr10".into());
        hdr10.hdr_format = Some("HDR10".into());
        let asked = review_client_plan(
            &dolby_vision_client(),
            None,
            &hdr10,
            &capable_node(),
            false,
            true,
            NOW_MS,
        );
        assert!(asked.hdr10);
        assert!(!asked.mismatched);
    }

    /// The counters are the fleet's only view of this, so something has to
    /// prove they move — and that they cannot report more outcomes than
    /// reviews.
    #[test]
    fn every_review_is_counted_and_the_parts_never_exceed_the_total() {
        let before = plan_derivation::snapshot();
        let file = dolby_vision_p8_file();

        review_client_plan(
            &dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        review_client_plan(
            &no_dolby_vision_client(),
            None,
            &file,
            &capable_node(),
            true,
            false,
            NOW_MS,
        );
        review_client_plan(
            &dolby_vision_client(),
            Some(&CreateOverrides {
                compatible_hdr_base: Some(true),
                force: Some("transcode".to_owned()),
            }),
            &file,
            &capable_node(),
            false,
            false,
            NOW_MS,
        );

        // Lower bounds, for the reason given in
        // `a_create_that_sends_no_caps_document_still_gets_a_review`: these are
        // process-global counters and this binary's tests run in parallel, so
        // an exact delta is a flake wearing a regression's name. What survives
        // the trade is what the test is for — every review counts itself, the
        // exceeding one counts a mismatch, the overridden one counts an
        // override — plus the invariant in the name, which holds whoever else
        // is counting: neither part can exceed the total.
        let after = plan_derivation::snapshot();
        assert!(after.2 - before.2 >= 3, "one `rederived` per review");
        assert!(after.3 - before.3 >= 1, "the exceeding review mismatched");
        assert!(
            after.4 - before.4 >= 1,
            "the review carrying two overrides counted itself overridden"
        );
        assert!(after.3 <= after.2 && after.4 <= after.2);
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
            control_sequence: None,
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
            caps: None,
            overrides: None,
        }
        .into_request(7, 1080);

        assert_eq!(request.audio_offset_ms, 15_000);
        assert_eq!(request.file_id, 7);
    }

    #[test]
    fn bitmap_fallback_still_carries_an_explicit_burn_request() {
        let request = CreateSession {
            control_sequence: None,
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
            caps: None,
            overrides: None,
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
            convert_dolby_vision: false,
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

    fn hls_context_with(codecs: &str, supplemental: Option<&str>) -> crate::transcode::HlsContext {
        crate::transcode::HlsContext {
            file_id: 1,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: codecs.to_owned(),
            supplemental_codecs: supplemental.map(str::to_owned),
            frame_rate: None,
        }
    }

    /// A real muxer init carrying the Profile 7 record a stripping copy leaves
    /// behind, plus the `dby1` brand movenc writes beside it.
    fn init_with_a_stale_dolby_vision_record() -> Vec<u8> {
        use plurx_core::fmp4::{FragmentReader, Unit};
        let feed = plurx_core::testfixtures::pipe("clean-cra");
        let mut reader = FragmentReader::new();
        reader.push(&feed);
        let Ok(Some(Unit::Init(mut init))) = reader.next_unit() else {
            panic!("the fixture opens with an init");
        };
        let record = plurx_core::fmp4::DolbyVisionRecord::new(7, 6, true, true, false, 0)
            .expect("a describable record");
        assert!(plurx_core::fmp4::set_dolby_vision_record(&mut init, &record).expect("insert"));
        init.bytes[8..12].copy_from_slice(b"dby1");
        init.bytes
    }

    /// The legacy muxer path's catch: a playlist that advertises no Dolby
    /// Vision must not serve an init that declares it.
    ///
    /// That path has ffmpeg's own HLS muxer write `init.mp4` straight to disk
    /// with no reader in between, so neither `copyseg`'s removal nor the VOD
    /// promotion's runs. `filter_units` took the layers out by NAL type and
    /// left the DOVI side data the record was written from, so the init
    /// declares Profile 7 with an enhancement layer over a stream carrying
    /// neither — which VideoToolbox honours by refusing the hardware path.
    #[test]
    fn an_init_declaring_dolby_vision_the_playlist_does_not_is_stripped() {
        plurx_core::testfixtures::require_ffmpeg();
        let mut init = init_with_a_stale_dolby_vision_record();
        assert!(
            init.windows(4).any(|f| f == b"dvcC" || f == b"dvvC"),
            "the fixture must carry a record for this test to mean anything"
        );

        let stripped = hls_context_with("hvc1.2.4.L153.90", None);
        assert!(strip_unadvertised_dolby_vision(&stripped, &mut init));
        assert!(
            !init.windows(4).any(|f| f == b"dvcC" || f == b"dvvC"),
            "the record survived a playlist that advertises plain HEVC"
        );
        // The brand goes with it: `dby1` over a sample entry with no record is
        // the contradictory init AVPlayer refuses outright, which is a harder
        // failure than the stutter.
        assert!(!init.windows(4).any(|f| f == b"dby1"));
        // Idempotent — an init with nothing to remove is not an error.
        assert!(!strip_unadvertised_dolby_vision(&stripped, &mut init));
    }

    /// …and a session that DOES advertise Dolby Vision keeps its record.
    ///
    /// Both spellings, because a preserved Profile 5 names Dolby Vision in
    /// `CODECS` with no `SUPPLEMENTAL-CODECS` beside it — so "no supplemental"
    /// is not the question, and asking it would strip the record off the one
    /// kind of session that genuinely needs it.
    #[test]
    fn an_advertised_dolby_vision_session_keeps_its_record() {
        plurx_core::testfixtures::require_ffmpeg();
        let original = init_with_a_stale_dolby_vision_record();

        // Converted, and preserved-8.1: named in SUPPLEMENTAL-CODECS.
        for supplemental in ["dvh1.08.06/db1p", "dvh1.08.06/db4h"] {
            let mut init = original.clone();
            let context = hls_context_with("hvc1.2.4.L153.90", Some(supplemental));
            assert!(!strip_unadvertised_dolby_vision(&context, &mut init));
            assert_eq!(init, original, "{supplemental}");
        }

        // Preserved Profile 5: named in CODECS, nothing supplemental.
        let mut init = original.clone();
        let profile5 = hls_context_with("dvh1.05.06", None);
        assert!(!strip_unadvertised_dolby_vision(&profile5, &mut init));
        assert_eq!(init, original);
    }

    /// Anything this reader does not account for byte-for-byte is served as it
    /// is, rather than as the reader's idea of it.
    ///
    /// Two shapes, and the second is the one that matters. Bytes that do not
    /// parse at all are refused by the parse itself. Bytes that parse but
    /// carry more than the init — a fragment appended, a trailing box this
    /// model does not walk — would have their tail silently dropped, because
    /// the rewrite replaces the whole buffer with what the reader accounted
    /// for. This rewrites a file a client is about to play; truncating it is a
    /// worse outcome than leaving the record in.
    #[test]
    fn anything_the_reader_does_not_fully_account_for_is_left_alone() {
        let context = hls_context_with("hvc1.2.4.L153.90", None);

        let mut garbage = b"\x00\x00\x00\x10ftypiso6\x00\x00\x00\x00not-a-moov".to_vec();
        let before = garbage.clone();
        assert!(!strip_unadvertised_dolby_vision(&context, &mut garbage));
        assert_eq!(garbage, before);

        let mut empty = Vec::new();
        assert!(!strip_unadvertised_dolby_vision(&context, &mut empty));
        assert!(empty.is_empty());

        // A real init with a stale record — which this DOES strip — plus a
        // trailing byte it does not account for, which must stop it.
        plurx_core::testfixtures::require_ffmpeg();
        let mut alone = init_with_a_stale_dolby_vision_record();
        assert!(
            strip_unadvertised_dolby_vision(&context, &mut alone),
            "the control: this init on its own is rewritten"
        );

        let mut with_tail = init_with_a_stale_dolby_vision_record();
        let expected = with_tail.clone();
        with_tail.extend_from_slice(b"trailing");
        let before_tail = with_tail.clone();
        assert!(
            !strip_unadvertised_dolby_vision(&context, &mut with_tail),
            "an init with bytes past its end must not be rewritten"
        );
        assert_eq!(with_tail, before_tail, "and not truncated");
        assert_ne!(with_tail.len(), expected.len());
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
        let dir = crate::test_tempdir().expect("segment directory");
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
        let dir = crate::test_tempdir().expect("segment directory");
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
        let dir = crate::test_tempdir().expect("segment directory");
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
