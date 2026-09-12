# docs — where everything is, and what each file answers

The front door is the repo [README](../README.md): what plurx is, how to run
it, and the reading path for a newcomer. This page is the map of `docs/`
itself — every file, the question it answers, and whether it is still live.

Three tiers, and only three:

- **This directory** holds the reference set: the dozen-and-a-half documents
  that describe plurx *as it is today*. They are maintained, and a change to
  behavior is expected to change them in the same commit.
- **Subject folders** hold everything written *about* a piece of work — the
  plan, its reviews, the handoffs an agent executed, the status tracker, the
  diagnoses. One subject, one folder, so the whole thread of a project reads
  in one place.
- **[`archive/`](archive/)** holds work that is finished and not part of a
  live thread. Nothing is deleted; it is out of the way, not gone.

Status words below mean exactly this: **live** — maintained, describes the
current system · **open** — work in progress or awaiting a decision ·
**built** — the work landed; the doc is the record of what and why ·
**done** — closed out, kept for history. Accurate as of 2026-09-07; when a
row and a doc's own `**Status:**` header disagree, the doc wins.

## Find it fast

| You want to know… | Read |
|---|---|
| What does plurx actually do? | [FEATURES.md](FEATURES.md) |
| How do I run it, and what does this output mean? | [OPERATIONS.md](OPERATIONS.md) |
| What do I type? | [CHEATSHEET.md](CHEATSHEET.md) |
| How is it built, and why that way? | [ARCHITECTURE.md](ARCHITECTURE.md) |
| What endpoint do I call, and what authorizes it? | [API.md](API.md) |
| How does a file become a stream? | [PLAYBACK.md](PLAYBACK.md) |
| Why is this title playing badly? | [PLAYBACK-TESTING.md](PLAYBACK-TESTING.md), then [streaming/](streaming/) |
| Why is a Dolby Vision title arriving as HDR10? | [streaming/DV-DELIVERY-FINDINGS.md](streaming/DV-DELIVERY-FINDINGS.md) |
| What is the cluster supposed to do when a node dies? | [cluster/](cluster/), starting at [CLUSTERING-PLAN.md](cluster/CLUSTERING-PLAN.md) |
| Why was the transport-recovery campaign red on main for so long? | [cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md](cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md) |
| What does its resource check assert now, and why? | [cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md) |
| Which button does what on which client? | [clients/PLAYER-INPUT-CONTRACT.md](clients/PLAYER-INPUT-CONTRACT.md) |
| What is missing from the Apple / Android client? | [clients/APPLE-CLIENT-PARITY.md](clients/APPLE-CLIENT-PARITY.md) · [clients/ANDROID-CLIENT-PARITY.md](clients/ANDROID-CLIENT-PARITY.md) |
| How do I cut a release? | [RELEASING.md](RELEASING.md), then [PUBLISHING.md](PUBLISHING.md) |
| What does CI gate, and why did it fail? | [VALIDATION.md](VALIDATION.md) |
| How does work get from a branch to `main`? | [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) |
| What is being built right now? | [STATUS.html](STATUS.html) · [ROADMAP.md](ROADMAP.md) |
| What did we decide about X, and when? | the subject folder for X — plans and reviews carry dated `**Status:**` headers |

## The reference set

Maintained documents describing the current system. Everything here is
`live`.

| File | The question it answers |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | How plurx is built, and why — the diagrams and the founding decisions. |
| [API.md](API.md) | Every HTTP endpoint, the credential it takes, and what comes back. There is no OpenAPI document; this is the specification. |
| [FEATURES.md](FEATURES.md) | Everything plurx does, exhaustively, including what it deliberately refuses to do. |
| [OPERATIONS.md](OPERATIONS.md) | Running it day to day, and what every output means. |
| [CHEATSHEET.md](CHEATSHEET.md) | What to type, in what order. |
| [PLAYBACK.md](PLAYBACK.md) | How a file becomes a stream — the end-to-end delivery path. |
| [PLAYBACK-TESTING.md](PLAYBACK-TESTING.md) | Turning a playback failure into a reproducible matrix. |
| [CLIENTS.md](CLIENTS.md) | The client strategy and the platform matrix — what runs native, what runs web. |
| [INTEGRATION.md](INTEGRATION.md) | Every seam with monarr, and how to prove each one works. |
| [SECURITY.md](SECURITY.md) | What plurx protects, and what it leaves to the network. |
| [VALIDATION.md](VALIDATION.md) | The functionality-point system: what CI gates and how impact is selected. |
| [REQUIREMENTS.md](REQUIREMENTS.md) | The product requirements, and the decision behind each. |
| [ROADMAP.md](ROADMAP.md) | The phases, and what each one ends with. |
| [RELEASING.md](RELEASING.md) | One version for the whole workspace — how it moves. |
| [PUBLISHING.md](PUBLISHING.md) | TestFlight, the App Store, and Google Play. |
| [BENCHMARKING.md](BENCHMARKING.md) | Measuring plurx against Plex without grading different tests. |
| [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) | Fast effort branches, deliberate qualification — how work reaches `main`. |
| [STATUS.html](STATUS.html) | The pinned status page: what is deployed, on which nodes and clients. |

