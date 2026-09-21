# Live TV guide and UI — a real channel list, a real grid, and a player that follows you

**Status:** open — built baseline; native TV presentation rules superseded by
[LIVE-TV-NATIVE-LAYOUTS-STATUS.md](LIVE-TV-NATIVE-LAYOUTS-STATUS.md) · **Executes:** the layout decision of 2026-09-07
(list view + grid view with a switch; HDHomeRun guide first, XMLTV when
configured; web, Apple and Android) · **Written:** 2026-09-07 ·
**Effort branch:** `effort/live-tv-guide` · **Renderings:**
[`docs/mockups/live-tv/`](../mockups/live-tv/) (also on the design canvas
"Live TV Layouts" in Paul's claude.ai artifact gallery)

Companion to [HDHOMERUN-LIVE-TV-PLAN.md](HDHOMERUN-LIVE-TV-PLAN.md) (the
tuner contract — one owner, one ingest, bounded live HLS, no DVR) and
[HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md) (what is proved
on a real FLEX 4K). This document is *what the Live TV page becomes, where
programme data comes from, and how fullscreen and picture-in-picture work on
every client* — the plan an agent executes milestone by milestone.

**How to work this plan.** Read §1–§4 once, then build §7's milestones in
order; each ends with an acceptance check that is a command or an observable
fact. §5 is the guardrail list — if a step seems to require crossing one of
those lines (storing `DeviceAuth`, a second tuner GET, a seek bar on a live
stream, a new outbound host), stop and flag it instead of improvising. Every
literal in §3 was copied from the code on 2026-09-07 at the cited line;
re-verify at build time, `main` moves.

The pipeline rules apply unchanged: task PRs target the effort branch as
`WIP:` drafts until they are ready, the effort's PR into `main` gets the one
full qualification run, and a change to what the system does changes the
docs in the same commit
([DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md),
[docs/README.md](../README.md) for where documents go).

## 1. Objective — the page should say what is on

Today the Live TV page is one column: a small player card, a status
sentence, a toolbar, then a wall of identical cards reading `7.1 · WABC ·
Available · Watch live`
([`viewLiveTv`](../../crates/plurxd/src/web/index.html) around line 13583,
[`renderLiveTvChannels`](../../crates/plurxd/src/web/index.html) around
13601). Nothing on it says what is playing on any channel, fullscreen is the
bare `<video>` element with no overlay, there is no picture-in-picture, and
leaving the route tears the stream down
([`render()`](../../crates/plurxd/src/web/index.html) line 17104:
`if(h!=="#/live-tv") stopLiveTv()`). The Apple and Android screens are the
same shape in SwiftUI and Compose. `before.png` in the mockups folder is the
web page as it is.

The finished effort delivers, on all three clients:

- **A channel row that carries information**: network chip · number ·
  callsign · what is on now · a progress bar to the end of the programme ·
  what is next · favorite star or a lock for protected channels. Protected
  channels are dimmed, never hidden by default, and a `Hide protected`
  filter exists because a lineup with thirty DRM channels is mostly noise.
- **Two views of the same page, one switch**: the **list view** (dense
  vertical list beside the player, the selected channel's evening as a
  timeline strip under the video) and the **grid view** (the classic
  half-hour grid with a red *now* line, the player docked small and
  expandable). The switch is in the page heading and is remembered per
  browser / per device. `list-view.png` and `grid-view.png` are the
  targets; §4 has the same layouts as ASCII.
- **Programme data** from the HDHomeRun's own guide service by default, or
  from an admin-supplied XMLTV source when configured. The guide is
  information only: there is no recording, no reminders, no "tune at 9",
  because plurx has no DVR ([HDHOMERUN-LIVE-TV-PLAN.md §7](HDHOMERUN-LIVE-TV-PLAN.md#7-non-goals--guardrails-for-the-first-complete-release)).
- **A fullscreen that is a surface, not a bare element**: channel and
  programme in the top-left, a hover strip of neighbouring channels along
  the bottom, a progress bar, the controls, all auto-hiding after 4 s —
  `fullscreen.png`.
- **Picture-in-picture, two layers**: an in-app dock that keeps the stream
  playing while you browse the library (`pip-dock.png`), and the platform's
  native PiP window (browser / iOS / Android) from a button on that dock and
  on the player.
- **A phone layout** (player pinned at the top, list beneath, landscape is
  fullscreen — `phone.png`) and **a ten-foot overlay** for Apple TV and
  Android TV (video always fullscreen, a directional press only reveals the
  overlay, preview-then-commit channel list — `tv-overlay.png`).

What does not change: one owner, one tuner GET per session, the capability
lease and its keepalive/status rules, the 30 s no-progress release, the
uncertainty barrier, DRM failing before a tuner opens. This effort is a UI
and a read-only data feed on top of that contract; §5 keeps it that way.

## 2. Requirements — the behaviour a finished effort proves

### 2.1 Programme data is a feed the tuner contract never depends on

The guide is fetched by the owner node, cached in memory, and served to
every client through one authenticated endpoint. Its failure modes are
rendered, never fatal: a channel list without programme lines is the
degraded state, and `Watch` must work with the guide off, stale, or
erroring. `GET /live-tv/channels` and session start are not touched by
guide state and never wait on a guide fetch.

Sources, in order of precedence when both are configured:

| Source | Selected by | Data | Egress |
|---|---|---|---|
| `hdhomerun` | `live_tv.guide_source = "hdhomerun"` (default once enabled) | Silicondust's guide service, keyed by the device's `DeviceAuth` from `discover.json` | one pinned `https` host; the owner node only |
| `xmltv` | `live_tv.guide_source = "xmltv"` + `live_tv.xmltv_url` | any XMLTV document (Schedules Direct grabbers, tv_grab_*) | the configured URL; the owner node only |
| `off` | `live_tv.guide_source = "off"` | none — rows show number, callsign, favorite/lock | none |

"HDHomeRun first" means: a fresh install that enables Live TV gets the
HDHomeRun guide with no further setup; XMLTV is the override for someone who
already runs a grabber. Both are admin choices on the Developer card. The
guide is **off** until an admin turns it on, because it is the first
outbound internet call the tuner owner makes on the operator's behalf
([SECURITY.md](../SECURITY.md) lists every outbound host; §3.3 adds one).

### 2.2 The `DeviceAuth` policy changes by exactly one clause

[HDHOMERUN-LIVE-TV-PLAN.md §3.2](HDHOMERUN-LIVE-TV-PLAN.md#32-deviceauth-and-capabilities-never-cross-the-browser-boundary)
says plurx parses `DeviceAuth` only to ignore it. The guide needs it. The
new rule: **the owner reads `DeviceAuth` from `discover.json` at the moment
of each guide refresh, sends it to the guide host over TLS, and forgets
it.** It is never written to the store, never in a snapshot or any internal
relay body, never in a log line, a metric label, an error message, or an
API response, and it never crosses to a non-owner node — the guide *result*
is relayed, not the credential. Silicondust rotates `DeviceAuth`; reading it
fresh each refresh is also why the guide keeps working after a rotation.

### 2.3 Both views are the same page, and the switch is cheap

The list view and the grid view render from the same channel + guide state
and share the player, the filters, the search and the heading. Switching
views never stops or restarts the stream and never refetches; it is a
re-render of the browse region. The choice persists: `localStorage
plurx_live_tv_view` on the web, `UserDefaults` on Apple, DataStore on
Android, defaulting to `list`.

### 2.4 The player survives the route, and picture-in-picture keeps the lease

Leaving `#/live-tv` (or the Live TV tab) with a channel playing moves the
video into a dock instead of stopping it; the lease, keepalive and status
polling continue exactly as on the page. Stopping is always explicit (the
stop button on the page, the dock, or the overlay) or the existing
watchdogs (30 s without decoded frames; the server's idle expiry). Native
PiP is the same media element / `AVPlayerLayer` / `PlayerView` handed to
the platform; entering it must not stop the heartbeat, and the existing
"stop when hidden / not active" rules are narrowed to *hidden and not in
PiP* (§3.4, §3.6, §3.7).

### 2.5 Fullscreen and the ten-foot overlay obey one input table

Every client implements the Live TV input table in §3.9 — the same rulings
Paul made for the finite-media player on 2026-09-02, applied to a stream
with no timeline: a directional press on a hidden overlay only reveals it;
the channel list is preview-then-commit (move focus, Select tunes, Back
hides); nothing seeks, because there is nothing to seek. `hide_after_ms` is
the contract's 4000 and controls hide only while playing.

## 3. Contract — exact interfaces

Copied from the code on 2026-09-07; the cited files are the source of truth
at build time.

### 3.1 Settings keys (`crates/plurx-core/src/store/mod.rs`, `keys` around line 950)

Three new keys beside the existing `live_tv.*` set:

| Key | Default | Bound | Editable while Live TV is enabled |
|---|---:|---|---|
| `live_tv.guide_source` | `off` | `off` · `hdhomerun` · `xmltv` | **yes** — the guide never touches the tuner |
| `live_tv.xmltv_url` | empty | `http`/`https` URL, ≤ 1024 bytes, no userinfo; required when source is `xmltv` | yes |
| `live_tv.guide_hours` | `24` | `4..=72` — how far ahead the cache tries to fill | yes |

They ride the existing generation CAS: `PUT /api/v1/settings` with
`live_tv_guide_source`, `live_tv_xmltv_url`, `live_tv_guide_hours` and the
mandatory `live_tv_config_generation`
([`UpdateSettings`](../../crates/plurxd/src/http/system.rs) around line
1944; rules at 2165–2192). The "address/owner/sessions/height only while
disabled" rule at 2184–2192 gets a carve-out for these three: they are
information settings, not tuner settings, and an operator must be able to
turn the guide on without draining every viewer. They are mirrored into
`SettingsDto` (1544–1551, populated at 1854–1864) as `live_tv_guide_source`,
`live_tv_xmltv_url`, `live_tv_guide_hours`. Changing any of them bumps the
generation like every other live-TV write, which is what invalidates the
owner's guide cache (§3.3).

### 3.2 Public HTTP (`crates/plurxd/src/http/live_tv.rs`, routes in `http/mod.rs` 90–119)

One new authenticated route, registered beside `GET /live-tv/channels`:

```
GET /api/v1/live-tv/guide?from=<unix seconds>&hours=<1..=72>
    auth: AuthUser (any signed-in profile), same as /live-tv/channels
    route matrix: same eligibility as /live-tv/channels — refused on
      learners and in maintenance mode (mod.rs 565, 667)
```

`from` defaults to now − 3600 (so the programme that started before you
opened the page is present); `hours` defaults to `live_tv.guide_hours`.
The response is the cached window clipped to the request; it never triggers
a fetch (the refresh loop does — §3.3), so it is always fast and always
answers:

```json
{
  "source": "hdhomerun",
  "freshness": "fresh",
  "age_seconds": 412,
  "fetched_at": 1789000000,
  "window": { "start": 1788996400, "end": 1789086400 },
  "refresh_error": null,
  "channels": [
    {
      "id": "7.1",
      "guide_number": "7.1",
      "affiliate": "ABC",
      "image_url": "https://…/logo.png",
      "programmes": [
        {
          "start": 1789000800,
          "end": 1789002600,
          "title": "City Beat",
          "episode_title": "Pier 40",
          "episode": "S3E14",
          "synopsis": "…",
          "image_url": null,
          "original_air_date": "2026-09-07",
          "filters": ["News"]
        }
      ]
    }
  ]
}
```

Field rules, all bounded because the guide host is untrusted input:

| Field | Bound | Absent when |
|---|---|---|
| `source` | `off` · `hdhomerun` · `xmltv` | never |
| `freshness` | `fresh` (≤ refresh interval) · `stale` (older, ≤ 6 h) · `unavailable` (no cache, or source `off`) | never |
| `channels[].id` | matches a `LiveTvChannel.id` from `/live-tv/channels` | channels without guide rows are simply absent |
| `affiliate` | ≤ 32 bytes, no control chars | not provided |
| `image_url` | `https` only, ≤ 512 bytes; **passed through, not proxied** (§5) | not provided |
| `programmes[]` | ≤ 200 per channel per response, sorted by `start`, non-overlapping after normalisation | — |
| `title` | ≤ 256 bytes | never — a programme without a title is dropped |
| `episode_title` · `synopsis` | ≤ 256 · ≤ 1024 bytes | not provided |
| `episode` | `S<n>E<n>` or `E<n>`; anything else dropped | not provided |
| `filters` | ≤ 8 entries, each ≤ 32 bytes | none |

The whole body is capped at `MAX_GUIDE_RESPONSE_BYTES = 2 MiB` (512
channels × ~30 programmes fits with room); a cache that would exceed it is
truncated by dropping the furthest-out programmes, never by dropping
channels. Errors use `ApiError::typed` (`http/error.rs` 117) with one new
code, `guide_unavailable` (503), used only when the guide is *configured*
and has no cache at all; `source: off` is a 200 with `freshness:
unavailable` and an empty `channels` array. Every error message passes
`sanitize_public_error` (`http/live_tv.rs` 892) — no URL, no `DeviceAuth`.

`GET /live-tv/channels` is unchanged. Clients call both and join on `id`.

### 3.3 The guide engine (`crates/plurxd/src/live_tv.rs`, new `guide` module)

**Placement.** A `LiveTvGuide` struct owned by `LiveTvManager`
(`live_tv.rs` ~807), sibling of the lineup `SnapshotCache` (657–796) and
following its shape: process memory on the owner, a TTL, a stale window, a
single in-flight refresh with followers reusing the result, generation
invalidation. Nothing in the store — the `settings` table is replicated to
every voter on every write and copied whole into every
`settings_snapshot()` (§6 of the survey behind this plan), which is the
wrong place for a multi-hundred-KB blob that changes every twenty minutes.
A durable cache is a non-goal (§5).

**Refresh loop.** A tokio task on the owner, started when `config.enabled
&& guide_source != off`, stopped on disable/owner change through the same
drain boundary as sessions:

| Constant | Value | Why |
|---|---:|---|
| `GUIDE_REFRESH_INTERVAL` | 20 min | Silicondust's own clients poll on this order; the free tier only ever answers a few hours ahead, so anything slower leaves the end of the grid empty |
| `GUIDE_STALE_TTL` | 6 h | after that the cache is dropped and `freshness: unavailable` is honest |
| `GUIDE_FETCH_TIMEOUT` | 15 s per request | a dedicated `reqwest::Client` with `timeout`, `redirect(Policy::none())`, `user_agent("plurx/<version>")`, `no_proxy()` — the same client shape as the lineup fetch (832–837) |
| `GUIDE_MAX_DOCUMENT_BYTES` | 4 MiB per response | XMLTV for a large lineup is a few MiB; anything larger is refused, not streamed |
| `GUIDE_MAX_REQUESTS_PER_REFRESH` | 1 + number of channels | one bulk call, then at most one per-channel extension |
| `GUIDE_MAX_PROGRAMMES_PER_CHANNEL` | 200 | bounds memory regardless of source |

Followers arriving during a refresh await the in-flight one; a forced
refresh (the Developer card's button, `POST /live-tv/guide/refresh`, admin)
is bounded by the same semaphore pattern as `MAX_FORCED_REFRESH_CALLERS`
(47). A refresh failure keeps the previous cache and records
`refresh_error` (sanitized). A settings generation change discards the
cache.

**HDHomeRun source.** Two HTTP steps per refresh, both on the owner:

1. `GET http://<device_ipv4>/discover.json` through the existing pinned,
   validated path (`pinned_url`, 3375) to read `DeviceAuth` — the field
   `DiscoverDocument` already deserialises and marks `#[allow(dead_code)]`
   (3163–3178; remove the allow, keep it private to the guide module,
   never `Debug`-print the struct).
2. `GET https://api.hdhomerun.com/api/guide?DeviceAuth=<…>` for the bulk
   window, then optionally per channel
   `…&Channel=<guide_number>&Start=<unix>` to extend toward
   `live_tv.guide_hours`, stopping early when a response is empty or the
   request budget is spent. The host is pinned by an allowlist exactly as
   `approved_artwork_url` pins artwork hosts
   (`http/comingsoon.rs` 534–560): `https`, port 443, host in
   `{"api.hdhomerun.com", "my.hdhomerun.com"}`, redirects refused. **Verify
   the host and path against a real device at build time** — third-party
   clients use both `api.hdhomerun.com/api/guide` and
   `my.hdhomerun.com/api/guide.php`; whichever answers with the documented
   shape is the one to pin, and the other stays in the allowlist only if it
   is observed to redirect. Query parameter `SynopsisLength=1024` keeps the
   body small.

   The observed response shape (Silicondust does not publish a schema; this
   is what every open-source client reads and what the hardware acceptance
   in M1 must confirm):

   ```json
   [
     { "GuideNumber": "7.1", "GuideName": "WABC", "Affiliate": "ABC",
       "ImageURL": "https://…",
       "Guide": [
         { "StartTime": 1789000800, "EndTime": 1789002600,
           "Title": "City Beat", "EpisodeTitle": "Pier 40",
           "EpisodeNumber": "S03E14", "Synopsis": "…",
           "OriginalAirdate": 1789000800, "ImageURL": "https://…",
           "Filter": ["News"] }
       ] }
   ]
   ```

   Mapping: `GuideNumber` → `channels[].id` (join key; rows whose number is
   not in the current lineup are dropped), `Affiliate` → `affiliate`,
   `EpisodeNumber` `S03E14` → `S3E14`, `OriginalAirdate` (unix) →
   `YYYY-MM-DD`, `Filter` → `filters`. Unknown fields ignored. The free tier
   answers roughly four hours for the bulk call and about eight per
   extension; a DVR subscription answers up to fourteen days — the engine
   asks for `guide_hours` and renders whatever came back, and the grid says
   where data ends (§4.2).

**XMLTV source.** `GET <live_tv.xmltv_url>` with the same client
(`Content-Encoding: gzip` accepted; a `.gz` body is also accepted by
sniffing the magic bytes, because grabbers write gzipped files). Parse
with `quick-xml` (already a workspace dependency — verify; add it with the
minimal features if not): `<channel id>` with its `<display-name>`s and
optional `<lcn>`; `<programme start stop channel>` with `<title>`,
`<sub-title>`, `<desc>`, `<episode-num system="xmltv_ns">` (convert
`2.13.` → `S3E14`), `<episode-num system="onscreen">` (`S03E14` accepted
as-is), `<category>` → `filters`, `<icon src>` → `image_url` (https only),
`<date>` → `original_air_date`. Times are XMLTV's `YYYYMMDDhhmmss ±hhmm`;
a missing offset is an error for that programme, not a guess.

Channel matching, in order, first hit wins: a `<display-name>` equal to
the lineup's `guide_number` · `<lcn>` equal to `guide_number` · a
`<display-name>` equal to `guide_name` (case-insensitive). Unmatched XMLTV
channels are dropped; lineup channels without a match simply have no
programmes. There is no manual mapping UI in this effort (§5); the
Developer card shows `matched N of M channels` so the operator knows.

**Relay.** Non-owner ingress nodes serve `/live-tv/guide` by asking the
owner, mirroring the snapshot relay (`http/live_tv.rs` `owner_snapshot`
764–849): a signed `POST /_internal/v1/live-tv/guide` with
`PeerAuthMode::ExactRequestAndResponse`, a 25 s deadline and a 2 MiB body
cap, returning the owner's public response verbatim. **Do not gate this on
`LIVE_TV_CAPABILITY`** (`membership.rs` 8327): an owner that predates the
guide answers 404 and the ingress renders `freshness: unavailable` with
`refresh_error: "owner does not serve a guide yet"` — a mixed fleet during
rollout must not flip every node to `live_tv_protocol_unready` over a
read-only feature. Ingress nodes keep a 60 s memory of the last relayed
body so a click-storm does not become a relay-storm.

**Activity and metrics.** `LiveTvActivity` (479–490) gains
`programme_title: Option<String>` (≤ 256 bytes; extend the bound check in
`internal_activity.rs` 393–399 and the escaped-rows test at 842–875); the
web Activity page shows it after the channel name. Metrics:
`plurx_live_tv_guide_refresh_total{source,outcome}` (`ok` · `error` ·
`skipped`), `plurx_live_tv_guide_age_seconds`, and
`plurx_live_tv_guide_programmes` (total cached rows).

### 3.4 Web: page, host, dock (`crates/plurxd/src/web/index.html`, `live-tv.js`)

**Pure logic goes in `live-tv.js`**, which is already a UMD module tested
directly by `tests/web/live-tv.test.js`. Add to its export:

```js
// Pure: (guide response, channel id, now in unix seconds) →
// { now: programme|null, next: programme|null, progress: 0..1|null }
function programmeAt(guide, channelId, now)
// Pure: (guide, channels, window {start,end}, slotSeconds=1800, pxPerSlot=240)
// → rows of positioned cells {channel, cells:[{left,width,programme,airing}]}
// and the x of the now line. Clips to the window; never overlaps.
function gridLayout(guide, channels, window, now, slotSeconds, pxPerSlot)
// Pure: (channels, guide, {query, filter:'all'|'favorites', hideProtected}) → visible channels
function filterChannels(channels, guide, opts)
// Pure: which channel is ±1 from `current` in the visible order (wraps).
function adjacentChannel(visible, currentId, delta)
```

Shipped page functions in `index.html` stay top-level, one per name, body
indented, closing brace at column 0 — `tests/web/live-tv.test.js` slices
them out by regex (`shipped(name)` at lines 8–14) and executes them with
stubbed globals.

**The media element leaves `#main`.** Today `<video id="live-tv-video">`
is inside the page markup (13587) and dies on every route change, which is
why `render()` stops Live TV at 17104. The rebuild adds one fixed host as a
sibling of `#modal` (index.html 3192–3193, outside `#app` so
`layoutChrome` never re-renders it):

```html
<div id="live-tv-host" class="lth" hidden>
  <video id="live-tv-video" playsinline aria-label="Live television"></video>
  <div class="lth-overlay">…channel/programme, strip, progress, controls, hints…</div>
</div>
```

Three modes, a `data-mode` attribute on the host:

| Mode | When | Geometry |
|---|---|---|
| `slot` | route is `#/live-tv` | the page renders an empty `<div id="live-tv-slot">` in the list or grid layout; a `ResizeObserver` on the slot (and a `scroll`/`resize` listener) positions the host over it. One observer for the document's lifetime, rebound when the slot is re-rendered — the same discipline as the rail observers (4058) |
| `dock` | route is anything else and a channel is playing | `position:fixed; right:24px; bottom:24px; width:384px` (`pip-dock.png`); draggable within the viewport by its caption bar; position remembered in `plurx_live_tv_dock` |
| `full` | fullscreen | the host is the fullscreen element (§3.5) |

`render()` 17104 changes from `stopLiveTv()` to `liveTvLeaveRoute()`:
dock if a lease is current, hide the host if not. `logout()` (3934) still
stops. The dock's caption bar carries chip · programme title · number ·
callsign · time left · LIVE pill · mute, and three icon buttons: **back to
Live TV** (navigates to `#/live-tv`, host returns to `slot`), **native PiP**
(§3.5), **stop**.

**Preferences** (`localStorage`, `try/catch` around every access as at
3133): `plurx_live_tv_view` (`list`|`grid`), `plurx_live_tv_filter`
(`all`|`favorites`), `plurx_live_tv_hide_protected` (`1`),
`plurx_live_tv_dock` (`{"x":…,"y":…}`), `plurx_live_tv_last` (channel id —
the list view pre-selects it on open; it does **not** auto-tune, a tuner is
a physical resource and opening a page must not take one).

**Page phases.** `pagePhaseName()` (17050) gains `if(route==="#/live-tv")
return "live-tv"`; `viewLiveTv` keeps emitting `shell` → `content` →
`settled` (`scripts/ui-baseline` 1551 waits on `settled` for this route).
`content` is stamped after channels render; the guide arrives second and
re-renders rows in place — the page is usable before the guide answers.

**Visibility.** The `visibilitychange` handler (13733–13738) stops Live TV
when the document hides. Narrow it: stop only when hidden **and** not in
picture-in-picture (`document.pictureInPictureElement === video ||
video.webkitPresentationMode === "picture-in-picture"`) **and** not docked
with the tab merely backgrounded for less than the server idle window — in
practice: if PiP, never stop on hide; otherwise keep today's behaviour. The
keepalive guard `if(document.visibilityState!=="hidden")` (13680) becomes
"visible or in PiP" for the same reason: a PiP window is playing, the
tuner is in use, and the lease must be renewed.

### 3.5 Web: fullscreen and picture-in-picture

**Fullscreen targets the host**, not the video, so the overlay is in the
fullscreen tree: `#live-tv-host.requestFullscreen()` →
`webkitRequestFullscreen` → `video.webkitEnterFullscreen()` on iPhone
(no overlay there; the native player's controls apply). This is exactly the
VOD order in `toggleFullscreen` (10209–10247); `exitLiveTvPresentation`
(13708–13720) already mirrors `exitPresentationModes` and stays.

**Overlay behaviour** (`fullscreen.png`): top-left chip · programme title ·
LIVE pill · number/callsign/times/next; top-right mute · PiP · guide ·
exit; bottom strip of neighbouring channels (7 cards, current centred and
highlighted, each with its now-title and progress); a progress bar with
minutes left; a hint row of keys. Auto-hide after 4000 ms while playing
(`.lth.idle` hides `.lth-overlay` and sets `cursor:none`, the `.player.idle`
pattern at 333–392); reveal on `mousemove` / `pointerdown` / `touchstart`
bound at window level while `data-mode="full"` — **not** on keydown, the
same deliberate choice as 10296–10302. The `guide` button opens the grid as
a sheet over the video (dimmed, 70 % height) without leaving fullscreen;
Escape or the button closes it.

**Picture-in-picture**: `togglePip()` at 10208 is the model —
`document.pictureInPictureElement ? exitPictureInPicture() :
video.requestPictureInPicture()`, plus Safari's
`webkitSetPresentationMode("picture-in-picture")`. Show the button only
when `document.pictureInPictureEnabled || video.webkitSupportsPresentationMode`.
Entering native PiP from the page leaves the host in `slot` mode showing
the poster-black surface with a "Playing in picture-in-picture" line and a
*Return* button; `leavepictureinpicture` restores the inline picture.

### 3.6 Apple (`clients/apple/Sources/LiveTv*.swift`, `PlayerSurface.swift`)

**API and models** (`LiveTv.swift`): add `LiveTvGuide`, `LiveTvGuideChannel`,
`LiveTvProgramme` decoded with the existing `.convertFromSnakeCase`
strategy (174–178); `LiveTvAPI.guide(from:hours:)` (bearer, control
session). `LiveTvPlayerController` (LiveTvView.swift 7–148) gains
`@Published guide: LiveTvGuide?`, a 20 min refresh `Task` while the tab is
alive, and `programme(for channel:)` built on the same `programmeAt` rule
as the web (now/next/progress).

**iOS layout** (`phone.png`): the player pinned at the top at 16:9 with
the LIVE pill, number/callsign and a progress bar drawn over the picture;
under it the programme line and mute/stop; then a segmented
`On now · Guide · Favorites` and the list. `Guide` is the grid: a
horizontally scrolling `ScrollView` per row sharing one `ScrollViewReader`
offset, 30 min = 160 pt on phones, a red now line; every cell opens the
programme sheet, whose first action is Watch. Rotation to landscape presents
the fullscreen cover with the §3.5 overlay (a SwiftUI overlay on
`PlayerSurface`, auto-hide via the `autoHideGeneration` task pattern in
`PlayerView.swift` 927–953).

**Picture-in-picture on iOS**: pass `allowsPictureInPicture: true` to the
live `PlayerSurface` (LiveTvView.swift 169 and 214). Nothing in the parity
doc's "explicit controls only" rule forbids it — that rule keeps the finite
controller out of live playback; `PictureInPictureController`
(PlayerSurface.swift 22–230) attaches to the `AVPlayerLayer` directly and
already sets `canStartPictureInPictureAutomaticallyFromInline`. Two
consequences to handle: the `scenePhase != .active → stop` rule (226–228)
becomes *stop unless PiP is active*, and the 5 s heartbeat keeps running in
PiP (it is a `Task` on the controller, not on the view, so it does — verify
with a test). The audio session must already be `.playback` for VOD PiP;
confirm `UIBackgroundModes` includes `audio` in `project.yml`, otherwise
PiP suspends on background.

**tvOS** (`tv-overlay.png`): the live surface is always fullscreen; there
is no inline player and no grid in this effort (§5 — a focus-navigable
half-hour grid on tvOS is its own milestone). The overlay is the §3.9
ten-foot table: hidden → any direction or Select reveals (a `.reveal`
focusable clear surface exactly like `PlayerView.swift` 738–758); visible →
Up/Down move focus through the channel list on the left (chip · number ·
callsign · now-title · until · progress), Select tunes, Back hides, 4 s of
no input hides. A `Channels · Favorites · Guide` row above the list; `Guide`
on tvOS is a per-channel vertical schedule (the selected channel's evening
as a list), not the grid. PiP is never offered on tvOS
([PLAYER-INPUT-CONTRACT.md](../clients/PLAYER-INPUT-CONTRACT.md) row
grammar: `pip` "never tvOS").

**View persistence**: `UserDefaults.standard` key `liveTvView`
(`list`|`grid`), iOS only.

### 3.7 Android (`clients/android/app/src/main/java/tv/plurx/app/livetv/`)

**API and state**: `LiveTvApi.guide(from, hours)` (`LiveTvApi.kt` 171–192
pattern); `LiveTvPlayerState` (`LiveTvPlayer.kt` 23–31) gains `guide:
LiveTvGuide?` and `programmeTitle: String?`; a 20 min refresh coroutine in
the singleton controller while a `LiveTvScreen` is composed.

**Phone / tablet layout** (`phone.png`, `FormFactor.Compact/Expanded` from
`ui/Layout.kt` 10–23): same anatomy as iOS. The grid is a `LazyColumn` of
rows whose cells are laid out with a shared horizontal `ScrollState`
(one state object passed to every row's `horizontalScroll`), 30 min =
160 dp, the now line drawn in a `Canvas` overlay. Fullscreen uses
`ImmersivePlaybackEffect` (`player/PlayerScreen.kt` 563–605) and the §3.5
overlay as a Compose overlay with a 4000 ms hide keyed on `lastInteraction`
(1119–1130 pattern).

**Picture-in-picture**: copy the VOD wiring — `pictureInPictureParams()`
(401–415: aspect ratio, source rect hint, API 31+ auto-enter and seamless
resize), `enterPictureInPicture()` (427–433), `canUsePip` from
`FEATURE_PICTURE_IN_PICTURE` (662–663), the
`addOnPictureInPictureModeChangedListener` chrome hide (1043) and the
API 26–30 `addOnUserLeaveHintListener` (1050–1067). The manifest already
declares `supportsPictureInPicture` and `uiMode` in `configChanges` (49–50).
The `ON_STOP → controller.stop()` rule in `LiveTvScreen.kt` 63 becomes
*stop unless the activity is in PiP* (`activity.isInPictureInPictureMode`),
and `DisposableEffect { onDispose { stop } }` (62) becomes *dock*: the
singleton keeps the player alive across navigation and `HomeScreen` shows
a dock composable (`pip-dock.png`, 200 dp wide on phones) bound to the same
`ExoPlayer` while a lease is current.

**Television** (`FormFactor.Television`): the §3.6 tvOS behaviour with the
TV focus components (`ui/components/TvFocus.kt`: `RequestInitialFocus`,
`tvFocusRing`, `TvButton`, `TightTvButtonFocusBounds`) remain the platform
foundation. Issue #227 supersedes the former list-only/fullscreen-only
exclusion: television viewers choose Guide + preview, Guide over picture, or
Channel browser, while fullscreen remains an explicit presentation reached
from every layout. The shared input fixture now owns browser, temporary-guide,
menu, programme-detail, and stream-info states.

**View persistence**: DataStore preference `live_tv_view`.

### 3.8 Developer card (`liveTvSettingsCard`, index.html ~13771; `settings-sections.test.js` 238–272)

A **Guide** section under the existing HDHomeRun fields, editable while Live
TV is enabled:

```
Guide                                              [fresh · 7 min old]
  Source      ( ) Off  (•) HDHomeRun guide  ( ) XMLTV
  XMLTV URL   [ https://…/guide.xml.gz            ]   (only when XMLTV)
  Look ahead  [24] hours
  Last refresh: 8:05 PM · hdhomerun · 12 channels · 318 programmes
  matched 12 of 12 lineup channels                        [Refresh now]
  <hint> The HDHomeRun guide sends the device's DeviceAuth to
  api.hdhomerun.com over TLS from the owner node only; plurx never stores
  it. XMLTV fetches the URL you give from the owner node.
```

`Refresh now` is `POST /api/v1/live-tv/guide/refresh` (admin; forces a
refresh, returns the new `freshness`/counts; rate-limited by the refresh
semaphore). The card's save goes through `liveTvSettingsWrite` (13791–13806)
with the three new fields and the generation.

### 3.9 The Live TV input table — every client, one ruling

Live TV is a surface without a timeline. The table below is the finite
player's ten-foot/desktop/touch tables from
[PLAYER-INPUT-CONTRACT.md](../clients/PLAYER-INPUT-CONTRACT.md) §2 with
`skip`/`scrub`/`timeline` removed and `channel` added. **Preferred
implementation:** add a `live` surface to
`tests/playback/player-input-contract.json` so `PlaybackPolicy.routeInput`,
`PlayerInputRouting.swift` and `PlayerInputPolicy.kt` all route it from the
one fixture and `scripts/player-contract-table --write --embed` regenerates
the doc. If the fixture's shape resists a surface with no timeline, stop and
flag it; the fallback is a named region in `index.html`
(`// live-tv-input-adapter:begin … :end`, added to `INDEX_REGIONS` in
`scripts/player-input-fence` line 60 with the reason "Live TV owns channel
keys while its host is fullscreen") and hand-written routing in the two
native clients — with this table copied into the contract doc as prose.

| State → input | `left` | `right` | `up` | `down` | `select` | `back` | `idle` (4000 ms) | `play_pause` |
|---|---|---|---|---|---|---|---|---|
| **ten-foot · hidden** | reveal | reveal | reveal | reveal | reveal | exit | ignore | toggle_play |
| **ten-foot · overlay** | focus_row | focus_row | focus_row | focus_row | activate (tune if a channel is focused) | hide | hide | toggle_play |
| **desktop · hidden** (fullscreen host) | strip_prev (reveals + previews) | strip_next (reveals + previews) | channel_up (tunes) | channel_down (tunes) | ignore | exit_fullscreen | ignore | toggle_play |
| **desktop · overlay** | strip_prev | strip_next | channel_up | channel_down | tune previewed strip channel | exit_fullscreen | hide | toggle_play |
| **desktop · page** (not fullscreen) | — | — | — | — | — | — | — | Space toggles when the host has focus |
| **touch** | ignore | ignore | ignore | ignore | tap: toggle_chrome | system back: exit | hide | — |

Hotkeys on desktop, only while the host is fullscreen or focused: `F`
fullscreen · `M` mute · `P` picture-in-picture · `G` guide sheet · `Esc`
exit. Up/Down tune *directly* on desktop because a keyboard user has no
focus ring to preview with and the mouse strip already previews; on the
ten-foot surface they only move focus, per the 2026-09-02 ruling. Nothing in
this table seeks, pauses on a direction, or changes channel on a hidden
ten-foot overlay.

Timings are the contract's: `hide_after_ms` 4000, `hidden_only_while_playing`
true, direction coalescing 350 ms (holding channel-down must not fire ten
tuner starts — coalesce, and start only on release or after 350 ms of
stillness).

## 4. The layouts — the renderings, as text

The PNGs are the target; these are here so the plan reads without them.
Frame values are the web at 1440 px wide; tokens are the `classic` dark
theme (`--panel #171922 · --line #2a2f3e · --accent #6ea8fe · radius 12px`)
— use the CSS variables, never the literals, so every theme in `THEMES`
(index.html 2986) gets the page for free.

### 4.1 List view (`list-view.png`)

```
┌ noirr   Home  [Live TV]  Activity  Settings ─────────── media1 · 2 tuners ○ ┐
│ Live TV [≡ List|▦ Grid] (12 channels)(1 of 2 tuners)   [All|Favorites|Hide protected] [🔍 Number or name] │
│ ┌ Channel ──────── On now · 8:12 PM ┐ ┌──────────────────────────────────┐ │
│ │ CBS  2.1 WCBS              ★     │ │                                  │ │
│ │      Evening Edition             │ │                                  │ │
│ │      ▬▬▬▬▬▬▬▬░░░░░░░░  8:30 PM   │ │            (video 16:9)          │ │
│ │      Next · Harbor Lights        │ │                                  │ │
│ │ NBC  4.1 WNBC                    │ │                                  │ │
│ │      The Long Game               │ └──────────────────────────────────┘ │
│ │      ▬▬▬▬░░░░░░░░░░░░  9:00 PM   │ ABC City Beat 8:00–8:30 PM·18 min left ●LIVE │
│ │      Next · Late Local News      │     7.1 WABC · Next: Kitchen Table    ▬▬▬░░ [⏸][🔇][⧉][⛶][■] │
│ ▌ABC  7.1 WABC   (selected) ★     │ ┌ Tonight on WABC · from the guide ─┐ │
│ │      City Beat                   │ │[7:30 Local News][8:00 City Beat NOW][8:30 Kitchen Table][9:00 The Precinct]… │
│ │      ▬▬▬▬▬▬▬░░░░░░░░░  8:30 PM   │ └───────────────────────────────────┘ │
│ │ …                                │                                      │
│ │ UMÁS 68.1 WFUT  🔒 (dimmed)      │                                      │
│ │      Protected channel · not playable                                   │
│ └──────────────────────────────────┘                                      │
```

Left column 400 px, fixed; rows 78 px; the row is one click target (tune);
the selected channel has the accent left rule and `--panel2`-ish tint.
Right column: video fills the width at 16:9, then the now bar (chip · title
· times · LIVE · number/callsign/next · progress · five icon buttons), then
the selected channel's evening as a horizontal strip of cells proportional
to duration (30 min ≈ 165 px) with the airing cell highlighted. Under
1100 px the columns stack (list under player); under 700 px it is the
phone layout.

### 4.2 Grid view (`grid-view.png`)

```
│ Live TV [≡ List|▦ Grid] (12 channels)(1 of 2 tuners) [All|Favorites|Hide protected][🔍]  ┌──────────┐ │
│ ┌ ABC City Beat ●LIVE 8:00–8:30 PM · 18 min left      [⤢][⏸][🔇][⧉][⛶][■] ┐   │  docked  │ │
│ │     7.1 WABC · Next: Kitchen Table 8:30 PM    ▬▬▬▬▬▬▬▬░░░░░░░░░░░░░░░   │   │ 400×225  │ │
│ └───────────────────────────────────────────────────────────────────────┘   └──────────┘ │
│ ┌────────────┬ 8:00 PM ──┬ 8:30 ──────┬ 9:00 ──────┬ 9:30 ──────┬ 10:00 ─────[Now]┐ │
│ │ CBS 2.1 WCBS ★│ Evening Ed.│ Harbor Lights          │ The Precinct            │ │
│ │ NBC 4.1 WNBC  │ The Long Ga│me         ┃ Studio Sessions          │ Late Local│ │
│ │ ABC 7.1 WABC ★│▐City Beat▌ │ Kitchen T.┃ The Precinct            │ Harbor L. │ │
│ │ …             │            │           ┃(red now line at 8:12)               │ │
│ └───────────────┴────────────┴───────────┴───────────┴───────────┴───────────┘ │
```

Channel column 180 px; 30 min = 240 px; rows 52 px; the grid scrolls
horizontally (header times sticky) and vertically (channel column sticky);
`Now` scrolls the now line into the first third. Cells: `--panel` for
future, `--panel2` for airing, accent border for the airing cell of the
playing channel, ellipsised titles, `title=` tooltip with times. Where the
cache ends, a final hatched cell reads *Guide data ends 12:00 AM*. Click
any cell → a popover: title, times, episode, synopsis, `filters` as pills,
and the cell's verbs. It is a modal — `role="dialog"`, it takes the
keyboard, Escape closes it and hands focus back to the cell. Watch is first
and takes focus, so an airing cell is one more Enter from the picture, and
Record is reachable on it at all, which it was not while an airing cell
tuned and returned. Keyboard: roving `tabindex` over cells, arrows move,
Enter opens the popover, `Home` jumps to now. The **docked player** is 400 × 225 top-right with the now
bar to its left; the `⤢` expands to the list-view geometry (player wide,
grid beneath — this is the same page, so it is a class toggle) and `⛶` is
fullscreen.

### 4.3 Fullscreen (`fullscreen.png`), dock (`pip-dock.png`), phone (`phone.png`), TV (`tv-overlay.png`)

All described in §3.5–§3.7. Dimensions: fullscreen chip 52 × 34, strip
cards 170 px, hints 12 px; dock 384 × 216 video + 48 px caption; phone
video 390 × 219, rows ≥ 64 dp, segmented control 44 dp tall; TV overlay
list rows 92 px with a 4 px white focus ring at 2 px offset (the ten-foot
contract's), left column 760 px, controls bottom-right at 18 pt.

## 5. Non-goals — guardrails for this effort

Do not do these; each has a reason, and most were argued once already in
[HDHOMERUN-LIVE-TV-PLAN.md §7 and §9](HDHOMERUN-LIVE-TV-PLAN.md#7-non-goals--guardrails-for-the-first-complete-release).

1. **No DVR, reminders, or "tune at 9".** The guide is read-only. A future
   cell has a popover and nothing else. Recording is a separate, large
   effort with storage and legal questions the tuner contract avoids.
2. **No `DeviceAuth` at rest, in relay bodies, logs, metrics or errors.**
   §2.2 is the whole exception. A test asserts the string never appears in
   a snapshot, an activity row, or a sanitized error.
3. **No new outbound host beyond the guide allowlist and the admin's
   XMLTV URL**, and no redirect following. Programme `image_url`s are
   *passed through* to clients, not fetched by the server: proxying
   third-party images is the artwork allowlist's job and a later
   milestone if wanted. Clients may render the chip (network/callsign)
   instead of the image — the mockups do.
4. **No second tuner GET for anything.** Preview on hover, a "mini preview"
   in the grid, channel-up while a start is in flight — all refused. The
   350 ms coalescing rule exists so a held key is one start.
5. **No durable guide cache.** Memory on the owner; a restart refetches.
   The `settings` table is replicated on every write and is not a cache.
6. **No manual XMLTV channel mapping UI.** Match by display-name / lcn /
   name; report `matched N of M`. A mapping editor is its own task if the
   automatic match proves insufficient on Paul's grabber.
7. **No grid on tvOS / Android TV in this effort.** A focus-navigable
   half-hour grid is a milestone of its own; the TV `Guide` is the
   per-channel schedule list.
8. **No auto-tune on open.** Opening the page or the app never takes a
   tuner; `plurx_live_tv_last` only pre-selects.
9. **No seek, pause-with-rewind, or timeline.** Pause stays the existing
   pause (the tuner is released after 30 s without playback); the UI says
   so in the overlay when paused.
10. **No change to the lease, barrier, keepalive or status semantics**
    beyond the two visibility/PiP narrowings in §3.4/§3.6/§3.7. If dock or
    PiP seems to need a longer server idle window, flag it — do not change
    `live_tv.rs` timeouts.
11. **No new JS file unless `live-tv.js` genuinely cannot hold it.** A new
    asset needs `web.rs`, `mod.rs` routes and allowlist, the `<script>` tag,
    `make web-check` and the input fence — five places to forget.

## 6. Testing — what proves each piece

| Layer | Test | Runs in |
|---|---|---|
| Guide engine | `live_tv.rs` unit tests with `serve_once` (3555) and the proxy-injected loopback fixture (3836–3845): bulk + per-channel HDHomeRun pages, XMLTV plain and gzipped, bounds (2 MiB, 200/channel, title 256), stale/unavailable transitions, generation invalidation, `DeviceAuth` never in snapshot/activity/error, unmatched channels dropped, `matched N of M` | `make unit` |
| Public route + relay | `http/mod.rs` route-matrix tests (learner/maintenance refusals for `/live-tv/guide`), ingress relay against an owner fixture, 404-from-old-owner → `unavailable` | `make unit` |
| Settings | `live_tv_settings_are_runtime_only_generation_cas_and_enable_fenced` (mod.rs 3444) extended: guide fields writable while enabled, invalid source/URL refused, generation still mandatory | `make unit` |
| Two-node | `crates/plurxd/tests/live_tv_two_node.rs`: one new case — the ingress serves the owner's guide and a guide outage on the owner does not affect a relayed session start | `make live-tv-two-node-check` |
| Hardware | `scripts/live-tv-hardware` gains a guide step: real `DeviceAuth`, real host, observed shape recorded into the STATUS doc | `make live-tv-hardware-check DEVICE=…` |
| Web pure logic | `tests/web/live-tv.test.js`: `programmeAt`, `gridLayout` (no overlaps, clipping, now-line x), `filterChannels`, `adjacentChannel` wrap, view persistence, `liveTvLeaveRoute` docks vs hides, visibility narrowing keeps the lease in PiP, keepalive guard in PiP | `make web-check` |
| Web page | `page-read-budget.test.js`: `viewLiveTv` emits shell/content/settled and issues exactly two authoritative reads (channels, guide); `settings-sections.test.js`: the Guide section renders and saves the three fields | `make web-check` |
| Web structure | `scripts/ui-baseline` mocks `/live-tv/guide` beside its `/live-tv/channels` mock (1482–1495) for both views; regenerate the golden with `make ui-golden` and commit it; a11y invariants (named controls, no duplicate ids) must hold for the grid's cells | `make ui-check` |
| Input | contract fixture tests if the `live` surface lands there (`tests/playback/player-input-contract.test.js`); otherwise the fence (`scripts/player-input-fence`) must pass with the new region | `make validate-staged` |
| Apple | `LiveTvTests.swift`: guide decoding (snake_case), `programmeAt` parity cases (shared JSON fixture `tests/playback/live-tv-guide-cases.json` used by all three clients), heartbeat continues in PiP, `scenePhase` narrowing, view persistence | `make apple-test` |
| Android | `LiveTvTest.kt`: the same fixture cases; `LiveTvUiTest.kt`: list shows now-title and progress from the mocked guide, grid tunes on an airing cell and refuses a future one, TV overlay reveal-then-tune | `make android-test` · `make android-instrumentation` |

`tests/playback/live-tv-guide-cases.json` is new: a guide response, a
lineup, a set of `now` values and the expected now/next/progress per
channel, plus grid rows for one window. Three clients, one truth — the
way `player-input-contract.json` already works.

Run the Python gates in a local worktree, not on the bridge mount
(`git worktree add --detach /tmp/plurx-gate HEAD`): the mount is
case-insensitive and a directory walk on it takes 75 s, so
`test_ci_require_disk` and the docs-index path checks give the wrong
answer there. `make validation-lint operations-check` there is ~7 s.

## 7. Milestones — task PRs into `effort/live-tv-guide`

Each PR is a `WIP:` draft until its acceptance passes, then un-drafted and
merged into the effort by its author. Build in order; M3 can start once
M1's endpoint shape is merged even if XMLTV (M2) is still open.

### 7.1 M0 — contract, fixture, index

- Add this document's index row (`docs/README.md`, features table:
  `| [LIVE-TV-GUIDE-AND-UI-PLAN.md](…) | The Live TV page, the guide feed, fullscreen and PiP on every client. | open |`)
  and `docs/mockups/live-tv/` with the seven PNGs.
- Add `tests/playback/live-tv-guide-cases.json` and the `live` surface
  to the input contract fixture (or the fence region — §3.9).
- Amend [HDHOMERUN-LIVE-TV-PLAN.md](HDHOMERUN-LIVE-TV-PLAN.md) §3.2 and
  §7 with the one-clause `DeviceAuth` change and the lifted "no EPG"
  non-goal, dated, pointing here.

**Acceptance:** `make validation-lint operations-check` green in a local
worktree; `tests/operations/test_docs_index.py` passes; the contract table
regenerates without diff if the fixture route was taken.

### 7.2 M1 — HDHomeRun guide on the server

Settings keys and DTO (§3.1), the guide module with the HDHomeRun source,
cache and refresh loop (§3.3), `GET /live-tv/guide`, `POST
/live-tv/guide/refresh`, the ingress relay, activity `programme_title`,
metrics, the Developer card section (§3.8), [API.md](../API.md) rows,
[SECURITY.md](../SECURITY.md) outbound-host row, [OPERATIONS.md](../OPERATIONS.md)
runbook lines ("guide is off / stale / matched N of M").

**Acceptance:** `make unit` green with the §6 unit rows; on media1 with the
FLEX 4K, `curl -H "authorization: Bearer $T" http://media1:32400/api/v1/live-tv/guide | jq '.freshness, (.channels|length)'`
prints `"fresh"` and the lineup's channel count, the observed response
shape is recorded in [HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md),
and `grep -c DeviceAuth` over the daemon's log for the run is `0`.

### 7.3 M2 — XMLTV source

The XMLTV fetcher and parser, matching, `matched N of M`, the Developer
card's URL field, gzip.

**Acceptance:** unit tests over a 300-channel gzipped XMLTV fixture stay
under the bounds; pointing `live_tv.xmltv_url` at a file served from lab3
fills the grid for the matched channels and the card shows the count.

### 7.4 M3 — web

`live-tv.js` pure functions; the host/dock/slot machinery; list view; grid
view; the switch and preferences; fullscreen overlay; native PiP; the input
adapter; `pagePhaseName`; visibility narrowing; Activity shows the
programme title; ui-baseline mocks and golden; `make web-check`.

**Acceptance:** `make web-check` and `make ui-check` green; in Chrome and
Safari on the MacBook: tune from the list, switch to the grid without a
restart (one `POST …/sessions` in the network log for the whole exercise),
navigate to Home and the dock keeps playing with keepalives every 10 s,
enter native PiP and background the tab for 2 min without a stop, return
and the picture is inline, `F`/`M`/`P`/`G`/`Esc` and Up/Down behave per
§3.9. Record the exercise in the STATUS doc.

### 7.5 M4 — Apple

Models and API, controller guide refresh, iOS layout with list and grid,
fullscreen overlay, PiP with the narrowed `scenePhase` rule, tvOS overlay
and per-channel schedule, view persistence, tests; `make apple-build-bump`
(118 → 119 with the `docs/apple-builds/` fragment);
[APPLE-CLIENT-PARITY.md](../clients/APPLE-CLIENT-PARITY.md) Live TV section.

**Acceptance:** `make apple-test` green on both simulators; the physical
checks in §8.

### 7.6 M5 — Android

Models and API, state, phone list/grid, immersive fullscreen with overlay,
PiP, dock on Home, TV overlay, DataStore persistence, unit and
instrumentation tests; `versionCode` 73 → 74 and the README status line;
[ANDROID-CLIENT-PARITY.md](../clients/ANDROID-CLIENT-PARITY.md) — delete
the "guide scheduling is deliberately unsupported" clause and say what is.

**Acceptance:** `make android-test` and `make android-instrumentation`
green on the API 36 emulator; the physical checks in §8.

### 7.7 M6 — close out

[FEATURES.md](../FEATURES.md) §4a, [CHEATSHEET.md](../CHEATSHEET.md) §5,
[CLIENTS.md](../CLIENTS.md) matrix, `docs/STATUS.html` (what is deployed
where, the new build numbers), `CHANGELOG.md`, the two-node case, the
hardware step, an adversarial review of the effort recorded in
`docs/reviews/`, then the effort PR into `main`, then the Ansible server
deploy to every node and the client deploy prompt.

**Acceptance:** the effort's qualification run is green; `docs/STATUS.html`
names the deployed server commit and client builds; the STATUS doc's
"Known limits" is updated with whatever §8 found.

## 8. Physical verification — the prompt for the session at Paul's Mac

Cloud sessions cannot install on devices. When M4/M5 are merged to the
effort, hand Paul this, verbatim, for the GPT session that has the Mac,
Xcode, the Apple TV and the Android devices
([CLIENT-DEPLOY-PROMPT.md](../clients/CLIENT-DEPLOY-PROMPT.md) is the
deploy half; this is the check half):

> Deploy the `effort/live-tv-guide` client builds (Apple 119, Android 74)
> per docs/clients/CLIENT-DEPLOY-PROMPT.md. Then, with Live TV enabled and
> the guide source set to HDHomeRun on media1, verify and report each line
> as pass/fail with one sentence of what you saw:
> 1. iPhone: the list shows a programme title and progress for every
>    unprotected channel; tapping a row tunes; rotating to landscape goes
>    fullscreen with the overlay, which hides after 4 s and returns on tap.
> 2. iPhone: Grid shows the half-hour grid with a red now line; tapping any
>    cell — airing or future — opens the programme sheet, whose first action
>    is Watch and which also offers Record and Record series.
> 3. iPhone: the PiP button opens the system PiP window; press Home; after
>    2 minutes the stream is still playing and Activity on the web shows
>    the session with the programme title.
> 4. Apple TV: with a channel playing and the overlay hidden, press each
>    direction once — the overlay appears and the channel does NOT change;
>    Down twice then Select tunes the focused channel; Back hides the
>    overlay; 4 s of no input hides it.
> 5. Android phone: repeat 1–3 (PiP via the button, then Home).
> 6. Android TV: repeat 4.
> 7. Both phones: leave Live TV for Home while playing — the dock keeps
>    playing; its stop button stops it and Activity clears within 15 s.
> Report the exact app build numbers shown in Settings and the server
> commit from `/api/v1/system`.

## 9. Decisions and their reasons

1. **Both views, one page, one switch.** Paul chose B and C together on
   2026-09-07: the list is for surfing, the grid for planning, and they
   share every piece of state. The switch is a class toggle over one
   render tree so it costs nothing and never touches the tuner.
2. **HDHomeRun guide by default, XMLTV as the override.** Zero-setup for
   the common case; an escape hatch for a grabber Paul may already run.
   Precedence is explicit (`guide_source`), never inferred.
3. **`DeviceAuth` read at refresh, never stored.** The plan's original
   rule was written before there was a reason to send it anywhere; the
   narrowest change that makes the guide work is to use it in flight and
   forget it. Everything that made the old rule safe (no snapshot, no
   log, no relay) is kept.
4. **Owner fetches, ingress relays, no capability bump.** The credential
   is only on the owner, so the fetch is too; relaying the result reuses a
   proven pattern; not gating on the protocol capability keeps a mixed
   fleet playing during rollout — the guide degrades to "unavailable",
   Live TV does not.
5. **The video element lives outside `#main`.** It is the only way a dock
   survives `layoutChrome`, and it is what `#modal` already does for VOD.
   The slot/observer trick costs one observer and removes the
   route-change stop, which was a workaround for the element dying.
6. **Fullscreen targets the host, not the video.** A bare fullscreen
   video cannot show a channel strip; the VOD player already fullscreens
   `#player` for the same reason.
7. **Desktop Up/Down tune directly; ten-foot Up/Down only focus.** A
   keyboard user has no preview affordance except the strip, which the
   mouse already drives; a remote user has a focus ring and the
   2026-09-02 ruling. Two tables, one principle: a hidden overlay is never
   changed by a direction.
8. **Guide data is passed through unproxied, images included.** Proxying
   would add an artwork-allowlist host and a cache for a feature nobody
   has asked for; the chip renders fine without logos.
9. **No grid on TV yet.** A focus-navigable grid is a milestone's worth of
   focus engineering on each platform; shipping the phone/desktop grid
   first gets the data feed and the overlay proved on hardware.
