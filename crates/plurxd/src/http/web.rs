//! The embedded single-page web app.
//!
//! Bundled into the binary (single-binary promise, works offline). It uses hash
//! routing, so the server serves the shell at `/` and as a fallback for any
//! non-API GET path.
//!
//! The app is not one file. `index.html` is a 97-line shell of markup and tags;
//! the CSS and the JavaScript live in the sixty-two files of [`WEB_ASSETS`],
//! which is also their load order. There is no bundler and no build step —
//! `docs/clients/WEB-SHELL-LAYOUT.md` is the map, and adding a file means a row
//! there, a row here, and a tag in the shell, or the tests below say so.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use qrcode::{render::svg, QrCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::state::AppState;

/// The hand-written shell: markup, and one tag per [`WEB_ASSETS`] row. Served
/// through [`SHELL`], which rewrites those tags to carry a content hash.
const INDEX_HTML: &str = include_str!("../web/index.html");

/// Which tag the shell carries for a row, and therefore where it goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WebAsset {
    /// A `<link rel=stylesheet>` in `<head>`.
    HeadStyle,
    /// A synchronous `<script src>` in `<head>`. It runs before first paint —
    /// no `defer`, no `async`, no `type=module`, because the body reads ten of
    /// its names and the theme has to be applied before anything is drawn.
    HeadScript,
    /// A `<script src>` at the end of `<body>`.
    BodyScript,
}

