# Dolby Vision processing — implementation evidence and remaining work

**Status:** built for supported bounded routes; broader reference and hardware acceptance open ·
**Updated:** 2026-10-09 · **Integration:** `effort/dv-hdr-processing`

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
with independent adversarial review. Settings and generation-bound client
presentation are implemented; normal serving integration and the installed Linux
helper bundle have bounded local serving evidence in §22. Current-head qualification
and promotion receipts are recorded on [PR #968](http://192.168.4.7:3000/noirr/plurx/pulls/968). The [M1 contract](DV_HDR_M1_CONTRACTS.md)
initially introduced only additive core types, validation and pure/test-only
selection; code review approved that bounded Rust diff. On its effort base `03fa9d2`,
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
| M1 typed contracts | Merged in #933: 25 focused and six compatibility tests passed; registry intentionally empty in that first slice | Final serving qualification in M2/M3 |
| M2 processing and lifecycle | Built for supported bounded P7/FEL and base/RPU routes; owned adapter #946 and actual HTTP evidence in §22 | Broader metadata/reference scope and physical hardware acceptance |
| M3 routing and cache identity | Built: #964 and follow-ups cover first-window/generation paths, ownership and publication; §22 records actual reports, nonzero windows, seek, AAC and fresh compatible reopen | Exact late-metadata refusal diagnostic remains unproven; physical playback and hardware capacity remain open |
| M4 settings and HDR10-E badge | Built: settings #937, presentation #941 and accepted-control adoption/clearing #955; §22 verifies actual served-generation reports and compatible fallback. #960 keeps readiness advisory | Physical client/display acceptance; both preferences remain default-off while broader acceptance is open |
| M5 quality and performance | Matched 24-picture software-decode/NVENC comparison recorded in §16; sustained throughput unresolved | Held-out corpus, matched bitrate, physical playback and full graph performance; current 4K helper is slower than realtime |
| M6 release qualification | Portable Linux bundle #963 and follow-ups passed ARM64/AMD64 loader/manifest checks; §22 records bounded serving evidence | Exact-head unit, gate and promotion results are retained on [PR #968](http://192.168.4.7:3000/noirr/plurx/pulls/968); those receipts, not this source ledger, determine qualification |

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

Do not use this ledger as a production qualification receipt. HDR10-E
presentation is implemented, but a source label or saved preference
cannot enable it: only an accepted current-generation processing report can.
Native serving acceptance and final promotion remain outstanding. No measured
universal quality gain is claimed.

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
and the UUID's 16 bytes. It then checks the production registry, plan,
generation and current object shape. There is no caller-supplied independent digest/UUID pairing. In this
initial reporting slice, the registry was empty and response constructors
supplied no report; actual publication-backed issuance belongs to M3.

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
conditional on an actual generation report. This helper slice did not mint
one; normal serving integration owns that final publication boundary.

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
remained open in that slice. M5 comparative resource measurements were
subsequently completed in §16. Correctness controls and configured resource caps
alone are not a benchmark. No broad CI or full suite was dispatched for these
helpers.

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

At this stage, normal VOD routing, audio, cache/publication receipts and
fallback integration remained open; §17 records the later implementation.
Base-only P5/P8 processing has a separate profile-aware path and cannot inherit
a FEL receipt. §16 records the matched resource measurements. Independent
quality, device acceptance and final effort qualification remain open. No Windows validation,
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


## 15. Base-only processing helper

The separate base mode applies the public libplacebo Dolby mapper to supported
fresh P5, P8.1 and P7 inputs. Its source/timing/RPU observations identify one
decoded layer and explicitly report no FEL contribution. Thirteen focused
controls cover polynomial and MMR mapping, omitted FEL, metadata refusals,
window limits and diagnostic mode. The [helper guide](DV_PROCESSING_TOOLS.md)
contains the exact replay command and supported envelope. Production selection
and effective-report projection were subsequently implemented in §17.

The independent scalar comparison did not meet its original four-RGB48-code
tolerance: P5 reached six codes and the P8 affine/piecewise cases reached 27.
A later same-encode comparison found identical P5 YUV and at most one chroma
code of difference for P8; that diagnostic does not replace the failed bound or
establish a general quality improvement. Shared finite RGB48 packing preserves
the previous expression across 1,830,864 inputs and four rounding modes.
Decoder counts remain one thread per layer. Increased-thread experiments are
not production resource settings or evidence of realtime playback.


## 16. First matched real-source resource comparison

On 2026-10-08, a private ROG run selected the same 24 pictures from the
720–721-second window of the P7 FEL source used above. The processed cases used
helper source `4662bb5dffeda73bb57f0f199a66b6d989e6d26b` (merged in #952),
binary SHA256 `10881d42d734f3f5ceda333bbeace7b07dc0d2e38dbbc0145d9d197bc7213a16`.
Both used five preroll pictures, one software HEVC thread per decoded layer,
diagnostic pixel hashes off, and the actual RTX 4080 Laptop Vulkan device.
Every case ran under a two-CPU quota and 4 GiB RAM limit. The ordinary case
used the compatible P7 base with Dolby metadata removed before encoding.

| Processing plus the same NVENC recipe | Elapsed seconds | CPU seconds, user + system | Cgroup peak RAM, bytes | Encoded bytes |
|---|---:|---:|---:|---:|
| Ordinary HDR10 base | 6.536 | 7.595 | 665,460,736 | 2,646,850 |
| Base with DV RPU processing | 12.934 | 15.274 | 877,154,304 | 2,619,285 |
| FEL reconstruction with DV processing | 13.622 | 15.668 | 1,340,755,968 | 2,785,301 |

In this window, FEL added 0.688 seconds (5.3%) over processed base-only output.
Base-only processing took 1.98 times the ordinary elapsed time; FEL processing
took 2.08 times. These are measurements of this bounded experiment, not a
prediction for every movie, a deployment capacity estimate or sustained
throughput. All three cases used software decoding; this is not a comparison
with the deployed ordinary hardware-decode or direct-copy path. The ordinary
FFmpeg 8.1.2 decoder also differs from the helper's pinned FFmpeg 9 build and
does not share its aggregate IO or BL/EL pairing guards.

The common encoder was host FFmpeg 8.1.2, `hevc_nvenc`, Main10, preset `p1`,
constant QP 18, no B frames, GOP 24, 24 fps, limited-range BT.2020/PQ and
center-sited P010. Processed RGB48 was explicitly converted with zscale from
full-range BT.2020 RGB to limited BT.2020 NCL. Source PTS were shifted by
exactly 720 seconds before encoding. Both processed outputs consumed their
complete NUT stream; the ordinary output was capped at 24 pictures **after**
source-time trimming. Actual pre-encode source PTS and all encoded PTS matched.
An earlier input-duration cap counted preroll and emitted only 19 pictures;
that trial was rejected, preserved and replaced by the corrected measurement.

Encoder completion followed helper completion by 0.415 seconds for FEL and
0.268 seconds for base-only processing. Those tails are not the encoder's
isolated cost: concurrent color conversion and raw-pipe backpressure are
included in the elapsed measurement. Audio, GPU utilization and peak VRAM
were not measured. Cgroup peak RAM is not a VRAM measurement.

Same QP is not matched bitrate, and a larger output is not evidence of better
quality. A nearest-sampled 32×18 region grid from the first encoded frame
showed FEL/base differences up to 13/3/3 ten-bit Y/U/V codes. That demonstrates
a changed picture only; it is neither a full-frame metric nor an independent
Dolby reference. Matched-rate quality, broader sources and actual serving
acceptance remain open. The completed retained measurement receipt is SHA256
`5a09e4e8bfae4aa39240861be2b8f113e0bbfd82539ad4ed13745d2b7ab14bbf`.

A separate private NVDEC experiment preserved all 24 decoded BL/EL hashes,
raw RPUs, timestamps and rendered RGB hashes. Its clean software/NVDEC pair
took 9.064/8.844 seconds without encoding (eight-CPU quota, four software
threads per layer versus one hardware-decoder thread). CPU time fell from
15.346 to 7.897 seconds, but elapsed time improved only 2.4%. It used the
native HEVC decoder with CUDA acceleration, downloaded P010 and converted it
exactly to planar ten-bit samples. That scratch implementation is not merged:
the CPU saving did not resolve the wall-time bottleneck, and no real-time
claim follows. It used pinned `nv-codec-headers` commit
`57f8cc0bb68e5f16f3787ea92cea59000f7bf97f` and a separate FFmpeg installation;
no host package or production service was changed.


## 17. Final serving integration and packaging

The normal-serving implementation merged in PR #964 after adversarial review
and exact-source compile and focused regression checks. It admits the first bounded window before selecting
the processed route, retains process/resource ownership, and uses the existing
publication path. A receipt must describe freshly published media in the
accepted generation; replayed retained media cannot establish a new producer's
processing claim. Later failure excludes that processing attempt for an ordinary
reopen of the same playback, without requiring predecessor fields from clients.

The HDR and FEL preferences remain independent and default off. HDR processing
does not require permission to convert to Profile 8.1. FEL reconstruction for
Profile 8.1 additionally requires that existing conversion permission. Native
Dolby Vision precedence and compatible fallback remain in place. The default
window budget remains eight seconds. The §16 4K source measurements
are slower than realtime; neither startup success on a tiny fixture nor a
passing build proves that ordinary 4K windows will meet this budget.

Linux packaging builds private pinned FFmpeg decode libraries, libplacebo and
libdovi separately from the existing production encoder. The bundle is intended
for `/usr/lib/plurx/dv-processing`, with source, ABI and artifact identities.
PR #963 merged this packaging after independent review and successful ARM64
and AMD64 helper-only builds. Minimal Debian runtime checks verified the
manifest, ABI 374/63/63/61, Vulkan provider definitions and all three executable
loader/usage checks. AMD64 compilation used emulation; it is not native
performance evidence. Host Vulkan discovery is preserved; no machine-specific
ICD file is installed. Two focused manifest integrity regressions passed.
Neither the production image nor the full suites were built for this task.

PR #958 fixes the exclusive source-window boundary: a later unsupported RPU
cannot invalidate an already complete prior window, but the later window must
refuse it. Actual FEL and base-only controls decoded all 48 requested frames;
separate later-window controls refused the unsupported metadata. The existing
NTSC B-frame control also passed. PR #960's settings wording passed all 55
settings-file checks and independent review. Neither task ran the full suites.

Reviewed runtime source `5a872cded` passed workspace all-target Clippy with
denied warnings and the normal Linux daemon compile on Rust 1.97.1. The actual
FFmpeg-to-publisher regression retained all 48 video frames from two to four
seconds, aligned AAC within one encoder frame and audio through the endpoint.
Focused final regressions also passed for retained replay, incarnation-bound
public reporting, compatible peer control and failed-episode exclusion. These
checks do not replace normal HTTP serving acceptance.

Final acceptance must use the normal daemon's HTTP create, control, HLS and
media endpoints, including audio, a nonzero seek, multiple supported windows,
a later unsupported window and ordinary reopen. Synthetic 256×144 fixtures qualify the Profile 8.1 copy route but clamp below
the HDR10 output geometry; HDR10 acceptance uses 1920×1080 sources. The 4K
resource case must retain the eight-second budget and demonstrate compatible
fallback if it is exceeded. The final bounded results are recorded in §22; no general
serving result is claimed here.


## 18. Verified inputs for final HTTP acceptance

**Status:** independently verified source fixtures, 2026-10-09. Processed
HTTP serving acceptance is recorded separately; this preparation establishes
no successful processed daemon result.

Normal HDR10 processing inherits the ordinary source-height clamp. A 256×144
source meets the minimum transcode contract but its clamped output is outside
the qualified HDR10 points, so requesting 1080 does not make that source an
HDR10 acceptance fixture. The prepared 1920×1080 P5/P7/P8 variants qualify the
source geometry; small P7 remains useful for bounded Profile 8.1 routing.
The separate 3840×2160 synthetic P7/FEL variant checks resource refusal under
the unchanged eight-second window budget. It is not real-movie quality or
native ROG performance evidence.

Two fixture errors were preserved and corrected in separate `*-zero` outputs.
The compound muxer omitted average/nominal frame-rate fields, causing a
`293/12` probe result despite encoder PTS at 24 fps. It now copies both fields
from the encoder stream without rewriting packet PTS. Default AAC muxing also
shifted video by +21 ms to avoid negative priming timestamps. The corrected
mux uses `-copyts -avoid_negative_ts disabled`: first video PTS is actually
zero, first AAC PTS is −21 ms, and both frame-rate fields are `24/1`.
Every picture PTS was checked against `frame / 24` within 0.501 ms of the
Matroska clock. Every encoded video packet payload hash matches its parent,
so source pixels and RPU payloads were preserved by the timing correction.
The renderer's zero initial source-origin guard was not relaxed.

Supported fixtures contain 144 decoded pictures and independently parsed
RPUs. The 1080 late P7 case contains 66; after 48 valid RPUs, Level 5 top and
bottom offsets become 540/540, leaving no active 1080-line area. Earlier
32/32 offsets were invalid only for the original 64-line source and cannot
qualify the enlarged late-refusal case. Ten-bit PQ/BT2020 NCL, AAC, profile
5/7/8 compatibility 0/6/1 and P7-only enhancement configuration were checked.

The compact [fixture reproduction archive](../evidence/dv-processing-2026-10-09/dv-fixture-reproduction.tar.gz) is
61,575 bytes, SHA256
`9cea3c16f462f203186ea47d94e7b3000305bbce5c36ecf2847b617cdabc5ef2`.
Its sorted, timestamp-normalized inventory contains source/patches, frozen
RPU/editor inputs, recipes, receipts and dependency identities. The internal
manifest SHA256 is
`046d73d8e4bbeaba842c6c5d689fbb0878e72c1660fa1392649a77c3603f5055`.
Generated movies, credentials, daemon configuration and user movies are
excluded; their synthetic output hashes are retained in receipts.

The muxer links the helper-pinned FFmpeg revision
`bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa` (avformat 63.1.101). The recorded
fixture encoder is FFmpeg 5.1.9 with libx265 ultrafast in local ARM64 image
`sha256:96951921bc396f9fb96e579d9c15b83abe8b488390f8a7851c3754b2d96114c9`.
The independent parser is `dovi_tool 2.1.2`, built offline from existing pinned
source revision `83e1fdad6dcd5995556235946e7c5c0f9010d5a1`; its observed
macOS executable SHA256 is
`ab0c865bd8b28b9be59d7dd64ca061c305bb65b6b31eee7ade3d8bd6143ef2bc`.
The manifest distinguishes encoder identity from helper dependency identity.

After extracting the archive, use an existing dependency prefix built by
`tools/dv_processing/build-dependencies.sh` with pinned Rust 1.97.1. Select an
FFmpeg encoder with libx265/lavfi/AAC and an independent parser explicitly:

```bash
python3 fixtures/reproduce.py 1080 --out /absolute/new-output \
  --ffmpeg /path/to/ffmpeg --ffmpeg-prefix /path/to/dependencies/ffmpeg
python3 fixtures/verify.py --out /absolute/new-output \
  --ffmpeg /path/to/ffmpeg --ffprobe /path/to/ffprobe \
  --dovi-tool /path/to/dovi_tool
```

These source receipts do not prove daemon adoption, audio playback, multiple
processed windows or ordinary reopen. Those require actual advertised HTTP
bytes and the fresh publication marker. Parsed metadata differences prove
adaptation, not FEL visual gain. A/V checks using a 100 ms tolerance are an
acceptance bound, not a frame-perfect synchronization claim. Nonzero initial
source origin remains a documented fallback limitation of the supported
renderer scope rather than being hidden by modified probe facts.


## 19. Serving checks found and corrected two integration failures

PR #967 corrected the reconstructed-video encoder clock. The real HTTP path
reached an adapter that expected `1/<timescale>`, while the builder emitted
frame-duration notation. The reconstructed route now binds the encoder clock,
filter time base, mapped PTS and MP4 timescale to the same rational grid. Its
actual builder-to-adapter regression covers Software/NVENC, 24 fps/NTSC and a
nonzero window with AAC. This argument-contract check is not an emitted NTSC
media qualification. Ordinary output clocks are unchanged.

PR #970 retains completed output in a private named temporary file through
its held-descriptor probe. On the Docker macOS bind cache, an anonymous file's
original descriptor remained valid but reopening it through `/proc/self/fd`
failed with `ENOENT`. The same bytes probed successfully from a named file on
that cache and an anonymous file on the container overlay. The actual
completed-MP4 regression verifies HEVC Main 10 headers, `0600` permissions and
automatic removal after the probe. Input custody and header validation remain
unchanged. Both fixes passed their smallest focused regression, independent
review and the normal pinned compiler/lint hook; no full suite was repeated.

The HTTP acceptance daemon is built from committed source, with the normal
Jellyfin FFmpeg 8.1.3 encoder/probe and the separately pinned static bound probe.
A temporary diagnostic wrapper was removed before acceptance. The helper uses
software Vulkan on the local ARM64 Linux container. These checks can establish
serving mechanics and conservative fallback, not native ROG throughput or a
physical Dolby Vision display result.


## 20. Complete publication and keep reports bound to their viewer

Normal HTTP checks exposed two further integration defects. The Profile 8.1
encoder produced valid pictures and metadata, but packet authoring dropped
its completion trailer. The unchanged publisher therefore refused to commit
the output. HDR10 windows did publish successfully, but starting each next
prefetch erased the prior processing report before clients could observe it.

Reviewed commit `d1c6216e8d2c61ac37c72bb0f41b0c31827d7adc` fixes both boundaries.
Profile 8.1 authoring requires the authentic input completion trailer and no
unconsumed remainder. Since RPU insertion changes sample sizes, it regenerates
a version-1 zero-entry random-access footer for the actual track identifiers
and its matching size record instead of copying invalid old offsets. The
publisher's completion requirements remain unchanged. A retained three-picture
NTSC control now passes through the real Rust author and publisher; missing
and truncated completion inputs are refused.

HDR10 reporting retains the completed publication during ordinary prefetch.
Its allowance belongs to the physical viewer attachment and binds once to the
accepted incarnation UUID. Replacement, accepted seek, retained replay and
failure cannot reuse that allowance. A producer captures the report revision
when it starts; a seek changes that revision under the publication lock, so
an older producer cannot restore the retired report. The prepared first
window retains its original revision. Owner lookup completes before the
final synchronous report projection; independent review caught and corrected
the opposite ordering, which could return a report invalidated during an
await. The paused-query regression exercises the portable authorization
state, not the complete Linux HTTP endpoint.

The admitted Profile 8.1 plan also projects its actual destination and encoder
into Start responses and HLS metadata. Its physical method remains transcode,
with a PQ-compatible base. An optional bounded, session-bound owner header
carries the marker between peers; the existing strict START JSON body stays
compatible with older readers. Recovered descriptions preserve the admitted
marker and actual encoder. Source tags alone cannot grant the marker.

Pinned Rust 1.97.1 compiled the affected daemon and passed the real
source-author-to-publisher regression, two report-owner regressions, prepared
Profile 8.1 Start projection, peer-header compatibility and retained-replay
negative. The core completion regression passed with `hiqlite-store` enabled.
Seven ownership inventory checks passed, followed by the normal formatting,
workspace all-target Clippy and embedded JavaScript hook. Independent final
review approved the committed DV correction and verified its source hashes.
These focused checks do not claim final HTTP acceptance or qualification of
the complete effort; those results are recorded only after execution.


## 21. Final-window duration belongs to the selected video track

The first actual Profile 8.1 HTTP case published two complete windows with
48 reconstructed FEL pictures each, but prefetch of the final window refused
`source EOF extent unavailable or inconsistent`. For the multi-track Matroska
fixture, FFmpeg left `AVStream.duration` unavailable while the selected video
track's `DURATION` tag declared `00:00:05.999000000`. That endpoint matched the
last decoded picture plus its duration; the audio/container maximum was
6.000 seconds. Requiring a single-stream input for the fallback declaration
therefore refused a valid video ending whenever this duration representation
accompanied audio.

Commit `deaa1496868bf559e5704a009436df021b296e25` reads only the selected
video's fixed decimal duration clock
when its ordinary stream duration is unavailable. Parsing is bounded, minute
and second fields are checked, and nanosecond arithmetic rejects overflow.
The declared endpoint must still match the decoded ending within the existing
one-source-tick tolerance. Actual EOF, requested-window coverage, zero initial
source origin and the source-file fence are unchanged. Audio duration cannot
stand in for video duration; unknown, malformed and inconsistent declarations
remain refusals. A known positive container extent must also not end earlier
than the decoded video, within the same tolerance. This is an upper bound,
not a substitute for the selected video's declaration; a later AAC tail is
valid. The first patch passed the selected-track controls but the retained
long-terminal regression exposed this missing contradiction check. Its
1.708-second video declaration conflicted with a mutated 0.600-second
container declaration. That attempt remains evidence of the caught defect,
not final acceptance.

The warm ARM64 helper build passed with warnings denied. The focused real
renderer control passed the final-picture case with AAC in both FEL and
base/RPU modes. Missing video duration, malformed clock, invalid minute and
mismatched endpoint each refused with its specific cause and no completion
record. A seventh case refused the shorter-container contradiction, and the
existing long-terminal control again refused with its original cause. The
independent final source review approved the committed correction. Final bounded HTTP results with the rebuilt helper and matching daemon
source identity appear in §22; the earlier failed attempt remains retained.

The AMD64 helper bundle also compiled offline against the retained public
library prefix, with normal `-Wall -Wextra -Werror` flags. Manifest validation
passed in both the compiler image and the existing minimal runtime image;
all three helper binaries loaded and returned their expected usage refusal.
No daemon or dependency rebuild was needed for that packaging check. Its
source aggregate SHA256 is
`cd20e90a091990675be2c8660305407432a0deacb5519fb7965b18dd0c2f0a8b`,
manifest SHA256 is
`5921263b0adc5d6df324dff1cad74144d42aee96eb3c9fd0913de56798ae5337`,
and renderer SHA256 is
`962e62499840d9ef471c20f8d732aa6e0c4690f4e77b5c5d7d89bad11473645f`.
The observed helper ABI remains 374 / avcodec 63 / avformat 63 / avutil 61.
This emulated AMD64 loader/build proof is not a performance measurement.


## 22. Final bounded HTTP serving evidence

The [HTTP summary](../evidence/dv-processing-2026-10-09/http-summary.json)
and [artifact manifest](../evidence/dv-processing-2026-10-09/http-manifest.json)
bind the final local Linux daemon to `deaa1496868bf559e5704a009436df021b296e25`.
Its Rust 1.97.1 build passed. The matching ARM helper manifest is
`8f239187b834882f12f6cea16737d070e552c688a9c9e993a8b7ebbaf9041760`.
The normal Jellyfin FFmpeg 8.1.3 path and unchanged eight-second private
window limit were used with software Vulkan. The export retains 487 artifacts
including failed attempts; credentials and private configuration are excluded.

- P5 and P8 produced actual HEVC Main 10 PQ HDR10 with AAC and effective
  enhancement reports. P8 correctly reported base/RPU processing without FEL.
- P7/FEL survived one genuine in-session seek: accepting it cleared the old
  report; the actual 4–6 second source window then published 48 reconstructed
  pictures with 48 FEL/NLQ contributions and a fresh report. The collector
  honored one explicit segment-pending retry within its 20-second bound.
- P7 to P8.1 published nonzero 2–4 and 4–6 second windows. Independent parsing
  found 48 profile-8 RPUs per window, associated with the corresponding
  source frames, alongside 48 FEL/NLQ contributions and synchronized AAC.
- Synthetic 4K exceeded the unchanged eight-second private-window deadline.
  Ordinary compatible HDR10/AAC succeeded, including a fresh same-playback
  reopen with the failed enhancement excluded and no HDR10-E report.
- The late malformed-metadata case instead reached a private-output resource
  refusal. Fresh compatible fallback passed, but the intended invalid active
  area diagnostic remains **unproven**. Its strict analyzer failure is retained.

A/V agreement uses a 100 ms bound. These are synthetic serving, timing,
metadata and fallback results, not physical ROG/display acceptance, sustained
4K throughput, independent reference-picture agreement or Dolby conformance.
The full-unit campaign and promotion result are recorded on PR #968 against
its final integrated head; the earlier serving source does not certify later
main changes. Both optional features remain default-off in Developer settings
with the broader reference, hardware and metadata limits still visible.


## 23. Qualification evidence stays bound to its source

Promotion [PR #968](http://192.168.4.7:3000/noirr/plurx/pulls/968) retains the
actual full-unit outcomes, source identities and gate receipts. The first
Android JVM run passed 1,092 tests with no failures or skips; its combined
command then failed lint on a pre-existing restricted Activity key API. The
public Window.Callback correction preserves observation, dispatch and event
consumption. Only the affected 29 tests and lint ran again, and passed. The
original failed command receipt is preserved unchanged alongside the passing
unit outcomes and focused repair. Windows was explicitly waived.

The first remote campaign stopped in corrective-history preflight, before any
of the remaining unit suites ran: three client fixes needed source/test anchor
rows. A separate local Linux compile passed but warnings-denied Clippy found
six Linux-only DV runtime lint errors. Their correction keeps the same runtime
behavior: a callback type alias, standard divisibility predicates and narrowly
justified argument-count annotations. These failures remain recorded; they
are not successful qualification claims. Subsequent commands and final gate
results belong to the promotion record, so this document need not change the
source being qualified just to announce its result.
