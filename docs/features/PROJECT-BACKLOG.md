# Project backlog — designed work and what remains to finish it

**Status:** live · **Reconciled:** 2026-10-08 · **Source baseline:**
`6e1089d3f` plus the Dolby Vision proposal in this change.

Start here to find a substantial proposal that has not become a finished
feature. The [docs index](../README.md) still lists every document; the
[roadmap](../ROADMAP.md) preserves product phases. This page groups work by
project instead of making a plan, review and status file look like three jobs.
The linked project ledger owns detailed execution and evidence. This is its
cross-project discovery index, not a competing claim board.

## 1. How to use and maintain this queue

**Planned** means a concrete design/build contract exists and implementation
is not recorded. **Partial** means named implementation remains. **Acceptance**
means code exists and specific integration, deployment or evidence remains.
**Reconcile** means dated documents disagree or later work may supersede the
proposal; inspect current source/landing before starting. **Decision** means
scope or a prerequisite design remains unresolved. These are work states,
not priorities, delivery promises or permission to deploy.

When adding a substantial proposal, add or update its row here in the same
commit as its docs-index row. When claiming, merging, qualifying, deferring or
superseding a project, update its canonical ledger and this summary together.
Record the owner and branch/PR in the canonical ledger. An absent owner means
unassigned, not abandoned. Do not declare a feature unfinished just because
its old plan header still says “ready to build.” Do not declare physical
acceptance from a merge or a compile result. Move completed rows to section 6
with a closure pointer rather than silently deleting them.

The initial inventory read indexed plans, status ledgers and closeouts across
all subject folders, with selected source/history checks for contradictions.
The client/streaming/playback/performance pass covered 290 Markdown documents.
Historical review transcripts, archived records and per-build notes are not
independent tasks. This is a documentation reconciliation, not a fresh audit
of every implementation or a new verification of the running fleet. Uncertain
cases remain visible in section 4. The numbered IDs below are stable references,
not an instruction to build in that order.

## 2. Concrete work to build or investigate

### 2.1 Video, playback and client projects

