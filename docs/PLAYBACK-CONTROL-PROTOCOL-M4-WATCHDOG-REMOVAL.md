# Playback control protocol M4 — one producer deadline

**Status:** implementation contract
**Baseline:** `origin/main` at `9063bb1e` (PR #626)
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
- one actor-owned decision slot with a nonblocking executor wake;
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
| copy direct failures | `InvalidHevcConfiguration` and reader failures set `Session::fail` directly | one copy classifier publishes a typed actor outcome |
| `playlist_producer_failed` | request-side child inspection creates a second exit verdict | deleted; requests consume the actor's producer disposition |
| initial start/install paths | spawn a raw child, then separately register actor/session ownership | use the same exact-attempt supervisor registration used by retry |
| `child_transition` | serialize replacement, teardown, flow signals, publication/path sampling, index refresh, retention, and terminal settlement | replace each duty with the specific actor fence, attempt tag, response admission, flow ticket, or resource mutex named below |
| `watchdog_active` | elect one detached watcher | deleted; the actor has one optional deadline |
| `replacing_child` | hide intentional exits and mask reads | deleted; attempt identity makes predecessor events stale |
| `downgrade_one_step` | kill and install a successor in place | deleted; the action executor starts only the actor-authorized retry |

The exact-attempt process supervisor remains. Its job is to reap the process
and publish an exit fact without blocking pipe drainage. It is event delivery,
not a watchdog and not a recovery owner. It remains the sole owner of the real
OS `Child`, preserves the biased cancel-safe wait, and is the only task allowed
to signal its still-reserved PID. `Session.child` continues to hold an
`AttemptChild` command proxy, not the OS process.

The implementation inventory is not complete until a checked source catalog
lists every production child start, supervisor registration, install, signal,
terminate, wait, direct failure, response admission, and each current
`child_transition` acquisition with its replacement invariant.

## 3. Actor model

### 3.1 Policy supplied at attempt admission

The initial `BeginProducerAttempt` receives one bounded
`InitialProducerPolicy`:

```text
InitialProducerPolicy
  presentation_contract_fingerprint
  startup_budget
  progress_budget
  exit_classification_budget
  expected_remaining_ms: known | unavailable
  completion_tolerance
  exit_classifier: immediate | copy_reader
  generation_retry: None | ValidatedRetryRecipe
```

The initial transcode attempt uses the current behavior-compatible budgets:

- hardware attempt: 12-second startup budget and the exact prevalidated retry
  recipe selected by today's color-safe ladder;
- software-only attempt: 30-second startup budget and no retry;
- copy attempt: 30-second startup budget, no timeout retry, and one validated
  HLS-muxer recipe that only a typed `unsupported` outcome may consume; and
- every running published attempt: 10-second advancing-progress budget.

Exit classification uses a five-second mode of that same producer deadline.
`expected_remaining_ms` has one equation:
`max(0, file.duration_ms - round(media_origin_seconds * 1000))`.
`media_origin_seconds` is already the achieved absolute source origin; takeover
`origin_base_ms` is not subtracted again. If the source has no trustworthy
duration, valid exact-attempt ENDLIST is sufficient for
`complete_unverified_duration`; absence of ENDLIST is never completion.

`ValidatedRetryRecipe` is an opaque actor key plus a recipe fingerprint and
the exact frozen presentation-contract fingerprint; the session executor
retains the immutable concrete command inputs. The actor retains that same
presentation-contract fingerprint in `InitialProducerPolicy` and requires an
exact match both when emitting and admitting `Retry`. For a transcode the
recipe names either the same encoder with the proved CPU/color-safe pipeline,
or the software encoder with its family-correct rate control and admission change.
If the current pipeline has no color-safe fallback, there is no retry. It is
never a blanket hardware-to-software rule. For copy it names the exact legacy
ffmpeg HLS-muxer arguments.

The retry token belongs to the delivery generation, not an attempt. A distinct
`AdmitProducerRetry(decision_sequence, recipe_fingerprint)` command can consume
it once and creates a successor whose retry state is necessarily `consumed`.
Ordinary `BeginProducerAttempt` cannot be called again after initial admission.

These values become named policy inputs beside the actor. They do not remain
sleep/poll constants in `transcode.rs`. Configuration is deliberately deferred:
changing a timeout must not recreate multiple owners.

### 3.2 State

The actor adds one producer-control record:

```text
ProducerControl
  phase: absent | starting | running | hold_requested | held |
         classifying_exit | decided | complete | terminal
  attempt
  metadata_response_authorized
  producer_media_published
  last_progress_at
  progress_deadline: None | ProducerProgressDeadline(mode, instant)
  pending_action: None | PendingProducerAction
  pending_probe: None | PendingProducerProbe
  last_probe_outcome: none | published | cancelled | stale
  generation_retry: unavailable | available(recipe) | consumed
  flow_revision
  desired_flow
  applied_flow
  completion: incomplete | complete(verified_duration | unverified_duration)
  decision_sequence
  pending_decision: None | ProducerDecision
  proposal: None | ActionProposal
  executor: unregistered | idle | queued | executing | acknowledged | lost
```

`ProducerProgressDeadline.mode` is one of `starting`, `advancing`, or
`classifying_exit`. `PendingProducerAction` is defined in §4.1; a classifier
probe has its own attempt/sequence fence but never its own verdict timer.

The record is actor-private. Status exposes a copied view; callers cannot
inspect it and then independently act.

### 3.3 Arm and disarm rules

In `starting` or `advancing` mode, the producer deadline is armed only when all
are true:

1. the session is live;
2. current demand requires additional output;
3. the exact current producer attempt is running;
4. production is not intentionally held; and
5. the actor has not already emitted a decision for that attempt.

In `classifying_exit` mode, the exact producer has exited and the deadline is
armed solely to bound the immediate completion/copy classification described
below. It is independent of demand and cannot be reset by later progress.

It is disarmed while demand is paused or ended, after a decision, after valid
completion, and after any terminal session event. Requesting a hold does not
change the running deadline: only a successful exact-attempt SIGSTOP
acknowledgement moves the actor to `held`, disarms it, and discards the old
remaining budget. A Resume does not arm anything when requested or queued:
only the exact-attempt supervisor's successful SIGCONT acknowledgement moves
the actor to `running` and grants one fresh full progress budget. This is the
sole hold semantic; no frozen remainder is retained.

An exact exit moves the actor to `classifying_exit`. Non-success transcode
exit classifies immediately. Successful exit, and any exit governed by the
copy reader, rearms the same `ProducerProgressDeadline` at
`exit_published_at + exit_classification_budget`; they do not disarm it. The
actor immediately wakes one attempt-fenced completion/classifier probe rather
than waiting for the 15-second repair tick. The probe performs one bounded
playlist read and publishes ENDLIST, indexed frontier, expected duration, and
copy outcome through ingress. Expiry of this mode becomes a typed
`exit_classification_deadline` failure.

Before the first advancing progress event, the deadline is
`attempt_started_at + startup_budget`. Each advancing `out_time_ms` resets it
to the event's fenced `published_at + progress_budget`. A source observation
timestamp remains telemetry and cannot backdate or extend the actor's ingress
deadline. Speed-only telemetry updates status but does not prove advancing
output and cannot reset the deadline. Attempt-bound response authorization
sets `producer_media_published=true`; it does not itself excuse a producer that
has stopped advancing. Generation-stable metadata authorization is tracked
separately and does not change retry eligibility or the producer deadline.

The actor accepts `ApplyProducerFlow` with the exact attempt, a monotonic flow
revision, and the complete evaluated inputs for demand, time-ahead, byte, disk,
and global caps. It rejects a revision older than the last accepted one and
derives one desired state from current actor demand plus those caps. A reason
change is a new revision, not an independent owner. The per-attempt supervisor
holds the actor's synchronous signal authorization through the PID syscall and
publishes `FlowApplied(attempt, revision, state, outcome)` before releasing it.
Desired-state mutation, authorization, terminal/decision revocation, syscall,
and acknowledgement therefore share that fence.

Signal outcomes are typed:

- `applied` commits the physical state using the arm/disarm rules above;
- `stale` is rejected and the newest actor flow state is evaluated;
- `exited` enters exact exit classification; and
- `live_signal_error` may retry an interrupted syscall once inside the same
  bounded operation, then becomes `flow_stop_failed` or `flow_resume_failed`.

A live signal error is a producer failure: before publication it fails the
attempt, and after publication it retains bytes and records one proposal. A
failed SIGSTOP therefore cannot leave running production without its old
deadline, while a failed SIGCONT cannot leave a stopped process with no
terminal outcome. The external supervisor-command wait is also bounded by the
actor-owned action deadline in §4.1. A decision or terminal transition revokes
signal authorization immediately.

If demand does not yet contain an explicit runway, compatibility demand means
"output required" while the complete legacy flow evaluation says resume. Once
an explicit runway is present, the existing demand/target calculation is
authoritative.

### 3.4 Exact timeout ordering

Commands and producer events share one `RollingIngressFence` sequence. An
async command first reserves bounded mailbox capacity. Only then does it take
the short fence, allocate its sequence and `published_at`, publish through the
reserved permit, and release the fence. It cannot receive a pre-deadline stamp
and then be descheduled before insertion.

Nonblocking producer ingress is bounded but deadline-chain aware. Under that
same fence, each barrier-free interval accumulates one
`ProgressCoverageBatch`:

```text
ProgressCoverageBatch
  attempt
  first_advancing(sequence, published_at, out_time_ms)
  covered_last(sequence, published_at, out_time_ms)
  covered_deadline
  first_gap(sequence, published_at, out_time_ms) | None
  latest_telemetry(sequence, published_at, speed, status)
```

The first unconsumed `out_time_ms` increase initializes `first_advancing` and
`covered_last`. Each later increase extends `covered_last` and
`covered_deadline` only while no gap is recorded and its `published_at` is at
or before `covered_last.published_at + progress_budget`. This proves the
internal A-to-B-to-C chain without retaining every sample. The first increase
after that horizon is retained as `first_gap`; it freezes coverage, and no
later progress may bridge it. Speed-only telemetry updates
`latest_telemetry` but cannot create coverage or erase the first, covered-last,
or first-gap evidence.

The batch does not decide whether its first event beat the actor's current
armed deadline. On drain, the actor first requires
`first_advancing.published_at <= current_deadline`. If true, the batch's
already-proved contiguous prefix may advance the deadline to
`covered_deadline`; if false, none of the batch can rescue the attempt. A
recorded first gap is then necessarily after that resulting deadline and
causes the ordinary single deadline verdict unless a higher-priority event
wins. Thus delayed A/B/C is accepted only when every link was timely, while a
missing middle sample cannot either create a false timeout or bridge a real
stall.

Every state-changing non-progress fact is an ordering barrier:

- after reserving its one bounded mailbox permit and taking the ingress fence,
  a command sender seals the current open progress batch into
  `command.preceding_progress_batch`, resets the open batch, then allocates and
  publishes the command sequence; and
- before publishing an exit, physical-flow acknowledgement, or classifier/
  probe result, the nonblocking publisher takes the fence, verifies that the
  exact fixed event slot is empty, seals the open progress batch into that
  event's `preceding_progress_batch`, resets the open batch, then allocates the
  event sequence and fills the slot. A duplicate is rejected before sealing or
  allocating anything.

Progress after any barrier occupies a distinct batch and can never be folded
across the state change. Drain turns each fixed producer slot into the ordered
block `preceding_progress_batch -> barrier event`, sorts those blocks by the
barrier's original sequence alongside command envelopes, and places the final
open progress batch last. The actor applies each block in that order. Thus
`progress A -> FlowApplied(held) -> progress B`, `A -> FlowApplied(resumed) ->
B`, `A -> exit -> B`, and `A -> classifier -> B` cannot be collapsed into one
coverage proof or rearm from the wrong physical phase.

The bound remains fixed: at most one sealed batch belongs to each bounded
command envelope; the exact current exit, flow-acknowledgement, and classifier
slots each own at most one sealed batch; and there is one open batch. Memory is
`O(mailbox_capacity + fixed_event_slots)`, not progress rate. Each fixed slot
retains original sequence and `published_at` plus exact attempt and
operation/probe sequence. A higher admitted attempt may replace predecessor
slots only while the actor and ingress fences are held and after the predecessor
blocks have been drained or declared stale. A late lower-attempt publisher
cannot evict a successor fact.

Actor dispatch is due-first: before every receive it compares the nearest
deadline with `now`, and its `tokio::select!` is biased with the deadline branch
first. Bias alone is not the proof: after either receive branch returns, the
actor retains the selected envelope, rereads `now`, and runs a now-due cutoff
before handling that envelope. This closes the interval in which the timer was
polled pending just before its instant and a command became ready just after
it. The selected envelope's ingress sequence and fenced `published_at` decide
whether it participates in the cutoff or remains queued for afterward. When a
cutoff starts, the actor performs it without awaiting external work:

1. acquire the synchronous actor transition fence and the ingress fence, then
   record the deadline instant plus the ingress high-water sequence;
2. close and drain the command and fixed producer barrier blocks plus the open
   progress batch published through that sequence, preserving original
   publication metadata and every state-change barrier;
3. retain the already-selected envelope, then `try_recv` at most the bounded
   mailbox capacity into an actor-owned deque capped at mailbox capacity plus
   that one selected envelope; process
   only facts through the captured high-water mark, folding each one against
   the current armed deadline as earlier accepted progress may extend it;
   facts later than that resulting deadline stay ordered in the deque for
   post-verdict handling; senders cannot publish a new sequence until the
   ingress fence is released;
4. give an already-due End, authority fence, or playback-lease expiry first
   precedence;
5. apply response-publication admissions, successful flow acknowledgements,
   copy classifications, progress, and exit facts in their ingress sequence;
6. let an accepted attempt-media publication convert only an uncommitted
   provisional due observation to a post-publication failure, let accepted
   progress rearm, and only then emit a producer decision if the exact armed
   deadline is still due; an immutable `pending_decision` can never be
   converted; and
7. release both fences and perform any wake or external action. Every retained
   producer batch or command envelope then re-enters the same due-first
   dispatch one item at a time: reread `now`, run any now-due cutoff, and only
   then handle that item. A plain loop may not dispatch the retained deque
   without those per-item deadline checks.

An observation is eligible only if its fenced `published_at` is at or before
the deadline armed immediately before that observation and its sequence is
within the captured high-water mark. Contiguous advancing progress may extend
that deadline before the next observation is evaluated. Merely existing before
cutoff processing begins is not sufficient. An earlier source timestamp
published after its applicable armed deadline is late and cannot reverse an
already-emitted decision. If exit and deadline are both eligible, a classified
exit published at or before its applicable armed deadline supplies the reason;
otherwise the deadline does. Session terminal/lease events always beat either.

This gives deterministic outcomes under scheduler delay:

- advancing progress published at or before the armed deadline rearms it even
  if processed later;
- progress published after the armed deadline cannot rescue the attempt;
- a stale predecessor cannot affect a successor; and
- a session terminal event prevents any subsequent producer decision.

### 3.5 Response-publication fence

Retry eligibility and response visibility share one actor linearization point.
Every successful or bodyless response resolved from a live rolling generation
crosses `AuthorizeResponsePublication` after it is fully prepared or opened
but before a body or reader becomes visible. Typed startup/failure responses
that expose no resolved producer object do not claim media publication: a
generation-fenced `503` may report starting or retry-pending state, a
decision-fenced `502` may report final producer failure, and a no-actor `404`
reports that the capability is gone. The prepared media response carries one
of four bounded bindings. Classification follows the body's actual data
dependencies, not its route name:

- `generation_metadata` contains only immutable delivery-generation facts and
  relative capability URLs. The actor verifies the live generation and frozen
  presentation contract but does not set `producer_media_published`, consume
  the retry token, or alter the producer deadline;
- `attempt_media` carries the exact producer attempt and bounded object
  identity. The actor rejects a stale, terminal, prepublication-failed,
  already-decided, or retrying attempt, atomically sets
  `producer_media_published=true` before replying yes, and records the admitted
  object frontier when applicable;
- `attempt_status` carries an exact-attempt classification but no media body.
  The actor rejects stale or terminal state without closing retry, claiming
  first-media ownership, renewing the lease, or advancing a frontier; and
- `protocol_only` is reserved for a live-generation protocol response that
  contains no producer-object-derived fact. It records no publication, lease
  renewal, or frontier progress.

The following classification is normative:

| Prepared response | Binding | Retry effect |
|---|---|---|
| Synthesized master using only a frozen presentation contract | `generation_metadata` | none |
| Master whose body depends on attempt-produced init, playlist, or timeline data | `attempt_media` | consumes availability |
| High-tier master route returning the live media-playlist envelope | `attempt_media` | consumes availability |
| Video media playlist | `attempt_media` | consumes availability |
| Subtitle media playlist or VTT segment | `attempt_media` | consumes availability |
| Init or media segment, any satisfiable range, or `304` existence confirmation | `attempt_media` | consumes availability |
| Unsatisfiable `416` carrying the opened object's length or ETag | `attempt_status` | none |
| Internal init or playlist probe that produces no response | no admission | none |

Subtitle declarations may be generation metadata inside a frozen master.
Subtitle playlists and VTT bodies are attempt media because their windows and
timelines depend on the exact video attempt. Unknown or unproved response
dependencies default to `attempt_media`.

The session generation freezes a `PresentationContract` before serving stable
metadata. It contains every master-affecting source, track, codec, HDR,
dimension, bitrate, frame-rate, URL-topology, and high-tier-collapse fact.
Request-time master rendering cannot reread a mutable library row or probe the
current attempt's init. The initial policy and validated retry recipe carry the
same presentation-contract fingerprint, and the actor compares it at Retry
emission and admission. A mismatch makes transparent same-generation retry
illegal.

Cancellation after attempt-media authorization remains publication because
bytes might have escaped. Cancellation after generation-metadata authorization
does not affect retry eligibility. Direct and guessed segment paths use the
same admission. Response-EOF media commit remains a separate token-authorized
lease and fetched-frontier fact. Compatibility playlist projection occurs only
after actor authorization and never authorizes a response itself.

The manager confirms exact `Arc<Session>` registry identity before and after
the actor reply and returns an opaque, non-clone authorization token. Buffered
completion and streamed EOF carry that token instead of reconstructing
authority from strings. Rolling cached sessions use the actor path; immutable
VOD responses use the same manager facade with VOD-registry authorization.

Retry emission, retry admission, and final successor installation recheck the
same actor state. Attempt-media authorization at or before the armed deadline
is applied before final cutoff and prevents `Retry`; authorization after the
deadline loses to the retained decision. A `Retry` decision that linearizes
first prevents a predecessor body from becoming visible. Generation metadata
does not participate in that retry race and remains admissible during a
compatible in-place retry while the generation remains live.

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
  proposal: None | ActionProposal,
  cleanup: ProducerFailureCleanup {
    kind: producer_failure_cleanup,
    cleanup_policy: retain_published | discard_prepublication
  }
}
```

Reasons are exhaustive and typed: `startup_deadline`, `progress_deadline`,
`exit_classification_deadline`, `process_exit`, `partial_success_exit`,
`unsupported`, `invalid_configuration`, `reader_failed`, `flow_stop_failed`,
`flow_resume_failed`, `flow_stop_deadline`, `flow_resume_deadline`,
`install_deadline`, or `executor_lost`. A physical action timeout or cleanup
error is always recorded separately as `last_action_failure`. If an immutable
decision already exists, execution failure cannot rewrite its reason, decision
sequence, cleanup policy, or proposal identity. If no decision exists yet, the
per-action settlement table below emits exactly one typed decision rather than
leaving a fenced producer with no disposition.

`ProducerDecision::Fail.cleanup` is fixed when the decision is emitted.
Pre-publication failure uses `discard_prepublication`; its cleanup kills and
confirms reap before deleting scratch and resetting compatibility projections.
Post-publication failure uses `retain_published`; its cleanup kills and confirms
reap but cannot delete, truncate, rename, or reset the published generation's
scratch and catalogs. A deadline on that producer-failure cleanup action
applies the same recorded policy. It never performs a generic
scratch-directory deletion.

The actor owns one decision slot. Writing it is an in-memory actor mutation;
the actor then calls `Notify::notify_one`, which never awaits and retains a
permit. The session executor asks the actor for the decision after the last
sequence it acknowledged. Taking work does not erase the slot. It remains
redeliverable until an exact `DecisionApplied` or terminal transition settles
it, so a spurious wake or duplicate event cannot mint another action.

External work is represented exactly:

```text
PendingProducerAction
  action_sequence
  kind: signal_stop | signal_resume | retry | install |
        producer_failure_cleanup | terminal_cleanup
  attempt
  deadline
  origin: independent | initial | decision_sequence | terminal_sequence
  cleanup_registration
  cleanup_policy: none | retain_published | discard_prepublication
