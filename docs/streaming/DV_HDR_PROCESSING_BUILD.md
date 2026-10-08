# Dolby Vision processing — Sol 6.1 build handoff

**Status:** open — executable investigation and implementation specification;
backend feasibility is not yet proven · **Updated:** 2026-10-08 · **Base inspected:**
`a3158eadc9a4f26d2d8293b4a91f23a94c9012b6`

Read the [proposal](DV_HDR_PROCESSING_PLAN.md) for the product decisions and
quality model, and the [adversarial review](DV_HDR_PROCESSING_REVIEW.md) for
resolved objections. This document tells the builder what to inspect, build,
prove and stop claiming when a capability is absent. No commands below imply
that a new renderer or quality harness already exists.

The [M0 reuse findings](DV_HDR_PROCESSING_FEASIBILITY.md) record existing
implementations worth reusing for both destinations and their unresolved
correctness, packaging and reference boundaries.

## 1. Start here: scope, compiler and integration

Build two independently qualified playback improvements: DV-aware HDR10 and
FEL-preserving P7 to P8.1. Preserve the native DV and compatible-base fallbacks.
Do not replace system libraries on the ROG or modify library source files as
part of this implementation. Host HDR detection/kernel changes are separate.

Before Rust edits, read [AGENTS.md](../../AGENTS.md),
[development pipeline](../DEVELOPMENT_PIPELINE.md),
[validation](../VALIDATION.md) and the
[compile loop](../ci/AGENT-COMPILE-LOOP.md). Verify the actual compiler:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustc +1.97.1 --version
cargo +1.97.1 --version
cargo +1.97.1 check -p plurx-core --lib --locked
```

The last command passed on the inspected base, with 34 existing default-feature
warnings. It is not clean Clippy evidence. The host's Homebrew Rust 1.98.0 is
not the repository-pinned compiler. If 1.97.1 is unavailable, establish the
source-only cloud loop first: archive committed source, never transfer `.git`
or credentials, and keep the remote build cache warm.

Create `effort/dv-hdr-processing` from then-current main for implementation.
Use `codex/dv-<milestone>` task branches against that effort. This project has
shared files and does not qualify for the independent-file exception. Keep
source changes reviewable and serialize integrations. Refresh exact-source
compiler evidence after rebasing or merging main. The present documentation
branch is not the integration branch and does not contain implementation.

Follow the current workflow amendments in the canonical documents, including
Forgejo as the authoritative remote, normal hooks, local focused regressions,
and required effort/promotion evidence. Main-bound PRs start as draft, receive
one adversarial review with findings addressed, then enter the ready lane.
Dispatch `Effort development gate` manually before merging each task; it is
`workflow_dispatch` only and does not run merely because a task PR exists.
Require a green run for the candidate. Its compile checks do not replace
focused local unit evidence or final promotion qualification.
Do not use CI as a compiler. User-visible corrective commits use `fix(` or
`perf(` and carry real `Regression-Test: <path>::<test>` lines into the landing
message, including API merges. Do not invent regression names before tests
exist or treat a documentation review as implementation review.

## 2. Existing integration points

Paths are relative to the repository root. Names below exist at the inspected
base; confirm signatures again when implementation starts.

| Owner | Existing surface | Required work |
|---|---|---|
| Core policy | `crates/plurx-core/src/playback/mod.rs`: `RenderCaps`, `HdrRoute`, `source_grade`, `hdr_route`, `target_grade`, `dv_handling` | Add destination-aware strategy without changing native-copy and force/subtitle semantics |
| Source conversion | `crates/plurx-core/src/transcode/dvconvert.rs`: `convert_length_prefixed(sample, nal_length_size, out)` | Keep base-copy converter as fallback; retain MEL/FEL classification without calling it reconstruction |
| fMP4 conversion | `crates/plurxd/src/dvpipe.rs`: `Converter::for_init`, `convert`, report | Reuse tested timing/container behavior only where semantically appropriate |
| Metadata library | `vendor/dolby_vision/src/rpu/dovi_rpu.rs`: `ConversionMode::To81` | Inspect adaptation semantics; no assumption this is a FEL authoring engine |
| Video graph | `crates/plurx-core/src/transcode/pipeline.rs`, `decode.rs`, `encoder.rs` | Explicit supported decoder/renderer/encoder combinations and output contracts |
| Identity | `crates/plurx-core/src/transcode/recipe.rs`: `Recipe`, `PipelineDigest` | Include semantic processing and target policy in cache identity |
| Planning | `crates/plurxd/src/transcode/manager/plan.rs`: `needs_dovi_reshape`, `require_dovi_renderer`, `hdr10_grade_for_with_preference` | Separate P5 safety requirements from optional DV quality processing |
| Capabilities | `crates/plurxd/src/ffmpeg.rs`, `state.rs`, `main.rs` | Probe exact deployed graph and expose versioned facts, not filter names |
| Lifecycle | `crates/plurxd/src/transcode/manager/`, `vod/generation.rs`, `vodgen.rs`, `producer_spawn.rs` | Own processes, admission, generation retirement and restart fallback |
| HTTP decisions | `crates/plurxd/src/http/stream.rs`, `http/hls/create.rs` | Capture settings/capability snapshots and expose effective route |
| Settings | `crates/plurx-core/src/store/mod.rs`, `crates/plurxd/src/http/system.rs`, `http/developer.rs` | Add default-off fields, persistence and advisory readiness |
| UI | `crates/plurxd/src/web/pages/settings-developer.js`, settings panels/playback scripts, `detail/dynamic-range.js` | Two independent switches, graduation criteria and truthful processing detail |

Before web work read [web shell layout](../clients/WEB-SHELL-LAYOUT.md). Scripts
share global scope and load order; no ES module imports. If adding a file,
update assets, shell tags and layout table together. Prefer existing surfaces.
The public setting schema and fleet advertisements need additive defaults so
older clients/nodes continue using current paths; missing capability means
unqualified, not implicitly supported.

## 3. Proposed contracts: keep intent separate from observed work

These names are **proposed**, not existing Rust APIs. Resolve final locations
in M1 without duplicating policy between core and daemon.

| Contract | Minimum content and invariant |
|---|---|
| `DvSourceFacts` | Numeric profile/compatibility/level; RPU presence/validity; EL kind unknown/none/MEL/FEL; source identity and parser evidence. Unknown is not MEL. |
| `DvProcessingRequest` | Both saved preferences, destination, existing conversion permission and target-policy ID. Immutable for a running generation. |
| `DvBackendCaps` | Backend/dependency digest, profile and metadata subset, decoded BL/EL support, intermediate domain, target control, allowed decode/encode pairs, measured resource envelope and proof schema version. |
| `DvProcessingPlan` | Selected strategy, expected applied operations, fallback candidates, reason, output contract, target policy and source binding. No enhanced P8.1 without authoring proof. |
| `DvProcessingReceipt` | Actual accepted/rejected EL and RPU frame counts, dropped residuals, supported/applied operations, backend digest, effective output, failure/fallback reason and generation ID. |
| `DvTargetPolicy` | Peak/black luminance, gamut, mapping algorithm and parameters, static metadata policy, precision/dither, measurement provenance or named fixed-rendition policy. |

Prefer typed variants to combinatorial booleans. Distinguish base copy,
normalized base plus supported metadata rendering, full reconstruction with
supported mapping, and validated reconstructed P8.1 encoding. Unknown or
unsupported creative trim levels remain explicit even when FEL works.

The existing per-file P5 proof uses changed pixels. Do not reuse that as an
eligibility test: identity RPUs are valid. Known-answer fixtures and parsed
metadata/frame receipt prove processing capability; optional on/off hashes
are diagnostics only. A backend reporting “EL failed, base only” must never
retain a successful FEL receipt. Unexpected omission after FEL admission
fails the enhanced generation and enters the explicit fallback policy.

Persisted defaults follow proposal section 4. Save must succeed without a
capable worker. The existing DV conversion setting still controls whether P7
conversion is permitted. Settings and availability do not silently override
native compatible DV. Include tests for all combinations of the three switches.

## 4. M0 — bounded backend and reference spike

**Deliverable:** a reproducible feasibility receipt, small legal fixtures and
an evidence-backed backend choice. No production routing edits in this step.

Investigate these immutable source revisions, then record every additional
FFmpeg/decoder change and build flag required:

| Component | Research pin / current boundary |
|---|---|
| libplacebo candidate | `0d043c7f6f79cd3687c023454bdacbe615e4d96f`; inspect `pl_frame.enhancement_layer`, NLQ and intermediate export semantics |
| mpv reference integration | `36abaa32d00a7229ee206aae12dc0e97e7962dca`; API 367 EL attachment and base-only fallback illustrate integration, not an encoder backend |
| Production FFmpeg | Existing Jellyfin `8.1.3-1-bookworm` package pin; preserve current codec/filter features when evaluating a replacement |
| ROG experiment | Ubuntu FFmpeg `8.1.2-2ubuntu2` and libplacebo `7.360.1`; P8.1 difference evidence only |

Inspect the decoder's BL/EL extraction and frame association, including
hardware decoders that omit EL or side data. Trace actual frames through the
renderer. Test software and hardware independently. A GPU's HEVC support does
not prove dual-layer decoding. The libplacebo API's support does not imply
the FFmpeg `libplacebo` filter wires the enhancement-layer frame into it.

Build a minimal isolated harness that accepts a timestamped P7 fixture and
emits high-precision reconstructed frames plus per-frame association records.
It must expose whether output is before or after reshaping/display mapping.
If a custom FFmpeg bridge or linked helper is necessary, document that choice,
ABI, patches, license compatibility, redistribution obligations and upgrade
maintenance before adding it to runtime images. Prefer a maintained upstream
interface; submit general-purpose dependency changes with that upstream's own
contribution rules. Do not turn a local ROG package into a fleet dependency.

Select an independent reference: a proven separate implementation, authorized
reference renders, or analytic synthetic fixtures for bounded operations.
Record its supported metadata levels and uncertainty. If none can validate
commercial full-DV output, the spike may prove mechanics but must label
quality/conformance claims unresolved.

Acceptance is operation-specific:

1. Nonzero synthetic FEL changes reconstructed pixels by the known residual;
   omitted/misaligned EL negative controls fail. Identity/no-op fixtures pass.
2. BL, EL and RPU match display frames through B-frame reorder, seek, variable
   frame rate and discontinuity. No frame duplication/drop or timestamp drift.
3. Mapped HDR output has known target semantics and removable DV metadata;
   unsupported trims are enumerated. Record null for unavailable evidence.
4. Separately prove an authoring/adaptation backend can produce conforming
   P8.1 from the reconstruction without double processing. Header parsing alone
   is insufficient: compare decoded/rendered output to independent references.
   Also decode with DV disabled and validate the HDR10-compatible base against
   its separate base-picture reference, including color/transfer/range and
   highlight/shadow behavior. Keep both DV-on and DV-off output artifacts
   and reference comparisons in the receipt; reject the enhanced route if
   either contract fails. DV rendering cannot mask a broken base.
5. Record full graph resource costs at source raster/frame rate. No qualification
   from the earlier 640x360 experiment or a renderer-only benchmark.

If item 4 fails, document the exact missing operation and continue the HDR
branch if independently sound. If items 1–3 fail for FEL, keep FEL explicitly
unavailable; qualified base/RPU processing may proceed as a partial feature.
Do not call this completion of the full FEL objective. A receipt states what
remains to be built, rather than substituting normalization for reconstruction.

## 5. M1–M3 — processing, identity and safe lifecycle

M1 adds typed contracts and a test-only strategy resolver. Keep old behavior
until the new route has an end-to-end capability receipt. Model unsupported
profile, unknown FEL type, malformed/missing RPU, missing decoder EL and stale
proof separately. Bound parsing, memory, sample sizes and decode time; feed
malformed cases into the existing regression/fuzz infrastructure where useful.

M2 adds the selected processing graph and producer ownership. Existing
`dvpipe` conversion occurs after the copy muxer; it cannot be placed directly
ahead of an encoder without an input adapter. For the partial normalized
fallback, use a timestamp-preserving container or typed timestamped frames.
Do not revive untimestamped Annex B piping: it previously lost PTS/DTS and
reordered B frames. Validate time bases, edit offsets, audio priming, seeks,
B-frame display order, variable rate and subtitle timing through the adapter.

Any helper process belongs to the existing producer/generation lifecycle.
Account for both decoders, rendering and encoding in scheduler admission.
Hold existing source-identity fences and descriptors through every stage.
Bound queues with backpressure; cancellation, client disconnect, timeout,
node loss and reseek must terminate children and release all permits/files.
Do not spawn extra FFmpeg work around admission controls. Prefer one owned
pipeline where possible; helpers need explicit child/process-group ownership.

M3 wires runtime planning and cache identity. Include processing strategy,
engine/patch/proof version, source identity, decoder contract, output grade,
encoder parameters, target policy, metadata adaptation recipe, precision and
relevant user choices in the canonical recipe digest. Avoid a redundant key
that can disagree with actual arguments. Legacy base-only caches must not be
accepted as newly reconstructed output. Deterministic receipts should describe
what the recipe promises; observed deviations retire the generation.

Before publishing an init segment, preflight the graph and first usable
output. On failure before publication, select the next bounded fallback and
create its own recipe/generation. On failure after publication, retire the
old generation and let the session restart at a verified timeline anchor
with new init/manifest/session state as required. Prove actual clients reload
that state; merely changing a database grade is not a handoff protocol.
Preserve audio continuity as far as the established restart contract permits,
and report any user-visible interruption. Never splice base-only media into
a FEL/P8.1 generation. Each candidate is attempted at most once per restart;
circuit-break repeated backend failures to prevent retry loops.

Fallback ordering is destination-specific and bounded by existing policy:

| Destination | Candidate order when enabled and individually qualified |
|---|---|
| HDR10 | Original reconstruction + supported DV mapping; base/RPU renderer (normalized P7 if required); existing compatible-base HDR10 path; existing valid lower-grade fallback |
| P8.1 DV | Validated reconstructed P8.1; existing P7 metadata conversion/base copy; existing supported playback fallback |
| Native supported DV | Existing native copy; quality settings do not force re-encode |

Reject unsupported candidates before admission. Profile 5 never enters the
compatible-base candidate. HLG is not labeled PQ without actual conversion.
A cache miss is not a reason to ignore real-time limits and stall playback.

## 6. M4 — settings, fleet compatibility and UI

Implement the two settings and defaults in proposal section 4 using existing
store/API patterns. Older nodes receiving additive fields remain eligible
only for their old proven routes. New route capabilities have a versioned
schema and expire when executable/dependency/driver identity changes. Route
on the selected worker's proof, not the controller host's installed tools.
Probes are bounded, cached and invalidated; unavailable probes mean fallback.

Expose requested preference separately from effective processing. Readiness
is advisory in both HTTP and UI. Add Developer cards with specific waiting
conditions: backend/reference proof, production encoder matrix, bitrate
quality results and physical playback validation. Graduate each independently
to Playback when its complete matrix is met; do not move the HDR card merely
because P8.1 still needs work, or vice versa.

Implement the user-requested **HDR10-E** badge on web, Apple and Android
quality/playback surfaces, expanding to “Dolby Vision–enhanced HDR10” in
accessible details/tooltips. Drive it from actual processing receipts for the
effective generation, not from source metadata, settings, probe availability
or intended pipeline. Until confirmation, and on base-only fallback, show
HDR10. Native DV and new P8.1 output retain their DV format labels. The badge
covers qualified metadata-only or FEL-plus-metadata processing; details must
separately report FEL contribution and supported metadata levels. Reset it on
failed/replaced generations and confirm cross-client consistency. Include
identity-RPU cases: actual application can be valid without changing pixels.

Playback detail reports numeric profile, MEL/FEL/unknown, whether EL actually
contributed, applied metadata subset, target policy, output format and fallback
reason. It must not call L1-only mapping “all DV processing.” Reuse existing
source/delivery fields where practical and add backward-compatible detail.
No new per-title modal or permission flow is required for fallback.

## 7. M5 — reproducible quality and performance harness

Implement the benchmark design in proposal section 5 as an offline tool, not
a per-playback quality scan. M0 now supplies the bounded
[manifest-driven comparator](../../tools/dv_quality/run.py) in
`tools/dv_quality/`; its [implementation ledger](DV_HDR_PROCESSING_STATUS.md)
records the tiny-frame limits and executable workflow. It validates inputs,
retains input snapshots and leaves reference-error fields null unless an
independent reference is declared. M5 still needs full-raster streaming,
encoded-output adapters, SSIM, scene/corpus analysis and rate comparisons;
the M0 comparator is not the completed benchmark.

Each manifest records fixture digest/license/provenance, profile and metadata
levels, BL/EL/RPU association, expected residual behavior, reference origin,
reference decoder/render versions, target peak/black/gamut, and exact frame
intervals. Record reconstruction-domain frames separately from mapped frames.
Treat secure playback surfaces or OS screenshots as unsuitable reference pixel
captures unless their full color/capture path has independently been validated.

The minimum corpus includes synthetic nonzero/zero FEL, MEL, P8.1 identity
and nontrivial RPUs, P5 with valid normalization, bright/saturated highlights,
near-black gradients, motion/grain, scene cuts, reordered/VFR frames and
malformed/truncated metadata. Keep redistribution-safe tiny fixtures in tests;
large or licensed clips live in an external manifest with hashes and acquisition
instructions. Separate calibration/tuning clips from held-out evaluation clips.

For each relevant clip run base, BL/RPU and full reconstruction candidates,
then the HDR10 and P8.1 destination experiments separately. Do not compare a
DV-client rendering at one peak against HDR at another. Validate P8.1 metadata
and picture semantics first; invalid output has no quality score.

Use the [PU21 authors' implementation](https://github.com/gfxdisp/pu21) as a
starting point, pinning its revision and parameters. Disable optional camera
response/exposure correction: tone and color errors are part of this test.
Use pinned metric implementations with standard test vectors and explicitly
record conversion matrices, PQ absolute-luminance interpretation, range and
chroma upsampling. Delta E ITP and PU21-based PSNR/SSIM are complementary.
An optional VMAF result must name a model validated for this HDR use and its
pin; a stock SDR model is not the acceptance metric. Keep metrics in their
units, with directionality; do not invent a percent-quality composite.

Use at least four delivery-rate points around the intended production rate
(e.g. 0.75x, 1.0x, 1.25x and 1.5x), tuning to within a proposed 3% measured-rate
tolerance before comparing. Record actual bytes/duration and audio exclusion.
Use matched encoder preset/GOP/chroma/raster across picture-path comparisons;
report the original base-copy baseline separately without pretending it shares
the test encode. Compute rate savings only within overlapping monotonic
rate-distortion intervals, with method and uncertainty stated. Record each
path's encoding loss and total reference error independently.

Report per-clip and per-scene mean/p95/p99 Delta E ITP, PU21 scores, clipped
fraction, luminance extrema, worst-frame locations, frame counts and temporal
error traces. Aggregate with explicit weighting and clip-level uncertainty.
Difference-only runs keep reference-error fields null. Pixels changing is not
proof the new output is closer to the reference or preferred by viewers.

Blind viewing uses randomized labels/order, identical targets and playback
settings, ties and repeated sham comparisons. Record display model, measured
peak/black, picture mode, OS/compositor path, viewing conditions, viewer/clip
counts and uncertainty. Disable uncontrolled adaptive brightness and dynamic
picture enhancement. The ROG HDR10 result validates only that tested display
path; enhanced P8.1 also needs a physical DV client and an independently
trusted DV renderer for the quantitative comparison.

Pre-register acceptance thresholds in the M0/M5 receipt before evaluating
held-out candidates: zero structural/timing mismatches; analytic error within
known quantization tolerance; no systematic clipping/flicker regression; and
net reference-error benefit exceeding reference/measurement uncertainty for
supported workload/rates. Set allowed per-scene regressions explicitly before
seeing results. If only preferences are measurable, report preference evidence
without claiming absolute fidelity. Do not qualify a broad route from one clip.

As a proposed performance gate, require at least 1.25x sustained end-to-end
speed over ten minutes at each qualified raster/frame rate, inside the worker's
memory/concurrency budget, plus startup and seek behavior within the existing
playback SLO. Record actual SLO values from the then-current implementation
before testing. Measure cold and warm cases, simultaneous normal transcodes,
queue wait and p95 frame time; a renderer-only speed cannot qualify the route.
Exclude pairs that fail rather than silently taking resources from other jobs.

Receipt fields include run/schema ID, exact Plurx commit/tree, backend and
container digests, dependency patches, driver/GPU/CPU, fixture/reference hashes,
requested/effective operations, target policy, every command and exit status,
metrics/raw artifacts, performance, test names, and pass/fail/unknown per
capability. Keep durable evidence with hashes; `/tmp` is not a release receipt.

## 8. M6 — tests, qualification and rollout

Run the smallest focused tests that reject the old behavior, along with the
compiler checks required by touched surfaces. Existing starting points:

```bash
cargo +1.97.1 test --locked -p plurx-core --features hiqlite-store --lib transcode::dvconvert
cargo +1.97.1 test --locked -p plurxd dvpipe
node tests/web/settings-sections.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Confirm discovered test counts and that filters actually match. These commands
cover existing fallback conversion, settings contracts and documentation; they cannot verify the
new feature by themselves. Use `make unit-core` or the explicit feature above
for replicated storage. Follow canonical feature flags for daemon/cluster
integration tests. Before pushing, run pinned check, rustfmt and Clippy with
warnings denied on the affected feature surface; fix introduced issues and
record baseline problems rather than claim the baseline was clean.

Add focused tests with real names for these contracts, then record the exact
command and `Regression-Test:` anchors in each implementation PR:

| Test family | Required regressions |
|---|---|
| Routing/settings | Off retains existing behavior; native DV unchanged; all three conversion-switch combinations; saved On with unavailable node falls back without changing Save |
| Metadata | Identity accepted; invalid/missing RPU; unknown/MEL/FEL classification; nonzero analytic reconstruction; missing EL negative case; unsupported trims reported |
| P8.1 authoring | Correct profile/config/level, no EL or residual dependency, matched frame metadata and independent decoded/rendered reference; valid HDR10 base with DV disabled; no double mapping |
| Output contracts | PQ/BT.2020/Main10/range/static metadata consistency; no leaked DV config in HDR10; P5 and HLG controls cannot be mislabeled |
| Timing/lifecycle | B frames/VFR/seek/audio/subtitles; bounded queues; cancellation and child cleanup; pre/post-publication failure restart; no retry cycle |
| Identity/fleet | Old cache isolation; changed target/backend invalidation; worker-specific capabilities; stale proof and mixed-version fleet; source mutation fence |
| Product UI | HDR10-E only after actual DV-processing receipt; base fallback/unconfirmed stays HDR10; native DV unchanged; reset on generation changes; identity-RPU and FEL/no-FEL details; Developer readiness stays advisory; graduation entries; requested/effective details; existing `tests/web/settings-sections.test.js` contracts |
| Quality/performance | Held-out corpus at matched rates, negative controls, physical HDR and DV playback, concurrent workload and resource budget |

Use separate task ownership, but integrate serially where paths overlap:

| Milestone | Primary file ownership | Depends on |
|---|---|---|
| M0 spike | New offline harness/fixtures, dependency experiment and evidence docs | Independent reference and backend inspection |
| M1 contracts | Core playback, decode, encoder, recipe types and unit tests | M0 capability boundaries |
| M2 processing | Pipeline, daemon producer/manager lifecycle, adapter and timing tests | M1; backend proof for each supported operation |
| M3 routing/cache | Core policy, manager plan/start/publication, HTTP decisions and recipe | M2 end-to-end graph |
| M4 settings/UI | Store, system/developer HTTP, web settings/detail scripts, Apple and Android playback/quality presentation | M1 contracts, M3 effective-route reporting |
| M5 benchmark | Offline harness, metrics fixtures, corpus manifests and receipts | M2/M3 functional outputs; reference selection in M0 |
| M6 qualification | Regression catalog, packaging, operations docs and rollout evidence | All routes proposed for release pass their own gates |

M0 starts the benchmark harness; M5 completes it against product output. Add
new docs to the index in the same commit. Follow dependency projects' coding,
license and submission rules for upstreamable patches; Plurx-specific policy
and UI belong here, not in Ubuntu packaging or libdisplay-info.

Freeze task merges, integrate current main into the effort, and qualify the
exact candidate under the current repository promotion rules before release.
Package replacement must retain the existing media feature matrix, not merely
pass DV samples. Default-off canary deployment validates both switches on
qualified workers; preserve the previous image and rollback plan. Disabling
the preference affects new sessions; drain/restart affected generations using
the established lifecycle for an urgent rollback. Backend-specific caches
must never be reused by an older image as equivalent base output.

Completion means reviewed code, all relevant regressions, durable exact-tree
receipts, physical playback and measured net quality at supported rates. A
partial HDR route can ship with honest scope while enhanced P8.1 stays open;
neither may be described as full FEL support without its own proof.
