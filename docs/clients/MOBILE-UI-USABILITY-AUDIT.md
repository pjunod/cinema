# Mobile and television usability — scope, findings and delivery

**Status:** open · **Reviewed:** 2026-09-14 · **Branch:** `codex/ui-usability`

Extends the [web usability audit](WEB-UI-USABILITY-AUDIT.md) to iPhone,
iPad, Apple TV and Android. The user explicitly included Apple TV. The
[implementation ledger](WEB-UI-IMPLEMENTATION-STATUS.md) records review,
verification and merge status; this document distinguishes observed source
behavior from device evidence.

## Make the next action visible without losing platform conventions

Touch layouts need readable content and reachable controls at narrow widths.
Television needs a visible focus target, generous spacing and a predictable
route back to the previous shelf. A scaled desktop page is insufficient for
either. Preserve the existing player, offline and remote-input contracts.

| ID | Surface | Source finding | Change / disposition |
|---|---|---|---|
| M01 | iPhone / iPad series | Continuation was resolved only on tvOS; touch users had to descend through seasons | Share the existing episode resolver, show loading and an explicit unavailable state, and name the target below Play / Resume |
| M02 | Apple recording filters | Seven buttons and a summary shared one row, regardless of width | Native recording-view menu on touch; scrollable focus section on TV; count summary on its own line |
| M03 | Apple continuation cards | Episode details competed with remaining time in one small line | Separate the episode and remaining time; allow two title lines; use caption rather than caption2 |
| M04 | iPhone navigation | Eight tabs put Search and secondary destinations under the system More menu | Add a 44-point Search shortcut and More destinations menu to Home; preserve existing tab navigation, offline launch and iPad sidebar behavior |
| M05 | Apple recording visibility | Recording activity required navigating away from Home | Home uses the shared recorder observation to link to capture activity; no additional observation loop |
| M06 | Apple TV recording activity | Plain recording-card button style supplied no custom focus indication | Use the established readable TV button style; keep one focusable destination per card |
| M07 | Apple library / search | Watch filters had no in-page reset; no local title narrowing; search order buried exact matches | Add title narrowing and Clear filters; put exact search titles first; discard superseded search responses |
| M08 | iPhone detail actions | Download, watched and channel controls competed in one row | Fit the row when possible and stack when it cannot fit; keep playback above secondary actions |
| M09 | Web phone layouts | Theater navigation extended beyond the viewport; the recording badge crowded its top bar | Six visible navigation destinations in a compact grid; recording status owns a second header row; full synopsis remains accessible through About |
| M10 | Android phone detail | Main and secondary actions share a horizontally scrolling LazyRow | Follow-up design: primary Play / Resume owns a full-width row; secondary actions wrap underneath, with downloads retaining their state |
| M11 | Android / Apple comprehensive coverage | Player, reader, download recovery, channel editor, settings and very large text need scenario-specific review | Explicit follow-up coverage; source review is not a claim of device validation |

## What is being verified

The Apple source compiles with Xcode 26.6 (17F113), XcodeGen 2.46.0,
iOS and tvOS simulator targets. Build 159 claims this change. No unit tests
run during implementation; the ready PR runs the fast lane after the single
adversarial review and its fixes.

Actual web HTML and compiled native views are inspected against a temporary,
loopback-only API fixture. Fixtures use synthetic titles, watch positions and
recording observations. They are not evidence that a live tuner, media file,
download or playback session succeeded. Missing artwork is an intentional
empty-artwork case, not a visual rendering of production posters.

The live simulator/browser record and limits belong in the implementation
ledger. Do not describe Android as visually verified until an Android device
or emulator has been inspected. Do not describe every Apple surface as
redesigned: this slice addresses the concrete findings above while preserving
the established player and navigation contracts.

## Next native layout pass

Prioritize the Android playback action row, then the native channel editor
and recording-detail confirmations at large text sizes. On Apple TV, inspect
focus travel from the last item of each shelf, Back after playback, long
programme names, stale DVR observations and dialogs. On iPad, compare full
screen, narrow split view and landscape: horizontal size class alone is not
proof that every action row fits its actual available width.

These are recorded follow-up candidates rather than hidden feature gates.
