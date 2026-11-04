# Adaptive owned containment — what still stands before real trials

**Status:** open · scoped implementation qualified; production coverage open ·
2026-11-04. Companion to the [production policy](ADAPTIVE-PLAYBACK-PRODUCTION-POLICY.md)
and [canonical resource ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md).
The [resource plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md) remains normative.
The handoff's “M0 owned containment” includes native containment work; the
plan's M0 also requires the complete affected-work inventory below.

## Original kernel ownership precedes executable entry

The Linux domain factory pins the original cgroup2 directory, its parent link,
namespace and control descriptors. It requires enabled memory/pids controllers,
an ordinary empty domain, exact finite limit readbacks, zero swap and grouped
OOM handling. A regular directory, substituted mount, unsupported namespace,
missing controller or uncertain setup cannot produce entry authority.

The fixture launcher uses `clone3(CLONE_INTO_CGROUP | CLONE_PIDFD)`: the child
is born inside that original configured domain. There is no post-spawn PID
migration or arbitrary `Command`/`pre_exec` callback. The sealed descriptor
specification has no production constructor until its executable, syscall and
filesystem profile are audited. Unsupported atomic entry refuses; it never
falls back to execution outside the domain.

Production FFmpeg still uses its existing launcher. Durable graph/recovery
integration must retain the original domain identity before this backend can
serve production work; the fixture path supplies no restart acceptance.

The original child owns its domain and pidfd. Dropping a waiter transfers
those same objects to bounded cleanup custody. Root wait, exec-error reporting,
pipe closure and descendant extinction remain distinct facts. A sealed
original domain can prove kernel task extinction only from its actual retained
interfaces; a kill request or a leader's exit supplies no such proof.
Identity or cleanup uncertainty retains custody instead of returning capacity.

The delegated subtree is trusted controller authority. Pinning descriptors
does not prevent another principal with that authority from raising limits or
migrating tasks. A production child must have no controller-write, descriptor
acquisition, namespace or privilege escape. The fixture-only profile does not
claim that production sandbox.

## Kernel enforcement does not certify a complete producer peak

