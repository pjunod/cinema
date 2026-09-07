# Playback control M3a — rolling lease actor foundation

This slice replaces the rolling fallback's split control mutex and inferred
activity clock with one bounded per-generation mailbox. It is the first M3
implementation slice from
[`PLAYBACK-CONTROL-PROTOCOL-PLAN.md`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md), not
the completion of M3 or the watchdog-removal milestone.

## Outcome

Each rolling HLS generation now has one owner for:

- control client, generation, owner-epoch, and sequence fencing;
- the complete validated demand, runway, render, selection, capability, and
  observation snapshot;
- capability-authenticated media renewal;
- legacy-to-explicit lease-mode binding;
- retirement fencing; and
- the atomic decision that an expired lease may be retired.

The mailbox is bounded at 128 commands. Commands contain no media bytes and
perform no filesystem, Store, cluster, or child-process I/O. A dead mailbox
fails renewal closed and the repair loop retires its session instead of
leaking an encoder.

## State and ordering

```text
session start
    |
    v
Legacy lease ---- accepted sequence ----> Explicit lease
    |                                          |
    +---- authenticated media renewal <-------+
                                               |
                                claim expiry / fence
                                               v
                                            Retired
```

Legacy mode preserves mixed-version playback. The first newly accepted
control sequence switches the same lease to explicit mode and stores the
complete client snapshot. It does not create a second clock. Equal-sequence
replays, stale sequences, old owners, and retired sessions do not renew or
replace the stored demand.

Media GETs and control messages enter the same mailbox. Retirement publishes a
shared monotonic fence before its actor reply, so a cancelled caller cannot
leave the mailbox retired while process teardown still considers it renewable.
The periodic repair
loop no longer reads a timestamp and retires later from a stale observation;
it asks the actor to claim expiry. Claiming checks the deadline and marks the
actor retired in one command, so a concurrent segment renewal is ordered
strictly before or after the claim. Only the winner proceeds to existing
process teardown.

This slice deliberately retained the rolling 60-second compatibility lifetime.
The follow-up that enables the 30-second explicit lease, indefinite foreground
`hold`, and demand-derived production is specified in
[`PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md`](PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md).

## Instrumentation

The live status shape adds:

- `lease_mode`: `legacy`, `explicit`, `unavailable`, or `vod` on the aggregated
  immutable-delivery status shape;
- `control_demand`;
- `reported_position_ms`;
- `client_runway_ms`; and
- `render_state`.

Prometheus adds rolling-lease renewals by bounded `control`, `media`, or
`internal` source, atomic expirations, and non-expiry retirements. Internal
fallback starts are never counted as authenticated media consumption. Raw
session capabilities and client instance IDs remain excluded.

## Watchdogs and timers still present

No watchdog is being disguised as actor work in this slice. These mechanisms
still exist and must remain visible until their owning milestone replaces
them:

| Mechanism | Status after M3a | Why it remains |
|---|---|---|
| first-segment grace and producer stall watcher | unchanged compatibility recovery owner | Child progress and exit events do not enter the actor yet. M4 replaces these with one actor-owned producer deadline. |
| `child_transition`, `watchdog_active`, and `replacing_child` | unchanged | They still serialize the old in-place fallback actions. They are deleted only after the actor owns child transitions. |
| playlist and segment wait budgets | unchanged bounded HTTP waits | A request cannot wait forever for bytes. They become actor waiters in the remaining M3/M4 work. |
| 15-second flow-control/repair tick | retained as scheduling | It prunes retention, records admission speed, and asks the actor to claim expiry. It no longer decides expiry from an independently read clock. |
| inferred ahead-window suspension | unchanged | Actual client runway is now retained, but production policy does not consume it until the next M3 slice. |
| platform stall/reopen watchdogs | unchanged | Passive reporters are not yet the sole action owner. M5 removes the competing client recovery paths. |

The old `last_request` clock and its retirement mutex are removed from rolling
sessions. `last_request` remains only as a backward-compatible status field,
derived from the actor's last accepted renewal source.

## Verification contract

The review and test sequence for this slice must prove:

1. one accepted sequence stores every validated policy fact and renews once;
2. replay and rejection preserve both the demand snapshot and renewal age;
3. expiry can be claimed only once;
4. retirement rejects every later media or control renewal;
5. a media renewal immediately before the deadline prevents expiry;
6. the real manager reports explicit mode and demand after control; and
7. existing VOD control semantics remain unchanged.

The legacy fallback remains the rollback path while the actor absorbs child,
playlist, segment, progress, flow-control, and cluster-fence events in the
remaining M3 work.
