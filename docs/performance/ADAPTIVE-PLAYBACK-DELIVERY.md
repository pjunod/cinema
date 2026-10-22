# Adaptive playback — finite delivery closure

Status: implementation incomplete. Owner: Sol 6.1 (`hdr-sol`, `gpt-6.1-sol`,
`openai-codex`), task `t_161f6652`, branch `effort/adaptive-playback-resources`,
draft PR1248. This is a delivery breakdown of the existing
[approved M0–M7 plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md#10-delivery-sequence--integrate-one-effort-prove-each-seam),
not new design, feature scope, a framework or another review gate.
The [source/evidence ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) retains pins.

## Actual distance to usable playback

Ordinary production playback still uses legacy admission. The new policy,
accounting, persistence and Store components exist, but they are not a completed
adaptive playback feature. No M0–M7 milestone is claimed accepted end-to-end.
There is no defensible percentage-complete or merge-date estimate from compiler
passes, source lines or authored test counts. Significant integration remains,
not just final tests. The table below distinguishes reusable source already
implemented from the missing deliverables that must close.

All implementation rows are owned by Sol in the same integration checkout.
The controller owns compiler transport/publication and coordinates specific hard
design questions; actual independent Fable 5.1 owns final adversarial review.
No source-ownership transfer or additional permission gate is introduced.

| Milestone / deliverable | Source already implemented | Missing source deliverable and dependency | Definition of done / observable application behavior | Owner |
|---|---|---|---|---|
| M0 — coverage/contracts | Spawn/resource coverage table; process identity and ledger types; settings writer/import contracts; local journal and M4 request/attempt schemas. | Finish the existing coverage table by mapping each production owner to its concrete M3 adapter and exact regression; reconcile implemented M4 wire/runtime with the approved contract. Depends on the adapters/runtime it inventories, not another repository-wide search campaign. | Every planned pipeline has one identified admission/acquisition/retirement owner and resource domain; no silently excluded spawn, sink or relay. This closes coverage, not playback capacity by itself. | Sol; coordinator resolves only a specific unresolved contract. |
| M1 — observations/diagnostics | `process/observation.rs`, launcher observations and `resource_observation.rs` provide identity-bound Known/Unknown counters/service windows. | Complete production progress/phase sampling and diagnostic projection for each M3 adapter; bind overhead/platform limits to actual owners. | Operator can see current node/workload CPU, memory, progress and Unknown reasons; pacing is not misreported as low encode capacity. Platform/overhead acceptance remains M7. | Sol. |
| M2 — policy/settings/learning | `adaptive_resources.rs` has atomic vectors/families/retained roles; `resource_history.rs` bounded priors; `resource_policy.rs` advisory trial/deadline/cooldown logic; `resource_settings` and `store/planning_capacity.rs` preserve settings provenance/authority. | Wire these components into the live node owner and production planning/admission; feed real service samples/epochs, persist useful history, apply current hard bounds and four-state settings without rewriting saved intent. No daemon `ResourceHistory`/trial-policy consumer was found by the bounded current source query; helper existence is not integration. Depends on M1 and finite M3 owners. | A normal request uses current hard caps and genuine measured evidence; Automatic can learn safely, explicit/legacy caps and both zero meanings retain their contract; failure cooldown and paced workloads cannot invent headroom. | Sol. |
| M3a — critical custody/runtime backend | Dedicated node-local journal, lock-bound startup/reconstruction, retained prepare/arm/seal/settlement book, actual root/source/discard-pipe ownership; Windows LIVE original-Job proof plus restricted private local-graph receipt. | C1–C3 below: supported Linux pre-entry/nonescape/object-continuity recovery; exact runtime manifest/member settlement; real production source/writer/scratch adapters and retirement. Current Windows storage/restart unsupported and current Unix cleanup UnknownHeld do not satisfy this. | One finite producer can actually start, stop, commit settlement and free capacity; restart retains/reconciles its original obligations without deleting protected work or leaking every normal slot forever. | Sol; controller supplies existing authorized native compiler/environment evidence. |
| M3b — all pipeline adapters | VOD/rolling supervisor joins and custody bridges, bounded overlap/family borrowing, real source handles and pipe task helpers are present. Ordinary launches still remain legacy. | After the first finite path: connect finite/rolling/continuous and processed helper graphs, audio/subtitle/copy/cached/direct/range, Live/DVR shared transport plus each sink, fragment/index/background work and ingress relay; replace whole-budget background holders with real yield/retirement; preserve shared-role transfer and paced/producing phases. Depends on M3a plus M1/M2. | Every M0 row uses the service, counts each retained/shared owner once and retains costs until its real terminal event; foreground playback can reclaim background capacity without stealing live work. | Sol. |
| M4 — worker runtime (Store is delivered) | Integrated SQLite/Hiqlite attempt/request/domain transactions, Prepared/winner/adoption fences, legacy-writer guards and Source activation guards. | Implement worker Prepare/Adopt/cancel/status handlers and capability advertisement; reconstruct actual source/executable/credit/admission locally, retain cleanup ownership, recheck serving/drain/epoch/winner before publication/activation. Compose ordinary and processed output. Depends on local M3a/M3b owners; Store rows alone are not remote workers. | A same-quality remote preparation really runs on an eligible worker, only one exact winner adopts, losers retain physical cleanup, old peers keep legacy behavior and cannot bypass new authority. | Sol. |
| M5 — placement/delivery domains | Advisory policy contains peer-first/common-deadline concepts; M4 Store supplies bounded attempt/domain claims. These are prerequisites, not dispatch. | Connect offers/ranking and bounded same-quality peer retry before quality fallback; spend one original deadline including activation reserve; bind NAS/shared source and ingress/relay commitments, partition/mixed-peer/drain decisions to actual owners and telemetry. Depends on M4 and M1/M2/M3 accounting. | Local-full/peer-idle requests preserve quality and incumbent playback; shared storage/relay saturation or drain refuses misleading capacity instead of moving only the processing label. | Sol. |
| M6 — operations/client explanations | Custody/source evidence docs and existing settings authority/API contracts are present. | Expose normal settings and read-only readiness/Unknown/fallback/placement explanations, real drain blockers and compatible client diagnostics; complete API/operations references. Depends on actual M2–M5 behavior, not a new feature toggle. | Operator sees why a request stayed local, moved, waited or fell back; drain does not report reboot-ready while prepared/losing/relay owners remain. | Sol; controller publishes same PR. |
| M7 — final review/qualification/landing | Source-bound compile receipts and authored regression anchors; draft-paperwork checks only. No implementation behavioral test batch or completed-candidate review yet. | Finish implementation, actual independent final Fable review, fix findings, then once-only affected tests/fast lane and applicable native/cluster/platform acceptance; merge authorized exact candidate. Retry only failed/invalidated checks. | A reviewed, actually qualified artifact is merged with explicit platform/evidence limits. Compilation/body counts never replace this. No deployment is authorized. | Fable independent review; Sol fixes/qualification; controller authenticated publication/merge. |

## Critical path and next vertical closure target

The next user-visible vertical target is deliberately narrow and already in M3:
**one ordinary software finite VOD producer using durable finite admission through
normal stop and same-host restart reconciliation, with its capacity reusable.**
It is not all pipelines, a remote worker, a new platform promise or speculative
growth. It cannot be declared done by adding another private proof helper.

| Step | Exact source work still required | Completion condition / dependency |
|---|---|---|
| C1 — positive supported process backend and recovery | Extend the core launcher/`physical_graph` with the approved protected Linux pre-entry containment, retained original-object identity and nonescape validation. Journal the recovery locator before acquisition; rebind original containment after restart and prove old owner fenced. Preserve Unsupported when the already-authorized environment cannot uphold those premises; no permission/security setup is implied. | Normal and crash/restart cleanup have a real process-extinction route in the declared domain, not PID/path absence or a writable same-UID cgroup. Controller determines existing environment capability; a concrete missing contract goes to the coordinator, not the human by default. |
| C2 — real graph/member settlement | In `adaptive_resources/runtime/operations.rs`, compose actual root/task/resource selections and the matching graph proof into exact manifest facts; retain the original proof through queue refusal/unknown commit, settle once, then refund once. Add only the typed source/output/scratch recovery identity contract needed by this existing VOD path; current SourceFile disk codec is absent, not implicitly Scratch. | Successful normal retirement and restart reconciliation preserve member identity/costs; uncertain acquisition/commit remains held. Depends on C1 and concrete audited adapters, not serialized success fields. |
| C3 — connect the existing VOD owner | Wire the one software finite path through `vodencode.rs`, `producer_spawn.rs`, `prodrun/owned.rs` and its actual output/diagnostic supervisor. Replace generic writer closure assumptions with audited owned source/output/scratch lifetimes; prepare/arm before acquisition, retire after real joins. Compose current cap/settings demand and shared role pins, without bypassing held legacy competitors. | The actual application request, not a fixture-only branch, reserves once, produces output, stops and returns its finite capacity. Depends on C1/C2 plus the necessary M1/M2 demand wiring. |
| C4 — restart availability and acceptance | Replace the all-startup pending refusal in `resource_runtime.rs` with protected selective reconciliation and applicable admission refusal; keep unrelated serving safe. Author exact normal/cancel/crash/restart/capacity-reuse cases now; execute the required behavioral cases only after complete candidate review/fixes. | First finite path can be marked source-complete only when its normal/recovery lifecycle is wired. It is accepted only with actual applicable post-review evidence. Other M3 adapters and M4–M6 remain separate table rows. |

The immediate source seam after the current checkpoint is C2's exact retained
runtime-member settlement composition, while the coordinator confirms C1's
existing environment premises. That seam advances the vertical target but does
not by itself close C1/C3/C4 or make ordinary playback finite. A blocker in C1
must identify the precise missing premise, not trigger another general inventory.

## Why Windows LIVE advanced before Linux/restart

The source already had a Windows suspended-before-assignment, no-breakaway Job
launcher, so its missing live terminate/query and local owner composition were
implementable against an actual existing native object contract. Genuine SDK-bound
cross compilation became available and exposed real source errors, which were
fixed. Those receipts establish compilation only; no Windows behavior was run.

Linux has no equivalent positive backend implemented here. The approved cgroup
route additionally requires pre-entry membership, enforced nonescape, protected
original-object lineage and durable same-host restart continuity. A writable
same-UID subtree, process-group wait or reused/missing path cannot supply that.
Windows' unnamed Job also cannot supply restart continuity, and its durable
storage path is unsupported. Therefore the Windows slice closes a local source
prerequisite, not the critical Linux/recovery requirement, normal finite playback
or any whole milestone. Continuing to count such slices as completed features
would be misleading. C1–C4 are the explicit route to the first usable finite path.