| ID / project | State and remaining scope | Next step and canonical plan |
|---|---|---|
| P01 · DV-processed HDR10 | **Planned; M0 active.** Reconstruct original FEL where possible, apply supported DV metadata, encode HDR10 and retain compatible fallback. Includes the user-requested **HDR10-E** badge, expanded as “Dolby Vision–enhanced HDR10”; show only from actual qualified processing receipts, with FEL contribution separate. | Sol 6.1 FEL/HDR session is investigating reusable backends, timestamped dual-layer decode and output semantics. Read the [proposal](../streaming/DV_HDR_PROCESSING_PLAN.md), [build handoff](../streaming/DV_HDR_PROCESSING_BUILD.md) and [review](../streaming/DV_HDR_PROCESSING_REVIEW.md). The [M0 reuse findings](../streaming/DV_HDR_PROCESSING_FEASIBILITY.md) are recorded; no production feature or measured quality gain yet. |
| P02 · FEL-preserving P7 to P8.1 | **Planned; M0 active.** Encode a reconstructed picture with metadata valid for it, preserve existing base-copy conversion fallback, and measure net quality at delivery bitrate. | Sol 6.1 P8.1/quality session is investigating authoring/adaptation and independent references. Prove both DV-on rendering and the DV-disabled HDR10 base. Same [build contract](../streaming/DV_HDR_PROCESSING_BUILD.md); no assumption that existing RPU rewrite reconstructs FEL. |
| P03 · Cinema remote / CEC / companion | **Planned.** Production implementation not started; B01–B10 are specified. | Start wire/receiver guard, semantic web input and Apple navigation, then relay/storage, companions, desktop CEC, invitations and integration. [Status](../clients/TV-REMOTE-AND-COMPANION-STATUS.md), [implementation](../clients/TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md), [protocol](../clients/TV-REMOTE-PROTOCOL.md). |
| P04 · Media write-stall guard | **Planned; reviewed.** A progress-renewed transport write deadline, distinct from header and HLS-pump timers. | Build bounded direct/range/progressive/HLS/offline behavior across listener variants; prove healthy slow clients and stopped readers. Reconcile the plan's initially-off qualification policy with current Developer rules. [Plan](../streaming/MEDIA-WRITE-STALL-GUARD.md). |
| P05 · DV strip trial probe | **Planned.** M0 malformed-SEI fallback/identity proof remains prerequisite. | Prove the bounded per-file trial and cache identity, then persist verdicts and make every copy-argument consumer agree. [Plan](../streaming/DV-STRIP-TRIAL-PROBE.md). |
| P06 · VideoToolbox VOD/DVR captions | **Acceptance; F3 repair 3 file-VOD API cases pass.** The caption/source-clock implementation is integrated. Final combined validation, DVR delivery and client presentation qualification remain open; the file-VOD cases do not establish them. | Retain the repair-specific API receipts and qualify the final combined package, DVR and client presentation scope. [Canonical follow-up ledger](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md), [caption follow-up](../streaming/VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md), issue #345. |
| P07 · Mac E1 Dolby/HLG acceleration | **Planned extension.** Initial Mac processing does not establish effective DV metadata transport or HLG correctness. | Prove P5 reconstruction/metadata and separately HLG reference-white, color and temporal behavior. [Build §9.1](../streaming/MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md), [status](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md). Coordinate with P01/P02 rather than create duplicate pipelines. |
| P08 · Mac E2 subtitle processing | **Acceptance; F1 source built.** GPU text delivery and the performance/cost acceptance case remain pending; source and mechanism observations do not establish those results. | Finish the normal-API text/control matrix and the measured comparison under the recorded resource admission. [Canonical follow-up ledger](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md), [build §9.2](../streaming/MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). |
| P09 · Mac E3 deinterlacing / Live TV | **Planned, demand-dependent.** Progressive processing does not qualify field handling. | Prove field cadence, captions, startup, reconnect and paced delivery. [Build §9.3](../streaming/MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). |
| P10 · Mac E4 HEVC/Main10 output | **Planned, demand-dependent.** Encoder output, negotiation and consumers are a separate contract. | Coordinate encoder, container/master facts, cache/cluster identity and physical clients. [Build §9.4](../streaming/MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). |
| P11 · Cross-platform DV hardware decode | **Implementation integrated; qualification open.** The Vulkan SDK export repair awaits generation. Linux check/Clippy pass on the pre-main-integration source; current-base compilation, full shipping-package generation/build and real shipping tuple/device qualification remain pending. Diagnostic VA-API controls are mechanism evidence, not production availability. | Complete the pinned Jellyfin producer/parser package and qualify the exact captured driver/device graphs; do not infer AMD, NVIDIA or other API support. [Canonical follow-up ledger](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md), [build §9.5](../streaming/MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). |
| P12 · Client picture enhancement | **Older detailed proposal; no implementation found in the inventory.** Web sharpening/debanding, optional anime enhancement and Apple scaler experiments; Android TV enhancement is excluded. | Refresh rendering/ownership contracts, then perform a bounded quality experiment before shipping filters. [PERF2 §9](../performance/PERF2-PLAN.md), [handoff](../performance/PERF2-IMPLEMENTATION-HANDOFF.md). |
| P13 · Nightly playback quality digest | **Planned.** Structured playback-event baseline/regression analytics with optional narration. | Build useful deterministic analytics first; refresh scheduler/settings against current jobs. [PERF2 §10.1](../performance/PERF2-PLAN.md). A narrator is optional, not a dependency of the report. |
| P14 · Offline subtitle transcription | **Planned.** Separate from existing online subtitle downloads. | Implement bounded command-backed transcription, queue/Stop ownership, sidecar publication and attribution using current storage/security contracts. [PERF2 §10.2](../performance/PERF2-PLAN.md). |
| P15 · Measured intro/recap/credits detection | **Deferred detailed capability.** Marker navigation and destination prewarm already exist; automatic detection does not. | Qualify audio correlation and black/silence detection accuracy, then publish exact markers into existing analysis data. [PERF2 §10.3](../performance/PERF2-PLAN.md), [playback status](../playback-control/PLAYBACK-CONTROL-STATUS.md). |
| P16 · Native watch-and-browse | **Partial design; native implementation deferred.** Web watch view is built; Apple/Android retained-owner designs still need review. | Produce those owner/surface designs before layouts; prove fullscreen, PiP, rotation and physical rendering. [Implementation](../clients/WATCH-AND-BROWSE-IMPLEMENTATION.md), [built web layout](../clients/WATCH-VIEW-LAYOUT.md). |
| P17 · Jellyfin expansion and Emby | **Scoped later phases.** First Jellyfin slice exists; broader app support and Emby are not established. | Finish A03 acceptance, select real apps/traces, then scope Emby authentication/entitlements and each optional Live TV/music/download/discovery/remote/web package. [Outline P2–P6](../clients/JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md), [status](../clients/JELLYFIN-COMPATIBILITY-STATUS.md). |

