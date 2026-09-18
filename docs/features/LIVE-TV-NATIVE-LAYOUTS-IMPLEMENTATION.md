# Native Live TV layouts — three TV presentations, one player

**Status:** ready to build · **Decision:** Paul approved selectable TV layouts
and channel metadata on 2026-09-09 · **Written:** 2026-09-09

This implements the Live TV design discussion: Guide + preview, Guide over
picture, and Channel browser on Apple TV and Google TV, with a compact touch
presentation on iOS and Android. Read the contracts below, then execute the
three work packages in §8. This is one bounded delivery, not a new client
framework or a programme of exploratory milestones.

Companion to [the existing Live TV plan](LIVE-TV-GUIDE-AND-UI-PLAN.md),
[the player input contract](../clients/PLAYER-INPUT-CONTRACT.md), and
[the development pipeline](../DEVELOPMENT_PIPELINE.md). This plan supersedes
the old plan's TV-grid exclusion and prohibition on a TV preview player.
It does not supersede tuner ownership, cleanup, authentication, or playback
transport contracts. The three designs in the conversation are illustrative;
the dimensions, states, and metadata rules here are the build specification.

## 1. Deliver the whole choice without feature gates

Every TV viewer gets **Layout → Guide + preview · Guide over picture ·
Channel browser** in Live TV. Default to Guide + preview. Make the same
preference accessible in ordinary viewer settings. All three ship together;
none requires a developer toggle, build flag, environment variable, account
allowlist, experimental mode, or rollout-readiness check.

Changing layout takes effect immediately, preserves the current channel and
playback session, and saves locally. It must not start or stop playback,
reset filters, trigger another guide fetch, or return focus to the app's Home
tab. A layout controls presentation, not access to a capability.

Live TV itself remains a visible destination when disabled or unconfigured.
The existing Settings → Developer configuration may expose one runtime
Enable/Disable control and the actual requirements with met/unmet reasons.
Reuse that surface; do not add another activation system. Actual tuner
capacity, credentials, DRM, decoder availability, and safe session ownership
still determine whether a requested operation can succeed. Report those
failures explicitly; do not hide layouts or classify a device as ineligible.
An unavailable guide leaves a working channel browser inside every layout.

**Scope boundaries:** no DVR, rewind, recording, scheduled tuning, new tuner
provider, 4K delivery upgrade, surround passthrough upgrade, app-wide navigation
redesign, VOD player rewrite, new theme engine, or background channel scanning.
The existing transport can deliver 720p or 1080p H.264/AAC. Displaying a 4K or
surround *source* is not a promise to deliver that format.

## 2. What exists and what must change

Re-verify these source anchors against the integration base before editing.
These are source observations, not a physical-device acceptance report.

| Surface | Existing implementation | Required change |
|---|---|---|
| Apple TV | [LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift) forces list mode on tvOS and presents fullscreen when playback starts. | Add three selectable presentations; starting or changing channel must not override the viewer's chosen browse presentation. |
| Apple guide | The same file contains a two-axis scroll view holding both times and channel labels. | Pin labels and time header; add deterministic TV focus and variable-duration cells. |
| Google TV | [LiveTvScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt) excludes the grid on television and renders a separate now bar outside fullscreen video. | Enable TV guide, consolidate player chrome, and make fullscreen a real edge-to-edge picture. |
| Android input | `applyOutcome` returns true for `FocusRow` and `Activate`; [LiveTvKeyAdapter.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvKeyAdapter.kt) returns that from `onPreviewKeyEvent`. | Fix event consumption: delegated focus/activation must reach the framework exactly once. Reproduce with real D-pad events. |
| Apple input | [PlayerRemoteAdapter.swift](../../clients/apple/Sources/PlayerRemoteAdapter.swift) installs Live TV move/tap handlers at the root, unlike the VOD adapter's scoped handlers. | Scope interception to the surfaces that own it; do not intercept ordinary button activation as a second gesture. Check on a Siri Remote. |
| Metadata | [Apple](../../clients/apple/Sources/LiveTv.swift), [Android](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt), and [web](../../crates/plurxd/src/web/live-tv.js) expose HD/SD and video/audio codec badges. | Preserve those facts everywhere and add measured resolution/audio-layout facts under §5. |
| Mobile | Player, technical facts, status, administrative actions, filters, and search compete with the guide. | Compact player and browse controls; move diagnostics to Info and recovery controls to the error that needs them. |