```

Exit classification instead uses:

```text
PendingProducerProbe
  probe_sequence
  attempt
  state: queued | reading
  progress_deadline
```

Its result must match probe sequence and attempt. Terminal or producer-decision
preemption advances the probe sequence and aborts the I/O future; any late
result is observation-only. Settlement clears `pending_probe` and records the
bounded `last_probe_outcome` retained for status. The progress deadline alone
decides classification failure.

Every acknowledgement carries `action_sequence`, kind, and attempt. One
nonterminal action is active at a time. New flow evaluations coalesce into the
newest desired revision while a signal is outstanding; its acknowledgement is
applied first, then the actor issues another signal only if the desired state
still differs. A producer decision cancels an outstanding flow action,
increments the action sequence, and occupies the slot with retry/failure work.

End, authority fence, and playback-lease expiry preempt every nonterminal
action: they revoke its authorization under the transition fence, mark its
cleanup registration cancelled, advance `action_sequence`, and install
`terminal_cleanup`. A late flow, probe, retry, or install acknowledgement whose
sequence/kind/attempt no longer matches is observation-only and cannot clear a
deadline or mutate state. Terminal cleanup never waits for the action it
preempted.

Every pending external action creates one actor-owned
`ProducerActionDeadline`. This is a lifecycle/transaction bound, not a second
progress verdict: it is armed only while an actor-issued flow signal, retry,
install, producer-failure cleanup, or terminal cleanup awaits physical
acknowledgement. Classification is excluded:
`ProducerProgressDeadline(classifying_exit)` is its sole verdict timer, and its
one-shot I/O uses that same instant only as a cancellation bound. I/O
cancellation cannot independently change the outcome. The operation kind
supplies a bounded budget; retry's overall budget encloses smaller kill/reap,
directory-clear, actor-reply, and proxy-slot wait budgets. Expiry fences the
actor, records the bounded action failure without changing the producer
decision when one exists, and applies the exact settlement below. It cannot
request another recipe or compete with a progress decision.

Deadline priority is exact: an already-due session lifecycle terminal wins
first; `ProducerActionDeadline` wins a same-instant tie with running progress;
then `ProducerProgressDeadline` is evaluated. A progress decision that becomes
due strictly before an outstanding flow-action deadline cancels that flow
action as above. In `classifying_exit` there is no action deadline to compete.

Action-deadline settlement is exhaustive:

| Pending action | Required settlement at its exact deadline |
|---|---|
| `signal_stop`, no decision | Fence the signal token, emit one `Fail(reason=flow_stop_deadline)` using current publication state, then run its exact producer-failure cleanup. |
| `signal_resume`, no decision | Fence the signal token, emit one `Fail(reason=flow_resume_deadline)` using current publication state, then run its exact producer-failure cleanup. |
| initial `install`, no decision | Reject publication/manager registration, emit one pre-publication `Fail(reason=install_deadline, cleanup_policy=discard_prepublication)`, and return a typed `producer_action_failed` create response. |
| retry or retry-origin `install` | Preserve the retry decision and original producer reason; record `last_action_failure`, settle `prepublication_failed`, and terminate/reap every registered predecessor or candidate before policy-authorized discard. |
| `producer_failure_cleanup` | Preserve the exact `Fail`, disposition, proposal, and scratch policy; record the cleanup timeout and keep registered termination/reap active. |
| `terminal_cleanup` | Preserve End, authority-fence, or lease-expiry terminal cause and acknowledgement replay; record the cleanup timeout and keep registered termination/reap active. |

If a decision or terminal transition preempted the action first, the old action
deadline is stale and observation-only. For a live winning action deadline,
the actor advances the action token before requesting cleanup, so a late
physical acknowledgement cannot revive or relabel the producer. Status and
HTTP projections are deterministic: a newly created pre-publication failure is
`prepublication_failed`; a newly created post-publication flow failure is
`producer_ended_with_proposal`, preserves admitted bytes, and returns typed
`producer_ended` only beyond its frontier. Action-failure detail is additional
evidence, never a substitute for the immutable producer or terminal cause.

The executor is registered before the initial producer deadline can arm. Its
join monitor holds only a weak session reference. Unexpected receiver closure,
task return, or panic publishes `ExecutorLost`; the actor terminally fences new
work and uses the registered exact-attempt `SupervisorCleanupHandle` to request
nonblocking emergency termination. The supervisor confirms reap; the manager
cleanup owner then releases process permits and applies the registered cleanup
policy. It never infers scratch deletion from executor loss. Terminal lifecycle
actions have priority
over the slot, cancel it without waiting for the executor, and use the same
supervisor termination path, so there is no actor/executor circular wait.

The actor registers an abort handle for the executor and a manager-owned
cleanup registry outside that task. Creating a PID-safe supervisor returns two
different values:

- one move-only `AttemptChild` installed lease, whose terminating `Drop`
  behavior remains unchanged and which the executor later moves into
  `Session.child`; and
- one cloneable `SupervisorCleanupHandle` containing attempt identity, command
  sender, and terminal receipt. Its `Drop` has no process side effect, and its
  terminate/reap request is idempotent.

M4 introduces one node-local hard `RollingProcessCapacity` for the compatibility
rolling presentation. `playback.rolling_process_limit` is read at daemon start,
defaults to 64, and accepts 4–256; changing it requires restart so shrinking the
limit cannot orphan existing permits. One slot covers one spawned rolling
process and its cleanup-registry entry. It includes copy producers, hardware
and software transcodes, initial attempts, and pending retry candidates. VOD's
separate producer scheduler is outside this limit. `active_session_count` and
cluster `session_pressure_limit` remain telemetry/scoring only and are
explicitly not admission bounds.

Before every process spawn the executor must acquire one
`RollingProcessPermit` and atomically reserve its fixed manager-owned cleanup
registry entry. If no slot is available, admission fails before process spawn,
scratch publication, or session registration; a child can never exist without
a registry entry. A `RollingSpawnReservation` drop guard owns the empty entry
and permit until spawn returns. A no-process spawn error removes the entry and
releases the permit; a successful spawn synchronously fills the entry before
the next await, so cancellation cannot strand an unregistered child. The
executor registers the `SupervisorCleanupHandle`, rolling-process permit, and
any hardware/software admission permit in that entry before actor admission or
proxy-slot acquisition, while retaining the move-only installed lease locally.
Copy has no encoder-family permit but always has the rolling-process permit.

After installation the Session owns the installed lease and the registry keeps
the separate cleanup handle and process-capacity resources. An entry survives
actor, executor, and Session retirement until its terminal receipt confirms
reap. Only that receipt may remove the entry and release its rolling-process
and hardware/software admission permits.

Capacity exhaustion is explicit compatibility behavior:

- an initial local create returns typed HTTP 503
  `rolling_process_capacity` with bounded retry guidance, leaves no registered
  session or scratch generation, and settles its `request_id` claim as that
  replayable result;
- cluster placement advertises zero rolling capacity for that node so another
  eligible owner may be selected, but the local manager still enforces the
  permit and never trusts scoring as admission;
- a pre-publication retry that cannot acquire a second process slot spawns
  nothing, records `last_action_failure=rolling_process_capacity`, and settles
  the original producer decision as `prepublication_failed`; and
- no path silently changes recipe, swaps to VOD, waits without a deadline, or
  exceeds the hard process bound.

Executor-task and action-concurrency permits are different resources: they may
be released as soon as an action settles or is aborted. Rolling-process,
cleanup-registry, and hardware/software admission permits are never released
on abort, action-deadline expiry, Session retirement, or a best-effort kill
request. A supervisor whose reap cannot be confirmed therefore continues to
consume capacity and is visible as `reap_unconfirmed`; the full registry
refuses subsequent spawns instead of allowing an unbounded retired-process
tail.

The registry can therefore reach both the installed supervisor and a pending
candidate without cloning a terminating lease. If an executor stays alive but
a step exceeds
`ProducerActionDeadline`, the actor fences the operation, aborts the executor,
asks every registered supervisor to terminate and reap, releases only the
executor/action permits, preserves the original producer decision and scratch
policy, and wakes manager cleanup. The registry retains each process-capacity
and admission permit until confirmed reap. Faults in cleanup are reported as
action failures but cannot reopen actor authority or change the immutable
producer decision.

### 4.2 Pre-publication retry

`Retry` is legal only when no attempt-bound producer-media response has been
publication-authorized and the generation retry token is available for this
reason. Generation-stable metadata authorization does not consume or suppress
the retry. Emission consumes that token atomically. The process executor owns
an explicit transaction:

1. kills and reaps the exact failed attempt, if it is still present;
2. clears the name-reusing session directory and verifies it is empty;
3. resets the compatibility catalog/frontiers and releases predecessor scratch
   byte accounting only after the verified clear;
4. calls the distinct `AdmitProducerRetry` command, which rechecks that no
   attempt-media response was authorized and allocates a successor with retry
   state `consumed`;
5. spawns the immutable validated recipe into a new exact-attempt supervisor;
6. asks the actor to authorize final exact-attempt proxy installation;
7. installs the proxy under the process-slot mutex; and
8. acknowledges the decision.

If any step fails or the session terminates, the candidate is killed and
reaped. Cancellation before actor retry admission leaves the killed
predecessor but no mixed catalog and settles as a typed prepublication failure.
Cancellation or failure after admission terminally fences that admitted
attempt; it never reopens predecessor paths. A transaction drop guard publishes
that settlement synchronously. A failed retry cannot recursively request
another recipe.

Copy `Unsupported` follows this same path. It is an observation about the
current pre-publication attempt, not permission for the reader task to mutate
the child.

### 4.3 Copy and exit classification

Copy exit classification is frozen per attempt. A custom-segmenter attempt
registers `exit_classifier=copy_reader`; a direct or fallback HLS-muxer attempt
registers `exit_classifier=immediate`. Every supervisor publishes the exact
process exit fact. For `copy_reader`, the actor retains that fact until the
reader publishes exactly one of `unsupported`, `invalid_configuration`,
`reader_failed`, or `completed` through the same sequenced ingress; every
current direct `Session::fail` in that task is removed. For `immediate`, a
non-success exit is decisive, while a zero exit still requires the same
ENDLIST/frontier completion proof as every other managed producer.

`unsupported` and invalid/reader failure are decisive even if the process exit
arrives later. If exit arrives first, it is retained until the classifier fact.
If the reader task exits or panics without classifying, its join monitor
publishes `reader_failed`. The first exact fact among `completed` and process
exit starts one five-second `classifying_exit` rendezvous. Both facts must
arrive within it before the actor asks for the one-shot completion probe;
missing either fact yields `exit_classification_deadline`. A late generic exit
cannot consume a retry or replace the typed reason. Model tests cover all
orderings of successful and non-success exit, every copy outcome, completion
probe, and the producer deadline.

### 4.4 Post-publication failure

Once attempt-bound producer media has been published for the generation,
timeout, non-success exit, or `unsupported` cannot replace its producer in
place. Generation-stable metadata alone does not close the validated retry
window. The actor emits one `Fail`, the executor kills/reaps only the exact
failed attempt, and the actor retains one internal proposal:

```json
{
  "proposal_id": "UUID generated once and retained by the actor",
  "kind": "replace_failed_producer",
  "reason": "progress_deadline",
  "source": "server",
  "severity": "required"
}
```

Until M5.5 stages a real successor, this is an `ActionProposal`, not a
`ControlAction::PrepareReplacement`: there is no successor URL or safe switch
boundary to send yet. M4 therefore keeps the v1 wire action as `none`; existing
local, relay, web, and passive-client validators remain truthful and cannot
enter an equal-sequence retry loop. M5.5 will atomically turn a retained
proposal into a bounded wire action with an action UUID, successor, boundary,
strict relay validation, passive parsing, and exact-sequence replay before it
can be emitted.

An already-authorized response body may finish, and init plus numeric media at
or behind the frozen committed frontier remain readable. The actor uses
distinct `prepublication_failed` and `producer_ended_with_proposal`
dispositions. Only the former projects `Session::failed`. In the latter state,
every fresh video-playlist authorization is rejected, as are full, range, and
conditional media requests beyond the frozen frontier; each fails promptly
with typed `producer_ended` instead of waiting for bytes that can no longer
appear. M4 must not point the generation at another child, reset its sequence,
or overwrite its scratch directory.

Successful natural exit is not completion by itself. Its immediate one-shot
probe must publish a parsed exact-attempt `#EXT-X-ENDLIST`, an indexed final
frontier, and the initial policy's expected remaining duration. Completion is
valid only when the endlist and frontier are attempt-fenced and the frontier is
within one target duration of the expected remaining media duration. Otherwise
a zero exit is `partial_success_exit` and follows the same pre/post-publication
failure rule. Exit classification uses the same producer deadline in
`classifying_exit` phase; it does not create another progress watchdog. When
expected duration is unavailable, ENDLIST plus a valid nonempty final frontier
yields `complete_unverified_duration` and is instrumented separately.

