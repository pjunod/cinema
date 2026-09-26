# Durable cluster work — build status

**Status:** M1/M2 worker integration in progress · **Updated:** 2026-09-25 ·
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
| M1 durable queue and pre-transcode | In progress | Queue ownership, publication, upkeep and pre-transcode discovery/worker integration committed. Legacy cutover and fault-injection coverage remain |
| M2 fragment analysis and hydration | In progress | Shared fragment worker, durable hydration and atomic request handoff connected; cancellation/provenance integration passed pinned workspace Clippy. Repair and legacy cutover remain |
| M3 UI, recovery and migration | In progress | Admin list/detail/cancel, paged Activity observation and independent upkeep compiled; retry, metrics and legacy execution removal remain |
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

- 2026-09-25: typed fragment retry policy committed and pushed as `c3b3e3999`.
  Draft PR #532 remains a draft, verified through Forgejo; its description now
  reflects the pre-transcode integration and remaining fragment work.

- 2026-09-25: added atomic analysis-to-shared-job admission and per-target
  history links. One shared claim projects its owner and retry outcomes into
  the existing domain rows, retaining their prior diagnostics and attempt
  history. Domain-history deletion releases its compact request identity.
  Core/Hiqlite compilation passed; production fragment entry points and
  workers still use their old implementation until the adapter is complete.

- 2026-09-25: atomic fragment request admission and per-target history links
  committed and pushed as `aba86328d`; the normal pinned hook passed.

- 2026-09-25: connected fragment discovery, request handoff, repair admission,
  claim/renewal and publication to the common queue on both Store backends.
  Workers reserve conservative CPU capacity before claiming. The old two
  singleton media-slot leases are replaced by shared source-I/O reservations.
  Durable delivery receipts schedule hydration on the receiving node; verified
  bytes publish their location through the job transaction. Retrieving or
  rebuilding existing bytes preserves the immutable artifact's provenance.

- 2026-09-25: fragment cancellation now signals the probe/index child and joins
  it before releasing physical admission. Analysis cancellation retires only
  its waiter, preserving other consumers of the same computation. Added a
  real-process cancellation regression for the final fast lane. The integrated
  workspace and all test targets passed Rust 1.97.1 Clippy with warnings denied.
  No test has run.
  Legacy migration, remaining old ownership APIs, bounded artifact repair,
  operations/Developer UI and final fault-injection evidence remain open.

- 2026-09-25: shared fragment execution committed and pushed as `d5929a80d`.
  Pinned workspace Clippy and the normal hook passed. PR #532 is still a draft.
  The next queue operations batch adds bounded administrator observations and
  cancellation, Activity paging and attempts, and upkeep independent of feature
  preferences. New observations omit source paths, payloads and ownership tokens.

- Open correctness work before acceptance: migration must preserve per-interest
  fragment retry ledgers when old targets have different attempts/deadlines;
  per-interest ledgers are now implemented, while the importer remains outstanding.
  Missing artifact holders must enqueue bounded canonical repair. Explicit retry,
  offline-demand joining, legacy execution API removal, fairness and the final
  recovery/cancellation tests remain required; this draft is not deployable yet.

- 2026-09-25: operations batch passed pinned workspace Clippy with all targets.
  Added an HTTP regression for administrator-only access, redacted responses and
  repeated cooperative cancellation. Activity labels are fetched in one bounded
  read per page; queue refresh does not block the live Activity overview.
  Test execution remains deferred until the complete PR's adversarial review.

- 2026-09-25: fragment consumers now retain independent attempt limits, due
  times, retry windows, diagnostics and participation fences. Shared execution
  totals no longer exhaust a newly joined consumer's budget. Takeover charges
  only participating requests; cancellation recomputes the remaining schedule.
  Added budget-joining, takeover and delayed-interest regression contracts;
  test execution remains deferred to the final review/fast-lane sequence.

- 2026-09-25: retry-ledger batch committed and pushed as `bf83ee2e4`; the
  normal pinned hook passed. The migration batch now captures a finite legacy
  snapshot with the schema and imports pages atomically, bounded to 128 rows
  and 512 KiB including SQL/parameter framing. Queue-full results remain in
  `awaiting_import`; ordinary background producers yield admission to the
  backlog while foreground headroom remains available.

- 2026-09-25: migrated fragment targets converge onto one computation, with
  independent retry budgets and target receipts. Submitted analysis identities
  survive, including cancellation before import. Pre-transcode preserves the
  existing unique job/staging identity and offers its staging node a 30-second
  preference before another compatible worker may restart it. Existing part
  validation remains authoritative. There is at most one active legacy
  pre-transcode row per dedupe key, so no staging files are combined.

- 2026-09-25: added backup/import/reset mappings and Activity migration counts.
  Wrote duplicate-target/restart and 4,100-request overflow regression fixtures;
  no tests executed. Removal of old ownership APIs and further migration fault
  cases remain before this draft is ready for its final adversarial review.

- 2026-09-25: bounded migration committed and pushed as `5b740b9f7`; the
  normal pinned hook passed. Removed both backends' legacy pre-transcode and
  fragment claim, renew, yield, failure and completion implementations. Domain
  histories and staging keep-lists remain readable; production execution uses
  the common JobToken contract exclusively. Existing cache/offline/history
  fixtures now construct artifacts through the common queue, with a test-only
  helper shared across their test crates. Old queue-specific timing/capacity
  assertions still require reconciliation with the shared queue contract.

- Remaining first-release work: bounded repair when artifact holders are
  unavailable, offline demand joining, explicit admin retry, fairness/metrics,
  advisory Developer observations and final fault-injection evidence. No final
  review or tests have run, and PR #532 remains a draft.
