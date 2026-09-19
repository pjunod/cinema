# Web shell split — `index.html` becomes a tree, with no build step

**Status:** BUILT — see [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) for the
shell as it is now; this document is the record of how and why ·
**Executes:** the architecture decision Paul approved 2026-09-19, revised by
the same-day adversarial review · **Pinned to:** `main` @ `395ce5912d08` ·
**Written:** 2026-09-19 · **Built:** 2026-09-19 from `main` @ `a2d9c2fb`

Re-derived per §3.4 before cutting: `index.html` was byte-identical between
`395ce5912d08` and `a2d9c2fb`, so every line number and every row of §3.3
applied verbatim, and nothing in the table below needed changing. The one
adjustment M0 makes is arithmetic — it removes nine lines at 4306, so every
range below that starts at 4330 or later is nine lines lower in the file the
split actually cut.

Companion to [UI-LAYOUTS-IMPLEMENTATION.md](UI-LAYOUTS-IMPLEMENTATION.md)
(what the layouts are) and
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (the fences that scan
the shell) — this is *how the web app's one file becomes sixty, and how to
prove nothing else changed*.

Read this whole document before touching
[`crates/plurxd/src/web/index.html`](../../crates/plurxd/src/web/index.html)
for anything but a feature change. Work milestone by milestone (§8); every
milestone ends with a runnable check, and M1's check is the reason the PR is
safe. Every line number below was taken at the pinned SHA and **drifts with
every merge** — re-derive the cut table (§3) against the base commit you
branch from, with the script in §3.4, before cutting anything. If a step
seems to require changing what the app does at runtime, stop and flag it: the
whole premise of this PR is that it cannot.

## 1. Why — the facts as measured at `395ce5912d08`

`index.html` is 23,901 lines. It holds one `<style>` block of 3,209 lines
(436 KB, of which 196 KB is three `data:` font URLs — that is why the file is
1.7 MB), about 36 lines of markup, and **two** inline scripts: a 218-line
theme engine in `<head>` (lines 3228–3445) that runs before first paint, and
the 20,433-line application in `<body>` (lines 3466–23898). The body script
has 1,152 top-level function declarations, 103 top-level `let` and 109
top-level `const`, no `var`, no classes, and only 39 statements that execute
at load (event-listener registrations, the `LAYOUTS.*` registrations, one
`setInterval`, `applyLayout()`, the router boot). Everything else runs after
the last byte has been parsed.

It was 598 lines on 2026-07-22. 433 commits have touched it since
2026-08-01. Nobody re-decided the shape while it grew forty-fold; every
session added its feature to the file that was there.

The founding decisions still hold and are not up for revision here:
single binary · `include_str!` · no bundler · no framework · hash routing.
What was wrong is that "no build step" got read as "one file". The repo
already knows the difference: seven sidecar scripts exist beside the shell
(`playback-policy.js`, `playback-control.js`, `cluster-panel.js`,
`live-tv.js`, `library-channels.js`, `reader.js`, `offline-reader.js`), and
the `web.rs` comment on each says why — so its logic runs under Node
"instead of a regex that slices functions out of the app shell". This plan
applies that reasoning to the other sixty sections.

What the monolith costs, concretely:

- Every stacked PR collides in one file. Twelve commits on it since August
  are conflict resolutions.
- An agent editing the reminder overlay loads or greps 1.7 MB, and every
  `Edit` match has to be unique across 1,152 functions. That is where the
  wrong-place edits come from.
- 22 test files read the shell as a string; twelve slice functions out of it
  by name, and two of those end a slice at `indexOf("\n}")`. At the pinned
  SHA, six of the nineteen Node entries of `make web-check` are already red
  (§9), three of them with a `ReferenceError` out of a slice that dragged a
  function in without its dependency.
- The AI-harness assessments found JS-in-HTML unverifiable (Ripwire refused
  the file outright).
- Devtools and stack traces say `index.html:9871`, not a file and a line.

## 2. What changes and what does not

**Changes.** The two inline scripts and the `<style>` block move into files
under `crates/plurxd/src/web/`, grouped one level deep by area. `web.rs`
serves them from one ordered table that is also the load order. Asset URLs
carry a content hash. Every reader of `index.html` (§5) is pointed at what
it actually reads.

**Does not change.** The seven existing sidecars, `reader.css`,
`offline-reader.html`, `hls.min.js`, the icons and the manifest **stay at
their current paths and URLs**. They are UMD-shaped modules `require()`d by
path from 40-odd tests and scripts, named in `validation/points.toml`
scopes, asserted on by `web.rs`/`mod.rs` unit tests — and three of them
(`reader.js`, `offline-reader.js`, `offline-reader.html`) are bundled into
both native clients by relative path (`clients/apple/project.yml:27–31`,
`clients/android/app/build.gradle.kts:28,35`). Moving them would put Xcode
and Gradle inputs in the diff, trip the `mobile-version` gate, and fan out
Apple and Android CI for a change whose premise is "no runtime change".
The tree below is for the **new** files only. If a sidecar's folder matters
later, it moves in its own §10-style PR.

