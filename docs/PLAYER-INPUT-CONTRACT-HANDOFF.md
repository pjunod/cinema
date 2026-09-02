# Player input contract — implementation handoff

**Status:** ready to execute · **Executes:**
[PLAYER-INPUT-CONTRACT-PLAN.md](PLAYER-INPUT-CONTRACT-PLAN.md) M1–M4 against
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) · **Written:** 2026-09-02
· **Baseline:** `main` at `095a0d0a` plus PR
[#791](https://github.com/pjunod/plurx/pull/791) (the contract, the fixtures,
the test) — every line number below is from that tree; re-verify before
editing, this file moves · **Rulings:** Paul, 2026-09-02, in §1.2 — not
negotiable

## 1. Orientation — read this, work like this

You are implementing one rule across three clients: **a press asks a pure
reducer "given this state and this input, what happens?", the reducer's
answer is a row in `tests/contracts/player-input-contract.json`, every
client's reducer is tested against every row, and nothing else in the
player is allowed to answer that question.** The same shape applies to the
playback-info panel: its rows come from
`tests/contracts/playback-info-fields.json`, and every client renders that
list.

Read, in order: [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) §1–§3
and §7 (the rule and the row grammar), then
[UI-NAVIGATION-AUDIT.md](UI-NAVIGATION-AUDIT.md) §1 (the five mechanisms you
are removing) and the audit's section for the client you are on, then the
milestone here. The plan doc is the *what*; this document is the *how*,
file by file, with the shape of each change and the test that proves it.

Work one milestone at a time, in order — **M1 web first** (runs in CI
without hardware and sets the test pattern), then M2 Android, then M3
Apple, then M4. Each milestone is one PR into the lane
`effort/player-input-contract` (create it from `main` if it does not exist;
task PRs to `effort/**` get the compile-only gate — see
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md)); the lane's PR into
`main` gets the one full qualification run. Do not start M2 while M1's
acceptance is red. Open PRs; **do not merge** — Paul merges. **Do not
deploy** — Paul deploys.

### 1.1 The standing instruction

If a step here seems to require changing a row in either fixture, **stop
and flag it** in the PR description rather than editing the fixture. The
fixture is the ruling; a client that cannot implement a row is a finding
about the row. `tests/playback/player-input-contract.test.js` fails if the
three rulings below are edited out, and that is deliberate.

### 1.2 Rulings (Paul, 2026-09-02)

1. A directional press on hidden chrome only reveals it. Never seeks.
2. The ten-foot seek bar is its own row and is preview-then-commit:
   Left/Right move a pending position, Select commits, Back cancels,
   Up/Down cancel and leave.
3. The ±30 s vertical seek is dropped everywhere.

Open rulings with defaults already in the fixtures (build the default; do
not wait): the options set is a `settings` menu (M4); tvOS episode cards
keep play-on-select; `select` on an idle timeline is `toggle_play`; the
info panel's Debug list is the union as written.

### 1.3 Vocabulary

| Term | Meaning |
|---|---|
| surface | `ten-foot` (Siri Remote, D-pad, TV browser, and the web timeline when focused) · `touch` · `desktop` |
| state | `hidden` · `transport` · `timeline` · `scrub` · `menu` · `info` · `failed` — where focus is and what is pending |
| input | `left` `right` `up` `down` `select` `back` `play_pause` `skip_back` `skip_forward` `tap_surface` `idle` |
| outcome | the fixture's `outcomes` keys — `reveal`, `focus_row`, `preview`, `commit`, `cancel`, `hide`, `exit`, … |
| adapter | the one file per client allowed to mention a key code; maps platform events to inputs |
| reducer | `route(surface, state, input) → outcome`, pure, no platform imports |
| row grammar | marker row / timeline row (only the bar focusable) / transport row — contract §3 |

---

## 2. What exists today — verified at `095a0d0a`

Line numbers here are current `main`; the audit's are at `18886477`
(PlayerView.swift is +15…+48 lines from there).

### 2.1 Web (`crates/plurxd/src/web/`)

- `index.html:3039` — the whole player DOM on one line: `#modal > #player`
  with `#pbar` (top buttons), `#ptransport` (`#pbplay`, the two `.nudge`
  buttons, `#ptcur`, **`#pseek`** `role="slider" tabindex="0"`, `#ptdur`,
  `#pbaudio`, `#pbsubs`, `#pbinfo`, `#pbpip`, `#pbfs`), `#pinfo`, `#pskip`,
  `#ploading`, `#statsov`, `#pmenu`.
- `index.html:10804-10817` — the player `keydown` on `window`: `typing`
  guard, `seekDelta = PlaybackPolicy.seekDeltaSeconds(e.key)`, `onBtn`
  guard, then `i` → `toggleStats`, `Escape` → `closePlayer` (unless
  `document.fullscreenElement`), Space/`k` → `togglePlay`, arrows →
  `nudge(seekDelta)`, `f` → `toggleFullscreen`, `c` → `cycleSub`. No
  modifier check. Two other `window` keydowns at `:3918` (QR dialog,
  capture) and `:10798` (lightbox) — leave them.
- `playback-policy.js:747-760` — `seekDeltaSeconds`: ←/→ ±10, ↑/↓ ±30;
  exported at `:1025`.
- `index.html:9056-9075` — `pbWireSeek()`: pointer drag with
  `e.preventDefault()` on `pointerdown` (so the bar never takes focus),
  seek on `pointerup`; keydown handles only Home/End.
- `index.html:9003-9013` — `nudge(delta)`: accumulates against a frozen
  base, previews via `PLAYER._seekPreview`, one `seekTo` after 350 ms.
- `index.html:8875-8887` — `playerActivity()`: 2600 ms idle timer,
  `.idle` class; wake listeners on `#player` (`mousemove pointerdown
  touchstart keydown`, `:9081`). CSS `:320`, `:335`, `:339`, `:364`,
  `:377` — `.idle` is `opacity:0` only.
- `index.html:7246-7276` — `play()`; opens the modal at `:7276`; never
  calls `focus()`; `#app` is not made `inert`.
- `index.html:10748-10793` — `closePlayer()`; re-renders the item page
  via `setTimeout(()=>viewItem(id),100)` near the end; no focus return.
- `index.html:9603-9655` — `toggleMenu(kind, low)`: rebuilds
  `#pmenu.innerHTML` with `<button>`s; no Escape, no click-outside, no
  focus move, no `aria-expanded`.
- `index.html:10019-10046` — `setStatsMode` / `toggleStats`; mode
  persisted in `localStorage plurx_stats_mode`; `updateStats` at `:10125`
  rebuilds `.statsbody` `innerHTML` every 1 s; `fmtMbps`/`fmtBytes` at
  `:4802-4803` (bytes are 1024-based).
- Tests: `tests/playback/web-policy.test.js` slices top-level functions
  out of `index.html` by name (`sliceDeclaration`, `:38`) and executes
  them; `:1658-1662` pins `seekDeltaSeconds` including the ±30 rows —
  **you will change that test**. `tests/ui-structure.golden` contains
  the player DOM (90 `pseek`/`ptransport` mentions) and is checked by the
  `web-layout` validation check — **the DOM change in M1 regenerates it**.

### 2.2 Android (`clients/android/app/src/main/java/tv/plurx/app/player/`)

- `PlayerScreen.kt:324-344` — `playerBackAction(panelOpen,
  controlsVisible)` and `playerSeekDeltaMs(keyCode, controlsVisible)`
  (the ±10/±30 map, null while visible); `:346-358` `HiddenSeekAccumulator`.
- `:974-1014` — the surface `onPreviewKeyEvent` (ACTION_DOWN only):
  hidden directions → `nudgeHiddenSeek`, consumed; `MEDIA_PLAY_PAUSE` →
  `playPause`; CENTER/ENTER → `poke()` (consumed when hidden); REW/FF →
  ±10 s + `poke()`; visible directions → `poke()`, not consumed.
