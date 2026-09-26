# Durable cluster work — build status

**Status:** M1–M3 merged; E0 library dispatch implemented · **Updated:** 2026-09-26 ·
**Core branch:** `codex/durable-cluster-work` · **Follow-on:** `codex/cluster-work-adapters` ·
**Base:** `b4b488556` · **PR:** [#532 — merged](http://192.168.4.7:3000/noirr/plurx/pulls/532)

Companion to the [implementation contract](DURABLE-WORK-QUEUE-IMPLEMENTATION.md).
This page records actual implementation and evidence. “Planned” means no
implementation is claimed; “compiled” does not mean tests passed.

## Delivery progress

| Work | State | Evidence / next action |
|---|---|---|
| Isolated clone | Complete | Agent-owned `/private/tmp/plurx-durable-work-agent`; original checkout untouched |
| Compiler | Ready | Rust 1.97.1; core + Hiqlite all-target compile and baseline daemon compile passed |
| M1 durable queue and pre-transcode | Implemented; focused validation passed | Queue ownership, publication, upkeep and pre-transcode discovery/worker integration committed. Bounded cutover and retirement implemented; local exact-recipe offline joining and scheduling implemented; lost-reply and process-expiry regressions written; cross-node transcode convergence belongs to E1 |
| M2 fragment analysis and hydration | Implemented; focused validation passed | Shared fragment worker, durable hydration and atomic request handoff connected; cancellation/provenance integration passed pinned workspace Clippy. Bounded repair and legacy cutover implemented; final regression evidence remains |
| M3 UI, recovery and migration | Implemented; focused validation passed | Admin list/detail/cancel, paged Activity observation and independent upkeep compiled; metrics added to the Store-free scrape cache; explicit retry and advisory Developer controls implemented; final regression evidence remains |
| E0 subtitle and library workers | Library dispatch compiled; remaining adapters in progress | Durable admission replaces the in-memory queue and request ring; scans share physical capacity and preserve coalesced hints. Provider/storage budgets, subtitles and learner permissions remain open |
| E1 reads, caches, prediction, artwork | Planned | Reconcile newly landed K-04 replica reads |
| E2 embeddings, probes and repair | Planned | Reuse queue contracts |
| E3 placement and shared Live TV ingest | Planned | Individual peer compatibility; no fleet enablement gates |
| Final adversarial review | Complete; all four findings addressed | Reviewed `f05f664b8`; four actionable findings below. No repeat review loop |
| Core fast lane | Passed: run 3296 on `3784d5ec5` | Preflight, Rust, Windows, web and main promotion gate all passed |
| Core merge | Complete | PR #532 merged as `b4b488556`; E0–E3 continue in the agent clone |

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

- 2026-09-25: legacy-execution removal committed and pushed as `58df786bb`;
  pinned workspace Clippy and the normal hook passed. No tests executed.
  Hydration now schedules a canonical repair when holders cannot provide the
  artifact, yielding permits while that repair is pending. Repair is a separate
  automatic cache-maintenance interest at normal priority, with a one-hour
  deadline and one request cycle per target/hour. Cancelling a delivery does
  not cancel that independently visible repair; the repair job can itself be
  cancelled in Activity. This bounds background repair after transient demand.
  Completed repair receipts no longer suppress later loss for seven days.

- 2026-09-25: bounded queue counts and oldest age by closed kind/state labels,
  active source-I/O reservations and legacy backlog now travel in the existing
  single Store aggregate and atomic metrics cache. Prometheus scrapes still do
  no Store reads; stale/failed sample reporting is unchanged. Extended the
  backend publication contract and snapshot renderer checks; tests remain unrun.

- 2026-09-25: explicit Retry now preserves terminal history and creates a fresh
  administrator interest (priority 2, 24-hour deadline) for preparation or
  hydration. Fragment Retry creates a forced analysis generation on the node
  receiving the request. It does not resurrect other users' cancelled interests
  or every historical target. The Activity button retains its UUID on transport
  failure; replays return the same request. Source generation checks remain
  atomic at admission. Current tests are written/compiled, not executed.

- 2026-09-25: preparation, indexing and hydration now share an explicit
  one-heavy-worker permit on each node, held alongside real CPU/GPU admission
  until the child joins. Candidate selection is read-only: per-worker-kind
  completion history offers a lower-class turn after eight higher completions,
  always below priority 3. User/library scopes rotate within the selected class;
  keyset cursors retain the fairness window across pages. Added backend
  ordering/pagination and physical admission guard contracts (not yet run).

- 2026-09-25: Developer now exposes the existing analysis switch and
  speculative worker schedule as enable controls. Advisory observations cover
  durable storage, tool inventory, instantaneous capacity, fresh scratch
  headroom, unknown per-source access and peer compatibility. Observation
  timestamps stay visible; delayed results patch evidence rows only. The save
  path reads no readiness value. Also corrected Retry's request-body encoding
  and added transport-replay and unavailable-readiness browser regressions.

- 2026-09-25: speculative discovery can replace a successfully completed
  transcode after its last complete cache location is retired. It creates a new
  repair interest tied to that successful computation and preserves the old
  receipt. Admission atomically rechecks cache absence, source identity and the
  latest computation; failed/cancelled repairs stay terminal across ordinary
  scheduler ticks. Added a replicated backend eviction/replay contract.

- 2026-09-26: continuing core integration. The commit hook caught a private
  clock-helper import in the new integration contract; replaced it with the
  test clock. No test execution or CI run was needed to find the compile error.

- 2026-09-26: offline packages can attach to an already running, resolved
  transcode on their delivery node when the effective recipe and source match.
  Admission rechecks the quota-admitted package claim and producer authority
  atomically, promotes the existing job to priority 2, and pins delivery while
  those interests remain. Package deletion/requeue and caller timeout retire
  only their own interest. Recipe binding survives a yield and rejects changed
  output while an offline consumer waits. Recovery validation precedes recipe
  mutation. New backend contracts are written and awaiting final execution.
  This is deliberately partial: matching work on another node still needs a
  portable transcode delivery path; no cross-node offline convergence is claimed.

- 2026-09-26: exact offline joining committed as `d00a7a532`; pinned all-target
  compile and the normal hook passed. Added fixed-label process counters for
  claim writes/results, renew writes, takeovers, charged failure attempts,
  yields, cancellation and publication fencing, plus claim, queue-wait and
  physical-worker-duration histograms. The scrape reads atomics only. These
  counters reset with the process and describe acknowledged events, so a lost
  reply may undercount; replicated job/attempt history remains authoritative.
  Histogram boundary contracts are written, not executed.

- 2026-09-26: porting prior domain regressions to the common queue: concurrent
  independent handles now assert two shared source-I/O slots and release them
  between rounds; transcode clocks stay within 30-second leases before takeover;
  cache publication fixtures use digest identities. Terminal payload assertions
  preserve audited-retry inputs. Fragment renewal checks retain owner/fence/
  expiry coverage while treating delivery target as an independent interest.
  These are source updates, not a claim that the regressions have passed.

- 2026-09-26: added caller-scoped, test-only loss of committed claim, renewal
  and publication replies. Contracts assert original claim identity, exact
  renewal revision and one artifact/location after publication reconciliation.
  Corrected claim recovery to fetch the committed job row: pairing a recovered
  token with the old candidate snapshot lost takeover failure accounting and
  could retain stale priority/checkpoint information. No product setting or
  runtime feature gate was introduced; fault hooks compile only in test builds.

- 2026-09-26: added independent durable consumers with 5–30 second jittered
  idle backoff; work no longer waits for the minute-long discovery tick.
  Existing scheduler and request paths remain disposable wake hints and share
  the same local execution guards. Fast fragment polling does not run the old
  analysis-settlement/pruning writes; it reads candidates/delivery intents and
  writes only when there is actual work. Missing delivery metadata now settles
  the exact unused claim explicitly instead of spending a crash timeout.

- 2026-09-26: migration contracts now inject a failure on the second mapping
  inside a page, verify that jobs/receipts/target mappings/cursor all roll back,
  and reopen the database before replay. Another contract preserves the old
  transcode staging identity and charges an interrupted owner exactly once
  across restart. The implementation document's milestone prose now matches
  the already-authorized batched-PR directive instead of repeating its old
  three-task-PR wording. These new contracts have not been executed.

- 2026-09-26: explicit fragment retries now resolve and retain the original
  copy-video variant instead of passing an empty “whichever identity is next”
  selector. A changed source/pipeline requires a new media-analysis request,
  preventing a retry click from silently rebuilding another Dolby Vision variant.

- 2026-09-26: integrated current main's transcode module split, process-priority
  accounting and web type checker. Queue admission and publication now live in
  the corresponding extracted modules. Appended queue migrations after main's
  read indexes (SQLite v71, replicated v49); retained literal v47 fixtures for
  the earlier index migration. Web types match the unchanged baseline. The
  merged workspace all-target compile and pinned Clippy passed (`b199a0b1b`).
  Kept the queue's 30-second local refusal: its bounded candidate scan can
  advance past busy candidates, so the old 24-hour suppression is unnecessary.
  No tests or production deployments have run.

- 2026-09-26: added a real-child lease-expiry regression using the production
  queue heartbeat and cancellable process collector. It checks child reaping,
  stale-token refusal, uncharged retirement and capacity release within the
  five-second playback budget. Written and compiled evidence remains separate
  from test execution, which is still deferred.

- 2026-09-26: core implementation is ready for its single final adversarial
  review. Review started against the integrated main base after pinned Clippy
  passed. PR #532 remains a draft. Tests have not started and no merge or
  production change is claimed.

## Final core review — 2026-09-26

Independent review of `f05f664b8` requested four corrections before testing:

| Finding | Correction | Regression / evidence |
|---|---|---|
| P1: v70 backup reads nonexistent queue tables | One v71 introduction constant governs table imports and legacy sealing | Literal v70/v71 reader and replicated v70-import contracts written |
| P1: incompatible fragment worker terminally fails shared work | Check local pipeline before claim; defensive mismatch yields without charging failure | Two worker identities leave original work available to the compatible worker |
| P1: 40,000 retained attempts deadlock pending work | Bounded pressure compaction preserves newest attempt, reconciliation window and counters | 2,500 queued jobs × 16 yields; next claim progresses after upkeep |
| P2: ordinary legacy receipts never release capacity | Only analysis-history interests retain identity indefinitely; sealed mapping prevents reimport | Terminal legacy receipts expire after seven days without replay |

Fixes committed as `b15899175`; normal pinned Clippy/hook passed. The first
focused core run passed all 24 tests (93 seconds execution). Seven of eight
replicated contracts passed; the remaining offline-sharing regression exposed
missing priority recomputation on package deletion. That transition now also
retires a producer when its final interest disappears. Activity fixture coverage
was updated to execute the queue renderer; all 28 checks pass. Developer settings
passed 34 checks, docs index passed four, and the process UI contract passed.
The failing replicated test is being rerun before the main fast lane starts.

- Integrated main `cb67fe938` before final lane: Apple menus/runner updates
  and the equivalent upstream macOS process-priority compile fix. Queue code
  is unchanged by this merge; normal pinned compiler checks are rerun.

- The offline-sharing contract now passes on both backends. Existing adapter
  contracts passed 7/10, exposing two projection defects: staging cleanup
  retained every past owner and fragment status ignored the queue yield delay.
  Staging now follows only the current fence (or pre-claim migration staging),
  and projected readiness honors both job and interest delays. The abandoned
  owner fixture now crosses the real 30-second lease and invokes independent
  upkeep instead of assuming the retired claim-time sweep.

- The expanded replaced-queue contract batch first passed 15/29, then 27/29
  after repairs. Fixed terminal-failure reporting in the legacy admission API
  and preserved charged successful attempts in the domain projection. Updated
  fixtures for atomic request settlement, fixed 30-second leases, independent
  upkeep and the shared two-reader budget. Demand expiry now retains its
  seven-day receipt and failure history; it is not an implicit budget reset.
  Artifact repair must retain immutable builder provenance. The final affected
  SQLite/replicated batch is running; daemon lifecycle tests follow it.

- The replicated cache-clock contract found that common transcode publication
  could move `last_seen_at` backwards. The upsert now preserves the maximum
  observed timestamp, matching the existing fenced-cache contract. Its focused
  rerun follows the in-progress batch; no assertion was weakened.

- Final replicated adapter batch: 32/34 passed. The two failures were the
  cache timestamp regression and a fixture that seeded retired queue rows
  without the v71 migration. The fixture now runs that real migration and
  checks exact parity for common jobs, waiters and legacy mappings. Both
  focused reruns passed (18 seconds execution). The batch included all 29
  affected SQLite contracts and five replicated-only import/concurrency checks.
  Daemon lifecycle validation and the remote fast lane remain pending.

- Integrated main `e680849fb` before the daemon/fast-lane run. Its only new
  surface is web Live TV session controls; the durable queue changes do not
  overlap those files. The normal commit hook and corrective-history audit
  passed for the repaired queue batch (`4df68184b`).

- Daemon lifecycle batch: 9/10 passed, including real-child expiry and probe
  cancellation, lost publication/renewal acknowledgements, retirement, and
  heterogeneous worker dispatch. The remaining lost-claim fixture incorrectly
  dated dispatch in the future while resolution used real time. It now records
  real dispatch time while advancing only the synthetic queue clock; its
  focused rerun passed (one test, 0.14 seconds). Production recovery logic is
  unchanged. All focused failures are resolved. Marking PR #532 ready is next;
  merge remains blocked on its exact candidate's fast lane.

- 2026-09-26: ready PR #532 fast-lane run 3282 stopped in Python preflight,
  before compiler jobs. Reviewed all eight changed task/timer/process counts
  against the main diff and recorded ownership in the existing ledger. Updated
  the terminal health-refusal contract to assert durable request receipts and
  stable automatic request identity, replacing an assertion against the removed
  enqueue API. The two affected static suites passed (21 + 7 tests). No second
  adversarial review and no production deployment.

- Preflight follow-up: all 247 validation tests passed across the batch and the
  permission-enabled runner rerun (69 tests, one platform skip). The initial
  sandbox denied process inspection in six cleanup cases. Operations checked
  551 cases: repaired the jobs-list API table path and route count (236). API
  (3), docs (4), and eight loopback cases passed focused reruns. Three unrelated
  Linux-fixture failures remain local-only: this Mac has no `/bin/true` or GNU
  `timeout`. The Linux fast lane is authoritative for those unchanged fixtures.

- Integrated main `116559cb8` before restarting the lane. Its changes are
  mobile PDF reading, client regression declarations and their documentation;
  no Rust or queue ownership source changed. The final normal hook rechecks
  the integrated tree before push.

- E0 construction: added the durable binding between a queue attempt and the
  existing per-library publication lease. Both SQLite and Hiqlite recheck the
  queue owner inside every existing fenced catalogue transaction. Cancellation,
  expiry, takeover and compaction cannot restore a stale domain owner. Schema
  migration, fresh bootstrap and backup import include the binding. The core
  all-target compile passed; the dual-owner contract is written but will run
  only after this follow-on PR's adversarial review. No scan entry point has
  been redirected yet. Core PR run 3289 passed preflight and web syntax; Rust
  and Windows validation are running.

- E0 storage: durable library admission now commits input hints with its waiter,
  and typed per-request completion checks both owners. Requests arriving during
  execution remain pending until processed; completion cannot settle them by
  accident. Results survive independently of process memory and retire with
  their seven-day receipts. Bounds are 256 pending interests per library,
  16 KiB input and 64 KiB result per request; the handler must report oversized
  results explicitly rather than silently truncate item IDs. The all-target
  core compile passed. These APIs are not yet connected to scan entry points;
  regression execution is deferred to the follow-on review.

- 2026-09-26: fast-lane run 3289 passed preflight, Windows compilation and web
  checks. Rust compilation and Clippy passed, then unit execution found stale
  adapter ownership inventories, downgrade fixtures retaining common-queue
  triggers, invalid publication fixtures, and unobserved cancellation results.
  Repaired the inventories and common-queue downgrade cleanup. The repair-cap
  regression now fills the common queue and preserves its foreground reserve.
  Thirteen focused core cases and both failed migration contracts pass.
  Daemon fixtures now use valid recipe identities and inject unsafe database
  paths only after valid publication; cancellation errors receive bounded
  accounting. All five failed daemon cases pass (four in the batch, scrub
  after correcting its deliberate corruption fixture). Docs index passes. Main remains
  `116559cb8`; no production deployment has occurred.

- E0 library dispatch: full, startup, scheduled and targeted entry points now
  persist their accepted intent. A source-readable voter claims the common
  job and existing library lease, binds both owners, and keeps physical
  admission through cooperative cancellation and process/walker joins. Busy
  library leases yield without charging retries. Per-request results survive
  manager restart and a late arrival cannot be completed by an earlier pass.
  Recent status comes from durable receipts; live local counters overlay it.
  New targeted requests normally return the existing 202 response. Oversized
  results produce an explicit failed receipt rather than truncated item IDs.
  Old process-local pending queues, retry tasks and result rings are removed.
  All-target compilation passed before the final thumbnail/fixture changes;
  the final Rust 1.97.1 workspace Clippy with all targets and denied warnings
  passed. New ownership sites are recorded in the static inventory. No
  follow-on test has been executed.
- Run 3293 stopped in preflight on one stale Python copy of the transaction
  census count; no Rust lane was allocated. The Rust census still checks the
  exact count and transaction ownership. The Python recovery contract now
  checks the explicit assertion's shape, as it already does for migration
  counts, rather than maintaining another unrelated numeric baseline.

- Core PR #532 merged after all required fast-lane gates passed on the reviewed
  candidate. Merge `b4b488556` preserves the regression declarations. The
  follow-on branch now includes current main; no production deployment occurred.
