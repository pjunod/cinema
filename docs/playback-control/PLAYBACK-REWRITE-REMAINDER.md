# Playback rewrite remainder — what is unfinished and how it relates to freezes

**Status:** open — reconciled implementation and acceptance backlog,
2026-09-12 UTC. **Baseline:** `efd54247adddeb3978812e55ebbb6a7f08adc9d9`, the
server revision observed during the September 11 Apple TV investigation.
**Scope:** playback-control M1–M9 and directly related delivery work; this is a
source audit and execution order, not a claim of new physical acceptance.

Companion to [PLAYBACK-LIFECYCLE-COVERAGE.md](PLAYBACK-LIFECYCLE-COVERAGE.md)
(the 33 transitions, buffers and communications),
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
(the original design), and
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md)
(the historical execution record). Use this dated reconciliation to interpret
conflicting old status paragraphs. Re-verify source before implementation.

The [implementation contract](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) now supplies
the bounded build sequence and current policy. Historical proof/headroom
restrictions below describe audited source, not the desired enablement rule:
remove empirical vetoes, keep Developer advice nonbinding, and use the revised
fast lane. Evidence gaps are not software permission gates.

## 1. The answer — yes, but the intersections are different

The strongest intersection is **steady-state buffer coordination and its
acceptance evidence**. The client can want to play while waiting, the server
can hold production, and both can report data available without frames moving.
This is the behavior the explicit-demand protocol was intended to coordinate.
The protocol is implemented and active; the existence of its messages does not
prove that the resulting feedback loop always makes progress.

The unfinished cleanup/ownership work matters because several client recovery
entry points and status polling remain. Prepared handoff gaps matter when
recovering or changing recipe: they can turn a disturbance into a visible
replacement delay. Cluster handoff matters when an owner dies or drains.
Subtitle and marker work matter at their own transitions. None of those is
established as the cause of every ordinary-play freeze.

**Do not wait for the entire rewrite to finish before fixing the demonstrated
buffering case. Do not assume finishing the entire rewrite will fix it.**
Use the lifecycle map to reproduce the failing transition, correct its owning
policy, and prove that it stays fixed through refill, pause, seek and restart.

## 2. Work that already landed must not be rebuilt

| Work | Source-backed state at this baseline | Correction to older prose |
|---|---|---|
| M1–M3 protocol, client reporting, actor and explicit leases | [Control source](../../crates/plurxd/src/playback_control.rs) and all three client implementations exist; tonight's Apple trace reports explicit demand | This was implemented; lack of a protocol is not tonight's explanation |
| M4 server recovery ownership | Actor-managed producer progress, physical flow acknowledgement, publication and terminal handling exist | Server watchdog removal landed; remaining lifecycle timers are not proof that the old producer watchdog pile returned |
| M5 client consultation | [Apple controller](../../clients/apple/Sources/PlayerController.swift) asks for and applies server verdicts before bounded recovery; web and Android have corresponding paths | M5's revised scope is built. M5c/M5h budget deletion was explicitly struck; it is not an uncompleted instruction to delete safety bounds |
| M5.5 staged generations | [Store contracts](../../crates/plurx-core/tests/store_contract.rs) exercise preparation, commit, abort, expiry and predecessor accounting | The staged-row foundation is built; a row alone never proved playable successor media |
| M6 server reservation and priming | Forgejo #203 merged as `9a765bff`; [HLS source](../../crates/plurxd/src/http/hls.rs) has `stage_and_prime_prepared_successor`, `stage_prepared_successor_with_prime`, and committed publication/retirement handling | “Stage only,” “successor always 503,” and “prime merge pending” are obsolete descriptions of the general implementation |
| M6 clients and enablement | Apple, Android and web prepared adapters are present; settings permit default-on use, subject to current capability and admission checks | Historical platform-wide `false` capability literals are not the current default policy. A default-on checkbox still does not prove physical continuity |
| M7 subtitle windows and seek coalescing | Readiness, bounded windows, directed subtitle retry, per-playback extraction ownership and burn-join work landed | Old lower status tables still list subtitle windows/seek coalescing as missing; do not rebuild them |
| VOD marker destination prewarm | [VOD source](../../crates/plurxd/src/vodserve.rs) calls `apply_marker_prewarm_control`, dispatches subordinate production, and credits actual publication; `stored_marker_prewarm_is_subordinate_and_hits_only_its_own_production` tests it. Merge `1a64fa8a` contains the feature | The blanket “metric without a consumer” statement is stale. This verifies the VOD consumer, not equivalent rolling prewarm or every subtitle-warming extension |
| M9 initial activation | Default-on advertisement/prepared settings already landed | “M9 not started” is too broad. Final compatibility deletion and acceptance remain unfinished |
| Decoder selection/recovery implementation | [Decoder ledger](../DECODER_SELECTION_RECOVERY_STATUS.md) records the merged implementation and #203 priming completion | Decoder M8 is a separate fleet-evidence milestone; it is not playback-control M8 cluster handoff |

