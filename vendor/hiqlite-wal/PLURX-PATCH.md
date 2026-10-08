# Vendored Hiqlite WAL 0.14.0

This directory is the crates.io `hiqlite-wal` 0.14.0 package, licensed under
Apache-2.0. Plurx carries six patches for replicated SQLite: three
restart-recovery repairs and three Plurx policies:

**Owner:** Paul Junod (repository owner). Rows 1-3 are generic bugs; rows 4-6
are Plurx policies.
As of 2026-09-30, row 2 has an accepted upstream mechanism; the two
`pending M6` rows still need upstream coordination and a real public URL.

| # | Patch | Kind | Upstream | Drop condition |
|---:|---|---|---|---|
| 1 | Reconstruct missing purge boundary | generic bug | pending M6 | Upstream release derives the boundary from a retained entry above the initial range. |
| 2 | Atomic `meta.hql` replacement | generic bug | https://github.com/sebadob/hiqlite/pull/357 | Upstream release syncs and atomically renames same-directory metadata updates. |
| 3 | WAL incarnation and layout guard | generic bug | pending M6 | Upstream release rejects stale memo/mmap reuse and serializes path reuse with readers. |
| 4 | K-06 staged WAL construction (`start_staged`, `PartialStartupWriter`) | plurx policy | — | Never; Plurx's cancellable clock-observation start requires a failed constructor to drain its writer, not abandon it. |
| 5 | Bounded writer-owned WAL runtime status | plurx policy | — | Never; Plurx owns this observational runtime status and its writer lifecycle attribution. |
| 6 | Read-only stopped-node WAL inspection | plurx policy | — | Never; Plurx requires stopped-node boundary diagnostics without returning application payloads or reopening a writer. |

- Missing `last_purged_log_id` metadata is reconstructed whenever the first
  retained WAL entry is above the initial log range. Snapshot installation can
  leave entries `10000–19999` in WAL file 1, so the retained entry—not a WAL
  rollover—is the evidence that earlier logs were purged. Without this patch,
  OpenRaft requests log 0, the WAL refuses a read below 10000, and the voter
  panics before opening its cluster listener.
  Files: `src/writer.rs`.
- Metadata updates are written and synced to a same-directory staging file,
  then atomically renamed over `meta.hql`. The upstream remove-then-create
  sequence exposes an empty file if the process exits between those operations;
  the next start then refuses `invalid metadata file length` before it can read
  the intact WAL.
  Files: `src/metadata.rs`.
- Every process-local `WalFile` incarnation has a non-persistent identity, and
  reader refresh reconstructs the file deque in writer order. A full purge
  deletes the old WAL and creates a new file numbered 1; matching only that
  number used to retain both a `LogReadMemo` offset and an mmap of the deleted
  inode, then overlay the new file's boundaries on the old bytes. Memo reuse is
  now incarnation- and bounds-checked, suffix rewrites renew the incarnation,
  and a shared/exclusive layout guard spans reader mmap/read and writer
  remove/truncate. Recreated files drop the stale mmap; exact record IDs and
  counts are checked; and contradictory ranges reach OpenRaft through a
  capacity-one record/terminal stream instead of becoming successful short
  reads or an unbounded raw batch.
  Files: `src/wal.rs`, `src/reader.rs`, `src/log_store_impl.rs`,
  `src/writer.rs`.
- `LogStore::start_staged` in `src/log_store.rs` constructs the WAL exactly as
  `start` does, but a `PartialStartupWriter` (`src/writer.rs`) holds the
  writer's sender inside the blocking constructor. If construction fails after
  the writer thread exists, including the interval syncer or reader spawn,
  dropping the guard sends `Action::Shutdown` and waits for its
  acknowledgement, so the lock-holding writer is drained rather than abandoned
  with a live syncer sender; success hands the guard off. Ordinary `start`
  passes `staged = false` and is unchanged. Hiqlite's
  `startup_cleanup::start_wal` uses it only for K-06 clock-observation startup
  (row 23 of `vendor/hiqlite/PLURX-PATCH.md`,
  `docs/cluster/CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md`), whose
  `k06_cancelled_wal_constructor_retains_real_writer_until_drained` and
  `k06_staged_wal_timer_panic_drains_actual_writer_before_error` tests pin it.

- **Row 5 — WAL runtime status:** `src/status.rs` defines a cloneable
  `WalStatusHandle` over the writer's existing WAL/metadata state. Snapshots
  report lifecycle state, lock ownership, unclean startup, sync policy,
  segment allocation, retained/log/purge and writer-reported sync boundaries,
  transition times and bounded errors. They do not reopen or walk WAL files,
  return application payloads or certify fsync durability. `src/writer.rs`
  constructs and returns the handle beside its sender, carries its clone into
  the writer loop, and records sync, compaction, error and clean-stop
  transitions. `src/log_store.rs` retains it and exposes `status_handle()`;
  `src/lib.rs` exports the status types. `Cargo.toml` already enables Tokio
  `macros` beside `fs`, `sync` and `rt-multi-thread` in the normal dependency
  for the `#[tokio::test]` status regression in `src/writer.rs`. This row
  records that existing placement; it does not move or add a feature edge.
  Files: `src/status.rs`, `src/writer.rs`, `src/log_store.rs`, `src/lib.rs`,
  `Cargo.toml`.
