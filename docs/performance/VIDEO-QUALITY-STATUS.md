# Video quality status — what is built, measured and merged

**Status:** implementation merged; autonomous follow-up measurements complete
· **Updated:** 2026-10-08 · **Resume base:** `1088d7529`

Companion to [the programme](VIDEO-QUALITY-PROGRAM.md), which owns scope,
acceptance and order. This ledger records actual execution. Empty evidence is
unverified, not a pass. Dates use America/New_York unless a receipt states UTC.

## Remaining acceptance — resumed on October 8

[PR #766](http://forge.lan:3000/noirr/plurx/pulls/766) landed the implementation
and its required checks as `342521018`. That did not complete every performance
and image acceptance goal. Paul resumed the remaining work with parallel
sessions on October 8. The coordinator uses `codex/video-qualification-followup`
in a fresh independent clone; the owner's checkout is not used.
[Draft PR #956](http://forge.lan:3000/noirr/plurx/pulls/956) collects the logical
commits. Its review, per-check qualification and landing receipts are the
authoritative merge status; the measured outcomes below do not imply an
unmeasured device or image path passed.

| Lane / exclusive ownership | Work being completed | Current evidence and next step |
|---|---|---|
| Encoding: encoding qualification harnesses and receipts | Real-title C2 sample and whole-clip quality/size/time comparison; B-frame client delivery. | Three genuinely tagged SDR titles retain baseline: candidates lose quality or increase bytes. The real-title durable owner persisted source-matched measurements and produced a Ready baseline package. Actual Plurx HLS passes Chrome and all 16 AVPlayer plus all 16 Media3 phases, retaining three initial Android passes and selectively retrying only the failed case. B-frames show no consistent quality/size win; retain the default. Safari automation did not reach media. |
| HDR: captured-image diagnostic harnesses and receipts | Explain the retained sharp-edge failure, then measure broader authored color/highlight/shadow cases with the shipping graph. | Decode, planar/P010 conversion and GPU transfer are byte-exact; independent decoders agree. Quantization/reconstruction causes the sharp-edge error. A quantizer limit improves the image but violates a constrained bitrate budget, so it is rejected. Authored chromatic tone-map inspection is complete; retain the tone-map policy. The bitrate-preserving effort control also trades away quality on real windows and is rejected. Calibration is complete; the original sharp-edge criterion remains failed, and broader graph image/client acceptance is not claimed. |
| Latency: isolated playback measurement harnesses and receipts | Actual-frame episode transitions, startup distributions and delivery resource comparisons. | All 16 initial transition cells passed. Final compiled release passes both natural-ended cells and three ownership boundaries. The close/cancel root cause is fixed through existing playback intent. All 24 release startup cells pass; cold API preparation median is 360 ms versus 491 ms with one versus two source collections. HLS serves 3,600 identical-hash responses with lower daemon CPU in every paired cell; concurrent latency is mixed. These are frozen-source, shared-host measurements, not fleet guarantees. |
| Coordinator: this ledger, programme, docs index and backlog | Integrate logical commits, final review, scoped fast lane, merge and cleanup. | Pinned Rust 1.97.1 release builds passed for both measured variants; native harnesses compile. Newer main is integrated separately for final candidate compilation and checks. PR #956 records the final adversarial review, retained successful checks, selective retries, landing and local cleanup. No deployment is part of this campaign. |

The [programme](VIDEO-QUALITY-PROGRAM.md#8-integration-evidence-and-limits)
remains the acceptance contract. Its original autonomous captured-image scope
explicitly excludes physical display calibration. Wider S-11/Dolby and
hardware-family programmes retain their own acceptance; synthetic or desktop
results will not close their physical-device rows. No threshold is weakened,
no existing failed capture is erased, and no new watchdog or feature gate is
introduced. Each receipt identifies its exact source, tool, client, control,
resource bounds and limitations.

The previously authorized batch workflow remains: one review when this batch
is merge-ready, then required checks with successful results retained and only
failed checks retried. Qualification measurements are the work being done now;
unit suites are not repeatedly run during development. Source changes retain
pinned compile checks and normal hooks. Tracked records use neutral
infrastructure names; private media and fleet configuration stay out of Git.

## October 8 measured outcomes and limits

The [latency receipt](../evidence/video-quality-2026-10-08/latency/qualification-summary.json)
and raw captures bind the current release to `9e018160c`, with a runtime-input
match to `af2156bea` before the two explicit control patches. Newer main adds
preparation-storage lifecycle changes; these measurements are not relabelled
as captures of that later code. The combined landing receives its own pinned
compile and fast-lane checks, recorded in PR #956.

| Mechanism | Matched result | What the result establishes |
|---|---|---|
| Source collection sharing | Four cold pairs: API preparation median 359.55 ms current versus 490.61 ms control; exactly one versus two collections. Warm/resume use one collection in both. | Avoids duplicate cold collection. API preparation has no HLS request. The separate one-frame decoder-process completion metric is not a display clock and uses a combined control. |
| HLS acknowledgement batching | 3,600 responses, identical 473,153-byte objects, zero source collections during serving. Median daemon CPU current/control is 180/430 ms at concurrency 1, 240/460 ms at 8, 330/530 ms at 32. | Lower coordination CPU in all nine paired cells. Serial batch latency improves in all three pairs; concurrent timing is mixed. Sampled RSS is about 92–107 MiB, not an allocator high-water guarantee. |
| Episode metadata preparation | Compiled-release pair with 80 ms metadata delay: gap 383.3 to 135.7 ms, trigger-to-frame 294.5 to 44.3 ms. All three lifecycle boundaries pass. | The actual browser path benefits under this declared delay. Earlier 16-cell results remain separate. Close can no longer synchronously recreate preparation through the pending-seek cancellation tick. |

The HLS control preserves authorization, proof granularity, timeouts and
storage ownership. Different independently encoded objects were rejected
before timing; the final serving-only comparison uses source-copy VOD through
the same production pump. Debug parser identity fallback, rejected HLS setup,
and the original failed close boundary remain retained as failed or invalid
captures, never promoted into passing evidence.

The [native summary](../evidence/video-quality-2026-10-08/native/qualification.json)
records four cases × four phases on each framework. The fresh final Android
case passes with a maximum 154 ms video/player-clock difference against the
unchanged 250 ms bar. Attempt 4 read a stale receipt and is explicitly invalid;
the launcher now clears only its own probe package and verifies receipt
absence. The earlier loaded clock failure remains unexplained by that later
pass. These probes do not qualify physical speaker/display synchronization
or the shipping application UI.

The HDR calibration decision is **retain**: quantizer changes violate bitrate
constraints and the effort control has quality regressions. The sharp-edge
image bar still fails (35 versus <32 codes). Chromatic pixel/filter comparisons
omit production HDR side-data cleanup and do not qualify complete SDR metadata.
Broader graph/device acceptance stays open in its owning programme; neither a
successful neutral ramp nor a simulator pass erases those limitations.

## October 3 execution — merged consolidated batch

Paul authorized replacing per-task testing/effort gates with one larger
merge-ready PR, one final adversarial review, review fixes, and then fast-lane
checks. Only failed checks are retried; pinned Rust compilation and normal
commits remain. This supersedes the historical lane process below for this
effort only. Whole-suite unit repair remains with the separate process.

| Work | Current state | Cause and architectural direction |
|---|---|---|
| Independent workspace | Isolated | Direct Forgejo clones only; no borrowed Git objects or access to Paul's checkout. Final landing and workspace cleanup are recorded in PR #766. |
| Consolidated branch | Qualified for landing | [PR #766](http://forge.lan:3000/noirr/plurx/pulls/766), `codex/video-quality-batch`; main `3197d0c58` integrated as `408297c01`; commit `6ef61fb1f` retains all three tool branches. PRs #760/#761/#762 are closed as superseded. |
| Old per-task CI | Stopped | Encoder run 3936 passed; still-running C1/HDR runs 3938/3942 were cancelled after the workflow override. No additional per-task test campaigns. |
| Content-aware runtime C2 | Integrated `8fd3bba7d` + `e56c80274` | Bounded measurements in existing durable producers, persisted source/recipe identities, offline snapshots and a packaged static scorer. Retained-HLS can reuse a completed measured artifact; misses preserve live policy. Modern immutable VOD is separate. |
| Next episode | Implemented `f632af81d` | Warm one successor metadata page during the final 30 seconds, cancel with the existing playback lifecycle, and retain a fresh authoritative decision at Play. Preparation and extracted player-owner regressions passed; hook passed. |
| HLS acknowledgement batching | Implemented; four delivery/accounting regressions passed | Current pump allocates and schedules a separate channel acknowledgement for every 4 KiB despite 128 KiB storage reads; preserve the 4 KiB proof while reducing coordination, rather than weakening accounting. |
| Apply encoder calibration (2) | Retain baseline | QSV Q22 failed all six quality comparisons; no default change is justified. |
| VOD B-frames (6) | Integrated `6a9599295` and Developer controls | Strict decode/presentation-grid publication, signed offsets and optional software x264 recipe; compile and hook passed. Native FFmpeg 6 and 9 proofs pass, including exact AAC ownership across restarted segments; physical-client evidence remains outstanding. |
| Broader HDR (3) | Implemented `5a8356329` | Plain HDR10 VAAPI Main10 at 1080p with P010 upload and its own graph proof. Compile/hook and isolated lab6 continuous-PQ output qualification passed; hard-edge image and client limits remain recorded below. |
| Cold start (4) | Integrated `133be1706` | One sealed full source collection serves verification and decoder planning. Existing 5-second verification and 10-second refinement budgets remain separate; all three source-sharing/refinement regressions passed and count one stream probe instead of two. PR #745 is integrated. |

The agent's first pinned Rust 1.97.1 compile check in the independent clone
passed. Package-only unused-item warnings are pre-existing; the normal
workspace hook remains the lint check. The combined tree passed pinned all-target
compilation and the normal hook at `d717b82b6`; the cache-consumer follow-up is
compiled in the integrated normal hook at `6cdddaeef`. The final adversarial
review found two blocking issues: application-owned probe metadata entered
source comparison, and fixed-level VAAPI HDR admitted higher source cadences.
The first is corrected in `827023fd2`; the second now uses the same known
at-most-30-fps contract in grade selection and the facts-aware planner.
Runtime qualification passed within the scopes below. The fast lane retains
every passing check and retries only failed checks after these review fixes.
Historical reports below retain their original scope and dates.

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
| Shipping scorer | Actual Docker runtime-assets stage passed on idle lab3 in 398 s with 2 CPUs / 3 GiB, including static ELF closure and built-in VMAF model smoke. The first build exposed the missing C++ runtime link; `555c7defb` fixes it. [Receipt](../evidence/video-quality-2026-10-03/scorer/qualification.json). |
| VAAPI HDR graph | Continuous neutral PQ ramp passed on idle lab6: 96 frames at 1920×1080/24, exact timestamps, Main10/PQ/BT.2020/limited range and complete decode. Actual `hvc1.2.4.H120.B0` equals the declaration. Mean luma error 0.531 and maximum 2 ten-bit codes; 1.960 s encode for 4 s content (2.04× in this synthetic capture). [Receipt](../evidence/video-quality-2026-10-03/vaapi-qualification.json). |
| Developer settings | 36/36 passed, once after review; this changed suite is outside fast lane. Both saved choices remain independent of readiness. [Receipt](../evidence/video-quality-2026-10-03/settings-sections.json). |
| Fast-lane preflight | [Run 3969](http://forge.lan:3000/noirr/plurx/actions/runs/3969) passed scope, history and regression fields. Retain 249 validation methods and 604 operations methods; four validation inventories and one operations inventory passed targeted repairs. Nine environment-dependent operations methods passed on Linux in [run 3972](http://forge.lan:3000/noirr/plurx/actions/runs/3972). Original failed results remain recorded. |
| Player contracts | All seven preflight commands are covered by retained passing checks and focused repaired harness checks. Current-main policy passed 203 assertions; control passed five manual groups and the prepared-buffer case. Twenty-two unchanged native-HLS cases retain matching-main CI evidence. [Receipt](../evidence/video-quality-2026-10-03/web-preflight-repair.json). |
| Rust continuation | Retained 4,863 passing tests; all eight failures now have targeted passing results. Store inventory and the new three-backend contract, probe negative-control and VAAPI fixture repairs passed individually. The AAC partition fix passed packet conservation and native FFmpeg 9 splicing. All four unchanged scheduler tests, the original B-frame failure under FFmpeg 6 and both previously unrun restart checks passed in [run 3979](http://forge.lan:3000/noirr/plurx/actions/runs/3979), pinned to `1cc946893`. [Selective receipt](../evidence/video-quality-2026-10-03/selective-rust-41557-receipt.json). |
| Windows compilation | Passed on final runtime `1cc946893` in [run 3984, job 41591](http://forge.lan:3000/noirr/plurx/actions/runs/3984). The original attempt against `bd24f2767` also passed. Final-source job 41558 lost its runner before timeout; the [failed receipt](../evidence/video-quality-2026-10-03/selective-windows-41558-receipt.json) remains recorded. Only Windows was retried on an online baremetal runner, with the same source, container, toolchain and compile command. No runner service changed. |
| Web continuation | Syntax, 13 media/layout tests and generated jsconfig passed. Two new checkbox typing diagnostics were corrected without runtime changes or a baseline increase; only the failed type check and previously unrun typedef check were then executed and passed. [Receipt](../evidence/video-quality-2026-10-03/continuation-receipt.json). |

The [combined qualification receipt](../evidence/video-quality-2026-10-03/batch-qualification.json)
binds the final runtime source, retained checks, targeted repairs, all 37
landing regression references and 27 named Rust passes. Documentation-only
follow-ups retain the normal hook; no successful suite was repeated. The
original failed CI jobs remain failed in Forgejo. This aggregate records
per-check qualification under the owner-approved workflow, not a fabricated
all-green run. Main `3197d0c58` was unchanged at final qualification.

**Rust failure disposition:** the initial continuation retained 4,863 passes
and eight failures. Store coverage omitted the newly added publication method;
the inventory now includes it and a contract passes against in-memory SQLite,
file SQLite and replicated Hiqlite. Two test fixtures were corrected: the
probe negative control now changes a fact present in both documents, and each
VAAPI cadence case owns its own store. Four unchanged scheduler tests failed
before their behavior assertions at provider-entry/startup deadlines; all four
passed serially, unchanged, in the original Linux environment. Full-suite
deadline fragility remains a separate repair concern; contention is a hypothesis,
not a proven production defect.

The B-frame failure identified a production cause: mux fragments grouped AAC
by decode order while independently restarted segments used presentation
boundaries. The segmenter now partitions encoded AAC at the shared planned
presentation boundary, preserving every packet and leaving copied-media
segmentation unchanged. Packet conservation and the original end-to-end test
pass; both previously unrun restart tests also pass. No new retry loop,
watchdog or timing relaxation was added. The [timeline design](../streaming/VOD-BFRAMES-TIMELINE-DESIGN.md)
records the corrected model.

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

**Failure capture continuation (2026-10-07):** the original sharp-panel
receipt is unchanged, but its raw source, encoded output and decoded frame
were removed by the old wrapper. It cannot establish where the 35-code
maximum occurred. Capture version 2 of the existing
[VAAPI harness](../evidence/video-quality-2026-10-03/qualify-vaapi.py) records
full media SHA-256 and byte sizes before the unchanged mean <4 / maximum <32
assertion. Its reference is one planar `yuv420p10le` frame repeated into the
96-frame FFV1 source; it compares decoded frame 0, refusing wrong dimensions,
truncated/extra raw frames and samples outside ten-bit range. Maximum-error
coordinates/count and a reference-derived partition (constant four-neighbour
interior versus transition or frame border) are diagnostics, not new quality
criteria or an encoder-cause verdict.

The [wrapper](../evidence/video-quality-2026-10-03/run-vaapi.sh) accepts an
optional recipe path (default: its adjacent recipe) and allocates a new
mode-0700 private `/var/tmp/plurx-vaapi-qualification.*` directory. The exit
trap attempts bounded watcher termination (3 seconds TERM, then 2 seconds
KILL) and removes only its validated exact owned container ID (5 seconds,
then a 1-second forced timeout). A valid regular nonsymlink private cidfile
recovers the identity if Docker stdout is empty or invalid. Success still
removes all four media files; failure retains only that invocation's
synthetic media if the entire bundle is at most 512 MiB. `retention.json`
names the exact private path, four cleanup files and a 48-hour review/delete
deadline: you must archive privately or delete those exact media files by
then; no broad temporary-directory purge is scheduled. Over-budget media
are deleted, with the receipt recording that they were not retained.
`retention.json` preserves the original command exit status, actual cleanup
results and exact recovery CID; `cleanup.confirmed=false` means cleanup is
incomplete even if the qualification command succeeded. Review the private
cleanup log and recover only that owned identity, never a container guessed
by name. The capture does not claim that a failed removal stopped anything.
This plumbing does not recreate the lost pixels, rerun the failed cell,
qualify sharp edges or change the encoder, graph, recipe or source generator.

**Original input recovery and diagnostic selection (2026-10-08,
gpt-6.1-sol, `agent:/root/remaining_requirements_audit_sol61`):** a selected
historical generator record was recovered and independently source-reviewed.
One separately authorized, encoder-free local restoration produced the exact
one-frame reference: 6,220,800 bytes, SHA-256
`777e283050d76a5d51c723ee0a23d04f2abee492aa02046803441715fb6b17f9`.
The private restoration receipt has SHA-256
`09ed50aa1a7130b8cfabd41db791ee0955777c533a347cf80a51666a96085586`.
This restores the original three 360-row panels (0–1000, 0–10 and 20–21 nits),
not the lost encoded/decoded output or the `d54d3edb` source-media container.
The historical max-35 failure remains unresolved.

The existing wrapper now accepts a fixed optional second argument,
`original-sharp-panels`, after its existing recipe argument. Omitting it
retains the byte-identical `continuous-pq-ramp` default; no arbitrary raw path
is accepted. Unsupported selectors fail before private-path allocation or
Docker/HTTP work. The qualifier validates independently, pins the selected
raw digest and reports requested/actual signal without substituting the ramp.
Capture version 2, geometry, 96 encoded repetitions, all recipe/filter/encoder
arguments, mean <4 / maximum <32 bars and bounded cleanup are unchanged.
The new pure-source regression and three existing capture regressions have
changed shared source witnesses; their current inputs require new applicable
receipts, not reuse of old passes. No regression, encode or decode was run
while authoring this continuation; a future failed-cell diagnostic needs its
own reviewed runtime admission. Input recovery is not S-11 qualification.

**One original-sharp diagnostic and watcher correction (2026-10-08,
gpt-6.1-sol, `agent:/root/remaining_requirements_audit_sol61`):** one separately
admitted case used the freshly exported `4a737e92` recipe projection on
lab6's actual older `8e242787` image/Jellyfin FFmpeg 8.1.3. The reference raw
SHA-256 is the original `777e283050d76a5d51c723ee0a23d04f2abee492aa02046803441715fb6b17f9`.
All six media commands returned 0, but the unchanged quality bar **failed**:
mean error 0.368015, maximum 35 ten-bit codes. Capture version 2 records one
maximum at x=7, y=360, reference=99, decoded=134; the reference-constant
interior maximum is 5 (mean 0.28236), versus 35 in transitions/frame borders.
This locates the observed worst pixel; it does not establish encoder cause,
fix it, qualify current daemon output, or close physical/client/S-11 bars.
Output SHA-256 is `9807fc7be12652a8b44f5b13ffb732cfba90333e0699382179a27903aa9a2198`;
decoded raw SHA-256 is `6c3a33672cb1e21ea7c49dbbdcbd0003c40aba9d48e84eb1db225bd05e982147`.
These are the new failed-case bytes, not recovered historical d54d/encoded/
decoded outputs.

The original retention receipt reports watcher `stop_not_confirmed`; the
outer and read-only recovery receipts also preserved unsuccessful absence
interpretations; their negative flags remain unchanged. Offline interpretation
of the recovery's raw Docker exit 1, whitespace-only stdout and exact lowercase
no-such-object error supports a separate exact-container absence observation;
its recorded wrapper-group query also found absence. That is not retroactive
helper success. Recovery did not rerun the case or kill any such group.
The wrapper's `jobs -p` included completed jobs
until wait, causing a false unconfirmed watcher status and a five-second
delay. Its helper now counts running **and stopped** jobs (`jobs -pr` plus
`jobs -ps`), excludes completed jobs and does not signal a watcher already
known complete before retrieving its actual wait status. TERM/KILL budgets,
owned CID recovery, media retention, signals, defaults and quality bars are
unchanged. One bounded Bash-process regression is authored but not executed
here; shared source witnesses change the applicable capture control family.
The batching owner owns those five focused receipts and all broader input
applicability. No old passing control or media case was replayed by this
source-only correction; historical failures remain failed.
The seven failed-case media/report files are retained privately. The original
batch-cleanup paragraph below retains its #766 scope, not this new case.

The scorer builder/image/containers/volumes and lab6 container/media/control
scratch are removed. No production settings, queues or deployments changed.
The batch keeps B-frames and measured per-title encoding in Developer pending
their stated broader evidence; readiness cannot veto either saved choice.

## 1. Integration and compiler

| Fact | Value |
|---|---|
| Planning base | Forgejo main `4fa50b79e4b3b196797a9b7a1a7a4abd2184ea43` |
| Planning branch | `codex/video-quality-program` |
| Documentation PR | [#758](http://forge.lan:3000/noirr/plurx/pulls/758) merged as `cfb52bf402e603523f6fd5b3d71b6ffa2c751565`; one adversarial review, two P2 ambiguities addressed; exact-head Main promotion gate passed in [run 3923](http://forge.lan:3000/noirr/plurx/actions/runs/3923) |
| Effort branch | `effort/video-quality`, published from `cfb52bf40` after the plan merged |
| Compiler | `~/.cargo/bin/rustc`: `1.97.1 (8bab26f4f 2026-07-14)` |
| Compiler-loop baseline | `cargo check -p plurx-core --lib --locked --offline` passed on planning base; package-only baseline reported pre-existing unused-item warnings. Pinned workspace Clippy and tracked hook passed. |
| Original checkout | Left untouched, including pre-existing uncommitted documents |
| Production changes | None |

## 2. Historical first wave — superseded task workflow

| Lane | Owner | State | Evidence / next action |
|---|---|---|---|
| A: encoder calibration | `/root/encoder_calibration` | E1 implemented; [PR #762](http://forge.lan:3000/noirr/plurx/pulls/762) open into effort | Commit `1942cf0ca`; production-argument exporter and six-fixture QSV screen. 13 focused tests, 56 existing bench tests and tracked hook passed. QSV quality 22 fails the benefit gate; bitrate mode retained. |
| B: content-aware encoding | `/root/content_aware` | C1 implemented; [PR #760](http://forge.lan:3000/noirr/plurx/pulls/760) open into effort | Initial commit `162418ab1` passed 13 local tests; Linux preflight exposed a same-timestamp source rewrite. Fix commit `d7e96b12c` requires final source rehash; 14 local tests, refreshed real smokes and the tracked hook passed. C2 durable/runtime integration remains unstarted. |
| C: HDR-to-SDR images | `/root/hdr_calibration` | Implemented; [PR #761](http://forge.lan:3000/noirr/plurx/pulls/761) open into effort | Commit `f68e6cd0a`; four authored neutral PQ/HLG cases × three variants × eight frames inspected. Three focused tests and tracked hook passed. Complements existing S11 scorer. |

These rows record the original lane state before consolidation. PRs #760,
#761 and #762 are now closed with their implementation retained in #766;
the current execution table above supersedes their gate and queue states.

Coordinator owns docs, index, validation catalogue and shared integrations.
Encoder lane reserved media1, but the immediate pre-run check found one active
viewer and skipped all calibration there. The reservation moved to idle lab4
for one isolated QSV workload, with cancellation when playback appears. HDR lane reserves lab3 for one CPU-only process/container
(1 CPU, 1 GiB) after the same idle check. Local Docker queries timed out, so
no daemon restart was attempted. Content-aware smoke runs locally with bounded
scratch. Shared Cargo checks serialize with incremental compilation disabled;
the coordinator removed only this task's disposable incremental cache after an
ENOSPC hook failure, then reran the hook successfully.

Required effort gates were dispatched for content-aware
[run 3931](http://forge.lan:3000/noirr/plurx/actions/runs/3931), HDR
[run 3933](http://forge.lan:3000/noirr/plurx/actions/runs/3933), and the
encoder's corrected metadata commit
[run 3936](http://forge.lan:3000/noirr/plurx/actions/runs/3936).
The first C1 run caught the source-rewrite regression described above. Its
fix reproduces identical stat metadata with changed bytes and requires a final
SHA256 match within the original deadline before recommending anything. An
optional direct Linux source transfer was rejected by automatic approval
review because it lacked explicit payload/destination authorization; no such
transfer or direct Linux test ran. Normal repository CI supplies Linux
verification; the corrected C1 candidate is running in
[run 3938](http://forge.lan:3000/noirr/plurx/actions/runs/3938). HDR
preflight, Rust, web, Apple and Android checks passed, but Windows setup could not resolve
its pinned `dtolnay/rust-toolchain` action revision
`4716b85f2fac3e324e64fa2810f6b5c3905760a5`, before compiling any project code.
The required gate remains blocking; these task PRs have not merged into the
effort or main, and no runner/action pin was changed to bypass the failure.

## 3. Requested follow-on queue

| Priority | Work | State |
|---|---|---|
| 1 | Next-episode preparation | Implemented in `f632af81d`; metadata only, fresh authoritative playback decision; preparation and player-owner regressions passed. |
| 2 | Apply qualified encoder policies / retain measured baseline | Retain bitrate: QSV Q22 failed every fixture quality comparison. No justified default change. |
| 5 | HLS acknowledgement batching | Implemented in `157f31ab9`; 128 KiB coordination with unchanged 4 KiB proof. Hook and all four delivery/accounting regressions passed. |
| 6 | VOD B-frames | Implemented `6a9599295`; Settings → Developer controls integrated. FFmpeg 6 and 9 server proofs passed, including restarted AAC ownership; broader physical-client evidence remains outstanding. |
| 3 | Broader codec/HDR output | Implemented plain HDR10 VAAPI Main10 at 1080p; lab6 continuous-PQ signal qualification passed; broader image/client evidence remains outstanding. Does not extend Dolby processing or HDR subtitle burn. |
| 4 | Remaining cold-start latency | Integrated `133be1706` after PR #745: one authoritative held-source collection removes the second stream FFprobe launch. Compile checks and all three source-sharing/refinement regressions passed. |

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
The isolated lab3 run used the installed Jellyfin FFmpeg 8.1.3 image, one CPU,
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
idle lab4, production VBR/QVBR arguments with quality 22, two software threads,
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
