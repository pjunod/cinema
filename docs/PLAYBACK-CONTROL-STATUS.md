# Playback control rewrite — project status

**Updated:** 2026-08-26
**Merged baseline:** `origin/main` at `8fca4ab2` (PR #611)
**Current work:** `codex/playback-control-m3-demand-lease` — M3b, pushed branch,
draft PR [#612](https://github.com/pjunod/plurx/pull/612), review corrections in progress
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
| M3 — actor and explicit lease | **In progress** | Rolling generation has one bounded actor for control fencing, demand snapshot, renewal, expiry claim, and retirement fence | M3b explicit lease/demand pacing; then playlist, segment, progress, child-exit, end, and cluster-fence event ownership |
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

## Active slice: M3b explicit demand lease

The active branch is pushed as draft PR #612. Adversarial review has driven
remediation across response ownership/commit ordering, same-id VOD
reattachment, exact snapshot and physical-signal timing, cancellation-safe
producer convergence, full-object Range semantics, overlay truth, Activity
instrumentation, and this ledger. The current work carries an opaque
engine/incarnation token through every response; commits streamed
lease/frontier state only at successful EOF; queues producer work outside the
response body; and names VOD/rolling ownership correctly. A fresh review of
the final committed remediation still precedes every test command. The
slice's acceptance boundary is intentionally smaller than the whole actor
migration; the exact behavior
and timer ledger are in
[`PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md`](PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md):

- explicit rolling sessions use a 30-second lease while legacy media-only
  sessions retain the 60-second compatibility lease;
- an expired explicit lease cannot be revived between repair-loop ticks;
- `hold` stops production immediately while accepted control or media
  renewals keep the session alive;
- `active` production follows the actor-owned reported playhead/runway state;
- per-session byte and fleet scratch caps remain hard safety bounds even when
  client observations are malicious or wrong;
- `end` stops production in this slice, but durable terminal/tombstone
  semantics are not claimed until rolling, relay, and VOD end ordering share
  one implementation; and
- status and events explain lease mode, demand, production frontier, and the
  exact reason for every hold/resume transition.

M3b temporarily adds one short process-local producer-transition fence so the
actor's exact timer and a physical SIGSTOP/SIGCONT call have a strict order.
It is never held across an await and is deleted with `child_transition` when
M4 moves child ownership into the actor; it is not a watchdog or another
recovery owner.

Review and test state for M3b:

| Gate | State |
|---|---|
| Implementation | Formal `1d229162` findings remediated on the branch; exact-head re-review pending |
| Adversarial diff review | **Changes requested** at `1d229162`; fresh review required on the remediation head |
| Unit/focused tests | Not run — intentionally waits for second-review approval |
| Full local gate | Not run |
| Hosted CI | Draft PR exists; local test gate remains closed until adversarial approval |
| Merge | Pending all gates |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

### Server mechanisms still present on merged `main`

| Mechanism | What it currently does | Removal owner |
|---|---|---|
| first-segment/software grace and progress watcher | Polls startup/progress and can replace or kill the encoder | M4 replaces it with one actor-owned `ProducerProgressDeadline` |
| `child_transition`, `watchdog_active`, `replacing_child` | Serializes and masks the old in-place replacement paths | M4 deletes them after every child event/action enters the actor |
| playlist and live-segment wait budgets | Bound an HTTP request waiting for publication | M3 routes waiters through actor state; bounded HTTP deadlines remain by design |
| 15-second repair/flow-control tick | Refreshes indexes, prunes retention, records speed, evaluates flow, and claims lease expiry | Scheduling remains; recovery decisions move to actor events/deadlines |
| fetched-frontier ahead-window inference | Suspends/resumes production based on download behavior | M3b replaces the time policy only after a session enters explicit mode; legacy pacing and byte/disk safety remain |
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

## Remaining delivery order

1. Finish M3b, adversarially review the exact PR diff, then run/fix all tests
   and merge only when hosted CI is green.
2. Finish M3 event ownership for playlist, segment, progress, child exit, end,
   and cluster fences, including model tests over event reorderings.
3. Complete Apple and Android M2 reporters and record timer/alternate-ingress
   behavior.
4. Complete M4 and prove the old server watchdog/replacement symbols are gone.
5. Complete M5/M5.5/M6 so quality, codec, dynamic range, tracks, subtitles, and
   node placement use the same prepare/commit/abort transaction.
6. Complete semantic indexing: exact intro/credits destinations with
   provenance/confidence, manual overrides, subtitle readiness, queue metrics,
   and marker-destination prewarm.
7. Complete clustered planned/hard handoff, mixed-fleet cutover, compatibility
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
