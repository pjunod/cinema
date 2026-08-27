# Playback control M3c2 — producer progress and process-exit events

## Status

Implementation slice M3c2. This document is the review and handoff contract
for `codex/playback-control-m3-producer-events`, based on merged PR #615 at
`46c08439`.

The slice is deliberately action-passive. It makes producer motion and process
exit actor-owned facts; it does not replace, restart, fail, hold, or resume a
producer. M4 moves those actions only after this event path is reviewed and
proven.

## Why this slice exists

The compatibility engine currently learns producer health from several places:
ffmpeg progress atomics, child `try_wait()` calls inside request paths, startup
fallback loops, and a lifetime watchdog. Those readers can sample different
producer attempts and can act on different clocks. More importantly, deleting
the old watchdog would also delete the only unconditional child-exit poll.

M3c1 established one actor attempt number for installation, publication, and
response commits. M3c2 carries that same number through the remaining producer
facts:

- output timeline progress;
- cumulative and recent production speed;
- age since the output timeline last advanced; and
- terminal process success, exit code, or Unix signal.

Once those facts are ordered by the actor, M4 can replace overlapping recovery
loops with one named `ProducerProgressDeadline` without losing process death or
mistaking a predecessor for its successor.

## Requirements

The implementation must preserve all of these invariants:

1. ffmpeg stdout and stderr drains never await actor mailbox capacity.
2. Memory used by unconsumed progress is constant.
3. A delayed predecessor observation cannot evict a pending successor fact.
4. Actor output time never moves backward.
5. Repeated timestamps and speed chatter do not reset the progress clock.
6. Exit is terminal for one exact attempt; later buffered progress cannot
   reopen it.
7. The first exit verdict wins and identical re-observation is idempotent.
8. Attempt replacement resets every producer observation atomically.
9. Retirement rejects later progress and exit observations.
10. Process exit is reported without an HTTP request or watchdog poll.
11. M3c2 changes no recovery action or playback-visible fallback policy.

## Event path

```text
ffmpeg progress pipe                 installed ffmpeg process
        |                                      |
        | parse exact attempt                  | supervisor owns Child
        v                                      v
latest-progress slot                     exact exit slot
        \                                      /
         \-- bounded producer ingress --------/
                       |
                       | wake (never mailbox backpressure)
                       v
              rolling control actor
                       |
                       v
       one exact-attempt delivery snapshot
                       |
          +------------+-------------+
          |                          |
   Activity/status                 metrics
```

The producer ingress is separate from the bounded control-command mailbox. It
contains exactly two optional slots: latest progress and latest exit. A short
synchronous mutex linearizes publication and a `Notify` wakes the actor.
Publishing never awaits. Repeated progress is merged into repeated progress;
it cannot grow a queue. For one attempt, output time keeps its monotonic
maximum and speed retains the newest available sample. The first pending exit
for an attempt is immutable; only an exit from a later attempt can replace a
pending predecessor.

The slots retain their own sequence numbers. A drain orders the two retained
facts before applying them. Slot replacement also compares monotonic producer
attempts, so a late attempt N progress line cannot overwrite already-pending
attempt N+1 progress.

Before handling a command, the actor drains already-published producer events.
This gives a snapshot sent after an observation a causal view without merging
high-frequency progress into the mailbox. Events published after that drain
linearize after the command and remain exact-attempt fenced.

## Progress ownership

The compatibility `Progress` object remains the parser and old-action
projection in M3c2. After a valid `out_time` or `speed` line updates it, the
drain submits one normalized actor observation:

- `producer_attempt`;
- `out_time_ms`;
- cumulative speed scaled by 1,000;
- recent speed scaled by 1,000; and
- monotonic observation time.

The actor stores integers rather than floating-point values. It accepts only
its current attempt, ignores all observations after that attempt exits, and
advances the motion clock only when `out_time_ms` strictly increases. Speed may
still change when the output timestamp repeats, but that chatter cannot make a
stalled encoder look alive.

Attempt admission establishes the initial motion time. A producer that never
emits its first block therefore accumulates progress age on the same coordinate
as a producer that moves and later wedges.

## Process supervision

Every installed ffmpeg child is converted to an `AttemptChild` only after actor
authorization. One supervisor task permanently captures that installation
attempt and exclusively owns the `Child` from installation through signaling,
waiting, and reaping. Other paths send typed signal or terminate commands and
never act on a cached PID. Immediately before a requested signal, the
supervisor rechecks the actor's exact-attempt and live-transition fence while
the owned child is still unreaped. Compatibility readers consult the
supervisor's terminal cell instead of calling `Child::try_wait()` themselves.

The supervisor selects between the cancel-safe `Child::wait()` future and its
typed command channel, with a ready process result taking priority. There is no
per-child `SIGCHLD` listener and no exit polling interval: one child exit wakes
only its own waiter. Explicit kill, hold, and resume are commands to that same
owner. The exact-attempt transition guard stays held from authorization through
the synchronous signal syscall. Dropping a live session requests `SIGKILL`;
the supervisor still reaps it and publishes the final status. This prevents a
waiter from reaping a process between another task's PID lookup and signal,
which could otherwise signal an unrelated process after PID reuse.

