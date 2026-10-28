//! HTTP surface of a plurxd node: liveness/readiness plus the native `/api/v1`.
//!
//! The native API is JSON. An OpenAPI description will be generated from these
//! routes as they stabilize (clients on five platforms consume it). The
//! Plex-compat façade (a separate crate) and playback routes mount alongside
//! in later slices.

mod analysis;
mod analysis_reconcile;
pub(crate) mod annotations;
mod auth;
mod background_jobs;
mod preparation;
pub(crate) use auth::{LoginThrottle, PasswordCapacity};
mod browse;
pub(crate) use browse::index_refusal_summary;
pub(crate) mod chapter_thumbs;
mod cluster;
pub(crate) mod cluster_operations;
pub mod comingsoon;
pub use comingsoon::ComingSoonCache;
mod developer;
pub(crate) mod dto;
mod dv_disk;
pub(crate) mod dvr;
pub(crate) mod error;
pub(crate) mod extract;
pub(crate) mod file_grants;
pub(crate) mod playback_handoffs;
pub(crate) use extract::CacheOnlyAdminProofCache;
pub(crate) mod hls;
mod https_admin;
pub(crate) mod images;
pub(crate) mod index_coverage;
pub(crate) mod internal_activity;
pub(crate) mod internal_auth_revocation;
pub(crate) mod internal_clock;
pub(crate) mod internal_live_tv;
pub(crate) mod internal_media;
pub(crate) mod internal_media_sessions;
mod items;
mod jellyfin;
pub(crate) use jellyfin::{
    standard_port_app as jellyfin_standard_port_app, StandardPort as JellyfinStandardPort,
    StandardPortStatus as JellyfinStandardPortStatus,
};
mod keys;
mod libraries;
pub(crate) mod library_channels;
pub(crate) mod live_tv;
mod network;
mod offline;
pub(crate) mod peer_transport;
mod pgs_overlay;
mod photos;
mod plex;
pub(crate) mod plex_census;
pub(crate) mod publication;
mod reading;
pub(crate) mod remote;
mod scan;
pub(crate) mod scan_identity;
pub(crate) mod shared_artwork;
pub(crate) mod shared_captions;
pub(crate) mod shared_library;
pub(crate) mod shared_playback;
pub(crate) mod shared_receiver_assets;
#[cfg(test)]
mod shared_receiver_fixture;
#[allow(
    dead_code,
    reason = "candidate Shared Start router remains unregistered pending actual relay qualification"
)]
pub(crate) mod shared_receiver_ingress;
#[allow(dead_code)] // Owner remains unregistered until relay/control qualify.
pub(crate) mod shared_receiver_playback;
pub(crate) mod shared_source_assets;
pub(crate) mod shared_source_playback;
// Candidate projection helpers remain unregistered until Source/B lifecycle
// authority and actual delivery binding are qualified.
pub(crate) mod sharing;
mod sharing_decision_decode;
pub(crate) use sharing_decision_decode::bounded_decision_value;
pub(crate) mod sharing_direct_wire;
#[allow(dead_code)]
pub(crate) mod sharing_playback_wire;
#[allow(dead_code)]
mod sharing_start_decode;
pub(crate) use sharing_start_decode::{
    decode_source_start_response, validate_source_start_request, DecodedSourceHlsStart,
};
pub(crate) mod stream;
pub(crate) mod subtitle_downloads;
pub(crate) mod system;
mod trakt;
pub(crate) mod transcode_copies;

/// Unmodified User-Agent strings as the shipping clients actually send them.
/// Shared with the HTTP wire tests so the write-then-read proof and the
/// classifier unit tests cannot drift onto different inputs.
#[cfg(test)]
pub(crate) mod test_agents;

pub(crate) mod users;
mod watch;
pub(crate) mod web;

use axum::extract::State;
use axum::extract::{DefaultBodyLimit, MatchedPath};
use axum::http::{header, HeaderValue, Method, Request, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use crate::state::AppState;
use plurx_core::cluster::membership::LocalServingRole;
use serde::{Deserialize, Serialize};

const JSON_SHORT_DEADLINE: Duration = Duration::from_secs(30);
const JSON_LONG_DEADLINE: Duration = Duration::from_secs(300);
static HANDLER_DEADLINES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

const HTTP_ROUTE_GROUPS: [&str; 9] = [
    "auth", "home", "library", "item", "search", "playback", "settings", "cluster", "other",
];
const HTTP_STORE_CLASSES: [&str; 3] = ["local_read", "authority_read", "write"];
const HTTP_NODE_ROLES: [&str; 6] = [
    "standalone",
    "voter",
    "learner",
    "remote_authority",
    "fenced",
    "unknown",
];
const HTTP_ROUTE_BUCKETS: [(u64, &str); 12] = [
    (1_000_000, "0.001"),
    (5_000_000, "0.005"),
    (10_000_000, "0.01"),
    (25_000_000, "0.025"),
    (50_000_000, "0.05"),
    (100_000_000, "0.1"),
    (250_000_000, "0.25"),
    (500_000_000, "0.5"),
    (1_000_000_000, "1"),
    (2_500_000_000, "2.5"),
    (5_000_000_000, "5"),
    (10_000_000_000, "10"),
];

#[derive(Default)]
struct HttpRouteCell {
    count: AtomicU64,
    elapsed_nanos: AtomicU64,
    buckets: [AtomicU64; HTTP_ROUTE_BUCKETS.len()],
}

struct HttpRouteMetrics {
    store_reads: [AtomicU64; 9 * 3 * HTTP_NODE_ROLES.len()],
    routes: [HttpRouteCell; 9 * HTTP_NODE_ROLES.len()],
}

impl Default for HttpRouteMetrics {
    fn default() -> Self {
        Self {
            store_reads: std::array::from_fn(|_| AtomicU64::new(0)),
            routes: std::array::from_fn(|_| HttpRouteCell::default()),
        }
    }
}

impl HttpRouteMetrics {
    fn saturating_add(target: &AtomicU64, amount: u64) {
        let _ = target.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current != u64::MAX).then(|| current.saturating_add(amount))
        });
    }

    fn record(&self, group: usize, role: usize, counts: [u64; 3], elapsed: Duration) {
        for (class, count) in counts.into_iter().enumerate() {
            Self::saturating_add(
                &self.store_reads
                    [(group * HTTP_STORE_CLASSES.len() + class) * HTTP_NODE_ROLES.len() + role],
                count,
            );
        }
        let elapsed_nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        let cell = &self.routes[group * HTTP_NODE_ROLES.len() + role];
        Self::saturating_add(&cell.count, 1);
        Self::saturating_add(&cell.elapsed_nanos, elapsed_nanos);
        if let Some(index) = HTTP_ROUTE_BUCKETS
            .iter()
            .position(|(upper, _)| elapsed_nanos <= *upper)
        {
            Self::saturating_add(&cell.buckets[index], 1);
        }
    }

    fn render(&self) -> String {
        use std::fmt::Write;

        let mut out = String::from(
            "# HELP plurx_http_store_reads_total Replicated Store operations attributed to matched HTTP route group and serving role.\n\
             # TYPE plurx_http_store_reads_total counter\n",
        );
        for (group_index, group) in HTTP_ROUTE_GROUPS.iter().enumerate() {
            for (class_index, class) in HTTP_STORE_CLASSES.iter().enumerate() {
                for (role_index, role) in HTTP_NODE_ROLES.iter().enumerate() {
                    let count = self.store_reads[(group_index * HTTP_STORE_CLASSES.len()
                        + class_index)
                        * HTTP_NODE_ROLES.len()
                        + role_index]
                        .load(Ordering::Relaxed);
                    let _ = writeln!(
                        out,
                        "plurx_http_store_reads_total{{route_group=\"{group}\",class=\"{class}\",role=\"{role}\"}} {count}"
                    );
                }
            }
        }
        out.push_str(
            "# HELP plurx_http_route_seconds Matched HTTP route latency by bounded route group and serving role.\n\
             # TYPE plurx_http_route_seconds histogram\n",
        );
        for (group_index, group) in HTTP_ROUTE_GROUPS.iter().enumerate() {
            for (role_index, role) in HTTP_NODE_ROLES.iter().enumerate() {
                let cell = &self.routes[group_index * HTTP_NODE_ROLES.len() + role_index];
                let mut cumulative = 0_u64;
                for (bucket_index, (_, upper)) in HTTP_ROUTE_BUCKETS.iter().enumerate() {
                    cumulative = cumulative
                        .saturating_add(cell.buckets[bucket_index].load(Ordering::Relaxed));
                    let _ = writeln!(
                        out,
                        "plurx_http_route_seconds_bucket{{route_group=\"{group}\",role=\"{role}\",le=\"{upper}\"}} {cumulative}"
                    );
                }
                let count = cell.count.load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "plurx_http_route_seconds_bucket{{route_group=\"{group}\",role=\"{role}\",le=\"+Inf\"}} {count}"
                );
                let seconds = cell.elapsed_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
                let _ = writeln!(
                    out,
                    "plurx_http_route_seconds_sum{{route_group=\"{group}\",role=\"{role}\"}} {seconds:.9}"
                );
                let _ = writeln!(
                    out,
                    "plurx_http_route_seconds_count{{route_group=\"{group}\",role=\"{role}\"}} {count}"
                );
            }
        }
        out
    }
}

static HTTP_ROUTE_METRICS: LazyLock<HttpRouteMetrics> = LazyLock::new(HttpRouteMetrics::default);