Assets and generated records also live at this level:
[`img/`](img/) (README screenshots) · [`apple-builds/`](apple-builds/) (one
note per Apple build, read by `validation/apple_build.py`) ·
[`mockups/`](mockups/) · [`evidence/`](evidence/).

Apple build 137: [Library channel startup and contrast](apple-builds/245-library-channel-start-and-contrast.md).
Apple build 138: [Library channel playback decision](apple-builds/245-library-channel-playback-decision.md).
Apple build 141: [Library channel buffering demand](apple-builds/245-library-channel-buffering-demand.md).

Previous: [Live TV playback method](apple-builds/245-live-tv-playback-method.md).

---

## playback-control/ — the playback control rewrite

Explicit demand, one owner per stream, prepared handoffs. The largest single
thread in the repo: a protocol plan, its milestone contracts M1–M4, and the
M5–M7 handoffs that carry the remainder. Start at the plan, then the status
page; the milestone files are contracts an executing agent works from.

| File | Answers | |
|---|---|---|
| [PLAYBACK-LIFECYCLE-COVERAGE.md](playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md) | Playback states and transitions, buffer handoffs, communication contracts, existing test anchors, and open acceptance gaps. | open |
| [PLAYBACK-REWRITE-REMAINDER.md](playback-control/PLAYBACK-REWRITE-REMAINDER.md) | What remains unfinished in the rewrite, which old backlog claims are stale, and how each gap intersects with playback freezes. | open |
| [PLAYBACK-LIFECYCLE-IMPLEMENTATION.md](playback-control/PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) | Sol's executable S01–S09 handoff: code entry points, lifecycle/buffer contracts, compiler commands, deferred regression coverage, advisory Developer settings and batched fast-lane delivery. | open |
| [Retained playback observations](evidence/playback-lifecycle-observation-2026-09-11.json) | Sanitized September 11 Apple TV observations used by the lifecycle audit; diagnostic evidence, not a runnable test fixture or acceptance result. | done |
| [PLAYBACK-CONTROL-PROTOCOL-PLAN.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-PLAN.md) | The whole design: explicit demand, one owner, prepared handoffs. | open |
| [PLAYBACK-CONTROL-STATUS.md](playback-control/PLAYBACK-CONTROL-STATUS.md) | What is actually built and merged, milestone by milestone. | open |
| [PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](playback-control/PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md) | The numbered work items the milestones execute. | open |
| [PLAYBACK-CONTROL-PROTOCOL-REVIEW.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-REVIEW.md) | Twelve contracts corrected before any code was written. | done |
| [PLAYBACK-CONTROL-PROTOCOL-M1.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M1.md) · [review](playback-control/PLAYBACK-CONTROL-PROTOCOL-M1-REVIEW.md) | M1: the fenced, observable control path. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md) · [review](playback-control/PLAYBACK-CONTROL-PROTOCOL-M2-WEB-REVIEW.md) | M2: the browser shadow reporter. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-ANALYSIS.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-ANALYSIS.md) · [review](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-ANALYSIS-REVIEW.md) | M3: operator-facing analysis control and status. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-LEASE-ACTOR.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-LEASE-ACTOR.md) | M3a: the rolling lease actor that replaced the split control mutex. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md) | M3b: explicit demand lease and producer pacing. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md) | M3c1: the attempt-fenced delivery ledger. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-PRODUCER-EVENTS.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-PRODUCER-EVENTS.md) | M3c2: producer progress and process-exit events. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md) | M3c3: who owns the terminal event, and the orderly client release. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md) | M4: one producer deadline, and the watchdog it removes. | built |
| [REMAINING-ROADMAP-HANDOFF.md](playback-control/REMAINING-ROADMAP-HANDOFF.md) | M5.5 through M9 — the rest of the rewrite, scoped. | open |
| [M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](playback-control/M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md) | M5: moving recovery authority off the clients. | built |
| [M5-FLEET-ACCEPTANCE.md](playback-control/M5-FLEET-ACCEPTANCE.md) | How to accept M5 on the real fleet rather than a branch. | open |
| [M5-FLEET-RESULTS-18886477.md](playback-control/M5-FLEET-RESULTS-18886477.md) | M5 fleet run at `18886477` — protocol partially accepted. | done |
| [M5-FLEET-RESULTS-943D8A9A.md](playback-control/M5-FLEET-RESULTS-943D8A9A.md) | M5 fleet run at `943d8a9a` — deployed, acceptance not established. | done |
| [M5-FLEET-RESULTS-C571A50D.md](playback-control/M5-FLEET-RESULTS-C571A50D.md) | M5 fleet run at `c571a50d` — deployed, acceptance blocked. | done |
| [M5.5-PREPARATION-FEASIBILITY-SPIKE.md](playback-control/M5.5-PREPARATION-FEASIBILITY-SPIKE.md) | Can a client hold two pipelines at once? The spike that decides. | open |
| [M5.5-SPIKE-EXECUTION-HANDOFF.md](playback-control/M5.5-SPIKE-EXECUTION-HANDOFF.md) | Running that spike on real hardware, without Paul. | open |
| [M5.5-STAGED-GENERATIONS-HANDOFF.md](playback-control/M5.5-STAGED-GENERATIONS-HANDOFF.md) | A successor stream that exists without being current. | open |
| [M6-CALLER-HANDOFF.md](playback-control/M6-CALLER-HANDOFF.md) | M6: where the preparation decision is made, and by whom. | open |
| [M6-IMPLEMENTATION-HANDOFF.md](playback-control/M6-IMPLEMENTATION-HANDOFF.md) | M6: the prepared-recipe contract, with its measured numbers. | open |
| [M6-AXIS-CASE-HANDOFF.md](playback-control/M6-AXIS-CASE-HANDOFF.md) | The one measurement M6 waits on, and how to take it. | done |
| [M6-AXIS-CASE-RESULTS.md](playback-control/M6-AXIS-CASE-RESULTS.md) | The 30 Mbit/s axis run, below the throughput floor and superseded the same day by a 40 Mbit/s re-run that admitted the pair. Read the correction at the top. | superseded |
| [M6-CLIENT-REPLACEMENT-CONTRACT.md](playback-control/M6-CLIENT-REPLACEMENT-CONTRACT.md) | The byte-level wire contract all three clients implement against. | open |
| [CLIENT-PREPARED-SWITCH-CONTRACT.md](playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md) | The streaming-reliability prepared-switch adapter contract shared by Apple, Android, and web. | built |
| [CLIENT-APPLE-HANDOFF.md](playback-control/CLIENT-APPLE-HANDOFF.md) | The implementation handoff for Apple prepared-switch message adaptation. | built |
| [CLIENT-ANDROID-HANDOFF.md](playback-control/CLIENT-ANDROID-HANDOFF.md) | The implementation handoff for Android prepared-switch message adaptation. | built |
| [CLIENT-WEB-HANDOFF.md](playback-control/CLIENT-WEB-HANDOFF.md) | The implementation handoff for web prepared-switch message adaptation. | built |
| [M6-APPLE-CLIENT-BUILD.md](playback-control/M6-APPLE-CLIENT-BUILD.md) | Building the Apple half of M6 — the only one a viewer sees. | open |
| [M6-WEB-CLIENT-BUILD.md](playback-control/M6-WEB-CLIENT-BUILD.md) | Building the web half of M6, proven by tests until a measurement lands. | open |
| [M6-ANDROID-CLIENT-BUILD.md](playback-control/M6-ANDROID-CLIENT-BUILD.md) | Building the Android half of M6, proven by tests until a measurement lands. | open |
| [M6-ANDROID-CLIENT-STATUS.md](playback-control/M6-ANDROID-CLIENT-STATUS.md) | M6: what the Android client does with a staged successor, and how an operator turns it on. | built |
| [M6-APPLE-HARDWARE-ACCEPTANCE.md](playback-control/M6-APPLE-HARDWARE-ACCEPTANCE.md) | The two M6 numbers a simulator cannot supply, and the prompt that takes them. | open |
| [M6-WEB-CLIENT.md](playback-control/M6-WEB-CLIENT.md) | M6: what the browser does with a staged successor, and how an operator turns it on. | built |
| [M6-SERVER-PRIME-HANDOFF.md](playback-control/M6-SERVER-PRIME-HANDOFF.md) | Phase 3 — the reserve/prime constraints and the implementation that now attaches the staged VOD worker. | built |
| [M7-REMAINDER-HANDOFF.md](playback-control/M7-REMAINDER-HANDOFF.md) | M7: subtitle readiness, bounded materialization, seek coalescing, burn-join, prewarm. | open |
| [M7-R-M3-CLAUDE-HANDOFF.md](playback-control/M7-R-M3-CLAUDE-HANDOFF.md) | Finish M2, then build seek coalescing. | open |
| [M7-M1-LARGE-MKV-OBSERVATION.md](playback-control/M7-M1-LARGE-MKV-OBSERVATION.md) | Large-MKV readiness and bounded subtitle publication, observed. | done |
| [M7-M1-M3-BLOCKER-REVIEW.md](playback-control/M7-M1-M3-BLOCKER-REVIEW.md) | Why the M7 remainder is not accepted yet. | open |
| [M7-M4-BURN-JOIN-DESIGN-REVIEW.md](playback-control/M7-M4-BURN-JOIN-DESIGN-REVIEW.md) | The burn-join design: approve, with one contract correction. | done |
| [MARKER-PREWARM-HANDOFF.md](playback-control/MARKER-PREWARM-HANDOFF.md) | Marker-destination prewarm — the consumer its metric waits for. | open |
| [CONTENT-ANALYSIS-INDEX-HANDOFF.md](playback-control/CONTENT-ANALYSIS-INDEX-HANDOFF.md) | Exact skip markers, an explicit queue, and its instrumentation. | open |

