# Cluster transport recovery — fix buffered writes and bound recovery end to end

**Status:** ready to implement; transport defect reproduced; production patch not
written · **Written:** 2026-09-05 · **Executor:** Sol

This is the complete implementation handoff. Work through the milestones in
order; do not replace the flush correction with a mismatch reconnect patch.
Companion to [the incident diagnosis](NUC3_SNAPSHOT_CATCHUP_DIAGNOSIS.md)
(the original observations), [OPERATIONS.md](OPERATIONS.md) (operator contracts),
and [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (branching and qualification).
This document supersedes the diagnosis's proposed fix ordering and timeout
interpretation. You do not need the preceding conversation.

## 1. What the investigation established

### 1.1 The primary defect is missing TLS flushes

All four production WebSocket payload writers call `write_frame()` and then
wait for another queue item without calling `flush()`:

| Surface | Production function |
|---|---|
| Raft request | [`NetworkStreaming::stream_writer`](../vendor/hiqlite/src/network/raft_client.rs) |
| Raft response | writer in [`handle_socket`](../vendor/hiqlite/src/network/raft_server.rs) |
| Cluster API request | [`stream_writer`](../vendor/hiqlite/src/client/stream.rs) |
| Cluster API response | writer in [`handle_socket_concurrent`](../vendor/hiqlite/src/network/api.rs) |

[`HandshakeSecret`](../vendor/hiqlite/src/network/handshake.rs) also writes
challenge/response frames without flushing before waiting for the next step.
The proxy response writer in
[`server/proxy/stream.rs`](../vendor/hiqlite/src/server/proxy/stream.rs)
has the same omission and is included in the correction.

The deployed dependency chain is fastwebsockets 0.10.0, tokio-rustls 0.26.4,
rustls 0.23.42, and Tokio 1.53.1. `write_frame()` writes the frame but does not
flush the underlying stream. Tokio-rustls can accept plaintext, encounter
backpressure while emitting ciphertext, and return a successful byte count.
The remaining ciphertext stays in the TLS session. Making the underlying
transport writable does not cause an idle application writer to flush it.
The independent reader does not provide that guarantee either.

This was reproduced with those exact versions and Rust 1.97.1, using real TLS
and fastwebsockets over a deterministic backpressure adapter. Both cases passed:

| Case | Without flush after backpressure clears | After explicit flush |
|---|---|---|
| Server sends a 32-byte reply | Peer lacks the frame after 200 ms; readers remain alive | Same frame arrives on the same connection |
| Client sends a 3,145,728-byte frame | Peer lacks the complete frame after 200 ms; readers remain alive | Same frame arrives on the same connection |

There is no `SnapshotMismatch`, offset manipulation, or fabricated protocol
failure in the probe. Appendix A contains its complete source and commands.
The 200 ms observation interval demonstrates lack of autonomous progress; it
is not the proposed production deadline. Probe build, execution, and Clippy
with denied warnings succeeded. A source-only extraction of the deployed
Hiqlite also passed its SQLite compile check on Rust 1.97.1; its standalone
lockfile differs from the daemon lockfile, so the probe deliberately pins the
actual daemon transport versions separately.

**Causal interpretation:** a missing reply flush explains a receiver file that
stops at a whole-chunk boundary while both endpoints wait. A missing request
flush explains a partly delivered next frame. Independent heartbeat sockets
can remain active in either case. This is a demonstrated defect in the actual
library combination and the production write pattern. The historical 15 MiB
stall had no TLS-buffer trace, so do not claim that the exact blocked endpoint
or request ID has been forensically identified.

### 1.2 The earlier outage also involved the cluster API

Read-only retrieval of the retained `nuc3` and `m6` Docker logs established:

| UTC time, 2026-09-05 | Observation |
|---|---|
| 17:43:39 | `nuc3` listened and recovered serving authority on build `c661d387` |
| 17:49:59–17:50:00 | Leader API disconnect/reconnect errors during the `m6` restart; learner briefly recovered serving authority |
| 17:50:52 onward | Repeated replicated-operation timeouts on `nuc3` |
| 17:55:32 | Several late API transaction responses found their callers gone; serving authority expired again |
| 18:08:11 onward | Retrieved learner logs already show `/readyz` returning 503 |
| 18:26:37–18:26:47 | Learner drained and restarted |
| 18:26:50.446 | Leader preserved the typed mismatch for snapshot `01a072c8-0dcd-75e1-ad27-9cd231c96a2e`, sent offset 6,291,456, expected zero |
| 18:41:50.682 | The subsequent snapshot RPC hit its soft timeout |
| 18:42:18–18:43:04 | Learner ran encoder/probe initialization; two probes each consumed roughly 20 seconds |
| 18:43:04.250 | HTTP listener opened |

These observations extend the degraded interval beyond the original report's
18:09 bound. They do not prove every earlier API timeout shares the flush
cause. They do establish that a snapshot-only patch has insufficient scope.
Also, the 73 seconds from snapshot timeout to listener startup were not all
snapshot transfer time: startup media probes account for a substantial part.
Keep those phases distinct in recovery measurements.

### 1.3 Logical snapshot state does not belong to the socket

OpenRaft's `Raft::install_snapshot()` locks `self.inner.snapshot` and invokes
`Chunked::receive_snapshot()`. A mismatch for a different snapshot ID and a
nonzero offset returns an error without altering the current stream. An
offset-zero request can start the new stream on the same socket.

For the same snapshot ID, the receiver seeks when the requested offset differs
from its recorded offset. Socket replacement does not clear this state.
Therefore, preserve typed mismatch propagation but **do not require a
reconnect on every mismatch**. The mandatory regression is successful
same-socket recovery after a legitimate mismatch. Timeout, writer failure,
malformed protocol data, and explicit cancellation remain reconnect triggers.

### 1.4 Existing deadlines do not form a whole-recovery contract

OpenRaft 0.9.25 applies `RPCOption::hard_ttl()` separately to each chunk.
Hiqlite currently uses the soft TTL for each snapshot request. With the fleet's
1,200-second setting, one unanswered chunk waits 900 seconds. Successful
chunks and mismatches reset upstream retry accounting. No full-transfer timer
bounds that loop. The startup gate, in contrast, allows the configured value
plus 45 seconds for the entire catch-up phase.

Source references: [upstream snapshot driver](https://raw.githubusercontent.com/databendlabs/openraft/v0.9.25/openraft/src/network/snapshot_transport.rs),
[upstream network API](https://docs.rs/openraft/latest/openraft/network/trait.RaftNetwork.html),
[Hiqlite transport](../vendor/hiqlite/src/network/raft_client.rs), and
[Plurx startup](../crates/plurx-core/src/cluster/migration.rs).

## 2. Scope and invariants

**Objective:** after transient transport backpressure, a node either advances
replication promptly or crosses a bounded, observable recovery boundary.
Ordinary API calls must obey the same frame-delivery rule. A failed learner
must not destabilize the healthy voting majority, and a recovering voter must
use exactly the same corrected transport.

The following invariants govern every milestone:

1. **Frame completion includes flush.** Queue acceptance and plaintext
   acceptance are not delivery. Record write completion only after flush.
2. **Socket cleanup cannot wait behind the socket it is cleaning up.** Failure
   notification uses retained state or task completion, never only a full
   bounded payload queue.
3. **Socket cancellation does not cancel accepted state-machine work.** A
   partially written snapshot chunk must finish under its existing owner.
4. **Retry preserves protocol semantics.** Keep typed remote snapshot errors,
   vote checks, snapshot IDs, offsets, and existing API mutation ambiguity.
5. **Recovery budgets are absolute within an attempt.** Retries, duplicate
   chunks, reconnects, and mismatch resets cannot renew the attempt deadline.
6. **Authority remains fail-closed.** Neither heartbeats nor transfer progress
   authorize reads, membership changes, media work, or readiness.
7. **Diagnostics remain available before port 32400 opens.** Report from the
   existing authenticated cluster listener and process-local state.

**Non-goals:** no Raft algorithm replacement, OpenRaft upgrade, membership
rewrite, WAL format change, database deletion, learner promotion, automatic
mutation replay, or weakening of startup/serving fences. Do not change election
or heartbeat timers to compensate for transport stalls. Do not move hardware
probes in this effort; identify their time separately. Do not edit the Cargo
registry's source cache as an implementation technique.

## 3. Milestone 1 — flush every request/response boundary

### 3.1 Centralize the write-and-flush operation

Add [`vendor/hiqlite/src/network/frame_io.rs`](../vendor/hiqlite/src/network/)
and expose it crate-internally from `network/mod.rs`. Provide a generic helper
for `WebSocketWrite<S>` where `S: AsyncWrite + Unpin`:

```rust
// Proposed interface; adapt lifetimes to fastwebsockets 0.10.0.
async fn write_frame_flushed<S>(
    write: &mut WebSocketWrite<S>,
    frame: Frame<'_>,
) -> Result<(), fastwebsockets::WebSocketError>;
```

Its body must await `write.write_frame(frame)` and then `write.flush()`.
Use it in all four payload writers from §1.1. For unsplit handshake sockets,
call `ws.flush().await?` after every successful challenge/response frame and
before waiting for the peer. Flush best-effort graceful Close frames within
the existing short cleanup allowance; never let Close delay forced teardown.

Keep message ordering, serialization, request IDs, masking, and existing
stream split behavior unchanged. Do not add a queue of unflushed frames or
batch until the queue fills: sparse request/reply traffic is the failing case.
Audit all `write_frame` sites in vendored Hiqlite, including proxy forwarding;
each site must flush or have a documented enclosing flush before any wait.
Use the existing five-second `LEADER_STREAM_CONNECT_TIMEOUT` for the complete
server challenge/response exchange too; unauthenticated or half-completed
handshakes must not retain tasks indefinitely. Do not change authentication
or use plaintext transport to make the regression disappear.

### 3.2 Bound the write including its flush

Use a 30-second frame-write budget covering the combined write and flush,
measured from when the writer starts that frame. This is a transport bound,
not a database-query execution timeout. Existing shorter Raft RPC deadlines
and connection reset still win. The accepted receiver-side final snapshot
installation keeps its separate long budget; do not apply the frame-write
budget to the database restore operation.

On write/flush error or timeout, the writer must report a terminal outcome to
its connection supervisor and exit. The supervisor promptly fails affected
acknowledgements and disposes of that socket. A flush failure must not be
reported as a successful send. Preserve existing API behavior for requests
whose application outcome is unknown; reconnect is not permission to replay
a transaction.

**Acceptance:** the Appendix A fault fails through each old production writer
and passes through the corrected writer. Test both directions, both Raft and
API messages, small replies, 3 MiB chunks, and flush failures. A no-backpressure
control and same-socket mismatch retry pass. A permanently blocked flush exits
at the configured writer budget, with a shorter scaled budget in unit tests.

## 4. Milestone 2 — own the entire connection lifecycle

### 4.1 A supervisor owns reader and writer tasks

For Raft client/server and API client/server, retain both task handles and
observe writer completion even when the reader is still waiting for input.
Preserve the dedicated frame-reader task: do not put a non-cancel-safe
`read_frame()` directly into a loop that repeatedly cancels and resumes it.

Use this ownership shape:

```text
connection supervisor
  ├── reader task: completes frames, reports EOF/error
  ├── writer task: writes + flushes frames, reports error/timeout
  └── bounded channels carrying complete requests/responses

first terminal event
  → mark connection closed and stop admitting new socket work
  → fail/drain socket-owned acknowledgements
  → best-effort close, abort and await reader/writer
  → reconnect according to the existing peer/leader policy

accepted snapshot operation
  → node-owned executor; it is not a child that teardown aborts
```

A failure flag/watch or the retained JoinHandle must wake the supervisor
independently of bounded channels. Include task panics as terminal events.
Every `send_async()` that can block a connection supervisor must be selected
against terminal state and shutdown. Replace the Raft server's unconditional
`tx_write.send_async(Break).await` cleanup with nonblocking cleanup. Ensure
shutdown can interrupt connection attempts and reconnect delay as well.

Retain the Raft client's reset epochs, canceled-ack filtering, stale socket
checks, and forced-cleanup tests. Add writer completion to the same state
machine; do not create a second competing reconnect owner. A stale task may
not reset a replacement socket. JoinHandle drop alone detaches a task and is
not cleanup.

### 4.2 Keep snapshot writes alive after their socket disappears

Do not implement receiver cleanup by indiscriminately aborting
`raft.install_snapshot(req)`. OpenRaft's `Streaming::receive()` performs
`write_all()` and only then increments its recorded offset. Canceling midway
can leave the real file cursor advanced while that offset remains unchanged;
a same-offset retry may then write at the wrong file position.

Route accepted incoming snapshot requests through a node-owned executor per
Raft group. Allow one running snapshot request and one queued request, with
bounded admission. The executor owns the entire `raft.install_snapshot(req)`
future until completion; callers own only a response receiver. Dropping the
socket response receiver must not drop the operation. Drop queued requests
whose response receiver has closed before execution begins. Requests that
cannot be admitted within their bounded admission wait are not executed;
close that requesting transport so its caller retries.

Admission wait is bounded by the receiver's snapshot chunk budget (§5), and
must also end when the requesting connection closes. The executor belongs to
node lifecycle, not a connection; hold its task handle in node-owned state.
Register its shutdown with the existing graceful node shutdown sequence.
Do not enqueue a poison-pill shutdown behind a stuck operation and call that
bounded shutdown. Report an unfinished operation and let the existing process
shutdown/recovery policy handle terminal storage failure.

Keep the durable pending-snapshot publication and recovery behavior in
[`state_machine.rs`](../vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs).
A canceled reply must not remove `temp`, rename `current`, clear the pending
marker, or restart an install over a still-running owner. This milestone does
not promise recovery from permanently stuck storage I/O; it makes that failure
bounded in resource use, visible, and fenced.

**Acceptance:** close the socket after a controlled partial file write, then
retry the same offset. The accepted write completes first and the final image
matches the source digest. Closing during final installation preserves its
pending/recovery contract. Repeating connection failures 100 times leaves no
extra reader/writer tasks or sockets after quiescence; snapshot execution stays
within the one-running/one-queued limit. Run existing WAL and pending-install
regressions unchanged.

## 5. Milestone 3 — separate chunk, transfer, and installation time

### 5.1 Configuration and exact semantics

Add these fields to `ClusterConfig`, environment parsing/validation, and the
production Hiqlite builder. Keep the current install setting and its bounds.
New names below are proposed interfaces to implement, not existing APIs.

| Setting under `[cluster]` | Default | Range, inclusive | Meaning |
|---|---:|---:|---|
| `snapshot_chunk_timeout_secs` | 30 s | 5–300 s | One non-final chunk RPC, including queue wait, frame delivery and reply |
| `snapshot_transfer_timeout_secs` | 1,200 s | 60–14,400 s | Entire transfer stage, across chunks, retries and mismatches |
| `install_snapshot_timeout_secs` | 120 s, unchanged | 10–3,600 s, unchanged | Final chunk/install RPC cap |

Add `PLURX_CLUSTER_SNAPSHOT_CHUNK_TIMEOUT_SECS` and
`PLURX_CLUSTER_SNAPSHOT_TRANSFER_TIMEOUT_SECS`. Match the existing rule that an
empty environment value does not override TOML. Reject a chunk budget greater
than the transfer budget. Use checked conversion to milliseconds. Apply the
same settings in validation-node construction, not a harness-only copy.

Thirty seconds is the initial operational bound for a 3 MiB chunk on the
supported LAN, not a measured fleet percentile. The qualification in §8 must
measure chunk gaps under load and show adequate margin; if a healthy chunk
exceeds it, adjust the documented default with that evidence, not by deleting
the watchdog. Full transfers may legitimately take much longer than 30 seconds.

Add a crate-local `SnapshotRpcBudgets` value to the Hiqlite network factory and
connections. Give standalone Hiqlite construction defaults matching this
contract. Do not add fields to serialized Raft messages or change bincode enum
layouts; this change must interoperate with old peers during a rolling update.

### 5.2 Wrap the existing chunk driver instead of forking OpenRaft

Implement `RaftNetwork::full_snapshot()` for SQLite and cache connections using
OpenRaft's existing `Chunked::send_snapshot()`. Keep its offset/seek logic,
terminal remote errors, higher-vote handling, and five-consecutive-transient-
failure policy. Do not copy and maintain a second chunk algorithm.

The wrapper owns one attempt context, identified by a monotonically increasing
attempt ID, containing start instant, snapshot ID, transfer deadline, first
final-RPC instant when present, and progress fields. Attach it to the
connection for its `install_snapshot()` calls; clear it with an attempt guard
on every exit. Retained copies may not mutate a newer attempt's state.

Let `start` be full-snapshot entry, `T` the transfer setting, `I` the install
setting, and `C` the chunk setting:

- Transfer deadline is `start + T`, never renewed by progress or retry.
- Each non-final RPC gets `min(C, remaining transfer time,
  option.hard_ttl())` and retains cancellation-triggered connection reset.
- Before the first final RPC, require transfer time remaining. Latch its
  dispatch instant `final_start`; its deadline is
  `min(final_start + I, start + T + I)`. Never renew it on a final retry.
- The overall attempt cannot exceed `start + T + I`. While no final RPC has
  begun, the wrapper enforces `start + T` even during source reads, upstream
  backoff, and repeated immediate mismatches.
- If a mismatch returns the driver to offset zero after final dispatch, the
  original transfer deadline still applies. Do not grant another transfer
  window. The first final deadline remains fixed too.

Use a watchable phase/deadline context. On first final dispatch the outer
timer switches to the latched final deadline. A return to transfer after a
mismatch shortens it back to the original transfer deadline. Reentering final
uses the already-latched final deadline; neither deadline can be renewed.
Select the complete upstream driver against that timer and OpenRaft's supplied
`cancel` future. Give the inner driver a pending cancellation future so the
wrapper owns cancellation exactly once. Dropping the driver drops any active
RPC guard and requests reset. Return `ReplicationClosed` on caller cancellation;
return an appropriate `StreamingError` on deadline expiry, with a phase-specific
log. Error text alone must not drive retry decisions.

For final RPCs, use the hard deadline rather than the current 75% soft cutoff;
the final install must get the configured allowance, bounded by the attempt.
The upstream option is currently constructed from the install timeout, so
respect its hard cap even if the configured chunk allowance is larger.

A new outer replication attempt gets a new bounded attempt. Do not claim this
places a finite limit on the lifetime of replication itself: replication must
continue retrying while membership requires it. The startup gate and serving
fence supply the node-level availability decision.

### 5.3 Startup and Docker use the same recovery allowance

Set the startup catch-up allowance to `T + I + SNAPSHOT_CATCHUP_GRACE`.
Wrap each awaited quorum-watermark acquisition in `timeout_at(catchup_deadline,
...)`; checking the clock only after an unbounded await is insufficient.
Keep the subsequent target-local applied-index gate and all admission rules.

Update [`validate-docker-startup-budget`](../scripts/validate-docker-startup-budget),
Compose defaults, deployment environment examples, and their tests to derive
`T + I + existing named startup allowances`. Do not count catch-up grace twice.
Preserve conservative handling of unreadable container TOML paths and environment
precedence. Report both effective settings and where they came from. Do not
silently let existing health settings become too short after the formula changes.

The fleet's final-install setting remains 1,200 seconds for this rollout.
Do not lower it during this effort. The short non-final bound is now independent.

**Acceptance:** advancing transfers exceed `C` in total duration and complete;
an unanswered non-final RPC cancels within `C`; repeated mismatches terminate
at `T`; final restore may exceed `C` but cannot renew `I`; leader cancellation
interrupts an in-flight wait immediately. Virtual-time tests assert the exact
bounds and guard drops. Config and Docker tests cover minima, maxima, empty
overrides, invalid combinations, and both changed settings.

## 6. Milestone 4 — expose progress without creating authority or writes

Store bounded process-local progress per Raft group and peer, owned by the
network/node state rather than global statics shared between embedded nodes.
No SQL transaction, WAL entry, or roster heartbeat write is generated per chunk.
Use monotonic instants internally and serialize durations, not instants.

Expose a local snapshot handle and a new authenticated, read-only JSON route
on the existing Hiqlite cluster API listener, `/cluster/transport/sqlite`.
Reuse the authentication used by adjacent cluster metrics routes. It must read
only in-memory observations and work while Plurx store startup waits. Add a
client method for this route; do not change existing binary WebSocket response
variants to carry the new data. Older peers returning 404 mean unavailable.

Required fields, with explicit optionality:

| Field group | Required content |
|---|---|
| Identity | observing node, peer, Raft group, boot/attempt ID, snapshot ID, socket epoch |
| Transfer | attempted offset, acknowledged offset, locally received bytes when observed, total bytes when known |
| Time | attempt age, last acknowledgement age, last local receive age, active deadline remaining |
| Phase | connecting, transferring, awaiting acknowledgement, installing, retrying, failed, complete |
| Outcome | reconnect count, retry count, last error category, whether a receive/install operation still owns work |

Distinguish acknowledgements from local file bytes: the reproduced failure can
have new received bytes and a missing acknowledgement. Repeated duplicate
chunks are not net progress. Unknown totals and missing samples stay unknown.
Snapshot IDs and request IDs belong in logs/status, not unbounded Prometheus
labels. Bound peer records by membership plus a small expiring retired set.

Emit one structured phase-transition line, one completion/failure line, and a
rate-limited waiting line every 10 seconds before port 32400 opens. Include
startup target and local applied index once available. Do not log frame payloads,
SQL parameters, handshake secrets, or entire serialized requests to diagnose
transport progress.

Integrate the read-only transport observations into the existing cluster
support/status collection. Poll with bounded concurrency and a one-second
per-peer timeout; cache for five seconds. A page render or Prometheus scrape
must not wait for a peer, create a quorum read, or generate Raft writes.
Before startup, healthy peers can report their own outbound snapshot evidence
and retrieve the learner's cluster-listener evidence.

Keep `bounded_read_ready` as the permit. Render its explanation separately:
receiving snapshot · waiting for snapshot acknowledgement · installing snapshot
· retrying snapshot · snapshot stalled · waiting for startup watermark ·
progress unavailable. Expired status data must become unavailable, never
continue claiming active transfer. Show the observer and sample age. Port 32400
being unavailable before startup must not hide the transport status route.

**Acceptance:** fixtures for active, idle, installing, retrying, unavailable,
and recovered states yield distinct explanations. A learner with port 32400
closed is diagnosable from a healthy peer. Status/scrape tests assert zero
store calls and no unbounded network fan-out.

## 7. Milestone 5 — prove the combined behavior through the actual transport

Extend [`plurx-cluster-check`](../crates/plurx-cluster-check/src/lib.rs) with a
`transport-recovery` case and add a `make cluster-transport-recovery-check`
target. These names are new deliverables. Keep deterministic transport probes
in Hiqlite unit/integration tests, and use the separate-process harness for
node lifecycle. Enable fault injection only under `validation-test-helpers`.
Never use a production environment variable to enable the fault.

Use real TLS, the production handshake, WebSocket writer/reader functions,
production chunk size, and real snapshot files. A fake transport is allowed
below TLS for deterministic backpressure, not in place of the Raft protocol.
The old implementation must fail the missing-flush cases before the fix is
applied; record that result. Test timeouts observe a persistent reader task,
not a repeatedly canceled/resumed frame-reader future.

| Case | Injection | Required result |
|---|---|---|
| Sparse API request/reply | TLS tail backpressure with no following traffic | Reply completes after transport resumes; no keepalive or new request is required |
| Snapshot request tail | Hold final encrypted bytes of a 3 MiB frame | Frame is flushed and acknowledged on the same socket |
| Snapshot reply | Hold acknowledgement ciphertext after receiver accepts bytes | No idle wait beyond transport recovery; bytes and acknowledged offset converge |
| Real mismatch | Restart receiver after a nonzero chunk; keep sender's logical offset | Typed mismatch, offset-zero retry, same-socket progress, complete matching image |
| Lost reply | Deliver chunk, discard its reply, restore network | Retry cannot append duplicate bytes or corrupt the image |
| Mid-write disconnect | Pause actual snapshot write after a partial write, close socket | Node-owned operation finishes before same-offset retry |
| Final-install disconnect | Close socket after installation owns its durable candidate | Pending/current/WAL invariants survive; no concurrent overwrite |
| Broken writer, live reader | Return write or flush error while reads remain pending | Supervisor terminates both socket tasks and reconnects promptly |
| Queue pressure | Full write queue during reset, shutdown and leader change | No reset loss, no stale queued send, no blocked cleanup |
| Repeated mismatch/stall | Recoverable errors with no net transfer completion | Absolute stage/attempt deadlines fire; retries cannot extend them |
| Slow valid install | Final installation longer than `C`, shorter than `I` | Installation completes once; chunk watchdog does not kill it |
| Leader change | Change leader during transfer and while reply is missing | Old attempt cancels; new leader catches target up; votes and state agree |
| Restarting voter | One voter needs a snapshot under continuing writes | Majority continues committing; recovered voter reaches the checked watermark |
| Restarting learner | Learner needs snapshot during load | Majority and other serving nodes remain available; learner reenters only after proof |
| Rolling compatibility | Old/new sender and receiver combinations | No wire decode errors; new-side protection works; mixed fleet not declared fully fixed |

Use a generated SQLite image at least 88,559,616 bytes, with known marker data,
and a second image at least twice that size. Force snapshot transfer rather
than accidentally testing log replay. Record snapshot index, purge boundary,
applied index, actual bytes transferred, and image/content verification.
Continue a workload of uniquely identified writes; record acknowledged writes
and verify all of them after recovery. Ambiguous API writes may have committed:
resolve their outcome by identifier and do not blindly replay them.

After a one-time fault is released, the clean transfer must finish without
using the long installation timeout as a reconnect trigger. For a 30-second
chunk budget and a successful reconnect within the existing five-second
connection limit, require the next retry dispatch within 36 seconds of the
unanswered non-final RPC starting, including default retry backoff. This is a
healthy-network-after-release acceptance condition, not a guarantee during a
continuing partition. Writer-error cases should reconnect without waiting for
that 30-second budget.

Run 20 learner and 20 recovering-voter cycles on the supported Linux transport
path, including representative load. Record worst recovery time, transfer and
install durations separately, attempt counts, and post-quiescence socket/task
counts. A failed cycle fails qualification; do not retry the campaign until it
happens to produce a green artifact. Preserve the failure and fix its cause.

**Acceptance:** every matrix case passes, the old writer fails the deterministic
flush regressions, all acknowledged writes survive, and resource counts return
to their established baseline. Existing cluster failure drills, bounded reads,
WAL generation, snapshot publication, and membership regressions remain green.

## 8. Execution order, validation, and rollout

### 8.1 Branch and compiler discipline

Use one `effort/cluster-transport-recovery` branch and reviewable task branches
based on its current head. Follow the repository's effort merge convention.
This document's reviewed production SHA is
`c661d3874945f2e15bd151481fd7413286a127e7`. Remote `main`, verified read-only on
2026-09-05, was `d9fb4dafaa0206aeede31ad0b844625101e30938`; its relevant Raft
transport files match the production SHA. Re-verify current source before
editing because the local checkout was 41 commits behind remote main and had
unrelated user changes. Never reset or overwrite those changes.

The local host does have Rust 1.97.1 under rustup; the default shell selected
Homebrew Rust 1.95.0. Put the pinned toolchain on PATH or use rustup explicitly
and verify `rustc --version`. Do not call a successful unpinned build evidence.
Use the source-only cloud loop if the actual executor lacks the pin.

For each milestone, run its focused regression plus the relevant compiler loop.
Existing commands to retain include:

```bash
cargo fmt --all -- --check                         # Workspace formatting
cargo check --locked -p plurxd --all-targets        # Daemon integration
cargo clippy --locked -p plurxd --all-targets -- -D warnings
cargo test --locked -p plurxd --bin plurxd          # Daemon behavior
cargo test --locked --manifest-path vendor/hiqlite/Cargo.toml \
  --no-default-features --features auto-heal,cache,macros,sqlite \
  network:: --lib                                  # Vendored transport tests
python3 -m unittest tests.operations.test_docker_startup_budget
```

Add and wire the new recovery target into affected-surface validation and full
promotion qualification, not merely a Makefile target nobody runs. Record
nonzero test counts; exact filters that run zero tests are failures. Check both
root and standalone vendor lockfiles, SQLite-only and SQLite+cache features.
Format/check the vendored crate explicitly because workspace formatting does
not necessarily cover excluded path dependencies. Do not broaden dependency
versions incidentally.

After integrating all milestones, merge current main into the effort, archive
that exact tree, and rerun the checks and recovery campaign. Require the current
`Main promotion gate` and qualification receipt before merging the effort.
Update [OPERATIONS.md](OPERATIONS.md), deployment docs, patch provenance, and
regression mappings in the commits that change their behavior.

### 8.2 Milestone completion is evidence, not a helper-test checklist

| Milestone | Reviewable output | Blocking evidence |
|---|---|---|
| M1 | Flush all message boundaries; bounded frame writer | Both pinned-library failures reproduced, production writer regressions pass |
| M2 | Supervised socket lifecycle; node-owned snapshot execution | Partial-write safety, queue-pressure teardown, resource bounds |
| M3 | Chunk/transfer/install deadlines and startup formula | Exact virtual-time bounds; config and Docker precedence tests |
| M4 | Local transport status and operator explanation | Pre-HTTP diagnosis; no store writes; stale observations expire |
| M5 | Separate-process recovery campaign and gate integration | Full matrix, Linux cycles, acknowledged-write preservation |

Do not mark the effort complete after M1 alone. Do not keep M1 unshipped merely
to make unrelated clustering improvements; it can be reviewed and qualified as
an independent ordinary change if an earlier mitigation release is necessary,
with the remaining milestones explicitly open.

### 8.3 Fleet acceptance and rollback

First qualify in the harness. Then use the supported rolling deployment and
restart-preparation proof. Keep a ready voting majority throughout. Confirm
build identity and effective budgets on every node. Do not induce partitions,
kill voters, or delete state in production as a substitute for the harness.

Observe one normal learner restart and supported one-at-a-time voter restarts
with production-sized state. Retain phase/status output, voter readiness,
write success, and convergence. Where normal restarts use retained logs,
identify them as log replay rather than claiming snapshot-transfer evidence.
Snapshots should complete near the clean baseline; no 900-second idle episode
is acceptable. Measure startup probes separately from transport recovery.

This effort makes no database or Raft wire-format migration, so binary rollback
remains possible subject to the repository's normal rollback contract. Rollback
restores the old missing-flush exposure; it is not a recovery fix. On regression,
stop the rollout, preserve the ready majority and evidence, and use the existing
operational recovery procedure. Never wipe a database to make the test pass.

## Appendix A — complete pinned-library reproduction

This probe demonstrates the TLS defect independently of database contents. It
uses a real TLS handshake and real WebSocket frames; only underlying transport
backpressure is controlled. It deliberately reproduces the old write sequence,
then proves that flushing the same frame resolves it. During implementation,
port the mechanism into tests of the production helper and full transport.

Create a scratch Cargo project with the following `Cargo.toml` and `src/main.rs`.
The project needs no repository credentials. The four transport dependencies
and certificate helper below match the deployed root lockfile.

```toml
[package]
name = "plurx-tls-probe"
version = "0.1.0"
edition = "2024"
[dependencies]
fastwebsockets = { version = "=0.10.0", features = ["unstable-split"] }
tokio = { version = "=1.53.1", features = ["full"] }
tokio-rustls = "=0.26.4"
rustls = "=0.23.42"
rcgen = "=0.14.8"
```

```rust
use fastwebsockets::{FragmentCollectorRead, Frame, Payload, Role, WebSocket};
use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Default)]
struct Gate {
    remaining: Option<usize>,
    waker: Option<Waker>,
}
struct GatedIo {
    inner: DuplexStream,
    gate: Arc<Mutex<Gate>>,
}
impl AsyncRead for GatedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for GatedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        {
            let mut gate = self.gate.lock().unwrap();
            if gate.remaining == Some(0) {
                gate.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        let limit = self
            .gate
            .lock()
            .unwrap()
            .remaining
            .unwrap_or(buf.len())
            .min(buf.len());
        let result = Pin::new(&mut self.inner).poll_write(cx, &buf[..limit]);
        if let Poll::Ready(Ok(n)) = result
            && let Some(remaining) = self.gate.lock().unwrap().remaining.as_mut()
        {
            *remaining -= n;
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for (role, len) in [(Role::Server, 32), (Role::Client, 3 * 1024 * 1024)] {
        tokio::time::timeout(Duration::from_secs(5), run(role, len))
            .await
            .expect("probe must complete within five seconds");
    }
}

async fn run(role: Role, len: usize) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert.der().clone()], key.into())
        .unwrap();
    let client = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let gate = Arc::new(Mutex::new(Gate::default()));
    let other_gate = Arc::new(Mutex::new(Gate::default()));
    let connector = TlsConnector::from(Arc::new(client));
    let acceptor = TlsAcceptor::from(Arc::new(server));
    let (client_tls, server_tls) = tokio::join!(
        connector.connect(
            "localhost".try_into().unwrap(),
            GatedIo {
                inner: client_io,
                gate: if role == Role::Client {
                    gate.clone()
                } else {
                    other_gate.clone()
                },
            }
        ),
        acceptor.accept(GatedIo {
            inner: server_io,
            gate: if role == Role::Server {
                gate.clone()
            } else {
                other_gate.clone()
            }
        })
    );
    let client_tls = tokio_rustls::TlsStream::Client(client_tls.unwrap());
    let server_tls = tokio_rustls::TlsStream::Server(server_tls.unwrap());
    let (sender_tls, receiver_tls, remote_role) = if role == Role::Server {
        (server_tls, client_tls, Role::Client)
    } else {
        (client_tls, server_tls, Role::Server)
    };
    let sender = WebSocket::after_handshake(sender_tls, role);
    let receiver = WebSocket::after_handshake(receiver_tls, remote_role);
    let (sr, mut sw) = sender.split(tokio::io::split);
    let (rr, _rw) = receiver.split(tokio::io::split);
    let sender_reader = tokio::spawn(async move {
        let mut sr = FragmentCollectorRead::new(sr);
        sr.read_frame(&mut |_| async { Ok::<_, io::Error>(()) })
            .await
    });
    let mut receive_task = tokio::spawn(async move {
        let mut rr = FragmentCollectorRead::new(rr);
        rr.read_frame(&mut |_| async { Ok::<_, io::Error>(()) })
            .await
            .unwrap()
            .payload
            .to_vec()
    });
    gate.lock().unwrap().remaining = Some(if len > 64 * 1024 { len } else { 0 });
    sw.write_frame(Frame::binary(Payload::Owned(vec![42; len])))
        .await
        .unwrap();
    println!(
        "{} {len} bytes: write_frame returned Ok with ciphertext transport blocked",
        if role == Role::Server {
            "server"
        } else {
            "client"
        }
    );
    {
        let mut g = gate.lock().unwrap();
        g.remaining = None;
        if let Some(w) = g.waker.take() {
            w.wake();
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut receive_task)
            .await
            .is_err()
    );
    println!(
        "transport writable again; peer still missing frame after 200ms; sender reader remains active"
    );
    assert!(
        !sender_reader.is_finished(),
        "sender reader must remain alive"
    );
    sw.flush().await.unwrap();
    let received = tokio::time::timeout(Duration::from_secs(1), &mut receive_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, vec![42; len]);
    println!(
        "explicit flush delivered the same frame on the same connection, without mismatch or reconnect"
    );
    sender_reader.abort();
}
```

Run from the scratch project's directory using the repository-pinned compiler:

```bash
rustc --version                                    # Must report 1.97.1
cargo run                                          # Reproduce both stalls, then flush
cargo clippy -- -D warnings                         # Check the probe itself
```

Once Cargo has resolved the scratch project, retain its lockfile and use
`--locked` for subsequent runs. The investigation used cached dependencies
with `--offline --locked`; offline mode is optional on a new machine.

Expected output:

```text
server 32 bytes: write_frame returned Ok with ciphertext transport blocked
transport writable again; peer still missing frame after 200ms; sender reader remains active
explicit flush delivered the same frame on the same connection, without mismatch or reconnect
client 3145728 bytes: write_frame returned Ok with ciphertext transport blocked
transport writable again; peer still missing frame after 200ms; sender reader remains active
explicit flush delivered the same frame on the same connection, without mismatch or reconnect
```

**How to read it:** the adapter first permits TLS to buffer the end of a frame,
then restores underlying transport writes and wakes the registered task. Both
readers remain active. No complete frame arrives until an explicit flush.
Changing the fault to a fabricated mismatch is not an equivalent reproduction.
This is a dependency-level probe; the release requires §7's tests through the
actual production writer and node stack as well.
