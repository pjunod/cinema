# Web type checking and player decomposition — a ratcheted `tsc` over one global scope, then `play()` and `attachHls()` cut at their seams

**Status:** ready for review · **Executes:** W8 / F-web-10 and W9 /
F-web-11, F-web-12, F-web-13, F-web-15 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) (the served-order
rules this plan builds on and must not weaken) and
[WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md](WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md)
(the behaviour changes in the same files — land those first or after, never
interleaved with the decomposition PRs).

Read first: the W8 and W9 rows in the review (§3.5), then the assessment's
W8, W9, F-web-10, F-web-11, F-web-12, F-web-13 and F-web-15 rows in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md),
then WEB-SHELL-LAYOUT.md §1 and §3. Work milestone by milestone (§5); one
draft PR each into `main` under the fast lane.

The standing instruction: **if a step seems to require a bundler, ES
modules, `import`/`export` in a served row, moving a sidecar, or deleting
one of the three ordering gates (`asset-order`, `asset-load`,
`asset-layout`), stop and flag it.** The type checker is added *beside* the
script model, not instead of it. Line numbers are from `88a3957a`; re-verify
by function name.

**Correction to the review:** W8's "only `node --check`" undersells the
gates: `tests/web/asset-order.test.js` parses every row with a vendored
acorn and refuses forward references, duplicates and a missing
`"use strict"`, and `asset-load.test.js` executes all rows in a `vm`
(WEB-SHELL-LAYOUT §1.1). The assessment already said so; this plan keeps all
three and adds two checkers that answer a different question (shape, not
order). W9's "repaints the whole page" was withdrawn in revision 2; the
per-batch *item region* rebuild (`library-grids.js:151`) is what stands.

---

## 1. Objective

1. **W8.** A merge-blocking `tsc --noEmit --allowJs --checkJs` over the
   served rows as one global-scope program, plus ESLint `no-undef`
   configured for that script model, both **ratcheted from a committed
   baseline** so day one blocks new diagnostics and not the existing ones;
   one JSDoc `@typedef Player`; the three ordering gates unchanged.
2. **W8 (second half).** `play()` (`decode-tiers.js:606-1085`, 479 lines)
   and `attachHls()` (`player.js:499-876`, 378 lines) split into named
   functions at the comment seams they already draw, byte-identical in
   behaviour, proven by the existing tests plus the checker from (1).
3. **W9.** Five small, separately-evidenced fixes: append-only library
   batches where order ownership permits; `decoding="async"` and a `?w=`
   poster variant; the paused progress heartbeat suppressed only after its
   readers are traced; MediaSession and TV back-key codes with feature
   detection; one shared reduced-motion rule before the two per-layout
   overrides go.

Done means: `make web-check` runs `scripts/web-types` green against a
baseline that only shrinks; `play()` and `attachHls()` are each under 120
lines of orchestration; and each W9 item has landed with its own test.

---

## 2. Contract today

Copied from `main` @ `88a3957a`; **re-verify at build time**.

### 2.1 The script model and its gates

- Sixty-one rows in `<body>` (`core/app.js` … `router.js`) plus one
  `<head>` row (`core/theme.js`), served in the order of `WEB_ASSETS` in
  [`http/web.rs`](../../crates/plurxd/src/http/web.rs) and mirrored by
  `index.html`. Plain `<script src>`; one global scope; hoisting per file
  (WEB-SHELL-LAYOUT §1.1).
- Seven sidecars (`cluster-panel.js`, `playback-policy.js`,
  `playback-control.js`, `live-tv.js`, `library-channels.js`, `reader.js`,
  `offline-reader.js`) plus `hls.min.js` are UMD-shaped:
  `if (typeof module === "object" && module.exports) module.exports = policy;`
  (`playback-policy.js:3`) and a `window.Plurx…` global. Body rows alias
  them: `const PlaybackPolicy = window.PlurxPlaybackPolicy;`
  (`core/app.js:2`). They are `require()`d by forty-odd tests and bundled
  into both native clients by relative path (WEB-SHELL-LAYOUT §4).
- Gates today: `scripts/js-check` (`node --check` per served row, skipping
  `hls.min.js`), `tests/web/asset-order.test.js` (acorn:
  `missingPrologue`, `duplicates`, `forwardRefs`), `tests/web/asset-load.test.js`
  (vm load), `tests/web/asset-layout.test.js` (doc ↔ shell). All run from
  `make web-check` (`Makefile:1419-1453`) and `web-static` in
  `validation/points.toml:129`.
- The repo has **no `package.json` and no `node_modules`**; acorn 8.18.0
  is vendored at `tests/vendor/acorn.js` and
  [`THIRD-PARTY-NOTICES.md`](../../THIRD-PARTY-NOTICES.md) §7 records that
  adding either "to run one test is a worse trade than 245 KB". CI's node
  job uses `setup-node` with Node 22 (`main-fast-lane.yml:97`).
- `tests/web/asset-graph.js` already computes, per row, the top-level
  declarations and the load-time references — the exact input an ESLint
  `globals` list needs.

### 2.2 `PLAYER`

```js
let PLAYER={fileId:null,timer:null,offset:0,hls:null,knownDur:0,durMs:0,
  markers:[],source:null,started:false,idleTimer:null,autoskip:false,
  deliveredRange:null,deliveredDvProfile:null,waitTimer:null,bookOffset:0,
  bookDuration:0,bookParts:null,_seekPreview:null,_seekPending:null,
  _lastFocusedControl:"pbplay",_opener:null,_openerClick:null};     // player.js:7
```