### 4.5 Proposal identity and later wire replay

The proposal UUID is generated once and retained beside immutable generation,
attempt, decision sequence, reason, and evidence. Later observations for the
failed attempt do not change it. M4 status returns that same proposal snapshot,
while control exchanges continue to atomically accept/replay `action:none`.
M5.5 adds durable preparation, wire action selection inside sequence
acceptance, byte-equivalent replay, and action acknowledgements.

## 5. Process and lifecycle ownership

The per-attempt supervisor is the sole OS-process owner. A single session
executor orchestrates initial registration and retry, and mutates only the
`AttemptChild` proxy slot. Request handlers may read actor snapshots and commit
media facts but cannot replace a proxy. Flow evaluation submits one revised
desired state to the actor; only the exact supervisor signals its reserved PID
and publishes the physical acknowledgement.

`child_transition` can then be deleted:

- producer installation is fenced by exact attempt plus the actor's existing
  synchronous terminal/install boundary;
- End, authority fence, and lease expiry first close actor authority, then ask
  the exact supervisor to kill/reap its owned child without waiting on the
  executor;
- publication and fetch paths use exact actor response admission;
- an exit from a killed predecessor is merely a stale exact-attempt fact; and
- index refresh and retention prepare storage observations outside locks, then
  merge only if actor attempt plus catalog revision still match.

