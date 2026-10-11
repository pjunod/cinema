# Tone-map census — every GPU graph against the CPU chain, on real PQ

**Status:** built (C-M0 of
[STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md](STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md)
§6.2) · fleet results pending · **Feeds:** Paul's ruling R2 (the house
tone-map operator, and what a node does when its only fast graph is a driver
curve) · **Code:** `crates/plurxd/src/pipeprobe/census.rs`,
`crates/plurxd/src/pipeprobe/signal.rs`

## What it answers

Three of four nodes tone-map HDR10 on `vpp_qsv`, a driver curve, admitted by a
boot gate that compares one relabelled-SDR frame by mean Y/U/V within 24
levels ([TONE-MAP-CHAIN-CORRECTIONS.md](TONE-MAP-CHAIN-CORRECTIONS.md) §2.4).
That gate says a graph *runs and lands on BT.709*; it cannot say whether its
picture differs visibly from the CPU chain. The census measures exactly that,
for every graph the node could use, so R2 is decided with numbers and stills
in front of the person deciding.

It changes nothing. Selection, the boot gate and its fixture, and every
production filter string on every node stay as they are; the argv goldens do
not move.

## What it measures

**Signal.** Authored by the census in Rust as real SMPTE ST 2084 code values in
BT.2020 — not `testsrc2` relabelled, and not the calibration script's neutral
panels. One 1920 × 1080 frame on the 16 × 9 measurement grid:

| Grid rows | Content |
|---|---|
| 0 | grey ramp, 16 steps uniform in PQ, black → the signal's peak |
| 1 | grey ramp, 16 steps uniform in linear light up to the peak |
| 2–3, 4–5, 6–7 | BT.2020 R, G, B, C, M, Y and two skin tones (ColorChecker light/dark skin) at 100 nits, a quarter of the peak, and the peak |
| 8 | grey ramp, 16 steps uniform in PQ, black → 203 nits |

Two versions, MaxCLL 1000 and 4000, each encoded once per run to six frames of
HEVC Main10 (libx265, CRF 12 — a normal rate a hardware decoder takes, not
lossless), narrow range, with mastering-display and content-light metadata
that match the picture (MaxCLL is the ramp's peak; MaxFALL is computed from
the frame).

**Baseline.** The production CPU chain (`zscale_tone_map_filter`), in
production order — scale, then tone map — at each signal's own peak, software
decoded. It is a baseline, not an analytical reference; every number below is
*distance from the CPU chain* and nothing more.

**Candidates.** Every graph `Pipeline::pairs_with` allows for the node's
encoder — the encoder production sessions use, which on "auto" is the
transcoder optimiser's fastest family, the same one the boot probe paired
its graphs with — not only the selected one — so on a QSV node libplacebo and OpenCL are tried too.
Each runs its production device flags, decode flags and filter string,
unchanged. The vendor graphs end in GPU surfaces and get the same
`hwdownload,format=nv12` the production builder adds before a subtitle
composite. The output is decoded to raw 8-bit `yuv420p` rather than encoded,
so encoder loss is not in the numbers.

**Per candidate and signal:** whether it started (and ffmpeg's reason when it
did not); per-channel block means on the 16 × 9 grid against the baseline,
reported as the maximum and the 95th-percentile (nearest-rank) block delta for
Y, U and V in 8-bit levels; the brightest block's luma; and one PNG still,
from the same decoded frame (the third) for every graph and the baseline.

**Noise floor.** The CPU chain runs twice per signal and is compared with
itself. It is deterministic, so this is expected to be **zero**, and the
result says so (`noise_floor_zero`). With a zero floor the bar for "visibly
different" is a margin chosen from the stills, not a derived number.

Speed is not measured here: six frames time nothing. The boot probe's speed-up
over the CPU chain, on its 4K fixture, is already on the same row.

## How to run it

Settings → System → **HDR tone-map** → **Run census**. Or, as an admin:

```
POST /api/v1/system/tone-map/census        # queue; 202, or 409 if one is active
GET  /api/v1/system/tone-map/census        # status and stored result
POST /api/v1/system/tone-map/census/stop   # stop; kills the running child
GET  /api/v1/system/tone-map/census/stills/<name>   # a PNG the result names
```

`GET /api/v1/system` carries the same object as `tone_map.census`
([API.md](../API.md) §5.3a has the fields).

It is GPU work on a production node, so:

- It runs only when an admin asks; nothing starts it on boot or on a timer.
- It takes a **background** permit through admission (one hardware slot for a
  hardware encoder, plus a small CPU share). While a viewer holds or wants the
  hardware it stays **queued**, retrying every 500 ms.
- While a step runs it polls `background_must_yield` every 500 ms. When a live
  start arrives it kills its ffmpeg, releases the permit and reports
  **yielded to playback** — not a failure; run it again when the node is idle.
- **Stop** does the same on demand.
- The row shows the state (idle, queued, running with the step and graph,
  finished, yielded, stopped, failed). Its ffmpeg children appear on the
  Activity page as "tone-map census", through the boot probe's own spawn seam.

A run takes about a minute on a node with a GPU. Run it with no viewers.

## Where results live

Node-local: `<data dir>/tone-map-census/<key>/census.json` and the stills
beside it, where `<key>` is a digest of (ffmpeg version line, encoder family,
GPU device — the render node and its PCI ids). A node shows only the result
for its current key, so a new ffmpeg, a different encoder or a different GPU
starts from no result instead of showing a stale one. The scratch it encodes
in (`<data dir>/tone-map-census/work`) is removed when a run ends.

## What happens with the output

The four per-node results and stills go to Paul for **R2**. Nothing about
selection or any filter string changes until he rules; after that, §6.3 of
the handoff governs (executed boot probe with incumbent fallback, the
effective graph configuration in the plan identity, and a re-run of this
census on the node that selects a changed graph).