### 2.2 Cluster, engineering and platform projects

| ID / project | State and remaining scope | Next step and canonical plan |
|---|---|---|
| P18 · Raft fault-testing extension | **Planned; reviewed with corrections.** No implementation recorded; original review has no second independent approval. | Start the bounded existing-harness extension, preserve false-result controls and record revised-contract review status before building. [Implementation](../cluster/RAFT-FAULT-TESTING-IMPLEMENTATION.md), [review](../cluster/RAFT-FAULT-TESTING-REVIEW.md). |
| P19 · Membership credential split | **Designed, not started.** Separate membership authority from the credential given to ordinary members/learners. | Revalidate current vendored Hiqlite routes, then implement the non-shipping membership credential and mixed-version/rotation boundaries. [Plan](../cluster/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md). This does not by itself isolate members from replicated data. |
| P20 · Agent engineering harness remainder | **Partial / reconcile before selecting a milestone.** Detailed M0–M10 plan exists; Ripwire and some extraction/proof tools already landed. | Refresh the baseline and inventory missing `agent-check`, test seams, proof/fence/guide/process/fragments/PR-ledger work against current tooling. Do not repeat the completed Ripwire pilot or assume all ten milestones are missing. [Implementation](../ci/AI-HARNESS-IMPLEMENTATION-PLAN.md), [Ripwire result](../ci/RIPWIRE-PILOT.md). |
| P21 · Optional CI overhaul remainder | **Partial, optional.** M1 docs/preflight delivered; M2–M5 propose suite ownership, invalidation, safe reuse and cost/flake telemetry. Later receipt/shard work overlaps. | Map proposals onto the current execution and once-per-PR receipt systems before selecting a missing slice. [Overhaul](../ci/CI_TEST_OVERHAUL_PLAN.md), [acceleration](../ci/CI_EXECUTION_ACCELERATION_PLAN.md), [receipt policy](../ci/PYTHON-UNIT-PR-RECEIPTS.md). |
| P22 · Windows AMD AMF | **Deferred extension.** Windows server exists; AMF waits on NVIDIA/Intel qualification cost. | Complete native Windows M4 evidence, then scope and qualify AMD hardware independently. [Windows status](WINDOWS-PORT-STATUS.md), [plan](WINDOWS-PORT-PLAN.md). |
| P23 · Cluster operations panel follow-ups | **Detailed proposal; reconcile current implementation first.** Refresh direct status, isolate panel logic and consolidate credential UI. Its inline-shell/module examples predate the shell split. | Compare the three requested behaviors to current scripts and update the build contract; preserve current global-script conventions. [Plan](../cluster/CLUSTER-PANEL-FOLLOWUPS.md), [web layout](../clients/WEB-SHELL-LAYOUT.md). The old PR #651 dependency is not proof these follow-ups shipped. |

## 3. Built work that still needs landing, rollout or acceptance

These are not requests to rebuild the feature. The linked ledger owns the
exact remaining cell, device, failure and source-bound receipt. “Acceptance”
here does not create a hidden feature gate or override a saved setting.

