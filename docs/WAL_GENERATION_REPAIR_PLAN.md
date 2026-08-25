# WAL generation repair plan — make snapshot compaction invalidate every stale reader cache

**Status:** adversarial review reconciled · **Repairs:** the 2026-08-25
four-voter outage and the incomplete stale-memo guard from PR #560 ·
**Review:** [WAL_GENERATION_REPAIR_PLAN_REVIEW.md](WAL_GENERATION_REPAIR_PLAN_REVIEW.md)
· **Written:** 2026-08-25 · **Code:** `origin/main` at `a0eb5216`

Companion to [OPERATIONS.md](OPERATIONS.md) (how to preserve and inspect a
failed voter) and [CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md)
(why three voters are the ordinary HA topology) — this plan repairs the WAL
reader's process-local generation confusion and gives large snapshot transfers
an explicit deadline. Execute §4 in order. If a step appears to require editing
Raft metadata, deleting the preserved `nuc4` evidence, weakening WAL integrity
checks, or changing live membership as part of the code change, stop and return
that action for operator approval.

## 1. Objective — disk truth and reader truth must name the same WAL incarnation

The work is complete only when all four outcomes below are true.

| Outcome | Required result |
|---|---|
| Generation safety | Recreating `0000000000000001.wal` cannot preserve an mmap or read memo from the deleted file. |
| Read failure truth | A WAL read failure reaches OpenRaft as a storage error; it is never converted into a successful short range. |
| Snapshot runway | Production voters default to a 120-second install deadline, with a bounded operator override expressed in seconds. |
| Regression evidence | A focused test holds the old mmap and memo across repeated full-purge/recreate cycles and reads the first entry from each new generation. |

The physical WAL captured from `nuc4` inspected clean. The running process still
reported a missing first entry at `1,530,001`, then at later snapshot boundaries
`1,590,001` and `1,600,001`. That combination points at stale process memory,
not missing bytes on disk.

## 2. Root cause — `wal_no` is a reusable name, not a file identity

The current reader retains two optimizations across requests:

- `reader.rs` keeps one `LogReadMemo` for the reader thread's lifetime; and
- each cloned `WalFile` may retain a read-only mmap.

`WalFileSet::clone_files_from_no_mmap` refreshes the reader by retaining every
local file whose `wal_no` exists in the writer's set. It then copies only
`id_from`, `id_until`, `data_start`, and `data_end`. A full purge can empty the
writer's set and make `add_file` start again at WAL number `1`. The reader sees
the same number, retains the old object and mmap, and overlays the new logical
boundaries on bytes mapped from the deleted inode.

PR #560 added this memo guard:

```rust
memo.last_wal_no == self.wal_no
    && memo.last_log_id >= self.id_from
    && memo.last_log_id < id_from
```

That rejects the captured stale memo when the new file starts above the old
log ID. It does not identify the mapped file, and its regression constructs
only a new standalone `WalFile`; it never carries a live mmap through
`WalFileSet::clone_files_from_no_mmap`. The patch therefore fixed one cache and
left the second ABA match intact.

```text
writer: WAL 1 / generation A ── full purge ──▶ WAL 1 / generation B
                │                                  │
reader:    mmap generation A ── match `wal_no` ────┘
                │
                └── new B boundaries + old A bytes ──▶ false missing log
```

### 2.1 This repair names file incarnation, not every concurrent layout mutation

Truncating a suffix and appending differently sized conflicting records can
also invalidate offsets while keeping the same inode. A generation change
published after that mutation would invalidate the next reader action, but it
would not serialize a read already in progress. Solving that broader race needs
an action-level shared/exclusive layout guard or copy-on-write replacement.

This PR stays tied to the reproduced incident: a full purge unlinks the mapped
inode and creates a different file under the same WAL number. `incarnation`
therefore means one `WalFile::new` or `WalFile::read_from_file` object lineage.
The existing retained-range guard continues to cover an in-place front purge.
General read-versus-suffix-rewrite serialization is an explicit non-goal in
§7, not a safety property claimed without coordination.

### 2.2 The current error path hides the storage failure

For one `Action::Logs` branch, `reader.rs` logs `read_logs` errors and then sends
the range terminator. `try_get_log_entries` consequently returns a shorter
successful vector, and OpenRaft later reports `LogIndex(...): got None`. The
other branch unwraps the same error and can terminate the reader thread.
`read_logs` also checks its requested result count only with `debug_assert_eq!`,
so a release build can accept a plausible but incomplete range.

## 3. Contract — incarnation is process-local and correctness always beats reuse

### 3.1 `WalFile` carries a non-persistent, non-wrapping incarnation

Add a monotonically allocated `u64` incarnation to `WalFile`.