The global scope does not change either. The split files are plain
`<script src>` scripts sharing one realm, exactly as the sidecars do today.
No ES modules, no `export`, no threading globals through parameters — that
is §10, later, per area.

## 3. The cut table — M1's input and M6's output

### 3.1 Rules

1. A cut point is a top-level statement boundary. Every `// ---- ` banner in
   the body script is one (verified: none falls inside a function or object
   literal), but banners are the default, not the law — §3.3 adds five cuts
   at function boundaries where a banner's title stopped being true long
   ago, and skips two banner-shaped lines that are not banners.
2. Each new file begins with the banner comment (or the top-level comment
   block) that opened the range, then its code, byte for byte. The fences
   anchor on those banner lines (§5.4); they must stay the first line of
   their file.
3. Each new file gets a `"use strict";` prologue as its first statement. The
   body script has one directive at line 3466 and the head script one at
   3232; a directive applies per script, so without this 59 of the 60 files
   would run in sloppy mode. Nothing throws today that would go silent
   (there is no assignment to an undeclared identifier in the shell), but
   the shell freezes `STATS_ROWS` and other tables, and a write to a frozen
   object is a `TypeError` in strict mode and a silent no-op in sloppy mode.
   The identity gate (§8, M1) strips exactly one leading `"use strict";`
   line per file before comparing.
4. Exactly one block moves: the eight lines `5140–5147`,

   ```js
   LAYOUTS.classic.chrome=classicChrome;
   LAYOUTS.classic.views={
     home:classicHomeBody,
     item:classicItemBody,
     // Region-shaped rather than a single function: library is the app's one
     // incremental route (§3.2) and its regions are redrawn independently.
     library:{shell:classicLibraryShell, items:classicLibraryItems, count:classicLibraryCount},
   };
   ```

   execute at load and name `classicHomeBody` (declared at 5175),
   `classicLibraryShell` (5451), `classicLibraryItems` (5472),
   `classicLibraryCount` (5480) and `classicItemBody` (6619). They resolve
   today only because function hoisting spans the whole script; a hoisted
   declaration in a later file has not been parsed yet, and cut at the
   banners the app dies at load with `ReferenceError: classicItemBody is not
   defined` (reproduced by the review in a `vm` context; the same script
   loads clean un-split in the same stub). They become
   `layouts/register-classic.js`, placed **after `layouts/catalog.js` and
   before `layouts/theater.js`** — so the classic registration still
   completes before `applyLayout()` runs at 22995, exactly as it does today.
   This is the only relocation. A static walk of every load-time statement
   and every `const`/`let` initializer against the section order found no
   second case (§8, M3 makes that walk a permanent gate).
5. One pre-existing duplicate trips the M3 name check and is removed
   **before** the split, in its own one-line PR (§8, M0): `function
   fmtDate` is declared at 4309 (dead, inside `playback measurements`) and
   again at 4752 (`cards`, the one that wins).

### 3.2 The `<head>` script → `core/theme.js`

Lines 3228–3445 (`APP_NAME`, `FONT_*`, `THEMES`, `FAVICONS`, `applyTheme`,
`POSTER_SIZES`, `LAYOUTS`, `surfaceClass`, `layoutId`, `applyLayout`, …)
run in `<head>` "before first paint so there's no flash of the wrong theme",
and the body uses ten of their names (`LAYOUTS` ×21, `APP_NAME` ×14, …).
`core/theme.js` must therefore be a **synchronous `<script src>` in
`<head>`, after the stylesheet links**, so the parser still blocks on it.
No `defer`, no `async`, no `type="module"`.

### 3.3 The `<body>` script — sixty files in served order

Ranges are 1-based, inclusive, at `395ce5912d08`. "End" is the line before
the next cut. `<style>` (lines 17–3225, the content between the tags) is
`app.css` and is not in this table.

