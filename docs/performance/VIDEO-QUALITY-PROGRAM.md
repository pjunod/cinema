# Video quality programme — measured improvements and their build order

**Status:** approved scope; documentation merge and first three lanes pending
· **Written:** 2026-10-02 · **Source census:** `4fa50b79e`

Companion to [the execution ledger](VIDEO-QUALITY-STATUS.md), which records
what actually ran, and [Performance II](PERF2-PLAN.md), which supplies the
existing rate-control and content-analysis designs. This document answers
what to build next, who owns each change, and what evidence permits a
production change. Read the [development pipeline](../DEVELOPMENT_PIPELINE.md)
and [benchmark identity rules](../BENCHMARKING.md) before executing it.

Paul requested the plan be merged before implementation, then two or three
parallel tasks. Calibration must proceed without asking him to operate a
player, inspect a television, supply ratings, or run commands. An inaccessible
measurement stays explicitly unverified; it does not hold up independent work.

## 1. Sequence — three measurement lanes, then the requested priorities

The repeated encoder item is intentional: the first wave measures candidate
policies; the later item applies and qualifies any policy those results justify.
A measured decision to retain the current policy is a valid calibration result.

| Order | Work | Acceptance before claiming completion |
|---|---|---|
| A, parallel | Encoder calibration | Representative per-family baseline/candidate captures, exact tool and source identities, existing benefit/non-regression verdicts, and an explicit apply-or-retain decision. |
| B, parallel | Content-aware encoding | Bounded offline sample search first; later durable analysis and opt-in integration into cached/offline encodes, with stale-analysis and resource-contention regressions. A CLI recommendation alone does not finish the feature. |
| C, parallel | Visual HDR-to-SDR calibration | Automated reference comparisons, retained stills and measured errors with source/route provenance; agent visual inspection; only the hardware/renderer paths actually exercised qualify. |
| 1 | Next-episode preparation | Resolve and prepare a bounded successor while the incumbent plays; lower measured episode-to-episode gap without wasted unbounded work or incumbent stalls. |
| 2 | Apply encoder calibration | Promote only beneficial, qualified per-family policies; otherwise retain bitrate mode with the no-change evidence. No duplicate calibration campaign. |
| 5 | HLS delivery batching | Lower delivery bookkeeping cost under concurrency while preserving exact cancellation, authorization, timeout and accounting behavior. |
| 6 | Encoded VOD B-frames | Presentation-grid validator and reordered-fragment proof before any recipe change, followed by seek/restart and client qualification. |
| 3 | Broader codec and HDR output | Qualify one fleet-relevant HEVC SDR or additional HDR hardware graph end to end, including subtitle composition and cache identity. |
| 4 | Remaining cold-start latency | Reconcile already-landed startup work, identify the remaining measured critical path, and improve matched first-frame latency without rebuffering regression. |

Start A/B/C together. Later work follows the numbered priority order; a second
or third slot may investigate a later item while the preceding item waits for
measurements, but must not overtake its integration or touch its owned files.

## 2. Existing implementation — extend it instead of rebuilding it

The source census is newer than the original recommendation checkout. Re-read
these symbols on the intended branch before implementing a milestone.

