# Playback control M3c3 — terminal event ownership

**Status:** implementation contract
**Baseline:** merged PR #616 at `8c6ccdf7`
**Scope:** terminal ownership for rolling compatibility and VOD session
handles, including the protocol's orderly client release

## Outcome

M3c3 gives one rolling-session actor the sole right to decide that a generation
is terminal, and gives a VOD attachment one lifecycle-guarded equivalent for
the protocol's orderly client release. The rolling actor receives three
bounded causes:

1. `end` — the viewer, operator, replacement coordinator, or local lifecycle
   path deliberately ends the generation;
2. `authority_fence` — durable ownership or cluster serving authority is no
   longer valid on this node; and
3. `lease_expired` — neither valid control nor authenticated media renewed the
   actor's monotonic playback lease before its deadline.

The first terminal event linearized by the actor wins. It publishes the shared
serving fence and one flow wake, records its immutable cause, and rejects every
later mutation. Late terminal events are acknowledged as already terminal and
cannot overwrite the winning cause. A newly accepted `demand=end` is that
terminal `end` event; it is not a lease renewal or a production hold. Its exact
identity/sequence can replay the same terminal acknowledgement after response
loss. The accepted reply is retained for 60 seconds in replicated storage and
is addressable through the ended durable route, so local cleanup or an owner
handoff cannot erase the retry result. VOD performs the corresponding
tombstone and reader detach under its existing per-session lifecycle gate and
retains the same bounded reply. M3c3
does not kill, replace, or restart a rolling encoder. Existing rolling physical
teardown remains outside the actor until M4 moves that one action owner and
deletes the compatibility watchdogs.

## Why the current retirement flag is insufficient

The merged actor already owns a boolean `retired` transition, but every
non-expiry caller sends the same `Retire` command. That erases the distinction
between an ordinary end and loss of cluster authority. It also makes ordering
tests prove only that a bit became true, not which event won or whether a late
event silently rewrote operational truth.

That ambiguity matters during failover. An operator stop may safely publish a
durable end event; a quorum-loss fence must stop serving immediately and must
not depend on Store I/O. M4 and M8 need the actor to retain that distinction
before they can attach different teardown and handoff actions.

## State and API

The actor stores `terminal: Option<RollingTerminalCause>` where the cause is
one of `End`, `AuthorityFence`, or `LeaseExpired`. `retired` remains as a
compatibility projection for hot serving paths during M3c3, and
`expiration_claimed` remains an exact projection for existing reaper behavior.
Both are derived by the same transition:

```text
live + terminal(cause)
  -> terminal = cause
  -> retired = true
  -> expiration_claimed = (cause == lease_expired)
  -> publish retired fence
  -> request flow evaluation
  -> outcome = won

terminal(existing) + terminal(any)
  -> no state change
  -> outcome = already_terminal(existing)
```

The public actor handle exposes `end()` and `authority_fence()` rather than a
generic `retire()`. Both await an actor verdict. If the actor mailbox is gone,
the existing fail-closed local fence remains the last-resort safety mechanism;
it cannot invent an actor terminal cause and status therefore reports control
unavailable rather than fabricated ownership.

Sequence acceptance and `demand=end` terminalization are one actor command.
The response therefore cannot report an accepted End while the actor remains
live. Once End wins, the only accepted control operation is replay of that
exact generation, owner epoch, client instance, platform, and sequence. A
higher sequence or different identity receives `session_ended`.

VOD applies the same rule under its per-session lifecycle gate. A fresh End
stores the accepted terminal result, writes the `Deleted` tombstone, and
starts one session-owned reader cleanup task. The owner exchange waits for
that task before forming the reply. The tombstone-to-cleanup handoff contains
no suspension point, so disconnect or timeout cannot cancel the cleanup after
End has linearized. Exact replay, ordinary end, and maintenance join the same
idempotent cleanup. The durable acknowledgement is attempted only after the
reader has detached; a replay can therefore never observe an ended route while
the old VOD reader remains attached. Tombstones created by supersession,
operator stop, revocation, or file replacement have no client acknowledgement
to replay.

Before a successful terminal HTTP response is exposed, one session-owned
continuation inserts its complete bounded `ControlResponseV1` immutably into
replicated storage under the generation, session, owner node/epoch, client
instance, sequence, and a canonical digest of the complete parsed request.
That same Store transaction fences the exact owner route as ended, clamps its
job lease, removes its playback pointer, and releases its cache pin. There is
no crash window in which the reply exists while the route remains eligible for
takeover. The row survives route settlement and expires after 60 seconds. Both
transient Store failures and unknown write outcomes are retried for a bounded
window with these same immutable bytes; read-after-unknown recognizes a write
whose reply was lost without manufacturing a second response timestamp. Both
public ingress and the exact-write cluster endpoint consult it before active
route and rate-limit rejection, validate the complete response against the
exact terminal request, and return it as a replay. A different identity,
sequence, request payload, owner tuple, active demand, malformed body, or
expired row receives the ordinary terminal/stale verdict and cannot consume
the record.

## Routing table

| Source | Actor event | Why |
|---|---|---|
| accepted control `demand=end` | `end` plus exact replay record | the protocol's orderly release is a terminal state transition, never a lease renewal |
| viewer release outside control, operator stop, supersession, local replacement cancellation | `end` | local lifecycle intentionally ends this generation |
| durable route loss, takeover fencing, quorum/readiness loss | `authority_fence` | this node can no longer prove serving authority |
| actor monotonic timer or atomic expiry claim | `lease_expired` | playback demand disappeared past the negotiated deadline |
| producer exit | producer observation only | M3c3 records it but does not choose recovery |
| manager child kill and scratch removal | no new event | physical effect remains compatibility-owned until M4 |