```rust
pub struct WalFile {
    incarnation: u64,
    pub wal_no: u64,
    // existing fields
}
```

`WalFile::new` and `WalFile::read_from_file` allocate a new value from a
relaxed atomic with checked, non-wrapping increment. Exhaustion is a process
failure rather than a collision; a reused identity would make cache safety
ambiguous.
`clone_no_mmap` copies it. `clone_from_no_mmap` requires the same value because
that path publishes append-only header growth for one existing file.

A newly created replacement automatically receives a new incarnation.
Incarnations do not enter the 32-byte WAL header: process-local readers are the
only consumers, and every restart constructs a fresh writer/reader set before
any memo exists.

### 3.2 Reader refresh matches the incarnation, not only the number

Replace the current retain/enumerate/skip algorithm rather than adding another
predicate to it. Reconstruct the refreshed deque in `other`'s authoritative
order. For each writer file, reuse exactly one old object only when both
`wal_no` and `incarnation` match; otherwise clone the writer file without an
mmap. Copy `active` only after reconstruction. This prevents a retained suffix
from being paired positionally with a replaced front file in release builds.

An incarnation mismatch drops the whole old `WalFile`, which unmaps the
deleted inode. Ordinary append-only refreshes keep the mmap and copy the
expanded boundary fields as they do now.

`LogReadMemo` records the incarnation that produced its byte offset. A memo is
eligible only when all of these hold:

| Check | Failure prevented |
|---|---|
| Same incarnation and WAL number | Recreated-path and rewritten-layout ABA. |
| Remembered log is in the retained range and below the request | Front-purge and non-forward lookup reuse. |
| Remembered byte end is at or above `data_start` and below `data_end` | Reusing an offset outside the current readable interval. |

A failed check is a cache miss. Reading restarts at the authoritative
`data_start`; cache metadata must never be required to retrieve a log.

### 3.3 Every contradictory WAL read is one explicit storage result

Replace the bounded `Option<Result<...>>` stream with a one-shot
`Result<Vec<Vec<u8>>, Error>`. OpenRaft requests at most a bounded log batch,
and the reader already buffers a file range before publication. One result
eliminates the invalid `Err`-then-terminator sequence: the current consumer
returns and drops its receiver as soon as it sees `Err`, so a second send would
panic or block the producer. A cancelled response send is not unwrapped.

Within one `WalFile`, metadata has already asserted that the requested subrange
exists. `read_logs` replaces its release-only count assertion with an
`Error::Integrity` only when scanning that claimed subrange produces a gap,
wrong ID, CRC failure, or insufficient count. Absence outside every retained
file remains a possibly short result for OpenRaft's stricter wrapper to
interpret; it is not relabeled as disk corruption.

Offset arithmetic uses checked additions and mmap reads use checked slice
access. A malformed length or out-of-bounds memo returns `Error::Integrity`
rather than panicking. A memo is published only after the complete requested
subrange passes validation.

`Action::LogState` likewise returns its existing `Result` error rather than
panicking if the last retained log cannot be mapped or decoded.

### 3.4 Snapshot install timeout is a Plurx production setting

Add `cluster.install_snapshot_timeout_secs` with these rules:

| Property | Value |
|---|---:|
| Default | 120 seconds |
| Accepted range | 10–3,600 seconds |
| Environment override | `PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS` |
| Hiqlite mapping | checked seconds-to-milliseconds conversion into `RaftConfig::install_snapshot_timeout` |

The 72 MiB incident snapshot repeatedly transferred about 50 MiB before the
old 10-second deadline and completed with 120 seconds. The knob belongs in
Plurx's `production_hiqlite_defaults`; changing Hiqlite's global default would
also affect isolated vendor tests and non-production consumers without making
the operator contract visible. Hiqlite leaves `send_snapshot_timeout = 0`, so
OpenRaft applies this install deadline to non-final snapshot transfer work too.

## 4. Milestones — one correctness patch, then its deployment evidence

### 4.1 Reproduce the mmap ABA before changing production code

Add a vendored-WAL test that:

1. appends records to WAL 1;
2. clones a reader, maps WAL 1, and establishes a memo;
3. fully purges the writer and recreates WAL 1 at a high index;
4. refreshes the reader through `clone_files_from_no_mmap`;
5. reads the first new record; and
6. repeats the purge/recreate cycle at another high index.

The pre-fix test must fail with the same zero-length or wrong-generation read
shape seen in production. The repeated cycle prevents a one-time special case
for `wal_no == 1` from passing.

**Acceptance:** the regression fails when only the incarnation-aware refresh is
reverted.

### 4.2 Implement incarnation-aware file and memo reuse