| # | Lines | File | Opens with | Notes |
|---|---|---|---|---|
| 1 | 3466–3507 | `core/app.js` | `"use strict";` + globals | the preamble before the first banner: `PlaybackPolicy`/`ReaderCore`/`LibraryChannelCore` aliases, `API`, `TOKEN`, `ME`, native-reader boot |
| 2 | 3508–3636 | `core/api.js` | `// ---- api helpers` | |
| 3 | 3637–4329 | `player/measurements.js` | `// ---- playback measurements` | contains the dead `fmtDate` at 4309 until M0 removes it |
| 4 | 4330–4466 | `core/auth.js` | `// ---- auth flows` | |
| 5 | 4467–4525 | `core/keyboard-reach.js` | `// ---- keyboard reach for click-only cards` | `nav-keyboard-adapter` fence region lives here |
| 6 | 4526–4560 | `core/chrome.js` | `// ---- chrome` | first of two `chrome` banners |
| 7 | 4561–4706 | `core/theme-menu.js` | `// ---- theme + appearance menu` | ends with the QR-dialog Escape handler the input fence bounds by the *next* banner (§5.4) |
| 8 | 4707–4742 | `core/activity-indicator.js` | `// ---- global activity indicator` | that banner is an input-fence end anchor |
| 9 | 4743–4845 | `core/cards.js` | `// ---- cards` | the live `fmtDate` at 4752 |
| 10 | 4846–4915 | `core/lightbox.js` | `// ---- photo lightbox` | |
| 11 | 4916–5026 | `pages/home-helpers.js` | `// ---- views` | the one-line `views` banner plus `get-the-app install prompt`: install prompt, `comingCard`, home grouping helpers |
| 12 | 5027–5134 | `pages/home.js` | `// ---- the page-model seam` | `buildHomeSections`, `viewHome` |
| 13 | 5135–5372 | `layouts/renderers.js` | `// ---- layout renderers` | **minus 5140–5147** (row 58); keeps the comment block 5136–5139 |
| 14 | 5373–5392 | `layouts/header.js` | `// ---- page header` | |
| 15 | 5393–5657 | `layouts/library-grids.js` | `// ---- library grids` | `classicLibraryShell/Items/Count` |
| 16 | 5658–5697 | `detail/helpers.js` | `// ---- detail helpers` | |
| 17 | 5698–5859 | `detail/dynamic-range.js` | `// ---- dynamic range` | |
| 18 | 5860–6005 | `detail/track-facts.js` | `// ---- detail-screen track facts` | |
| 19 | 6006–6744 | `detail/preplay-selection.js` | `// ---- pre-play audio / subtitle selection` | also holds `classicItemBody` (6619) |
| 20 | 6745–6854 | `detail/edit.js` | `// ---- edit details` | |
| 21 | 6855–7714 | `player/player.js` | `// ---- player` | |
| 22 | 7715–7949 | `player/session.js` | `// ---- session lifecycle` | |
| 23 | 7950–8531 | `player/prepared-replacement.js` | `// ---- prepared replacement` | |
| 24 | 8532–9156 | `player/prepared-switch-measurement.js` | `// ---- M3: measuring the prepared switch` | |
| 25 | 9157–9855 | `player/directed-change.js` | `// ---- a directed selection change` | |
| 26 | 9856–11667 | `player/decode-tiers.js` | `// ---- runtime playback capabilities` | the 7-line capabilities banner plus the 1,805-line `HEVC decode tiers`; `PLAY_CAPS` initializer at 10095 executes at load and resolves within the file |
| 27 | 11668–12190 | `player/decode-margin.js` | `// ---- decode margin` | |
| 28 | 12191–12758 | `player/stall-diagnosis.js` | `// ---- stall self-diagnosis` | |
| 29 | 12759–13072 | `player/surface.js` | `// ---- the playback surface` | the presenter the surface fence scans (§5.4) |
| 30 | 13073–13178 | `player/projection-chrome.js` | `// ---- projection chrome, skip intro/credits` | plus `title info panel` |
| 31 | 13179–13954 | `player/transport.js` | `// ---- custom transport` | |
| 32 | 13955–14185 | `player/autoplay-next.js` | `// ---- autoplay next episode` | |
| 33 | 14186–14439 | `player/menus.js` | `// ---- audio-language + subtitle menus` | plus `quality override` |
| 34 | 14440–14685 | `player/audio-sync.js` | `// ---- audio sync` | |
| 35 | 14686–15427 | `player/stats.js` | `// ---- playback stats overlay` | the `player-contract-table --embed` fields block (markers at 14687 and 14690) stays **inside** this file; the two `// ---- generated …` / `// ---- end playback info fields` lines are embed markers, **not** cut points |
| 36 | 15428–15447 | `pages/activity.js` | `// ---- activity page` | twenty lines; fine |
| 37 | 15448–16149 | `pages/analysis.js` | `// ---- analysis status` | `viewAnalysis` … 62 functions |
| 38 | 16150–16560 | `pages/activity-stream.js` | `// ---- Now playing: the Stream cell` | |
| 39 | 16561–16838 | `pages/settings.js` | `// ---- settings` | |
| 40 | 16839–17349 | `pages/settings-panels.js` | `// ---- settings sections` | `isSettingsRoute` … `playbackPanel`; **extra cut** before the `LIVE_TV` comment block at 17350 |
| 41 | 17350–18465 | `pages/live-tv.js` | `// Live TV owns its own media element…` | `LIVE_TV`, `LIVE_TV_DVR`, `viewLiveTv`, the guide, grid, popover, `live-tv-input-adapter` region |
| 42 | 18466–18751 | `pages/live-tv-dvr.js` | `// ---- recording on the Live TV page` | |
| 43 | 18752–18983 | `pages/recordings.js` | `// ---- Recordings` | |
| 44 | 18984–19064 | `pages/dvr-reminders.js` | `// ---- the due-reminder overlay` | `pollDvrReminders` … `dvrReminderRecord`; the banner's title stopped being true at 19065 |
| 45 | 19065–19342 | `pages/live-tv-controls.js` | `async function detachLiveTvMedia` | **extra cut**; `stopLiveTv`, `watchLiveTv`, pause/mute/fullscreen, the `LIVE_TV_TABS` channel and the visibilitychange stop |
| 46 | 19343–19799 | `pages/settings-developer.js` | `function setPreparedHandoff` | **extra cut**; readiness rows, every Developer-tab card, `developerPanel`, `decodeRecoveryCard`, `verifiedDecodeCard` |
| 47 | 19800–20020 | `pages/settings-live-tv.js` | `// The unsaved state of the guide panel…` | **extra cut**; `LIVE_TV_GUIDE_DRAFT`, guide/settings cards, `liveTvPanel`, readiness |
| 48 | 20021–20238 | `pages/settings-system.js` | `function usersPanel` | **extra cut**; users panel, build/storage/replication facts, `systemPanel` |
| 49 | 20239–20557 | `pages/cluster.js` | `// ---- cluster membership` | |
| 50 | 20558–20762 | `pages/cluster-operations.js` | `// ---- the operations rail` | |
| 51 | 20763–20859 | `pages/cluster-database.js` | `// ---- the replicated database` | plus `folding, and remembering it` |
| 52 | 20860–21468 | `pages/cluster-troubleshooting.js` | `// ---- troubleshooting` | |
| 53 | 21469–21740 | `pages/settings-playback.js` | `// ---- playback defaults + Trakt` | |
| 54 | 21741–21787 | `pages/users-admin.js` | `// ---- users (admin)` | 47 lines; the other users code is row 48 — non-contiguous, so two files until §10 merges them |
| 55 | 21788–22457 | `layouts/catalog.js` | `// ---- layout: catalog` | eight banners: catalog, sidebar, one-time wiring, phone sheet, the second `chrome`, `home`, G2b, the first `registration` (`LAYOUTS.catalog` at 22135 and 22450–22456 resolve within the file) |
| 56 | *5140–5147* | `layouts/register-classic.js` | `LAYOUTS.classic.chrome=classicChrome;` | **the relocated block** (§3.1 rule 4) |
| 57 | 22458–22996 | `layouts/theater.js` | `// ---- layout: theater — G4` | includes the second `registration` banner, `LAYOUTS.theater` (22984) and the load-time `applyLayout()` at 22995 |
| 58 | 22997–23326 | `pages/reader.js` | `// ---- EPUB reader` | page glue over the existing `reader.js` sidecar (which does not move) |
| 59 | 23327–23783 | `pages/library-channels-page.js` | `// ---- Library channels` | page glue over the existing `library-channels.js` sidecar; named `-page` so the two URLs cannot be confused |
| 60 | 23784–23898 | `router.js` | `// ---- router` | `render` (23838) and the `hashchange`/boot statements at 23893–23898 |