Keep the existing guide reducers, channel filtering, tune coalescer, and
player controllers. Extend their interfaces only where the new UI needs
state. Do not create a controller, lease, or metadata-fetch loop per layout.

## 3. The three TV layouts

### 3.1 Shared visual and information grammar

Use the existing theme tokens and native typography. At 1920×1080, start with
64 px horizontal and 48 px vertical safe insets, a 28–32 px programme title,
22–24 px row copy, and 18–20 px secondary copy/badges. These are design-space
sizes: convert for platform density rather than treating Android dp as
physical pixels. Check the result from normal sofa distance.

Use restrained surfaces, a clear accent focus outline, and a separate
**Watching** marker. Focus and playing are independent states. Prefer a ring
and surface change to enlarging a grid cell into its neighbours. Retain
native focus/accessibility behavior and sufficient contrast in every existing
theme. Long programme titles truncate in cells and expand in the detail area;
channel number, source-quality badge, and audio badge remain readable.

The common toolbar contains **Guide · On now · Favorites**, **Search**,
**Layout**, and **Return to live** when playing. View choice and layout choice
are distinct: Guide chooses schedule content, while Layout arranges picture
and browsing. A full Guide is reachable from all three presentations.
Search matches channel number, name, and programme on now, using the existing
filter semantics. Do not add recommendation rails, category taxonomies, or
an editable favorites backend. Favorites uses the existing lineup property.

Move **Refresh channels** and **Hide protected** into a small More menu.
Keep protected channels visible by default with an explicit reason. Keep
**Info** available from a programme/player, with stream facts under §5.
Do not put signal meters or cleanup jargon permanently over a broadcast.

### 3.2 Guide + preview — default

```text
 Live TV      Guide  On now  Favorites      Search  Layout  More
 ┌────────────────────────────────────┬───────────────────────┐
 │ Focused programme                  │ Current live picture  │
 │ Time · synopsis · source badges    │ Watching 2.1           │
 └────────────────────────────────────┴───────────────────────┘
 Channel       7:00           7:30            8:00
 2.1 [HD]      ├──── Programme ──────────────┤ Next
 4.1 [HD]      ├── News ─────┤ Interview ────┤ Film
 5.1 [4K]      ├──────────── Game ────────────────────────┤
```

The guide owns roughly the lower two-thirds of the content area. Target
6–8 visible channel rows, 76–88 design px per row, a 190–220 px channel
column, and 90 minutes of visible time. Programmes have proportional widths;
do not split a one-hour programme into two independently focusable half-hours
as the illustrative mockup did. Time ticks remain at 30-minute intervals.

The upper detail region follows focus. The preview keeps the *playing*
channel, even when another channel is focused. Selecting an airing programme
starts that channel in the preview and keeps the guide open. **Return to live**
expands the picture. Before the first tune, use the preview region for the
selected programme's metadata; no automatic tune and no invented video.

### 3.3 Guide over picture — watch while browsing

The same guide renders in an opaque lower panel over the continuing picture.
Target 4 visible rows and 90 minutes; retain an unobscured upper picture.
An opaque panel guarantees legibility over arbitrary video. Programme details
occupy one compact header within the panel. Keep the current channel's audio.

This layout is selectable as the normal browse presentation. Also expose
**Guide** from fullscreen playback in every layout; that temporary overlay
uses this presentation without changing the saved layout. The open guide
never auto-hides. Explicit Back/Close dismisses a temporary guide.

With no playing channel, render the guide on a normal opaque background.
Selecting an airing programme starts video behind it and keeps the guide open.

### 3.4 Channel browser — large picture and quick surfing