The literal that replaces it per playback is at `decode-tiers.js:833-903`
with **94** named fields; other rows add fields by assignment
(`hlsRetryUsed`, `triedFallback`, `abr`, `controlSeek`, `mediaAttachment`,
`hlsStartup`, `_levelAt`, `_hlsEvt`, …). No declaration names the shape.

### 2.3 The two functions and their seams

`play(fileId, title, resumeMs, knownDurMs, meta, reservedOpenAttempt,
retryIntent)` — [`decode-tiers.js:606-1085`](../../crates/plurxd/src/web/player/decode-tiers.js).
The seams, by the comment that opens each:

| Lines | Seam (existing comment) | Becomes |
|---|---|---|
| 608-688 | Live TV release · `PLAY_OPEN_GATE.begin` · predecessor · `beginPlaybackPreparation` · `failPreparation` | `beginPlayAttempt(...)` → `{openAttempt, predecessor, preparation, failPreparation, openIsCurrent}` |
| 689-733 | "Consume both one-shot inputs before the first await" · selection · focus capture · `watchPrepare` · `clickedAt` | `capturePlayInputs(...)` → frozen object |
| 735-740 | modal open when not already open | stays inline (6 lines) |
| 741-796 | "The selection travels WITH the decision" · `askDecision` · learned decode-limit reroute | `decideForPlay(...)` → `{decision, retestDecodeLimit, learnedLimitView}` |
| 797-832 | `startSec`/ladder/prior/`autoStartHeight` · "Keep the real outgoing decoder until preparation succeeds" · `prePlayApplication` | `preparePlayOutgoing(...)` |
| 833-903 | `PLAYER={…}` | `buildPlayer(...)` → the literal, typed `Player` |
| 904-942 | `openIsAttached` · HDR degraded notice · DOM toggles · "Server-chosen default subtitle" | `presentPlayerChrome(...)` |
| 943-969 | `useNativeHls` · `playbackInitialRoute` · library-channel override · pre-burn · `unsupported_hevc_delivery` | `choosePlayRoute(...)` → `initialRoute` |
| 970-1059 | `if(initialRoute==='transcode_hls'){…}` and the copy/remux/direct branches | `attachPlayRoute(initialRoute, ...)` |
| 1060-1085 | "A pre-play TEXT subtitle, applied to the stream that just attached" · "Once, per playback" capability log | `finishPlayAttach(...)` |

`attachHls(video, playlistUrl, startAt)` —
[`player.js:499-876`](../../crates/plurxd/src/web/player/player.js):

| Lines | Seam | Becomes |
|---|---|---|
| 499-519 | intent capture · `teardownHls` · `beginPlaybackMediaAttachment` · `clearStreamFailure` · `hlsRetryUsed=0` | `beginHlsAttachment(video)` |
| 520-557 | buffer targets · the `startup` episode literal · `observesCurrent` | `hlsStartupEpisode(...)` |
| 558-608 | `new Hls({…})` · bandwidth seed · `loadSource`/`attachMedia` | `constructHls(startup, tgt, video)` |
| 609-705 | `MANIFEST_*`, `SUBTITLE_TRACKS_UPDATED`, `BUFFER_FLUSHING`, `FRAG_CHANGED`, `LEVEL_LOADED`, `FRAG_LOADED`, `BUFFER_APPENDED` | `wireHlsObservers(hls, startup, video)` |
| 706-864 | `hls.on(Hls.Events.ERROR, …)` | `onHlsError(hls, startup, video)` returning the handler |
| 865-876 | native HLS branch | `attachNativeHls(video, playlistUrl, startAt, attachment)` |

The closures share `attachedPlayer`, `attachment`, `startup`, `hls`,
`video`, `observesCurrent`; the split passes them explicitly through
`startup` (which already holds `player`, `attachment`, `hls`) so no new
shared mutable lives outside it.

### 2.4 The W9 sites

| Item | Where | Today |
|---|---|---|
| Library batches | [`layouts/library-grids.js:100-165`](../../crates/plurxd/src/web/layouts/library-grids.js) | `draw()` rebuilds `#libbody` (`body.innerHTML=layoutRegion("library","items",m)`) per 200-item batch; `o.resort` re-sorts the merged set for a category; `LIB_FILTER`/`LIB_SCOPE`/`LIB_FIND` filter before paging; `LIB_PAGE=200` (`:15`) |
| Poster images | `core/cards.js` `artHtml` | no `decoding="async"`, no width variant; `C6` (server) owns `?size=` derivatives |
| Paused heartbeat | [`player/decode-margin.js:517-521`](../../crates/plurxd/src/web/player/decode-margin.js) | `p.timer` every `AUTO_DEFAULTS.sampleMs` (5 s) calls `reportProgress(p.fileId)` regardless of `v.paused`; `reportProgress` (`stats.js:463`) POSTs `/items/{id}/progress`; the server route ([`http/watch.rs:69-75`](../../crates/plurxd/src/http/watch.rs)) says "This beat is also the heartbeat for a direct play (`crate::delivery`)… a viewer who paused with the rest of the film already buffered makes no further range requests, and without this would drop off the activity page" |
| Media keys / TV back | [`player/stats.js:586-598`](../../crates/plurxd/src/web/player/stats.js) `playerContractInput` — the `player-input-adapter` fence region | `Escape` → `back`; no `MediaPlayPause`/`MediaTrackNext` keys; no `navigator.mediaSession`; no TV back key codes (Tizen 10009, webOS 461, Fire TV/Android TV `GoBack`/keyCode 4) |
| Reduced motion | [`app.css:147-148`](../../crates/plurxd/src/web/app.css) `.poster{…transition:transform .12s,border-color .12s…} .poster:hover{transform:translateY(-3px)…}`; overrides at `:2174-2177` (catalog) and `:3076-3079` (theater) each set `transition:none` **and** `transform:none` | the base rule animates unconditionally; two layouts opt out; classic does not |

