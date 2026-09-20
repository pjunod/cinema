# Raft snapshot cadence and the consistent cut — measure, then move the copy off the writer without moving the cut

**Status:** ready for review · **Executes:** S2, S5, F-sc-2, F-sc-5 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Read §2 first: it is the exact writer sequence the assessment's correction 4
protects, and every design line in §3 is derived from it. Work M0 before
anything else — the review's "≥8 s stall self-fences every lease" is a model
until a voter has reported a build-time histogram. M1 and M3 are safe to ship
on measurement alone; M2 is a vendored patch to the state machine and does
not ship until its single-cut test (§5.3) is green on the fork. If a step
seems to require changing `max_in_snapshot_log_to_keep`, letting a read-pool
connection produce the snapshot without the writer fixing the cut, or turning
a storage refusal into a builder `Err`, stop and flag it.

**Correction to the review:** two placements are wrong, one nuance matters.

1. S5 reads as if the 512 MiB floor guards snapshot space. It does not.
   `MIN_VOTER_STORAGE_HEADROOM_BYTES`
   ([membership.rs:144](../../crates/plurx-core/src/cluster/membership.rs))
   is consulted only when publishing `voter_storage_ready` in the heartbeat
   (`:4320`) and in the learner→voter promotion preflight (`:7548-7558`).
   Nothing checks free space before a snapshot build; the first symptom of a
   full disk is the fatal error S5 describes. M3 adds the check where it is
   missing rather than resizing a constant that was never on that path.
2. Openraft does not serialize the build with apply. `sm::Worker::build_snapshot`
   spawns the builder as its own task (openraft 0.9.25,
   `src/core/sm/worker.rs:177-193` in the cargo registry, not vendored) and
   its contract (`:167-175`) says the builder must either hold a consistent
   view or take a lock that excludes writes. The
   serialization is hiqlite's: one writer thread, one `flume::bounded(1)`
   channel, and apply awaiting each request (§2.1). That is the thing M2
   changes, and it is why M2 is a fork patch and not an openraft one.
3. A builder `Err` is fatal because `RaftCore` propagates it with `?`
   (`raft_core.rs:1381`, `let res = command_result.result?;`). There is no
   retryable variant to return; owning the builder lets us *wait*, not
   *refuse* (§3.3).

## 1. Objective

Know the snapshot cost on every voter as a number; pick `logs_until_snapshot`
from that number, the disk, and the restart-replay it buys; then take the
`VACUUM INTO` off the apply path while keeping the database image, the
last-applied log id and the membership at one logical cut; and never let a
full disk turn a routine snapshot into a dead voter without warning first.

## 2. Contract today

Re-verify each line at build time.

### 2.1 The writer sequence, exactly

```rust
// vendor/hiqlite/src/store/state_machine/sqlite/writer.rs:143
let (tx, rx) = flume::bounded::<WriterRequest>(1);
```

Apply, per entry ([state_machine.rs:1131-1146](../../vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs)):

```rust
EntryPayload::Normal(QueryWrite::Execute(Query { sql, params })) => {
    let (tx, rx) = oneshot::channel();
    let query = writer::Query::Execute(writer::SqlExecute { sql, params, last_applied_log_id, tx });
    self.write_tx.send_async(WriterRequest::Query(query)).await.expect(..);
    let result = rx.await.expect("to always get a response from sql writer");
```

Membership entries (`:1256-1270`) send `WriterRequest::MetadataMembership
{ last_membership, last_applied_log_id }` and the writer stores both in the
in-memory `sm_data` only (`writer.rs:615-619`). Blank entries send nothing
(`:1128`, with a `TODO`).

Snapshot ([writer.rs:503-536](../../vendor/hiqlite/src/store/state_machine/sqlite/writer.rs)):

