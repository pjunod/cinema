# Raft snapshot cadence and the consistent cut — measure, then move the copy off the writer without moving the cut

**Status:** done — M0–M3 on `main` since 2026-10-04 (#793): fork patch 22
(writer-fixed cut, off-writer copy) and storage admission run on every voter,
with no switch, by design. Paul accepted the shipped path on 2026-10-04 and
declined a 24-hour undisturbed readout; the retained 2026-10-07 production-data
snapshot observation below closes that accepted residual. Cadence tuning and
restore/release qualification are separate, not newly satisfied by this receipt
· **Executes:** S2, S5, F-sc-2, F-sc-5 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first: it is the exact writer sequence the assessment's correction 4
protects, and every design line in §3 is derived from it. Work M0 before
anything else — the review's "≥8 s stall self-fences every lease" is a model
until a voter has reported a build-time histogram. M1 and M3 are safe to ship
on measurement alone; M2 is a vendored patch to the state machine and does
not ship until its single-cut test (§5.3) is green on the fork. If a step
seems to require changing `max_in_snapshot_log_to_keep`, letting a read-pool
connection produce the snapshot without the writer fixing the cut, or turning
a storage refusal into a builder `Err`, stop and flag it.

**2026-09-30 execution amendment (owner ruling, with Astra consultation):**
the original 24-hour M0 readout is a prerequisite for **changing N**, not for
implementing M1's validated default plumbing, M3's specified size-based floor,
or M2's tested consistent-cut correctness. Preserve N = 10,000 and retention
1. Do not infer fleet B/E/S/W/A from unit tests or restart a voter to obtain
them. The historical M0 stop below remains as a receipt of that earlier run;
this amendment supersedes its instruction not to begin correctness work.
These automatic correctness changes have no meaningful manual on/off and
therefore do not acquire a Developer switch. Readiness remains advisory.

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

## 2. Contract at the assessment base — retained source evidence

This section records the 2026-09-20 source boundary. The 2026-09-30 execution
amendment and execution receipt below describe the implemented replacement;
the old VACUUM writer sequence is evidence for the design, not a claim that
the task branch still runs it.

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

The 2026-09-30 task implements this plumbing at the incumbent default; it
does not select a new N. The allowed range is a validation envelope, not
evidence that every value inside it is appropriate for this fleet.

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

  The implementation opens one dedicated read-only connection per copy
  rather than retaining an idle connection between builds. At most one is
  outstanding; all production builders share `snapshot_files`. The writer
  pins its transaction before handing it to the blocking pool. Shutdown and
  install wait for the outstanding copy to release its read mark.
- **The pinned read mark stops WAL checkpoints from resetting past it** for
  the duration of the copy. The WAL grows by the writes applied during the
  copy; M3's floor accounts for that (`wal_growth ≈ apply_rate × copy_time ×
  W`).
- **Failure**: a copy error acks `Err` exactly as today (fatal, unchanged
  semantics); the writer is not involved after the handoff, so a failed copy
  cannot corrupt the live database. `last_snapshot_id` is set only in the
  ack path, after success.

  Completion persists only the latest live metadata with the successful id;
  it never replaces later applies or membership with the cut's older values.
  A cancelled reply still owns its running copy and read mark. One successor
  can wait locally without blocking ordinary applies; cancelled queued work
  is discarded before fixing another cut. This closes the cancellation gap
  where a builder has released `snapshot_files` but its copy is still running.

Files touched: `writer.rs` (new arm shape, second connection),
`snapshot_builder.rs` (unchanged publication contract), `PLURX-PATCH.md`
(#22; #16 was already assigned to the deployed Backup ordinal). The
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

The target computes the floor, not the coordinator. Its existing heartbeat
durability probe and free-space sample both use the database parent, not a
different root filesystem if the state-machine directory is separately mounted.
Its existing heartbeat
batch publishes the readiness boolean and the bounded capability
`snapshot_storage_floor_v1`. Promotion requires the capability timestamp to
equal both `cluster_node_progress.observed_at` and `cluster_nodes.last_seen_at`,
in addition to the existing freshness, durability, and apply-barrier checks.
This distinguishes an upgraded target's floor proof from a legacy target's
512 MiB-only readiness without a new table, schema migration, or extra Raft
proposal. A missing/stale marker refuses promotion as unknown; upgrade the
target rather than treating the old boolean as proof. The diagnostic
`required_bytes = 512 MiB` remains the minimum, not the measured target floor;
the passive `plurx_raft_snapshot_required_storage_bytes` gauge reports the
last known exact local floor. Its absence is unknown, not zero.

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

For the default-preserving 2026-09-30 implementation, record B/E/W/A as
unmeasured and N = 10,000 unchanged. Those measurements remain required
before an operational tuning change.

Acceptance: `cargo test -p plurx-core cluster::migration::production_hiqlite`
proves the default is 10,000 and the configured value reaches
`raft_config.snapshot_policy`; the harness test
`tests::a_legacy_launch_retains_the_production_snapshot_policy` in
`plurx-cluster-check` still passes (`make cluster-harness-check`).

### 5.3 M2 — off-writer copy (vendored patch #22)

Acceptance, in `vendor/hiqlite` tests plus `make hiqlite-vendor-clippy`.
Use the production `auto-heal,cache,macros,sqlite` features for the recovery
group; a SQLite-only build deliberately lacks the auto-heal required by two
retained recovery cases. The implemented focused names are:

- `fixed_readmark_copy_allows_1000_applies_and_membership_without_cut_drift`:
  start a copy, apply 1,000 entries
  while it runs (the test pauses the backup step), assert the image's
  `_metadata.last_applied` equals the acked `meta.last_log_id`, the image
  contains rows for every entry ≤ that index and none above it, and the
  writer's applied index advanced past it during the copy.
  The same case proves a mid-copy `MetadataMembership` is absent from the
  image and returned membership, TRUNCATE is busy while pinned, and the WAL
  truncates after completion.
- `full_destination_copy_keeps_live_metadata_and_database_intact` and
  `off_writer_copy_failure_preserves_published_generation_and_live_integrity`:
  a real SQLite FULL error acks `Err`, live integrity remains `ok`, and the
  actual builder's `current` pointer does not move.
- `cancelled_copy_reply_retains_reader_and_queues_successor_without_blocking_apply`:
  dropped replies neither detach a reader nor turn the successor into a fatal
  busy error; ordinary applies finish while the first copy is paused.
- `blank_entry_advances_the_actual_snapshot_cut`: the actual Raft apply path
  acknowledges blank log ids before the snapshot cut.
- The seven-case `snapshot_metrics_contracts` group retains the real
  build/install, legacy-current and cancelled-promotion recovery checks.
  Full `cluster-store-check`/`cluster-daemon-check` remain final-promotion work.

### 5.4 M3 — floor and deferral

Acceptance: `snapshot_admission::tests` proves the exact floor, ten-second
insufficient-space retries, passive counter increments, successful release,
and a ten-minute bound even when the filesystem is unknown. The builder
awaits that admission before creating/sending its writer request. Core's
`voter_snapshot_storage_floor_is_target_local_known_fresh_and_at_least_512_mib`
pins the maximum floor and fail-closed unknown/stale durability. The
`snapshot_floor_requires_current_process_heartbeat_proof` regression runs
the production target SQL against real SQLite legacy, stale, and current
capability rows (including unchanged removed-node lookup behavior).
`real_enospc_on_an_owned_bounded_filesystem_preserves_live_state` is explicitly
ignored without its agent-owned bounded filesystem; the recorded isolated
OS ENOSPC run refuses success and retains live integrity. The broader Linux
`storage-pressure-check` remains a separate promotion qualification, not
permission to fill a host disk.

## 6. Verification and rollout

Each task branches from `effort/architecture-review-2026-09-20` and opens its
PR back into that effort. Establish the pinned Rust 1.97.1 loop before edits;
run the normal tracked hook, affected checks, vendor Clippy, and the smallest
named regressions. Core storage regressions enable `hiqlite-store`. Record
each `Regression-Test` in the task PR and landing message; the current
`Effort development gate` blocks task merging. Full store/daemon suites and
qualification belong to the final promotion, not a broad per-task unit sweep.
When tasks finish, freeze merges, merge current `main` into the effort, and
qualify the exact candidate through `Main promotion gate` plus its receipt.
Neither a task test nor an effort gate authorizes production rollout.

Implementation order is M1 default plumbing → M3 → M2; M0 tuning evidence
continues independently. Any later cadence change requires fleet evidence,
identical configured values, and separately authorized voter restarts. M2
changes neither wire ordinals nor the image/install format; mixed M2/non-M2
voters preserve the cut contract. M3 promotion intentionally requires the
target's fresh floor capability, so an old target needs upgrading before
promotion. M3's bounded deferral affects builds only under insufficient or
unknown storage. Its default wait is ten minutes with ten-second rechecks;
after expiry the original fatal-error semantics remain visible.

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

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M0 instrumentation | [#427](http://forge.lan:3000/noirr/plurx/pulls/427) | Four fixed-label filesystem gauges are sampled by the existing passive local-Raft tick without Store or network access; direct node status and the Cluster page carry the database byte count. Rust 1.97.1 compiled `plurx-core`, `plurxd`, and `plurx-cluster-check`; the focused filesystem, Prometheus, and web contracts passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M0 fleet readout | [#427](http://forge.lan:3000/noirr/plurx/pulls/427) | Blocked, not estimated: this execution host could not resolve the plan's `media1`/`lab1`–`lab3` aliases; SSH to the documented current addresses for media1, lab3, lab4, and lab6 timed out, and the configured `jump1` jump host could not reach their HTTP listeners. No voter was restarted. M1, M3, and therefore M2 remain unchanged until every current voter supplies the 24-hour B/E/S/W readout and an authorized follower restart supplies A. |
| 2026-10-07 | gpt-6.1-sol | 01a117e1-13a6-78a0-9b82-9a9adbe668c1 | Objective post-merge production-data observation | Evidence-only branch `codex/k02-production-snapshot-evidence-20261007`; independent root review pending | Under Paul's delegated architecture-management instruction, this session records retained R1 evidence collected by `agent:/root/remaining_requirements_audit_sol61`, not new fleet execution. Successful build counters advanced 4→5 on all four nodes at unchanged revision `42eb851ec48df72488d45b5d3d29bdb67d37d3d0`; immutable published snapshot metadata and 6,328-row aggregate counts are retained. The accepted one-observation residual is fulfilled; original authors, failed access/window receipts and separate tuning/restore limits remain unchanged. |

### Historical execution boundary — 2026-09-21

The implementation deliberately stops inside M0. The deployed fleet does not
yet run the new size gauges, and this session has no network path to collect
the allowed `du -b` fallback or the 24-hour histogram and index history. A
follower restart is also an operational mutation, not implied by permission to
perform read-only fleet inspection. Consequently there is no evidence-backed
`logs_until_snapshot` value, no measured storage floor, and no safe basis for
changing the writer/copy path.

Resume from a host with fleet access by first resolving the committed voter
set, then run the prompt in §5.1 against every voter over the same 24-hour
window. Perform one prepared follower restart only with explicit operational
authorization and record the applied-index catch-up rate. Put B, E, S, W, A,
the voter roster, timestamps, and raw query or command receipts on this PR
before beginning M1. This is an evidence dependency, not an invitation to use
the stale host list or the appendix's proposed threshold.

### 2026-09-30 correctness/default execution

The owner/Astra ruling above supersedes the historical stop for correctness
work only. The task retains N = 10,000, `max_in_snapshot_log_to_keep = 1`,
16 MiB log segments, and all transfer/install/publication contracts. M1
validates 1,000..=200,000 and threads the configured threshold into production
Hiqlite. M3 measures the live local image/WAL, waits before enqueueing the
writer, and publishes passive floor/deferral metrics. The existing heartbeat
transaction carries the versioned proof; no replicated table is added.

M2 persists cloned cut metadata, opens `BEGIN` and reads `_metadata` on the
dedicated read-only WAL connection while still on the writer, then restores
the live row's prior snapshot id. The blocking copy owns the pinned reader.
Its completion updates only the live snapshot id on success, never rolling
back later applied entries or membership. Blank Raft entries now acknowledge
their last-applied index through the same writer boundary. Copy errors retain
the old current generation and return the existing storage error.

Focused fork regressions prove 1,000 concurrent applies and a mid-copy
membership change cannot drift the image/metadata/returned cut; TRUNCATE is
busy during the pin and truncates after completion; real SQLite FULL and
builder-copy failures preserve live integrity and the current pointer; a
blank entry reaches the actual snapshot cut. The explicit ignored ENOSPC
regression requires an owned bounded filesystem and refuses a broad one.
It is not a silent pass when the lab environment is absent. Exact commands
and receipts belong to the task PR. No fleet 24-hour readout, B/E/S/W/A tuning,
production rollout, restart, or release acceptance is claimed here.

The isolated 2026-09-30 OS pressure receipt used a fresh 16 MiB HFS+ sparse
volume on the compile host, not a host-disk fill or a production filesystem.
Available bytes before the copy were 16,347,136; an owned synthetic 64 MiB
SQLite blob forced `database or disk is full`, and the operating system
returned errno 28. Live integrity remained `ok` and the read mark released.
The first lab assertion incorrectly expected ENOSPC only on write, while
the already-full volume refused file creation; the assertion was corrected
to accept errno 28 at either boundary and the regression reran successfully.
The volume was detached and synthetic leftovers removed. This is a local
failure-semantics receipt, not the deferred fleet tuning or release drill.

### 2026-10-07 production-data observation — accepted residual closed

The October 4 owner acceptance narrowed K-02's remaining bar to observing one
real snapshot on production data, not the declined undisturbed 24-hour readout
or a new follower-restart requirement. The retained R1 passive collection
fulfills that bar. This evidence-only continuation records it under Paul's
delegation; it does not rerun a collector, change a setting, deploy or restart
a node. The September access failures and interrupted/mixed-build windows
remain failed historical receipts, not retrospectively qualified intervals.

**Measured source:** all four running OCI/internal build identities agree on
`42eb851ec48df72488d45b5d3d29bdb67d37d3d0`, tree
`815503e354a9eeb04dd8ae7b24de5b2e0f6faa27`. Container identity, healthy state
and zero restart count are unchanged between the first and final selected
points. Three nodes are voters; `lab3` is the healthy nonvoting learner.
These are retained observations on that deployed revision, not a claim that
this docs candidate or the complete newer `main` binary ran on the fleet.

| Public alias | First→final UTC, 2026-10-07 | Build-ok count | Build-error count | Build-ok sum first→final (s) |
|---|---|---|---|---|
| media1 | 19:47:16→19:58:21 | 4→5 | 0→0 | 2.513197679→3.164870502 |
| lab6 | 19:47:18→19:58:39 | 4→5 | 0→0 | 4.371022808→5.243155817 |
| lab3 | 19:47:19→19:58:19 | 4→5 | 0→0 | 3.870248822→4.852100876 |
| lab4 | 19:47:18→19:58:21 | 4→5 | 0→0 | 3.428218964→4.746506528 |

One new completed build per node contributes respectively
0.651672823 · 0.872133009 · 0.981852054 · 1.318287564 seconds to the success
histogram sum. Those deltas are not p50/p99, daily rates or continuous coverage.

The retained inspector resolves the canonical `current` pointer, opens only
its published SQLite image with `mode=ro&immutable=1` and `query_only=ON`,
reads `_metadata` plus `SELECT count(*) FROM files`, then rechecks the
pointer and inode/size/mtime. Each accepted inspection has stable pointer and
file stat, 413 metadata bytes containing the current snapshot UUID, and 6,328
file rows. No database file, row value, title, path or credential is exported.

| Public alias | Inspection UTC, 2026-10-07 | Image bytes | Metadata SHA256 |
|---|---|---|---|
| media1 | 19:51:45.883646 | 319930368 | `f70c457f23d063036a7f398ba1294317792064aab9a1471956d22265cfa83b3f` |
| lab6 | 19:51:48.375332 | 314499072 | `06988f511eb1d33a45802beae19185013cf321ab6111943cc530b74b1774c22b` |
| lab3 | 19:51:53.314205 | 319930368 | `c20c24442ecdd0b4db09b17ea617b1a2a7f32a397358c3be539f305e0f886d46` |
| lab4 | 19:58:43.232242 | 319930368 | `a59418caaeedb15c3f34ec92f1a935bec6e7a625bb41bdc2cc687576ee4a1528` |

Media1/lab6 inspection captures the previously published generation; their
later build-count increase proves a subsequent successful build, not that
the inspected bytes are that later generation. Lab3's pointer changes before
inspection and is stable during it. The receipt does not decode the cut log
id or membership, restore an image, compute a full-database checksum, test a
crash, measure K-01 RPO/RTO or qualify an entire release tree.

**Current-source applicability:** the evidence writer and snapshot builder
are byte-identical from measured `42eb851e` through fresh docs base
`2362ff310c67b19bda4c1b039c7ae99d2e43ffef`. Their Git blobs are respectively
`5b35806bf36ca7ae00c2f1f8ee685e09d8cc85ba` and
`f81bfa899bed8635a48e2ffc2b670818c8203a30`. This supports the narrow unchanged
snapshot mechanism, not deployment of the newer GPU or other unrelated work.

**Raw retention and reproduction:** the private receipt directory is
`~/code/plurx-agent/architecture-final-evidence-20261007/r1-passive-fleet/`.
Its manifest SHA256 is
`5b0e1c30b458e898df727b41edbce5d627eb3ce8ac4beff6d17205eeeb5b224a`;
all 13 listed file hashes were reverified during this docs continuation.
The manifest retains the inspector and exact command descriptions,
acquisition brackets, selected metrics, pointer and metadata outputs.
`details-results.json` is explicitly truncated/incomplete and excluded
from acceptance. For the selected records, the binding hashes are:

| Private receipt basename | SHA256 |
|---|---|
| `sample-1-results.json` | `357f8f7830056cdafe79996259395d63b9e45ff0d08384d8333b9465e28e567f` |
| `final-selected-results.json` | `59b9bb9a76004b907ab3cc93b09a9ac9f848cc14f44a5acea3efcdddfcf71ac0` |
| `snapshot-metadata-results.json` | `c120aaf8688e7bfaf03a8c1b10888e5b28ac754aaf467bc9988947313705ef3d` |
| lab4 metadata-result receipt (private basename in manifest) | `b6367addd9e4f131092006879bc81ea3aca14f63a9df3a1f793d1e83924ada49` |
| `inspect-existing-snapshot.py` | `c3e8523db1bd6117aae30a229780cb1e2bf1c31841350c3ddd1b715938090c07` |

Reproduce the source comparison without contacting the fleet:

```bash
git diff --exit-code 42eb851ec48df72488d45b5d3d29bdb67d37d3d0 \
  2362ff310c67b19bda4c1b039c7ae99d2e43ffef -- \
  vendor/hiqlite/src/store/state_machine/sqlite/writer.rs \
  vendor/hiqlite/src/store/state_machine/sqlite/snapshot_builder.rs
```

Verify the private manifest/files, compare the exact named build count/sum
series under unchanged container/revision, and verify pointer-before/after,
file stat, metadata digest and aggregate count in the retained outputs.
That reproduces the evidence calculation; it does not authorize reacquiring
production data. N = 10,000 and retention 1 are not changed. B/E/S/W/A
measurement remains required before a future cadence tuning change; declined
duration and follower-restart prompts are not resurrected as this closure bar.