Use approximately one-third of the width for a vertical On now list and
two-thirds for the current picture, focused-programme facts, and upcoming
schedule. Target 5–7 list rows. Each channel is one focus stop, including its
number, name, programme, progress, next title, and source badges. Avoid a
separate tiny Watch button inside each row.

Moving focus updates programme details and the schedule; it does not retune.
Select tunes the channel. The Guide toolbar action opens the full grid with
the selected channel/time retained. Returning to On now restores its row.
Do not fit a second miniature grid beside the picture.

## 4. One navigation and session model per platform

### 4.1 State and preference contracts

Implement equivalent native models in Swift and Kotlin. The names below are
proposed interfaces, not existing declarations:

```text
TvLiveLayout = guide_preview | guide_overlay | channel_browser
LiveBrowseState = layout + mode + query + favoritesOnly + hideProtected
                + focusedChannelId + focusedProgrammeId + anchorTime
                + windowStart + verticalPosition + returnTarget
LivePresentation = browser | fullscreen_hidden | fullscreen_controls
                 | temporary_guide | menu | programme_details | stream_info
```

`playingChannelId`, start state, and lease remain owned by the existing
controller. Never derive the playing channel from focus. Resolve focus by
stable channel/programme identity and UTC time, not list index or cell width.

**Proposed persisted keys:** Apple `plurx.liveTvLayout`; Android
`live_tv_layout` in the existing SettingsStore. Use the stable values above,
not localized labels. Missing/unknown values fall back to `guide_preview`.
Save locally per device through the existing settings mechanism. No server
setting, migration, or cross-device synchronization is needed. Leave the
existing phone `liveTvView` preference independent; do not reinterpret a
stored phone list/grid choice as a TV layout. Add enum decoding and persistence
to [Apple settings](../../clients/apple/Sources/SettingsStore.swift) and
[Android settings](../../clients/android/app/src/main/java/tv/plurx/app/data/SettingsStore.kt).

### 4.2 Input rules and escape paths

Extend the `live` section of
[the shared fixture](../../tests/playback/player-input-contract.json) with the
needed browse/menu/detail states and update each client's transcription.
Keep existing desktop/touch player behavior unchanged unless §6 explicitly
changes a touch browse presentation. Generate the input document's tables
with [player-contract-table](../../scripts/player-contract-table); do not edit
its generated rows. Keep [player-input-fence](../../scripts/player-input-fence)
intact: platform key decoding still has one adapter per platform.

| Context | Required behavior |
|---|---|
| Fullscreen, controls hidden | First direction or Select only reveals controls; it must not tune or move focus behind the picture. Initial control focus is Guide; hardware Play/Pause retains its existing action. |
| Fullscreen, controls visible | Arrows move among Guide, Channels, Pause/Play live, Info, and More; Select activates only the focused control. Back hides controls. Four seconds of genuine inactivity hides these controls only while playing and no panel is open. |
| Guide | Left/Right select adjacent programmes. Up/Down select the programme covering the same anchor time in the adjacent channel. Crossing a short programme must not make vertical focus drift through the evening. |
| Guide boundary | Up from first row enters the toolbar; Down returns to the remembered cell. Left from the earliest cell enters the channel header, then a boundary stops movement. Past/future movement respects the fetched window and exposes available edge actions. |
| On now / Channel browser | Up/Down move one channel; Left/Right reach adjacent regions deliberately. Focus never tunes. Select on a playable row tunes once. |
| Airing programme | Select tunes once, or expands the picture if that channel is already playing. It does not restart the same session. |
| Future or past programme | Select opens programme details; no record, schedule, rewind, or watch-from-start action. Back closes details and restores the exact cell. |
| Missing programme data | A focusable channel header or full-width "No programme information · Watch live" cell still tunes that channel. Never make empty guide rows inaccessible. |
| Layout / More / Info | Focus stays within the open panel. Back closes it and returns to its opener. No auto-hide while a panel is open. |
| Temporary guide opened over fullscreen | Back restores fullscreen on the same session. The saved layout does not change. |
| Fullscreen hidden, Back | Return to the saved root browse layout with channel and focus retained. |
| Root browser, Back | Leave Live TV and release playback through the existing controller. This makes repeated Back escape the feature rather than alternate forever between browser and fullscreen. |

