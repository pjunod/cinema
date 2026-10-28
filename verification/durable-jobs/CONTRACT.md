# Durable-job contract — connect finite authority transitions to source

**Status:** reviewed source contract; bounded qualification complete ·
**Initial checked source:** `663b3aa548` · **Latest scoped source review:**
`be610d6472b8717bc034e45404141ef0854c4bf2` (adaptive boundaries),
`7d2d4a9c777d42f6f3cb28db8e2a7d5be6f46a37` (title-analysis delta) · **Scope:** bound `library_scan` and
`metadata_refresh` workers calling `insert_item_fenced`.

Companion to [README.md](README.md), which gives the reproducible commands,
and the [pilot proposal](../../docs/cluster/FORMAL-VERIFICATION-PILOT-PROPOSAL.md),
which owns acceptance. The transition correspondence below is a **reviewed
source mapping**, not an automatic refinement proof. Source hashes in
`mapping.json` detect drift in the reviewed dependency set; they cannot prove
that set complete or that the abstractions are correct.

## Linux-only title helper correspondence — 2026-10-28

A source-only comparison of `state.rs` at `e1d42fb2afc0` and `947b9f86d`
finds exactly three cfg changes: `TITLE_SCAN_INDEX_WAIT` and
`title_scan_awaits_index_pass` become Linux-only, and the test setter
`set_local_index_pass` becomes `all(test, target_os = "linux")`. The resulting
file is byte-identical in composed effort `9ff0fb7b6` and candidate `78a3a4e3e2f6`.
The consumers are in the already Linux-gated `source_probe::title_work` module
and its test child; the helper bodies themselves do not change.

The selected library scheduler/worker, authority, claim, bind, renewal,
settlement, fenced publication and token/publication lock ordering are
unchanged. Existing anchors remain unique. Only the affected whole-file pins
are refreshed; earlier review identities and exclusions are retained. Models,
configurations, bounds, fairness premises, action/check/test inventories and
validators do not change. This correspondence audit runs no model or
behavioral tests and grants no native closure, progress or new qualification.

## Metadata-only title analysis — scoped refresh on 2026-10-28

Codex compared PR #1301 source `7d2d4a9c7` with main `77e985827`.
The two title-only trait/delegation additions expose stale-FEL selection and
analysis publication. The latter validates source identity, scanner and demand
generation, preserves another scanner's active job, and uses the existing
`publish_dv_title` trigger with `job_id` NULL. Only title probe JSON receives
the aggregate; no queue or waiter settlement row matches that NULL identity.
Title demand binding and FEL requalification remain separate from the selected
library-worker authority. The schema and trigger definitions are unchanged.

`state.rs` adds bounded fresh-title timing, local-index mode and the analysis
result offer. It changes no selected library claim, binding, renewal,
cancellation, settlement, fenced item publication or token/publication lock
order. Store-contract edits add title method names and examples; the exact
`formal_publication` registration and selected examples are unchanged.

Nineteen whole-file pins are refreshed after this source disposition; every
existing anchor remains unique. Title scanning, local index waiting and both
title publication paths remain excluded. Models, configurations, bounds,
properties, toolchain pins and historical explanatory source anchors retain
their identities. This review is source correspondence, not new execution or
qualification; fresh affected smoke receipts are separate evidence.

## HTTPS and adaptive effort source binding — 2026-10-26

This source audit covers the resolved union of effort `8eee8a897` and Main
`656a85540`. Its source pins read the final formatted integration files before
the merge commit; the historical sections below keep their original source
and evidence identities. Current wait-map companion rows bind this resolved
source rather than transferring an old source snapshot to it.

The retained heartbeat/token guards, failed-join and ambiguous-retirement gaps
remain those recorded by the accepted adaptive source audit. Main's new
fragment checks precede enqueue receipt reuse only when fragment/analysis
fields are present; ordinary selected library inputs omit both fields. The
identity-repair classifier permits zero-to-many rows only for its guarded
annotation set-copy and retains full-vector/all-other-exact-one checks. That
repair, managed-schema bootstrap and public TLS ownership remain excluded;
selected publication, worker admission, wait and settlement paths retain their
existing scope. The unextended library admission still claims `(0, threads)`.

Title requalification is separate: its active-job read and later `bind` remain
two calls, so a newly queued title job can still race them. Main's fragment
receipt fix supplies no title-race proof. Models, configurations, action/check
and test inventories, validators and qualification claims are unchanged. This
metadata resolution runs no model or behavioral tests and claims no native
closure, last-alias release, global progress or new acceptance.

## HTTPS and Linux-player integration — 2026-10-26

The merge of HTTPS `67b09d29c` with main `167af4616` retains both scoped
source reviews below. The new FEL trait/delegation methods, off-boot VA-API
probe and independent handoff-pruning loop do not alter selected library
claim, renewal, binding, wait/lock, settlement or fenced-publication paths.
The Store parent retains strict historical fixtures and inventories; its
`formal_publication` module is unchanged. Reviewed whole-file pins follow
the composed source; models, configurations, anchors and scope do not change.
FEL, handoff lifecycle, identity repair, historical migrations and public TLS
remain outside these guarantees. This is source correspondence, not new
execution, physical acceptance or qualification. Other historical explanatory
anchors in `source-anchors.json` retain their recorded historical identity.