/// Served order == dependency order. These are plain scripts sharing one global
/// scope, exactly as the inline script they were cut out of did, so a row may
/// only use a binding declared in an earlier row —
/// `tests/web/asset-order.test.js` refuses a table that says otherwise, and
/// `tests/web/asset-load.test.js` loads the whole thing to check.
///
/// The seven pre-existing sidecars keep their own routes below and are **not**
/// in this table: they are UMD modules `require()`d by path from forty-odd
/// tests, and three of them are bundled into the native clients by path.
pub const WEB_ASSETS: &[(&str, WebAsset, &str)] = &[
    ("app.css",                                WebAsset::HeadStyle,   include_str!("../web/app.css")),
    ("core/theme.js",                          WebAsset::HeadScript,  include_str!("../web/core/theme.js")),
    ("core/app.js",                            WebAsset::BodyScript,  include_str!("../web/core/app.js")),
    ("core/api.js",                            WebAsset::BodyScript,  include_str!("../web/core/api.js")),
    ("player/measurements.js",                 WebAsset::BodyScript,  include_str!("../web/player/measurements.js")),
    ("core/auth.js",                           WebAsset::BodyScript,  include_str!("../web/core/auth.js")),
    ("core/keyboard-reach.js",                 WebAsset::BodyScript,  include_str!("../web/core/keyboard-reach.js")),
    ("core/chrome.js",                         WebAsset::BodyScript,  include_str!("../web/core/chrome.js")),
    ("core/theme-menu.js",                     WebAsset::BodyScript,  include_str!("../web/core/theme-menu.js")),
    ("core/activity-indicator.js",             WebAsset::BodyScript,  include_str!("../web/core/activity-indicator.js")),
    ("core/cards.js",                          WebAsset::BodyScript,  include_str!("../web/core/cards.js")),
    ("core/lightbox.js",                       WebAsset::BodyScript,  include_str!("../web/core/lightbox.js")),
    ("pages/home-helpers.js",                  WebAsset::BodyScript,  include_str!("../web/pages/home-helpers.js")),
    ("pages/home.js",                          WebAsset::BodyScript,  include_str!("../web/pages/home.js")),
    ("layouts/renderers.js",                   WebAsset::BodyScript,  include_str!("../web/layouts/renderers.js")),
    ("layouts/header.js",                      WebAsset::BodyScript,  include_str!("../web/layouts/header.js")),
    ("layouts/library-grids.js",               WebAsset::BodyScript,  include_str!("../web/layouts/library-grids.js")),
    ("detail/helpers.js",                      WebAsset::BodyScript,  include_str!("../web/detail/helpers.js")),
    ("detail/dynamic-range.js",                WebAsset::BodyScript,  include_str!("../web/detail/dynamic-range.js")),
    ("detail/track-facts.js",                  WebAsset::BodyScript,  include_str!("../web/detail/track-facts.js")),
    ("detail/preplay-selection.js",            WebAsset::BodyScript,  include_str!("../web/detail/preplay-selection.js")),
    ("detail/edit.js",                         WebAsset::BodyScript,  include_str!("../web/detail/edit.js")),
    ("player/player.js",                       WebAsset::BodyScript,  include_str!("../web/player/player.js")),
    ("player/session.js",                      WebAsset::BodyScript,  include_str!("../web/player/session.js")),
    ("player/prepared-replacement.js",         WebAsset::BodyScript,  include_str!("../web/player/prepared-replacement.js")),
    ("player/prepared-switch-measurement.js",  WebAsset::BodyScript,  include_str!("../web/player/prepared-switch-measurement.js")),
    ("player/directed-change.js",              WebAsset::BodyScript,  include_str!("../web/player/directed-change.js")),
    ("player/decode-tiers.js",                 WebAsset::BodyScript,  include_str!("../web/player/decode-tiers.js")),
    ("player/decode-margin.js",                WebAsset::BodyScript,  include_str!("../web/player/decode-margin.js")),
    ("player/stall-diagnosis.js",              WebAsset::BodyScript,  include_str!("../web/player/stall-diagnosis.js")),
    ("player/surface.js",                      WebAsset::BodyScript,  include_str!("../web/player/surface.js")),
    ("player/projection-chrome.js",            WebAsset::BodyScript,  include_str!("../web/player/projection-chrome.js")),
    ("player/transport.js",                    WebAsset::BodyScript,  include_str!("../web/player/transport.js")),
    ("player/autoplay-next.js",                WebAsset::BodyScript,  include_str!("../web/player/autoplay-next.js")),
    ("player/menus.js",                        WebAsset::BodyScript,  include_str!("../web/player/menus.js")),
    ("player/audio-sync.js",                   WebAsset::BodyScript,  include_str!("../web/player/audio-sync.js")),
    ("player/stats.js",                        WebAsset::BodyScript,  include_str!("../web/player/stats.js")),
    ("pages/activity.js",                      WebAsset::BodyScript,  include_str!("../web/pages/activity.js")),
    ("pages/analysis.js",                      WebAsset::BodyScript,  include_str!("../web/pages/analysis.js")),
    ("pages/activity-stream.js",               WebAsset::BodyScript,  include_str!("../web/pages/activity-stream.js")),
    ("pages/settings.js",                      WebAsset::BodyScript,  include_str!("../web/pages/settings.js")),
    ("pages/settings-panels.js",               WebAsset::BodyScript,  include_str!("../web/pages/settings-panels.js")),
    ("pages/live-tv.js",                       WebAsset::BodyScript,  include_str!("../web/pages/live-tv.js")),
    ("pages/live-tv-dvr.js",                   WebAsset::BodyScript,  include_str!("../web/pages/live-tv-dvr.js")),
    ("pages/recordings.js",                    WebAsset::BodyScript,  include_str!("../web/pages/recordings.js")),
    ("pages/dvr-reminders.js",                 WebAsset::BodyScript,  include_str!("../web/pages/dvr-reminders.js")),
    ("pages/live-tv-controls.js",              WebAsset::BodyScript,  include_str!("../web/pages/live-tv-controls.js")),
    ("pages/settings-developer.js",            WebAsset::BodyScript,  include_str!("../web/pages/settings-developer.js")),
    ("pages/settings-live-tv.js",              WebAsset::BodyScript,  include_str!("../web/pages/settings-live-tv.js")),
    ("pages/settings-system.js",               WebAsset::BodyScript,  include_str!("../web/pages/settings-system.js")),
    ("pages/cluster.js",                       WebAsset::BodyScript,  include_str!("../web/pages/cluster.js")),
    ("pages/cluster-operations.js",            WebAsset::BodyScript,  include_str!("../web/pages/cluster-operations.js")),
    ("pages/cluster-database.js",              WebAsset::BodyScript,  include_str!("../web/pages/cluster-database.js")),
    ("pages/cluster-troubleshooting.js",       WebAsset::BodyScript,  include_str!("../web/pages/cluster-troubleshooting.js")),
    ("pages/settings-playback.js",             WebAsset::BodyScript,  include_str!("../web/pages/settings-playback.js")),
    ("pages/users-admin.js",                   WebAsset::BodyScript,  include_str!("../web/pages/users-admin.js")),
    ("layouts/catalog.js",                     WebAsset::BodyScript,  include_str!("../web/layouts/catalog.js")),
    ("layouts/register-classic.js",            WebAsset::BodyScript,  include_str!("../web/layouts/register-classic.js")),
    ("layouts/theater.js",                     WebAsset::BodyScript,  include_str!("../web/layouts/theater.js")),
    ("pages/reader.js",                        WebAsset::BodyScript,  include_str!("../web/pages/reader.js")),
    ("pages/library-channels-page.js",         WebAsset::BodyScript,  include_str!("../web/pages/library-channels-page.js")),
    ("router.js",                              WebAsset::BodyScript,  include_str!("../web/router.js")),
];