The visible **Return to live** action expands the current picture; **Stop**
under More stops playback but keeps browsing. **Leave Live TV** under More
provides an explicit exit as well. The layout selector works from the root
browser and fullscreen More. Closing it restores its opener in the new layout.

For Android, return false when delegating an unhandled key to Compose;
consume true only after performing an action or explicit focus move. For
Apple, restrict move/tap interception to the reveal surface and custom grid
navigation that actually requires it. Do not intercept a button's Select
again at the root. Key repeat moves focus; activation must not fire on both
key-down and key-up. Preserve the existing 350 ms tune coalescer.

On filter changes, keep the same channel if visible, otherwise choose the
nearest visible channel. On an empty result, move focus to Clear filters.
On refresh, preserve channel and anchor time; if a programme disappears,
choose the cell covering that time. No refresh may steal focus from a menu.
VoiceOver/TalkBack announces channel, programme, time, source facts, and
whether it is focused versus currently playing.

### 4.3 Keep one player alive while its surface moves

A layout switch, temporary guide, fullscreen transition, or programme detail
is presentation-only: zero session POSTs/DELETEs and no new decoder. Keep
heartbeat, refresh, error state, and tune cancellation in the existing
[Apple controller](../../clients/apple/Sources/LiveTvView.swift) and
[Android controller](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt).

Prefer a single persistent player host with changing geometry and panels.
On Apple, do not attach one AVPlayer to two simultaneous PlayerSurface
instances; existing layer/PiP ownership makes that destructive. On Android,
do not let switching layout dispose the only retained PlayerView or invoke
`stopUnlessRetained`. Teardown belongs to leaving Live TV/background policy,
not a child view disappearing. Keep the existing PiP and profile-change
cleanup behavior. During a tune, retain the browser, focus, and visible
"Starting <channel>" state instead of bouncing through fullscreen covers.

### 4.4 Guide range and work bounds

Reuse the guide reducer's interval clipping, channel matching, and freshness
states. Keep channel labels pinned horizontally and time headings pinned
vertically; all programme rows share one horizontal offset. Keep one now line.
A current-programme progress bar is informational, never a seek control.

Start at the half-hour containing now. Request six hours through the existing
`from`/`hours` API, render 90 minutes, and page within the response. Apple
already has `guide(from:hours:)`; extend Android's parameterless `guide()`
with equivalent optional parameters. At the fetched edge, fetch another
bounded window only if within the server's published guide availability.
A Now button restores the current window. Show "No further schedule" at the
provider boundary; do not imply the server has 72 hours because the request
parameter permits it. Render cached data while refreshing.

Use lazy/virtualized rows with stable identities and a small overscan region.
Resolve an offscreen focus target by scrolling/composing it before requesting
focus; never retain a FocusRequester attached to a disposed child. Compute
programme geometry on guide/window changes, not on every remote press.
No network request on focus, no per-layout polling, no per-row timers. Retain
the existing guide refresh cadence and a single clock update for now/progress.

## 5. Channel metadata must say what is known

### 5.1 Existing contract and the small missing extension

The lineup currently carries these optional facts, from
[the server](../../crates/plurxd/src/live_tv.rs):

```text
hd: bool?                  # true = HD, false = SD, absent = unknown
video_codec: string?       # source codec reported by the tuner
audio_codec: string?       # source audio codec; not a channel-layout claim
```

The current web `channelBadges` uppercases codec labels and omits missing
values. It has
no measured source dimensions or audio channel-layout field. Do not infer
4K from HEVC, channel number, ATSC generation, or a tuner's product name.
Do not infer 5.1 or Atmos from AC-3, E-AC-3, AC-4, or AAC.

Add this **optional** `source_format` object to `LiveTvChannel`, therefore to
lineup rows and the `channel` member already present in session status:

```json
{
  "source_format": {
    "video_width": 3840,
    "video_height": 2160,
    "scan": "progressive",
    "audio_channels": 6,
    "audio_layout": "5.1",
    "observed_at": 1788998400
  }
}
```

All fields except `observed_at` may be absent; `scan` accepts `progressive`,
`interlaced`, or omission. The example is an illustrative measured source,
not a claim about the deployed tuner. Dimensions must be positive and bounded
at 16384; audio count 1–32; layout a normalized, bounded string. Reject an
invalid field independently without failing the channel. Old servers omit
the object; new clients still show the existing HD/SD/codecs. Old clients
ignore the additive object. No new protocol-version eligibility flag.

**Bounded acquisition:** collect input format facts from the *existing*
FFmpeg producer's initial input stream description, associated with the
selected `0:v:0` and `0:a:0` maps. Inspect the actual producer stderr before
implementing its parser; its current `warning` log level will not supply an
input description. Use a bounded input-descriptor capture with info logging
on that producer and the existing continuously drained stderr path. Parse
only the input block, never the output stream description. Preserve existing
error detection, redaction, pipe draining, startup timeout and probe budgets.
Keep at most 64 KiB of descriptor text and stop parsing after the initial
stream-mapping boundary; do not stop draining stderr. An unrecognized format
produces missing facts, not a playback failure or invented defaults.

There is no separate ffprobe against the tuner, no extra tuner connection,
no input-stream tee, and no tune-on-focus. The parser is required to decode
captured descriptors for the pinned FFmpeg software and hardware paths;
fixtures must include interlaced HD, 2160p, stereo, and 5.1. This is one
metadata adapter within work package 1, not a media-analysis subsystem.

Expose current-session observations through status without delaying first
picture. Cache observations in memory on the owner, keyed by configuration
generation, device identity, and channel ID. Bound the cache to the current
lineup, invalidate on configuration/lineup removal, and expire after 20
minutes or the known programme end, whichever is earlier. Cached facts in
the browser are labelled "Last observed <time>" in Info; they do not become a
permanent promise about a channel. Do not add a database migration. Channels
never observed continue to show their existing tuner-reported facts.

Apple and Android status DTOs currently omit the server's `channel` member;
decode it and merge source observations into the display model for that
channel only. Fence late status results by active session/channel identity.
Use the same field precedence and display cases in the web helper, so web and
native clients retain metadata parity without redesigning the web layout.

### 5.2 Badges and where they appear

| Fact | Display rule |
|---|---|
| Source picture class | Measured height 2160: `4K`; above 2160: `4K+`; 720–2159: `HD`; below 720: `SD`. Exact dimensions stay in Info. Without fresh measurements use the explicit `hd` Boolean; missing is unknown. |
| Exact source format | Details/Info: e.g. `3840×2160p · HEVC` or `1920×1080i · MPEG-2`. Omit p/i if scan is unknown. Do not invent frame rate or HDR. |
| Source sound | Keep the tuner codec badge, e.g. `AC3` or `AC4`. Append `Stereo`, `Mono`, or normalized `5.1`/`7.1` only when measured. Six channels without a recognized layout is `6 ch`, not automatically `5.1`. |
| Delivered picture/sound | Player Info separates `Source` from `Playing`: e.g. `Source: 4K · HEVC · AC3 5.1`; `Playing: 1080p · H.264 · AAC`. Channel layout after conversion is unknown unless separately observed from the output/player. |
| Reception | Strength, quality, and symbol quality percentages in expandable Info, retaining missing versus zero. These are not quality badges. |
| Protection/availability | Explicit lock/reason beside the channel; details remain readable even when Watch cannot succeed. |

Compact channel rows and pinned guide labels always show the source picture
class and audio codec when known. The focused programme detail and player
header show the complete compact strip, including video codec and measured
audio layout. On narrow cells, put badges on the channel label or focused
detail, not repeated inside every programme. Info shows exact source,
delivery, freshness, and reception. Never remove metadata entirely to fit a
layout, and never use a source badge as a delivered-playback badge.

