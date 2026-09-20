# Scan identity status — build, review and promotion ledger

**Status:** complete candidate frozen for final review · **Executes:**
[SCAN-IDENTITY-IMPLEMENTATION.md](SCAN-IDENTITY-IMPLEMENTATION.md) ·
**Started:** 2026-09-19 · **Updated:** 2026-09-19

Companion to the
[root-cause analysis](SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md) and
[implementation contract](SCAN-IDENTITY-IMPLEMENTATION.md) — this page records
what exists, what has been proved, and what can safely happen next. It does
not authorize a production catalogue repair.

## Current position — prevention and bounded repair are qualified

| Field | Value |
|---|---|
| Forgejo repository | `noirr/plurx` |
| Base | current `main` at `15324dd9`, merged into the effort at `b0baa97c` |
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
| M0 base, compiler and failure fixtures | `effort/scan-identity` | complete | Pinned compiler, clean-base compile and three failing/one passing pre-fix probes recorded below. |
| M1 ownership and directory lookup | `codex/scan-identity-ownership` | merged by PR #379 at `2cbea369` | Core, daemon, SQLite and three-voter Hiqlite focused contracts passed. Current workflow intentionally allocates no CI jobs for task PRs into an effort. |
| M2 guarded series hints | `codex/scan-identity-hints` | merged by PR #380 at `54a65e93` | Atomic SQLite/Hiqlite outcomes, fenced stale-lease refusal and daemon import/report behavior passed. Current workflow intentionally allocates no CI jobs for task PRs into an effort. |
| M3 bounded repair | `codex/scan-identity-repair` | merged by PR #384 at `86cf04b4` | Deterministic planner, bounded admin cache/API, SQLite/Hiqlite fenced apply and retry recognition pass focused core, daemon, SQLite, three-voter and documentation contracts. |
| Final adversarial review | `effort/scan-identity` into `main` | not started | One review after the complete candidate is frozen. |
| Main fast lane and merge | final effort PR | not started | Exact-tree green lane and receipt required before merge. |
| Production catalogue repair | separate operator action | not authorized | Fresh deployed preview and explicit authorization required. |

## Evidence — commands identify the exact tree they describe

| Date | SHA | Command | Result |
|---|---|---|---|
| 2026-09-19 | `535f95d228b1393033b09d497964079e4740e8ad` | `rustup run 1.97.1 rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| 2026-09-19 | `535f95d228b1393033b09d497964079e4740e8ad` | `rustup run 1.97.1 cargo check --locked -p plurx-core -p plurxd --all-targets` | passed in 1m 25s |
| 2026-09-19 | `0e1a3bfb` against pre-fix implementation | `rustup run 1.97.1 cargo test --locked -p plurx-core --lib scan_identity_` | expected reproduction: 3 failed (renamed new season, renamed second version, changed-file watch preservation); 1 no-rename control passed |
| 2026-09-19 | `1ac7f00b` | `rustup run 1.97.1 cargo test --locked -p plurx-core --lib scan_identity_` | 11 passed; 839 filtered out |
| 2026-09-19 | `1ac7f00b` | `rustup run 1.97.1 cargo test --locked -p plurxd --bin plurxd scan_identity_` | 1 passed; 2,340 filtered out |
| 2026-09-19 | `1ac7f00b` | `rustup run 1.97.1 cargo test --locked -p plurx-core --test store_contract scan_identity_directory_contract` | SQLite: 1 passed; 116 filtered out |
| 2026-09-19 | `1ac7f00b` | `rustup run 1.97.1 cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract scan_identity_directory_contract -- --test-threads=1` | three-voter Hiqlite: 1 passed; 162 filtered out |
| 2026-09-19 | `1ac7f00b` | `rustup run 1.97.1 cargo check --locked -p plurx-core -p plurxd --all-targets` | passed in 21s after the final M1 edit |
| 2026-09-19 | `1ac7f00b` | `rustup run 1.97.1 cargo clippy --locked -p plurx-core -p plurxd --all-targets -- -D warnings` | blocked by unchanged pre-existing `http/hls.rs:27556` Boolean assertion lint; M1 paths compile cleanly |
| 2026-09-19 | `04a7727a` | `rustup run 1.97.1 cargo test --locked -p plurx-core --test store_contract scan_identity_series_hint_contract` | SQLite: 1 passed; 117 filtered out |
| 2026-09-19 | `04a7727a` | `rustup run 1.97.1 cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract scan_identity_series_hint_contract -- --test-threads=1` | three-voter Hiqlite: 1 passed; 163 filtered out |
| 2026-09-19 | `04a7727a` | `rustup run 1.97.1 cargo test --locked -p plurxd --bin plurxd scan_identity_hint_` | 4 passed; 2,341 filtered out |
| 2026-09-19 | `04a7727a` | `rustup run 1.97.1 cargo check --locked -p plurx-core -p plurxd --all-targets` | passed in 13s after the final M2 edit |
| 2026-09-19 | `04a7727a` | `rustup run 1.97.1 cargo clippy --locked -p plurx-core -p plurxd --all-targets -- -D warnings` | same unchanged pre-existing `http/hls.rs:27556` Boolean assertion lint; M2 paths compile cleanly |
| 2026-09-19 | M3 working tree from `54a65e93` | `rustup run 1.97.1 cargo check --locked -p plurx-core -p plurxd --all-targets` | passed after planner, both backends, cache/coordinator and HTTP routes were connected |
| 2026-09-19 | M3 working tree from `54a65e93` | `rustup run 1.97.1 cargo test --locked -p plurx-core --test store_contract scan_identity_repair_contract` | SQLite: 1 passed; 118 filtered out |
| 2026-09-19 | M3 working tree from `54a65e93` | `rustup run 1.97.1 cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract scan_identity_repair_contract -- --test-threads=1` | three-voter Hiqlite: 1 passed; 164 filtered out; the first sandboxed attempt could not bind loopback and the authorized rerun passed |
| 2026-09-19 | M3 working tree from `54a65e93` | `rustup run 1.97.1 cargo test --locked -p plurx-core --lib scan_identity_` | 12 passed; 839 filtered out |
| 2026-09-19 | M3 working tree from `54a65e93` | `rustup run 1.97.1 cargo test --locked -p plurxd --bin plurxd scan_identity_` | 6 passed; 2,340 filtered out |
| 2026-09-19 | M3 working tree from `54a65e93` | `python3 -m unittest tests.operations.test_docs_index tests.operations.test_api_doc_routes` | 7 passed |
| 2026-09-19 | M3 working tree from `54a65e93` | `rustup run 1.97.1 cargo check --locked -p plurx-core -p plurxd --all-targets` | passed in 25s after the final M3 edit |

Only the milestone's named focused regressions have run. Broader affected
tests remain deferred to the frozen final candidate. The all-target Clippy
failure is outside this branch's diff and is recorded rather than hidden or
silently repaired as unrelated work.

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

Open the frozen effort candidate against `main`, complete the one adversarial
review, address its findings, then run the final fast lane and promote the
exact qualified tree.
