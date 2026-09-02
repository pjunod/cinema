# Player input contract — implementation plan

**Status:** ready to build, M0 landed with this document · **Executes:**
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) against the findings in
[UI-NAVIGATION-AUDIT.md](UI-NAVIGATION-AUDIT.md) · **Written:** 2026-09-02 ·
**Anchors:** `file:line` at commit `18886477` unless stated; re-verify with
`git show 18886477:<path>` and against `main` at build time.

Read the contract first, then the audit's §1 (the five mechanisms) and the
per-client section for the client you are building. Work one milestone at a
time, in order — M1 (web) first because it runs in CI without hardware and
sets the test pattern the native clients copy. Each milestone is its own PR
into the `effort/player-input-contract` lane; the lane's PR into `main` gets
the one full qualification run ([DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md)).
If a step seems to require changing a fixture row, stop and flag it: the
fixture is the ruling, and a client that cannot implement a row is a finding
about the row, not a licence to make the client special.

---

## 1. Objective

After this plan, the question "what does Right do right now?" has one
answer per state on every client, that answer is a row in
[`tests/playback/player-input-contract.json`](../tests/playback/player-input-contract.json),
each client's reducer is tested against every row, and no key code exists
outside each client's one adapter file. The three reports that started
this — the unusable seek bar, the seek that fires while navigating, the
player and its info panel that differ per client — are closed by
construction rather than by patch.

Success is measurable: the audit's mechanisms M1–M5 each map to fixture
rows (M1 → `hidden/*direction → reveal`; M2 → `timeline`/`scrub` rows and
the row grammar; M3 → `timings` and the `idle` column; M4 → the `back`
column; M5 → `playback-info-fields.json`), and each milestone's acceptance
runs the rows.

---

## 2. Contract — what already exists that the work extends

Re-verify every anchor at build time.

**Apple.** `TVPlayerRemoteRouting.moveOutcome(focusedControl:progressEngaged:direction:progressRightNeighbor:markerAvailable:) -> PlayerRemoteMoveOutcome`
(`clients/apple/Sources/PlayerView.swift:77-107`), outcomes
`.seek(seconds:) | .focus(PlayerControl) | .ignore` (`:71-75`), applied by
`applyRemoteMoveOutcome` (`:1208-1226`). Installed on two views: the hidden
`.reveal` surface (`:761-776`) and `tvProgressBar` (`:1894-1958`). Auto-hide
predicate `shouldAutoHideControls(visible:scrubbing:changingStream:optionMenuOpen:tearingDown:)`
(`:1048-1056`), delay `controlAutoHideDelayNanoseconds` (`:682`), driver
`.task(id: autoHideGeneration)` (`:908-926`). `hideControls()` (`:1149-1166`),
`revealControlsFromRemote()` (`:1178-1184`). Menu precedence `.onExitCommand`
(`:961-972`). `onPlayPauseCommand` only at `:771`. `PlayerControl` enum
(`:7-22`). Tests to invert or extend: `AppleClientTests.swift:2269-2357`
(step sizes, engage routing, hidden seeking, up/down routing),
`:5498-5540` (auto-hide), `:6796-6815` (1.5 pt focus ring — keep the ring
compact but the test must allow a visible pending-preview state).

**Android.** `playerBackAction(panelOpen:controlsVisible:)` and
`playerSeekDeltaMs(keyCode:controlsVisible:)`
(`clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt:322-341`),
`HiddenSeekAccumulator` (`:343-355`), the surface `onPreviewKeyEvent`
(`:972-1012`), `Controls` (`:1234-1392`), `TransportButtons` (`:1404-1444`),
`PlaybackPositionSlider` (`:1446-1482`), auto-hide effect (`:918-923`), focus
hand-off on hide (`:953-957`), `RequestInitialFocus`
(`ui/components/TvFocus.kt:44-56`, throws on an unattached requester),
panels `TrackMenu` (`Controller.kt:1602-1743`), `PlayerSettings`
(`PlayerScreen.kt:1493-1548`), `PlaybackInfoOverlay` (`:1655-1729`),
`PlayerPanelSurface` (`:2604-2629`). Tests: `PlayerPolicyTest.kt:12-39`,
`androidTest/.../PlayerControlsFocusTest.kt:21-89`, `ShelfFocusTest.kt`
(the pattern for driving real key events). Offline player:
`OfflinePlayerScreen.kt:87`.

