# nuc3 snapshot catch-up — why a live learner waited fifteen idle minutes

**Status:** confirmed live diagnosis; implementation fix not started ·
**Incident window:** 2026-09-05 18:09–18:43 UTC · **Written:** 2026-09-05 ·
**Production build:** `v0.3.0-700-gc661d387`

Companion to [OPERATIONS.md](../OPERATIONS.md) (the supported cluster and snapshot
runbook) and [WAL_GENERATION_REPAIR_PLAN.md](WAL_GENERATION_REPAIR_PLAN.md)
(the snapshot-transfer timeout rationale). This document answers one narrower
question: why did `nuc3` remain labelled *Catching up* during the 2026-09-05
rollout, and what must a corrective change prove?

The implementation references below name the production tree at commit
`c661d387`. At diagnosis time the local checkout was 41 commits behind
`origin/main`; the four relevant files were byte-identical between deployed
`c661d387` and `origin/main`, so older worktree line numbers are not production
evidence.

## Executive finding — catch-up was waiting on an idle snapshot RPC

`nuc3` was not steadily applying a backlog. Its first offset-zero snapshot retry
stopped after writing exactly 15 MiB and then transferred no application data
for fifteen minutes. The Raft heartbeat stream remained active throughout, so
the cluster could still observe the learner while the learner could not pass
its bounded-read or process-readiness gates.

The sequence is confirmed:

1. `m6` sent a snapshot chunk at offset 6 MiB after the restarted receiver
   expected offset zero.
2. `nuc3` returned `SnapshotMismatch`; OpenRaft logged that it reset the sender
   offset to zero and retried.
3. The retry produced a 15 MiB temporary file, then stopped making progress.
   The snapshot TCP stream stayed established with empty send and receive
   queues. A separate heartbeat stream continued exchanging data.
4. The configured snapshot timeout was 1,200 seconds on every node. The
   snapshot transport uses OpenRaft's 75% soft deadline, so the idle RPC was
   canceled after 900 seconds.
5. Cancellation reset the WebSocket. The temporary file disappeared, the next
   transfer installed an 88,559,616-byte snapshot, and `nuc3` opened HTTP 73
   seconds after the timeout.

The user-facing symptom was therefore truthful but incomplete. The Cluster
page renders every learner with `bounded_read_ready == false` as *Catching up*;
it does not distinguish active byte progress, an idle RPC, a retry backoff, or
startup before the HTTP listener exists.

The deepest confirmed defect is **absence of a useful progress boundary inside
the 900-second RPC wait**. Source and runtime evidence strongly implicate reuse
of the same WebSocket after `SnapshotMismatch`, but the evidence does not yet
prove why that post-mismatch stream stopped advancing at 15 MiB. A review or
fix must preserve that distinction instead of claiming the mismatch alone is
the complete root cause.

## Expected and actual behavior

| | Expected | Observed |
|---|---|---|
| Offset mismatch | Receiver rejects the nonzero chunk; sender resets to zero and resumes promptly. | Retry restarted at zero, the receiver wrote 15 MiB, then the RPC waited idle until its soft deadline. |
| Snapshot progress | An established snapshot stream either advances or fails within an operationally useful bound. | TCP remained established with no queued bytes or file growth for 900 seconds. |
| Learner startup | Startup stays fenced until the local applied index reaches a quorum-confirmed watermark. | The fence worked, but exposed no progress while the transport was idle. |
| Cluster page | Tell the operator whether a learner is progressing, retrying, or stalled. | All four states collapse to *Catching up*. |
| Automatic recovery | Cancel an abandoned RPC, reconnect, and complete a clean transfer. | This worked, but only after the full 15-minute soft timeout. |

The safety gates behaved correctly: `nuc3` did not open port 32400 or serve
bounded reads from an unproved database image. The recovery latency and the
operator explanation did not behave well enough.

## Timeline — the timeout explains the recovery to the millisecond

All timestamps are UTC on 2026-09-05.

