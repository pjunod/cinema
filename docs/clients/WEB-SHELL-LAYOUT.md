# Web shell layout — where the app's sixty-four files are, and what each one holds

**Status:** live · **Describes:** `crates/plurxd/src/web/` as served ·
**Split:** 2026-09-19, executing
[WEB-SHELL-SPLIT-PLAN.md](WEB-SHELL-SPLIT-PLAN.md)

Companion to [UI-LAYOUTS-IMPLEMENTATION.md](UI-LAYOUTS-IMPLEMENTATION.md)
(what the layouts are) and
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (the fences that scan
the player) — this is *which file to open*, and the rules a new one has to
obey.

Until 2026-09-19 the web app was one file. `crates/plurxd/src/web/index.html`
was 23,901 lines: a 3,209-line `<style>` block, about 36 lines of markup, a
218-line theme engine in `<head>`, and a 20,433-line application in `<body>`
holding 1,152 top-level functions. It is now a 97-line shell of markup and
tags, and everything else lives in the files below. There is still no
bundler, no build step and no ES modules: these are plain `<script src>`
scripts sharing one global scope, exactly as the inline script they were cut
out of did.

## 1. The rules

### 1.1 Served order is load order

`WEB_ASSETS` in [`crates/plurxd/src/http/web.rs`](../../crates/plurxd/src/http/web.rs)
is the table, and the order of the rows is the order the browser runs them.
One shared scope and no modules means hoisting is per file: **a row may only
name a binding an earlier row declared**, at the moment it loads. Almost
nothing in the shell cares — 1,152 of the declarations are functions nobody
calls until a route renders — but the thirty-nine statements that do run at
load care absolutely, and getting it wrong is a blank page with a
`ReferenceError`, not a degraded feature.

Two gates hold it. `tests/web/asset-order.test.js` reads every row with a
real parser and refuses an order where a load-time statement or a `const`
initializer names a later row's binding. `tests/web/asset-load.test.js`
evaluates the sidecars, the head script and all sixty body rows in a `vm`
stub and checks the app is there afterwards.

### 1.2 `<head>` runs before first paint, `<body>` does not

`core/theme.js` is a synchronous `<script src>` in `<head>`, after the
stylesheet links. No `defer`, no `async`, no `type="module"`. It applies the
theme before anything is drawn — that is the whole reason it is in `<head>`
rather than at the top of the body — and ten of its names (`LAYOUTS`,
`APP_NAME`, `POSTER_SIZES`, …) are read from the body rows. `app.css` is
linked after `reader.css`: the two have no selector overlap today, and the
order is what keeps that true.

Everything else is a `<script src>` at the end of `<body>`, after the seven
sidecars.

### 1.3 The one block that moved

Eight lines register the classic layout:

```js
LAYOUTS.classic.chrome=classicChrome;
LAYOUTS.classic.views={ home:classicHomeBody, item:classicItemBody, … };
```

They execute at load and name `classicHomeBody`, `classicItemBody`,
`classicLibraryShell`, `classicLibraryItems` and `classicLibraryCount` — all
declared later in the old file. In one script, hoisting covered that. Cut at
the section banners, the app dies at load. So they are
`layouts/register-classic.js`, served after `layouts/catalog.js` and before
`layouts/theater.js`, which still puts the registration before the
`applyLayout()` that paints the first frame. **This is the only relocation
the split made.**

The rest is byte for byte the file it came from, and that was proved rather
than asserted: the split commit carried a `scripts/web-shell-identity` that
reassembled the tree from the shell's own tag order — stripping one
`"use strict";` prologue per row — and compared it to the pre-split
`index.html`.

```
web-shell-identity: OK against d651fc61
  app.css              3209 lines == old <style> content
  core/theme.js         218 lines == old <head> script
  60 body rows   20425 lines == old <body> script (one prologue stripped per
  row; 8 relocated lines re-inserted where layouts/register-classic.js is served)
```

It was removed in the same PR. It could only ever answer that one question
about that one commit, and a script in `scripts/` that fails the first time
anybody edits a web file is not a gate, it is a trap. The standing gates are
the three in §1.1 and §3.

### 1.4 Every file carries its own `"use strict";`

