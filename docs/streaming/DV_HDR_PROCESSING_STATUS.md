# Dolby Vision processing — implementation evidence and remaining work

**Status:** open — M0 implementation in progress; product routes unqualified ·
**Updated:** 2026-10-08 · **Integration:** `effort/dv-hdr-processing`

Companion to the [build handoff](DV_HDR_PROCESSING_BUILD.md) and
[source feasibility findings](DV_HDR_PROCESSING_FEASIBILITY.md). This is the
implementation ledger: commands and evidence here describe completed work;
remaining milestones stay in the build handoff. The
[project backlog](../features/PROJECT-BACKLOG.md) points here for progress.

## 1. Current boundary

The reviewed proposal landed in documentation PR #919. Implementation starts
from its merged main commit `079960daebd5a1e23dff2b0e8f8506c1238bc1b0`, with
the comparator in PR #920 and backend experiments on
`codex/dv-m0-backends`, both targeting the integration branch. Sol 6.1 sessions
provide the renderer, CPU/authoring and offline measurement work, with
independent adversarial review. Production routing, settings and badges remain
unqualified and unchanged.

The first task must retain reproducible commands, source pins, actual frame
artifacts and negative controls. Numerical agreement is evidence for the named
operation, not full Dolby Vision conformance or an improvement on a movie.
Software Vulkan is suitable for mechanics; it cannot establish GPU throughput
or real-time admission. Enhanced P8.1 still requires independent DV-on and
DV-off validation. Current playback keeps its compatible fallback.

## 2. Milestone ledger

| Milestone | State | Evidence still required |
|---|---|---|
| M0 backend and reference spike | Partial: bounded comparator, structured/parsed GPU controls, CPU reconstruction, encoded-layer association and bounded affine P8.1 authoring retained with replay recipes | Seek/VFR/discontinuity and metadata-reuse acceptance, general metadata adaptation, independent reference limits and full graph resource evidence |
| M1 typed contracts | Not started | Backend operation boundaries established by M0 |
| M2 processing and lifecycle | Not started | Qualified graph, bounded ownership and timestamped adapter |
| M3 routing and cache identity | Not started | Effective operation receipts and fallback generation handling |
| M4 settings and HDR10-E badge | Not started | M3 reporting; actual processing evidence on each client |
| M5 quality and performance | Not started; M0 supplies the measurement foundation | Held-out corpus, matched bitrate, physical playback and full graph performance |
| M6 release qualification | Not started | Exact-tree gates and separate acceptance for each proposed route |

## 3. Run the bounded offline comparison

The [offline tool](../../tools/dv_quality/run.py) accepts explicit manifests
and already exported RGB48LE frames. It does not run arbitrary commands from
manifests, decode video, reconstruct FEL or certify an independent reference.
This deliberately small M0 interface permits 64 frames, 256 pixels per raster
side and 262,144 pixels per stream. Full-raster M5 measurements need a later
streaming implementation; these bounds cannot establish production speed.

Create an authored mathematical example in a new scratch directory:

```bash
scratch=$(mktemp -d)                           # fresh local evidence root
scratch=$(cd "$scratch" && pwd -P)             # resolve symlinked temp parents
python3 tools/dv_quality/example.py --output "$scratch/controls"
python3 tools/dv_quality/run.py compare \
  --candidate "$scratch/controls/candidate/manifest.json" \
  --baseline "$scratch/controls/baseline/manifest.json" \
  --output "$scratch/comparison"
```

**How to read the result:** `receipt.json` contains per-frame and pixel-weighted
Delta E ITP (lower is closer), PU21 luminance PSNR (higher is closer), exact
rational timestamps and observed luminance threshold fractions. A zero-error
PSNR is represented by `null` plus `identical: true`, with the mathematical
infinity interpretation stated explicitly. Threshold fractions alone do not
prove clipping. Frame weighting is by pixel count, not duration.

Without `--reference`, `reference_error` is `null`: the tool measures only the
difference between candidate and baseline. A reference manifest must provide
hashed evidence and explicit independent provenance. Its independence and
picture semantics remain declarations to be reviewed; hashing does not prove
them. `capability_qualification` stays `null` in either mode. No result from
this tool grants the HDR10-E badge or qualifies P8.1 authoring.