**Web.** `PlaybackPolicy.seekDeltaSeconds(key)`
(`crates/plurxd/src/web/playback-policy.js:747-760`), the player `keydown`
(`crates/plurxd/src/web/index.html:10776-10789`), `pbWireSeek()`
(`:9046-9065`), `nudge()` (`:8992-9002`), `playerActivity()` (`:8865-8877`),
`play()` (`:7236-7266`), `closePlayer()` (`:10720-10765`), `toggleMenu()`
(`:9575-9627`), `toggleStats()` (`:9999-10018`), the player DOM (`:3039`,
one line), idle CSS (`:320`, `:335`, `:339`, `:364`, `:377`). Tests:
`tests/playback/web-policy.test.js` (slices shipped functions out of
`index.html` and executes them — copy that harness), `web-control.test.js`.

**Fixtures.** `tests/playback/player-input-contract.json` — `surfaces`,
`timings`, `steps`, `controls`, `inputs`, `states`, `outcomes`,
`routing[surface][state][input]`; and `tests/playback/playback-info-fields.json`
— `modes`, `sections`, `formats`, `health_pill`, `fields[]` with `id`,
`label`, `section`, `modes`, `format`, optional `placement: notes`,
`available_on`, `always`. `scripts/player-contract-table` renders §2 and §7
of the contract doc; `tests/playback/player-input-contract.test.js` guards
all of it.

**Info panel builders.** Apple `standardSections`/`debugSections`
(`PlayerView.swift:3161-3644`; tvOS Standard `:3392-3512`, iOS Standard
`:3514-3644`; `PlaybackLedgerRow`/`Section` are `private`, `:2253-2401`),
Mini `:2496-2569`, pill `:2586-2602`; Android `PlaybackInfoDetails` builders
(`PlayerScreen.kt:2197-2380`), Mini `:1733-1795`, overlay `:1655-1729`,
selector `:2034-2055`; web `updateStats` (`index.html:10443-10664`), Mini
`:10621-10630`, `setStatsMode`/`toggleStats` (`:9976-10018`), formatters
`fmtMbps`/`fmtBytes` (`:4800-4801`).

**Build claims.** Any client change ships with the build bump the
five-surface contract demands: `make apple-build-bump` / the Android
equivalent, never a hand edit (see [RELEASING.md](RELEASING.md) and
`validation/mobile_versions.py`).

---

## 3. Milestones

### M0 — the contract exists (this PR)

Both fixtures, the generator, the test wired into `make web-check`, the
three docs, README reading-path links. No client behaviour changes.

**Acceptance:**

```bash
node tests/playback/player-input-contract.test.js   # 6 ok lines
make web-check                                      # green, includes the above
scripts/player-contract-table | diff - <(sed -n '/contract:routing:begin/,/contract:routing:end/p' docs/PLAYER-INPUT-CONTRACT.md)  # empty
```

### M1 — web conforms

1. **Reducer.** Add `PlaybackPolicy.routeInput(surface, state, input)` to
   `playback-policy.js`, a table lookup over the fixture's `routing`
   embedded at build time (the module is UMD and served; embed the
   `routing` object, and have `web-policy.test.js` assert it equals the
   fixture so the served copy cannot drift). Keep `seekDeltaSeconds` for
   the desktop body hotkeys but drop `ArrowUp`/`ArrowDown` (`:747-760`).
