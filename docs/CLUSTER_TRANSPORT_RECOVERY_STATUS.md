# Cluster transport recovery — live implementation status

**Status:** active · **Effort:** `effort/cluster-transport-recovery` ·
**Started:** 2026-09-05 · **Last updated:** 2026-09-05

Companion to [OPERATIONS.md](OPERATIONS.md) (operator contracts) and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (branch, gate, and
qualification rules) — this page answers *what is implemented, what has been
proved, and what remains*. Update it in the same commit that changes a
milestone's state. A green helper test is evidence for that helper, not for the
whole recovery contract.

## Outcome — bounded recovery without weakening authority

After transient transport backpressure, every Raft and cluster-API frame must
be written and flushed, and a connection whose writer fails must be torn down
without waiting for its reader. Snapshot retry keeps OpenRaft's typed mismatch,
vote, snapshot-ID, and offset semantics while one absolute attempt budget
bounds chunk transfer and final installation.

The completed work will expose process-local transport progress before the
public HTTP listener opens. It will not authorize reads, replay ambiguous
mutations, weaken readiness, change election timers, or introduce a wire-format
change.

## Delivery — one effort, reviewable milestone PRs

| Milestone | Task branch | PR | State | Blocking evidence |
|---|---|---|---|---|
| M1 · frame completion | `codex/cluster-transport-m1` | not opened | review fixes passing | Raw buffered-write failure reproduced; all production writer seams pass over real TLS; persistent gate wiring is present |
| M2 · connection and snapshot ownership | `codex/cluster-transport-m1` | same review | review fixes passing | Reader/writer terminal supervision, shutdown interruption, bounded node-owned snapshot execution, and partial-write retry tests pass |
| M3 · recovery budgets | planned | not opened | not started | Virtual-time exact bounds · config/env/Compose precedence |
| M4 · transport status | planned | not opened | not started | Authenticated pre-HTTP status · zero store calls · stale samples expire |
| M5 · recovery campaign | planned | not opened | not started | Actual TLS transport matrix · 20 learner and 20 voter cycles |
| Final promotion | `effort/cluster-transport-recovery` | not opened | not started | Full suite once after all review fixes · current-tree qualification receipt |

## Current evidence — Rust 1.97.1 is the compiler of record

The independent clone is based on remote `main` commit `3d847b58`. The local
Homebrew default is Rust 1.95.0, so every recorded Rust command uses
`rustup run 1.97.1`; unpinned results do not count.

| Check | Result | Detail |
|---|---|---|
| Vendored Hiqlite SQLite+cache compile | pass | `cargo check --locked` completed on Rust 1.97.1 |
| Buffered-write reproduction | pass · 1 negative test | Raw fastwebsockets `write_frame` leaves a small TLS response unavailable for 200 ms; an explicit flush delivers the same frame on the same socket |
| Production writer regressions | pass · 4 surfaces | Serialized Raft/API requests and responses use their production writer seams over real TLS, including a 3 MiB snapshot chunk and deterministic ciphertext-tail backpressure |
| Deadline and failure regressions | pass | Flush error, blocked combined write/flush budget, 250 ms Close budget, and scaled server-handshake budget all terminate at their documented boundary |
| Vendored network suite | pass · 37 tests | Prescribed SQLite+cache+auto-heal+macros `network::` library filter on Rust 1.97.1; no failures or ignored tests |
| API client stream suite | pass · 9 tests | Includes production request writer and malformed-frame reader outcome; no failures or ignored tests |
| Snapshot ownership | pass · 3 tests | One running/one queued admission, unfinished shutdown reporting, abandoned queued work, and partial-write completion before same-offset retry |
| Broad optional-feature probe | invalid baseline lane | `--all-features` enables mutually exclusive `cast_ints` modes and reaches unrelated existing optional-feature compile defects; the repository-prescribed feature matrices remain authoritative |
| Daemon check and denied-warning Clippy | pass | `plurxd --all-targets` completed on Rust 1.97.1 with no warnings |
| Vendored denied-warning Clippy | pass | Prescribed auto-heal+cache+macros+SQLite library lane passes with `-D warnings`; seven pre-existing Rust 1.97.1 lint findings were corrected rather than suppressed |
| Dependency resolution | pass | Standalone vendor lock now matches the daemon transport stack: Tokio 1.53.1 and rustls 0.23.42 |
| Persistent regression map | pass | The exact transport tests are in `cluster-wal-check` and the operations contract asserts each command appears once with the prescribed feature lane |
| Focused cluster/WAL fast lane | pass | `make cluster-wal-check` completed after all review fixes; loopback HTTP tests required the normal unsandboxed test allowance |
| Full repository suite | deferred | Run once on the final fixed promotion candidate, as requested |

## Decisions to review — autonomous choices and external blockers

1. **Work only in an independent clone.** The implementation lives at
   `/private/tmp/plurx-cluster-recovery-agent`. A mistakenly created worktree
   and its two branches in the shared repository were removed after a
   byte-for-byte patch comparison; no pre-existing shared changes were reset
   or deleted.
2. **Use runtime activation, never compile-time feature gating.** The finished
   capability will have an enablement section in Settings → Dev that explains
   prerequisites and refuses unsafe activation. Validation-only fault
   injection remains test-only because exposing a production fault switch
   would itself be unsafe; production transport behavior is always compiled.
3. **Keep protocol compatibility.** New budgets and observations stay local;
   serialized Raft and binary API variants do not change during the rolling
   update.
4. **Fold the first review's M2 findings into the M1 task PR.** Writer failure
   is only safe when the connection owns both split tasks and accepted
   snapshot work has a longer-lived owner. Keeping these coupled changes in
   one review prevents the flush fix from shipping with known teardown races.
5. **Do not merge without a real reviewed PR.** `gh auth status` reported the
   configured GitHub credential as invalid on 2026-09-05. The authenticated
   Forgejo Git remote can fetch and push. PR creation/merge remains blocked
   until an authenticated review API is available; implementation and local
   evidence continue meanwhile.

## Next checkpoint — second review of the corrected transport lifecycle

The first three adversarial reviews found lifecycle, test-seam, persistent-gate,
dependency-resolution, and strict-lint gaps. Their fixes now pass the focused
compiler loop. The next checkpoint commits and pushes the corrected revision,
then reviews that exact SHA again before opening the milestone PR.

**How to read this page:** “pass” means the named command completed against the
named tree. “In progress” does not mean shippable. The effort is complete only
after every milestone is merged into the effort branch, the current `main` is
integrated, the full suite passes once on that fixed tree, and the promotion
receipt exists.