/// A content hash per row, computed once. The shell stays hand-written, so it
/// cannot carry `?v=<hash>` and stay correct — the hash is applied at serve
/// time instead.
static ASSET_HASHES: LazyLock<Vec<String>> = LazyLock::new(|| {
    WEB_ASSETS
        .iter()
        .map(|(_, _, body)| hex::encode(&Sha256::digest(body.as_bytes())[..8]))
        .collect()
});

/// The shell as served: every row's `src`/`href` rewritten to carry its hash.
///
/// A row whose tag is missing is a panic at first request, not a silent miss,
/// because it means the shell and the table disagree about what the app is —
/// and `web_assets_match_the_shell` below should have failed the build first.
static SHELL: LazyLock<String> = LazyLock::new(|| {
    let mut html = String::from(INDEX_HTML);
    for ((path, _, _), hash) in WEB_ASSETS.iter().zip(ASSET_HASHES.iter()) {
        let tag = format!("\"/assets/{path}\"");
        let at = html.find(&tag).unwrap_or_else(|| {
            panic!("index.html carries no tag for the WEB_ASSETS row {path}")
        });
        html.replace_range(at..at + tag.len(), &format!("\"/assets/{path}?v={hash}\""));
    }
    html
});

/// `.css` or `.js` — the table holds nothing else, and nothing else may be
/// added without deciding this deliberately.
fn asset_content_type(path: &str) -> &'static str {
    if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/javascript"
    }
}
/// Pure playback-routing policy, separated from the player adapter so the
/// decisions that change bytes or transport can run under Node unit tests.
const PLAYBACK_POLICY_JS: &str = include_str!("../web/playback-policy.js");
/// The Settings → Cluster panel's model: quorum arithmetic, the operations
/// rail's preconditions, and the replicated-database ledger. Served separately
/// so its decisions run under Node unit tests instead of a regex that slices
/// functions out of the app shell.
const CLUSTER_PANEL_JS: &str = include_str!("../web/cluster-panel.js");
/// Passive playback-control reporter. It owns exchange sequencing and
/// coalescing, but deliberately has no authority over playback recovery.
const PLAYBACK_CONTROL_JS: &str = include_str!("../web/playback-control.js");
/// Live-TV channel/session lifecycle policy. It serializes tuner changes and
/// retains capability ownership until release succeeds, independently of DOM rendering.
const LIVE_TV_JS: &str = include_str!("../web/live-tv.js");
/// Library-channel editor, guide and stale-tune policy.
const LIBRARY_CHANNELS_JS: &str = include_str!("../web/library-channels.js");
/// EPUB pagination, locator, and sandbox-frame policy. Kept out of the app
/// shell so native WebViews can reuse the same navigator in M3.
const READER_JS: &str = include_str!("../web/reader.js");
const READER_CSS: &str = include_str!("../web/reader.css");
/// hls.js (bundled for the transcode playback path; keeps the single-binary,
/// works-offline promise instead of a CDN dependency).
const HLS_JS: &str = include_str!("../web/hls.min.js");
/// PWA manifest + icons — bundled so "Add to Home Screen" (iOS) and installable
/// PWA (Android/desktop) work with no external assets.
const MANIFEST: &str = include_str!("../web/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../web/icons/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../web/icons/icon-512.png");
const ICON_MASKABLE: &[u8] = include_bytes!("../web/icons/maskable-512.png");
const APPLE_TOUCH: &[u8] = include_bytes!("../web/icons/apple-touch-icon.png");