fn http_route_group(path: &str) -> usize {
    // Axum supplies the registered MatchedPath, never the credential-bearing
    // request URI. Keep this exhaustive and exact: a new route is `other`
    // until its owner adds one fixed-cardinality entry here and the route
    // inventory test fails if a registered pattern is left unclassified.
    match path {
        // Authentication and identity administration.
        "/jellyfin"
        | "/jellyfin/"
        | "/jellyfin/System/Info/Public"
        | "/jellyfin/System/Info"
        | "/jellyfin/Users/Public"
        | "/jellyfin/Sessions/Capabilities"
        | "/jellyfin/Sessions/Capabilities/Full"
        | "/jellyfin/Users/AuthenticateByName"
        | "/jellyfin/Users/{user_id}"
        | "/jellyfin/Users/Me"
        | "/api/v1/me"
        | "/api/v1/setup"
        | "/api/v1/auth/login"
        | "/api/v1/auth/logout"
        | "/api/v1/users"
        | "/api/v1/users/{id}"
        // The device inventory and per-device revocation are account
        // administration on the same rows as login and logout, so they are
        // attributed here rather than opening a group of their own.
        | "/api/v1/me/devices"
        | "/api/v1/me/devices/{prefix}"
        | "/api/v1/users/{id}/devices"
        | "/api/v1/users/{id}/devices/{prefix}"
        | "/api/v1/keys"
        | "/api/v1/keys/{id}" => 0,

        // Home projections.
        "/api/v1/home/previews" | "/api/v1/hubs" | "/api/v1/features" | "/hubs/search" => 1,

        // Library management, catalogue pages and schedules.
        "/api/v1/libraries"
        | "/api/v1/libraries/{id}"
        | "/api/v1/libraries/{id}/items"
        | "/api/v1/libraries/{id}/schedule"
        | "/api/v1/libraries/{id}/scan"
        | "/api/v1/libraries/{id}/refresh"
        | "/api/v1/libraries/{id}/dv-conversion"
        | "/api/v1/libraries/{id}/dv-conversions"
        | "/api/v1/libraries/{id}/root-identity/reset"
        | "/api/v1/libraries/{id}/identity-repairs/preview"
        | "/api/v1/libraries/{id}/identity-repairs/{plan_id}"
        | "/api/v1/libraries/{id}/identity-repairs/{plan_id}/apply"
        | "/api/v1/library-channels"
        | "/api/v1/library-channels/"
        | "/api/v1/library-channels/preview"
        | "/api/v1/library-channels/guide"
        | "/api/v1/library-channels/{id}"
        | "/api/v1/library-channels/{id}/rebuild"
        | "/api/v1/library-channels/{id}/build"
        | "/api/v1/library-channels/{id}/favourite"
        | "/api/v1/library-channels/{id}/resolve"
        | "/api/v1/library-channels/{id}/sessions"
        | "/library"
        | "/library/sections"
        | "/library/sections/{id}/all" => 2,

        // Item metadata, artwork, reading, analysis and DVR catalogue rows.
        "/api/v1/items/{id}"
        | "/api/v1/files/{id}/subtitles/search"
        | "/api/v1/files/{id}/subtitles/download"
        | "/api/v1/files/{id}/subtitles/transcribe"
        | "/api/v1/files/{id}/subtitle-text-translation/options"
        | "/api/v1/files/{id}/subtitle-translation/options"
        | "/api/v1/files/{id}/subtitle-translation/assets"
        | "/api/v1/items/{id}/classification"
        | "/api/v1/items/{id}/photo"
        | "/api/v1/items/{id}/reanalyze"
        | "/api/v1/items/{id}/refresh-artwork"
        | "/api/v1/items/{id}/reading-state"
        | "/api/v1/items/{id}/annotations"
        | "/api/v1/items/{id}/watchlist"
        | "/api/v1/items/{id}/favorite"
        | "/api/v1/lists/{list}"
        | "/api/v1/files/{id}/preparation"
        | "/api/v1/files/{id}/analysis"
        | "/api/v1/files/{id}/dv-conversion"
        | "/api/v1/files/{id}/timeline-annotations/{kind}"
        | "/api/v1/dv-conversions"
        | "/api/v1/analysis/summary"
        | "/api/v1/analysis/coverage"
        | "/api/v1/analysis/jobs"
        | "/api/v1/analysis/jobs/{id}"
        | "/api/v1/analysis/jobs/{id}/retry"
        | "/api/v1/analysis/reopen"
        | "/api/v1/analysis/reconcile"
        | "/api/v1/dvr/status"
        | "/api/v1/dvr/overview"
        | "/api/v1/dvr/recordings"
        | "/api/v1/dvr/recordings/{id}"
        | "/api/v1/dvr/recordings/{id}/events"
        | "/api/v1/dvr/recordings/{id}/attention/ack"
        | "/api/v1/dvr/recordings/{id}/restore"
        | "/api/v1/dvr/attention"
        | "/api/v1/dvr/schedule"
        | "/api/v1/dvr/rules"
        | "/api/v1/dvr/rules/order"
        | "/api/v1/dvr/rules/{id}"
        | "/api/v1/dvr/reminders"
        | "/api/v1/dvr/reminders/{id}"
        | "/api/v1/dvr/reminders/{id}/ack"
        | "/jellyfin/Users/{user_id}/Views"
        | "/jellyfin/UserViews/GroupingOptions"
        | "/jellyfin/Library/VirtualFolders"
        | "/jellyfin/DisplayPreferences/{id}"
        | "/jellyfin/Items/{item_id}/Intros"
        | "/jellyfin/MediaSegments/{item_id}"
        | "/jellyfin/Items/{item_id}/LocalTrailers"
        | "/jellyfin/Items/{item_id}/SpecialFeatures"
        | "/jellyfin/UserViews"
        | "/jellyfin/Items/Latest"
        | "/jellyfin/Items/Resume"
        | "/jellyfin/UserItems/Resume"
        | "/jellyfin/Users/{user_id}/Items/Latest"
        | "/jellyfin/Users/{user_id}/Items/Resume"
        | "/jellyfin/Shows/Upcoming"
        | "/jellyfin/Items/{item_id}/Similar"
        | "/jellyfin/Shows/NextUp"
        | "/jellyfin/Items"
        | "/jellyfin/Users/{user_id}/Items"
        | "/jellyfin/Items/{item_id}/Images/{kind}"
        | "/jellyfin/Items/{item_id}/Images/{kind}/{index}"
        | "/jellyfin/Items/{item_id}"
        | "/jellyfin/Users/{user_id}/Items/{item_id}"
        | "/jellyfin/Shows/{item_id}/Seasons"
        | "/jellyfin/Shows/{item_id}/Episodes"
        | "/library/metadata/{key}"
        | "/library/metadata/{key}/children"
        | "/library/metadata/{key}/{kind}"
        | "/api/v1/images/{filename}"
        | "/api/v1/files/{id}/chapters/{index}/thumb"
        | "/api/v1/shared/imports/{import}/files/{locator}/chapters/{index}/thumb"
        | "/api/v1/shared/imports/{import}/files/{locator}/hls/sessions"
        | "/api/v1/shared/imports/{import}/files/{locator}/playback" => 3,

        // Search only; maintenance of the search index is a settings action.
        "/jellyfin/Search/Hints" | "/api/v1/search" | "/api/v1/search/related" | "/api/v1/search/settings" | "/search" => 4,

        // Playback decisions, control, media bodies and watch state.
        "/jellyfin/Items/{item_id}/PlaybackInfo"
        | "/jellyfin/Videos/{item_id}/{source_id}/Subtitles/{index}/{filename}"
        | "/jellyfin/Videos/{item_id}/{source_id}/Subtitles/{index}/{start_ticks}/{filename}"
        | "/jellyfin/Videos/{item_id}/stream"
        | "/jellyfin/Videos/{item_id}/{play_id}/hls/{*resource}"
        | "/jellyfin/Videos/{item_id}/{filename}"
        | "/jellyfin/Sessions/Logout"
        | "/jellyfin/Sessions/Playing"
        | "/jellyfin/Sessions/Playing/Progress"
        | "/jellyfin/Sessions/Playing/Stopped"
        | "/jellyfin/Sessions/Playing/Ping"
        | "/jellyfin/Videos/ActiveEncodings"
        | "/jellyfin/UserPlayedItems/{item_id}"
        | "/jellyfin/Items/{item_id}/Download"
        | "/jellyfin/Users/{user_id}/PlayedItems/{item_id}"
        | "/api/v1/items/{id}/progress"
        | "/api/v1/items/{id}/scrobble"
        | "/api/v1/items/{id}/unscrobble"
        | "/api/v1/files/{id}/decision"
        | "/api/v1/files/{id}/subtitle-translation"
        | "/api/v1/subtitle-translation/operations/{id}"
        | "/api/v1/subtitle-translation/operations/{id}/demands/{demand}"
        | "/api/v1/subtitle-translation/operations/{id}/stop"
        | "/api/v1/subtitle-translation/operations/{id}/retry"
        | "/api/v1/subtitle-translation/operations/{id}/failed-ranges"
        | "/api/v1/subtitle-translation/assets/{id}/manifest"
        | "/api/v1/subtitle-translation/assets/{id}/segments"
        | "/api/v1/subtitle-translation/assets/{id}/coverage"
        | "/api/v1/subtitle-translation/assets/{id}/segments/{segment}"
        | "/api/v1/files/{id}/audio-offset"
        | "/api/v1/files/{id}/offline-options"
        | "/api/v1/files/{id}/offline-packages"
        | "/api/v1/files/{id}/publication"
        | "/api/v1/files/{id}/direct"
        | "/api/v1/files/{id}/download"
        | "/api/v1/files/{id}/content"
        | "/api/v1/files/{id}/grants"
        | "/api/v1/grants/{id}"
        | "/api/v1/grants/{token}/content"
        | "/api/v1/playback-handoffs"
        | "/api/v1/playback-handoffs/redeem"
        | "/api/v1/playback-handoffs/self"
        | "/api/v1/playback-handoffs/self/context"
        | "/api/v1/playback-handoffs/self/next"
        | "/api/v1/playback-handoffs/{id}"
        | "/api/v1/files/{id}/stream.mp4"
        | "/api/v1/files/{id}/subs/{subtitle}"
        | "/api/v1/files/{id}/subs/{index}/overlay.json"
        | "/api/v1/files/{id}/subs/{index}/overlay/{generation}/objects/{object}"
        | "/api/v1/shared/imports/{import}/files/{locator}/subs/{subtitle}"
        | "/api/v1/shared/imports/{import}/files/{locator}/subs/{index}/overlay.json"
        | "/api/v1/shared/imports/{import}/files/{locator}/subs/{index}/overlay/{generation}/objects/{object}"
        | "/api/v1/shared/imports/{import}/files/{locator}/stream.mp4"
        | "/api/v1/shared/imports/{import}/files/{locator}/direct"
        | "/api/v1/files/{id}/hls/sessions"
        | "/api/v1/files/{id}/hls/continuous-sessions"
        | "/api/v1/files/{id}/hls/continuous-candidates"
        | "/api/v1/files/{id}/hls/start"
        | "/api/v1/offline/packages/{id}"
        | "/api/v1/offline/packages/{id}/lease"
        | "/api/v1/offline/packages/{id}/complete"
        | "/api/v1/offline/media/{token}/master.m3u8"
        | "/api/v1/offline/media/{token}/index.m3u8"
        | "/api/v1/offline/media/{token}/subs/{index}/{segment}"
        | "/api/v1/offline/media/{token}/{segment}"
        | "/api/v1/publication/{session}"
        | "/api/v1/publication/{session}/{*resource}"
        | "/api/v1/stream/{id}/status"
        | "/api/v1/hls/{session}/master.m3u8"
        | "/api/v1/hls/{session}/index.m3u8"
        | "/api/v1/hls/{session}/video.m3u8"
        | "/api/v1/hls/{session}/subs/{index}/index.m3u8"
        | "/api/v1/hls/{session}/subs/{index}/{segment}"
        | "/api/v1/hls/{session}/status"
        | "/api/v1/hls/{session}/quality-control"
        | "/api/v1/hls/{session}/quality-schedule"
        | "/api/v1/hls/{session}/quality-family"
        | "/api/v1/hls/{session}/control"
        | "/api/v1/hls/{session}"
        | "/api/v1/hls/{session}/{segment}"
        | "/api/v1/hls/{session}/{role}/{rendition}/{kind}/{object}"
        | "/api/v1/hls/{session}/{role}/{rendition}/index.m3u8"
        | "/api/v1/live-tv/readiness"
        | "/api/v1/live-tv/readiness/refresh"
        | "/api/v1/live-tv/channels"
        | "/api/v1/live-tv/channels/{channel}/intents"
        | "/api/v1/live-tv/channels/{channel}/sessions"
        | "/api/v1/live-tv/guide"
        | "/api/v1/live-tv/guide/readiness"
        | "/api/v1/live-tv/guide/refresh"
        | "/api/v1/live-tv/sessions/{capability}/master.m3u8"
        | "/api/v1/live-tv/sessions/{capability}/index.m3u8"
        | "/api/v1/live-tv/sessions/{capability}/status"
        | "/api/v1/live-tv/sessions/{capability}/keepalive"
        | "/api/v1/live-tv/sessions/{capability}/{segment}"
        | "/api/v1/live-tv/sessions/{capability}"
        | "/api/v1/live-tv/starts/{request_id}"
        | "/api/v1/live-tv/starts/{request_id}/resume"
        | "/library/parts/{file_id}/{mtime}/{name}"
        | "/photo/:/transcode"
        | "/:/timeline"
        | "/:/scrobble"
        | "/:/unscrobble" => 5,

        // Configuration, diagnostics, scans, public shell and maintenance.
        "/"
        | "/api/v1/server"
        | "/api/v1/server/node"
        | "/api/v1/settings"
        | "/api/v1/subtitle-transcription"
        | "/api/v1/subtitle-translation"
        | "/api/v1/subtitle-translation/models"
        | "/api/v1/subtitle-text-translation/config"
        | "/api/v1/subtitle-text-translation/models"
        | "/api/v1/subtitle-text-translation/models/install"
        | "/api/v1/subtitle-text-translation/models/verify"
        | "/api/v1/files/{id}/subtitles/{index}/translation"
        | "/api/v1/subtitle-text-translation/jobs/{id}"
        | "/api/v1/subtitle-text-translation/jobs/{id}/retry"
        | "/api/v1/subtitle-transcription/managed"
        | "/api/v1/subtitle-transcription/models"
        | "/api/v1/subtitle-transcription/models/{id}/install"
        | "/api/v1/subtitle-transcription/models/{id}/verify"
        | "/api/v1/subtitle-transcription/jobs/{id}"
        | "/api/v1/subtitle-provider"
        | "/api/v1/developer/readiness"
        | "/api/v1/developer/macos-video-processing/reprobe"
        | "/api/v1/system/video-processing/reprobe"
        | "/api/v1/scan"
        | "/api/v1/scan/status"
        | "/api/v1/scan/requests/{id}"
        | "/api/v1/activity"
        | "/api/v1/activity/detail"
        | "/api/v1/activity/sessions/{id}"
        | "/api/v1/activity/offline/{id}"
        | "/api/v1/activity/producer"
        | "/api/v1/activity/retained/{nonce}"
        | "/api/v1/activity/processes/{pid}"
        | "/api/v1/trakt/status"
        | "/api/v1/trakt/link"
        | "/api/v1/trakt/sync"
        | "/api/v1/system"
        | "/api/v1/system/https"
        | "/api/v1/system/https/certificate"
        | "/api/v1/system/logs"
        | "/api/v1/system/playback-events"
        | "/api/v1/system/library-shape"
        | "/api/v1/system/storage"
        | "/api/v1/system/transcoder"
        | "/api/v1/system/tone-map/census"
        | "/api/v1/system/tone-map/census/stop"
        | "/api/v1/system/tone-map/census/stills/{name}"
        | "/api/v1/system/search-index/rebuild"
        | "/api/v1/client-log"
        | "/api/v1/coming-soon"
        | "/api/v1/monarr/status"
        | "/assets/cluster-panel.js"
        | "/assets/playback-policy.js"
        | "/assets/playback-control.js"
        | "/assets/live-tv.js"
        | "/assets/library-channels.js"
        | "/assets/reader.js"
        | "/assets/reader.css"
        | "/assets/{*path}"
        | "/connect.svg"
        | "/manifest.webmanifest"
        | "/icons/{file}"
        | "/healthz"
        | "/readyz"
        | "/metrics"
        | "/download/plurx-android.apk"
        // The ordinary listener only refuses peer paths; peers use the
        // separate sharing listener.
        | "/sharing"
        | "/sharing/{*path}"
        | "/identity" => 6,

        // Cluster administration and authenticated internal transport.
        "/api/v1/cluster/nodes"
        | "/api/v1/cluster/status"
        | "/api/v1/cluster/work/storage-domains"
        | "/api/v1/cluster/jobs"
        | "/api/v1/cluster/jobs/{id}"
        | "/api/v1/cluster/jobs/{id}/cancel"
        | "/api/v1/cluster/jobs/{id}/retry"
        | "/api/v1/cluster/backups"
        | "/api/v1/cluster/ingress"
        | "/api/v1/cluster/media"
        | "/api/v1/cluster/media/offers"
        | "/api/v1/cluster/artwork/{filename}"
        | "/api/v1/cluster/join-tokens"
        | "/api/v1/cluster/learner-join-tokens"
        | "/api/v1/cluster/support-bundle"
        | "/api/v1/cluster/nodes/{node_id}/restart-preparation"
        | "/api/v1/cluster/nodes/{node_id}/promote"
        | "/api/v1/cluster/nodes/{node_id}/maintenance"
        | "/api/v1/cluster/election"
        | "/api/v1/cluster/leave"
        | "/api/v1/cluster/protocol/learner/activate"
        | "/api/v1/cluster/protocol/learner/deactivate"
        | "/api/v1/cluster/nodes/{node_id}"
        | "/api/v1/cluster/join/redeem"
        | "/api/v1/cluster/join/finalize"
        | "/api/v1/cluster/learner/join/redeem"
        | "/api/v1/cluster/learner/join/finalize"
        | "/internal/media/cache-copy/{recipe}/{digest}/{object}"
        | "/internal/media/fragment-index/{cache_key}"
        | "/internal/media/subtitle-source/{file_id}/{ordinal}/{format}" => 7,
        crate::diagnostics::PEER_PATH
        | internal_activity::PATH
        | internal_clock::PATH
        | cluster_operations::INTERNAL_PATH
        | internal_auth_revocation::PATH
        | crate::subtitle_ranges::PATH
        | crate::media_pool::SNAPSHOT_PATH
        | crate::media_pool::QUALITY_CANDIDATES_PATH
        | crate::media_pool::QUALITY_CANDIDATES_V2_PATH
        | crate::media_pool::OFFERS_PATH
        | crate::shared_cache::CANARY_PATH
        | crate::live_tv::SNAPSHOT_PATH
        | crate::live_tv::START_PATH
        | crate::live_tv::START_V2_PATH
        | crate::live_tv::ACTIVATE_PATH
        | crate::live_tv::RESOURCE_PATH
        | crate::live_tv::STOP_PATH
        | crate::live_tv::RETIRE_PATH
        | crate::live_tv::RESUME_PATH
        | crate::live_tv::START_STATE_PATH
        | crate::live_tv::GUIDE_PATH
        | crate::media_sessions::START_PATH
        | crate::media_sessions::ACTIVATE_PATH
        | crate::media_sessions::PREPARE_PATH
        | crate::media_sessions::ABORT_PATH
        | crate::media_sessions::RELAY_PATH
        | hls::QUALITY_CONTROL_PATH
        | hls::QUALITY_SCHEDULE_PATH
        | crate::media_sessions::CONTROL_PATH => 7,
        _ => 8,
    }
}