Row numbering is the served order. The only order constraints that exist are
the ones the static gate (M3) enforces: row 56 after rows 15 and 19 and
before row 57; everything else is free, and moving it is still a PR of its
own because the identity gate is per-order.

The banner-shaped lines that are **not** cut points: 14687 and 14690 (embed
markers, row 35), 4917 (`get-the-app install prompt`, one line after the
`views` banner — row 11 keeps both), and the dashes-only sub-banners inside
rows 55 and 57.

### 3.4 Re-deriving the table at the base commit

The line numbers above are a snapshot. At build time:

```bash
F=crates/plurxd/src/web/index.html
grep -nE '^\s*<(script|/script|style|/style|link)[ >]' "$F"   # the four boundaries
awk 'NR>=BODY_START && NR<=BODY_END && /^\/\/ ---- /{print NR": "$0}' "$F"  # banners
for f in detachLiveTvMedia setPreparedHandoff usersPanel; do        # the extra cuts
  grep -nE "^(async )?function $f\(" "$F"; done
grep -nE '^const (LIVE_TV|LIVE_TV_GUIDE_DRAFT)=' "$F"               # …and their comment blocks
grep -nE '^LAYOUTS\.classic\.' "$F"                                 # the relocated block
```

Then walk each range against the table and confirm the "Opens with" column
still matches. If a new banner has appeared since the snapshot, it gets its
own row in the same folder as its neighbours; if a banner has gone, its
range folds into the row before it. Update the table in this document in
the same commit.

## 4. Contract — `web.rs`, `index.html`, and the URLs

Re-verify every name below against
[`crates/plurxd/src/http/web.rs`](../../crates/plurxd/src/http/web.rs) and
[`mod.rs`](../../crates/plurxd/src/http/mod.rs) at build time.

### 4.1 One ordered table replaces the per-file constants

```rust
/// Kind decides the tag `index.html` carries for the row and where it goes.
pub enum WebAsset { HeadScript, HeadStyle, BodyScript }

/// Served order == dependency order. These are plain scripts sharing one
/// global scope, so a row may only use a binding declared in an earlier
/// row, and the static gate (tests/web/asset-order.test.js) refuses a table
/// that says otherwise. Existing sidecars keep their own routes below and
/// are NOT in this table; the hand-written <script src> rows for them in
/// index.html are unchanged.
pub const WEB_ASSETS: &[(&str, WebAsset, &str)] = &[
    ("app.css",                 WebAsset::HeadStyle,  include_str!("../web/app.css")),
    ("core/theme.js",           WebAsset::HeadScript, include_str!("../web/core/theme.js")),
    ("core/app.js",             WebAsset::BodyScript, include_str!("../web/core/app.js")),
    ("core/api.js",             WebAsset::BodyScript, include_str!("../web/core/api.js")),
    // … rows 3–60 of §3.3, in order …
    ("router.js",               WebAsset::BodyScript, include_str!("../web/router.js")),
];
```