Configured `memory.max` is not `KnownMemoryPeak`: the kernel permits temporary
overshoot, and moving a process does not transfer charges for already
instantiated memory. `pids.max` counts tasks and constrains fork/clone;
independent task migration can exceed it. See the primary
[kernel cgroup v2 contract](https://docs.kernel.org/admin-guide/cgroup-v2.html).

The daemon still owns variable allocator/executor/channel/pump state outside
the native child. The implementation preserves its unknown memory/task claims
and the existing trial barriers. Domain extinction also leaves separate root
reap, pipe drain, source/cache, scratch and application closure obligations.
Residual `memory.current` is an observation, not proof of full refund.

The next complete boundary is an owned attempt worker carrying variable
planning/parsing/production state, with separately bounded daemon IPC and
startup bookkeeping, or an audited equivalent allocator/executor design.
Neither sampled maxima nor a successful SIGKILL call establishes that bound.
Startup and cleanup qualification must retain their original environment,
source and continuous cutoff. Timeout leaves the same owner charged.

## Inventory includes owners after serving rows disappear

The bounded census has 256 original-owner slots, separate from policy service
samples. Tokens register before entry and survive Closing/UnknownHeld. Shared
consumers are references to one original producer. Their application closure
cannot be inferred from the producer's physical receipt.

Each node runtime owns one book, initially unbridged. The selected runtime
bridge uses census → operations book → ledger lock order. It retains a compact
original close-proof handoff through rich bundle destruction and conserves the
same row during R→S. Recovered obligations register as held before native
imports open. This bridge is built and has scoped core qualification;
its implementation does not mint coverage for any broader family.

A snapshot has an epoch and explicit unbridged-family coverage. Admission must
hold the same mutation lock through the actual reservation and new original
registration. Overflow, stale epochs, unavailable observation or any unbridged
family refuses positive completeness. Production coverage has no generic
boolean constructor. Each adapter must establish its own real entry and final
close boundary before obtaining coverage.

| Affected work | Original entry hook | Final-close ownership hook | Current census bridge |
|---|---|---|---|
| Selected planning, R, retained S, cleanup and UnknownHeld | [Core operations](../../crates/plurx-core/src/adaptive_resources/runtime/operations.rs): original role before journal publication/native latch | Same `OwnedRole` through physical/application settlement and rich bundle destruction | Built; 51 distinct core cases pass; broader coverage open |
| Buffered/shared finite VOD | [Shared finite build slot](../../crates/plurxd/src/vod/shared.rs): original unpublished producer/output before the first post-R await | `drive_finite_closing`: join original driver, drain allocations/application aliases, finish same close ticket | Original driver custody built; rich R/S application census open |
| Rolling and continuous output | [Transcode manager](../../crates/plurxd/src/transcode.rs): original session/producer attempt before publication | [Rolling retirement](../../crates/plurxd/src/transcode/rolling/retirement.rs): retired presentation retains its original session; [compact copy-ticket maintenance](../../crates/plurxd/src/transcode/manager/maintenance.rs) joins original actor and settles same native ticket | Open |
| Live transport | [DVR transport](../../crates/plurxd/src/live_tv/dvr.rs): one original tuner worker; separate viewer feed queues | `close_transport_arc` and actual tuner/body join, plus each viewer pump/feed destruction; its `closed` signal alone is insufficient | Open |
| DVR recording sinks | [DVR sink](../../crates/plurxd/src/live_tv/dvr.rs): original sink before `start_sink_writer` | `sink_writer_loop` file write/flush/fsync and `settle_sink` joining the original handle; activity-row removal supplies no proof | Open |
| Source actors and escaped response bodies | [Source factory](../../crates/plurxd/src/transcode/source_actor.rs): original registry owner before spawn; original response reference before body publication | Original physical/preparation settlement and every `SourceResponseGuard`; [Source direct task registry](../../crates/plurxd/src/transcode/source_direct.rs) retains Active/Settling/Failed custody | Direct applications built; other Source paths open |
| Local direct reads | [Local direct open](../../crates/plurxd/src/transcode/local_direct.rs): fixed 64-slot claim, core preparation and original worker before native entry | `reconcile_local_direct_owners`: join Active worker, destroy rich entry/application lease, retain Settling ticket until final retirement; [core direct domain](../../crates/plurx-core/src/adaptive_resources/runtime/direct_source.rs) owns the actual reads | Native/application adapters built; scoped qualification passes |
| Background static generation | [Static run reservation](../../crates/plurxd/src/transcode/background_static.rs): bounded original group before heartbeat spawn; same group binds to M before P native awaits | `reconcile_static_artifact_run`: retain AdmissionUnknown/Closing; compact cleanup waits for actual controller/entry/inline-F destruction, original heartbeat join and original Store Close | Scoped P/M adapter qualified; pre-M census and broader family coverage open |
| Preparation and retained output | [Retained artifact registry](../../crates/plurxd/src/vod/retained.rs): same retained S and original preparation origin | Original finite closing ticket plus each artifact/read alias; [cache reader guards](../../crates/plurxd/src/cachekeep.rs) last through actual consumer destruction | Open |
| Relay, network/source domains and legacy helper graphs | [Relay response pump](../../crates/plurxd/src/media_sessions.rs) before spawn; original [probe](../../crates/plurxd/src/transcode/source_preparation.rs), [index](../../crates/plurxd/src/fragindex.rs) and [subtitle](../../crates/plurxd/src/transcode/source_subtitles.rs) operations | Actual pump/upstream/queued-body destruction and original helper settlement; other legacy audio/processed graphs still need their own adapters | Open |

These rows identify required hooks; they are not assertions that those hooks
are installed. Copying current serving maps or a truncated capture observation
cannot establish absence. CPU and RAM initially share a whole-node domain;
narrower device, source and storage domains require their own backed identity.

### Transfers and disappearing rows cannot certify close

The Source factory's `finish_source_owner` removes its registry entry even
when physical cleanup remains unresolved. Its census selector must survive in
independent UnknownHeld custody; the registry's absence is not a close event.
Live TV's `run_live_session` transfers an unconfirmed process and scratch
obligation to `OrphanSession` before retiring the session. That transfer must
move the same original census identity, with no absent interval.

The DVR transport's `close_transport_arc` currently removes the transport,
ignores timeout/JoinError from its worker join and then signals `closed`.
A census adapter cannot use that signal as positive settlement. It must retain
the original task until its actual join and owned tuner body destruction are
confirmed, with uncertainty remaining held. DVR writer completion also remains
separate from successful fsync and from destruction of queued body allocations.

### Direct applications retain original workers and whole payloads

Local direct and Source direct now register one separate application reference
against the original native source residency before entry. They do not count a
second producer. The [application owner](../../crates/plurx-core/src/adaptive_resources/runtime/direct_source/application.rs)
wraps the actual rich payload inside its Arc; only a normally completed payload
destructor decrements its bounded count. Source direct wraps both the file and
the whole actor, so escaped cached actors retain their own charged allocations.
The nonclone lease seals registration; its Drop never closes the census row.

The same core owner retains the original worker's actual JoinHandle. A cancelled
waiter can rejoin that handle; it cannot spawn a replacement. Positive join must
return a retirement ticket for the same original application Arc. Panic, wrong
ticket, poisoned state and destructor panic keep the reference UnknownHeld.
Local native-retirement timeouts retry within the same original worker.
Spawn runs outside the application mutex because a shut-down executor can
synchronously destroy its future. A core gate waits for publication of that
same handle before polling the original future; its measured frame is charged.
Reference removal requires sealed registration, all original payloads destroyed
and positive original join, under the census boundary before native settlement.
A producer physical receipt cannot retire this separate application reference.

The [HTTP cache](../../crates/plurxd/src/http/shared_source_playback.rs) previously
kept the whole Source actor through 64 later cache entries. It now compacts only
a direct actor with its actual successful terminal outcome, outside map/result
locks. Authentication, original typed assignment, published lineage and the same
End receipt remain. Cached terminal status grants no new live operation or close
authority. Any independent actor alias still blocks actual application closure.
This compaction does not prove that the bounded HTTP cache's own authentication,
assignment and lineage metadata has been destroyed; that wider inventory remains
open.

Focused qualification passes **17 distinct cases**: 12 core direct-source
cases and five actual daemon cases, all with zero ignored. Six cases are new.
Source tree `082c1445` supplies 14 unchanged-path passes; the three daemon cases
with diagnostic-only assertion edits pass again on `4f886314`. The runtime
implementation is unchanged between those trees. The core batch includes real executor shutdown, driver panic,
destructor panic, cancelled original waiter, escaped application aliases,
original read jobs and real restart history:

```bash
cargo test -p plurx-core --lib --locked \
  --features fixtures,hiqlite-contract-tests,scratch-fault-injection \
  adaptive_resources::runtime::direct_source::tests -- --nocapture --test-threads=1
```

The five daemon cases use `cargo test -p plurxd --bin plurxd --locked --features
cluster-integration-tests <exact-case> -- --exact --nocapture`:

| Exact case | Actual boundary |
|---|---|
| `http::shared_source_playback::direct::start_failure_tests::local_direct_original_worker_and_payload_reference_close_after_response_guard` | Original local worker, response guard and payload/reference destruction |
| `http::shared_source_playback::direct::tests::sharing_source_direct_original_restart_admission_moves_to_retained_ticket` | Escaped actor holds charge after terminal cache compaction; status/duplicate End remain valid; Start/body replay cannot reopen |
| `http::shared_source_playback::direct::start_failure_tests::direct_unpublished_open_failure_returns_original_ticket_and_releases_own_r` | Actual missing-file failure retains original R until the rich actor drops |
| `http::shared_source_playback::direct::tests::sharing_source_direct_read_join_before_end_receipt` | Original read job joins before native/SQL End |
| `http::shared_source_playback::tests::sharing_source_http_native_copy_h1_h2_vtt_and_held_body_end` | Shared non-direct actor retains its existing native VOD/body/End lifecycle |

These are scoped native results, not full-suite, M0 or real-trial acceptance.
Exact-candidate formatting/Clippy and the manual Effort gate belong to the task
PR. Earlier superseded-source passes are not added to this count. Existing
zero-application-byte core fixtures retain their native-only boundary; neither
those fixtures nor the explicit untracked VOD wrapper certify application
coverage. Background static now has the scoped adapter below; finite R/S application
leases and broader static entry paths still need adapters.

Keep admission/settlement mutation under a single documented lock order.
Local direct holds its slot mutex while preparing the core role; Source direct
holds registry/task locks across that preparation. A future census→daemon-map
snapshot would invert those orders. Move original typed selectors through the
existing slots without introducing the reverse census acquisition.

All family coverage bits remain unbridged until every relevant entry path,
legacy path, recovery import and application reference in that family has a
concrete adapter. Completing local direct alone cannot certify all SourceReader
work. Lost selectors and census overflow retain uncertainty and never evict an
older owner to claim completeness.

## Static P/M counts actual controllers, entries and inline futures

The bounded [application group](../../crates/plurx-core/src/adaptive_resources/runtime/application.rs)
is created before the original heartbeat spawn. It has four payload/frame
slots and one original task. The group wraps the actual shared controller
state inside JobFence's original Arc, the encoded controller payload, the
whole pinned inline encoded pass in
[state.rs](../../crates/plurxd/src/state.rs), and the actual static entry
inside its escaped Arc aliases. `Ready` does not release the inline F count;
only successful destruction of the original pinned F does. Destructor panic,
poll panic, poisoned custody and failed original join retain uncertainty.
There is no observer task or daemon-supplied positive-close Boolean.

Core retains and polls the same original heartbeat JoinHandle. Cancelling its
waiter leaves that handle in original custody; retry joins it without spawning
replacement work. Spawn occurs outside the group mutex, and the original F
cannot run before handle publication. A shut-down executor may synchronously
destroy its submitted F, so this ordering is required even before first poll.

Before `prepare_inputs`, one-shot binding seals registration and registers the
same original group identity as a separate reference to the actual M role.
Census → original book → group → finite is the binding lock order; every
deterministic group/capacity refusal precedes census insertion, and no fallible
step separates insertion from retaining the same reference and application
fence. Failed byte checks, a taken M slot or a sealed cross-runtime group
cannot leave an orphan reference.
Its measured inline/heartbeat frames and structural bookkeeping join the
existing requested application projection. These are requested capacities,
not a complete allocator/executor peak or startup bound. The original finite
owner retains the group if a daemon selector disappears. Existing finite
callers without this adapter keep their own alias fence; registration is not
globally imposed on unaudited R/S lifetimes.

The daemon preserves native P, report, original R/S, cache/compatibility,
AdmissionUnknown and original logical Store fences. Once original Store Close
and heartbeat join are positive, reconciliation extracts the entry's original
controller selector, transfers the registered group into compact cleanup
custody, and extracts the rich entry under the map lock.
Actual destruction and reference finalization run after that lock is released.
Compact cleanup holds no rich-entry Arc. An escaped entry, JobFence, controller or
completed inline F therefore keeps the original M reference and application
charge while permitting later reconciliation. Successful reference closure
then releases the application fence; native and journal settlement remain
separate. Retained S and its aliases keep their existing independent lifetime.

**Startup boundary:** claim candidate parsing, Store claim dispatch and the
whole span before M binding remain explicitly unbridged in the census. The
bounded pending group preserves original objects in that span, but later M
registration cannot certify absence of earlier work. Full static-family
coverage, finite R application closure, conserving R-to-S application custody,
pre-Winner ownership, complete startup/cleanup bounds, the production sandbox
and the production launcher remain open. All family coverage stays unbridged;
full M0 and real trials remain ineligible.

Focused task qualification passes **63 distinct cases**: 39 core and 24
daemon, all with zero ignored. Twelve cases are new. Tree `0972b7d2` supplies
60 unchanged cases; tree `82cb40b1` supplies the three strengthened native
survivors. The earlier three survivor passes are excluded from this count.
All production Rust is identical between those trees; the colocated retained-S
case and all other test bodies are unchanged. Historical totals above remain
separate.

| Executed scope | Distinct passes | Actual boundary |
|---|---:|---|
| Core application group | 6 | Original task cancellation/failure, real executor shutdown, actual inline F destruction, poll/destructor panic |
| Core planning bind | 6 | Three isolated M survivors, deterministic bad-bind/no-orphan/refund, existing constructor and continuous-cutoff cases |
| Core finite alias, direct and census | 27 | Existing finite fence (1), direct ownership (12), owner inventory (14) |
| Daemon original controller and static registry | 13 | Original heartbeat/Store (5), carrier/configuration/AdmissionUnknown (8) |
| Daemon native static survivors | 3 | Entry-only, completed-F-only and JobFence-only each retain M under driven cleanup; original M retires after the sole survivor drops |
| Daemon retained S, pre-Winner and direct | 8 | Retained output (1), original dormant drivers (2), existing direct/HTTP ownership (5) |

The core group and planning cases use these focused filters:

```bash
cargo test -p plurx-core --lib --locked \
  --features fixtures,hiqlite-contract-tests,scratch-fault-injection \
  adaptive_resources::runtime::application::tests -- --nocapture --test-threads=1
cargo test -p plurx-core --lib --locked \
  --features fixtures,hiqlite-contract-tests,scratch-fault-injection \
  adaptive_resources::runtime::planning_probe::plan::tests -- --nocapture --test-threads=1
```

The three native variants in
[automatic_static.rs](../../crates/plurxd/src/vod/tests/automatic_static.rs)
use a fixture inline coordinator with the actual selected Store, P/R,
FFmpeg, static entry and retained-S machinery. They verify original Store
closure and rich-entry extraction before isolating a survivor, actively drive
M cleanup while it is held, and require positive original M retirement after
it drops. They do not execute the production `state.rs` background loop end
to end; that concrete F is source-reviewed and compiled. Exact-candidate
format, strict Clippy and workspace-check receipts belong to the task PR.
These scoped results grant no full-suite, M0 or real-trial acceptance.
[Static task #1383](http://forge.lan:3000/noirr/plurx/pulls/1383) merged at
`aa29e01e` after all eight jobs in Effort gate 6477 passed; its original
source-scoped counts remain separate from the driver follow-up below.

## Finite VOD keeps the original driver through every join

Fresh finite-R publication validates the original cleanup arm before creating its
single dormant driver. In one await-free publication step it retains that
actual JoinHandle, activates it and exposes the rendition. Losing builders and
requests cancelled before this step remain genuinely never-spawned. Spawn
holds no installed-driver mutex; even executor shutdown drops only extra Arc
clones of caller-held values, with no rich destructor under the map locks.

The producer distinguishes never installed, pending original, successfully
joined original and failed original. A cancelled join waiter leaves the same
handle in custody. A successful callback is only a daemon observation: actual
task/F destruction can still fail afterward. Such a failed join retains the
same consumed handle and task identity, never polls it again, and refuses on
every retry. Absence is accepted only for an original producer whose driver
was never registered.

Adopt preserves its existing original dormant task. Abandon transfers that
same handle into producer custody without activation and uses the same join
before registering existing finite closing. Native closure, retained S, rich
application references and final refund remain distinct. R-to-S does not join
its own driver. The producer projection uses its actual concrete size, and
the existing driver/cleanup projections measure the changed concrete frames.
Private executor and complete startup/application bounds remain unknown.

**Qualification:** all 13 distinct daemon cases pass, zero ignored, on source
tree `2300c5c3ba2a9bd7ec4048c3cacba0b715f40834`. The two new cases exercise
cancellation of the installed original join and a real completion-destructor
panic after the callback reported return. Repeated failed joins retain the
same original task identity and full unresolved charge. Ordinary retained-S,
original dormant Adopt/abandon, metadata cancellation and all four finite
attempt/refusal cases also pass. Raw logs and all 13 exact commands remain in
the task-owned receipts; counts overlap earlier tasks and are not added to them.

The actual effort landing `aa29e01e` reproduces exactly the same combined tree
as the qualified stacked source. Pinned Rust 1.97.1 formatting, workspace
all-target compile, strict Clippy and three offline formal source-mapping
preflights pass; the mapping checks are not TLC execution or an implementation
refinement theorem. Final candidate checks and the manual Effort gate retain
their own source boundary.

The new regression commands are:

```bash
cargo test -p plurxd --bin plurxd --locked --features cluster-integration-tests \
  vodserve::tests::cancelled_installed_original_driver_join_retains_the_same_dormant_task \
  -- --exact --nocapture --test-threads=1
cargo test -p plurxd --bin plurxd --locked --features cluster-integration-tests \
  vodserve::tests::returned_original_driver_destructor_panic_remains_failed_on_every_join \
  -- --exact --nocapture --test-threads=1
```

These are real selected-store and FFmpeg/FFprobe fixtures: missing tools or
capacity fail explicitly. This task does not complete counted R/S rich payload
destruction, pre-Winner coordinator/abandoned-cleanup whole-F custody, broader
census coverage, full M0 or real-trial eligibility.

## Selected runtime keeps its original census row

Registration precedes journal publication and the common native-entry latch.
The latch checks its current physical binding under the same census boundary.
An exhausted census retains an explicit selector under the existing operation
book and permanently loses positive completeness; ordinary admission keeps its
existing ledger capacity. Overflow lookups publish only after accepted enqueue,
so failed construction cannot accumulate stale bindings.

A committed R→S transfer changes the current binding, family and kind in the
original slot under the same lock as the ledger swap. Its original identity
survives. Observed uncertainty is never cleared by ordinary activation or
retirement. The original recovered selector moves into its imported role;
unsupported or unclassified recovery stays held.

Native absence and application destruction are separate. A private nonclone
`NativeNeverAcquired` comes only from the original sealed graph and its exact
Reserved ledger custody; it cannot construct `PhysicalCloseReceipt`. Sources,
bootstrap tasks, response allocations and application aliases need their own
actual closure. Generic baseline refunds keep a pending original census owner
visible as UnknownHeld until the final unaliased rich bundle is destroyed.
Lost or unsupported proof retains uncertainty. Finite finalization also carries
earlier P close handoffs through the same final bundle destruction, preserving
original proofs on every busy or fallible boundary.

These are selected-runtime hooks. Live transports, recording sinks, escaped
source bodies, background generation, relay and other legacy application
owners still need concrete registration and positive-close adapters. Missing
family coverage and unknown peaks continue to refuse real production trials.

## Prepared work keeps one driver through adoption or abandonment

The pre-Winner owner must retain the original driver handle separately from
route publication. Its registration may precede Adopt while ordinary work
remains dormant. Adopt publishes that same owner and output; it must not spawn
a second driver. Abandonment requests stop, wakes the dormant task, joins the
original handle, then continues the existing native/journal cleanup owner.

A future trial entry must use an opaque authorization backed by complete
containment, inventory and policy evidence, the original preparation cutoff
and `TrialAllocation.stop_by`. It starts without viewer demand or a public
session/route. Only verified `Materialized` output supplies candidate progress.
Abort drains the original bounded channel while joining original root/writer
and application tasks. A lost waiter, replay or failed join cannot grant a
new permit, deadline or close receipt.

## Selected runtime qualification — 51 distinct core cases

The selected runtime bridge is built and has scoped core qualification on
2026-11-03. Core production sources are identical between source trees
`6ee933307bf59d4b8ae656c9aa62ca4b22b0d2df` and
`ca85f07f995da9938e607244fb200e27e2ff7fcf`; the latter changes three fixtures
only. The parent qualification ran pinned Rust 1.97.1 on Linux with
`fixtures,hiqlite-contract-tests,scratch-fault-injection`. Behavioral runs used
protected repository-local temporary storage after the default temporary
location correctly refused custody setup as `UnsupportedStorage`.

| Executed scope | Distinct passes | Evidence boundary |
|---|---:|---|
| [Owner census](../../crates/plurx-core/src/adaptive_resources/owner_inventory/tests.rs) | 14 | Original ownership, references, overflow and serialized admission |
| [Runtime operation book](../../crates/plurx-core/src/adaptive_resources/runtime/operations/tests.rs) | 29 | Includes all six new [inventory bridge cases](../../crates/plurx-core/src/adaptive_resources/runtime/operations/tests/inventory.rs); seven baseline exceptions remain below |
| [Actual software retirement/readers](../../crates/plurx-core/src/adaptive_resources/runtime/finite_vod/tests/retirement_readers.rs) | 7 | Includes the new original-identity R→S case and final empty census after actual application/native closure |
| [Actual journal reopen](../../crates/plurx-core/src/adaptive_resources/runtime/tests.rs) | 1 | Original durable-keyed selectors are UnknownHeld before native imports |

These are 51 distinct passing cases, including all seven new regressions.
The new bridge cases exercise registration before the native latch, dropped
selection and waiter custody, a remaining rich-bundle alias, lost original
tokens, ordinary admission after census exhaustion, and later-construction
refusal without stale overflow lookups. The actual software case conserves one
original identity through R→S and retires it only at final closure.

Seven existing multi-role bookkeeping fixtures still fail at unknown-producer
overlap admission. Every failure was reproduced individually as zero passed,
one failed on exact effort baseline
`edb5b79ff902b7eea5fe95d8954b1e5af99a5752` under the same protected temporary
storage. Their source and the governing refusal remain unchanged. They cover
three preparation/successor cases, two application preflight cases, the
second-role overage latch, and dropped typed multi-role preparation. They are
recorded exceptions, not passing evidence or permission to relax unknown
memory/task policy.

Two other historical fixture failures were reproduced on that baseline and
corrected here: capacity refusal now advances both cap and demand to epoch 2;
duplicate settlement now asserts the existing refunded-owner rejection before
journal entry. Both corrected cases pass. The new overflow-construction case
also passes after using the admitted P/M shape, retaining P's unknown peak and
a heap-only M, so its intended invalid second attempt ID is reached. None of
these fixture corrections changes production policy.

Three affected daemon cases pass on `ca85f07f`: same-original-driver Adopt,
real finite lifecycle (79.10 s native body; 81.97 s invocation), and real
process restart (85.38 s native parent body; 87.85 s invocation). Restart
includes a separate successful child invocation, not a fourth distinct case.
The earlier daemon results below retain their earlier source boundary.
This qualification does not close full inventory coverage, M0, complete native
and allocator bounds, production trials, V1–V5, or Main promotion.

## Qualification records distinct boundaries

The native lab uses a task-owned transient delegated scope beneath an explicit
18 GiB/no-swap/four-CPU ancestor. The control process moves to its own child
before enabling memory/pids for workload children. The actual limit readbacks
are verified; no build runs uncapped on a voter. Committed source reaches the
pinned Rust 1.97.1 compiler through source-only archives.

Nine daemon cases pass on staged source tree
`c45428b43d1010f6aa8eb94afb40c65aa7951ce4`, zero failed/ignored. They include
three new original-driver cases (same task through Adopt, cancelled preparation
and cleanup waiter, aborted task retains full charge), four affected
Adopt/fence cases, real finite lifecycle (80.90 s including invocation) and
graceful restart (87.76 s including invocation). Those four daemon source files
are unchanged on the committed native candidate.

On `fd81da243a3d8d1012ca9a0bf0686ec884d54243`, all 35 focused core cases pass:

| Scope | Passed | Evidence boundary |
|---|---:|---|
| Owner census | 14 | Includes callback Drop/unwind, finalization, references, overflow and atomic reservation |
| Domain validation/custody | 8 | Synthetic refusals/ownership and bounded control parsing |
| Sealed launcher validation | 3 | Argument capacity and original pidfd status handling |
| Actual delegated kernel | 10 | Empty/removal/reuse; atomic birth; fork/thread refusal; private OOM; surviving `setsid` descendant; dropped owner, exec failure and pre-clone refusal |

The ten kernel cases ran explicitly with `--ignored`, each reporting zero
ignored tests under verified ancestor caps; ordinary fixture-file success is
not substituted for enforcement. The private child entry runs only through
the pinned executable owned by those parent cases. All 44 selected core/daemon
cases pass, zero failed/ignored, at their stated sources.

Pinned workspace/all-target strict Clippy, formatting, catalog and JavaScript
syntax pass; task paperwork passes 14/14. Independent source reviews cover the
domain/launcher, census and driver boundaries. Task
[PR #1375](http://forge.lan:3000/noirr/plurx/pulls/1375) merged into the effort at
`edb5b79ff902b7eea5fe95d8954b1e5af99a5752` after all eight jobs of Effort
development gate 6437 passed. Its landing tree matches the qualified candidate.

Kernel enforcement, ownership regressions and exact-source compiler results
are recorded separately. Authored fixture cases do not establish
production coverage, startup/cleanup bounds, M0 acceptance or trial eligibility.
The effort remains draft; V1–V5 and Main promotion remain open.

## Direct retirement follow-up — original selectors replace permanent aliases

**Status:** built amendment to [PR #1377](http://forge.lan:3000/noirr/plurx/pulls/1377);
scoped core/daemon qualification passes. The PR records exact-candidate checks,
the required Effort development gate and landing. The earlier 51-case heading
and source boundaries above remain unchanged.

On exact committed source `7658db4c4fdb5027a6283bafa00018c121ed6ec5`, the
[actual direct-source module](../../crates/plurx-core/src/adaptive_resources/runtime/direct_source/tests.rs)
ran five cases: two passed, three failed, zero ignored. The failures covered
last-source/domain retirement, a cancelled original read waiter, and bounded
chunk retirement. Two actual daemon cases also failed: original restart
admission moving to the retained direct ticket, and unpublished direct-open
failure releasing its own source reservation. Both retained one cleanup ticket
where none should remain. The original read-join-before-End daemon case passed.
These failures are this task's direct-finalization blocker, separate from the
seven unchanged baseline overlap exceptions above.

The direct retirement ticket cloned its preparation into finalization while
retaining that same preparation. The shared read domain retained another
permanent clone. Actual native and journal settlement could therefore finish,
but final `Arc::try_unwrap` could never consume the rich bundle. Retrying the
same ticket could not remove those permanent aliases.

Source tree `4cb874ba40ed4f607138e271eb2d113f3cec7df8` changes only
[the runtime finalizer](../../crates/plurx-core/src/adaptive_resources/runtime/operations.rs),
[direct-source ownership](../../crates/plurx-core/src/adaptive_resources/runtime/direct_source.rs)
and its actual-read tests. The synchronous owned-selector finalizer consumes
an `Option<OwnedPreparation>` and restores the same original selector on Busy
or error. A live source owner still retains its alias, so rich worker ownership
continues to block premature destruction.

The first source shares the domain's bundle. Its ticket drops only the
redundant selector after known source settlement; the domain retains the
original until its last member closes. The last ticket takes that original,
releases domain/registry locks, then finalizes without an intervening await.
A missing selector with no committed completion flag means Busy, never absence.
An explicit completed-domain flag lets another retired source finish its own
remaining bundle even after the registry entry disappears. Original physical
proofs, application closure, rich bundle destruction and matching ledger
absence still precede census removal.

The two new actual-read regressions cover restoring the original domain
selector after busy finalization, and a source retry after another member
finishes the shared domain in the opposite closing order. Existing actual-read
cases now also assert census retention while an application or original read
is held, and an empty census only after final closure. Source review found no
blocking issue. All 21 focused core cases pass on `4cb874ba`: seven actual
direct cases, six inventory cases, six binding/typed/settlement callers and
two finite-initialization fences. Twelve repeat affected earlier cases; the
source-scoped distinct core total is 60. The pinned test build passes in
2 min 56 s. All three affected daemon Direct cases also pass on `4cb874ba`, including
the two reproduced retained-ticket failures. Combined source-scoped evidence
has 60 distinct core and six daemon passes: 24 amended-source cases and
42 retained earlier cases. Exact-candidate compiler checks and the required
Effort development gate are recorded in the task PR.
Full inventory/M0, producer bounds, trials and Main promotion remain open.
