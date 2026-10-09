# Dolby Vision processing — implementation evidence and remaining work

**Status:** open — M0 implementation in progress; product routes unqualified ·
**Updated:** 2026-10-08 · **Integration:** `effort/dv-hdr-processing`

Companion to the [build handoff](DV_HDR_PROCESSING_BUILD.md) and
[source feasibility findings](DV_HDR_PROCESSING_FEASIBILITY.md). This is the
implementation ledger: commands and evidence here describe completed work;
remaining milestones stay in the build handoff. The
[project backlog](../features/PROJECT-BACKLOG.md) points here for progress.

## 1. Current boundary

The reviewed proposal landed in documentation PR #919, the comparator in
PR #920, and the initial backend controls in PR #924. PR #928 merged at
`a1e56700d3940363dc06c16afcc4f919d63eac97`; run 4517 passed scope, Python
(1,203 tests), Rust, web, Apple and Android. PR #930 merged the separate
[timeline follow-up](DV_HDR_TIMELINE_CONTROLS.md) at
`03fa9d2d104122d40d168260e96647fd14884104`; run 4519 passed all six non-Windows
jobs on exact head `9d33aee3caf0c0c442eb248c43cef39b5e1aee0c`. Windows and
both blocked aggregates were cancelled under the user's explicit Windows
waiver; they are not Windows success evidence. PR #928 retained all 14 named
regression trailers. These are integration checks, not product qualification.

Sol 6.1 sessions provide renderer, CPU/authoring and offline measurement work,
with independent adversarial review. Production routing, settings and badges
remain unqualified and unchanged. The [M1 contract](DV_HDR_M1_CONTRACTS.md)
owns only additive core types, validation and pure/test-only selection; actual
code review approved the bounded Rust diff. On current effort base `03fa9d2`,
25 focused tests, six compatibility regressions, core all-target checking and
warnings-denied Clippy passed with Rust 1.97.1. PR #933 merged at `0925e26db6336e9f17442635eee8e361de90745f`.
Its existing exact-head run 4524 completed successfully before the cancellation request.
The first task must retain reproducible commands, source pins, actual frame
artifacts and negative controls. Numerical agreement is evidence for the named
operation, not full Dolby Vision conformance or an improvement on a movie.
Software Vulkan is suitable for mechanics; it cannot establish GPU throughput
or real-time admission. Enhanced P8.1 still requires independent DV-on and
DV-off validation. Current playback keeps its compatible fallback.

## 2. Milestone ledger

| Milestone | State | Evidence still required |
|---|---|---|
| M0 backend and reference spike | Partial: bounded comparator, structured/parsed GPU controls, CPU reconstruction, encoded-layer association, bounded VFR/seek/epochs and affine P8.1 authoring retained with replay recipes | Broader timing and metadata-reuse acceptance, general metadata adaptation, independent reference limits and full graph resource evidence |
| M1 typed contracts | Merged in #933: 25 focused and six compatibility tests passed; production registry empty | Runtime integration and qualification in M2/M3 |
| M2 processing and lifecycle | In progress: combined bounded P7/FEL to timestamped HDR10 helper | Persistent bounded-segment graph, producer ownership/admission and output evidence |
| M3 routing and cache identity | Not started | Effective operation receipts and fallback generation handling |
| M4 settings and HDR10-E badge | Settings merged in #937: two default-off preferences persist independently with advisory Developer cards; receipt-backed web/Apple/Android presentation built and reviewed | Effective-generation reporting, runtime integration and route qualification |
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


## 8. Bounded timeline controls

The [timeline follow-up](DV_HDR_TIMELINE_CONTROLS.md) retains 21 actual
BL/EL/RPU associations across authored VFR, real demuxer seek/preroll and
two independently decoded inputs whose timestamps restart. Stored Matroska
durations are checked independently against demux/split/decoder observations.
A retained omission control demonstrates why inferred API duration cannot
stand in for source-duration evidence. Five negative cases require exact
lifecycle or association refusals.

Adversarial review corrected event ordering, required drain/reset evidence,
finite typed fields and shell status capture. The exact-source replay passed
26 checker/dependency mutations and four real failures injected after decoder
output writes. All 63 bundle and 1,094 selected runtime hashes were verified.
The earlier frozen evidence remains unchanged. This does not establish general
timing, legitimate RPU reuse, target-presentation policy, live discontinuities,
production lifecycle or new rendering arithmetic.

