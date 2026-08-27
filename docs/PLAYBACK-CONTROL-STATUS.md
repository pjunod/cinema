# Playback control rewrite — project status

**Updated:** 2026-08-26
**Merged baseline:** `origin/main` at `5908e838` (PR #614)
**Current work:** `codex/playback-control-m3-delivery-ledger` — M3c1 passive,
attempt-fenced publication/fetch ledger; seven exact-head adversarial reviews
have driven production-build, validation-history, cancellation, path-ownership,
scratch accounting, bounded cleanup, terminal-cause, and exact-publication
corrections. The PR review's history-mapping finding is locally remediated and awaits
another exact-head review; no tests have run before that approval
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
| M3 — actor and explicit lease | **In progress** | Rolling generation has one bounded actor for control fencing, explicit demand/pacing, renewal, expiry claim, retirement fence, and response commit ownership | M3c playlist/fetch ledger, then progress, child-exit, end, and cluster-fence event ownership |
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

## Active slice: M3c1 delivery ledger

M3b merged as PR #614 at `5908e838`. It established the explicit 30-second
rolling lease, actor-owned demand/pacing policy, response EOF commit ownership,
hard byte safety bounds, and the Activity/control instrumentation described in
[`PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md`](PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md).

The active M3c1 slice is deliberately passive. It gives the actor one ordered,
attempt-fenced snapshot of playlist readiness, published frontier, next media
sequence, completed-fetch frontier, and a fetch whose `EXTINF` is still
pending. Initial and fallback children use attempts allocated by that actor;
playlist refreshes and HTTP response commits carry the exact attempt they
observed. A predecessor response completing after replacement therefore cannot
renew or advance its successor.

Existing `SegmentIndex`, `playlist_published`, `high_segment`, and
`fetched_end_ms` values remain compatibility projections for pruning, byte
accounting, legacy pacing, and old watchdogs. Rolling status reads the combined
actor snapshot when available. No process action or recovery policy is added
or removed; the existing pre-publication fallback must now obtain actor
admission before touching its predecessor. Its full ordering and rollback
contract is in
[`PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md`](PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md).

Review and test state for M3c1:

| Gate | State |
|---|---|
| Implementation | `fe73efaa`, `fbae4eb9`, `9e0f6c4a`, `af4b8e35`, `2fbb8a4b`, `7c375425`, `3470a0f5`, `298901be`, `15dfea39`, and compile repair `089cb102` on `codex/playback-control-m3-delivery-ledger` from merged `5908e838` |
| Adversarial diff review | Reviews 1–4 drove production-build, history-mapping, ordering, cancellation, retention, exact-publication, and false→true→false path-ownership corrections. Review 5 found the actor-mutation/reply cancellation gap, silent predecessor cleanup failures, detached-garbage under-accounting, unbounded per-session cleanup workers, and final actor rejections mislabeled as cancellation. Review 6 found that one permanent unlink failure could strand an otherwise deletable cleanup tail. PR review 7 found the automatic history policy requires explicit mappings for corrective runtime commits even when their tests are inline. The local remediation marks admission pending before the await, verifies scratch clearing, keeps only genuinely failed garbage charged/retryable behind one coalesced owner, attempts every item once per repair pass, preserves typed terminal causality, adds deterministic race/storage/accounting tests, and maps both corrective heads to the retained Rust gate. Exact-head re-review is pending and no tests will run first. |
| Unit/focused tests | Began only after exact PR-head approval. The first focused Rust invocation reached compilation and exposed non-`Send` guard lifetimes, three stale replacement tuple call sites, and two test-module compile defects before any test executed. The first reviewed repair cleared all but the install-rejection guard lifetime; a synchronous kill plus detached reap repair now awaits review before the next rerun. |
| Full local gate | Not run yet |
| Hosted CI | PR #615 opened; automatic `history-check` preflight failed only for missing corrective-commit mappings, now remediated locally before any unit run |
| Merge | Pending implementation, review, tests, PR, and hosted green gate |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

### Server mechanisms still present on merged `main`

| Mechanism | What it currently does | Removal owner |
|---|---|---|
| first-segment/software grace and progress watcher | Polls startup/progress and can replace or kill the encoder | M3c2 feeds progress/exit observations to the actor; M4 replaces this watcher with one actor-owned `ProducerProgressDeadline` |
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

## Remaining delivery order

1. Finish M3c1's passive delivery ledger, adversarially review the exact head,
   then run/fix all tests and merge only when hosted CI is green.
2. Add nonblocking, coalesced producer progress and exact-attempt child-exit
   observations in M3c2, then finish end and cluster-fence event ownership with
   model tests over event reorderings.
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