Serving-fence teardown first sends `authority_fence`; its later generic cleanup
may send `end`, which must return `already_terminal(AuthorityFence)` without
changing the cause. A normal stop sends only `end`. A registration rejected by
the serving gate is authority-fenced even though it was never inserted in the
manager map.

## Ordering and cancellation

Mailbox arrival is the linearization order for `end`, `authority_fence`, and
explicit expiry claims, except that a deadline already due when a terminal
command acquires the producer-transition mutex is claimed first. Timer
scheduling therefore cannot let a later End or authority fence steal an
already-expired terminal cause. The same mutex prevents a producer install or
signal from crossing that decision.

Producer progress and exit use their independent constant-space ingress. The
actor drains already-published producer facts before a dequeued lifecycle
command, preserving the M3c2 observation contract. Neither producer event is a
session terminal action in this slice. After a terminal event wins, both are
rejected and cannot mutate the frozen snapshot.

An actor command whose reply is already closed, or whose inherited absolute
deadline is due before actor admission, mutates nothing. Once the actor accepts
End, however, it synchronously installs one session-owned result receipt and
starts its continuation before attempting the fallible reply send. Exact
retries attach to that same receipt. For rolling delivery the continuation
holds a terminal-response handoff fence through physical-state joining and the
atomic replicated commit; the reaper observes that fence under the existing
child-transition gate. VOD reserves its receipt under the lifecycle gate,
detaches the reader, and only then starts the same durable commit. Accepted End
therefore settles even when no HTTP waiter remains, while queued nonterminal
work can never renew state after its caller's deadline.

## Exhaustive model

A small deterministic in-memory ordering model enumerates permutations of:

- current-attempt producer exit;
- lease expiry;
- accepted control End;
- authority fence;
- publication;
- replacement admission; and
- control acceptance.

For every explored order it proves:

1. at most one terminal event returns `won`;
2. the winning cause is immutable and equals the first terminal event that
   linearized while live;
3. exactly one terminal transition publishes the logical serving fence;
4. control, media commit, publication, progress, exit, and replacement
   admission cannot mutate state after terminal;
5. producer exit before terminal remains attributable, while producer exit
   after terminal is rejected;
6. end after authority fence cannot relabel failover as a normal stop;
7. expiry after either explicit event reports the existing terminal snapshot;
8. a terminal event that reaches actor acceptance transfers continuation
   ownership before a subsequently lost reply; and
9. no ordering creates a second producer attempt after terminal.

The implementation tests compare actor transition results with this model and
assert every late typed terminal result returns the retained winner. The model
uses the production control-End and expiry-claim transitions; authority-fence
ordering remains a synchronous state-model operation. Separate real
async-handle and manager tests exercise the actual mailbox commands, exact
deadline precedence, accepted End acknowledgement/replay, response loss,
rolling status joining, VOD tombstone/detach, cancellation-independent reader
cleanup, settled-route replay for rolling and VOD, terminal response decoding
through the actual cluster-relay path, and concurrent end/fence races. A
backend-neutral Store contract proves the acknowledgement and exact-route end
are atomic, a crash cannot permit takeover, the reply is immutable, it expires
at the exact retention boundary, and maintenance prunes it. Deterministic
regressions drop the actor reply from inside terminal admission, attach two
simultaneous exact retries while the continuation is paused, run the reaper,
cancel the original HTTP task, inject a first Store failure for both rolling
and VOD route shapes, and still require one durable reply plus an ended route.
A VOD regression observes reader ownership at commit start. Never-polled,
closed-waiter, and expired-deadline exchanges prove pre-admission cancellation
has no later mutation.

## Instrumentation

Activity/status exposes the actor's bounded terminal cause through
`lease_state`: `active`, `ended`, `authority_fenced`, `expired`, or
`unavailable`. No free-form reason or node identifier becomes a metric label.

Prometheus adds:

```text
plurx_playback_rolling_terminal_events_total{event="end",outcome="won"}
plurx_playback_rolling_terminal_events_total{event="end",outcome="already_terminal"}
plurx_playback_rolling_terminal_events_total{event="authority_fence",outcome="won"}
plurx_playback_rolling_terminal_events_total{event="authority_fence",outcome="already_terminal"}
plurx_playback_rolling_terminal_events_total{event="lease_expired",outcome="won"}
plurx_playback_rolling_terminal_events_total{event="lease_expired",outcome="already_terminal"}
```

The existing aggregate expiry and non-expiry retirement counters remain during
M3c3 for dashboard continuity. M4 may remove them only after consumers move to
the typed metric.

## Rollout, rollback, and M4 boundary

M3c3 changes no wire shape, media URL, or rolling physical recovery action. It
corrects the already-defined meaning of `demand=end`: successful delivery now
ends the local generation/attachment and returns `lease.state=ended`. Mixed
nodes remain wire-compatible, although an older owner may retain a session
until its lease expires after acknowledging End. Rollback restores the generic
`Retire` command and the old `retired` status label; it does not need to
recreate a watchdog. The 60-second terminal-ack expiry is a bounded idempotency
retention window: it makes no liveness or recovery decision and is not a
watchdog.

M4 may begin only after this slice is merged and green. M4 then:

1. introduces the one actor-owned `ProducerProgressDeadline`;
2. routes the resulting typed recovery proposal through this terminal/action
   owner;
3. removes first-segment/software-grace and progress watcher decisions;
4. deletes `child_transition`, `watchdog_active`, `replacing_child`, and the
   in-place post-publication fallback paths; and
5. updates the watchdog ledger with every remaining deadline and its single
   owner.