- `:920-925` — auto-hide `LaunchedEffect(lastInteraction, isPlaying,
  panel)` 3 800 ms; `:955-959` focus → surface on hide.
- `:1238-1396` — `Controls(...)`: top row at `:1266-1288` (Back, CC,
  Tune, Info, PiP), bottom column with the 700 dp split at `:1341`;
  `TransportButtons` `:1408-1448`; **`PlaybackPositionSlider`
  `:1450-1486`** (Material3 `Slider`, `steps` default, vertical-only key
  intercept); `playFocusRequester` initial focus `:1262-1263`.
- `:1497-1552` — `PlayerSettings`: `RequestInitialFocus(initialFocusRequester)`
  unconditional at `:1510`, requester attached only to the selected
  quality row `:1520-1524` → crash path when the stored quality is not in
  `qualityOptions` (`PlaybackPolicy.kt:131-148`).
- `:1634-1750` — `PlaybackInfoDetails` / `PlaybackInfoOverlay`; mode
  selector `:2037-2058`; ledger body `verticalScroll` with no focusable
  children (`:1907`); builders `:2200-2383`; `formatBitrate`/`formatBytes`
  `:2554-2568`; `deliveryLabel` `:2455`.
- `Controller.kt:1644-1746` — `TrackMenu` (guards its initial focus with
  `initialFocusAttached`, `:1659-1662` — copy that guard).
- `OfflinePlayerScreen.kt:87` — stock Media3 controller
  (`useController = true`).
- `ui/components/TvFocus.kt:45-57` — `RequestInitialFocus` (throws on an
  unattached requester).
- Tests: `test/.../PlayerPolicyTest.kt:12-39` (back precedence, the seek
  map, burst accumulation — **you will rewrite two of these**);
  `androidTest/.../PlayerControlsFocusTest.kt:21-89`;
  `androidTest/.../PlaybackInfoOverlayTest.kt`; `ShelfFocusTest.kt` is
  the model for driving real key events. `build.gradle.kts:80-82` already
  puts `tests/contracts/` on the JVM test classpath.

### 2.3 Apple (`clients/apple/Sources/`)

- `PlayerView.swift:7-22` — `enum PlayerControl` (`reveal close retry
  progress marker skipBack playPause skipForward pictureInPicture audio
  subtitles quality autoplay stats`); `:55-69` `PlayerSeekDirection`
  (±10/±30); `:77-107` **`TVPlayerRemoteRouting.moveOutcome`** — the
  existing reducer, covering `.reveal` and `.progress` only.
- `:776-791` — the hidden `.reveal` surface: `onMoveCommand →
  seekFromRemote` (`:785`), the **only** `onPlayPauseCommand` (`:786`).
- `:976-987` — `.onExitCommand` precedence: engaged bar → stats → hide →
  exit.
- `:697` — `controlAutoHideDelayNanoseconds = 4 s`; `:923-941` the
  `.task(id: autoHideGeneration)` driver; `:1063-1071`
  `shouldAutoHideControls(visible:scrubbing:changingStream:optionMenuOpen:tearingDown:)`;
  `:1133-1143` `optionMenuOpen` (true while focus rests on
  audio/subs/quality on tvOS); `:1164-1181` `hideControls()` (parks focus
  on `.reveal`); `:1193-1199` `revealControlsFromRemote()` (always
  `.playPause`); `:1201-1221` `seekFromRemote`; `:1223-1241`
  `applyRemoteMoveOutcome`.
- `:1296-1333` — `playbackControls` (tvOS branch: transport · time · bar
  · time · options in one row); `:1706-1739` `touchPlaybackRows` (iOS;
  already the right grammar); `:1741-1762` `touchProgressSlider`;
  `:1773-1782` `playbackOptionGroup`.
- `:1947-2003` — `tvProgressBar` (`focusable`, Select toggles
  `tvProgressEngaged` at `:1985`, `onMoveCommand` → `moveOutcome`);
  `:2005-2014` `progressRightControl`.
- `:2261-2275` `PlaybackStatsMode`; `:2314-2400` `PlaybackLedgerRow` /
  `Section` (**`private`**); `:2632-2648` `miniHealth` (stall pill);
  `:2834-2846` `ledgerBackdrop` (iOS Debug swallows taps); `:2849-2861`
  `ledgerInitialFocus`; `:3206` `debugSections`; `:3428`
  `standardSections` (two bodies under `#if os(tvOS)` / iOS); formatters
  `bitRate` `:3904`, `byteCount` `:3908`.
- `PlayerController.swift:5505-5558` — `MPRemoteCommandCenter`, iOS only.
- Tests (`Tests/AppleClientTests.swift`): `:2269` step sizes, `:2276`
  engage routing, **`:2312` `testTVHiddenControlsKeepDirectionalSeeking`**,
  `:2326` up/down routing, `:5658` auto-hide predicate, `:5692` info
  survives hide, `:6956` 1.5 pt ring, `:7017` three mode names, `:6815`
  the reflection pattern for "this modifier is on this view".
  `project.yml:116-119` and `:132-135` bundle `tests/contracts/native-api.json`
  into both test targets — copy those two entries for the two new files.

### 2.4 The contract artefacts (PR #791)

- `tests/contracts/player-input-contract.json` — `surfaces`, `timings`,
  `steps`, `controls`, `inputs`, `states`, `outcomes`,
  `routing[surface][state][input]`, `surface_notes`.
- `tests/contracts/playback-info-fields.json` — `modes`, `sections`,
  `formats`, `health_pill`, `unavailable_rendering`, `fields[]`.
- `scripts/player-contract-table` (`--write` regenerates the doc blocks)
  and `tests/playback/player-input-contract.test.js` (in `make web-check`).

---

## 3. The shape every client implements

Three layers. Only the middle one decides anything.

```
 platform event ──▶ ADAPTER ──▶ REDUCER ──▶ OUTCOME ──▶ VIEW applies it
```

**Reducer signature, all three clients** (names are yours to spell per
language; the semantics are not):

```
route(surface: Surface, state: InputState, input: ContractInput) -> Outcome
```

The reducer body is the fixture, transcribed. Do not "simplify" it into
if-chains that happen to agree today — a table lookup keyed by
`(surface, state, input)` is what the fixture test compares against, and a
table is what stays honest when a row changes. On the web the table *is*
the fixture's `routing` object embedded at build time; on Android and
Apple it is a `when`/`switch` per state generated by hand from the fixture
and checked row-for-row by the test in each milestone.

**State derivation** lives beside the reducer as one pure function from
the view model's observable facts:

```
hidden    = !chromeVisible
failed    = failure view showing                     (checked first)
info      = info panel open in Standard/Debug        (Mini is not a state)
menu      = any option menu/panel open
scrub     = timeline focused && pendingPosition != nil
timeline  = timeline focused
transport = otherwise (chrome visible, a button focused)
```

**Outcome application** is the view's job and is allowed to be
platform-specific: `focus_row` means "return unhandled so the platform
engine moves focus"; `reveal` means "show chrome, restore
`lastFocusedControl`"; `preview` means "move `pendingPosition` by the
ladder and repaint"; `commit` means "seek to `pendingPosition`, clear it".
The acceleration ladder (`steps.preview_acceleration`) is applied by the
view from a repeat counter the adapter supplies (`e.repeat` count on the
web, `KeyEvent.repeatCount` on Android, consecutive `onMoveCommand`s within
250 ms on tvOS — the Siri Remote has no repeat flag).

**Focus memory:** `lastFocusedControl` (initially `play_pause`) is written
whenever focus moves among chrome controls and read by `reveal` and
`focus_transport`. Menus and the info panel record their opener and return
focus to it on close.

**The fence rule:** after M4, `make validate-staged` fails if `KEYCODE_`,
`onMoveCommand`, `onExitCommand`, `onPlayPauseCommand`,
`addEventListener("keydown"`, or `e.key` appear in a client file other than
that client's adapter. Write M1–M3 as if the fence already existed.