A directive applies per script, so without one, fifty-nine of the sixty
files would run sloppy. Nothing throws today that would go silent — there is
no assignment to an undeclared identifier anywhere in the shell — but the app
freezes `STATS_ROWS` and other tables, and a write to a frozen object is a
`TypeError` in strict mode and a silent no-op in sloppy mode. The prologue is
the file's first statement, and `asset-order` refuses a row without one.

### 1.5 Two things that really are different

The bytes are identical and the app behaves identically, but sixty-eight
scripts are not one script, and two consequences are worth knowing before you
debug something strange.

**A load-time throw is no longer fatal.** In one `<script>`, anything that
threw at load took the rest of the file with it, including the `boot()` on
the last line — you got a blank page and a stack trace. Now the browser logs
the error and moves to the next `<script>`, so the app boots with one row's
`let`/`const` permanently in the temporal dead zone: a half-working UI where
there used to be a loud failure. `tests/web/asset-load.test.js` rethrows, so
it sees the throw; a browser will not.

**`typeof` on a later row's `let`/`const` changed meaning.** On a binding in
the temporal dead zone, `typeof X` *throws*; on an undeclared binding it
returns `"undefined"`. Three guards in the shell do exactly this across a file
boundary — `detail/dynamic-range.js`'s `typeof PLAYER`,
`player/decode-tiers.js`'s `typeof LIVE_TV_LEASE`, and
`pages/settings-live-tv.js`'s `typeof PAGE_RENDER_GENERATION`. None is
reachable at load, so nothing differs in practice, but they are load-order
sensitive now: if `pages/live-tv.js` ever failed to load, `play()` would
silently stop stopping Live TV instead of failing loudly.

## 2. The table

Served order, top to bottom. The last column is where that code was in
`index.html` before the split, at `main` `a2d9c2fb` — the 114 `index.html:NNNN`
citations across 39 documents resolve through it, which is why none of them
had to be edited.

