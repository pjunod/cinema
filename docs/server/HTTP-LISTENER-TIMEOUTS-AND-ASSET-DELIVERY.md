# HTTP listener timeouts and asset delivery — a timer on the wire, gzip on the shell, headers on the page

**Status:** ready for review · **Executes:** §2.5, C2, W1, W2, W5, W6-now from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(assessment rows 2.5, C2, F-core-2, F-web-1, F-web-2, F-web-3, F-web-8, W6 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Read §2 first — it quotes the listener, the router layers and the asset
handlers as they are, with the one hyper fact the whole item rests on. Then
build §5 in order: M1 (listener + handler deadlines), M2 (compressed and
versioned assets), M3 (security headers). Each is one draft PR into `main`
under the fast lane, with the focused tests named in its acceptance check.
The three are independent after M1's dependency bump lands; M2 and M3 may
run in parallel sessions. Every `file:line` here is from `88a3957a` —
re-verify each anchor by function name before editing.

**If a step seems to require a deadline on a streaming route, a
`CompressionLayer` on the router, removing an existing body limit, or
changing the shell's load order, stop and flag it.** Those are the four ways
this item breaks playback instead of hardening it.

**Correction to the review:** two narrow ones. (1) The review says the
listener "has no timeouts, limits or compression"; the assessment already
narrowed that, and §2.2 lists what exists: twenty-five per-route body
limits in `http/mod.rs` (more in the nested routers), the cluster capacity
gate, and per-subsystem deadlines on every media route.
What is missing is exactly the connection-level timer and handler deadlines
on JSON routes. (2) The assessment's F-core-2 says "header-read deadlines do
not alone bound idle keep-alive sockets". In hyper 1.10.1 the h1 header
timer is armed on every `poll_read_head`
(`hyper-1.10.1/src/proto/h1/conn.rs:219-233`), which includes the wait for
the *next* request on a keep-alive connection, so one timer bounds both.
M1's acceptance test proves this rather than arguing it; h2 idle is a
separate mechanism (ping keepalive) and is handled separately.

---

## 1. Objective

1. Every accepted TCP connection has a bound on how long it may sit without
   delivering a complete request head (h1) or answering a ping (h2), so a
   client that opens sockets and goes silent cannot hold them for the life
   of the process.
2. Every JSON handler has a deadline sized for what it legitimately does, so
   a wedged Store or a hung child cannot hold a request slot forever — and
   no streaming route gets one, because a paced remux, a blocked segment
   GET and a live playlist poll are *supposed* to be long.
3. The web shell's 71 script/style requests are compressed, versioned and
   validated, so a warm reload is 304s and a cold load is ~0.9 MB rather than
   2.6 MB — measured, not asserted.
4. The shell carries the three headers that cost nothing today (`nosniff`,
   `Referrer-Policy`, a CSP of `frame-ancestors`/`base-uri`/`object-src`),
   checked against the two places that embed server content.

Not an objective: `script-src 'self'`. That needs the 398 inline handlers
delegated first (W6-then), which is its own plan.

## 2. Contract today

Re-verify at build time; every line number is from `88a3957a`.

### 2.1 The listener has no timer, so hyper's default timeout is inert

`crates/plurxd/src/main.rs:2418-2430`:

```rust
let server = axum::serve(
    listener,
    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
)
.with_graceful_shutdown(async move {
    shutdown.await;
    let _ = drain_started.send(());
})
.into_future();
```

`axum-0.8.9/src/serve/mod.rs:391` builds
`hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())` and
never calls `.timer()`. hyper's h1 builder defaults the header read timeout
to 30 s (`hyper-1.10.1/src/server/conn/http1.rs:249`,
`Dur::Default(Some(Duration::from_secs(30)))`), but
`common/time.rs:72-76` turns a default with no timer into `None` after one
`warn!("timeout `header_read_timeout` has default, but no timer set")`. So
the timeout that hyper documents as "default 30 seconds" is not running.

HTTP/2 is compiled in: `crates/plurxd/Cargo.toml` asks axum for defaults
only, but `vendor/hiqlite/Cargo.toml:136-141` enables axum's `http2`
feature, and Cargo unifies features per package. The auto builder therefore
accepts h2c prior-knowledge connections today, with no ping keepalive
(`auto/mod.rs:1026-1041` are never called).

Neither `hyper` nor `hyper-util` is a direct dependency of `plurxd`
(`Cargo.toml:29-31`: `axum = "0.8"`, `tower = "0.5"`,
`tower-http = { version = "0.6", features = ["trace"] }`; the plurxd
manifest adds `fs` and `cors`). `TokioTimer` lives in `hyper-util` under
its `tokio` feature, so M1 adds that edge.

The drain: `SHUTDOWN_DRAIN_TIMEOUT = 5 s` (`main.rs:2478`) and
`PROGRESS_DRAIN_TIMEOUT = 3 s` (`:2480`) bound shutdown; the tests at
`main.rs:4402-4501` pin that a discovery daemon that cannot be withdrawn
still drains. M1 keeps `serve()`'s signature so those tests stay as they are.

### 2.2 What limits exist, so nobody removes them

`crates/plurxd/src/http/mod.rs:629-643` applies two layers to the whole
router: `TraceLayer` with the redacting `safe_trace_target`, and
`cluster_capacity_gate` (maintenance / learner / fenced refusals with
`Retry-After: 1`). Under them:

- `DefaultBodyLimit::max(...)` on 25 routes in `mod.rs` (plus `dvr.rs:63`,
  `library_channels.rs:50`) — `64 * 1024` on `/items/{id}/reading-state`,
  `/files/{id}/decision` and `/files/{id}/hls/sessions`
  (`mod.rs:338, 351, 409`);
  `playback_control::MAX_REQUEST_BYTES` on `/hls/{session}/control`
  (`:431`); `live_tv::MAX_INTERNAL_BODY_BYTES`, `1_024`,
  `media_sessions::MAX_CONTROL_REQUEST_BYTES` and friends on every internal
  POST (`:503-620`). Everything else has axum's 2 MiB default.
- `mutable_media_serving_gate` on the `/api/v1` and Plex routers
  (`:441-444, 466-469`).
- Per-subsystem deadlines on the media paths: `ROUTE_QUERY_DEADLINE = 3 s`
  and `START_DEADLINE = 50 s` (`media_sessions.rs:130, 54`),
  `ARTWORK_FETCH_TOTAL_TIMEOUT = 3 s` and `PEER_RACE_DEADLINE = 3.25 s`
  (`http/images.rs:41, 35`), the publication resource bounds
  (`http/publication.rs:35-42`), the blocked-GET cap
  (`plurx_vod_blocked_get_cap` on `/metrics`).

None of this is touched. M1 adds one more layer beside them.

### 2.3 The asset handlers

`crates/plurxd/src/http/web.rs`:

- `:197-199` `index()` — the shell, `Cache-Control: no-cache`, no other
  header. `SHELL` (`:139-149`) rewrites each `WEB_ASSETS` tag to
  `/assets/<path>?v=<16 hex>` from `ASSET_HASHES` (`:127-132`, SHA-256
  prefix of the row's bytes).
- `:207-225` `asset()` — a `WEB_ASSETS` row: `Content-Type`,
  `Cache-Control: public, max-age=31536000, immutable`, the raw body. No
  `Content-Encoding`, no `ETag`, no `Vary`. The query string is ignored;
  the path is the identity.
- `:228-238` `hls_js()` — `public, max-age=604800`, no validator, no hash.
  The app subclasses its loader (`web/player/player.js:395` `constructor(config){
  super(config); ...}`), so a browser may run a week-old hls.js against a
  new `player.js`.
- `:240-328` six sidecars and `reader.css` — `Cache-Control: no-cache`, no
  `ETag`, no `Last-Modified`; every document reload re-downloads all of them.
- `:347` `connect.svg` already sends `Content-Security-Policy:
  default-src 'none'`; `http/publication.rs:47, 386-396` already sends
  `nosniff`, `Referrer-Policy: no-referrer`, `frame-ancestors 'self'` and
  `Cross-Origin-Resource-Policy: same-origin` on publication resources. M3
  extends the same posture to the shell; it invents nothing new.

The shell's tag order (`web/index.html:15-97`): `reader.css`, `app.css`,
`core/theme.js` in `<head>`; then seven sidecars (`cluster-panel.js`,
`playback-policy.js`, `playback-control.js`, `reader.js`, `hls.min.js`,
`live-tv.js`, `library-channels.js`) then the sixty body rows. Three tests
pin this: `web_assets_match_the_shell` (`web.rs:550`),
`tests/web/asset-order.test.js`, `tests/web/asset-load.test.js`; the doc is
[WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) §4–§5, and
`tests/web/asset-layout.test.js:42-47` asserts that doc names every sidecar.

`flate2 = "1.1.9"` is already a workspace dependency (`Cargo.toml:87`,
`crates/plurxd/Cargo.toml:56`).

### 2.4 Who embeds server content

- The browser reader frames publication resources
  (`web/pages/reader.js:38`, `<iframe sandbox="allow-same-origin">`); the
  resources answer `frame-ancestors 'self'`. The shell itself is never
  framed by anything in `crates/plurxd/src/web` (grep for `<iframe` finds
  only the reader page and `offline-reader.html`).
- The native readers load the shell as a **top-level** WebView document:
  `clients/apple/Sources/ReaderView.swift:7-16` builds
  `<origin>/?native-reader=1`;
  `clients/android/.../ui/ReaderScreen.kt:64,167` loads the same. A
  top-level navigation has no frame ancestor, so `frame-ancestors 'none'`
  does not apply to it.
- `offline-reader.html` is bundled into the native apps
  (`clients/apple/project.yml:27-29`, `clients/android/app/build.gradle.kts:28`)
  and is not served by `plurxd` at all.

## 3. Change

### 3.1 M1 — the timer, the h1 header timeout, h2 keepalive, handler deadlines

**Dependencies.** Add to `crates/plurxd/Cargo.toml`:

```toml
hyper = { version = "1", features = ["server", "http1", "http2"] }
hyper-util = { version = "0.1", features = [
    "server-auto", "server-graceful", "tokio", "http1", "http2", "service",
] }
```

`Cargo.lock` already holds 1.10.1 / 0.1.20, so this compiles no new crate.
Add `"timeout"` to plurxd's `tower-http` features only if the
`tower_http::timeout::RequestBodyTimeoutLayer` option in §7 is taken; the
handler deadline below does not need it.

**The accept loop.** Replace the `axum::serve(...)` call inside `serve()`
(`main.rs:2418-2430`) with an accept loop that keeps the function's
signature and its `drain_started`/`SHUTDOWN_DRAIN_TIMEOUT` semantics:

```rust
let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
builder.http1()
    .timer(TokioTimer::new())
    .header_read_timeout(HEADER_READ_TIMEOUT)      // 15 s
    .keep_alive(true);
builder.http2()
    .timer(TokioTimer::new())
    .keep_alive_interval(Some(H2_KEEPALIVE_INTERVAL)) // 20 s
    .keep_alive_timeout(H2_KEEPALIVE_TIMEOUT)         // 20 s
    .enable_connect_protocol();                        // what axum::serve set
let graceful = hyper_util::server::graceful::GracefulShutdown::new();
loop {
    let (stream, remote) = tokio::select! {
        accepted = listener.accept() => accepted?,
        () = &mut shutdown => break,
    };
    let app = app.clone();
    let svc = TowerToHyperService::new(tower::ServiceBuilder::new()
        .map_request(move |mut req: Request<Incoming>| {
            req.extensions_mut().insert(ConnectInfo(remote));
            req.map(axum::body::Body::new)
        })
        .service(app));
    let conn = graceful.watch(builder.serve_connection_with_upgrades(TokioIo::new(stream), svc));
    tokio::spawn(async move { let _ = conn.await; });
}
// then: drain_started.send(()); graceful.shutdown() raced against SHUTDOWN_DRAIN_TIMEOUT
```

`ConnectInfo<SocketAddr>` is inserted by hand because `axum::serve`'s
`IncomingStream` is private; `http/network.rs:30` and `main.rs:5016` read it
from extensions and keep working unchanged. The `Router` is the tower
service (`axum::Router: Service<Request<Body>>`), so no `into_make_service`
is needed. Why not `axum-server` (already compiled for hiqlite,
`http_builder()` at `axum-server-0.8.0/src/server.rs:223`): it installs no
timer either (no `Timer` anywhere in its `src/`), so it saves nothing and
adds a second direct HTTP-server dependency.

**Constants, with reasons.** `HEADER_READ_TIMEOUT = 15 s`: a LAN client
sends a request head in milliseconds; 15 s is generous for a phone on a bad
Wi-Fi hop and half of hyper's inert default. `H2_KEEPALIVE_INTERVAL = 20 s`
/ `H2_KEEPALIVE_TIMEOUT = 20 s`: an h2 connection with no streams is not
covered by the h1 head timer; a ping every 20 s closes a dead peer within
40 s, which is under the 60 s an Android box can hold an idle socket
through a suspend. No idle timeout on h1 beyond the head timer: §2's
correction — the timer re-arms at every `poll_read_head`.

**Handler deadlines.** One middleware, `handler_deadline(Duration)`, an
`axum::middleware::from_fn` that wraps `next.run(request)` in
`tokio::time::timeout` and, on expiry, answers
`503 {"code":"handler_deadline","message":"…"}` with `Retry-After: 2` via
`ApiError::typed` (`http/error.rs:52`). Not `tower_http::timeout::TimeoutLayer`:
it answers `408 Request Timeout`, which RFC 9110 §15.5.9 defines as "the
server did not receive a complete request message within the time it was
prepared to wait" — a client fault. A handler that could not finish is a
server condition, and clients already treat 503 with `Retry-After` as
"try again" (`cluster_capacity_gate` uses the same shape). The deadline
bounds the handler future *including extraction*, so a slow JSON body is
covered without a separate body-timeout layer.

The groups. The `api` router in `http/mod.rs:83-444` becomes three merged
sub-routers plus the untouched Plex and top-level routers. A route belongs
to exactly one group; a test enumerates the router and refuses a route in
none or two (§6):

| Group | Deadline | Routes | Why this budget |
|---|---|---|---|
| `json_short` | 30 s | `/server`, `/me`, `/settings` GET, `/developer/readiness`, `/libraries` GET, `/libraries/{id}/items`, `/items/{id}` GET/PATCH, `/hubs`, `/home/previews`, `/search*`, `/items/{id}/classification`, `/items/{id}/progress|scrobble|unscrobble|reading-state`, `/files/{id}/decision` GET+POST, `/files/{id}/audio-offset`, `/files/{id}/offline-options`, `/offline/packages/{id}` GET/DELETE + `/lease` + `/complete`, `/activity*`, `/scan/status`, `/scan/requests/{id}`, `/trakt/status`, `/system`, `/system/logs`, `/system/playback-events`, `/client-log`, `/users*`, `/keys*`, `/live-tv/readiness`, `/live-tv/channels`, `/live-tv/guide`, `/live-tv/guide/readiness`, `/dvr/*`, `/library-channels/*`, `/analysis/*` GET, `/coming-soon`, `/monarr/status`, `/dv-conversions`, `/files/{id}/dv-conversion`, `/cluster/nodes` GET, `/cluster/status`, `/cluster/ingress`, `/cluster/media` | Reads and small writes. On hiqlite a consistent read is one leader round trip; 30 s is ten times the store's own `TimedClient` budget and still finite. `/files/{id}/decision` carries its own 2 s decode-fact budget inside. |
| `json_long` | 300 s | `/setup`, `/auth/login`, `/auth/logout`, `/settings` PUT, `/libraries` POST/PUT/DELETE, `/libraries/{id}/schedule|scan|refresh|identity-repairs/*|dv-conversion|dv-conversions|root-identity/reset`, `/scan` POST, `/items/{id}/reanalyze|refresh-artwork`, `/files/{id}/analysis`, `/files/{id}/timeline-annotations/{kind}`, `/analysis/jobs/{id}*` mutations, `/analysis/reopen`, `/system/storage`, `/system/search-index/rebuild`, `/system/library-shape`, `/trakt/link|sync`, `/live-tv/readiness/refresh`, `/live-tv/guide/refresh`, `/files/{id}/offline-packages` POST, `/files/{id}/publication` POST, `/cluster/*` mutations (join tokens, promote, maintenance, election, leave, protocol, remove, join redeem/finalize, restart-preparation), `/cluster/support-bundle` | Legitimately long: a settings write fans out to every node; a scan trigger acquires a cluster lease; `logout`/user mutations run the two-phase revocation whose own fences are `FANOUT_TIMEOUT = 2 s` × `MAX_STABLE_ROSTER_PASSES` plus `LOCAL_CLAIM_APPLY_TIMEOUT = 1.5 s` (`internal_auth_revocation.rs:24-31`); `login` waits `PASSWORD_HASH_ADMISSION_WAIT = 2 s` then Argon2. 300 s is above every internal deadline so the middleware never pre-empts a fence mid-protocol — cancelling a revocation between Begin and End is exactly what `arm_ambiguity()` (`extract.rs:157`) exists to survive, and the plan does not make it routine. |
| `media` | none | `/files/{id}/direct|download|content|stream.mp4|subs/*|hls/start|photo`, `/files/{id}/hls/sessions` POST, `/hls/{session}/*` (all), `/stream/{id}/status`, `/publication/{session}*`, `/offline/media/*`, `/live-tv/channels/{channel}/sessions` POST, `/live-tv/sessions/*`, `/live-tv/starts/*`, `/images/{filename}`, `/items/{id}/photo`, `/cluster/artwork/{filename}`, `/cluster/media/offers` | Streaming bodies, blocked GETs, paced remuxes, live playlists and session creates that wait on a child. Each carries its own deadline (§2.2). A middleware deadline here would cancel a producer attempt mid-splice or cut a paced body. |

The Plex router (`mod.rs:450-469`) gets `json_short` on everything except
`/library/parts/*` and `/photo/:/transcode` (media). The top-level router
(`:471-`) gets `json_short` on `/healthz`, `/readyz`, `/metrics`, the asset
routes and `/connect.svg`; `/download/plurx-android.apk` and every
`/internal/*` route are `media` (peer relays carry their own deadlines).

Where a deadline fires on a write, the write may still commit — the same
truth C5 states for HTTP clients. The 503 body says so
(`"the request may still complete; re-read before retrying a mutation"`), and
`arm_ambiguity` already covers the one protocol where that matters.

**Metric.** `plurx_http_handler_deadlines_total{group="json_short|json_long"}`
(two label values, hand-rendered like `telemetry.rs:295-310`), appended to
`/metrics` in `http/system.rs:4991`. Zero is the expected steady state; a
non-zero rate on `json_short` is the alert.

### 3.2 M2 — precompressed, versioned, validated assets

**Precompression.** Beside `ASSET_HASHES` (`web.rs:127`), a
`static ASSET_GZIP: LazyLock<Vec<Box<[u8]>>>` compressing each `WEB_ASSETS`
row once with `flate2::write::GzEncoder` at `Compression::best()` (a
one-time cost; the shell is served for the life of the process). Force it
in `main` before `bind_listener` with
`tokio::task::spawn_blocking(|| LazyLock::force(&web::ASSET_GZIP))` so the ~2.6 MB deflate never runs on a runtime worker during a request.
`hls.min.js` (543,002 bytes) is included because M2 makes it a row (below).

**Negotiation.** A pure function `fn accepts_gzip(accept_encoding: Option<&str>) -> bool`
implementing RFC 9110 §12.5.3: split on `,`, parse `coding;q=` weights,
`gzip;q=0` and `*;q=0` refuse, `*` with q>0 accepts unless `gzip` is named
with q=0, absent header means identity only (the review's "substring test
accepts `gzip;q=0` incorrectly" is the bug this avoids). Unit-tested with
the table: `gzip` · `gzip;q=0` · `*` · `*;q=0, gzip` · `br` · absent ·
`GZIP` (case-insensitive) · `identity;q=0` (still identity; we never 406).

**Response shape** for `asset()` and every sidecar handler:

| Header | Identity | Gzip |
|---|---|---|
| `Content-Encoding` | absent | `gzip` |
| `Content-Length` | raw length | compressed length |
| `ETag` | `"<16hex>"` | `"<16hex>-gz"` |
| `Vary` | `Accept-Encoding` | `Accept-Encoding` |
| `Cache-Control` | `public, max-age=31536000, immutable` | same |
| `X-Content-Type-Options` | `nosniff` (M3 adds it; M2 may land it here first) | same |

Two ETags because RFC 9110 §8.8.3.3 requires a strong validator to differ
between representations that differ in content-coding; a shared cache that
stored the gzip body under the identity ETag would serve compressed bytes to
a client that refused them. `If-None-Match` is honoured on both: a match on
either tag answers `304` with the same `ETag`/`Vary`/`Cache-Control` and no
body. The path stays the identity and `?v=` stays ignored, exactly as
`web.rs:202-206` says.

**`hls.min.js` becomes a `WEB_ASSETS` row.** Row `("hls.min.js",
WebAsset::BodyScript, include_str!("../web/hls.min.js"))` inserted as the
first body row (before `core/app.js`), and the shell tag moves from line 33
to just before `core/app.js` at line 36. That position satisfies
`web_assets_match_the_shell`'s "every body row loads after the last sidecar"
rule without weakening it, and hls.js depends on nothing in the shell.
Contracts updated together, as the assessment's F-web-2 requires:
`SIDECARS` in `web.rs:475-484` (drop `hls.min.js`), `tests/web/shell-source.js:30`,
[WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) §3 (new row 4) and §4
(six sidecars, not seven), and `tests/web/asset-layout.test.js` follows the
doc. The `require("../../crates/plurxd/src/web/hls.min.js")` in
`tests/playback/web-control.test.js:6198` reads the file by path and is
unaffected; the network-shaping test's `/assets/hls.min.js` route stays a
valid URL. `scripts/js-check:50-54` keeps skipping it by name. Risk to
check first: `tests/web/asset-graph.js` will now parse a 543 KB minified
file for binding analysis; if it cannot, give the graph an explicit
by-name skip for vendored rows rather than weakening the rule. The route
`/assets/hls.min.js` (`mod.rs:476`) is deleted — the catch-all
`/assets/{*path}` serves the row.

**The six remaining sidecars and `reader.css`.** Hashed through the same
tag rewrite: a `SIDECAR_HASHES` beside `ASSET_HASHES`, `SHELL` rewriting
their tags to `?v=<hash>`, and their handlers serving
`public, max-age=31536000, immutable` + ETag + gzip through one shared
`serve_static(name, body, gz)` helper. This is W5's "hash them via the tag
rewrite". Their routes and paths do not move: `reader.js` and
`offline-reader.js` are Xcode and Gradle inputs by path
([WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) §4), forty tests
`require()` them by path, and the native readers navigate to `/` and let the
rewritten shell name them. `served_shell_versions_every_asset_it_names`
(`web.rs:645`) changes from "sidecars keep unversioned URLs" to "every tag
is versioned" and its `?v=` count becomes `WEB_ASSETS.len() + SIDECARS.len()`.

**Not done.** No `CompressionLayer` on any router: it would negotiate on
`/hls/*`, `/api/v1/files/*` and the Plex parts, buffering media through
deflate. No compression of the shell itself (97 lines) or of JSON API
responses — a later item, measured separately. The 196 KB of base64 fonts
inside `app.css` (W5 second half) is not split here; F-web-4 shows gzip
already recovers most of the base64 overhead, so the remaining win is
cache granularity, to be measured after M2 with the protocol in §6.3.

### 3.3 M3 — security headers on the shell and static assets

One helper, `fn shell_headers() -> [(HeaderName, HeaderValue); 4]`, applied
by `index()` and `fallback()` (the shell) and, minus the CSP, by
`serve_static`, `manifest()`, `icon()` and `download_android()`:

| Header | Value | Why |
|---|---|---|
| `X-Content-Type-Options` | `nosniff` | Stops a browser executing a mistyped asset as script; `publication.rs:391` already sends it. |
| `Referrer-Policy` | `same-origin` | The app calls only its own origin; nothing outside should learn `/?native-reader=1` or a hash route. `no-referrer` (what publication resources use) would also drop the referrer on same-origin fetches that some upstream loaders use for diagnostics — `same-origin` keeps those and leaks nothing cross-origin. |
| `Content-Security-Policy` (shell only) | `frame-ancestors 'none'; base-uri 'none'; object-src 'none'` | No page frames the shell (§2.4), nothing sets `<base>`, nothing embeds plugins. Each directive is checked below. `script-src` is deliberately absent: 398 inline handlers and `hls.js`'s worker (blob) would break; that migration is W6-then. |
| `Cross-Origin-Resource-Policy` | `same-origin` on assets | Assets are for this origin's shell; `publication.rs:394` already does this for resources. |

**Checked against the embeds** (all from §2.4, each an acceptance item):

- Browser reader: the shell frames *publication resources*; they answer
  `frame-ancestors 'self'` and keep doing so. The shell's own
  `frame-ancestors 'none'` restricts who may frame the shell, not what the
  shell may frame. Verified by opening a book in Chrome and Safari after M3.
- Native readers: `WKWebView`/`WebView` load `/` top-level; CSP
  `frame-ancestors` has no effect on a top-level document. Verified on one
  iPhone and one Android phone (§6.4 prompt).
- `offline-reader.html`: not served; unaffected.
- `hls.js` worker: `enableWorker:false` today (`player.js:560`); when Q10
  turns it on it uses a blob worker, which `object-src` and `base-uri` do
  not govern. Noted so Q10's PR does not have to rediscover it.

Also: `X-Frame-Options: DENY` is *not* added — CSP `frame-ancestors`
supersedes it in every browser the client matrix names
([CLIENTS.md](../CLIENTS.md)), and sending both invites a mismatch later.

## 4. Guardrails (non-goals)

Each with the assessment row it honours and how.

- **No deadline on a streaming route** (2.5 "apply handler deadlines by
  semantics"; F-core-2 "specify each … streaming exception separately").
  The `media` group is explicit and the route-inventory test refuses an
  unassigned route, so a new route cannot silently inherit a deadline.
- **Keep every body limit and gate** (C2 "existing route/body and
  publication deadlines remain meaningful"). M1 only *adds* a layer; the
  diff to `mod.rs` moves routes between sub-routers and touches no
  `DefaultBodyLimit`. Acceptance greps the count of `DefaultBodyLimit::max`
  before and after: 25 = 25.
- **The middleware never pre-empts a fence** (correction 1 / C7). `json_long`
  is sized above the revocation protocol's own deadlines; the logout path's
  `arm_ambiguity` stays the owner of the cancelled case.
- **Negotiate, do not substring** (F-web-1). `accepts_gzip` is pure and
  table-tested, including `gzip;q=0` and `*;q=0`.
- **Validators and `Vary` on every compressed representation** (2.5, F-web-3
  "adding ETag alone without evaluating If-None-Match does not produce
  304"). Both are in the response table and the 304 path is tested.
- **No `CompressionLayer` on the router** (§2.5 "it would touch `/hls/*`").
  Compression is per handler, on static bytes precomputed once.
- **Preserve sidecar paths, native build inputs and load order** (F-web-2,
  W5, F-web-3). Only `hls.min.js` changes table membership; its file stays
  put; the other sidecars keep their routes. The three ordering gates stay
  and are run.
- **CSP subset only; no `script-src`** (W6 "the restricted CSP subset can
  precede handler migration"; F-web-8 "include blob workers if retained").
- **No feature gate.** None of M1–M3 needs a switch. If a deployment behind
  a reverse proxy needs a different header timeout, the constant becomes a
  `[server]` config key in a follow-up; it is not a replicated setting
  because a proxy sits in front of one node, not the cluster.
- **Measure before crediting a number** (2.5 / W1 "script execution order
  does not prove sequential network fetching"). §6.3 is the protocol; the
  PR body carries its table, not a percentage.

## 5. Milestones

### 5.1 M1 — listener timer, h1 header timeout, h2 keepalive, handler deadlines (`server/http-listener-timeouts`)

1. Add the `hyper`/`hyper-util` edges (§3.1). `cargo tree -d -p plurxd |
   grep -E 'hyper|hyper-util'` shows no new duplicate.
2. Rewrite the body of `serve()` per §3.1 keeping its signature, the
   `drain_started` oneshot, `SHUTDOWN_DRAIN_TIMEOUT` and the progress
   drain. `main.rs:4402-4501` tests pass unchanged.
3. Add `handler_deadline` in `http/mod.rs`, split `api` into the three
   sub-routers, apply the Plex/top-level assignments, add the counter.
4. Add `route_deadline_inventory_is_total` (a test that walks a fresh
   `router()` through `axum::Router`'s route list — or, if the router does
   not expose it, the same `(method, path)` table the learner/maintenance
   tests already keep — and asserts each route is in exactly one group).
5. Tests: `header_read_timeout_closes_a_silent_connection` (connect, send
   `GET / HTTP/1.1\r\n`, sleep past a test-shortened timeout, expect EOF);
   `keep_alive_idle_is_bounded_by_the_header_timer` (one full request, then
   idle, expect EOF — the §2 correction); `h2_keepalive_is_configured` (a
   prior-knowledge h2 client sees PING frames within the interval);
   `json_short_handler_deadline_answers_503` (a test route that sleeps);
   `media_routes_have_no_handler_deadline` (a `/hls/{session}/{segment}`
   stub that sleeps longer than `json_long` still answers 200);
   `deadline_does_not_cancel_a_logout_fence` (logout under an injected
   slow peer completes or fails through `propagation_error`, never through
   `handler_deadline`).

Acceptance: `cargo test -p plurxd http::deadline serve::` green;
`grep -c 'DefaultBodyLimit::max' crates/plurxd/src/http/mod.rs` prints 25
before and after; on `lab1` after deploy,
`(printf 'GET / HTTP/1.1\r\nHost: lab1\r\n'; sleep 20) | nc 10.42.1.11 32400`
returns before the sleep ends (connection closed by the server at 15 s), and
`curl -s http://10.42.1.11:32400/metrics | grep plurx_http_handler_deadlines_total`
shows both groups at 0.

### 5.2 M2 — gzip, hashes and validators for the web shell (`web/asset-delivery`)

1. `ASSET_GZIP`, `SIDECAR_HASHES`, `accepts_gzip`, `serve_static` (§3.2);
   force the LazyLock in `main` before the bind.
2. `hls.min.js` row + tag move + `SIDECARS`/`shell-source.js`/layout doc
   §3–§4 updated in the same commit; delete the `/assets/hls.min.js` route.
3. Sidecar handlers go through `serve_static`; `SHELL` versions their tags;
   the two `web.rs` tests updated as §3.2 says.
4. Tests: the `accepts_gzip` table; `asset_gzip_and_identity_have_distinct_etags`;
   `if_none_match_answers_304_for_either_representation`;
   `vary_is_sent_on_both`; `every_shell_tag_is_versioned`;
   `hls_js_is_a_table_row_and_the_first_body_row`.

Acceptance: `cargo test -p plurxd http::web` green; `make web-check` and
`node --test tests/web/` green (asset-order, asset-load, asset-layout,
shell-source); `curl -sI -H 'Accept-Encoding: gzip' http://lab1:32400/assets/player/player.js`
shows `content-encoding: gzip`, `vary: Accept-Encoding`, an `etag` ending
`-gz"`; the same with `-H 'Accept-Encoding: gzip;q=0'` shows no
`content-encoding`; a second request with `If-None-Match` returns 304; the
§6.3 waterfall table is in the PR body.

### 5.3 M3 — security headers on the shell (`web/security-headers`)

1. `shell_headers()` and its application (§3.3).
2. Tests: `shell_sends_nosniff_referrer_policy_and_frame_csp`,
   `assets_send_nosniff_and_corp`, `shell_csp_has_no_script_src` (so
   nobody adds it without W6-then), `publication_resources_still_allow_self_framing`
   (unchanged behaviour, pinned).

Acceptance: `cargo test -p plurxd http::web::security` green;
`curl -sI http://lab1:32400/ | grep -iE 'content-security-policy|referrer-policy|x-content-type-options'`
prints the three; a book opens and pages in the browser reader on Chrome
and Safari; the §6.4 prompt reports both native readers open a book.

## 6. Verification and rollout

### 6.1 Test lanes

Per milestone: the focused `cargo test -p plurxd <filter>` above, then
`make unit` once before un-WIP. M2 additionally runs `make web-check` and
`node --test tests/web/`. Each PR is a draft into `main` under the fast
lane; `make validate-staged` before every push.

### 6.2 Rollout

Deploy M1 to `lab1` first (Ansible, one host), watch
`plurx_http_handler_deadlines_total` and the access log for one evening of
playback from the living-room Apple TV and one Android box, then the fleet.
M2 and M3 are shell-only and go to the fleet directly after `lab1` passes
their acceptance. Rollback is the previous `sha-` image
([OPERATIONS.md](../OPERATIONS.md) §308); nothing here changes schema or
recipe identity, so nothing invalidates.

### 6.3 Cold/warm waterfall protocol (M2's evidence)

Run before and after M2 on the same client and network, three runs each,
report the median:

1. Chrome desktop on the LAN, DevTools → Network, "Disable cache" **on**
   → load `http://lab1:32400/` → record: request count, transferred bytes,
   `DOMContentLoaded`, `Load`, and the timestamp `boot()` logs its first
   `/server` call (this is "time to usable UI"). This is the **cold** row.
2. Same tab, "Disable cache" **off**, reload → the **warm** row (expect 304s
   for the shell and sidecars after M2, immutable hits for rows).
3. Repeat (1) on the Android TV browser or the tvOS Safari proxy if
   available (this is the client the 2.6 MB hurts); if not, Chrome with
   "Fast 3G" throttling as the stand-in, labelled as such.
4. Export each run as HAR into
   `docs/evidence/web-asset-delivery/<date>-<client>-<cold|warm>.har`.

The PR body carries a five-column table (client, cold/warm, requests,
bytes, time-to-`/server`). No percentage is quoted without its row.

### 6.4 What only a device can prove — GPT prompt

```text
On the iPhone and one Android phone signed in to lab1 (10.42.1.11):
1. Open Settings → Developer and confirm the server build is the one
   carrying PR <M3 number>.
2. Open any EPUB from the Books library in the native reader. Page forward
   three times and back once. Report: did the book render, did paging work,
   any blank frame or error banner.
3. In the browser on each phone, open http://10.42.1.11:32400/, sign in,
   open the same EPUB, page forward. Report the same.
4. On the Android phone only: open http://10.42.1.11:32400/ in Chrome,
   chrome://inspect from a laptop, and paste the response headers of the
   document request (Content-Security-Policy, Referrer-Policy,
   X-Content-Type-Options).
Report exact text of any error; do not retry more than once.
```

## 7. Open questions

1. **Deadline for `/api/v1/files/{id}/hls/sessions` POST.** It is in the
   `media` group because a create can wait on a producer's first
   publication. If Paul wants a ceiling, it must be above `START_DEADLINE`
   (50 s) and outside this plan's tests; flagging rather than choosing.
2. **`RequestBodyTimeoutLayer` on the `media` POSTs** (`/hls/{session}/control`,
   `/live-tv/channels/{channel}/sessions`): their bodies are small and
   bounded by size; a slow-body sender on these routes is not covered by
   M1. Add `tower-http`'s `timeout` feature and the body layer if the
   `lab1` soak shows such connections; not adding it blind.
3. **Font splitting (W5 second half).** After M2's waterfall shows the
   `app.css` transfer size and cache behaviour, decide whether `.woff2`
   rows with `preload` earn a fourth PR.
4. **`Referrer-Policy` choice.** `same-origin` vs `no-referrer`: the plan
   picks `same-origin` for the reason in §3.3; if any client-side
   diagnostic turns out to read `document.referrer` cross-origin, this is
   the one line to revisit.