---

## streaming/ — delivery: VOD, Dolby Vision, capabilities, reliability

Everything about what gets sent to a player and why: the VOD presentation
programme, the capability negotiation that picks a grade, Dolby Vision
conversion, decoder selection, and the stall/stutter investigations.

| File | Answers | |
|---|---|---|
| [VOD-PRESENTATION-PLAN.md](streaming/VOD-PRESENTATION-PLAN.md) | Every title is a film, not a broadcast — the programme. | open |
| [VOD-PRESENTATION-PLAN-REVIEW.md](streaming/VOD-PRESENTATION-PLAN-REVIEW.md) · [response](streaming/VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md) | Six contracts that were not buildable, and the answers to them. | done |
| [VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md](streaming/VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md) | How to build the plan without breaking the live path. | built |
| [VOD-PRESENTATION-M0-REVIEW-BRIEF.md](streaming/VOD-PRESENTATION-M0-REVIEW-BRIEF.md) | Three normative sentences re-decided before M1. | done |
| [VOD-M0-ISSUES.md](streaming/VOD-M0-ISSUES.md) | Every issue M0 found. | done |
| [VOD-M2-QUESTIONS.md](streaming/VOD-M2-QUESTIONS.md) | The M2 questions and how each was ruled. | done |
| [VOD-M3-HANDOFF.md](streaming/VOD-M3-HANDOFF.md) | Serving from the plan — the first code on the live path. | built |
| [VOD-M4-HANDOFF.md](streaming/VOD-M4-HANDOFF.md) | Web adoption and release acceptance. | built |
| [VOD-CUTOVER.md](streaming/VOD-CUTOVER.md) | The VOD-only HLS cutover, and what it retired. | built |
| [VOD-ENCODING.md](streaming/VOD-ENCODING.md) | How immutable transcode and subtitle-burn VOD produce and verify the bytes named by a playlist. | built |
| [VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md](streaming/VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md) | Restoring the seek-storm case that CI disabled 2026-08-26. | open |
| [VOD-STALL-ACCEPTANCE-HANDOFF.md](streaming/VOD-STALL-ACCEPTANCE-HANDOFF.md) | Restoring the bandwidth-recovery case; blocked on a device measurement. | open |
| [VOD-STEADY-ACCEPTANCE-HANDOFF.md](streaming/VOD-STEADY-ACCEPTANCE-HANDOFF.md) | Restoring the steady-play case that CI disabled 2026-08-26. | open |
| [STREAMING-RELIABILITY-REVIEW.md](streaming/STREAMING-RELIABILITY-REVIEW.md) | Keeping the stream alive while its future changes. | done |
| [STREAMING-RELIABILITY-STATUS.md](streaming/STREAMING-RELIABILITY-STATUS.md) | Review, repair and promotion status of that effort. | open |
| [STREAMING-RELIABILITY-HANDOFF.md](streaming/STREAMING-RELIABILITY-HANDOFF.md) | The remaining work, for the next streaming agent. | open |
| [STREAMING-CONTINUATION-HANDOFF.md](streaming/STREAMING-CONTINUATION-HANDOFF.md) | The continuation-session workflow, branch state, and integration queue for streaming reliability. | open |
| [PLAYBACK-CAPS-V2-PLAN.md](streaming/PLAYBACK-CAPS-V2-PLAN.md) | Highest deliverable grade, negotiated rather than guessed. | open |
| [PLAYBACK-CAPS-V2-M0.md](streaming/PLAYBACK-CAPS-V2-M0.md) | The M0 measurements taken on nuc4, 2026-08-30. | done |
| [MEDIA-BADGES-PLAN.md](streaming/MEDIA-BADGES-PLAN.md) | Making the play menu tell the truth about HDR and Dolby Vision. | built |
| [M5A-CLIENT-BADGE-HANDOFF.md](streaming/M5A-CLIENT-BADGE-HANDOFF.md) | The `DV P7 → DV P8` badge in the Apple and Android clients. | built |
| [M5A-VERIFICATION-ON-NUC4.md](streaming/M5A-VERIFICATION-ON-NUC4.md) | The container-truth check that has to run on real media. | open |
| [M5B_STATUS.md](streaming/M5B_STATUS.md) | Permanent Dolby Vision Profile 7 conversion — what merged. | built |
| [M5-VERIFICATION-PROMPT.md](streaming/M5-VERIFICATION-PROMPT.md) | Fleet verification: the first converted stream a browser ever plays. | open |
| [DV-DELIVERY-FINDINGS.md](streaming/DV-DELIVERY-FINDINGS.md) | Why Dolby Vision titles arrive as HDR10 or lower. | open |
| [DV-DISK-CONVERSION-DIAGNOSIS.md](streaming/DV-DISK-CONVERSION-DIAGNOSIS.md) | Why the on-disk conversion never completes, and a proposal. | open |
| [QUEUE-REPAIR-VERIFICATION-PROMPT.md](streaming/QUEUE-REPAIR-VERIFICATION-PROMPT.md) | Did the fragment-index queue come back, and stay back? | open |
| [DECODER_SELECTION_AND_RECOVERY_PLAN.md](streaming/DECODER_SELECTION_AND_RECOVERY_PLAN.md) | An explicit decoder choice and recovery path for each producer. | open |
| [DECODER_SELECTION_RECOVERY_STATUS.md](DECODER_SELECTION_RECOVERY_STATUS.md) | The live execution ledger for that plan: what is merged, what was tested, and what remains unsafe. | open |
| [DECODER-EFFORT-HANDOFF.md](streaming/DECODER-EFFORT-HANDOFF.md) | Picking the decoder effort up: what blocks it, what is left, how to work here, and the traps it already fell into. | open |
| [DECODER-M8-HANDOFF.md](streaming/DECODER-M8-HANDOFF.md) | Running the remaining hardware, workload, latency, concurrency, and three-client evidence after the decoder implementation merged. | open |
| [AVI_VIDEOTOOLBOX_REVIEW_DECISION.md](streaming/AVI_VIDEOTOOLBOX_REVIEW_DECISION.md) | The VideoToolbox decode fix to build, and the follow-up it requires. | open |
| [APPLE-PACING-HOLD-FREEZE-ROOT-CAUSE.md](streaming/APPLE-PACING-HOLD-FREEZE-ROOT-CAUSE.md) | Why Apple and web HLS froze on a pacing hold, and the repair contract. | built |
| [STUTTER-4K.md](streaming/STUTTER-4K.md) | 4K copy-path stutter: what it is, what it isn't, what to try next. | open |
| [SEGMENTER-PLAN.md](streaming/SEGMENTER-PLAN.md) | GOP-aware segmenting — zero boundary drops on the copy path. | built |
| [ADAPTIVE-QUALITY.md](streaming/ADAPTIVE-QUALITY.md) | The design for bandwidth-aware streaming. | live |

