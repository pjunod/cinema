# Playback control M3b — explicit demand lease and producer pacing

This slice makes the M3a rolling actor's retained client state authoritative
for liveness and producer pacing. It is the second M3 implementation slice in
[`PLAYBACK-CONTROL-PROTOCOL-PLAN.md`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md).
Project-wide delivery state remains in
[`PLAYBACK-CONTROL-STATUS.md`](PLAYBACK-CONTROL-STATUS.md).

It does not yet claim complete actor ownership of child exit, publication
waiters, progress deadlines, durable end, or cluster-fence events. Those
boundaries remain explicit because they are the prerequisite for deleting the
old recovery watchdogs in M4.

## Outcome

The first newly accepted rolling control sequence now changes the same actor
lease from the 60-second media-only compatibility mode to a 30-second explicit
mode. Control heartbeats and successful capability-authenticated media GETs
renew that one deadline. A command received at or after the deadline expires
and fences the actor before it can renew. The actor also arms its own exact
monotonic deadline, so a session receiving no command is fenced at expiry;
the 15-second repair cadence is cleanup scheduling rather than hidden lease
authority.

The actor's newest accepted demand immediately controls the producer:

- `active` permits production within the explicit high/low watermarks;
- `hold` renews the session but SIGSTOPs the producer, including before its
  first publication;
- a later `active` SIGCONTs it and resets the old producer-motion clock before
  the compatibility watcher can mistake the deliberate hold for a stall; and
- `end` stops production immediately and then naturally expires if the client
  sends no more control or media.

`end` is intentionally not described as a durable terminal transition in this
slice. Correct terminal behavior needs the same idempotent tombstone and
retry ordering across local rolling, VOD, and relayed control. Until that
lands, the server stops work promptly but preserves the M3a lease-expiry
cleanup path.

## Production policy

Every accepted snapshot is validated and actor-owned. For an explicit active
session the server computes:

```text
anchor_ms = seek_target_ms when seeking, otherwise position_ms
runway_ms = max(buffered_through_ms - anchor_ms, 0)
reserve_s = ceil(playback_rate * 30 seconds)
target_s  = clamp(ceil(runway_ms / 1000) + reserve_s,
                  1, configured_hls_ahead_max_seconds)
ahead_s   = (actual_media_origin_ms + published_end_ms - anchor_ms) / 1000
```

The thirty-second reserve is wall-clock protection, so 2× playback receives
sixty media seconds. The existing configured ahead ceiling remains absolute;
a malicious or stale client cannot enlarge it. A zero configured ceiling
continues to mean the time limit is disabled.

The time policy answers a different question from the disk policy. Reported
playhead/runway controls how much media to produce. Published bytes beyond the
authenticated download frontier still control the per-session byte ceiling,
and total live scratch plus drainable fleet reserve still control the global
ceiling. Client observations therefore cannot bypass physical disk safety.

Capacity holds retain priority while active. A demand hold is named
`demand`. When intent changes from `hold` to `active` but a byte, global, or
time release line still applies, the producer remains stopped and its held
reason changes in place. This prevents a brief SIGCONT between two valid hold
owners and makes the transition visible.

Legacy media-only sessions keep the exact old fetched-frontier time policy and
60-second lease until mixed-version cutover. The initial start bootstrap still
advertises 60 seconds because a client has not established explicit mode yet;
the accepted exchange log and live status report the enforced 30-second
timeout after binding.

## Ordering

The actor owns sequence, demand, renewal, expiry, and retirement. The existing
`child_transition` compatibility gate temporarily serializes the physical
SIGSTOP/SIGCONT with:

- accepted control mutation;
- expiry claim;
- explicit/admin/cluster retirement; and
- the old in-place child replacement path.

Flow control always re-reads the newest actor snapshot after it acquires that
gate. A control request may therefore be superseded by a newer request, but a
stale snapshot cannot signal the child after that newer request, an expiry, or
a retirement. M4 deletes the gate only when child transitions themselves are
actor messages.

Equal-sequence replays, rejected controls, expired actors, and unavailable
mailboxes never change demand or signal the producer. A dead mailbox fences
the session for repair cleanup.