## 6. Phone and tablet finish within the platform work

Do not put a three-layout TV selector on a phone. Retain **On now · Guide ·
Favorites**, the existing theme, and ordinary app navigation.

- Portrait On now: one compact 16:9 player, title/live indicator, then channel
  rows. A row is the touch target; no repeated Watch buttons. Keep playback
  controls on the player and diagnostics in Info.
- Portrait Guide: default to the selected channel's vertical schedule with an
  obvious channel picker and day/time context. Keep an explicit Grid option
  for viewers who prefer the existing multi-channel grid; remember that
  phone-only choice. A compact player remains available while browsing.
- Landscape/tablet: offer the full grid with pinned channel/time labels and
  an appropriately sized picture. Do not fight an explicit guide choice by
  automatically entering fullscreen on every rotation. Fullscreen remains
  available from the player.
- Selecting a future programme opens details without tuning. Changing the
  schedule's channel picker changes browsing, not the playing channel.
- Keep metadata on rows and details using §5. Accessible text size may wrap
  metadata into a second line; it must not shrink to unreadable type.
- Preserve current system PiP behavior and existing stop/release policy when
  leaving Live TV. Cross-tab in-app docking is outside this delivery.

These are adaptations of the same browser/metadata components, not separate
mobile redesign projects. Check iPhone/Android portrait and landscape plus
one tablet-size rendering; do not build a new responsive-layout framework.

## 7. Verification is focused work, not another CI programme

The quality bar is correct behavior plus the current compile/static lane.
Write focused regression tests alongside changed logic. Exercise them during
development where useful; do not turn runtime suites into PR prerequisites.
Use the existing native test targets and shared fixture readers rather than
creating a new harness or generic navigation-testing framework.

| Concern | Focused evidence to retain |
|---|---|
| Remote routing | Actual D-pad events move focus and Select activates once; hidden input reveals only; menus/details close to their opener. Pure reducer tests alone do not prove this. |
| Grid navigation | Different programme durations, a gap, first/last row, window edge, offscreen row, filter removing focus, refresh crossing programme end, and empty results. |
| Presentation lifetime | Switch all layouts, open/close guide and Info, enter/leave fullscreen while playing: same session ID, no start/stop requests, no lost picture; a confirmed new channel selection starts only one replacement. |
| Metadata | HD/SD/unknown fallback, 2160p, interlace, codec missing, stereo/5.1/channel-count distinction, stale cache, unrelated late status, and input-versus-output descriptor parsing. Compare the web and two native badge helpers on the same cases. |
| Fallback | Guide off/stale/error/empty still allows tuning; disabled Live TV remains visible; protected channels explain refusal; all three layout choices remain visible. |
| Lifecycle | Channel start failure, rapid Select, sign-out, background, PiP return/stop, and explicit Stop preserve the existing lease semantics. Reuse existing tests rather than rerun the whole tuner campaign. |

Take one focused author walkthrough on physical Apple TV/Siri Remote and one
Google TV/D-pad: enter each layout, reach every toolbar/control, traverse past
the viewport, tune, inspect metadata, return, and leave. Record exact build,
device, commands/actions, and observed failures in the PR or work-package
notes. If a device is unavailable, record that limit; do not claim a simulator
or compile proves the hardware path. This walkthrough is author evidence,
not a new automated gate or an extra review round.

Use the separate manual sweep for the complete regression matrix at Paul's
cadence. Fix real behavior defects through this normal implementation effort,
not by disguising them as sweep maintenance. There is no new benchmark
campaign, soak-test prerequisite, or exhaustive device matrix in this plan.

## 8. Three implementation packages, then one integration closeout

Use one temporary `effort/live-tv-native-layouts` branch. Task branches use
`codex/` names and target the current effort. Keep each package cohesive;
there is no required PR per component, milestone review, or redesign panel.
If working serially, complete the packages in order. Platform packages may
proceed independently after package 1's contracts are committed.

### 8.1 Shared contract and metadata