fn http_node_role(
    local_source: bool,
    watermark_source: bool,
    serving_role: Result<LocalServingRole, ()>,
) -> usize {
    if !local_source && watermark_source {
        3
    } else if !local_source {
        0
    } else {
        match serving_role {
            Ok(LocalServingRole::Voter) => 1,
            Ok(LocalServingRole::Learner) => 2,
            Ok(LocalServingRole::Unclustered) if !watermark_source => 0,
            Ok(LocalServingRole::Unclustered) => 5,
            Ok(LocalServingRole::Fenced) => 4,
            Err(()) => 5,
        }
    }
}

async fn http_store_attribution(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let group = request
        .extensions()
        .get::<MatchedPath>()
        .map_or(8, |path| http_route_group(path.as_str()));
    let raft = state.replication.metrics_handle().snapshot();
    let role = http_node_role(
        raft.local_source,
        raft.watermark_source,
        state.membership.local_serving_role().await.map_err(|_| ()),
    );
    let counts = plurx_core::store::HttpStoreOperationCounts::default();
    let watch_ack = plurx_core::store::HttpWatchWriteAck::default();
    let started_at = Instant::now();
    let mut response = plurx_core::store::scope_http_store_operations(
        counts.clone(),
        plurx_core::store::scope_http_watch_write_ack(watch_ack.clone(), next.run(request)),
    )
    .await;
    HTTP_ROUTE_METRICS.record(group, role, counts.snapshot(), started_at.elapsed());
    // K-04 M2: offer the acknowledged watch-write position so a client can
    // echo it as `X-Plurx-Read-After` to whichever node serves its next read,
    // or say that a watch write's position is unknown so it drops its echo.
    if let Some(offer) = watch_ack.offer() {
        let value = match offer {
            plurx_core::store::CommitIndexOffer::Index(index) => {
                axum::http::HeaderValue::from(index)
            }
            plurx_core::store::CommitIndexOffer::Unknown => {
                axum::http::HeaderValue::from_static(extract::COMMIT_INDEX_UNKNOWN)
            }
        };
        response
            .headers_mut()
            .insert(extract::COMMIT_INDEX_HEADER, value);
    }
    response
}

pub(crate) fn prometheus_http_store_attribution() -> String {
    HTTP_ROUTE_METRICS.render()
}

// ---------------------------------------------------------------------------
// Plex façade request census (C-07 M0).
//
// C-07's board row says the M0 census decides whether the façade gets paging
// work, and the plan's §5.0 says to read it out of the access log. There is no
// access log — that is C-08's finding and it is accurate — and no metric
// separates the façade from the native API either: `http_route_group` folds
// `/library/sections/{id}/all` in with `/api/v1/libraries` under `library`,
// and `/library/parts/{file_id}/{mtime}/{name}` in with the native media
// routes under `playback`. So the question "is any Plex-family client using
// this" is not answerable from what a node exposes today, and the census
// cannot be completed by reading harder.
//
// This is the instrument that makes it answerable, and it is all this change
// is: fourteen bounded handler labels times four bounded outcomes, recorded by
// a layer over the façade sub-router so nothing native can reach it. No
// handler is touched, no response changes, and no paging is built.
const PLEX_HANDLERS: [&str; 14] = [
    "root",
    "identity",
    "library_root",
    "sections",
    "section_all",
    "metadata",
    "children",
    "image",
    "part",
    "photo_transcode",
    "timeline",
    "scrobble",
    "unscrobble",
    "search",
];
/// `unauthorized` is separate from `error` deliberately, and is the plan's
/// three outcomes plus one. Every façade route but `root` and `identity`
/// requires a plurx token presented as `X-Plex-Token`, so a Kodi or
/// PlexKodiConnect box that is configured but not yet paired shows up here and
/// nowhere else — and for a census "somebody tried" is the single most
/// interesting thing that can happen.
const PLEX_OUTCOMES: [&str; 4] = ["ok", "not_found", "unauthorized", "error"];

/// The census cells, one per `(handler, outcome)`.
///
/// Held on `AppState` behind an `Arc`, not in a process-wide `static`, so a
/// router counts only the requests that router served. In production that is
/// the same thing — one state per process — but it is what lets a test assert
/// an exact delta: a process-global counter is also moved by every other test
/// in the binary that sends façade traffic, on libtest's parallel threads, and
/// the adversarial review of PR #462 measured that race failing 34 runs in
/// 400.
///
/// `cells` are this process's counts and keep the in-process meaning of
/// `plurx_plex_requests_total`. `ledger` is the durable census (C-07 §8.5,
/// [`plex_census`]): restored once at startup from the node's data directory,
/// it adds what earlier processes counted, so one read of
/// `plurx_plex_requests_since_census_total` covers every restart since the
/// census began. A router built without a restore (every test router) has no
/// ledger and exposes the process counter alone.
pub(crate) struct PlexCensus {
    cells: [AtomicU64; plex_census::CELLS],
    ledger: std::sync::OnceLock<plex_census::CensusLedger>,
}

impl Default for PlexCensus {
    fn default() -> Self {
        Self {
            cells: std::array::from_fn(|_| AtomicU64::new(0)),
            ledger: std::sync::OnceLock::new(),
        }
    }
}

/// The façade's registered templates, and nothing else.
///
/// `/search` and `/hubs/search` are one label because they are one handler
/// (`plex::search`) per API.md §20. A template that is not on this list cannot
/// reach this function: the layer is attached to the façade sub-router, so the
/// `None` arm means axum matched a façade route this list has not been told
/// about, which a test asserts cannot happen.
fn plex_handler_index(template: &str) -> Option<usize> {
    let name = match template {
        "/identity" => "identity",
        "/library" => "library_root",
        "/library/sections" => "sections",
        "/library/sections/{id}/all" => "section_all",
        "/library/metadata/{key}" => "metadata",
        "/library/metadata/{key}/children" => "children",
        "/library/metadata/{key}/{kind}" => "image",
        "/library/parts/{file_id}/{mtime}/{name}" => "part",
        "/photo/:/transcode" => "photo_transcode",
        "/:/timeline" => "timeline",
        "/:/scrobble" => "scrobble",
        "/:/unscrobble" => "unscrobble",
        "/search" | "/hubs/search" => "search",
        _ => return None,
    };
    PLEX_HANDLERS.iter().position(|known| *known == name)
}

fn plex_outcome_index(status: StatusCode) -> usize {
    match status.as_u16() {
        401 | 403 => 2,
        404 => 1,
        code if (200..400).contains(&code) => 0,
        _ => 3,
    }
}

impl PlexCensus {
    pub(crate) fn record(&self, handler: usize, status: StatusCode) {
        self.cells[handler * PLEX_OUTCOMES.len() + plex_outcome_index(status)]
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn prometheus(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_plex_requests_total Plex-compat façade requests by handler and outcome, since this process started. The census that decides whether the façade gets further work.\n\
             # TYPE plurx_plex_requests_total counter\n",
        );
        for (handler_index, handler) in PLEX_HANDLERS.iter().enumerate() {
            for (outcome_index, outcome) in PLEX_OUTCOMES.iter().enumerate() {
                let count = self.cells[handler_index * PLEX_OUTCOMES.len() + outcome_index]
                    .load(Ordering::Relaxed);
                out.push_str(&format!(
                    "plurx_plex_requests_total{{handler=\"{handler}\",outcome=\"{outcome}\"}} {count}\n"
                ));
            }
        }
        if let Some(ledger) = self.ledger.get() {
            out.push_str(&ledger.prometheus(&self.cells));
        }
        out
    }

    /// Restore the durable census from `data_dir` and write it back at once,
    /// marked as running, so a crash before the first periodic write still
    /// leaves a bounded gap. Never fails: an unusable file starts a new census
    /// with the reason logged. `floor_unix_s` is the earliest clock reading
    /// trusted (the build's source date); below it, or with no clock, nothing
    /// is written until a later write sees a trustworthy one. Called once,
    /// before the listener accepts; the returned start is what was logged.
    pub(crate) async fn restore_durable(
        &self,
        data_dir: &std::path::Path,
        now_unix_s: Option<u64>,
        floor_unix_s: u64,
    ) -> plex_census::CensusStart {
        let (ledger, start) =
            plex_census::CensusLedger::restore(data_dir, now_unix_s, floor_unix_s).await;
        ledger.log_start(&start);
        if self.ledger.set(ledger).is_err() {
            tracing::warn!("the Plex façade census was already restored in this process");
            return start;
        }
        if let Err(error) = self.flush_durable(now_unix_s, false).await {
            tracing::warn!(
                %error,
                "could not write the Plex façade census at startup; the periodic write retries"
            );
        }
        start
    }

    /// Write the durable census; `clean` records a clean stop and is final.
    /// `Ok(false)` when there is no ledger, the clean stop already landed, or
    /// the clock is unreadable or below the build's source date.
    pub(crate) async fn flush_durable(
        &self,
        now_unix_s: Option<u64>,
        clean: bool,
    ) -> std::io::Result<bool> {
        match self.ledger.get() {
            Some(ledger) => ledger.flush(&self.cells, now_unix_s, clean).await,
            None => Ok(false),
        }
    }

    #[cfg(test)]
    fn since_census(&self) -> Option<[u64; plex_census::CELLS]> {
        self.ledger
            .get()
            .map(|ledger| ledger.since_census(&self.cells))
    }
}

/// Counts one façade request. Attached to the façade sub-router only, so a
/// native `/api/v1/...` request never reaches it and cannot be mistaken for
/// Plex traffic — which is the whole reason this is a separate layer rather
/// than another label on `http_route_group`.
async fn plex_facade_census(
    State(census): State<std::sync::Arc<PlexCensus>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let handler = request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|path| plex_handler_index(path.as_str()));
    let response = next.run(request).await;
    if let Some(handler) = handler {
        census.record(handler, response.status());
    }
    response
}

/// `root` is not a façade route: `/` serves the web app to a browser and the
/// Plex capabilities container to a Plex client, from one handler. Counting it
/// by path would count every page load as Plex traffic, so `root_dispatch`
/// records it itself, on the branch it actually took.
pub(crate) const PLEX_ROOT_HANDLER: usize = 0;

// ---------------------------------------------------------------------------
// Request outcome and body delivery (observability baseline, C-08 M1).
//
// `http_store_attribution` above already times the matched route by bounded
// group and serving role. That timer stops when the handler returns its
// `Response`, so it is response-header latency and nothing else. Two things
// F-core-12 asks for are absent from it and are added here:
//
//   * the outcome of a request — no method and no status class exist anywhere
//     in the exposition today, so a node answering every request with 503 and
//     a node answering every request with 200 render identically;
//   * how long the response body took to deliver, and whether its consumer
//     stayed to the end. That is the "streaming-body completion" half of
//     F-core-12's distinction, and on a media route an abandoned body is the
//     ordinary end of a seek rather than an error, so the two endings are
//     counted apart.
//
// The route label is `http_route_group`, the same bounded classification the
// Store attribution uses, and not a per-route template. `http_route_group` is
// this repository's one exhaustive inventory of registered `MatchedPath`
// patterns, held by `registered_routes_reach_every_non_other_attribution_family`
// and by the unclassified-pattern assertion in the route inventory test; a
// second per-template table would be a second spelling of the same fact with
// no test holding the two together, and it would have to be maintained by
// hand because axum exposes no route list at runtime. The cost is real and is
// stated rather than hidden: a 5xx on one item route and a 5xx on another are
// one series here. The 5xx WARN line below carries the redacted target and the
// request id, and that is what takes an operator from a series to a request.
const HTTP_METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "other"];
const HTTP_STATUS_CLASSES: [&str; 5] = ["2xx", "3xx", "4xx", "5xx", "other"];
const HTTP_BODY_OUTCOMES: [&str; 2] = ["complete", "aborted"];
/// Body delivery spans two orders of magnitude more than header latency — a
/// JSON page finishes in milliseconds and a direct play runs for hours — which
/// is the whole reason it is a separate family from `plurx_http_route_seconds`
/// rather than more buckets on it.
const HTTP_BODY_BUCKETS: [(u64, &str); 6] = [
    (100_000_000, "0.1"),
    (1_000_000_000, "1"),
    (10_000_000_000, "10"),
    (60_000_000_000, "60"),
    (600_000_000_000, "600"),
    (3_600_000_000_000, "3600"),
];

fn add_saturating(target: &AtomicU64, amount: u64) {
    let _ = target.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (current != u64::MAX).then(|| current.saturating_add(amount))
    });
}

/// Bounded by construction: an unlisted method is `other`, so no caller can
/// mint a series by inventing a verb.
fn http_method_index(method: &Method) -> usize {
    match method.as_str() {
        "GET" => 0,
        "POST" => 1,
        "PUT" => 2,
        "PATCH" => 3,
        "DELETE" => 4,
        "HEAD" => 5,
        _ => 6,
    }
}

fn http_status_class_index(status: StatusCode) -> usize {
    match status.as_u16() {
        200..=299 => 0,
        300..=399 => 1,
        400..=499 => 2,
        500..=599 => 3,
        _ => 4,
    }
}

#[derive(Default)]
struct HttpBodyCell {
    complete: AtomicU64,
    aborted: AtomicU64,
    elapsed_nanos: AtomicU64,
    buckets: [AtomicU64; HTTP_BODY_BUCKETS.len()],
}

struct HttpRequestMetrics {
    requests: [AtomicU64; HTTP_ROUTE_GROUPS.len() * HTTP_METHODS.len() * HTTP_STATUS_CLASSES.len()],
    bodies: [HttpBodyCell; HTTP_ROUTE_GROUPS.len()],
}

impl Default for HttpRequestMetrics {
    fn default() -> Self {
        Self {
            requests: std::array::from_fn(|_| AtomicU64::new(0)),
            bodies: std::array::from_fn(|_| HttpBodyCell::default()),
        }
    }
}

impl HttpRequestMetrics {
    fn record_request(&self, group: usize, method: usize, status: usize) {
        add_saturating(
            &self.requests
                [(group * HTTP_METHODS.len() + method) * HTTP_STATUS_CLASSES.len() + status],
            1,
        );
    }