The proxy-slot mutex and segment/resource mutexes remain. They protect owned
data, not recovery election; none can authorize a retry. Initial producer
construction creates the supervisor first, registers its separate
`SupervisorCleanupHandle` with the actor/manager cleanup registry, authorizes
installation, moves the sole `AttemptChild` installed lease into the Session,
and only then publishes the Session in the manager. Rejection drops the
move-only lease, while the retained cleanup handle confirms that its supervisor
terminates and reaps the candidate.

The action executor must be cancellation-safe. Dropping an HTTP request or
flow waiter cannot cancel a decision after the actor emits it. Actor shutdown
settles the slot, revokes signal/install authority, requests supervisor
termination, and waits for neither executor I/O nor an executor-owned lock.

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

- producer phase: `disarmed`, `starting`, `running`, `hold_requested`, `held`,
  `classifying_exit`, `decided`, `complete`, or `terminal`;
- progress-deadline mode and remaining milliseconds when armed;
- pending action kind, action sequence, attempt, deadline remaining, cleanup
  registration state, and latest terminal action outcome;
- pending probe sequence, attempt, active state (`queued` or `reading`),
  progress-deadline milliseconds remaining, and last settled outcome
  (`published`, `cancelled`, or `stale`);
- current attempt, `metadata_response_authorized`, and
  `producer_media_published`;