| # | File | What it holds | Old lines |
|---|---|---|---|
| 1 | [`app.css`](../../crates/plurxd/src/web/app.css) | The whole stylesheet, fonts and all — the `<style>` block verbatim. Cascade order is load-bearing, so it is not split. | 17–3225 |
| 2 | [`core/theme.js`](../../crates/plurxd/src/web/core/theme.js) | `THEMES`, `FAVICONS`, `applyTheme`, `POSTER_SIZES`, the `LAYOUTS` registry, `surfaceClass`, `layoutId`, `applyLayout`, `APP_NAME`. | 3228–3445 |
| 3 | [`core/app.js`](../../crates/plurxd/src/web/core/app.js) | `API`, `TOKEN`, `ME`, the `PlaybackPolicy`/`ReaderCore`/`LibraryChannelCore` aliases, and the native-reader handoff. | 3466–3507 |
| 4 | [`core/api.js`](../../crates/plurxd/src/web/core/api.js) | `api()`, request ids, the 401 path, `toast`. | 3508–3636 |
| 5 | [`player/measurements.js`](../../crates/plurxd/src/web/player/measurements.js) | Stall, hitch and TTFF measurement, and the formatters (`esc`, `fmtDur`, `fmtSize`). | 3637–4329 |
| 6 | [`core/auth.js`](../../crates/plurxd/src/web/core/auth.js) | Sign-in, sign-out, `boot`, session recovery. | 4330–4466 |
| 7 | [`core/keyboard-reach.js`](../../crates/plurxd/src/web/core/keyboard-reach.js) | Enter/Space activation for click-only cards — the `nav-keyboard-adapter` fence region. | 4467–4525 |
| 8 | [`core/chrome.js`](../../crates/plurxd/src/web/core/chrome.js) | The app frame: header, nav, the shared shell chrome. | 4526–4560 |
| 9 | [`core/theme-menu.js`](../../crates/plurxd/src/web/core/theme-menu.js) | The per-user theme and appearance menu, the account menu, and the connection-QR dialog. | 4561–4706 |
| 10 | [`core/activity-indicator.js`](../../crates/plurxd/src/web/core/activity-indicator.js) | The global in-flight indicator. | 4707–4742 |
| 11 | [`core/cards.js`](../../crates/plurxd/src/web/core/cards.js) | `card`, `grid`, `artHtml`, `fmtDate` — the shared item cards every page paints. | 4743–4845 |
| 12 | [`core/lightbox.js`](../../crates/plurxd/src/web/core/lightbox.js) | The photo lightbox and its own Escape / previous / next keys. | 4846–4915 |
| 13 | [`pages/home-helpers.js`](../../crates/plurxd/src/web/pages/home-helpers.js) | The get-the-app install prompt, `comingCard`, and Home's grouping helpers. | 4916–5026 |
| 14 | [`pages/home.js`](../../crates/plurxd/src/web/pages/home.js) | `buildHomeSections` and `viewHome` — the page-model seam. | 5027–5134 |
| 15 | [`layouts/renderers.js`](../../crates/plurxd/src/web/layouts/renderers.js) | `classicChrome`, the layout resolution helpers, and the classic renderers. | 5135–5372 (less 5140–5147) |
| 16 | [`layouts/header.js`](../../crates/plurxd/src/web/layouts/header.js) | The page header: back control and breadcrumb trail. | 5373–5392 |
| 17 | [`layouts/library-grids.js`](../../crates/plurxd/src/web/layouts/library-grids.js) | `classicLibraryShell`, `classicLibraryItems`, `classicLibraryCount` — the one incremental route. | 5393–5657 |
| 18 | [`detail/helpers.js`](../../crates/plurxd/src/web/detail/helpers.js) | Detail-screen helpers, and the four inlined Material icon paths. | 5658–5697 |
| 19 | [`detail/dynamic-range.js`](../../crates/plurxd/src/web/detail/dynamic-range.js) | Source vs delivered vs rendered HDR/DV, and the badges that say which. | 5698–5859 |
| 20 | [`detail/track-facts.js`](../../crates/plurxd/src/web/detail/track-facts.js) | `codecLabel`, `premiumAudio`, `fmtChannels` — the shared track vocabulary. | 5860–6005 |
| 21 | [`detail/preplay-selection.js`](../../crates/plurxd/src/web/detail/preplay-selection.js) | Pre-play audio and subtitle selection, and `classicItemBody`. | 6006–6744 |
| 22 | [`detail/edit.js`](../../crates/plurxd/src/web/detail/edit.js) | Editing metadata and home libraries (admin), including the tag-chip field. | 6745–6854 |
| 23 | [`player/player.js`](../../crates/plurxd/src/web/player/player.js) | `PLAYER`, opening and closing a stream, the play/pause transport core. | 6855–7714 |
| 24 | [`player/session.js`](../../crates/plurxd/src/web/player/session.js) | Session lifecycle: start, keepalive, teardown. | 7715–7949 |
| 25 | [`player/prepared-replacement.js`](../../crates/plurxd/src/web/player/prepared-replacement.js) | The prepared successor: staging, commit, rollback. | 7950–8531 |
| 26 | [`player/prepared-switch-measurement.js`](../../crates/plurxd/src/web/player/prepared-switch-measurement.js) | M3's measurement of the prepared switch — windows, silence gaps, samplers. | 8532–9156 |
| 27 | [`player/directed-change.js`](../../crates/plurxd/src/web/player/directed-change.js) | A directed selection change: quality, audio or subtitle chosen by the viewer. | 9157–9855 |
| 28 | [`player/decode-tiers.js`](../../crates/plurxd/src/web/player/decode-tiers.js) | Runtime playback capabilities and the HEVC decode tiers; builds `PLAY_CAPS` at load. | 9856–11667 |
| 29 | [`player/decode-margin.js`](../../crates/plurxd/src/web/player/decode-margin.js) | Decode margin: whether this machine can keep up with what it asked for. | 11668–12190 |
| 30 | [`player/stall-diagnosis.js`](../../crates/plurxd/src/web/player/stall-diagnosis.js) | Stall self-diagnosis — why it stopped, and what to offer. | 12191–12758 |
| 31 | [`player/surface.js`](../../crates/plurxd/src/web/player/surface.js) | The playback surface: one presenter, one render. The `playback-surface-render` fence region. | 12759–13072 |
| 32 | [`player/projection-chrome.js`](../../crates/plurxd/src/web/player/projection-chrome.js) | Projection chrome, skip intro/credits, and the title info panel. | 13073–13178 |
| 33 | [`player/transport.js`](../../crates/plurxd/src/web/player/transport.js) | The custom transport: scrubber, times, seeking, and the `player-input-adapter` region's key handling. | 13179–13954 |
| 34 | [`player/autoplay-next.js`](../../crates/plurxd/src/web/player/autoplay-next.js) | Autoplay next episode (per browser, default on). | 13955–14185 |
| 35 | [`player/menus.js`](../../crates/plurxd/src/web/player/menus.js) | Audio-language, subtitle and quality-override menus. | 14186–14439 |
| 36 | [`player/audio-sync.js`](../../crates/plurxd/src/web/player/audio-sync.js) | The A/V offset control, and the subtitle track plumbing it shares. | 14440–14685 |
| 37 | [`player/stats.js`](../../crates/plurxd/src/web/player/stats.js) | The playback stats overlay, `STATS_ROWS`, and the generated `PLAYBACK_INFO_FIELDS` embed. | 14686–15427 |
| 38 | [`player/watch-presentation.js`](../../crates/plurxd/src/web/player/watch-presentation.js) | Watch-and-browse presentation and browser interaction; retains the existing media owner. | **Relocated.** New watch-and-browse module. |
| 39 | [`detail/watch-browser.js`](../../crates/plurxd/src/web/detail/watch-browser.js) | Watch-and-browse presentation and browser interaction; retains the existing media owner. | **Relocated.** New watch-and-browse module. |
| 40 | [`pages/activity.js`](../../crates/plurxd/src/web/pages/activity.js) | The Activity page shell and its poll — twenty lines, and the smallest row there is. | 15428–15447 |
| 41 | [`pages/analysis.js`](../../crates/plurxd/src/web/pages/analysis.js) | Analysis status: the queue, its failures, and what to do about them. | 15448–16149 |
| 42 | [`pages/activity-stream.js`](../../crates/plurxd/src/web/pages/activity-stream.js) | Now playing: the Stream cell — state pill, meter strip, details disclosure. | 16150–16560 |
| 43 | [`pages/settings.js`](../../crates/plurxd/src/web/pages/settings.js) | The Settings frame: `SETTINGS_MANIFEST`, `SETTINGS_ENDPOINTS`, tab routing. | 16561–16838 |
| 44 | [`pages/settings-panels.js`](../../crates/plurxd/src/web/pages/settings-panels.js) | Shared panel machinery, Metadata/search, Maintenance/Windows, Analysis and Playback. | 16839–17349 |
| 45 | [`pages/live-tv.js`](../../crates/plurxd/src/web/pages/live-tv.js) | `LIVE_TV`, `viewLiveTv`, the guide, the grid, the popover, and the `live-tv-input-adapter` region. | 17350–18465 |
| 46 | [`pages/live-tv-dvr.js`](../../crates/plurxd/src/web/pages/live-tv-dvr.js) | Recording from the Live TV page. | 18466–18751 |
| 47 | [`pages/recordings.js`](../../crates/plurxd/src/web/pages/recordings.js) | The Recordings page: what is scheduled, what recorded, and what failed. | 18752–18983 |
| 48 | [`pages/dvr-reminders.js`](../../crates/plurxd/src/web/pages/dvr-reminders.js) | The due-reminder overlay and its polling. | 18984–19064 |
| 49 | [`pages/live-tv-controls.js`](../../crates/plurxd/src/web/pages/live-tv-controls.js) | Tuning, stopping, pause/mute/fullscreen, and the visibilitychange stop. | 19065–19342 |
| 50 | [`pages/settings-developer.js`](../../crates/plurxd/src/web/pages/settings-developer.js) | Experimental Developer cards, `developerPanel`, and shared advisory readiness helpers. | 19343–19799 |
| 51 | [`pages/settings-live-tv.js`](../../crates/plurxd/src/web/pages/settings-live-tv.js) | `LIVE_TV_GUIDE_DRAFT`, guide, tuner, recording and library-channel cards, `liveTvPanel`. | 19800–20020 |
| 52 | [`pages/settings-system.js`](../../crates/plurxd/src/web/pages/settings-system.js) | The users panel, build/storage/replication facts, `systemPanel`. | 20021–20238 |
| 53 | [`pages/cluster.js`](../../crates/plurxd/src/web/pages/cluster.js) | Cluster membership: the node cards and what each one is claiming. | 20239–20557 |
| 54 | [`pages/cluster-operations.js`](../../crates/plurxd/src/web/pages/cluster-operations.js) | The operations rail and its preconditions. | 20558–20762 |
| 55 | [`pages/cluster-database.js`](../../crates/plurxd/src/web/pages/cluster-database.js) | The replicated database ledger, and remembering what is folded. | 20763–20859 |
| 56 | [`pages/cluster-troubleshooting.js`](../../crates/plurxd/src/web/pages/cluster-troubleshooting.js) | Cluster troubleshooting: what is wrong, and the one thing to try. | 20860–21468 |
| 57 | [`pages/settings-playback.js`](../../crates/plurxd/src/web/pages/settings-playback.js) | Playback defaults, recovery and protocol cards, Trakt, and settings save handlers. | 21469–21740 |
| 58 | [`pages/users-admin.js`](../../crates/plurxd/src/web/pages/users-admin.js) | The admin users route. Non-contiguous with `pages/settings-system.js`'s users panel. | 21741–21787 |
| 59 | [`layouts/catalog.js`](../../crates/plurxd/src/web/layouts/catalog.js) | The catalog layout: sidebar, phone library sheet, chrome, Home, G2b item detail, and `LAYOUTS.catalog`. | 21788–22457 |
| 60 | [`layouts/register-classic.js`](../../crates/plurxd/src/web/layouts/register-classic.js) | **Relocated.** The eight lines registering `LAYOUTS.classic.chrome` and `.views`. See §1.3. | 5140–5147 |
| 61 | [`layouts/theater.js`](../../crates/plurxd/src/web/layouts/theater.js) | The theater layout, `LAYOUTS.theater`, and the load-time `applyLayout()` that paints the first frame. | 22458–22996 |
| 62 | [`pages/reader.js`](../../crates/plurxd/src/web/pages/reader.js) | The EPUB reader page — glue over the `reader.js` sidecar, which did not move. | 22997–23326 |
| 63 | [`pages/library-channels-page.js`](../../crates/plurxd/src/web/pages/library-channels-page.js) | The Library channels page — glue over the `library-channels.js` sidecar, which did not move. | 23327–23783 |
| 64 | [`router.js`](../../crates/plurxd/src/web/router.js) | `PAGE_TIMER`, `setPageTimer`, `render`, and the `hashchange` and boot statements that start the app. | 23784–23898 |