    fn record_body(&self, group: usize, elapsed: Duration, complete: bool) {
        let cell = &self.bodies[group];
        add_saturating(
            if complete {
                &cell.complete
            } else {
                &cell.aborted
            },
            1,
        );
        let elapsed_nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        add_saturating(&cell.elapsed_nanos, elapsed_nanos);
        if let Some(index) = HTTP_BODY_BUCKETS
            .iter()
            .position(|(upper, _)| elapsed_nanos <= *upper)
        {
            add_saturating(&cell.buckets[index], 1);
        }
    }

    fn render(&self) -> String {
        use std::fmt::Write;

        let mut out = String::from(
            "# HELP plurx_http_requests_total HTTP requests by bounded route group, method and status class.\n\
             # TYPE plurx_http_requests_total counter\n",
        );
        for (group_index, group) in HTTP_ROUTE_GROUPS.iter().enumerate() {
            for (method_index, method) in HTTP_METHODS.iter().enumerate() {
                for (status_index, status) in HTTP_STATUS_CLASSES.iter().enumerate() {
                    let count = self.requests[(group_index * HTTP_METHODS.len() + method_index)
                        * HTTP_STATUS_CLASSES.len()
                        + status_index]
                        .load(Ordering::Relaxed);
                    let _ = writeln!(
                        out,
                        "plurx_http_requests_total{{route_group=\"{group}\",method=\"{method}\",status=\"{status}\"}} {count}"
                    );
                }
            }
        }
        out.push_str(
            "# HELP plurx_http_body_seconds Response body delivery time, from the handler's response to the body's last frame or its drop.\n\
             # TYPE plurx_http_body_seconds histogram\n",
        );
        for (group_index, group) in HTTP_ROUTE_GROUPS.iter().enumerate() {
            let cell = &self.bodies[group_index];
            let mut cumulative = 0_u64;
            for (bucket_index, (_, upper)) in HTTP_BODY_BUCKETS.iter().enumerate() {
                cumulative =
                    cumulative.saturating_add(cell.buckets[bucket_index].load(Ordering::Relaxed));
                let _ = writeln!(
                    out,
                    "plurx_http_body_seconds_bucket{{route_group=\"{group}\",le=\"{upper}\"}} {cumulative}"
                );
            }
            let count = cell
                .complete
                .load(Ordering::Relaxed)
                .saturating_add(cell.aborted.load(Ordering::Relaxed));
            let _ = writeln!(
                out,
                "plurx_http_body_seconds_bucket{{route_group=\"{group}\",le=\"+Inf\"}} {count}"
            );
            let seconds = cell.elapsed_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
            let _ = writeln!(
                out,
                "plurx_http_body_seconds_sum{{route_group=\"{group}\"}} {seconds:.9}"
            );
            let _ = writeln!(
                out,
                "plurx_http_body_seconds_count{{route_group=\"{group}\"}} {count}"
            );
        }
        out.push_str(
            "# HELP plurx_http_bodies_total Response bodies by how they ended. An aborted body is a consumer that went away, which on a media route is the ordinary end of a seek and not an error.\n\
             # TYPE plurx_http_bodies_total counter\n",
        );
        for (group_index, group) in HTTP_ROUTE_GROUPS.iter().enumerate() {
            let cell = &self.bodies[group_index];
            for outcome in HTTP_BODY_OUTCOMES {
                let count = if outcome == "complete" {
                    cell.complete.load(Ordering::Relaxed)
                } else {
                    cell.aborted.load(Ordering::Relaxed)
                };
                let _ = writeln!(
                    out,
                    "plurx_http_bodies_total{{route_group=\"{group}\",outcome=\"{outcome}\"}} {count}"
                );
            }
        }
        out
    }
}

static HTTP_REQUEST_METRICS: LazyLock<HttpRequestMetrics> =
    LazyLock::new(HttpRequestMetrics::default);

/// At most one 5xx access line per route group per second, carrying how many
/// it swallowed since the last one.
///
/// Without this the line is a hazard rather than a help. A fenced node, a
/// learner outside its eligible routes, and a node in maintenance each refuse
/// **every** request with 503, so at any real request rate one WARN per
/// request would evict the whole bounded ring behind Settings → System → Logs
/// — the very place an admin goes to find out why the node is refusing. The
/// counters stay exact; only the prose is sampled, and the line says by how
/// much.
const ACCESS_LINE_INTERVAL_NANOS: u64 = 1_000_000_000;

struct AccessLineThrottle {
    /// Nanoseconds since `started_at` of the last emitted line, per group.
    /// Zero means none has been emitted yet.
    last_nanos: [AtomicU64; HTTP_ROUTE_GROUPS.len()],
    suppressed: [AtomicU64; HTTP_ROUTE_GROUPS.len()],
    started_at: Instant,
}

impl Default for AccessLineThrottle {
    fn default() -> Self {
        Self {
            last_nanos: std::array::from_fn(|_| AtomicU64::new(0)),
            suppressed: std::array::from_fn(|_| AtomicU64::new(0)),
            started_at: Instant::now(),
        }
    }
}

impl AccessLineThrottle {
    /// `Some(n)` when a line may be written, `n` being how many were suppressed
    /// since the last one; `None` when it must not be.
    fn admit(&self, group: usize) -> Option<u64> {
        // `max(1)` so the first request of the process, which may land on
        // nanosecond zero, still counts as "a line has been emitted".
        let now = (self
            .started_at
            .elapsed()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64)
            .max(1);
        let last = self.last_nanos[group].load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < ACCESS_LINE_INTERVAL_NANOS {
            self.suppressed[group].fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // Two threads can both pass the check at once. The loser writes a
        // second line rather than a wrong one, which is the safe way to lose
        // this race.
        self.last_nanos[group].store(now, Ordering::Relaxed);
        Some(self.suppressed[group].swap(0, Ordering::Relaxed))
    }
}

static ACCESS_LINES: LazyLock<AccessLineThrottle> = LazyLock::new(AccessLineThrottle::default);

/// Times one response body and classifies how it ended.
///
/// `size_hint` and `is_end_stream` are delegated to the inner body unchanged.
/// That is load-bearing rather than tidy: hyper derives `Content-Length` from
/// the size hint when the handler has not set the header itself, so a wrapper
/// that reported an unknown length would silently move range responses onto
/// chunked framing.
///
/// A body is `complete` when it handed the connection everything it promised,
/// and there are two ways to know that because hyper ends bodies two ways:
///
/// - a body polled to its end (`Poll::Ready(None)`). A chunked body always
///   ends like this: hyper has to see the end to write the terminating chunk.
/// - a body whose response head declared a length and which has yielded that
///   many data bytes. hyper **does not poll such a body to its end.** Once its
///   length encoder has written the declared bytes the dispatcher drops the
///   body unpolled (hyper 1.x `proto/h1/dispatch.rs`, the `!can_write_body()`
///   branch), and a stream-backed body — every direct play range and every HLS
///   segment plurx serves — reports `is_end_stream() == false` until it has
///   been polled to `None`. Without the byte count every fully delivered media
///   body would be counted as abandoned.
///
/// "Handed the connection" is not "acknowledged by the client": a connection
/// that dies after the last bytes were yielded but before they reached the
/// socket still counts `complete`. That is the same line the chunked case
/// draws, and the last point at which a body can observe its own delivery.
struct MeasuredBody {
    inner: axum::body::Body,
    metrics: &'static HttpRequestMetrics,
    group: usize,
    started_at: Instant,
    /// The number of body bytes the response head promised, when it promised
    /// a number (see `declared_body_length`).
    declared: Option<u64>,
    /// Data bytes this body has yielded to the connection so far.
    delivered: u64,
    recorded: bool,
}

impl MeasuredBody {
    fn new(
        metrics: &'static HttpRequestMetrics,
        group: usize,
        inner: axum::body::Body,
        declared: Option<u64>,
    ) -> Self {
        Self {
            inner,
            metrics,
            group,
            started_at: Instant::now(),
            declared,
            delivered: 0,
            recorded: false,
        }
    }

    /// Whether everything this body promised has been handed over: it is at
    /// its end, or it has yielded the length its response head declared.
    fn delivered_everything(&self) -> bool {
        http_body::Body::is_end_stream(&self.inner)
            || self
                .declared
                .is_some_and(|declared| self.delivered >= declared)
    }

    fn finish(&mut self, complete: bool) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        self.metrics
            .record_body(self.group, self.started_at.elapsed(), complete);
    }
}

impl http_body::Body for MeasuredBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::task::Poll;

        let this = self.get_mut();
        let polled = std::pin::Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.delivered = this.delivered.saturating_add(data.len() as u64);
                }
            }
            Poll::Ready(None) => this.finish(true),
            // A body that ended in an error did not deliver what it promised,
            // so it is counted beside the abandoned ones rather than as a
            // completed delivery.
            Poll::Ready(Some(Err(_))) => this.finish(false),
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for MeasuredBody {
    fn drop(&mut self) {
        // A body dropped short of what it promised is a consumer that went
        // away. One dropped after handing over everything — at its end, or
        // with its declared length yielded, which is how hyper lets go of
        // every fixed-length body — was delivered.
        let complete = self.delivered_everything();
        self.finish(complete);
    }
}

/// The number of body bytes a response's head promised, if it promised one.
///
/// A response to HEAD, and a 1xx, 204 or 304, promises none whatever its
/// `Content-Length` says: there the header describes the representation, and
/// hyper writes no body at all. Otherwise the header wins when present,
/// because it is what hyper frames the response by; when the handler set none,
/// hyper derives it from the body's exact size hint, so that is the fallback.
/// Neither means a chunked body, which is `complete` only at its end.
fn declared_body_length(
    head_request: bool,
    status: StatusCode,
    response: &Response,
) -> Option<u64> {
    if head_request
        || status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
    {
        return Some(0);
    }
    response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or_else(|| http_body::Body::size_hint(response.body()).exact())
}

/// Counts every request's outcome and times its body.
///
/// Layered outside `http_store_attribution`, and therefore outside
/// `cluster_capacity_gate`, so the 503s the gate produces for a learner, a
/// fenced node or a node in maintenance are counted. A refusal is a request,
/// and a node refusing everything with an uncounted 503 is exactly the failure
/// this family exists to show.
async fn http_request_metrics(request: Request<axum::body::Body>, next: Next) -> Response {
    measure_http_request(&HTTP_REQUEST_METRICS, &ACCESS_LINES, request, next).await
}

/// `http_request_metrics` over a metrics table the caller names, so a test can
/// own one and read exact counts without racing every other router test in
/// the binary for the process-wide cells.
async fn measure_http_request(
    metrics: &'static HttpRequestMetrics,
    access_lines: &'static AccessLineThrottle,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // `other` is the last group, and is what both ways of having no template —
    // the web fallback and a genuinely unmatched path — resolve to.
    let group = request
        .extensions()
        .get::<MatchedPath>()
        .map_or(HTTP_ROUTE_GROUPS.len() - 1, |path| {
            http_route_group(path.as_str())
        });
    let method = http_method_index(request.method());
    let head_request = request.method() == Method::HEAD;
    let target = safe_trace_target(request.uri());
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let started_at = Instant::now();
    let response = next.run(request).await;
    let status = response.status();
    metrics.record_request(group, method, http_status_class_index(status));
    if status.is_server_error() {
        // Only 5xx, and at most one line per route group per second.
        // `logbuf::LogBuffer` is a bounded ring and it is the only log the
        // product itself can show; a line per successful request would evict
        // everything else on a node serving one HLS segment per second per
        // viewer, and a line per refusal would do the same on a fenced node.
        // The counters above carry the volume, this ring carries the
        // exceptions — and `also_suppressed` says what the sampling cost.
        if let Some(also_suppressed) = access_lines.admit(group) {
            tracing::warn!(
                target: "plurxd::http",
                route_group = HTTP_ROUTE_GROUPS[group],
                status = status.as_u16(),
                latency_ms = started_at.elapsed().as_millis() as u64,
                request_id = %request_id,
                path = %target,
                also_suppressed,
                "http request failed"
            );
        }
    }
    let declared = declared_body_length(head_request, status, &response);
    response.map(|body| axum::body::Body::new(MeasuredBody::new(metrics, group, body, declared)))
}

pub(crate) fn prometheus_http_request_metrics() -> String {
    HTTP_REQUEST_METRICS.render()
}

// ---------------------------------------------------------------------------
// Request ids (observability baseline, C-08 M2).

pub(crate) const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_REQUEST_ID: usize = 64;

/// An id a caller supplied is adopted only if it is a short, boring token.
///
/// Anything else is replaced rather than refused: the id is a correlation aid,
/// and refusing a request over a malformed log field would make a log field
/// load-bearing for service. The discarded value is never returned, logged or
/// stored anywhere — a caller that puts a bearer token in this header must not
/// get it written into journald or into the in-product log ring.
fn adopt_or_mint_request_id(incoming: Option<&HeaderValue>) -> HeaderValue {
    match incoming.and_then(|value| value.to_str().ok()) {
        Some(value)
            if !value.is_empty()
                && value.len() <= MAX_REQUEST_ID
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') =>
        {
            HeaderValue::from_str(value).unwrap_or_else(|_| mint_request_id())
        }
        _ => mint_request_id(),
    }
}

fn mint_request_id() -> HeaderValue {
    let mut buffer = [0_u8; uuid::fmt::Simple::LENGTH];
    let text = uuid::Uuid::new_v4().simple().encode_lower(&mut buffer);
    HeaderValue::from_str(text).expect("lowercase hex is a legal header value")
}

/// Gives every request an id, on the request, on its span, and on the response.
///
/// Outermost of all the layers, because everything inside it — the 5xx access
/// line, the `http_request` span and any handler — reads the id back out of
/// the request headers, and only this layer is allowed to decide what that
/// value is. The id is deliberately **not** a metric label: a per-request label
/// is unbounded, which is the single most common way a metrics system is
/// destroyed.
async fn http_request_id(mut request: Request<axum::body::Body>, next: Next) -> Response {
    let id = adopt_or_mint_request_id(request.headers().get(REQUEST_ID_HEADER));
    request.headers_mut().insert(REQUEST_ID_HEADER, id.clone());
    let mut response = next.run(request).await;
    // A handler that already answered with an id of its own keeps it.
    response
        .headers_mut()
        .entry(REQUEST_ID_HEADER)
        .or_insert(id);
    response
}

#[derive(Clone, Copy)]
enum DeadlineGroup {
    JsonShort,
    JsonLong,
}

impl DeadlineGroup {
    const fn index(self) -> usize {
        match self {
            Self::JsonShort => 0,
            Self::JsonLong => 1,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::JsonShort => "json_short",
            Self::JsonLong => "json_long",
        }
    }
}