---

## cluster/ — replication, membership, and recovery

Phase 4 and everything under it: the clustering transition, the performance
and media-pool work built on top, and the diagnoses of specific replicated
failures.

| File | Answers | |
|---|---|---|
| [CLUSTERING-PLAN.md](cluster/CLUSTERING-PLAN.md) | From one plurxd node to Phase 4, milestone by milestone. | open |
| [CLUSTERING-PLAN-REVIEW.md](cluster/CLUSTERING-PLAN-REVIEW.md) | The right first slice, on top of eight contracts that did not hold. | done |
| [PHASE3-SPIKE.md](cluster/PHASE3-SPIKE.md) | The decision gate: two HA risks spiked, one answer. | done |
| [CLUSTER-PERFORMANCE-PLAN.md](cluster/CLUSTER-PERFORMANCE-PLAN.md) | Turning replicated correctness into useful capacity. | open |
| [CLUSTER-MEDIA-POOL-PLAN.md](cluster/CLUSTER-MEDIA-POOL-PLAN.md) | Making every node improve playback rather than just hold state. | built |
| [CLUSTER-PANEL-FOLLOWUPS.md](cluster/CLUSTER-PANEL-FOLLOWUPS.md) | Live status, a real module seam, one credential surface. | open |
| [CLUSTER_TRANSPORT_RECOVERY_IMPLEMENTATION.md](cluster/CLUSTER_TRANSPORT_RECOVERY_IMPLEMENTATION.md) | Fixing buffered writes and bounding recovery end to end. | open |
| [CLUSTER_TRANSPORT_RECOVERY_STATUS.md](cluster/CLUSTER_TRANSPORT_RECOVERY_STATUS.md) | Live implementation status of that effort. | open |
| [CLUSTER-TRANSPORT-RECOVERY-POST-MERGE-HANDOFF.md](cluster/CLUSTER-TRANSPORT-RECOVERY-POST-MERGE-HANDOFF.md) | Finishing qualification and rollout after the merge. | open |
| [TRANSPORT-RECOVERY-CI-STATUS.md](cluster/TRANSPORT-RECOVERY-CI-STATUS.md) | Which independent-role CI milestone is built, reviewed, and proved. | open |
| [TRANSPORT-RECOVERY-RESOURCE-BASELINE.md](cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md) | Why that lane never passed under the per-cycle ceiling: the measurement. | done |
| [TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md) | The campaign-floor contract that replaced it, and the two options not taken. | built |
| [CLUSTER_PAGE_LATENCY_REVIEW.md](cluster/CLUSTER_PAGE_LATENCY_REVIEW.md) | Evidence for why Home, Activity and Settings were slow in a cluster. | done |
| [CLUSTER_PAGE_LATENCY_FIX_PLAN.md](cluster/CLUSTER_PAGE_LATENCY_FIX_PLAN.md) · [review](cluster/CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md) | Restore quorum truth, then unblock first paint. | built |
| [WAL_GENERATION_REPAIR_PLAN.md](cluster/WAL_GENERATION_REPAIR_PLAN.md) · [review](cluster/WAL_GENERATION_REPAIR_PLAN_REVIEW.md) | Making snapshot compaction invalidate every stale reader cache. | built |
| [MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md](cluster/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md) | Making admission an authorization decision. | open |
| [NUC3_SNAPSHOT_CATCHUP_DIAGNOSIS.md](cluster/NUC3_SNAPSHOT_CATCHUP_DIAGNOSIS.md) | Why a live learner waited fifteen idle minutes. | open |
| [ACTIVITY_PEER_READ_FIX_PLAN.md](cluster/ACTIVITY_PEER_READ_FIX_PLAN.md) | Coalescing peer-read bursts and reporting failures truthfully. | built |