## 3. Adding a file

Three places, one commit, or a test says so:

1. The file, under the folder for its area, opening with `"use strict";`.
2. A row in `WEB_ASSETS` (`crates/plurxd/src/http/web.rs`), at the point in
   the order where its dependencies are already declared.
3. A `<script src="/assets/<path>"></script>` row in `index.html`, in the
   same position, in `<body>` after the sidecars.
4. A row in the table above.

`web_assets_match_the_shell` (a `web.rs` unit test) fails if the table and
the shell disagree about what exists or about the order;
`tests/web/asset-layout.test.js` fails if this document and the shell
disagree. A file cannot be added without being served, served out of order,
served in the wrong half, or left undocumented.

Which folder: `core/` for what every page uses, `layouts/` for a layout's
chrome and registration, `detail/` for the item screen, `player/` for the
finite player, `pages/` for a route, and `router.js` for the router. If a new
file does not fit one of those, the folder is probably the thing that is
wrong.

**Do not** add `export`, `import` or `module.exports` to one of these files.
They are plain scripts in one realm; making one a module breaks every other
row that names it. Converting an area to real modules is its own piece of
work, one area at a time, with the behavior tests to go with it.

## 4. What is *not* in the table

Seven sidecars and `reader.css` keep their own routes and their own URLs:
`cluster-panel.js`, `playback-policy.js`, `playback-control.js`,
`live-tv.js`, `library-channels.js`, `reader.js`, `offline-reader.js`, and
`hls.min.js`. They are UMD-shaped modules `require()`d by path from forty-odd
tests, they are named in `validation/points.toml` scopes, and three of them
are bundled into both native clients by relative path
(`clients/apple/project.yml`, `clients/android/app/build.gradle.kts`).
Moving one puts Xcode and Gradle inputs in the diff and fans out Apple and
Android CI. `offline-reader.html` is not served by `plurxd` at all — nothing
routes it; it exists for the native WebViews and `scripts/reader-browser`.