| Time | Evidence | Interpretation |
|---|---|---|
| 17:40:56 | `nynuc` container start | Rolling deployment had begun. |
| 17:49:59 | `m6` container start | All observed fleet images later reported the same build. |
| 18:08:14 | `nuc4` container start | Third production voter restarted. |
| 18:09:47 onward | `m6` repeatedly logged replication timeouts for target Raft id 7. | `nuc3` was already not accepting normal replication before its own restart. The first causal event before 18:09 was not captured. |
| 18:26:47.579 | `nuc3` container start, restart count zero | The learner restarted as Raft id 7. This was a fresh process, not a crash loop. |
| 18:26:50.446 | `m6` logged `SnapshotMismatch`: receiver expected offset 0, sender sent 6,291,456. | An interrupted or stale sender-side transfer position met a fresh receiver-side snapshot state. |
| 18:26:50.446 | OpenRaft logged `snapshot mismatch, reset offset and retry`. | The remote error survived translation and reached OpenRaft's offset recovery. |
| 18:26:50.678 | `/srv/plurx/hiqlite/state_machine/snapshots/temp` reached 15,728,640 bytes. | The offset-zero retry began and wrote five 3 MiB chunks. |
| 18:26:50–18:41:50 | File size and modification time did not change. Snapshot socket remained established and idle; heartbeat socket remained active. | This was a stalled RPC, not slow catch-up or a host/network outage. |
| 18:41:50.682 | `m6` logged `InstallSnapshot RPC ... deadline has elapsed`. | Exactly 900 seconds after the temp file's last progress: 75% of the configured 1,200-second hard timeout. |
| 18:42:03 | The temporary snapshot file no longer existed. | Cancellation/reset cleanup had crossed a transport and receiver-state boundary. |
| 18:42 | New snapshot `01a072e0-de58-7293-8f22-d109de80100f`, 88,559,616 bytes, became `current`. | The clean retry completed. |
| 18:43:04.106 | `plurxd starting` appeared in the learner log. | Store selection and startup catch-up gate completed. |
| 18:43:04.249 | `listening addr=0.0.0.0:32400` | HTTP service began. |
| 18:43:28 | `/readyz` returned `ready`, HTTP 200; Docker health was `healthy`. | Automatic recovery completed without another container restart. |

The directly observed post-restart outage was 16 minutes 17 seconds. The
earlier target-7 replication errors show that the degraded interval began no
later than 18:09:47, but the available logs do not establish its first event.

## Causal chain — heartbeat freshness and serving readiness diverged

```text
rolling restart / interrupted transfer state
                  │
                  ▼
receiver expects snapshot offset 0
sender presents offset 6 MiB
                  │
                  ▼
remote SnapshotMismatch reaches OpenRaft
OpenRaft resets logical offset to 0
                  │
                  ▼
same transport attempts a clean transfer
temp file reaches 15 MiB, then stops
                  │
        ┌─────────┴─────────┐
        │                   │
        ▼                   ▼
heartbeat stream lives   snapshot RPC waits
roster stays fresh       local applied gate stays closed
        │                   │
        └─────────┬─────────┘
                  ▼
Cluster page says "Catching up"
                  │
          900-second soft TTL
                  ▼
RPC cancellation bumps reset epoch
WebSocket is torn down and recreated
                  │
                  ▼
88.6 MiB snapshot installs · listener opens · readyz=200
```

This split is intentional at the safety boundary: a heartbeat proves only that
Raft can still exchange control traffic. It does not prove that the local state
machine has applied a current database image.

## Live evidence — what ruled alternatives in and out

### Every node ran the same binary and timeout

The four containers reported `plurxd 0.3.0 (v0.3.0-700-gc661d387)`. Every
host's `/srv/plurx/plurx.toml` contained:

```toml
[cluster]
install_snapshot_timeout_secs = 1200
```

The corresponding environment variable was present but empty on all four
containers, so it did not override the file. Version skew and timeout skew are
not explanations.

### The other three nodes stayed ready

During the initial sample:

| Host | `/readyz` | Role relevant to impact |
|---|---|---|
| `nynuc` | HTTP 200, `ready` | voter |
| `m6` | HTTP 200, `ready` | voter and observed snapshot sender |
| `nuc4` | HTTP 200, `ready` | voter |
| `nuc3` | connection reset/refused before listener startup | committed learner, Raft id 7 |

The cluster retained its three voting members. `nuc3` held no vote, so this
event removed learner read/media capacity but did not reduce voting quorum.

### The learner process was alive, small, and waiting

At 18:32–18:37:

| Signal | Value |
|---|---|
| Container state | running; restart count 0; OOM killed false |
| Process | `plurxd run`, one process, approximately 20 tasks |
| CPU | approximately 0.5% |
| Memory | approximately 24.7 MiB |
| HTTP health | port 32400 refused/reset; Docker health `starting` |
| Snapshot temp | 15,728,640 bytes, unchanged since 18:26:50.678 |
| Snapshot TCP stream | established; zero kernel send/receive queues; no payload movement for more than eight minutes when sampled |
| Heartbeat TCP stream | established; packets continued approximately every 100–300 ms |

This combination rules out a crash loop, OOM, gross CPU saturation, and a
whole-host network partition. It also rules out the benign reading of
*Catching up* as continuous transfer at a low rate: the file did not grow.

### The leader named the protocol failure

The essential `m6` log sequence was:

```text
18:26:50.446 ERROR target 7 ... SnapshotMismatch ... expect offset 0,
                     got offset 6291456
18:26:50.446 WARN  snapshot mismatch, reset offset and retry
18:41:50.682 WARN  error sending InstallSnapshot RPC ... deadline has elapsed
```

The 900.236-second distance between the mismatch/retry log and deadline is the
configured behavior, not scheduler noise. `RPCOption::soft_ttl()` is 75% of
the 1,200-second hard timeout, and production passes that soft TTL to snapshot
requests.

### The old Raft id warning was a red herring

Healthy nodes also logged that historical SQLite node 4 was absent from the
current membership `[1, 5, 6, 7]`. `nuc3`'s active identity is node 7 with role
`learner`; node 4 was its earlier incarnation. The failing snapshot RPC named
target 7 and the correct `192.168.4.7` addresses. No evidence connects the
historical node-4 warning to the stalled stream, so a fix must not delete or
rewrite membership state on that basis.

### The deployed build already contained three snapshot-recovery fixes

This event is a gap in the current recovery work, not evidence that production
was running the pre-fix transport:

| Commit | Intended invariant | What this incident adds |
|---|---|---|
| `03379035` | Preserve a peer's typed `SnapshotMismatch` so OpenRaft can reset to offset zero. | The typed error worked, but offset reset alone did not ensure forward progress. |
| `93039753` | A dropped RPC future requests an independent WebSocket reset even when the bounded request queue is full. | The reset worked at the soft timeout, but nothing requested it immediately after the mismatch. |
| `dcb76dec` | Retain reset epochs, reject stale queued work, and make forced reader/writer cleanup non-blocking. | Timeout-driven cleanup recovered cleanly; the untested interval is the same-stream retry before cancellation. |
| `c661d387` | Derive Docker health/startup budgets so bounded recovery is not killed by the supervisor. | The process survived long enough to recover, while remaining operationally opaque for fifteen minutes. |

The first three commits are ancestors of deployed `c661d387`. Review should
extend their invariants rather than revert them: typed mismatch propagation,
epoch-safe cancellation, and non-blocking cleanup all contributed to the
eventual safe recovery.

## Production code path — the safety gate worked, then transport recovery waited

These line references are from deployed commit `c661d387`, which is unchanged
in the relevant files on `origin/main` as of 2026-09-05.

### The page maps one boolean to the entire explanation

[`crates/plurxd/src/web/index.html`](../../crates/plurxd/src/web/index.html), lines
13187–13190, renders a non-voter learner as either *Read worker ready* or
*Catching up* based only on `bounded_read_ready`:

```javascript
const worker=!voter&&n.role==="learner"
  ? n.bounded_read_ready
    ? `<span ...>Read worker ready</span>`
    : `<span ...>Catching up</span>`
  : "";
```

**How to read it:** the pill is an authorization verdict, not a transfer
progress report. A false value is safe, but it cannot tell an operator whether
the next useful action is to wait ten seconds, wait fifteen minutes, or inspect
a stalled transport.

### Startup correctly withholds the listener

[`crates/plurx-core/src/cluster/migration.rs`](../../crates/plurx-core/src/cluster/migration.rs),
lines 1997–2050, obtains a quorum-confirmed commit watermark and refuses to
finish store construction until the target-local applied index reaches it. The
catch-up deadline is the configured snapshot timeout plus a separate grace.