| ID / project | Remaining work and next action | Canonical evidence |
|---|---|---|
| A01 · Mac initial accelerated processing | Initial SDR/HDR10 implementation landed in #882 (`339abbced`), superseding the Oct 7 ledger’s pending-main/PR #870 text. Retain package/runtime/physical acceptance limits. The landing explicitly records a Windows waiver and makes no full-green promotion claim. | [Mac status](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md) |
| A02 · Continuous quality switching | Merged as `f01031b45` in the current build ledger. Retain physical display/audio, native-device and full Firefox campaign acceptance; old incomplete-build handoffs are superseded. | [Build §10.238](../playback-control/CONTINUOUS-QUALITY-BUILD.md), [status](../playback-control/CONTINUOUS-QUALITY-STATUS.html) |
| A03 · First Jellyfin release | J0–J5 delivered; J6 retains frozen Infuse/Android TV, HDR/DV, multipage corpus and SDR HEVC High-tier master cases. Trace-only `RandomSeriesItems` remains deliberately unbuilt. | [Status §13](../clients/JELLYFIN-COMPATIBILITY-STATUS.md), [build](../clients/JELLYFIN-COMPATIBILITY-BUILD.md) |
| A04 · Playback lifecycle / recovery | Collect finite forced-VOD/rolling, preparation success/failure/compound changes, stop/cancel, owner loss and cleanup observations. Existing prepared-change code is not a fresh rewrite task. | [Lifecycle status](../playback-control/PLAYBACK-LIFECYCLE-STATUS.md), [coverage map](../playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md) |
| A05 · Subtitle cluster and physical reliability | M0–M5 extraction is built. Complete narrow cold/warm/off/slow checks, backfill and directed retry without restarting video; retain named physical cases. Reconcile parallel-range delivery before using its old PR state. | [Extraction status](../clients/SUBTITLE-CLUSTER-EXTRACTION-STATUS.md), [physical handoff](../clients/SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md), [ranges](../clients/PARALLEL-SUBTITLE-RANGES-STATUS.md) |
| A06 · Codec/GPU and HDR fidelity | Genuine S-11 films, skin/gamut, DV, 4K, HDR burn, exact graph throughput and physical displays remain. Synthetic PQ/NAL/internal cells do not close them; preserve failed sharp-edge evidence. | [Qualification](../streaming/CODEC-AND-GPU-QUALIFICATION.md), [HDR references](../performance/HDR-REFERENCE-SCORING.md), [quality status](../performance/VIDEO-QUALITY-STATUS.md) |
| A07 · Compatibility corpus expansion | Catalog of 52 families exists; further work explicitly deferred Oct 2. Acquire missing representative fixtures and player/regression evidence only when resumed. | [Catalog](../streaming/MEDIA-COMPATIBILITY-CATALOG.md) |
| A08 · B-frame VOD | Software implementation landed; zero B-frames remains normal default. Physical/compression qualification is still separate. | [Timeline design](../streaming/VOD-BFRAMES-TIMELINE-DESIGN.md), [quality status](../performance/VIDEO-QUALITY-STATUS.md) |
| A09 · Current browser/native playback | Retain physical Safari seek/startup, growing Apple resume, HDMI/audio/session, native paging/ART/heap, shaped-network and remote-input cases. Recent #888/#914/#915/#918 repairs exist; qualify their exact scope instead of reopening them. | [Apple resume](../clients/APPLE-GROWING-RESUME-PREPARATION.md), [Android repair](../clients/ANDROID-PLAYBACK-REPAIR-STATUS.md), [web seek](../clients/WEB-VOD-SEEK-IMPLEMENTATION-STATUS.md), [Safari](../streaming/SAFARI-SEEK-STATUS.md), [architecture closeout](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) |
| A10 · MKV duration / sliding HLS | Merged #377. Preserve intended-media physical playback limits; existing ledger does not authorize production requeue. | [Status](../streaming/MKV-DURATION-AND-SLIDING-HLS-STATUS.md) |
| A11 · Ebook offline / PDF | EPUB M0–M5 and Apple PDF implementation exist. Qualify real offline/reconnect sync, page/search/refusal/cleanup and VoiceOver. Other formats remain separate demand decisions. | [Reader plan](../clients/EBOOK-READER-PLAN.md) |
| A12 · Shared libraries / live activation | Integrated code and Oct 7 promotion corrections exist. PR #828 owns current qualification; live activation changes have separate pending qualification. Keep two-home Tailscale, physical handoff, remote-only mounts and historical upgrade/restore boundaries explicit. | [Shared status](SHARED-LIBRARIES-STATUS.md), [live activation](SHARING-LIVE-ACTIVATION.md), [operations](SHARED-LIBRARIES-OPERATIONS.md) |
| A13 · Cluster clock observation/enforcement | Runtime merged; identified measurement and controlled disposable-lab evidence remain. Do not launch step drills from this backlog or infer enforcement qualification from the setting. | [Measurement](../cluster/CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md), [owned lab](../cluster/CLOCK-MEASUREMENT-OWNED-LAB.md), [enforcement](../cluster/CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md) |
| A14 · Cluster restore, write cost and replica reads | Restore image/recovery RPO/RTO, attributed post-change write measurements, cross-node watch/fallback, read cost and rolling-version checks remain. Later closeout supersedes old prerequisite wording. | [Restore](../cluster/CLUSTER-BACKUP-AND-RESTORE.md), [write hygiene I](../cluster/REPLICATED-WRITE-RATE-HYGIENE.md), [II](../cluster/REPLICATED-WRITE-RATE-HYGIENE-II.md), [replica rollout](../cluster/BOUNDED-REPLICA-READS-ROLLOUT.md), [closeout](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) |
| A15 · Transport recovery qualification | Implementation and campaign repair exist; retain the scoped 20-voter/20-learner and exact-source qualification gap. Reconcile later resource-envelope decision before running an older zero-margin handoff. | [Recovery status](../cluster/CLUSTER_TRANSPORT_RECOVERY_STATUS.md), [CI status](../cluster/TRANSPORT-RECOVERY-CI-STATUS.md), [resource decision](../cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md) |
| A16 · Server delivery / observability | Listener/assets, scanner/provider, derivatives, token-expiry recovery, detail/unavailable-storage, slow-sidecar playback and real-use metrics are implemented but retain named fleet/browser/device observations. Missing old backfill order cannot be retroactively invented. | [Architecture closeout, server group](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md), [telemetry](../server/TELEMETRY-BACKPRESSURE.md), [observability](../server/OBSERVABILITY-BASELINE.md), [details](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md) |
| A17 · Live TV / DVR / native layouts | Tuner playback, shared transport, DVR, scheduling and native proportions have implementation. Finish extended-guide/reminder, mixed-storage interruption, restart/warm-start/capacity/caption and physical layout checks. Consult status ledgers before believing an old “ready to build” plan. | [DVR status](LIVE-TV-DVR-STATUS.md), [hardware handoff](../clients/DVR-HARDWARE-VERIFICATION-PROMPT.md), [shared transport](LIVE-TV-SHARED-TRANSPORT.md), [reliability](LIVE-TV-RELIABILITY-STATUS.md), [layouts](LIVE-TV-NATIVE-LAYOUTS-STATUS.md) |
| A18 · Windows native runtime | Merged server still needs native x64 smoke, clean-VM service install/reboot/uninstall and NVIDIA/Intel receipts. Cross-compilation does not provide runtime evidence. | [Windows status](WINDOWS-PORT-STATUS.md) |
| A19 · Hiqlite upstream coordination | Local cleanup largely integrated; generic upstream rows and semantic-navigation observations remain. Reconcile existing submissions and full patch equivalence before dropping a fork patch. | [Fork/dependency plan](../cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md), [closeout](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) |
| A20 · Build/deploy hygiene and release | Busy child-priority/limit readback, isolated optional hardening and concrete release/tag/index/deployed-version work remain. Declined text-contract pruning and historical missing timings are not revived. | [Service/build hygiene](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md), [release ledger](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md), [closeout](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) |
| A21 · CI execution infrastructure | Cache/package/shard/native-ARM work exists. Reconcile remaining disk facts, host/inventory drift and qualified execution-mode rollout against current runner policy. Do not reprovision hosts because an old handoff says “pending.” | [Acceleration implementation record](../ci/CI_EXECUTION_ACCELERATION_PLAN.md), [runner disk guide](../ci/RUNNER-DISK.md) |