## Completion identity and fixture correspondence — 2026-10-25

Source `5eacfd4c6b83a6f036627a0bfb80c60d0d59cba9` changes only the identity-repair result
classifier in the mapped publication file: the existing guarded annotation
set-copy may affect zero-to-many rows; the full result vector and every other
exact-one guard/mutation/postcondition/ack check remain required. Identity repair
is outside this selected model and **is not proved by it**. The shared
`atomic_publication` renewal, authority binding, wait/lock order and selected
`insert_item_fenced` SQL/anchors are unchanged.

The Store parent adds title-method inventory, strict historical empty-extension
fixtures and rejection through an already consumed fragment-analysis claim.
Its `formal_publication` registration and selected conformance examples are
unchanged. General fragment admission, historical migrations and title jobs
remain excluded; valid installed schema and selected payloads remain premises.
The fourteen publication pins and one parent-module pin track whole-file
correspondence only. Models, configurations, anchors, validators and scope are
unchanged. This review supplies no new theorem, runtime, physical-acceptance or
qualification claim; affected smoke receipts are separate source-bound evidence.

## HTTPS integration — scoped refresh on 2026-10-25

Codex reviewed source `f18c39e66954b71fcedcf8f463914f112498a518` against
`9bcfa9ca1f792d214122762981c84c85ba5b3e64`; the parent coordinator approved
the source disposition. Five drifted whole-file pins are refreshed; every
existing exact anchor remains unique.

The shared enqueue SQL moves fragment analysis/source guards before receipt
reuse. `EnqueueJob` and ordinary library admission omit `analysis_request`
and `fragment_domain`; only the fragment adapter adds them. The selected
`library_scan` / `metadata_refresh` admission and claim-token replay guards
are unchanged. General fragment enqueue receipts are outside this model's
`ReplayClaim` / `ResolveClaim` transitions and S4 claim-token ledger property.
The separate runtime receipt matrix passed replay, cancellation, reclaimed
analysis and replaced-source cases on memory/pooled SQLite and three voters;
that is runtime evidence, not a model result.

Managed-schema bootstrap now validates/preserves the installed resource view;
its mapped `QueueSql` implementation is byte-identical. Store-contract edits
repair/add fixtures while retaining the exact formal-publication registration.
`AppState` only gains the addressed-node HTTPS control field and initializer.
The lockfile adds daemon dependency edges without changing Tokio 1.53.1 or
Hiqlite 0.14.0. Valid installed schema and valid selected payloads remain
premises; this refresh claims no bootstrap, fragment-admission or TLS theorem.
Model/configuration/toolchain bytes, anchors, governed scope and rejection
rules are unchanged. Prior qualification retains its original source binding;
fresh affected smoke receipts are recorded separately.
## Main and effort source binding — 2026-10-26

Current source inspected: `2dc07bd0995f07f26281c72de80117f2c9afa9f0`,
the resolved union of effort `647e4bb68` and Main `94da6eb93`. The accepted
source/Unknown disclosures remain descriptive abstractions; this audit
rechecks their actual retained guards and does not transfer a theorem to the
merged source.

Main adds FEL title-verdict/requalification methods, separate playback-handoff
schema and queries, a handoff-pruning task and a detached startup decoder probe.
Those application paths remain outside the selected `library_scan` and
`metadata_refresh` model. The selected library admission never installs the
FEL extension: its claimed resources remain `(0, threads)`, so the updated
idle predicate is equivalent for that lane. A title requalification preflight
read and its later `bind` are separate Store calls; they do not establish an
atomic exclusion of a job queued between them.

The source pins below bind the actual resolved files, including nested worker
correspondence and the wait-map source-anchor companion. Models, configurations,
validators and test inventory are unchanged. Only offline mapping checks are
run for this audit; earlier model or behavioral results retain their original
source and scope. No new native closure, progress or qualification is claimed.

## FEL qualification review fixes — scoped refresh on 2026-10-26 (2)

The PR #1290 review delta changes two mapped files. In
[dv_title_jobs.rs](../../crates/plurx-core/src/store/dv_title_jobs.rs) the
re-qualification request now selects the title demand only when no
`dv_title_scan` job for that file is queued, running or cancelling in that
preflight read. It then awaits a separate call to the existing reviewed `bind`
command. A job can be queued or claimed between that read and `bind`; the bind
command and trigger do not recheck active jobs. The added example covers an
already-running job, not this interleaving. Atomic exclusion of a newly
in-flight title attempt remains unresolved and outside this model. The verdict
counts walk
`dv_title_jobs_demand` (one row per DV title, `file_id` UNIQUE) and read each
`files` row by key instead of scanning the catalogue; a read. The store
contract gains one assertion block in the FEL example (the no-op while a job
runs). No selected-library queue, waiter, fence, lease, reservation or publication
transition changed and every anchor string is unchanged. **Model decision:**
unchanged;
`DurableJobs.tla` still describes the selected library-worker scope, which
these title-extension reads and the guarded `bind` call do not enter. Hashes
rebound; no new qualification is claimed.

