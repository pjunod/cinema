# Cinema modular builds — smaller dependency graphs and measured rebuilds

**Status:** built and locally qualified — SDK-free builds, AUTH81/AUTH82 CLI
migration, native HTTP and storage authority checks pass. Task PR #1396 passed
its exact-head gate and merged; main promotion PR #1397 records delivery.
**Written:** 2026-11-04 · **Owner:** Codex · **Reviewer:** Astra.
**Review:** [Astra findings and dispositions](../reviews/CINEMA-MODULAR-BUILD-ASTRA-REVIEW.md).
**Source baseline:** `f30dfbbc9237046a0f6a6f20d893044593f56600`.
**Integration:** `effort/standalone-sqlite`; task PRs target that effort.

Companion to the [native SQLite decision](../cluster/STANDALONE-SQLITE-MODE-PROPOSAL.md),
which separates runtime authority, and the [development pipeline](../DEVELOPMENT_PIPELINE.md),
which owns review, gates and promotion. This plan separates compilation and
packaging as well. Execute one milestone at a time. If an extraction appears
to require weakening authority, losing stored feature data, or trusting stale
compiler output, fix the boundary rather than removing the check.

## 1. Outcome — standalone has no Hiqlite dependency

The finished standalone package must build, run and upgrade without including
`hiqlite`, `hiqlite-wal`, or `openraft` in its selected Cargo dependency graph.
Disabling their startup at runtime does not satisfy this contract. Explicit
cluster packages retain real membership, quorum-loss fencing and recovery.

The second outcome is a shorter development loop, measured before and after
each build improvement and after extraction. Moving files within one crate
does not establish a separate compilation boundary. A changed shared API may
still invalidate dependents, and the final executable still links. Release
Thin LTO is retained; no universal rebuild-speed promise is made.

The runtime prerequisite began at `f30dfbbc9`: native SQLite selection,
verified singleton conversion, native backup/restore and explicit promotion.
Reviewed corrections at `5484d7986` passed effort gate 6468 and merged through
PR #1379 into `effort/standalone-sqlite` as `fa5ed825a`. Dependent task branches
include that actual effort base before their own gates. The current implementation makes the standalone selected graph SDK-free;
release qualification and final integrated acceptance remain open.

