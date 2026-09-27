# Durable cluster work — build status

**Status:** M1–M3 and E0 merged; E1–E3 reviewed; combined promotion qualifying · **Updated:** 2026-09-27 ·
**Core branch:** `codex/durable-cluster-work` · **Follow-on:** `codex/cluster-work-adapters` · **E1:** `codex/cluster-cache-preparation` ·
**E2:** `codex/cluster-batch-analysis` · **E3:** `codex/cluster-media-placement` · **Base:** `2b09d7a32` · **Core:** [#532 — merged](http://192.168.4.7:3000/noirr/plurx/pulls/532) · **E0:** [#564 — merged](http://192.168.4.7:3000/noirr/plurx/pulls/564) · **E1:** [#566 — superseded by combined promotion](http://192.168.4.7:3000/noirr/plurx/pulls/566) · **E2:** [#567 — superseded by final promotion](http://192.168.4.7:3000/noirr/plurx/pulls/567) · **Final:** [#572 — E1–E3 promotion](http://192.168.4.7:3000/noirr/plurx/pulls/572)

Companion to the [implementation contract](DURABLE-WORK-QUEUE-IMPLEMENTATION.md).
This page records actual implementation and evidence. “Planned” means no
implementation is claimed; “compiled” does not mean tests passed.

## Delivery progress

| Work | State | Evidence / next action |
|---|---|---|
| Isolated clone | Complete | Agent-owned `/private/tmp/plurx-durable-work-agent`; original checkout untouched |
| Compiler | Ready | Rust 1.97.1; core + Hiqlite all-target compile and baseline daemon compile passed |
| M1 durable queue and pre-transcode | Merged in #532 | Queue ownership, publication, upkeep and pre-transcode discovery/worker integration committed. Bounded cutover and retirement implemented; local exact-recipe offline joining and scheduling implemented; lost-reply and process-expiry regressions written; cross-node transcode convergence belongs to E1 |
| M2 fragment analysis and hydration | Merged in #532 | Shared fragment worker, durable hydration and atomic request handoff connected; cancellation/provenance integration passed pinned workspace Clippy. Bounded repair and legacy cutover implemented; fast lane 3296 passed |
| M3 UI, recovery and migration | Merged in #532 | Admin list/detail/cancel, paged Activity observation and independent upkeep compiled; metrics added to the Store-free scrape cache; explicit retry and advisory Developer controls implemented; fast lane 3296 passed |
| E0 subtitle and library workers | Merged | PR #564 merged as `1d70a1fed`; one final review addressed and run 3322 passed all required fast-lane jobs |
| E1 reads, caches, prediction, artwork | Reviewed; promoting in #572 | Read path, artwork, portable transcodes, bounded hot copies and durable predictive analysis implemented. Single final review addressed; focused store, image, copy and cleanup checks passed. Required fast lane remains. Scheduled verification/repair belongs to E2 |
| E2 embeddings, probes and repair | Reviewed; promoting in #572 | Three final findings addressed; focused regressions passed. Candidate `b01b58416` includes current main and the daemon fixture deadline correction |
| E3 placement and shared Live TV ingest | Reviewed; three findings fixed | Advisory enable controls, observed-resource ranking, stable remote Live TV placement and bounded shared ingest implemented. All 12 targeted E3/fixture regressions plus advisory settings passed; 11 docs/ownership checks passed; combined fast lane remains |
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

- E0 storage/provider concurrency: appended SQLite 74 / Hiqlite 52. Root
  aliases map to replicated storage domains; unmapped work uses the existing
  global fallback. Claims reserve every library domain and both provider
  concurrency slots atomically, and normal renewal/settlement owns their
  lifetime. Administrator API and Developer settings expose the mapping.
  Changing identities while live reservations exist returns conflict; feature
  enable switches remain independent. Contracts for aliases, independent
  domains and all-or-none provider contention are written, not executed.
  Rust 1.97.1 workspace/all-target compilation and Clippy with Hiqlite passed.
  Provider rate pacing, subtitles and learner authority remain unfinished.

- E0 provider pacing: appended SQLite 75 / Hiqlite 53. A bounded two-provider
  ledger charges each maintenance dispatch, shares cooldowns and reported
  limits, and retains spent allowance after a lost response. Queue-bound
  library owners and existing artwork/genre publication leases use the same
  budget; no per-node multiplication. HTTP bodies and retry waits cooperate
  with queue cancellation. Wrote cross-owner pacing, cooldown, stale-owner
  transport and populated-backup parity regressions; none executed yet.
  The replicated state digest now includes all four E0 domain tables.

- Construction follow-up: library path edits can change the required storage
  domain without using the mapping editor. Renewal, library binding, catalogue
  publication, per-request completion and provider charging now require the
  claim to hold every current resource. A changed root invalidates those
  operations; the worker stops and a later attempt reacquires current slots.
  Lost-acknowledgement reconciliation checks the same resource snapshot, so
  recovery cannot accidentally restore the invalid claim.
  The ownership contract includes this race. Source eligibility now opens the
  directory rather than treating a directory metadata record as readability.

- E0 learner execution: added per-kind execution authority independently of
  voter-only singleton coordination. A ready, committed, non-removed learner
  may consume immutable work, and renewal/publication recheck that permission.
  Both backends share the artifact-only claim allowlist; catalogue/provider
  kinds and wrong-kind claim replays are refused. Discovery, outbox admission,
  library binding and catalogue completion remain voter operations. Developer
  observations report both authorities without affecting saved preferences.
  Role-loss and claim-scope regressions are written, not run. Rust 1.97.1
  workspace Clippy with all targets, Hiqlite and denied warnings passed after
  correcting the fixture import. Subtitle migration remains the E0 remainder.

- E0 subtitle construction: reuse the existing all-track extractor as one
  computation, including foreground fallback. Analysis records retain durable
  demand/history; common claims project ownership and fenced artifact writes.
  SQLite 76 / Hiqlite 54 add the adapter triggers and invalidate legacy owners
  at maintenance cutover. Store changes compiled; daemon construction checks
  and regression adaptation are in progress. No E0 tests have run.

- Subtitle adapter now compiles with Rust 1.97.1 workspace/all-target Clippy,
  Hiqlite and denied warnings. Foreground and background execution share the
  common job; foreground CPU admission uses the attached TranscodeManager pool.
  Removed the old independent foreground claim API and adapted its coverage/
  repair fixtures. Added exact-owner publication, takeover, forged-source,
  cancellation and lost-completion-acknowledgement regressions. Construction
  caught fixture-only missing imports and lint violations, all corrected.
  No E0 regression has been executed; final review precedes the fast lane.

- Subtitle batch committed as `d2d41217b` through the normal hook, after
  catalog registration and the migration fixture length were corrected.
  Main `0386c78ec` merged cleanly into the candidate. The cutover also preserves
  spent subtitle retry allowance and the configured analysis attempt limit.
  Rechecking the integrated source before opening the final review.

- E0 candidate `1f378c4a1` passed the normal hook against main `0386c78ec`.
  Draft PR #564 is open. Its single independent adversarial review is now
  examining that frozen source; only this status record changes during review.
  Draft status keeps the fast lane idle. The desktop cannot attach Forgejo PR
  artifacts, so the ordinary PR link above is the review entry point.


## E0 adversarial review and corrections

The single review of `1f378c4a1` against `0386c78ec` requested five changes.
All are corrected in the candidate. Post-review queue validation passed on
SQLite and three-voter Hiqlite; the required CI fast lane remains pending.

1. Library success now uses the permitted `succeeded` waiter state; the
   existing durable-result projection and regression use the same state.
2. Subtitle completion atomically settles its receipts and result reference,
   allowing bounded retention to retire completed jobs.
3. Activity Retry successors can enter the common queue. Retrying reuses valid
   immutable representations and retries missing work. Unsupported legacy
   demand is cancelled; individual admission errors do not stop an outbox page.
   Only shared queue capacity stops the page early.
4. Imports from SQLite schemas before 76 revoke legacy running/submitted
   subtitle owners after parity verification, preserving spent attempts. The
   old-backup import regression now includes a running subtitle owner.
5. Audiobook cover extraction uses the cancellable process-group collector,
   joins killed children, and checks cancellation between audiobook parts.
   A stalled-child regression enforces the five-second release budget.

No second review is planned. Run the focused regressions with replicated-store
coverage and the required fast lane, address failures, then merge #564 and
continue E1–E3.

- 2026-09-26: committed the five review corrections as `078bac0cc`; normal
  catalog, formatting, all-target Clippy and JavaScript checks passed. Ran
  `cargo test -p plurx-core --features hiqlite-contract-tests --test
  store_contract background_ -- --test-threads=1`: 14 passed, three failed.
  Fixed a missing SQL parameter in storage-domain listing and two fixture
  setup/cleanup defects, then reran each failing contract: all three passed.
  The old-backup test also proves that normal admissions resume after the
  existing finite legacy import. Documentation and process-ownership inventory
  checks passed (11 tests). This is focused evidence, not a green CI claim.

- 2026-09-26: `cargo test -p plurx-core --features hiqlite-store --lib
  cancellation -- --nocapture` passed all nine selected regressions, including
  audiobook child kill/reap and the sparse scanner join (0.39 s execution).
  The subtitle outbox now also retires permanently fenced/source-invalid
  entries, so stale pages cannot block later work. Required PR fast lane next.

- 2026-09-26: fast lane [3314](http://192.168.4.7:3000/noirr/plurx/actions/runs/3314)
  on `bf6b1365e` stopped in static preflight: the publication-guard inventory
  mistook `self.local_serving_role().await` for direct field access. Registered
  that exact read-only accessor call; direct stores/swaps remain prohibited.
  Rust/Windows did not execute in that failed run. E1 has its own temporary
  branch, `codex/cluster-cache-preparation`, with no implementation changes yet.

- 2026-09-26: E1 construction began on `codex/cluster-cache-preparation` while
  #564 runs its required fast lane. Added browser write-position echo with
  monotonic expiry, full-width decimal indexes, auth-generation isolation and
  out-of-order/unknown-reply handling. Bounded reads become the initial default;
  a replicated Developer preference overrides the seed without restarts. Its
  local preference lookup runs inside the existing per-read proof. Auth stays
  authoritative, and clients without a valid watch floor retain authority reads.
  New regressions are written, not executed; E1 review and tests remain deferred.

- 2026-09-26: lane [3315](http://192.168.4.7:3000/noirr/plurx/actions/runs/3315)
  passed the corrected guard inventory, then found the API overview's stale
  route count (236 versus 237 after storage-domain routes). Corrected the count.
  A local TypeScript check also found three new storage-input element type
  errors; annotated those inputs without raising the baseline. Batch these
  corrections before the next lane run.

- 2026-09-26: E1 read path committed as `5ba02212f`; inherited E0 contract
  fixes through `27e1c0f47`. Pinned workspace/all-target Clippy, served-script
  syntax and TypeScript checks passed without a baseline increase. Added one
  bounded named-settings snapshot on both backends; playback language defaults
  now use one Authority read instead of three. Its backend contract is written
  and compiled, not run. Shared artifact work remains in construction.

- 2026-09-26: lane [3317](http://192.168.4.7:3000/noirr/plurx/actions/runs/3317)
  passed preflight and web checks, then stopped before Rust compilation because
  the isolated Hiqlite spike lockfile lacked core's new cancellation dependency.
  Refreshed only that workspace dependency list; both isolated lockfiles pass
  `make spike-lock-check` with Rust 1.97.1. No dependency version changed.

- 2026-09-26: E1 adds typed immutable artwork identities, bounded verified-holder
  records, exact-attempt publication and target-specific delivery receipts on
  SQLite 77 / Hiqlite 55. Import, replicated-state digest and retention include
  the new records. Source aliases share a content identity; pipeline changes
  create a different one. The cross-backend contract is written, not executed.
  HTTP admission, byte transfer and the worker are still under construction;
  this storage commit alone does not move image generation off requests.

- 2026-09-26: main advanced to `2b09d7a32` with the frozen Fontconfig
  environment implementation. Integrated that source into E0 before promotion;
  the normal pinned hook validates the combined tree before its next lane run.
  The prior lane's result cannot qualify this new tree. Preserved both parents'
  process and timer inventory entries in the only merge conflict.

- 2026-09-26: E0 candidate `715074e19` includes main `2b09d7a32`.
  Lane [3320](http://192.168.4.7:3000/noirr/plurx/actions/runs/3320) passed
  preflight and web checks; Rust and Windows are running. The merged process
  ownership inventory passed seven focused checks before push. No second review.

- 2026-09-26: E1 now connects durable artwork admission, a physical-capacity
  worker, exact-attempt publication and verified peer hydration. Cold requests
  return originals; repeated misses share demand. Pipeline identity includes
  the actual renderer/dependencies, and final filenames include the blob digest.
  Unpublished files cannot become hits, and obsolete owners cannot overwrite
  a published generation. Child cancellation is joined; staging cleanup checks
  current queue ownership. Added coalescing, unpublished/corrupt output and
  real-child cancellation regressions, with execution still deferred. Cache
  prediction, hot second copies, portable transcodes and E2/E3 remain unfinished.

- Agent-only cleanup removed 164 obsolete incremental compiler directories
  (about 35 GiB), preserving the dependency cache, source and all commits.
- 2026-09-26: E0 fast lane 3320 passed preflight, Windows and web; Rust
  reported stale schema/import/census assertions, an old subtitle claim fixture,
  missing scan consumers in HTTP/scheduler fixtures, an unclassified storage-domain
  route and two discarded subtitle cancellation results. Fixes are being checked
  against the pinned compiler before the next candidate. No merge is claimed.
  E1 remains in [draft PR #566](http://192.168.4.7:3000/noirr/plurx/pulls/566);
  portable transcode copies, prediction and E2–E3 remain outstanding.

- 2026-09-26: the missing-root regression exposed a visibility gap. An unreadable
  node correctly leaves accepted scan work for another member, but the status
  did not explain why. Added a bounded node-local readiness observation to the
  existing scan status; it names the last local read failure without settling
  the durable job or spending an attempt. The regression now asserts pending
  work, a visible explanation and zero claims/failures on an unreadable node.

- 2026-09-26: E0 correction evidence: 376 store tests passed on the first
  focused run; its two census failures passed after tightening the ratchet
  (five census/import cases rerun). The subtitle transient-repair contract
  passed SQLite and real three-voter Hiqlite. All 16 selected daemon
  regressions pass with the missing-root visibility fix. Ownership inventory:
  seven passed. Full fast-lane confirmation remains required before merge.

- 2026-09-26: E0 correction `fd0317594` passed its pinned normal commit hook
  and is pushed to PR #564. E1 inherits the fixes, adjusts schema expectations
  for its additional artwork migration, and retains both parents' ownership sites.

- 2026-09-26: E1 portable-transcode storage is in construction. SQLite 78 /
  Hiqlite 56 retain producer source/manifest provenance independently of queue
  receipt cleanup; hydration uses the existing fenced cache publication and
  settles only its receiving target. Bounded upkeep recovers provenance from
  retained completed jobs, never from a cache path or timestamp alone. Added
  copy, stale-owner, changed-source, acknowledgement-replay and target-settlement
  regression coverage; it is written and awaiting final E1 review, not executed.
  The transfer worker and serving endpoint are the next part of this batch.

- 2026-09-26: E1 adds signed manifest-addressed transcode transfers and one
  durable target worker per node. Objects are bounded, hashed before writing,
  and staged under exact job/fence ownership. Cache publication is fenced;
  ambiguous replies leave final bytes for ownership-aware GC. Transfer bodies
  retain capacity and cache pins until dropped. Up to three holders can supply
  each verified object; playback pressure cancels and joins the transfer before
  capacity is released. Authentication, bounded paths, manifest tampering and
  response lifetime regressions are written; execution remains deferred until
  E1's single final review. Hot placement, prediction and E2/E3 remain open.

- 2026-09-26: E0 PR #564 merged as `1d70a1fed50da407b05ab2f2ad523d8e2a970195`
  after the single final adversarial review, fixes, and all required fast-lane
  jobs passed on `fd0317594` (run 3322). Its merge preserves all 18 regression
  declarations. E1 remains in construction; no E1 tests have been executed.

- 2026-09-26: E1 demand selection now uses a bounded replicated snapshot:
  the last day's top 128 watched items, at most 64 active leased viewers, and
  the next item of at most 64 enabled library channels. Each viewer contributes
  at most one next-up episode. Full encoding retains its existing preference
  and budget. A singleton planner requests one secondary hot copy, excludes
  unreachable holders, uses available configured cache budget for transcodes,
  and leaves byte verification to the destination. Automatic transcode and
  hot-copy interests share an atomic 64-interest cap; each pass admits at most
  16 copies. Dormant-user, demand-expiry, target stability, and foreground
  capacity contracts are written/compiled, not run. Predictive index/subtitle
  lifecycle and repair are still outstanding; this is not an E1 completion claim.

- 2026-09-26: Predictive index/subtitle preparation now persists at most 64
  shared admission intents before creating existing typed analysis requests.
  SQLite 79 / Hiqlite 57 carry their bounded lifecycle and import/digest support.
  Demand changes and a 24-hour deadline cancel only prediction-owned interests;
  a real subtitle request can atomically adopt an in-flight extraction. Finished
  analyses release intent capacity. A stale planner or delayed outbox insert
  cannot revive retired work. Source/recipe changes remain fenced by the normal
  workers. Prediction uses the existing bounded all-track subtitle extractor
  instead of adding a second extraction path; this preserves its one-read and
  cancellation behavior. Contracts for ownership, adoption, lease loss and the
  active viewer's following episode are written and compile-only so far.

- 2026-09-26: E1 cleanup now recognizes durable transcode hydration attempts,
  including after process restart. Both staging and renamed directories retain
  exact node/fence ownership; an old generation cannot borrow a newer attempt.
  Artwork final names bind the full variant and output digests within a fixed
  filename bound. A bounded sweep excludes active publication, retains current
  local holder records and reclaims aged unpublished outputs. Added restart,
  stale-fence, orphan, transfer tampering/overflow and stalled-body cancellation
  regression cases; these remain unexecuted until final review.
- 2026-09-26 execution decision: scheduled artifact verification and repair ship
  together in E2, as one recovery path. E1 retains verified serving/transfers and
  the existing integrity sweep. This changes batch ownership, not programme
  scope. No fleet performance gain or production rollout is claimed.

- 2026-09-26: E1's single final adversarial review of `d4064bb21` returned
  two actionable findings. P1: local transcode reuse hashed a whole object
  before observing playback cancellation. Verification now checks between
  bounded reads, awaits the current read before releasing ownership, and uses
  the copy-wide deadline from before local reuse. Copy writes likewise check
  between bounded chunks and join temporary cleanup. P2: hot-copy and ordinary
  delivery used different identities, duplicating work and rejecting long node
  IDs. Both now use the same fixed-length payload hash. The backend contract
  covers both admission orders, independent cancellation and a 256-byte node.
  Regression execution follows these fixes; no second review is scheduled.

- 2026-09-26: Post-review focused evidence: 23 durable SQLite contracts, four
  cooperative cancellation/cleanup tests, six browser read-after tests, seven
  ownership inventory tests and all 50 selected daemon cases passed (44 image,
  four copy, one restart-GC and one placement case). The first image pass had
  one macOS temporary-path fixture failure; the fixture now uses the existing
  canonical temp helper and that case passed on rerun. Read-after coverage is
  wired into the existing web check and fast preflight. The local CI-contract
  subset passed 66/67; the Linux janitor timeout case failed on this macOS host
  without `timeout`. Its Linux fast-lane result remains required; no unrelated
  janitor behavior was changed. Replicated contracts and the fast lane remain
  in progress.

- 2026-09-26: All seven new replicated cases passed against SQLite and real
  three-voter Hiqlite (artwork, prediction, transcode copies, demand expiry,
  and active-viewer next-up). Fast-lane run 3330 stopped in a playback-control
  fixture that extracted `api()` without its new session helpers. Both fixture
  sites now load the shared prelude; the full playback-control, player-DOM and
  Live TV web checks pass. This is a harness correction, with no product change
  and no second adversarial review. The corrected candidate must rerun the lane.

- 2026-09-26: E2 started on `codex/cluster-batch-analysis` from E1 candidate
  `1c885264b`. E1 run 3337 is queued; no merge is claimed. Shared embeddings
  carry exact model/config/tokenizer/normalization identity, verified normalized
  vectors and catalogue source fences. Both storage schemas, import/reset/digest
  mappings and the existing semantic worker are connected. Execution reserves
  physical capacity before claiming and joins inference before releasing it.
  New contracts cover cross-node reuse, cancellation, wrong identity and stale
  publication. E2 tests have not run; compiler/lint evidence is in progress.

- 2026-09-27: committed shared embeddings as `535969244`; the normal hook
  passed. Leaf probes now carry the coordinator lease generation, parser/reporter
  identity and exact file snapshot. The existing re-probe pass admits at most
  128 leaves, uses completed remote facts, and keeps local fallback for work
  not yet started. Filesystem discovery and identity placement stay unchanged.
  Worker publication and coordinator application are separate atomic fences;
  a changed source or coordinator rejects either transition. Contracts are
  written and workspace Clippy passed; E2 tests remain deferred. Semantic jobs
  use background priority and no longer reserve source-media I/O slots.
  Scheduled artifact verification/repair remains to be built. E1 run 3337 has
  green preflight, Windows and web checks; the Rust gate is still running.

- 2026-09-27: main advanced to `24a268339` with K-08's bounded semantic
  inference pool while E1 run 3337 was executing. Integrated that current main
  into E1 without conflicts. The prior candidate has green preflight, Windows
  and web jobs; Rust was still running. The merged candidate requires a fresh
  fast lane. E2's embeddings and leaf probes are committed separately on
  `codex/cluster-batch-analysis` in draft PR #567; scheduled repair remains open.

- 2026-09-27: current-main integration committed as `8e8b9de09`; E2 retains
  K-08's bounded inference pool. Tightened leaf handoff: accepted queued leaves
  now remain with their durable attempts, use one five-minute budget per page,
  and never become duplicate local subprocesses. Admitted work completes before
  legacy local work in the page; failed leaves are reported. Workspace Clippy
  and all test-target compilation passed. No E2 tests have run. E1's new run
  3340 is qualifying `20d2a6bcb`; the obsolete run was cancelled automatically.

- 2026-09-27: scheduled transcode scrubbing moved out of cleanup into admitted,
  node-targeted verification jobs. Exact location generation and worker fencing
  guard corruption retirement; playback/cancellation yields never report bad
  bytes. Artwork uses the same queue and existing bounded cache reader. Repair
  tries a peer copy, then one retained typed producer and delivery; cancellation,
  source changes and exhausted work stop the plan. Original transcode intent
  survives job-history retirement. Activity shows bounded, redacted repair
  observations. SQLite schema 82 / Hiqlite 60 and import mapping are integrated.
  Shared/local embedding digests now reject corrupt vectors, and fragment reads
  stop at declared size plus one byte. New contracts cover generation fencing,
  acknowledgement replay, history retirement and finite repair transitions.
  Construction checks compile test targets only; no E2 tests have executed.
  E1 run 3340 passed preflight, Windows and web; its Rust gate remains running.
- 2026-09-27: E1 run 3340 reached its 30-minute Rust deadline with the
  migration-overflow case unfinished. The full log also identified a SQLite
  placeholder-census failure: the named-settings query added one unchecked
  binding shape. Kept its SQL and binding in one statement; the unchanged
  census now passes. Diagnosed queue admission with `EXPLAIN QUERY PLAN`:
  flattened stages expanded to 21,663 planner steps; materializing the four
  single-row stages reduces this to 120. Added a bounded-plan regression.
  The overflow test still seeds 4,100 accepted jobs and proves the 3,840
  background ceiling; it now frees exactly the 260 needed slots, retaining
  the other jobs instead of spending time cancelling the whole inventory.
  All six migration regressions pass in 47.35 seconds; the placeholder census
  passes separately. Main advanced to `c61bb6409`; integrate and qualify that
  candidate before resubmission. No green merge is claimed.

- 2026-09-27: integrated current main `c61bb6409` (S-14 M8 playback
  ownership seams). Ownership-census conflicts retain both parents' comments
  and count their combined source. Current candidate compilation and the
  affected queue contracts precede the next required fast-lane run.

- 2026-09-27: E1 candidate `02b265bdf` incorporates current main
  `c61bb6409` and the admission-planner fix. Its normal hook, 23 durable
  queue contracts and seven ownership checks passed before push. E2
  integrates the same source; E2 tests remain deferred until its final review.

- 2026-09-27: E1 fast lane 3353 is running on `02b265bdf`. E2 integrates
  that candidate as `3c62043aa`. Completed the no-holder repair transition: it
  advances to the retained producer without charging an impossible copy. A
  source change retires the plan and cancels only its own child interest.
  Repair-ledger capacity cannot keep a known corrupt locator advertised; the
  verification job records `repair_capacity` if no plan slot is available.
  Updated architecture, operations, API, feature and execution-ledger docs.

- 2026-09-27: removed the obsolete unfenced manifest-scrub Store methods and
  their payload type. Existing cursor and consensus-cost coverage now uses
  claimed durable verification; each publication is one transaction. Store,
  transaction, placeholder and consistent-read censuses shrink with the removed
  paths. E2 implementation is complete; final review and fast lane remain.

- 2026-09-27: E2's single adversarial review covered `02b265bdf..a43205ab6`
  and found three P2 issues. Published embedding completions now enter the
  local serving index immediately; queued repair copies whose final holder
  disappears advance to one rebuild while cancelling only their own interest;
  an overdue leaf is reported without discarding later completed probes.
  Added regressions for all three. Post-review qualification is starting.

- 2026-09-27: post-review probe coverage exposed and corrected canonical JSON
  ordering at publication (the source identity now serializes the same way as
  enqueue). The overdue-first-leaf regression passes, as does immediate local
  embedding installation. Three-voter contracts are running; their first probe
  run used the prior snapshot and will be rerun with the serialization fix.
  E1's full fast lane identified stale artwork/readiness inventories and a
  pagination fixture exceeding the new automatic-demand cap. The fixture now
  uses explicit manual demand for its 129-row capability scan; the automatic
  limit itself remains covered and unchanged.

- 2026-09-27: E2 post-review focused qualification passed: 28 shared queue
  contracts on SQLite and a real local three-voter cluster, followed by the
  corrected probe contract on the same backends; the migrated distributed
  pre-transcode and one-consensus-entry verification contracts also pass.
  Immediate embedding installation and overdue-leaf preservation pass, as do
  four Store source censuses, four documentation-index checks and seven
  ownership-inventory checks. E1 run 3353 finished with exactly three failures:
  the two stale inventories and the pagination demand fixture, all corrected
  in `7cfafe6cc` with focused regressions passing. No merge is claimed yet.

- 2026-09-27: promote E1 and E2 together through #567. Neither is merged yet;
  requalifying E1 separately would repeat the same full workspace fast lane.
  #566 returns to draft as superseded and will close after #567 merges. The
  E1 review and its fixes remain recorded; E2's one review covered its exact
  additional implementation. This batches the completed commits without a
  repeat adversarial review. Both inventories, capability pagination, three
  cache-verification tests and the 445-method Store inventory pass. Candidate
  `465f16594` passed the normal pinned hook; only promotion bookkeeping follows.

- 2026-09-27: combined E1–E2 candidate `05192c812` is running required fast
  lane 3358. #566 is verified draft and superseded. E3 continues separately on
  `codex/cluster-media-placement`, based on the integrated candidate. Placement
  and takeover no longer require a uniformly fresh fleet; saved preferences
  always persist. Developer controls show requirements as advisory. Recent
  storage-read latency, client delivery and peer throughput are bounded node
  observations; missing samples never refuse work. Shared remote Live TV
  processing remains unfinished. No E3 tests have run.

- 2026-09-27: E3 draft [#572](http://192.168.4.7:3000/noirr/plurx/pulls/572)
  contains advisory placement controls and recent I/O ranking. Added an
  exact-authenticated raw Live TV consumer using the existing shared tuner
  transport and bounded per-consumer queue. Viewer execution can consume a
  peer response through its existing process/admission lifecycle. Placement,
  recovery routing and remote start authorization remain under construction;
  this is not yet a completed remote playback path. Pinned workspace
  all-target Clippy passes; no E3 tests have run.

- 2026-09-27: connected the new placed-start protocol to the tuner owner's
  bounded assignment history, worker selection, signed processing/ingest
  requests, and existing activation/resource/recovery routes. An ambiguous
  start keeps its worker; retirement fences new ingest before cleanup, and
  unreachable assignments are retained rather than reassigned. Workers reuse
  the existing FFmpeg admission, fan-out, source probing and cleanup paths;
  remote sessions do not fetch tuner signal directly. Mixed-version ingress
  retains the older local-owner start protocol. All-target compilation passed;
  adversarial review and tests still await the completed E3 batch.

- 2026-09-27: combined fast lane 3358 passed 1,360 core unit tests and
  all 179 storage contracts, including the real replicated backend. The daemon
  suite reported `decode_fact_source_shipped_shape`: its non-deadline fixture
  allowed only 100 ms for process scheduling. Expanded that fixture budget to
  five seconds while retaining its exact invalid-JSON and permit-release
  assertions. Integrated current main's Apple Live TV URL repair (`1cbdc8d51`)
  for the next promotion candidate. E3 remains isolated on its draft branch.


- 2026-09-27: E3 implementation and regression-writing complete. New coverage
  exercises one owner ingest with three viewers across two processing nodes,
  independent slow-peer eviction, retry/retirement ownership, bounded assignment
  pressure, household-bearer refusal, maintenance admission, missing metrics
  and incompatible peers. Static ownership counters record the additional
  bounded peer/recovery deadlines and the retained test HTTP server. No E3
  tests have run. Next: the one final adversarial review, findings, then the
  fast lane. E1–E2 candidate `b01b58416` runs separately in fast lane 3360.


- 2026-09-27: the single E3 adversarial review of `d48607ee2` found three
  issues. All are addressed: (1) peer-backed viewer transports now survive
  non-owner DVR reconciliation, and the two-node regression runs the actual
  DVR loop; (2) unknown retirement fences have a separate eight-entry per-user
  budget and cannot exhaust other viewers' assignment capacity; (3) assignment
  generations advance monotonically, rejecting delayed older starts before
  they can remove newer ownership. Added targeted regressions for all three.
  Pinned all-target Clippy passed; test execution follows this fix commit.

- Promotion decision: main advanced to Apple build 190 while E1–E2 qualified.
  With E3's one final review also complete, the remaining E1–E3 changes will
  promote together through #572 after integrating current main. This replaces
  separate overlapping E1–E2 and E3 qualifications. Prior reviews remain the
  final reviews of their respective implementation batches. #566 and #567
  will close as superseded after the combined candidate merges; no production
  deployment is authorized or claimed by these merges.

- 2026-09-27: final E1–E3 candidate integrates main `9660f698a` (Apple build
  190). E3 review fixes committed as `fd4d45ae2`. All 12 targeted daemon
  regressions passed, plus the existing settings authorization/advisory-enable
  regression. Two socket fixtures initially hit the local sandbox restriction;
  rerunning those exact tests with loopback permission passed (3 tests, 4.31s,
  including settings). The corrected E2 source-shape fixture passed as well.
  Documentation index and transport ownership suites passed all 11 checks.
  Commands: `cargo test -p plurxd --bin plurxd -- --exact` with the PR's E3
  regression names and `decode_facts::tests::decode_fact_source_shipped_shape`;
  `python3 -m unittest tests.validation.test_rolling_producer_ownership_inventory
  tests.operations.test_docs_index`. PR #572 now targets main; #567 is draft
  pending supersession. The final required fast lane is next.