**Change:** extend the live input fixture and its native transcriptions,
record the layout preference enum, add bounded source-format observation and
DTO fields under §5, extend the web badge helper, and establish shared metadata
cases. Update generated input tables and relevant source-contract assertions
that currently forbid a TV grid/preview. Do not simply delete those assertions;
replace them with the new invariants. Keep existing live guide cases and
playback state rules outside this scope intact.

**Files:** [server Live TV](../../crates/plurxd/src/live_tv.rs),
[web metadata helper](../../crates/plurxd/src/web/live-tv.js),
[web UI](../../crates/plurxd/src/web/index.html),
[shared input fixture](../../tests/playback/player-input-contract.json),
[Apple input](../../clients/apple/Sources/LiveTvInputRouting.swift),
[Android input](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvInputPolicy.kt),
and the DTO/settings files linked above. Add new narrow helper files only
where extraction makes those files smaller; no shared cross-language runtime.

**Completion:** the three clients interpret the same badge/input cases; old
server payloads still decode; observed source facts never come from the
transcoded output; metadata observation creates no extra tuner connection.
The pinned compiler loop is green before handing Rust source onward.

### 8.2 Apple TV and iOS

**Change:** create a persistent Live TV shell and compose the three TV layouts
from reusable guide, channel row, programme detail, metadata, and player
controls. Add native Layout preference UI, pinned guide navigation, scoped
remote adapter handling, and the compact phone/tablet changes. Remove forced
fullscreen-on-start and teardown-on-layout-disappearance paths.

**Files:** [LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift),
[LiveTvGuide.swift](../../clients/apple/Sources/LiveTvGuide.swift),
[PlayerRemoteAdapter.swift](../../clients/apple/Sources/PlayerRemoteAdapter.swift),
[SettingsView.swift](../../clients/apple/Sources/SettingsView.swift),
[LiveTvTests.swift](../../clients/apple/Tests/LiveTvTests.swift), and narrowly
extracted view/state files beside them as needed. Reuse
[PlayerSurface.swift](../../clients/apple/Sources/PlayerSurface.swift).

**Completion:** all layouts selectable and persisted; guide and every control
reachable; layout/fullscreen changes preserve the player; metadata present;
iOS remains usable in portrait/landscape. Compile both iOS and tvOS, advance
the Apple build counter and its generated records in this package, and update
[Apple parity](../clients/APPLE-CLIENT-PARITY.md) with actual evidence.

### 8.3 Google TV and Android mobile

**Change:** compose the same three presentations around the existing Media3
player, add native preference UI and pinned guide navigation, correct key
consumption, remove the second fullscreen now bar, and implement §6. Ensure
lazy-list focus restoration works beyond the first screen.

**Files:** [LiveTvScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt),
[LiveTvGuideUi.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvGuideUi.kt),
[LiveTvKeyAdapter.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvKeyAdapter.kt),
[SettingsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SettingsScreen.kt),
and existing Live TV unit/instrumentation targets. Prefer reusable native
components over copying composables three times.

**Completion:** the Apple package's observable outcomes hold on Google TV and
phone; D-pad events reach focus and button handlers exactly once. Compile the
Android app with the repository's pinned JDK/SDK, advance its store build
counter, and update [Android parity](../clients/ANDROID-CLIENT-PARITY.md).

### 8.4 Close the effort using the current fast lane

Follow [AGENTS.md](../../AGENTS.md) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) as they exist at execution
time. The 2026-09-09 process is intentional; do not revive the older workflow
from historical Live TV/layout plans.

1. Commit normally. No pre-commit hook, no hook installer, no automatic tests
   at commit. Task PRs into the effort have no required review or CI lane.
2. Freeze task merges, merge current main into the effort, and verify
   compilation against that exact integrated source. For Rust, establish the
   pinned 1.97.1 source-only compile loop **before editing**, retain a warm
   target directory, and use `git archive` without `.git` or credentials when
   the compiler is elsewhere. See [the compile loop](../ci/AGENT-COMPILE-LOOP.md).