`INDEX_HTML` stays: it is the hand-written shell, and it is what the
table-order unit test inspects (§4.4). The old
`PLAYBACK_POLICY_JS`/`CLUSTER_PANEL_JS`/… constants and their handlers stay
as they are — those files are not in the table (§2).

### 4.2 Hashes are computed at serve time, not written by hand

`index.html` "stays hand-written", so it cannot carry `?v=<hash>` and stay
correct. At startup, once, compute a SHA-256 per row into a `LazyLock`
(`sha2` and `hex` are already `plurxd` dependencies), and serve `/` as the
shell with each table row's `src`/`href` rewritten to
`/assets/<path>?v=<first 16 hex>`. The rewrite is a literal replacement of
the exact attribute value `"/assets/<path>"` for each row, in order; a row
whose tag is not found is a startup panic, not a silent miss, because it
means the hand-written shell and the table disagree (and §4.4 should have
caught it first).

### 4.3 Cache headers and the 404

| URL | Header | Why |
|---|---|---|
| `/` (the shell) | `Cache-Control: no-cache` | today it carries **no** cache header; with immutable assets a heuristically cached shell would pin old hashes across a deploy |
| `/assets/<row>?v=<hash>` | `Cache-Control: public, max-age=31536000, immutable` | the hash is the version |
| `/assets/<unknown>` | `404` | today a mistyped asset URL falls through `web::fallback` and returns the shell as `200 text/html`; the new `/assets/{*path}` handler looks the path up in the table and refuses anything else |
| existing sidecar routes | unchanged (`no-cache`) | not in scope; `hls.min.js` keeps its `max-age=604800` until it gets a hashed URL in its own PR |

The handler ignores the query string. `maintenance_route_eligible` and
`learner_route_eligible` (`mod.rs:637`, `:750`) already admit the `/assets/`
prefix, so the new URLs need no fence change — assert that in the M2 test
rather than assuming it.

### 4.4 The shell, and the test that pins it to the table

`index.html` shrinks to the markup plus tags. The `<head>` becomes, in
order: the existing `<link>` rows, `<link rel="stylesheet"
href="/assets/reader.css">` (unchanged, before `app.css`: no selector
overlap today, keep the order anyway), `<link rel="stylesheet"
href="/assets/app.css">`, then `<script src="/assets/core/theme.js"></script>`.
The `<body>` keeps the existing seven sidecar `<script src>` rows exactly
where they are, followed by rows 1–60 as `<script src="/assets/<path>">`.

A `web.rs` unit test parses `INDEX_HTML` and asserts: every `HeadStyle` row
appears as a stylesheet `<link>` in `<head>`, in table order, after
`reader.css`; every `HeadScript` row appears as a `<script src>` in `<head>`
after the last stylesheet and before `</head>`; every `BodyScript` row
appears as a `<script src>` in `<body>`, in table order, after the sidecar
rows; and no `<script src="/assets/…">` in the shell names anything that is
neither a table row nor a sidecar route. A file cannot be added without
being served, served out of order, or served in the wrong half.

### 4.5 `mod.rs`

Replace the hand-listed sidecar `.route("/assets/…")` lines with the sidecar
routes as they are **plus** one `.route("/assets/{*path}", get(web::asset))`
for the table. The existing `mod.rs:10472` test that enumerates `/assets/`
URLs by hand iterates `WEB_ASSETS` instead.

## 5. Everything that reads `index.html`, and what each needs

This is the inventory the review produced; treat it as the checklist for
M4 and M5, and re-run the `rg` in M4's acceptance to catch anything added
since.

### 5.1 The one helper that replaces the regex slicing's input

`tests/web/shell-source.js`: reads `index.html`, follows its `<link
rel=stylesheet>` and `<script src>` rows in document order, and returns
`{ html, css, headScript, bodyScript, files: [{path, source}] }` where
`bodyScript` is the concatenation of the body rows' sources (the exact
string the identity gate compares). The slicing tests keep their
`shippedSource()`/`shipped()`/`shippedFunction()`/`sliceDeclaration()`
helpers unchanged and feed them `bodyScript` instead of the file. The
"next top-level declaration" end rule survives concatenation as-is: a
function that is last in its file now ends at the next file's prologue
line, which is a no-op expression statement inside the slice.

`require()` of a split file is **not** the mechanism: the files are flat
top-level scripts with no `module.exports`, and §7 forbids adding one.

### 5.2 Node tests (22 files name `web/index.html`)