2. **State.** A `playerInputState()` that derives `hidden | transport |
   timeline | scrub | menu | info | failed` from `#player.idle`,
   `document.activeElement`, `PLAYER._seekPreview`, `#pmenu.on`,
   `#statsov` mode, and the loading/failure overlay. One function; the
   test slices and executes it.
3. **Adapter.** The `window` `keydown` at `:10776` becomes: guard modal
   open → `typing` guard → map `e` to a contract input (Escape→`back`,
   Enter/Space-on-timeline→`select`, Space/K→`play_pause`, J/L→
   `skip_back`/`skip_forward`, arrows→directions; **check
   `e.ctrlKey||e.metaKey||e.altKey` first and pass through**) → `routeInput`
   → apply. `i`, `f`, `c` stay as hotkeys but are dispatched from the same
   place. Remove the "(a)" hint or add the handler (`:3039`).
4. **Timeline row.** Move `#ptcur · #pseek · #ptdur` out of `#ptransport`
   into a `#ptimeline` row above it (`:3039`); CSS so the bar spans the
   row. Tab order becomes timeline → transport, which is DOM order.
5. **Preview/commit on the focused bar.** `pbWireSeek` gains: arrows →
   `PLAYER._seekPreview` moves by the acceleration ladder (count repeats
   per key hold; `e.repeat` is the signal); Enter/Space → `seekTo(preview)`;
   Escape → clear preview; `blur` → clear preview. `pointerdown` drops
   `preventDefault()` so a click focuses the bar (`:9051`) — use
   `setPointerCapture` alone to keep the drag.
6. **Focus and dialog semantics.** `play()` sets `#app` `inert` and focuses
   `#pbplay`; `closePlayer()` removes `inert` and focuses the element that
   opened the player (store it at `play()` time; the item page re-render
   at `:10764` must run *before* the focus restore and target the same
   `.btnplay`). `#modal` gets `role="dialog" aria-modal="true"`.
7. **Menus and stats.** `toggleMenu` records the opener, moves focus into
   the first `.sel` (or first) option, traps Tab inside `#pmenu`, closes on
   Escape and on click outside, returns focus to the opener, and toggles
   `aria-expanded` on the anchor. `toggleStats` does the same for the
   panel's radios and `.statsx`. Escape precedence per contract §2.2 —
   `closePlayer()` is no longer the first thing Escape does.
8. **Hide timer.** `playerActivity()` 2600 → 4000; never idle while
   `PLAYER._seekPreview != null`, `#pmenu.on`, or stats open in
   Standard/Debug; the wake listener moves from `#player` to `window`
   while the modal is open so a Tab from anywhere re-shows the chrome;
   `.idle` additionally sets `visibility:hidden` on `.pbar`/`.ptransport`
   after the opacity transition so hidden controls cannot hold focus.
9. **Info panel.** `updateStats` builds each mode from one row list keyed
   by the fixture's `id`s (a `STATS_ROWS` table: id → builder), so Standard
   gains `Method`, `Position`, `Buffer` and `Stalls` under NOW DECODING, and
   Debug gains `Status`, `Encoder`, `Subtitles`; labels `Ahead` →
   `Server ahead` with the hold clause as a note, `Delivery` →
   `Delivery rate`, `Dropped / total` → `Frames`; `fmtBytes` becomes
   decimal SI (`:4801`); the header title reads "Playback debug" in Debug;
   `toggleStats` focuses the mode radiogroup on open and returns focus to
   `#statsbtn` on close, the radios get arrow-key movement, Escape closes
   the panel (§2.2), and the body re-renders by patching text nodes rather
   than replacing `innerHTML` (`:10665`) so a selection survives a tick.
   Retire the second `ⓘ` ambiguity: `#pbinfo` "Title info" keeps its
   synopsis job but gets a distinct glyph.
