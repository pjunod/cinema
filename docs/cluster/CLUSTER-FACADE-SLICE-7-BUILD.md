# Cluster facade slice 7 — close ownership and prove honest zero

**Status:** open — 7a and 7b are implemented in preparatory checkpoints; current-base integration, final review and gates remain. The plan preceded implementation. **Owner:** Codex.

Companion to [slice 6](CLUSTER-FACADE-SLICE-6-BUILD.md),
[the facade contract](CLUSTER-FACADE-BUILD.md), and
[the boundary decision](CLUSTER-FACADE-BOUNDARY-DECISION.md). The
preparatory base is checkpoint `2f39bc76a`; every reviewable task is ported
to the actual current `effort/cluster-facade` before final checks/review/gate.
This is the final facade stage, not completion of the wider cluster audit.

## Acceptance contract

Feature code has no raw membership, serving-fence or replication handles.
Remove those AppState fields rather than keep duplicate private owners.
The facade owns bootstrap and runtime composition and gives feature code
narrow typed capabilities. Existing C2/C7 shutdown ownership, clock startup,
request admission, fresh learner job decisions, opaque custody proofs,
backup sequencing and settings-change proof logic retain their rules.

The final source census has an empty measured table and zero ceiling. Its
classification distinguishes proven local DTO/watch/counter fields from
raw handles without renaming fields, broad method exclusions or new file
exemptions. Unknown receivers remain counted. Strengthen negative controls
for aliases, renamed fields, clones, shorthand/destructuring, split lines,
macros, raw-returning accessors and new files. Actual sibling-module
compiler probes must reject direct raw AppState access.

Core role/roster methods become module-private only after real consumers
use the explicit core capability bridge. Rust cannot allow one foreign
module to call a private method: the boundary decision records that limit
and the chosen high-level public core contract. No raw-manager getter,
Deref or public renamed equivalents are introduced to fake privacy. Keep
explicit ROLE/ROSTER census detection after visibility changes. Actual
source arrays currently name seven role and eleven roster methods.

## Sequential ownership and delivery

All pieces target one effort branch. Cluster.rs has one integration owner,
and these are serialized reviewable tasks, not file-disjoint main changes.
Each piece receives focused regressions, one complete-diff adversarial
review, exact-head Effort gate and landing before the next final task base.
Preparatory source checkpoints do not establish landing or qualification.

| Piece | Files and ownership | Result |
|---|---|---|
| 7a — incoming authentication and admission | `cluster.rs`; `diagnostics.rs`; HTTP `activity_starts`, `internal_activity`, `internal_auth_revocation`, `internal_live_tv`, `internal_media`, `internal_media_sessions`, `remote`, `cluster_operations`, `mod`, `stream`; HLS `control`, `create`, `fence`, `preparation`, `response`; `offline.rs`, `state.rs`, associated fixtures | Reuse PeerAuthentication; preserve distinct legacy/read/voter/exact algorithms. Inject owned restart admission into OfflineManager. HLS response observation retains the same authority source and arrival/answer sampling. Route classification stays stateless and exact. |
| 7b — job, backup and applied-change services | `cluster.rs`, `background_jobs.rs`, `background_jobs/analysis_reconcile.rs`, `cache_preparation.rs`, `state.rs`, `subtitle_models/mod.rs`, small remaining role callers, `backup.rs`, `settings_changes.rs`; backup-only Boot/AppStateConfig wiring in `main.rs` and corresponding `startup_tests.rs` literals | Owned ClusterJobAuthority with fresh learner authority, typed reconciliation that preserves unreadable refusal, exact backup apply/anchor/apply/snapshot deadline sequence, typed applied-index/liveness/change source with unchanged settings proof. |
| 7c — core boundary | `cluster.rs`; scoped core membership/access APIs; cluster-check and external startup observer fixture; census declaration check and exact source assertions | High-level core services and private raw role/roster methods, constructed through the existing manager at composition without changing SelectedBackend/listener ownership. Preserve C7 on its actual landed base. |
| 7d — runtime ownership, privacy and semantic census | `cluster.rs`, `main.rs`, `state.rs`, last media-pool fence/media-session clock consumers, `cluster/census.rs`, ratchet/ownership tests and fixtures, final plan/audit/backlog ledger | Typed Boot/AppConfig and lifecycle composition, remove last raw fields/reach-arounds without a raw-parts getter, exact non-raw receiver classification, strengthened negative controls, compiler probes, empty measured pins and zero ceiling. |

Associated formal source mappings and ownership inventory are updated only
after reviewing actual modeled statements/owners. Matcher sentinels and
existing raw-type inventories are retained or strengthened. No unrelated
B3/C1/source-reader mechanisms are rebuilt.

