# Player input contract — one routing table, every client obeys it

**Status:** ruled and implemented 2026-09-02 ·
**Source of truth:** [`tests/playback/player-input-contract.json`](../tests/playback/player-input-contract.json) (input) and [`tests/playback/playback-info-fields.json`](../tests/playback/playback-info-fields.json) (info panel) ·
**Kept honest by:** `tests/playback/player-input-contract.test.js` (runs in
`make web-check`) · **Built by:** [PLAYER-INPUT-CONTRACT-PLAN.md](PLAYER-INPUT-CONTRACT-PLAN.md)

Companion to [UI-NAVIGATION-AUDIT.md](UI-NAVIGATION-AUDIT.md) (what each
player does today and why it diverged) — this is *what every player must do
with a press, on every surface, from now on*. The tables in §2 are rendered
from the JSON fixture; the fixture is the contract, this page is its
reading. A client is conformant when its reducer reproduces every row of the
fixture under test ([§5](#5-how-a-client-implements-it)), not when someone
has read this page.

The one-sentence version: **a press asks a pure function "given this state
and this input, what happens?", the function's answer is a row in one table
all three clients are tested against, and nothing else in the player is
allowed to answer that question.**

---

## 1. The state machine

Seven states, eleven inputs, three surfaces. States are about *where focus
is and what is pending*, never about which platform delivered the press.

```
                 back                         idle (playing, 4 s)
  ┌──── exit ◀───────── hidden ◀───────────────────────────────┐
  │                        │ any direction / select / tap      │
  │                        ▼ reveal (restores last focus)       │
  │                    transport ──── up ────▶ timeline ◀───┐   │
  │                        ▲ ◀──── down ────      │        │   │
  │      select: activate  │                 left/right    │ back: hide
  │      back:   hide ─────┘                      ▼   select│   │
  │                                             scrub ──────┘   │
  │                                            (preview pending)│
  │      audio/subs/quality/settings ▶ menu ── back ──▶ opener  │
  │      info ▶ info ────────────────── back ──▶ info button    │
  └──────────────── failed ──── back ──▶ exit
```

The two edges that changed on 2026-09-02, and why:

- **hidden → reveal on any direction.** Previously hidden Left/Right/Up/Down
  seeked ±10/±30 s and *then* revealed. The chrome hides 4 s after your last
  press, so the same physical press meant "move focus" and then "seek" with
  no visible boundary — the mechanism behind most "navigating skips the
  film" reports (audit §1 M1). A reveal costs one press; a surprise seek
  costs your place in the film.
- **timeline → scrub → commit.** Previously Android's slider committed a
  1 %-of-film seek on every press and tvOS needed an invisible "engage"
  click before Left/Right meant anything (audit §1 M2). Now Left/Right on
  the bar always *preview* (time label and fill move, nothing is fetched),
  Select commits, Back cancels. On transcode and remux a committed seek
  reopens the server session, so preview-then-commit is also the difference
  between one reopen per intention and one per keypress.

---

## 2. Routing tables

Read a cell as *state ⨯ input → outcome*. Outcomes are defined in the
fixture's `outcomes` map and paraphrased in §2.1. A cell that says `ignore`
is a decision, not an omission: the press does nothing and focus does not
move.

<!-- contract:routing:begin -->

_Generated from [`tests/playback/player-input-contract.json`](../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

**Surface `ten-foot`** — Directional input moves focus: Siri Remote (tvOS), D-pad (Android TV / Google TV), keyboard arrows on a TV browser, and keyboard arrows on the web when the timeline has focus.

| state \ input | `left` | `right` | `up` | `down` | `select` | `back` | `play_pause` | `skip_back` | `skip_forward` | `tap_surface` | `idle` |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `hidden` | `reveal` | `reveal` | `reveal` | `reveal` | `reveal` | `exit` | `toggle_play` | `skip` | `skip` | `reveal` | `ignore` |
| `transport` | `focus_row` | `focus_row` | `focus_row` | `focus_row` | `activate` | `hide` | `toggle_play` | `skip` | `skip` | `ignore` | `hide` |
| `timeline` | `preview` | `preview` | `focus_marker_or_ignore` | `focus_transport` | `toggle_play` | `hide` | `toggle_play` | `skip` | `skip` | `ignore` | `hide` |
| `scrub` | `preview` | `preview` | `cancel_then_focus_marker_or_ignore` | `cancel_then_focus_transport` | `commit` | `cancel` | `commit_then_toggle_play` | `ignore` | `ignore` | `ignore` | `ignore` |
| `menu` | `menu_focus` | `menu_focus` | `menu_focus` | `menu_focus` | `activate` | `close_menu` | `toggle_play` | `ignore` | `ignore` | `close_menu` | `ignore` |
| `info` | `menu_focus` | `menu_focus` | `menu_focus` | `menu_focus` | `activate` | `close_info` | `toggle_play` | `ignore` | `ignore` | `close_info` | `ignore` |
| `failed` | `focus_row` | `focus_row` | `ignore` | `ignore` | `activate` | `exit` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` |

**Surface `desktop`** — Pointer plus keyboard on the web. Tab moves focus; arrows are global hotkeys except while the timeline has focus, where the ten-foot timeline rows apply verbatim.

| state \ input | `left` | `right` | `up` | `down` | `select` | `back` | `play_pause` | `skip_back` | `skip_forward` | `tap_surface` | `idle` |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `hidden` | `skip` | `skip` | `ignore` | `ignore` | `ignore` | `exit` | `toggle_play` | `skip` | `skip` | `toggle_play` | `ignore` |
| `transport` | `skip` | `skip` | `ignore` | `ignore` | `activate` | `exit` | `toggle_play` | `skip` | `skip` | `toggle_play` | `hide` |
| `timeline` | `preview` | `preview` | `ignore` | `ignore` | `toggle_play` | `exit` | `toggle_play` | `skip` | `skip` | `toggle_play` | `hide` |
| `scrub` | `preview` | `preview` | `ignore` | `ignore` | `commit` | `cancel` | `commit_then_toggle_play` | `ignore` | `ignore` | `ignore` | `ignore` |
| `menu` | `menu_focus` | `menu_focus` | `menu_focus` | `menu_focus` | `activate` | `close_menu` | `toggle_play` | `ignore` | `ignore` | `close_menu` | `ignore` |
| `info` | `menu_focus` | `menu_focus` | `menu_focus` | `menu_focus` | `activate` | `close_info` | `toggle_play` | `ignore` | `ignore` | `close_info` | `ignore` |
| `failed` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `exit` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` |

**Surface `touch`** — Direct manipulation: iPhone/iPad and Android phones/tablets. Focus states do not apply; the pointer rows below are the whole contract.

| state \ input | `left` | `right` | `up` | `down` | `select` | `back` | `play_pause` | `skip_back` | `skip_forward` | `tap_surface` | `idle` |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `hidden` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` | `exit` | `toggle_play` | `skip` | `skip` | `toggle_chrome` | `ignore` |
| `transport` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `hide` | `toggle_play` | `skip` | `skip` | `toggle_chrome` | `hide` |
| `timeline` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` | `hide` | `toggle_play` | `skip` | `skip` | `toggle_chrome` | `hide` |
| `scrub` | `ignore` | `ignore` | `ignore` | `ignore` | `commit` | `cancel` | `commit_then_toggle_play` | `ignore` | `ignore` | `ignore` | `ignore` |
| `menu` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `close_menu` | `toggle_play` | `ignore` | `ignore` | `close_menu` | `ignore` |
| `info` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `close_info` | `toggle_play` | `ignore` | `ignore` | `close_info` | `ignore` |
| `failed` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `exit` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` |

<!-- contract:routing:end -->

### 2.1 What the outcomes mean

<!-- contract:outcomes:begin -->

_Generated from [`tests/playback/player-input-contract.json`](../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

| Outcome | What the client does |
|---|---|
| `reveal` | Show chrome; focus the last focused control (initially `play_pause`). No seek. |
| `focus_row` | Move focus within the current row or to the adjacent row per `controls.rows` order; the platform focus engine does this, the reducer only names the target row. |
| `focus_marker_or_ignore` | Focus `skip_marker` if present, else nothing. |
| `focus_transport` | Move focus to the transport row, restoring the last transport control (initially `play_pause`). |
| `activate` | Press the focused control. |
| `toggle_play` | Play/pause. Chrome reveals if hidden and the hide timer restarts. |
| `skip` | Immediate relative seek of ±`steps.skip_seconds`. Chrome reveals if hidden. |
| `preview` | Enter or continue `scrub`: move the pending position by the acceleration ladder; no seek, no network. |
| `commit` | Seek to the pending position; return to `timeline` with chrome visible. |
| `cancel` | Discard the pending position; return to `timeline`. |
| `cancel_then_focus_transport` | `cancel`, then `focus_transport`. |
| `cancel_then_focus_marker_or_ignore` | `cancel`, then `focus_marker_or_ignore`. |
| `commit_then_toggle_play` | `commit`, then `toggle_play`. |
| `close_menu` | Close the open menu; focus returns to the control that opened it. |
| `close_info` | Close the info panel; focus returns to `info`. |
| `menu_focus` | Focus stays inside the menu (trapped) — the platform moves it among the menu's own items. |
| `hide` | Hide chrome; remember the focused control; focus the surface. |
| `exit` | Leave the player. |
| `toggle_chrome` | Touch: hide chrome if visible, else reveal. |
| `ignore` | Nothing happens; focus does not move. |

<!-- contract:outcomes:end -->

### 2.2 Precedence of `back`

`back` (Menu · BACK · Escape — a key, never the ✕) resolves by state, so it
is never ambiguous which thing closes:

```
 scrub?  ── yes ──▶ cancel (stay on the timeline)
   │ no
 menu?   ── yes ──▶ close_menu (focus → opener)
   │ no
 info?   ── yes ──▶ close_info (focus → info button)
   │ no
 chrome visible?  ── yes ──▶ ten-foot/touch: hide · desktop: exit
   │ no
   ▼
  exit
```

Desktop exits instead of hiding because a pointer user has no Menu-hide
idiom and the chrome hides itself; a keyboard user who wants the chrome
gone waits 4 s. A failed player exits on the first `back` on every surface
— today tvOS needs two.

**The ✕ is `close`, not `back`.** The iOS ✕, the Android phone's back arrow
and the web's `✕ Close` are the `close` control of the `bar` row, and a
button whose only purpose is to leave cannot share a table with a key that
hides. Routed through `back`, the iOS ✕ answered `hide` in `transport` — the
state it is tapped from — `close_info` in `info` and `cancel` in `scrub`, and
exited in no state a viewer could reach it in, so the only way out of a film
was to force-quit the app (Paul, 2026-09-03). `close_control` in the fixture
gives it its own rows: close whatever is open, then `exit`, in every state.

```
 scrub → cancel, exit · menu → close_menu, exit · info → close_info, exit
 hidden · transport · timeline · failed → exit
```

Each native reducer transcribes it (`PlayerInputRouting.closeSteps`,
`PlayerInputPolicy.closeSteps`), each client suite checks the transcription
against the fixture, and each suite pins its call site: the iOS ✕ and the
failure view's Close run `closePlayer()`, the Android arrow walks
`closeSteps`, and neither manufactures a `back` press. The `menu` and `info`
rows say what `close` does when it is reached; a client's own modal may
take the tap first — on iOS the info panel's backdrop lies over the ✕, so
that tap is `info × tap_surface` (`close_info`) and the ✕ exits on the next
one, and Android composes no arrow while a panel is open.

---

## 3. Controls — the row grammar

Three rows, top to bottom. The timeline row contains exactly one focusable
thing. That single constraint is what makes horizontal input on the bar
safe to mean "scrub": there is nothing beside it to be unreachable.

```
 ten-foot / wide touch / desktop                 narrow touch
 ┌────────────────────────────────────┐          ┌──────────────────────┐
 │ title · badges · context           │          │ title · badges       │
 │                        [Skip intro]│  marker  │            [Skip …]  │
 │ 0:42:10 ━━━━━━━━●──────── 1:59:00  │  timeline│ 0:42 ━━━●──── 1:59   │
 │ ⟲10  ▶  ⟳10        🔊 CC ◆ ⚙ ⓘ ◲ ✕ │  transport│   ⟲10   ▶   ⟳10      │
 └────────────────────────────────────┘          │ 🔊  CC  ◆  ⚙  ⓘ  ◲ ✕ │
                                                 └──────────────────────┘
```

<!-- contract:rows:begin -->

_Generated from [`tests/playback/player-input-contract.json`](../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

| Row | Items in order | Surfaces | Focusable | Notes |
|---|---|---|---|---|
| `bar` | `airplay` · `close` | `desktop` · `touch` | all | A corner strip over the picture, not a row in the chrome: every client that has a Close puts it there — top-trailing on the web, top-leading on iOS and on Android, where a television cannot focus it at all. `close` is last here for the same reason it was last in the transport row: the control that ends the session is the one you should not land on by accident. Nothing here is a horizontal neighbour of a transport button. `close` when touch and desktop only — a ten-foot player exits with `back` · `airplay` when web, and only while the browser reports an AirPlay target |
| `marker` | `skip_marker` | all | all | Present only while a skip-intro/credits marker is active. Sits above the timeline row, right-aligned. Reachable by `up` from the timeline; never a horizontal neighbour of anything. |
| `timeline` | `time_elapsed` · `timeline` · `time_total` | all | `timeline` | Its own full-width row. The timeline is never a horizontal neighbour of a button — Left/Right on it belong to scrubbing, so a button beside it would be unreachable without a seek. |
| `transport` | `skip_back` · `play_pause` · `skip_forward` · spacer · `audio` · `subtitles` · `quality` · `settings` · `info` · `title_info` · `pip` · `fullscreen` | all | all | One row on ten-foot and on wide touch/desktop. Narrow touch splits it after `spacer` into a transport line and an options line; order within each line is unchanged. `surface_placement` names the one control a surface renders somewhere else: the web puts `info` in the `bar` row, where its ✕ already lives, and the native clients keep it here. `audio` when more than one audio track · `subtitles` when at least one subtitle track · `quality` when server ladder has rungs · `pip` when platform reports picture-in-picture possible (never tvOS) · `title_info` when web only — the native clients put title and synopsis on the detail screen · `fullscreen` when web only — a native player is already full screen `desktop` renders `info` at position 0 of the `bar` row |

**`settings` holds:** `autoplay_next` · `auto_skip` · `audio_sync` · `playback_speed_reserved`.

<!-- contract:rows:end -->

Narrow touch splits the transport row at the spacer into two lines; order
within each line is unchanged. Nothing else moves between surfaces.

These are preferences, not moment-to-moment controls; today they are
scattered as top-level buttons on the web (`⏭ Auto-skip`, `▶ Autoplay`,
`⇄ Sync`), a top-level `autoplay` button on Apple, and Android's "Playback
settings" panel — Android's shape is the one kept. Paul ruled the
recommended default: web and Apple fold those preferences into `settings`,
and no preference remains a transport-row peer.

**Focus memory.** `initial_focus` is `play_pause`. `reveal` restores the
last focused control; `focus_transport` restores the last *transport*
control. Closing a menu or the info panel returns focus to whatever opened
it. Focus is never sent to an invisible element: a hidden surface takes
focus only while chrome is hidden, and the failure view's buttons keep it
until `back`.

---

## 4. Timings and steps

<!-- contract:timings:begin -->

_Generated from [`tests/playback/player-input-contract.json`](../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

| Name | Value |
|---|---|
| `hide_after_ms` | 4000 |
| `hidden_only_while_playing` | true |
| `preview_auto_commit_ms` | none |
| `desktop_hotkey_coalesce_ms` | 350 |
| `steps.skip_seconds` | 10 |
| `steps.preview_acceleration` | 10 s from repeat 0 · 30 s from repeat 5 · 60 s from repeat 10 |
| `steps.vertical_seek_seconds` | none |

- hide_after_ms: chrome hides this long after the last input while playing. Never while paused, failed, scrubbing, or with a menu or the info panel open.
- preview_auto_commit_ms is null: a pending preview commits only on `select` (ten-foot) or pointer release (touch); it never commits on a timer.
- desktop_hotkey_coalesce_ms: arrow hotkeys on the desktop body accumulate against one frozen base and issue one seek after this much quiet — the shipped web `nudge()` behaviour, kept.

<!-- contract:timings:end -->

Why these values: Apple already used 4 s for the hide, while Android's 3.8 s
and the web's 2.6 s were independent guesses — one number, long enough to
read a row. A paused film with no chrome is a black frame with no
affordance, so the hide runs only while playing. The preview ladder replaces
the ±30 s vertical seek with something a held key discovers by itself.

---

## 5. How a client implements it

Three layers, and the middle one is the only one allowed to think.

```
 platform event ──▶ ADAPTER ──▶ REDUCER ──▶ OUTCOME ──▶ VIEW applies it
 (KeyEvent,         maps to      pure:                   focus moves,
  UIPress,          (state,      (state, input)          seeks, timers,
  KeyboardEvent)    input)       → outcome               chrome
```

- **Adapter** — the only file per client that may mention a key code,
  `MoveCommandDirection`, `KeyEvent.KEYCODE_*`, or `e.key`. It turns the
  platform event into one of the eleven contract inputs and reads the
  current state off the view model. It is deliberately boring.
- **Reducer** — one pure function per client, no platform imports,
  signature-equivalent to `route(surface, state, input) → outcome`. Apple
  extends `TVPlayerRemoteRouting` (`PlayerView.swift:77-107` at
  `18886477`) into `PlayerInputRouting`; Android extends
  `playerSeekDeltaMs`/`playerBackAction` (`PlayerScreen.kt:322-341`) into
  `PlayerInputPolicy`; the web extends `PlaybackPolicy.seekDeltaSeconds`
  (`playback-policy.js:747-760`) into `PlaybackPolicy.routeInput`. Each is
  tested against the fixture, row for row.
- **View** — applies outcomes. It may own *how* focus moves within a row
  (that is the platform's engine) but not *whether* a press seeks.

The rule that keeps the moles from coming back: **no key code outside the
adapter.** `scripts/player-input-fence` runs from the pre-commit hook and
from `make validate-staged` on any diff that touches a client or the web —
it hangs off `apple.client`, `android.client`, `playback.pipeline` and
`web.experience` — and it fails on `KEYCODE_`, `onMoveCommand`,
`onExitCommand`, `onPlayPauseCommand`, `onKeyEvent`/`onPreviewKeyEvent`, a
Compose `Key.` constant, `pressesBegan`/`UIKeyCommand`/`onKeyPress`,
`MPRemoteCommandCenter`, a `keydown`/`keyup` listener, an `onkeydown`
attribute, or `event.key`/`event.keyCode` in any shipped client file other
than the named adapter. Allowed regions are named with a reason beside
their anchors; test trees are not scanned, because driving real key events
is how a view is proved to consume its reducer. A new behaviour has to go
through the table, where the test will ask what the other two clients do.

Per-surface adapters map inputs as follows; anything not listed is
`ignore`d by the adapter before it reaches the reducer.

| Contract input | tvOS (Siri Remote) | Android TV (D-pad) | iOS / Android touch | Web |
|---|---|---|---|---|
| `left`/`right`/`up`/`down` | `onMoveCommand` directions | `KEYCODE_DPAD_*` | — | Arrow keys |
| `select` | Select (`onTapGesture` on the focused view) | `DPAD_CENTER`/`ENTER` | tap on a control | Enter; Space when the timeline is focused |
| `back` | Menu (`onExitCommand`) | `BACK` | system back (Android); iOS has no producer | Escape |
| `close` (`close_control`, not a table input) | — | — | ✕ (iOS) / back arrow (Android phone) | `✕ Close` |
| `play_pause` | Play/Pause (`onPlayPauseCommand`, **on every state's root**, not only the hidden surface) | `MEDIA_PLAY_PAUSE` | lock-screen / headset commands | Space or K when the timeline is not focused |
| `skip_back`/`skip_forward` | — (no producer) | `MEDIA_REWIND`/`MEDIA_FAST_FORWARD` | remote-command skips | J / L |
| `tap_surface` | — | — | tap on the video | click on the video |
| `idle` | hide timer | hide timer | hide timer | hide timer |

---

## 6. How it is enforced

1. **The fixture is well-formed and the doc matches it.**
   `tests/playback/player-input-contract.test.js` checks every surface ×
   state × input has one defined outcome, that the three rulings are
   encoded (hidden directions reveal; timeline is preview/commit/cancel;
   nothing vertical seeks; Play/Pause works with chrome up; no auto-hide
   under scrub/menu/info/failed), and that §2, §2.1, §3's row table, §4 and
   §7 of this page are byte-identical to `scripts/player-contract-table`'s
   rendering. Change the fixture, run `scripts/player-contract-table --write
   --embed`, commit all of it. The test runs in the `preflight` job of both
   pull-request workflows, and a fixture-only diff also selects both native
   client suites and the web layout lane.
2. **Each reducer reproduces the fixture.** Web: `web-policy.test.js` loads
   the JSON and asserts `routeInput` over every row. Android: a JUnit test
   whose resource directory *is* `tests/playback/`
   (`app/build.gradle.kts`). Apple: an XCTest over the same files, added to
   both test targets as resources in `project.yml`. There are no copies to
   keep in step — every client reads the one fixture. The numbers are read
   too: the web takes `hide_after_ms`, `skip_seconds` and the coalesce
   window from the generated embed, and Apple asserts
   `controlAutoHideDelayNanoseconds` against `timings.hide_after_ms`.
3. **Each view consumes its reducer.** A reducer nobody calls proves
   nothing — this repo has already had seven production lines revert green
   because a test pinned an extracted helper instead of the call site.
   Android's Compose test
   drives real key events through `Controls` and asserts outcomes;
   Apple's test reflects on the view tree the way
   `testTVSeriesAndSeasonShelvesAcceptDirectionalFocusFromTheirHeaders`
   already does; the web's `web-policy.test.js` slices the shipped
   `keydown` handler out of `index.html` and executes it, the pattern the
   stall-counter tests use.
4. **The row grammar is a DOM/semantics fact, not a screenshot.** Web: a
   test reads the `#player` line of `index.html` and asserts the id order
   per row. Android: a Compose semantics test asserts the sibling order and
   that only the timeline is focusable in its row. Apple has no equivalent
   yet — an earlier draft of this section named a `PlayerControlPlan` type
   that was never built, and the shipped test only substring-orders the
   options group.

   The web used to diverge here and the divergence is now written down
   rather than argued with: `close` is in no client's transport row (the web
   puts it top-trailing, iOS and Android top-leading, a television has none
   and exits with `back`), so it moved to a `bar` row; `title_info`,
   `fullscreen` and `airplay` exist only on the web and are named as such;
   and the one control a surface genuinely relocates — desktop's `info`,
   which sits beside the ✕ rather than in the button row — is
   `surface_placement` on the transport row, so `player-dom.test.js` derives
   the expected order for BOTH rows from the fixture. Paul ruled it this way
   on 2026-09-02: describe what each surface does, rather than move two
   buttons on the web to satisfy a row grammar written for a remote.
5. **No key code outside the adapter** — the grep gate in §5.
6. **The playback-info panel shows the fixture's rows.** Each client's
   panel is built from a row list the test compares, label for label and
   mode for mode, against
   [`tests/playback/playback-info-fields.json`](../tests/playback/playback-info-fields.json)
   (§7). Android already has a section/row builder and an instrumented test
   of its Standard labels; Apple's ledger types become non-private so the
   same test can exist; the web's `updateStats` builders are sliced and
   executed like the rest of the shipped JS.

---

## 7. The playback-info panel — one row list, three modes, three clients

The panel is the `info` state of §2 for navigation — `back` closes it and
returns focus to the `info` button, focus is trapped inside it, the chrome
never hides under it, and on ten-foot every section box and the notes strip
is focusable so the remote can scroll it. What it *shows* is this row list.
The audit found the three clients agreeing only on the three mode names:
Android never shows the container, Apple's Standard has no buffer row and
files subtitles under SERVER on iOS, web Debug drops Status, Encoder and
Subtitles, the same datum is labelled `Ahead` / `Buffer ahead` /
`Server ahead` depending on the screen, `Transferred` means server bytes on
one Apple screen and client bytes on another, bitrates are spelled three
ways and bytes are 1024-based on one client only, and the health pill
counts stalls on Apple but shows server state elsewhere.

**Modes.** `mini` is one line — method · position · resolution and range
chip · buffer · delivery rate · health pill — with the mode selector and
close inline at its right end and the transport live beneath it.
`standard` and `debug` are the top-right two-column ledger already shipped
(PLAYBACK · SOURCE · NOW DECODING left, NETWORK · SERVER right, sentence
values in the NOTES strip), `debug` titled "Playback debug". The ledger's
placement rule stands: a row's column is a property of the row, never of
its value.

**Mini ⊂ Standard ⊂ Debug.** A row never appears in the small panel and
vanishes from the big one; the test enforces it.

**Formats are part of the row.** Bitrate is `12.3 Mb/s` / `800 kb/s`, bytes
are decimal SI, position is `1:02:03 / 2:03:04`, resolution is `3840×2160`,
lists join with ` · ` in every mode. The fixture's `formats` map is the
spelling; a client with a different formatter fixes the formatter, not the
fixture.

**Unavailable is omitted, not dashed.** A row the platform cannot supply
(`available_on`) is left out. A row marked `always` shows `—` instead,
because its absence would itself be information (Build, AV offset, Status).
A row the platform *can* supply and does not is a parity defect.

**The health pill means server state** on every client — Direct · Active ·
Held · VOD · Filling · Complete · Failed. Apple's stall count moves into the
`Stalls` row where the other two already keep theirs.

**Mode persistence.** The chosen mode persists per client the way the web's
already does (`plurx_stats_mode`); Apple and Android reset to Standard on
every player instance today.

<!-- contract:info:begin -->

_Generated from [`tests/playback/playback-info-fields.json`](../tests/playback/playback-info-fields.json) by `scripts/player-contract-table`; do not edit by hand._

**PLAYBACK**

| Row | mini | standard | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|
| `Method` | ✓ | ✓ | ✓ | text | grid | all | The delivery verdict — the same vocabulary on every client: Direct play · Remux · Transcode · Transcode · cached; the encoder and rung follow as a clause ("Transcode · nvenc · 1080p"). |
| `Playback mode` | – | – | ✓ | text | grid | web | Live HLS / VOD HLS / progressive — the web presentation kind. Native players have one presentation. |
| `Position` | ✓ | ✓ | ✓ | position | grid | all |  |
| `Reason` | – | ✓ | ✓ | list | notes | all | Why the server chose this method. |
| `Build` | – | – | ✓ | text | grid | all | always shown |
| `Transport` | – | – | ✓ | text | notes | all |  |
| `File ID` | – | – | ✓ | text | grid | all |  |
| `Session` | – | – | ✓ | text | notes | all |  |
| `Switched` | – | – | ✓ | list | notes | web | Auto-quality moves this session. |

**SOURCE**

| Row | mini | standard | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|
| `Video` | – | ✓ | ✓ | list | notes | all | codec · profile · bit depth · HDR format. |
| `Resolution` | – | ✓ | ✓ | resolution | grid | all |  |
| `Bitrate` | – | ✓ | ✓ | bitrate | grid | all |  |
| `Container` | – | ✓ | ✓ | text | grid | all |  |
| `Audio` | – | ✓ | ✓ | list | notes | all | codec · channels · language, "+N tracks" when more exist. |
| `File` | – | – | ✓ | text | notes | all |  |
| `AV offset` | – | – | ✓ | millis | grid | all | always shown Applied offset; the container-declared value follows in parentheses when it differs. |

**NOW DECODING**

| Row | mini | standard | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|
| `Resolution` | ✓ | ✓ | ✓ | resolution | grid | all |  |
| `Dynamic range` | ✓ | ✓ | ✓ | text | notes | all | Mini shows the chip form ("DV P7 → HDR10"); the ledger shows the sentence. |
| `Audio` | – | – | ✓ | list | notes | all | What the player's audio output actually is (route-aware where the platform says). |
| `Buffer` | ✓ | ✓ | ✓ | seconds | grid | all | Runway ahead of the playhead. Seconds only — no clock form, no percentage. |
| `Frames` | – | ✓ | ✓ | fraction | grid | web · android | dropped / total. AVPlayer does not expose it. |
| `Frame rate` | – | – | ✓ | text | grid | web |  |
| `Player state` | – | – | ✓ | text | grid | all | One vocabulary: Playing · Paused · Buffering · Ended · Failed. |
| `Waiting reason` | – | – | ✓ | text | notes | apple |  |
| `Decoder` | – | – | ✓ | text | grid | web · android | hardware / software, with the reason when software. |
| `Stalls` | – | ✓ | ✓ | text | grid | all | "2 (1 supply · 1 decode)" — player-side stall count this session. |
| `Subtitles` | – | ✓ | ✓ | text | grid | all | Track name; the delivery clause (native · burned · overlay) is a note under the same label. |

**NETWORK**

| Row | mini | standard | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|
| `Delivery rate` | ✓ | ✓ | ✓ | bitrate | grid | all | Server-reported delivered rate; "· idle" appended when delivery has gone quiet. One label — not Delivery in one mode and Delivery rate in another. |
| `Observed rate` | – | – | ✓ | bitrate | grid | all | The player's own throughput estimate. |
| `Stream rate` | – | – | ✓ | bitrate | grid | all | The declared rate of the rendition being played. |
| `Delivered` | – | ✓ | ✓ | bytes | grid | all | Server-reported bytes for this session. Never labelled Transferred. |
| `Transferred` | – | – | ✓ | bytes | grid | apple | Client access-log bytes — a different number from Delivered, so a different label. |
| `Requests` | – | – | ✓ | count | grid | apple |  |
| `Delivery idle` | – | – | ✓ | millis | grid | all |  |
| `Started in` | – | – | ✓ | seconds | grid | all | Time to first frame. |

**SERVER**

| Row | mini | standard | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|
| `Status` | – | ✓ | ✓ | text | grid | all | always shown One vocabulary: No server-side session · Active · Holding buffer · Served from cache (+ the VOD states on the web). Debug shows it too. |
| `Encoder` | – | ✓ | ✓ | text | grid | all |  |
| `Encode speed` | – | ✓ | ✓ | speed | grid | all | "(avg)" appended when only the cumulative figure exists. Never abbreviated to Encode. |
| `Server ahead` | – | ✓ | ✓ | seconds | grid | all | Produced beyond the playhead. Held state and time-release are a note under the same label, never inlined into the value. One label — not Ahead / Buffer ahead / Server ahead. |
| `Ahead bytes` | – | – | ✓ | bytes | grid | all |  |
| `Produced` | – | – | ✓ | clock | grid | all |  |
| `Pacing` | – | – | ✓ | speed | grid | all |  |
| `Held` | – | – | ✓ | yesno | grid | all |  |
| `Hold reason` | – | – | ✓ | text | grid | all |  |
| `Suspend count` | – | – | ✓ | count | grid | all |  |
| `Demand window` | – | – | ✓ | text | grid | web |  |
| `Request idle` | – | – | ✓ | seconds | grid | all |  |
| `Last request` | – | – | ✓ | text | notes | all |  |
| `Playlist` | – | – | ✓ | text | grid | all |  |
| `Published end` | – | – | ✓ | millis | grid | all |  |
| `Fetched end` | – | – | ✓ | millis | grid | all |  |
| `Control` | – | ✓ | ✓ | text | grid | all | Playback-control session owner and state; timing and failure are notes under the same label in Debug. |

<!-- contract:info:end -->

---

## 8. Non-goals

- **Not a redesign of the chrome.** Same buttons, same icons, same themes.
  Only *where the bar sits*, *what a press does*, and *when the chrome
  hides* change.
- **Not thumbnails, chapters or a time bubble on the bar.** The preview is
  the time label and the fill. Add those later behind the same `preview`
  outcome; the contract does not need to change.
- **Not the offline reader, the lightbox, or the connect dialog.** They
  have their own key maps; the audit lists their gaps (§3.3 items 9–10)
  and the plan's M5 picks them up after the players agree.
- **Not a spatial-navigation engine for the web.** TV browsers get the
  `ten-foot` table applied to the *focused* timeline and the existing
  Tab order; arrows moving focus between buttons on a TV browser is a
  separate effort with its own plan.
- **Not a change to what seeking does on the server.** Session reopen on
  transcode/remux, VOD immutable-timeline seeks, coalescing — all
  unchanged. The contract only changes *when* a seek is issued.

---

## 9. Defaults ruled with the contract

Paul ruled the fixture defaults on 2026-09-02: the options set uses Android's
`settings` shape; tvOS episode cards keep play-on-select; `select` on an idle
timeline toggles play; and the Debug info list is the union in
`playback-info-fields.json`. These are contract rows, not client exceptions.