10. **Tests.** `web-policy.test.js`: `routeInput` over every fixture row for
   `desktop`; the sliced `updateStats` executed against a fixed telemetry
   shape per mode and its row labels compared to the fields fixture
   (`modes` membership, exact `label`, `available_on` includes `web`); slice-and-execute the adapter with synthetic `KeyboardEvent`s
   for each state and assert the outcome function called; DOM-order test
   over the `#player` line asserting `#ptimeline` precedes `#ptransport`
   and that `#pseek` is the only `tabindex` in its row. Update
   `tests/ui-structure.golden` if the web-layout check sees the new row.

**Acceptance:** `make web-check` and `make validate-staged` green; then in
Chrome on the M3 Max: open a remux title, Tab to the bar, Right ×3 shows
`+30 s` in `#ptcur` with no network request, Enter seeks once (one
`/sessions` reopen in the Network tab), Escape with the audio menu open
closes the menu not the player, Space immediately after clicking Play does
not restart playback.

### M2 — Android conforms

1. **Reducer.** `player/PlayerInputPolicy.kt`: `enum Surface`, `enum
   InputState`, `enum ContractInput`, `sealed class Outcome`, and
   `route(surface, state, input): Outcome` — a `when` table transcribed from
   the fixture. Delete `playerSeekDeltaMs`; keep `playerBackAction` as a
   thin call into `route`.
2. **Adapter.** `player/PlayerKeyAdapter.kt` is the only file with
   `KeyEvent.KEYCODE_*`: `fun contractInput(event: KeyEvent): ContractInput?`.
   The surface `onPreviewKeyEvent` (`:972-1012`) becomes adapter → state →
   reducer → apply. Directions while hidden return `Outcome.Reveal`, which
   re-shows `Controls` with `lastFocusedControl` restored (a
   `rememberSaveable` slot; `initial_focus` when empty). Drop
   `HiddenSeekAccumulator` and the ±30 s rows; `PlayerPolicyTest.kt:19-39`
   becomes the fixture test.
3. **Timeline row.** Replace `PlaybackPositionSlider` (Material3 `Slider`)
   with `TimelineRow`: a `Box` that is `focusable()`, draws elapsed/preview
   fill and a thumb, owns `pendingMs: Long?`; key handling is *not* in it —
   the adapter dispatches `preview/commit/cancel` and the row renders.
   Touch: `detectDragGestures` → preview while dragging, commit on end; a
   tap is preview+commit. Put it in its own row above the transport row
   with the two time labels (`Text`, unfocusable). Move the top-row buttons
   (`:1262-1284`) into the transport row after a `Spacer(weight = 1f)` so
   the row reads `⟲10 ▶ ⟳10 … 🔊 CC ◆ ⚙ ⓘ ◲` per contract §3; keep the
   narrow (<700 dp) split as the contract's two-line variant but decide
   compact-vs-wide from `currentFormFactor()` (`ui/Layout.kt:14-23`) plus
   width, so a TV never gets the phone shape.
4. **Focus properties.** `focusProperties { up = …; down = … }` between the
   three rows so `focus_transport`/`focus_marker_or_ignore` are the engine's
   own answer; the marker button becomes reachable (`up` from the
   timeline) and is composed only while chrome is visible or focused.
5. **Play/Pause everywhere.** `MEDIA_PLAY_PAUSE` handled in the adapter for
   every state (it already is on the surface; make the panel scrims not
   swallow it).
6. **Hide timer.** 3 800 → 4 000; effect keyed on `scrubbing`/`pendingMs`,
   `panel`, `isPlaying`, and the failure state; on hide store
   `lastFocusedControl`.
7. **Panels trap and return focus.** `TrackMenu`, `PlayerPanelSurface`,
   `PlaybackInfoOverlay`: wrap in `focusGroup()` with
   `focusProperties { exit = { FocusRequester.Cancel } }`, do not compose
   `Controls` beneath an open panel (or mark it `canFocus = false`),
   return focus to the opener on dismiss. One dismiss chrome: `back` and
   scrim tap; drop the per-panel ✕/ArrowBack variance or give all three
   the same one.
