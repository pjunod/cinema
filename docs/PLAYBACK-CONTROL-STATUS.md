# Playback control rewrite — project status

**Updated:** 2026-08-27
**Merged baseline:** `origin/main` at `9063bb1e` (PR #626)
**Current work:** first actor-owned prepublication recovery cut on
`codex/playback-control-m4-prepublication` in the disposable clone. The
response-publication contract correction is committed at `43bd459d` after an
adversarial review found and resolved four race and classification defects.
The actor, HTTP admission, transcode executor, frozen-presentation facade,
cancellation settlement, first-media handoff, and checked ownership inventory
are now assembled. The working-tree adversarial passes found and repaired
deadline, failure-replay, retry-cutoff, cancellation, executor-loss,
metadata-commit, process-reap, registry-lock, typed-error, and subtitle-owner
defects. Independent actor, transcode, and HTTP reviewers now report no
remaining P1/P2/P3 finding in their targeted scopes. Formal exact-head review
of `ab3808b1` then rejected the combined candidate: normal prepublication
retirement released admissions before confirmed reap; the owner ledger missed
the new termination wrappers; response rejection could still become false
`404`; rolling ETags could collide across attempts; playlist reclassification
could restart the full wait budget; and HTTP/EOF settlement lacked its claimed
bound. Every finding is now repaired in the working tree. A follow-up moving
tree audit also found that playlist preparation could outlive its shared
deadline in actor observation, storage/registry waits, polling, and synchronous
flow control; those paths now share one outer absolute deadline, HTTP actor
observation is fail-closed, and consequential flow work is queued to the
session-owned worker. Formatting, whitespace, and the read-only mechanical
ownership recount are clean. The repaired tree still needs to be committed and
approved as one immutable exact head.
No unit test has run on this branch. The
full unit suite runs once, only after final exact-head adversarial approval; a
failure permits only the failed or directly affected tests to rerun. PR #626's runtime commits
`c04898e2`, `732d3442`, `91486148`, `ba3a504d`, and `4d0a0c0f` add a
behavior-neutral, one-slot immutable actor decision, a non-consuming poll
command, and one bounded passive executor inbox/task. Root static review
repaired a mailbox self-retention cycle before the first commit. Adversarial
rounds 1 through 3 found timer-only terminal wake, executor-loss visibility,
terminal-cause projection, compile/lint, test-scheduling, actor-loss
classification, terminal/receiver-loss races, stale executor-state overwrite,
and a four-variant poll-contract violation. The final defensive pass also made
the first terminal-or-lost settlement win in either order. The runtime head
received unanimous exact-head approval at `981ebb51`. The focused Rust suite
then passed 85/85; the ownership inventory exposed six stale normalized
counts/anchors and one warnings-as-errors risk. `73cd33e9` fixed the normalized
counts and warning; the rerun cleared five failures and proved the remaining
entrypoint row was invalid because that table is deliberately source-local to
`transcode.rs`. `9d100a04` removes the misplaced row while whole-module
sentinels continue to cover the transport. Targeted review approved that
repair and the inventory passed 7/7. `make check` then reached the history gate
and stopped because four corrective mapping commits were not themselves named
by their existing evidence records. After that reviewed repair, the rerun
passed catalog, history, 121 validation tests, and 52 additional tests, then
Clippy identified the intentionally retained strong transport owner as unread
outside tests. `f463d4d4` documents and allows that one lifetime-owner field.
The repair received targeted approval, the corrected handoff received final
static approval, and the final documentation corrections received exact-head
approval at `1ead3944`. `make check` and `make cluster-check` passed. Hosted
run `33127652859` then passed every selected job and the aggregate PR
validation gate on that reviewed implementation/status head. Final exact head
`62a4f535` passed hosted run `33129200705`; PR #626 merged as `9063bb1e`.
PR #624 merged as `dba35f98` after unanimous exact-head review, green local
`make check` and `make cluster-check`, and hosted run `33118301414`. Its bounded
shared command/producer sequencing, publication-time ordering, stale-exit
fencing, observation-only actor projection, and command/deadline metrics are
now the validated baseline. Legacy watchdogs and recovery actions remain
active. The merged decision transport emits no production decision and owns no
retry, cleanup, response-admission, or process action. The active cut is the
first one authorized to take those duties from the compatibility path.
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
| M4 — server watchdog removal | **In progress** | Merged contract, deadline-chain ingress, legacy-owner inventory, sequenced actor/producer ordering, and passive decision transport through PR #626; the active candidate assembles production response admission and actor-owned transcode startup recovery | Finish exact-head review, run the full unit suite once, satisfy cluster/hosted gates, and merge; later cuts remove published-lifetime and copy compatibility owners |
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
| [#624](https://github.com/pjunod/plurx/pull/624) | Bounded shared actor-command/producer sequencing and observation-only operational projection | Formal runtime round 5 unanimously approved; `make check`, `make cluster-check`, and hosted run `33118301414` green; merged as `dba35f98` |
| [#626](https://github.com/pjunod/plurx/pull/626) | Immutable actor decision slot, non-consuming poll contract, and bounded passive executor transport | Exact-head adversarial approval; focused Rust 85/85, ownership 7/7, `make check`, `make cluster-check`, and hosted run `33129200705` green; merged as `9063bb1e` |

## Active slice: M4 watchdog removal

PR #618 merged the complete M4 implementation contract at `f9cef83b`, after
seven adversarial passes and green local, cluster, and hosted gates. The actor
already owns explicit End, authority fence, and exact lease expiry; this slice
starts moving producer recovery evidence through that same owner.

PR #621 merged the passive deadline and cutoff-safe ingress foundation as
`8e331672`. PR #624 then merged bounded shared command/producer sequencing,
publication-time ordering, stale-exit fencing, partial observation-only actor
projection in `SessionInfo` and telemetry, and bounded command/deadline metrics
as `dba35f98`. PR #626 then merged the immutable typed decision contract,
non-consuming `PollProducerDecision`, one-slot actor pending value, and one
move-only executor inbox/task as `9063bb1e`. That merged transport remains
observation-only. The active branch binds it to production transcode startup:
the actor emits the first real decision, HTTP admission closes retry before
attempt bytes escape, and one executor applies the sole validated
prepublication retry.

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
| Static owner inventory | **Merged in #619 and extended in the active slice.** `tests/playback/rolling-producer-owners.toml` exact-counts the known recovery/election/replacement owners and scans every Rust module under `plurxd/src`. The active candidate records `FIRST_SEGMENT_GRACE` at zero, names the actor executor/response/cancellation/first-media owners, and truthfully retains published-lifetime and copy compatibility counts. A read-only static count pass matches every row. |
| Progress ingress | **Merged in #619.** `ProgressCoverageBatch` retains first, covered tail/deadline, first gap, latest progress, and latest telemetry with a persistent exact-attempt watermark and exit barrier. The local actor consumes that proof without flattening away publication time. |
| Passive deadline/cutoff | **Merged through #624.** The actor folds producer facts under transition plus ingress, uses fenced publication timestamps, classifies non-success exits immediately, preserves progress around bounded sequenced physical-flow barriers, fails closed when the actor/mailbox disappears, serializes actor-task exit fencing with producer transitions, and re-authorizes exact attempts before full-capacity STOP/CONT syscalls. It still emits no recovery action. |
| Operational projection | **Active candidate assembled.** The actor now exposes startup policy, contract fingerprint, response/retry cutoff, executor state, and immutable decision application. The transcode executor is the sole prepublication action owner; the first admitted attempt-media response synchronously transfers to the retained published-lifetime owner. |
| Adversarial implementation review | **Exact head `ab3808b1` rejected; repairs complete in the working tree, immutable review pending.** Earlier targeted passes repaired deadline/retry ordering, first-media cancellation and handoff, immediate-exit replay, executor loss, bounded failure cleanup, registry-lock scope, typed error reclassification, and subtitle owner fencing. Formal review then found one P1 admission-before-reap defect, five P2 lifecycle/HTTP/owner-ledger defects, and one stale-handoff P3. The repair pass also closed a later moving-tree audit of the full playlist deadline. A new exact head must receive independent approval before tests. |
| Unit/focused tests | **Gate closed; none run on this branch.** Per user direction there will be one full unit-suite run, on the final reviewed merge candidate. After a failure, only failed or directly affected tests may rerun. |
| Full/cluster/hosted gates | **Pending active-candidate review.** The merged #626 baseline is green. Cluster/integration and required hosted PR checks remain required, but no local unit suite runs during review. |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

Merged `main` has not reached that target: every legacy watchdog, replacement
path, action owner, and response-admission compatibility path remains active.
The active branch is replacing the prepublication transcode-start owner first.
Published-lifetime and copy recovery remain explicit later cuts and must not
overlap the actor-owned scope after their respective cutovers.

### Active-candidate watchdog disposition

| Mechanism | Active-candidate disposition | Why it remains, if retained |
|---|---|---|
| `FIRST_SEGMENT_GRACE` and the detached startup downgrade loop | **Removed from production.** `FIRST_SEGMENT_GRACE` has zero source owners, and `downgrade_one_step` is no longer a production recovery path. | The actor's one exact `ProducerProgressDeadline` and immutable retry decision replace them. |
| `SOFTWARE_GRACE` | **Retained outside actor-owned prepublication.** The first-media transfer starts the compatibility lifetime watcher without an additional startup grace. | Copy startup and published-lifetime compatibility still use the value until those bounded cuts move to the actor. |
| `PROGRESS_STALL` | **Retained in disjoint scopes.** The actor uses the explicit prepublication progress budget; compatibility code still uses the published-lifetime/copy threshold. | A producer cannot self-report a silent wedge. The later published/copy cuts must move the remaining decisions without overlapping prepublication. |
| `WATCHDOG_POLL`, `watch_for_stall*`, and `watchdog_active` | **Retained only for published-lifetime and copy compatibility.** | They cannot decide actor-owned prepublication recovery after this cut; later M4 cuts replace and delete those decision owners. |
| `child_transition` and `replacing_child` | **Retained for process/resource serialization and compatibility replacement.** | Exact child ownership, confirmed reap, published-lifetime handling, and copy replacement still require serialization until the later cuts remove in-place replacement. |
| copy fallback/recovery | **Retained.** | Copy delivery has not yet moved its recovery decision into the actor. |
| `PREPUBLICATION_REAP_RETRY` and `PREPUBLICATION_REAP_ATTEMPT_TIMEOUT` | **Retained lifecycle cleanup bounds, not playback watchdogs.** | They retry bounded termination until the exact predecessor is confirmed reaped; they cannot choose retry, replacement, playback failure, or response publication. |
| exact-EOF settlement task | **Retained bounded response completion owner, not a watchdog.** | At most 256 visible streams can reserve one owner; each owner gets one fresh five-second deadline at exact advertised EOF, commits only complete delivery, and releases capacity on every terminal path. |
| absolute playlist preparation budget | **Retained network lifecycle limit, not a watchdog.** | One deadline covers registry/storage reads, actor observation, catalog projection, and every poll across reclassification; timeout returns retryable `503` and cannot select or replace a producer. |
| bounded HTTP actor/handoff waits | **Retained network lifecycle limits, not recovery watchdogs.** | One five-second absolute publication deadline covers admission, first-media application, buffered commit, and actor command/reply waits; cancellation is fail-closed before later actor mutation. |

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

1. Complete the active actor-owned prepublication transcode cut, review it
   adversarially before tests, make local/cluster/hosted gates green, and merge
   it. Then migrate published-lifetime and copy recovery and prove the old
   server watchdog/replacement symbols are gone.
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