The decoded/nonidentity controls merged as PR #928, commit
`a1e56700d3940363dc06c16afcc4f919d63eac97`. Exact-head run 4517 passed scope,
Python receipts (1,203 tests), Rust, web, Apple and Android. Windows and the
blocked aggregate were cancelled under the user's explicit Windows waiver;
neither is reported as a passing check.


## 9. Explicit mapping reuse and remaining combined backend

The [reviewed reuse controls](DV_HDR_RPU_REUSE_CONTROLS.md) distinguish valid
mapping-ID reuse from stale parsed metadata after omitted or rejected raw RPUs.
The bounded observed configuration is Profile 8 / compatibility ID 6 /
compression 1, not qualified P8.1. Review verified 92 source ledger entries,
882 scoped runtime entries and 398 extracted prerequisite files. General
metadata reuse and product lifecycle acceptance remain open.

The next build combines actual P7 demux/split, BL/EL/RPU association, reviewed
FEL reconstruction and timestamped Main10 HDR10 encoding. A fixture-only
transport protocol was deferred because it did not close that backend gap.
The first combined cohort is synthetic and bounded; it does not grant
production registry admission or an HDR10-E badge.

**Effort validation policy, clarified by the user on 2026-10-08:** task branches
run focused regressions and affected compilation only. Do not dispatch the
existing broad effort workflow per task: it runs the Python suite again for
each new PR. Run full suites once on the completed effort branch. The earlier
runs above are historical evidence, not instructions to repeat them. Windows
validation remains waived for this effort. No merge-coordinator handoff is
authorized. This effort-specific user instruction supersedes the normal
per-task dispatch requirement without changing global CI policy.


## 10. Independent processing preferences

The settings slice persists `playback.dolby_vision_hdr_processing` and
`playback.dolby_vision_fel_reencode`; the API exposes the same names without
the `playback.` prefix. Both default off and save independently in Developer.
Unmet or unavailable readiness does not reject or rewrite a saved choice.
FEL preference does not enable the existing conversion permission. Native
DV, running playback, compatible fallback and output labels are unchanged
by this settings slice. Each control graduates separately to Playback.

The focused API controls cover independent roundtrips, admin authorization,
conversion permission and Live TV compare-and-swap separation. The existing
readiness census was corrected to include the already-present Mac card.
Adversarial review found that an earlier Save reply could overwrite a newer
unsaved edit; card identity and revision now fence the response. A deferred
response regression covers later edits, replacement cards and navigation for
both controls. Three focused daemon checks and 55 settings web checks passed;
no broad suite or per-task CI was dispatched.

The preceding reuse slice merged as PR #935 at
`c76632e63d179da2402bf8d31c6c3eadea6503b5`. Its original approved bundle is
unchanged; a verified prerequisite archive is additionally preserved outside
temporary scratch in the local Codex artifact directory.


## 11. Combined decoder-to-HDR10 execution

The [combined control](DV_HDR_COMBINED_CONTROLS.md) now connects real P7
BL/EL/RPU decoding, public reconstruction, timestamped RGB48 NUT and an actual
Main10 HDR10 encode. Six synthetic frames preserve their timestamps and stored
durations. Corrected execution and 17 focused checker controls passed review.
The stricter initial numerical allowance failed and stays recorded as failure;
post-observation diagnostic bounds do not establish a movie-quality gain.

The active implementation replaces per-picture GPU initialization with a
persistent context and bounded actual-segment input. Real producer admission,
source/generation fencing, fallback and device acceptance remain outstanding.
No production registry entry or route is enabled by this finite proof.

Settings merged in PR #937 at `a23d550401da292c9c19c9da3e1e213603ff0b1a`,
with all five named landing regressions retained and no broad CI dispatch.


## 12. Effective processing report and HDR10-E presentation

The additive `effective_processing` sidecar carries the active playback control
generation, `hdr10_enhanced`, separate FEL contribution and applied operations.
Its only core constructor derives the M1 generation digest from the canonical
UUID using the fixed `plurx.dv.playback-generation.v1` domain plus a zero byte
and the UUID's 16 bytes. It then checks the unchanged production registry,
plan, generation and current object shape. There is no caller-supplied
independent digest/UUID pairing. The registry remains empty and every current
production response constructor supplies no report.

The sidecar is omitted when absent and ignored when deserializing a durable
response, preventing stored JSON from restoring processing authority. Web,
Apple and Android require delivered HDR10, a valid matching active generation
and an accepted report before displaying **HDR10-E**, expanded as
“Dolby Vision–enhanced HDR10.” FEL details are separate. Source metadata,
preferences and intended plans are insufficient. Display output remains
unverified independently of server processing.