- **Row 6 — Stopped-node inspection:** `src/inspection.rs` exposes metadata,
  physical WAL and decoded Raft log-id boundaries through a read-only
  diagnostic API exported by `src/lib.rs`. It refuses a live advisory lock,
  non-regular WAL files and oversized WAL files, checks record CRCs and
  contiguity, and reports boundary inconsistencies without returning
  application payloads. It neither starts a writer nor publishes metadata.
  The `plurx-cluster-check inspect-wal` caller adds immutable SQLite
  snapshot/applied boundaries and file hashes to its versioned artifact.
  Files: `src/inspection.rs`, `src/lib.rs`.

**M1 ledger continuation, 2026-10-08:** rows 5 and 6 own the status and
inspection surfaces and their existing plumbing previously disclosed as
unledgered on October 4. `PLURX-FILES.toml` now classifies all three defining
or export sources as `patched`; the `unledgered` table is empty. This is
metadata reconciliation, not a new runtime acceptance result, upstream
disposition or permission to remove a patch.

**Upstream receipt, 2026-09-30:** row 2's PR was accepted at merge
[`5e93f594bb955449616bcf8f4f6128998944ec42`](https://github.com/sebadob/hiqlite/commit/5e93f594bb955449616bcf8f4f6128998944ec42).
The [v0.15.0 metadata source](https://github.com/sebadob/hiqlite/blob/v0.15.0/hiqlite-wal/src/metadata.rs)
stages same-directory bytes, calls `sync_data()`, and publishes with one
rename, closing the remove/create gap on POSIX. Its Unix directory sync is
best effort. The local 0.14 repair uses file `sync_all()` and requires Linux
directory `sync_all()` success; acceptance of the shared mechanism does not
prove full patch or durability equivalence. No upgrade or patch removal is
recorded here; the exit below still applies.

**Partial upstream receipt, 2026-10-03:** [upstream PR 367](https://github.com/sebadob/hiqlite/pull/367)
merged September 18 as
[`52122ae7163d051d6b488d751018f76596f7d8f7`](https://github.com/sebadob/hiqlite/commit/52122ae7163d051d6b488d751018f76596f7d8f7).
Its memo records `id_from` and rejects reuse when the starting range changes;
`read_logs_memo_survives_wal_no_reuse` covers different directories/ranges.
That accepted portion of row 3 is not the full incarnation/layout repair:
[observed immutable reader refresh](https://github.com/sebadob/hiqlite/blob/e0a6a8e9bdde7afb97156eef13a6e93574324feb/hiqlite-wal/src/wal.rs#L934)
still retains objects by WAL number while replacing boundaries. Same-path
stale mmap, suffix-rewrite identity and reader/path-reuse serialization need
their own exact disposition. Row 3 stays `pending M6`; no new upstream
reproduction, full equivalence, upgrade or patch removal is claimed.

Remove this vendor when both halves of its exit hold. First, the rows an
upstream release can retire (rows 1, 2 and 3: the `generic bug` kind) have
met their drop conditions in a release Plurx has upgraded to. Second, the
`plurx policy` rows (rows 4, 5 and 6) no longer need a patch: their drop
conditions are `Never` by design, so a row leaves only when upstream offers
a way to express its policy without patching this source, or when the owner
records that Plurx no longer needs it as a change to that row. Until then,
`single_file_snapshot_tail_restores_its_missing_purge_boundary`,
`interrupted_metadata_replacement_keeps_the_previous_record_readable`, and
`full_purge_replaces_stale_mmap_and_memo_across_reused_wal_numbers` keep the
production startup conditions load-bearing. The barrier-driven
`full_purge_waits_for_an_unmapped_reader_before_reusing_the_wal_path` test pins
the layout serialization boundary on Plurx's supported POSIX production
targets, where unlinking a mapped inode is permitted.

Cargo records this package as path-sourced, which means cargo-audit skips it.
The weekly `rust-audit.yml` job uses `scripts/vendor-audit-lock` to restore this
exact release's registry source and checksum before a second advisory scan.
That check must remain until this directory is removed.

Source: <https://crates.io/crates/hiqlite-wal/0.14.0>

The retained lockfile, README, and unit tests are upstream provenance. The
workspace excludes this directory deliberately; its focused regression runs
through its own manifest.