- retry state: `unavailable`, `available`, or `consumed`;
- immutable last typed producer-decision reason and decision sequence;
- completion verification: `verified_duration`, `unverified_duration`, or
  `incomplete`;
- outstanding proposal ID/type;
- executor state, pending-decision age, last acknowledged sequence, and
  separate `last_action_failure` class without relabeling the producer
  decision;
- flow revision plus desired/applied flow state; and
- rolling-process slots used/available/limit, cleanup-registry entries, and
  `reap_unconfirmed` count.

The timer inventory also exposes the two actor-owned server bounds:

- `ProducerProgressDeadline` detects lack of producer progress and provides
  its bounded `classifying_exit` phase; and
- `ProducerActionDeadline` bounds one actor-issued physical transaction so a
  live but wedged executor or signal path cannot strand ownership.

The latter is not a recovery voter: expiry can only fail/fence the already
chosen action and invoke its registered cleanup. It cannot choose a retry or
replacement.

Prometheus metrics are bounded-label:

- `plurx_playback_producer_deadline_total{phase,outcome}`;
- `plurx_playback_producer_decision_total{kind,reason}`;
- `plurx_playback_producer_retry_total{outcome}`;
- `plurx_playback_producer_action_total{outcome}`; and
- `plurx_playback_rolling_process_slots{state="used|available|reap_unconfirmed"}`.

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

