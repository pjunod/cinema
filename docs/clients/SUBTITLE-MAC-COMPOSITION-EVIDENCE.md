# Mac subtitle composition — corpus, seeks and cancellation

**Status:** done for the analytic graph contract below · **Observed:**
2026-10-09 · **Source:** `6d480b4ea55da964764d9de89c90361d334d8f5b`.

Companion to the [subtitle completion ledger](SUBTITLE-RELIABILITY-COMPLETION.md)
and [Mac processing status](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md).
This record qualifies native video processing followed by the existing CPU
subtitle renderer on original synthetic SDR8, HDR10 and HLG media. It closes
the bounded E2 corpus/seek/EOF/cancellation checks, not display calibration or
a throughput/energy benchmark. GPU subtitle composition remains conditional
on measured benefit; these checks require no second renderer.

## Package and source identity

The retained final native FFmpeg package SHA-256 is
`c8c4b5259b8129d4240599e16e57e9286b2e3c0ca5076e91fa6055b5872ab4c8`.
Its strict-Dolby, BWDIF and AC-4 provenance is retained in the
[Mac evidence record](../streaming/MACOS-VIDEO-PROCESSING-EVIDENCE.md).
No package, daemon or production configuration was changed by this run.

The [machine-readable receipt](SUBTITLE-MAC-COMPOSITION-EVIDENCE.json) retains
input, authored-subtitle and executed-driver hashes, analytic measurements,
seek results and cancellation outcomes. Inputs are the repository's CC0
[`sdr8.mp4`, `hdr10.mp4` and `hlg.mp4`](../../crates/plurxd/fixtures/macos-processing/manifest.json).
Each one-second input was looped into eight seconds of 96 frames at 12 fps.
New ASS and two-object PGS fixtures are authored by the reproducer below;
no real library media or external artwork is used.

## Observed assertions

| Contract | SDR8, HDR10 and HLG observation |
|---|---|
| Processing order | VideoToolbox scale; Metal tone mapping for HDR/HLG; download; CPU libass or bitmap overlay at 160×90 |
| ASS placement and fade | Anchored cue region blank before onset and after clear; active average RGB-channel difference 50–52 codes; fade samples 42–44 |
| ASS motion | Three measured horizontal centroids advance about 50→90→110 pixels according to authored movement |
| PGS geometry and alpha | Two authored objects occupy their expected sampled positions; half-alpha red over black is RGB 129/0/0 |
| Palette reuse/update | Opaque object is green before the update and blue after it; the unchanged red object remains half-alpha |
| Clear and free interval | No caption difference during the later blank interval; PGS samples return to the video baseline after explicit clear |
| Active-cue seek | At 2.5s and 4.5s, ASS and PGS differ visibly from their corresponding plain-video controls; at 6.5s both are blank |
| EOF | Complete 96-frame output; an ASS cue crossing EOF remains present through the final six frames |
| Cancel | Real-time HLG text and bitmap processes both produce media, accept SIGTERM and are reaped; exit 255, under 3 seconds, no child left |

The output is SDR after the existing HDR/HLG processing graph. This does not
introduce a new HDR subtitle burn policy. The observations establish authored
semantics in this finite analytic corpus; they do not claim all fonts, discs,
calibrated displays, or physical client output modes.

## Seek time stays on the source timeline through composition

The seek controls use the finite VOD contract: `-copyts -noaccurate_seek`,
input seek to `max(target-2,0)`, and source-relative timestamps through native
processing and CPU subtitle composition. Each target is then trimmed and
rebased for output. The bitmap control reads a complete subtitle-only
Matroska sidecar from zero, as
[`vod.rs`](../../crates/plurx-core/src/transcode/vod.rs) does. Seeking the video
input is not permission to discard the display set active at the destination.

Every active-seek assertion compares the first output frame with the plain
video at the same target, so a correctly timed later cue cannot conceal a
missing cue at seek completion. EOF and cancellation are separate checks.
Cancellation terminates only the fixture's own process group and reaps it;
it is not a product retry or watchdog.

## Reproduce explicitly on a Mac with the existing native package

```bash
# Supply an existing native FFmpeg and a fresh private output directory.
python3 scripts/subtitle-macos-composition-check \
  --ffmpeg /path/to/native/ffmpeg \
  --out /tmp/subtitle-composition-receipt
```

The [reproducer](../../scripts/subtitle-macos-composition-check) uses explicit
inputs, generates only synthetic media, and records package/driver/input
hashes before execution. A missing native filter, failed encode, missing cue,
wrong interval, bad seek or unreaped process fails the run. Its output files
are private test data and may be removed after the receipt is retained.