**How to read it:** “landed” describes source and merge history, not that a
specific installed client/server pair passed the real-device matrix. The
client build on the reported Apple TV was not established by the retained
AVPlayer event. Server build was independently checked on m6, nuc4 and nynuc.

## 3. The unfinished-work register

“Implementation gap” means missing or restricted behavior. “Evidence gap”
means implementation exists but the specified acceptance is not established.
“Reconciliation” means conflicting plans need an explicit current disposition.
Intersection describes a mechanism to test, not a causal verdict.

| ID | Remainder / kind | Intersection with this issue | Lifecycle cases |
|---|---|---|---|
| R01 | Closed-loop refill/pacing liveness — demonstrated behavior defect and evidence gap | **Direct.** A waiting player and held producer need a reachable route back to presentation, even while film position is fixed | L03–L11, L13 |
| R02 | Final recovery ownership and `/status` migration — M9 implementation/reconciliation | **Direct architectural overlap.** Client polling, observation, verdict deferral and local reopen must agree on one episode and current intent; their presence alone is not proof of conflicting actions | L09–L17, L23–L30 |
| R03 | Prepared replacement physical acceptance — M6 evidence gap | **Conditional.** Poor successor readiness or first-frame continuity adds delay to recovery/quality changes; not required to explain a self-recovering same-session wait | L18–L24 |
| R04 | Prepared recipe/device/headroom coverage — M6 implementation and evidence gap | **Conditional.** Unsupported combinations fall back to more disruptive changes; duplicate pipelines can contend for bandwidth/decoder capacity | L18–L24, L32 |
| R05 | Prepared planned node drain and complete failover — playback-control M8 implementation/evidence gap | **Conditional on owner transition.** Determines whether node loss/drain consumes remaining buffer smoothly or forces reopen | L29–L32 |
| R06 | Physical subtitle readiness/retry and seek evidence — M7 evidence gap | **Conditional on subtitle/seek activity.** Extra production and track readiness can interact with video refill, but missing captions alone do not prove video starvation | L07, L15–L17, L24 |
| R07 | Mixed-fleet and two-engine acceptance; live fallback retirement — M9 evidence/product decision | **Direct coverage overlap.** Tests must force and assert VOD versus rolling; healthy VOD does not qualify the fallback path | All applicable L01–L33 |
| R08 | Joined transition observability and native alternate-ingress proof — M2/M9 evidence and targeted instrumentation gap | **Direct diagnostic overlap.** Needed to identify which buffer boundary stalled; control success is not media success | L05–L11, L29–L31 |
| R09 | Broader decoder fleet qualification — adjacent decoder M8 evidence | **Conditional.** Helps distinguish loaded-but-undecodable media from supply/refill waits across codecs and hardware | L03–L11, L23–L24 |
| R10 | Semantic detection and remaining prewarm scope — deferred research/scope reconciliation | **Low for ordinary play.** Can affect automatic skip destinations and seek latency; no evidence it caused tonight's normal-play waits | L15–L17, L24 |

### 3.1 R01 — close the buffer loop before adding recovery