The check also enumerates the approved progress deadlines for the current
milestone:

1. rolling `ProducerProgressDeadline`;
2. VOD `SegmentMaterializationDeadline`.

M5 updates the allowlist when it actually adds the client
`PlaybackProgressDeadline`; M4 must not pre-approve a symbol that does not yet
exist.

The lifecycle allowlist separately names `ProducerActionDeadline` and checks
that it is constructed only by the rolling actor around an already-issued
physical transaction. It is forbidden in request handlers, supervisors, and
the session executor.

Lifecycle and network timers are listed separately so a useful bound is not
misclassified as a watchdog.

## 9. Race and failure matrix

| Ordering | Required result |
|---|---|
| delayed A/B/C where every advancing publication is within the deadline established by its predecessor | one contiguous coverage batch proves the chain and rearms through C |
| delayed A/C with B absent and C after A's resulting deadline | first gap freezes coverage; C cannot bridge the real stall |
| advancing A followed by speed-only B after the original deadline | A remains coverage evidence and rearms; B neither erases nor extends it |
| progress published after the armed deadline | one timeout decision |
| producer batch, command barrier, producer batch | batches are evaluated on their own sides of the command; coverage never crosses it |
| progress A, successful physical hold acknowledgement, progress B | A is applied before hold; B is a distinct held-phase batch and cannot rearm |
| progress A, successful physical resume acknowledgement, progress B | resume grants its fresh budget before the distinct B batch is evaluated |
| progress A, exact exit, progress B | exit enters classification before B; B cannot extend the exited attempt |
| progress A, classifier/probe result, progress B | classification settles before B; late progress cannot relabel or rearm it |
| command reserved before deadline but published after | post-deadline command; cannot reverse verdict |
| deadline becomes due while receive branches are ready | due-first cutoff runs before ordinary dispatch |
| timer polls pending, deadline passes, then receive wakes | selected envelope retained; cutoff runs before handling it |
| deadline becomes due between two retained deque items | due-first cutoff runs before the second item |
| classified exit published at or before the armed deadline plus deadline | exit reason wins; one decision |
| classification published after the armed deadline | deadline reason wins; later classification cannot duplicate |
| due End/authority fence/lease expiry plus producer deadline | lifecycle terminal wins; no producer decision |
| response admission plus retry decision | whichever actor event linearizes first; never both visible bytes and retry |
| pause or physical hold acknowledgement published at or before the armed deadline | disarmed; no decision |
| SIGSTOP live error | running deadline remains until one bounded retry settles or typed failure wins |
| SIGCONT live error | held state remains until one bounded retry settles or typed failure wins |
| signal command/reply blocks before a decision | action deadline emits one typed flow-deadline failure, fences the token, and supervisor cleanup owns the process |
| initial install blocks before a decision | typed pre-publication install failure; no session registration or unowned child |
| retry/install/cleanup blocks after a decision | original decision and proposal remain immutable; action failure is separate evidence |
| resume after a long hold | fresh full progress budget |
| publication just before failure | no retry; one proposal |
| retry decision then End | candidate is killed; no install |
| End then retry decision | no decision emitted |
| flow action, then End, then late flow ack | terminal cleanup token wins; late flow ack is observation-only |
| exit probe, then terminal event | probe is cancelled; late classification cannot relabel terminal state |
| retry/install, then authority loss | authority cleanup reaches pending supervisor; late install is rejected |
| copy `Unsupported` and timeout | one pre-publication retry total |
| copy `Unsupported` and generic exit | Unsupported classification owns the reason and sole retry token |
| zero exit without valid ENDLIST/frontier | typed partial-success failure, never complete |
| post-publication failure cleanup expires | terminate/reap continues; published scratch and catalogs remain readable; original decision reason remains |
| pre-publication failure cleanup completes | reap is confirmed before scratch is discarded and compatibility projections reset |
| rolling process/cleanup registry full before initial copy or transcode spawn | typed 503 admission failure without process, scratch generation, or registered session |
| rolling process/cleanup registry full before retry spawn | original decision settles pre-publication failure; no candidate process |
| terminate requested but reap remains unconfirmed | registry, process-capacity, and hardware/software admission permits remain held and observable |
| predecessor exit after retry install | stale fact rejected |
| executor loss with pending decision | actor fences; supervisor termination and manager cleanup proceed without executor |
| executor hangs at kill, clear, admission, or proxy install | action deadline aborts it and cleanup registry reaches every installed/pending supervisor |
| exact control replay in M4 | byte-equivalent `action:none`; stable proposal remains visible in status |