3. Complete corrective evidence, path ownership, documentation, platform build
   counters, and version alignment in the changes that owe them. Do not make
   paperwork-only repair commits afterward. A workspace release must align
   both counters and both marketing versions; a client-only iteration does
   not invent a new workspace release.
4. Open the integrated main-bound PR **as draft**. Request **exactly one**
   adversarial agent review of the integrated result. Address every finding;
   the author verifies the fixes. No re-review, panel, or follow-up approval.
5. Mark ready; the fast lane starts automatically. Merge only when the
   **current head** has a green **Main promotion gate**. Returning to draft
   stops the ready-PR lane; there is no fast-lane label. The lane runs
   policy/static contracts and affected compilation,
   not runtime, browser, simulator, emulator, playback, or smoke suites.
6. Merge is not deployment. Normal main pushes no longer start the old fleet
   publisher. Use the explicit client deployment instructions in
   [CLIENT-DEPLOY-PROMPT.md](../clients/CLIENT-DEPLOY-PROMPT.md) and the release
   process in [RELEASING.md](../RELEASING.md); release tags retain their separate
   qualification/publishing path. Do not restore auto-publication to speed
   this work up.

Useful author commands from the repository root; run only for affected work:

```bash
make apple-build                         # iOS + tvOS compilation, no tests
make validation-lint                     # path ownership and catalog shape
make history-check                       # corrective evidence bookkeeping
make operations-check                    # docs and repository static contracts
node scripts/player-contract-table --write # regenerate input documentation
node scripts/player-contract-table --embed # regenerate embedded web tables
scripts/player-input-fence               # one platform input adapter
git diff --check                         # whitespace check
```

For Android use `./gradlew --no-daemon :app:assembleDebug` from the Android
project inside the pinned repository build image/runtime. `make android-test`
is a JVM/lint suite, not the compilation command. Use the Rust commands in
the development pipeline on the exact integrated snapshot; do not wait for
CI to discover compiler errors. The commands above are not a replacement
workflow or an expanded merge gate.

## 9. Done means shipped choices, usable navigation, honest metadata

The implementation is complete when all three layouts ship on both TV
platforms, choice persists, every advertised control is reachable, and
layout changes preserve the active session. All known channel format/audio
facts appear consistently, and unknown facts are never guessed. Phones and
tablets get the compact browse treatment, with working guide and metadata.

Update the old TV-grid exclusions in the existing Live TV plan and native
parity docs in the implementation commits; update the generated input contract
from its fixture. Record what actually ran and any device limitations. Mark
this document built only after the integrated change lands, and distinguish
merged, deployed, and physically verified in the delivery report.

Do not add a fourth layout, customization framework, tuning analytics,
background metadata scanner, or a second implementation-planning phase to
finish this work. Unexpected transport/product requirements belong in a
separate issue with a concrete explanation, while unaffected layout work
continues.

## 10. Coordination with the proposed Library channels

The separate Library-channel proposal may reuse this guide, playback chrome,
focus model, and TV/mobile presentation conventions. Guide + preview remains
the default common TV layout. Its creation/editing flow is separate and must
support web and mobile; it is not part of this Live TV implementation.

Reserve a presentation seam without building a multi-source framework now:
channel rows should accept an explicit source label, and action rendering
should receive the actions that the concrete source actually supports.
For this delivery the source is always the existing tuner and the actions
remain the tuner actions defined above. Do not require a new capability
endpoint, change existing wire channel IDs, or add dormant Library-channel
code to prepare for it.

Future Library channels may propose Tune in at the shared schedule offset,
Watch from start as personal file playback, and Return to channel. These are
not approved tuner capabilities. Source-aware actions must preserve that
boundary. Future shared schedules must not be silently reordered by profile
restrictions, and browsing a source/channel must not implicitly tune it.

Naming the combined destination Channels versus Live TV, adding source
filters, and choosing how Library playback transitions into personal playback
remain decisions for that feature. This delivery retains Live TV and needs
none of those decisions to finish. The separate design task has confirmed
alignment with the three TV layouts and first-class iPhone/Android browsing.
