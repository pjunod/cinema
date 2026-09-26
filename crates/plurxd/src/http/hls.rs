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
    MAX_ADMITTED_MEDIA_BODY_LIFETIME, MEDIA_BODY_ACK_GRANULARITY, MEDIA_BODY_NO_PROGRESS_TIMEOUT,
    MEDIA_BODY_READ_BUFFER, OWNER_ASSIGNMENT_DEADLINE, PREPARED_SUCCESSOR_PLAN_NOTE,
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
/// What a client is told to wait before re-posting a start that was refused
/// because something it needs is still being produced.
const SIDECAR_PENDING_RETRY_AFTER_SECS: u64 = 5;
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

// split: begin hls-session-guard
#[path = "hls/session_guard.rs"]
mod session_guard;
pub use session_guard::*;
// split: end hls-session-guard

// split: begin hls-create
#[path = "hls/create.rs"]
mod create;
pub use create::*;
// split: end hls-create

// split: begin hls-relay
#[path = "hls/relay.rs"]
mod relay;
pub(crate) use relay::*;
// split: end hls-relay

// split: begin hls-release
#[path = "hls/release.rs"]
mod release;
pub use release::*;
// split: end hls-release

// split: begin hls-control
#[path = "hls/control.rs"]
mod control;
pub use control::*;
// split: end hls-control

// split: begin hls-preparation
#[path = "hls/preparation.rs"]
mod preparation;
pub(in crate::http) use preparation::*;
// split: end hls-preparation

// split: begin hls-status
#[path = "hls/status.rs"]
mod status;
use status::*;
// split: end hls-status

// split: begin hls-response
#[path = "hls/response.rs"]
mod response;
use response::*;
// split: end hls-response

// split: begin hls-playlist
#[path = "hls/playlist.rs"]
mod playlist;
pub use playlist::*;
// split: end hls-playlist

// split: begin hls-subtitle-playlist
#[path = "hls/subtitle_playlist.rs"]
mod subtitle_playlist;
pub use subtitle_playlist::*;
// split: end hls-subtitle-playlist

// split: begin hls-context
#[path = "hls/context.rs"]
mod context;
use context::*;
// split: end hls-context

// split: begin hls-subtitle-names
#[path = "hls/subtitle_names.rs"]
mod subtitle_names;
use subtitle_names::*;
// split: end hls-subtitle-names

// split: begin hls-playlist-text
#[path = "hls/playlist_text.rs"]
mod playlist_text;
use playlist_text::*;
// split: end hls-playlist-text

// split: begin hls-segment
#[path = "hls/segment.rs"]
mod segment;
pub use segment::*;
// split: end hls-segment

// split: begin hls-tests
#[cfg(test)]
#[path = "hls/tests.rs"]
mod tests;
// split: end hls-tests