The architecture programme's **49-row [workboard](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md)**
remains canonical for ownership and detailed acceptance. A13–A20 summarize its
remaining themes; they do not create duplicate claims or imply that every
historic checkbox is still required. The October 7
[closeout](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) and newer
plan-specific receipts supersede dated assertions (for example, the snapshot
plan's later receipt closes its accepted production observation).

## 4. Preserve these plans, but reconcile before assigning code

| ID / candidate | Why a fresh “unbuilt” label would mislead | First action / source |
|---|---|---|
| R01 · PERF2 N0–N5 / AV1 cached output | Telemetry, priors, measurement, cold-start/prewarm and quality work overlaps later shipped programmes; cached AV1 and broader codec ideas need individual review. | Compare [PERF2](../performance/PERF2-PLAN.md) with current [quality status](../performance/VIDEO-QUALITY-STATUS.md) and codec qualification. |
| R02 · Deck/Guide layout expansion | Genre migration and classic/catalog/theater layouts already exist despite old open headers. Deck/Guide and accessibility proposals need current source/design reconciliation. | [Layout implementation](../clients/UI-LAYOUTS-IMPLEMENTATION.md), [web layout map](../clients/WEB-SHELL-LAYOUT.md). |
| R03 · Startup Track S / source preparation | Older handoffs describe draft work; later prepared/growing/continuous startup fixes landed. | Audit the exact current owner/job paths before reopening [native HLS startup](../streaming/NATIVE-HLS-STARTUP-IMPLEMENTATION.md). |
| R04 · Permanent DV conversion diagnosis | Old unbuilt proposals predate permanent-conversion and queue/capacity work. | Compare [permanent conversion status](../streaming/M5B_STATUS.md) to the current converter/job paths; do not confuse this with playback P01/P02. |
| R05 · 4K copy-path stutter | Historical segment-boundary evidence remains useful, but segmentation/continuous-quality changes supersede parts of the diagnosis. | Reproduce current behavior before choosing a repair from [the investigation](../streaming/STUTTER-4K.md). |
| R06 · Cluster page latency / WAL generation | Dated reviewed plans coexist with later WAL, read-cost and transport repairs. The existence of a plan is not proof its whole repair is missing. | Match each [latency contract](../cluster/CLUSTER_PAGE_LATENCY_FIX_PLAN.md) and [WAL contract](../cluster/WAL_GENERATION_REPAIR_PLAN.md) to current source/landing and retain only actual gaps. |
| R07 · Learner snapshot catch-up | A live diagnosis identifies typed mismatch/transport/no-progress questions, but later transport work may already answer them. | Map [diagnosis completion bar](../cluster/LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md) onto A15 before a new patch. |
| R08 · Cluster performance / HA historical milestones | Original clustering phase text predates delivered learner, fencing and recovery work. | Use [cluster performance delivery status](../cluster/CLUSTER-PERFORMANCE-PLAN.md) and A13–A15 to isolate physical acceptance and any genuine later-client gap. |
| R09 · DVR visibility and library-channel repair | Build plans still say unbuilt while status records describe implemented/shipped slices and follow-ups. | Reconcile current [DVR visibility](LIVE-TV-DVR-VISIBILITY-STATUS.md) and [channel status](LIBRARY-CHANNELS-STATUS.md) / [repair](LIBRARY-CHANNELS-PLAYBACK-REPAIR.md); preserve specific physical/publication failures. |
| R10 · Online subtitle downloads | Implementation document says locally built/review-promotion open; its current landing and device scope need checking before starting another implementation. | [Implementation record](SUBTITLE-DOWNLOADS-IMPLEMENTATION.md). Keep separate from P14 transcription. |
| R11 · Runner orphan cleanup | Historical incident files still say fix-in-review/open; current cleanup and receipt infrastructure may supersede them. | Reconcile [process incident](../ci/MEDIA1-RUNNER-ORPHANED-PROCESSES.md) and [sleep incident](../ci/MEDIA1-RUNNER-ORPHANED-SLEEP-PROCESSES.md) with current cleanup ownership before any host mutation. |
| R12 · Settings graduation | Settings navigation and Developer lifecycle are implemented. Individual cards may still owe their own qualification/graduation; old “implementation pending” headers are stale. | Audit current [settings status and lifecycle](SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md) against each feature's real remaining evidence. |

## 5. Deliberately deferred decisions and prerequisite designs

Keep these visible without calling them ready-to-build projects:

| ID / decision | Remaining decision or scope | Reference |
|---|---|---|
| D01 · Watch state back to Monarr | Per-profile policy, multi-user semantics and whether watched state can ever authorize deletion. No implementation is implied by the existing Curator import integration. | [Roadmap integration remainder](../ROADMAP.md), [integration](../INTEGRATION.md) |
| D02 · Fleet trust and authorization | Quorum-authoritative ordinary authorization, per-node enrollment/rotation/removal and mixed-version activation need a coordinated design. P19 only splits membership authority. | [Remediation ledger F02/F04](WEEKLY-REVIEW-REMEDIATION-STATUS.md), [security assessment](../reviews/SECURITY-ASSESSMENT-2026-09-13.md) |
| D03 · Local administrator recovery | Design an OS-authenticated daemon control channel; do not invent a bypass in HTTP auth. | [Remediation F06](WEEKLY-REVIEW-REMEDIATION-STATUS.md), [security assessment](../reviews/SECURITY-ASSESSMENT-2026-09-13.md) |
| D04 · Security/platform hardening remainder | Full media-process/GPU sandboxing, registry TLS/signing/attestation, credential lifecycle/offline limits and durable ambiguous-start reconciliation remain separately scoped capabilities. Portable recovery overlaps A14 and must not be duplicated. | [Remediation deferred/narrow scope](WEEKLY-REVIEW-REMEDIATION-STATUS.md), [security assessment](../reviews/SECURITY-ASSESSMENT-2026-09-13.md) |
| D05 · Optional service/deployment hardening | Change limits/overflow/isolation only where the retained measurements and separate lab proposal justify it. | [Build hygiene](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md), [closeout decisions](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) |
| D06 · Broader roadmap ideas | Live filesystem watching, extra metadata providers, Tizen/webOS/Roku, OIDC/parental controls, trickplay/themes/extras and further NAS/music work are ideas until current scope and source are reconciled. Some neighboring roadmap bullets already shipped. | [Roadmap](../ROADMAP.md), [requirements](../REQUIREMENTS.md), [client strategy](../CLIENTS.md) |

Declined changes are not silently promoted back into this queue: keyset/
`next_up` rewrites, unmeasured tokenizer substitution, declined Plex paging,
removed adaptive reducers and rejected text-contract pruning retain the
[recorded decisions](../reviews/ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md).

## 6. Reconciled completions — do not accidentally rebuild these

| Project / stale plan claim | Completion or authoritative replacement |
|---|---|
| Architecture implementation programme | #793 and #814 delivered the integration/closeout; remaining acceptance is on the canonical workboard, not 49 new implementation jobs. |
| Durable work queue and capacity | M1–M3/E0–E3 merged in #532/#564/#572; #588 addressed subtitle-worker obstruction; worker visibility/throughput landed as `f16be4f22` (#651). [Queue status](../cluster/DURABLE-WORK-QUEUE-STATUS.md), [capacity record](../cluster/CLUSTER-WORK-CAPACITY-IMPLEMENTATION.md). |
| Scan identity prevention/repair tools | Main contains #385 (`e9f80d0fc`), despite the [status ledger](SCAN-IDENTITY-STATUS.md)'s old requalifying text. Production catalogue repair remains a separate preview/authorization decision, not unfinished code. |
| Local search and channel subject matching | #350 (`7fed9af78`) and #313 (`28ae8163b`) are in main. [Search](LOCAL-LIBRARY-SEARCH.md), [subject matching](LIBRARY-CHANNEL-SUBJECT-MATCHING-IMPLEMENTATION.md). |
| Ripwire navigation | #248 (`178224c09`) is in main. The [pilot](../ci/RIPWIRE-PILOT.md) is complete with an opt-in-only verdict; its old draft status is not missing implementation. |
| Docker GPU setup | #849 (`96f668128`) is in main; the [status file](../ci/DOCKER-HARDWARE-STATUS.md)'s pending merge text is historical. |
| Pi installation | October 8 [status](../clients/RASPBERRY-PI-STATUS.md) records #889 merged and bounded physical acceptance complete; retain its support limits. |
| Seek scratch and analysis completion | #389 and #352 landed. Their old implementation headings do not create new tasks. |
| PGS overlay startup gate | The [start-path RCA](../clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) records the two-device bar passed September 24 and overlay enabled. Separately named corpus/device limitations still belong in A05. |
| Web watch view / base layouts | Web watch view and classic/catalog/theater exist; native watch-and-browse is P16, and remaining layout proposals are R02. |

New completed rows should add their actual merged/qualified receipt and date.
No routine timer is required to keep this file current: project-changing PRs
must maintain it along with their canonical status and the documentation index.