## 7a contract

Incoming authentication reuses the existing inline calls owner. Add narrow
delegates for the original legacy activity, internal read and internal
voter request algorithms; do not merge them into a boolean mode or replace
fresh checks with a cached answer. Preserve exact target/method/path/body,
nonce consumption, clock/freshness/member floors, signature/status/body
binding, error mapping and caller deadlines.

Serving replacements use the existing authority/subscription/admission
mechanisms. The OfflineManager capability returns the original owned
RestartAdmission token; it is retained and released at the same points.
HLS response telemetry must sample the same authority at arrival and
answer, not freeze a readiness boolean. Keep cancellation and response
observation ownership unchanged.

## Validation

Use the owned normalized source-only compiler slot and dedicated target.
Verify every tracked archive blob before/after each command; share only
dependency/compiler caches, never CARGO_TARGET_DIR with another checkout.
Use pinned Rust 1.97.1 and the existing CI test stack of 8 MiB. A prior
shared-target result is not exact-tree qualification.

Before Rust edits establish the working compiler loop. For 7a retain/run
existing exact/legacy/read/voter authentication, clock/nonce/body/redirect
regressions, HLS arrival/answer authority and restart-admission/cancellation
cases, plus focused offline token ownership. Later pieces add their
existing job/backup/settings/bootstrap/privacy regressions and core,
daemon, cluster-check and external fixture compile coverage.

Run formatting, relevant strict Clippy, all-target compile, focused tests,
census/ratchet, ownership, source-mapping checks and exact-body paperwork.
Keep baseline/environment failures explicit; never suppress warnings,
weaken proof validators, widen pins or infer fleet acceptance. Final effort
promotion freezes task merges, integrates actual current main, qualifies
the new tree and passes Main promotion gate. Deployment remains Paul's
action. No implementation, review, gate or acceptance is claimed here.

## 7a preparatory delivery ledger

Checkpoint `c4164ecf0` passed the isolated daemon all-target check and 13
focused daemon/core tests for incoming authentication, HLS authority loss,
restart admission and offline cancellation. These include exact request and
response binding, learner/voter classification and read-window replay. The
core cases and structural HTTP checks do not establish successful signed
requests through every migrated route. Existing offline tests cover adjacent
ownership rules rather than directly proving the claim-to-registration race.

The unchanged census measured 302 → 242 raw sites (60 removed). Only actual
pins and the ceiling were lowered; matcher, method inventories and exemptions
were retained. The ownership inventory gains one `ServingAuthority` spelling:
the HLS response observer takes the same live authority directly and samples
its original source at arrival and answer. It adds no authority lease, task
or timer. The seven ratchet checks passed before the pin reduction.

The first census command selected a nonexistent nested `tests` module and
matched zero tests; its minimum-test guard rejected the receipt. The actual
module ran all ten cases, with nine passing and only the expected stale-pin
assertion failing. Updated pins and ownership bookkeeping still require the
new committed archive's checks. Dedicated source-only build receipts remain
outside compiler/cache directories. This preparatory evidence is not final
intended-base qualification, an independent review, landing or deployment.

After the bookkeeping update, all 14 ratchet/ownership checks passed.
Seven state source identities were repinned only after each unique modeled
42-line window was verified unchanged; all three mapping checks report no
drift. This is source correspondence evidence, not a new model execution.

## 7b execution contract — jobs, backup and applied changes

The preparatory base is `62c5f25fc`. Its exact source-only daemon all-target
check passed before 7b Rust edits. The task remains serialized behind the
actual landing of earlier slices. The table above includes backup-only Boot
and AppStateConfig wiring and matching startup fixture literals; it does not
authorize competing listener, startup-drain or vendor repairs.

**Job authority.** An owned facade capability implements the existing core
`ClusterJobAuthority` by delegating each decision to its original manager.
Keep fresh learner/voter authorization at every existing claim, renewal,
settlement and publication boundary. Inject that trait owner rather than the
raw manager. Existing trait calls in job guards remain meaningful calls,
not new facade reach-arounds to hide by renaming methods.

**Reconciliation.** Replace the optional raw manager with typed cluster/job
reconciliation access. Preserve the exact replicated-mode test, fresh
operations-member lookup and local-node inclusion. Missing capability or
unreadable replicated membership stays unknown (`None`), never a guessed
sole local member that could authorize retirement or reassignment. Preserve
the standalone engine fast path and peer inventory errors. Cache preparation
keeps both inventories, including the second reachable/base-address filter;
subtitle model selection keeps voter order/filter, readiness and maintenance
checks and its existing 60-second observation lifetime.

