# Adaptive owned containment — what still stands before real trials

**Status:** open · implementation and native qualification in progress ·
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
bridge needs census → operations book → ledger lock order, a compact original
close-proof handoff before rich bundle destruction, and an atomic conserving
R→S rebind. Closing R and registering a separate S row would create an absence
gap. Recovered obligations must register as held before native imports open.

A snapshot has an epoch and explicit unbridged-family coverage. Admission must
hold the same mutation lock through the actual reservation and new original
registration. Overflow, stale epochs, unavailable observation or any unbridged
family refuses positive completeness. Production coverage has no generic
boolean constructor. Each adapter must establish its own real entry and final
close boundary before obtaining coverage.

| Affected work | Original owner that must supply entry/close integration | Current census bridge |
|---|---|---|
| Selected planning, R, retained S, cleanup and UnknownHeld | Core runtime `Operations.bundles → OwnedRole`; original reservation and final physical/application settlement | Open |
| Buffered/shared finite VOD | `Shared.renditions`, unpublished prepared owners and `finite_closing`; one producer across viewers | Open |
| Rolling and continuous output | Transcode sessions, retired presentations/copy tickets and original rolling retirement settlement | Open |
| Live transport | `LiveTvRegistry`, original DVR transport task and each viewer feed | Open |
| DVR recording sinks | Original writer task through stop, fsync and actual join; absence from activity UI is insufficient | Open |
| Source actors and escaped response bodies | `SourceWorkerRegistry`, direct tasks, viewer owners and `SourceResponseGuard` | Open |
| Local direct reads | Fixed `LocalDirectOwners` Active/Settling slots and core `DirectReadDomain` jobs | Open |
| Background static generation | Original `StaticArtifactRuns` slot through planning, admission uncertainty and closing | Open |
| Preparation and retained output | Preparation storage, retained artifacts, rolling registry and active cache readers | Open |
| Relay, network/source domains and legacy helper graphs | Original relay/body, audio/subtitle/probe and processed-output owners | Open |

These rows identify required hooks; they are not assertions that those hooks
are installed. Copying current serving maps or a truncated capture observation
cannot establish absence. CPU and RAM initially share a whole-node domain;
narrower device, source and storage domains require their own backed identity.

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
graceful restart (87.76 s including invocation). Native domain/census results
remain pending; that earlier driver snapshot does not qualify their later
local refinements.

Kernel enforcement, ownership regressions and exact-source compiler results
are recorded separately. Authored fixture cases do not establish
production coverage, startup/cleanup bounds, M0 acceptance or trial eligibility.
The effort remains draft; V1–V5 and Main promotion remain open.