8. **Settings crash guard.** `PlayerSettings`: attach the requester to the
   selected row *or the first row* (`:1517-1521`) so `RequestInitialFocus`
   (`:1507`) never targets nothing; add a unit test with a stored quality
   absent from `qualityOptions`.
9. **Offline player.** `OfflinePlayerScreen` (`:87`) adopts the shared
   `Controls` with a local `PlayerController`-like adapter, or at minimum
   the same `TimelineRow` and key adapter — one player UI per app.
10. **Info panel.** `PlaybackInfoDetails` builds from the fields fixture's
    ids: add `Container`, `Stalls`, `Dynamic range` in Mini, `Encode speed`
    and `Server ahead` labels (not `Encode`/`Ahead`), `Range` → `Dynamic
    range`, `Frames` stays; `Buffer` becomes `"12.3 s"` (drop the clock +
    percent form, `:1578`); bitrate unit `Mb/s`; one Status vocabulary
    (`:2241` and `:2330` agree). Navigation: opening the panel no longer
    hides the transport (it is the `info` state — chrome stays, the timer
    holds), the root key handler routes through the reducer so the first
    Select is not swallowed (`:987-991`) and directions do not `poke()`
    the transport back under the panel (`:1002-1009`); every section card
    and the notes strip is `focusable()` so the D-pad scrolls the ledger
    (`:1904`); the mode chips re-request focus after a switch; close
    returns focus to the Info button; the mode persists in
    `ViewerPreferences`.
11. **Tests.** JUnit: `route` over every `ten-foot` and `touch` fixture row
    from `src/test/resources/player-input-contract.json` (copy; identity
    checked by §5). Instrumented: extend `PlayerControlsFocusTest` to drive
    real `pressKey`s through `Controls` (the `ShelfFocusTest` pattern) and
    assert with counting callbacks — Right on the timeline previews and
    does not call `onSeek`; CENTER commits exactly once; DOWN leaves to
    `play_pause`; Right on hidden chrome reveals and does not seek; UP from
    the timeline reaches the marker when present. `PlaybackInfoOverlayTest`
    extends to all three modes and compares the rendered labels to the
    fields fixture from `src/test/resources`.

**Acceptance:** `make android-test` (JVM + lint) green;
`./gradlew assembleDebugAndroidTest` builds; the instrumented suite passes
on the CI emulator lane; then the physical run in §6 on a phone (touch
rows) and on a Google TV / Shield (ten-foot rows).

### M3 — Apple conforms

1. **Reducer.** Extend `TVPlayerRemoteRouting` into `PlayerInputRouting`
   with `enum PlayerInputState`, `enum PlayerContractInput`, and
   `route(surface:state:input:) -> PlayerInputOutcome` covering all eleven
   inputs (today: four directions on two views). Keep the file pure —
   no SwiftUI import.
2. **Adapter.** One `PlayerRemoteAdapter` file owns every
   `onMoveCommand`, `onExitCommand`, `onPlayPauseCommand`, and the Select
   `onTapGesture`s: root-level modifiers on the player `ZStack` that map to
   contract inputs and call the reducer, replacing `seekFromRemote`
   (`:1186-1194`), the `.onExitCommand` chain (`:961-972`) and the
   surface-only `onPlayPauseCommand` (`:771`). `focus_row` outcomes return
   without handling so the system engine moves focus among buttons —
   which is what the buttons do today.
3. **Timeline row.** `tvProgressBar` moves into its own `HStack` row above
   `PlayerTrailingControlRow`/the transport row (`:1291-1301`), with the
   two capsule time labels; no button shares the row. Replace
   `tvProgressEngaged` with `pendingMs: Int?`: Left/Right set/move it (and
   the elapsed label shows it), Select commits via `controller.seek(toMs:)`,
   Menu clears it. The fill shows the pending position in a distinct
   accent while pending; the 1.5 pt focus-ring test (`:6796-6815`) keeps
   the ring but must allow the pending fill.