async fn handler_deadline(
    request: Request<axum::body::Body>,
    next: Next,
    deadline: Duration,
    group: DeadlineGroup,
) -> Response {
    match tokio::time::timeout(deadline, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            HANDLER_DEADLINES[group.index()].fetch_add(1, Ordering::Relaxed);
            error::ApiError::TypedRetry {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "handler_deadline",
                message: "the server did not finish this request before its deadline; the request may still complete, so re-read state before retrying a mutation".to_owned(),
                retry_after_seconds: 2,
            }
            .into_response()
        }
    }
}

async fn json_short_deadline(request: Request<axum::body::Body>, next: Next) -> Response {
    handler_deadline(request, next, JSON_SHORT_DEADLINE, DeadlineGroup::JsonShort).await
}

async fn json_long_deadline(request: Request<axum::body::Body>, next: Next) -> Response {
    handler_deadline(request, next, JSON_LONG_DEADLINE, DeadlineGroup::JsonLong).await
}

pub(crate) fn prometheus_handler_deadlines() -> String {
    let mut out = String::from(
        "# HELP plurx_http_handler_deadlines_total Requests ended by the server-side handler deadline.\n\
         # TYPE plurx_http_handler_deadlines_total counter\n",
    );
    for group in [DeadlineGroup::JsonShort, DeadlineGroup::JsonLong] {
        out.push_str(&format!(
            "plurx_http_handler_deadlines_total{{group=\"{}\"}} {}\n",
            group.label(),
            HANDLER_DEADLINES[group.index()].load(Ordering::Relaxed)
        ));
    }
    out
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn desktop_auth_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/auth/login",
            post(auth::login).layer(DefaultBodyLimit::max(auth::MAX_LOGIN_BODY_BYTES)),
        )
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/me", get(auth::me))
}