```rust
WriterRequest::Snapshot(SnapshotRequest { snapshot_id, path, ack }) => {
    sm_data.last_snapshot_id = Some(snapshot_id.to_string());
    persist_metadata(&conn, &sm_data).expect("Metadata persist to never fail");
    // REPLACE INTO _metadata (key, data) VALUES ('meta', $1)        (:722-730)
    match create_snapshot(&conn, path) {
    // VACUUM main INTO '{path}'                                        (:763-767)
        Ok(_)   => { PRAGMA optimize; ack.send(Ok(SnapshotResponse { meta: sm_data.clone() })) }
        Err(e)  => ack.send(Err(StorageError::IO { source: StorageIOError::write(&e) })),
    }
}
```

So the order on one thread is: **all entries ≤ K applied → `_metadata`
written with `last_applied = K` and the current membership → copy → ack
with the same `sm_data`**. Nothing is applied between the metadata write and
the end of the copy because the next `WriterRequest` is not dequeued until
this arm returns. The builder ([snapshot_builder.rs:86-120](../../vendor/hiqlite/src/store/state_machine/sqlite/snapshot_builder.rs))
turns `resp.meta` into `SnapshotMeta { last_log_id, last_membership }`,
fsyncs and renames the file, and publishes the `current` pointer. Restore
(`writer.rs:538-591`) reads `_metadata` back out of the installed image and
adopts it as `sm_data`. Three copies of the cut, one source.

The consequence the review names: while the copy runs, every apply on this
node waits on the bounded channel. A leader's apply stall delays commit
acknowledgement to clients; a follower's stall grows `apply_lag_entries`
and, past the watermark budget, drops it from bounded reads.

### 2.2 Cadence and retention

```rust
// crates/plurx-core/src/cluster/migration.rs:2326-2347
let mut raft_config = NodeConfig::default_raft_config(10_000);   // SnapshotPolicy::LogsSinceLast(10_000)
...
wal_size: HIQLITE_WAL_SIZE_BYTES,                                 // 16 MiB segments (:127)
```

`max_in_snapshot_log_to_keep` is 1 (asserted at `migration.rs:3726`) and
[CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) §6.6 says never to
change it. After each snapshot the log is purged to it, so a crash-restart
that hits `auto-heal` (lock file present → state-machine database deleted,
`migration.rs:1128-1139`) rebuilds from the last snapshot plus at most
`logs_until_snapshot` entries of replay.

### 2.3 What is measured already

`plurx_raft_snapshot_seconds{operation="build"|"install",outcome="ok"|"error"}`
is a 16-bucket histogram from 1 ms to 300 s
([snapshot_metrics.rs:9-26](../../vendor/hiqlite/src/snapshot_metrics.rs),
rendered at [system.rs:4761-4782](../../crates/plurxd/src/http/system.rs)).
`plurx_raft_commit_index`, `plurx_raft_applied_index`,
`plurx_raft_apply_lag_entries` are gauges. Nothing reports the state-machine
file sizes.

### 2.4 The leases that a stall touches

`LEASE_INTERVAL = 3 s`, `LEASE_TTL_MS = 12_000`, renewals filtered out with
less than `LEASE_RENEWAL_MIN_REMAINING_MS = 4_000` left, under a
`LEASE_RENEWAL_DEADLINE = 4 s`
([media_sessions.rs:89-90, 133-136](../../crates/plurxd/src/media_sessions.rs)).
A renewal proposed just before a stall of length B commits at B; the
assessment's point stands that "when every session fences" depends on when
each session's last renewal committed, so the exposure is a distribution over
B, not a step at 8 s. The cluster-job lease is 90 s with a 30 s heartbeat
([job_lease.rs:37-38](../../crates/plurxd/src/job_lease.rs)) and is not at
risk from any build under a minute.

## 3. Change

### 3.1 M0 — measure (no behaviour change)

Add `plurx_raft_state_machine_bytes{file="db"|"wal"|"snapshots"|"logs"}`
(gauge, four fixed label values) sampled with the existing passive metrics
tick from `stat` of `state_machine/db/plurx.db`, its `-wal`, the sum of
`state_machine/snapshots/*`, and the sum of `logs/*`. Then pull, per voter,
over 24 h: build p50/p99, builds per day, DB bytes, commit-index rate.
Snapshots per day = commit rate ÷ 10,000 — with S3's 1 Hz outbox proposal on
three voters alone contributing ~26 builds per node per day today.