4. **Hidden chrome.** `.reveal` surface's directions → `reveal`, restoring
   `lastFocusedControl` (default `.playPause`); `revealControlsFromRemote`
   (`:1178-1184`) takes the restored control instead of hard-coding
   `.playPause`.
5. **Auto-hide.** `shouldAutoHideControls` gains `playing`, `failed`,
   `infoOpen` (Standard/Debug), `pendingPreview`; drop the
   `optionMenuOpen`-by-focus special case (`:1118-1128`) — a *closed* menu
   button under focus hides like any other button; an *open* system `Menu`
   is the `menu` state and holds. `hideControls()` records
   `lastFocusedControl` before clearing focus (`:1160-1164`) and never
   runs while `failed`.
6. **Back precedence** per contract §2.2; the failure view exits on one
   Menu press.
7. **iOS.** The wide row and narrow rows already match the grammar
   (`:1661-1694`); only the touch `scrub` rows apply — `Slider` already
   previews then seeks on release. Verify the "More" popover contents
   match the `settings` decision when §8 rules.
8. **Info panel.** One `standardSections` for tvOS and iOS (today two,
   `:3392-3512` vs `:3514-3644`): Standard gains `Buffer` and `Stalls`
   under NOW DECODING and a `Status` row on iOS; `Subtitles` moves out of
   SERVER (`:3634-3642`); `Output` → `Resolution`, `Buffer ahead` /
   `Server ahead` unify, tvOS Standard's `Transferred` (server bytes,
   `:3509`) becomes `Delivered` so `Transferred` keeps its Debug meaning
   (client bytes, `:3301`); bitrate formatter follows the fixture
   (`:3859-3861`); resolution loses its spaces (`:3421`); the Mini pill
   shows server state and the stall count becomes the `Stalls` row
   (`:2586-2602`). Make `PlaybackLedgerRow`/`Section` internal so a test can
   read them. Navigation: `info` is a state — auto-hide holds, focus never
   leaves the panel for the reveal surface (`:1160-1164`), Mini wires
   initial focus like the other modes (`:2767`), dismiss returns focus to
   `.stats` not `.playPause` (`:1178-1184`), iOS Debug's backdrop
   (`:2788-2800`) lets a tap outside the panel toggle the chrome like
   Standard does, and the mode persists in `UserDefaults`.
9. **Tests.** XCTest over the fixture (`ten-foot` for tvOS, `touch` for
   iOS) as a test-target resource in `project.yml`; a test that renders
   each mode's sections from a fixed `PlayerController` state and compares
   labels and mode membership to the fields fixture; invert
   `testTVHiddenControlsKeepDirectionalSeeking` (`:2312-2324`) into
   `…RevealWithoutSeeking`; drop the 30 s cases from `:2269-2274`; add
   preview/commit/cancel cases; extend `testPlayerOverlayAutoHides…`
   (`:5498-5530`) for `failed`/`infoOpen`/`paused`; a reflection test that
   the player root carries the adapter modifiers the way
   `:6655-6687` checks focus sections.

**Acceptance:** `make apple-test` green on the self-hosted macOS runner
(the only place Swift compiles — push the branch and read the job); then
the physical Apple TV run in §6.

### M4 — the fence and the fold

1. **Grep gate.** A validation check (`validation/points.toml`, profiles
   `commit`/`ci`) that fails when `KEYCODE_`, `onMoveCommand`,
   `onExitCommand`, `onPlayPauseCommand`, `addEventListener("keydown"`, or
   `e.key` appear in client files outside the allow-list:
   `clients/android/app/src/main/java/tv/plurx/app/player/PlayerKeyAdapter.kt`,
   `clients/android/app/src/main/java/tv/plurx/app/ui/AuthScreens.kt`
   (select-to-edit, documented exception),
   `clients/apple/Sources/PlayerRemoteAdapter.swift`, and the web adapter
   region of `index.html` (mark it with `// player-input-adapter:begin/end`
   and allow only inside). Existing non-player handlers (lightbox, reader,
   QR dialog, catalog/theater poster keys) are listed with a reason each.
