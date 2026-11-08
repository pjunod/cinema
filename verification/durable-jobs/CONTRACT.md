# Durable-job contract — connect finite authority transitions to source

**Status:** original package qualified; all 18 updated configurations checked
locally; CI and promotion qualification separate ·
**Initial checked source:** `663b3aa548` · **Latest scoped source review:**
`460d8e6d3` plus the pinned scan/bootstrap corrections (2026-11-04 audits below) · **Scope:** bound `library_scan` and
`metadata_refresh` workers calling `insert_item_fenced`.

Companion to [README.md](README.md), which gives the reproducible commands,
and the [pilot proposal](../../docs/cluster/FORMAL-VERIFICATION-PILOT-PROPOSAL.md),
which owns acceptance. The transition correspondence below is a **reviewed
source mapping**, not an automatic refinement proof. Source hashes in
`mapping.json` detect drift in the reviewed dependency set; they cannot prove
that set complete or that the abstractions are correct.


## Pipeline v2 request handler (T3) — scoped correspondence review on 2026-11-08

Claude (`claude-opus-5-5`) reviewed PR #1406
(`claude/pipeline-v2-m5-t3-handler`) against its parent T2. This is an
author review awaiting an independent check, not execution evidence. T3
adds typed path outcomes, `request_key` replay, `wait_ms` and
`expected_files` to `POST /api/v1/scan`.

| T3 behaviour (source) | Model action or property | Still holds? | Model change |
|---|---|---|---|
| `request_scan_v2`: admission through `enqueue_library_work_v2` (reviewed with T1); a colliding `request_key` answers with the existing request; an advisory `queue_position:<n>` write when the request joins a running targeted job ([`library_work.rs`](../../crates/plurxd/src/library_work.rs)) | None in the worker models: each starts with one selected durable request already admitted | Yes: admission creates or joins demand through the unchanged `ENQUEUE_SQL`; the replay read and the advisory write change no queue, domain, binding or publication state | None |
| `await_scan_request`: the HTTP caller waits on a per-request local `Notify` with a bounded re-read of the durable record, then forgets the signal | None; caller-side waiting outside the worker | Yes: the worker's completion transaction and its waiter settlement are unchanged; the durable record stays the authority | None |
| A target under a root that cannot be read becomes `TargetError::Unreadable`, recorded as the request's failed result; a target outside every root stays `OutsideRoots` ([`library_work.rs`](../../crates/plurxd/src/library_work.rs), [`scan/mod.rs`](../../crates/plurx-core/src/scan/mod.rs), new `scan/containment.rs`) | Durable jobs `ApplyPublication` (none happens); operation progress `CompleteSend`/`StoreApply` (`complete_library_work` records the per-request result); worker refinement `CompleteSend` | Yes: the scan publishes nothing for that target and completes the request through the same fenced `complete_library_work` path that already recorded `OutsideRoots` failures | None |
| `ScanAdmission` re-export ([`state.rs`](../../crates/plurxd/src/state.rs)) | None | Yes | None |

Whole-file pins only are refreshed; models, configurations and properties are
unchanged.

## Pipeline v2 targeted job (T2) — scoped correspondence review on 2026-11-08

Claude (`claude-opus-5-5`, the pipeline-v2 builder) reviewed PR #1405
(`claude/pipeline-v2-m5-t2-targeted-job`, T2 of `effort/pipeline-v2`) against
its parent T1 `dd22ccd21` and main `1d11f0cdf`. This is an author review
awaiting an independent check; it is not execution evidence or a refinement
theorem. Contract v2 §5.1 adds a targeted library request as its own job:
kind `library_scan`, generation `library-targeted-v1`, dedupe
`library:targeted:{id}`, priority 3. It claims with a probe-only admission
and is serialised against a running full scan of its library.

