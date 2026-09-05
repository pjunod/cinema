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
| M1 · frame completion | `codex/cluster-transport-m1` | not opened | review pending | Shared write+flush helper and every writer migration compile; 24 focused network tests and the daemon denied-warning lane pass; adversarial review remains |
| M2 · connection and snapshot ownership | planned | not opened | not started | Partial-write retry safety · bounded admission · task/socket quiescence |
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
| Frame helper regression | pass · 5 tests | Real TLS covers Raft/API request and response directions, 32-byte and 3 MiB frames, no-backpressure control, flush failure, and blocked-flush expiry |
| Vendored network suite | pass · 24 tests | SQLite+cache `network::` library filter on Rust 1.97.1; no failures or ignored tests |
| Broad optional-feature probe | invalid baseline lane | `--all-features` enables mutually exclusive `cast_ints` modes and reaches unrelated existing optional-feature compile defects; the repository-prescribed feature matrices remain authoritative |
| Daemon check and denied-warning Clippy | pass | `plurxd --all-targets` completed on Rust 1.97.1 with no warnings |
| Vendored denied-warning Clippy | pass with baseline lint exceptions | SQLite+cache library completed; three pre-existing lint classes outside this patch remain explicitly allowed in this diagnostic lane |
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
4. **Do not merge without a real reviewed PR.** `gh auth status` reported the
   configured GitHub credential as invalid on 2026-09-05. The authenticated
   Forgejo Git remote can fetch and push. PR creation/merge remains blocked
   until an authenticated review API is available; implementation and local
   evidence continue meanwhile.

## Next checkpoint — finish M1 before widening the surface

M1 next receives adversarial agent review. Findings are fixed and the focused
suite reruns before the milestone PR is opened.

**How to read this page:** “pass” means the named command completed against the
named tree. “In progress” does not mean shippable. The effort is complete only
after every milestone is merged into the effort branch, the current `main` is
integrated, the full suite passes once on that fixed tree, and the promotion
receipt exists.
