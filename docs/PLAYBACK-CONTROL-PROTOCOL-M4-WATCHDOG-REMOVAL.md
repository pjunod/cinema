# Playback control protocol M4 — one producer deadline

**Status:** implementation contract
**Baseline:** `origin/main` at `9cd16d05` (PR #617)
**Parent design:**
[`PLAYBACK-CONTROL-PROTOCOL-PLAN.md`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
**Delivery ledger:**
[`PLAYBACK-CONTROL-STATUS.md`](PLAYBACK-CONTROL-STATUS.md)

M4 moves producer failure decisions into the rolling session actor. It removes
the detached startup and lifetime watchers, removes in-place
post-publication child replacement, and makes every producer timeout or exit
produce one attempt-fenced decision. This is the server-side ownership cutover
that makes later client and cluster handoffs possible; it does not yet prepare
or commit a transparent successor.

## 1. Outcome and non-goals

After M4, one rolling session has:

- one actor-owned `ProducerProgressDeadline`;
- one exact-attempt producer-event ingress;
- one bounded action channel from the actor to the session process owner;
- at most one validated retry before any media is published; and
- one immutable post-publication action proposal, with no child swap in the
  published generation.

M4 does not:

- enable automatic quality, codec, HDR, audio, subtitle, or node changes;
- implement prepare/commit/abort delivery handoff;
- delete the compatibility rolling presentation;
- remove ordinary HTTP, lease, admission, retirement, or VOD materialization
  deadlines; or
- remove client recovery paths. M5 owns that cutover.

The old rolling presentation therefore remains usable as the VOD fallback,
but its producer lifecycle no longer depends on polling tasks racing each
other.

## 2. Existing owners and their replacement

| Existing owner | Present action | M4 owner |
|---|---|---|
| hardware first-segment task | sleep, inspect files/progress, downgrade | actor deadline emits the only permitted pre-publication retry |
| software lifetime watcher | poll progress, inspect child, fail or kill | actor deadline or exact exit event emits one attempt decision |
| copy `Unsupported` continuation | replace copy child with ffmpeg | typed producer failure enters the actor and consumes the same retry right |
| `child_transition` | serialize child replacement, teardown, flow, and readers | actor fences lifecycle; one process owner serializes child mutation |
| `watchdog_active` | elect one detached watcher | deleted; the actor has one optional deadline |
| `replacing_child` | hide intentional exits and mask reads | deleted; attempt identity makes predecessor events stale |
| `downgrade_one_step` | kill and install a successor in place | deleted; the action executor starts only the actor-authorized retry |

The exact-attempt process supervisor remains. Its job is to reap the process
and publish an exit fact without blocking pipe drainage. It is event delivery,
not a watchdog and not a recovery owner.

## 3. Actor model

### 3.1 Policy supplied at attempt admission

`BeginProducerAttempt` receives a bounded `ProducerAttemptPolicy`:

```text
ProducerAttemptPolicy
  startup_budget
  progress_budget
  retry_recipe: None | validated recipe
```

The initial transcode attempt uses the current behavior-compatible budgets:

- hardware attempt: 12-second startup budget and one validated software retry;
- software-only attempt: 30-second startup budget and no retry;
- copy attempt: no time-based recipe retry, but one validated transcode retry
  may be consumed by a typed `unsupported` outcome; and
- every running published attempt: 10-second advancing-progress budget.

These values become named policy inputs beside the actor. They do not remain
sleep/poll constants in `transcode.rs`. Configuration is deliberately deferred:
changing a timeout must not recreate multiple owners.

### 3.2 State

The actor adds one producer-control record:

```text
ProducerControl
  phase: absent | starting | running | held | terminal
  attempt
  published
  last_progress_at
  deadline
  retry: unavailable | available(recipe) | consumed
  decision_sequence
  pending_decision: None | ProducerDecision
  proposal: None | ActionProposal
```

The record is actor-private. Status exposes a copied view; callers cannot
inspect it and then independently act.

### 3.3 Arm and disarm rules

The producer deadline is armed only when all are true:

1. the session is live;
2. current demand requires additional output;
3. the exact current producer attempt is running;
4. production is not intentionally held; and
5. the actor has not already emitted a decision for that attempt.

It is disarmed while demand is paused or ended, while ahead/disk/byte policy
holds the producer, after an exact exit, after a decision, and after any
terminal session event. A hold preserves the remaining budget. Resume sets a
fresh deadline from the resume instant so time deliberately stopped never
counts as producer failure.

Before the first advancing progress event, the deadline is
`attempt_started_at + startup_budget`. Each advancing `out_time_ms` resets it
to `observed_at + progress_budget`. Speed-only telemetry updates status but
does not prove advancing output and cannot reset the deadline. Publication
sets `published=true`; it does not itself excuse a producer that has stopped
advancing.

If demand does not yet contain an explicit runway, compatibility demand means
"output required" while the legacy flow policy says resume. Once an explicit
runway is present, the existing demand/target calculation is authoritative.

### 3.4 Exact timeout ordering

Commands and producer events queued at the same monotonic instant as the
deadline are drained before the timeout verdict. The actor then evaluates the
deadline at its armed instant. An event is accepted only for the current
attempt and only if its observation time is no later than that instant.

This gives deterministic outcomes under scheduler delay:

- progress observed before the deadline rearms it even if processed later;
- progress observed after the deadline cannot rescue the attempt;
- a stale predecessor cannot affect a successor; and
- a session terminal event prevents any subsequent producer decision.

## 4. Decisions and action execution

### 4.1 Internal decision

The actor emits one of:

```text
ProducerDecision::Retry {
  decision_sequence,
  failed_attempt,
  recipe,
  reason
}

ProducerDecision::Fail {
  decision_sequence,
  failed_attempt,
  reason,
  proposal
}
```

Reasons are typed: `startup_deadline`, `progress_deadline`,
`process_exit`, or `unsupported`. A bounded channel of capacity one carries
the decision to the session-owned process executor. The actor retains the
pending decision until that executor acknowledges the exact
`decision_sequence`; a wake or duplicate exit cannot mint another action.

### 4.2 Pre-publication retry

`Retry` is legal only when no playlist or segment has been published and the
retry right is available. Emission consumes that right atomically. The process
executor:

1. kills and reaps the exact failed attempt, if it is still present;
2. asks the actor to admit the supplied recipe as the next attempt;
3. spawns the candidate;
4. asks the actor to authorize exact-attempt installation;
5. installs it under the process-owner mutex; and
6. acknowledges the decision.

If any step fails or the session terminates, the candidate is killed and
reaped. It is never installed after a terminal fence. A failed retry is a
terminal producer failure; it cannot recursively request another recipe.

Copy `Unsupported` follows this same path. It is an observation about the
current pre-publication attempt, not permission for the reader task to mutate
the child.

### 4.3 Post-publication failure

Once anything has been published for the generation, timeout, non-success
exit, or `unsupported` cannot replace its producer in place. The actor emits
one `Fail`, the executor kills/reaps only the exact failed attempt, and the
actor retains one proposal:

```json
{
  "type": "prepare_replacement",
  "action_id": "stable generation/attempt/decision identity",
  "reason": "producer_stalled",
  "source": "server",
  "severity": "required"
}
```

Until M5.5 implements prepared successors, this proposal is advisory and
replayed on later accepted control exchanges. The already-published playlist
and bytes remain readable. M4 must not point the generation at another child,
reset its sequence, or overwrite its scratch directory.

Successful natural exit is recorded and disarms the deadline. Whether it is a
valid completion is derived from the current presentation and published
frontier; it does not request a retry.

### 4.4 Replay and acknowledgement

The action ID is derived from immutable actor identity and the monotonic
decision sequence. Equal control-sequence replay returns the identical action.
Later observations for the failed attempt do not change it. M4 does not mark
the proposal committed; M5.5 adds the preparation transaction and action
acknowledgements.

## 5. Process and lifecycle ownership

There is one task allowed to mutate `Session.child`: the session process
executor. Request handlers may read actor snapshots and commit media facts but
cannot replace a child. Flow control submits `Hold` or `Resume` to the actor;
only an actor-authorized executor signal reaches the process.

`child_transition` can then be deleted:

- producer installation is fenced by exact attempt plus the actor's existing
  synchronous terminal/install boundary;
- End, authority fence, and lease expiry first close actor authority, then ask
  the executor to kill/reap the owned child;
- publication and fetch paths accept only the current actor attempt;
- an exit from a killed predecessor is merely a stale exact-attempt fact; and
- retention/index refresh owns files, not producer replacement.

The child mutex itself remains because the OS process handle needs exclusive
access for wait, signal, and teardown. It is ordinary resource ownership, not
a recovery election lock.

The action executor must be cancellation-safe. Dropping an HTTP request or
flow waiter cannot cancel a decision after the actor emits it. Actor shutdown
closes its action stream only after terminal cleanup owns the child.

## 6. HTTP and compatibility behavior

Playlist and segment request budgets remain ordinary bounded waits. They may
return the existing typed not-ready or failed response, but they cannot start
a producer, switch recipes, or arm a watcher. `PLAYLIST_WAIT_BUDGET` is reduced
to a single HTTP wait budget; it no longer sums recovery sleeps.

The fallback presentation remains selected through the existing rollout
gates. There is no second "legacy watchdog" feature path inside it after M4.
Rollback is the git/release rollback, not a runtime branch that keeps both
recovery owners alive.

## 7. Observability

Activity and session status add:

- producer deadline state: `disarmed`, `starting`, `running`, `held`,
  `decided`, or `terminal`;
- deadline remaining milliseconds when armed;
- current attempt and whether anything is published;
- retry state: `unavailable`, `available`, or `consumed`;
- last typed decision reason and decision sequence; and
- outstanding proposal action ID/type.

Prometheus counters are bounded-label:

- `plurx_playback_producer_deadline_total{phase,outcome}`;
- `plurx_playback_producer_decision_total{kind,reason}`;
- `plurx_playback_producer_retry_total{outcome}`; and
- `plurx_playback_producer_action_total{outcome}`.

No generation, file, client, or action ID is a metric label. Logs may carry
those values as structured fields.

## 8. Repository ownership check

The validation catalog gains a source check over playback server modules. It
fails if an unapproved recovery pattern is introduced, including:

- the deleted symbol names `FIRST_SEGMENT_GRACE`, `SOFTWARE_GRACE`,
  `PROGRESS_STALL`, `WATCHDOG_POLL`, `child_transition`, `watchdog_active`,
  `replacing_child`, `downgrade_one_step`, and `watch_for_stall`;
- a detached `sleep` loop that inspects producer or publication state and then
  kills, starts, or swaps a process; or
- any child replacement outside the named process executor.

The check also enumerates the approved progress deadlines:

1. rolling `ProducerProgressDeadline`;
2. VOD `SegmentMaterializationDeadline`; and
3. the client `PlaybackProgressDeadline` added in M5.

Lifecycle and network timers are listed separately so a useful bound is not
misclassified as a watchdog.

## 9. Race and failure matrix

| Ordering | Required result |
|---|---|
| progress before deadline, processed after | progress wins and rearms |
| progress after deadline | one timeout decision |
| exit and deadline together | one decision with deterministic precedence; no duplicate |
| pause or hold at deadline | disarmed; no decision |
| resume after a long hold | fresh full progress budget |
| publication just before failure | no retry; one proposal |
| retry decision then End | candidate is killed; no install |
| End then retry decision | no decision emitted |
| copy `Unsupported` and timeout | one pre-publication retry total |
| predecessor exit after retry install | stale fact rejected |
| action receiver cancellation | decision remains owned and is completed once |
| exact control replay | byte-equivalent action and action ID |

## 10. Verification order

Per the delivery instruction, implementation is reviewed adversarially before
unit tests run.

1. Static source inventory demonstrates that every existing recovery owner is
   mapped in this document.
2. Implement actor state, exact deadline, decision channel, and model tests.
3. Implement the process executor and migrate transcode/copy paths.
4. Delete the old tasks, locks, atomics, and replacement helpers.
5. Add status, metrics, and the repository ownership check.
6. Obtain adversarial review of the exact cumulative diff; fix every finding.
7. Run focused actor and fault-injection tests.
8. Run `make check`, repair failures, and repeat until green.
9. Run `make cluster-check` because terminal, owner-fence, and producer action
   ordering cross cluster ownership.
10. Open the PR, obtain exact-head adversarial review, require every hosted job
    to pass, and merge only while the reviewed head is unchanged.

## 11. Acceptance

M4 is complete only when all of the following are true:

- the actor owns the sole rolling producer deadline;
- fault injection proves one and only one pre-publication retry;
- copy `Unsupported` consumes that same retry right;
- no post-publication event swaps or restarts the child;
- a post-publication failure yields exactly one stable action proposal;
- held or paused time cannot cause a producer timeout;
- terminal and attempt fences reject every late install, signal, and event;
- all deleted symbols and detached recovery loops are absent;
- the status surface names deadline, retry, decision, and proposal state;
- adversarial review has no unresolved P0-P3 finding; and
- focused, full local, cluster, and hosted gates are green at the exact merged
  head.