This is the right fail-closed contract. Removing or bypassing it would convert
a poor recovery explanation into stale reads, which is worse.

### A timeout is the only observed no-progress detector

In deployed
[`vendor/hiqlite/src/network/raft_client.rs`](../../vendor/hiqlite/src/network/raft_client.rs):

- Lines 742–768 wrap each request in `tokio::time::timeout(soft_ttl, ...)`.
  Dropping an armed `ConnectionResetGuard` increments a reset epoch.
- Lines 762–765 disarm the reset for any response delivered by the transport.
- Lines 772–784 preserve a remote `SnapshotMismatch`, because OpenRaft needs
  the typed error to reset its logical file offset.
- Lines 820–839 pass `option.soft_ttl()` to `send()` and translate the embedded
  snapshot response only after `send()` returns.
- Lines 457–586 make the WebSocket handler observe a changed reset epoch,
  abort its reader/writer tasks, drain in-flight acknowledgements, and reconnect.

The ordering matters. A peer-delivered mismatch is a successful transport
response, so `send()` disarms the reset guard before `install_snapshot()` turns
the embedded error into a typed `RemoteError`. OpenRaft resets the logical
offset, but this path does not itself require a fresh WebSocket. The live
offset-zero retry then used an established stream and stopped advancing.

At the later soft timeout, the guard remained armed because no response
arrived. Its drop changed the reset epoch, the handler crossed a real transport
boundary, and the following retry succeeded.

**Confidence boundary:** this ordering explains why the first mismatch did not
force a reconnect and why the later timeout did. It does not by itself prove
which reader, writer, request-id, receiver-state, or socket condition stopped
the intervening retry at 15 MiB. That internal stall still needs a deterministic
reproduction or additional trace points.

## Root cause and contributing conditions

### Confirmed root cause of the prolonged user-visible state

The post-mismatch snapshot request made no progress and had no failure boundary
shorter than its 900-second soft TTL. Because startup correctly waits for local
application of a quorum-confirmed watermark, `nuc3` remained unavailable and
the membership projection kept `bounded_read_ready` false until timeout-driven
transport reset allowed a clean snapshot.

### High-confidence transport hypothesis

The typed mismatch resets OpenRaft's file offset without resetting Hiqlite's
stream. The immediate retry on that existing stream entered a state that could
neither complete nor fail promptly. Requiring a new connection after a remote
snapshot mismatch is the smallest hypothesis consistent with all observed
boundaries:

- mismatch on the old connection;
- logical retry without byte progress;
- cancellation-driven connection reset;
- successful clean transfer immediately afterward.

This is a hypothesis to test, not permission to flatten `SnapshotMismatch`
back into `Unreachable`. OpenRaft must still receive the typed error so it
returns to offset zero.

### Conditions that amplified the symptom

1. **The 1,200-second hard timeout was chosen for legitimate large snapshots.**
   It prevents a slow 72–89 MiB transfer from being killed prematurely, but its
   900-second soft TTL is also the current idle-wait ceiling. Total-transfer
   budget and no-progress budget are different concepts.
2. **The UI reports authority, not motion.** A safe false authorization flag
   becomes the optimistic phrase *Catching up* even when no byte has moved.
3. **Startup logs begin after store selection.** The learner emitted no useful
   progress line while waiting before the HTTP listener. Diagnosis had to come
   from `m6`, Docker state, the temp file, and TCP counters.
4. **Existing regressions prove the mechanisms separately.** Tests cover typed
   mismatch preservation, retained reset epochs, canceled queue entries,
   backpressured writers, and forced cleanup. They do not reproduce a real
   mismatch followed by an offset-zero retry on the same stream and assert
   forward progress.
5. **Docker's long health start period is intentionally non-destructive.** It
   let the bounded recovery finish without restarting the container, but
   `health: starting` added no diagnosis beyond the missing listener.

## Impact and safety assessment

**User impact:** `nuc3` could not accept HTTP traffic, bounded catalogue reads,
or declared node-local learner work for at least 16 minutes 17 seconds after
its restart. The earlier target-7 replication errors may extend that window
back to 18:09:47.

**Cluster impact:** the three voters remained ready. Because `nuc3` was a
committed non-voting learner, the event did not lower quorum tolerance. It did
reduce worker capacity and made the Cluster page appear inexplicably stale.