## FEL qualification inside the DV title job — scoped refresh on 2026-10-26

The `claude/lp-fel-p81` delta against `origin/effort/linux-player` (b75b81fa5)
adds three `BackgroundJobStore` methods and their delegations
(`put_dv_fel_verdict`, `request_dv_fel_requalification`,
`dv_fel_verdict_counts`), their bodies in
[dv_title_jobs.rs](../../crates/plurx-core/src/store/dv_title_jobs.rs), and one
title contract example. Reviewed against the model: the verdict write is a
guarded `json_set` of one application key in `files.probe_json` (same file id,
size, mtime, Profile 7 with an EL); it touches no queue, waiter, reservation,
fence or binding row and writes no `scanned_at`, so it rotates no title
demand. Re-qualification calls the existing `bind` command (the reviewed title
demand rotation) with a binding no scanner equals; the scan worker's existing
discovery then rebinds and enqueues as before. The counts are a read. The
qualification itself runs inside the `dv_title_scan` attempt between its scan
and its existing `publish_dv_title`, under the same lease, fence and
settlement paths; preempting it cancels only its own children. The selected
library workers' bind-before-work path, claim CAS/replay, renewal and
cancellation guards, counters, reservations and `insert_item_fenced` are
untouched. All thirteen anchor strings are unchanged; the only change inside
an anchored item is the three methods appended to the queue protocol trait and
its shared adapter. The DV title extension stays excluded from this model. **Model
decision:** `DurableJobs.tla`, its configurations and properties still
describe the selected scope; nothing in them changes. Only the whole-file
hashes are rebound; no new qualification is claimed.

## Advisory title readiness — scoped refresh on 2026-10-23

The source delta from A2923b2c to be610d647 adds only the
`dv_title_readiness` trait/delegation and `TitleReadiness` helper in the two
drifted files. The helper SELECTs title summary JSON and derives advisory
counts; it does not enqueue, claim, renew, publish or cancel jobs. Existing
queue and title-publication transitions and all eleven exact-string anchors
are unchanged. Their whole-file mapping hashes are rebound to the inspected
source. This read-only API remains outside the selected library-worker proof;
no model/configuration change, model replay or new qualification is claimed.

## DV-title additions preserve the selected authority boundary

Codex reviewed the six drifted mapped files at
`4b359f7923c47a1e8715e826a9b6581a08f60e05` against
`1f30b4791476acb5d290c2f8e6ed38d07051f31d` on 2026-10-22. Their recorded
hashes matched that base. The added module registrations, schema installers,
queue kind/payload/delegates and `JobFence::publish_dv_title` leave the selected
library workers' bind-before-work path, stable binding, claim CAS/replay,
exact renewal/cancellation guards, fence/revision counters, reservation and
receipt transitions, and fenced insertion transaction unchanged. The two
new shared enqueue/claim predicates apply only to `dv_title_scan`.

The added [title schema](../../crates/plurx-core/src/store/dv_title_jobs.sql)
was also reviewed because its triggers run on shared files and queue tables.
Its job/waiter invalidation selects title jobs; its terminal trigger selects
the title kind. Ordinary file publication may rotate title demand and remove
title JSON in the same existing transaction, but does not change the selected
library job's authority or its binding. Both backends install the same
extension and verify its shape through
[dv_title_jobs_schema.rs](../../crates/plurx-core/src/store/dv_title_jobs_schema.rs).
The mapping now tracks these dependencies so later drift requires review.

The [job title publisher](../../crates/plurx-core/src/store/dv_title_jobs.rs)
requires a `dv_title_scan` job and uses its own queue-fenced command, outside
`insert_item_fenced`. The separate analysis publisher uses source, scanner and
demand-generation checks with the existing title trigger and `job_id` NULL;
it attaches title probe JSON without settling a queue job or waiter. Its source generations, JSON attachment, cancellation
suppression, scheduling/preemption and replay/retry behavior are **excluded
from this model**. The existing
[title contract examples](../../crates/plurx-core/tests/store_contract/dv_title_jobs.rs)
`title_publication_is_atomic_idempotent_and_never_self_enqueues` and
`title_manual_cancel_wins_publication_and_does_not_become_scheduler_yield`
are source-backed runtime regression anchors, not formal guarantees or claims
that those tests ran in this review. No model, configuration or property was
changed. This source-mapping review supplies no new execution or qualification
receipt; the historical bounded result retains its stated scope.

## Publication checks two independent owners in one transaction