Add the allocator and field and update clone/refresh behavior. Rebuild in the
writer's order; do not retain and then pair by deque position. Keep WAL version
1 and its 32-byte header unchanged so deployed data remains byte-compatible in
both directions.

Extend the existing PR #560 memo regression to name the incarnation condition,
then retain it beside the end-to-end mmap regression. Add refresh tests for a
replaced front beside an unchanged suffix and for a full reset followed by
multiple rollovers before refresh. One test proves the predicate; the others
prove ordering and the reader/writer handoff.

**Acceptance:**

```bash
cargo test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml
```

### 4.3 Make storage failures observable to OpenRaft

Return range and log-state errors through one-shot result channels and turn a
short read inside a WAL's claimed subrange into an integrity error in release
builds. Add reader-thread tests that inject an invalid claimed range and assert
the receiver obtains `Err`, then successfully processes a second action. Keep a
separate test proving a request outside the retained set may remain absent.

Harden record-header/data arithmetic and mmap slice access along the same error
path. No `mmap`, `read_logs`, buffer-index, or response-send failure in the
reader action loop may panic the reader thread.

**Acceptance:** focused reader tests pass and no `read_logs(...).unwrap()`
remains in the reader action loop.

### 4.4 Add the bounded production snapshot deadline

Add the config field, validation, environment override, example TOML, and
operator reference. Update the existing production-defaults regression to
assert `120_000` milliseconds and a custom value. Test boundary values 9, 10,
3,600, and 3,601 seconds plus the environment-value parser. Older binaries
tolerate the new cluster key by design, so a rolling rollback continues to
parse the file.

**Acceptance:**

```bash
cargo test --locked -p plurx-core config --lib
cargo test --locked -p plurx-core --features hiqlite-store \
  cluster::migration::tests::read_pool_tuning_leaves_raft_and_wal_safety_defaults_unchanged \
  --lib -- --exact
```

### 4.5 Run the repository's governed storage gates

Update `vendor/hiqlite-wal/PLURX-PATCH.md` and `docs/OPERATIONS.md` in the same
commit as the behavior. Extend `make cluster-check` so the new end-to-end WAL
generation regression is named explicitly, then update the corrective-history
fragment after the final commit SHA is known.

**Acceptance:**

```bash
cargo fmt --all --check
cargo clippy --locked --manifest-path vendor/hiqlite-wal/Cargo.toml --all-targets -- -D warnings
make cluster-check
make validate-staged
git diff --check
```

## 5. Adversarial review gates — challenge the design and the merged diff

Before implementation, a separate agent must inspect this plan and the pinned
source. It must try to disprove the mmap diagnosis, identify every layout
mutation that needs a new incarnation, check whether the reader refresh can
misalign file indices, and challenge the timeout surface and test strategy.
P0/P1 findings amend this plan before code starts.

After the PR is open, a fresh adversarial review must inspect the PR diff and
test results rather than relying on this plan. It must focus on concurrency,
cache invalidation, overflow and boundary arithmetic, cross-platform mmap/file
replacement behavior, rollback compatibility, error-channel deadlocks, and
whether the tests fail under targeted reverts. Every actionable finding is
fixed or answered with code evidence before merge.

## 6. Deployment — code merge does not mutate the live cluster

This PR changes source and tests only. It does not start `nuc4`, edit membership,
or deploy containers.

The current four-voter membership has only three running voters, which is its
entire quorum. A rolling restart is unsafe until an operator either removes the
offline voter through the supported membership API or schedules a maintenance
window. After that decision, deploy one non-leader at a time, require readiness
and zero apply lag between voters, then repair or rejoin the preserved node from
a healthy quorum. Never edit `meta.hql` or synthesize WAL entries by hand.

## 7. Non-goals — keep the repair tied to the observed failure

- **Do not change the WAL disk format.** No persistent generation is needed to
  invalidate process-local caches, and a format migration adds rollback risk.
- **Do not disable mmap or memo reuse globally.** Full reconstruction on every
  read is a safe emergency fallback, but it is unnecessary once invalidation
  follows layout generations.
- **Do not claim general read/truncate serialization.** In-place conflicting
  suffix rewrites need an action-level coordination or copy-on-write design.
  This PR repairs the observed unlinked-inode recreation ABA and leaves that
  broader concurrency contract for a separately reproduced change.
- **Do not call the clean forensic copy corrupt.** The diagnosis depends on the
  disk/process disagreement; erasing that distinction would send recovery in
  the wrong direction.
- **Do not change election, heartbeat, WAL sync, or snapshot-frequency values.**
  Only the measured snapshot RPC deadline changes.
- **Do not automate voter removal or deployment.** Membership and rollout are
  operational decisions outside this source PR.