2. **Fixture identity.** The same check asserts
   `clients/android/app/src/test/resources/player-input-contract.json` and
   the Apple test resource are byte-identical to
   `tests/playback/player-input-contract.json`.
3. **Options fold** once §8.1 of the contract is ruled: web's
   `⏭ Auto-skip`, `▶ Autoplay`, `⇄ Sync` and Apple's `autoplay` button
   become entries in a `settings` menu; Android's panel is the model. Row
   order per contract §3 on all three.
4. **Docs.** [PLAYBACK.md](PLAYBACK.md) and [CLIENTS.md](CLIENTS.md) link
   the contract where they describe seeking and the player; the
   `STATUS.html` player tile cites it.

**Acceptance:** `make validate-staged` red on a synthetic diff that adds
`KEYCODE_DPAD_LEFT` to `PlayerScreen.kt`, green on `main`; the three
fixture copies hash-equal.

### M5 — navigation outside the player

The audit §4 table, in this order: web focus restoration on route change
(the header search input first — `:3786`/`:14471` → keep `#q` out of the
`#app.innerHTML` rebuild or restore focus and caret after it); dialog
semantics and focus return for the lightbox and edit dialog; keyboard
access for classic posters and `.eprow`; Android initial focus on every
screen and `SearchScreen` adopting `AuthTextField`; Apple `DetailView`
focus re-grab limited to first appearance; the tvOS episode-card ruling.
Each item is small and independent; batch them into the lane as they land.

**Acceptance:** per item, a test in the existing suites; for the web, a
`ui-baseline` capture that opens the search field and types.

---

## 4. Guardrails

- **Do not change a fixture row to make a client easier.** Flag it. The
  test that checks the rulings (`player-input-contract.test.js`) will fail
  on the three ruled behaviours anyway.
- **Do not add a fourth place that answers "what does this key do".** If a
  panel or overlay needs a key, it goes through the adapter and gets a
  state.
- **Do not send focus to an invisible element.** The audit's worst tvOS
  finding is exactly that; `hide` is the only outcome that moves focus to
  the surface, and only while the surface is what is showing.
- **Do not touch the server's seek path.** `seekTo`, session reopen,
  coalescing, VOD immutable timelines are out of scope; the contract only
  changes when a seek is *issued*.
- **Do not hand-edit build numbers**; use the bump targets. Do not add
  tests to the deploy path. Do not merge the lane; open the PR and stop.
