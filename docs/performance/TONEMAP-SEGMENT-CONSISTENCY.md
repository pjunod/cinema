# Tone-map segment consistency — prove frame identity across filter resets

**Status:** open · **Updated:** 2026-10-15

Companion to [HDR reference scoring](HDR-REFERENCE-SCORING.md), which grades
color against an explicit reference. This check answers a separate question:
does resetting the same tone-map graph change the same decoded source frame?
The [Mac processing ledger](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md)
retains the broader shipping, seek and physical-display acceptance.

## 1. Contract — compare identical inputs at rational timestamps

[`qualify-tonemap-segments`](../../scripts/qualify-tonemap-segments) decodes
one finite PQ source into canonical 10-bit planar frames, retaining each
SHA-256 and rational PTS. It runs a declared graph once continuously, then
starts a new filter process for each partition of the same frame sequence.
The source hashes and PTS must match before any pixel comparison counts.

Every output frame must be byte-identical to its continuous counterpart.
The receipt reports per-plane maximum and mean differences for every frame,
and identifies the first and last three frames of each reset window. There
is no tolerance chosen after measurement: any changed byte fails this narrow
same-graph comparison. Missing, extra, shifted or duplicated frames fail
association rather than being scored as color differences.

Explicit cases bind peak nits, the filter's peak divisor and reference white
to the actual graph options. The corrected Metal Hable contract uses peak
nits/10 and reference white 100; the CPU reference uses peak nits/100 and
100-nit linear normalization. This harness does not change either graph.
Default-parameter controls are separate cases and cannot claim the explicit
production graph passed. Case graphs use a deliberately small grammar:
linear unquoted, unescaped filter chains with named tone-map options. Quotes,
backslashes and ambiguous positional tone-map arguments are rejected. This
keeps FFmpeg option aliases or nested parsing from contradicting the receipt.

## 2. Run a bounded control

Use an absolute path to the audited Jellyfin tools, a new durable output
directory and explicit JSON case files. The tool never installs packages or
changes a daemon. Do not run throughput measurements concurrently with this
filter experiment on the same GPU.

A CPU case for the 512 by 128 authored signal is:

```json
{
  "id": "cpu-fixed-1179",
  "mode": "explicit",
  "peak_nits": 1179,
  "peak_divisor": 100,
  "reference_white_nits": 100,
  "graph": "zscale=tin=smpte2084:min=bt2020nc:pin=bt2020:t=linear:p=bt709:npl=100,format=gbrpf32le,tonemap=tonemap=hable:desat=0:peak=11.79,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion",
  "output": {"width": 512, "height": 128}
}
```

```bash
python3 scripts/qualify-tonemap-segments \
  --ffmpeg /path/to/audited/ffmpeg \
  --ffprobe /path/to/audited/ffprobe \
  --runtime-file /path/to/audited/provenance/manifest.json \
  --case /path/to/cpu-fixed-1179.json \
  --synthetic --synthetic-vfr --signal-peak 1179 \
  --frames 48 --cuts 12,24,36 --timeout 90 \
  --output /path/to/owned-evidence/reset-vfr-1179
```

Repeat `--case` for other declared graphs and `--runtime-file` for runtime
dependencies whose bytes the receipt must bind. Supply actual bounded PQ
clips with repeated `--input` instead of `--synthetic`. Per-file outputs are
limited to 512 MiB by default; frame count and geometry are checked before
capture. Every subprocess has a deadline and retained command/stderr facts.
Use a new output directory for retries so the failed attempt remains intact.

**How to read it:** exit zero and `receipt.json` result `passed` mean all
frames in every supplied source/case/window matched under the stated scope.
A failed source or case makes the overall result fail. Read its `failure`,
command logs and changed-frame records before retrying. Tool, source,
manifest and explicitly supplied runtime hashes bind what actually ran;
unlisted external dependency identity is not inferred.

## 3. Acceptance boundaries — resets are not seek or display proof

Canonical rawvideo carries the verified PQ color tuple but does not retain
original HDR/Dolby side metadata. Explicit peak runs isolate the relevant
fixed-peak transform. Default-parameter runs measure canonical fallback only;
they do not qualify changing source metadata or Dolby Vision processing.
The harness selects the first video stream and rejects non-PQ or non-10-bit
4:2:0 material instead of silently treating it as a supported source.

This experiment uses software-decoded canonical frames plus any declared
upload adapter. It does not establish hardware decode, compressed-source
seek accuracy, decoder preroll, HLS boundary continuity or lossy encoder-reset
quality. Those need independent source-PTS and presentation receipts. It
also does not establish artistic grade fidelity or display calibration.

The focused regressions in
[`test_tonemap_segments.py`](../../tests/operations/test_tonemap_segments.py)
exercise changed chroma with matching luma, missing/extra/shifted source
frames, rational VFR association, corrupt raw bytes, color facts, graph
option declarations and bounded process failure. They do not substitute for
the actual candidate FFmpeg experiment.

## 4. Campaign state

The independent review is approved. Its option-alias/positional-parsing
finding is fixed by the restricted grammar described above. Seven focused
controls pass: six passed initially; one negative fixture accidentally used
an unchanged timestamp, was corrected, and alone was rerun successfully.

The [October 15 receipt](evidence/tonemap-reset-20261015.json) records four
candidate campaigns: 1,179- and 2,713-nit authored PQ, nominal CFR and VFR,
CPU and Metal, 48 frames per case and cuts at frames 12, 24 and 36. All 384
frame comparisons across 32 reset windows are byte-identical. The separate
8-frame CPU VFR fixed/default controls also pass. These compare each backend
with itself; they do not claim CPU and Metal are pixel-identical.

Nominal CFR means authored 24000/1001 timing; the Matroska source quantizes
PTS to milliseconds. Every comparison uses the actual captured rational PTS,
not an assumed CFR grid. After the campaigns, authored MaxFALL was corrected
from an arbitrary 100 to unknown zero. A separate CPU metadata/decode proof
shows the identical authored and decoded raw hash at 1,179 nits. The mapping,
source pixels and comparison are unchanged, so their passing results are
retained with the original harness identity; no fresh GPU pass is invented.

Main landing and broader acceptance remain pending. F7 white-point delivery
and Air presentation belong to the HDR follow-up effort. Genuine-content
segment/seek presentation, encoder restart and calibrated-grade acceptance
are not closed by these authored raw-filter controls.
