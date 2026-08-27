# Playback control M3c3 — terminal event ownership

**Status:** implementation contract
**Baseline:** merged PR #616 at `8c6ccdf7`
**Scope:** behavior-passive terminal ownership for rolling compatibility
sessions

## Outcome

M3c3 gives one rolling-session actor the sole right to decide that a generation
is terminal. The actor receives three bounded causes:

1. `end` — the viewer, operator, replacement coordinator, or local lifecycle
   path deliberately ends the generation;
2. `authority_fence` — durable ownership or cluster serving authority is no
   longer valid on this node; and
3. `lease_expired` — neither valid control nor authenticated media renewed the
   actor's monotonic playback lease before its deadline.

The first terminal event linearized by the actor wins. It publishes the shared
serving fence and one flow wake, records its immutable cause, and rejects every
later mutation. Late terminal events are acknowledged as already terminal and
cannot overwrite the winning cause. M3c3 does not kill, replace, or restart an
encoder. Existing physical teardown remains outside the actor until M4 moves
that one action owner and deletes the compatibility watchdogs.

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

## Routing table

| Source | Actor event | Why |
|---|---|---|
| viewer release, operator stop, supersession, local replacement cancellation | `end` | local lifecycle intentionally ends this generation |
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
explicit expiry claims. The exact monotonic deadline is also guarded by the
existing producer-transition mutex, so a producer install or signal cannot
cross a terminal decision.

Producer progress and exit use their independent constant-space ingress. The
actor drains already-published producer facts before a dequeued lifecycle
command, preserving the M3c2 observation contract. Neither producer event is a
session terminal action in this slice. After a terminal event wins, both are
rejected and cannot mutate the frozen snapshot.

Dropping an HTTP request or oneshot receiver does not cancel an enqueued actor
command. The actor applies the event even when no caller remains to receive its
verdict. The model therefore includes reply cancellation as an observation,
not as a state transition.

## Exhaustive model

A small deterministic model enumerates permutations of:

- current-attempt producer exit;
- lease expiry;
- explicit end;
- authority fence;
- publication;
- replacement admission; and
- caller cancellation after enqueue.

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
8. an enqueued terminal event still applies when its reply receiver is
   dropped; and
9. no ordering creates a second producer attempt after terminal.

The implementation tests compare actor transition results with this model and
also exercise the real async handle for cancellation and concurrent end/fence
races.

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

M3c3 changes no wire request, media URL, producer policy, or physical recovery
action. Mixed nodes can coexist because typed terminal causes remain local
actor state. Rollback restores the generic `Retire` command and the old
`retired` status label; it does not need to recreate a watchdog.

M4 may begin only after this slice is merged and green. M4 then:

1. introduces the one actor-owned `ProducerProgressDeadline`;
2. routes the resulting typed recovery proposal through this terminal/action
   owner;
3. removes first-segment/software-grace and progress watcher decisions;
4. deletes `child_transition`, `watchdog_active`, `replacing_child`, and the
   in-place post-publication fallback paths; and
5. updates the watchdog ledger with every remaining deadline and its single
   owner.
