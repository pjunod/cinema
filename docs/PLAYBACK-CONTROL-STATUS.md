# Playback control rewrite — project status

**Updated:** 2026-08-27
**Merged baseline:** `origin/main` at `8c6ccdf7` (PR #616)
**Current work:** PR [#617](https://github.com/pjunod/plurx/pull/617),
`codex/playback-control-m3-terminal-events` — M3c3 typed, actor-owned session
end and authority/cluster-fence events, replicated terminal acknowledgements,
and an exhaustive event-order model.
This is the final behavior-passive M3 ownership slice before M4 moves recovery
decisions into the actor and deletes the old server watchdog and in-place
replacement machinery.
**Source of truth:** this page tracks delivery; the design and acceptance
contracts remain in
[`PLAYBACK-CONTROL-PROTOCOL-PLAN.md`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md).

This is the canonical done-versus-left ledger for the playback-control
rewrite. Update it in every implementation PR. A feature listed as partial is
not being counted as complete merely because its foundation has landed.

## At a glance

| Workstream | State | Shipped result | Still required |
|---|---|---|---|
| Design and adversarial review | **Complete** | End-to-end protocol, actor, replacement, cluster, index, observability, and watchdog-deletion contracts | Re-review each implementation PR against the contract |
| M1 — typed control plane | **Complete** | Strict v1 messages, capability route, sequence and owner fencing, bounded cluster relay, default-off advertisement, metrics | Active actions remain deliberately disabled |
| M2 — passive clients | **Partial** | Web reports demand, playhead, contiguous runway, render state, selections, capabilities, and recovery evidence | Apple and Android reporters; alternate-ingress physical evidence |
| M3 — actor and explicit lease | **In progress** | Rolling generation has one bounded actor for control fencing, explicit demand/pacing, renewal, expiry claim, retirement fence, response commit ownership, the M3c1 attempt-fenced delivery ledger, and M3c2 nonblocking progress/exact-exit observations | Merge typed end and authority/cluster-fence ownership with exhaustive ordering and durable replay evidence |
| M4 — server watchdog removal | **Not started** | — | One producer deadline; remove detached recovery loops, in-place post-publication fallback, child-transition locks/atomics; add ownership check |
| M5 — one client action owner | **Not started** | — | Collapse web, Apple, and Android reopen/watchdog paths into one controller per platform |
| M5.5/M6 — prepared handoff and Auto | **Not started** | — | Staged generations and transactional resolution, bitrate, codec, HDR/Dolby Vision, audio, subtitle, and node changes |
| M7 — semantic indexes/subtitles | **Partial foundation** | Cluster-shared structural fragment index plus durable force-analysis queue and operator status page | Timeline annotations, exact intro/credits markers, feature sidecar, subtitle windows, seek coalescing, marker prewarm |
| M8 — cluster handoff | **Not started** | Control relay and owner fencing exist from M1 | Planned drain, VOD resurrection, rolling successor failover, compatibility-takeover retirement |
| M9 — cutover/deletion | **Not started** | — | Mixed-fleet evidence, defaults on, compatibility engine and `/status` polling deleted, physical matrix green |

## Merged evidence

| PR | Merged result | Verification at merge |
|---|---|---|
| [#600](https://github.com/pjunod/plurx/pull/600) | Detailed explicit playback-control design and independent adversarial finding ledger | Documentation review complete |
| [#602](https://github.com/pjunod/plurx/pull/602) | M1 fenced, behavior-neutral control plane and mutating cluster relay | Adversarial review, local gates, and hosted CI green |
| [#603](https://github.com/pjunod/plurx/pull/603) | Content-addressed VOD fragment-index work shared across cluster voters | Adversarial review, local gates, and hosted CI green |
| [#605](https://github.com/pjunod/plurx/pull/605) | M2 passive web control reporter and joined recovery evidence | Adversarial review, local gates, and hosted CI green |
| [#606](https://github.com/pjunod/plurx/pull/606) | Durable force-analysis requests, bounded queue/status API, Activity status UI | Merged with three red hosted jobs subsequently repaired by #610 |
| [#610](https://github.com/pjunod/plurx/pull/610) | Fixed all three #606 merge-gate failures and added the missing ops contract test | Adversarial review, local gates, and hosted CI green |
| [#611](https://github.com/pjunod/plurx/pull/611) | M3a bounded rolling lease actor and atomic expiry/retirement ownership | Adversarial approval at `dddc8d24`; full local gate and all hosted jobs green |
| [#613](https://github.com/pjunod/plurx/pull/613) | Preserved an empty Dolby Vision `hvcC` record when copying parameter sets upstream | Adversarial review, local gates, and hosted CI green |
| [#614](https://github.com/pjunod/plurx/pull/614) | M3b explicit demand lease, response-commit ownership, demand-based pacing, and operator instrumentation | Exact-head adversarial approval, full local gate, and all required hosted jobs green |
| [#615](https://github.com/pjunod/plurx/pull/615) | M3c1 actor-owned, exact-attempt publication/fetch ledger and fenced producer installation | Exact-head adversarial approval at `7dffa5f3`; full local gate and every required hosted job green; merged as `46c08439` |
| [#616](https://github.com/pjunod/plurx/pull/616) | M3c2 constant-space producer progress/exit ingress, exact-attempt actor facts, and one cancel-safe process supervisor | Exact-head adversarial approval at `4b818d39`; 12 focused tests, 1,927 full-workspace tests, every local gate, the long cluster gate, and all required hosted jobs green; merged as `8c6ccdf7` |

## Active slice: M3c3 terminal events

M3c2 merged as PR #616 at `8c6ccdf7`. It gives the actor nonblocking,
exact-attempt producer progress and process-exit facts without changing any
recovery action.

The active M3c3 slice replaces the actor's undifferentiated `Retire` command
with typed `End` and `AuthorityFence` events. Lease expiry is the third bounded
terminal cause. The first terminal event linearized by the actor wins once,
publishes the immediate serving fence once, and remains immutable while late
end, fence, control, media, publication, progress, and exit events receive
truthful terminal verdicts. A newly accepted protocol `demand=end` now enters
that same terminal transition atomically and can replay its exact terminal
acknowledgement after response loss. VOD applies the equivalent tombstone and
reader detach under its lifecycle gate. The exact accepted End response is
retained in replicated storage for a bounded 60-second idempotency window, so
rolling cleanup, VOD tombstoning, route settlement, client disconnect, and a
cluster relay cannot erase the reply. Physical rolling-child teardown remains
in the existing manager until M4.

Activity/status will expose the winning terminal cause and Prometheus will
count each bounded cause as `won` or `already_terminal`. An exhaustive small
model will explore permutations of producer exit, expiry, explicit end,
authority fence, publication, control acceptance, and replacement admission;
real async tests cover reply cancellation and concurrent terminal calls.
The full contract is in
[`PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md`](PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md).

Review and test state for M3c3:

| Gate | State |
|---|---|
| Implementation | **Revision in progress** on PR #617 from merged `8c6ccdf7`. The current revision adds an immutable replicated terminal-ack record, replay through settled rolling/VOD routes, terminal-aware cluster response validation, cancellation-independent owner execution and VOD reader cleanup, plus corrected production-path model assertions. |
| Adversarial diff review | **Changes requested** at `619765ab`: terminal relay responses were rejected as non-active; process-local replay did not survive route settlement; VOD detach could be cancelled with the request; two exhaustive-model assertions were incomplete; and the corrective commit lacked history mapping. All five findings are addressed in the working revision; a new exact-head review is required after it is committed. |
| Unit/focused tests | **Intentionally not run yet.** The required adversarial review comes first. |
| Full local gate | **Pending** exact-head adversarial approval and focused tests. |
| Hosted CI | The automatic pre-review run for `702cba62` had validation scope, policy/contract preflight, fast Rust, daemon-cluster, and WAL green. The run at `672c0421` failed history policy because the corrective runtime commit lacked an explicit regression mapping. The run at `fe367f18` passed preflight but found a test-only name-shadowing compile error in the fast Rust gate; a compile-only local check reproduced it and `619765ab` corrected it. The `619765ab` run then failed history preflight because that corrective commit was not yet mapped. The working revision maps both corrective commits. These automatic runs are recorded separately and are not post-review local evidence. |
| Merge | **Not ready.** |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

### Server mechanisms still present on merged `main`

| Mechanism | What it currently does | Removal owner |
|---|---|---|
| first-segment/software grace and progress watcher | Polls startup/progress and can replace or kill the encoder; it no longer needs to be the source of process-exit truth after M3c2 | M4 replaces this watcher with one actor-owned `ProducerProgressDeadline`; the exact-attempt process supervisor remains as event delivery, not a watchdog |
| `child_transition`, `watchdog_active`, `replacing_child` | Serializes and masks the old in-place replacement paths | M4 deletes them after every child event/action enters the actor |
| playlist and live-segment wait budgets | Bound an HTTP request waiting for publication | M3c1 records publication in actor state; later M3 routes waiters through it; bounded HTTP deadlines remain by design |
| 15-second repair/flow-control tick | Refreshes indexes, prunes retention, records speed, evaluates flow, and claims lease expiry | Scheduling remains; M3c1 copies publication/fetch facts into the actor but recovery decisions still move in M4 |
| `SegmentIndex`, `playlist_published`, `high_segment`, `fetched_end_ms` | Catalogs rolling files and feeds pruning, byte accounting, pacing, and status | M3c1 makes actor delivery state authoritative for status while retaining these as action-path compatibility projections until M4 |
| fetched-frontier ahead-window inference | Suspends/resumes production based on download behavior | M3b replaced the time policy only after a session enters explicit mode; legacy pacing and byte/disk safety remain |
| VOD segment materialization deadline | Bounds demand for an immutable segment that is not ready | Remains permanently as one of the three approved progress deadlines |

### Client recovery owners still present

| Client | Remaining independent recovery paths | Removal owner |
|---|---|---|
| Web | persistent-wait/startup/seek timers, hls.js fatal handlers, decode rescue, Auto rung replacement, truncated-end reopen, `/status` polling | M5 makes the controller the only action owner; M6 moves Auto to prepared actions; M9 deletes old polling |
| Apple | status polling, starvation/stall/black-frame monitors, reopen budgets/queue, early-end and item-failure handlers | Apple M2 reporter, then M5 controller cutover |
| Android | status polling, buffering stall tracker, stall guards/budgets, direct reopen coordinator, error/end compatibility handlers | Android M2 reporter, then M5 controller cutover |

After cutover, only these playback progress deadlines may remain:

1. server `ProducerProgressDeadline` for a producer that cannot report its own
   wedge or death;
2. client `PlaybackProgressDeadline` because only the playback framework knows
   whether decoded media is rendering; and
3. VOD `SegmentMaterializationDeadline` because an HTTP request cannot wait
   forever for a demanded immutable object.

Lease expiry, cluster owner expiry, preparation expiry, retirement grace,
HTTP/relay limits, admission waits, and control rate windows also remain, but
they are named lifecycle/network bounds rather than competing playback
recovery watchdogs.

The 60-second terminal-ack retention window introduced by M3c3 is likewise an
idempotency bound: it retains one immutable accepted reply and triggers no
playback, replacement, restart, or failure decision. It is not a watchdog.

## Remaining delivery order

1. Finish end and cluster-fence event ownership with model tests over event
   reorderings.
2. Complete Apple and Android M2 reporters and record timer/alternate-ingress
   behavior.
3. Complete M4 and prove the old server watchdog/replacement symbols are gone.
4. Complete M5/M5.5/M6 so quality, codec, dynamic range, tracks, subtitles, and
   node placement use the same prepare/commit/abort transaction.
5. Complete semantic indexing: exact intro/credits destinations with
   provenance/confidence, manual overrides, subtitle readiness, queue metrics,
   and marker-destination prewarm.
6. Complete clustered planned/hard handoff, mixed-fleet cutover, compatibility
   deletion, playback-lab fault injection, and physical web/Apple/Android
   acceptance.

## Definition of finished

This project is finished only when M9 acceptance passes: repository checks
find only the three approved progress deadlines and named lifecycle timers;
no recovery path outside the actor/controller can replace media; transparent
prepared handoffs cover the supported recipe axes and cluster ownership;
semantic analysis publishes exact skip destinations; and the automated plus
physical playback matrices are green. Merged foundations are not a substitute
for that exit condition.