### 3.2 M1 — the threshold, from the numbers

Let B = measured p99 build seconds, E = entries per day, S = DB bytes, W =
average serialized entry bytes (log bytes ÷ entries from
`plurx_raft_state_machine_bytes{file="logs"}` between two purges), A =
apply throughput in entries/s during a replay (measure once: restart a lab
follower after N entries and time `applied_index` catching up).

Raising `N = logs_until_snapshot` does **not** shorten B — B depends on S —
it only divides the stalls per day by N/10,000. It costs: log disk ≈ N × W
(plus one 16 MiB segment), and restart replay ≈ install(S) + N ÷ A. Choose
the smallest N such that stalls/day × B is below the budget you accept for
apply latency (proposal: ≤ 60 s of cumulative writer stall per node per day
until M2 lands), subject to N × W ≤ 1 GiB and N ÷ A ≤ 60 s. Do not adopt the
appendix's 100k–500k without those three numbers; with W ≈ 10 KB and a
replay at a few hundred entries/s, 500k is gigabytes of log and minutes of
replay on every crash.

The setting is symmetric across voters (CLUSTER-PERFORMANCE-PLAN.md §3.5):
a node-local `cluster.logs_until_snapshot` config key with a documented
range (`1_000..=200_000`), default 10,000, validated at startup, applied
through `production_hiqlite_defaults`, and OPERATIONS.md says it takes
effect at the next restart on each voter and must match on all.

### 3.3 M2 — off-writer copy, same cut

Requirement (assessment correction 4): the image, the `_metadata` row inside
it, and the `SnapshotMeta` returned to openraft must describe the same
applied prefix and membership, with nothing applied in between.

Design: the writer thread still fixes the cut; only the byte copy leaves it.

```text
writer thread                              snapshot copy task (blocking pool)
─────────────────────────────────────────  ─────────────────────────────────
recv Snapshot{id, path}
persist_metadata(conn, sm_data)   ── commit ──►  (row is in the WAL)
snap_conn.execute_batch(
  "BEGIN; SELECT count(*) FROM _metadata;")   ← pins the WAL read mark on a
                                                second connection, on this
                                                thread, before the next recv
hand (snap_conn, sm_data.clone(), path) ──►  rusqlite Backup(snap_conn → path)
return to recv loop; apply continues          step(…) until Done
                                              COMMIT (releases the read mark)
                                              fsync; ack Ok(meta)
```

Why these pieces:

- **The read transaction is opened by the writer thread, after its own
  commit and before it dequeues anything.** SQLite WAL gives that connection
  a snapshot at the end mark of the last commit — the one that contains
  `_metadata(last_applied = K)`. Anything the writer applies afterwards is
  beyond the mark and invisible to the copy. That is the single cut.
- **`VACUUM INTO` cannot be used off-writer.** It refuses to run inside a
  transaction, and outside one it would read the moving head. The online
  backup API (`rusqlite::backup::Backup`, already used by
  `snapshot_sqlite_file` at
  [migration.rs:1405-1440](../../crates/plurx-core/src/cluster/migration.rs))
  copies the source connection's current read snapshot page by page.
- **`snap_conn` is a dedicated connection**, opened once by the writer, never
  in the read pool, `SQLITE_OPEN_READ_ONLY`. One outstanding copy at a time;
  a second `Snapshot` request while one is in flight waits for the ack (the
  builder already serializes on `snapshot_files`).