---

## 3. Change

### 3.1 The tooling decision — where `tsc` and `eslint` come from

The repo's stance (§2.1) is no root `package.json`. The plan keeps the root
clean and adds **`tools/web-types/package.json` + `package-lock.json`**
pinning `typescript` and `eslint` by exact version, with
`tools/web-types/node_modules/` git-ignored. `scripts/web-types` runs
`npm ci --prefix tools/web-types` when `node_modules` is absent, then the
checkers. Reason for not vendoring: `typescript/lib/tsc.js` alone is ~9 MB
against acorn's 245 KB, which is the trade the notice refused. Reason for
a subdirectory: nothing at the root starts treating the repo as an npm
project. CI: the node job already has `setup-node`; add the `npm ci` step
with the lockfile cached by hash. **This is the one decision Paul should
confirm before 5.1 starts** (§7.1). *Confirmed 2026-09-25 — see §7 Q1 for the
answer and for what the build changed (TypeScript only, no ESLint; no
separate CI cache step).*

### 3.2 `jsconfig.json`, generated from the shell

`crates/plurxd/src/web/jsconfig.json` is **generated** by
`scripts/web-jsconfig` from `tests/web/shell-source.js`'s `rows` (head row,
then body rows, in served order) and checked by a test that regenerates and
diffs, so the file list cannot drift from `WEB_ASSETS`:

```json
{
  "compilerOptions": {
    "allowJs": true, "checkJs": true, "noEmit": true,
    "target": "es2022", "lib": ["dom", "dom.iterable", "es2022"],
    "moduleDetection": "legacy", "module": "none",
    "strict": false, "noImplicitAny": false, "skipLibCheck": true,
    "types": []
  },
  "files": ["types/globals.d.ts", "core/theme.js", "core/app.js", "core/api.js", "…", "router.js"]
}
```

`moduleDetection: "legacy"` is what makes a file with no `import`/`export`
a *script* whose top-level declarations are global — the runtime truth
(W8 row: "TypeScript models scripts as one global scope"). The sidecars and
`hls.min.js` are **not** in `files`: each is a UMD module TypeScript would
classify as CommonJS, hiding its global. Instead
`crates/plurxd/src/web/types/globals.d.ts` (hand-written, ~40 lines)
declares `Hls`, `PlurxPlaybackPolicy`, `PlurxPlaybackControl`,
`PlurxReaderCore`, `PlurxLibraryChannels`, `PlurxClusterPanel`,
`PlurxLiveTv` as `any`-typed globals, and the `webkit`/`CinemaNative`
bridges. A file list does **not** enforce runtime order (assessment W8);
`asset-order` still does.

### 3.3 The baseline ratchet

`scripts/web-types` (python3, like `js-check`):

1. Runs `tsc -p crates/plurxd/src/web/jsconfig.json --pretty false`.
2. Normalises each diagnostic to a key `path<TAB>TS<code>` (line and column
   stripped — line numbers move on every edit and a line-keyed baseline
   would churn) and counts keys.
3. Compares with `tests/web/tsc-baseline.tsv` (`path\tcode\tcount`, sorted).
   **Fails** if any key's count rose or a new key appeared; prints the new
   diagnostics with their real lines. **Passes** otherwise, and if any count
   fell prints `web-types: baseline can shrink by N — run scripts/web-types
   --update`.
4. `--update` rewrites the baseline **only downward**. Raising a count needs
   `--accept-increase "<reason>"`, which writes the reason into a
   `# accepted 2026-09-2x: <reason>` comment line so the PR diff shows it.

Same mechanism for ESLint (`tests/web/eslint-baseline.tsv`, key
`path\t<rule>`), which is expected to start empty: `asset-order`'s
`forwardRefs`/`duplicates` already catch what `no-undef` catches at load;
`no-undef` adds the *call-time* references (a function body naming a global
that exists nowhere), which no gate covers today.

`eslint.config.js` (flat config, in `tools/web-types/`):
`sourceType: "script"`, `ecmaVersion: 2022`, `globals`: browser set + the
union of every row's top-level declarations, **generated** into
`tools/web-types/shell-globals.json` by `scripts/web-jsconfig` from
`asset-graph.js`'s declarations (the assessment's "no-implicit-globals
conflicts with the intentional script model unless configured" — we do
not enable it; we feed the script model's globals in). Rules: `no-undef:
error` only. Nothing stylistic.

A few source edits will be needed (W8 row: "no source edits was
optimistic"): `// @ts-nocheck` at the top of a row is allowed **only** for
`layouts/theater.js`, `layouts/catalog.js` and `pages/settings-panels.js`
if their template-string-heavy bodies produce more than 200 diagnostics
each, and each such line carries a `// TODO(web-types): remove` with the
count. JSDoc `@param` edits are allowed anywhere they reduce the baseline.