---

## 4. M1 — web conforms

Branch `agent/pic-m1-web` → PR into `effort/player-input-contract`.
Everything is in `crates/plurxd/src/web/index.html`,
`crates/plurxd/src/web/playback-policy.js`, and
`tests/playback/web-policy.test.js` unless stated.

### 4.1 Reducer in `playback-policy.js`

1. Embed the routing table. `playback-policy.js` is a UMD module served
   to browsers, so it cannot `require` the fixture at runtime. Add a
   generated block:

   ```js
   // ---- generated from tests/contracts/player-input-contract.json by
   // scripts/player-contract-table --embed; do not edit by hand ----
   const INPUT_ROUTING = { "ten-foot": { hidden: { left: "reveal", … }, … }, desktop: {…}, touch: {…} };
   // ---- end generated ----
   ```

   Extend `scripts/player-contract-table` with `--embed` that rewrites
   that block (same begin/end-marker technique the doc uses), and extend
   `tests/playback/player-input-contract.test.js` with a case asserting
   `require("../../crates/plurxd/src/web/playback-policy.js").INPUT_ROUTING`
   deep-equals the fixture's `routing`. That is the "served copy cannot
   drift" guarantee.
2. Add and export:

   ```js
   function routeInput(surface, state, input) {
     const row = INPUT_ROUTING[surface] && INPUT_ROUTING[surface][state];
     if (!row || !(input in row)) throw new Error(`no route for ${surface}/${state}/${input}`);
     return row[input];
   }
   ```

   Throw, do not default: an unknown combination is a bug in the adapter,
   and a silent `ignore` is how the moles come back.
3. `seekDeltaSeconds` (`:747-760`): drop `ArrowUp`/`ArrowDown` (return
   `null`). Update `web-policy.test.js:1658-1662` to assert `null` for
   both. Add `previewStepSeconds(repeatCount)` implementing the ladder
   from `steps.preview_acceleration` (10 for repeats 0–4, 30 for 5–9, 60
   from 10) and test it against the fixture's ladder, not literals.

### 4.2 State and adapter in `index.html`

4. Add, near `playerActivity()`, one function the tests can slice
   (top-level `function`, column zero, followed by a top-level
   declaration — see the harness's terminator rules at
   `web-policy.test.js:20-45`):

   ```js
   function playerInputState(){
     const p=document.getElementById("player");
     if(document.getElementById("ploading").classList.contains("failed")) return "failed";
     const so=document.getElementById("statsov");
     if(so.classList.contains("on") && so.dataset.mode!=="mini") return "info";
     if(document.getElementById("pmenu").classList.contains("on")) return "menu";
     const seek=document.getElementById("pseek");
     if(document.activeElement===seek) return PLAYER._seekPreview!=null && PLAYER._seekPending ? "scrub" : "timeline";
     if(p.classList.contains("idle")) return "hidden";
     return "transport";
   }
   ```

   `ploading` gains a `failed` class wherever `setLoading(true, "Playback
   failed…")` is called (`:9112-9156` error path; `showStallRecoveryFailure`
   `:3551`) and loses it in `retryPlayback` and `closePlayer`. Keep
   `PLAYER._seekPreview` for the pointer-drag preview and add
   `PLAYER._seekPending` (a Number or `null`) for the keyboard preview so
   the two do not fight.
5. Replace the body of the `window` keydown at `:10804-10817` with an
   adapter. Mark the region so the M4 fence can allow-list it:

   ```js
   // player-input-adapter:begin — the only place in the player that reads e.key
   window.addEventListener("keydown",e=>{
     if(!document.getElementById("modal").classList.contains("open")) return;
     if(e.ctrlKey||e.metaKey||e.altKey) return;                    // browser shortcuts pass through
     if(/^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName||"")) return;
     const state=playerInputState();
     const input=playerContractInput(e, state);
     if(input==null){ playerHotkey(e); return; }                    // i / f / c stay hotkeys
     const outcome=PlaybackPolicy.routeInput("desktop", state, input);
     if(applyPlayerOutcome(outcome, {repeat:e.repeat, direction:input})) e.preventDefault();
   });
   // player-input-adapter:end
   ```

   `playerContractInput(e, state)`: `Escape`→`back`; `ArrowLeft/Right/
   Up/Down`→`left/right/up/down`; `Enter`→`select`; `" "`→`select` when
   `state` is `timeline`/`scrub`, else `play_pause`; `k`/`K`→`play_pause`;
   `j`/`J`→`skip_back`; `l`/`L`→`skip_forward`; anything else `null`.
   `playerHotkey(e)`: the existing `i`/`f`/`c` branches, `f` gated on
   `!e.repeat` like the others.
6. `applyPlayerOutcome(outcome, ctx)` — a `switch`:
   `skip` → `nudge(ctx.direction==="left"?-10:10)` for arrows, `nudge(±10)`
   for `skip_back/forward`; `preview` → `PLAYER._seekPending = clamp((PLAYER._seekPending ?? pbPosSec()) ± PlaybackPolicy.previewStepSeconds(repeatCount))`, `pbTick()`, `playerActivity()`;
   `commit` → `const t=PLAYER._seekPending; PLAYER._seekPending=null; seekTo(t)`;
   `cancel` → `PLAYER._seekPending=null; pbTick()`;
   `commit_then_toggle_play` → both; `toggle_play` → `togglePlay()`;
   `close_menu` → `closeMenu()` (new, §4.4); `close_info` → `toggleStats()`;
   `exit` → `closePlayer()`; `activate` → return `false` (let the button
   click); `ignore` → return `false`; `hide` → `p.classList.add("idle")`.
   Return `true` when the event was consumed. Track `repeatCount`
   yourself: reset on any keyup or a different key; increment on
   `e.repeat`.
7. Pointer: `pbWireSeek` (`:9056-9075`) — remove `e.preventDefault()` from
   `pointerdown` so a click focuses the bar (use `setPointerCapture` alone
   to keep the drag); `blur` on `#pseek` → `PLAYER._seekPending=null;
   pbTick()`. The `pbTick` preview branch (`:8958`) reads
   `PLAYER._seekPending ?? PLAYER._seekPreview`.
8. Clicking the video: the existing `click` → play/pause (`:9166`) is
   `tap_surface → toggle_play` on desktop; leave it but route it through
   the adapter (`applyPlayerOutcome(PlaybackPolicy.routeInput("desktop",
   playerInputState(), "tap_surface"))`) so a click with a menu open
   closes the menu instead.

### 4.3 The timeline row

9. On the `#player` line (`:3039`): move `#ptcur`, `#pseek`, `#ptdur` out
   of `#ptransport` into a new `<div class="ptimeline" id="ptimeline">`
   placed immediately before `#ptransport`. `#pseek` keeps `tabindex="0"`;
   nothing else in `#ptimeline` is focusable. `#ptransport` becomes
   `pbplay · nudge · nudge · spacer · pbaudio · pbsubs · [quality] · [settings] · pbinfo-as-info · pbpip · pbfs`
   — for M1 keep the existing buttons where they are in `#pbar` (the
   options fold is M4); only the bar moves. CSS: `.ptimeline{display:flex;
   align-items:center;gap:8px}` under `.ptransport`'s rules (`:361-363`),
   `.pseek{flex:1}`; `.idle .ptimeline{opacity:0;pointer-events:none}`
   alongside `:364`.
10. Regenerate the structural golden: `make ui-baseline-update` (the
    Makefile target that runs `scripts/ui-baseline --self-host --update`,
    `Makefile:379`) — the `web-layout` check compares against
    `tests/ui-structure.golden`. Commit the regenerated golden in the same
    commit as the DOM change and say so in the message.

### 4.4 Focus, dialog semantics, menus, hide timer