The actor accepts an exit only for its current attempt. Replacement admission
can therefore advance from N to N+1 before killing N: the N supervisor will
still reap correctly, but its later exit cannot terminate N+1.

## Actor snapshot and instrumentation

The rolling delivery snapshot adds:

- producer output time;
- cumulative and recent speed in milli-units;
- progress idle age;
- exit success;
- exit code;
- exit signal; and
- exit observation age.

Rolling Activity/status chooses one owner attempt from its actor snapshot. It
projects no child or progress fact from another attempt even if replacement
lands while status is being assembled. If the actor is unavailable, status
samples the fallback attempt once, attributes an exact matching supervisor
terminal cell to that attempt, and reports compatibility progress explicitly
unknown as `progress_idle_ms = -1` with null rates and output time. It does not
try to reconstruct a coherent snapshot from resettable progress atomics.
`producer_state` reports
`complete` for a successful supervised exit and `exited` for another terminal
status unless an existing explicit session failure has the stronger `failed`
verdict. A terminal process verdict also precedes a stale `held` projection.

Prometheus adds bounded counters for:

- progress and exit ingress;
- observations coalesced before actor drain; and
- progress and exit observations accepted or rejected by the actor fence.

The existing compatibility progress atomics remain visible only to old action
paths until M4. Status no longer samples them independently when the rolling
actor is available.

## What remains a watchdog

M3c2 removes no recovery mechanism. Exact inventory after this slice:

| Mechanism | M3c2 role | M4 disposition |
|---|---|---|
| first-segment hardware fallback loop | Still decides whether to replace a pre-publication producer | Delete as an independent owner; actor deadline chooses a prepared action |
| lifetime stall watcher | Still polls compatibility progress and can fail/kill | Replace with one actor `ProducerProgressDeadline` |
| `child_transition` | Still serializes compatibility replacement and teardown actions | Delete after those actions enter the actor |
| `watchdog_active` | Still prevents duplicate lifetime watchers | Delete with the watcher |
| `replacing_child` | Still masks intentional predecessor exit and path replacement | Delete after actor replacement state owns that interval |
| playlist/segment wait limits | Still bound individual HTTP waits | Retain as request deadlines, not recovery owners |
| process supervisor | Sole child signal/wait/reap owner; delivers exact exit once and never decides an action | Retain as the producer lifecycle event source |
| VOD materialization deadline | Unchanged | Retain as an approved progress deadline |

The process supervisor is not a health watchdog. It awaits the process's own
completion future, has no poll interval or progress threshold, and chooses no
recovery action: it is the asynchronous equivalent of collecting a process
return value.

## Failure behavior

- A full actor command mailbox does not block progress drainage. The latest
  progress remains in the bounded slot.
- A poisoned ingress or terminal mutex recovers the inner value; poisoning
  must not strand a process or progress pipe.
- A wait failure is recorded locally, logged with the attempt, and returned to
  compatibility readers. It is not fabricated into an exit code.
- An unavailable actor cannot turn an exit into a recovery action. Existing
  lease/repair fencing handles actor unavailability as before.
- A signal that races a natural exit treats `ESRCH` as already terminal and
  waits for the supervisor's actual result.
- Progress buffered before process exit but drained afterward is rejected once
  the actor has accepted exit. Terminality is more important than reconstructing
  a final diagnostic sample that no client can consume.

## Verification contract

Adversarial review must precede all unit and full test execution. The reviewed
head must cover:

1. latest progress survives at least 10,000 publications with constant slots;
2. stale progress cannot evict pending successor progress;
3. progress and exit slots drain in publication order;
4. output time is monotonic while speed can still refresh;
5. replacement rejects predecessor progress and exit;
6. exit is idempotent, terminal, and immutable;
7. retirement rejects both event types;
8. a current child exit reaches the actor without a watchdog or request;
9. a delayed predecessor exit after successor admission is rejected;
10. hold/resume/kill/drop cannot signal outside the supervised unreaped child;
11. a full real command mailbox cannot block or reorder accepted progress;
12. contradictory same-attempt exits preserve the first outcome;
13. actor-unavailable and held-terminal status remains truthful;
14. a replacement during status assembly cannot mix two attempts;
15. Activity/status and Prometheus expose the new facts; and
16. focused tests, full local gates, browser contracts, and every required
    hosted job pass on the final reviewed head.

## Rollout and rollback

This slice is additive and action-passive, so rollout needs no mixed-fleet
protocol negotiation. Nodes that include M3c2 report richer local actor state;
wire control messages and media URLs do not change.

Rollback removes the producer ingress, supervisor, actor fields, status fields,
and metrics, then restores `Mutex<Option<Child>>`. Because M3c2 deliberately
does not remove a recovery owner, rollback does not need to recreate a
watchdog. M4 must not merge unless this slice is already present and green.

## Next slice

Finish actor-owned end and cluster-fence events and model their reorderings.
Then M4 can move the single producer deadline and every recovery action into
the actor, delete the compatibility watchdog state, and update the watchdog
ledger with exact remaining lifecycle and request deadlines.