pub fn router(state: AppState) -> Router {
    // A route belongs to exactly one deadline group. The grouping is applied
    // before the shared serving gate so media bodies and blocked GETs remain
    // unlimited while JSON work cannot occupy a request slot forever.
    let json_short = Router::new()
        .merge(sharing::admin_router(state.clone()))
        .merge(shared_library::viewer_router(state.clone()))
        .merge(shared_playback::viewer_router(state.clone()))
        .route("/server", get(system::server_info))
        .route("/server/node", get(system::server_node))
        .route("/me", get(auth::me))
        .route("/features", get(system::features))
        .route("/settings", get(system::get_settings))
        .route("/system/https", get(https_admin::get))
        .route("/system/https/certificate", get(https_admin::certificate))
        .route(
            "/subtitle-transcription",
            get(crate::subtitle_transcription::settings),
        )
        .route("/subtitle-provider", get(subtitle_downloads::settings))
        // Advisory only. The Developer section lists what must be true
        // before each switch is safe; this is the other half — what this
        // process can currently observe. Nothing reads it to decide
        // whether a switch may be flipped.
        .route("/developer/readiness", get(developer::readiness))
        .route(
            "/developer/macos-video-processing/reprobe",
            post(developer::reprobe_macos_video),
        )
        .route(
            "/system/video-processing/reprobe",
            post(developer::reprobe_video_processing),
        )
        .merge(library_channels::collection_router())
        .nest("/library-channels", library_channels::router())
        .nest("/dvr", dvr::router())
        .route("/live-tv/readiness", get(live_tv::readiness))
        .route("/live-tv/channels", get(live_tv::channels))
        .route("/live-tv/guide", get(live_tv::guide_document))
        .route("/live-tv/guide/readiness", get(live_tv::guide_readiness))
        .route("/scan/status", get(system::scan_status))
        .route("/activity", get(system::activity))
        .route("/activity/detail", get(system::activity_detail))
        .route(
            "/activity/sessions/{id}",
            axum::routing::delete(system::stop_session),
        )
        .route(
            "/activity/offline/{id}",
            axum::routing::delete(system::stop_offline_package),
        )
        .route(
            "/activity/producer",
            axum::routing::delete(system::stop_producer),
        )
        .route(
            "/activity/retained/{nonce}",
            axum::routing::delete(system::stop_retained_output),
        )
        .route(
            "/activity/processes/{pid}",
            axum::routing::delete(system::stop_process),
        )
        .route("/trakt/status", get(trakt::status))
        .route("/system", get(system::system_info))
        .route("/system/logs", get(system::logs))
        .route("/system/playback-events", get(system::playback_events))
        .route("/cluster/nodes", get(cluster::nodes))
        .route("/cluster/status", get(cluster_operations::aggregate))
        .route("/cluster/jobs", get(background_jobs::list))
        .route(
            "/cluster/work/storage-domains",
            get(background_jobs::storage_domains),
        )
        .route("/cluster/jobs/{id}", get(background_jobs::detail))
        .route("/cluster/ingress", get(cluster::ingress))
        .route("/cluster/media", get(internal_media::directory))
        // Any signed-in user can post a client-side playback error here so it
        // lands in the admin log (browsers that reject a stream produce no
        // server log on their own). A playback handoff token may too.
        .route("/client-log", post(playback_handoffs::scoped_client_log))
        // Users (admin)
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", put(users::update).delete(users::delete))
        // API keys (admin) — the machine credential. Managing keys is a
        // user action; USING one is not, and those routes take ScopedKey.
        .route("/keys", get(keys::list).post(keys::create))
        .route("/keys/{id}", delete(keys::delete))
        // Targeted scan — key-scoped, for other applications. Not under
        // /libraries/{id} on purpose: the caller knows a path, not a plurx
        // library id, and plurx resolving it is one less thing for two
        // applications to keep in sync.
        .route("/coming-soon", get(comingsoon::coming_soon))
        .route("/monarr/status", get(comingsoon::monarr_status))
        .route("/scan/requests/{id}", get(scan::request_status))
        // Libraries
        .route("/libraries", get(libraries::list))
        .route(
            "/libraries/{id}/identity-repairs/{plan_id}",
            get(scan_identity::status),
        )
        .route("/libraries/{id}/items", get(browse::list_items))
        // Browse
        .route("/items/{id}", get(browse::item_detail).patch(items::edit))
        .route("/files/{id}/dv-conversion", get(dv_disk::file_status))
        .route(
            "/files/{id}/chapters/{index}/thumb",
            get(chapter_thumbs::serve),
        )
        .route("/dv-conversions", get(dv_disk::status))
        .route("/files/{id}/preparation", get(preparation::status))
        .route("/analysis/summary", get(analysis::summary))
        .route(
            "/analysis/coverage",
            get(index_coverage::status).post(index_coverage::start),
        )
        .route("/analysis/jobs", get(analysis::jobs))
        .route("/analysis/jobs/{id}", get(analysis::job))
        .route("/hubs", get(browse::hubs))
        .route("/home/previews", get(browse::home_previews))
        .route("/search", get(browse::search))
        .route(
            "/search/related",
            get(crate::library_search::related_search),
        )
        .route(
            "/search/settings",
            get(crate::library_search::settings).put(crate::library_search::update_settings),
        )
        .route(
            "/items/{id}/classification",
            get(crate::library_search::get_classification)
                .put(crate::library_search::correct_classification),
        )
        // Annotations: scores and a person's watchlist and favorites.
        .route(
            "/items/{id}/annotations",
            get(annotations::item_annotations),
        )
        .route(
            "/items/{id}/watchlist",
            put(annotations::add_to_watchlist).delete(annotations::remove_from_watchlist),
        )
        .route(
            "/items/{id}/favorite",
            put(annotations::add_to_favorites).delete(annotations::remove_from_favorites),
        )
        .route("/lists/{list}", get(annotations::list_items))
        // Watch
        // A playback handoff token may report progress for its file's item.
        .route(
            "/items/{id}/progress",
            post(playback_handoffs::scoped_progress),
        )
        .route("/items/{id}/scrobble", post(watch::scrobble))
        .route("/items/{id}/unscrobble", post(watch::unscrobble))
        .route(
            "/items/{id}/reading-state",
            get(reading::get_state)
                .put(reading::put_state)
                .delete(reading::delete_state)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        // Playback. The decision also accepts a playback handoff token for
        // its own file (`playback_handoffs.rs`).
        .route(
            "/files/{id}/decision",
            get(playback_handoffs::scoped_decision)
                .post(playback_handoffs::scoped_decision_post)
                // A capabilities document is a short list of codecs and a
                // handful of learned limits; axum's 2 MiB default is four
                // orders of magnitude of headroom for an authenticated caller
                // to spend on a body the server has to walk. The same 64 KiB
                // the reading-state route uses is more than any real client
                // needs.
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/files/{id}/audio-offset", put(stream::set_audio_offset))
        .route("/files/{id}/offline-options", get(offline::options))
        .route("/files/{id}/grants", post(file_grants::mint))
        .route("/grants/{id}", delete(file_grants::revoke))
        // Playback handoff: mint with a bearer, redeem with the code alone,
        // poll the redemption, and let the redeemed token revoke itself.
        .route("/playback-handoffs", post(playback_handoffs::mint))
        .route("/playback-handoffs/redeem", post(playback_handoffs::redeem))
        .route(
            "/playback-handoffs/self",
            delete(playback_handoffs::revoke_self),
        )
        .route(
            "/playback-handoffs/self/context",
            get(playback_handoffs::context),
        )
        .route(
            "/playback-handoffs/self/next",
            post(playback_handoffs::advance_next),
        )
        .route("/playback-handoffs/{id}", get(playback_handoffs::status))
        .route(
            "/offline/packages/{id}",
            get(offline::package_status).delete(offline::delete_package),
        )
        .route("/offline/packages/{id}/lease", put(offline::put_lease))
        .route(
            "/offline/packages/{id}/complete",
            post(offline::complete_package),
        )
        .layer(axum::middleware::from_fn(json_short_deadline));

    let json_long = Router::new()
        .route(
            "/files/{id}/subtitles/search",
            get(subtitle_downloads::search),
        )
        // These handlers legitimately coordinate cluster writes, password
        // hashing, scans, or child work. Five minutes stays above their own
        // fences while still making a wedged request finite.
        .route("/setup", post(system::setup))
        .route(
            "/auth/login",
            post(auth::login).layer(DefaultBodyLimit::max(auth::MAX_LOGIN_BODY_BYTES)),
        )
        .route("/auth/logout", post(auth::logout))
        .route("/me/devices", get(users::list_my_devices))
        .route("/me/devices/{prefix}", delete(users::revoke_my_device))
        .route("/users/{id}/devices", get(users::list_user_devices))
        .route(
            "/users/{id}/devices/{prefix}",
            delete(users::revoke_user_device),
        )
        .route(
            "/subtitle-transcription",
            put(crate::subtitle_transcription::update_settings),
        )
        .route(
            "/subtitle-text-translation/config",
            get(crate::subtitle_text_translation::api::settings)
                .put(crate::subtitle_text_translation::api::update_settings),
        )
        .route(
            "/subtitle-text-translation/models",
            get(crate::subtitle_text_translation::api::models),
        )
        .route(
            "/subtitle-text-translation/models/install",
            post(crate::subtitle_text_translation::api::install),
        )
        .route(
            "/subtitle-text-translation/models/verify",
            post(crate::subtitle_text_translation::api::verify),
        )
        .route(
            "/files/{id}/subtitle-text-translation/options",
            get(crate::subtitle_text_translation::api::options),
        )
        .route(
            "/files/{id}/subtitles/{index}/translation",
            post(crate::subtitle_text_translation::api::create),
        )
        .route(
            "/subtitle-text-translation/jobs/{id}",
            get(crate::subtitle_text_translation::api::job),
        )
        .route(
            "/subtitle-text-translation/jobs/{id}/retry",
            post(crate::subtitle_text_translation::api::retry),
        )
        .route(
            "/subtitle-translation",
            get(crate::subtitle_translation::api::settings)
                .put(crate::subtitle_translation::api::update_settings),
        )
        .route(
            "/subtitle-translation/models",
            get(crate::subtitle_translation::api::models),
        )
        .route(
            "/files/{id}/subtitle-translation/options",
            get(crate::subtitle_translation::api::options),
        )
        .route(
            "/files/{id}/subtitle-translation",
            post(crate::subtitle_translation::api::create),
        )
        .route(
            "/files/{id}/subtitle-translation/assets",
            get(crate::subtitle_translation::api::assets),
        )
        .route(
            "/subtitle-translation/operations/{id}",
            get(crate::subtitle_translation::api::operation),
        )
        .route(
            "/subtitle-translation/operations/{id}/demands/{demand}",
            put(crate::subtitle_translation::api::demand)
                .delete(crate::subtitle_translation::api::release),
        )
        .route(
            "/subtitle-translation/operations/{id}/stop",
            post(crate::subtitle_translation::api::stop),
        )
        .route(
            "/subtitle-translation/operations/{id}/retry",
            post(crate::subtitle_translation::api::retry),
        )
        .route(
            "/subtitle-translation/operations/{id}/failed-ranges",
            get(crate::subtitle_translation::api::failed_ranges),
        )
        .route(
            "/subtitle-translation/assets/{id}/manifest",
            get(crate::subtitle_translation::api::manifest),
        )
        .route(
            "/subtitle-translation/assets/{id}/segments",
            get(crate::subtitle_translation::api::segments),
        )
        .route(
            "/subtitle-translation/assets/{id}/coverage",
            get(crate::subtitle_translation::api::coverage),
        )
        .route(
            "/subtitle-translation/assets/{id}/segments/{segment}",
            get(crate::subtitle_translation::api::segment),
        )
        .route(
            "/subtitle-transcription/managed",
            get(crate::subtitle_models::api::settings)
                .put(crate::subtitle_models::api::update_settings),
        )
        .route(
            "/subtitle-transcription/models",
            get(crate::subtitle_models::api::models),
        )
        .route(
            "/subtitle-transcription/models/{id}/install",
            post(crate::subtitle_models::install),
        )
        .route(
            "/subtitle-transcription/models/{id}/verify",
            post(crate::subtitle_models::verify),
        )
        .route(
            "/subtitle-transcription/jobs/{id}",
            get(crate::subtitle_models::api::job),
        )
        .route(
            "/files/{id}/subtitles/transcribe",
            get(crate::subtitle_models::api::options).post(crate::subtitle_models::generate),
        )
        .route("/settings", put(system::update_settings))
        .route(
            "/system/https",
            put(https_admin::put).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/subtitle-provider",
            put(subtitle_downloads::update_settings),
        )
        .route(
            "/files/{id}/subtitles/download",
            post(subtitle_downloads::download),
        )
        .route(
            "/live-tv/readiness/refresh",
            post(live_tv::refresh_readiness),
        )
        .route("/live-tv/guide/refresh", post(live_tv::refresh_guide))
        .route("/trakt/link", post(trakt::link).delete(trakt::unlink))
        .route("/trakt/sync", post(trakt::sync_now))
        // Membership control is admin-only. Join tokens remain the narrow
        // credential for role-specific peer admission.
        .route("/cluster/join-tokens", post(cluster::issue_join_token))
        .route(
            "/cluster/learner-join-tokens",
            post(cluster::issue_learner_join_token),
        )
        .route(
            "/cluster/support-bundle",
            get(cluster_operations::support_bundle),
        )
        .route(
            "/cluster/nodes/{node_id}/restart-preparation",
            post(cluster_operations::prepare_restart).delete(cluster_operations::cancel_restart),
        )
        .route(
            "/cluster/nodes/{node_id}/promote",
            post(cluster::promote_node),
        )
        .route(
            "/cluster/nodes/{node_id}/maintenance",
            post(cluster::enter_maintenance).delete(cluster::exit_maintenance),
        )
        .route("/cluster/election", post(cluster::force_election))
        .route(
            "/cluster/work/storage-domains",
            put(background_jobs::replace_storage_domains),
        )
        .route("/cluster/jobs/{id}/cancel", post(background_jobs::cancel))
        .route("/cluster/jobs/{id}/retry", post(background_jobs::retry))
        .route("/cluster/backups", post(crate::backup::create))
        .route("/cluster/leave", post(cluster::leave))
        .route(
            "/cluster/protocol/learner/activate",
            post(cluster::activate_learner_protocol),
        )
        .route(
            "/cluster/protocol/learner/deactivate",
            post(cluster::deactivate_learner_protocol),
        )
        .route("/cluster/nodes/{node_id}", delete(cluster::remove_node))
        .route("/cluster/join/redeem", post(cluster::redeem_join))
        .route("/cluster/join/finalize", post(cluster::finalize_join))
        .route(
            "/cluster/learner/join/redeem",
            post(cluster::redeem_learner_join),
        )
        .route(
            "/cluster/learner/join/finalize",
            post(cluster::finalize_learner_join),
        )
        .route("/system/library-shape", get(system::library_shape))
        .route("/system/storage", post(system::remeasure_storage))
        .route(
            "/system/transcoder",
            put(system::update_hardware_preference).post(system::optimize_transcoder),
        )
        .route(
            "/system/tone-map/census",
            get(system::tone_map_census).post(system::start_tone_map_census),
        )
        .route(
            "/system/tone-map/census/stop",
            post(system::stop_tone_map_census),
        )
        .route(
            "/system/tone-map/census/stills/{name}",
            get(system::tone_map_census_still),
        )
        .route(
            "/system/search-index/rebuild",
            post(system::rebuild_search_index),
        )
        .route("/scan", post(scan::scan))
        .route("/libraries", post(libraries::create))
        .route(
            "/libraries/{id}",
            put(libraries::update).delete(libraries::delete),
        )
        .route("/libraries/{id}/schedule", put(libraries::set_schedule))
        .route("/libraries/{id}/scan", post(libraries::scan))
        .route(
            "/libraries/{id}/identity-repairs/preview",
            post(scan_identity::preview),
        )
        .route(
            "/libraries/{id}/identity-repairs/{plan_id}/apply",
            post(scan_identity::apply),
        )
        .route("/libraries/{id}/refresh", post(libraries::refresh))
        .route("/libraries/{id}/dv-conversion", put(dv_disk::set_mode))
        .route(
            "/libraries/{id}/dv-conversions",
            post(dv_disk::queue_library),
        )
        .route(
            "/libraries/{id}/root-identity/reset",
            post(libraries::reset_root_identity),
        )
        .route("/items/{id}/reanalyze", post(items::reanalyze))
        .route("/items/{id}/refresh-artwork", post(items::refresh_artwork))
        .route("/files/{id}/analysis", post(analysis::request))
        .route("/files/{id}/dv-conversion", post(dv_disk::queue_file))
        .route(
            "/files/{id}/timeline-annotations/{kind}",
            put(analysis::set_manual_annotation).delete(analysis::discard_manual_annotation),
        )
        .route("/analysis/jobs/{id}", delete(analysis::cancel_job))
        .route("/analysis/jobs/{id}/retry", post(analysis::retry_job))
        .route("/analysis/reopen", post(analysis::reopen))
        .route("/analysis/reconcile", post(analysis_reconcile::reconcile))
        .route("/files/{id}/offline-packages", post(offline::create))
        .route("/files/{id}/publication", post(publication::open))
        .layer(axum::middleware::from_fn(json_long_deadline));

    // No deadline here. These bodies, blocked GETs, paced remuxes, live
    // playlists, and session creates own their deadlines at the subsystem.
    let media = Router::new()
        .route(
            "/live-tv/channels/{channel}/intents",
            post(live_tv::issue_start).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            "/live-tv/channels/{channel}/sessions",
            post(live_tv::start_session).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            "/live-tv/sessions/{capability}/master.m3u8",
            get(live_tv::master),
        )
        .route(
            "/live-tv/sessions/{capability}/index.m3u8",
            get(live_tv::playlist),
        )
        .route(
            "/live-tv/sessions/{capability}/status",
            get(live_tv::session_status),
        )
        .route(
            "/live-tv/sessions/{capability}/keepalive",
            put(live_tv::keepalive),
        )
        .route(
            "/live-tv/sessions/{capability}/{segment}",
            get(live_tv::segment),
        )
        .route(
            "/live-tv/sessions/{capability}",
            delete(live_tv::stop_session),
        )
        .route(
            "/live-tv/starts/{request_id}",
            delete(live_tv::retire_start).get(live_tv::start_state),
        )
        .route(
            "/live-tv/starts/{request_id}/resume",
            post(live_tv::resume_start),
        )
        .route(
            "/cluster/media/offers",
            post(internal_media::diagnostic_offers),
        )
        .route("/cluster/artwork/{filename}", get(images::serve_peer))
        .route("/items/{id}/photo", get(photos::serve))
        .route("/offline/media/{token}/master.m3u8", get(offline::master))
        .route("/offline/media/{token}/index.m3u8", get(offline::playlist))
        .route(
            "/offline/media/{token}/subs/{index}/{segment}",
            get(offline::subtitle),
        )
        .route("/offline/media/{token}/{segment}", get(offline::segment))
        // A playback handoff token may read its own file (direct play).
        .route("/files/{id}/direct", get(playback_handoffs::scoped_direct))
        .route("/files/{id}/download", get(stream::download))
        .route("/files/{id}/content", get(stream::book_content))
        .route("/grants/{token}/content", get(file_grants::content))
        .route("/publication/{session}", delete(publication::close))
        .route(
            "/publication/{session}/{*resource}",
            get(publication::resource),
        )
        .route("/files/{id}/stream.mp4", get(stream::stream_mp4))
        .route(
            "/files/{id}/subs/{index}/overlay.json",
            get(pgs_overlay::manifest),
        )
        .route(
            "/files/{id}/subs/{index}/overlay/{generation}/objects/{object}",
            get(pgs_overlay::object),
        )
        // How a progressive remux is doing. Session auth, not capability: the
        // id is the client's own playback id, so the check is that the asker
        // owns the stream.
        .route("/stream/{id}/status", get(stream::stream_status))
        // The final segment accepts both `0` (legacy) and `0.vtt` (the
        // documented sidecar URL); the handler validates the suffix/index.
        // A playback handoff token may read its own file's text tracks.
        .route(
            "/files/{id}/subs/{subtitle}",
            get(playback_handoffs::scoped_subtitles_vtt),
        )
        // Creating a stream spawns a process and supersedes its predecessor,
        // so it is a POST. The GET is a deprecated bridge over the same path.
        .route(
            "/files/{id}/hls/sessions",
            // A playback handoff token may create sessions for its own file.
            post(playback_handoffs::scoped_hls_create)
                // The web client now sends its capabilities document here as
                // well, so this route walks exactly what
                // `/files/{id}/decision` walks and inherits the same bound for
                // the same reason. A real create is under a kilobyte; a
                // browser holding the full 256 learned limits is about 40 KiB.
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/files/{id}/hls/continuous-candidates",
            post(hls::continuous_candidates).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/files/{id}/hls/continuous-sessions",
            post(hls::create_continuous).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/files/{id}/hls/start", get(hls::start))
        .route(
            "/hls/{session}/master.m3u8",
            get(hls::master_playlist_response),
        )
        .route("/hls/{session}/index.m3u8", get(hls::playlist))
        .route("/hls/{session}/video.m3u8", get(hls::video_playlist))
        .route(
            "/hls/{session}/subs/{index}/index.m3u8",
            get(hls::subtitle_playlist),
        )
        .route(
            "/hls/{session}/subs/{index}/{segment}",
            get(hls::subtitle_vtt),
        )
        // Before the `{segment}` catch-all in intent, though the router
        // prefers the static segment regardless of registration order.
        .route("/hls/{session}/status", get(hls::status))
        .route("/hls/{session}/quality-family", get(hls::continuous_family))
        .route(
            "/hls/{session}/control",
            post(hls::control).layer(DefaultBodyLimit::max(
                crate::playback_control::MAX_REQUEST_BYTES,
            )),
        )
        .route(
            "/hls/{session}/quality-control",
            post(hls::quality_control).layer(DefaultBodyLimit::max(hls::QUALITY_CONTROL_MAX_BYTES)),
        )
        .route(
            "/hls/{session}/quality-schedule",
            post(hls::quality_schedule)
                .layer(DefaultBodyLimit::max(hls::QUALITY_SCHEDULE_MAX_BYTES)),
        )
        // Capability auth (the session id is the credential) so a closing tab
        // can send this with `keepalive`, which cannot set headers.
        .route("/hls/{session}", delete(hls::delete))
        .route("/hls/{session}/{segment}", get(hls::segment))
        .route(
            "/hls/{session}/{role}/{rendition}/{kind}/{object}",
            get(hls::child_segment),
        )
        .route(
            "/hls/{session}/{role}/{rendition}/index.m3u8",
            get(hls::child_playlist),
        )
        .route("/images/{filename}", get(images::serve))
        // Shared Start: the receiver actor owns dispatch, persistence and
        // physical cleanup; the request is deadline-free like a local create.
        .merge(shared_receiver_ingress::start_router())
        // Shared pre-session file assets, relayed fresh from the Source, and
        // the typed refusal for every shared file suffix B does not serve.
        .merge(shared_receiver_assets::viewer_router(state.clone()))
        .merge(shared_receiver_assets::closure_router())
        // Shared direct play: GET/HEAD bytes bound to an exact B session.
        .merge(shared_receiver_playback::direct_router());

    let api = Router::new()
        .merge(json_short)
        .merge(json_long)
        .merge(media)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            shared_receiver_playback::receiver_media,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            mutable_media_serving_gate,
        ));

    // Plex-compat Tier 1 façade at Plex's absolute paths (docs/CLIENTS.md §3).
    // Plex uses literal `:` path segments (`/:/timeline`, `/photo/:/transcode`)
    // which axum 0.8 rejects by default — `without_v07_checks` matches them
    // literally (we still use `{capture}` syntax for real captures).
    // Every merged root family shares the literal-colon routing policy used
    // by Plex; merging an ordinary router would turn those checks back on.
    let jellyfin_json = Router::new()
        .without_v07_checks()
        .route("/jellyfin/", axum::routing::any(jellyfin::not_found))
        .nest("/jellyfin", jellyfin::router())
        .layer(axum::middleware::from_fn(json_long_deadline))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            jellyfin::enabled_gate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            mutable_media_serving_gate,
        ))
        .layer(axum::middleware::from_fn(jellyfin::cache_policy));
    let plex_short = Router::new()
        .without_v07_checks()
        .route("/identity", get(plex::identity))
        .route("/library", get(plex::library_root))
        .route("/library/sections", get(plex::sections))
        .route("/library/sections/{id}/all", get(plex::section_all))
        .route("/library/metadata/{key}", get(plex::metadata))
        .route("/library/metadata/{key}/children", get(plex::children))
        .route("/library/metadata/{key}/{kind}", get(plex::image))
        .route("/:/timeline", get(plex::timeline))
        .route("/:/scrobble", get(plex::scrobble))
        .route("/:/unscrobble", get(plex::unscrobble))
        .route("/search", get(plex::search))
        .route("/hubs/search", get(plex::search))
        .layer(axum::middleware::from_fn(json_short_deadline));
    let plex_media = Router::new()
        .without_v07_checks()
        .route("/library/parts/{file_id}/{mtime}/{name}", get(plex::part))
        .route("/photo/:/transcode", get(plex::photo_transcode));
    let plex_routes = Router::new()
        .without_v07_checks()
        .merge(plex_short)
        .merge(plex_media)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            mutable_media_serving_gate,
        ))
        // Outside the serving gate, so a request `mutable_media_serving_gate`
        // refuses is still counted. That is the only refusal it sees.
        // `cluster_capacity_gate` is layered outside the whole router (below)
        // and answers 503 before routing — maintenance, a fenced node, a
        // learner-ineligible route — so those refusals never reach this layer
        // and are not counted. The plan's §8.5 closing rule accounts for that.
        .layer(axum::middleware::from_fn_with_state(
            std::sync::Arc::clone(&state.plex_census),
            plex_facade_census,
        ));

    let public_short = Router::new()
        .without_v07_checks()
        // `/` serves the web app for browsers, Plex capabilities for Plex clients.
        .route("/", get(root_dispatch))
        .route("/assets/cluster-panel.js", get(web::cluster_panel_js))
        .route("/assets/playback-policy.js", get(web::playback_policy_js))
        .route("/assets/playback-control.js", get(web::playback_control_js))
        .route("/assets/live-tv.js", get(web::live_tv_js))
        .route("/assets/library-channels.js", get(web::library_channels_js))
        .route("/assets/reader.js", get(web::reader_js))
        .route("/assets/reader.css", get(web::reader_css))
        // Everything else under /assets/ is a `web::WEB_ASSETS` row — the split
        // web shell. The six JavaScript sidecars and reader.css above keep their own routes because
        // they are not in that table (docs/clients/WEB-SHELL-LAYOUT.md).
        .route("/assets/{*path}", get(web::asset))
        .route("/connect.svg", get(web::connect_qr))
        // PWA install assets. The APK is a potentially long body and belongs
        // to the media group below.
        .route("/manifest.webmanifest", get(web::manifest))
        .route("/icons/{file}", get(web::icon))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(system::metrics))
        .route("/sharing", axum::routing::any(sharing::local_peer_refusal))
        .route(
            "/sharing/{*path}",
            axum::routing::any(sharing::local_peer_refusal),
        )
        .fallback(web::fallback)
        .layer(axum::middleware::from_fn(json_short_deadline));

    // Internal relays and control routes already carry their own subsystem
    // deadlines and may move media. A generic handler timeout would interrupt
    // their ownership protocols, so this group deliberately has none.
    let public_media = Router::new()
        .route("/download/plurx-android.apk", get(web::download_android))
        .route(internal_activity::PATH, get(internal_activity::snapshot))
        .route(internal_clock::PATH, get(internal_clock::snapshot))
        .route(
            cluster_operations::INTERNAL_PATH,
            get(cluster_operations::local),
        )
        .route(
            internal_auth_revocation::PATH,
            post(internal_auth_revocation::receive).layer(DefaultBodyLimit::max(
                internal_auth_revocation::MAX_REQUEST_BYTES,
            )),
        )
        .route(
            crate::media_pool::SNAPSHOT_PATH,
            get(internal_media::snapshot),
        )
        .route(
            crate::media_pool::QUALITY_CANDIDATES_V2_PATH,
            post(internal_media::quality_candidates_v2)
                .layer(DefaultBodyLimit::max(crate::media_pool::MAX_REQUEST_BYTES)),
        )
        .route(
            crate::media_pool::QUALITY_CANDIDATES_PATH,
            post(internal_media::quality_candidates)
                .layer(DefaultBodyLimit::max(crate::media_pool::MAX_REQUEST_BYTES)),
        )
        .route(
            crate::media_pool::OFFERS_PATH,
            post(internal_media::offers)
                .layer(DefaultBodyLimit::max(crate::media_pool::MAX_REQUEST_BYTES)),
        )
        .route(
            crate::shared_cache::CANARY_PATH,
            post(internal_media::shared_cache_canary).layer(DefaultBodyLimit::max(
                crate::shared_cache::MAX_CANARY_REQUEST_BYTES,
            )),
        )
        .route(
            crate::live_tv::SNAPSHOT_PATH,
            post(internal_live_tv::snapshot).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            crate::live_tv::START_PATH,
            post(internal_live_tv::start).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            crate::live_tv::START_V2_PATH,
            post(internal_live_tv::start_v2).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            crate::live_tv::ACTIVATE_PATH,
            post(internal_live_tv::activate).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            crate::live_tv::RESOURCE_PATH,
            post(internal_live_tv::resource).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            crate::live_tv::STOP_PATH,
            post(internal_live_tv::stop).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            crate::live_tv::RETIRE_PATH,
            post(internal_live_tv::retire).layer(DefaultBodyLimit::max(1_024)),
        )
        .route(
            crate::live_tv::RESUME_PATH,
            post(internal_live_tv::resume).layer(DefaultBodyLimit::max(1_024)),
        )
        .route(
            crate::live_tv::START_STATE_PATH,
            post(internal_live_tv::start_state).layer(DefaultBodyLimit::max(1_024)),
        )
        .route(
            crate::live_tv::GUIDE_PATH,
            post(internal_live_tv::guide).layer(DefaultBodyLimit::max(
                crate::live_tv::MAX_INTERNAL_BODY_BYTES,
            )),
        )
        .route(
            "/internal/media/cache-copy/{recipe}/{digest}/{object}",
            get(transcode_copies::serve),
        )
        .route(
            "/internal/media/fragment-index/{cache_key}",
            get(internal_media::fragment_index),
        )
        .route(
            crate::subtitle_ranges::PATH,
            post(internal_media::subtitle_range).layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/internal/media/subtitle-source/{file_id}/{ordinal}/{format}",
            get(internal_media::subtitle_source),
        )
        .route(
            crate::media_sessions::START_PATH,
            post(internal_media_sessions::start).layer(DefaultBodyLimit::max(
                crate::media_sessions::MAX_CONTROL_REQUEST_BYTES,
            )),
        )
        .merge(shared_receiver_playback::internal_forwarding_router())
        .route(
            crate::media_sessions::ACTIVATE_PATH,
            post(internal_media_sessions::activate).layer(DefaultBodyLimit::max(
                crate::media_sessions::MAX_ACTIVATION_REQUEST_BYTES,
            )),
        )
        .route(
            crate::media_sessions::PREPARE_PATH,
            post(internal_media_sessions::prepare).layer(DefaultBodyLimit::max(
                crate::media_sessions::MAX_CONTROL_REQUEST_BYTES,
            )),
        )
        .route(
            crate::media_sessions::ABORT_PATH,
            post(internal_media_sessions::abort).layer(DefaultBodyLimit::max(
                crate::media_sessions::MAX_CONTROL_REQUEST_BYTES,
            )),
        )
        .route(
            crate::sharing_connection_custody::CLOSE_PATH,
            post(crate::sharing_connection_custody::close_http).layer(DefaultBodyLimit::max(2048)),
        )
        .route(
            crate::media_sessions::RELAY_PATH,
            post(internal_media_sessions::relay).layer(DefaultBodyLimit::max(
                crate::media_sessions::MAX_CONTROL_REQUEST_BYTES,
            )),
        )
        .route(
            hls::QUALITY_CONTROL_PATH,
            post(internal_media_sessions::quality_control)
                .layer(DefaultBodyLimit::max(hls::QUALITY_CONTROL_MAX_BYTES)),
        )
        .route(
            hls::QUALITY_SCHEDULE_PATH,
            post(internal_media_sessions::quality_schedule)
                .layer(DefaultBodyLimit::max(hls::QUALITY_SCHEDULE_MAX_BYTES)),
        )
        .route(
            crate::media_sessions::CONTROL_PATH,
            post(internal_media_sessions::control).layer(DefaultBodyLimit::max(
                crate::playback_control::MAX_RELAY_BYTES,
            )),
        )
        .merge(shared_source_playback::internal_forwarding_router());

    Router::new()
        // Also opted out of the v0.7 checks so the merged Plex `:` routes pass.
        .without_v07_checks()
        .nest("/api/v1", api)
        .merge(remote::router())
        .merge(crate::diagnostics::router())
        .merge(crate::diagnostics_delivery::router())
        .merge(jellyfin_json)
        .merge(plex_routes)
        .merge(public_short)
        .merge(public_media)
        // Never put capability credentials or query-string tokens in a span.
        // HLS session ids and offline media tokens are bearer credentials even
        // though they live in the path; query strings can also contain Plex
        // tokens. The access log only needs the redacted route-shaped target.
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    target = %safe_trace_target(request.uri()),
                    version = ?request.version(),
                    // Written by `http_request_id` outside this layer, so the
                    // value here is always a short alphanumeric token this
                    // process vouches for, never the raw header a caller sent.
                    request_id = request
                        .headers()
                        .get(REQUEST_ID_HEADER)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default(),
                )
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            cluster_capacity_gate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http_store_attribution,
        ))
        // Outside the Store attribution, and therefore outside the capacity
        // gate, so a 503 the gate produces is still counted as a request.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            desktop_http_admission_gate,
        ))
        .layer(axum::middleware::from_fn(http_request_metrics))
        // Outermost: every layer inside reads the id back out of the request
        // headers, so this one has to have written it first.
        .layer(axum::middleware::from_fn(http_request_id))
        .with_state(state)
}

