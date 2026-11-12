# FEL quality without an encode: re-fitted 8.1 RPU curves

**Status:** offline result, not built · **Date:** 2026-11-12

Companion to [FEL-PICTURE-QUALITY-INVESTIGATION.md](../../../../docs/streaming/FEL-PICTURE-QUALITY-INVESTIGATION.md).
Idea and design by Fable 5.2; [protocol](protocol-rpu-refit-20261112.json)
fixed before outputs; [results](results/rpu-refit-20261112.json).

## The idea

Today's EL-dropped serve (F) converts the Profile 7 RPU to 8.1 with identity
mapping curves and plays the original BL bits. A single-layer 8.1 RPU can
still carry real curves: up to 8 luma pieces of 2nd-order polynomials and a
3rd-order chroma MMR over Y, Cb and Cr. Fitted per scene so that the untouched
BL lands as close as possible to the FEL composition, they recover whatever
part of the EL is a function of each pixel's own BL value. That needs no
encode, keeps the original BL bits, and plays as ordinary 8.1 Dolby Vision on
every client. Spatial detail and grain in the EL are out of its reach.

## Result

All-48 lattice, pooled whole-picture ΔITP against the composition (model B;
A, C and D agree):

| Fixture | F | Re-fit, no encode (F+) | Share of F's error removed | Best re-encode at 130 Mb/s |
|---|---:|---:|---:|---:|
| Michael5400 | 3.200 | **1.449** | 55% | ~1.73 (P1) |
| Michael1800 | 3.190 | **1.341** | 58% | ~1.41 (base) |
| Beekeeper (held out) | 1.465 | **0.964** | 34% | 1.56 (never beat F) |

- F+ beats F in all 48 frames on every fixture and model, on the whole
  picture and on shadow p99.
- The deployable form costs nothing against the ideal curve: 8 equal luma
  pieces with coefficients at the RPU's 2^-23 precision, one fit per window.
  A per-frame fit gains under 0.002, so there is no flicker trade-off. Taking
  the chroma MMR's luma at the chroma sample site changes nothing measurable.
- Full-resolution frames 0/24 agree. Michael5400's shadow p99 improves from
  4.74 to 4.01 at frame 0, but the shadow-mapped encodes still reach about
  3.2 there: the encodes fix shadows better, while F+ wins on the whole
  picture.
- **Real RPUs round-trip exactly.** `rpu_refit_write` writes each frame's
  8.1 RPU with the fitted curves. Read back and applied with libplacebo's
  reshaping semantics, they score identically to the fit (difference 0.0000
  on every fixture and model).

## Do decoders apply the curves?

- **macOS: yes.** `dv_display_check.swift` renders AVPlayerLayer offscreen,
  so the platform's own Dolby Vision display path runs. The same frames were
  played with three RPUs: identity, luma ×0.5, and a +0.15 Cr constant.
  Mean linear R moved from 0.202 to 0.012 for the luma change, and from 0.202
  to 0.483 for the Cr change. AVAssetReader and AVPlayerItemVideoOutput show
  no change: decode output only carries the RPU as an attachment, and the
  mapping is applied at display time. iOS and tvOS use the same AVFoundation
  pipeline but were not measured.
- **Android: not observed.** On the Google TV Streamer, screencap and
  screenrecord return black for the video plane. Its attached monitor is
  HDR10-only. Observing it needs HDMI capture.

## What it changes

- F+ should become the baseline serve for P7 FEL titles: it beats F
  everywhere measured, at no encode cost.
- Any re-encode must now beat F+, not F. On these fixtures none does on the
  whole picture.
- To build it, the per-title job fits curves from BL+EL once per scene (CPU
  seconds) and stores them. The existing 8.1 conversion writes them into each
  RPU instead of identity curves.