---

## clients/ — Apple, Android, and the web player

The native clients and the web UI: parity trackers, the input contract every
player obeys, subtitles and overlays, layouts and themes.

| File | Answers | |
|---|---|---|
| [APPLE-CLIENT-PARITY.md](clients/APPLE-CLIENT-PARITY.md) | What the Apple client has, what it lacks, and which build proved it. | live |
| [ANDROID-CLIENT-PARITY.md](clients/ANDROID-CLIENT-PARITY.md) | The same, for Android. | live |
| [PLAYER-INPUT-CONTRACT.md](clients/PLAYER-INPUT-CONTRACT.md) | One routing table every client obeys — which key does what, in which state. | live |
| [PLAYER-INPUT-CONTRACT-PLAN.md](clients/PLAYER-INPUT-CONTRACT-PLAN.md) | How that contract was implemented. | built |
| [PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md](clients/PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md) | What the physical devices did on 2026-09-02. | done |
| [UI-NAVIGATION-AUDIT.md](clients/UI-NAVIGATION-AUDIT.md) | Why the player felt different on every client, anchored at `file:line`. | done |
| [CLIENTS-CODE-REVIEW.md](clients/CLIENTS-CODE-REVIEW.md) · [assessment](clients/CLIENTS-CODE-REVIEW-ASSESSMENT.md) | Capable players that under-ask the server — findings, and what to trust. | done |
| [CLIENTS-REMEDIATION-PLAN.md](clients/CLIENTS-REMEDIATION-PLAN.md) | Restoring trust, then raising the quality ceiling. | built |
| [APPLE-NATIVE-SUBTITLES-PLAN.md](clients/APPLE-NATIVE-SUBTITLES-PLAN.md) · [handoff](clients/APPLE-NATIVE-SUBTITLES-HANDOFF.md) | Native text subtitles on Apple: the road, and what shipped. | built |
| [PGS_OVERLAY_PLAN.md](clients/PGS_OVERLAY_PLAN.md) | Dolby Vision-safe PGS subtitle overlay. | open |
| [PGS-OVERLAY-M0-FEASIBILITY.md](clients/PGS-OVERLAY-M0-FEASIBILITY.md) | The feasibility evidence for M0. | open |
| [PGS-OVERLAY-REVIEW-ASSESSMENT.md](clients/PGS-OVERLAY-REVIEW-ASSESSMENT.md) | Accepted findings, and the re-review requested. | done |
| [APPLE-PGS-OVERLAY-ACCEPTANCE.md](clients/APPLE-PGS-OVERLAY-ACCEPTANCE.md) | One iPad Pro run, one decidable acceptance record. | open |
| [UI-LAYOUTS-PLAN.md](clients/UI-LAYOUTS-PLAN.md) | Four layouts and five themes, proposed for review. | done |
| [UI-LAYOUTS-REVIEW.md](clients/UI-LAYOUTS-REVIEW.md) | Ship a smaller, honest first slice. | done |
| [UI-LAYOUTS-IMPLEMENTATION.md](clients/UI-LAYOUTS-IMPLEMENTATION.md) | The accepted slice, as built. | built |
| [UI-LAYOUTS-STATUS.md](clients/UI-LAYOUTS-STATUS.md) | Ground truth for what of that slice is proven. | open |
| [UI-LAYOUTS-G3-DECISION.md](clients/UI-LAYOUTS-G3-DECISION.md) | Did the layout abstraction pay for itself? | done |
| [WEB_LAYOUT_CONTAINMENT_STATUS.md](clients/WEB_LAYOUT_CONTAINMENT_STATUS.md) | Live delivery status of web layout containment. | open |
| [OFFLINE-VIEWING-PLAN.md](clients/OFFLINE-VIEWING-PLAN.md) · [review](clients/OFFLINE-VIEWING-REVIEW.md) | One-tap, app-managed downloads — the plan and its review. | built |
| [EBOOK-READER-PLAN.md](clients/EBOOK-READER-PLAN.md) | plurx reads what Curator acquires. | open |
| [CLIENT-DEPLOY-PROMPT.md](clients/CLIENT-DEPLOY-PROMPT.md) | Getting a merged build onto the phones and the Apple TVs. | live |