The queue tuple is job/node/boot/claim/fence/revision/expiry. The domain tuple
is resource/owner/fence/revision/expiry. Binding stores the stable queue
identity (including fence, boot and claim), plus the domain fence. Queue and
domain renewal revisions deliberately do not appear in the stored binding.
A worker can retain stale local tuples; only the authoritative Store guards
decide whether a transaction succeeds.

Both backend publication guards permit domain-only operation if no binding
matches the current domain fence. This is real behavior, not a universal
queue-bound invariant. The selected worker must bind successfully before
starting work. An obsolete binding survives queue expiry, compaction and
ordinary domain release; only deleting the domain row deletes its binding.
A newly acquired higher domain fence may temporarily have no matching binding.

| Transition / boundary | Source and guard | Effect / observable result | Omitted state or premise |
|---|---|---|---|
| Enqueue / demand | `store/background_jobs.rs::enqueue_job`, `ENQUEUE_SQL`, `background_jobs_retention.sql::background_job_enqueue_command` | Accepted demand creates or joins a queued job; request identity differs from job identity | Library id, kind and payload-v1 validation are fixed valid; waiter priorities and payload data are abstracted |
| Claim read, then CAS | `background_jobs.rs::claim_job`, `CLAIM_SQL` | First read can return the same live claim; otherwise exact observed revision CAS claims eligible queued/expired work, increments fence/revision, installs attempt and both required reservations atomically | Read and CAS are separate transitions; capability/kind predicates fixed to selected kinds; no Raft implementation |
| Claim eligibility | `CLAIM_SQL` and resource triggers in `background_jobs_resources.sql` | Valid demand, not-before reached, retry budget, owner not removed, dispatch window, fresh identity, reservation slots available; live reservations of other jobs compete | Finite identities never recycle; counter exhaustion is an exploration boundary, not a production terminal state |
| Replay / resolution | `background_jobs.rs::claim_job` and `resolve_claim` | Same identity returns existing current token only with matching node/boot, running state, live expiry and resource coverage; absent history gives expired/pruned, never a new attempt | Resolve without fence additionally checks dispatch window; per-operation caller `now_ms` is retained |
| Queue renewal | `RENEW_SQL`, `background_jobs_schema.sql::background_job_renew_command` | Exact predecessor, running state, live lease, no removed owner, complete reservations; increments revision and updates reservation expiry | Cancellation changes revision as well as state; replay is not a new renewal |
| Cancellation / settlement | `cancel_job`, `SETTLE_SQL`, cancellation/settlement triggers | Queued→cancelled, running→cancelling; exact live settlement clears owner and reservations; yield preserves demand and installs not-before; failure consumes retry budget | S5 checks no revival of cancelling by modeled actions; cancelling is absorbing here, with no modeled completion/settlement from that state. New requests and general cancellation/completion precedence are outside this fragment |
| Domain acquire / renew / release | `store/sqlite/coordination.rs`; `store/hiqlite_coordination.rs`; `cluster/coordination.rs::Lease` | Acquire expired row with higher fence/revision; exact renew; release clamps expiry and advances revision without deleting row | SQLite acquire/renew do not have Hiqlite's explicit removed-owner filter; publication still checks its bound job |
| Bind | `background_jobs_domain.rs::BIND_SQL` | Selected kind and scan resource; exact live queue/domain tuples; reservations; owner not removed; stable tuple installed | Same domain fence accepts only identical stable tuple; different attempt needs a higher domain fence |
| Bind refusal | `plurxd/library_work.rs` around `bind_library` | Release domain lease, yield job until `now+5000`; work never starts | Poll/backoff runtime scheduling is a recovery fairness premise |
| Facade precheck | `store/publication.rs::PublicationStore::fenced_call` | Under write lock, reject revoked/missing/expired local lease; sample precheck timestamp; compute successor; call backend | Ordinary metadata time is not an authority clock |
| Successor | `cluster/coordination.rs::Lease::publication_successor` | Independent time sample; revision+1; expiry=max(sample+90000,pred.expiry+1); checked counter/minimum overflow | Finite counters never wrap; production i64 saturation/overflow remains ordinary test evidence |
| SQLite insertion | `store/sqlite/mod.rs::with_fenced_conn`; `sqlite/publication.rs::insert_item_fenced` | Sample inside transaction; validate successor; exact predecessor UPDATE plus binding guard; insert; commit, or rollback both changes | SQLite connection transaction serialization assumed |
| Hiqlite insertion | `hiqlite_publication.rs::atomic_renewal_statement`, `atomic_publication`, `insert_item_fenced`, `bind_atomic_authority` | Sample before await for each submission; statement zero renews predecessor with binding guard; insert rechecks the successor tuple returned by statement zero; transaction commits or rolls back | Serializable authoritative transaction/rollback is assumed, with real backend examples below |
| Retry | `hiqlite_publication.rs::insert_item_fenced` loop `0..4` | Same predecessor and successor, fresh candidate and backend sample; only allocated mode plus identity-error branch retries; up to four submissions | Identity candidate and error text are opaque; no retry on arbitrary lost reply/transport failure |
| Reply / facade state | `PublicationStore::fenced_call` | Backend error clears local lease; success records successor; local revocation after commit can return FenceRejected while insertion remains | Commit and acknowledgement are separate transitions; S7 is not a claim that every facade error means no item |
| Crash / reboot / removal | `plurxd/background_jobs.rs::JobFence`; `plurxd/job_lease.rs`; `membership.rs` owner removal | Crash discards locals and can leave requests in flight; reboot gets a distinct boot identity; removal marker rejects bound authority and renames/expires owned domain rows | Monotonic local deadlines may revoke authority independently of wall-clock expiry; transport and consensus internals omitted |