This is not simply an unchecked original milestone; it is a concrete
acceptance gap exposed after the foundations landed. The
[lifecycle map, §3](PLAYBACK-LIFECYCLE-COVERAGE.md#3-every-buffer-boundary-needs-a-demand-and-progress-contract)
identifies source reads, producer output, complete publication, HTTP transfer,
loaded ranges, decoder readiness and presented frames as separate boundaries.

Current rolling pacing computes its time target from client runway plus
intended-rate reserve; the held release threshold has hysteresis. AVPlayer
also decides when enough media is usable to resume. The two policies must not
wait for each other's progress indefinitely. VOD has a different upstream
mechanism—materialization and working-set/read-window demand—but converges on
the same loader, decoder and render behavior.

**Required closure:** a coupled regression drives the real flow/materialization
policy and controller/loader transitions with the playhead held still, for both
empty and substantial loaded buffers. Show where new usable media comes from,
why any hold releases, and what proves first resumed presentation. Then run
that case on Apple TV on both forced paths. Fix the owning condition, not a new
independent restart timer. A timed reopen may bound failure, but cannot pass
healthy steady-play acceptance.

### 3.2 R02 — finish ownership deliberately, preserving necessary bounds

The Apple source still calls `startStatusPolling` and
`startPlaybackRecoveryMonitor`, observes delivery starvation, and handles
`retrySameDeliveryAfterStall` through server consultation and bounded reopen.
These are active paths, not merely unused historical types. The original M9
contract requires migration away from old polling/recovery code.

However, **detector count is not action-owner count**. Some objects report
facts; others serialize operations or bound retries. The M5 record explicitly
struck deletion of budgets that still bound server deferral and client-local
failure. Removing those because an old plan says “three timers” could restore
unbounded waiting.

**Required closure:** inventory every live recovery entry point on all three
clients, identify the component allowed to mutate the player/session, and join
its evidence to one current episode/intent. Move remaining status-only facts
into the supported observation path before removing their poll. Prove no
competing reopen, no stale callback after stop, and no reset of the stall clock
by an unrelated heartbeat. Reconcile the original M9 wording with the accepted
M5 scope instead of silently weakening either contract.

### 3.3 R03–R04 — complete the prepared-handoff promise, not just its transaction

Server prime is implemented. What remains is the complete continuity claim:
real successor buffer, measured first frame, correct film origin, preservation
of pause/tracks/grade, and safe cleanup on refusal, expiry and failed commit.
The old [Apple hardware handoff](M6-APPLE-HARDWARE-ACCEPTANCE.md) still describes
a pre-priming server; its procedure needs that premise corrected before reuse.
The receipt must distinguish an actual prepared switch from a fallback reopen.

Current `PREPARED_AXIS_SETS` in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs) admits only:

- resolution/bitrate changes; and
- resolution/bitrate plus delivery-method change, subject to the bounded
  successor-rate direction check (`server_selected`).

Audio/offset, subtitle-burn, dynamic-range changes, and other combinations are
not generally admitted as prepared transitions by this decision function.
The reverse transition toward an unbounded source bitrate has a separate
restriction. The headroom check requires reported observed throughput at
least twice the predecessor's measured delivered rate; missing measurements
refuse preparation. This is a conservative floor, not a measurement of the
successor's actual buffering cost or every device's decoder capacity.

**Required closure:** document each allowed recipe/device/direction and its
measured limits; exercise both simultaneous streams on a constrained link and
under occupied hardware capacity. Keep unsupported changes explicit. The
roadmap's one-slot software-bridge/buffered-break-before-make strategy is not
established by finding reserve/commit functions; account for it separately
before claiming the general one-slot contract. Refusal must preserve useful
predecessor playback and the fallback must preserve viewer intent.

### 3.4 R05 — distinguish stopping a node from transferring its playback

Playback-control M8 calls for prepared planned drain, owner-loss recovery and
fencing across the entire transaction. Existing drain/fence/status and
compatibility takeover mechanisms are useful foundations; they do not by
themselves prove “reserve and prime elsewhere while the old stream plays.”
The historical ledger explicitly identifies prepared planned drain as unbuilt;
this audit found no corresponding node-replacement orchestration in the
inspected control/HLS/session path.

**Required closure:** establish the target-node placement → reservation →
readable successor → client switch → old-owner retirement path. Kill or isolate
the owner during each phase and verify one durable result, no old-owner
resurrection, and buffer continuity or a measured bounded reopen. Preserve the
existing rule against splicing unrelated production into a VOD/EVENT URL.
Test no-snapshot recovery separately from recovery with a current control
snapshot. Do not attribute tonight's freeze to M8 without an owner/epoch change
in the same episode; the retained trace does not establish one.

### 3.5 R06–R10 — close evidence and scope without reopening finished projects

**Subtitle/seek:** implementation of readiness, windows, directed retry and
coalescing is already present. Capture real web/Apple/Android evidence that
captions become ready without restarting video, that abandoned seeks do not
leave extra production, and that subtitle work cannot deprive the playhead of
resources. Burn-join state support does not imply every burn-change recipe is
admitted as a seamless prepared transition.

**Two engines and cutover:** the retained live engine currently serves real
viewing. Do not delete it to make the architecture diagram simpler. First
measure the four typed fallback causes, close VOD eligibility gaps for the
supported catalog, and prove all applicable lifecycle transitions on both
engines. Final compatibility deletion is a separate supported-catalog and
rollback decision, after evidence—not a prerequisite to investigating R01.

