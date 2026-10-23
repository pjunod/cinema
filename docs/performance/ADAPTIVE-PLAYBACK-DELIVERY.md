# Adaptive playback — finite delivery closure

**Status:** open · implementation incomplete. Published source checkpoints
`269923bb7` and `ee8cc4674` add bounded client payload partitions and indexed
custody history. The corrected source checkpoint `26633a1a9d75e2631d52cbb1799e40ee10c99115`
passed pinned Rust 1.97.1 native Linux daemon all-target compilation, including
cluster fixtures, in 231.509 seconds (archive SHA-256
`64edbf1ce91945e20f35cc31c5c606961f2e639a8705bf646c4e1d0f6b088227`, log
SHA-256 `7ecb186feef175f59dd8d7d97cf3b974e0d22414c8ae827f87b44a0a440d8a46`,
task receipt `direct-linux-26633a1a9d75-RESULT.json`). This retry
compiled the daemon and its core dependency; it was not a core test-target
PASS. The earlier host all-target core+daemon+cluster-fixture compilation
passed in 112.938 seconds (log SHA-256
`652e173afdea2098959099136202d057432d664592079b11efa659730a68bbeb`); host
checks exclude Linux-only test bodies. Authored behavioral tests have not run.
Candidate S now retains
the selected path's enforced immutable, client, response and native partitions
after known retirement; strict S<R memory-return behavior remains unrun. All
V1–V5 and R1/M0–M7 remain OPEN. Independent final Fable review, qualification,
fast lane and merge have not run. Draft PR #1248 carries the live owner/state.
Primary owner: direct Codex harness
(`/root`; actual model unspecified), sole integration writer. Work remains on
`effort/adaptive-playback-resources`, draft PR #1248. This is a delivery
breakdown of the existing
[approved M0–M7 plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md#10-delivery-sequence--integrate-one-effort-prove-each-seam),
not new design, feature scope, a framework or another review gate.
The [source/evidence ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) retains pins.

The current integration extends bounded reader/demand tables and transient
control-clone reservations. HTTP terminal/error chunks retain their actual
slot until the final `Bytes` alias drops; cancellation does not return credit
early. Custody v6 streams the full operation history and validates indexed
family/asset-operation membership while retaining bidirectional comparisons
for old SQL rows; it adds no family/history count cap. Candidate S narrows its
memory reservation only to enforced retained partitions after known retirement;
the strict S<R behavior is still unrun. Direct-source adapter and root
diagnostic-event edits remain uncommitted and are not covered by the native
receipt above. Other pipelines, full memory limits, partial startup, cold
cache/restart and client-state growth remain outside this selected path. No
authored behavioral test has run.

## Authoritative first ordinary VOD source checklist

All five items remain **OPEN**. The selected Linux ordinary software-transcode
branch connects producer/source custody, counted HTTP, cleanup, R→S transfer,
same-process reuse and protection from legacy deletion. Cold-cache/restart
adoption, the complete allocation profile and remaining task contracts are
unfinished; this path is not behavior-verified or qualified.

| Item | Actual missing production connection / binary closure |
|---|---|
| V1 — admission/source — OPEN | The selected Linux ordinary software-transcode branch enters finite preparation with its held source and original producer ownership. Other recipes still use the legacy path. The full production capacity/memory profile, partial-start closure and cold restart/cache adoption remain open. |
| V2 — held output/durability — OPEN | The selected branch uses the same held child, selected-ID publisher and sync jobs. Counted HTTP selection and actual S cleanup/R→S transfer are connected. Full memory accounting and other legacy recipes remain outside this path; this does not close V2. |
| V3 — producer/stop — OPEN | The selected branch joins its original writer/root and already-registered native jobs outside the bundle lock. Stop retains the original task/control through registration, and source-parent closure uses its original job. Partial-start/inherited-copy closure and the complete memory profile remain unresolved; the branch is not qualified. |
| V4 — retained output — OPEN | The selected branch connects counted HTTP readers, cleanup and one journal R→S transfer to the retained native output. After known retirement, S retains the enforced immutable metadata, client, response and native partitions; strict S<R memory-return behavior remains unrun. Producer-only storage/process/task components shrink only through known transfer. Unknown cleanup/commit keeps ownership charged. Cold restart/cache adoption and broader recipe integration remain open. |
| V5 — terminal/reuse — OPEN | Ordinary completion/cancel/error cleanup consumes exact known settlement once; the next ordinary admission can use returned capacity. Unknown commit retains original operation/custody; eviction waits actual readers. |

Current sequence: complete the selected ordinary branch’s remaining capacity,
partial-start and cold-cache/restart gaps, then close V1/V3; proceed to V5
settlement/reuse. Same-process seeks reuse the existing
rendition owner and its full-plan producer; the cold-cache reuse gap is R1.
All V1–V5 remain OPEN; no source item has newly closed. The
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

## Active integration — 2026-10-23

The selected ordinary Linux software-transcode request enters the finite owner
before selected native acquisition. It retains the real source, fixed software
process, publication jobs and counted HTTP bodies through native close and one
known journal settlement. Stable same-boot engine identity permits compatible
cold-S reuse; original owner records remain immutable across ordered recovery
adoptions. Partial cleanup, unsupported continuity and unknown commits keep
charges and refuse reuse. Obsolete mismatched recovered-cache eviction remains
unimplemented after automatic approval review rejected that destructive change;
no rejected edit or live deletion was performed.

Engine freshness uses the original registered check worker across cancelled
waits, and settlement requires its known close/join. Immutable retained metadata
has requested-capacity accounting and a lifetime lease. Native archive checking
is pending for the exact current source; host compilation alone does not qualify
the native lifecycle or strict S<R behavior. Direct-source adapter work is in
progress. Same-process seeks reuse the current full-plan rendition owner; cold
cache/restart adoption remains open. M1 sampling/diagnostics cover the selected
finite writer only; live policy does not yet consume those observations. All
authored tests remain unrun, all milestones remain open, and final review and
qualification have not started.

## Actual distance to usable playback

The selected Linux ordinary software-transcode path now uses the connected
admission, accounting, persistence and serving path; other recipes remain
legacy. This is not a completed adaptive playback feature. No M0–M7 milestone
is claimed accepted end-to-end.
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
| M2 — policy/settings/learning | `adaptive_resources.rs` has atomic vectors/families/retained roles; `resource_history.rs` bounded priors; `resource_policy.rs` advisory trial/deadline/cooldown logic; `resource_settings` and `store/planning_capacity.rs` preserve settings provenance/authority. | Wire these components into the live node owner and production planning/admission; feed real service samples/epochs, persist useful history, apply current hard bounds and four-state settings without rewriting saved intent. The selected finite core owner now records actual identity-bound service windows and projects history diagnostics. Production planning still does not consume those samples or the trial policy; helper existence is not adaptive admission. Depends on M1 and finite M3 owners. | A normal request uses current hard caps and genuine measured evidence; Automatic can learn safely, explicit/legacy caps and both zero meanings retain their contract; failure cooldown and paced workloads cannot invent headroom. | Primary Codex harness (`/root`). |
| M3a — critical custody/runtime backend | Dedicated node-local journal, lock-bound startup/reconstruction, retained prepare/arm/seal/settlement book, actual root/source/discard-pipe ownership; Windows LIVE original-Job proof plus restricted private local-graph receipt. | C1–C3 below: supported Linux pre-entry/nonescape/object-continuity recovery; exact runtime manifest/member settlement; real production source/writer/scratch adapters and retirement. Current Windows storage/restart unsupported and current Unix cleanup UnknownHeld do not satisfy this. | One finite producer can actually start, stop, commit settlement and free capacity; restart retains/reconciles its original obligations without deleting protected work or leaking every normal slot forever. | Primary Codex harness (`/root`); use existing authorized native compiler/environment evidence. |
| M3b — all pipeline adapters | VOD/rolling supervisor joins and custody bridges, bounded overlap/family borrowing, real source handles and pipe task helpers are present. Ordinary launches still remain legacy. | After the first finite path: connect finite/rolling/continuous and processed helper graphs, audio/subtitle/copy/cached/direct/range, Live/DVR shared transport plus each sink, fragment/index/background work and ingress relay; replace whole-budget background holders with real yield/retirement; preserve shared-role transfer and paced/producing phases. Depends on M3a plus M1/M2. | Every M0 row uses the service, counts each retained/shared owner once and retains costs until its real terminal event; foreground playback can reclaim background capacity without stealing live work. | Primary Codex harness (`/root`). |
| M4 — worker runtime (Store is delivered) | Integrated SQLite/Hiqlite attempt/request/domain transactions, Prepared/winner/adoption fences, legacy-writer guards and Source activation guards. | Implement worker Prepare/Adopt/cancel/status handlers and capability advertisement; reconstruct actual source/executable/credit/admission locally, retain cleanup ownership, recheck serving/drain/epoch/winner before publication/activation. Compose ordinary and processed output. Depends on local M3a/M3b owners; Store rows alone are not remote workers. | A same-quality remote preparation really runs on an eligible worker, only one exact winner adopts, losers retain physical cleanup, old peers keep legacy behavior and cannot bypass new authority. | Primary Codex harness (`/root`). |
| M5 — placement/delivery domains | Advisory policy contains peer-first/common-deadline concepts; M4 Store supplies bounded attempt/domain claims. These are prerequisites, not dispatch. | Connect offers/ranking and bounded same-quality peer retry before quality fallback; spend one original deadline including activation reserve; bind NAS/shared source and ingress/relay commitments, partition/mixed-peer/drain decisions to actual owners and telemetry. Depends on M4 and M1/M2/M3 accounting. | Local-full/peer-idle requests preserve quality and incumbent playback; shared storage/relay saturation or drain refuses misleading capacity instead of moving only the processing label. | Primary Codex harness (`/root`). |
| M6 — operations/client explanations | Custody/source evidence docs and existing settings authority/API contracts are present. | Expose normal settings and read-only readiness/Unknown/fallback/placement explanations, real drain blockers and compatible client diagnostics; complete API/operations references. Depends on actual M2–M5 behavior, not a new feature toggle. | Operator sees why a request stayed local, moved, waited or fell back; drain does not report reboot-ready while prepared/losing/relay owners remain. | Primary Codex harness (`/root`); same draft PR #1248. |
| M7 — final review/qualification/landing | Source-bound compile receipts and authored regression anchors; draft-paperwork checks only. No implementation behavioral test batch or completed-candidate review yet. | Finish implementation, actual independent final Fable review, fix findings, then once-only affected tests/fast lane and applicable native/cluster/platform acceptance; merge authorized exact candidate. Retry only failed/invalidated checks. | A reviewed, actually qualified artifact is merged with explicit platform/evidence limits. Compilation/body counts never replace this. No deployment is authorized. | Fable independent review; primary Codex harness (`/root`) fixes/qualification and authenticated publication/merge. |

## Production owner map — open adapters

Known source seams are mapped below; this is not exhaustive coverage proof.
Each missing adapter remains open.

| Work | Construct, terminal owner and current admission | Adaptive adapter still needed |
|---|---|---|
| Direct play, byte ranges and cached segments | HTTP handlers; selected VOD serves materialized files through its retained producer owner. Direct/range source paths use `SourceFence` / source actors; response bodies retain their reader owners. | Selected finite VOD is connected. Direct/range adapters still need to bind source reads, response body lifetime and egress to the same runtime claim. |
| Progressive/HLS copy and remux | [`produce_copy_output_job`](../../crates/plurxd/src/transcode/manager/produce.rs) opens the source; [`spawn_copy_reader_owner`](../../crates/plurxd/src/transcode/producer/prepublication_executor.rs) owns stdout and the scratch writer. Rolling retirement owns child reap and scratch cleanup. | [`EncodePermit::admit_source_copy`](../../crates/plurxd/src/vodencode.rs) estimates CPU/decoder capacity; copy preparation also uses `admit_fragment`. A runtime adapter must join source, child, reader, package/scratch and egress through retirement. |
| Software and hardware transcode | Rolling start constructs session and child in [`manager/start.rs`](../../crates/plurxd/src/transcode/manager/start.rs) / [`rolling/session.rs`](../../crates/plurxd/src/transcode/rolling/session.rs); `RollingRetirementSettlement` owns reap and scratch convergence. | Existing `Admissions` bundles and rolling settlement remain separate from whole-graph runtime accounting, including helper processes and response delivery. |
| HDR/DV/HLG/tone mapping, scaling and deinterlacing | DV runtime, Mac processing and ordinary filtered encode construct distinct processing graphs and helper children. | Adapter must account the complete graph, transfers, authoring and helper-child terminal ownership; an encoder-only claim does not cover this row. |
| Audio-only and subtitle work | Playback owns audio-only processes; [`subtitle_work::run_subtitle_work`](../../crates/plurxd/src/subtitle_work.rs) owns the durable job fence and cancellation through settlement. | Subtitle queue work uses [`admit_fragment`](../../crates/plurxd/src/transcode/manager/construct.rs); foreground self-claim has a separate one-CPU admission. Join extraction/source I/O, decoder process, memory and realtime output to a common runtime owner. |
| Continuous/adaptive VOD | `rolling::Session` owns producer state; its retirement settlement retains admission, child and scratch cleanup. | Existing encoder admission and scratch ledger do not account successor overlap, source reads, retained reader memory and response egress as one family. |
| Live TV and timeshift | [`DvrTransport::spawn_transport_worker`](../../crates/plurxd/src/live_tv/dvr.rs) owns tuner ingress; `LiveTvProcess` retains child/job and optional video admission. | Transport lease and `admit_live_tv` cover distinct pieces; add a runtime claim spanning tuner, process, fanout, ring buffers and latency. Copy/audio-only paths have no video permit. |
| DVR recording | [`DvrSink`](../../crates/plurxd/src/live_tv/dvr.rs) owns a bounded queue and writer task through `sync_all`/close; transport fans bytes out to attached sinks. | Sink queue and writer lifecycle need recording storage, write deadlines and transport resources joined under one durable runtime owner. |
| Background indexing, previews, pretranscode and probes | [`admit_fragment`](../../crates/plurxd/src/transcode/manager/construct.rs), [`produce_pretranscode_job`](../../crates/plurxd/src/transcode/manager/produce.rs), and [`start_source_probe`](../../crates/plurxd/src/transcode/source_preparation.rs) have separate owners; probe settlement retains source, child/pipe cleanup and permit. | Preserve each existing job fence while unifying source, scratch/result memory, child and yield/reap accounting with foreground priority. |
| Cluster relay and remote source read | [`relay_if_remote`](../../crates/plurxd/src/http/hls/relay.rs) resolves the owner; [`relay_response_with_limits_observed`](../../crates/plurxd/src/media_sessions.rs) owns upstream stream and bounded body-channel pump. | Relay limits and durable routing do not create local adaptive claims for ingress, network/egress or remote worker/source resources. Add worker-owned preparation and per-node accounting; do not infer capacity from peer booleans. |

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

## Decision — process instances and kernel tasks

**Status:** accepted implementation direction, 2026-10-23; source integration and
qualification pending. The primary integration owner selected this within the
approved plan's separate thread-limit and CPU-use contract (§4.1).

Linux pids limits, threads-max and RLIMIT_NPROC constrain kernel tasks. A fixed
FFmpeg root is one process instance, but encoder thread options do not bound all
its decoder, filter, demuxer and runtime threads. Charging one process against
these limits therefore supplies a known minimum, not a known task peak.

Keep process-instance charge and kernel-task capacity separate. Every current
producer with a persisted positive process count has an explicit derived unknown
task-peak claim; process-free retained output has none. This derives from the
existing durable vector and does not fabricate a numeric peak or rewrite frozen
journal formats. No current adapter has an enforced total task bound. Unknown
task producers serialize against all other active process producers in either
insertion order, with minimum root count checked against task headroom. Existing
physical retirement/settlement is still required before that claim disappears.

This is the conservative cold-producer policy, like the approved unknown-memory
path. It permits one producer without promising that the producer cannot exhaust
the kernel limit; task peak and managed peak remain Unknown in diagnostics.
A hard total-task guarantee requires an enforceable execution bound, not a
thread-count guess, a watchdog or a fabricated zero. A future known task claim
must carry that bound and durable evidence before it can relax serialization.
Reserving an arbitrary count, treating encoder threads as total tasks, and using
a memory claim to label task uncertainty were rejected because their units or
evidence do not support admission. No feature switch is introduced.

## Decision — daemon metadata follows its real owner

**Context.** Native scratch closure does not destroy the daemon rendition's
recipe, immutable plans, manifest, engine references or buffered responses.
Crediting that memory at native close would permit the next producer while
those allocations remain resident. A never-started pump also retained a
back-reference to its own preparation until its planning payload was disposed.

**Decision.** Keep a separate, explicit metadata partition in the original
finite reservation, with recursively measured requested capacities and a
nonclone lease held after the rendition's payload fields. The same response
budget tracks the independent metadata/body lifetimes. Its drain event is a
factual notification, not native closure or refund authority. The existing
maintenance owner closes registration under the per-key gate, moves exact
control/output selectors into a compact closing entry, releases the gate while
real driver/request owners drain, and settles through the original core
operation. Unknown outcomes retain the closing entry and block replacement.
Known cancellation disposes planning payloads before credit and removes its
settled registry entry. These changes remain unqualified source.

**Limits and next source work.** The estimator covers immutable requested heap
capacities; it does not establish a whole-RSS or allocator bound. Reader maps,
per-reader marker state and missing-output demand state still need an enforced
client-state subpartition. The complete original memory component therefore
remains charged at S. The native lifecycle fixture's strict producer-memory
return criterion is intentionally still unmet until this accounting is complete.
No milestone is closed by the helper, lease or compiler alone.