Missing reports, replacement, failure and seek clear the presentation. Review
caught Apple relative and manual/automatic marker jumps bypassing the public
seek method; clearing at the shared seek boundary now covers those paths,
with an actual-controller regression. This is reviewed reporting and client
plumbing, not backend admission or proof that current playback is enhanced.

Focused checks passed: two core regressions, one HLS omission/durable-discard
regression, four web badge controls, four Apple presentation/controller tests
and 20 Android MediaFacts checks. Affected Rust checking/Clippy, iOS and tvOS
compilation, Android compilation and JavaScript syntax passed. No broad suite
or CI dispatch was run. The finite combined proof merged in PR #940 at
`75cb8f80b58febe2aaddfd3fee2017c9bf1bf760`.

## 13. Streaming helper and P8.1 authoring integration

Settings landed in effort PR #937 and effective-report/client presentation in
PR #941 (merge `2730facccf2a35e768f0efb499b4924417e37cd6`). Presentation remains
conditional on an actual generation report; current production routes do not
mint one.

The [helper build and execution guide](DV_PROCESSING_TOOLS.md) records the
reviewed persistent renderer, direct NUT pipe, exact-clock overflow protection
and source-bound P8.1 authoring. The streaming path eliminates the raw-file
scratch limit that allowed only ten 4K frames. Its existing 64-picture finite
source envelope remains explicit; continuous movie/interval ownership belongs
to the serving integration still in progress.

Independent review found and closed ignored metadata fields, preallocation
raster limits, a 4K point-sampling tie and NUT clock overflow. Source-bound
authoring keeps coded base bytes unchanged and uses exact PTS association and
explicit Matroska durations; final MP4/HLS packaging is not yet qualified.

M3 worker selection, actual routing/cache receipts and concurrent graph custody
remain open. M5 comparative resource measurements have not run. Correctness
controls and configured resource caps are not a benchmark. No broad CI or full
suite was dispatched for these helpers.

## 14. Original movie windows and current integration limits

The owned adapter merged in effort PR #946 at
`f7bca9202b44124bfd3ac42609a7e05297e82ef3`. It uses the held original source,
real child registration, cancellation/reaping, resource admission and bounded
post-completion evidence. The exact merged tree passed focused source-only
Linux compilation and actual 3- and 64-picture streaming controls. A process
exit alone cannot publish media or mint an effective-processing report.

The helper now seeks bounded original-source windows, verifies coded/decoded
frame and RPU membership, and accepts retained L2/L3/L4/L5/short-L8 metadata in
its supported master-reconstruction subset. Nonzero L4 anchors remain opaque
display instructions; they are not temporal processing implemented by this
renderer. Actual parser roundtrips preserve three varying anchor pairs, while
master pixels and encoded base packets remain unchanged. Reviewed arithmetic
proves the omitted residual clipping cannot bind for an admitted 10-bit code.
The optional one-fragment authoring mode validates the actual output clock.

A private native ROG check reconstructed 24 actual 4K FEL pictures from a
one-second Beekeeper window at 720 seconds. With the RTX 4080 Laptop selected
as the sole Vulkan device, two CPU cores of quota and no encoder, the initial
unoptimized helper took 33.39 seconds; an isolated `-O2` helper took 15.56
seconds. All 24 source timestamps and reconstructed RGB SHA256 values matched
between builds; the build now defaults to `-O2`. Both runs included five preroll
pairs, source seeking and startup.
These individual observations are not steady-state throughput, a matched
FEL-versus-base comparison, or a quality result. They identify an unresolved
performance problem. The running production service was not changed.

Normal VOD routing, audio, cache/publication receipts and fallback integration
remain in progress. Base-only P5/P8 processing needs its own profile-aware path;
it cannot inherit a FEL receipt. Matched quality/resource measurements, device
acceptance and final effort qualification remain open. No Windows validation,
per-task full suite or merge-coordinator handoff is part of this effort.

The production trace now omits decoded BL/EL and RGB pixel digests; explicit
`PLURX_DV_FRAME_HASHES=1` retains them for quality diagnostics. These hashes have
no independently known post-lossy-encode value, so they do not establish serving
authority. Coded access-unit/RPU binding, timestamps and process/source custody
remain mandatory. Ten focused controls preserve identical NUT pixels between
modes and retained authored metadata. A real 24-picture ROG window took 9.14
seconds with diagnostic scans disabled, still including startup and five preroll
pairs and excluding encoding. This remains too slow for realtime 4K playback;
it is not a steady-state or matched base-versus-FEL benchmark. Actual Vulkan
device and driver UUIDs, versions and IDs now accompany the runtime trace.