| Test | Reads | Change |
|---|---|---|
| `web-policy`, `web-control`, `activity-node-names`, `analysis-node-names`, `cluster-membership`, `cluster-recovery-session`, `content-analysis-failures`, `dvr-visibility`, `live-tv`, `nav-keyboard`, `page-read-budget`, `settings-sections` | slice functions by name | feed `shell-source().bodyScript` |
| `live-tv.test.js:1451`, `page-read-budget.test.js:32` | end a slice at `indexOf("\n}")` | same feed; the rule still works on the concatenation (§5.1) |
| `layout-containment`, `calm-library` | the CSS | `shell-source().css` (or read `app.css` directly) |
| `theme-family` | `const THEMES` | read `core/theme.js` |
| `player-input-contract` — "served web playback-info field list is the fixture field list" | the embed out of `index.html` | read `player/stats.js` |
| `player-dom`, `reader`, `library-channels`, `playback-surface-contract`, `seek-control` | markup or sidecars only | verify; expected unchanged |

### 5.3 Python and Rust tests

- `tests/operations/test_contracts.py:345` slices `setPageTimer`…`render`
  out of the shell; both are declared in the `router` range (23829 and
  23838) → read `router.js`.
- `tests/operations/test_evidence_workflows.py:305`,
  `tests/validation/test_decoder_recovery_status.py:1180`,
  `tests/validation/test_runner.py` → check what each asserts; point at the
  file that holds it.
- `crates/plurxd/src/http/web.rs`: seven of its eight unit tests assert on
  `INDEX_HTML` for things that will no longer be in it (`function
  activityStreamState`, `const PlaybackPolicy`, `function
  profileMenuHtml()`, `STATS_ROWS=Object.freeze`, …). Add a test-only
  `fn shell_source() -> String` that joins the `BodyScript` rows, or name
  the row each test means. Do not weaken the assertions.
- Three TOML inventories name the path in prose only; leave them.

### 5.4 Scripts (ten; none were in the first plan)

| Script | What it does today | After the split |
|---|---|---|
| `scripts/js-check` | extracts inline `<script>` blocks, skips `src=` "vendored libraries", `node --check`s each; exits 2 on "no inline blocks found" | `node --check` every `WEB_ASSETS` row (read the table from `web.rs` or from a generated `web/assets.txt` list — pick one and have the M2 test assert they agree); keep the inline-block path for the markup shell's remaining zero blocks so an inline script sneaking back in is still checked |
| `scripts/contrast-check --from-index` | parses `const THEMES={` out of `index.html`; fails with "no `const THEMES={` found" after the split | point `--from-index` at `core/theme.js` (the `FONT_*` consts it carries over are in the same file); update the `make web-check` line |
| `scripts/player-input-fence` | bounds keyboard regions by comment anchors in `index.html` (`INDEX_REGIONS`); one end anchor is the banner `// ---- global activity indicator`, i.e. the first line of the **next** file; fails "lost keyboard allowlist anchors" after the split | rescope per file: each region names its file and its start/end anchors within that file; a region whose end was the next file's banner (rows 7, 55, 57) ends at end-of-file instead; the `catalogWireOnce`/`theaterWireOnce` regions live in `layouts/catalog.js`/`layouts/theater.js`; the `live-tv-input-adapter` region in `pages/live-tv.js`; the player adapter in `player/transport.js` (verify) |
| `scripts/player-contract-table --embed` | writes `PLAYBACK_INFO_FIELDS` into `index.html` between `FIELDS_EMBED_BEGIN/END` (14687–14690); its other three embeds are indented (inside functions) and move with their functions | retarget `WEB` to `player/stats.js`; the three indented embeds need their own file targets (`grep -n 'generated from' crates/plurxd/src/web/**/*.js` at build time) |
| `scripts/web-hls-startup-browser-check` | serves a hand-written `/assets/` → file map | drive it from the table (same list `js-check` reads) |
| `scripts/playback-surface-fence` | `SCANNED` names `index.html` and `playback-policy.js` only; the web render-region anchor is `required=False` | **goes silently vacuous** — proven: with a rogue `document.getElementById("psurface").innerHTML=…` appended to a split file it prints `PASS`, while the same line in the un-split shell fails (`1 surface writes outside the presenter's render, budget is 0`). Add every `player/*.js` row to `SCANNED`, make the web anchors required, and add the must-trip probe to the fence's own self-check |
| `scripts/ui-baseline` | captures the DOM the app builds, not `<head>` | unchanged; `tests/ui-structure.golden` has zero `<script>` rows (its 14 grep hits are the word inside `devcheck-description`), so `make ui-check` stays green **without** regeneration |
| `control-reporter-browser-check`, `reader-browser`, `themes-proposed.json`, `contrast-allow.txt` | mention the path in prose | leave |

### 5.5 `validation/points.toml`

The `web.experience` point already globs `crates/plurxd/src/web/**`, so the
new files are in scope for `web-static`. The playback point lists only
`playback-policy.js` and `playback-control.js`; adding
`crates/plurxd/src/web/player/**` would make a player change select the
playback checks under `make validate-staged`, which an `index.html` change
cannot do today. That is a real improvement and **its own PR** after this
one.