- **The pinned read mark stops WAL checkpoints from resetting past it** for
  the duration of the copy. The WAL grows by the writes applied during the
  copy; M3's floor accounts for that (`wal_growth ≈ apply_rate × copy_time ×
  W`).
- **Failure**: a copy error acks `Err` exactly as today (fatal, unchanged
  semantics); the writer is not involved after the handoff, so a failed copy
  cannot corrupt the live database. `last_snapshot_id` is set only in the
  ack path, after success.

Files touched: `writer.rs` (new arm shape, second connection),
`snapshot_builder.rs` (unchanged contract), `PLURX-PATCH.md` (#16). The
`_metadata` contents, the snapshot file format and the install path do not
change, so mixed-version fleets are unaffected.

`probe_json`: moving it to a side table in the same database does not shrink
the image — every page is still copied. Compressing the *file* (zstd on the
published snapshot, decompress on install) or moving probe JSON to a separate
file are storage/transfer changes with their own measurement; neither is in
this plan.

### 3.4 M3 — storage floor and deferred admission

Floor, computed on the state-machine filesystem (statvfs of
`state_machine/db`) at build time:

```text
required = S                          (image copy)
         + max(WAL now, 2 × 16 MiB)   (pinned WAL growth during copy; M2)
         + S                          (the previous `current` remains until
                                       cleanup, plus the `.temp` in flight)
         + 64 MiB                     (SQLite temp, directory fsyncs, slack)