/// Serve the web app shell.
///
/// `no-cache` because the assets it names are immutable: a heuristically cached
/// shell would pin a deploy's old hashes for as long as the browser felt like
/// it, and the whole point of the hash is that the shell is the only thing that
/// has to be re-read.
pub async fn index() -> Response {
    (
        [(header::CACHE_CONTROL, "no-cache")],
        Html(SHELL.as_str()),
    )
        .into_response()
}

/// Serve one row of [`WEB_ASSETS`].
///
/// The query string is the version and is ignored here — the path is the
/// identity. An unknown path is a 404 rather than the shell: before this
/// existed a mistyped asset URL fell through to [`fallback`] and came back as
/// `200 text/html`, which a browser then tried to execute as JavaScript.
pub async fn asset(AxPath(path): AxPath<String>) -> Response {
    match WEB_ASSETS.iter().find(|(name, _, _)| *name == path) {
        Some((name, _, body)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, asset_content_type(name)),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable",
                ),
            ],
            *body,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "no such asset",
        )
            .into_response(),
    }
}

/// Serve the bundled hls.js.
pub async fn hls_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=604800"),
        ],
        HLS_JS,
    )
        .into_response()
}

/// Serve the cluster panel's model.
pub async fn cluster_panel_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        CLUSTER_PANEL_JS,
    )
        .into_response()
}

/// Serve the web player's unit-tested routing policy.
pub async fn playback_policy_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        PLAYBACK_POLICY_JS,
    )
        .into_response()
}

/// Serve the browser's passive playback-control reporter.
pub async fn playback_control_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        PLAYBACK_CONTROL_JS,
    )
        .into_response()
}

/// Serve the browser's unit-tested Live TV lifecycle controller.
pub async fn live_tv_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        LIVE_TV_JS,
    )
        .into_response()
}

pub async fn library_channels_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        LIBRARY_CHANNELS_JS,
    )
        .into_response()
}

/// Serve the unit-tested EPUB navigator shared by the browser reader.
pub async fn reader_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        READER_JS,
    )
        .into_response()
}

/// Serve the trusted reader chrome; publication styles stay inside the frame.
pub async fn reader_css() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        READER_CSS,
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct ConnectQrQuery {
    origin: String,
}

/// Render the current browser origin as a QR code native clients can scan.
/// It deliberately contains no credential: scanning chooses a server, then
/// the app signs in normally so passwords and tokens never cross the code.
pub async fn connect_qr(Query(query): Query<ConnectQrQuery>) -> Response {
    let Ok(svg) = connection_qr_svg(&query.origin) else {
        return (StatusCode::BAD_REQUEST, "invalid server origin").into_response();
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'"),
        ],
        svg,
    )
        .into_response()
}

fn connection_qr_svg(origin: &str) -> Result<String, ()> {
    let origin = origin.trim().trim_end_matches('/');
    let uri: Uri = origin.parse().map_err(|_| ())?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none()
        || uri
            .path_and_query()
            .is_some_and(|path| path.as_str() != "/")
    {
        return Err(());
    }
    let code = QrCode::new(origin.as_bytes()).map_err(|_| ())?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#111217"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

/// Serve the PWA manifest (enables install / Add-to-Home-Screen).
pub async fn manifest() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        MANIFEST,
    )
        .into_response()
}

/// Serve one of the embedded PWA / apple-touch icons by name.
pub async fn icon(AxPath(name): AxPath<String>) -> Response {
    let bytes: &'static [u8] = match name.as_str() {
        "icon-192.png" => ICON_192,
        "icon-512.png" => ICON_512,
        "maskable-512.png" => ICON_MASKABLE,
        "apple-touch-icon.png" => APPLE_TOUCH,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=604800"),
        ],
        bytes,
    )
        .into_response()
}