## 6. Baseline — what is red before you start

`make web-check` at `395ce5912d08`, Node entries only (the browser check,
`scripts/web-hls-startup-browser-check`, depends on the machine and is
excluded from every before/after here):

- red: `layout-containment`, `page-read-budget` ("Activity names missing
  cluster nodes…"), `activity-node-names`, `analysis-node-names`,
  `settings-sections` ("Developer keeps explicit enablement…"),
  `cluster-membership` ("the two components stay in their own columns…")
- green: the other thirteen Node entries, `seek-control`, `js-check`,
  `contrast-check`

Re-record this list at your base commit and paste it into the PR
description. The acceptance for every milestone that says "unchanged" means
**the red set at the base SHA, and no new red** — without the list written
down the gate cannot tell the split's breakage from what it inherited. Six
red on `main` is a known state (the `web layout and accessibility` lane has
been red on `main` for weeks); it is not this PR's job to fix them, and it
must not hide behind them either.

## 7. Non-goals — guardrails for the executing agent

- **No ES modules, no `export`/`import`, no `module.exports`**, no threading
  globals through parameters. That is §10, one area at a time, later, and
  only when someone is already in that area for another reason.
- **No bundler, minifier, or Node build step.** Single binary, works
  offline, is a founding decision.
- **No moving the sidecars, `reader.css`, `offline-reader.*`, `hls.min.js`,
  the icons or the manifest** (§2). `offline-reader.html` is not served by
  `plurxd` at all — nothing routes it; it exists for the native WebViews and
  `scripts/reader-browser` — so it does not enter `WEB_ASSETS` either.
- **No renames, no reformatting, no "while I'm here" fixes** inside the
  split files. The gate is byte identity modulo the prologues and the one
  relocation; any other intentional change breaks it and belongs in its
  own PR. The dead `fmtDate` is removed *before* the split (M0), not during.
- **No CSS split.** `app.css` is the `<style>` block verbatim, fonts and
  all. Cascade order is load-bearing and splitting it is a separate,
  riskier move.
- **No touching the 114 `index.html:NNNN` citations across 39 docs.** They
  were stale a week after they were written; M6 ships one table they all
  resolve through instead.
- **No editing `clients/apple/**` or `clients/android/**`.** Nothing in
  this PR needs it, and touching a release input forces a version bump.

## 8. Milestones

Then the PR lifecycle: proper commits · fast lane only until the PR is
together · open as **`WIP:`** · adversarial review · implement the findings ·
fast lane once after the fixes · green → merge it yourself.

### M0 — remove the dead `fmtDate` (its own one-line PR, before the split)

Delete the declaration at 4309 (`player/measurements` range). Acceptance:
`grep -cE '^(async )?function fmtDate\(' crates/plurxd/src/web/index.html`
prints `1`; `make web-check` Node entries match §6.

### M1 — the mechanical split

Cut the body script into rows 1–60 of §3.3 (re-derived per §3.4), the head
script into `core/theme.js`, the `<style>` content into `app.css`;
relocate 5140–5147 into `layouts/register-classic.js`; prepend
`"use strict";\n` to every new `.js` file; rewrite `index.html` per §4.4.

Acceptance, all three, output pasted into the PR description:

1. **Identity.** A one-off script in the PR (`scripts/web-shell-identity`,
   deleted in the same PR once the description carries its output, or kept
   under `scripts/` if the reviewer wants it — decide, don't leave it half
   in) computes: `concat(body rows in order, each with exactly one leading
   "use strict";\n removed)` and compares it to `old body script (lines
   3466–23898 at the base SHA) with lines 5140–5147 cut out and re-inserted
   at the position row 56 occupies`. Equal, byte for byte. Same check for
   `core/theme.js` against 3228–3445 (nothing to relocate; one prologue at
   3232 is already in place, so nothing to strip either — assert the file
   has exactly one), and `app.css` against lines 17–3225.
2. **It loads.** The M3 smoke stub (below) evaluates the head row then the
   body rows in table order with no `ReferenceError`, `render` is a
   function, and `LAYOUTS` has `classic`, `catalog`, `theater` with `home`,
   `item`, `library` on `classic`.
3. **Nothing else moved.** `make ui-check` green against the unchanged
   `tests/ui-structure.golden`; `make web-check` Node entries equal §6's
   red set exactly.

### M2 — serving

`WEB_ASSETS`, `web::asset`, the serve-time hash rewrite of `/`, the cache
headers, the 404, the `mod.rs` route, and the table-order unit test (§4.4).
Acceptance: `cargo test -p plurxd http::web` green; `curl -sI
localhost:32400/assets/player/decode-tiers.js?v=<hash>` shows `immutable`;
`curl -sI localhost:32400/assets/nope.js` is `404`; `curl -s localhost:32400/
| grep -c 'v=[0-9a-f]\{16\}'` equals the number of table rows; a unit test
asserts `maintenance_route_eligible(GET, "/assets/core/api.js?v=x")` and
the learner equivalent are true.

### M3 — the order gate is static, the load is a smoke check

"Reorder any two rows and the test fails" is **false** for a runtime load:
with one shared scope and 39 load-time statements, swapping each adjacent
pair of the sixty files and loading in a stub catches 4 of 59 swaps. So:

- `tests/web/asset-order.test.js` (fast lane, ~1 s, no DOM): parse every row
  with acorn (vendored under `tests/vendor/` if it is not already there —
  check first; no new runtime dependency), and fail if any top-level
  executed statement or `const`/`let` initializer names a binding declared
  in a **later** row (transitively through the callees it invokes; deferred
  callbacks handed to `addEventListener`/`setTimeout`/`.then` are not
  load-time), if any top-level name is declared in two rows, or if any row
  lacks the `"use strict";` prologue as its first statement.
- `tests/web/asset-load.test.js` (smoke): the `vm` stub load from M1's
  acceptance 2.

Acceptance: move `layouts/register-classic.js` above `layouts/renderers.js`
locally → `asset-order` fails naming `classicItemBody`; swap `core/cards.js`
and `core/lightbox.js` → passes, and that is correct; declare a second
`fmtDate` anywhere → fails; remove one prologue → fails; restore → green.

### M4 — every reader points at what it reads

Add `tests/web/shell-source.js` (§5.1) and apply §5.2, §5.3, §5.4 (except
the fences, which are M5). Acceptance: `rg -l 'web/index\.html' tests
scripts validation crates` returns only files that assert on the markup
shell itself or mention it in prose — list them in the PR description with
which of the two each is; `make web-check` Node entries equal §6;
`python3 -m unittest tests.operations.test_contracts
tests.operations.test_evidence_workflows` and
`tests/validation/test_decoder_recovery_status.py` pass under the same
Python the CI lane uses; `cargo test -p plurxd http::web` green with the
seven assertions **strengthened, not deleted**.

### M5 — the fences must trip

`player-input-fence` rescoped per file; `playback-surface-fence` scanning
every `player/*.js` row with its web anchors required and a self-check
probe. Acceptance, all three recorded in the PR description as
command + output: a syntax error added to a split file fails `js-check`; a
`.innerHTML=` write added to `player/menus.js` outside the presenter fails
`playback-surface-fence`; a `window.addEventListener("keydown", …)` added
to `pages/settings.js` fails `player-input-fence`. Then revert all three
and show green. A fence that was not seen to trip has not been tested —
four guards in one branch (PR #147) once passed for reasons unrelated to
what they claimed to check, and this is how that was caught.

### M6 — the layout doc, and the index

Write `clients/WEB-SHELL-LAYOUT.md` (under `docs/`, indexed from
`docs/README.md` in the same commit or `test_docs_index` fails): the §3.3
table as the living reference — served order, file, what it holds, old
line range at the split SHA — plus the head/body rule (§3.2), the prologue
rule, and where a new file goes (its folder, and a row in `WEB_ASSETS`
**and** the shell **and** this table). Keep it honest with a test: every
`WEB_ASSETS` row appears in the table and every table row is a file. Point
`AGENTS.md`'s web section at it. The 114 old citations resolve through the
old-range column; do not edit them.

Acceptance: `python3 -m unittest tests.operations.test_docs_index` green;
the new honesty test green; `rg 'index\.html:[0-9]+' docs/ | wc -l` is
**unchanged** from the base (the point is that nothing had to be touched).

### M7 — rebase helper (optional, cheap once the table exists)

Anything branched from pre-split `main` conflicts on every line of
`index.html`. A `scripts/web-shell-relocate` that rewrites a `git diff`'s
hunk paths and line offsets from the old ranges to the new files, driven by
the §3.3 table, makes that a one-command rebase. Only if an open branch
actually needs it.

## 9. Nits the review settled, so nobody re-derives them

- 22 test files name the shell (not 21); two use `indexOf("\n}")` (not
  three); seven JS sidecars plus `reader.css` (not six); the review's
  "181 mutable globals" figure matched nothing — it is 103 `let` + 109
  `const`; the markup is ~36 lines, not ~220 (the first plan counted the
  head script as markup).
- Section sizes are uneven enough to matter for §10: `HEVC decode tiers`
  1,805 lines, `settings sections` 1,627, `the due-reminder overlay` 1,255
  and misnamed since the Live TV work landed in it. The extra cuts in §3.3
  make the file names true; they do not make the files small.
- "12 conflict resolutions" could not be verified from commit subjects;
  the count of commits since 2026-08-01 is 433.
- Ripwire refusing the file is from the AI-harness assessment, not
  re-checked here; nothing in this plan depends on it.

## 10. Afterwards — one area at a time, never in the split PR

`player/` first: explicit exports, and a grep-able rule that nothing outside
`player/` touches `PLAYER`. Then merge rows 48 and 54 (the two users
files), rename the misnamed banners inside their files, split `app.css` by
area once the cascade has been measured, and add `player/**` to the
playback validation point (§5.5). Each is its own PR with behavior tests,
and each starts from the §3.3 table, not from the banner titles.