### 3.4 `@typedef Player`

At the top of `player/player.js`, above `let PLAYER`:

```js
/**
 * The one playback object. Replaced per title by `buildPlayer()`
 * (decode-tiers.js) and mutated in place by reopens; `null` fields are
 * "not yet", absent fields are a bug the checker now reports.
 * @typedef {Object} Player
 * @property {number|null} fileId
 * @property {number} offset          film position of media-time zero, seconds
 * @property {import("hls.js")|null} hls   — typed `any` via globals.d.ts
 * @property {number} hlsRetryUsed
 * @property {boolean} triedFallback
 * @property {PlaybackAttachment|null} mediaAttachment
 * @property {HlsStartup|null} hlsStartup
 * …one line per field of the decode-tiers.js:833 literal (94) plus the
 * assigned-elsewhere set found by `grep -o "PLAYER\.[a-zA-Z_]*=" player/*.js`
 */
/** @type {Player|null} */
let PLAYER={…};
```

Every field named, with a short comment where the name does not say the
unit. The typedef lands with the first baseline: it will *raise* the count
of property diagnostics in rows that misspell a field, which is the point,
and the first baseline commit accepts them with `--accept-increase
"initial Player typedef"`.

### 3.5 The decomposition

Two PRs, one per function, after 5.1-5.3 are merged so the checker sees
the moved code. Rules that make this a move and not a rewrite:

- Each new function is a `function` declaration in the **same row** as the
  original (`decode-tiers.js`, `player.js`) so no `WEB_ASSETS` row changes
  and `asset-order` is unaffected.
- The original becomes orchestration: a sequence of calls with the
  `openIsCurrent()`/`openIsAttached()`/`observesCurrent()` guards kept at
  the exact statement positions they occupy today. A guard that used to be
  between two statements of the same block stays between the two calls
  that now hold those statements.
- No closure is turned into a parameter it did not already read. The
  shared-state object is `startup` for `attachHls` and a new frozen
  `attempt` object for `play()` holding what §2.3's first two seams
  produce.
- Proof: `make web-check` green, the CDP browser checks green, and
  `scripts/web-types` reporting the baseline **unchanged or lower** —
  the split must not add a diagnostic.

### 3.6 W9 — five small fixes, each with its guardrail

1. **Append batches only where order is owned** (F-web-11). In `draw()`,
   when `!o.resort && LIB_FILTER==="all" && !LIB_SCOPE && !LIB_FIND.trim()
   && LIB_PER==="all"`, a new batch's cards are appended to the existing
   `#libbody` grid instead of rebuilding it; every other combination keeps
   the rebuild, because a later batch of a *category* (multi-library, each
   share sorted alone) may belong before existing cards, and a filter or
   page slice changes which items are visible. The alpha rail and count
   still redraw. Generation guard (`LIB_LOAD!==gen`) and focus retention
   unchanged.
2. **`decoding="async"` and a width variant** on `artHtml` posters. The
   `?w=` parameter is only emitted once the server's C6 derivative route
   exists; until then the attribute lands alone. Reason: a query the server
   ignores is a cache-key change for nothing.
3. **Paused heartbeat** (F-web-12). *Not* suppressed in this plan. The
   trace in §2.4 shows the beat is the direct-play liveness signal for the
   Activity page and `crate::delivery`. The change that is safe now:
   `reportProgress` skips the POST when `v.paused` **and** the position has
   not changed since the last accepted beat **and** the method is not
   `direct_play`; and `pausePlaybackInternally`/`togglePlay` send one
   explicit beat on the pause edge. Direct play keeps beating. A full
   suppression needs a separate liveness message the server does not have.
4. **MediaSession and TV back codes** (F-web-13). In the
   `player-input-adapter` region: `if("mediaSession" in navigator)` set
   `setActionHandler` for `play`, `pause` (distinct — never `togglePlay`
   for an idempotent command), `seekbackward`, `seekforward`, `nexttrack`
   (autoplay-next only when enabled); `setPositionState` from the existing
   500 ms tick, not a new timer. Back codes: `playerContractInput` maps
   keyCode `10009` (Tizen), `461` (webOS) and key `GoBack`/`BrowserBack`
   to `back` beside `Escape`. No Remote Playback / Chromecast claim.
5. **Reduced motion** (F-web-15). Add one shared rule in the base
   stylesheet: `@media (prefers-reduced-motion: reduce){.poster{transition:none}
   .poster:hover{transform:none}}` immediately after `:148`; **then**, in
   the same PR, delete the catalog (`:2174-2177`) and theater
   (`:3076-3079`) overrides. Order matters: the shared rule must cover
   both `transform` and `transition` before the per-layout rules go, or
   the hover motion returns in those layouts.

---

## 4. Guardrails (non-goals)

- **No bundler, no ESM, no `import`/`export` in a served row** (W8 row,
  WEB-SHELL-LAYOUT §3). `moduleDetection: "legacy"` keeps the checker on
  the script model; a row that gains an `export` becomes a module and
  breaks every other row that names it.