The [measurement ledger](CINEMA-BUILD-MEASUREMENTS.md#3-primary-capped-linker-comparison--2026-11-04)
records the completed capped linker comparison. It showed no useful gain;
the experimental linker change is not selected for promotion. The SDK-free
packaging milestone remains the delivery path.

## 2. Contracts — package boundaries cannot change data or authority

### 2.1 Independent crates, with a thin composition root

Proposed names are implementation targets, not claims that these crates exist:

```text
                      plurx-contracts
              domain / Store ports / lease values
                    /              \
     plurx-store-sqlite             plurx-cluster (optional)
     native ownership              Hiqlite + membership + quorum
                    \              /
                   application services
             playback / library / sharing ports
                            |
                       thin plurxd

     plurx-storage-migrate (separate, version-matched tool)
                  -> both storage adapters
```

Historical schema chains and shared SQL get one maintained owner, initially a
shared schema package or explicit backend-neutral storage layer. Do not copy
the SQL into two independently evolving implementations. Existing migration
numbers, identities, encryption contexts and schema refusal floors survive.

Feature libraries accept narrow service interfaces and owned context, not the
whole daemon `AppState`. They must not depend back on the daemon. Keep closely
coupled Hiqlite storage and cluster runtime together initially; separate them
only after reverse imports have been removed. Avoid one crate per source file.

The source-baseline audit identified these boundaries. Re-verify them for
each future extraction; the draft packaging cut does not move their owners:

| Boundary | Existing coupling | Required treatment |
|---|---|---|
| Daemon packaging | [daemon manifest](../../crates/plurxd/Cargo.toml) now selects the existing optional SDK only through explicit `cluster`; native is the candidate default | Preserve independent package/target graph checks and complete distribution omission gates |
| Store contracts | [Store module](../../crates/plurx-core/src/store/mod.rs) imports leases from cluster coordination; catalogue reader carries concrete replicated state | Neutral values and injected read capability |
| Trusted Source authority | [Source authority](../../crates/plurx-core/src/sharing_source_sessions.rs) has sealed implementations | Preserve trusted construction and actual local/replicated validation; do not expose a public grant constructor |
| Receiver custody | [receiver delivery](../../crates/plurx-core/src/sharing_receiver_delivery.rs) has a private relay-read proof; ingress uses actual local-registry checks with no member roster | Preserve real native Source/Receiver sharing; the member type alone is not a native capability test |
| Optional inference | [daemon manifest](../../crates/plurxd/Cargo.toml) unconditionally includes Candle/tokenizers | Own dependency subtree in an optional search adapter |
| Web iteration | [web provider](../../crates/plurxd/src/http/web.rs) embeds shell and assets in the daemon | Development provider independent of Rust rebuilding; embedded release contract retained |

Keep storage authority and capability construction sealed across the crate
split. Validate existing native and replicated fixtures against real stores and
membership evidence. A trait made public merely to get an extraction compiling
is not an acceptable authority design.

### 2.2 Packaging is explicit; saved state is preserved

The candidate's default Cargo/Make daemon build selects native SQLite. The
existing Core `hiqlite-store` implementation remains under its private owner;
the daemon selects it only through its explicit `cluster` build feature. This
is compile/package selection, not a saved-settings switch. Current recipes are:

```bash
make build                  # native daemon; runs the standalone graph check
make build-cluster          # explicitly includes replicated capabilities
make standalone-graph-check # checks daemon, controller and optional tray
make build-migration-helper # separate SDK-bearing migration executable
```

These recipes are implemented in the draft. The standalone selected graph for
`plurxd`, `plurx-desktop-control` and `plurx-tray`, including normal/build edges,
contains no Hiqlite, Hiqlite WAL or OpenRaft. This graph result is not runtime
or whole-distribution release acceptance. P3b/P4 retain the final native/cluster
behavior, helper, packaging and supported-upgrade gates before shipping.
Inspect the resolved production graph for the selected package/target, not
merely `Cargo.lock`: a workspace lockfile may legitimately contain cluster
packages. The gate covers the complete distribution closure described below.
Run standalone and cluster graph checks in separate Cargo invocations;
workspace feature unification must not mask missing declarations or restore a
cluster dependency to the standalone graph.

Local playback, authentication, library state, native backup and supported
remote-user sharing must work without clustering. Remote-user Source/Receiver
sharing is not synonymous with a Raft cluster. Do not remove it accidentally to
make the dependency check green. Later capability packages declare what they
include; absent capabilities neither erase their data nor silently reset saved
settings. Avoid introducing a second settings feature-gate system.

### 2.3 Distribution closure includes every shipped executable

P4 must establish this explicit matrix against the real installer/image recipes.
The supported platform set is retained; a missing recipe is a failed gate, not
an opportunity to drop a platform.

| Distribution | Platform/target | Cargo roots and bundled executables | Required graph |
|---|---|---|---|
| Standalone Linux archive/image | GNU x86_64 and aarch64 | Native daemon plus declared non-cluster helpers only | No Hiqlite/WAL/OpenRaft in any selected root or host build/proc-macro closure |
| Standalone macOS package | Existing supported macOS target set | Native daemon, desktop/control and declared media helpers | Same ban; embedded parser/media tools retain their own declared provenance |
| Standalone Windows archive | Existing Windows release target set | Native daemon, service/control and declared media helpers | Same ban; existing manual release build retained |
| Explicit cluster distributions | Same supported release platforms | Cluster daemon and explicitly declared cluster-check tooling | Raft is intentional; real cluster acceptance applies |
| Migration/update helper bundle | Platform matching the installation | Version-matched migration executable and exact compatibility manifest | Cluster dependencies allowed only in this separately identified update artifact |

The draft [Docker recipe](../../Dockerfile) separates native runtime/binary
exports from `build-cluster`/`runtime-cluster`; only the explicit cluster image
contains `plurx-cluster-check`. Its separate `storage-migration-helper` export
is absent from the default runtime. Publication uses distinct
`plurxd-standalone` and `plurxd-cluster` registry repositories and checks the
selected distribution label; fleet publication explicitly selects cluster.
These recipe changes still require release qualification. Fresh standalone
archives, images and installation roots must contain neither cluster checker
nor migration helper. Updaters fetch the helper only for legacy data or an
explicit transition.

Resolve each declared Cargo root separately with its actual features and target,
including normal/build dependencies and host-side procedural macros. Reject
forbidden real package identities, their macro packages and aliases/transitive
edges. A package manifest lists every bundled executable, its SHA-256, source,
feature/capability set and matching build receipt; packaging verifies the allowlist
and exact hashes and rejects undeclared executables or cross-flavor artifacts.
Add negative tests inserting cluster-check into a clean standalone package and
introducing a forbidden host build dependency. A daemon-only graph cannot pass
these tests on behalf of the rest of the distribution.

Windows remains supported. This plan does not re-enable the disabled Windows
PR/qualification jobs: inspect its graph and packaging recipe and retain the
explicit Windows release build/runtime obligations. Linux success is not
reported as Windows execution evidence.

### 2.4 Existing installations use a separate migration helper

The current converter temporarily needs a real old runtime to settle and export
its current committed cut. A genuinely Hiqlite-free daemon cannot contain that
converter. Installation/update tooling supplies a version-matched migration
helper only when an old activated database or an explicit storage transition
requires it. Fresh standalone builds need no helper dependency.

The helper must hold the same exclusive directory lease, use the existing
verified handoff/receipt protocol, preserve IDs/master/purpose keys/local data,
and retain rollback material. The daemon refuses legacy or interrupted state
before opening a native writer if the required helper is unavailable or wrong.
Never reopen the retained pre-activation database as current data. Explicit
create/join likewise uses the cluster distribution or managed helper; changing
a config preference cannot replace the storage authority transaction.

The draft adds only the thin [migration executable](../../crates/plurx-storage-migrate/Cargo.toml).
It calls the existing private Core conversion owner; it does not extract the
credential, schema, Backend or authority factories into new crates. Release
packaging builds four separate thin-target bundles: GNU x86_64/aarch64 and
Darwin x86_64/aarch64. Each bounded native `describe` output accompanies an
archive containing exactly one regular `plurx-storage-migrate` executable.
The signer independently checks the description, archive and executable hashes.

The [signed feed generator](../../scripts/create-desktop-update-manifest)
preserves the V1 nine-asset managed cohort. Explicit helper descriptors produce
V2 optional helper metadata, binding source commit/tree, target, protocol,
recognized cohorts, output formats and converter/archive/executable hashes.
The existing Ed25519 root and accepted-release ledger authenticate managed
selection; a digest sidecar or version string cannot authorize code. The
[controller](../../crates/plurx-desktop-control/src/migration_helper.rs) stages
immutable generations and holds a shared cleanup lease through actual helper
execution. Pending `storage-migration.json` fences a new update or helper
activation so an interrupted operation retains its original tuple.

Source/external installs without an accepted signed installation record use
an explicit operator-selected source helper, rather than unsigned automatic
discovery. Build it from the verified matching source, establish its actual
commit/tree/executable SHA-256, then use `migrate-private --config ...
--expected-commit ... --expected-tree ... --expected-executable-sha256 ...`.
That command verifies its actual self tuple and uses the same Core root-lock,
conversion, receipt and drain checks. It does not create release trust. The
managed and source CLI acceptance tests pass on the §9 snapshots.

The P4 entry design fixes a compatibility tuple, rather than matching the
product version string alone:

```text
helper_protocol=1, request/receipt_format=1,
expected_helper_build_commit + executable_sha256,
accepted_source_cohort=(auth_version, canonical_layout_fingerprint),
output=(native_schema_version, manifest_format, transition_format)
```

The installed update manifest binds the expected helper artifact and tuple to
the daemon/update package. A bounded describe operation opens no data directory.
Wrong-build/same-version, unsupported protocol/layout, missing helper, or changed
executable refuses before native writer construction. Initially supported real
fixtures are the closed pre-Source AUTH81 private singleton and current Source
AUTH82 private singleton, including enabled-sharing keys and all six local
telemetry tables. Future/unknown cohorts, restored/joined/ever-peered stores and
unsettled custody/admission are explicit refusal fixtures. Do not imply support
for older cohorts without a named upgrade fixture and corresponding receipt.

Use a documented release/reacquire protocol, avoiding an inherited lock
assumption across platforms:

1. The launcher owns the root lease and performs read-only selection. If a
   helper is needed, it validates the installed helper tuple, durably records a
   unique migration request and expected authority under the lease, and starts
   no Store, worker or normal listener.
2. The launcher releases its lease before starting the owned helper. The helper
   acquires the same root lease itself, validates the request, tuple and current
   durable authority again, and either resumes that operation or refuses.
3. The helper retains the lease through export, target validation, durable
   journal/receipt publication and actual old-writer shutdown. A successful
   result is reported only after terminal drainage; a failed/ambiguous drain
   retains ownership until process teardown. Helper timeout is not success.
4. The launcher waits for/reaps the actual helper, then reacquires the root lease
   and revalidates the committed manifest, operation/receipt, identity, target
   schema and database binding before opening a writer. It cannot rely on a
   pre-helper observation or an exit code alone.
5. Every competing launcher/helper inspects authority anew under the lease.
   Completed native state is never reimported or compared against an initial
   import image after accepted writes. A second launcher either owns the valid
   current store or refuses because another daemon owns it; it never resets it.

The exact request fields, describe bounds, executable verification and process
ownership API are a P4 entry artifact tied to the then-current source. Tests
cover two launchers, helper crash at each publication boundary, daemon restart,
wrong-build/same-version helper, unsupported cohort, failed reap/timeout,
post-publication accepted writes and missing/corrupt request/receipt. This is a
cross-process authority protocol; the existing in-process lock transfer is not
credited as its acceptance.

### 2.5 Authority ownership and capability parity are entry gates

Before cross-crate authority-bearing extraction changes code, record a
source-backed ownership/API decision. Two narrow preceding phases are distinct:
primitive P3a below, and P3b SDK-free packaging under Core's unchanged private
co-owner. P3b makes existing cluster compilation/composition optional; it moves
no proof, credential owner, private Backend, trusted factory or SQL owner into
a public cross-crate adapter. The future extraction target remains a small
`plurx-authority` package that owns the sealed trait, private proof representations,
revocation state and trusted local/replicated implementations. Backend-neutral
Store/domain contracts may consume its opaque proofs; the authority package
must not depend on the daemon or the full Store API. The native implementation
validates actual owned SQLite/identity/key state. Its replicated implementation
is optional, validates actual applied/clock/Hiqlite evidence, and does not import
the high-level cluster backend back into the package. Extract the minimum
verification primitives together when this is necessary to prevent a cycle.
A feature-disabled authority package must itself pass the no-Hiqlite graph gate.

The source audit identified two concrete extraction constraints. Primitive
DTO/error/crypto/lease contracts and the full Store API need separate owners:
Store methods consume authority proofs, while authority consumes primitives.
Combining these packages would create a dependency cycle. Also, generic SQL
implementations such as `impl<T: Backend> SharingStore for T` cannot move to a
third crate once their trait is foreign under Rust's orphan rules. Keep each
trait with its generic implementation initially, or use shared free functions
with backend-owned forwarding implementations; never duplicate the SQL.

The P3 constructor/API decision must therefore refine the initial sketch into
an acyclic primitive-contracts → canonical-schema → authority → Store-API
layering before extracting those owners/adapters across crates. P3b need not
perform that extraction to omit the SDK from its selected native graph. One
integrator owns central manifests,
core re-exports and the heavily shared Store module. These tasks land serially;
their ownership overlaps and does not qualify for the disjoint-file exception.
Native Receiver custody needs an actual capability/parity fixture preserving
the existing privately issued relay-read proof, local registry and guarded
`members=None` path. Its uninhabited replicated-member type alone establishes
neither absence nor acceptance of native custody.

This is an ownership sketch, not an approved constructor API. P3c entry requires
actual proposed signatures, the dependency-cycle check, identification of every
trusted minting path, and downstream compile-fail fixtures proving callers
cannot implement the sealed authority or construct/modify its proofs. Real-store
revocation, stale native boot and lost replicated-authority tests accompany it.
Do not proceed if the sketch can only compile by exposing an unchecked mint.
The complete cross-crate gate applies before moving any proof, trusted factory,
credential owner, SDK hook or resource-binding/backend-selection authority to
a new owner, regardless of task name. P3b changes which current implementation
is compiled/instantiated while retaining those private owners. MediaSessionStore
and its Source publication/Receiver retirement dependencies remain a full
cross-crate gate; P3b does not approve a new public Backend bridge.

The [boundary decision](CINEMA-CONTRACTS-BOUNDARIES.md) records Astra's admission
of only the first primitive slice: StoreError with its SQLite conversion,
Lease/LeaseClaim and checked validators, catalogue DTOs, principal/checked UUID
values and CandidateRoute. Core keeps explicit compatibility reexports and
existing visibility. Newly public contracts validators are checked value
operations; they cannot mint opaque authority. The task owns its manifests and
compatibility facades, preserves private viewer-key construction and wire
formats, and proves a contracts-only consumer/dependency graph in isolation.
Identity/authority extraction remains blocked on that decision's unresolved
startup, credential, SDK-hook, Receiver and downstream proof obligations.

P3b follows the checked runtime and P1 source baseline as a reviewed SDK-free
packaging phase. The draft daemon now selects the existing optional Hiqlite/WAL
implementation only through its explicit cluster profile. Keep private authority, Store
traits/generic SQL and credential factories co-owned in Core. Actual native
Source/Receiver remote-user sharing remains supported. No cfg-away feature,
fake roster, successful unavailable membership or dummy replication provider
can satisfy the capability cut. Actual native and explicit-cluster binaries
must pass separate composition/graph and behavior tests, including native
local-registry Receiver ingress and physical closure plus real cluster quorum
loss/recovery. P3b is neither a full Core compilation split nor accepted timings.

A native-only candidate may precede the helper, but a legacy replicated root
must then refuse before opening a native writer. P4's separate compatibility,
root handoff, crash, supported-upgrade and complete distribution gates remain
mandatory before replacing the shipping default. Future cross-crate P3c
identity/schema/authority/adaptor extraction remains blocked and may follow
P3b/P4; compiling cluster out does not require first extracting authority.

Every timing pair includes a capability manifest. Equal-capability optimization
and intentionally smaller packages are separate result categories. The initial
standalone/runtime baseline preserves local playback and supported remote-user
sharing; it differs in cluster omission only where explicitly labeled. P5 keeps
inference available in the full-capability distribution and defines named
minimal/full recipes before changing defaults. A missing optional feature is
shown as unavailable in that package, not silently disabled in saved settings.
Retain stored data/models/settings; install/re-enable tests prove continuity.
No reduction in shipped behavior earns an equal-capability speedup claim.

## 3. Measurement — compare identical work and retain failures

P3a primitive extraction and P2c live assets/metadata can advance independently;
neither is a prerequisite for the SDK-free delivery path. P2b measurements and
the final before/after comparison retain their own acceptance obligations.

Use the existing isolated non-voter compiler slot with pinned Rust 1.97.1.
Send source archives only, without Git history or credentials. No uncapped
build runs on a voter. The host, source revision/tree, target, toolchain,
linker/version, CPU/memory limits, flags, features, capability manifest,
profile and cache identity
belong in each receipt. Record concurrent load and free disk space.

### 3.1 Baseline and comparison cases

Capture the untouched prerequisite source before build changes. Also capture
the eventual current integration base if it differs; evidence against an older
base never qualifies a new candidate. Compare each optimization against its
immediate parent with the same workload, and report a final end-to-end pair.

| Case | Required measurement | Interpretation |
|---|---|---|
| Warm no-op | At least three repeated identical commands | Cargo freshness and wrapper overhead; not changed-source speed |
| UI edit | Three distinct CSS/JS byte changes, then restoration | Embedded build cost and development asset iteration reported separately |
| Daemon leaf edit | Three controlled changes to a used function body | Actual compiler invalidation, not just a file touch |
| Shared API edit | A documented small public API change | Dependent rebuild cost, distinct from a leaf change |
| Focused test rebuild | Build then run a named real regression after a controlled edit | Report compilation and test execution separately |
| Fresh package build | New task-owned target slot, registry already fetched | Cold compiler cost; never call this a cold download |
| Production release | Standalone and cluster, same release policy | Final package cost, size and graph; Thin LTO/link time included |

An exclusive source/cache-slot lease spans staging, Cargo execution and
artifact/receipt collection, not just extraction. No other candidate may mutate
that source or target slot while the command runs. An empty target directory
with a warm compiler-result cache is labeled separately from a cold compiler
build; record/control `RUSTC_WRAPPER` and compiler-cache state.

Use a monotonic clock. Retain raw logs, command/exit code, rebuilt crate list,
Cargo timings, compiler/link wall time where available, peak RSS and target
size. Record source transfer/staging separately from Cargo wall time. Report
all samples, median and range; no cherry-picked fastest run. Alternate before
and after order where possible, warm each variant consistently, and report
noise rather than attributing it to a patch. A skipped/failed case stays marked
as such and earns no improvement claim.

Controlled edits occur only in an owned source extraction. Restore its exact
original bytes in a `finally` path, preserve actual source/probe hashes in the
receipt, and never describe a benchmark probe as a release artifact. Keep
version/epoch inputs fixed within a comparison so metadata churn is not an
unreported confounder. Do not suppress or falsify release identity to make a
no-op benchmark appear faster.

The September [K-08 measurements](../cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md)
recorded 82–109 seconds for a daemon edit and 95–122 seconds for a test rebuild,
but are historical context, not this effort's baseline. The previous temporary
compiler helper stamped a complete staged-tree hash into the daemon, causing
documentation-only snapshots to invalidate it. That helper artifact must be
identified separately from repository behavior.

### 3.2 What counts as a successful build improvement

Correctness gates apply before speed claims. A no-op may not start a Rust
compile. Changed source, deleted/renamed files, assets, vendored patches and
build environment changes must invalidate the right units, including a content
change with preserved size/mtime. Interrupted staging must never present a
partially updated generation as a valid input.

For wall-time comparisons, target at least a 20% median improvement in the
affected changed-source case. Treat this as a target, not permission to hide
noise or weaken a check. Accept a speed patch only with demonstrated benefit
for its claimed workflow and no material unrelated regression; otherwise
revise or revert it. Explain structural improvements such as zero Rust
compilations for development UI edits independently of release timings.

## 4. Milestones — review and merge each bounded change

| Stage | Implementation and ownership | Acceptance before task merge |
|---|---|---|
| P0 — plan and prerequisite | This plan/index/backlog; runtime prerequisite stays its own PR | Astra review/findings recorded and addressed; prerequisite reviewed, focused regressions retained, effort gate green |
| P1 — reproducible baseline | `scripts/` measurement runner, focused operations tests, evidence ledger | Untouched baseline receipts; no-op/edit distinction; exact edit restoration; failures retained; reproducible report |
| P2a — source/cache freshness | Docker fallback and reusable source staging helper/tests | Same-size/mtime mutation, deletion, rename, vendored input, crash, concurrent owner, interrupted staging and stale-output cases pass; measured before/after |
| P2b — developer linking/profile | Developer recipe/config and relevant docs | Benchmark existing versus candidate linker on supported Linux target; keep backtraces/native dependencies; no release-profile reduction; measured decision |
| P2c — UI and build metadata | Web asset provider/loader, thin metadata boundary and tests | Development CSS/JS edit needs no Rust build; bounded path/asset allowlist and content hashes correct; embedded release byte parity; precise truthful build identity |
| P3a — primitive contracts | Only enumerated error/value/DTO slice and compatibility facades | Existing primitive-only gate, isolated downstream type/visibility/wire/graph checks |
| P3b — SDK-free packaging | Optional current cluster implementation/composition; private authority, Backend, factories and SQL stay co-owned in Core | Actual native/cluster capability-cut tests and independent selected graphs; real native Source/Receiver sharing; no fake membership or public mint |
| P4 — migration and shipping recipes | Separate version-matched migration helper, composition and distribution packaging | Unchanged helper/root entry, whole-distribution graph/package, two-launcher/crash/compatibility, supported upgrade and native/cluster parity gates |
| P3c — future cross-crate authority extraction | Identity/schema/authority/Store/adaptor owners after P3b, optionally after P4 | Full concrete ownership/API gate and downstream/runtime proof/custody tests; MediaSessionStore closure resolved; no public unchecked Backend or cycles |
| P5 — cohesive capability libraries | Playback/library libraries first, then optional inference and supported feature packages | No reverse dependency on daemon; feature/data parity; independent focused builds; graph and timing comparison per extraction |
| P6 — final qualification/promotion | Freeze effort, integrate current main, final receipts and PR | Final source/package graphs, full affected suites, representative migration, actual Mac playback/suspend and supported Linux acceptance, before/after build report, green main promotion gate |

P2 starts only after Astra's plan findings are addressed and P1 has captured
the relevant baseline. Start with the simplest measured build improvement;
do not block it on completing the entire crate extraction. Do not merge a
performance patch on a compiler-only success without the promised timings.

## 5. PR train — every task is reviewed and followed to merge

Use the existing `effort/standalone-sqlite` integration branch because the
native runtime and new package boundaries share files. Do not use the
disjoint-file exception to bypass effort integration. Task branches use the
`codex/` prefix and are based on the current effort. Rebase/adapt the candidate
after a dependency lands and run the affected checks again.

| Task PR | Depends on | Principal files |
|---|---|---|
| Native runtime prerequisite | Existing baseline | Existing `f30dfbbc9` source change and ledger |
| Modular build plan | Runtime prerequisite | This plan, CI index, project backlog, Astra review/dispositions |
| Baseline measurement | Plan | New measurement runner/tests and evidence report |
| Verified source/cache reuse | Measurement | Source staging helper/tests and Docker fallback |
| Developer linker/profile | Measurement | Explicit developer build recipe and measured report |
| Development assets/metadata | Measurement | Web/provider and build-identity files/tests |
| Primitive contracts P3a | Prior integrated effort | Enumerated primitive contracts and compatibility facades |
| SDK-free packaging P3b | Checked runtime and P1 source baseline; P3a and P2c independent | Core/daemon manifests and private module/composition boundaries; graph and real capability-cut fixtures |
| Migration/shipping P4 | SDK-free candidate plus accepted helper/root design | Separate migration helper/updater, composition and shipping recipes |
| Future cross-crate P3c | Full ownership/API entry independently accepted | Identity/schema/authority/Store/adaptor closure; no preemptive movement under packaging task |
| Capability extractions | Relevant boundary PR | One cohesive subsystem per PR |
| Effort promotion to main | All accepted task PRs | Frozen current-main candidate and qualification receipt |

Each PR states the before/after behavior, exact regression commands, evidence
scope and remaining acceptance. Behavior fixes use `fix(` or `perf(` and carry
one `Regression-Test:` anchor per named regression. Do not push to discover
compiler errors: use the pinned source-only compiler loop and normal hooks.

Dispatch the effort development gate explicitly for the current candidate;
task PRs do not trigger it automatically. Follow each PR's jobs and failures,
address review, requalify changed candidates, and merge only a green reviewed
candidate. Preserve the regression fields in the landing commit (including
the explicit merge-message field when using the Forgejo API). Freeze task
merges for final effort-to-main qualification. No force-push or reset of other
work and no source/receipt deletion to get a green result.

The authenticated history binding requires live main to be an ancestor of the
effort base and the effort base to be an ancestor of the task head. Reconcile
those refs before dispatch, then use a new first-attempt workflow dispatch.
Re-running an old failed attempt does not satisfy that binding. A green
paperwork-only task status does not replace the explicit effort gate.

## 6. Final gates — prove omission and preserved behavior

The P4 implementation must supply graph and artifact-inclusion tests for every
standalone distribution in §2.3. The complete closure includes every bundled
executable, selected target, host build dependency and procedural macro, aliases
and transitive edges. Cargo.lock membership alone neither passes nor fails it.
Run the package graph independently from cluster harness/workspace tests.

Build and exercise the shipping standalone binary: fresh install, native
restart, auth/library/resume, local playback, native Source/Receiver behavior,
no cluster listeners/quorum tasks, native backup and isolated restore. Upgrade
actual supported historical private singleton fixtures using the separate
helper; verify complete classified data, IDs/keys, local telemetry, receipt
recovery and old-binary refusal. Missing/mismatched helper and interrupted
handoff must refuse without opening stale/native writers.

The cluster package retains actual multi-node membership, partition/quorum
loss fencing, promotion/removal and never-auto-downgrade behavior. Test the
same feature/capability combinations that packaging supports, not every
possible flag permutation. Final timings and package sizes are reported for
each supported distribution, including new migration tooling.

## 7. Non-goals — keep the first changes bounded

- No dynamic Rust plugin ABI, new microservice topology or nightly toolchain.
  Crate boundaries and optional dependencies address the initial problem.
- No weaker fsync, SQL/Source ownership, credential or quarantine policies.
  Compilation speed cannot change acknowledged-data guarantees.
- No deletion of tests, broad cache sharing across incompatible configurations,
  removal of the Docker freshness defense without replacement, or fabricated
  provenance. Reusing stale output is not a build improvement.
- No blanket compiler-profile change on production hosts. All measurement and
  compilation use isolated non-voter capacity; release policy remains intact.
- No deployment from a task branch or unqualified effort. The user authorized
  proper PRs and follow-through, not bypassing the existing promotion gates.

## 8. Evidence ledger — distinguish implementation from acceptance

| Stage | Source/evidence | Disposition |
|---|---|---|
| P0 | Plan draft based on `f30dfbbc9`; source/dependency and historical timing audit | Astra confirmed all four findings and two clarifications addressed; P1/P2 allowed in order; no build improvement implemented |
| P1 | Fixed `cf095b203` / `e1ce9af4ce` source; three samples per case, preserved raw reports, actual source restoration and Cargo artifact events | Baseline collected; guarded runner belongs to its own task PR |
| P2b historical diagnostic measurement | Equal-capability system/LLD Cargo commands; all 450 observed units had identical features/profiles | Historical diagnostic only: initial apparent reductions and failed isolation evidence are retained in the measurement ledger. The completed capped primary comparison found no useful gain; LLD is not selected. No cold/release/Mac claim |
| P3b/P4 draft | Native default/explicit cluster composition, separate registry namespaces, optional signed four-target helper and verified-source CLI implemented; selected native daemon/controller/tray graph has no Hiqlite/WAL/OpenRaft | Integrated Clippy, SDK compatibility, actual CLI/HTTP and graph checks pass; see §9 and the promotion PR for delivery evidence |
| Core helper regressions | Tree `470952445623eba336ec3d61a1f1d39db694caa2`; run `1f93ad65ebb149eb84307e6bb9a49306`; 16 passing tests including real AUTH81/AUTH82 private singleton data | Focused primary execution on that snapshot; not current candidate or cross-process upgrade qualification |
| Signed staging regressions | Tree `31f1b3e488e1c0bb7a47d8d2117218756b4b9434`; run `762075cc9e6b4134859683f846ad476f`; seven passing tests | Actual signed capsules, closed archive/hash checks and cleanup leases on that snapshot; not release or daemon/helper child-process acceptance |
| SQLite close regressions | Tree `888cb4f00d801b162089f28ff6b058c03f67cbfe`; run `20261104-225421-26b4a24c`; three passing tests | ARM execution of concrete native close/ownership checks; no Windows/macOS qualification claim |
| P3a/P3c | Primitive-only entry admitted separately; full identity/schema/authority/Store extraction remains subject to the existing ownership and downstream negative-test gates | Future separate work; this candidate introduces no contracts/authority/backend crate extraction |

Record Astra's findings and individual dispositions with the review, then
update this ledger and the backlog row in the same task changes. Retain the
original before measurements and failures when adding after results.

## 9. Integrated delivery evidence — 2026-11-08

[Task #1396](http://forge.lan:3000/noirr/plurx/pulls/1396) merged after gate6532;
Its retained nine-step qualification includes AUTH81/AUTH82 CLI conversion,
native reopen and data/key preservation.
[Correction task #1442](http://forge.lan:3000/noirr/plurx/pulls/1442) integrates
current-main contracts and confines new diagnostic workers to explicit clusters.

The [measurement ledger §6](CINEMA-BUILD-MEASUREMENTS.md#6-integrated-standalone-build--2026-11-08)
records source `83da88f89e`, all final warm samples and the sealed
bundle. The actual daemon compiles 380 units with no Hiqlite/WAL/OpenRaft.
Cold-build gains and further crate extraction remain outside this delivery.

[Promotion #1397](http://forge.lan:3000/noirr/plurx/pulls/1397) owns exact-head
full qualification, migration/graph replay, final receipt and landing. SQLite124
remains the published dispatch migration; native identity/census is125. Models,
quorum guards and data/credential preservation rules are unchanged.
