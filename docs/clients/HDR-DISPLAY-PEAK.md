# HDR display peak — Android reports and browser choices

**Status:** client candidate compiled; independent review and regression
execution pending · **Written:** 2026-10-22.

Companion to [the display-targeted HDR10-E build](../streaming/DISPLAY-TARGETED-DV-BUILD.md)
(the server policy and phased delivery). This document describes the client
peak capability, Settings controls, and synthetic PQ comparison asset. It
records implementation separately from display qualification.

## Peak reports belong to one playback

Both clients send optional integer `display.max_nits` and `display.peak_source`
(`platform` or `viewer`) in the v2 capability document. Unknown omits both
fields. Neither HDR support, decoder capability, CSS dynamic range nor the
player's pixel rectangle implies luminance. The server owns bucket selection;
clients send the report rather than a rounded bucket. No minimum luminance is
collected in phase 1. Apple clients do not report a peak in this phase.

A fresh playback captures a peak. Seeks, audio/subtitle changes, Auto-rung
re-decisions and prepared successors retain that peak, including Unknown, so
one session cannot silently change its mapped luminance. Settings changes and
display changes take effect on the next fresh playback. Stop and reopen the
title if you need the new display's target immediately.

| Client | Auto | Explicit choice | Storage |
|---|---|---|---|
| Android / Android TV | Reads the active/default display's `HdrCapabilities.desiredMaxLuminance`; non-finite, ≤0 or ≥5000 nits means Unknown. Valid fractions truncate to integer nits. | 400 · 600 · 1000 · 1600 · 2000 · 4000 nits, sent with `viewer` provenance. | Device DataStore, Settings → Playback → HDR display peak. |
| Browser | Unknown; the browser has no nits API. | The same six choices, sent with `viewer` provenance. | Per-browser `localStorage` key `plurx.hdrDisplayPeak`, Settings → Playback → HDR display peak. |

Android probes fresh capability documents on every decision. A display listener
lives with the player screen, re-reads HDMI/display changes for the HDR badges
and diagnostic log, and unregisters on disposal. It never mutates admitted
`decisionCaps`; the next fresh decision re-reads both the display report and
saved override. Browser incumbent re-decisions copy the original peak even
while decoder and measured-presentation geometry facts refresh.

Android build 178 includes the client control. There is no readiness gate on
the viewer's choice. Browser storage refusal displays a save error and keeps
Auto/Unknown rather than claiming that the preference persisted.

## PQ comparison — signal values, not optical measurement

The browser card includes a synthetic HEVC Main10 `hvc1` MP4. Eight columns,
left to right, are labeled 100 · 200 · 400 · 600 · 1000 · 1600 · 2000 · 4000 nits.
Each column contains a 90% left half and a 100% right half. Choose the highest
labeled step whose halves remain distinguishable on an actual HDR output.
For Pro/Studio Display XDR, the suggested starting value is **1000 nits**,
using sustained full-field capability rather than the 1600-nit highlight peak.

HEVC Main10 and actual HDR presentation are required. A browser can reject the
clip or tone-map it to SDR, and the app cannot verify the output mode. If HDR
presentation is uncertain, keep Auto or use a known sustained specification.
The pattern is encoded PQ video, not a CSS/SDR imitation, and it is not an
optical calibration or a display certification.

The clip uses BT.2020 primaries and non-constant luminance matrix, ST 2084
transfer, 10-bit limited range, neutral chroma 512, and luma
`round(64 + 876 × PQ(nits / 10000))`. Its synthetic mastering metadata uses
BT.2020/D65, maximum 4000 nits, minimum 0.005 nits, MaxCLL 4000 and MaxFALL 0
(unknown). These describe the synthetic signal, not a source title or a
measured monitor.

| Step (nits) | 90% code | 100% code |
|---|---|---|
| 100 | 500 | 509 |
| 200 | 562 | 571 |
| 400 | 626 | 636 |
| 600 | 664 | 674 |
| 1000 | 713 | 723 |
| 1600 | 757 | 768 |
| 2000 | 779 | 789 |
| 4000 | 845 | 855 |

Generate or replace the embedded clip from the repository root:

```bash
python3 scripts/generate-hdr-peak-pattern --update  # FFmpeg with libx265 + ffprobe
```

The generator writes its raw source, MP4, probe and exact command/version/hash
provenance under `target/hdr-peak-pattern/`. It uses no external media. The
embedded MP4 is under 20 KB; no separate media endpoint or runtime generator is
required. FFmpeg 9.0.2 produced SHA-256
`3a5d3270dd673ccd1a3c3bb0b7088d0a98acf4927511784f355e44bbef770e1f`.
The two decoded frames' raw SHA-256 is
`efd17bd8d92f23be053f7738fbb6362f48cdf68f5e2195719d0e6ecd5673cf80`,
identical to the generated source. Reproducible MP4 bytes require the same
encoder/tool versions; the signal code values remain defined above.

## Validation — compilation is not display acceptance

The candidate passed Android `:app:compileDebugKotlin` and
`:app:compileDebugUnitTestKotlin` with Java 21 / SDK 37. No tests executed at
that stage. Changed JavaScript passed `node --check`. FFprobe confirmed Main10,
`hvc1`, `yuv420p10le`, limited BT.2020/ST2084 tags and static metadata. A lossless
FFmpeg decode reproduced the raw pixels exactly. These are artifact/static
checks; no browser HDR rendering, HDMI hot-plug, Android TV interaction or
photometric qualification is claimed.

After final independent review, focused regressions are:

```bash
node --test tests/web/hdr-display-peak.test.js tests/web/shared-decision.test.js
node tests/web/settings-sections.test.js
clients/android/gradlew -p clients/android :app:testDebugUnitTest --tests 'tv.plurx.app.data.CapsPolicyTest'
```

`hdr-display-peak.test.js` exercises Unknown, local persistence/provenance,
incumbent/new-playback separation, storage refusal and embedded `nclx` tags.
`CapsPolicyTest` covers invalid Android reports, overrides, serialized provenance
and retained capabilities. Physical acceptance remains open: prove actual HDR
presentation before using the comparison, and verify HDMI changes reach a new
session while the current session and successors keep the admitted target.