| T2 behaviour (source) | Model action or property | Still holds? | Model change |
|---|---|---|---|
| Targeted enqueue: new generation, priority 3, own dedupe key, optional sidecar in the command body ([`background_jobs_library.rs`](../../crates/plurx-core/src/store/background_jobs_library.rs)) | Durable jobs `SendClaim`…`DeliverClaim`, S1–S8; `ENQUEUE_SQL` and `CLAIM_SQL` unchanged. The model does not represent kind, generation or priority | Yes: claim CAS, attempt and reservation triggers, binding, renewal and settlement are the existing ones for `library_scan` | None |
| `claim_library` accepts the targeted generation beside `library-work-v1`, through main's dispatch-turn readiness ([`background_jobs.rs`](../../crates/plurxd/src/background_jobs.rs)) | Operation progress `ProbeReturn`, `Admission`, `ClaimSend`; worker refinement `Admission`/`ClaimSend` (eligibility over-approximated) | Yes: same `claim_with_resolution`, `ActiveBackgroundJob` and shared library driver; an unknown generation is still skipped | None |
| Probe-only admission: no heavy permit and no CPU/GPU bundle; `fragment_worker_idle` is always true for it, so heavy-lane occupancy never raises pressure ([`construct.rs`](../../crates/plurxd/src/transcode/manager/construct.rs)) | Operation progress `Admission`, `DropAdmission`, `AdmissionUntilJoined`, `AttemptCleanup`; durable jobs `PauseSource`/`ResumeSource`; worker refinement boundary `select.admission_pressure` | Yes: the admission is still one owned object, held in the environment's `Arc` through `JoinWork` and dropped after release. Removing heavy-lane pressure only removes optional pause and pressure schedules; a subset of the modeled schedules keeps S8 and the safety results. Foreground source pressure still reaches the Linux source driver. Admission availability was never modeled | None |
| A claim the Store refuses is followed by a read of the existing source-reader observation and an advisory `waiting_on` write (`full_scan:<library>` or `heavy_lane:<kind>`), only when the reason changes ([`background_jobs.rs`](../../crates/plurxd/src/background_jobs.rs) `targeted_claim_blocker`) | No modeled transition. Claim eligibility contention is outside all three models (operation progress already records the extra eligibility states as unmodeled; worker refinement over-approximates the production same-root claim refusal) | Yes: the refused job holds no claim, domain or binding; the probe-only admission is dropped at `continue`. The reads and the sidecar write change no queue, domain, binding or publication state, and no lock is held across them | None. The refusal is the existing `CLAIM_SQL` rule that keeps a source's second reader for live demand |
| Fence refusal of a targeted job: advisory `waiting_on = full_scan:<library>`, yield with not-before `min(driver, now+250 ms)`; acquired fence clears `waiting_on`; a non-targeted release wakes the local library loop ([`env.rs`](../../crates/plurxd/src/library_work/env.rs)) | Durable jobs `AcquireDomain`, `RefusalRelease`, `RefusalSettle` (`notBefore = sample + YieldMs`); operation progress `PeerAcquire`, `CallerReturn` (domain refused), `Wake`; worker refinement `DomainSend`/`DomainApply`, `Wake` | Yes: the refusal still releases nothing it does not own and yields with a positive not-before. `YieldMs` (2) is an abstract positive delay; the model keeps no duration ratio (its `LeaseMs` is 1). The concrete 5 s and 250 ms both map to it. The local wake only shortens polling, a fairness premise | None; the bind-refusal row now names both delays |
| Targeted scan with `expected_files`: forced probe of the named files and per-file placed reports written to the sidecar ([`library_work.rs`](../../crates/plurxd/src/library_work.rs), [`scan/mod.rs`](../../crates/plurx-core/src/scan/mod.rs) `scan_path_targeted`) | Durable jobs `BindAttempt` → `FacadePrecheck`/`Submit`/`ApplyPublication`; operation progress `ScanStart`…`ScanFinish`, `ItemSend`, `CompleteSend`; worker refinement `ScanStart`, `ItemSend` | Yes: it publishes through the same bound `PublicationStore` and `insert_item_fenced` after binding (bind-before-work unchanged). The report write is advisory sidecar state | None |
| Local completion signals for `wait_ms` and the targeted scan returning placed reports ([`state.rs`](../../crates/plurxd/src/state.rs)) | None; caller-side waiting outside the worker | Yes: node-local `Notify`; the durable request record stays the authority | None |

**Isolation of the refusal.** While another worker runs the full scan of the
same library, the targeted job is never claimed. The refusing predicate is
`CLAIM_SQL`'s "a second source reader is reserved for live demand" clause.
Another non-live holder of a conflicting `source_io` reservation refuses a
background claim, and a running scan always holds its library's reader.
`library_scan` also requires `provider:tmdb` and `provider:anilist`, but each
has two reservation slots (`background_job_claim_effects`), so a single
running scan leaves one free and they do not refuse it. The regression
asserts that the refused claim coincides with the full scan's
`source_io` reservation (`source_io_holders`). The per-library mutation
fence remains the serialiser of record whenever the reader does not
conflict.

**Limits.** No model or configuration byte changes, and no TLC run is
claimed for these sources. Only whole-file pins are refreshed. Progress for
a targeted request behind a running scan rests on that scan finishing, the
same premise the models state for a domain peer; it is not proved.

## Pipeline v2 targeted-scan sidecar — scoped author audit on 2026-11-08

