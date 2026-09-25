# Durable cluster work — build status

**Status:** M1 worker integration in progress · **Updated:** 2026-09-25 ·
**Branch:** `codex/durable-cluster-work` ·
**Base:** `9f9786b2e` · **PR:** [#532 — draft](http://192.168.4.7:3000/noirr/plurx/pulls/532)

Companion to the [implementation contract](DURABLE-WORK-QUEUE-IMPLEMENTATION.md).
This page records actual implementation and evidence. “Planned” means no
implementation is claimed; “compiled” does not mean tests passed.

## Delivery progress

| Work | State | Evidence / next action |
|---|---|---|
| Isolated clone | Complete | Agent-owned `/private/tmp/plurx-durable-work-agent`; original checkout untouched |
| Compiler | Ready | Rust 1.97.1; core + Hiqlite all-target compile and baseline daemon compile passed |
| M1 durable queue and pre-transcode | In progress | Queue ownership, publication and upkeep committed; pre-transcode discovery/worker integration being compiled. Legacy cutover and fault-injection coverage remain |
| M2 fragment analysis and hydration | In progress | Atomic fragment publication and durable delivery receipts compiled; existing worker, request history and retry adapters still need conversion |
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
   fixes, then the fast lane. This build follows that explicit 2026-09-25 instruction over the older
   pre-push focused-test requirement. The dated pipeline amendment already
   supersedes automatic full qualification; no receipt wrapper is planned.
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

- 2026-09-25: foundation commit `346f4241a` passed the normal tracked hook.
  Follow-up adds independent waiter cancellation, candidate keyset pages,
  expired-cancellation cleanup and bounded attempt compaction. Ten queue
  regression contracts are written, with no test execution yet. At that checkpoint no PR had been opened or branch pushed; no production
  node has been changed.

- 2026-09-25: bounded cleanup now preserves seven-day request receipts and
  compact domain identities independently of retired job details. Added
  expiry and retention regressions, target delivery identity independent of
  execution placement, and the queue interface to the aggregate Store.

- 2026-09-25: typed pre-transcode publication now commits the cache location,
  manifest digest, queue result and waiter transitions together. A shared
  backend contract covers acknowledgement replay, source replacement and
  cancellation before publication. Worker integration and legacy migration
  are still outstanding; the new queue does not yet execute production work.

- 2026-09-25: upkeep and publication committed as `00c5e6bac`; normal tracked
  hook passed. Three commits pushed to draft PR #532, confirmed `draft: true`
  through Forgejo. The draft deliberately has no final review or test receipt.
  Scope remains the full requested programme, with the durable core first.

- 2026-09-25: pre-transcode discovery and execution now use the common queue
  in this draft. Singleton discovery fencing advances atomically with the
  admission verdict. Workers reserve real hardware/CPU admission before the
  durable claim, resolve ambiguous ownership by boot/claim identity, and
  self-cancel at the monotonic lease deadline. Publication and retirement
  serialize against renewal. Cache cleanup recognizes both shared and legacy
  ownership while migration remains unfinished.

- 2026-09-25: added regression contracts for cancellation racing retirement,
  a blocked snapshot after revocation, discovery-lease reuse, unsupported
  payload visibility, and repeated worker crashes exhausting their budget.
  Test execution remains deferred. Source-change cancellation, bounded retry
  delays and conservative pre-claim CPU admission are implementation choices;
  the conservative reservation can reduce hardware-only background throughput
  until a plan-specific estimate is available before claim.

- 2026-09-25: Rust 1.97.1 workspace Clippy with all targets and denied warnings
  passed for the integrated pre-transcode path. No tests have been executed.

- 2026-09-25: pre-transcode worker integration committed and pushed as
  `84c188a87`. The pinned normal pre-commit hook passed, with no tests run.

- 2026-09-25: added the fragment publication transaction and a bounded durable
  delivery outbox derived from waiting target receipts. Hydration preserves
  the artifact's original builder and completes only the receiving target.
  Cancellation retains independent interests; unsupported payloads stay
  inspectable. The replicated-state digest now includes queue jobs, receipts,
  attempts and reservations. A backend contract exercises one shared build,
  independent cancellation and delivery after a scheduler restart. These
  Store paths are compiled but not yet connected to the fragment worker.

- 2026-09-25: fragment publication/delivery foundation committed and pushed as
  `82e79ba89`; pinned Clippy, formatting, catalog and JavaScript checks passed.
  Fragment execution has not yet switched queues.

- 2026-09-25: the common queue now carries the fragment adapter's bounded
  attempt history, diagnostics and fixed retry deadline. It calls the existing
  typed index retry policy, including its half-hour initial retry and seven-day
  window, rather than substituting the transcode backoff. The configured
  attempt limit is captured on acceptance so an accepted job keeps a stable
  budget; setting changes apply to newly accepted jobs. Core and Hiqlite tests
  compile with Rust 1.97.1; execution remains deferred to the final fast lane.