const LEARNER_ROUTE_INELIGIBLE_JSON: &str = r#"{"code":"learner_route_ineligible","message":"this non-voting learner serves only bounded catalogue reads and declared node-local media routes"}"#;
const NODE_REMOVAL_FENCED_JSON: &str = r#"{"code":"node_removal_fenced","message":"this cluster node is draining or has been removed"}"#;
const NODE_MAINTENANCE_JSON: &str = r#"{"code":"node_maintenance","message":"this cluster node is in maintenance mode and is not accepting new work"}"#;

/// Maintenance keeps enough surface alive to administer the node and drain
/// already-issued media capabilities. Everything that can begin fresh work is
/// refused, including catalogue browsing that may lead a client to select this
/// ingress for a new stream.
fn maintenance_route_eligible(method: &Method, path: &str) -> bool {
    if https_admin::eligible(method, path) {
        return true;
    }
    if method == Method::GET
        && (matches!(path, "/" | "/healthz" | "/readyz" | "/metrics")
            || path.starts_with("/assets/")
            || path.starts_with("/icons/")
            || matches!(path, "/manifest.webmanifest" | "/connect.svg")
            || matches!(
                path,
                "/api/v1/me"
                    | "/api/v1/server/node"
                    | "/api/v1/features"
                    | "/api/v1/auth/login"
                    | "/api/v1/auth/logout"
                    | "/api/v1/activity"
                    | "/api/v1/activity/detail"
                    | "/api/v1/system/logs"
            ))
    {
        return true;
    }
    if method == Method::GET
        && matches!(
            path,
            "/api/v1/cluster/nodes"
                | "/api/v1/cluster/status"
                | "/api/v1/cluster/support-bundle"
                | "/api/v1/cluster/media"
                | "/api/v1/cluster/ingress"
                | cluster_operations::INTERNAL_PATH
                | internal_clock::PATH
        )
    {
        return true;
    }
    if method == Method::POST && matches!(path, "/api/v1/auth/login" | "/api/v1/auth/logout") {
        return true;
    }
    if method == Method::POST && path == internal_auth_revocation::PATH {
        return true;
    }
    if method == Method::POST && path == "/api/v1/cluster/election" {
        return true;
    }
    if matches!(method, &Method::POST | &Method::DELETE)
        && path.starts_with("/api/v1/cluster/nodes/")
        && path.ends_with("/maintenance")
    {
        return true;
    }
    if method == Method::DELETE
        && path.starts_with("/api/v1/cluster/nodes/")
        && path.ends_with("/restart-preparation")
    {
        return true;
    }
    if method == Method::POST
        && matches!(
            path,
            crate::media_sessions::ABORT_PATH
                | crate::media_sessions::RELAY_PATH
                | crate::media_sessions::CONTROL_PATH
                | hls::QUALITY_CONTROL_PATH
                // Schedule mutations are control of an existing session: a
                // node in maintenance keeps streaming it, so it must keep
                // honouring the viewer's quality changes too.
                | hls::QUALITY_SCHEDULE_PATH
        | crate::live_tv::RESOURCE_PATH
                | crate::live_tv::STOP_PATH
                // A retire is a stop plus a fence. Refusing it during
                // maintenance refuses it precisely when a client needs it.
                | crate::live_tv::RETIRE_PATH
                | crate::live_tv::RESUME_PATH
                | crate::live_tv::START_STATE_PATH
        )
    {
        return true;
    }
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let existing_media_read = method == Method::GET
        && (matches!(
            segments.as_slice(),
            ["api", "v1", "hls", _, _]
                | ["api", "v1", "hls", _, "subs", _, _]
                | ["api", "v1", "publication", _, _]
                | ["api", "v1", "stream", _, "status"]
                | ["api", "v1", "offline", "packages", _]
                | ["api", "v1", "offline", "media", _, _]
                | ["api", "v1", "offline", "media", _, _, _]
                | ["api", "v1", "offline", "media", _, "subs", _, _]
                | ["api", "v1", "live-tv", "sessions", _, _]
        ) || (segments.len() >= 6 && segments[0..3] == ["api", "v1", "publication"]));
    let existing_media_control = (method == Method::POST
        && matches!(
            segments.as_slice(),
            [
                "api",
                "v1",
                "hls",
                _,
                "control" | "quality-control" | "quality-schedule"
            ]
        ))
        || (method == Method::DELETE
            && matches!(
                segments.as_slice(),
                ["api", "v1", "hls", _]
                    | ["api", "v1", "publication", _]
                    | ["api", "v1", "live-tv", "sessions", _]
                    | ["api", "v1", "live-tv", "starts", _]
            ))
        || (method == Method::POST
            && matches!(
                segments.as_slice(),
                ["api", "v1", "live-tv", "starts", _, "resume"]
            ));
    let live_keepalive = method == Method::PUT
        && matches!(
            segments.as_slice(),
            ["api", "v1", "live-tv", "sessions", _, "keepalive"]
        );
    existing_media_read || existing_media_control || live_keepalive
}

