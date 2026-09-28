# Live TV on Apple TV — the focus graph, and why every press is reversible

**Status:** built · **Reported:** 2026-09-27 ("the ui navigation for live tv
on apple tv is so bad that you can almost consider it broken") · **Written:**
2026-09-27 · Companion to
[LIVE-TV-NATIVE-LAYOUTS-STATUS.md](LIVE-TV-NATIVE-LAYOUTS-STATUS.md) (what
the screens look like) — this is *where a remote press goes, and why*.

Read this before touching `focusedControl`, `LiveTvGuideGrid`,
`LiveTvGuideFocusNavigator` or anything under `liveTvRemoteAdapter` in
[LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift). The rule the
page follows is one sentence: **every direction press either moves focus to a
named neighbour or is deliberately dead; the engine never guesses.** The
failures Paul reported were every place that rule was not kept.

## 1. What was wrong, in the viewer's terms

| Press | Before | After |
|---|---|---|
| Fullscreen pills, Select on **Info** or **More** | Nothing. The sheets hung off the root view, which was already presenting the cover, so they could not present. The flag stayed `true`, the overlay stopped auto-hiding, and the sheet popped up later over the browser. | The sheet opens over the cover; closing it returns focus to the pill that opened it. |
| Guide grid, **Up** from the first row | Toolbar "Guide" segment — skipping the picture and Watch/Record/Record series/Remind me between them. In the temporary guide it aimed at a pill that was not drawn, so focus went wherever the engine liked. | Watch (or the picture, or the toolbar, whichever is the first that exists) on the page; **Close** in the temporary guide; **Close** in Over picture. Down from any of those returns to the cell the grid last held. |
| Guide grid, **Right** past the last programme / **Left** from the channel header | Dead. | Pages the window later / earlier and lands on the first / last cell of the same row. |
| Guide grid, **Up** from the first channel header | Toolbar. | The paging chips, which sit directly above the header column. Down from a chip returns to the header. |
| Paging chip **‹** at the guide's first hour | The chip became `.disabled` under focus and focus was thrown away. | Chips are never disabled on television — dimmed and inert at the limit. Left/Right step over a chip at its limit. |
| On now, **Select** a channel row | Every row was `.disabled` while the tune ran; focus jumped to the toolbar and stayed there after the picture came up. | Rows stay focusable; `busy` is checked in the action. |
| On now, **Left** from the picture or an action | Whichever list row the engine found at that height — never the row you came from. | The row you came from (`tvFocusedChannelId`). |
| Fullscreen, **Guide** pill | Opened on whatever cell the page's grid last held — any channel, or none — and switched the page under the cover to Guide for good. | Opens on the channel you are watching; closing it puts the page back to the view you left, and focus on the Guide pill. |
| Fullscreen, **Channels** pill / **Back** to the browser | The restore write raced the cover's dismissal and was dropped. | Requested from the cover's `onDismiss`, on the channel now playing. |
| Guide cell, **Select** → programme sheet → Back | Focus at the top of the page. | Back on the cell the sheet was about. |
| Toolbar **Favorites** | Toggled the filter and jumped focus into the list — from the Guide page too. | Focus stays on Favorites. |

## 2. The focus graph

Three regions on the page and two on the cover. Solid arrows are explicit
moves the page makes by name; dotted arrows are the platform focus engine,
which is trusted only where geometry is unambiguous (a horizontal row, a
vertical list).

```
 PAGE (tvBrowseRegion)
 ┌──────────────────────────────────────────────────────────────────────┐
 │ toolbar   [On now][Guide][Recordings]   …   [Favorites][Search][Layout][More] │
 └──────┬──────────────────────────────────────────────────┬────────────┘
        ┆ engine (down)                                    ┆ engine (down)
   On now page                                        Guide page
 ┌────────────┐   engine (right) ┌──────────────────┐ ┌──────────────────────────┐
 │ channel    │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄▶ │ picture          │ │ stage: picture · Watch   │
 │ list       │ ◀──────────────  │   ▲ up / ▼ down  │ │   Record · Series · Remind│
 │ (engine    │  left: the row   │ Watch·Record·…   │ └───┬───────────────▲──────┘
 │  up/down)  │  you came from   └──────────────────┘     │ down: last cell│ up from row 0:
 └────────────┘                                            ▼               │ cells → Watch
                                                    ┌──────────────────────┴──────┐
                                                    │ ‹ Now ›   ← chips           │
                                                    │ hdr │ cell cell cell  ──▶ page later
                                                    │ hdr │ cell cell       (right past last)
                                                    │  ▲ up from row-0 header → Now chip
                                                    │ ◀── page earlier (left from header)
                                                    └─────────────────────────────┘
 COVER (fullscreenSurface)
   overlay hidden:  [reveal layer]  any direction → reveal · Back → browser
   overlay shown:   [Pause/Play][Guide][Channels][Info][More]   (engine, one row)
                        Guide → temporary guide:  [Close] ▲ up from row 0
                                                  ‹ Now ›  grid as above
                        Info / More → sheet on the cover → close → back to opener
```

Every box with a name in it is a `FocusTarget` case, and the enum has one
case per view — the page toolbar and the cover's pills used to share
`.guide`/`.channels`/`.more`, so a write meant for a pill could land on a
segment under the cover and be dropped.

## 3. The five rules the code keeps

1. **A region under `onMoveCommand` moves focus itself, to a key.** On tvOS
   the handler takes every direction; the engine does not move focus at all
   while focus is inside. So every focusable inside such a region has a
   `.focused(_, equals:)` key and the region's navigator names the
   neighbour. There are three such regions: the guide grid (`moveFocus`),
   the detail region beside the list / above the grid (`moveDetailFocus`),
   and the reveal layer. A focusable added inside one of them without a key
   is a trap with no exit — the paging chips were exactly that once.
2. **A write aimed at a view being inserted or removed in the same update is
   deferred, and a write after a presentation closes waits for `onDismiss`.**
   tvOS drops a `@FocusState` write to a view that does not exist yet, and
   to a covered presentation — a sheet animating out is still covering.
   So: `focusedControl = nil`, `await Task.yield()`, re-check the state,
   write (closing the temporary guide, revealing the overlay); and every
   sheet and the cover request their focus return from `onDismiss`, not
   from the `onChange` of the flag that started the dismissal. Closing the
   cover also clears any sheet it was still showing, so an Info panel with
   nothing to show does not re-present on the page.
3. **A requested restore survives the engine's accidents, in the grid.**
   `LiveTvFocusRestoreCoordinator(arrivalInvalidates: false)` for the grid:
   focus *arriving* on a cell does not cancel a pending restore, because
   inside an `onMoveCommand` region an arrival the coordinator did not
   order is the engine relocating focus after the focused view went away
   (the Guide pill removed under the finger that pressed it). The grid
   copies the requested position into its own `requestedTarget` the moment
   the request arrives, because that accidental arrival runs `onFocus` and
   the parent then remembers the accident. The restore is applied through
   `onChange(of: restoreTick)` against the *current* view, never from the
   yielded task's stale copy of `layout`. A remote press inside the grid
   still wins (`invalidateForNavigation`). The On now list keeps
   `arrivalInvalidates: true`: it has no adapter, so an arrival on a row
   *is* a press. On the page, an arrival on a toolbar or detail key within
   one second of a request is likewise the engine's (the cover handing
   focus back to the picture that opened it) and does not cancel.