---

## performance/ — where the seconds go

Two rounds of performance work, each with its plan, review, and response.

| File | Answers | |
|---|---|---|
| [PERF-PLAN.md](performance/PERF-PLAN.md) | Round one: where the seconds go, and the plan to get them back. | built |
| [PERF-PLAN-REVIEW.md](performance/PERF-PLAN-REVIEW.md) | What to correct before implementing it. | done |
| [PERF-REVIEW-RESPONSE.md](performance/PERF-REVIEW-RESPONSE.md) | What the review got right, what already shipped, what changed. | done |
| [PERF-REVIEW-ASSESSMENT.md](performance/PERF-REVIEW-ASSESSMENT.md) | What is resolved and what still needs a contract. | done |
| [PERF2-PLAN.md](performance/PERF2-PLAN.md) | Round two: better starts, steadier streams, smarter bits. | open |
| [PERF2-PLAN-REVIEW.md](performance/PERF2-PLAN-REVIEW.md) | The seam contracts are not implementation-ready. | done |
| [PERF2-REVIEW-RESPONSE.md](performance/PERF2-REVIEW-RESPONSE.md) | All eight findings accepted. | done |
| [PERF2-IMPLEMENTATION-HANDOFF.md](performance/PERF2-IMPLEMENTATION-HANDOFF.md) | How to build round two without breaking it. | open |
| [CODE-QUALITY-PERFORMANCE-REVIEW.md](performance/CODE-QUALITY-PERFORMANCE-REVIEW.md) | A strong data plane with lifecycle gaps. | done |