Claude (`claude-opus-5-5`, the pipeline-v2 builder) compared PR #1404 (task T1
of `effort/pipeline-v2`) with main `1d11f0cdf`. This is authored
correspondence, not independent review or an execution receipt.

| Reviewed changed dependency | Correspondence and limit |
|---|---|
| `background_jobs.rs` | Six new trait methods delegate to the [sidecar module](../../crates/plurx-core/src/store/scan_request_v2.rs); no existing method body changes |
| `background_jobs_library.rs` | `enqueue` routes through `enqueue_with`, which validates an optional contract-v2 seed, refuses it for anything but `Targeted` input and adds it to the enqueue command body. `ENQUEUE_SQL` and the selected claim, binding, renewal, cancellation, settlement and `insert_item_fenced` paths are unchanged |
| `hiqlite.rs`, `sqlite/mod.rs` and [the v125 SQL](../../crates/plurx-core/src/store/scan_request_v2.sql) | The sidecar installs as an independent extension (SQLite v125; Hiqlite after dispatch turns). Its admission trigger writes only when the enqueue command carries a `scan_request_v2` object, so full scans and metadata refresh are unaffected; a `(key_id, request_key)` collision aborts only that targeted admission. The census pins its two committed-authority reads; the unannotated ceiling is unchanged |
| `store_contract.rs` | A test module, the six methods in its inventory, a replicated sidecar check and the sidecar import-plan name. The [shared fixtures](../../crates/plurx-core/tests/support/source_extensions_fixture.rs) validate and remove the exact empty v125 sidecar before main's v122–v124 source rewind, and refuse drift or retained rows before mutation |

Only whole-file pins change. Anchors, models, configurations, properties and
qualification identities keep their bytes; fresh mapping and smoke results are
separate execution evidence.

## Source custody replay on 2026-11-07

Against `cc2ca1338`, scheduler upkeep now rechecks settlement proof and
the exact owner's current authority inside the cleanup transaction. This
preserves newly registered readers through activation and lets a renewed
background job retain its pending registration. The resource-mapping and
selected claim, renewal, binding and fenced-publication anchors keep their
existing transitions. Physical source activation and settlement are runtime
obligations outside this finite model.

## Queue observation source review on 2026-11-07

Compared with `4ddbf1229`, the shared queue adds read-only counts of recent
worker attempts and waiting manual requests. The SQLite and Hiqlite metrics
queries count expired source custody, and the daemon caches that count for
Activity and its sampler warning. These reads have no effect on claim,
renewal, physical settlement, or any modeled transition. The focused queue
regressions cover the observable rates and warnings.

## Scan stack and canonical bootstrap — scoped source audit on 2026-11-04

Codex agent `/root/recovery_design_audit` reviewed the frozen working-tree
corrections against `460d8e6d3ec494f2335ed4bbdfa6601f54bfa380`. The two previous
source fingerprints match that commit; all retained anchors remain unique.
This is an authored correspondence audit, not independent Astra review or an
execution receipt. Three existing rows change: two for the library environment
and one for the Hiqlite queue adapter. No dependency or modeled action is added.

The [library environment](../../crates/plurxd/src/library_work/env.rs) extracts
the concrete `probe_work` factory and heap-pins `process_library_requests`
before passing it through the probe-input and admission scopes. It remains
directly awaited in the same task, so task-local budget/cancellation/pause
bindings still cover the producer. The outer owned work future retains the
publisher and domain reference. Physical admission, `JoinWork`, completed-work
drop, domain release, queue retirement and environment field drop order retain
their prior implementations. Heap allocation changes native stack demand,
not the selected worker authority or cleanup transitions.

The [Hiqlite adapter](../../crates/plurx-core/src/store/hiqlite_background_jobs.rs)
recognizes version 4 in both managed-bootstrap success branches. Its existing
`managed_bootstrap_layout` first checks the canonical DDL shape through
`subtitle_managed_schema::bootstrap_version` and the matching singleton marker.
The repair therefore preserves an already canonical version-4 resource view;
it does not accept an arbitrary version marker, replace that view, alter SQL
guards, or change claim/renewal/bind/publication protocol. Membership and node
identity checks remain outside and unchanged. Startup/schema convergence is
not a transition of this worker model.

The PR records separate runtime evidence: the actual scoped future shrinks
from 16104 to 64 bytes, the driver future is 656 bytes, and an empty-library
scan completes on the ordinary daemon worker stack with zero scan errors.
That limited scan does not qualify media parsing or the full UI. The real
one-voter bootstrap/reopen and identity-drift rejection regression is separate
from TLC. Future-state size does not prove the whole native poll-stack bound,
and no old-base comparison establishes when the stack problem was introduced.
Model/configuration bytes, bounds, properties, fairness, source-custody proof
obligations and historical qualification identities remain unchanged.