Each input declares source and renderer identity, picture domain and target
policy. Frame sizes, hashes, color/range tags, source binding, frame counts and
exact PTS/duration pairs must agree. Unequal frame counts, mismatched pairs,
duplicates and overlapping intervals are refused; equivalent fractions are
normalized. Matching gaps are recorded. Source-timeline completeness remains
explicitly unverified: shared omissions and source boundaries are not checked. The tool does no implicit
retiming, scaling or exposure adjustment. Optional source bytes are hash
verified; absent source bytes and renderer identity are marked unverified.

The new output directory retains original manifests, exact processed inputs,
normalized reproduction manifests, tool code and licenses. Its receipt records
their hashes and a reproduction command to run from that directory. Existing
outputs are refused, including an incomplete prior run. Inputs must be regular
local files under their manifest directory, without symlink components.

The [generated example](../../tools/dv_quality/example.py) demonstrates the
schema. In addition to its fields, an `independent-reference` role requires:

```json
{
  "provenance": {
    "kind": "independent",
    "scope": "analytic-control",
    "producer": "identify the separate reference producer",
    "method": "state the bounded operation and derivation",
    "evidence": {"path": "reference-evidence.json", "sha256": "<64 lowercase hex digits>"}
  }
}
```

Use `picture-reference` only for a justified reference render. Domain
`reconstruction` and domain `mapped` are different contracts and cannot be
compared as interchangeable pictures. License/provenance and reference target
assumptions still need external review before fidelity claims about a movie.

The metric constants and expected vectors are pinned to Colour and PU21 in
[the module](../../tools/dv_quality/metrics.py); BSD notices accompany their
reuse. BT.2020 luminance weights are explicit, and camera/exposure fitting is
disabled. The authored two-frame example proves harness mechanics only.

## 4. Validation boundary

The pinned compiler loop was established on the implementation base:
`rustc +1.97.1 --version` reports 1.97.1, and
`cargo +1.97.1 check -p plurx-core --lib --locked` passes with 34 existing
warnings on the default-feature surface. No production Rust source changed in these slices. Isolated fixture generators
compile against their separately pinned Rust 1.97.1 source recipes.
Focused implementation regressions apply to the new offline tools. The earlier
request to omit unit tests covered the documentation-only PR.

Do not use this ledger as a production qualification receipt. Until the
remaining gates pass, HDR10-E is a specified product label, not an emitted
playback badge, and no measured universal quality gain is claimed.

