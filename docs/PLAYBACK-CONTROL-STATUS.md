# Playback control rewrite — project status

**Updated:** 2026-08-27
**Merged baseline:** `origin/main` at `8e331672` (PR #621)
**Current work:** PR #624 on `codex/playback-control-m4-command-sequencing`;
formal round 5 unanimously approved implementation head `dc7c7667`. All 75
focused tests passed before the final behavior-neutral lint repair, and the
complete `make check` and `make cluster-check` gates then passed at the approved
head. Hosted CI and the final evidence-only exact-head review remain before
merge.
PR #621 merged as `8e331672` after exact-head round 10 received unanimous
**APPROVE** verdicts and hosted run `33109297301` passed. The active branch
adds bounded shared command/producer sequencing, publication-time ordering,
stale-exit fencing, a partial observation-only actor projection in
`SessionInfo` and telemetry, and bounded command/deadline metrics. Runtime is
committed at `383d62d8`, with regression mapping at `de23104d`. These are
groundwork only: legacy watchdogs and recovery actions remain active, and there is no
decision, executor, response-admission, or watchdog-deletion cutover yet.
The active runtime head has received its required exact-head adversarial review
before unit and full gates were run.
The detailed M4 contract is in
[`PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).
The resumable execution state is in
[`PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md`](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md).
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
| M3 — actor and explicit lease | **Complete** | Rolling generation has one bounded actor for control fencing, explicit demand/pacing, renewal, expiry claim, typed End/authority-fence ownership, durable terminal replay, response commit ownership, the attempt-fenced delivery ledger, nonblocking progress/exact-exit observations, and exhaustive event-order evidence | M4 now moves recovery decisions through that owner |
| M4 — server watchdog removal | **In progress** | Contract merged in #618; cutoff-safe deadline-chain ingress and checked legacy-owner inventory merged in #619; PR #621 merged as `8e331672`; PR #624 implementation head `dc7c7667` has unanimous round-5 approval and green focused/full/cluster gates for bounded shared command/producer ordering, publication-time ordering, stale-exit fencing, partial observation-only projection, and bounded command/deadline metrics | Finish hosted CI and merge #624; then move decisions, response admission, retries, cleanup, and process actions behind the actor/executor before deleting detached recovery loops, in-place fallback, and transition locks/atomics |
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
| [#617](https://github.com/pjunod/plurx/pull/617) | M3c3 typed actor-owned End, authority fence, and lease expiry; atomic durable terminal acknowledgement and exact replay; cancellation-safe settlement and exhaustive event ordering | Exact-head adversarial approval at `6f127463`; `make check`, `make cluster-check`, and every hosted job green; merged as `9cd16d05` |
| [#618](https://github.com/pjunod/plurx/pull/618) | M4 watchdog-removal ownership, deadline ordering, executor, cleanup, process-capacity, and post-publication proposal contract | Seven exact-head adversarial passes resolved 31 findings; final approval at `96799e60`; `make check`, `make cluster-check`, and hosted PR gate green; merged as `f9cef83b` |
| [#619](https://github.com/pjunod/plurx/pull/619) | Constant-space cutoff-safe producer progress coverage plus checked whole-module legacy owner/task/timer/process inventory | Thirteen exact-head reviews; final approval at `11315987`; focused tests, `make check`, `make cluster-check`, and hosted PR gate green; merged as `48ea494c` |
| [#621](https://github.com/pjunod/plurx/pull/621) | Passive producer deadline, cutoff-safe ingress/fencing, and process-flow barriers | Ten exact-head rounds ended unanimously **APPROVED**; hosted run `33109297301` green; merged as `8e331672` |

## Active slice: M4 watchdog removal

PR #618 merged the complete M4 implementation contract at `f9cef83b`, after
seven adversarial passes and green local, cluster, and hosted gates. The actor
already owns explicit End, authority fence, and exact lease expiry; this slice
starts moving producer recovery evidence through that same owner.

PR #621 then merged the passive deadline and cutoff-safe ingress foundation as
`8e331672`, after unanimous round-10 approval and hosted run `33109297301`.
The active branch, `codex/playback-control-m4-command-sequencing`, has passed
five formal exact-head review rounds plus its focused, full-workspace, and
cluster gates. It adds bounded shared
command/producer sequencing, publication-time ordering, stale-exit fencing,
partial observation-only actor projection in `SessionInfo` and telemetry, and
bounded command/deadline metrics. It does not yet move decisions, response admission,
retry, cleanup, or process actions behind an executor, and it does not delete
the legacy watchdogs or action owners.

M4 makes the same actor the sole recovery decision owner. It replaces hardware
startup grace, software startup/lifetime polling, and copy-segmenter fallback
with one exact-attempt `ProducerProgressDeadline`. One pre-publication retry is
allowed only when the actor supplies the validated recipe. After publication,
failure retains published bytes and produces one stable internal replacement
proposal; it never swaps the child in place or emits an incomplete wire action.
The existing exact-attempt supervisor remains the sole OS-child/PID owner, and
a single session executor orchestrates actor-authorized proxy changes. The full
contract is
[`PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).

Review and test state for M4:

| Gate | State |
|---|---|
| Implementation contract | **Merged in #618.** It defines deadline policy, contiguous cutoff-safe ingress with command and producer-event barriers, arm/disarm and event ordering, exhaustive action-timeout settlement, the one-retry invariant, hard rolling-process admission, process-executor ownership, publication-aware cleanup, post-publication proposal behavior, instrumentation, source ownership checks, and the race/failure matrix. |
| Static owner inventory | **Merged in #619.** `tests/playback/rolling-producer-owners.toml` exact-counts the known recovery/election/replacement owners and scans every Rust module under `plurxd/src`; the current branch adds the one named actor deadline while leaving every legacy count unchanged. |
| Progress ingress | **Merged in #619.** `ProgressCoverageBatch` retains first, covered tail/deadline, first gap, latest progress, and latest telemetry with a persistent exact-attempt watermark and exit barrier. The local actor consumes that proof without flattening away publication time. |
| Passive deadline/cutoff | **Merged in #621; active branch extends the foundation.** The merged slice folds producer facts under transition plus ingress, uses fenced publication timestamps, classifies non-success exits immediately, preserves progress around bounded sequenced physical-flow barriers, fails closed when the actor/mailbox disappears, serializes actor-task exit fencing with producer transitions, and re-authorizes exact attempts before full-capacity STOP/CONT syscalls. The active branch adds reviewed and locally validated shared command/producer ordering and publication-time barriers. It still emits no recovery action. |
| Operational projection | **Partial and observation-only.** The active branch exposes bounded actor producer truth through `RollingLeaseSnapshot`, `SessionInfo`, and joined telemetry, including deadline/due, process-exit, physical-flow, and last-applied-sequence observations. It does not expose a decision, action, retry, executor, or response-admission verdict. The bounded deadline and command metrics are instrumentation, not recovery authority. |
| Adversarial implementation review | **Round 5 unanimously approved `dc7c7667`.** Rounds 1–3 resolved two stale P3 comments, inventory counts, and history metadata; round 4 approved the lexical `MutexGuard` scope repair; round 5 approved the behavior-neutral Clippy cleanup. All three reviewers returned **APPROVE** with no P1/P2/P3 finding. |
| Unit/focused tests | **Green.** All 72 rolling-control tests plus the three changed status/telemetry tests passed before the lint-only repair. The complete workspace suite subsequently passed at approved head `dc7c7667`, covering the same tests after that repair. |
| Full/cluster/hosted gates | **Local full and cluster gates green at `dc7c7667`; hosted pending.** `make check` passed catalog/history, 173 Python policy/contract tests, formatting, Clippy with warnings denied, and every workspace/doc test. `make cluster-check` passed vendor snapshot/WAL recovery, 63 serialized clustered store contracts with two helper processes ignored, live failure/topology drills, and all nine daemon cluster-activation/activity tests. Hosted CI and final evidence-only exact-head confirmation remain. |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

The active command-sequencing branch has not reached that target: every legacy
watchdog, replacement path, action owner, and response-admission compatibility
path remains active. The new sequencing and projection are observational
foundations; they do not authorize a retry, kill, replacement, cleanup, or
transparent handoff.

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

1. Adversarially review and test the active command-sequencing branch, then
   complete M4 and prove the old server watchdog/replacement symbols are gone.
2. Complete Apple and Android M2 reporters and record timer/alternate-ingress
   behavior.
3. Complete M5/M5.5/M6 so quality, codec, dynamic range, tracks, subtitles, and
   node placement use the same prepare/commit/abort transaction.
4. Complete semantic indexing: exact intro/credits destinations with
   provenance/confidence, manual overrides, subtitle readiness, queue metrics,
   and marker-destination prewarm.
5. Complete clustered planned/hard handoff, mixed-fleet cutover, compatibility
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