**Data safety:** no evidence of data loss or divergent committed state was
observed. The receiver rejected an invalid offset, deleted the failed temp
generation after cancellation, atomically published a new 88.6 MiB snapshot,
and opened serving only after the startup watermark gate passed.

**Operational risk:** restarting voters to shake loose this condition would
trade a learner-capacity problem for quorum risk. The automatic retry did
recover. A future incident should preserve the three ready voters while
collecting sender and learner evidence.

## Recommended corrective work — separate progress recovery from transfer budget

### 1. Force a transport boundary after `SnapshotMismatch`

Preserve the typed remote error so OpenRaft resets to offset zero, and also
request a reset for the exact socket epoch that carried the mismatched response.
The next live request must either wait for the handler to reconnect or be
rejected and retried; it must not write an offset-zero replacement onto a
stream whose request/receiver state belongs to the rejected transfer.

**Acceptance:** an injected nonzero offset receives `SnapshotMismatch`; the
next offset-zero chunk crosses a newly established WebSocket and the complete
snapshot installs without waiting for `soft_ttl`.

### 2. Give idle progress its own measured deadline

Do not lower `install_snapshot_timeout_secs` as a hot fix. That value is the
budget for a valid full transfer and was raised because production snapshots
could not finish under ten seconds. Add a no-progress boundary keyed to
accepted snapshot bytes or completed chunks, with a threshold derived from
production transfer gaps and enough margin for slow disks.

**Acceptance:** a transfer that continues accepting chunks may use the full
1,200 seconds; a transfer with no accepted byte/chunk progress resets well
before 900 seconds.

### 3. Publish progress before HTTP startup

Emit bounded structured fields on the learner and sender:

| Field | Why it is needed |
|---|---|
| snapshot id | Distinguishes a clean retry from continued work on an old generation. |
| expected and sent offset | Makes mismatch direction unambiguous. |
| accepted bytes and last-progress age | Separates slow transfer from no transfer. |
| attempt and reconnect count | Shows whether recovery is cycling. |
| hard and soft deadline | Explains the next automatic action in absolute time. |
| startup target and local applied index | Connects transfer completion to the serving gate. |

**Acceptance:** an operator can diagnose this state from one support bundle or
one healthy peer without shelling into the learner's filesystem.

### 4. Stop calling every false permit “Catching up”

Keep the authorization predicate fail-closed, but render the reason separately:
`receiving snapshot` · `retrying snapshot` · `snapshot stalled` ·
`waiting for quorum watermark` · `applied and waiting for fresh proof`.

**Acceptance:** a test fixture with unchanged accepted bytes beyond the
no-progress threshold cannot render the plain *Catching up* pill.

## Regression plan — reproduce the live sequence, not isolated helpers

1. **Typed mismatch plus reset.** Return `SnapshotMismatch` from a fake peer,
   assert the caller still receives `RemoteError<SnapshotMismatch>`, and assert
   the carrying socket epoch is invalidated before the next request writes.
2. **Same-stream counterexample.** Make an offset-zero retry hang if it reuses
   the original stream. The fixed implementation must reconnect and complete;
   the deployed implementation should fail this test first.
3. **Interrupted learner restart.** In the separate-process three-voter
   harness, interrupt a learner after a nonzero snapshot chunk, restart it with
   an empty temp receiver state, and require one clean retry to reach the
   startup watermark without waiting for the soft TTL.
4. **Progress versus duration.** Prove a continuously advancing large transfer
   is allowed beyond the idle threshold while a zero-progress transfer is
   canceled. This prevents a new watchdog from recreating the old large-file
   failure.
5. **Request-id and queue pressure.** Combine the mismatch with a full bounded
   request queue and a blocked writer, because existing tests exercise those
   cases only independently.
6. **Operator state.** Feed active, idle, timed-out, and recovered snapshot
   samples into the shipped Cluster page and assert four different sentences.
7. **Fleet acceptance.** Re-run one learner restart against a production-sized
   snapshot. Record offset progression, socket epochs, transfer duration,
   startup watermark, `/readyz`, and unchanged voter readiness.

The Rust compile/test loop in [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)
must be established before changing the vendored transport or cluster startup
code. CI is not the compiler for this fix.

