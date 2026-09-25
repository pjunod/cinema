# Durable cluster work — build status

**Status:** M1 foundation in progress · **Updated:** 2026-09-25 ·
**Branch:** `codex/durable-cluster-work` ·
**Base:** `9f9786b2e` · **PR:** not opened

Companion to the [implementation contract](DURABLE-WORK-QUEUE-IMPLEMENTATION.md).
This page records actual implementation and evidence. “Planned” means no
implementation is claimed; “compiled” does not mean tests passed.

## Delivery progress

| Work | State | Evidence / next action |
|---|---|---|
| Isolated clone | Complete | Agent-owned `/private/tmp/plurx-durable-work-agent`; original checkout untouched |
| Compiler | Ready | Rust 1.97.1; core + Hiqlite all-target compile and baseline daemon compile passed |
| M1 durable queue and pre-transcode | In progress | Typed queue, shared SQLite/Hiqlite SQL, claims, renewal and cancellation compile; publication, retention and worker integration remain |
| M2 fragment analysis and hydration | Planned | Preserve request identity, history and target completion |
| M3 UI, recovery and migration | Planned | Advisory requirements, admin operations, bounded cutover |
| E0 subtitle and library workers | Planned | Reuse newly landed subtitle extraction implementation |
| E1 reads, caches, prediction, artwork | Planned | Reconcile newly landed K-04 replica reads |
| E2 embeddings, probes and repair | Planned | Reuse queue contracts |
| E3 placement and shared Live TV ingest | Planned | Individual peer compatibility; no fleet enablement gates |
| Final adversarial review | Not started | Only at a complete main-bound PR |
| Fast lane | Not run | After review findings are addressed |
| Merge / cleanup | Not started | Green required lane before merge |

## Decisions and unresolved policy

1. Work only in the independent clone. Copy the reviewed plan, not unrelated
   changes from the original checkout.
2. Keep the existing queue behavior until its replacement is complete; do not
   temporarily route accepted work to an unfinished adapter.
3. Do not run tests during construction. The user requested final review,
   fixes, then the fast lane. Clarification is pending because AGENTS.md
   also requires pre-push focused tests and an older qualification receipt.
4. Compile and lint locally with the pinned toolchain; these do not execute
   unit tests. Keep build outputs inside this clone.
5. No product certification flags. Developer settings display prerequisites
   as advisory observations and always allow the preference to be saved.

## Session record

- 2026-09-25: independent Forgejo clone created; Rust 1.97.1 verified;
  baseline compile started. Main includes changes absent from the reviewed
  plan, including K-04 replica reads and K-09 subtitle extraction. Inspect
  and reuse those changes before writing overlapping implementations.

- 2026-09-25: committed plan and status as `76f79006d`; tracked pre-commit
  catalog, formatting, Clippy and served-JavaScript checks passed. Added the
  common queue foundation and six regression contracts; compiled core tests
  with `hiqlite-store` without executing them. This is not an accepted queue
  implementation: publication, retention, authority integration, worker
  adapters and legacy cutover remain unfinished.