```

so roughly 2 × S + WAL + slack — not "1.5 × DB", which the assessment
correctly calls a proposal. Where it is enforced: at the top of
`build_snapshot` in the fork, **before** the `WriterRequest::Snapshot` is
sent. If `available < required`, the builder does not return `Err` (fatal);
it sleeps 10 s and re-checks, up to a bound (default 10 min), incrementing
`plurx_raft_snapshot_deferrals_total{reason="storage"}` and logging once per
minute with both numbers. The builder task is openraft's own spawned task
(§0 correction 2), so waiting there does not block apply; it does delay log
purge, which is why the wait is bounded. Past the bound it proceeds exactly as
today, so behaviour under a permanently full disk is unchanged rather than
newly hidden — and the alert has had ten minutes to fire.

The same `required` replaces `MIN_VOTER_STORAGE_HEADROOM_BYTES` in the
promotion preflight and the heartbeat's `voter_storage_ready`, with the
constant kept as the minimum (`max(512 MiB, required)`), so a learner is not
promoted onto a disk that cannot take its first snapshot.

### 3.5 Alerts

| Series | Condition | Owner |
|---|---|---|
| `plurx_raft_snapshot_seconds{operation="build",outcome="error"}` count | any increase | page |
| `plurx_raft_snapshot_deferrals_total{reason="storage"}` | rate > 0 for 5 min | page |
| build p99 (`_bucket` ratio at `le="5"`) | < 0.99 over 1 h | ticket |
| `plurx_raft_state_machine_bytes{file="logs"}` | > 2 × N × W | ticket |
| `plurx_raft_apply_lag_entries` | > `bounded_replica_max_lag_entries` for 30 s | ticket |

## 4. Guardrails (non-goals)

- **`max_in_snapshot_log_to_keep` stays 1** (CLUSTER-PERFORMANCE-PLAN.md
  §6.6; the disaster-recovery retention hiqlite ties to it).
- **No read-pool `VACUUM INTO`.** The cut is fixed by the writer thread or
  it is not a cut (§3.3, correction 4).
- **No new `StorageError` variant, no "retryable" builder result** —
  openraft 0.9.25 has none; F-sc-5. Deferral is a bounded wait.
- **No blanket threshold.** N is chosen per §3.2 from measured B, E, W, A.
- **No `probe_json` side table as a snapshot-size remedy.** Row-access
  wins belong to [SQLITE-READ-PATH-AND-QUERY-PLANS.md](SQLITE-READ-PATH-AND-QUERY-PLANS.md).
- **Snapshot file format, `_metadata` schema and install path unchanged**,
  so [CLUSTER-BACKUP-AND-RESTORE.md](CLUSTER-BACKUP-AND-RESTORE.md)'s
  artefact and the transfer/catch-up contracts in
  [LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md](LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md)
  keep working.

## 5. Milestones

### 5.1 M0 — size gauges and the fleet readout

Add `plurx_raft_state_machine_bytes` to the passive metrics sampler and
`/metrics`; extend the Cluster page's `SnapshotStatus` with `db_bytes`.

Acceptance: `cargo test -p plurxd http::system::metrics` renders the four
label values; the GPT readout below is attached to the PR that picks N.

```text
GPT prompt (fleet, Prometheus or curl on lab1–lab3 and media1):
For each voter, over the last 24 h, give me: plurx_raft_snapshot_seconds
build count and the p50/p99 from the _bucket series; the increase in
plurx_raft_commit_index; plurx_raft_state_machine_bytes for db, wal,
snapshots and logs (or `du -b` of hiqlite/state_machine/db,
hiqlite/state_machine/snapshots, hiqlite/logs if the gauge is not deployed
yet); and whether plurx_raft_apply_lag_entries exceeded 64 at any build.
Then restart lab3's plurxd once and report seconds until
plurx_raft_applied_index equals plurx_raft_commit_index.
```

### 5.2 M1 — `cluster.logs_until_snapshot`

Config key, validation, plumbing through `production_hiqlite_defaults`,
OPERATIONS.md entry, and the decision recorded in the PR body with B, E, W,
A and the chosen N.

Acceptance: `cargo test -p plurx-core cluster::migration::production_hiqlite`
proves the default is 10,000 and the configured value reaches
`raft_config.snapshot_policy`; the harness test
`tests::a_legacy_launch_retains_the_production_snapshot_policy` in
`plurx-cluster-check` still passes (`make cluster-harness-check`).

### 5.3 M2 — off-writer copy (vendored patch #16)

Acceptance, all in `vendor/hiqlite` tests run by `make hiqlite-vendor-clippy`
plus `cargo test -p hiqlite --features sqlite snapshot`:

- `single_cut_under_concurrent_apply`: start a copy, apply 1,000 entries
  while it runs (the test pauses the backup step), assert the image's
  `_metadata.last_applied` equals the acked `meta.last_log_id`, the image
  contains rows for every entry ≤ that index and none above it, and the
  writer's applied index advanced past it during the copy.
- `membership_change_during_copy`: a `MetadataMembership` applied mid-copy
  is absent from the image and from `meta.last_membership`.
- `copy_failure_is_fatal_and_leaves_db_intact`: an injected step error acks
  `Err`, the live database passes `integrity_check`, no `current` pointer
  moved.
- `wal_read_mark_released`: after the ack, a checkpoint truncates the WAL.
- `make cluster-store-check` and `make cluster-daemon-check` unchanged.

### 5.4 M3 — floor and deferral

Acceptance: a fork unit test with an injected `statvfs` returns
`available < required` → `deferrals_total` increments, no request reaches
the writer, and after the bound the build proceeds; `cargo test -p plurx-core
cluster::membership::promotion` proves `max(512 MiB, required)` in the
preflight; `make storage-pressure-check` (root, Linux; see
CLUSTER-PERFORMANCE-PLAN.md §6.6) records a real `ENOSPC` refusal that does
**not** report a successful snapshot.

## 6. Verification and rollout

Fast lane per PR: `make unit`; M2/M3 additionally `make
hiqlite-vendor-clippy` and the named `cargo test -p hiqlite` filters; M1 runs
`make cluster-harness-check`. Rollout order M0 → M1 → M3 → M2. M1 is a
config change rolled voter by voter with identical values; a rollback is the
previous value. M2 changes no on-disk format and can be rolled back by
image; a fleet may run mixed M2/non-M2 voters because the cut semantics are
identical. M3's deferral is observable only under low space.

## 7. Open questions

1. The apply-stall budget in §3.2 (60 s cumulative per node per day before
   M2) is a proposal; Paul picks the number.
2. Whether `cluster.logs_until_snapshot` should instead be a replicated
   setting so it cannot drift between voters — it is read at hiqlite
   construction, before the store exists, so a config key is the honest
   choice unless a restart-time read of the replicated value is added.
3. M2's pinned read mark and hiqlite's own checkpointing: confirm hiqlite
   does not run `wal_checkpoint(TRUNCATE)` on a timer that would now log a
   busy error during copies (grep `wal_checkpoint` in the vendor tree at
   build time).