11. `play()` (`:7246-7276`): before `modal.classList.add("open")`, record
    `PLAYER._opener = document.activeElement`; after, set
    `document.getElementById("app").inert = true` and
    `document.getElementById("pbplay").focus({preventScroll:true})`.
    `#modal` gets `role="dialog" aria-modal="true" aria-label="Player"` in
    the markup.
12. `closePlayer()` (`:10748-10793`): `#app.inert = false`; after the
    `viewItem` re-render at the end, restore focus: query the re-rendered
    page for the same `.btnplay`/`playCall` control (by `data-file` or
    item id) and `focus()` it; fall back to `#main`. Do this inside the
    same `setTimeout` so it runs after the render.
13. Menus (`toggleMenu`, `:9603-9655`): on open, store the anchor as
    `PLAYER._menuOpener`, set `aria-expanded="true"` on it and
    `aria-haspopup="menu"` in markup, focus the `.sel` option (else the
    first), and trap Tab inside `#pmenu` (a `keydown` on `#pmenu` for
    Tab/Shift-Tab only — inside the adapter region, since it reads
    `e.key`). Add `closeMenu()`: clear `.on`, `aria-expanded="false"`,
    focus the opener. Click-outside: a `pointerdown` on `#player` whose
    target is outside `#pmenu` and not the opener → `closeMenu()`.
    `toggleStats` (`:10027`): same pattern with `#statsbtn` as opener and
    the first radio as initial focus; closing focuses `#statsbtn`. Give
    the radiogroup arrow-key movement inside the adapter (Left/Right
    among the three radios when one is focused — this is `menu_focus`
    for the `info` state).
14. `playerActivity()` (`:8875-8887`): 2600 → 4000; never idle while
    `PLAYER._seekPending != null`, `#pmenu.on`, `#statsov.on` in
    Standard/Debug, or `ploading.failed`; move the wake listeners
    (`:9081`) from `#player` to `window`, guarded by `modal.open`, so a
    Tab from anywhere re-shows the chrome. CSS: `.player.idle .pbar,
    .player.idle .ptransport, .player.idle .ptimeline{visibility:hidden;
    transition:opacity .2s, visibility 0s .2s}` so hidden controls cannot
    hold focus; on un-idle, if `document.activeElement` is `body`, focus
    `lastFocusedControl` (track it with a `focusin` listener on `#player`
    — also inside the adapter region).
15. `Escape` precedence is now the fixture's `back` column via
    `playerInputState()` — verify by reading the table: `scrub`→cancel,
    `menu`→close_menu, `info`→close_info, else exit. Keep the
    `document.fullscreenElement` guard as a pre-check before the adapter
    (the browser owns Escape in fullscreen), but use
    `isFullscreenAnywhere()` (`:9019`).

### 4.5 Info panel

16. `updateStats` (`:10125-10690`): restructure into a `STATS_ROWS` table
    keyed by the fields fixture's `id`s — `{ id, label, section, modes,
    build(telemetry) → value | null, note?: build(telemetry) → string | null }`
    — and a renderer that walks `playback-info-fields.json`'s order.
    Missing today and required: Standard `Method` (from `baseMethod` +
    encoder clause), `Position`, `Buffer` under NOW DECODING, `Stalls`
    (`:10308` moves from SERVER to NOW DECODING); Debug `Status`,
    `Encoder`, `Subtitles`. Renames: `Ahead` → `Server ahead` with the
    hold/release/suspends clause as a **note** under the same label
    (`:10268-10270` inlines it today), `Delivery` → `Delivery rate`,
    `Dropped / total` → `Frames`, `Delivery method` → `Method`.
    `fmtBytes` (`:4803`) → decimal SI. The header title reads "Playback
    debug" in Debug (`applyStatsMode`, `:10010`). Render by patching text
    nodes (`row.querySelector(".v").textContent = …`) rather than
    replacing `innerHTML` (`:10665`) so a text selection survives the
    1 s tick; rebuild the DOM only when the row set changes.
17. Retire the `ⓘ` ambiguity: `#pbinfo` ("Title info", `:3039`) keeps its
    synopsis job with a different glyph (`≡` or `☰`); `#statsbtn`'s label
    becomes "ⓘ Playback info".

### 4.6 Tests (`tests/playback/web-policy.test.js` + one new file)

18. `routeInput` over every `desktop` row of the fixture (read the JSON,
    loop, `assert.equal`). `previewStepSeconds` against the fixture's
    ladder. `INPUT_ROUTING` deep-equals `routing`.
19. Slice-and-execute `playerInputState`, `playerContractInput`, and
    `applyPlayerOutcome` with a `document` stub the harness already knows
    how to build (see how `:2142` region sets up `document.getElementById`
    stubs for the stall tests); for each state × a representative key,
    assert which of `nudge`/`seekTo`/`togglePlay`/`closeMenu`/
    `closePlayer` was called. At minimum: Escape with `#pmenu.on` calls
    `closeMenu` not `closePlayer`; Right with `#pseek` focused sets
    `_seekPending` and calls neither `nudge` nor `seekTo`; Enter then
    calls `seekTo` once; ArrowUp on body calls nothing; Ctrl+F calls
    nothing.
20. New `tests/web/player-dom.test.js` (add to `web-check` in the
    Makefile): read the `#player` line, assert `#ptimeline` precedes
    `#ptransport`, that `#pseek` is the only `tabindex` inside
    `#ptimeline`, that `#modal` carries `role="dialog"`, and that every
    `.pbtn` that opens a menu carries `aria-haspopup`.
21. Info panel: slice `updateStats`'s row builders and, for a fixed
    telemetry object per mode, assert the rendered label list equals the
    fields fixture filtered by `modes` and `available_on` includes `web`
    (or is absent), in fixture order.

### 4.7 Acceptance

```bash
make web-check                      # green, incl. player-input-contract + player-dom + web-policy
make validate-staged                # web-static, web-layout (regenerated golden), playback-shaping
```

Then in Chrome on the M3 Max against a remux title: Tab to the bar → Right
×3 shows `+30 s` in `#ptcur`, Network tab shows no `/sessions` request →
Enter → exactly one reopen → Escape with the audio menu open closes the
menu, player still open → click Play on the item page, press Space
immediately → playback does not restart. Record those five observations
in the PR body.

---

## 5. M2 — Android conforms

Branch `agent/pic-m2-android`. Bump the Android build with the repo's
bump target (never by hand — `validation/mobile_versions.py` and
`doc_versions.py` sweep every mention; see
[RELEASING.md](RELEASING.md)).

### 5.1 New files

- `player/PlayerInputPolicy.kt` — pure Kotlin, no `android.*` imports:

  ```kotlin
  enum class Surface { TenFoot, Touch }               // desktop is web-only
  enum class InputState { Hidden, Transport, Timeline, Scrub, Menu, Info, Failed }
  enum class ContractInput { Left, Right, Up, Down, Select, Back, PlayPause, SkipBack, SkipForward, TapSurface, Idle }
  enum class Outcome {
      Reveal, FocusRow, FocusMarkerOrIgnore, FocusTransport, Activate, TogglePlay, Skip,
      Preview, Commit, Cancel, CancelThenFocusTransport, CancelThenFocusMarkerOrIgnore,
      CommitThenTogglePlay, CloseMenu, CloseInfo, MenuFocus, Hide, Exit, ToggleChrome, Ignore;
      val contractName: String get() = name.replace(Regex("([a-z])([A-Z])"), "$1_$2").lowercase()
  }
  object PlayerInputPolicy {
      fun route(surface: Surface, state: InputState, input: ContractInput): Outcome = when (surface) {
          Surface.TenFoot -> tenFoot(state, input)
          Surface.Touch -> touch(state, input)
      }
      private fun tenFoot(state: InputState, input: ContractInput): Outcome = when (state) {
          InputState.Hidden -> when (input) {
              ContractInput.Left, ContractInput.Right, ContractInput.Up, ContractInput.Down,
              ContractInput.Select, ContractInput.TapSurface -> Outcome.Reveal
              ContractInput.Back -> Outcome.Exit
              ContractInput.PlayPause -> Outcome.TogglePlay
              ContractInput.SkipBack, ContractInput.SkipForward -> Outcome.Skip
              ContractInput.Idle -> Outcome.Ignore
          }
          // … one `when` per state, transcribed from the fixture's ten-foot table …
      }
      fun previewStepMs(repeatCount: Int): Long = when { repeatCount >= 10 -> 60_000L; repeatCount >= 5 -> 30_000L; else -> 10_000L }
      fun backAction(state: InputState): Outcome = route(Surface.TenFoot, state, ContractInput.Back)
  }
  ```

  Delete `playerSeekDeltaMs` and `HiddenSeekAccumulator`
  (`PlayerScreen.kt:334-358`); `playerBackAction` becomes a one-line call
  into `route` so the existing back test keeps a target.