The shared queue operations are carried by the
[replicated `QueueSql` adapter](../../crates/plurx-core/src/store/hiqlite_background_jobs.rs#L206)
(`impl QueueSql for HiqliteAuthStore`) and the
[SQLite `QueueSql` adapter](../../crates/plurx-core/src/store/sqlite/background_jobs.rs#L10)
(`impl QueueSql for SqliteStore`). `queue_sql` forwards encoded caller
`now_ms` unchanged: Hiqlite uses `execute_returning_map` for mutations and
`query_consistent_map` for authoritative reads; SQLite executes the prepared
statement through `with_conn`. Claim reconciliation read and later claim CAS
are separate calls, not one transaction. `queue_transaction` groups its
provided statements in a Hiqlite transaction or SQLite connection transaction.
The shared `claim_job` / `resolve_claim` / renewal / settlement methods and
installed SQL triggers supply their guards; the bridges do not replace those
caller timestamps with backend publication time. These adapters are reviewed
mapping dependencies, not extra modeled consensus algorithms.

Paths in this table are relative to `crates/plurx-core/src/` except explicitly
prefixed `plurxd/`, which means `crates/plurxd/src/`. The dependency inventory
must include shared queue validation, schema/resource triggers, binding schema,
worker/facade, both backend bridges and coordination helpers; naming only the
publication files would miss guards they depend on.

## Boundary comparisons preserve equality and sample location

| Predicate | Exact source comparison | Consequence at equality | Source anchor |
|---|---|---|---|
| Current queue/domain authority | stored expiry `> sample` | Rejected | [queue renewal:640](../../crates/plurx-core/src/store/background_jobs.rs#L640); [Hiqlite domain/bound checks:49](../../crates/plurx-core/src/store/hiqlite_publication.rs#L49) |
| Expired queued takeover | stored queue expiry `<= caller now_ms` | Eligible subject to all other guards | [claim:542](../../crates/plurx-core/src/store/background_jobs.rs#L542) |
| Reservation coverage | held expiry `>= job lease expiry` with same fence/key | Sufficient | [queue renewal:646](../../crates/plurx-core/src/store/background_jobs.rs#L646); [publication:61](../../crates/plurx-core/src/store/hiqlite_publication.rs#L61) |
| Reservation occupancy | held expiry `> caller now_ms` | No longer occupies a slot | [claim:562](../../crates/plurx-core/src/store/background_jobs.rs#L562) |
| Not-before | `not_before_ms <= caller now_ms` | Eligible | [claim:545](../../crates/plurx-core/src/store/background_jobs.rs#L545) |
| Dispatch/resolution window | `now < dispatched` or `now-dispatched >= 120000` | Expired/pruned at upper boundary | [resolve:3548](../../crates/plurx-core/src/store/background_jobs.rs#L3548) |
| Domain release | `min(stored expiry, caller now_ms)` | Does not extend lease | [SQLite:125](../../crates/plurx-core/src/store/sqlite/coordination.rs#L125); [Hiqlite:162](../../crates/plurx-core/src/store/hiqlite_coordination.rs#L162) |
| Successor | same resource/owner/fence, revision+1, successor expiry `> predecessor expiry` | Equal successor expiry invalid | [successor construction:52](../../crates/plurx-core/src/cluster/coordination.rs#L52); [Hiqlite validation:34](../../crates/plurx-core/src/store/hiqlite_publication.rs#L34) |
| SQLite replacement | additionally successor expiry `> backend sample` | Equal successor expiry invalid | [inside-transaction validation:1988](../../crates/plurx-core/src/store/sqlite/mod.rs#L1988) |
| Hiqlite acceptance | predecessor expiry `> backend sample` and successor expiry `> predecessor expiry` | Implies successor expiry `> sample`; no accepted-publication strength difference | [replacement validation:38](../../crates/plurx-core/src/store/hiqlite_publication.rs#L38); [predecessor UPDATE:49](../../crates/plurx-core/src/store/hiqlite_publication.rs#L49) |

Facade precheck, successor-computation and each backend submission retain
separate timestamps. SQLite's backend sample is at the transaction action;
Hiqlite's sampled request may wait before its transaction action. Clock
anomalies and delayed samples are allowed in safety. The accepted authority
claim is relative to that operation's sample; physical commit-time expiry
is not proved. Equality, before and after classes must remain visible in the
finite model; expired Booleans alone cannot express the reservation or
resolution-window boundaries.

## Retention uses installed statements and preserves binding evidence

The installed maintenance trigger is the later replacement in
`background_jobs_receipt_pressure.sql`, included after
`background_jobs_retention.sql` and installed by both migrations. These are
separate transitions; a declared Rust `MAX_RETAINED_JOBS` is not their source.

| Action | Exact production eligibility and effect | Finite abstraction obligation |
|---|---|---|
| `PruneAttempts` | Finished AND resolve_until<=now AND (at least16 newer finished attempts for same job OR global attempts>=39872 AND any newer attempt); oldest-first LIMIT128 | Reduce newer-finished threshold16→1 and pressure39872→2; compute both branches and finished/window/newer predicates from modeled rows; never a fixed-size FIFO |
| `CompactInternalReceipts` | Terminal, retain_identity=0, receipt expired; pressure alternative count>=15360, internal scope, no active job; LIMIT128 | Receipt/demand eligibility classes, not all waiter fields; compaction cannot erase active demand |
| `RetireTerminalJob` | Terminal AND (updated<=now-604800000 OR global jobs>=9000), no pending/awaiting-hydration waiter, no legacy awaiting-import fragment pin; LIMIT128 | Explicit eligibility classes before allowing deletion |
| `EnqueueRetirement` | On accepted enqueue, count>=10000; terminal, no waiter/legacy pin, excluding admitted job id; LIMIT128 | Mapped source operation outside finite admission Next; no claim that this branch is explored |
| Detail deletion | `background_jobs_schema.sql::background_job_retire_details` removes attempts/reservations before job deletion | Domain binding remains; absent job rejects matching bound publication |
| Domain-row deletion | `background_jobs_domain.sql::background_job_domain_lease_removed` | Deletes binding only with domain lease row; ordinary release does not |

Ordering, pagination and payload-specific legacy state are outside the pilot;
their eligibility is represented explicitly rather than claiming all terminal
jobs may disappear. Stable recovery preserves valid demand and disables new
compaction/removal/cancellation interference.

## Caller scope is narrower than the facade

| Production route | Classification |
|---|---|
| `plurxd/library_work.rs`: acquire, publisher, bind, `process_library_requests` | Selected bound route; bind refusal releases/yields before work |
| Worker scan/enrichment and targeted scan, `plurxd/state.rs` passed publisher | Bound only when reached from that worker |
| Direct artwork repair/retry, `state.rs` `provider:artwork` | Domain-only, outside S2's bound-route claim |
| Genre backfill, `state.rs` `provider:genres` | Domain-only |
| Transcode `manager/produce.rs` with publication_fence | Domain-only example, not a bindable library job kind |
| `scan/mod.rs`, `metadata/mod.rs`, `metadata/{book,local,genres}.rs` convenience wrappers | Explicitly unfenced; outside the selected model |
| `plurxd/background_jobs.rs::JobFence::publish_dv_title` → `store/dv_title_jobs.rs::publish` | Queue-fenced title JSON publication; outside the selected `insert_item_fenced` model |

A control that ignores binding must be able to accept an insertion after the
bound queue authority is invalidated. A control that bypasses bind-before-work
must expose the domain-only interval; that is a caller-premise failure, not a
claim that the backend universally requires a binding.

## Executable examples support correspondence; they do not prove refinement

The integrated `tests/store_contract/formal_publication.rs` contains these
named examples. Their existence is not a claim they have run; qualification
records live in the pilot status/evidence receipts after independent review.

| Example name (prefix `formal_publication_`) | Boundary exercised |
|---|---|
| `live_bound_insertion_commits_and_cancellation_orders_around_commit` | Actual insertion before/after cancellation; retained committed row |
| `binding_survives_independent_renewals_and_rejects_exact_stale_tokens` | Stable binding versus exact renewable tuples |
| `queue_expiry_with_live_domain_rejects_at_backend_sample` | Both-owner requirement with unequal expiries |
| `reservations_owner_removal_and_binding_lifetime_follow_code_semantics` | Resource/removal guards and retained binding |
| `resolved_claim_keeps_attempt_identity_through_insertion` | Replay/resolution identity through selected mutation |
| `hiqlite_statement_zero_missing_output_is_fence_rejected_and_rolled_back` | Actual pinned-vendor missing-output error and transaction rollback |
| `hiqlite_lost_acknowledgement_retains_commit_and_clears_facade` | Real commit with ambiguous acknowledgement; retained item and cleared facade |
| `hiqlite_allocator_conflict_retries_same_authority_fresh_candidate` | Actual allocator-conflict retry branch; four-submission authority contract |

Existing dual-owner regressions use `put_setting_fenced`; they remain useful
but cannot stand in for insertion conformance. The Hiqlite classifier's
substring `StmtIndex(0) does not have observable row output` is an explicit
**compatibility assumption**; the actual pinned vendor text also ends
`, or index out of bounds`. The allocated retry classifier matches
`item_identity_reuse` or `UNIQUE constraint failed: items.id`; the installed
schema's identity guard makes the latter branch unreachable in the integrated
allocator scenario, so the example exercises real identity reuse instead.
No artificial transport error is relabeled a retryable identity conflict.

## Trust and omissions remain explicit

One serialized durable Store, atomic rollback, correct statement-zero output
binding, finite model-checker/fingerprint behavior and the reviewed mapping
are assumptions. The pilot proves no Raft, quorum implementation, arbitrary
cluster size, physical exactly-once work, universal catalogue correctness,
resource deadlock freedom or production refinement. Existing tests and release
gates remain required. Recovery fairness and its narrower suffix are specified
in README and the model; finite safety exhaustion is never liveness evidence.

## Operator index — every transition has a source or abstraction class

| Model operators | Source correspondence / class |
|---|---|
| `InitFresh`, `InitRecovery`, `Next`, `ModelView`, `Symm` | Constructed reachable-shape seeds and explicit finite exploration machinery; not production operations |
| `Cancel`, `Remove`, `LoseResource`, `Takeover`, `Outage`, `Restart` | Queue cancellation; membership removal; reservation loss; environment takeover abstraction of expired `CLAIM_SQL` plus triggers; unreachable authority; crash/reboot transport abstraction |
| `SendRenew`, `ApplyRenew`, `DeliverRenew` | Caller queue heartbeat; atomic `RENEW_SQL`/renew trigger; worker adoption of returned exact token |
| `DomainChange`, `AcquireDomain` | Exact domain renewal/release and expired-lease acquisition in both coordination backends |
| `BindAttempt`, `RefusalRelease`, `RefusalSettle` | Binding transaction followed by distinct domain release and exact queue yield; caller not-before is now+5000 (model2) |
| `BypassBind` | Deliberate control violating the library worker's bind-before-work premise |
| `FacadePrecheck`, `ComputeSuccessor`, `Submit` | Facade precheck and independent successor sample; fresh Hiqlite sample per submission; SQLite sample is replaced by the transaction's own sample |
| `ApplyPublication`, `DeliverPublication` | Atomic domain renewal+insert or rollback, then separately delivered backend reply/facade outcome |
| `SendClaim`, `ReadClaim`, `ApplyClaim`, `DeliverClaim` | Claim dispatch with observed revision; initial read; CAS plus attempt/reservation triggers; optionally lost acknowledgement |
| `ReplayClaim`, `ResolveClaim` | Same dispatch/claim identity, local window checks, current stable tuple and retained-attempt lookup; current token revision can advance independently |
| `PrepareRetention` | Abstract maintenance-input classes and terminal settlement seed; flags explicitly summarize age, global pressure, demand, retain-identity, user scope and legacy pin |
| `PruneAttempts`, `CompactInternalReceipts`, `RetireTerminalJob` | Installed receipt-pressure maintenance statements and before-delete detail trigger |
| `RecoveryTick`, `RecoveryPoll`, `RecoveryRetry`, `RecoverySend`, `RecoveryApply`, `RecoveryAck` | Constructed running-job suffix combining expiry and not-before barriers, then worker polling/backoff and fresh authority dispatch/application/acknowledgement; concrete fairness premises, not a scheduler theorem |
| `DeliverOldReply` | Fair scheduling placeholder toggling only a flag; no reply payload or boot check, and no L1 old-reply nonadoption claim |

The selected model fixes installed allocator mode for the identity-conflict
branch. It does not model every input-validation failure, UUID syntax,
payload-specific queue adapter, retry-deadline class, priority, waiter table,
full maintenance pagination, real i64 boundary or duration ratio. Selected
seeds assume sufficient remaining attempt budget. Both backends accept under
the same sampled-time implications, while SQLite validates its successor
against its inside-transaction sample. Only Hiqlite retries the selected
identity-conflict submissions; generic lost replies never enter that branch.

`Restart` intentionally over-approximates pending old-boot continuations:
local held authority is cleared but an old request stage can still complete.
The fresh safety relation's `DeliverClaim` and `DeliverRenew` check payload
boot identity before adoption, and the Store rechecks durable authority. These
checks are not part of the recovery placeholder. This is not a statement that a reboot preserves runtime Rust futures. A delayed
backend request may commit after its initiating process has gone away; an
old facade result is not new-boot authority.

`Takeover` is an environment overapproximation for safety: unlike
`ApplyClaim`, it does not recheck demand or not-before before installing a
successor attempt. It can invalidate old authority more broadly than the
source claim path; it is not an exact claim-eligibility implementation or a
recovery action. The normal dispatch/CAS path retains those predicates.

Recovery Init has time0, a running old-boot claim expiring at2 and not-before2
or3. Fair time reaches the applicable barriers; a delay2 claim may occur at2,
whereas delay3 requires3. This constructed joint-barrier seed does not prove
the queued yielded/failed-retry state shapes or arbitrary larger deadlines.
`DeliverOldReply` has no payload, so the recovery result establishes only
eventual first fresh current claim under the declared fairness assumptions.

The integer queue duration1 cannot produce a forward expiry extension under
its strict live-renewal guard: sample+1 is at most the old expiry. Queue
revision/binding continuity and backward-clock updates are modeled; ordinary
forward expiry extension remains executable conformance evidence rather than
a claim of this finite exploration.

Every authority operation uses the same shared safety relation from all seeds.
Only retention preparation is selected by its maintenance seed. Publication
records its entire relevant pre-state for S2/S3; later cancellation never
retroactively invalidates a committed event. Exact event horizons and the
proof obligation for the state view are in README. These limits
are part of every result, including a completed baseline.

## Measured coverage split preserves the shared action set

The required safety checks now use two jobs/eight events (`baseline.cfg`)
and one job/eleven events from claim seeds (`lifecycle.cfg`), both with two
workers, two boot identities per worker and three claim identities. The
shared relation and all failure actions are identical. The former covers
seeded bound publication under contention; exhaustive fresh claim through
commit/delivery composition is checked only in the latter single-job scope.
The abandoned two-job ten-event run is incomplete cost evidence, kept outside
the required registry. README gives its observed counts and the precise
eleven-action path. A predeclared single-job ten-event fallback would cover
commit only and must be labeled explicitly if used; it is not yet selected.

## Subtitle completion construction correspondence

The 2026-10-26 source mapping is reconciled against `origin/main` at
`087e836d369eb2b8f94ec1d0454ea6c157612df9`. Text-caption admission is scoped to
its new job kinds. The selected library claim/binding/token/publication
predicates retain their base behavior; the new schema extensions do not alter
the mapped Hiqlite timeout or SQLite fenced-publication transaction. Text
translation, speech tasks and audio ordinals, caption leases, and content
read attestation are outside this finite model. The mapping notes identify
the changed source files without expanding the modeled guarantees.

This is authored construction correspondence. Final adversarial review and
current-source qualification remain pending; refreshing hashes does not
relabel historical model receipts or execute any runtime regression.

## Subtitle producer recovery correspondence

The independent final review on 2026-10-26 inspected the frozen completion
and retargeting delta atop `be9ee45ae`. Shared queue changes add producer-node
and preparation-incarnation checks only for `subtitle_translation_prepare`
and `subtitle_translation_range`; legacy payloads remain inspectable but
cannot acquire new authority. The selected library enqueue/claim, binding,
renewal, cancellation, publication and cleanup paths retain their behavior.

The [exact-shape migration recognizer](../../crates/plurx-core/src/store/subtitle_translation_schema.rs)
and [replacement trigger](../../crates/plurx-core/src/store/subtitle_translation_retarget.sql)
were also inspected because migration installs effects on the shared job
table. They recognize the complete legacy caption shape, replace its trigger
transactionally, preserve domain data and refuse unknown shapes. Queue
updates select caption kinds for one operation; they do not cancel the
selected library jobs. New mapping dependencies pin these two boundaries.
SQLite v117, Hiqlite extension installation and added store-contract module
wiring leave the selected publication transaction and timeout unchanged.

This source review supersedes the earlier construction-only correspondence
status for these changes. It does not extend the finite model to caption
completion, producer recovery, remote-worker liveness or admission fairness.
Model/configuration bytes, fairness premises and historical receipts remain
unchanged. Offline mapping validation checks the reviewed inputs and anchors;
it is not Rust test execution, TLC qualification or a refinement proof.

The later 2026-10-26 formatter pass only moved the unchanged feature-gated
`subtitle_translation_upgrade` test-module declaration into lexical order.
Its visibility, feature condition and source path are unchanged. The one
inspected `store_contract.rs` mapping hash was rebound; this adds no model
semantics or execution evidence.

## Moving-playback continuity correspondence

The independent bounded review on 2026-10-26 inspected the frozen delta atop
`352137f764d8cf87d530352222d5948a77275d4f`. Hiqlite changes only the
recognized caption extension chain; SQLite adds caption migration118. The
selected library transaction, publication and timeout paths are unchanged.

The exact original0/retarget1/continuity2 recognizer refuses unknown shapes.
The new [continuity triggers](../../crates/plurx-core/src/store/subtitle_translation_continuity.sql)
capture and retire grants only for `subtitle_translation_range`; demand
revocation deletes only matching captured caption grant rows. Comparing
the replacement mutation trigger with the previous retarget trigger confirms
that its shared-job effects retain their caption-kind/operation scope. New
dependency anchors pin these isolation boundaries. The expiry/UUID-reuse
review finding was fixed by atomic demand-trigger grant withdrawal.

The selected library enqueue, claim, renewal, binding, settlement and cleanup
semantics remain unchanged. This source correspondence does not extend the
finite model to caption continuity, playback deadlines or remote-worker
liveness. Model/configuration bytes, fairness premises and historical
receipts retain their identities. Offline mapping validation is distinct
from runtime tests, TLC qualification and a refinement proof.
