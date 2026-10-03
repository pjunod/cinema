# Video quality status — what is built, measured and merged

**Status:** implementation and runtime receipts retained; selective fast-lane qualification · **Updated:** 2026-10-03

Companion to [the programme](VIDEO-QUALITY-PROGRAM.md), which owns scope,
acceptance and order. This ledger records actual execution. Empty evidence is
unverified, not a pass. Dates use America/New_York unless a receipt states UTC.

## Current execution — consolidated batch

Paul authorized replacing per-task testing/effort gates with one larger
merge-ready PR, one final adversarial review, review fixes, and then fast-lane
checks. Only failed checks are retried; pinned Rust compilation and normal
commits remain. This supersedes the historical lane process below for this
effort only. Whole-suite unit repair remains with the separate process.

| Work | Current state | Cause and architectural direction |
|---|---|---|
| Independent workspace | Ready | `/private/tmp/plurx-video-quality-build-20261003`, cloned directly from Forgejo; no borrowed Git objects or access to Paul's checkout. |
| Consolidated branch | Implementation integrated | [PR #766](http://192.168.4.7:3000/noirr/plurx/pulls/766), `codex/video-quality-batch`; main `9416dbe89` integrated as `9c93f303f`; commit `6ef61fb1f` retains all three tool branches. PRs #760/#761/#762 are closed as superseded. |
| Old per-task CI | Stopped | Encoder run 3936 passed; still-running C1/HDR runs 3938/3942 were cancelled after the workflow override. No additional per-task test campaigns. |
| Content-aware runtime C2 | Integrated `8fd3bba7d` + `e56c80274` | Bounded measurements in existing durable producers, persisted source/recipe identities, offline snapshots and a packaged static scorer. Retained-HLS can reuse a completed measured artifact; misses preserve live policy. Modern immutable VOD is separate. |
| Next episode | Implemented `f632af81d` | Warm one successor metadata page during the final 30 seconds, cancel with the existing playback lifecycle, and retain a fresh authoritative decision at Play. Five regressions authored; hook passed. |
| HLS acknowledgement batching | Implemented, validation deferred | Current pump allocates and schedules a separate channel acknowledgement for every 4 KiB despite 128 KiB storage reads; preserve the 4 KiB proof while reducing coordination, rather than weakening accounting. |
| Apply encoder calibration (2) | Retain baseline | QSV Q22 failed all six quality comparisons; no default change is justified. |
| VOD B-frames (6) | Integrated `6a9599295` and Developer controls | Strict decode/presentation-grid publication, signed offsets and optional software x264 recipe; compile and hook passed, runtime/client evidence pending. |
| Broader HDR (3) | Implemented `5a8356329` | Plain HDR10 VAAPI Main10 at 1080p with P010 upload and its own graph proof. Compile/hook and isolated m6 continuous-PQ output qualification passed; hard-edge image and client limits remain recorded below. |
| Cold start (4) | Integrated `133be1706` | One sealed full source collection serves verification and decoder planning. Existing 5-second verification and 10-second refinement budgets remain separate; tests count one stream probe instead of two. PR #745 is integrated. |

The agent's first pinned Rust 1.97.1 compile check in the independent clone
passed. Package-only unused-item warnings are pre-existing; the normal
workspace hook remains the lint check. The combined tree passed pinned all-target
compilation and the normal hook at `d717b82b6`; the cache-consumer follow-up is
compiled in the integrated normal hook at `6cdddaeef`. The final adversarial
review found two blocking issues: application-owned probe metadata entered
source comparison, and fixed-level VAAPI HDR admitted higher source cadences.
The first is corrected in `827023fd2`; the second now uses the same known
at-most-30-fps contract in grade selection and the facts-aware planner.
Authored regressions remain deferred to the final lane. Runtime qualification
and the once-only fast lane follow these review fixes. Existing reports below remain dated evidence,
not a claim that newly implemented code has already passed its future checks.

Decisions taken without requiring Paul: preparation caches metadata but retains a fresh
authoritative decision at actual play because the server exposes no policy
revision token. It must not consume a viewer session or mark the next item
watched; use the existing autoplay preference rather than an extra feature
switch. Preserve delivery-proof granularity in the batching design.

## Final qualification — retained per-check receipts

The one final adversarial review completed on `6cdddaeef`. Its two findings
were addressed in `827023fd2` (application-owned probe metadata) and
`09ab30374` (VAAPI source cadence), with normal compile hooks passing.
[Review receipt](../evidence/video-quality-2026-10-03/adversarial-review.json).

| Check | Result and precise scope |
|---|---|
| Native content samples | Passed one test: three windows × four recipes, metrics, source identity and scratch cleanup. The fixture now assigns actual BT.709 frame metadata and reads its emitted probe. Production checks remain strict. [Receipt](../evidence/video-quality-2026-10-03/native-content-proof-fixed.json). |
| Shipping scorer | Actual Docker runtime-assets stage passed on idle nuc3 in 398 s with 2 CPUs / 3 GiB, including static ELF closure and built-in VMAF model smoke. The first build exposed the missing C++ runtime link; `555c7defb` fixes it. [Receipt](../evidence/video-quality-2026-10-03/scorer/qualification.json). |
| VAAPI HDR graph | Continuous neutral PQ ramp passed on idle m6: 96 frames at 1920×1080/24, exact timestamps, Main10/PQ/BT.2020/limited range and complete decode. Actual `hvc1.2.4.H120.B0` equals the declaration. Mean luma error 0.531 and maximum 2 ten-bit codes; 1.960 s encode for 4 s content (2.04× in this synthetic capture). [Receipt](../evidence/video-quality-2026-10-03/vaapi-qualification.json). |
| Developer settings | 36/36 passed, once after review; this changed suite is outside fast lane. Both saved choices remain independent of readiness. [Receipt](../evidence/video-quality-2026-10-03/settings-sections.json). |
| Fast lane | [Run 3969](http://192.168.4.7:3000/noirr/plurx/actions/runs/3969) passed scope, history and regression fields; 249/253 validation methods passed. Four stale inventory methods are repaired and passed individually, with runtime source unchanged. Continue the previously unrun steps; [PR #766](http://192.168.4.7:3000/noirr/plurx/pulls/766) is the live per-check and merge record. |

**What the failed HDR captures taught us:** the first output used constraints
`B0`, not the shared `90`, and MP4 defaulted to `hev1`. Commits `117570654`
and `addfac9f1` align the encoder-specific declaration and explicit sample
entry; the actual mux argv already participates in immutable VOD identity.
The next capture exposed missing tags on authored FFV1 frames; the fixture now
sets and verifies them before encoding. A discontinuous three-panel fixture
then met metadata/timing checks with mean error 0.368 but exceeded the
worst-pixel criterion (35 vs less than 32 codes). That remains a
[failed image result](../evidence/video-quality-2026-10-03/vaapi-third-qualification.json),
not a pass. A separate continuous PQ ramp met the unchanged criteria; it
qualifies signal preservation only and does not establish sharp-edge quality,
gamut/skin fidelity, Dolby processing, 4K, subtitle burn, physical displays or
the full client matrix. Original failures and exact commands are retained.

The scorer builder/image/containers/volumes and m6 container/media/control
scratch are removed. No production settings, queues or deployments changed.
The batch keeps B-frames and measured per-title encoding in Developer pending
their stated broader evidence; readiness cannot veto either saved choice.

## 1. Integration and compiler

| Fact | Value |
|---|---|
| Planning base | Forgejo main `4fa50b79e4b3b196797a9b7a1a7a4abd2184ea43` |
| Planning branch | `codex/video-quality-program` |
| Documentation PR | [#758](http://192.168.4.7:3000/noirr/plurx/pulls/758) merged as `cfb52bf402e603523f6fd5b3d71b6ffa2c751565`; one adversarial review, two P2 ambiguities addressed; exact-head Main promotion gate passed in [run 3923](http://192.168.4.7:3000/noirr/plurx/actions/runs/3923) |
| Effort branch | `effort/video-quality`, published from `cfb52bf40` after the plan merged |
| Compiler | `/Users/pjunod/.cargo/bin/rustc`: `1.97.1 (8bab26f4f 2026-07-14)` |
| Compiler-loop baseline | `cargo check -p plurx-core --lib --locked --offline` passed on planning base; package-only baseline reported pre-existing unused-item warnings. Pinned workspace Clippy and tracked hook passed. |
| Original checkout | Left untouched, including pre-existing uncommitted documents |
| Production changes | None |

## 2. Historical first wave — superseded task workflow

| Lane | Owner | State | Evidence / next action |
|---|---|---|---|
| A: encoder calibration | `/root/encoder_calibration` | E1 implemented; [PR #762](http://192.168.4.7:3000/noirr/plurx/pulls/762) open into effort | Commit `1942cf0ca`; production-argument exporter and six-fixture QSV screen. 13 focused tests, 56 existing bench tests and tracked hook passed. QSV quality 22 fails the benefit gate; bitrate mode retained. |
| B: content-aware encoding | `/root/content_aware` | C1 implemented; [PR #760](http://192.168.4.7:3000/noirr/plurx/pulls/760) open into effort | Initial commit `162418ab1` passed 13 local tests; Linux preflight exposed a same-timestamp source rewrite. Fix commit `d7e96b12c` requires final source rehash; 14 local tests, refreshed real smokes and the tracked hook passed. C2 durable/runtime integration remains unstarted. |
| C: HDR-to-SDR images | `/root/hdr_calibration` | Implemented; [PR #761](http://192.168.4.7:3000/noirr/plurx/pulls/761) open into effort | Commit `f68e6cd0a`; four authored neutral PQ/HLG cases × three variants × eight frames inspected. Three focused tests and tracked hook passed. Complements existing S11 scorer. |

These rows record the original lane state before consolidation. PRs #760,
#761 and #762 are now closed with their implementation retained in #766;
the current execution table above supersedes their gate and queue states.

Coordinator owns docs, index, validation catalogue and shared integrations.
Encoder lane reserved nynuc, but the immediate pre-run check found one active
viewer and skipped all calibration there. The reservation moved to idle nuc4
for one isolated QSV workload, with cancellation when playback appears. HDR lane reserves nuc3 for one CPU-only process/container
(1 CPU, 1 GiB) after the same idle check. Local Docker queries timed out, so
no daemon restart was attempted. Content-aware smoke runs locally with bounded
scratch. Shared Cargo checks serialize with incremental compilation disabled;
the coordinator removed only this task's disposable incremental cache after an
ENOSPC hook failure, then reran the hook successfully.

Required effort gates were dispatched for content-aware
[run 3931](http://192.168.4.7:3000/noirr/plurx/actions/runs/3931), HDR
[run 3933](http://192.168.4.7:3000/noirr/plurx/actions/runs/3933), and the
encoder's corrected metadata commit
[run 3936](http://192.168.4.7:3000/noirr/plurx/actions/runs/3936).
The first C1 run caught the source-rewrite regression described above. Its
fix reproduces identical stat metadata with changed bytes and requires a final
SHA256 match within the original deadline before recommending anything. An
optional direct Linux source transfer was rejected by automatic approval
review because it lacked explicit payload/destination authorization; no such
transfer or direct Linux test ran. Normal repository CI supplies Linux
verification; the corrected C1 candidate is running in
[run 3938](http://192.168.4.7:3000/noirr/plurx/actions/runs/3938). HDR
preflight, Rust, web, Apple and Android checks passed, but Windows setup could not resolve
its pinned `dtolnay/rust-toolchain` action revision
`4716b85f2fac3e324e64fa2810f6b5c3905760a5`, before compiling any project code.
The required gate remains blocking; these task PRs have not merged into the
effort or main, and no runner/action pin was changed to bypass the failure.

## 3. Requested follow-on queue

| Priority | Work | State |
|---|---|---|
| 1 | Next-episode preparation | Implemented in `f632af81d`; metadata only, fresh authoritative playback decision, regressions deferred. |
| 2 | Apply qualified encoder policies / retain measured baseline | Retain bitrate: QSV Q22 failed every fixture quality comparison. No justified default change. |
| 5 | HLS acknowledgement batching | Implemented in `157f31ab9`; 128 KiB coordination with unchanged 4 KiB proof. Hook passed; tests deferred. |
| 6 | VOD B-frames | Implemented `6a9599295`; Settings → Developer controls integrated. Runtime/client evidence remains pending. |
| 3 | Broader codec/HDR output | Implemented plain HDR10 VAAPI Main10 at 1080p; m6 inventory confirms encoder/options, qualification remains pending. Does not extend Dolby processing or HDR subtitle burn. |
| 4 | Remaining cold-start latency | Integrated `133be1706` after PR #745: one authoritative held-source collection removes the second stream FFprobe launch. Runtime validation remains deferred. |

## 4. Evidence interpretation

Each measurement row must name source commit, source/corpus hash, tool build,
recipe, host/encoder, resource limits, command, result and scope. Synthetic and
non-production-tool runs are screening evidence. Failed or unavailable evidence
is retained with its reason. No screenshot or metric substitutes for an absent
client, renderer or physical-display run.

Calibration proceeds without user-operated tests. If a route cannot be measured
autonomously, continue other routes and report that limitation; do not silently
broaden a pass from one hardware family or source to another.

The documentation/evidence follow-up received one independent adversarial
review with no blocking finding. Its optional source-container reproducibility
clarification was incorporated. Four docs-index/link tests and the tracked
hook passed; the affected-surface resolver confirms documentation-only scope.

## 5. Initial measured evidence

These receipts are diagnostic screening, not a production-policy qualification.
Source hashes attest the particular captures retained in these receipts.
Regenerated Matroska files may differ in container bytes even when their pixel
recipe is unchanged.

The implementation branches start at `cfb52bf40`; the production CPU tone-map
source hash and actual encoder/tool identities are recorded in the reports.

### Content-aware C1

The [receipt](../evidence/video-quality-2026-10-02/content-aware/receipt.json)
records exact generated fixture recipes and invocation options. The
[easy](../evidence/video-quality-2026-10-02/content-aware/easy.json) and
[motion](../evidence/video-quality-2026-10-02/content-aware/motion.json) reports
retain tool/model/analyzer/exporter identities, production arguments, every
command, and window-level measurements. Both retained VBR: the flat source
missed 10% savings, while cheaper motion candidates lost quality in individual
windows. This is evidence that the rejection rules work, not that every title
should retain VBR. The [deadline control](../evidence/video-quality-2026-10-02/content-aware/motion-deadline.json)
returned inconclusive with no recommendation.

Both successful reports record the matching final source SHA256 after analysis.
Fourteen local focused tests passed, including the frozen-metadata rewrite
regression.

The actual screening used local ARM64 FFmpeg 9.0.1/libvmaf, 320×180 24 fps
six-second generated BT.709 sources, three one-second windows, 500 kbps VBR and
quality values 18/23/28, VMAF floor 90, one thread, 120 seconds and 100 MiB
scratch. The deadline negative control used 0.01 seconds. Run the focused
regressions with `python3 -m unittest discover -s tests/operations -p
test_content_aware_encoding.py`; the script's `--help` describes its bounded
CLI. This requires the sibling production-argument exporter; it is not yet
wired into playback or durable jobs.

After the exporter and C1 branches are integrated, reproduce a bounded run
with an explicitly tagged SDR source:

```bash
cargo build -p plurx-core --example encoder-calibration-args --locked
scripts/content-aware-encoding \
  --input /absolute/path/to/tagged-sdr.mkv \
  --json /tmp/content-aware-report.json \
  --encoder-args target/debug/examples/encoder-calibration-args \
  --height 180 --bitrate-kbps 500 --qualities 18,23,28 --min-vmaf 90 \
  --windows 3 --window-seconds 1 --threads 1 \
  --budget-seconds 120 --scratch-bytes 104857600
```

Use the repository-pinned Rust toolchain and a local FFmpeg with libvmaf;
the source must meet the script's explicit SDR checks. This example is a
small screening run, not a recommended playback bitrate.

### Neutral HDR-to-SDR comparison

The [summary](../evidence/video-quality-2026-10-02/hdr/summary.json),
[full report](../evidence/video-quality-2026-10-02/hdr/evidence-c/report.json),
and [runtime/cleanup receipt](../evidence/video-quality-2026-10-02/hdr/runtime-c.json)
retain exact graphs, source/tool hashes, decoded frame metadata, measurements
and container outcome. The current graph is checked against the Rust production
template, and source MaxCLL presence/value is validated before measurement.
The isolated nuc3 run used the installed Jellyfin FFmpeg 8.1.3 image, one CPU,
1 GiB RAM, no network or GPU, a 40-second command timeout and a 240-second
controller budget. The calibration command completed successfully and its container was removed.

An agent inspected all four contact sheets:
[PQ without MaxCLL](../evidence/video-quality-2026-10-02/hdr/evidence-c/pq-absent-contact.png),
[PQ 1000](../evidence/video-quality-2026-10-02/hdr/evidence-c/pq-1000-contact.png),
[PQ 4000](../evidence/video-quality-2026-10-02/hdr/evidence-c/pq-4000-contact.png),
and [HLG](../evidence/video-quality-2026-10-02/hdr/evidence-c/hlg-contact.png).
Rows are historical/current/current-without-dither; columns are 0.25, 1 and
1.75 seconds. No tint, frame corruption or unexpected temporal discontinuity
was observed in these neutral controls.

For the authored 4000-nit signal, the historical graph reached luma code 254;
the current graph stayed at 235. Dithering reduced the longest equal-code run
in the narrow midtone panel from 272 to 104 pixels (PQ 1000), 272 to 41 (PQ
4000), and 477 to 56 (HLG). This supports reduced quantization banding in these
signals. Historical/current PSNR measures a difference, not correctness; the
neutral signals do not qualify skin tones, gamut mapping, real titles, GPU
paths or physical displays. Tiny-run elapsed times are not performance proof.

Run `python3 -m unittest discover -s tests/operations -p
test_tone_map_calibration.py` for metadata, graph-drift, failure and analytical
control regressions. No production tone-map change is justified by this
receipt alone. Earlier isolated-container attempts failed closed on scratch
permissions and container-level metadata; complete decoded-frame metadata is
required when container color tags are absent, and contradictions still fail.

### QSV encoder E1

The [complete six-fixture screen](../evidence/video-quality-2026-10-02/encoder/qsv-screen-summary.json)
embeds all input reports and their hashes, exact encoder/export/source/corpus/
scorer/model identities, capture argv, decoded frame counts and individual
measurements. Captures used the installed Jellyfin FFmpeg 8.1.3 encoder on
idle nuc4, production VBR/QVBR arguments with quality 22, two software threads,
and one isolated encode at a time. Scoring used local libvmaf model
`vmaf_v0.6.1`. The exact isolated runtime limits and build identities are in
the reports; no replicated setting or production stream was changed.

| Fixture | VBR VMAF | Q22 VMAF | VBR bytes | Q22 bytes |
|---|---:|---:|---:|---:|
| Animation | 97.449873 | 97.075370 | 1,199,051 | 573,849 |
| H.264 motion | 75.865165 | 75.711883 | 12,215,749 | 12,105,230 |
| Web | 97.425834 | 97.046541 | 27,756 | 24,974 |
| Dark gradient | 95.773330 | 93.376812 | 596,194 | 443,137 |
| Grain | 63.424158 | 60.316996 | 11,831,465 | 12,348,054 |
| Sport | 64.618831 | 64.397381 | 12,385,147 | 12,421,483 |

Aggregate bytes fell only **0.885196%** (38,255,362 to 37,916,727), and VMAF
fell on every fixture. Both existing benefit-policy routes failed. The
recorded decision is **retain bitrate pending production qualification**;
this screen does not justify a Q22 default flip. It also does not settle
other quality values, other encoder families, title-level quality, streaming
recovery, or power/throughput under production load.

The first grain capture hit its per-file byte cap and produced 248 frames;
it was rejected and recaptured before scoring. Final reference/VBR/QVBR
counts all match (288 frames per 24 fps fixture; 719 for the 59.94 fps sport
fixture), and duration/grid checks passed. The first QSV container lacked
the render-device supplemental group and failed before encoding; its retry
used the production render-group access. Calibration containers were removed. The [cleanup receipt and failed-attempt hashes](../evidence/video-quality-2026-10-02/encoder/receipt.json) record their disposition.

Run `python3 -m unittest discover -s tests/operations -p
test_encoder_calibration_screen.py` for the 13 focused regressions. The
`encoder-calibration-screen` CLI has separate `capture`, `score` and
`summarize` commands so the shipped encoder can be measured without requiring
libvmaf in its image. The exporter calls the production Rust implementation;
it is not an independently maintained copy of encoding constants.

## Selective fast-lane continuation

The initial preflight found stale ownership/source-count assertions. The owner
ledger now identifies the admitted content phase's cancellation/yield interval,
bounded source snapshot, existing child constructors and awaited reap branches,
one-shot VAAPI boot probe, and test-only VOD/body fixtures. It adds no recovery
owner. The provenance inventory names the three shared ordinal-zero consumers
instead of an anonymous call count; provenance still cannot enter artifact names.

Only the four failed methods were rerun, all passing. The 249 passing methods,
history audit and regression-field checks are retained. Forgejo's aggregate
preflight result remains a truthful record of its failed attempt; its dependency
skips are not test failures. The user's explicit workflow override permits
per-check continuation without a full promotion rerun. Previously skipped
operations, player-contract and affected compile/test jobs run once. The
[selective-repair receipt](../evidence/video-quality-2026-10-03/preflight-selective-repair.json)
and linked PR hold the resulting evidence without relabelling failed CI as green.

The operations suite ran once (614 methods): 604 passed. The explicit native
qualification increased the reasoned-ignore inventory from 20 to 21; that
failed method is corrected and passed once. Nine environment-dependent methods
move to the Linux runner: local loopback/process inspection was sandbox-denied,
and macOS lacks the Linux janitor's GNU timeout. Their original errors remain
in the [operations receipt](../evidence/video-quality-2026-10-03/operations-selective-repair.json).
Five of seven player-contract commands passed. Two extracted-function fixtures
need the new successor cancellation dependency wired into their harness.

Main's seek repair `3197d0c58` merged cleanly as `408297c01` before the first
Rust/Windows/web compiler jobs. Rust source is unchanged by that merge. The
disposable validation branch executes only still-unrun job definitions against
an explicit batch source revision; it will be deleted after receipts are retained.
This avoids repeating successful preflight methods or claiming its original
failed aggregate job passed. PR #766 owns the latest continuation/merge outcome.
