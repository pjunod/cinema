# Playback rewrite remainder — the next Sol implementation handoff

**Status:** ready to execute; observability implementation is already assigned.
**Reconciled:** 2026-09-12. **Source baseline:**
`10f2afe60b3d177866fdcc5741acd9f494525d73` on `main`.
**Lifecycle landing:** [PR #259](http://192.168.4.7:3000/noirr/plurx/pulls/259),
merge `5548c3d3c0b631af6df8d15c3c27e0d6a9880068`.

Read this first, then the [lifecycle coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md)
for the relevant L01–L33 transitions and C01–C21 test anchors. The
[implementation contract](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) explains the
behavior already built; the [execution ledger](PLAYBACK-LIFECYCLE-STATUS.md)
retains its compilation and historical test-deferral receipts. This document
assigns the remaining work. Old M5–M9 milestone headings do not establish that
code is still missing.

The next Sol session owns packages B02–B05 below: execute the focused cases,
fill missing regression coverage, repair defects they expose, and reconcile
remaining compatibility callers. B01 belongs to an existing session. Do not
start another wholesale playback rewrite or another instrumentation effort.
Maintain one result ledger in the existing lifecycle status page, with exact
source/build identities and explicit `passed`, `failed`, or `not run` results.

## 1. What changed since the previous remainder

The previous header said source implementation was closed and only acceptance
remained. That described P1–P4's landing, but it omitted the now-requested
buffer instrumentation/UI work and overstated what unexecuted tests establish.
The accurate state is: lifecycle code is merged; the initiating freeze is not
yet isolated; observability is being built; runtime/device acceptance and
any defects found by it remain open.

| Already on main | Existing owner/source | What the new session must not rebuild |
|---|---|---|
| Explicit client demand, control consultation and bounded leases | [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | A new client/server control protocol |
| Rolling production pacing and acknowledgement; VOD demand/materialization | [transcode.rs](../../crates/plurxd/src/transcode.rs), [vodserve.rs](../../crates/plurxd/src/vodserve.rs) | Another refill credit, watchdog or global buffer increase without a demonstrated failing rule |
| Loaded-wait recovery ownership | Apple [PlayerController](../../clients/apple/Sources/PlayerController.swift), Android [Controller](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt), [web player](../../crates/plurxd/src/web/index.html) | A second attachment/reopen executor; detector count is not action-owner count |
| Prepared reservation, both-engine priming, compound recipe planning and settlement | [hls.rs](../../crates/plurxd/src/http/hls.rs), client prepared adapters, [store contracts](../../crates/plurx-core/tests/store_contract.rs) | Another staged-generation store or commit protocol |
| Planned remote relocation under restart/maintenance fences | [serving_fence.rs](../../crates/plurxd/src/serving_fence.rs), [internal media routes](../../crates/plurxd/src/http/internal_media_sessions.rs), HLS preparation path | A separate cluster handoff coordinator |
| Subtitle readiness/windows, seek coalescing and VOD marker prewarm | [subtitle source](../../crates/plurxd/src/http/subtitles.rs), VOD/control paths | The historical M7 backlog from scratch |
| Advisory enablement and removal of empirical preparation vetoes | Server preparation decisions and all three client adapters | Device allowlists, throughput/proof admission gates or permanent opt-out after one failed attempt |

**How to read it:** merged source is not evidence of uninterrupted physical
playback. The source build counters advanced again in PR #260; neither that
commit nor a compile result identifies what is installed on the Apple TV.
Re-read current main and deployment identities before calling a result current.

## 2. Ownership and bounded delivery order

| Package | Owner and state at this reconciliation | Deliverable |
|---|---|---|
| B01 · five-stage buffer observability and playback UI | Existing Sol task `01a095b1-0e77-72f1-8b48-b04da47f8f10`; in progress, no product commit or PR at receipt time | Anchored server-ready/transfer/client-loaded/presentation measurements; Apple TV-first stats and waiting UI; Android/web parity |
| B02 · refill and operation-race closure | New Sol session | Executed focused regressions, missing cases added, demonstrated defects repaired, useful incident trace |
| B03 · prepared transition closure | New Sol session | Actual readiness/settlement/cleanup coverage across recipes and clients; bounded fallback verified |
| B04 · cluster and alternate-ingress closure | New Sol session | Planned/abrupt failure cases, cross-owner routing/auth behavior and resource cleanup verified |
| B05 · subtitle, hybrid compatibility and final accounting | New Sol session | Targeted subtitle/two-engine/mixed-client coverage and justified deletion/disposition of remaining callers |

B01's published branch is `effort/buffer-observability`, last reported at
`10f2afe60b3d177866fdcc5741acd9f494525d73`. Its independent clone is
`/private/tmp/plurx-codex-buffer-observability-20260912`. That is the other
session's working directory, not a checkout for the new session to edit.
The owner reported compiler setup complete, source survey in progress and no
tests executed. Verify its latest branch/PR before integration; these are
receipt-time facts, not a permanent assertion of no progress.

Start B02's existing tests while B01 continues. B03/B04 can also proceed without
waiting for the stats layout. Join B01's final metrics when recording traces;
do not copy uncommitted changes or label an unpublished branch as merged.
Use substantial packages rather than one PR per transition. No new session is
created by this document; Paul will start the builder.

## 3. B01 contract — the observability work already assigned

This section is the integration contract for the new session, not a duplicate
assignment. The request is to distinguish buffer levels clearly in stats/info
and show useful context more prominently during an actual wait.

| Stage | Authoritative meaning | UI interpretation |
|---|---|---|
| Source | Measured source availability/read activity, when exposed | Unknown if unmeasured; file size alone is not a source buffer |
| Server ready | Complete, currently readable media contiguous from a named current film anchor | Seconds of available media ahead, independent of the client buffer |
| HTTP transfer | Server-observed active request/wait, response completion and measured byte rate | Completion is not proof of client receipt, demux or decoding |
| Client loaded | Native contiguous loaded range containing current playhead, or pending seek target | Seconds available locally; distant islands do not count |
| Presentation | Observed frame/clock progress and its age | Loaded media with no presentation is a distinct state, not proof of decoder failure |

Production target, producer running/paced/resource-wait state and production
progress sit beside those stages. They are not another playable buffer.
Do not invent a decoded queue size when the platform cannot report one.

### 3.1 Server-ready semantics must survive seeks and eviction

All media coordinates use one absolute film timeline. Capture the anchor,
incarnation/attempt, owner epoch and observation age with the measurement.
`ready_ahead_ms = max(0, ready_end_ms - anchor_ms)` is valid only if the
inventory covers that anchor continuously through `ready_end_ms`.
Unknown coverage is unknown; a known missing anchored segment is zero.

**Rolling HLS:** read complete publication and the retained/readable segment
window of the active attempt. A publication endpoint is insufficient if the
anchor fell out of retention or there is a hole. FFmpeg `out_time_ms`, pending
output and advertised playlist horizon cannot establish readiness. Apply
`media_origin_ms` exactly once. If an evicted segment remains loaded on the
client, report the two facts separately; do not diagnose an empty client.

**Immutable VOD:** use the active rendition's materialized segment inventory
and timeline, starting at the segment containing the accepted playhead or seek
target and stopping at the first gap. In this baseline, `ready_ahead_end_ms`
is computed from the last-served segment and `ahead_seconds` subtracts the
fetched frontier. They are fetch-anchored quantities and cannot simply be
renamed playhead-anchored buffer. Distant cached islands, total cached bytes,
planned entries and movie duration are not contiguous runway. Include required
init/track readiness or explicitly report the narrower measurement available.

Reuse bounded manifest/index snapshots and existing update cadence. Do not
scan files or probe every segment on each UI refresh. Samples of different
ages must not be subtracted to manufacture a causal diagnosis. Current and
prepared-successor measurements stay separate; old-epoch responses cannot
overwrite the current view.

### 3.2 Stats and waiting UI must explain the numbers

Standard stats/info gets separate named server-ready and client-loaded rows,
transfer activity and presentation state; production target remains labeled
as a target. Debug detail can show anchors, raw frontiers, sample age, owner,
episode and recovery action/deadline without capability URLs or credentials.

A compact playback-control indicator may show the client buffer. During an
actual wait, use observed facts such as `Waiting for media · 0.3 s loaded` or
`Playback waiting · 22 s loaded`. A healthy paced producer does not trigger a
buffering warning. Never replace the explanation of a frozen frame with the
unsupported claim that server pacing caused it.

Instrumentation is ordinary observability, available with stats/info by
default. Existing display preferences may control visibility. It needs no
Developer enablement/advisory checklist. The no-software-gating requirement
continues to apply to capabilities; measurements do not grant permission.

**Acceptance owned by B01:** regression coverage for nonzero origins, disjoint
ranges, seek/reset, null/zero/stale, old-owner replies, transfer versus loaded
media, healthy pacing, both engines and current/prepared separation; inspect
the Apple TV layout and corresponding Android/web mappings. Integrate its
receipt, not a second implementation, into the final remainder accounting.

## 4. B02 — execute the buffer and lifecycle cases, repair proven defects

**Entry points:** `evaluate_flow`, `DeliveryView::from_status` and
`recovery_outranks_hold`; Apple recovery monitor/control snapshot mapping;
Android `OpenPlaybackStallTracker`; web `beginWait` and control application.
The [coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md) names the owning source and
test fixtures. Existing seed regressions can run without a new framework:

```bash
rustup run 1.97.1 cargo test -p plurxd --locked --bin plurxd lifecycle_refill_
node tests/playback/web-control.test.js
```

The first command includes
`lifecycle_refill_empty_and_loaded_waits_are_not_answered_with_their_hold`
and `lifecycle_refill_loaded_native_wait_outranks_a_producer_hold` at this
baseline. Confirm selected tests actually execute; a zero-test result proves
nothing. Run changed Apple ownership/mapping XCTest cases and Android
ownership/telemetry cases using their existing platform test targets.

**Cases to close:** empty supply; complete server media awaiting transfer;
substantial client-loaded media with stopped presentation; repeated `hold` or
passive `none`; unavailable control with healthy media and its inverse; pause
while buffering; resume; seek storm; stop during pending create; restart before
old release completes; recipe change during recovery. Exercise normal play,
initial start and refill on explicitly identified VOD and rolling engines.

**Implement only where needed:** missing state-transition fixtures, association
between wait episode and accepted control/status snapshots, and the owning
rule that a failing regression identifies. Preserve the one native nudge and
absolute deadline. Duplicate messages cannot mint work or reset the bound.
Do not add speculative pacing policy merely because the incident mentioned
pacing. Remove obsolete action branches only after their actual callers are
routed through the current owner.

**Acceptance:** healthy refill resumes presentation on the same session;
exhausted recovery ends or performs one bounded replacement under latest
intent; pause/stop never resurrect playback; no leaked worker or stale attach.
Record wait start, stage samples, action, resumed presentation or terminal
outcome. A reopened player is not evidence that same-session refill passed.

The retained [September 11 trace](../evidence/playback-lifecycle-observation-2026-09-11.json)
for Ronny Chieng: Speakeasy contains both near-empty and substantial loaded
waits with producer holds. It does not isolate the initial cause. Use that
title on Apple TV if available, but do not depend on it for deterministic
fixtures or claim the original freeze fixed without a new observation.

## 5. B03 — prove prepared readiness and settlement across clients

**Entry points:** HLS `process_preparation_candidate`,
`stage_prepared_successor_with_prime`, `settle_committed_preparation`;
[Apple](../../clients/apple/Sources/PreparedReplacement.swift),
[Android](../../clients/android/app/src/main/java/tv/plurx/app/player/PreparedReplacement.kt)
and web prepared adapters; SQLite and Hiqlite preparation contracts.

**Preserve the implemented v1 order:** reserve/prime → client readiness →
local switch and observed first presentation → committed acknowledgement →
durable pointer CAS → predecessor drain. Metadata or `buffer_ready` cannot
commit the transaction. A lost reply after local switch must reconcile the
winner, not assume the predecessor is still attached.

**Cases to close:** Original from transcode, quality up/down, supported
copy/transcode changes, audio/offset/subtitle/grade and compound intent;
second intent during prime; pause/stop/disable before and during switch;
duplicate/delayed acknowledgements; refusal, expiry, allocation failure;
first presentation followed by rejected/lost commit; target readiness at a
nonzero origin; ordinary single-player fallback. Force both engines and
record actual selected recipe rather than only requested intent.

**Build remainder:** extend existing fixtures for missing cases, execute them,
and repair failures in the existing adapters/transaction. Test resource
release after each terminal path. Separate required media readiness from
optional subtitle metadata. If browser frame APIs differ, observe a genuine
supported progress signal or use the bounded fallback; never fabricate a
first-frame timestamp or gate the feature on empirical proof.

**Acceptance:** one durable current incarnation, preserved viewer intent and
position, bounded preparation, no leaked second decoder/worker, and an actual
prepared switch distinguishable from fallback reopen. Runtime receipts should
measure interruption and position error; no new universal "seamless" promise.
A small physical pass covers Apple TV plus available Android/web clients;
unavailable hardware stays an explicit evidence gap, not an enablement veto.

## 6. B04 — close relocation, owner loss and alternate ingress

**Entry points:** `planned_preparation_still_current`,
`prepared_relocation_owner`, remote preparation/retirement in HLS;
[cluster operations](../../crates/plurxd/src/http/cluster_operations.rs),
serving fences, media-session takeover and the two store implementations.
The restart/maintenance caller already triggers relocation on accepted control;
merely adding an internal relocation helper is not the remaining assignment.

**Cases to close:** ready/unready target; missing source; exhausted real
resources; restart cancellation/expiry while reserving and priming; crash
before switch, after first frame/before commit, and after commit/before reply;
duplicate commit; old-owner rejoin; source owner unavailable with/without a
fresh client position; control ingress different from media owner. Run both
SQLite transaction cases and replicated ownership cases where applicable.
Enable `hiqlite-contract-tests` and `cluster-integration-tests` when selecting
those fixtures; default compilation does not activate them.

**Build remainder:** fill missing integration cases and repair demonstrated
routing, cancellation or cleanup defects. Audit native alternate-ingress
retry and the browser CORS/auth contract using existing relay routes. Validate
session/user/media authorization on the real request, not only a mocked route.
Never expose signed URLs or add a local redirect that bypasses owner CAS.

**Acceptance:** exactly one winning incarnation/epoch; staged media reachable
before current-owner publication; old owner drains only its predecessor;
uncommitted target resources eventually release; cancellation cannot commit;
healthy loaded media remains usable while takeover resolves. Abrupt owner loss
may interrupt after buffer is consumed and must end in bounded recovery/error.
Moving one VOD stream does not prove offline/direct/live activities drained.
Use isolated test nodes for destructive fault injection; production restart or
deployment requires the applicable operational authorization.

## 7. B05 — finish subtitle and compatibility coverage, then reconcile deletion

**Subtitle/seek:** execute native text readiness/retry, repeated seeks and
burn-change joining alongside playback. Assert text-only retry does not
restart video; abandoned seeks release their work; the latest subtitle/seek
wins; foreground refill outranks prewarm/extraction. The VOD marker consumer
already exists. Fix demonstrated failures; do not build semantic detection.

**Hybrid and mixed-client coverage:** force immutable VOD and rolling fallback
and assert the returned engine. Include pause/resume, EOF, stop/restart,
control loss and fallback recovery. Verify direct/progressive, live TV and
library-channel adapters retain their distinct lifetimes. Do not mistake a
movie title or remux codec for an engine assertion. Exercise supported legacy
control clients and current clients without enabling two recovery owners.

**Cleanup deliverable:** inventory remaining action-making callers by symbol,
owner, engine/client requirement and disposition. Remove only proven duplicate
or unreachable code with focused regressions. Keep observation-only polling,
lease renewal and valid legacy/fallback adapters. Explicitly supersede old M9
instructions whose timer-count target conflicts with correct bounded ownership.
Update the remainder/index and historical entry-point notices rather than
rewriting every old evidence document.

**Acceptance:** the applicable L01–L33 rows have a named test/result or a precise
unavailable-evidence reason. No source gap is closed merely by marking a table
built. All remaining compatibility paths have a reason or a removal commit.
Retirement of rolling HLS is a separate product decision; it is not part of
"build the rest" under the user's hybrid-playback requirement.

## 8. R01–R10 dispositions and explicit exclusions

| Original ID | Current disposition | Closing package |
|---|---|---|
| R01 refill/pacing | Code landed; initiating freeze and runtime liveness evidence remain open | B01/B02 |
| R02 recovery ownership | One-owner source inventory exists; operation ordering and justified cleanup remain | B02/B05 |
| R03 physical prepared acceptance | Implementation exists; runtime and device continuity remain unestablished by the ledger | B03 |
| R04 recipe/device coverage | Ordinary planner and compound candidates landed; constrained-resource and supported recipe coverage remain | B03 |
| R05 planned/abrupt owner transition | Remote prepared path landed; phase-failure/ingress evidence remains | B04 |
| R06 subtitles/seeks | Source exists; physical readiness, priority and cancellation evidence remain | B05 |
| R07 mixed fleet/two engines | Required coverage remains; rolling retirement explicitly excluded | B05 |
| R08 joined observability/alternate ingress | Concrete instrumentation/UI implementation in flight; ingress evidence remains | B01/B04 |
| R09 broader decoder fleet | Adjacent evidence project; only cases implicated by this work belong in this handoff | Targeted B02/B03; broader work stays separate |
| R10 semantic detection/extra prewarm | Authored markers and VOD prewarm built; semantic detection and unconfirmed extensions deferred | No new assignment |

Do not add a decoder backend, cluster scheduler, new control protocol, semantic
intro/credits detector, blanket buffer increase or watchdog architecture.
Do not turn all historical hardware permutations into a development blocker.
The [decoder fleet handoff](../streaming/DECODER-M8-HANDOFF.md) remains the
separate reference for broader evidence; no software qualification gates may
be imported from old documents.

## 9. Execution rules — current instructions supersede historical deferral

Paul supplied replacement AGENTS.md instructions on 2026-09-12. They require
focused local behavior regressions before pushing, blocking effort compilation
and current-tree main qualification. The previous lifecycle campaign's
`written; not run` instruction describes its historical receipts only. It is
not the testing policy for this new work.

1. Use an independent clone from current main. Record the base and coordinate
   B01 integration without editing its clone. Reuse existing tests and actors.
2. Before Rust edits, establish the pinned Rust 1.97.1 loop in the
   [compile-loop guide](../ci/AGENT-COMPILE-LOOP.md). Run formatting, check,
   Clippy and the smallest relevant regressions locally before pushing.
   Archive committed source only for a remote compiler; no `.git` or credentials.
3. Compile affected Apple/Android/web surfaces and execute focused changed
   cases. Record exact commands, selected test counts and outcomes. Retain
   historical `not run` results rather than retroactively rewriting them.
4. Use task branches into one temporary effort when the work spans packages.
   `Effort development gate` is blocking. Freeze integration, incorporate
   current main, then require the current candidate's `Main promotion gate`
   and qualification receipt. Requalify if either head or main moves.
5. Follow the tracked lint/syntax hook policy and complete current catalog,
   documentation, corrective-evidence and release-counter bookkeeping. Do not
   copy the old handoff's blanket hook bypass or fast-lane-only instructions.
6. Reconcile the CI mechanism before the first effort push. At this baseline,
   [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) still opens with the
   September 10 fast-lane-only correction, and checked-in workflows retain
   that lane. The replacement user instructions take precedence. An old green
   compile-only lane is not the newly required qualification receipt. Resolve
   the supported current workflow with its owner; do not silently bypass it,
   fabricate evidence or redesign CI as incidental playback work. Continue
   local implementation and focused tests while an infrastructure gap is open.

Repository qualification controls source promotion. User-facing feature
availability remains independent: requirements are advisory, not empirical
software gates. This handoff does not itself authorize deployment or fault
injection on a viewing node.

## 10. Completion receipt — finish the work without inventing another rewrite

For each B package, record source/base SHA, PR, changed symbols, tests actually
run and their counts/results, device/build/engine when observed, and remaining
issue/owner. For a wait or handoff capture intent, film anchor, current and
successor identity, available/loaded media, sample ages, action, presentation
resume, interruption and cleanup. Keep traces bounded and sanitized.

The source assignment is complete when B01 is integrated or explicitly retained
with its existing owner, B02–B05's focused cases execute and demonstrated code
defects are repaired, and each retained adapter has a justified owner. Report
physical cases still unavailable separately. Do not call the overall request
complete while B01's requested UI remains unbuilt; give its exact remaining
owner/PR instead. Broad fleet evidence, semantic detection and rolling-engine
retirement remain separate scopes rather than indefinite reasons to keep this
effort open.