- **Commit subjects:** the history audit's `ISSUE_RE`
  (`validation/history.py:21-34`) treats "fix", "normalize", "wrong",
  "stale", "prevent", "keep", "remove" and friends as corrective and then
  demands ledger evidence. Describe the change ("player: timeline row with
  preview-then-commit"), not the defect.
- **Line numbers in this plan are at `18886477`.** `PlayerView.swift`
  shifted +15 to +48 lines between that commit and `main` on 2026-09-02;
  `index.html` shifted around `:3626`, `:6771`, `:9169-9226`, `:11187`.
  Re-anchor before editing.

---

## 5. The validation check (M4 §1–2), spelled out

```toml
[[checks]]
id = "player-input-fence"
title = "Key handling lives only in the player input adapters; fixture copies match"
command = "scripts/player-input-fence"
profiles = ["commit", "ci", "full", "nightly"]
requires = ["python3"]
missing = "fail"
timeout_seconds = 30
```

`scripts/player-input-fence` greps the client trees for the tokens in M4 §1,
subtracts the allow-list (each entry with a one-line reason in the script),
and compares SHA-256 of the three fixture copies. Exit 1 with the offending
`file:line` list. Add it to the `playback.pipeline` and `web.ui` points'
`checks` arrays in `validation/points.toml` (`:590-591`, `:683-685`) so a
touch to either client tree runs it under `make validate-staged`.

---

## 6. Physical acceptance — the script the device runner executes

Nothing below is claimed until it is run on hardware; CI covers simulators
and emulators only. Run after M2 (Android) and M3 (Apple) respectively, on
the installed build named in the PR, one title that transcodes and one
that remuxes.

```
Apple TV (Siri Remote) — expect
 1. Play; wait 5 s for chrome to hide; swipe right          → chrome returns, focus on ▶, film did NOT skip
 2. Swipe up twice; press Play/Pause                        → toggles every time, chrome up or not
 3. Focus ⟳10; swipe right                                  → focus lands on 🔊 (or next option), NOT on the bar
 4. Swipe up from ▶                                         → focus on the bar; elapsed label unchanged
 5. Swipe right ×3 on the bar                               → label shows +30 s, film position unchanged
 6. Hold right 3 s on the bar                               → label accelerates (10 → 30 → 60 s steps)
 7. Click                                                    → one seek to the shown time; chrome stays
 8. Swipe right ×2, press Menu                              → label snaps back, no seek, chrome stays
 9. Swipe right ×2, swipe down                              → focus on ▶, no seek
10. Open Audio, press Menu                                  → menu closes, focus on 🔊, player still open
11. Open Playback info (Standard), wait 6 s                 → chrome and panel both still visible, focus inside panel
12. Pause; wait 6 s                                          → chrome still visible
13. Force a failure (unplug the network mid-play); press Menu once → player exits
14. Skip-intro marker showing; from the bar swipe up        → focus on Skip Intro; click → skips
15. Open Playback info; swipe down through every section   → focus walks the boxes and the notes strip; nothing seeks
16. Switch Mini → Standard → Debug → Mini                   → focus stays in the panel each time; Debug title reads "Playback debug"
17. Press Menu with the panel open                          → panel closes, focus on ⓘ, chrome still up
18. Read the SERVER box on a transcode                      → Status · Encoder · Encode speed · Server ahead · Delivery rate · Delivered, in that order, with Mb/s units

Google TV / Shield (D-pad) — the same 14 steps with LEFT/RIGHT/UP/DOWN/CENTER/BACK,
plus: 15. With chrome hidden press REW then FF               → −10 s, +10 s, chrome reveals
      16. Open Settings with a stored quality the file lacks  → panel opens (no crash), focus on first row
      17. Offline title                                       → same chrome and behaviour as online
      18. Open Playback info; press CENTER once                → the ✕ (or focused chip) activates on the FIRST press
      19. Press DOWN three times in Debug                      → the ledger scrolls; the transport does NOT reappear under it
      20. Compare the Standard rows with the Apple TV's        → same labels, same order, same units for the same title

Android phone / iPhone (touch)
 1. Tap the video                                            → chrome toggles
 2. Drag the bar for 6 s while playing                       → chrome does not hide mid-drag; one seek on release
 3. Tap the bar                                              → one seek
 4. Back (Android) with the Tracks panel open                → panel closes, player stays
 5. Lock-screen Play/Pause and ±10 with chrome hidden         → all work

Report: build number, device, title, and per step PASS/FAIL with a sentence.
```

This section is the prompt to hand to the device runner verbatim, prefixed
with the branch, the build number, and which of the three blocks to run.

---

## 7. Non-goals for this plan

- A spatial-navigation engine for TV browsers (the web keeps Tab order;
  the ten-foot table applies to the focused timeline only).
- Thumbnails, chapter marks or a time bubble on the bar.
- Playback-speed control (the fixture reserves the `settings` entry; no
  client implements it).
- Any change to what the server does with a seek.
- Roku, Tizen, webOS — no client exists yet; when one does, its reducer
  starts from the fixture.
