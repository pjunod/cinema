# Adaptive playback — finite delivery closure

**Status:** open · implementation incomplete · updated 2026-10-22 at committed checkpoint `765179517fd559d2220f4fcff2704b0821fafb55`.
Primary owner: direct Codex harness (`/root`; actual model unspecified). The former
Hermes Sol task `t_161f6652` is parked and its worker exited; its heartbeat remains
paused. Work remains on `effort/adaptive-playback-resources`, draft PR #1248.
Primary is the sole integration writer; lower-model assistants may take bounded,
non-overlapping tasks. This is a delivery breakdown of the existing
[approved M0–M7 plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md#10-delivery-sequence--integrate-one-effort-prove-each-seam),
not new design, feature scope, a framework or another review gate.
The [source/evidence ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) retains pins.

## Authoritative first ordinary VOD source checklist

All five items are **OPEN** at `765179517fd5`. The immediate target is one ordinary
local-file software VOD request on supported Linux, using a fresh rendition in
the existing configured root. Source-complete is not behavior-verified.

| Item | Actual missing production connection / binary closure |
|---|---|
| V1 — admission/source — OPEN | `vod/shared.rs::build_rendition_with_storage` must enter the same finite preparation before any source/directory/init/marker acquisition and carry its actual held source to generation. No selected-path legacy pre-acquisition remains. |
| V2 — held output/durability — OPEN | `7b783effa` and `735387e96` retain the held child and selected-ID publisher. The concrete finite request carries an immutable `SoftwareAvcCommand` and pump plan in the same reserved bundle before I/O, checks source identity/path/scanner values, prearms source cells without a lease, and uses the predeclared root acquisition ID. The same retained `File` crosses spawn; after known launch, source-copy observation is enriched, then the retained pump task is observed and acknowledged. The bounded Segmenter prefix/merge/finish split/published batch now flows through `GenerationMemoryLimits`/`Generation`; init parsing has a four-copy bound, parser/read/inventory allocations and plan-boundary vectors are fallible, and canonical grid/producer-clock alignment is compiled. These components do not establish an end-to-end quota: parser metadata, native process, launcher and runtime-journal memory remain unresolved. Known durable Observe ends pending-I/O payload accounting; actual payload is cleared and the same directory returned under the original bundle lock before another queue operation can enter. The whole role reservation remains held until retirement. Ordinary `build_rendition_with_storage` still uses legacy admission. |
| V3 — producer/stop — OPEN | Core now joins the same retained writer/root outside the publication bundle lock and quiesces already-registered native jobs. Stop aborts the original task through its retained handle, including late registration; result summaries do not move the graph-owned diagnostics. Observed parent source descriptors close once, but inherited-copy/fixed-process/partial-start closure and all charges remain held. Ordinary generation must carry the same source, original FFmpeg and writer jobs through one retained normal/cancel terminal owner. Actual joins/drains/closure precede settlement; ambiguous owners remain held. |
| V4 — retained output — OPEN | Compact V3 frozen references, a proof-bearing atomic SQLite R→S mutation/recovery and a post-COMMIT same-ledger swap primitive are authored; production native retirement/S ownership is not connected. Ordinary HLS bodies hold actual accounted file/buffer leases. Known producer retirement conserves all retained parent/child/files/readers through ONE journal R→S transaction and same-ledger swap; output remains usable/charged while producer capacity returns. |
| V5 — terminal/reuse — OPEN | Ordinary completion/cancel/error cleanup consumes exact known settlement once; the next ordinary admission can use returned capacity. Unknown commit retains original operation/custody; eviction waits actual readers. |

Current sequence: wire this finite runtime composition into ordinary
`build_rendition_with_storage`, then close V1/V3; proceed to V4
serving/transfer → V5 settlement/reuse. All V1–V5 remain OPEN; no source item
has newly closed. The
detailed execution artifact remains the existing
`operator/FIRST-VOD-DELIVERY-CHECKLIST.md` under
`~/.hermes/hdr-collaboration/workspaces/adaptive-playback-parallel-20261021/`,
SHA256 `9908a6a60e0b2bce4dfa509d181d96a3459cb065aa45309bb83daf8b58e9c92d`.
It specifies the concrete failure, existing gap, smallest change, prerequisites
and binary closure for each item; this section is the sole product-doc summary.
No new approval/pause gate, backend/framework/security prerequisite or scope
cancellation follows from the list. Any new dependency must identify the blocked
V item, concrete ordinary-request failure and smallest necessary addition.

R1 (separate recovery integration): conservative restored holds/protected cleanup
are mandatory now; selective startup/reconciliation and positive exact old-domain
completion remain required before final acceptance. A universal restart transport
is not a prerequisite for connecting the ordinary live path. Later generality
remains all other agreed M0–M7 recipes/pipelines, policy, worker/placement,
platforms and operations below. Acceptance-only remains complete integration →
independent actual Fable review/fixes → once-only affected qualification/fast lane
→ authorized merge. No pre-review behavioral tests or deployment.

## Active integration — 2026-10-22

Core exposes a concrete
[`finite_vod` runtime interface](../../crates/plurx-core/src/adaptive_resources/runtime/finite_vod.rs):
`SoftwareVodOwner` reserves the whole role before source or output acquisition;
nonserde `FiniteOutput` selects that same custodian and returns opaque tickets
for retained publications. The core-owned VOD generation engine now includes
identity and conversion modules; the daemon re-exports it while keeping its
existing test bodies. The explicit `PublishMetadata` operation records
opted-in `identity.json` and software-transcode marker output; copy omits the
marker. A same-child sync cell keeps its original job through known
Arm/Observe and limits batches to 16. Parser input is preflighted against
track/run/sample limits; `Init`, `Fragment` and `Segmenter` capacities and
bounded-generation accumulation/transfer are accounted. Exact registered-child
signals use the original registration and pidfd, not a numeric PID lookup, and
introduce no root-wait-lock dependency. The graph retains the original concrete software pump task, actual pipes and
diagnostics; the adapter does not grant a physical-close receipt. The finite
request requires `SoftwareAvcCommand` and `SoftwareAvcPumpPlan` before I/O and
checks source identity/path/scanner sizes against command inputs. It resolves
and prearms source cells without a lease, then uses the predeclared root copy ID
and same retained `File` through spawn. Only after actual root launch does it
stage source-copy enrichment; it arms/joins the retained pump and acknowledges
its typed task observation once. A cancelled graph-lock wait leaves the root
entry latch untouched, and the pump remains in the bundle until registration.
Shared-fd helpers are extracted with daemon re-exports preserving behavior. The
bounded AVC merge artifact preflights workspace/output before allocation but is
not connected to Segmenter or `GenerationMemoryLimits`. Existing
[`SourceFence`](../../crates/plurxd/src/fragment_index_cluster.rs) and daemon
[`PendingWrites` adapter](../../crates/plurxd/src/renditiondir/finite_publication.rs)
remain available. Ordinary `build_rendition_with_storage` still selects legacy
admission; full memory partitioning remains incomplete. This is runtime
composition, not ordinary source closure.

All V1–V5 remain OPEN. Ordinary request entry and the production capacity
connection, full memory profile, serving leases, retained-output transfer,
conserving retirement and exact settlement/capacity reuse still lack their
production connections. The
[source ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md#direct-harness-integration--2026-10-22)
records the limits. The latest committed base is
`765179517fd559d2220f4fcff2704b0821fafb55`. The frozen sixth-checkpoint source
passed pinned Rust 1.97.1 host core+daemon all-target compilation in 132.333s
with stable hashes; the source ledger records the exact receipt and the existing
root-constructor field fixed before this pass. This is warning-level compilation.
Host-eligible authored cases compiled; Linux-only bodies and Windows remain
outside that evidence. No behavioral tests or review ran for this slice.
Review and qualification wait for the complete integrated candidate.

## Actual distance to usable playback

Ordinary production playback still uses legacy admission. The new policy,
accounting, persistence and Store components exist, but they are not a completed
adaptive playback feature. No M0–M7 milestone is claimed accepted end-to-end.
There is no defensible percentage-complete or merge-date estimate from compiler
passes, source lines or authored test counts. Significant integration remains,
not just final tests. The table below distinguishes reusable source already
implemented from the missing deliverables that must close.

The direct Codex harness (`/root`) is the primary and sole integration writer
in the same integration checkout; its actual model is unspecified. Lower-model
assistants may handle bounded, non-overlapping tasks. The former task
`t_161f6652` is parked, its worker exited, and its heartbeat remains paused.
Actual independent Fable 5.1 still owns final adversarial review.
No source-ownership transfer or additional permission gate is introduced.

| Milestone / deliverable | Source already implemented | Missing source deliverable and dependency | Definition of done / observable application behavior | Owner |
|---|---|---|---|---|
| M0 — coverage/contracts | Spawn/resource coverage table; process identity and ledger types; settings writer/import contracts; local journal and M4 request/attempt schemas. | Finish the existing coverage table by mapping each production owner to its concrete M3 adapter and exact regression; reconcile implemented M4 wire/runtime with the approved contract. Depends on the adapters/runtime it inventories, not another repository-wide search campaign. | Every planned pipeline has one identified admission/acquisition/retirement owner and resource domain; no silently excluded spawn, sink or relay. This closes coverage, not playback capacity by itself. | Primary Codex harness (`/root`); resolve only a specific unresolved contract. |
| M1 — observations/diagnostics | `process/observation.rs`, launcher observations and `resource_observation.rs` provide identity-bound Known/Unknown counters/service windows. | Complete production progress/phase sampling and diagnostic projection for each M3 adapter; bind overhead/platform limits to actual owners. | Operator can see current node/workload CPU, memory, progress and Unknown reasons; pacing is not misreported as low encode capacity. Platform/overhead acceptance remains M7. | Primary Codex harness (`/root`). |
| M2 — policy/settings/learning | `adaptive_resources.rs` has atomic vectors/families/retained roles; `resource_history.rs` bounded priors; `resource_policy.rs` advisory trial/deadline/cooldown logic; `resource_settings` and `store/planning_capacity.rs` preserve settings provenance/authority. | Wire these components into the live node owner and production planning/admission; feed real service samples/epochs, persist useful history, apply current hard bounds and four-state settings without rewriting saved intent. No daemon `ResourceHistory`/trial-policy consumer was found by the bounded current source query; helper existence is not integration. Depends on M1 and finite M3 owners. | A normal request uses current hard caps and genuine measured evidence; Automatic can learn safely, explicit/legacy caps and both zero meanings retain their contract; failure cooldown and paced workloads cannot invent headroom. | Primary Codex harness (`/root`). |
| M3a — critical custody/runtime backend | Dedicated node-local journal, lock-bound startup/reconstruction, retained prepare/arm/seal/settlement book, actual root/source/discard-pipe ownership; Windows LIVE original-Job proof plus restricted private local-graph receipt. | C1–C3 below: supported Linux pre-entry/nonescape/object-continuity recovery; exact runtime manifest/member settlement; real production source/writer/scratch adapters and retirement. Current Windows storage/restart unsupported and current Unix cleanup UnknownHeld do not satisfy this. | One finite producer can actually start, stop, commit settlement and free capacity; restart retains/reconciles its original obligations without deleting protected work or leaking every normal slot forever. | Primary Codex harness (`/root`); use existing authorized native compiler/environment evidence. |
| M3b — all pipeline adapters | VOD/rolling supervisor joins and custody bridges, bounded overlap/family borrowing, real source handles and pipe task helpers are present. Ordinary launches still remain legacy. | After the first finite path: connect finite/rolling/continuous and processed helper graphs, audio/subtitle/copy/cached/direct/range, Live/DVR shared transport plus each sink, fragment/index/background work and ingress relay; replace whole-budget background holders with real yield/retirement; preserve shared-role transfer and paced/producing phases. Depends on M3a plus M1/M2. | Every M0 row uses the service, counts each retained/shared owner once and retains costs until its real terminal event; foreground playback can reclaim background capacity without stealing live work. | Primary Codex harness (`/root`). |
| M4 — worker runtime (Store is delivered) | Integrated SQLite/Hiqlite attempt/request/domain transactions, Prepared/winner/adoption fences, legacy-writer guards and Source activation guards. | Implement worker Prepare/Adopt/cancel/status handlers and capability advertisement; reconstruct actual source/executable/credit/admission locally, retain cleanup ownership, recheck serving/drain/epoch/winner before publication/activation. Compose ordinary and processed output. Depends on local M3a/M3b owners; Store rows alone are not remote workers. | A same-quality remote preparation really runs on an eligible worker, only one exact winner adopts, losers retain physical cleanup, old peers keep legacy behavior and cannot bypass new authority. | Primary Codex harness (`/root`). |
| M5 — placement/delivery domains | Advisory policy contains peer-first/common-deadline concepts; M4 Store supplies bounded attempt/domain claims. These are prerequisites, not dispatch. | Connect offers/ranking and bounded same-quality peer retry before quality fallback; spend one original deadline including activation reserve; bind NAS/shared source and ingress/relay commitments, partition/mixed-peer/drain decisions to actual owners and telemetry. Depends on M4 and M1/M2/M3 accounting. | Local-full/peer-idle requests preserve quality and incumbent playback; shared storage/relay saturation or drain refuses misleading capacity instead of moving only the processing label. | Primary Codex harness (`/root`). |
| M6 — operations/client explanations | Custody/source evidence docs and existing settings authority/API contracts are present. | Expose normal settings and read-only readiness/Unknown/fallback/placement explanations, real drain blockers and compatible client diagnostics; complete API/operations references. Depends on actual M2–M5 behavior, not a new feature toggle. | Operator sees why a request stayed local, moved, waited or fell back; drain does not report reboot-ready while prepared/losing/relay owners remain. | Primary Codex harness (`/root`); same draft PR #1248. |
| M7 — final review/qualification/landing | Source-bound compile receipts and authored regression anchors; draft-paperwork checks only. No implementation behavioral test batch or completed-candidate review yet. | Finish implementation, actual independent final Fable review, fix findings, then once-only affected tests/fast lane and applicable native/cluster/platform acceptance; merge authorized exact candidate. Retry only failed/invalidated checks. | A reviewed, actually qualified artifact is merged with explicit platform/evidence limits. Compilation/body counts never replace this. No deployment is authorized. | Fable independent review; primary Codex harness (`/root`) fixes/qualification and authenticated publication/merge. |

## Critical path and next vertical closure target

The next user-visible vertical target is deliberately narrow and already in M3:
**one ordinary software finite VOD producer using durable finite admission through
normal completion/cancel and retained accounting, with its capacity reusable.**
It is not all pipelines, a remote worker, a new platform promise or speculative
growth. It cannot be declared done by adding another private proof helper.

| Step | Exact source work still required | Completion condition / dependency |
|---|---|---|
| C1 — supported owned-workload cleanup and recovery | Final C1 amendment accepts trusted fixed-software/known-helper normal ownership, without hostile arbitrary-descendant claims or a custom broker. Implement that concrete graph; preserve uncertainty, write-ahead and all real owners. | Normal actual waits/joins/FD/sink closure or transfer; restart needs positive exact old-domain completion, not a restart request/new namespace. Current managed-runtime evidence transport remains missing; normal source work does not wait for it. No host setup implied. |
| C2 — real graph/member settlement | `f51a5475f` connects typed preparation and exact scratch member/ArmIo commands to the same retained runtime book, applies local folds only after known commits and refuses early legacy proof consumption. Bounded event/reader audit remains. Missing: actual native bootstrap/outcomes, bounded native/index-memory owners, overage correction, snapshot/RetainedAssetsV2, private proof/adoption and atomic R→S transaction/swap. | Host-COMPILED NOT RUN, not C2 accepted. Data folds cannot mint native authority. Every actual owner is closed or transferred before the same-ledger R→S swap; no omitted holder. |
| C3 — connect the existing VOD owner | Concrete actor/terminal/parent-source ownership and original moved-payload/registration latch remain. `1584a357c` adds actual Unix held-directory/no-follow/exclusive-temp/no-clobber publication and retained partial-object facts; `a4465e4b0` preserves drain errors, `521a6d5fb` authors actual-native cancellation. Next: already-charged prearmed storage-root/init bootstrap, scratch-v2 write-ahead/native outcomes, source/copy/HTTP leases, bounded native/index memory and current-cap/settings/shared-role demand. | The actual application request, not a fixture-only branch, reserves once, produces output, stops and returns its finite capacity. Admission STILL LEGACY; local native facts are not finite authority. C1/C2 and conserving R→S remain. |
| R1 / C4 — restart availability and acceptance (separate from first live-path connection) | Replace the all-startup pending refusal in `resource_runtime.rs` with protected selective reconciliation and applicable admission refusal; keep unrelated serving safe. Author exact normal/cancel/crash/restart/capacity-reuse cases now; execute required behavioral cases only after complete candidate review/fixes. | Conservative restored holds are mandatory. Positive old-domain completion and selective recovery remain required before final acceptance, not a universal restart-backend gate on V1–V5. Acceptance needs actual applicable post-review evidence. Other M3 adapters and M4–M6 remain separate rows. |

The final C1/SourceFile-v2/retained-output and first-root bootstrap contracts are
delivered, not blanket design gates. `bb4979850` binds source-copy/task plans to the
actual runtime graph before prepare; `394498d0f` implements strict V3 first-root
audit plus shared output-policy source. `d9f8f82ca` wires V3 manifest/journal/history
and retained known-result runtime. `7c51011f1` owns an immutable configured-root
binding and actual prearmed namespace-root job/fd; `7b783effa` extends that SAME
owner through real component/child/known-outcome composition. `735387e96` adds
held-directory native publication input, not the ordinary journal/caller bridge.
Next is that actual source/sink/readers bridge and conserving retirement; no production
settlement or finite admission is claimed. These are source gaps, not test gates.
Adaptive policy does NOT require a
custom broker. Trusted owned-workload cleanup and crash accounting remain;
completed exact PID-namespace teardown can close its process domain, but a restart
request/new namespace cannot. No service/security/configuration/deployment change.
Same-host restart target means same-kernel-boot daemon restart with proven original
object/owner continuity; reboot/rollback/foreign-domain recovery remains unsupported.

## Why Windows LIVE advanced before Linux/restart

The source already had a Windows suspended-before-assignment, no-breakaway Job
launcher, so its missing live terminate/query and local owner composition were
implementable against an actual existing native object contract. Genuine SDK-bound
cross compilation became available and exposed real source errors, which were
fixed. Those receipts establish compilation only; no Windows behavior was run.

Linux's positive cleanup/restart backend is not implemented here. The final
trusted-workload amendment supersedes the broker proposal; source still must own
and close every actual helper/task/FD/sink. Windows' unnamed Job and unsupported
durable-storage path do not prove restart recovery. The Windows slice remains a
source prerequisite, not usable finite playback or a milestone.
C1–C4 remain the explicit route to the first usable finite path.
