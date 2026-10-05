# Vendored Hiqlite WAL 0.14.0

This directory is the crates.io `hiqlite-wal` 0.14.0 package, licensed under
Apache-2.0. Plurx carries four patches for replicated SQLite: three
restart-recovery repairs and one K-06 startup-ownership policy:

**Owner:** Paul Junod (repository owner). Rows 1-3 are generic bugs; row 4
is a Plurx policy.
As of 2026-09-30, row 2 has an accepted upstream mechanism; the two
`pending M6` rows still need upstream coordination and a real public URL.

| # | Patch | Kind | Upstream | Drop condition |
|---:|---|---|---|---|
| 1 | Reconstruct missing purge boundary | generic bug | pending M6 | Upstream release derives the boundary from a retained entry above the initial range. |
| 2 | Atomic `meta.hql` replacement | generic bug | https://github.com/sebadob/hiqlite/pull/357 | Upstream release syncs and atomically renames same-directory metadata updates. |
| 3 | WAL incarnation and layout guard | generic bug | pending M6 | Upstream release rejects stale memo/mmap reuse and serializes path reuse with readers. |
| 4 | K-06 staged WAL construction (`start_staged`, `PartialStartupWriter`) | plurx policy | — | Never; Plurx's cancellable clock-observation start requires a failed constructor to drain its writer, not abandon it. |

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

**Unledgered fork changes, 2026-10-04:** `src/status.rs` (bounded WAL runtime
status), `src/inspection.rs` (described below) and the `src/lib.rs` exports
for both are Plurx code no row above owns yet. `PLURX-FILES.toml` lists those
three under `unledgered` so the gap stays visible and cannot grow; they need a
row before this ledger is complete. The status surface is wider than those
files. Its plumbing also lives in two files the manifest classifies as
`patched` because rows 1, 3 and 4 name them, though no row describes this
change: `src/writer.rs` builds the `WalStatusHandle` in `spawn` (about lines
112-182: the sync-mode label, WAL size and integrity flag it reports, the
clone handed to the writer thread, `record_error` on a failed writer, and the
handle returned beside the sender), threads it into `run` (about line 238),
and records state, sync, compaction, error and stop transitions through the
writer loop (from about line 304); its two `status_*` tests are there too.
`src/log_store.rs` carries the handle on `LogStore` and exposes it as
`status_handle()`. And `Cargo.toml` adds tokio's `macros` feature beside the
upstream `fs`, `sync` and `rt-multi-thread` in the normal (not dev)
dependency, which only the `#[tokio::test]` status test in `src/writer.rs`
uses. A row for the status surface must name all of these.

The additive `inspection` module is a read-only stopped-node diagnostic
surface. It refuses a live lock and exposes metadata, WAL, and decoded log-id
boundaries without returning application payloads. `plurx-cluster-check
inspect-wal` adds immutable SQLite snapshot/applied boundaries and file hashes
to its versioned JSON artifact.

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
`plurx policy` row (row 4) no longer needs a patch: its drop condition is
`Never` by design, so it leaves only when upstream offers a way to express
staged writer ownership without patching this source, or when the owner
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