## Instrumentation

Live status and Activity detail now expose:

- `lease_mode`, `lease_state`, and the actually enforced
  `lease_timeout_ms`;
- `control_demand`, seek-aware `client_runway_ms`, reported position, and
  render state;
- `production_policy`: `legacy_fetch_frontier`, `explicit_demand`,
  `explicit_missing_demand`, `immutable_vod`, or `unavailable`;
- `production_ahead_seconds` and `production_target_seconds`;
- exact stored `hold_reason` and matching release line; and
- the existing suspend count, physical fetched-frontier reserve, byte totals,
  delivery rate, and producer progress.

The playback overlay names an explicit demand window and distinguishes the
30-second enforced lease from the bootstrap's compatibility lease. Joined
client events persist the same server-side fields. Structured hold/resume
logs include the policy, measured ahead/target, physical bytes, fleet bytes,
and exact reason without a raw session capability.

Prometheus adds
`plurx_playback_rolling_producer_transitions_total{transition,reason}` with
fixed `hold|resume` and `demand|time|bytes|global` dimensions. Lease renewal,
expiry, and retirement counters from M3a remain the liveness view.

## Watchdogs and timers after this slice

| Mechanism | Status after M3b | Why it still exists or when it is removed |
|---|---|---|
| playback session lease | **Retained by design** | A server must reclaim a generation after both explicit control and media consumption stop. The actor is its only authority. |
| 15-second repair/flow tick | **Retained as scheduling** | It refreshes publication, prunes retention, records speed, repairs flow state, and tears down an already actor-fenced expiry. Its defensive claim is the same actor command, not an independent clock. |
| first-segment/software grace and progress watcher | **Still compatibility recovery** | Child progress/exit are not actor events yet. M4 replaces the detached watcher with one actor-owned producer deadline. |
| `child_transition`, `watchdog_active`, `replacing_child` | **Still compatibility serialization** | M3b uses the gate to order physical signals safely. M4 deletes all three after actor child ownership. |
| playlist and segment wait budgets | **Retained bounded HTTP waits** | A request cannot wait forever. Remaining M3 work converts their observation/wakeup ownership to actor state. |
| fetched-frontier time suspension | **Removed for explicit mode** | Only legacy media-only sessions use it. Explicit time production is demand/playhead/runway based. |
| per-session and fleet byte holds | **Retained by design** | These are hard scratch-capacity bounds, not playback-failure watchdogs. |
| web recovery timers/reopens | **Still authoritative for recovery** | The web reporter is now active for lease/demand but server actions are still `none`. M5 removes independent reopen authority. |
| Apple/Android recovery timers/reopens | **Unchanged** | Their passive M2 reporters and M5 controller cutover have not landed. |
| VOD segment materialization deadline | **Retained by design** | A demanded immutable object can fail to materialize and an HTTP request needs a bound. |

The full symbol-level removal ledger is maintained in
[`PLAYBACK-CONTROL-STATUS.md`](PLAYBACK-CONTROL-STATUS.md) and the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

## Verification contract

The PR is not mergeable until adversarial review precedes tests and the final
diff proves:

1. the actor's exact explicit timer fences a commandless session and expiry
   cannot be revived between repair ticks;
2. media renewal immediately before the deadline moves the one deadline;
3. a virtual 30-minute foreground hold with delivered heartbeats stays live;
4. accepted `hold` and `active` signal a real child immediately;
5. hold before first publication stops production;
6. seek uses the target rather than the stale pre-seek playhead;
7. playback rate scales reserve while client runway cannot raise the configured
   ceiling;
8. byte and global caps remain effective under adversarial snapshots;
9. demand-to-capacity hold changes never briefly resume the child;
10. status, events, logs, and metrics agree on the current reason and policy;
11. legacy media-only pacing and lifetime remain compatible; and
12. the focused suites, full local gate, and every required hosted job pass.

Rollback disables protocol advertisement for new sessions. Those sessions
remain in the 60-second legacy lease and fetched-frontier pacing path. An
already explicit session retains the contract it accepted until it ends.