---

## ci/ — the build, the gate, and the agents that use it

| File | Answers | |
|---|---|---|
| [RIPWIRE-PILOT.md](ci/RIPWIRE-PILOT.md) | Measured query cost, source/fixture omissions, and completed opt-in adoption verdict. | open |
| [RIPWIRE.md](ci/RIPWIRE.md) | Explicit setup, bounded navigation, output meanings, and coverage limits. | live |
| [RIPWIRE-STATUS.md](ci/RIPWIRE-STATUS.md) | Ripwire implementation, measured evidence, decisions, and promotion progress. | open |
| [CI_TEST_OVERHAUL_PLAN.md](ci/CI_TEST_OVERHAUL_PLAN.md) | Fast failures, selective evidence, safe reuse. | open |
| [CI_EXECUTION_ACCELERATION_PLAN.md](ci/CI_EXECUTION_ACCELERATION_PLAN.md) · [review](ci/CI_EXECUTION_ACCELERATION_REVIEW.md) | Persistent caches, native packaging, exact sharding. | open |
| [AGENT-COMPILE-LOOP.md](ci/AGENT-COMPILE-LOOP.md) | A compiler for a checkout that has none. | live |
| [AI-HARNESS-FABLE-ASSESSMENT.md](ci/AI-HARNESS-FABLE-ASSESSMENT.md) | Fable's assessment: fix the compile loop, test evidence, and prompt drift before navigation tooling; positions on Gemini's and Codex's proposals. | open |
| [AI-HARNESS-IMPLEMENTATION-PLAN.md](ci/AI-HARNESS-IMPLEMENTATION-PLAN.md) | Ten milestones, in that order, with exact contracts and acceptance checks: agent-check, tests out of the hotspots, prove-fix, fences, guides, swarm/ alignment, status fragments, PR ledger, Ripwire pilot, first extraction. | open |
| [FORGEJO-MAIN-IMAGE-HANDOFF.md](ci/FORGEJO-MAIN-IMAGE-HANDOFF.md) | Publishing the main image from Forgejo. | open |
| [RUNNER-DISK.md](ci/RUNNER-DISK.md) | What fills a runner, what bounds it, how to reclaim it. | live |
| [NYNUC-RUNNER-ORPHANED-PROCESSES.md](ci/NYNUC-RUNNER-ORPHANED-PROCESSES.md) | Why effort preflight leaked stopped children on nynuc, and how cleanup is proved. | open |

---

## features/ — whole capabilities, planned end to end