- **The three ordering gates stay** (assessment W8, F-web-10). A file list
  is not a load order; `asset-order` proves the order, `asset-load` runs
  it, `asset-layout` documents it. `scripts/web-types` is a fourth check,
  not a replacement for any.
- **Baseline first, blocking second** (assessment W8: "establish an
  actionable typing baseline before making all existing diagnostics
  merge-blocking"). Day one blocks *new* keys and *increases* only.
- **`@ts-nocheck` is a listed exception with a count, never a habit.**
  Three rows may carry it (§3.3); a fourth is a review question.
- **The typedef exposes shape errors; it does not prove
  generation/lifetime correctness** (F-web-10). Ownership races still need
  the behaviour tests in `web-policy.test.js`; this plan adds none of those
  and claims none.
- **The decomposition is a move.** No guard reordered, no closure widened,
  no new shared mutable outside `startup`/`attempt`, same row, baseline not
  raised. If a seam needs a behaviour change to cut cleanly, cut elsewhere.
- **Do not suppress the direct-play heartbeat** (F-web-12). It is the
  liveness signal; §3.6 item 3 says exactly what is skipped and when.
- **Appending is conditional on order ownership** (F-web-11). Categories,
  filters, scopes, finds and page slices keep the rebuild.
- **MediaSession with feature detection; play and pause are separate
  handlers; no Chromecast claim** (F-web-13).
- **Shared reduced-motion rule lands before the overrides are deleted**
  (F-web-15), in one PR so no commit ships the regression.
- **Do not move a sidecar or add one to `jsconfig.json`** — Xcode and
  Gradle inputs (WEB-SHELL-LAYOUT §4).
- **No settings key, no metric, no recipe identity change.** Everything
  here is tooling and client presentation.

---

## 5. Milestones

Fast-lane lifecycle per PR as in
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md).

### 5.1 Tooling in place: `tools/web-types`, generated `jsconfig`, `globals.d.ts`

`tools/web-types/package.json` + lockfile (exact versions);
`scripts/web-jsconfig` (writes `jsconfig.json` and `shell-globals.json`
from `shell-source.js` + `asset-graph.js`); `types/globals.d.ts`;
`tests/web/jsconfig-generated.test.js` (regenerate, diff, fail on drift);
THIRD-PARTY-NOTICES §7 gains one sentence saying dev-only npm tooling lives
under `tools/web-types/` and is not redistributed.

**Acceptance:** `scripts/web-jsconfig --check` exits 0;
`node tests/web/jsconfig-generated.test.js` passes; `npx --prefix
tools/web-types tsc -p crates/plurxd/src/web/jsconfig.json --noEmit | wc -l`
prints a number (the raw baseline size, recorded in the PR body).

### 5.2 `scripts/web-types` with the ratchet, wired into `make web-check` and CI

