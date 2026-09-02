# Player input contract — one routing table, every client obeys it

**Status:** ruled 2026-09-02, fixtures committed, clients not yet conformant ·
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

| Outcome | What the client does |
|---|---|
| `reveal` | Show chrome; focus the control that had focus when it hid (initially `play_pause`). Never seeks. |
| `focus_row` | Let the platform focus engine move within the current row or to the adjacent row in §3 order. The reducer names the row, never a coordinate. |
| `focus_transport` | Move focus to the transport row, restoring the last transport control. |
| `focus_marker_or_ignore` | Focus `skip_marker` if a marker is showing, else nothing. |
| `preview` | Enter or stay in `scrub`: move the pending position by §4's ladder. No seek, no network. Show it in `time_elapsed` and on the bar. |
| `commit` | Seek to the pending position; back to `timeline` with chrome up. |
| `cancel` | Drop the pending position; back to `timeline`. The `cancel_then_…` variants do that and then the named focus move. |
| `toggle_play` / `skip` | Play-pause / immediate ±10 s. Both reveal chrome if hidden and restart the hide timer. |
| `activate` | Press the focused control. |
| `menu_focus` | Focus stays inside the open menu or panel — trapped — while the platform moves it among the menu's own items. |
| `close_menu` / `close_info` | Close it; focus returns to the control that opened it. |
| `hide` | Hide chrome, remember the focused control, focus the surface. |
| `exit` | Leave the player. Single press: no state requires two. |
| `toggle_chrome` | Touch only: hide if visible, else reveal. |

### 2.2 Precedence of `back`

`back` (Menu · BACK · Escape · the ✕ on touch) resolves by state, so it is
never ambiguous which thing closes:

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

| Row | Items in order | Focusable | Present when |
|---|---|---|---|
| `marker` | `skip_marker` | yes | a skip-intro/credits marker is active. Right-aligned above the timeline; reachable by `up` from the timeline only. |
| `timeline` | `time_elapsed` · `timeline` · `time_total` | only `timeline` | duration known. Unknown-duration streams show `--:--` and an indeterminate bar that ignores `preview`. |
| `transport` | `skip_back` · `play_pause` · `skip_forward` · spacer · `audio` · `subtitles` · `quality` · `settings` · `info` · `pip` · `close` | all | `audio` when >1 audio track · `subtitles` when any · `quality` when the ladder has rungs · `pip` when the platform says PiP is possible (never tvOS) · `close` on touch and desktop only. |

Narrow touch splits the transport row at the spacer into two lines; order
within each line is unchanged. Nothing else moves between surfaces.

**`settings` holds:** autoplay next · auto-skip markers · audio sync ·
(reserved) playback speed. These are preferences, not moment-to-moment
controls; today they are scattered as top-level buttons on the web
(`⏭ Auto-skip`, `▶ Autoplay`, `⇄ Sync`), a top-level `autoplay` button on
Apple, and Android's "Playback settings" panel — Android's shape is the one
kept. **This is the open ruling in audit §6**; until Paul rules, the plan
builds the row with the existing per-client option buttons in the
`settings` slot's position and folds them in M4.

**Focus memory.** `initial_focus` is `play_pause`. `reveal` restores the
last focused control; `focus_transport` restores the last *transport*
control. Closing a menu or the info panel returns focus to whatever opened
it. Focus is never sent to an invisible element: a hidden surface takes
focus only while chrome is hidden, and the failure view's buttons keep it
until `back`.

---

## 4. Timings and steps

| Name | Value | Why this value |
|---|---|---|
| `hide_after_ms` | 4000 | Apple already used 4 s; Android 3.8 s and web 2.6 s were independent guesses. One number, and long enough to read a row. |
| hide only while playing | yes | A paused film with no chrome is a black frame with no affordance. Apple hid while paused; Android and web did not. |
| hide suppressed in | `scrub`, `menu`, `info`, `failed` | Every one of these has focus inside something the hide would remove. tvOS today hides under the failure view and the stats panel and parks focus on an invisible surface (audit §3.1 item 3). |
| `skip_seconds` | 10 | The only immediate step: the two buttons and FF/REW media keys. |
| `preview_step_seconds` | 10, then 30 from the 5th repeat, 60 from the 10th | Holding a direction while scrubbing accelerates; releasing resets. Replaces the ±30 s vertical seek with something a held key discovers by itself. |
| `vertical_seek_seconds` | none | Up/Down are rows. Dropped by ruling. |
| `preview_auto_commit_ms` | none | A preview commits on Select or pointer release, never on a timer. The web's *body* hotkeys keep their existing 350 ms coalesce (`desktop_hotkey_coalesce_ms`) because they are an immediate-seek idiom, not a scrub. |

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
adapter.** `make validate-staged` fails a diff that adds `KEYCODE_`,
`onMoveCommand`, `onExitCommand`, `onPlayPauseCommand`, `keydown`, or
`e.key` to any client file other than the named adapter (plan §5 lists the
allowed files). A new behaviour has to go through the table, where the
test will ask what the other two clients do.

Per-surface adapters map inputs as follows; anything not listed is
`ignore`d by the adapter before it reaches the reducer.

| Contract input | tvOS (Siri Remote) | Android TV (D-pad) | iOS / Android touch | Web |
|---|---|---|---|---|
| `left`/`right`/`up`/`down` | `onMoveCommand` directions | `KEYCODE_DPAD_*` | — | Arrow keys |
| `select` | Select (`onTapGesture` on the focused view) | `DPAD_CENTER`/`ENTER` | tap on a control | Enter; Space when the timeline is focused |
| `back` | Menu (`onExitCommand`) | `BACK` | ✕ / system back | Escape |
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
   under scrub/menu/info/failed), and that §2 of this page is byte-identical
   to `scripts/player-contract-table`'s rendering. Change the fixture, run
   `scripts/player-contract-table --write`, commit both.
2. **Each reducer reproduces the fixture.** Web: `web-policy.test.js` loads
   the JSON and asserts `routeInput` over every row. Android: a JUnit test
   over `src/test/resources/player-input-contract.json`. Apple: an XCTest
   over the same file as a test resource. The two copies are checked
   byte-identical against `tests/playback/` by a validation check (plan
   §5), the same way the five-surface build-claims contract keeps
   `project.yml` and the docs in step.
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
   that only the timeline is focusable in its row. Apple: `PlayerControlPlan`
   is a pure function from `(surface, capabilities)` to rows, tested
   against the fixture, and the view is a `ForEach` over it.
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

## 9. Open rulings

Recommended defaults are already in the fixture; ruling the other way
changes rows, not architecture.

1. **The options set** (§3): keep Android's `settings` panel shape and fold
   the web's auto-skip/autoplay/sync buttons and Apple's autoplay button
   into it — recommended — or keep top-level toggles and make all three
   clients show the same ones.
2. **tvOS season episode cards**: Select plays today
   (`Components.swift:313-322`); every other surface opens detail. Keep,
   or add a "details" region as iOS has.
3. **`select` on an idle timeline** is `toggle_play` (the Apple TV system
   player's behaviour). Alternative: `ignore`. Both are one cell.
4. **The info panel's row list** (§7) is the union of what the three
   clients show today, with the duplicate labels retired and the
   platform-only rows marked. Prune it if Debug is too long; a pruned row
   is one line removed from the fixture.
