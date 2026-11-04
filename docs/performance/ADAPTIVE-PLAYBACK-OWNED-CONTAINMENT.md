# Adaptive owned containment — what still stands before real trials

**Status:** open · scoped implementation qualified; production coverage open ·
2026-11-03. Companion to the [production policy](ADAPTIVE-PLAYBACK-PRODUCTION-POLICY.md)
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
| Buffered/shared finite VOD | [Shared finite build slot](../../crates/plurxd/src/vod/shared.rs): original unpublished producer/output before the first post-R await | `drive_finite_closing`: join original driver, drain allocations/application aliases, finish same close ticket | Open |
| Rolling and continuous output | [Transcode manager](../../crates/plurxd/src/transcode.rs): original session/producer attempt before publication | [Rolling retirement](../../crates/plurxd/src/transcode/rolling/retirement.rs): retired presentation retains its original session; [compact copy-ticket maintenance](../../crates/plurxd/src/transcode/manager/maintenance.rs) joins original actor and settles same native ticket | Open |
| Live transport | [DVR transport](../../crates/plurxd/src/live_tv/dvr.rs): one original tuner worker; separate viewer feed queues | `close_transport_arc` and actual tuner/body join, plus each viewer pump/feed destruction; its `closed` signal alone is insufficient | Open |
| DVR recording sinks | [DVR sink](../../crates/plurxd/src/live_tv/dvr.rs): original sink before `start_sink_writer` | `sink_writer_loop` file write/flush/fsync and `settle_sink` joining the original handle; activity-row removal supplies no proof | Open |
| Source actors and escaped response bodies | [Source factory](../../crates/plurxd/src/transcode/source_actor.rs): original registry owner before spawn; original response reference before body publication | Original physical/preparation settlement and every `SourceResponseGuard`; [Source direct task registry](../../crates/plurxd/src/transcode/source_direct.rs) retains Active/Settling/Failed custody | Open |
| Local direct reads | [Local direct open](../../crates/plurxd/src/transcode/local_direct.rs): fixed 64-slot claim, core preparation and original worker before native entry | `reconcile_local_direct_owners`: join Active worker, destroy rich entry/application lease, retain Settling ticket until final retirement; [core direct domain](../../crates/plurx-core/src/adaptive_resources/runtime/direct_source.rs) owns the actual reads | Open |
| Background static generation | [Static run reservation](../../crates/plurxd/src/transcode/background_static.rs): original controller slot before P native awaits | `reconcile_static_artifact_run`: retain AdmissionUnknown/Closing, original controller receipt and actual rich payload destruction before application lease release | Open |
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

### Application references need their own sealed final-close mechanism

The next implementation slice is local direct and Source direct application
ownership, then static background controllers. The selected core bridge already
registers native roles created by these typed preparations. Daemon application
hooks must reference those original identities, rather than count a second
producer. `OwnerInventory::register_reference` currently creates a reference,
but its physical-receipt close method cannot retire it: a reference deliberately
has no producer physical binding. Add a sealed application-close mechanism
backed by the original owner, actual rich payload destruction and original
worker join. A copied UUID, registry removal, dropped waiter or producer receipt
cannot mint this authority.

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
