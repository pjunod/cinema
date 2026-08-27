# Playback control rewrite — project status

**Updated:** 2026-08-27
**Merged baseline:** `origin/main` at `46c08439` (PR #615)
**Current work:** PR [#616](https://github.com/pjunod/plurx/pull/616),
`codex/playback-control-m3-producer-events` — M3c2 passive, nonblocking producer
progress and exact-attempt process-exit observations. Four adversarial passes
requested changes on `405f6d3e`, `79d9460b`, and `e3924882`; a fourth pass on
`1d056c2c` found one nondeterministic test assertion. All finding sets and the
two Clippy failures reported by automatic run `33042791225` are remediated on
the current untested head. A fifth exact-head approval is required before any
local unit or full-gate run
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
| M3 — actor and explicit lease | **In progress** | Rolling generation has one bounded actor for control fencing, explicit demand/pacing, renewal, expiry claim, retirement fence, response commit ownership, and the merged M3c1 attempt-fenced delivery ledger | Finish M3c2 progress/exit observations, then add end and cluster-fence event ownership |
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

## Active slice: M3c2 producer events

M3c1 merged as PR #615 at `46c08439`. It gives the actor the ordered,
attempt-fenced publication and completed-fetch coordinates described in
[`PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md`](PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md).

The active M3c2 slice remains behavior-passive: it moves observations, not
recovery actions. ffmpeg progress enters a two-slot coalescer independent of
the bounded command mailbox, so a full control queue cannot stop stdout or
stderr drainage. The actor accepts only monotonic progress for its exact
attempt and treats process exit as terminal. Each installed process is owned
by a small supervisor that waits for exit immediately and reports its immutable
attempt, so exit detection no longer depends on any old watchdog poll.

Rolling Activity/status reads rate, output position, progress age, and exit
code/signal from the actor snapshot. Prometheus exposes producer-event ingress,
coalescing, and accepted/rejected outcomes. The existing recovery watchdogs and
actions remain unchanged until M4 can delete them in one reviewed cutover. The
full ordering, supervision, instrumentation, and rollback contract is in
[`PLAYBACK-CONTROL-PROTOCOL-M3-PRODUCER-EVENTS.md`](PLAYBACK-CONTROL-PROTOCOL-M3-PRODUCER-EVENTS.md).

Review and test state for M3c2:

| Gate | State |
|---|---|
| Implementation | Remediation is complete but untested on PR #616 from merged `46c08439`. The current implementation has constant-space event ingress, actor-owned facts, a cancel-safe targeted process supervisor, one guarded signal linearization, single-attempt status projection, bounded metrics, and regression mapping. Its actor constructor now receives one named runtime bundle, and cached/successful-exit completion is one predicate. |
| Adversarial diff review | **Changes requested four times.** Head `405f6d3e`: compile blockers, cached-PID reuse, terminal/held and first-exit ordering, unavailable status, and coverage/status gaps. Head `79d9460b`: guard-before-signal gap, history recursion, mixed N/N+1 status, and per-child `SIGCHLD` fan-out. Head `e3924882`: relay omitted `exited`, merged progress retained pre-exit ordering, replacement status fabricated `complete`, signal-race test lacked a contender handshake, and this ledger lagged the automatic run. Head `1d056c2c`: the handshake still did not prove the contender reached the transition mutex. The test now directly proves the paused supervisor owns that mutex with `try_lock`; a fifth exact-head review is required. |
| Unit/focused tests | **Not run locally**, by design, until the remediated exact head passes adversarial review. Added cases cover a genuinely full command mailbox, same-attempt progress regression, progress-after-exit terminal ordering, contradictory exits, relay `exited` validation, supervised signals/kill/drop/reap, authorization-to-syscall fencing with a direct held-mutex assertion and contender handshake, actor-unavailable status, replacement-during-status `waiting`, terminal-over-held priority, joined status fields, and metric names. |
| Full local gate | Not run. After review: focused tests, `make test`, `make check`, unrestricted `make validate`, and applicable browser contracts. |
| Hosted CI | Automatic run `33040710196` failed initial-head compile/Clippy; `33041867989` found the mapping filename; `33041927855` found the filename-fix subject; and `33042661180` classified the mapping-update subject itself. Both metadata commits are now neutral (`be83782d`, `f54b426a`), while corrective runtime heads remain mapped. Run `33042791225` passed history preflight, WAL recovery, and daemon cluster contracts but its fast Rust job found duplicate completion branches and an eight-argument actor constructor. Commits `ddd448d0` and `27224bd0` address and map those findings. Superseded head `1d056c2c` then passed the automatic preflight, fast Rust, WAL recovery, and daemon cluster jobs while its long topology job continued; the current head adds only the deterministic mutex-ownership assertion and mapping. Hosted evidence is not a substitute for the required local sequence. |
| Merge | Not ready. Requires exact-head approval, local green, hosted green, and no unresolved review findings. |

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

## Remaining delivery order

1. Complete, adversarially review, validate, and merge M3c2 nonblocking
   producer progress and exact-attempt process-exit observations.
2. Finish end and cluster-fence event ownership with model tests over event
   reorderings.
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