| Area | Current contract and existing owner |
|---|---|
| Rate control | `Encoder::default_rate_mode`, `default_quality` and `encode_args_for` in [encoder.rs](../../crates/plurx-core/src/transcode/encoder.rs); [encoder defaults](../streaming/ENCODER-RATE-CONTROL-DEFAULTS.md) owns the per-family acceptance contract. [scripts/bench](../../scripts/bench) already has deterministic SDR fixtures, VMAF scoring, provenance and benefit gates. |
| Content analysis | [Performance II §5](PERF2-PLAN.md#5-n2--per-title-intelligence) is the broader N2 design. This programme narrows its first delivery to offline evidence; it does not claim the durable analysis or rate bias is built. `PretranscodePolicySnapshot` and `OfflineSpec` already snapshot encoding policy and must remain explicit boundaries. |
| HDR reference | [Tone-map corrections](../streaming/TONE-MAP-CHAIN-CORRECTIONS.md) still distinguishes source changes from image and fleet evidence. [Codec/GPU qualification](../streaming/CODEC-AND-GPU-QUALIFICATION.md) owns expanded graph acceptance. CPU filter construction is in [core transcode](../../crates/plurx-core/src/transcode/mod.rs); the reference boot probe is [pipeprobe.rs](../../crates/plurxd/src/pipeprobe.rs). |
| Existing HDR work | Main already contains PR #750's resident VAAPI/Vulkan path. Open [PR #753](http://192.168.4.7:3000/noirr/plurx/pulls/753) owns codec-corpus reference associations at census time. Reuse that work after it lands; do not publish a competing HDR scorer or claim another PR's evidence as this programme's. |
| Next episode | [autoplay-next.js](../../crates/plurxd/src/web/player/autoplay-next.js) resolves after the end; Performance II N3 already proposes pre-resolution. Inspect native continuation paths and current preparation leases before adding a new owner. |
| HLS batching | [Media body buffers](../streaming/MEDIA-BODY-BUFFERS.md) owns the distinction between storage read size and downstream acknowledgement. M1 already shipped; this programme advances only the remaining measured work. |
| B-frames | [VOD timeline design](../streaming/VOD-BFRAMES-TIMELINE-DESIGN.md) selects signed version-1 composition offsets as the admissible future design; production remains no-reorder. |
| Startup | [Startup status](../streaming/PLAYBACK-STARTUP-LATENCY-STATUS.html) and [startup implementation](../streaming/PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md) retain the original incident. Open [PR #745](http://192.168.4.7:3000/noirr/plurx/pulls/745) owns preparation and idle-start work at census time. Refresh before starting item 4. |

These links are contracts, not proof that their historical source line numbers
or deployment claims match today's fleet. Capture the actual binary and route.

## 3. Parallel ownership — one integration branch

Merge this documentation-only PR into `main` first. Create
`effort/video-quality` from that merged main. Task branches use
`codex/video-quality-<task>` and target the current effort. The coordinator owns
integration, rebases, the index and shared documentation; workers use isolated
checkouts and never modify the user's working tree.

| Lane | Initial exclusive files | Shared integration boundary |
|---|---|---|
| A: encoder | `crates/plurx-core/examples/encoder-calibration-args.rs`; `scripts/encoder-calibration-screen`; `tests/operations/test_encoder_calibration_screen.py`; calibration receipts. Export production encoder arguments and reuse existing bench scoring. | `scripts/bench`, encoder defaults and production recipe changes are coordinator-serialized. |
| B: content-aware | `scripts/content-aware-encoding`; `tests/operations/test_content_aware_encoding.py` | Durable jobs, storage, offline/pretranscode snapshots, settings and recipe identity are a later task with explicit ownership. |
| C: HDR images | `scripts/tone-map-calibration`; `tests/operations/test_tone_map_calibration.py` | Existing codec scorer, `scripts/bench`, CPU/GPU filter construction and boot thresholds remain with their current owner until a separate reviewed task takes them. |
| Coordinator | This plan, the ledger, `docs/README.md`, validation catalogue mappings, evidence indexing, task PRs and merge receipts | No concurrent edits to these files. |

A worker reserves a benchmark host/window through the coordinator. Run at most
one calibration workload on a host/GPU at a time, with bounded duration,
threads and temporary disk. Three coding lanes do not authorize three competing
GPU jobs. Check for active playback before fleet work; use isolated processes,
fixtures and scratch directories. Never reset a production queue, stop a viewer,
or change replicated playback settings to obtain a benchmark.

The source compiler loop is established before Rust edits, using the repository
pin (1.97.1 at census time), not the shell's Homebrew default. The initial
measurement tools may be Python, but every commit still uses the tracked hook.

## 4. Encoder calibration — compare production recipes on actual hardware

**A1: census and corpus.** Inventory actual selected encoders, shipped FFmpeg
builds and active workloads without mutating settings. Reuse the existing six
SDR corpus classes and their hashes. Add representative licensed/local samples
when available; do not transfer private film content to an external service.
Record hardware, driver, source hash, exact argv, thread limit and warm/cold
state. A local different-FFmpeg run is screening evidence, not fleet acceptance.

**A2: isolated captures.** Reuse the existing bench scoring and benefit policy.
If its production API path would mutate replicated settings, use an isolated
daemon or a bounded offline recipe capture instead. Exercise the same resolved
production arguments; record any mismatch and retain the screening label until
actual route acceptance. Do not invent a second set of VMAF benefit thresholds.

**A3: decision.** Keep offline screening, isolated-daemon acceptance and
actual fleet playback/default qualification as separate verdicts. Generated
fixtures do not establish representative movie or sports quality.
Compare bytes, every corpus item's VMAF, encode speed,
bitrate bursts and memory. A missing family is unmeasured, not supported.
Hardware advertising an encoder is insufficient. A candidate that fails the
existing benefit gate produces a retain-bitrate result; there is no promised
percentage saving. HDR10 keeps its independently qualified rate-control policy.

**Acceptance:** existing rate-control unit tests pass; baseline/candidate source
and output identities match the declared comparison; the report explains every
failed or missing gate. Item 2 later updates defaults only for qualified wins,
retains operator overrides, and verifies recipe/cache identity and playback.

## 5. Content-aware encoding — learn from bounded samples before integration

### 5.1 C1 produces an offline recommendation with explicit scope

The initial tool accepts a local SDR source and explicit encoder/quality policy.
Use at most three non-overlapping windows of at most ten seconds each and at
most three quality candidates plus the bitrate baseline at one output height.
The output never exceeds source dimensions. Materialize each reference sample
once so candidates share the same presentation frames, geometry and color path.

Reuse the production software H.264 preset/profile, current no-reorder VOD
constraint and bitrate ceilings. Any departures required for isolated sampling
are recorded in argv/provenance and prevent production qualification. Refuse
HDR/Dolby Vision and ambiguous color metadata; those belong to the separate
color contract, not an SDR VMAF score.

For each window, p10 is the nearest-rank statistic from finite per-frame
VMAF values: sort ascending and select index `ceil(0.10 * N) - 1`.
A candidate must meet baseline mean and p10 quality on **every** window and any explicitly
configured quality floor (required input, applied to every window mean, no
universal default), meet the encode-time budget, and save at least 10%
aggregate bytes before the initial screening policy recommends it. This is a
proposal threshold for C1, not a claim of expected savings or a replacement for
A's existing fleet acceptance policy. An average may not hide one damaged scene. Candidate total encode elapsed
time divided by baseline total encode elapsed time must be at most an explicit
`--max-encode-time-ratio` (default 1.10); exclude scoring/materialization time
from both sides and retain per-window timings. This single-run screen is not
the repeated fleet p10-speed qualification from lane A.

The JSON report declares `scope: offline_sample_recommendation` and binds source
identity, sample intervals, tool/build identity, exact argv, metric model,
policy version and results. Source hashing and subprocesses obey a total time
budget; changed source, missing/non-finite metrics and exhausted budgets return
an explicit inconclusive/fallback result. Temporary media is cleaned on failure.
No production database, default, cached media recipe or viewer session changes.

**Acceptance:** regressions prove that a hard window vetoes an easy average,
invalid metrics fail closed, intervals/work remain bounded, changed source or
recipe invalidates evidence, and cancellation cleans child processes/scratch.
Run a real local libvmaf smoke comparison on easy and moving fixtures. Report
synthetic evidence separately from representative real-title evidence.

### 5.2 C2 makes analysis useful to cached and offline encoding

Only after C1 establishes useful recommendations on representative titles,
implement durable background analysis through existing job ownership and resource
admission. Bind the result to held source identity, encoder/tool/metric policy
and output recipe. A missing, stale or failed analysis uses the ordinary recipe;
Play must never wait for analysis. Preserve requested audio, subtitles, grade
and geometry, and snapshot the resolved policy in offline/pretranscode work.

Add an explicit Settings → Developer switch with advisory readiness and a
concrete graduation condition. Saving either choice always works. Results may
choose only recipes the node can actually execute; safety validation is distinct
from overriding the saved feature choice. Start with cached/offline outputs;
live Auto integration is a separately measured follow-up, not silently included.

Do not apply N2's proposed generic complexity-class bitrate bias merely from
`siti`/`scdet`: use measured per-title choices with baseline fallback. A sampled
recommendation is not a whole-title quality guarantee. Preserve explicit
operator policy and the separate HDR10 VBR policy.

**Acceptance:** stale-source, retry, restart, cancellation, resource contention,
cache separation and settings lifecycle tests; matched end-to-end encodes show
benefit without sample-analysis cost on the interactive start path.

## 6. HDR-to-SDR calibration — autonomous reference and image evidence

**H1: fixtures and provenance.** Generate authored PQ/HLG controls with known
luminance, gradients and saturated colors. Merely relabeling SDR as PQ is not a
valid HDR fixture. Include absent/1,000/4,000-nit peak metadata cases and real
local samples if available. Identify source, metadata, CPU baseline, candidate,
FFmpeg build and all filter arguments.

**H2: matched comparisons.** Use a zscale-capable production build in an
isolated process/container. Verify decoded output is BT.709 with matching
geometry, frame correspondence and range before comparing. Retain lossless
stills/contact sheets and per-channel/shadow/highlight/gradient measurements.
A before/after difference establishes a change, not correctness: distinguish a
known analytical reference from a historical implementation used as baseline.
PSNR/SSIM/VMAF alone do not establish faithful HDR highlight or gamut mapping.
Reuse the existing independent HDR reference scorer once its dependency lands.

**H3: inspect and decide.** The agent inspects contact sheets for clipping,
color casts, crushed blacks and banding, alongside numerical controls. Record
which source/renderer combinations passed, failed or remain unverified. GPU
changes require their own matched captures; CPU evidence never qualifies a GPU.
Do not weaken the boot reference threshold to make a candidate pass.

**Acceptance:** mismatched tags, frames, geometry and invalid references are
rejected; a bounded real FFmpeg run generates the report and inspectable stills.
State explicitly that captured-image evidence does not measure a physical
panel, viewing environment or user preference. Those limits require no action
from Paul and are not silently converted to pass results.

## 7. Later milestones — retain the established contracts

| Item | Implementation boundary | Required evidence |
|---|---|---|
| 1: next episode | Resolve while nearing credits; bounded preparation lease; cancel on seek/track/preference/navigation change; commit through existing playback owner. Avoid marking watched or consuming a tuner prematurely. | End-of-season, no successor, autoplay off, rapid navigation, slow preparation and cancellation; matched transition-gap and wasted-byte measurements on each changed client. |
| 2: defaults | Apply A's measured per-family decisions; keep explicit operator policy and HDR10 policy separate. | Exact recipe/manifest identity, real output and playback; record retain decisions where benefit is absent. |
| 5: HLS | Batch downstream acknowledgement without conflating read, yielded bytes and completed objects. | Revocation, disconnect, timeout, partial response and cancellation proofs; concurrency CPU/RSS and segment p95/p99 comparison. |
| 6: B-frames | Build signed-CTO presentation validation first, then one qualified encoder recipe. Preserve immutable init and random access. | Oracle mutations, seek/restart, A/V sync and three-client delivery; measured bytes/quality before accepting extra complexity. |
| 3: codecs | Separate output codec from grade; select one actual fleet encoder/client tuple; include burn-in and metadata propagation. | Runtime probe, color/output metadata, realtime margin, cache separation, real seek/play and compatibility fallback. |
| 4: startup | Rebase on completed startup work; measure remaining phase costs and change only a demonstrated bottleneck. | Matched cold/warm/resume distributions, network-shaped continuity and bounded resource usage; no arbitrary smaller runway. |

## 8. Integration, evidence and limits

Use normal commits and the tracked hook. Run the smallest meaningful regression
before push; record its command and `Regression-Test:` lines in behavioral task
PRs and landing commit messages. Use `fix(` or `perf(` for user-visible behavior.
Task PRs target the effort; dispatch and pass the required effort development
gate. Freeze task merges for promotion, merge current main, and qualify the
exact resulting candidate under the repository's current promotion rules.
Main-bound PRs receive exactly one adversarial review, its fixes, then the
ready fast lane; no merge happens on a stale or missing required result.

The initial documentation PR is ordinary docs-only work into main. It does not
change settings, recipes, production service state or the rules for later code.
Merge authority is granted by Paul's request; deployment is a separate action.

**Non-goals:** AI enhancement, sharpening by default, motion interpolation,
full pre-encoding of the library, a new playback owner, a second independent
benchmark policy, automatic live default flips, or physical display calibration.
These expand compute or alter the picture without the measured need this effort
is intended to establish.

The [ledger](VIDEO-QUALITY-STATUS.md) is the single progress record. It separates
planned, built, locally tested, measured, merged and production-qualified work.
A merged tool is not a calibrated fleet; an enabled encoder is not proof of a
better picture.
