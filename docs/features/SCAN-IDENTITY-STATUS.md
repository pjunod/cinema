# Scan identity status — build, review and promotion ledger

**Status:** M0 in progress · **Executes:**
[SCAN-IDENTITY-IMPLEMENTATION.md](SCAN-IDENTITY-IMPLEMENTATION.md) ·
**Started:** 2026-09-19 · **Updated:** 2026-09-19

Companion to the
[root-cause analysis](SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md) and
[implementation contract](SCAN-IDENTITY-IMPLEMENTATION.md) — this page records
what exists, what has been proved, and what can safely happen next. It does
not authorize a production catalogue repair.

## Current position — isolated base and compiler are established

| Field | Value |
|---|---|
| Forgejo repository | `noirr/plurx` |
| Base | `main` at `535f95d228b1393033b09d497964079e4740e8ad` |
| Effort branch | `effort/scan-identity` |
| Implementation checkout | Isolated clone; the authoring checkout remains untouched |
| Required compiler | Rust 1.97.1 |
| Production repair | Not authorized; implementation and fixture verification only |

The host's Homebrew `rustc` is 1.98.0, so every Rust command in this effort
uses `rustup run 1.97.1 ...`. That explicit prefix prevents evidence from
silently describing the wrong toolchain.

## Milestones — one integration line, three reviewed task branches

| Milestone | Branch | State | Evidence |
|---|---|---|---|
| M0 base, compiler and failure fixtures | `effort/scan-identity` | in progress | Pinned compiler and clean-base compile recorded below. |
| M1 ownership and directory lookup | `codex/scan-identity-ownership` | not started | Scanner and backend contracts required. |
| M2 guarded series hints | `codex/scan-identity-hints` | not started | Atomic SQLite/Hiqlite outcomes and queued-import behavior required. |
| M3 bounded repair | `codex/scan-identity-repair` | not started | Preview, reference inventory, atomic apply and retry behavior required. |
| Final adversarial review | `effort/scan-identity` into `main` | not started | One review after the complete candidate is frozen. |
| Main fast lane and merge | final effort PR | not started | Exact-tree green lane and receipt required before merge. |
| Production catalogue repair | separate operator action | not authorized | Fresh deployed preview and explicit authorization required. |

## Evidence — commands identify the exact tree they describe

| Date | SHA | Command | Result |
|---|---|---|---|
| 2026-09-19 | `535f95d228b1393033b09d497964079e4740e8ad` | `rustup run 1.97.1 rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| 2026-09-19 | `535f95d228b1393033b09d497964079e4740e8ad` | `rustup run 1.97.1 cargo check --locked -p plurx-core -p plurxd --all-targets` | passed in 1m 25s |

No unit or integration suite has run yet. The repository contributor policy
requires one smallest focused regression for each task PR; broader affected
tests remain deferred to the frozen final candidate.

## Decisions and limits — record assumptions instead of hiding them

1. **Repository policy wins on per-task regression evidence.** The build
   request asked for tests only at the final PR, while `AGENTS.md` requires a
   focused regression before each task PR. Each task will run only that
   smallest required test; the broad fast lane remains a one-time final cost.
2. **No feature gate will be added.** The maintenance API remains admin-only,
   preview-first and advisory about prerequisites. Production apply still
   needs an operator-reviewed preview because it is destructive catalogue
   maintenance, not because a code flag blocks the feature.
3. **No production data changes are in scope.** Fixtures may exercise apply;
   no live preview or apply happens during this build.
4. **Flat and renamed directories keep the documented fallback limits.** This
   effort does not introduce a persistent identity schema or broad merger.

## Next action

Commit the M0 documents and failure fixtures, capture the old behavior, then
start M1 from the effort branch. Update this page in the same commit whenever
a milestone, review, test result or PR state changes.