**Observability/alternate ingress:** native retry code exists; physical retry
receipts and the association between a recovery and the snapshot provoking it
remain incomplete in the status record. Browser alternate ingress also needs
its own CORS/auth contract. Record supply restoration → first resumed frame,
with sample ages and timeline origins. Add only missing facts needed to explain
the observed transition; no new observer may acquire recovery authority.

**Decoder qualification:** the adjacent decoder project's implementation is
merged. Broader hardware, workload, false-positive, startup/concurrency and
three-client replacement evidence remains in
[DECODER-M8-HANDOFF.md](../streaming/DECODER-M8-HANDOFF.md). It can identify a
loaded-but-undecodable case, but neither a healthy producer nor a large loaded
range proves that diagnosis. Do not turn optional post-merge hardware evidence
into an invented pre-merge approval requirement.

**Markers/prewarm:** VOD prewarming is built and subordinate to active demand.
Verify coverage of rolling/subtitle-specific extensions before scheduling them
as missing; this audit does not establish their full scope. Automatic semantic
intro/credits detection remains separately deferred behind labeled-corpus and
false-positive evaluation. Exact authored/manual annotations are already
implemented. Neither detection nor extra prewarming is a prerequisite for
correct ordinary playback.

## 4. What tonight's trace does and does not connect

The [retained observation receipt](../evidence/playback-lifecycle-observation-2026-09-11.json)
records Ronny Chieng: Speakeasy on Apple AVPlayer, identified by the viewer as
Apple TV, with rolling/sliding HLS and explicit demand.

| Observed | Supported connection | Not established |
|---|---|---|
| Waiting with about 63.6 s or 22.6 s loaded and a pacing hold | R01/R02: “buffer available” is insufficient to decide whether the player can resume or should defer recovery | Which exact loader/decoder/track condition caused the initial wait |
| Near-empty buffer, unchanged film position, hold followed by reopen at the recorded 20 s ceiling | R01/R02: recovery is bounded but can still cost many visible seconds | That a new watchdog is needed, or that every freeze has this cause |
| First-frame report 5.704 s after one reopen | R03/R04 can affect recovery cost when the recovery path needs replacement | That this reopen was a prepared transaction, or that prime was missing |
| Several same-session self-recovered reports | Continuous-presentation acceptance remains unmet despite eventual recovery | A node handoff, quality change or subtitle replacement as the common trigger |
| This retained case used rolling HLS | R07 requires deliberate separate VOD acceptance | A root cause for the viewer's separately reported VOD failures |

Status is sampled, sometimes joined later, and not a synchronized trace of
every component. Retained zero-duration access-log reports mean unavailable
stall duration, not an absence of freezing. Treat this receipt as a reproducible
acceptance seed and avoid turning correlations into a causal conclusion.

## 5. Execution order and completion evidence

| Order | Work | Concrete exit evidence |
|---|---|---|
| 1 | R01 plus the minimal R08 instrumentation it requires | Reproduce empty-buffer and loaded-but-waiting cases; identify the blocked boundary; real coupled regression and Apple TV replay on both engines show resumed presentation without healthy-play reopen |
| 2 | R02 and lifecycle race coverage | One current operation owns each mutation; pause/seek/stop supersedes recovery correctly; stale reports cannot revive old work; any removed poll has equivalent observation coverage |
| 3 | R03/R04 | Forced prepared and forced fallback runs on each client family; first-frame/buffer/resource receipts; explicit recipe/device/one-slot coverage and measured interruption |
| 4 | R05, plus R06/R09 where their transitions apply | Multi-node phase-failure receipts; subtitle/decoder physical runs preserve position, selections, grade and bounded resource use |
| 5 | R07 and final M9 reconciliation/deletion | Full applicable lifecycle matrix, supported-catalog eligibility/fallback accounting, mixed-fleet evidence, and an explicit disposition for every remaining compatibility mechanism |
| Separate | R10 | Scope decision for residual prewarm work and research acceptance for semantic detection; no dependency imposed on the steady-play repair |

This order focuses the freeze investigation without abandoning the rest of
the rewrite. It maps directly to G1–G5 in the lifecycle coverage document.
Every receipt must name exact server/client builds, selected engine, state
transition, injected fault if any, buffer progress, first resumed frame and
resource cleanup. A green compiler or a passing local policy test is not a
physical continuity receipt.

**Non-goals:** adding a watchdog; globally increasing buffers without locating
the blocked boundary; deleting the live fallback before catalog coverage;
rebuilding already merged priming/prewarm; broadening prepared axes by changing
a capability boolean; or treating this audit as a deployment/acceptance claim.