/// Resolve the Android APK to serve, if one is published: `PLURX_ANDROID_APK`
/// (an explicit path) wins, else `<data_dir>/plurx-android.apk`. `None` when no
/// file is present, so the web UI's download link stays hidden.
pub fn android_apk_path(data_dir: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PLURX_ANDROID_APK") {
        if !p.is_empty() {
            let pb = PathBuf::from(p);
            return pb.is_file().then_some(pb);
        }
    }
    let pb = Path::new(data_dir).join("plurx-android.apk");
    pb.is_file().then_some(pb)
}

/// Serve the Android APK for sideloading. Unauthenticated on purpose: it's the
/// client app binary, not user data, and a TV's Downloader/browser can't attach
/// a bearer token anyway.
pub async fn download_android(State(state): State<AppState>) -> Response {
    let Some(path) = android_apk_path(&state.system.data_dir) else {
        return (StatusCode::NOT_FOUND, "no Android app published").into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive",
                ),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"plurx.apk\"",
                ),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no Android app published").into_response(),
    }
}

/// Fallback for unmatched routes: serve the app for browser navigations,
/// but return a JSON 404 for anything under `/api` so API clients get a clean
/// error instead of a page of HTML.
pub async fn fallback(uri: axum::http::Uri) -> Response {
    if uri.path().starts_with("/api") {
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"not found"}"#,
        )
            .into_response()
    } else {
        index().await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        asset_content_type, connection_qr_svg, WebAsset, INDEX_HTML, LIVE_TV_JS,
        PLAYBACK_POLICY_JS, READER_JS, SHELL, WEB_ASSETS,
    };

    /// The seven sidecars and `reader.css`. They are served by their own
    /// handlers and are deliberately absent from `WEB_ASSETS`, so the shell
    /// carries tags for them that the table knows nothing about.
    const SIDECARS: [&str; 8] = [
        "cluster-panel.js",
        "playback-policy.js",
        "playback-control.js",
        "reader.js",
        "hls.min.js",
        "live-tv.js",
        "library-channels.js",
        "reader.css",
    ];

    /// The body rows joined in served order — the string `INDEX_HTML` used to
    /// be before the shell was split, and what most of these assertions are
    /// actually about. Reading `INDEX_HTML` for them would now pass by
    /// vacuously finding nothing, which is the failure mode this replaces.
    fn body_source() -> String {
        WEB_ASSETS
            .iter()
            .filter(|(_, kind, _)| *kind == WebAsset::BodyScript)
            .map(|(_, _, body)| *body)
            .collect()
    }

    /// Where the shell carries the tag for a served path.
    fn tag_at(path: &str) -> usize {
        INDEX_HTML
            .find(&format!("\"/assets/{path}\""))
            .unwrap_or_else(|| panic!("the shell carries no tag for /assets/{path}"))
    }

    /// The one body row whose source carries `needle`. Two rows carrying it, or
    /// none, is itself the bug — an ordering assertion against "whichever file
    /// happened to match" proves nothing.
    fn row_with(needle: &str) -> &'static str {
        let hits: Vec<&str> = WEB_ASSETS
            .iter()
            .filter(|(_, kind, body)| *kind == WebAsset::BodyScript && body.contains(needle))
            .map(|(path, _, _)| *path)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one body row to carry {needle:?}, found {hits:?}"
        );
        hits[0]
    }

    /// Every `/assets/` tag the shell carries, in document order, as
    /// (path, is_stylesheet, in_head).
    fn shell_tags() -> Vec<(&'static str, bool, bool)> {
        let mut out = Vec::new();
        let mut in_head = true;
        for line in INDEX_HTML.lines() {
            if line == "</head>" {
                in_head = false;
                continue;
            }
            let (is_style, rest) = if let Some(rest) =
                line.strip_prefix("<link rel=\"stylesheet\" href=\"/assets/")
            {
                (true, rest)
            } else if let Some(rest) = line.strip_prefix("<script src=\"/assets/") {
                (false, rest)
            } else {
                continue;
            };
            out.push((
                rest.split('"').next().expect("a quoted asset path"),
                is_style,
                in_head,
            ));
        }
        out
    }

    #[test]
    fn web_assets_match_the_shell() {
        let tags = shell_tags();

        // Nothing under /assets/ anywhere in the shell — including inside a
        // line — that is neither a table row nor a sidecar route.
        assert_eq!(
            INDEX_HTML.matches("\"/assets/").count(),
            WEB_ASSETS.len() + SIDECARS.len(),
            "the shell names an /assets/ URL that is neither a WEB_ASSETS row \
             nor a sidecar route"
        );
        for (path, _, _) in &tags {
            assert!(
                SIDECARS.contains(path) || WEB_ASSETS.iter().any(|(row, _, _)| row == path),
                "the shell loads /assets/{path}, which is not a WEB_ASSETS row"
            );
        }

        // Every row is served, in table order, in the half its kind names.
        let table: Vec<&str> = WEB_ASSETS.iter().map(|(path, _, _)| *path).collect();
        let served: Vec<&str> = tags
            .iter()
            .map(|(path, _, _)| *path)
            .filter(|path| !SIDECARS.contains(path))
            .collect();
        assert_eq!(
            served, table,
            "the shell's tag order and WEB_ASSETS disagree"
        );

        let reader_css = tags
            .iter()
            .position(|(path, _, _)| *path == "reader.css")
            .expect("reader.css is still linked");
        let last_sidecar = tags
            .iter()
            .rposition(|(path, _, _)| SIDECARS.contains(path))
            .expect("the sidecars are still loaded");
        let mut last_style_in_head = 0usize;

        for (path, kind, _) in WEB_ASSETS {
            let at = tags
                .iter()
                .position(|(name, _, _)| name == path)
                .expect("checked above");
            let (_, is_style, in_head) = tags[at];
            match kind {
                WebAsset::HeadStyle => {
                    assert!(is_style, "{path} is a HeadStyle row but is not a stylesheet link");
                    assert!(in_head, "{path} is a HeadStyle row but is not in <head>");
                    assert!(
                        at > reader_css,
                        "{path} must be linked after reader.css — reader.css has no selector \
                         overlap with app.css today, and the order is what keeps that true"
                    );
                    last_style_in_head = last_style_in_head.max(at);
                }
                WebAsset::HeadScript => {
                    assert!(!is_style, "{path} is a HeadScript row but is a stylesheet link");
                    assert!(
                        in_head,
                        "{path} is a HeadScript row but is not in <head>: it has to run \
                         before first paint or the wrong theme flashes"
                    );
                    assert!(
                        at > last_style_in_head,
                        "{path} must come after every stylesheet in <head>"
                    );
                }
                WebAsset::BodyScript => {
                    assert!(!is_style, "{path} is a BodyScript row but is a stylesheet link");
                    assert!(!in_head, "{path} is a BodyScript row but its tag is in <head>");
                    assert!(
                        at > last_sidecar,
                        "{path} must load after the sidecars — the body rows read \
                         window.PlurxPlaybackPolicy and friends at load"
                    );
                }
            }
        }
    }

    #[test]
    fn served_shell_versions_every_asset_it_names() {
        let served = SHELL.as_str();
        for ((path, _, _), hash) in WEB_ASSETS.iter().zip(super::ASSET_HASHES.iter()) {
            assert_eq!(hash.len(), 16, "{path} hash is not 16 hex characters");
            assert!(
                served.contains(&format!("\"/assets/{path}?v={hash}\"")),
                "the served shell does not version /assets/{path}"
            );
        }
        // The sidecars are not in the table and keep their unversioned URLs.
        for path in SIDECARS {
            assert!(
                served.contains(&format!("\"/assets/{path}\"")),
                "the served shell lost the sidecar /assets/{path}"
            );
        }
        assert_eq!(
            served.matches("?v=").count(),
            WEB_ASSETS.len(),
            "the served shell versions something that is not a table row"
        );
        // Two rows with the same bytes would still be two rows; the hash is a
        // version, not an identity, and the path stays the identity.
        assert_eq!(asset_content_type("app.css"), "text/css; charset=utf-8");
        assert_eq!(asset_content_type("router.js"), "application/javascript");
    }

    #[test]
    fn app_shell_shows_the_running_build_to_signed_in_and_signed_out_users() {
        assert_eq!(
            body_source().matches("Version ${esc(buildLabel())}").count(),
            2
        );
    }

    #[test]
    fn app_shell_loads_the_tested_playback_policy_before_the_player() {
        assert!(tag_at("playback-policy.js") < tag_at(row_with("const PlaybackPolicy")));
        assert!(PLAYBACK_POLICY_JS.contains("function initialRoute"));
    }

    #[test]
    fn shared_stats_rows_render_the_tested_control_lease_presentation() {
        assert!(PLAYBACK_POLICY_JS.contains("function controlLeaseMode"));
        assert!(PLAYBACK_POLICY_JS.contains("function controlLeasePresentation"));
        let body = body_source();
        assert_eq!(body.matches("PlaybackPolicy.controlLeaseMode(").count(), 1);
        assert!(body.contains("const STATS_ROWS=Object.freeze"));
        assert!(body.contains("control,control_note:controlNote"));
        assert!(body.contains("`${lease.ownership} · accepted #"));
        assert!(body.contains("`${lease.ownership} · connecting`"));
        assert!(!body.contains("`Passive · accepted #"));
    }

    #[test]
    fn activity_stream_rows_render_control_and_production_facts() {
        // The Stream cell is a state pill, a meter strip and a details
        // disclosure, each its own painter; the sentence-joiner is gone.
        let body = body_source();
        for helper in [
            "function activityStreamState",
            "function activityStreamMeters",
            "function activityStreamDetails",
            "function activityStreamCell",
        ] {
            assert!(body.contains(helper), "missing Activity painter {helper}");
        }
        assert!(!body.contains("function activitySessionControlText"));
        for label in [
            "lease_timeout_ms",
            "control_demand",
            "reported_position_ms",
            "client_runway_ms",
            "production_policy",
            "production_ahead_seconds",
            "production_target_seconds",
        ] {
            assert!(body.contains(label), "missing Activity field {label}");
        }
        // …and they are all one row's business, so a reader knows where to go.
        assert_eq!(row_with("function activityStreamCell"), "pages/activity-stream.js");
    }

    #[test]
    fn app_shell_loads_the_reader_boundary_before_its_route() {
        assert!(tag_at("reader.js") < tag_at(row_with("async function viewReader")));
        assert!(READER_JS.contains("class FrameNavigator"));
        assert!(READER_JS.contains("stripExecutableMarkup"));
    }

    #[test]
    fn app_shell_loads_the_live_tv_controller_before_its_route() {
        assert!(tag_at("live-tv.js") < tag_at(row_with("async function viewLiveTv")));
        assert!(LIVE_TV_JS.contains("class Lease"));
        assert!(LIVE_TV_JS.contains("await this.requests.release(id)"));
    }

    #[test]
    fn connection_qr_accepts_only_an_http_server_origin() {
        let svg = connection_qr_svg("http://10.42.4.14:32400/").expect("valid origin");
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        assert!(svg.contains("#111217"));

        assert!(connection_qr_svg("javascript:alert(1)").is_err());
        assert!(connection_qr_svg("http://server.test/path").is_err());
        assert!(connection_qr_svg("").is_err());
    }

    #[test]
    fn signed_in_account_menu_can_show_the_server_qr_without_signing_out() {
        let body = body_source();
        let menu = body.find("function profileMenuHtml()").expect("account menu");
        let qr_action = body.find("Show server QR code").expect("signed-in QR action");
        let sign_out = body[menu..]
            .find("Sign out")
            .map(|offset| menu + offset)
            .expect("sign-out action");
        assert!(menu < qr_action && qr_action < sign_out);
        assert!(body.contains("function showConnectQr()"));
        assert!(body.contains("/connect.svg?origin=${encodeURIComponent(location.origin)}"));
    }
}