## Verified host-reboot recovery — scoped source audit on 2026-11-04

Codex agent `/root/recovery_design_audit` compared source candidate
`88fa37663` in [PR #1378](http://forge.lan:3000/noirr/plurx/pulls/1378) with
`edf6d08faf549554b9892dee582406ff79af1c97`. All five affected previous
whole-file fingerprints match that base. This is an authored correspondence
audit: the agent contributed implementation and tests, so it is not an
independent review. The PR owns subsequent Astra review and execution.

The [queue Store](../../crates/plurx-core/src/store/background_jobs.rs)
adds an exact physical-custody lookup and delegation. Its claim, renewal,
binding, cancellation, settlement and publication SQL is unchanged.
[Resource helpers](../../crates/plurx-core/src/store/background_jobs_resources.rs)
expose the existing canonical owner key without changing its body and add
a consistent exact-owner read. Neither equality nor absence is a new proof
of physical termination or authority to publish. The existing ledger's
claim veto and actual-reservation publication guards remain in force.

[Daemon cleanup](../../crates/plurxd/src/background_jobs.rs) passes its exact
Store to journal cleanup. [State](../../crates/plurxd/src/state.rs) retries
verified recovery before ordinary settled-custody replay;
[startup](../../crates/plurxd/src/main.rs) configures the exact Store/backend
and attempts recovery before producers. Selected worker acquisition, binding,
publication, heartbeat retirement and drop order retain their prior bodies.
These new filesystem/Store calls are additional external waits, not modeled
transitions or an eventual-recovery guarantee.

The mapping refreshes 17 existing rows and adds four explicit dependency pins
for startup, [custody integration](../../crates/plurxd/src/source_custody.rs),
[recovery](../../crates/plurxd/src/source_custody/recovery.rs), and the
[host helper](../../scripts/source-custody-recovery). Those pins expose future
drift; they do not map physical reboot proof into `DurableJobs`. Same-machine
boot identity, descendant termination, fsync durability, aborted-activation
reconciliation, exact-Store isolation and safe legacy challenge preparation
remain runtime obligations in the
[recovery plan](../../docs/server/JOB-QUEUE-ROOT-CAUSE-AND-FIX.md). Same-boot
or unproved ownership may still block indefinitely. No model result proves
that source custody becomes available or that a real scan completes.

**Identity-source corrections (2026-11-04):** subsequent recovery-module
dependency refreshes require both actual-host inputs, read-only bound at
`/run/plurx-host-machine-id` and `/run/plurx-host-boot-id`, for native services
and containers alike. Missing container markers never establish native
execution; neither image-local `/etc/machine-id` nor a namespace's `/proc`
boot UUID is a fallback or physical-stop proof. Machine identity must be
nonzero, and host boot identity a canonical nonnil UUID. Missing or invalid
input retains custody without automatic recovery. These external proof
prerequisites supply no additional modeled transition or guarantee.

All previous exact anchors remain unique. Model, configuration, bounds,
properties, fairness, toolchain and historical qualification identities are
unchanged. Fresh static/smoke receipts remain separate from this audit and
from runtime crash/replay evidence; no new refinement theorem is claimed.

## Source pause — finite correspondence on 2026-11-03

The source audit compares each drifted file with the Git version whose bytes
match its previously admitted whole-file fingerprint. Current runtime source
is `594210a783986d4da5ec7184f0895433ffe98b90`. This is an author source review;
previous independent reviews and execution receipts retain their historical
source/model identities. New static and bounded executions are separate
receipts, not renewal of the earlier full qualification.

The queue changes are substantive: `RENEW_SQL` and `CLAIM_RESOURCE_SNAPSHOT`
accept an exact paused source owner when its physical source reservation is
absent. Claim admission additionally consults source occupancy and foreground
permission. A hash-only refresh would miss this difference. The model now
records a stable job/node/boot/claim/fence pause identity and one selected
source key. `PauseSource` removes that key's reservation; `RenewCover` permits
its exact paused substitute only for renewal/replay/resolution; `ResumeSource`
reinstalls it under the current live owner and slot guard. Revisions may
advance while stable ownership remains. A stale fence/boot/claim cannot
substitute for the current paused owner. The record may retain obsolete
identity diagnostics; exact matching makes them inert after takeover or
settlement, rather than modeling the source cleanup trigger itself.

The binding implementation, publication facade, both backend publication
implementations and existing queue schema triggers remain byte-identical to
the admitted pins. Their actual-reservation `Cover` guards stay unchanged in
the model. S8 asserts no current paused source reservation. The bounded
witness composes pause, successful heartbeat delivery and replay, exact
reacquisition, then an actual insertion commit; its state invariants also
check publication authority. It uses a declared action subset of shared
`Next`, while the added positive safety configuration uses shared
`SafetyNext` without a schedule filter. Existing controls, horizons and
properties remain; positive safety configurations additionally check S8.

There is at most one pause/reacquisition pair per execution. The extra
source-pressure claim/resume veto is external; omitting that veto permits
more safety schedules, never proves admission availability. Physical
stop/join verification, custody settlement, source-byte service, indefinite
or repeated pressure and paused-worker liveness are not established. This
model supplies no permission to perform a physical read while paused.

| Reviewed changed dependency | Correspondence and limit |
|---|---|
| `background_jobs.rs`, `background_jobs_resources.rs` and [foreground SQL](../../crates/plurx-core/src/store/background_jobs_foreground.sql) | Exact paused-owner branch and source reservation removal/reacquisition mapped above; source checkpoint data and physical custody remain outside model state |
| `hiqlite.rs`, `sqlite/mod.rs`, `subtitle_managed_schema.rs` and [managed-v4 resource view](../../crates/plurx-core/src/store/subtitle_managed_resources_v4.sql) | Install the source schema and managed-v4 classification. `library_scan`/`metadata_refresh` still require source and provider rows; prepared-artifact exclusions do not reclassify them. Online migration correctness remains ordinary runtime evidence |
| `plurxd/background_jobs.rs`, `library_work.rs`, `library_work/env.rs`, `state.rs` | Source registration, budgets, owned probe input and settlement replay surround the existing library route. Bind-before-work and selected publication call sites remain; local source-driver progress/cleanup is outside this finite authority model |
| `transcode/manager/produce.rs` | Source budgeting and offline child ownership change the previously classified domain-only/out-of-scope route; no new bound library publication caller is admitted |
| `store_contract/formal_publication.rs` | Only the `Arc` import gains its Hiqlite-test condition; the eight selected conformance test bodies are unchanged |

The mapping refreshes 43 whole-file rows across 11 inspected files and adds
foreground-schema and managed-v4-view dependency pins. The historical review
§2 renewal citation is retained but its exact anchor now includes both the
real reservation predicate and exact paused alternative. This is not anchor
removal or a claim that the new physical source subsystem was proved.

**Observed checks (2026-11-03):** pinned Temurin 21.0.12+8 and TLC 1.7.4,
model SHA-256
`9ffa3459040c8b076af85370712a11f1c932ed204be3c6e57142142578fda2fe`,
mapping SHA-256
`d7680ca828c88fce963ee13c0c57f345a51b81bd9c2fea140607c03db07c31ed`.
The static `check-mapping --package durable-jobs --affected-base bd213c819`
reported no source drift. Single-configuration runs completed for
`source-pause.cfg` (8.069 s, exhaustive finite safety),
`witness-source-pause.cfg` (1.302 s, the named witness with 11 trace states),
and the existing `smoke.cfg` (7.719 s, exhaustive finite safety). The
`baseline.cfg` run completed in 688.180 s with 257,837,060 generated states,
31,710,453 distinct states and zero queued; `lifecycle.cfg` completed in
310.809 s with 84,457,632 generated states, 11,222,402 distinct states and
zero queued. The other 13 controls, recovery and witness configurations
returned their expected outcomes in 27.824 s combined. Each registered
configuration ran once, with no failures or retries. The existing
registry-profile unit assertion also passed.

All 18 per-configuration receipts bind identical model, mapping, runner and
input hashes, as checked by the task's `configuration-receipts.json` index.
The five positive/new-case receipt IDs are `20261103T191359.798063Z`,
`20261103T191401.511107Z`, `20261103T191425.267863Z`,
`20261103T191553.163147Z` and `20261103T191552.079302Z`; the index identifies
all remaining individual receipts. Each receipt retains its actual
single-configuration qualification label. This is local coverage of all
18 registered configurations on the exact recorded dirty source/model
snapshot; it does not establish authenticated CI custody or replace the
promotion gate. All 18 task-owned checker metadirs were removed after their
runs; receipts, raw logs, traces and hash/config evidence remain (about
0.76 MiB total), with the pinned toolchain cache retained separately.

## Local main integration — source correspondence on 2026-10-29

The repair source `59c648db` is combined locally with main `5ac9d8f25`.
Both parent audits below retain their historical source identities. Incoming
Cargo.lock and `main.rs` pins follow the reviewed macOS runtime update and
bounded test-only fork stress. The merged Store and caption fixtures retain
both parents' closed shape, marker, retained-data and atomic refusal checks,
including schema 121 with managed v3. Their live whole-file hashes follow the
resolved source; selected formal-publication and library-worker anchors remain.

This reconciliation changes no model, configuration, property, fairness
premise, validator or historical qualification receipt and supplies no new
runtime or TLC evidence. The frozen `59c648db` measurement and this integration
candidate are separate source identities; neither substitutes for the other's
required validation.

## Test-only schema expectation — scoped source review on 2026-10-29

Against incoming main `10f45ae5f2619ab3e807b5576bf6ffb39ade6e86`, the
repair changes only the upgraded-version expectation in
[the translation schema tests](../../crates/plurx-core/src/store/subtitle_translation_schema.rs)
from 119 to 120. The v115 starting fixture, retained handoff assertions and
production shape recognizer and upgrade guards are unchanged. One whole-file
row pin follows those inspected bytes; its exact anchor remains unique.

Codex agent `coverage_review` inspected the source and authored this mapping
refresh; parent and independent Codex diagnose_core_failures metadata review found no issues. Selected library-worker
transitions, model/configuration/toolchain bytes, bounds, properties, scope
and historical source receipts remain unchanged. This review ran no builds,
tests or TLC and supplies no new execution or qualification evidence.

## Combined-source integration — scoped author audit on 2026-10-29

The repair is combined with main `66a33d723096ee931ce2e05e860f51f8f3671513`.
Retain its reviewed resource-admission v2 schema and mapping inputs. The
historical rewind now validates schema 119 with managed v1, or schema 120
with managed v2, before any mutation; crossed pairs are refused atomically.
The conformance parent retains the selected `formal_publication` module and
combines the independently reviewed strict caption/continuity teardown with
main's exact managed shape/data planner. Relative to this main, `state.rs`
adds only the test-module declaration; mapped production bodies are unchanged.

Only affected whole-file pins follow the combined source; anchors, models,
configurations, properties, bounds and historical source receipts retain their
identities. Independent Codex agent `coverage_review` reviewed the merged source pins, both parents' anchors and the combined ownership census, finding no blockers. That source review ran no builds, tests or TLC. This is a
source correspondence audit and supplies no new runtime or TLC qualification.

## Unit-coverage repair — scoped author audit on 2026-10-28

Codex agent `/root/diagnose_core_failures` compared immutable base
`24d1bad72bed328b6de9a568a3537f375f9c206f` with runtime/test source
`d03c8abdf17dea7147e112a32fec274834151fca`. The base file hashes match the
previous pins. This agent authored the core fixture repairs; this audit is
source-author correspondence, not independent review of that implementation.
Independent Codex agent `coverage_review` reviewed the implementation diff and this metadata disposition on 2026-10-28, finding no blockers. That review ran no builds, tests or TLC checks and supplies no new qualification claim.

[State](../../crates/plurxd/src/state.rs) adds only a `cfg(test)` backfill
module declaration; the five mapped production callers are byte-identical.
The [Store parent](../../crates/plurx-core/tests/store_contract.rs) registers
four existing caption methods, corrects the method inventory, and validates
exact empty caption/handoff extensions before historical fixture teardown.
Its `formal_publication` registration and selected examples are unchanged.
The selected library claim, binding, renewal, cancellation, publication and
cleanup transitions retain their existing correspondence. Caption methods
and historical schema teardown remain outside this finite model.

Six whole-file row pins follow the inspected source; exact anchors remain
unique. Model/configuration/toolchain bytes, bounds, properties, scope,
validators and historical source-anchor snapshots are unchanged. This audit
supplies no new runtime or TLC execution, refinement theorem, liveness,
physical acceptance or qualification. Historical receipts retain their
recorded source identities; fresh affected execution is separate evidence.

## macOS parser runtime — scoped lock refresh on 2026-10-29

Wasmtime 50.0.0 includes the [upstream Mach receiver fix](https://github.com/bytecodealliance/wasmtime/pull/14414).
The macOS-only runtime and required Cranelift dependencies change; native traps,
epoch deadlines, memory/stack limits and import restrictions remain. Tokio
1.53.1 and Hiqlite 0.14.0 remain pinned. Refresh only the live Cargo.lock
whole-file hash. Selected library-worker behavior, historical anchors, models,
configurations, toolchain and qualification identities are unchanged. This
source correspondence adds no formal execution or progress claim.

## Current validation fixtures — scoped refresh on 2026-10-29

Scoped review on 2026-10-29 of conformance parent registration: the exact Store inventory records four existing text-caption methods with new typed dispatch/absence/refusal coverage on all backends. Formal-publication module registration and selected library positive examples are unchanged. Positive caption publication on Hiqlite is not claimed. Refresh only live M2 parent correspondence; historical anchors, model/configuration and qualification identities remain unchanged; source review is not execution evidence.

The final replicated fixture review also checks the exact current tail and marker
inventories before one child-first mutation transaction. Its refusal matrix and
ordinary-bootstrap round trip are runtime checks, separate from this correspondence
record. The selected formal-publication examples and historical identities stay
unchanged.

## Historical schema fixtures — scoped refresh on 2026-10-29

Compared with `d5cd473536`, the caption schema source changes only the
[v115 migration test](../../crates/plurx-core/src/store/subtitle_translation_schema.rs):
it expects the current composed schema and verifies the managed v2 resources.
The production recognizer and migration SQL remain unchanged. Refresh its one
whole-file pin while retaining the exact anchor and historical source anchors.

The [SQLite fixture rewind](../../crates/plurx-core/tests/support/queue_fixture.rs)
now validates the exact empty v120 text/caption tail, continuity layout and
managed v2 marker before planning any mutation. Unknown objects, marker drift
and retained work refuse without changing DDL, versions or data. Historical
SQLite layouts retain their legacy translation witness; the published
replicated fixture witnesses remain separate. This closes the SQLite v115-only
limitation recorded in the earlier scoped review, without claiming a general
downgrade or complete replicated-schema qualification.

The consistent-read census records existing committed-authority reads and keeps
its unannotated ceiling at 237. No selected library claim, publication,
reservation, lease or progress behavior changes. Models, configurations,
properties and qualification identities retain their bytes; fresh mapping and
smoke results are separate execution evidence.

## Managed-v2 fixture follow-through — scoped refresh on 2026-10-28

The test-only follow-through on `48e2acc2a` restores the current managed marker
as v2 in the owned replicated refusal fixtures. Both backend rewind helpers use
the canonical published v1/v2 DDL with a matching marker and reject populated,
partial or drifted managed extensions before mutation. Historical v1 authority
remains exact; a v1 marker cannot certify the v2 view. The v120 downgrade
inventory entry names the new managed marker definition. At that historical reviewed snapshot, the separate baseline
handoff rewind supported v115 only and its v116–v119 inventory gap remained;
these focused managed-fixture checks do not qualify a complete current-schema
historical downgrade.

Only the whole-file pin for the conformance parent module changes. Its
`formal_publication` registration remains unique and byte-identical, as does the
selected conformance module. No production queue, reservation, lease, binding or
publication transition changes. Model/configuration/property/toolchain inputs
remain unchanged; this source review supplies no new execution qualification.

## Artifact-only text translation — scoped source review on 2026-10-28

Managed v3 / SQLite v121 exempts only the new closed
`subtitle_text_translate_artifact` job kind. Its worker reads pinned, bounded,
materialized VTT bytes, verifies the original input SHA, and retains source,
model, configuration, administrative and publication authority checks. A miss
fails explicitly; Retry reacquires input before enqueue. Legacy
`subtitle_text_translate` remains source-fenced because old workers can extract
media. Old candidate-kind filters cannot dispatch the new kind; queued and
running legacy jobs are not rewritten. The upgrade requires exact v2 DDL and
marker, commits atomically, and preserves existing owners and reservations.

The selected `library_scan` / `metadata_refresh` source-domain and provider
rows, claim identity, renewal, binding, publication and settlement remain
unchanged, as do the two-reader and one-speculative-reader limits. The v3 view
and upgrade guards are mapped correspondence inputs. Caption inference,
rolling upgrades and input acquisition remain outside this finite model;
model/configuration/property/bound/toolchain bytes are unchanged. This source
review supplies no new runtime, formal-execution or qualification evidence.

## Subtitle resource admission — scoped refresh on 2026-10-28

Codex reviewed `c3d764a01` against `f2120bd9a`. The shared claim SQL adds
qualified progressive caption preparation/range demand to the reserved live
source-reader slot. A new claim requires the current operation, source,
configuration and unexpired administrative demand; a held range can retain its
exact claim-fence continuity grant while that demand remains valid. These
branches require caption job and waiter kinds. Ordinary `library_scan` and
`metadata_refresh` callers do not gain that authority. The existing two-reader
ceiling and one-speculative-reader ceiling remain unchanged.

The managed v2 resource view exempts artifact-only text model setup from
`source_io`; both selected library kinds retain their existing source-domain
and provider rows. SQLite v120 validates the exact v1 predecessor inside its
migration transaction. The replicated installer checks the predecessor marker
and exact view in one transaction, accepts a verified v2 winner after ambiguous
acknowledgement, and preserves that view during bootstrap. Existing queue
owners, leases, attempts and reservations are retained. Mapping now pins this
active view, its historical predecessor and the shape/transaction guards;
valid installed schema remains a premise, not a modeled migration theorem.

The changed queue trait/token/adapter, renewal, expiry, resource-currentness,
fenced SQLite transaction, replicated timeout and retention anchors remain
unique and unchanged. Selected library claim identity, binding, publication,
renewal and settlement authority is unchanged. Nineteen whole-file pins are
refreshed after this review; model, configuration, property, bound and toolchain
bytes retain their identities. Caption fairness, online upgrade correctness and
physical playback are outside this model. Runtime regressions for model
contention, upgrade preservation and demand-bound caption admission are separate
source evidence; this mapping refresh claims no new execution or qualification.

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
## FEL qualification review fixes — scoped refresh on 2026-10-26 (2)

The PR #1290 review delta changes two mapped files. In
[dv_title_jobs.rs](../../crates/plurx-core/src/store/dv_title_jobs.rs) the
re-qualification request now selects the title demand only when no
`dv_title_scan` job for that file is queued, running or cancelling, so it can
no longer rotate the demand under (and cancel) an in-flight attempt; it still
calls the existing reviewed `bind` command otherwise. The verdict counts walk
`dv_title_jobs_demand` (one row per DV title, `file_id` UNIQUE) and read each
`files` row by key instead of scanning the catalogue; a read. The store
contract gains one assertion block in the FEL example (the no-op while a job
runs). No queue, waiter, fence, lease, reservation or publication transition
changed and every anchor string is unchanged. **Model decision:** unchanged;
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
| Replay / resolution | `background_jobs.rs::claim_job` and `resolve_claim` | Same identity returns existing current token only with matching node/boot, running state, live expiry and actual resource coverage or the exact paused source substitute; absent history gives expired/pruned, never a new attempt | Resolve without fence additionally checks dispatch window; per-operation caller `now_ms` is retained |
| Queue renewal | `RENEW_SQL`, `background_jobs_schema.sql::background_job_renew_command` | Exact predecessor, running state, live lease, no removed owner, covering reservations or exact paused source substitute; increments revision and updates reservation expiry | Cancellation changes revision as well as state; replay is not a new renewal |
| Cancellation / settlement | `cancel_job`, `SETTLE_SQL`, cancellation/settlement triggers | Queued→cancelled, running→cancelling; exact live settlement clears owner and reservations; yield preserves demand and installs not-before; failure consumes retry budget | S5 checks no revival of cancelling by modeled actions; cancelling is absorbing here, with no modeled completion/settlement from that state. New requests and general cancellation/completion precedence are outside this fragment |
| Domain acquire / renew / release | `store/sqlite/coordination.rs`; `store/hiqlite_coordination.rs`; `cluster/coordination.rs::Lease` | Acquire expired row with higher fence/revision; exact renew; release clamps expiry and advances revision without deleting row | SQLite acquire/renew do not have Hiqlite's explicit removed-owner filter; publication still checks its bound job |
| Bind | `background_jobs_domain.rs::BIND_SQL` | Selected kind and scan resource; exact live queue/domain tuples; reservations; owner not removed; stable tuple installed | Same domain fence accepts only identical stable tuple; different attempt needs a higher domain fence |
| Bind refusal | `plurxd/library_work.rs` around `bind_library` | Release domain lease, yield job until `now+5000` (a contract v2 targeted job refused by the fence: `now+250`); work never starts | Poll/backoff runtime scheduling is a recovery fairness premise |
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
| `PauseSource`, `ResumeSource` | `background_jobs_resources.rs::pause_background` / `resume_background` plus foreground SQL triggers; exact stable owner and one source reservation; physical stop/join and extra admission veto external |
| `SendRenew`, `ApplyRenew`, `DeliverRenew` | Caller queue heartbeat; atomic `RENEW_SQL`/renew trigger; worker adoption of returned exact token |
| `DomainChange`, `AcquireDomain` | Exact domain renewal/release and expired-lease acquisition in both coordination backends |
| `BindAttempt`, `RefusalRelease`, `RefusalSettle` | Binding transaction followed by distinct domain release and exact queue yield; caller not-before is now+5000, or now+250 for a targeted job's fence refusal (both model2) |
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

## November 7 library dispatch and source release

A fresh, readable manual library request now participates in the next shared resource grant. The write still uses the existing claim identity, fence and physical source limits. The source controller can inspect that immutable identity while a completion holds its publication mutex. The durable model covers the resulting claim and settlement, while the dispatch classes, timing and physical file operations are exercised in the Store and worker tests.
