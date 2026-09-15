# Playback information redesign

> **Status:** implementation complete; adversarial review and final qualification pending.
> **Updated:** 2026-09-15. **Scope:** web, iPhone, iPad, Apple TV, Android phones,
> tablets and TV. No feature flag or new enablement requirement.

## What changed

Playback info opens on an Overview with player-reported resolution, original
source, delivery method and reason, tracks, device buffer and interruptions.
Details groups related observations into expandable sections. Diagnostics keeps
all platform-supported contract fields and history. Compact shows three labeled
essentials. Existing stored mode values remain compatible; `details` is additive.

The production layouts follow the approved proposal: a source/player comparison
at wide widths, stacked phone facts, readable typography, explanatory surfaces,
inline metric definitions and disclosure controls. The headline prints the exact
player-reported dimensions instead of guessing a progressive/interlaced badge.

## Implementation map

| Surface | Presentation | Existing data owner |
|---|---|---|
| Web library and VOD | `playbackInfoOverview`, `playbackInfoMarkup`, incremental diagnostic patcher in `index.html` | `PLAYER` and the attached video element |
| Web Live TV | Same renderers, separate bounded Playback info dialog | `LIVE_TV`, current lease and retained live video element |
| Apple library and VOD | `PlaybackInfoPanel.swift`, adapted by `PlaybackStatsView` | `PlayerController` and its current AVPlayerItem |
| Apple Live TV | Same panel, adapted by `LiveTvStreamInfoPanel` | `LiveTvPlayerController`; local one-second samples while visible |
| Android library and downloads | `PlaybackInfoPanel.kt`, adapted by `PlaybackInfoOverlay` | Existing Media3 controller/player |
| Android Live TV | Same panel, adapted by `LiveTvPlaybackInformation` | Retained `LiveTvPlayer`; local one-second samples while visible |

The [input contract](PLAYER-INPUT-CONTRACT.md#7-playback-info--a-shared-hierarchy-across-clients)
and its canonical field fixture define names, units and modes. Existing
playback ownership, session creation, keepalive and recovery are unchanged.
Diagnostic refresh does not renew a tuner lease or issue a new server request.

## Data rules and deliberate choices

- Positive dimensions from the attached player are the only playing-resolution
  measurement. Missing dimensions remain `Not reported`; original or manifest
  dimensions never fill that slot. Pending web replacements suppress predecessor
  measurements. Media3 uses `videoSize` for the player and `videoFormat` separately.
- Apple presentation dimensions and browser intrinsic dimensions can include
  presentation aspect correction. This is a player-reported picture size, not
  a claim about coded pixels or the display's physical resolution.
- Audio track metadata never implies speaker or HDMI output. Unsupported device
  audio output remains explicit. Apple currently does not supply stream dimensions
  independently from source/presentation, and says so.
- Device buffer, server-ready media, production progress, media bitrate, observed
  transfer rate and server response bytes keep separate meanings. Explanations
  stay with their values. Playback state does not stand in for server state.
- Live-edge distance is to available media, not end-to-end broadcast latency.
  Web/Android Live TV lack an interruption counter; they show `Not reported`.
- Source timestamps and existing server sample-age fields remain available.
  No invented health score, receive confirmation or freshness timestamp is shown.

## Verification record

The final candidate must compile locally for affected native clients before
pushing. Exactly one adversarial review precedes the single final main fast
lane. The fast lane includes the retained field/DOM contract regressions:
unknown cached-VOD picture size cannot become original size, and every diagnostic
field remains reachable. Native regressions cover resolution availability and
stored mode compatibility. No repeated unit suite is part of this work.

Compiler, review, rendering and final lane evidence will be recorded here before
merge. Compilation alone does not establish live playback or remote usability.
