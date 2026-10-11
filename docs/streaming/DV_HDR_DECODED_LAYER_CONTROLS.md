# Dolby Vision encoded layers — bounded M0 decode and rendering controls

**Status:** open — synthetic decoding and rendering reviewed; product
qualification remains incomplete · **Updated:** 2026-10-08

Companion to the [implementation ledger](DV_HDR_PROCESSING_STATUS.md) and
[build contract](DV_HDR_PROCESSING_BUILD.md). This records the encoded input
side of the M0 experiment: actual HEVC base/enhancement decoding, display-frame
metadata association and rendering of the accepted pictures. It does not
qualify a production playback route.

## 1. What the retained experiment establishes

| Operation | Accepted evidence | Boundary |
|---|---|---|
| Encode and decode both layers | Six distinct 64 × 64 Main10 pictures per layer; decoded native pixels exactly match their generated source | Synthetic identity reshaping and bounded residual; no movie reference |
| Associate display frames | Independent decoders preserve actual rational PTS/duration through no-reorder and B-frame cases | No seeking, variable frame rate, discontinuities or legitimate RPU reuse |
| Associate metadata | Fresh decoder-attached raw RPU bytes, parsed fixture tags and expected picture identity agree | Known generated tags detect intentional swaps; this is not a general artistic-metadata validator |
| Render accepted pairs | Twelve display pictures, five enhancement controls, reconstruction and full-render outputs: 120 arithmetic comparisons | Scalar public-shader algebra; not an independent Dolby oracle or creative target-mapping proof |

The [retained bundle](../../tools/dv_quality/backends/decoded-layers/README.md)
contains 130 hashed files plus its ledger: source, dependency locks, generated
RPUs, logs, receipts and inventories. It omits compiled binaries, upstream
archives and runtime pixel payloads; the replay regenerates those artifacts.
The separate runtime inventory identifies 958 checksummed runtime artifacts;
it does not inventory all downloaded source and intermediate build files.

## 2. Actual decoder timing and the missing-RPU control

The Matroska fixture has an observed time base of 1/1000. Display PTS are
0, 42, 83, 125, 167 and 208 ms. Reordered coded arrivals are 0, 125, 83, 42,
208 and 167 ms, and both decoders restore display order. Packet and frame
duration is 41 ms while adjacent PTS gaps alternate between 41 and 42 ms.
Those observed values are retained; they are not replaced with a calculated
1/24. Unknown initial DTS remains null.

At display time 83 ms in the missing-RPU case, the base picture lacks raw RPU
side data but retains parsed metadata from an earlier coded packet whose
display time is 125 ms. The enhancement picture lacks both because its
extraction mode omits RPU. Parsed metadata presence alone therefore cannot
prove fresh per-frame metadata acceptance. Legitimate metadata-reuse syntax
needs a separate state machine and is not covered by this fixture.

Missing or swapped enhancement payloads fail in the HEVC reference chain
before association completes. Their expected refusal requires exact exit
status 1 and the controlled diagnostic. Missing or swapped RPU cases decode
successfully and then fail their exact association check. A crash, unrelated
exception or wrong failure stage cannot satisfy those negative controls.

## 3. Replay and interpret the evidence

The bundle's README gives the complete pinned source, licensing and dependency
contract. Supply an approved parsed-renderer replay and its reviewed bundle;
the dependency lock checks source evidence before copying. It checks every
relevant copied file and the relative linker symlink before and after copying. A rebuild with different
library bytes requires a new reviewed identity lock. Image-ID syntax alone
does not prove dependency provenance.

```bash
export PYTHONDONTWRITEBYTECODE=1               # keep the source bundle read-only
scratch=$(mktemp -d)                           # parent for a new replay
scratch=$(cd "$scratch" && pwd -P)             # resolve temporary symlinks
sh tools/dv_quality/backends/decoded-layers/replay.sh \
  "$scratch/decoded" "$parsed_replay" \
  tools/dv_quality/backends/parsed-rpu
python3 tools/dv_quality/backends/decoded-layers/test_association.py \
  "$scratch/decoded"
python3 tools/dv_quality/backends/decoded-layers/test_prerequisite.py \
  "$parsed_replay" tools/dv_quality/backends/parsed-rpu
python3 tools/dv_quality/backends/decoded-layers/test_rendered.py \
  "$scratch/decoded"
```

`parsed_replay` must be the absolute path to the prerequisite whose exact
source/library identities match the bundle's reviewed lock. The replay writes
only to its new scratch directory, fetches the pinned FFmpeg source archive,
builds the isolated decoder, generates/muxes fixtures, decodes both layers,
checks association and renders accepted pairs. Linux ARM64 Docker support is
required. The inherited image uses live package resolution; this is not a
fully locked operating-system snapshot.

**How to read the result:** both normal association cases must be accepted;
eight encoded negative cases must match their expected refusal stage. The
17 association, 11 prerequisite and 13 renderer corruption controls must pass.
The 120 comparisons differ from their arithmetic reference by at most
1.056e-5 PQ and one RGB48 code, and reordered/no-reorder output hashes agree.
Every rendered result binds actual consumed BL/EL/RPU bytes, accepted
association, probe events, timestamps and flags. All RGBA channels must be
finite; altered inputs or timing cannot pass on unchanged output pixels.

The 2e-5 PQ and two-code bounds were inherited from the prior probe and written
before inspecting these numerical comparisons, after output generation. They
are regression bounds, not formally preregistered movie-quality thresholds.
Two-CPU/two-GiB container limits bound this fixture run; they do not measure
full-resolution throughput or establish a production queue contract.

## 4. Review and remaining acceptance

Adversarial review corrected negatives that accepted unrelated exceptions,
prerequisite identities that checked too little, and rendered results that
did not revalidate their actual frame/metadata/timestamp association. Focused
corruption tests cover each correction. The reviewer verified all bundle and
runtime inventory hashes after the final clean replay.

Seeking, variable frame rate, discontinuities, legitimate RPU reuse, broader
mapping and trim support, container/profile conformance, representative movie
references and matched-rate quality remain open. Production routing, settings,
cache identity, fallback lifecycle and HDR10-E reporting remain governed by
the build contract. This experiment does not grant those capabilities.