The script of §3.3, both baselines committed, `Makefile` `web-check` gains
`@scripts/web-types`, `main-fast-lane.yml`'s node job gains `npm ci
--prefix tools/web-types` (lockfile-hash cache) and `scripts/web-types`,
`validation/points.toml` `web.experience.paths` gains `tools/web-types/**`
and `tests/web/*-baseline.tsv`.

**Acceptance:** `scripts/web-types` exits 0 on `main`; introducing one
deliberate `undefinedFn()` call in `core/cards.js` makes it exit 1 naming
`core/cards.js` and the line; `scripts/web-types --update` after removing
that call refuses to write (nothing shrank) with exit 0.

### 5.3 `@typedef Player` and the first shrink

The typedef of §3.4; `--accept-increase "initial Player typedef"`; then at
least one row's diagnostics reduced by JSDoc `@param` edits chosen from the
baseline's largest keys, to prove the ratchet moves.

**Acceptance:** `grep -c "@property" crates/plurxd/src/web/player/player.js`
≥ 94; `scripts/web-types` exits 0 and the baseline diff in the PR shows the
`player.js`/`decode-tiers.js` rows changed and at least one other row's
count lower.

### 5.4 Split `attachHls()` at its six seams

§2.3's second table. Same row, same guards, `startup` carries the shared
state.

**Acceptance:** `attachHls` is ≤ 60 lines; `make web-check` green;
`scripts/web-hls-startup-browser-check` green; `scripts/web-types` baseline
not raised (the script prints `unchanged` or `shrank`).

### 5.5 Split `play()` at its ten seams

§2.3's first table, with the frozen `attempt` object.

**Acceptance:** `play` is ≤ 120 lines; `make web-check` green;
`scripts/subtitle-readiness-browser-check` green (it exercises pre-play
selection through `play()`); baseline not raised.

### 5.6 W9 items 1, 2 and 5 (grid append, poster attributes, reduced motion)

Three commits, one PR.

**Acceptance:** `node tests/web/calm-library.test.js` gains a case
asserting a second batch on a single-library, unfiltered, `per=all` view
appends (card count grows, first card's node identity unchanged) and a
category view rebuilds; `grep -c 'prefers-reduced-motion: reduce' app.css`
is one fewer than today (two overrides gone, one shared rule added);
`scripts/ui-baseline` golden unchanged except the poster attribute.

### 5.7 W9 items 3 and 4 (heartbeat edge, MediaSession and back codes)

**Acceptance:** `node tests/web/player-dom.test.js` gains cases: paused
remux with unchanged position → no POST; paused direct play → POST; pause
edge → exactly one POST; keyCode 10009/461 and `GoBack` → `back`; a stub
`navigator.mediaSession` receives `play` and `pause` as separate handlers.
`scripts/player-input-fence` still passes (the new key reads are inside the
`player-input-adapter` region).

---

## 6. Verification and rollout

- Per PR: `make web-check` (now including `scripts/web-types`), the two CDP
  browser checks, and for 5.6/5.7 `make ui-check` where Playwright is
  available (`web-layout` point, skip-if-missing).
- The two red tests the review found (§4.8: `web-policy.test.js:6007`,
  `web-control.test.js:3164` under Node 22) must be fixed by the review's
  own item (§5.1 15) before 5.2 can call `make web-check` green; do not
  widen a timeout to get there.
- CI cost: `tsc` over ~1.3 MB of JS is 10-20 s warm on the lab runners;
  `npm ci` with a cached lockfile is under 10 s. Record both in the 5.2 PR.
- Rollout: server deploy carries the web app; nothing native changes. The
  W9 poster attribute and CSS are visible immediately; rollback is a
  revert.

GPT prompt for the television check that no Node test covers:

```
On the LG TV's browser and on the Fire TV Silk browser, open
http://10.42.0.10:8080, sign in, play "Night Tide", and press the remote's
Back button once. Report whether the player closed (expected) or the
browser navigated away (defect), and which key the page logged under
Settings → Developer → input trace. Then with the film playing, press the
remote's play/pause key twice and report whether the play state toggled
exactly twice.
```

---

## 7. Open questions

1. **`tools/web-types/package.json` vs a vendored `tsc`.** The plan
   proposes the scoped npm directory (§3.1). If Paul prefers no npm
   anywhere, the alternative is vendoring `typescript/lib/tsc.js` (~9 MB)
   and `eslint`'s far larger tree, which the plan recommends against.

   **Answered by Paul, 2026-09-25:** TypeScript's own checker — `tsc` in
   checkJs mode over JSDoc types ("a duh obviously type thing"). Built in
   5.1-5.3 (branch `plan/W-02-3`) as §3.1 proposed: `tools/web-types/`
   holds `package.json` + `package-lock.json` pinning **typescript 6.0.3**
   exactly as a devDependency with no dependencies of its own;
   `scripts/web-types` runs `npm ci` there when the installed copy is
   missing or not the locked version; `node_modules/` is git-ignored;
   nothing is served, bundled or shipped. Decisions the build took, each
   reversible in one small diff:
   - **6.0.3, not 7.x.** `typescript@latest` is 7.0.2, the native port,
     which ships twenty per-platform binaries as optional dependencies; 6.0.3
     is the newest release that is still one platform-neutral JavaScript
     package, so one lockfile serves the Linux runners and the Mac alike.
   - **No ESLint.** The answer names the type checker, and `tsc` already
     reports the one class `no-undef` was to add: a call-time reference to a
     name no row declares is `TS2304 Cannot find name` (the 5.2 bite proof
     below is exactly that). A second tool, dependency tree and baseline
     would add nothing the checker does not already fail on;
     `shell-globals.json` and `eslint-baseline.tsv` are therefore not
     written.
   - **No CI cache step.** `npm ci` of one 24 MB tarball comes from the
     runner's `~/.npm` cache in about a second; the fast lane's web job runs
     `scripts/web-types` directly.
2. **Baseline key granularity.** `path + code` is drift-proof but coarse:
   fixing one TS2339 and introducing another in the same file nets to
   zero. The alternative (`path + code + message text`) catches that and
   churns when a renamed identifier changes the text. The plan starts
   coarse and can tighten per file once the counts are small.
3. **Should `@ts-check` per file replace project-wide `checkJs`** once the
   baseline is under ~50 keys? Per-file opt-in makes the remaining rows
   explicit. Decide at 5.3 from the numbers. **Decided at 5.3:** not yet —
   the baseline is 546 diagnostics in 86 keys, well above the ~50-key
   threshold, so project-wide `checkJs` stays; revisit when it is under 50.
4. **The heartbeat's other readers** — Trakt scrobbling and the watched
   coalescer read the same POST. §3.6 item 3 keeps every beat that changes
   position and every direct-play beat; a reader that depends on the
   *cadence* of unchanged-position remux beats is not known. The 5.7 PR
   greps `crates/` for consumers of the progress route's timing and lists
   them in the PR body before merging.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M0 (unplanned): `make web-check` was red on `main` | [PR #459](http://192.168.4.7:3000/noirr/plurx/pulls/459) | `9bd9b1fbe`. §6 says the two red tests the review found must be fixed before a milestone can call `make web-check` green. A third had appeared since: `fc125575` ("cluster: add portable backup and fenced restore") gave `developerPanel` a `clusterBackupCard` call that `tests/web/settings-sections.test.js` does not compose, so the whole Developer assertion died on a ReferenceError. Composing the card then exposed that this file's `shippedSource` ended a slice only at the next `function`, so the card's slice swallowed the `const DEV_READINESS_LABEL=` after it and `new Function` threw a SyntaxError; the terminator now ends at a top-level `const`/`let`/`var` too, as its two sibling harnesses already did, and the one further const that had been riding in by bleed (`LIVE_TV_GUIDE_DRAFT`) is composed explicitly. Evidence: at `origin/main` the file prints `FAIL Developer keeps only experiments … 27/28 passed`; with the commit, `28/28 passed`. `make web-check` exits 0 from here on. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.6 — W9 items 1, 2 and 5 | [PR #459](http://192.168.4.7:3000/noirr/plurx/pulls/459) | `552f10cad`. Append where order is owned; `decoding="async"`; one shared reduced-motion rule. Three cases in `tests/web/calm-library.test.js`, each proved by reverting the change under test: `library-grids.js` → `the second batch rebuilt the grid instead of extending it`; `app.css` → `the shared rule does not stop the transition`; `measurements.js` → `the poster image lost decoding="async"`. **Deviation:** the `?w=`/`?size=` width variant is NOT emitted. C-03 landed `?size=w300\|w500\|w780` while this plan was written, so the parameter exists — but choosing a bucket against the grid's real CSS width and device pixel ratio is a browser measurement, and Playwright is absent from both hosts this session can reach. `scripts/ui-baseline` records selectors and counts, not attributes, so the structural golden does not move for `decoding`; `make ui-check` was not run, for the same missing Playwright. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.7 — W9 items 3 and 4 | [PR #459](http://192.168.4.7:3000/noirr/plurx/pulls/459) | `03c3e17d8`. **Deviation, from this plan's own open question 4:** the trace of the progress route's cadence readers found `trakt.rs`'s `sweep_loop`, which REMOVES a session quiet for `IDLE_PAUSE` (150 s) and scrobbles a pause — and a removed session can never scrobble its stop. §3.6 item 3's unconditional skip on an unchanged paused position would therefore have lost the watched flip for anyone who paused for three minutes, so the repeat is bounded by `PAUSED_BEAT_FLOOR_MS = 60 s` rather than abolished. Direct play stays exempt (`delivery.rs`, `IDLE_TIMEOUT` 30 s); `progress.rs`'s coalescer bounds the store write, not the request. A beat whose POST failed is un-recorded so the close beat still goes out. `togglePlay` beats on the pause edge. MediaSession and the TV back codes are inside the one `player-input-adapter` region and `scripts/player-input-fence` passes. Eight revert proofs, listed in the PR body. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.1, 5.2, 5.3 — **not implemented** | — | Blocked on this plan's open question 1, which §3.1 marks "the one decision Paul should confirm before 5.1 starts": whether `tools/web-types/package.json` may exist at all, against the repository's recorded stance (THIRD-PARTY-NOTICES §7, no root `package.json`, no `node_modules`). No session may answer that on Paul's behalf, so no npm directory, no `jsconfig.json`, no baselines and no `@typedef Player` were written. `npm ping` does reach the registry from nuc3, so the decision is the only blocker, not the network. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.4, 5.5 — **not implemented** | — | Neither milestone's stated acceptance can be produced in this session. 5.4 requires `scripts/web-hls-startup-browser-check` green and 5.5 `scripts/subtitle-readiness-browser-check` green; both print `SKIP  playwright is not installed` on the Mac and Playwright is absent from nuc3 as well. Both also require `scripts/web-types` to report the baseline unchanged, which does not exist for the reason in the row above — and §3.5 orders the split after 5.1-5.3 precisely so the checker sees the moved code. Splitting 401 lines of closure-sharing hls.js attach code with neither proof available, on the strength of node tests the same commit would edit, is the shape of change this campaign refuses. Recorded for whoever picks it up: `attachHls` is `player/player.js:521-921` today (the plan's `:499-876` is from `88a3957a`), and three harnesses reconstruct it by slicing the served source — `web-policy.test.js` (six sites), `web-control.test.js` (two) and `web-media-recovery.test.js` (one) — so the split must compose the new functions in each; `web-control.test.js:1938`, `web-policy.test.js:6181` and `web-media-recovery.test.js:49` assert on the text of the slice and would move to `onHlsError`; and the `reason` prose of `validation/regressions.d/3dd6d187-web-lines-that-survived-deletion.toml` and `3d63b5ac-web-media-recovery-fence.toml` names `attachHls` as the function holding lines that would move, so both must be rewritten in the same commit or they become false. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.6, 5.7 — the one adversarial review (PR #459 comment 4106) | [PR #459](http://192.168.4.7:3000/noirr/plurx/pulls/459) | `origin/main` (`99d4abf8c`) merged in `95affd1ae`; one conflict in `tests/web/settings-sections.test.js`, where main composed `clusterBackupCard` relying on its slice bleeding into `DEV_READINESS_LABEL` and this branch composes both constants explicitly with the card already further down the list (30/30). **Finding 1 (P1), fixed in `f7a172617`:** the 5.7 row above and the PR body said MediaSession `play`/`pause` were "separate idempotent handlers"; they branched on the element's `paused`, so during a pending open or a reattach a headset `play` flipped the viewer's intent to pause. Both handlers and the OS `playbackState` now read `playerWantsPlayback` (the pending open's intent, else `PLAYER.wantsPlayback`), pinned in `tests/playback/web-control.test.js` through the real `togglePlay`. **Finding 2, fixed in the same commit:** the handlers now route through `watchRouteInput(playerInputState(), …)` like the keys, pinned in `tests/web/player-dom.test.js` state by state. **Finding 3, fixed in the same commit:** "`nexttrack` is offered only while autoplay-next is on" was false (registered unconditionally, no-op inside); `syncPlayerNextTrack` now registers it only while autoplay-next is on and the title is not known to be a non-episode, and `setAutoNext` re-syncs it. **Finding 4, `9dca9bcec`:** the paged-library case did not bite on the `LIB_PER` guard; it now pages after the load, and filter, scope and find each narrow and clear — deleting any of the four guards fails its case. **Finding 5, `52df3766d`:** the claim that the pause-edge beat is what carries the stop position was false — the repeat guard only drops a paused beat at the last accepted position, which was taken while playing — so the edge beat is prompt, not load-bearing; both comments reworded and a case added. Ledger: `validation/regressions.d/f7a17261-web-media-session-intent.toml`. 5.1–5.3 stay not done (open question 1 is still Paul's); 5.4–5.5 unchanged. No browser, device or fleet evidence was produced. |
| 2026-09-24 | gpt-6-astra | agent:/root/web_recon | 5.4 and 5.5, implementation pending acceptance | [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) · `8907b9cf5`, `cb7042ea2`, `858c8bc01` | `attachHls` is a 15-line orchestrator over six same-file phases; `play()` is a 24-line orchestrator over nine named helpers. The follow-up preserves same-turn direct/progressive attach finalization. Source-slicing harnesses, source assertions and regression-ledger reasons moved with the code. JavaScript syntax passed; the 5.1-5.3 type baseline, Playwright browser checks, final unit lane and the one adversarial review remain unrun, so neither milestone is accepted yet. The playback-lab readiness fix `2a6fbc855` permits a real Chrome shaped-network trace but that trace fails A-04 recovery. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.1, 5.2, 5.3 — built after Paul answered §7 Q1 (2026-09-25: `tsc` checkJs over JSDoc) | draft PR on `plan/W-02-3` (see the board row) | **5.1** `b8ca90d2e`: `tools/web-types` pins typescript 6.0.3 by lockfile; `scripts/web-jsconfig` generates `crates/plurxd/src/web/jsconfig.json` (the 64 served head+body rows in served order, minus the six UMD sidecars and `hls.min.js`, plus `types/globals.d.ts`); `tests/web/jsconfig-generated.test.js` refuses drift. `scripts/web-jsconfig --check` exit 0; the raw run printed **632** diagnostics in 86 `path+code` keys, 2.0 s on nuc3. **5.2** `c6ffdaf99`: `scripts/web-types` with the §3.3 ratchet (per `path+code` counts; `--update` only lowers; `--accept-increase` needs a reason and records it), wired into `make web-check` and the fast lane's web job; `web-static` now requires `npm`; `ci_scope` selects the web job for the gate's files. Baseline accepted at 632. No `@ts-nocheck` anywhere: the largest file (`player/stats.js`, 118) is under §3.3's 200. Bite proof: `undefinedFn();` inside `watchLine` in `core/cards.js` → `scripts/web-types` exit 1 naming `crates/plurxd/src/web/core/cards.js:20:3 TS2304 Cannot find name 'undefinedFn'`, and `make web-check` exit 2 at that line with every earlier gate (asset-order, asset-load, js-check) passing; removed → `scripts/web-types --update` prints `nothing shrank — … not written`, exit 0. `tests/operations/test_web_types.py` (12 cases) pins the ratchet on recorded tsc output; deleting the rise comparison fails 9, ignoring non-file tsc output fails 1. `tests/validation/test_runner.py` pins the job selection (dropping the two `ci_scope` entries fails it). **5.3** `9a8a3628c`: `@typedef Player` with **141** `@property` lines; `let PLAYER` is `@type {Player}`, `buildPlayer()` is `@returns {Player}`, so a new literal field must be named first (adding `brandNewField:1` → `TS2353`, gate exit 1). **Deviation from 5.3's acceptance:** the typedef moved no `player.js`/`decode-tiers.js` count — before it, `PLAYER`'s idle initialiser was an open ("expando") object in a JS file, so a misspelled field reported nothing rather than something; what the typedef adds is the error class, pinned by `tests/web/player-typedef.test.js` (`PLAYER.sesionId` and an alias's `wantsPlaybak` are TS2551; with the typedef removed both report nothing and the test fails). The shrink came from four `PLAYER\|\|{}` aliases typed `Player \| {}`, cast with `/** @type {Player} */(…)` (no runtime change): baseline **632 → 546** (`stats.js` TS2339 114→53, `measurements.js` 21→7, `api.js` 15→9, `decode-margin.js` 11→6). Gates on the branch head: `make web-check` 0 (the two Playwright browser checks print their pre-existing SKIP), `player-input-contract` 0, `web-policy` 0, `web-control` 0, `player-dom` 0, `history-check` 0, `validation-lint` 0, validation unittests 0, `operations-check` 0, `spike-lock-check` 0. No Rust source changed. **Findings the checker surfaced, not acted on here** (each is in the baseline, none is changed by this PR): `core/auth.js` reads the login form's `u`/`p`/`p2` inputs as window named-element globals (TS2304 ×10), and calls `nativeReaderPost` with an argument its declaration does not take (TS2554 ×3); `pages/analysis.js:66` compares a narrowed `visibilityState` with `"hidden"` (TS2367). No device or fleet evidence is needed: nothing served changes behaviour (JSDoc comments and parenthesised casts only). |