4. **A focused control never becomes disabled under the viewer.** Rows check
   `live.busy` in the action; chips are dimmed and inert, not `.disabled`;
   the restore never targets an unwatchable row.
5. **Reverse is a round trip.** Wherever the page moves focus by name, the
   opposite press returns to the origin: cell ↔ Watch, header ↔ Now chip,
   picture ↔ list row, pill ↔ sheet, pill ↔ temporary guide.

## 4. What is deliberately dead

Down from the grid's last row, left from the stage's picture, right from the
last action, down from the On now actions. Each is a screen edge with nothing
beyond it; a press there does nothing rather than teleporting. The reminder
overlay's buttons (lower-left) are reachable only when focus is outside a
navigated region, which is a known gap, not an accident — putting them in the
graph means a key in every navigator for a control that is usually absent.

## 5. Verification

Unit and source tests, run on the tvOS simulator:
`clients/apple/Tests/LiveTvTests.swift` —
`testGuideFocusEdgesPageTheWindowAndReachTheChips` (the navigator's edges
and landings), `testFocusCoordinatorRejectsStaleRestoresAndTransfersTheBoundaryAtomically`
(rule 3), `testAppleTvLiveNavigationIsReversibleAndReachesEverything` (every
row of the table in §1, pinned to the source that fixes it),
`testFullscreenFocusDefaultsToPauseAndReturnsThereFromTheRevealLayer`.

Nothing here can be proved without a remote in hand. The physical pass is
the checklist in §1's *After* column, run top to bottom on an Apple TV with a
tuner; the GPT prompt that runs it is in the status page entry for this
work. Two things to watch that the simulator cannot show: whether the
`ScrollView` brings a programmatically focused cell into view when Down walks
past the visible rows, and whether the cover's `onDismiss` fires late enough
on tvOS 26 for the list restore to land.