`hls.min.js` is vendored, so `scripts/js-check` skips it: a syntax error in
upstream's minified bundle is upstream's.

## 5. Serving, and the hashes

`/` is the shell with `Cache-Control: no-cache`. Each asset is
`/assets/<path>?v=<16 hex>` with
`Cache-Control: public, max-age=31536000, immutable`. The hash is a SHA-256
of the row's bytes, computed once at startup and applied to the shell's tags
at serve time — the shell stays hand-written, so it cannot carry the hash and
stay correct. `/assets/<anything not in the table>` is a `404`; before the
split it fell through to the fallback and came back as `200 text/html`, which
a browser then tried to execute as JavaScript.

## 6. Reading the shell from a test or a script

`tests/web/shell-source.js` follows the shell's own tag order and returns
`{html, css, headScript, bodyScript, everything, rows, files}`. Use it rather
than reading `index.html` — a test that greps the shell for a function now
passes by finding nothing, which is indistinguishable from passing for the
right reason.

Two traps worth naming, because both were shipped and caught in review. A
`doesNotMatch` against the wrong half is a guard that can never fail: the
cockpit-theme selector lives in `app.css`, so refusing it in `headScript`
proves nothing. And an ordering claim written as `everything.indexOf(tag) <
everything.indexOf(fn)` is true for every possible shell, because in that
string the markup always precedes the rows — compare positions in `rows`
instead.