## 10. Verification order

Per the delivery instruction, implementation is reviewed adversarially before
unit tests run.

1. Generate the checked source catalog of every child/recovery/failure owner
   and map each current `child_transition` invariant.
2. Implement actor state, exact deadline, decision channel, and model tests.
3. Implement the process executor and migrate transcode/copy paths.
4. Delete the old tasks, locks, atomics, and replacement helpers.
5. Add status, metrics, and the repository ownership check.
6. Open or update the implementation PR at the frozen cumulative head.
7. Obtain adversarial review of that exact PR head; fix every finding and
   repeat exact-head review before running unit tests.
8. Run the full local unit suite once on that reviewed merge candidate. If it
   fails, repair the cause and rerun only the failed or directly affected
   tests; do not restart the full suite.
9. Review every behavioral repair and restore exact-head approval.
10. Run the required non-unit cluster/integration gate because terminal,
    owner-fence, and producer-action ordering cross cluster ownership.
11. Require every hosted job to pass and merge only while the reviewed head is
    unchanged. Hosted PR validation remains required and is distinct from the
    single local full-suite run.

## 11. Acceptance

M4 is complete only when all of the following are true:

- the actor owns the sole rolling producer deadline;
- fault injection proves one and only one pre-publication retry;
- copy `Unsupported` consumes that same retry right;
- every successful or bodyless response resolved from a live rolling
  generation crosses exact actor publication admission with a
  dependency-correct `generation_metadata`, `attempt_media`, or
  `protocol_only` binding; actor-derived startup/failure and no-actor errors
  follow their explicit typed-error fences;
- master-only traffic leaves retry available, while every attempt-media
  response atomically closes the retry window before it becomes visible;
- no post-publication event swaps or restarts the child;
- a post-publication failure yields exactly one stable action proposal;
- M4 emits no incomplete `prepare_replacement` wire action;
- held or paused time cannot cause a producer timeout;
- only a successful physical resume acknowledgement arms a fresh budget;
- failed or blocked hold/resume signals settle in a typed failure and cannot
  leave unbounded running/stopped production;
- terminal and attempt fences reject every late install, signal, and event;
- executor loss fences and terminates without stranding a process;
- executor step expiry reaches pending as well as installed supervisors;
- every action-deadline kind has one typed settlement whether or not a
  producer decision existed first;
- the hard rolling-process limit includes copy, transcode, and retry
  candidates, and no permit returns before confirmed reap;
- every external acknowledgement is fenced by action sequence, kind, and
  attempt, and terminal preemption cannot be undone;
- zero exit needs exact ENDLIST/frontier proof to become complete;
- all deleted symbols and detached recovery loops are absent;
- the status surface names deadline, retry, decision, proposal, flow, and
  executor state;
- adversarial review has no unresolved P0-P3 finding; and
- focused, full local, cluster, and hosted gates are green at the exact merged
  head.