## Operational response until a fix ships

1. Confirm all three voters return HTTP 200 from `/readyz`. The learner is not
   worth risking quorum for.
2. On the current leader, filter for target 7 `SnapshotMismatch`,
   `InstallSnapshot`, and deadline lines.
3. On `nuc3`, sample the temp snapshot size and modification time twice. A
   fixed value plus an established idle snapshot socket is a stall, regardless
   of the UI wording.
4. If the voters are healthy, allow the bounded automatic cancellation to run
   before considering a controlled restart. Record the expected soft deadline
   from the configured value; for 1,200 seconds it is 900 seconds after the
   current RPC began.
5. Do not delete `hiqlite/`, the temp snapshot, membership files, or the old
   Raft-id record by hand. The observed automatic cleanup preserved data and
   completed successfully.
6. Do not lower the timeout or restart a voter ad hoc. Any voter restart must
   use the cluster's restart-preparation proof and happen one node at a time.

Useful read-only checks:

```bash
# Prove voter readiness before touching the learner.
for host in nynuc m6 nuc4; do
  ssh "$host" 'curl -fsS http://127.0.0.1:32400/readyz'
done

# Check learner process and current health state.
ssh nuc3 'docker inspect --format \
  "status={{.State.Status}} restart={{.RestartCount}} health={{.State.Health.Status}}" \
  plurxd'

# Compare snapshot progress twice; unchanged size and mtime mean no file progress.
ssh nuc3 'stat /srv/plurx/hiqlite/state_machine/snapshots/temp'

# Read the sender-side reason without unrelated application logs.
ssh m6 'docker logs --since 20m plurxd 2>&1 | \
  grep -Ei "snapshot|target=7|node 7"'
```

**How to read them:** `ready` on all three voters means quorum service is not
the emergency. A running learner with `health=starting`, a fixed temp file, and
a live heartbeat is a transport-progress problem. The leader's first timeout
or mismatch line anchors the expected automatic retry deadline.

## Non-goals — do not solve the wrong problem

- **Do not promote `nuc3` to a voter.** Its learner role is deliberate and did
  not cause the transport to stall.
- **Do not weaken the startup watermark gate.** That gate prevented unproved
  local state from serving.
- **Do not delete historical membership or snapshot state.** The node-4 warning
  was not the failing target, and manual deletion would destroy evidence and
  may create a recovery event larger than this one.
- **Do not equate a longer Docker start period with recovery.** It prevents a
  supervisor kill; it does not make the transfer progress.
- **Do not claim the internal 15 MiB stall mechanism is proven.** The review
  should close that evidence gap with a deterministic test or instrumentation.

## Review questions — decisions the corrective PR must answer

1. Should every remote `SnapshotMismatch` invalidate the carrying WebSocket,
   or only the database snapshot variant? What cache-snapshot invariant differs?
2. Can a reset be requested after preserving the typed error without racing a
   replacement socket or dropping OpenRaft's next offset-zero request?
3. What exact state held the retry at 15 MiB: writer queue, reader correlation,
   receiver install state, request id, or an OpenRaft scheduling boundary?
4. Which observable event constitutes progress: bytes written to the temp file,
   chunks acknowledged by the receiver, or applied snapshot publication?
5. What no-progress threshold survives measured slow disks without allowing a
   silent fifteen-minute wait?
6. Should pre-listener startup status be exposed through a small independent
   health surface, or is peer/support-bundle telemetry sufficient?
7. How should the Cluster page word a fresh heartbeat paired with an idle
   snapshot so that it remains accurate without prescribing an unsafe restart?

## Completion bar

This diagnosis is closed when a review agrees on the missing invariant and a
corrective change proves all of the following:

- a mismatch retains its typed offset-zero recovery signal;
- the next snapshot attempt cannot inherit the rejected transport state;
- no-progress recovery is materially shorter than 900 seconds;
- continuously progressing production-sized snapshots keep their 1,200-second
  transfer budget;
- startup still refuses to serve before the quorum watermark is applied;
- the operator can tell progress, retry, and stall apart;
- three voters remain ready through the learner recovery acceptance.

Until then, the accurate summary is: **automatic safety and eventual recovery
worked; prompt transport recovery and operator diagnosis did not.**
