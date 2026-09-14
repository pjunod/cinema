# Native layouts — phone fixes and watch-first television

**Status:** open · **Updated:** 2026-09-14 · **Branch:** `codex/apple-layout-followup`

Companion to the [mobile audit](MOBILE-UI-USABILITY-AUDIT.md): this records
implementation and validation of the native follow-up after PR317 merged at
`55a2fce0`. Work is confined to an agent-owned clone. The user's screenshots
show the production problems; no screenshot is treated as proof of the fix.

## Changes and current evidence

| Surface | Implemented behavior | Evidence / remaining work |
|---|---|---|
| iPhone detail | Resume stays primary; labeled secondary actions occupy an adaptive grid instead of four separate full-height rows | iOS source parsing passes; Xcode compilation and visual check pending |
| iPhone Live TV | View menu and recording activity action; summary gets its own wrapping line; bottom status reserves content space | iOS source parsing passes; device check pending |
| iPad Live TV | Large player and programme/recording actions beside the channel list in landscape; stacked watch/list regions in portrait; Guide and Recordings open above the retained player | Source parsing passes; full compile and PiP/presentation check pending |
| iPad fullscreen | Existing guide grid opens over the picture; closing it keeps playback; selecting through the modal guide returns to inline playback | Source parsing passes; running-player validation pending |
| Apple TV Home | Small mixed shelves measure their complete card height, reserve focus clearance, and allow two title lines; missing artwork has a title/symbol fallback | tvOS source parsing passes; runtime focus/scroll check pending |
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

The local Xcode installation lacks its Developer Applications directory and
has an unaccepted license; its SDK has also moved beyond the repository-pinned
Xcode 26.6. No license was accepted or developer directory changed globally.
The configured Apple runner hostname `mba` does not resolve. A working Mac
address was requested while independent work continued.

An isolated Android TV emulator and debug APK were prepared, but the available
computer-use tool cannot select its standalone QEMU window. No visual pass or
playback evidence is claimed from that attempt.

Before merge: complete iOS/tvOS compilation on the correct toolchain; inspect
actual phone, tablet and TV layouts/focus and retained playback; claim current
mobile build numbers and per-change release notes; open the follow-up PR;
record corrective-history anchors as required; run the single final fast lane
and merge only its passing candidate. This branch has not been pushed, merged,
published to a store, or installed on a physical device.