The first comparator slice passes 23 focused regressions:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s tests/operations -p test_dv_quality.py -v
```

Adversarial review found two issues before integration: a shared gap in both
input timelines could be mistaken for complete source coverage, and an
oversized JSON integer could bypass a clean refusal. The receipt now records
common gaps and explicitly leaves source completeness unverified; target
luminance bounds reject oversized integers without float conversion. Dedicated
regressions cover both. This review applies only to the offline comparator;
backend probes receive their own review before integration.

The reviewer verified both corrections and approved the bounded comparator
with no remaining findings.


The comparator task merged as PR #920 into the effort after the current-head
development gate passed in run 4509. Its interrupted attempts have
[exact receipt recovery](../ci/PYTHON-UNIT-PR-RECEIPTS.md#pr920--two-authenticated-empty-effort-failures);
all retained successes preserve their original attribution. This development
gate is not a main-promotion or playback qualification receipt.

The first backend bundles merged as PR #924 after run 4511 passed Python,
Rust, web, Apple and Android checks. Windows cross-compilation exceeded its
30-minute limit; the aggregate gate therefore remained red. The user explicitly
waived Windows validation for this Dolby Vision work on 2026-10-08, and that
exception is recorded in the PR and landing message. Windows is not claimed
to pass, and the other validation requirements remain in force.

## 5. Retained backend and authoring controls

The [first backend controls](DV_HDR_BACKEND_CONTROLS.md) retain source pins,
licenses, source-only container builds, known-answer frames and negative
controls for libplacebo and DoViBaker. Independent review approved their final
bundles, and fresh isolated replays passed before repository preservation.

The [parsed-metadata and authoring controls](DV_HDR_AUTHORING_CONTROLS.md)
extend that work through actual RPU parsing, four distinct encoded pictures,
four parsed metadata identities, DV-on rendering and an HDR10 encode. Their
combined recipe regenerates GPU outputs and checks exact frame/stage hashes.
Chroma siting is checked with pixel-phase controls; a crash cannot count as a
supported parser refusal. Explicit image identities replace developer-local
image assumptions.

These are bounded synthetic experiments. The first authoring fixture has
identity reshaping; it does not prove general protection against applying a
nonlinear curve twice. Same-renderer DV-on/off differences are not independent
Dolby reference errors. The separately reviewed encoded-layer experiment
below now establishes bounded BL/EL/RPU display association and reordering.
Seeking, variable frame rate, discontinuities, legitimate metadata reuse,
creative target mapping, general destination validity and whole-graph
performance remain open. The separately reviewed affine authoring control
below detects repeated reshaping for its declared subset; arbitrary nonlinear
adaptation and general profile conformance remain unqualified.

The CPU matrix-invariance and GPU bounded-residual-clipping findings remain
explicit unsupported-domain limits. No product route inherits capability from
a successful build or a source library's feature list. HDR10-E is still the
planned badge for qualified actual processing, and compatible fallbacks remain.


## 6. Encoded dual-layer association and rendering

The [decoded-layer controls](DV_HDR_DECODED_LAYER_CONTROLS.md) retain the
pinned FFmpeg decoder/splitter source recipe, six-picture BL/EL fixtures,
actual rational container timing, fresh-RPU association and accepted-frame
renderer bridge. Independent review approved the final source and evidence
inventories after a clean replay: 17 association, 11 dependency-identity and
13 renderer corruption controls pass. All 120 arithmetic comparisons agree
within one RGB48 code, with identical reordered/no-reorder output hashes.

Missing enhancement payloads fail at the decoder reference chain; missing or
swapped RPU cases can decode and then fail association. Each negative must
match its exact expected failure stage and diagnostic. The deliberately
missing-RPU fixture retains stale parsed base-layer metadata without fresh raw
RPU bytes, so metadata presence alone cannot establish current-frame acceptance.
Legitimate reuse remains untested and must not inherit this fixture's policy.

Every rendered result binds the current accepted decoded pictures, actual RPU,
parsed fixture identity, timestamps, duration and typed processing flags.
The dependency lock verifies source evidence before copying and the copied
library/header identities and linker symlinks before and after copying. These checks establish the bounded synthetic
operation; they do not establish a Dolby oracle, general conformance, creative
mapping, physical quality or production fallback. M0 remains partial.


## 7. Affine reconstruction and repeated-reshape discrimination

The [nonidentity authoring controls](DV_HDR_NONIDENTITY_CONTROLS.md) retain a
known affine luma map plus a known residual, independent integer CPU and
matrix/curve GPU equations, a separately checked encoded HDR10 base and a
deliberately incorrect P8.1 mapping. Correct metadata applies identity shaping
to the already reconstructed base; retaining the original affine curve is
rejected against the once-mapped reference.

CPU reconstruction agrees exactly; GPU comparison differs by at most one
RGB48 code. The deliberately repeated curve differs by 2,943–2,962 codes.
These values establish the synthetic mechanism, not a movie-quality gain.
The fixed standard-matrix restriction remains mandatory; creative trims and
general nonlinear adaptation are outside the admitted subset.

The fresh full replay and final package have separate source manifests. The
post-replay delta changes provenance wording and absent-empty-directory
packaging handling/tests, not CPU/GPU scientific stages. Independent review
verified both manifests, all 212 ledger entries and the final 16 replay-contract
controls. Repository copies also pass 16 scientific and six injector controls.
The original limit-selection chronology remains session-declared, without an
independently dated witness. No general P8.1 conformance, independent Dolby
picture reference, physical performance or product qualification is inferred.