| You want | Ask for |
|---|---|
| The string the shell used to be, for slicing a function by name | `bodyScript` |
| The stylesheet | `css` |
| `THEMES`, `FONT_*`, `applyLayout` | `headScript` |
| The player's DOM, the sidecar tags | `html` |
| An assertion that crosses all of it, especially an *absence* | `everything` |
| Which row is served before which | `rows`, never string offsets |

Python has no shared helper; `tests/validation/test_decoder_recovery_status.py`
carries a nine-line `web_body_script()` that does the same thing, and the
scripts that scan the tree glob it (`scripts/js-check`,
`scripts/player-input-fence`, `scripts/playback-surface-fence`). Glob, never
list: a fence that names its files by hand stops working the day someone adds
one, which is exactly what the surface fence did for the length of one
commit.

## 7. Known follow-ups

Deliberately not done in the split, because the split's whole safety argument
is that nothing else changed:

- Two comments inside moved code still say "paste into the body `<script>`
  of `crates/plurxd/src/web/index.html`" — `app.css` around the theater
  block, and the head of `layouts/theater.js`. They are historical build
  notes and they are now wrong. Correcting them is a one-line change that
  breaks byte-identity with the pre-split file, so it waits until that proof
  has served its purpose.
- `pages/users-admin.js` and the users panel in `pages/settings-system.js`
  are the same feature in two files, because they were not contiguous in the
  old one.
- Several banners inside their files still carry the title they had when the
  file was one section — `pages/dvr-reminders.js`'s in particular stopped
  being true when the Live TV work landed in it.
- `validation/points.toml`'s playback point lists only `playback-policy.js`
  and `playback-control.js`. Adding `crates/plurxd/src/web/player/**` would
  make a player change select the playback checks under `make
  validate-staged`, which an `index.html` change could never do. That is a
  real improvement and its own PR.
- `app.css` is still one 3,209-line file. Splitting it means measuring the
  cascade first, which is a riskier piece of work than moving JavaScript.
- The three `typeof` guards in §1.5 would read better as `X === undefined`
  against a binding the same row owns, or as a `null`-initialised `let` in an
  earlier row. Changing them is a behaviour change, however small, so it is
  not this PR's.