**Backup.** An owned `BackupSource` privately holds the local client and
returns plain local progress (applied index, last-log index and optional
published-snapshot index), the acknowledged commit anchor and the existing
local snapshot trigger. It exposes no raw client, SQL callback or new policy.
The backup manager retains this exact algorithm: acquire its lease; establish
one absolute deadline; wait for applied=current last log; commit the
forwarded anchor and refuse a missing acknowledged index; wait again for
applied=current last log and at least the anchor; trigger once; wait for a
published snapshot at least the anchor; then copy, validate and package.
All phases spend the original budget; keep bounded metrics reads, immediate
error handling, the existing 50 ms wait cap and lease cleanup. Missing local
source remains a refusal.

**Applied changes.** A narrow owned capability privately wraps
`LocalDbRaftMetrics` and supplies its existing applied-index, running and
change-wait operations. SettingsChangeSource changes only its payload and
delegates: retain arm-before-read, mark/confirm/applied-past, ProofMark and
Proved behavior, index agreement, unknown-force-refresh and cancel-safe watch
semantics. Replicated-without-local-watch remains Polled; preserve the
distinct clock fallback rather than combining the two policies.

**Focused evidence.** Run existing backup current-log/anchor catch-up,
original-deadline, stalled-read, metrics-error, deadline-completion and spent
snapshot-budget cases. Run settings proof/reload cases and reconciliation
missing-member/changed-engine cases, including a meaningful unknown-membership
refusal control, plus fresh learner job-claim regressions. Run core default
and Hiqlite compilation as affected, daemon all-target check, strict Clippy,
format, strict census, ownership/ratchet, formal source checks and exact-body
paperwork. Lower only measured pins. Record inherited compile debt honestly.
The final intended-base whole-diff review, gate and landing remain separate.

7b also owns the backup payload wiring in `main.rs` (`Boot` and
`build_state`) and the corresponding `startup_tests.rs` config literals.
Those fields become a typed local backup source. Main's existing job-trait
injections also use the facade owner; startup/listener, shutdown and vendor
mechanisms remain with their existing owners. The 62c5f25fc baseline passed
the isolated daemon all-target check before 7b Rust edits.

The first 7b all-target check found one remaining `build_state` fixture in
`desktop_control.rs`; 7b owns only its typed backup-argument conversion. The
protected runtime-directory rules and desktop shutdown algorithm stay intact.
Extend `http/analysis_reconcile.rs`'s existing departed-target regression with
an unknown-membership negative control before its known-membership positive
case, proving that no queued request is retired or replaced from ignorance.

## 7b preparatory delivery ledger

Implementation checkpoint `fea8f2b93` passed the isolated daemon all-target
check and 35 focused tests: backup current-log/anchor/deadline behavior, five
analysis reconciliation cases including the added unknown-membership control,
clock proof/enforcement followers, settings/takeover watch behavior and learner
job refusal. These exercise the existing algorithms and helper boundaries;
they do not constitute a full production backup/copy or fleet qualification.
The first compile found one remaining desktop fixture argument and was fixed
without changing runtime-directory validation. The initial external harness
expected six reconciliation cases; source and execution contain five. Its
receipt failed, the inventory was corrected, and the complete batch passed
without deleting a test or changing a source assertion.

The unchanged census measured 242 → 204 raw sites (38 removed). Updated pins
and ceiling retain all matchers, raw-method inventories and exemptions. All
14 ownership/ratchet checks passed, with no task, timer or process inventory
changes. Nine source-mapping entries were reviewed: eight unique 42-line
windows are unchanged; the other contains only two neighboring typed job
authority injections, with the modeled worker spawn/ownership unchanged.
All three source-mapping checks pass after hash-only repins. No new model
execution is claimed.

Strict daemon all-target Clippy remains red on the same six inherited
unused-item groups: `claim_skipped`, `demand_renewed`, `ProcessedTrialAdmission`'s
claim and two methods, `mark_admitted`, and playback qualification waiters.
No suppressions or unrelated owner changes were made. Final current-base
integration must resolve this debt; a passing all-target check is not a
Clippy pass. The new committed census pins/format still require their archive
checks. Final review, effort gate, landing and promotion remain outstanding.

## 7c implementation contract — core-owned domains

The preparatory base includes the actual roster-mark effort landing through
`6c95fed4c`. Requalify that archive before Rust edits. Final C7 integration,
complete-diff review and gates still use the actual intended effort base.

