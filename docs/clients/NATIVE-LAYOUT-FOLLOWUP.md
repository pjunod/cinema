# Native layouts — phone fixes and watch-first television

**Status:** draft PR320; native compilation and Apple sample captures passed · **Updated:** 2026-09-14 · **Branch:** `codex/apple-layout-followup`

Companion to the [mobile audit](MOBILE-UI-USABILITY-AUDIT.md): this records
implementation and validation of the native follow-up after PR317 merged at
`55a2fce0`. Work is confined to an agent-owned clone. Draft PR: http://192.168.4.7:3000/noirr/plurx/pulls/320. The user's screenshots
show the production problems; no screenshot is treated as proof of the fix.

## Changes and current evidence

| Surface | Implemented behavior | Evidence / remaining work |
|---|---|---|
| iPhone detail | Resume stays primary; labeled secondary actions occupy an adaptive grid instead of four separate full-height rows | iOS compiled on M4 Air; iPhone 17 Pro sample capture shows two labeled action rows |
| iPhone Live TV | View menu and recording activity action; summary gets its own wrapping line; bottom status reserves content space | iOS compiled on M4 Air; iPhone sample capture shows the complete toolbar and summary |
| iPad Live TV | Large player and programme/recording actions beside the channel list in landscape; stacked watch/list regions in portrait; Guide and Recordings open above the retained player | iOS compiled on M4 Air; 13-inch iPad sample capture shows adjacent player and channels; PiP/presentation check pending |
| iPad fullscreen | Existing guide grid opens over the picture; closing it keeps playback; selecting through the modal guide returns to inline playback | iOS compiled on M4 Air; running-player validation pending |
| Apple TV Home | Small mixed shelves measure their complete card height, reserve focus clearance, and allow two title lines; missing artwork has a title/symbol fallback | tvOS compiled on M4 Air; Apple TV 1080p sample capture shows complete title/metadata height; moving focus/scroll remains unverified |
| Android TV / tablets | Large player beside channels; portrait tablets stack the panes; wrapping DVR controls; Guide preview constrained by both width and height; touch Recordings overlays the retained player | Final reviewed source compiles and packages with `:app:assembleDebug`; visual/runtime check pending |

No unit tests have run for this follow-up. Native compilation is separate from
the user's single final fast lane. Apple parsing does not type-check SwiftUI
or prove a working application.

## Decisions made while implementation continued

1. **Keep the same playback owners.** Apple retains one attached player layer;
   Android moves the existing player content. No new tuner, session, polling
   loop, or background recorder was introduced.
2. **Use available width.** iPad switches to side-by-side panes at 850 points;
   Android tablets at 840 dp. Narrower tablets stack. TV retains remote focus
   behavior and uses the wide arrangement.
3. **Use overlays for secondary tablet work.** Opening Recordings or Guide
   must not discard an active player or PiP attachment. Saved tablet Guide and
   Recordings selections become overlays above an explicitly On now base.
4. **Bound eager shelf measurement.** Apple TV shelves of at most 40 items
   measure all cards so mixed metadata fits. Larger season shelves remain lazy
   to avoid downloading and decoding every episode thumbnail on entry.
5. **Keep this expansion focused on Live TV.** The user explicitly added Android
   TV/tablet Live TV and the photographed Apple TV Home defects. Broader iPad
   Home/title redesign has not been added without a scope reply.
6. **Keep features available.** These are direct layout changes, with no rollout
   gate or new readiness prerequisite.

## Single adversarial review

One independent, read-only review covered the native diff against `55a2fce0`.
It reported six P2 findings and no P1 finding. The following are source fixes,
not a claim that device acceptance has passed.

| Finding | Disposition |
|---|---|
| N1: portrait Android Guide picture consumes details width | Cap preview by 48% of width and 45% of available height; make details scroll within their own region |
| N2: tablet Recordings removes the player while retaining audio/tuner | Present Recordings above the retained iPad/Android tablet player |
| N3: iPad Guide requests fullscreen below a sheet | Close the guide into inline playback; route programme/activity sheets through their active presenter |
| N4: eager Apple TV shelves decode unbounded season artwork | Restrict eager measurement to at most 40 cards; preserve lazy large shelves |
| N5: tablet channel text exceeds the TV's 36 dp row | Replace the fixed row height with an intrinsic row and 48 dp minimum |
| N6: regular iPad shows On now while labeled Guide | Normalize saved or resized Guide/Recordings modes to explicit overlays and an On now base |

## Validation and delivery remaining

Android compilation and debug APK packaging succeeded with the installed SDK,
repository Gradle wrapper and Android Studio JDK. Existing deprecated Volume
icon warnings remain. No unit suite was executed.

The M4 Air is reachable through `pauls.macbook.air.lan`. Its formerly
documented numeric address `192.168.5.115` stopped responding later in this
session; the hostname remained usable. Both
`plurx-iOS` and `plurx-tvOS` compiled successfully at source `298eced8`, with
Xcode 26.6 (17F113), in an isolated source-only extraction. No credential or
`.git` directory was transferred. The final release candidate `27e2805f` also compiled both Apple targets
successfully, with Apple build 160. Android build 99 packaged successfully
from the same candidate. XcodeGen is the pinned 2.46.0.

Screen Sharing to the numeric address failed; that does not establish the
current host's sharing configuration. The local desktop then locked and the UI
tool requested manual unlock. Permission to use `simctl` for visual captures
was requested because the computer-use tool requires explicit user authorization
before another UI-control method is used. No sharing setting was changed.

An isolated Android TV emulator and debug APK were prepared, but the available
computer-use tool cannot select its standalone QEMU window. No visual pass or
playback evidence is claimed from that attempt.

The Forgejo create endpoint ignored `draft: true`; applying its `WIP:` title
convention immediately made PR320 draft. The prematurely allocated run 2085
(API run 2103) failed at release counters and history policy before the test
steps. Final single-lane execution is still pending. The history check found
an already-merged runtime correction `40d7c22d` with no mapping; this PR adds
its missing mapping to its existing SQLite regression, without changing Rust.
Existing native source guards were updated for the renamed wide browser and
touch-only semantic typography; no extra unit suite was introduced or run.

Before merge: run the single final fast lane and merge only its passing candidate. The device acceptance limits below remain explicit follow-up work. PR320 is draft. No app has been published to a store or
installed on a physical device by this follow-up.


## Simulator captures — September 14 evening

After the Air became reachable again, the disposable iPhone 17 Pro, 13-inch
M4 iPad Air and 1080p Apple TV simulators rendered the production SwiftUI
views with a temporary sample-data harness. The task's native screenshot
gallery contains iPhone detail and Live TV, iPad Live TV, and Apple TV Home.
The harness changes only the copied source on the Air; it is not a product
feature, a rollout gate, or part of this pull request. Artwork and video are
empty, and recording/selected-channel states are samples.

The TV capture exposed one additional horizontal truncation: movie year and
remaining time competed with the resolution label. TV cards now put resolution
on its own line and allow two metadata lines. The refreshed capture shows
“2023 62m left” in full. Both Apple targets compiled with this adjustment in
the capture build; the release source is compiled separately after removing
the harness.

These screenshots establish the pictured static layouts, not playback,
retained PiP, guide transitions, remote focus movement, or Android rendering.
Those checks remain unverified and must not be represented as acceptance
results. No unit suite was run for capture preparation.