| File | Answers | |
|---|---|---|
| [HDHOMERUN-LIVE-TV-PLAN.md](features/HDHOMERUN-LIVE-TV-PLAN.md) | One tuner, every plurx client. | open |
| [HDHOMERUN-LIVE-TV-STATUS.md](features/HDHOMERUN-LIVE-TV-STATUS.md) | What is built and what is proved on a real FLEX 4K. | open |
| [LIVE-TV-ORIGINAL-QUALITY-IMPLEMENTATION.md](features/LIVE-TV-ORIGINAL-QUALITY-IMPLEMENTATION.md) | Preserve the broadcast when the player can use it, convert only incompatible tracks, and track the effort to main. | built |
| [LIVE-TV-GUIDE-AND-UI-PLAN.md](features/LIVE-TV-GUIDE-AND-UI-PLAN.md) | The Live TV page rebuilt — list and grid views, the guide feed, fullscreen and picture-in-picture on every client. | open |
| [LIBRARY-CHANNELS-STATUS.md](features/LIBRARY-CHANNELS-STATUS.md) | Where Library channels is, what is proved, and what remains before promotion. | open |
| [LIBRARY-CHANNELS-PLAYBACK-REPAIR.md](features/LIBRARY-CHANNELS-PLAYBACK-REPAIR.md) | Repair progress, native-muxer diagnostic, and deployed channel acceptance evidence. | open |
| [WEEKLY-REVIEW-REMEDIATION-STATUS.md](features/WEEKLY-REVIEW-REMEDIATION-STATUS.md) | Which September 3–9 security, recovery, and playback findings were fixed, and which remain separately scoped capabilities. | done |
| [LIVE-TV-NATIVE-LAYOUTS-STATUS.md](features/LIVE-TV-NATIVE-LAYOUTS-STATUS.md) | Three native TV presentations, compact mobile browsing, and the exact implementation evidence. | open |
| [SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md](features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md) | What is built, reviewed, and proved for the settings navigation and Developer-page redesign. | open |
| [HOMEVIDEO-PLAN.md](features/HOMEVIDEO-PLAN.md) | Home video and photos — the `home` libraries. | built |
| [INTEGRATION-PLAN.md](features/INTEGRATION-PLAN.md) | plurx's side of the monarr pipeline. | built |
| [WINDOWS-PORT-PLAN.md](features/WINDOWS-PORT-PLAN.md) | What a native `plurxd.exe` would take. | open |

---

## reviews/ — pull-request review records

One file per reviewed PR, each recording the verdict and the findings against
a named head commit. Kept because the findings are cited elsewhere; none of
them describe current behavior.

[PR #79](reviews/PR79-CINEMA-HANDOFF-REVIEW.md) ·
[#80](reviews/PR80-ARTWORK-RETRY-REVIEW.md) ·
[#81](reviews/PR81-ARTWORK-RETRY-RESPONSE-REVIEW.md) ·
[#82](reviews/PR82-CLUSTERING-M0-REVIEW.md) ·
[#83](reviews/PR83-CLUSTERING-M1A-REVIEW.md) ·
[#84](reviews/PR84-CLUSTERING-M1B-REVIEW.md) ·
[#88](reviews/PR88-CLUSTERING-M1C-REVIEW.md) ·
[#89](reviews/PR89-IPAD-SYSTEM-CHROME-REVIEW.md) ·
[#90](reviews/PR90-CLUSTERING-M1D-REVIEW.md) ·
[#91](reviews/PR91-APPLE-HLS-AHEAD-WINDOW-REVIEW.md) ·
[#92](reviews/PR92-SKIP-CREDITS-LAYOUT-REVIEW.md) ·
[#94](reviews/PR94-ARTWORK-CACHE-REVIEW.md) ·
[#98](reviews/PR98-CLUSTERING-IMPORT-BACKUP-REVIEW.md) ·
[effort/live-tv-guide](reviews/EFFORT-LIVE-TV-GUIDE-REVIEW.md)

---

## archive/ — finished, kept for history

| File | Answers |
|---|---|
| [RETRO-REVIEW-2026-08-09.md](archive/RETRO-REVIEW-2026-08-09.md) | The retrospective for 2026-08-04 → 08-08, with its eleven work orders in [retro-2026-08-09/](archive/retro-2026-08-09/). |
| [PERF-REVIEW-RESPONSE-ASSESSMENT.md](archive/PERF-REVIEW-RESPONSE-ASSESSMENT.md) | A byte-identical duplicate of [performance/PERF-REVIEW-ASSESSMENT.md](performance/PERF-REVIEW-ASSESSMENT.md), kept so older links resolve. |

## Adding a document

Put it in the subject folder its work belongs to, add a row here in the same
commit, and give it the header the rest of the set uses — an H1 with an
em-dash tagline, then a `**Status:**` line carrying an absolute date. A plan
or handoff that an agent will execute also states what it executes and what
it must not touch. If a document has no subject folder, it probably belongs
in the reference set at the root — or it is a plan for something that does
not exist yet, which is a reason to write the plan, not a reason to add a
folder.

This index is kept honest by `tests/operations/test_docs_index.py`: every
Markdown file under `docs/` must appear here, every link here must resolve,
and no link anywhere in the repo may point at a `docs/` path that does not
exist.