Add `crates/plurx-core/src/cluster/membership/access.rs`, declared privately
and with its public domain types re-exported by `membership.rs`. A public
`MembershipManager::cluster_access()` constructs the private-handle aggregate;
its constructor stays within membership. This returns typed services, never
a raw manager, Deref, generic callback or arbitrary SQL executor. Existing
selected-backend and startup-observer signatures stay intact: the daemon
facade derives services inside composition; the external startup observer
derives its clock protocol from the existing callback argument. This avoids
an overlapping SelectedBackend/listener factory change. It neither replaces
the separately owned startup mechanism nor cherry-picks an unlanded C7 head.

The core aggregate owns these distinct domains:

- Local deployment, applied standing, heartbeat admission and maintenance
  observations retain their original sources and unavailable/standalone
  behavior. They are not one atomic or linearizable snapshot. Fresh committed
  membership returns a typed observation, rather than public renamed member
  and voter predicates. Active-voter confirmation is a separate fresh service.
- The existing stamped RosterDirectory is constructed through the aggregate.
  Purpose projections retain ordering, caps, learner route eligibility and
  time-dependent readiness/reachability. Naming/origin queries remain a
  distinct service: they deliberately include non-Raft nodes, and hostname
  evidence is not present in the general roster.
- Move the narrow Jobs owner behind the core boundary; keep its existing
  public trait and both fresh core algorithms. Reconciliation keeps unknown
  membership refusal and the exact operations inventory semantics.
- A clock protocol owns guard identity, leadership, applied changes, proved
  roster, local hint, final authenticated-request roster, request freshness
  and exact signed response binding. Do not turn proofs into directory hints.
  Keep peer-call factory timing and process-shared pool ownership unchanged.
- Revocation exclusion owns preparation/admission, claim-bound roster, local
  apply acknowledgement, exact release and cleanup as one coherent protocol.
  Preserve current opaque lease types, deadline and cancellation ownership.

After consumers migrate, make the seven ROLE and eleven ROSTER methods
`pub(super)` — **18 total**, including `media_peers_for_route`. Preserve their
algorithms. Core cluster descendants remain legal callers; migrate actual
external `plurx-cluster-check` activity/origin requests and the proved clock
reads in `plurx-core/tests/fixtures/startup_observer.rs`.

7c owns those core files, daemon `cluster.rs`, the census's declaration
reality check, and exact source-guard expectations in membership tests that
reference changed visibility. Retain the explicit ROLE/ROSTER arrays and
all counting patterns/exemptions; check selected private declarations without
counting every private helper as a new public mechanism. Strengthen the
visibility expectation rather than dropping source assertions.

For coherent composition without a temporary raw-parts getter, finish
`Boot`/AppConfig typed ownership together with raw AppState field removal
in 7d. That step owns main/state wiring, the last media-pool fence uses and
media-session clock uses, lifecycle delegates and compiler privacy probes.
This changes the delivery order within the same acceptance contract: 7c
closes core visibility; 7d closes runtime ownership and honest census zero.
It does not defer or retire any required endpoint. Preserve actual C7
shutdown behavior on its landed base before final delivery.

Compile actual consumers with pinned1.97.1: core default all-targets; core
all-targets with `hiqlite-contract-tests`; cluster-check all-targets; daemon
all-targets with `cluster-integration-tests`. Plain hiqlite-store does not
compile the external startup observer. Run focused core role/roster, clock
proof/identity, revocation exclusion, directory ordering/filtering, learner
job and daemon protocol regressions; strict Clippy, formatting, census,
ownership/ratchet, source mapping and paperwork remain required. Record
inherited debt and acceptance limits rather than weakening a guard.

7c compiler privacy evidence must have a positive control using the public
services and a negative external-crate control covering all 18 raw methods.
Accept only the expected private-method diagnostics for each named method;
an unrelated compile failure is not a privacy pass. Keep probes/receipts in
owned external task storage, compile against the exact normalized archive
under its workflow lock, and verify archive blobs before and after.

The current landed-mark base `6c95fed4c` has passed its isolated daemon
all-target check, 35 focused 7b regressions and all ten census cases. The 7c
plan changes documentation only, so this establishes its source compiler
baseline before implementation.

Administrative preflight needs its own core observation service: fresh
topology and uncached operation targets from the existing query, distinct
from both job placement and the stamped directory. Do not route mutation
preflight through a cached directory or relabel it as artifact work. This
service supplies observations, while the separate active-voter confirmation
and existing operation claims/deadlines retain authorization ownership.

7c also owns exact API migrations in actual daemon startup/test fixtures
that directly call one of the now-private methods. Preserve their original
assertions and runtime prerequisites; the compiler identifies those consumers
with the actual integration feature enabled. No fixture may replace a proved
clock roster with an applied hint to avoid an API error.
