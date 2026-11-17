# MPV live startup admission

**Owner:** Codex; Sol implements the Linux Player request and request regressions.
**Status:** revised source independently reviewed; Rust validation and runtime acceptance pending.
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
start and session recreation. The server freezes the consumer policy at create.
MPV retirement remains conservative. Native/Safari, omitted and unknown
transports retain their existing native admission; hls.js keeps its own window.
Older servers accept `mpv` and retain conservative behavior.

First-cut publication is insufficient: FFmpeg reloads after the last segment’s
duration, then waits half the target after an unchanged reload. A 2.795-second
cut followed by an 8.1-second cut produced at 2x misses the first reload and
cannot be discovered until about 10.795 seconds. The initial candidate is held
and superseded by a producer/reload budget. This follows the selected consumer
version’s [upstream HLS schedule](https://github.com/FFmpeg/FFmpeg/blob/n8.1.2/libavformat/hls.c);
the [MPV tag](https://github.com/mpv-player/mpv/blob/v0.42.0/demux/demux_lavf.c)
does not establish every distribution/native patch in the selected binary.

The supported operating envelope assumes sustainable production at least as
fast as playback, bounded 16-second complete cuts, ordinary 2-second refill
spacing and 250 ms polling. The scheduling/delivery margin is 1 second: retained
snapshot-to-first-frame observations were below 0.9 seconds. Neither this margin
nor recent average speed guarantees future network, disk or producer behavior.
Selected-build continuity is required for acceptance.

For playback rate `r`, presentation target/cut bound `T`, refill spacing `P`,
poll interval `q` and measured scheduling/delivery margin `m`:

```text
next publication envelope = max(T/r, P) + q
reload envelope = max(T, next publication envelope + T/2)
required media runway = ceil(r * (reload envelope + m))
```

At 1x this requires 25.25 seconds of media rather than 48. The computed floor is
not silently truncated to the reserve cap. Short MPV admission additionally
requires finite recent production at least equal to playback; unknown or slow
capacity retains native admission. The original initial-capacity assessment
remains due at the fixed production bank even after an early first snapshot,
so a slow producer cannot bypass it by continuing to complete cuts.

The existing flow controller owns steady production and reserve. Init and
complete-object validation, ownership/attempt fences and retired-object promises
remain in their existing paths. No timer, retry, watchdog or forced transport
capability masks missing media.

## Acceptance

The causal regression constrains complete inventory by production timestamps
and independently models FFmpeg’s reload schedule, including unchanged reloads,
short-to-long GOPs, seek offsets and supported playback rates. The original
first-cut policy must fail its counterexample. Actual publication tests cover
explicit/legacy demand, conservative unknown-capacity rounding, replenishment
and the initial capacity assessment after early publication. Safari’s fixture
remains a separate regression. Player tests cover initial remux/transcode,
normal seek, OSC film seek and quality recreation.

The Player run accounts for 120 tests. Successful cases were retained; only
55 failed methods were rerun in a Linux container after resolving Mac runner
socket-path/Bash limitations and package-version metadata. All 55 passed with
no skips. The 0.5.2 frontend package is built. Revised server review is clear after the reload and capacity boundary fixes.
Affected Rust validation and normal-route runtime acceptance remain open.

Before-change metrics were retained privately from the same lab node. Its
`plurx_ttff_ms` and `plurx_seek_to_picture_ms` series have zero observations, so
no historical latency distribution is claimed. Compare after-change server
phase timestamps with normal-player opening/MIME/first-frame timing; expect
`MpvReloadWindow` and a 25250 ms bootstrap at 1x with established capacity.
Unknown capacity should report the conservative 48000 ms requirement.

Device acceptance must establish advancing playback without the reload
counterexample, accepted controls through replacement and native mapping.
Synthetic tests or a PQ surface do not establish full Dolby Vision, every-frame
mapping or optical accuracy.