- `player/PlayerKeyAdapter.kt` — the **only** file with
  `KeyEvent.KEYCODE_*`:

  ```kotlin
  object PlayerKeyAdapter {
      fun contractInput(event: KeyEvent): ContractInput? = when (event.keyCode) {
          KeyEvent.KEYCODE_DPAD_LEFT -> ContractInput.Left
          KeyEvent.KEYCODE_DPAD_RIGHT -> ContractInput.Right
          KeyEvent.KEYCODE_DPAD_UP -> ContractInput.Up
          KeyEvent.KEYCODE_DPAD_DOWN -> ContractInput.Down
          KeyEvent.KEYCODE_DPAD_CENTER, KeyEvent.KEYCODE_ENTER -> ContractInput.Select
          KeyEvent.KEYCODE_BACK -> ContractInput.Back
          KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, KeyEvent.KEYCODE_MEDIA_PLAY, KeyEvent.KEYCODE_MEDIA_PAUSE -> ContractInput.PlayPause
          KeyEvent.KEYCODE_MEDIA_REWIND -> ContractInput.SkipBack
          KeyEvent.KEYCODE_MEDIA_FAST_FORWARD -> ContractInput.SkipForward
          else -> null
      }
  }
  ```

  (`AuthScreens.kt`'s select-to-edit keeps its own `KEYCODE_` reads; it
  is on the fence's allow-list with a reason.)
- `player/TimelineRow.kt` — the composable that replaces
  `PlaybackPositionSlider`:

  ```kotlin
  @Composable
  internal fun TimelineRow(
      positionMs: Long, durationMs: Long, pendingMs: Long?,   // pendingMs != null == scrub state
      focusRequester: FocusRequester,
      onFocusChanged: (Boolean) -> Unit,
      onTouchPreview: (Long) -> Unit, onTouchCommit: () -> Unit,   // drag = preview, lift = commit
      modifier: Modifier,
  ) {
      Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
          PlaybackTime(pendingMs ?: positionMs)                      // the label shows the preview
          Box(
              Modifier.weight(1f).height(24.dp)
                  .focusRequester(focusRequester).onFocusChanged { onFocusChanged(it.isFocused) }
                  .focusable()                                        // NO key handling here — the adapter owns keys
                  .semantics { contentDescription = "Playback position" }
                  .pointerInput(durationMs) { detectDragGestures(onDragEnd = { onTouchCommit() }) { change, _ -> onTouchPreview(fractionToMs(change.position.x, size.width, durationMs)) } }
                  .pointerInput(durationMs) { detectTapGestures { onTouchPreview(fractionToMs(it.x, size.width, durationMs)); onTouchCommit() } }
          ) { /* Canvas: track, elapsed fill, pending fill in accent-alt when pendingMs != null, thumb, focus ring via tvFocusRing */ }
          PlaybackTime(durationMs)
      }
  }
  ```

  Draw with `Canvas`; do not wrap a Material `Slider` — its key handling
  is the defect.

### 5.2 `PlayerScreen.kt` changes

1. **State.** Add `pendingMs: Long?` (scrub), `timelineFocused: Boolean`,
   `lastFocusedControl: PlayerControlId` (`rememberSaveable`, default
   `PlayPause`), `repeatCount` (reset on key-up or a different key). Add
   `fun inputState(): InputState` derived per §3 (`playFailure != null` →
   Failed; `panel == Info && statsMode != Mini` → Info; `panel != null` →
   Menu; `timelineFocused && pendingMs != null` → Scrub; `timelineFocused`
   → Timeline; `!controlsVisible` → Hidden; else Transport).
2. **Adapter wiring.** Replace the surface `onPreviewKeyEvent` body
   (`:974-1014`) with: ACTION_DOWN only → `PlayerKeyAdapter.contractInput`
   (null → `false`) → `PlayerInputPolicy.route(Surface.TenFoot,
   inputState(), input)` → `applyOutcome(outcome)`. `applyOutcome` returns
   `true` when consumed. Keep it on the root Box so it still runs in the
   preview phase before any child — that is what lets the reducer own
   Select and Back with a panel open. **Track BACK here too**: with the
   adapter owning `KEYCODE_BACK` the `BackHandler` (`:744-753`) becomes
   the touch/gesture path only — call the same `applyOutcome` from it.
3. **Outcome application.** `Reveal` → `controlsVisible = true; poke();
   request focus on lastFocusedControl` (a small map from
   `PlayerControlId` to `FocusRequester`s you create for play/pause,
   skip-back, skip-forward, each option button, the timeline, the marker);
   `FocusRow` → `false` (engine moves); `FocusTransport` → focus
   `lastFocusedControl` if it is a transport control else `PlayPause`;
   `FocusMarkerOrIgnore` → focus the marker requester if
   `activeMarker != null` else `true`; `Preview` → `pendingMs =
   ((pendingMs ?: realPosition) + sign * previewStepMs(repeatCount))
   .coerceIn(0, durationMs)`, `poke()`; `Commit` → `seekWithMarkerUndo(pendingMs!!); pendingMs = null; poke()`;
   `Cancel` → `pendingMs = null`; the `CancelThen…` and `CommitThen…`
   variants compose those; `TogglePlay` → `controller.playPause(); poke()`;
   `Skip` → `seekWithMarkerUndo(realPosition ± 10_000); poke()`;
   `CloseMenu`/`CloseInfo` → `panel = null; focus the opener`; `Hide` →
   `controlsVisible = false` (focus → surface via the existing effect
   `:955-959`, which should also record `lastFocusedControl` first);
   `Exit` → `onExit()`; `Activate`/`MenuFocus`/`Ignore` → `false`.
4. **Rows.** In `Controls` (`:1238-1396`): the bottom `Column` becomes
   `title/facts/context/overview` → `TimelineRow` (own row, always full
   width, rendered when `durationMs > 0`) → transport `Row`:
   `TransportButtons · Spacer(weight 1f) · [CC] [Tune] [Info] [PiP]` —
   i.e. move the four top-row option buttons (`:1270-1287`) down after the
   spacer and delete the top row except the Back arrow (keep Back at
   top-left for touch; on TV it is unreachable by design — `back` is the
   key). Narrow (`currentFormFactor() == Phone && maxWidth < 700.dp`): the
   transport row splits at the spacer into two lines. A TV never gets the
   split: decide on `currentFormFactor()` first, width second.
5. **Focus properties.** `Modifier.focusProperties { up = timelineRequester }`
   on every transport-row control; on the timeline `down =` the
   `lastFocusedControl` requester (update it as focus moves; default
   play/pause) and `up =` the marker requester when present else
   `FocusRequester.Cancel`. The marker button is composed only while
   `controlsVisible` (drop the `!scrubbing` gate; add
   `pendingMs == null`) so `up` from the timeline reaches it.
6. **Play/Pause everywhere.** Already on the surface handler; make sure
   panel scrims (`PlayerPanelSurface :2608-2633`, `TrackMenu`
   `Controller.kt:1668-1683`) do not consume `KEYCODE_MEDIA_PLAY_PAUSE` —
   they are `canFocus = false` scrims so the root preview handler sees the
   key first; verify with the instrumented test.
7. **Hide timer.** `LaunchedEffect(lastInteraction, isPlaying, panel,
   pendingMs, playFailure)` → `if (isPlaying && panel == null && pendingMs == null && playFailure == null) { delay(4_000); controlsVisible = false }`.
8. **Panels trap and return focus.** Wrap `TrackMenu`'s column,
   `PlayerPanelSurface`'s column, and `PlaybackInfoOverlay`'s panel in
   `Modifier.focusGroup().focusProperties { exit = { FocusRequester.Cancel } }`;
   do not compose `Controls` while `panel != null` (today `:1115-1117`
   leaves them under the scrim); on dismiss (`panel = null`) request
   focus on the opener's requester (store `panelOpener` when opening).
   One dismissal chrome: `back` and scrim tap; remove the ArrowBack
   "Close" header button in `PlayerPanelSurface` (`:2624-2629`) or give
   `TrackMenu` the same — pick "no header button, back closes" to match
   tvOS.
9. **Settings crash guard.** `PlayerSettings` (`:1497-1552`): attach
   `initialFocusRequester` to the selected row **or the first row when
   nothing matches** — `val focusIndex = qualityOptions.indexOfFirst { it.quality == preferences.playbackQuality }.coerceAtLeast(0)` and attach at that index. Unit test with a stored
   `Q1080` and a 720p-only ladder: no throw.
10. **Info panel.** `PlaybackInfoDetails` builders (`:2200-2383`) become a
    list of `InfoRow(id, label, section, modes, value, note, placement)`
    built in fixture order; add `container` (from `details`/source facts),
    `stalls` (from the stall reporter the telemetry already counts —
    `PlaybackTelemetry` supply/decode counters), Mini `dynamic_range`
    chip; rename `Encode` → `Encode speed`, `Ahead` → `Server ahead`,
    `Range` → `Dynamic range`, `Delivery` → `Delivery rate`,
    `Transferred` → `Delivered`; `Buffer` renders `"%.1f s"` (drop the
    clock+percent, `:1580`); `formatBitrate` (`:2554`) emits `Mb/s`/`kb/s`;
    one Status vocabulary (`:2244` and `:2333` agree: "No server-side
    session" / "Active" / "Holding buffer"). Navigation: opening Info no
    longer sets `controlsVisible = false` (`:1116` — it is the `info`
    state; chrome stays, timer holds); the mode selector chips re-request
    focus after a switch (`RequestInitialFocus` keyed on `statsMode`);
    every section card and the notes strip gets `.focusable()` +
    `tvFocusRing` so the D-pad scrolls the ledger (`:1907`); `statsMode`
    persists in `ViewerPreferences` (add a field; default Standard).
11. **Offline player.** `OfflinePlayerScreen.kt:87`: `useController =
    false` and render `Controls` with a local `OfflinePlayerAdapter`
    exposing the same callbacks (`playPause`, `seekTo`, `realPosition`,
    `duration`, `isPlaying`) — no panels beyond Info; same
    `TimelineRow`, same `PlayerKeyAdapter`.

### 5.3 Tests

- `test/.../PlayerInputPolicyTest.kt`: load
  `player-input-contract.json` from the classpath (the
  `ModelContractTest.kt:83` pattern), loop `routing["ten-foot"]` and
  `routing["touch"]`, assert `route(...).contractName == expected` for
  every cell; assert `previewStepMs` against `steps.preview_acceleration`.
  Delete the ±30 assertions in `PlayerPolicyTest.kt:19-39`; keep the back
  test pointed at `backAction`.
- `test/.../PlayerSettingsFocusTest.kt`: the stored-quality-absent case.
- `androidTest/.../PlayerControlsFocusTest.kt`: extend with counting
  callbacks (`var seeks = 0`, `var previews = 0`) and real `pressKey`
  sequences through `Controls` + the root handler (compose the same tree
  `PlayerContent` uses, or extract the handler modifier so the test can
  attach it — but the test must drive the **shipped** modifier, not a
  copy): Right from skip-forward lands on a transport control, not the
  timeline; Up from play/pause lands on the timeline; Right ×3 on the
  timeline → `previews == 3, seeks == 0`, label shows +30 s; CENTER →
  `seeks == 1`; DOWN → focus on play/pause, `seeks` unchanged; hidden
  chrome + Right → chrome visible, `seeks == 0`, focus on the last
  control; Up from the timeline with a marker → focus on the marker;
  MEDIA_PLAY_PAUSE with Tracks open → `playPauses == 1`; CENTER as the
  first press with Info open → the close button activates.
- `androidTest/.../PlaybackInfoOverlayTest.kt`: for each mode, render
  with a fixed `PlaybackInfoDetails`, collect the row labels in order,
  assert equality with the fields fixture filtered by mode and
  `available_on` (android or absent).

### 5.4 Acceptance

```bash
make android-test                                   # JVM + lint
make android-instrumentation-build                  # both APKs build
```

The instrumented suite runs on the CI emulator lane (it gates the PR —
there is no `-e class` filter). Then the physical script,
[PLAYER-INPUT-CONTRACT-PLAN.md](PLAYER-INPUT-CONTRACT-PLAN.md) §6, on a
phone and a Google TV / Shield; Paul runs it or hands it to the device
runner — say so in the PR, do not claim it.

---

## 6. M3 — Apple conforms

Branch `agent/pic-m3-apple`. `make apple-build-bump` for the build number.
Swift compiles only on the self-hosted macOS runner — push early, read
the job.

### 6.1 New files

- `Sources/PlayerInputRouting.swift` — pure, no SwiftUI import:

  ```swift
  enum PlayerInputSurface { case tenFoot, touch }
  enum PlayerInputState { case hidden, transport, timeline, scrub, menu, info, failed }
  enum PlayerContractInput: String, CaseIterable {
      case left, right, up, down, select, back, playPause = "play_pause",
           skipBack = "skip_back", skipForward = "skip_forward", tapSurface = "tap_surface", idle
  }
  enum PlayerInputOutcome: String {
      case reveal, focusRow = "focus_row", focusMarkerOrIgnore = "focus_marker_or_ignore",
           focusTransport = "focus_transport", activate, togglePlay = "toggle_play", skip,
           preview, commit, cancel, cancelThenFocusTransport = "cancel_then_focus_transport",
           cancelThenFocusMarkerOrIgnore = "cancel_then_focus_marker_or_ignore",
           commitThenTogglePlay = "commit_then_toggle_play", closeMenu = "close_menu",
           closeInfo = "close_info", menuFocus = "menu_focus", hide, exit,
           toggleChrome = "toggle_chrome", ignore
  }
  enum PlayerInputRouting {
      static func route(surface: PlayerInputSurface, state: PlayerInputState, input: PlayerContractInput) -> PlayerInputOutcome { /* one switch per state, transcribed */ }
      static func previewStepSeconds(repeatCount: Int) -> Double { repeatCount >= 10 ? 60 : repeatCount >= 5 ? 30 : 10 }
  }
  ```

  Keep `TVPlayerRemoteRouting` (`PlayerView.swift:77-107`) only as a thin
  wrapper for one release if anything else calls it; otherwise delete it
  with its tests and replace them (§6.3). `PlayerSeekDirection.seconds`
  loses its `up`/`down` cases (`:61-68`).
- `Sources/PlayerRemoteAdapter.swift` — a `ViewModifier` that owns every
  `onMoveCommand`, `onExitCommand`, `onPlayPauseCommand`, and the Select
  `onTapGesture`s, applied **once** at the player root:

  ```swift
  struct PlayerRemoteAdapter: ViewModifier {
      let state: () -> PlayerInputState
      let apply: (PlayerInputOutcome, PlayerContractInput) -> Bool   // true = handled
      func body(content: Content) -> some View {
          content
              .onMoveCommand { dir in
                  guard let input = Self.input(for: dir) else { return }
                  _ = apply(PlayerInputRouting.route(surface: .tenFoot, state: state(), input: input), input)
              }
              .onExitCommand { _ = apply(PlayerInputRouting.route(surface: .tenFoot, state: state(), input: .back), .back) }
              .onPlayPauseCommand { _ = apply(PlayerInputRouting.route(surface: .tenFoot, state: state(), input: .playPause), .playPause) }
      }
      static func input(for d: MoveCommandDirection) -> PlayerContractInput? { … @unknown default: nil }
  }
  ```

  **The one tvOS subtlety:** an `onMoveCommand` on the root consumes the
  press and the system focus engine does *not* move focus. For the
  `transport` state the contract says `focus_row` = let the engine move.
  So the root modifier must only be *installed* for the states that need
  it: attach `.onMoveCommand` to the hidden `.reveal` surface (as today,
  `:785`) and to the timeline view (as today, `:1988`), and attach
  `.onExitCommand` + `.onPlayPauseCommand` to the root. That keeps
  `focus_row` system-driven while every handler still goes through the
  reducer. Both attachments live in this file (as two small modifiers,
  `PlayerRemoteAdapter.surface` and `PlayerRemoteAdapter.timeline`) so
  the fence has one file to allow.

### 6.2 `PlayerView.swift` changes

1. **State.** Replace `tvProgressEngaged` with `pendingMs: Int?`. Add
   `lastFocusedControl: PlayerControl = .playPause` (update in the
   `onChange(of: focusedControl)` at `:972-975` whenever the new value is
   a chrome control), `repeatCount` with a 250 ms window timestamp, and
   `func inputState() -> PlayerInputState` per §3 (`controller.failed` →
   `.failed`; `showStats && statsMode != .mini` → `.info`;
   `activeOptionMenu != nil` (iOS) / a system `Menu` presented — track
   with `onChange` of the `Menu`'s focus, or treat focus on
   audio/subtitles/quality **with the menu open** via the existing
   `optionMenuOpen` (`:1133-1143`) → `.menu`; `focusedControl ==
   .progress && pendingMs != nil` → `.scrub`; `focusedControl ==
   .progress` → `.timeline`; `!controlsVisible` → `.hidden`; else
   `.transport`).
2. **Adapter wiring.** `.reveal` surface (`:776-791`): drop its own
   `onMoveCommand`/`onPlayPauseCommand`/`onTapGesture` bodies and apply
   `PlayerRemoteAdapter.surface(state:apply:)`; its Select maps to
   `.select`. Timeline (`:1947-2003`): drop `onTapGesture` (engage) and
   `onMoveCommand` bodies, apply `PlayerRemoteAdapter.timeline`. Root
   `ZStack` (`:976-987`): replace `.onExitCommand` with the adapter's
   root modifier. Delete `seekFromRemote` (`:1201-1221`),
   `applyRemoteMoveOutcome` (`:1223-1241`), `progressRightControl`
   (`:2005-2014`).
3. **Outcome application** `apply(outcome, input) -> Bool`: `.reveal` →
   `revealControlsFromRemote()` with `focusedControl = lastFocusedControl`
   (change `:1193-1199`); `.focusTransport` → `focusedControl =
   lastFocusedControl.isTransport ? lastFocusedControl : .playPause`,
   `revealControls()`; `.focusMarkerOrIgnore` → `.marker` if
   `activeMarker != nil`; `.preview` → `pendingMs = clamp((pendingMs ??
   currentMs) ± previewStepSeconds(repeatCount)*1000)`, `revealControls()`;
   `.commit` → `controller.seek(toMs: pendingMs!); pendingMs = nil`;
   `.cancel` → `pendingMs = nil`; `.togglePlay` → `controller.togglePlayPause()`
   + `revealControls()`; `.skip` → `controller.skip(seconds: ±10)`;
   `.closeMenu` → dismiss + `focusedControl = menuOpener`; `.closeInfo` →
   `dismissPlaybackInfo()` with focus → `.stats` (change `:1183-1190`
   from `.playPause`); `.hide` → `hideControls()`; `.exit` →
   `finishPlayback()`; `.ignore`/`.focusRow`/`.activate`/`.menuFocus` →
   `false`.
4. **Rows.** tvOS `playbackControls` (`:1296-1333`): the branch becomes
   `VStack { PlayerTrailingControlRow (marker) ; HStack { timeLabel; tvTimeline; durationLabel } ; HStack { skipBack; playPause; skipForward; Spacer(); playbackOptionGroup } }`.
   `tvProgressBar` → `tvTimeline`: keep the capsule and the hairline ring
   (the ring test at `:6956` stays), draw a second fill in
   `Palette.accent.opacity(0.5)` up to `pendingMs` when non-nil, and show
   `pendingMs` in the elapsed label (the tvOS capsule label in
   `playbackControls` reads `controller.currentMs` today — read
   `pendingMs ?? currentMs`). No `scaleEffect` "engaged" state.
5. **Auto-hide.** `shouldAutoHideControls` (`:1063-1071`) gains
   `playing: Bool, failed: Bool, infoOpen: Bool, previewPending: Bool` and
   returns `visible && playing && !scrubbing && !changingStream &&
   !optionMenuOpen && !failed && !infoOpen && !previewPending &&
   !tearingDown`. `optionMenuOpen` (`:1133-1143`) on tvOS becomes "a
   system `Menu` is presented", not "focus rests on its button": SwiftUI
   gives no direct signal, so track it with `onChange(of:
   focusedControl)` — focus leaving audio/subtitles/quality to *nothing*
   (nil) while `controlsVisible` is the menu opening; focus returning is
   it closing. `hideControls()` (`:1164-1181`): record
   `lastFocusedControl` before `focusedControl = nil`; never called while
   `controller.failed` (guard at the top).
6. **Failure view.** `back` in `.failed` → `.exit` on the first press
   (the reducer already says so; remove the `controlsVisible` branch's
   applicability by checking `inputState()` first).
7. **iOS.** `touchPlaybackRows` (`:1706-1739`) already has the grammar;
   `touchProgressSlider` (`:1741-1762`) already previews then seeks on
   release; apply the `touch` table's `back` semantics to the ✕
   (`closeButton`, `:1286`): with a popover open it closes the popover
   (`activeOptionMenu = nil`) and with the info panel open it closes the
   panel — one control, contract precedence. `MPRemoteCommandCenter`
   (`PlayerController.swift:5505`) stays iOS-only (`skip_back/forward`
   producers for touch).
8. **Info panel.** One `standardSections` for both platforms (delete the
   `#if os(tvOS)` split at `:3428`): Standard gains `Buffer` and `Stalls`
   under NOW DECODING, `Status` on iOS, `Method` + `Position` rows on tvOS
   (today only in the subtitle); `Subtitles` leaves SERVER on iOS
   (`:3679` region); `Output` → `Resolution`; `Buffer ahead`/`Server
   ahead` → `Server ahead` with the held clause as a note; tvOS Standard
   `Transferred` (server bytes) → `Delivered`; `Transferred` stays for
   Debug's client bytes; `bitRate` (`:3904`) → `"%.1f Mb/s"` below 10,
   `"%.0f Mb/s"` at or above, `"%.0f kb/s"` below 1; resolution without
   spaces (`:3466`); the Mini pill shows server state (`sessionStatus`)
   and the stall count becomes the `Stalls` row (`:2632-2648`); position
   uses `/` not `of` (`:2921`). Make `PlaybackLedgerRow`/`Section` and
   the two section builders `internal` so the test can read them. Mini
   wires `ledgerInitialFocus` like the other modes (`:2812` region);
   `dismissPlaybackInfo` returns focus to `.stats`; iOS Debug's
   `ledgerBackdrop` (`:2834-2846`) toggles the chrome like Standard
   instead of swallowing; `statsMode` persists in `UserDefaults`
   (`plurx.playbackInfoMode`).

### 6.3 Tests (`Tests/AppleClientTests.swift`)

- `project.yml`: add `../../tests/contracts/player-input-contract.json`
  and `../../tests/contracts/playback-info-fields.json` to both test
  targets' `sources` with `buildPhase: resources` (`:116-119`, `:132-135`).
- `testPlayerInputRoutingMatchesTheSharedContract`: decode the JSON
  (`Bundle(for:)` as at `:614-623`), loop `routing["ten-foot"]` and
  `routing["touch"]`, assert `route(...).rawValue == expected` for every
  cell. Replace `:2269` (drop the 30 s cases), `:2276` (engage routing —
  delete; the contract has no engaged state), **`:2312` → rename
  `testTVHiddenControlsRevealWithoutSeeking`** and assert `.reveal` for
  all four directions, `:2326` (up/down → the `timeline`/`scrub` rows).
- `testPreviewAccelerationMatchesTheContractLadder`: against the fixture's
  `steps.preview_acceleration`.
- Extend `:5658` `testPlayerOverlayAutoHidesWheneverItIsIdle` for
  `playing: false`, `failed: true`, `infoOpen: true`, `previewPending:
  true` → no hide.
- `testPlayerRootCarriesTheRemoteAdapter`: reflection over the view tree
  the way `:6815` finds focus sections — the root carries
  `PlayerRemoteAdapter`, the `.reveal` surface and the timeline carry its
  move-command variant, and **no other** view in `PlayerView` carries an
  `onMoveCommand`/`onExitCommand`/`onPlayPauseCommand` (walk the tree and
  count).
- `testPlaybackInfoRowsMatchTheSharedFieldList`: for a fixed
  `PlayerController` state per mode, collect `standardSections`/
  `debugSections` labels in order (grid rows and note labels) and compare
  to the fields fixture filtered by mode and `available_on` (apple or
  absent).
- Keep `:6956` (ring ≤ 1.5 pt) and `:7017` (mode names).

### 6.4 Acceptance

`make apple-test` green on the macOS runner (push the branch; the job is
the compiler). Then the physical script (plan §6) on an Apple TV and an
iPhone — Paul's or the device runner's, stated as unclaimed in the PR.

---

## 7. M4 — the fence and the fold

Branch `agent/pic-m4-fence`.

1. `scripts/player-input-fence` (Python, `#!/usr/bin/env python3`, no
   dependencies): grep `clients/**/*.{kt,swift}` and
   `crates/plurxd/src/web/index.html` for the tokens `KEYCODE_`,
   `onMoveCommand`, `onExitCommand`, `onPlayPauseCommand`,
   `addEventListener("keydown"`, `e.key`; allow
   `clients/android/app/src/main/java/tv/plurx/app/player/PlayerKeyAdapter.kt`,
   `clients/android/app/src/main/java/tv/plurx/app/ui/AuthScreens.kt`
   (select-to-edit — reason in the script),
   `clients/apple/Sources/PlayerRemoteAdapter.swift`, and in
   `index.html` only lines between `player-input-adapter:begin/end` plus
   the three listed non-player handlers (`:3918` QR dialog, `:10798`
   lightbox, the catalog/theater poster keys at `:13807`/`:14892`,
   `reader.js`/`offline-reader.js` — each with a reason). Also assert
   `build.gradle.kts` and `project.yml` still reference both fixture
   files. Exit 1 with `file:line` per offender.
2. `validation/points.toml`: the `[[checks]]` block from plan §5; add
   `"player-input-fence"` to the `checks` arrays of `playback.pipeline`
   (`:590` region) and `web.ui` (`:683` region), and the script path to
   `web.ui`'s `paths`.
3. Options fold (default ruling): web `⏭ Auto-skip`, `▶ Autoplay`,
   `⇄ Sync` and Apple's `autoplay` button become entries of a `settings`
   menu (`⚙`) after `quality` in the transport row; Android's
   `PlayerSettings` panel is the model and keeps its shape. Row order per
   contract §3 on all three; update the three DOM/semantics/plan tests.
4. Docs in the same PR: [PLAYBACK.md](PLAYBACK.md) §"Method" field
   (`:1281-1300`, stale) and [CLIENTS.md](CLIENTS.md) link the contract;
   `docs/STATUS.html` "Native clients" tile flips the item to done with
   the PR numbers; CHANGELOG entries for M1–M4 under Unreleased.

**Acceptance:** `make validate-staged` red on a scratch commit that adds
`KEYCODE_DPAD_LEFT` to `PlayerScreen.kt`, green on the lane head.

---

## 8. Guardrails — read before the first commit

- **Never edit a fixture to make a client fit.** Flag it (§1.1).
- **Never add a fourth answer to "what does this key do".** Every
  handler goes through the adapter and the reducer; a panel that needs a
  key gets a state.
- **Never send focus to an invisible element.** `hide` is the only
  outcome that focuses the surface, and only while the surface is what is
  showing.
- **Do not touch the server's seek path** (`seekTo`, session reopen,
  coalescing, VOD immutable timelines). The contract changes *when* a
  seek is issued, never what one does.
- **Build numbers via the bump targets only.** `validation/doc_versions.py`
  sweeps every unmarked build mention in `docs/STATUS.html`, both client
  READMEs and the parity docs; a hand edit reds the preflight job every
  other job `needs:`.
- **Commit subjects:** `validation/history.py:21-34` (`ISSUE_RE`) treats
  a subject starting with `fix`/`normalize`/`route`/`hide`/`align`/… or
  containing `wrong`, `stale`, `broken`, `crash`, `prevent`, `keep`,
  `remove`, `restore`, `bound`, `avoid`, `correct`, `preserve`, `refuse`,
  `recover`, `fallback`, `invalid`, `compatible`, `truth` as corrective
  and then demands ledger evidence. Describe the change, not the defect:
  `player(web): timeline row with preview-then-commit and the input adapter`.
- **The web tests must execute the shipped code.** Slice functions out
  of `index.html` and run them (the `web-policy.test.js` harness); a test
  that reads the JS as text proves nothing — the playback-control web
  plane shipped with a runtime error for that reason.
- **Tests pin the call site.** The Compose and reflection tests in §5.3
  and §6.3 exist because a reducer nobody calls is green forever.
- **Do not merge. Do not deploy.** Open the PR into the lane, wait for
  green, report the head SHA and what remains unclaimed on hardware.
- **Your clone, not Paul's checkout.** Clone `pjunod/plurx` yourself and
  work there; never run git in `~/code/plurx`.
- **Docs change with behaviour, same commit.** The contract doc's
  generated blocks are regenerated by `scripts/player-contract-table
  --write` (and `--embed` after M1 §4.1); the test fails otherwise.

---

## 9. Definition of done

M1–M4 merged to `effort/player-input-contract`, the lane merged to `main`
by Paul, and:

- `node tests/playback/player-input-contract.test.js` and every
  reducer test (web, JUnit, XCTest) green — each asserting every fixture
  row;
- `scripts/player-input-fence` in `make validate-staged`, green on `main`,
  red on the synthetic offender;
- the physical script (plan §6) run on an Apple TV, a Google TV / Shield,
  a phone of each kind, with PASS per step recorded in
  `docs/STATUS.html` — or explicitly listed as unclaimed;
- the audit's five mechanisms each pointing at a merged PR in
  [UI-NAVIGATION-AUDIT.md](UI-NAVIGATION-AUDIT.md) §7 (add a "closed by"
  line per mechanism in the M4 PR).

What "done" does not include: spatial navigation for TV browsers, bar
thumbnails, playback speed, and the M5 non-player items — each has its
own line in the plan.
