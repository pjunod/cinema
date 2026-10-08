# macOS video processing implementation — experiments, contracts and delivery

**Status:** implementation active; see the [execution ledger](MACOS-VIDEO-PROCESSING-STATUS.md) · **Written:**
2026-10-07 · **Executes:**
[the macOS processing proposal](MACOS-VIDEO-PROCESSING-DESIGN.md) ·
**Source inspected:** `890bca0fc186d00f7b10a379cd7dd2f1a37dc37c`.

Read the design first for the memory model, scope and acceptance thresholds.
This document is the build handoff. It names the changes to make and the
proof each milestone must leave. The [review record](MACOS-VIDEO-PROCESSING-REVIEW.md)
separates the original findings from the author corrections. No milestone is
implemented by this document.
Do not replace existing Dolby handling, remove AC-4 support, alter playback
ownership, or change source files to simplify an accelerated graph.

## 1. Execution order and integration ownership

**Execution amendment, 2026-10-07:** the user has authorized a managed,
parallel implementation in separate task-owned clones. The
[status page](MACOS-VIDEO-PROCESSING-STATUS.md#2-user-directed-workflow-and-file-ownership)
records ownership and the current instruction: proper commits integrated on
one effort branch, batched main-bound PRs, one adversarial review when a PR
is ready, then fast-lane validation after addressing findings. Unit suites
are not run during building; compile/lint and feasibility experiments remain
available. Retain valid passing checks and rerun failures rather than repeating
all suites. This amendment supersedes conflicting per-task PR/test/review
wording below and the earlier instruction against parallel agents. It changes
execution, not color correctness, metadata, runtime compatibility or evidence
requirements. Never use the user's checkouts for this work.

**Merge handoff amendment:** the user assigned final queueing, batched test
execution and main merging to
[Coordinate PR merge batches](codex://threads/01a11907-f720-71b1-8c51-89902b919e6f).
This effort prepares the integrated PR, performs the adversarial review and
addresses findings, then hands off exact commits, regression commands, review
dispositions and evidence/limitations. Do not duplicate the coordinator's unit
test runs or merge independently. The coordinator handles superficial test
fixes without behavior changes; failures needing behavior changes return to
the implementation effort for a reviewed fix. This later amendment supersedes
any instruction below assigning final test/merge execution to this manager.

Use one `effort/macos-video-processing` integration branch created from the
current intended main base, with `codex/macos-video-*` builder branches.
Parallel work has explicit disjoint file ownership; shared planner and argv
changes integrate sequentially through the manager. The effort remains the
integration point; this does not invoke the direct-to-main exception.

Follow [AGENTS.md](../../AGENTS.md) and the current
[development pipeline](../DEVELOPMENT_PIPELINE.md). Establish the pinned
Rust 1.97.1 compiler loop before any Rust edit, verify `rustc --version`,
and use the [source-only compile loop](../ci/AGENT-COMPILE-LOOP.md) if needed.
The cloud loop establishes compilation; it cannot establish Mac hardware
behavior. Re-run against the exact integrated tree after the base changes.

Run milestones M0–M6 for the initial progressive SDR/HDR10 scope. E1–E4 are
separately reviewable extensions with their own stop/go evidence; an extension
can finish as “measured, retained incumbent” without blocking a useful initial
release. Do not silently widen the initial scope to get a feature count.

| Step | Work owned | Files/surfaces expected to change | Depends on |
|---|---|---|---|
| M0 | Exact runtime and dependency inventory; baseline | Experiment output; existing bench harness only if needed | Reviewed design |
| M1 | Reproducible native/Metal graph experiment | Proposed `scripts/bench-macos-video`, harness tests, pinned FFmpeg packaging when necessary | M0 |
| M2 | Typed processing/surface contracts and identity | Core transcode pipeline, decode, recipe and decoder-selection tests | M1 viable graph |
| M3 | Runtime inventory, settings and diagnostics | Daemon probe/startup/manager, settings API/store use, Developer UI, metrics | M2 |
| M4 | SDR processing through production owners | Core argv/VOD, daemon manager, rolling/VOD integration tests | M3 |
| M5 | HDR10 native/Metal production processing | Same plan/argv seams, color tests and fixtures | M4 |
| M6 | Production qualification, packaging and graduation | Install surface, diagnostics/docs, regression and evidence records | M5 |
| E1 | HLG/Dolby acceleration | Color-class resolver, side-data contract and corresponding fixtures | M5 |
| E2 | Subtitle processing | Burn preparation/compositor and cue regressions | M5 |
| E3 | Interlace and Live TV | Live plan/argv/delivery and field/caption tests | M4; independently measured |
| E4 | HEVC/Main10 output | Encoder, presentation/manifest/cache/cluster contracts and clients | M5; independently measured |

Each builder reports actual touched files and commits to the manager.
Integrate against the current effort before claiming verification. The manager
records those changes in the batched PR and keeps ownership on the status page.

### 1.1 Start with experiments, then ship SDR and HDR10

The [design's priority order](MACOS-VIDEO-PROCESSING-DESIGN.md#31-scope-to-implement-first)
sets the default execution sequence:

1. **M0/M1 first:** inventory the actual daemon and Jellyfin FFmpeg build,
   establish the baseline, and compare the native/Metal candidates. Include
   the four-way P5 decode/renderer experiment now, with actual hardware use
   and metadata/frame association verified. Record a stop/go decision for
   each candidate before committing to its production integration.
2. **M2/M3, then M4:** establish contracts, recipe identity, capabilities and
   fallback; integrate hardware decode → native SDR scaling → hardware
   H.264 encode. This is the lowest-complexity production processing path.
3. **M5, then M6:** integrate the measured HDR10-to-SDR mapper, preserve the
   incumbent Jellyfin dependencies, and qualify/package the initial release.
   Deliver progressive SDR scaling and HDR10 tone mapping together as the
   initial scope, retaining the incumbent wherever a candidate fails proof.
4. **E1 next by default:** evaluate P5 hardware decode with CPU `tonemapx`
   first, then evaluate GPU Dolby rendering separately. Do not make a
   decoder-only improvement depend on a new renderer. Both still require
   E1's strict execution-time metadata enforcement and quality acceptance;
   HLG keeps its own qualification.
5. **E2 when burn-in is a demonstrated bottleneck:** measure processing
   followed by CPU burn at output resolution before adding GPU compositing.
6. **E3/E4 according to demand:** treat Live TV/deinterlacing and HEVC/Main10
   output as separate follow-ups. Their existing dependencies still apply;
   neither must wait for every other extension or delay the initial release.

M1's early Dolby evidence informs E1 without enabling Dolby acceleration in
M4/M5. Keep these extensions out of the initial delivery so its benefit can
be measured and shipped independently. A negative experiment is a valid
result; do not implement an accelerated path merely to complete this list.

### 1.2 Effort and what determines completion

These are qualitative engineering estimates, not elapsed-time promises.
Parallel builders shorten independent implementation work; they do not
remove serial integration, hardware measurements, visual review or client
qualification. The larger cost is preserving delivery and color contracts,
not adding a VideoToolbox filter name.

| Work | Relative effort | Main source of effort / current priority |
|---|---|---|
| M0/M1 package inventory, fixtures and harness | Medium | Reproducible comparisons and complete Jellyfin dependency preservation; start here |
| M2/M3 contracts, identity, runtime probes and settings | High | One consistent plan across cache, recovery and workers; shared foundation for every route |
| M4 progressive SDR | Medium after foundation | Production rolling/VOD integration and unsupported-source controls; first processing route |
| M5 HDR10 | High | Precision, metadata, peak/temporal behavior and actual visual acceptance; second route |
| M6 qualification and packaging | High, evidence-dependent | Real daemon/client matrix, concurrency/startup/soak, install/rollback and additional hardware access |
| E1 P5 decoder-only improvement | Medium–high, feasibility-dependent | Prove actual hardware reconstruction and effective Dolby metadata transport into the existing CPU renderer |
| E1 GPU Dolby rendering and strict metadata enforcement | Very high | Renderer-boundary enforcement, per-frame state after seeks/reordering, possible pinned FFmpeg patch and visual qualification |
| E1 HLG | Medium–high | Independent reference-white, color and temporal qualification |
| E2 subtitles | Medium for native processing then CPU burn; high for GPU compositing | Preserve shaping, bitmap geometry, alpha and cue lifecycle; pursue measured bottleneck first |
| E3 interlace / Live TV | High | Independent cadence, caption, startup, reconnect and paced-delivery contracts |
| E4 HEVC/Main10 output | High | Coordinated encoder, negotiation, container, manifest, cache, cluster and actual-client changes |
| Cross-platform P5 follow-up | Separate high effort per supported backend family | Vendor/interface-specific hardware and metadata evidence; Mac proof is not portable qualification |

Do not sum these labels into a delivery estimate. Narrowing unsupported
sources honestly can keep the initial SDR/HDR10 batch useful while its
Developer card lists remaining qualification. No measured benefit means
retain the incumbent rather than spend implementation effort maintaining an
unjustified alternative. The status ledger records that outcome explicitly.

## 2. Source map and interfaces to preserve

| Existing seam | Responsibility / required change |
|---|---|
| [pipeline.rs](../../crates/plurx-core/src/transcode/pipeline.rs): `Pipeline`, `filters`, `pairs_with`, `handles`, `fallback` | Add explicit Apple variants and exact class semantics; keep existing variants stable |
| [decode.rs](../../crates/plurx-core/src/transcode/decode.rs): `FrameDomain`, `DecodeSurfaceContract`, `ResolvedTranscode`, `surface_contract` | Represent VT surfaces and validate the complete selected graph |
| [mod.rs](../../crates/plurx-core/src/transcode/mod.rs): `video_filters_for_contract`, resolved decode argv | Emit actual selected representation and processing, not merely a manager default |
| [encoder.rs](../../crates/plurx-core/src/transcode/encoder.rs): `encode_args_for`, `video_codec_for` | Preserve rate control and H.264 output initially; Main10 is E4 |
| [vod.rs](../../crates/plurx-core/src/transcode/vod.rs) | Carry the same plan into independently produced VOD segments |
| [recipe.rs](../../crates/plurx-core/src/transcode/recipe.rs): `Recipe::hash`, `PipelineDigest` | Bind effective processing identity, preserving unaffected hashes |
| [pipeprobe.rs](../../crates/plurxd/src/pipeprobe.rs) | Existing HDR10 probe; add a distinct Mac compatibility report without inheriting speed gating |
| [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs) | Executable resolution/attestation and bounded child lifecycle |
| [main.rs](../../crates/plurxd/src/main.rs) | Startup wiring and retained capability snapshot |
| [manager plan](../../crates/plurxd/src/transcode/manager/plan.rs) | Per-session choice and effective recipe |
| [manager construction](../../crates/plurxd/src/transcode/manager/construct.rs) | Capabilities, node offers and resource estimates |
| [metrics.rs](../../crates/plurxd/src/transcode/metrics.rs) | Closed pipeline label set and accepted-start counting |
| [system.rs](../../crates/plurxd/src/http/system.rs), [developer.rs](../../crates/plurxd/src/http/developer.rs) | Saved preference round trip and advisory readiness |
| [Developer UI](../../crates/plurxd/src/web/pages/settings-developer.js) | Switch, actual state and graduation line |
| [media_pool.rs](../../crates/plurxd/src/media_pool.rs) | Worker-specific compatible offers and mixed-version behavior |
| [live_tv.rs](../../crates/plurxd/src/live_tv.rs), [live_tv_delivery.rs](../../crates/plurxd/src/live_tv_delivery.rs) | Separate live graph, cadence/bitrate and caption contracts |

Read the [web file map](../clients/WEB-SHELL-LAYOUT.md) before UI work. Use
existing scripts where possible; a new web file also needs its asset table,
shell tag and map entry in the same commit.

Existing interface examples, verified in the inspected source:

```rust
// Existing signatures; re-verify at implementation time.
pub fn video_codec_for(self, grade: OutputGrade) -> Option<&'static str>;
pub fn plan_digest(&self) -> String;
pub async fn probe(work_dir: &Path, encoder: Encoder) -> PipelineReport;
```

The first two belong to `Encoder` and `ResolvedTranscode`; `probe` belongs
to the daemon's `pipeprobe` module. Preserve their existing behavior for
non-Mac routes. Do not add independently mutable filter strings to an argv
builder after the plan has been hashed.

Proposed additions, not yet implemented:

```text
Pipeline:
  VtScaleSdr      -> "vt_scale_sdr"       -> SDR
  VtToneMapNative -> "vt_tonemap_native"  -> SDR
  VtToneMapMetal  -> "vt_tonemap_metal"   -> SDR
  DoviVtMetal     -> "dovi_vt_metal"      -> SDR, Dolby-aware (E1 only)
  VtScaleHdr10    -> "vt_scale_hdr10"     -> HDR10 (E4 only)

FrameDomain:
  VideoToolbox   -> "videotoolbox"

Settings API:
  macos_video_processing_enabled: boolean, initially false
  macos_hevc_output_enabled: boolean, initially false (E4 only)
```

Use the repository's stored-switch mechanism. Absent setting means the
documented default; explicit false must remain false through graduation.
Readiness must not reject a PUT or reset either saved value. Both apply to
new plans; existing sessions retain their snapshot. E4's second setting
separates a codec/output change from processing of existing H.264 output.

## 3. M0 — establish the actual Mac and Jellyfin dependency baseline

**Deliverable:** one inventory and baseline receipt for each proposed target
host. No daemon configuration or installed package is changed by this step.

Resolve the daemon's exact FFmpeg and FFprobe paths from its configuration
and launch environment. Do not substitute the interactive shell's `ffmpeg`.
Record the resolved paths, hashes, first version line, full build options,
linked-library identity, OS build, architecture, SoC/RAM and service context.
Redact credentials and unrelated environment values from retained output.

These read-only commands are templates; substitute the observed absolute
paths and an output directory owned by this experiment:

```bash
MAC_VIDEO_FFMPEG=/absolute/path/to/daemon/ffmpeg
MAC_VIDEO_FFPROBE=/absolute/path/to/daemon/ffprobe
"$MAC_VIDEO_FFMPEG" -version
"$MAC_VIDEO_FFPROBE" -version
"$MAC_VIDEO_FFMPEG" -hide_banner -buildconf
"$MAC_VIDEO_FFMPEG" -hide_banner -filters
"$MAC_VIDEO_FFMPEG" -hide_banner -decoders
"$MAC_VIDEO_FFMPEG" -hide_banner -encoders
"$MAC_VIDEO_FFMPEG" -hide_banner -bsfs
"$MAC_VIDEO_FFMPEG" -hide_banner -h filter=scale_vt
"$MAC_VIDEO_FFMPEG" -hide_banner -h filter=tonemap_videotoolbox
"$MAC_VIDEO_FFMPEG" -hide_banner -h filter=tonemapx
"$MAC_VIDEO_FFMPEG" -hide_banner -h decoder=ac4
sw_vers
uname -m
```

An unknown `-h filter=...` result must be parsed as absent even if FFmpeg
exits successfully. Record capability names exactly; do not infer them from
a substring in explanatory help output.

**Required incumbent capability inventory:** AC-4 demux/probe/decode;
`tonemapx` including `apply_dovi` and existing HDR passthrough behavior;
required Dolby bitstream filters and HLS metadata behavior; zscale/CPU
fallback if used by this installation; libass/subtitles and bitmap overlays;
H.264/HEVC decode and H.264 encode; required audio encode/downmix; current
HLS/fMP4/TS and worker-pacing behavior. Derive the full list from actual
production commands and runtime probes, not this minimum list alone.

AC-4 acceptance includes an independently decodable sample with a random
access frame, audio frames produced, sample rate/layout, downmix and A/V sync.
A clipped broadcast capture with no initialization frame is a separate
negative test. See the [ATSC 3.0 investigation](ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md).

**Acceptance:** baseline SDR, HDR10 and P5 existing routes have retained
outputs and exact commands, or their pre-existing failures are recorded.
Existing AC-4 and Dolby support is documented before any package substitution.
Unavailable hardware access is labeled as an environment limit, not a failed
codec. M0 does not claim speedups or require a week of fleet traffic.

## 4. M1 — pin a complete build and run the processing experiments

**Deliverable:** source/build provenance, runnable experiment harness,
normalized graph table, A/B measurements and a decision for each candidate.
M1 also owns the redistribution-safe embedded smoke corpus, its generation
recipe, license/provenance and versioned hash/expectation manifest described
in design §4.5. The private benchmark corpus is not an install dependency.

Start with the currently deployed Jellyfin build. Upgrade only if the needed
filter/options are absent or a reproduced defect requires it. Jellyfin
`v8.1.3-1` is a source candidate documented in design §3.4, not an automatic
upgrade instruction. An upstream FFmpeg major number is not a requirement.

Preserve a full incumbent-feature smoke matrix when building/replacing the
package. The new macOS binary must actually include the Metal library and
its dependencies; patch presence alone is insufficient. Record arm64 versus
x86_64, SDK/deployment target, configure arguments, patch/source commit and
binary checksums. Test the daemon's sanitized child environment and launch
session, including headless operation if that is the intended deployment.
Do not install globally during experiments.

Extend the existing bench conventions or add `scripts/bench-macos-video`
with argv arrays, bounded children, cancellation and structured JSON output.
It is an offline developer harness, not a production playback dependency.
It must distinguish inventory, failed, skipped, unmeasured and successful
runs. Every scoring tool has a separate explicit path; it must never replace
the production FFmpeg setting.

Use the design's corpus, pairing, warm/cold separation, concurrency and
thresholds. The graph table must contain these literal values after testing:

| Graph field | Required evidence |
|---|---|
| Decoder argv | Actual decoder and `hwaccel_output_format`, where used |
| Input/output frame formats | NV12/P010 and hardware representation observed |
| Filter chain | Complete normalized filter text, including explicit conversions and color options |
| Side data | Which metadata reaches the renderer, which is consumed/removed |
| Encoder argv | Same rate-control/keyframe policy as baseline |
| Graph implementation | Exact fork patches, OS-dependent behavior and known limits |
| Output observations | Color/codec/frame/cadence/segment checks plus visual verdict |

Do not put an unexecuted guessed tone-map command in production. In particular,
`tonemap_vt` is not an established filter name and a `scale_vt` color flag is
not by itself proof of correct HDR-to-SDR processing.

**Acceptance:** reproducible results identify which progressive SDR/HDR10
graphs meet correctness and benefit thresholds. If none does, conclude with
measured results and retain the incumbent; do not build M2–M6 for their own
sake. Any unsupported memory-mapping microbenchmark is marked unsupported.

## 5. M2 — encode the processing and cache contracts

**Deliverable:** typed Apple processing choices and tests, without default
routing to them. Runtime settings and activation arrive in M3/M4.

Add only the variants supported by M1. Extend the surface contract to
represent retained VT buffers, software-decoded input when applicable, and
explicit conversions. Preserve operator decoder overrides, required side
data, source binding and continuation restrictions. `on_gpu` is currently a
broad convenience method: audit every caller before teaching it that Apple
surfaces exist; its name does not prove a memory-copy property.

Add an optional versioned Apple implementation-identity extension to the
resolved plan. The absence path must feed precisely the existing hash
fields; adding a global plan-version bump would invalidate unrelated caches
and needs an explicit reason. The extension includes all byte-changing
processing parameters and the design's runtime implementation fingerprint.
The FFmpeg executable/library attestation must remain tied to spawning.

Do not let source path spelling, node ID, probe timestamp or benchmark score
enter the content identity. Same plan through VOD and rolling must resolve
to the same processing identity; their existing container/asset namespaces
remain distinct as required by the current recipe contract.

**Focused tests to add** in existing core test modules and
[decoder_selection.rs](../../crates/plurx-core/tests/decoder_selection.rs):

- `macos_processing_preserves_non_mac_plan_identity`
- `macos_surface_contract_matches_selected_graph`
- `macos_processing_respects_software_override`
- `macos_processing_rejects_missing_required_side_data`
- `macos_processing_identity_changes_with_graph_and_os_build`
- `macos_processing_identity_ignores_probe_time_and_node_name`
- `macos_processing_sdr_fallback_preserves_presentation`

Names above are planned tests, not existing evidence. Test observable plan,
argv and identity contracts, including negative combinations; do not just
mirror the enum's match arms.

**Acceptance:** focused tests execute with a nonzero reported count; the
pinned compiler checks all affected targets and Clippy passes. Existing
non-Mac golden plans remain identical. No source has been pushed to discover
whether it compiles.

## 6. M3 — connect runtime capabilities, preference and diagnostics

**Deliverable:** Mac capability snapshot, persisted preference and accurate
readiness/diagnostics. Keep the new setting false by default.

Implement the bounded asynchronous Mac smoke probes from design §4.4. Use
one snapshot generation for selection and identity; probe completion cannot
mutate a running plan. The manager keeps the existing legacy pipeline for
other routes and a separate Mac capability collection for eligible sessions.
A restart invalidates in-memory availability; a changed implementation
fingerprint invalidates cached smoke evidence.

Embed and load the M1 smoke corpus per design §4.5. Implement bounded
preparation, verified cache replacement and explicit reprobe, without
runtime downloads or fixture encodes. Test empty/corrupt cache, invalid
manifest, offline start, write failure, cancellation and reprobe recovery.
A clean install must reach graph execution without the offline harness ever
having run on that host.

Model observation state separately from selection reason:

```text
observation: pending | available | unavailable
selection: disabled | selected | probe_pending | incompatible_input |
           missing_filter | runtime_probe_failed | presentation_constraint |
           decoder_override | recovery_restriction
```

Map detailed diagnostics onto bounded identifiers. A missing offline
benchmark is advisory readiness, not `runtime_probe_failed`. Do not reject
a working graph because the 1.5 s legacy probe measures less than 1.2×.
Protect the existing fallback if a candidate cannot execute at all.

Settings API tests must prove that false→true→false persists on Mac and
non-Mac hosts, including unknown/unavailable readiness. Verify API readback,
restart persistence and the new-plan-only effect. Add the card and graduation
line under the existing Developer conventions, plus the corresponding
[settings UI contract](../../tests/web/settings-sections.test.js).

Extend existing pipeline metric arrays and parse/display consumers together.
Compatibility reports and cluster offers must be per worker and cannot
promise HLG/Dolby/HEVC merely because SDR scaling passed. Keep detailed graph
classes local and wire pipeline names diagnostic, as specified in design
§5.4; current peers do not reject unfamiliar names. Test old-to-new and
new-to-old dispatch with worker-local resolution, unsupported presentation
refusal, capability-generation changes between offer and start, and takeover
with a different effective recipe. Verify actual plan/cache revalidation at
acceptance. Any newly required remote graph semantics require an explicit
versioned protocol amendment before implementation.

**Acceptance:** mocked timeout/cancellation children are reaped; missing
filters leave startup and ordinary playback responsive; supported Mac smoke
graphs produce verified output; the switch remains on when readiness is
unmet. Existing nodes retain their prior routing and metric counts.

## 7. M4 — route progressive SDR through the real producers

**Deliverable:** enabled Mac SDR processing in rolling HLS and encoded VOD,
including seeking, fallback and cancellation.

Use the M1 graph and resolved surface contract for decoder arguments and
filter construction. Feed VT frames directly to the encoder where supported.
Do not append the generic `format=yuv420p` CPU filter to a hardware chain or
omit normalization simply because its CPU filter cannot accept a VT frame.
Initially select the incumbent for unsupported rotation, SAR, VFR and burns,
with a stable reason. Unknown scan/color facts follow existing policy.

Exercise same-size and downscale paths, odd input geometry, 8/10-bit SDR,
long GOPs, seek and resume. Use real decoder output; a `testsrc` raw-input
encode bypasses the integration boundary this milestone changes.

Inject process failure both before the first publishable segment and after
publication. Reuse the recovery owner and its limits. Verify that fallback
has a new plan/recipe and cannot write into the prior asset. A disabled
decoder-recovery preference must remain effective. Terminating a session
must cancel children and release its existing resources.

**Acceptance:** production tests demonstrate correct rendered geometry,
frame/timestamp contract, first frame, two minutes of playback, seek/resume,
clean cancellation and bounded failure handling. Re-run M1 performance
comparisons through plurxd, not just standalone FFmpeg. Unsupported controls
show unchanged behavior with a recorded reason.

**Discovered native dependency, 2026-10-07:** the isolated daemon run found
that production `DecodeProbeIdentity` accepts only its qualified Linux parser
execution contract. Rolling and non-normalized VOD have existing planning
paths, but normalized continuous VOD correctly refuses
`candidate_geometry_unavailable` without descriptor-bound current source
facts. Admitting its encoder envelope in core does not remove this dependency.
Native bound-source qualification is a separate architectural work item:
preserve held-source identity, parser confinement, executable/library
attestation, deadlines and child ownership in the existing fact collector.
Do not enable fixture launch mode in production, weaken normalized geometry
verification, or substitute scanner-time facts to make this route appear
supported. Track actual working delivery modes separately on the status page.

### 7.1 Native held-source parser — continuation decision, 2026-10-08

**Status:** building; final implementation review and qualification pending.
The existing Linux boundary combines immutable parser execution, a held source
handle and process/descendant restrictions. Launching ordinary FFprobe on macOS
would not preserve that boundary. The native implementation instead compiles
the same pinned Jellyfin FFprobe to WebAssembly and embeds a restricted host
adapter in the existing fact collector. This changes the parser's isolation
mechanism while retaining its media semantics and ownership.

The module receives descriptor 3 reads/seeks and bounded stdout/stderr. It
receives no directory, network, process, fork or executable-loading capability.
The host captures immutable module bytes, validates their structure/imports,
and binds the module/runtime/ABI identity to the existing parser snapshot and
single-flight cache. Output is still parsed by the existing fact collector.
Cancellation and deadline completion must settle in-flight source reads before
releasing the original source lease. Memory, output and execution stay bounded;
no new watchdog or secondary observation cache is introduced.

A measured interpreter prototype exceeded a 20-second exploratory cap on the
existing ten-second 1080p `idet` workload. Compiled Pulley bytecode exceeded
the same cap. The candidate therefore uses Wasmtime native compilation; the
production ten-second execution deadline remains unchanged. Cold compilation,
4K cost and simultaneous-source behavior still need acceptance evidence.

Wasmtime's native compiler needs executable memory. Its pinned macOS backend
does not use `MAP_JIT`; the normal `allow-jit` entitlement alone is insufficient.
The candidate package uses Apple's documented
[unsigned executable-memory entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.cs.allow-unsigned-executable-memory).
This weakens that particular hardened-runtime protection and expands the
trusted runtime dependency; the final security review must explicitly cover
both costs. A live-process signing preflight must reject an incompatible
hardened signature before compilation, because the operating system can kill
the process instead of returning a recoverable compiler error.

Packaging must provide one discoverable parser companion beside the selected
FFprobe, with matching provenance and immutable identity. A new native package
assembler copies and signs task-owned artifacts only, preserves explicit
operator executable overrides, and distinguishes an ad-hoc local candidate
from a Developer ID release. It does not install a service, modify a deployed
binary or claim notarization. Final acceptance covers the copied hardened
package, missing/wrong companion, metadata/AC-4 parity, source mutation,
read cancellation, malformed-module limits and the existing decode deadline.

## 8. M5 — integrate HDR10 processing without widening Dolby routing

**Deliverable:** enabled native/Metal HDR10-to-SDR processing in the same
production owners. Add each candidate only if M1 justified maintaining it.

Use the fixed per-class preference chosen in M1. Output BT.709 signaling,
range, precision and stale-side-data cleanup are explicit parts of the
recipe. Distinguish native automatic tone mapping from a named Metal curve
in diagnostics; do not reuse CPU tone-map labels that imply parameters the
candidate never consumed.

Use compressed HDR10 fixtures with scene cuts, dark and bright frames,
saturated patches, peak metadata variations and absent metadata. Exercise
independent VOD segment restarts: a scene-adaptive filter can look correct
in a continuous encode and pulse at each fresh segment. If state cannot be
reconstructed consistently at arbitrary seeks/segment boundaries, use a
qualified deterministic mode or restrict that backend to the continuous
producer. Do not silently accept temporal discontinuity.

**Tests:** decoded output tags and pixels, known patch ordering/contrast,
no accidental early 8-bit stage, no stale Dolby/HDR signaling, fallback and
cache separation, segmented-versus-continuous visual behavior. P5/P7/P8,
HLG, interlace and burns remain incumbent controls until their extensions
are accepted.

**Acceptance:** the complete design §6 performance/quality protocol passes
for the claimed HDR10 workload and delivery mode. A failed native mapper
may leave Metal as the only candidate, or vice versa; both are not required.
AC-4 and current P5 playback remain working with the selected package.

## 9. Extensions — optimize existing functionality independently

### 9.1 E1: HLG and Dolby Vision

HLG receives its own reference-white/color/temporal checks. Add it to a
supported graph class only after proof; no wildcard HDR matching.

Plurx already supports P5 processing through software decode and `tonemapx`.
First establish whether hardware decode improves the existing CPU Dolby
processing path; then assess equivalent Metal Dolby processing separately.
Use an RPU-positive sample and compare to the incumbent,
including direct-play/remux controls. Verify the pinned fork's actual
metadata propagation before allowing VT decoding; software decode plus
Metal processing is a legitimate candidate. Separate P5-to-SDR from any
P5-to-HDR10 output work. Generic native HDR10 mapping is never its fallback.

**Acceptance:** existing P5 output behavior is preserved, RPU influence is
observed in pixels, metadata and temporal checks pass, and the complete
pipeline meets the benefit criterion. No benefit means keep `tonemapx`.

Run the four-way decode/renderer experiment in design §3.5 during M1, even
though production changes wait for E1. Observe parsed RPU metadata at the
filter boundary, verify frame/PTS association after seek/reordering, prove
actual hardware decoding and exclude double Dolby processing. VT decode +
CPU `tonemapx` is a candidate in its own right if decode dominates. Refactor
the current coupling of required side data to software decode before
admitting a hardware tuple. Preserve the incumbent and non-Mac restrictions;
never globally delete `requires_software_decode` because one Mac passed.

E1 also owns strict execution-time Dolby metadata enforcement, not just a
planner boolean. Implement the design §5.1 renderer-boundary contract in a
verified strict mode or a minimal pinned FFmpeg patch; `apply_dovi=1` alone
silently accepts absent metadata on PQ/HLG and is insufficient. Verify valid
effective metadata/reuse and frame association after seek/reordering. Missing
or invalid required metadata must fail before the affected frame enters the
encoder. Test initial and midstream loss, valid reuse, stale seek state and
non-Dolby controls; observe that no affected output is published and the
existing owner performs any retry. Include strict-mode/patch identity in
the recipe and add an appropriate distributable P5 smoke fixture. If these
contracts cannot be provided, retain incumbent P5 behavior without changing
the saved processing preference.

### 9.2 E2: subtitle compositing

First test hardware scale/tone-map followed by CPU burn at output resolution.
Only then compare GPU overlay of prepared subtitle images. Keep libass
shaping/fonts and PGS geometry semantics, with explicit pixel format, range,
alpha and cue timing. Uploading a small subtitle image does not establish
that the main video is copy-free.

**Acceptance:** colored PGS, ASS animations/positioning, transparent edges,
subtitle-free intervals, last cue/EOF, seek into an active cue and cancellation
pass existing burn semantics. No new HDR burn policy. Retain the faster
correct complete graph even if it contains a CPU compositor.

### 9.3 E3: deinterlacing and Live TV

Prefer testing Jellyfin's hardware BWDIF when the pinned build supplies it;
YADIF is a separate alternative, not a silent quality downgrade. Preserve
file send-frame and Live TV configured frame/field cadence, parity,
progressive bypass, rational rates and matching bitrate/manifest facts.

Integrate with `LiveTvTranscodePlan` rather than borrowing a VOD argv string.
Retain Live TV's existing `-a53cc 0`. Include A/53-positive MPEG-2, 720p59.94,
1080i TFF/BFF, audio-late startup, AC-4 downmix, tuner reconnect and stop/start.
Resolve or explicitly bound the separate caption-bearing VOD limitation.

**Acceptance:** no combing/order errors, correct cadence, first frame before
input EOF on a paced source, sustained playback and the existing
`live_tv_videotoolbox_atsc1_publishes_decodable_segments` hardware regression.
Do not claim file-caption repair from that live-only test.

### 9.4 E4: HEVC/Main10 output

Add the explicit `macos_hevc_output_enabled` preference and Developer card.
Qualify HEVC SDR and HDR10 Main10 separately. Extend codec selection,
encoder options, pipeline pairing, presentation facts, manifests, container
handling, cluster offers and cache identity together. Keep H.264 compatibility
fallback negotiated through existing delivery policy.

**Client contract chosen during implementation:** add optional
`DeviceCaps.hls_hevc_sample_entries: Option<Vec<String>>`. A present list has
at most two distinct lowercase entries, `hvc1` and/or `hev1`; absent or empty
preserves the legacy H.264 selection for new SDR HEVC offers. Each entry is a
claim about the actual HLS fragmented-MP4 path. Original-progressive MP4 or
generic HEVC support does not supply it. The initial encoder emits `hvc1`, so
that entry is required in addition to existing profile, transfer, container,
transport, geometry, frame-rate and bitrate compatibility. This is an additive
client capability, not a required remote processing-graph field. The saved
HEVC preference remains independent and is always accepted.

Start HDR preservation with ordinary HDR10; do not admit Dolby passthrough
by widening an HDR10 input check. Preserve color and static HDR metadata
semantics, remove claims the encoder cannot maintain, and verify decoded
output and actual HDR presentation on a named display. Preserve the existing
VOD B-frame policy. Compare bitrate/quality at equivalent output settings;
HEVC is not guaranteed to be faster than H.264.

**Acceptance:** actual target Apple/native/web clients play the intended
HLS container, seek, switch quality and recover. Record unsupported clients
and their H.264 route. Failure cannot silently change an already promised
HDR grade. Graduate this control independently of the processing control.

### 9.5 Cross-platform follow-up

The hardware-decode investigation also has a cross-platform follow-up,
recorded in design §3.6. Reuse its experimental method for Intel native
VA-API/DXVA and NVIDIA NVDEC rather than assuming all interfaces on a GPU
preserve the same Dolby metadata. That work has a separate scope; the Mac
milestones must neither globally remove the software restriction nor encode
it as a permanent universal invariant.

## 10. M6 — qualify, package and graduate the supported scope

Freeze the initial supported matrix, merge the current main base into the
effort, and run exact-tree validation under the repository workflow. Record
one result per host, workload, graph, delivery mode and output contract.
Extensions not completed remain explicit incumbent routes and do not receive
capability claims.

The selected package must pass the entire M0 incumbent-feature matrix,
including AC-4 and existing P5, through the daemon's normal environment.
Verify that the release embeds every initial smoke fixture and manifest:
fresh offline install, empty cache and corrupt-cache recovery must reach
runtime probing without developer artifacts, network fetches or generation.
Exercise cache write failure and recovery separately from graph failure.
Exercise upgrade and rollback with an active worker: retain its binary and
libraries until exit, or drain before replacement. Confirm that turning the
switch off changes subsequent plans without breaking existing sessions.

Run the design's 30 min concurrency soak, 30-start latency sample and client
checks. Final evidence includes raw run receipts, visual review, decoder and
processing decisions, rebuffer/error counts, memory behavior and unavailable
measurements. No “all green” conclusion with missing real-Mac rows.

Move the completed processing switch to Playback → Advanced server delivery
and update the Developer audit in the same task. Keep its explicit user value.
Changing the default for unset installations is a separate documented decision
based on the evidence; completing a benchmark does not itself flip defaults.

**Acceptance:** the applicable blocking gate passes on the current candidate,
its qualification receipt exists, focused regressions are recorded, and
native packaging/rollback works. A compiler-only cloud result cannot stand
in for Mac hardware or client qualification.

## 11. Validation commands and evidence format

For this documentation-only proposal:

```bash
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
git diff --check
```

For implementation, use the verified pinned compiler. These are starting
commands, not permission to skip affected checks selected by the workflow:

```bash
rustc --version
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --locked -p plurx-core --test decoder_selection macos_processing
cargo test --locked -p plurx-core --features hiqlite-store --lib macos_processing
cargo test --locked -p plurxd macos_processing
node --test tests/web/settings-sections.test.js
```

The `macos_processing` filters refer to tests to be added; require a nonzero
executed count and list their fully qualified names in the PR. Broaden to
existing pipeline, recipe, VOD, recovery and settings regressions appropriate
to the changes. Use `make unit-core` or `--features hiqlite-store` whenever
storage behavior is covered. Run real-FFmpeg/hardware ignored tests explicitly
and serially; a skipped test is not hardware evidence.

Each behavior PR uses `fix(` or `perf(` and carries actual, existing
`Regression-Test: <path>::<test name>` lines. Preserve those lines in the
landing commit, including `MergeMessageField` for an API merge. Do not copy
planned names into a PR as if they already executed.

Proposed experiment receipt fields (schema version 1):

```json
{
  "schema_version": 1,
  "source_commit": "full SHA",
  "host": {"model": "observed", "os_build": "observed", "arch": "arm64"},
  "implementation": {
    "ffmpeg_sha256": "observed", "ffprobe_sha256": "observed",
    "source_tag": "observed", "patch_digest": "observed",
    "linked_libraries_digest": "observed"
  },
  "input": {"sha256": "observed", "class": "hdr10", "seconds": 60},
  "treatment": {"graph_id": "vt_tonemap_metal", "argv": []},
  "measurement": {
    "repetition": 1, "cache_condition": "warm",
    "throughput_x": null, "cpu_seconds_per_media_second": null,
    "joules_per_media_minute": null, "copy_bytes_measured": null
  },
  "result": "unmeasured",
  "limitations": ["Template only; no benchmark has run"]
}
```

Actual receipts also include raw-output references, frame/tag/timestamp
results, visual verdict, startup/event origins, sample counts and failure
causes. A `null` metric is unavailable/unmeasured with a reason, never zero.
Private media titles, tokens and private paths do not belong in public logs;
use hashes and sanitized fixture identifiers. Store large traces as artifacts,
link the small retained receipt, and index any new prose in `docs/README.md`
in the same commit.

## 12. Opus review handoff

Review the design and this execution plan together. Use source inspection to
challenge the seams; do not implement, deploy, benchmark a live service or
change settings as part of the review. Return prioritized findings with the
affected document section, concrete failure scenario and proposed correction.
Separate blocking contradictions from measurements the plan deliberately
leaves to M0/M1. Mark explicit agreement where a questioned premise survives.

Review especially:

1. Shared memory: does any argument still mistake a mapped buffer for a copy,
   assume PCIe costs, or demand copy-free processing without a benefit?
2. Dependency integrity: does the chosen Jellyfin package retain AC-4,
   `tonemapx`, Dolby bitstream behavior, subtitles, muxing and pacing?
3. Existing P5 support: does any step accidentally replace Dolby-aware
   rendering with generic HDR10 or present P5 as a newly added feature?
4. Plan fidelity: are both resolved argv and VOD/rolling builders changed,
   including normalization, explicit overrides and side-data requirements?
5. Probe policy: are runtime compatibility, offline qualification and
   advisory Developer readiness distinct? Can a user always save the switch?
6. Cache/recovery: can mixed graphs share a key, an OS upgrade reuse the wrong
   output, a retry append to an old asset, or a grade change bypass negotiation?
7. Experimental validity: are the baseline, different FFmpeg builds, tone
   curves, startup, cold shader compilation and concurrency fairly compared?
8. Segment behavior: can adaptive tone mapping reset at each VOD segment and
   create a visible boundary even though a continuous benchmark looks good?
9. Extension limits: are field cadence, caption policy, subtitle color and
   Main10 client support independently verified rather than inherited?
10. Execution workflow: does each milestone leave reproducible evidence on
    the actual integration tree without using CI as its first compiler?

**Review disposition:** one independent agent review returned request changes
on 2026-10-07. All three execution-contract findings have author corrections
in these documents; see the [review record](MACOS-VIDEO-PROCESSING-REVIEW.md).
No second independent approval is claimed. The user may still hand the
revised documents and this record to Opus.
