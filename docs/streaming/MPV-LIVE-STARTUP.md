# MPV live startup admission

**Owner:** Codex; Sol implements the Linux Player request and request regressions.
**Status:** implemented; adversarial review, final affected validation and runtime acceptance pending.
**Updated:** 2026-11-17.

A normal Linux Player seek recreated a live copy session. The first frame
arrived 26.993 seconds after the OSC click. The server had already committed
a complete 2.795-second fragment, but withheld the first playlist for another
22.281 seconds. The create request omitted its consumer transport, so MPV
inherited Safari's 48-second native presentation admission requirement.

## Measured cause

Retained production phase logs distinguish writer readiness, publication and
init validation. These are server observations, not optical picture measurements.

| Start | Complete cut to first snapshot | First snapshot media | Init validation |
|---|---:|---:|---:|
| Initial play | 10.803 s | 49.675 s | 122 ms |
| OSC seek | 22.281 s | 49.759 s | 13 ms |
| Return seek | 5.165 s | 49.675 s | 108 ms |

All three used `NativePresentationWindow` with `bootstrap_ms=48000`.
The copy writer's own gate was already open. The seek's first snapshot and
response admission immediately precede MPV receiving the playlist MIME type;
client decode to first frame then took approximately 0.28 seconds.
Private logs and source identities are retained in effort E-3B754319C5.
No raw media identity, bearer session capability or host capture belongs here.

## Correction

Linux Player 0.5.2 sends the existing create-time `transport: "mpv"` on initial
start and session recreation. The server freezes MPV's first-complete-cut
startup policy at creation. It admits complete media covering the requested
position plus two seconds at the current playback rate, then replenishes
while reserve is low. Production allowance and steady reserve remain owned
by the existing rolling flow controller. This separates consumer admission
from steady production; no timer, retry or watchdog masks missing media.

Safari/native, omitted and unknown transports retain their measured native
presentation window. hls.js retains its separate reload-window policy. MPV
retirement remains conservative, and old servers accept its transport string
while retaining their prior conservative startup. Init validation, complete
objects, ownership and producer-attempt fencing remain in the existing path.

## Acceptance

The causal regression replays the measured first cut at the measured seek
offset with both explicit control and legacy fetch-driven demand, then checks
replenishment with variable GOP lengths before the steady publication cadence.
The native Safari fixture remains a separate required regression. Player tests
cover initial remux/transcode, normal seek, OSC film seek and quality recreation.

Before-change metrics were retained privately from the same lab node. Its
`plurx_ttff_ms` and `plurx_seek_to_picture_ms` series have zero observations, so
they provide no historical latency distribution for this incident. After
validation, compare phase timestamps and the normal player first-frame timing;
expect `FirstCompleteSegment`, a 2000 ms bootstrap at 1x and no 48000 ms wait.
Device acceptance must also establish advancing playback and accepted controls
through replacement; neither synthetic tests nor a PQ surface proves full DV.