/// One published route matrix for the non-voting capacity role.
///
/// The native catalogue shapes and their Plex equivalents are the exact
/// handlers inventoried by `bounded_catalogue_handler_inventory...`; every
/// other application route retains Authority/voter semantics. Media paths are
/// admitted only through the existing serving fence, so a learner also needs
/// a fresh quorum/apply proof before any node-local work starts.
fn learner_route_eligible(method: &Method, path: &str) -> bool {
    if https_admin::eligible(method, path)
        || remote::eligible(method, path)
        || crate::diagnostics::eligible(method, path)
        || crate::diagnostics_delivery::eligible(method, path)
    {
        return true;
    }
    if method == Method::GET
        && (matches!(path, "/" | "/healthz" | "/readyz" | "/metrics")
            || path.starts_with("/assets/")
            || path.starts_with("/icons/")
            || matches!(
                path,
                "/manifest.webmanifest" | "/connect.svg" | "/identity" | "/library"
            ))
    {
        return true;
    }
    if method == Method::GET
        && matches!(
            path,
            "/api/v1/cluster/nodes"
                | "/api/v1/cluster/status"
                | "/api/v1/cluster/support-bundle"
                | cluster_operations::INTERNAL_PATH
                | internal_clock::PATH
        )
    {
        return true;
    }
    if method == Method::POST && path == "/api/v1/cluster/leave" {
        // The body is bound to this backend's node id; this is the only
        // membership mutation a learner may originate locally.
        return true;
    }
    if method == Method::POST && path == internal_auth_revocation::PATH {
        return true;
    }
    // The content-addressed fragment-index read. `fragment_index_cluster`
    // hydrates from `media_peers()`, which deliberately includes learners, so
    // leaving this out of the matrix meant every hydration aimed at a learner
    // was refused 503 before its handler ran — a peer directory pointing at a
    // door the route matrix had nailed shut. One key, no deeper path; the
    // handler still validates the key itself.
    let fragment_index_read = method == Method::GET
        && path
            .strip_prefix(crate::fragment_index_cluster::PEER_PATH_PREFIX)
            .is_some_and(|cache_key| !cache_key.is_empty() && !cache_key.contains('/'));
    let subtitle_source_read = method == Method::GET
        && path
            .strip_prefix("/internal/media/subtitle-source/")
            .is_some_and(|suffix| {
                let segments = suffix.split('/').collect::<Vec<_>>();
                matches!(segments.as_slice(), [file_id, ordinal, "sup" | "webvtt" | "matroska"]
                    if file_id.parse::<i64>().is_ok_and(|value| value > 0)
                    && ordinal.parse::<i64>().is_ok_and(|value| value >= 0))
            });
    if (method == Method::GET && transcode_copies::route_eligible(path))
        || fragment_index_read
        || subtitle_source_read
        || (method == Method::GET && path == crate::media_pool::SNAPSHOT_PATH)
        || (method == Method::POST
            && matches!(
                path,
                crate::subtitle_ranges::PATH
                    | crate::media_pool::QUALITY_CANDIDATES_PATH
                    | crate::media_pool::QUALITY_CANDIDATES_V2_PATH
                    | crate::media_pool::OFFERS_PATH
                    | crate::shared_cache::CANARY_PATH
                    | crate::media_sessions::START_PATH
                    | crate::media_sessions::ACTIVATE_PATH
                    | crate::media_sessions::ABORT_PATH
                    | crate::media_sessions::RELAY_PATH
                    | crate::media_sessions::CONTROL_PATH
                    | hls::QUALITY_CONTROL_PATH
                    // A learner can own a continuous session; every schedule
                    // relayed to it arrives here.
                    | hls::QUALITY_SCHEDULE_PATH
                    | crate::live_tv::RESOURCE_PATH
                    | crate::live_tv::STOP_PATH
            ))
    {
        return true;
    }
    if method == Method::GET {
        let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
        let bounded_native = path == "/api/v1/libraries"
            || path == "/api/v1/hubs"
            || path == "/api/v1/home/previews"
            || (segments.len() == 4 && segments[0..2] == ["api", "v1"] && segments[2] == "items")
            || (segments.len() == 5
                && segments[0..2] == ["api", "v1"]
                && segments[2] == "libraries"
                && segments[4] == "items");
        let bounded_plex = path == "/library/sections"
            || (segments.len() == 4
                && segments[0] == "library"
                && segments[1] == "sections"
                && segments[3] == "all")
            || (segments.len() >= 3 && segments[0] == "library" && segments[1] == "metadata");
        if bounded_native || bounded_plex {
            return true;
        }
    }
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let node_local_get = method == Method::GET
        && (matches!(
            segments.as_slice(),
            [
                "api",
                "v1",
                "files",
                _,
                "decision"
                    | "offline-options"
                    | "direct"
                    | "download"
                    | "content"
                    | "stream.mp4"
                    | "photo"
            ] | ["api", "v1", "stream", _, "status"]
                | ["api", "v1", "offline", "packages", _]
                | ["api", "v1", "images", _]
                | ["api", "v1", "items", _, "photo"]
                | ["library", "parts", _, _, _]
                | ["photo", ":", "transcode"]
        ) || matches!(
            segments.as_slice(),
            ["api", "v1", "files", _, "subs", _]
                | ["api", "v1", "files", _, "subs", _, "overlay.json"]
                | [
                    "api",
                    "v1",
                    "files",
                    _,
                    "subs",
                    _,
                    "overlay",
                    _,
                    "objects",
                    _
                ]
                | ["api", "v1", "files", _, "hls", "start"]
                | ["api", "v1", "hls", _, _]
                | ["api", "v1", "hls", _, "subs", _, _]
                | ["api", "v1", "offline", "media", _, _]
                | ["api", "v1", "offline", "media", _, _, _]
                | ["api", "v1", "offline", "media", _, "subs", _, _]
                | ["api", "v1", "publication", _, _]
                | ["api", "v1", "live-tv", "sessions", _, _]
        ) || (segments.len() >= 6 && segments[0..3] == ["api", "v1", "publication"]));
    let node_local_create = method == Method::POST
        && matches!(
            segments.as_slice(),
            ["api", "v1", "files", _, "hls", "sessions"]
                // The continuous family create walks the same create path as
                // `hls/sessions`, and its catalog read is the same kind of
                // node-local, write-nothing POST as the caps-v2 decision.
                | ["api", "v1", "files", _, "hls", "continuous-sessions" | "continuous-candidates"]
                | ["api", "v1", "files", _, "publication"]
                | ["api", "v1", "hls", _, "control" | "quality-control" | "quality-schedule"]
                // The caps-v2 spelling of the decision read. It is a POST only
                // because its capabilities are a JSON document rather than a
                // query string — it writes nothing, and a learner that answers
                // the GET and refuses the POST would stop playback dead for
                // exactly the clients that have migrated.
                | ["api", "v1", "files", _, "decision"]
        );
    let node_local_close = method == Method::DELETE
        && matches!(
            segments.as_slice(),
            ["api", "v1", "hls", _]
                | ["api", "v1", "publication", _]
                | ["api", "v1", "live-tv", "sessions", _]
        );
    let live_keepalive = method == Method::PUT
        && matches!(
            segments.as_slice(),
            ["api", "v1", "live-tv", "sessions", _, "keepalive"]
        );
    node_local_get || node_local_create || node_local_close || live_keepalive
}

fn capacity_role_refusal(
    role: Option<LocalServingRole>,
    method: &Method,
    path: &str,
) -> Option<Response> {
    match role {
        Some(LocalServingRole::Unclustered | LocalServingRole::Voter) => None,
        Some(LocalServingRole::Learner) if learner_route_eligible(method, path) => None,
        Some(LocalServingRole::Learner) => Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CONTENT_TYPE, "application/json")],
                LEARNER_ROUTE_INELIGIBLE_JSON,
            )
                .into_response(),
        ),
        Some(LocalServingRole::Fenced) if matches!(path, "/healthz" | "/metrics") => None,
        Some(LocalServingRole::Fenced) | None => Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::RETRY_AFTER, "1"),
                ],
                NODE_REMOVAL_FENCED_JSON,
            )
                .into_response(),
        ),
    }
}

fn capacity_maintenance_refusal(active: bool, method: &Method, path: &str) -> Option<Response> {
    (active && !maintenance_route_eligible(method, path)).then(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::RETRY_AFTER, "1"),
            ],
            NODE_MAINTENANCE_JSON,
        )
            .into_response()
    })
}

async fn cluster_capacity_gate(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(refusal) = capacity_maintenance_refusal(
        state.membership.local_maintenance_active(),
        request.method(),
        request.uri().path(),
    ) {
        return refusal;
    }
    let role = state.membership.local_serving_role().await.ok();
    if let Some(refusal) = capacity_role_refusal(role, request.method(), request.uri().path()) {
        return refusal;
    }
    next.run(request).await
}

fn safe_trace_target(uri: &Uri) -> String {
    let mut segments = uri.path().split('/').collect::<Vec<_>>();
    if segments.len() >= 7
        && segments[1].eq_ignore_ascii_case("jellyfin")
        && segments[2].eq_ignore_ascii_case("videos")
        && segments[5].eq_ignore_ascii_case("hls")
    {
        segments[4] = "[REDACTED]";
    }
    for marker in ["media", "hls", "publication", "sessions", "starts"] {
        if let Some(index) = segments.iter().position(|segment| *segment == marker) {
            let is_capability_route = match marker {
                "media" => index >= 2 && segments.get(index.wrapping_sub(1)) == Some(&"offline"),
                "hls" => true,
                "publication" => true,
                "sessions" | "starts" => index > 0 && segments.get(index - 1) == Some(&"live-tv"),
                _ => false,
            };
            if is_capability_route && index + 1 < segments.len() {
                segments[index + 1] = "[REDACTED]";
            }
        }
    }
    // Intentionally omit the entire query rather than trying to enumerate
    // every present and future spelling of an access token.
    segments.join("/")
}

/// Root path: Plex clients get the capabilities container; browsers get the app.
async fn root_dispatch(
    state: State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if plex::looks_like_plex(&headers) {
        let census = std::sync::Arc::clone(&state.plex_census);
        let response = match plex::root(state).await {
            Ok(resp) => resp,
            Err(e) => e.into_response(),
        };
        // Only this branch. A browser loading the web app from the same path
        // is not façade traffic and must not be counted as any.
        census.record(PLEX_ROOT_HANDLER, response.status());
        response
    } else {
        web::index().await.into_response()
    }
}

/// Liveness: the process is up. Never touches storage.
async fn healthz() -> &'static str {
    "ok\n"
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadinessFailure {
    Maintenance,
    QuorumUnavailable,
    StoreUnavailable,
    ClockUnbounded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ReadinessEvaluation {
    pub(crate) ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<ReadinessFailure>,
}

/// Shared Store-free clock consequence for readiness and operations status.
///
/// Only while the operator has turned the clock guard on: otherwise one
/// skewed node would take every node's `/readyz` down. The measurement stays
/// visible in the Developer readiness rows and the clock metrics either way.
fn clock_readiness_failure(state: &AppState) -> Option<ReadinessEvaluation> {
    let guard = state.membership.clock_guard();
    (guard.is_enforced() && guard.snapshot().readiness.is_unbounded()).then_some(
        ReadinessEvaluation {
            ready: false,
            reason: Some(ReadinessFailure::ClockUnbounded),
        },
    )
}

/// One typed readiness decision shared by `/readyz` and cluster status.
///
/// Replicated nodes use only the passive serving fence. A local SQLite node
/// retains its existing bounded Store ping because it has no quorum proof.
pub(crate) async fn evaluate_readiness(state: &AppState) -> ReadinessEvaluation {
    if state.membership.local_maintenance_active() {
        return ReadinessEvaluation {
            ready: false,
            reason: Some(ReadinessFailure::Maintenance),
        };
    }
    // A fresh quorum watermark is already a recent replicated-store proof.
    // Do not turn readiness into another multi-second Store request exactly
    // when an isolated node needs to self-fence promptly.
    if state.serving.is_quorum_managed() {
        return if state.serving.is_ready() {
            clock_readiness_failure(state).unwrap_or(ReadinessEvaluation {
                ready: true,
                reason: None,
            })
        } else {
            ReadinessEvaluation {
                ready: false,
                reason: Some(ReadinessFailure::QuorumUnavailable),
            }
        };
    }
    match state.store.ping().await {
        Ok(()) => clock_readiness_failure(state).unwrap_or(ReadinessEvaluation {
            ready: true,
            reason: None,
        }),
        Err(error) => {
            tracing::warn!(%error, "readiness probe failed");
            ReadinessEvaluation {
                ready: false,
                reason: Some(ReadinessFailure::StoreUnavailable),
            }
        }
    }
}

/// Readiness: this node can do work (storage answers).
async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match evaluate_readiness(&state).await {
        ReadinessEvaluation { ready: true, .. } => (StatusCode::OK, "ready\n"),
        ReadinessEvaluation {
            reason: Some(ReadinessFailure::Maintenance),
            ..
        } => (StatusCode::SERVICE_UNAVAILABLE, "maintenance\n"),
        ReadinessEvaluation {
            reason: Some(ReadinessFailure::QuorumUnavailable),
            ..
        } => (StatusCode::SERVICE_UNAVAILABLE, "quorum unavailable\n"),
        ReadinessEvaluation {
            reason: Some(ReadinessFailure::StoreUnavailable),
            ..
        } => (StatusCode::SERVICE_UNAVAILABLE, "store unavailable\n"),
        ReadinessEvaluation {
            reason: Some(ReadinessFailure::ClockUnbounded),
            ..
        } => (StatusCode::SERVICE_UNAVAILABLE, "clock unbounded\n"),
        ReadinessEvaluation {
            ready: false,
            reason: None,
        } => (StatusCode::SERVICE_UNAVAILABLE, "readiness unknown\n"),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn desktop_http_admission_gate(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    // The legacy prepare/maintenance HTTP request cannot count itself as
    // work it must drain. These read-only observations also keep cluster
    // diagnostics and authenticated clock sampling usable while fenced.
    let control_plane = path.starts_with("/api/v1/cluster/")
        || path == cluster_operations::INTERNAL_PATH
        || path == internal_clock::PATH
        || matches!(
            path,
            "/healthz" | "/readyz" | "/metrics" | "/api/v1/server" | "/api/v1/server/node"
        );
    if control_plane {
        return next.run(request).await;
    }
    let Some(admission) = state.serving.try_desktop_http_admission().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::RETRY_AFTER, "5"),
            ],
            crate::serving_fence::RESTART_DRAIN_JSON,
        )
            .into_response();
    };
    next.run(request)
        .await
        .map(|body| axum::body::Body::new(RestartAdmissionBody::new(body, Some(admission))))
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn desktop_http_admission_gate(
    State(_state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    next.run(request).await
}

/// Direct response bodies (including Range reconnects) own admission until
/// delivery finishes or the connection closes, rather than merely to headers.
struct RestartAdmissionBody {
    body: axum::body::Body,
    _admission: Option<crate::serving_fence::RestartAdmission>,
}
impl RestartAdmissionBody {
    fn new(
        body: axum::body::Body,
        mut admission: Option<crate::serving_fence::RestartAdmission>,
    ) -> Self {
        if http_body::Body::is_end_stream(&body) {
            admission.take();
        }
        Self {
            body,
            _admission: admission,
        }
    }
}
impl http_body::Body for RestartAdmissionBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let polled = std::pin::Pin::new(&mut self.body).poll_frame(cx);
        if matches!(&polled, std::task::Poll::Ready(None | Some(Err(_))))
            || http_body::Body::is_end_stream(&self.body)
        {
            self._admission.take();
        }
        polled
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

async fn mutable_media_serving_gate(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let starts_media = crate::serving_fence::ServingFence::starts_mutable_media(
        request.method().as_str(),
        request.uri().path(),
    );
    let restart_admission = if starts_media {
        match state.serving.try_restart_admission().await {
            Some(admission) => Some(admission),
            None => {
                let mut response = (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::CONTENT_TYPE, "application/json")],
                    crate::serving_fence::RESTART_DRAIN_JSON,
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
                return response;
            }
        }
    } else {
        None
    };
    let Some(policy) = state.serving.http_policy(request.uri().path()) else {
        let response = next.run(request).await;
        return response
            .map(|body| axum::body::Body::new(RestartAdmissionBody::new(body, restart_admission)));
    };

    let mut response = (
        StatusCode::from_u16(policy.status).expect("serving policy status"),
        [(header::CONTENT_TYPE, policy.content_type)],
        policy.body,
    )
        .into_response();
    if policy.retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

#[cfg(test)]
pub(crate) fn source_actor_test_state() -> AppState {
    tests::test_state_without_router()
}

#[cfg(test)]
mod tests;
