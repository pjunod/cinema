# Player input contract — one routing table, every client obeys it

**Status:** ruled and implemented 2026-09-02 ·
**Source of truth:** [`tests/playback/player-input-contract.json`](../../tests/playback/player-input-contract.json) (input) and [`tests/playback/playback-info-fields.json`](../../tests/playback/playback-info-fields.json) (info panel) ·
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

_Generated from [`tests/playback/player-input-contract.json`](../../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

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

**Watch presentation** — Presentation is independent of input surface. Web remains desktop at every width. Inline chrome ignores idle; overlay chrome retains the paused, failed, scrub and panel exclusions. Unmounted return_browser resolves to exit. Legacy native route() remains in force until retained-owner layout acceptance.

| browser / overlay | surface | back | idle |
|---|---|---|---|
| hidden | ten-foot | exit | ignore |
| transport | ten-foot | exit | hide |
| timeline | ten-foot | exit | hide |
| scrub | ten-foot | cancel | ignore |
| menu | ten-foot | close_menu | ignore |
| info | ten-foot | close_info | ignore |
| failed | ten-foot | exit | ignore |
| hidden | desktop | ignore | ignore |
| transport | desktop | ignore | hide |
| timeline | desktop | ignore | hide |
| scrub | desktop | cancel | ignore |
| menu | desktop | close_menu | ignore |
| info | desktop | close_info | ignore |
| failed | desktop | ignore | ignore |
| hidden | touch | exit | ignore |
| transport | touch | exit | hide |
| timeline | touch | exit | hide |
| scrub | touch | cancel | ignore |
| menu | touch | close_menu | ignore |
| info | touch | close_info | ignore |
| failed | touch | exit | ignore |

| browser / inline | surface | back | idle |
|---|---|---|---|
| hidden | ten-foot | exit | ignore |
| transport | ten-foot | exit | ignore |
| timeline | ten-foot | exit | ignore |
| scrub | ten-foot | cancel | ignore |
| menu | ten-foot | close_menu | ignore |
| info | ten-foot | close_info | ignore |
| failed | ten-foot | exit | ignore |
| hidden | desktop | ignore | ignore |
| transport | desktop | ignore | ignore |
| timeline | desktop | ignore | ignore |
| scrub | desktop | cancel | ignore |
| menu | desktop | close_menu | ignore |
| info | desktop | close_info | ignore |
| failed | desktop | ignore | ignore |
| hidden | touch | exit | ignore |
| transport | touch | exit | ignore |
| timeline | touch | exit | ignore |
| scrub | touch | cancel | ignore |
| menu | touch | close_menu | ignore |
| info | touch | close_info | ignore |
| failed | touch | exit | ignore |

| fullscreen / overlay | surface | back | idle |
|---|---|---|---|
| hidden | ten-foot | return_browser | ignore |
| transport | ten-foot | hide | hide |
| timeline | ten-foot | hide | hide |
| scrub | ten-foot | cancel | ignore |
| menu | ten-foot | close_menu | ignore |
| info | ten-foot | close_info | ignore |
| failed | ten-foot | return_browser | ignore |
| hidden | desktop | return_browser | ignore |
| transport | desktop | return_browser | hide |
| timeline | desktop | return_browser | hide |
| scrub | desktop | cancel | ignore |
| menu | desktop | close_menu | ignore |
| info | desktop | close_info | ignore |
| failed | desktop | return_browser | ignore |
| hidden | touch | return_browser | ignore |
| transport | touch | hide | hide |
| timeline | touch | hide | hide |
| scrub | touch | cancel | ignore |
| menu | touch | close_menu | ignore |
| info | touch | close_info | ignore |
| failed | touch | return_browser | ignore |

| fullscreen / inline | surface | back | idle |
|---|---|---|---|
| hidden | ten-foot | return_browser | ignore |
| transport | ten-foot | hide | ignore |
| timeline | ten-foot | hide | ignore |
| scrub | ten-foot | cancel | ignore |
| menu | ten-foot | close_menu | ignore |
| info | ten-foot | close_info | ignore |
| failed | ten-foot | return_browser | ignore |
| hidden | desktop | return_browser | ignore |
| transport | desktop | return_browser | ignore |
| timeline | desktop | return_browser | ignore |
| scrub | desktop | cancel | ignore |
| menu | desktop | close_menu | ignore |
| info | desktop | close_info | ignore |
| failed | desktop | return_browser | ignore |
| hidden | touch | return_browser | ignore |
| transport | touch | hide | ignore |
| timeline | touch | hide | ignore |
| scrub | touch | cancel | ignore |
| menu | touch | close_menu | ignore |
| info | touch | close_info | ignore |
| failed | touch | return_browser | ignore |

All other inputs retain the finite routing cells above.

<!-- contract:routing:end -->

### 2.1 What the outcomes mean

<!-- contract:outcomes:begin -->

_Generated from [`tests/playback/player-input-contract.json`](../../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

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
| `return_browser` | Restore the retained watch browser without stopping playback; resolve to exit when no browser is mounted. |

<!-- contract:outcomes:end -->

### 2.2 Precedence of `back`

For watch-and-browse, the generated watch table above adds presentation
(`browser` or `fullscreen`) and chrome placement (`overlay` or `inline`).
The seven interaction states remain unchanged. Web uses `desktop` at every
width, including coarse-pointer devices; resizing never changes input surface.

Back first cancels scrub, closes a menu, or closes playback info. At a browser
root, including failure, desktop ignores it and touch/ten-foot exit. In
fullscreen, hidden and failed return to the browser; visible transport/timeline
return on desktop and hide on touch/ten-foot. `return_browser` preserves media
and restores the mounted browser. Without a mounted browser it means `exit`.
Legacy native players retain their original routing until owner acceptance;
their adapters explicitly resolve the new outcome to exit.

Inline web chrome ignores idle so below-picture content cannot collapse.
Overlay idle keeps the existing playing-only timer and scrub/panel/failure
exclusions. Escape outside the player belongs to the page. Browser-API
fullscreen consumes Escape before the adapter; fullscreenchange performs the
return. Such unreachable key cells need browser evidence, not fabricated key
coverage. The Close button always cleans up the active interaction then exits.


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

_Generated from [`tests/playback/player-input-contract.json`](../../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

| Row | Items in order | Surfaces | Focusable | Notes |
|---|---|---|---|---|
| `bar` | `airplay` · `close` | `desktop` · `touch` | all | A corner strip over the picture, not a row in the chrome: every client that has a Close puts it there — top-trailing on the web, top-leading on iOS and on Android, where a television cannot focus it at all. `close` is last here for the same reason it was last in the transport row: the control that ends the session is the one you should not land on by accident. Nothing here is a horizontal neighbour of a transport button. `close` when touch and desktop only — a ten-foot player exits with `back` · `airplay` when web, and only while the browser reports an AirPlay target |
| `marker` | `skip_marker` | all | all | Present only while a skip-intro/credits marker is active. Sits above the timeline row, right-aligned. Reachable by `up` from the timeline; never a horizontal neighbour of anything. |
| `timeline` | `time_elapsed` · `timeline` · `time_total` | all | `timeline` | Its own full-width row. The timeline is never a horizontal neighbour of a button — Left/Right on it belong to scrubbing, so a button beside it would be unreachable without a seek. |
| `transport` | `skip_back` · `play_pause` · `skip_forward` · spacer · `audio` · `subtitles` · `quality` · `settings` · `info` · `title_info` · `pip` · `larger` · `fullscreen` | all | all | One row on ten-foot and on wide touch/desktop. Narrow touch splits it after `spacer` into a transport line and an options line; order within each line is unchanged. `surface_placement` names the one control a surface renders somewhere else: the web puts `info` in the `bar` row, where its ✕ already lives, and the native clients keep it here. Narrow web also splits after spacer, without changing its desktop surface placement. `audio` when more than one audio track · `subtitles` when at least one subtitle track · `quality` when server ladder has rungs · `pip` when platform reports picture-in-picture possible (never tvOS) · `title_info` when web only, wide watch or fullscreen; compact has an always-on side panel · `fullscreen` when web only — a native player is already full screen · `larger` when web only, non-narrow watch browser; absent from the DOM on narrow web `desktop` renders `info` at position 0 of the `bar` row |

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

_Generated from [`tests/playback/player-input-contract.json`](../../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

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
- desktop_hotkey_coalesce_ms: arrow hotkeys on the desktop body accumulate against one frozen base, but the physical key owns the gesture. The timer may commit only after keyup; blur commits the visible target and attachment replacement cancels it.

<!-- contract:timings:end -->

Why these values: Apple already used 4 s for the hide, while Android's 3.8 s
and the web's 2.6 s were independent guesses — one number, long enough to
read a row. A paused film with no chrome is a black frame with no
affordance, so the hide runs only while playing. The preview ladder replaces
the ±30 s vertical seek with something a held key discovers by itself.

---

## 4a. Live TV — the same rulings on a surface with no timeline

Live television is the finite player's contract with everything that assumes
a timeline removed and one thing added. There is nothing to seek, nothing to
scrub, no menu and no info panel; there is a channel. So the live table is a
sibling of §2's, generated from the `live` section of the same fixture, and
every client routes it through the same one-table reducer:
`PlaybackPolicy.routeLiveInput`, `PlayerInputRouting.routeLive`, and
`PlayerInputPolicy.routeLive`.

It is a second table rather than a fourth surface in §2 because the fixture's
own well-formedness test requires every surface to answer all seven finite
states. A `live` surface would have to invent a `timeline`, a `scrub`, a
`menu` and an `info` state that a live stream can never enter — a lie in the
place the clients are tested against. One fixture, two tables, three clients
keeps the property that mattered without the lie.

The two rulings that carry over unchanged, from 2026-09-02: a directional
press on a hidden overlay only reveals it, and a ten-foot channel list is
preview-then-commit. The one that does not: on the desktop, `up`/`down` tune
directly, because a keyboard user has no focus ring to preview with and the
mouse already drives the neighbour strip.

<!-- contract:live:begin -->

_Generated from the `live` section of [`tests/playback/player-input-contract.json`](../../tests/playback/player-input-contract.json) by `scripts/player-contract-table`; do not edit by hand._

**Surface `ten-foot`** — Siri Remote (tvOS) and D-pad (Android TV / Google TV). Root browsing delegates ordinary focus to the native framework; custom grids and fullscreen chrome handle only the events they own.

| state \ input | `left` | `right` | `up` | `down` | `select` | `back` | `play_pause` | `tap_surface` | `idle` |
|---|---|---|---|---|---|---|---|---|---|
| `browser` | `delegate` | `delegate` | `delegate` | `delegate` | `delegate` | `exit` | `ignore` | `delegate` | `ignore` |
| `fullscreen_hidden` | `reveal` | `reveal` | `reveal` | `reveal` | `reveal` | `return_browser` | `toggle_play` | `reveal` | `ignore` |
| `fullscreen_controls` | `focus_control` | `focus_control` | `focus_control` | `focus_control` | `activate` | `hide` | `toggle_play` | `ignore` | `hide` |
| `temporary_guide` | `focus_cell` | `focus_cell` | `focus_cell` | `focus_cell` | `activate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |
| `menu` | `focus_panel` | `focus_panel` | `focus_panel` | `focus_panel` | `activate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |
| `programme_details` | `focus_panel` | `focus_panel` | `focus_panel` | `focus_panel` | `activate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |
| `stream_info` | `focus_panel` | `focus_panel` | `focus_panel` | `focus_panel` | `activate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |

**Surface `desktop`** — Pointer plus keyboard on the web while the Live TV host is fullscreen or focused. Existing channel-strip hotkeys remain unchanged.

| state \ input | `left` | `right` | `up` | `down` | `select` | `back` | `play_pause` | `tap_surface` | `idle` |
|---|---|---|---|---|---|---|---|---|---|
| `browser` | `delegate` | `delegate` | `delegate` | `delegate` | `delegate` | `ignore` | `ignore` | `delegate` | `ignore` |
| `fullscreen_hidden` | `strip_prev` | `strip_next` | `channel_up` | `channel_down` | `ignore` | `exit` | `toggle_play` | `reveal` | `ignore` |
| `fullscreen_controls` | `strip_prev` | `strip_next` | `channel_up` | `channel_down` | `tune` | `exit` | `toggle_play` | `ignore` | `hide` |
| `temporary_guide` | `focus_cell` | `focus_cell` | `focus_cell` | `focus_cell` | `activate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |
| `menu` | `delegate` | `delegate` | `delegate` | `delegate` | `delegate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |
| `programme_details` | `delegate` | `delegate` | `delegate` | `delegate` | `delegate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |
| `stream_info` | `delegate` | `delegate` | `delegate` | `delegate` | `delegate` | `close_panel` | `toggle_play` | `ignore` | `ignore` |

**Surface `touch`** — iPhone/iPad and Android phones/tablets. Focus states do not apply; tapping the picture is the whole contract.

| state \ input | `left` | `right` | `up` | `down` | `select` | `back` | `play_pause` | `tap_surface` | `idle` |
|---|---|---|---|---|---|---|---|---|---|
| `browser` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` | `exit` | `ignore` | `ignore` | `ignore` |
| `fullscreen_hidden` | `ignore` | `ignore` | `ignore` | `ignore` | `ignore` | `exit` | `ignore` | `toggle_chrome` | `ignore` |
| `fullscreen_controls` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `exit` | `ignore` | `toggle_chrome` | `hide` |
| `temporary_guide` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `close_panel` | `ignore` | `ignore` | `ignore` |
| `menu` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `close_panel` | `ignore` | `close_panel` | `ignore` |
| `programme_details` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `close_panel` | `ignore` | `close_panel` | `ignore` |
| `stream_info` | `ignore` | `ignore` | `ignore` | `ignore` | `activate` | `close_panel` | `ignore` | `close_panel` | `ignore` |

| Outcome | What the client does |
|---|---|
| `reveal` | Draw the overlay and restart the auto-hide timer. Nothing else happens — the press that reveals never also acts. |
| `hide` | Hide the overlay and drop focus back to the picture. |
| `delegate` | Return the event unconsumed so the native focus or button framework receives it exactly once. |
| `focus_control` | Move deliberately among Guide, Channels, Pause/Play live, Info, and More. |
| `focus_cell` | Move to the adjacent programme, or to the programme covering the same anchor time in the adjacent channel. |
| `focus_panel` | Move within the open menu, details, or Info panel without escaping it. |
| `activate` | Activate the focused overlay control. On a focused channel row that means tune it. |
| `close_panel` | Close the temporary guide, menu, details, or Info and restore its exact opener. |
| `return_browser` | Leave fullscreen for the saved root layout without stopping or retuning the current session. |
| `strip_prev` | Move the neighbour strip's preview one channel earlier and reveal the overlay if it was hidden. Previewing never opens a tuner. |
| `strip_next` | Move the neighbour strip's preview one channel later and reveal the overlay if it was hidden. |
| `tune` | Open the previewed channel: release the current lease and start one session on the new channel. |
| `channel_up` | Tune the previous channel in the visible order directly, subject to the 350 ms coalescing rule. |
| `channel_down` | Tune the next channel in the visible order directly, subject to the 350 ms coalescing rule. |
| `toggle_play` | Pause or resume the live picture. Pausing does not rewind and does not hold the tuner past the no-progress release. |
| `toggle_chrome` | Show the overlay if it is hidden, hide it if it is shown. |
| `exit` | Leave the presentation: exit fullscreen on the desktop, leave the live surface on a television. |
| `ignore` | Do nothing. The input belongs to another owner in this state. |

| Timing | Value |
|---|---|
| `hide_after_ms` | 4000 |
| `hidden_only_while_playing` | true |
| `channel_coalesce_ms` | 350 |
| `preview_auto_commit_ms` | none |
| `guide_poll_unavailable_s` | 30 |
| `guide_poll_min_s` | 15 |
| `guide_poll_after_next_refresh_s` | 5 |
| `retire_liveness_probe_ms` | 250 |
| `retire_orphan_after_keepalives` | 3 |
| `start_replay_attempts` | 1 |
| `guide_poll_ceiling_s` | 1200 |

**Hotkeys** (desktop, only while the live host is fullscreen or focused): `f` fullscreen · `m` mute · `p` picture_in_picture · `g` guide_sheet · `escape` exit.

- A live stream has no seekable timeline, so no row here seeks, records, rewinds, or schedules playback.
- A directional press or Select on hidden ten-foot controls only reveals them; the revealing press never also moves focus or tunes.
- Root channel focus and playing channel are independent. Native focus movement is unconsumed, and Select reaches the focused button exactly once.
- `ten-foot`: The root browser uses native focus and a persistent preview player. Fullscreen reveal and the programme grid intercept only the moves they own.
- `desktop`: Existing web channel-strip keys remain direct tune controls. Desktop menus and details delegate ordinary focus while Back closes the panel.
- `touch`: `play_pause` is `ignore`: a phone has no hardware transport key over this surface and the tap answer is the whole contract.

<!-- contract:live:end -->

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
adapter.** `scripts/player-input-fence` runs from `make validate-staged` on
any diff that touches a client or the web —
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
| `back` | Menu (`onExitCommand`) | `BACK` | system back (Android); iOS has no producer | Escape; `GoBack`/`BrowserBack`; `keyCode` 10009 (Tizen) or 461 (webOS) |
| `close` (`close_control`, not a table input) | — | — | ✕ (iOS) / back arrow (Android phone) | `✕ Close` |
| `play_pause` | Play/Pause (`onPlayPauseCommand`, **on every state's root**, not only the hidden surface) | `MEDIA_PLAY_PAUSE` | lock-screen / headset commands | Space or K when the timeline is not focused; MediaSession `play` and `pause` as two handlers, each routed through this table and idempotent on the viewer's intent |
| `skip_back`/`skip_forward` | — (no producer) | `MEDIA_REWIND`/`MEDIA_FAST_FORWARD` | remote-command skips | J / L; MediaSession `seekbackward`/`seekforward`, routed through this table, honouring the browser's own `seekOffset` |
| `tap_surface` | — | — | tap on the video | click on the video |
| `idle` | hide timer | hide timer | hide timer | hide timer |

On the web the MediaSession commands are decoded in the same
`player-input-adapter` region as the keys, because they are the same question
asked by a different producer — the OS media keys, a headset button, the lock
screen. They are routed through the same table as the keys
(`watchRouteInput(playerInputState(), …)`): `failed` ignores `play_pause` and
both skips, `scrub` commits the pending seek before `play_pause` acts and
ignores the skips, and `menu`/`info` ignore the skips. `play` and `pause` are
separate handlers and each is idempotent on the viewer's **intent** (the
pending open's, else the player's `wantsPlayback`), never on the element's
`paused`: a pending open and every reattach leave the element paused while the
viewer still wants playback, so a `play` sent then leaves it playing and a
`pause` pauses it. The `playbackState` the OS shows is the same intent. Every
handler is installed behind a feature check for `mediaSession` and again per
action, because a browser throws on an action it does not implement.
`nexttrack` has a handler only while autoplay-next is on and the title is not
known to be something other than an episode — a browser shows a Next control
for any action that has a handler — and changing the setting re-registers or
removes it. There is no Remote Playback or Chromecast claim.

On Apple, every explicit Play/Pause producer—including the tvOS Siri Remote and
iOS lock-screen/headset commands—routes through the same playback-request
setter. Resume publishes active control intent in sequence order. If a seek or
prepared replacement owns the next destination, the controller retains the
requested-play state but does not start the predecessor item while that work is
pending. Readiness evidence may be shown in Developer settings, but it never
disables the user's enable control.

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
   [`tests/playback/playback-info-fields.json`](../../tests/playback/playback-info-fields.json)
   (§7). Android already has a section/row builder and an instrumented test
   of its Standard labels; Apple's ledger types become non-private so the
   same test can exist; the web's `updateStats` builders are sliced and
   executed like the rest of the shipped JS.

---

## 7. Playback info — a shared hierarchy across clients

The expanded panel is the `info` state of §2: Back closes it, focus returns
to the Info button, and playback chrome does not auto-hide underneath it.
TV disclosure buttons and observation rows are focusable so the remote can
reach every row in the scroll view. Phone rows stack; wider displays keep
source and player measurements beside each other.

| Stored mode | Displayed name | Purpose |
|---|---|---|
| `mini` | Compact | Labeled playing resolution, playback state and device buffer. |
| `standard` | Overview | Player picture, original source, method and reason, tracks, buffer and interruptions. |
| `details` | Details | Picture & sound, Buffer & delivery, Server work; Live stream & reception when applicable. |
| `debug` | Diagnostics | Every available observation, including session and surface history, grouped by purpose. |

Existing persisted `mini`, `standard` and `debug` values retain their meanings;
`details` is additive. Main players persist the choice; the Live TV sheet opens
on Overview. Compact leaves transport behavior unchanged. The fixture's mode
sets describe availability to each presentation, not an instruction to dump
all rows into Overview. Its `placement` remains a classification of prose
versus short values; both now keep explanations next to the value.

**Resolution never silently disappears.** The headline uses positive dimensions
from the attached player's presentation API. Missing, zero or invalid dimensions
show `Not reported`. Source metadata and stream/manifest metadata have separate
labels and never substitute for the observed picture. Web suppresses predecessor
measurements while a replacement is pending. Native views read the current
player/item, and Live TV samples only while its panel is open.

**Measurements have separate meanings.** Device buffer is contiguous loaded
media ahead of the playhead. Server-ready media is a separate observation.
Stream bitrate describes media; observed download rate describes transfers;
server response completion does not prove client receipt or playback. An audio
track is not proof of speaker or HDMI output. Buffering interruptions exclude
intentional pauses; unavailable counters say `Not reported`, not zero. Live-edge
distance is to available stream media and is not broadcast latency.

**Diagnostics remain available.** The canonical fields below retain their IDs,
units and platform applicability. Source, delivery, encoder, control and surface
history are still reachable under grouped disclosures. Extra Live TV fields
include source observation time, reception and live-edge distance. Diagnostics
show server sample age when available; no synthetic freshness or health score is
created for a platform that does not report it.

<!-- contract:info:begin -->

_Generated from [`tests/playback/playback-info-fields.json`](../../tests/playback/playback-info-fields.json) by `scripts/player-contract-table`; do not edit by hand._

**PLAYBACK**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Method` | – | ✓ | ✓ | ✓ | text | grid | all | The delivery verdict — the same vocabulary on every client: Direct play · Remux · Transcode · Transcode · cached; the encoder and rung follow as a clause ("Transcode · nvenc · 1080p"). |
| `Playback mode` | – | – | – | ✓ | text | grid | web | Live HLS / VOD HLS / progressive — the web presentation kind. Native players have one presentation. |
| `Position` | – | ✓ | ✓ | ✓ | position | grid | all |  |
| `Reason` | – | ✓ | ✓ | ✓ | list | notes | all | Why the server chose this method. |
| `Build` | – | – | – | ✓ | text | grid | all | always shown |
| `Transport` | – | – | – | ✓ | text | notes | all |  |
| `File ID` | – | – | – | ✓ | text | grid | all |  |
| `Session` | – | – | – | ✓ | text | notes | all |  |
| `Switched` | – | – | – | ✓ | list | notes | web | Auto-quality moves this session. |

**SOURCE**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Original video` | – | ✓ | ✓ | ✓ | list | notes | all | codec · profile · bit depth · HDR format. |
| `Source frame` | – | ✓ | ✓ | ✓ | resolution | grid | all |  |
| `Source pixel aspect` | – | – | ✓ | ✓ | text | grid | all |  |
| `Source display aspect` | – | – | ✓ | ✓ | text | grid | all |  |
| `Source bitrate` | – | ✓ | ✓ | ✓ | bitrate | grid | all |  |
| `Container` | – | ✓ | ✓ | ✓ | text | grid | all |  |
| `Source audio track` | – | ✓ | ✓ | ✓ | list | notes | all | codec · channels · language, "+N tracks" when more exist. |
| `File` | – | – | – | ✓ | text | notes | all |  |
| `AV offset` | – | – | – | ✓ | millis | grid | all | always shown Applied offset; the container-declared value follows in parentheses when it differs. |

**NOW DECODING**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Player display size` | ✓ | ✓ | ✓ | ✓ | resolution | grid | all | always shown Presentation dimensions reported by the attached player; not encoded frame dimensions. |
| `Stream frame` | ✓ | ✓ | ✓ | ✓ | resolution | grid | all | always shown Encoded output plan or eligible attached stream sample, with provenance shown beside the value. |
| `Stream pixel aspect` | – | – | ✓ | ✓ | text | grid | all |  |
| `Frame comparison` | – | – | ✓ | ✓ | text | grid | all |  |
| `Aspect comparison` | – | – | ✓ | ✓ | text | grid | all |  |
| `Stream format` | – | ✓ | ✓ | ✓ | text | grid | all | always shown Codec, scan and cadence only. Stream frame is the sole active output dimension row. |
| `Device audio output` | – | ✓ | ✓ | ✓ | text | grid | all | always shown Speaker or HDMI output only when reported by the platform; never inferred from the audio track. |
| `Dynamic range` | – | ✓ | ✓ | ✓ | text | notes | all | Mini shows the chip form ("DV P7 → HDR10"); the ledger shows the sentence. |
| `Stream audio track` | – | ✓ | ✓ | ✓ | list | notes | all | Selected stream audio track metadata; not a claim about speaker or HDMI output. |
| `Frames` | – | ✓ | ✓ | ✓ | fraction | grid | web · android | dropped / total. AVPlayer does not expose it. |
| `Frame rate` | – | – | – | ✓ | text | grid | web |  |
| `Player state` | ✓ | ✓ | ✓ | ✓ | text | grid | all | One vocabulary: Playing · Paused · Buffering · Ended · Failed. |
| `Waiting reason` | – | – | – | ✓ | text | notes | apple |  |
| `Decoder` | – | – | – | ✓ | text | grid | web · android | hardware / software, with the reason when software. |
| `Buffering interruptions` | – | ✓ | ✓ | ✓ | text | grid | all | "2 (1 supply · 1 decode)" — player-side stall count this session. |
| `Subtitles` | – | ✓ | ✓ | ✓ | text | grid | all | Track name; the delivery clause (native · burned · overlay) is a note under the same label. |

**BUFFERING / DELIVERY**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Source read` | – | – | – | ✓ | text | grid | all | always shown Measured source read activity only. Configured input pacing is not a measurement; unsupported clients show Unavailable. |
| `Ready on server` | – | ✓ | ✓ | ✓ | seconds | grid | all | always shown Contiguous complete media beginning at the latest accepted absolute playhead or seek anchor. Missing is 0.0 s; unobservable or evicted coverage is Unavailable. |
| `Ready state` | – | – | – | ✓ | text | grid | all | always shown Ready · Missing · Unavailable; kept separate so unknown is never rendered as zero. |
| `Ready anchor` | – | – | – | ✓ | millis | grid | all |  |
| `Ready end` | – | – | – | ✓ | millis | grid | all |  |
| `Later ready` | – | – | – | ✓ | text | notes | all | A later contiguous ready island is diagnostic context, never runway at the current playhead. |
| `HTTP wait` | – | ✓ | ✓ | ✓ | text | grid | all | Active server-side media responses waiting for publication. This does not describe client buffering. |
| `Buffered on device` | ✓ | ✓ | ✓ | ✓ | seconds | grid | all | Contiguous native/browser loaded media ahead of the attached current playhead. Prepared successors never contribute. |
| `Presentation` | – | ✓ | ✓ | ✓ | text | grid | all | The player's observed presentation state; it does not infer a server or network cause. |
| `Last advance` | – | ✓ | ✓ | ✓ | millis | grid | all | Age of the last observed film-clock or frame advance. Unavailable until an advance has been observed. |
| `Server response rate` | – | ✓ | ✓ | ✓ | bitrate | grid | all | Server-reported completed-response rate; "· idle" appended when delivery has gone quiet. Completion is not proof of receipt or decode. |
| `Server responses completed` | – | ✓ | ✓ | ✓ | bytes | grid | all | Server-reported completed response bytes for this session. Never labelled Transferred. |
| `Delivery idle` | – | – | – | ✓ | millis | grid | all |  |
| `Status sample age` | – | – | – | ✓ | millis | grid | all | Age since this client received the currently displayed server sample. |

**NETWORK**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Observed download rate` | – | ✓ | ✓ | ✓ | bitrate | grid | all | The player's own throughput estimate. |
| `Stream rate` | – | ✓ | ✓ | ✓ | bitrate | grid | all | The declared rate of the rendition being played. |
| `Transferred` | – | – | – | ✓ | bytes | grid | apple | Client access-log bytes — a different number from Delivered, so a different label. |
| `Requests` | – | – | – | ✓ | count | grid | apple |  |
| `Started in` | – | – | – | ✓ | seconds | grid | all | Time to first frame. |

**SERVER**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Server state` | – | ✓ | ✓ | ✓ | text | grid | all | always shown One vocabulary: No server-side session · Active · Holding buffer · Served from cache (+ the VOD states on the web). Debug shows it too. |
| `Encoder` | – | ✓ | ✓ | ✓ | text | grid | all |  |
| `Encode speed` | – | ✓ | ✓ | ✓ | speed | grid | all | "(avg)" appended when only the cumulative figure exists. Never abbreviated to Encode. |
| `Production actual` | – | ✓ | ✓ | ✓ | seconds | grid | all | Actor-owned media produced beyond accepted client demand. This is producer control, not a client or server-ready buffer. |
| `Production target` | – | ✓ | ✓ | ✓ | seconds | grid | all | Actor-owned pacing target. Advisory policy is never relabelled as measured buffer. |
| `Producer` | – | ✓ | ✓ | ✓ | text | grid | all |  |
| `Fetch reserve` | – | – | – | ✓ | seconds | grid | all | Compatibility frontier measured beyond the last fetched segment, not beyond the playhead. |
| `Fetch reserve bytes` | – | – | – | ✓ | bytes | grid | all |  |
| `Produced` | – | – | – | ✓ | clock | grid | all |  |
| `Pacing` | – | – | – | ✓ | speed | grid | all |  |
| `Held` | – | – | – | ✓ | yesno | grid | all |  |
| `Hold reason` | – | – | – | ✓ | text | grid | all |  |
| `Suspend count` | – | – | – | ✓ | count | grid | all |  |
| `Demand window` | – | – | – | ✓ | text | grid | web |  |
| `Request idle` | – | – | – | ✓ | seconds | grid | all |  |
| `Last request` | – | – | – | ✓ | text | notes | all |  |
| `Playlist` | – | – | – | ✓ | text | grid | all |  |
| `Published end` | – | – | – | ✓ | millis | grid | all |  |
| `Fetched end` | – | – | – | ✓ | millis | grid | all |  |
| `Control` | – | ✓ | ✓ | ✓ | text | grid | all | Playback-control session owner and state; timing and failure are notes under the same label in Debug. |

**SURFACE**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Surface` | – | ✓ | ✓ | ✓ | text | grid | all | What is drawn over the picture right now — none · indicator · banner · blocking. |
| `Fault` | – | ✓ | ✓ | ✓ | text | grid | all | The fault class the surface is projecting: preparing · buffering · recovering · hold · degraded · refused · exhausted · stopped. |
| `Source` | – | – | – | ✓ | text | grid | all | The raw event the fault came from, as named in tests/playback/playback-surface-contract.json. |
| `Attached/intent` | – | – | – | ✓ | text | grid | all | The media generation the fault is about and the viewer request it belongs to, if any. |
| `History` | – | – | – | ✓ | text | notes | all | Last 16 faults: class · source · raised → cleared (by) · player at raise (rate, position, presenting, stopped_by_owner). |

**PREPARED SWITCH**

| Row | mini | standard | details | debug | Format | Placement | Available on | Note |
|---|---|---|---|---|---|---|---|---|
| `Frames at the switch` | – | – | – | ✓ | text | grid | all | Dropped frames counted over the two seconds either side of a prepared commit, with how much of that window the samples cover. An observation, never a verdict: the bar it is measured against lives in docs/playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md. |
| `Audio at the switch` | – | – | – | ✓ | text | grid | all | Audio discontinuity over the same window — Apple access-log stalls, Android audio-sink underruns, and on the web the reason the analyser probe is deliberately not on the audible path. |
| `Tap to new quality` | – | – | – | ✓ | millis | grid | all | Wall time from the viewer's tap to the successor's first frame. Reported, never judged. |

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
