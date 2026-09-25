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

Mobile UI work: [Apple build 159 notes](apple-builds/317-ui-usability.md) · [Mobile and TV usability audit](clients/MOBILE-UI-USABILITY-AUDIT.md) — **open**.

Apple build 160: [Native Live TV and shelf layouts](apple-builds/320-native-layout-followup.md).

Native follow-up: [Phone, tablet and TV layout fixes](clients/NATIVE-LAYOUT-FOLLOWUP.md) — **open**.

Native Home restoration: [Original Apple Home screens](apple-builds/334-original-home.md) — **open**.

Library-page revision: [Quiet Home and open media details](clients/CALM-LIBRARY-PAGES.md) — **built**; web Home and item page since restored to their originals.

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
| Why is a title with PGS subtitles slow or refusing to start? | [clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) · [review](clients/PGS-SUBTITLE-START-PATH-RCA-REVIEW.md) |
| Why does a first subtitle wait for a full read on one node? | [clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md](clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) · [review](clients/SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md) · [status](clients/SUBTITLE-CLUSTER-EXTRACTION-STATUS.md) |
| Why did subtitles refuse, not appear, or stop? | [clients/SUBTITLE-RELIABILITY-ASSESSMENT.md](clients/SUBTITLE-RELIABILITY-ASSESSMENT.md) · [handoff](clients/SUBTITLE-RELIABILITY-HANDOFF.md) · [physical verification](clients/SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) |
| Why is there an error overlay while the picture is still playing? | [clients/PLAYBACK-SURFACE-CONTRACT.md](clients/PLAYBACK-SURFACE-CONTRACT.md) |
| Why does Live TV freeze a few seconds after it starts? | [features/LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md](features/LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md) |
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
| [INTEGRATION.md](INTEGRATION.md) | Every seam with Curator, and how to prove each one works. |
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
Apple build 143: [Playback lifecycle recovery and prepared handoff](apple-builds/257-playback-lifecycle.md).
Apple build 147: [The playback surface contract](apple-builds/278-playback-surface-contract.md).
Apple build 151: [Playback surface reach](apple-builds/278-playback-surface-reach.md).
Apple build 152: [Banner actions](apple-builds/278-banner-actions.md).
Apple build 153: [Live TV stall and fullscreen surface](apple-builds/300-live-tv-surface.md).

Previous: [Live TV playback method](apple-builds/245-live-tv-playback-method.md).

---

## playback-control/ — the playback control rewrite

Explicit demand, one owner per stream, prepared handoffs. The largest single
thread in the repo: a protocol plan, milestone contracts and implementation
receipts. Start at the current rewrite remainder, then the lifecycle coverage
map; older milestone files are historical contracts, not a fresh missing-work
list.

| File | Answers | |
|---|---|---|
| [STATUS-HISTORY.md](playback-control/STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [REPLACEMENT-GATE-SUPERSESSION-RCA.md](playback-control/REPLACEMENT-GATE-SUPERSESSION-RCA.md) | Why an abandoned replacement held its player key forever, the m6 evidence, and the supersedable gate that fixes it. | built |
| [CALLBACK-ROOT-CAUSE-AND-FIX.md](playback-control/CALLBACK-ROOT-CAUSE-AND-FIX.md) | Why Safari cold resume lost frame callbacks, the lifecycle fix, controlled evidence, and questions for Fable review. | open |
| [PLAYBACK-LIFECYCLE-COVERAGE.md](playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md) | Playback states and transitions, buffer handoffs, communication contracts, existing test anchors, and open acceptance gaps. | open |
| [PLAYBACK-REWRITE-REMAINDER.md](playback-control/PLAYBACK-REWRITE-REMAINDER.md) | Executed Sol handoff: the finite B01–B05 remainder, its boundaries, and the work promoted through PR #263. | done |
| [PLAYBACK-LIFECYCLE-IMPLEMENTATION.md](playback-control/PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) | Completed S01–S09 implementation contract: code entry points, lifecycle/buffer contracts, compiler commands, deferred regression coverage, advisory Developer settings and batched fast-lane delivery. | built |
| [PLAYBACK-LIFECYCLE-STATUS.md](playback-control/PLAYBACK-LIFECYCLE-STATUS.md) | Completed execution ledger for the lifecycle implementation: package state, compilation, deferred tests, review, qualification, promotion and cleanup. | done |
| [PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md](playback-control/PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md) | Completed execution ledger and live measurement contract for playhead-anchored server readiness, HTTP delivery, client loaded ranges, and presentation progress. | built |
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
| [M6-CALLER-HANDOFF.md](playback-control/M6-CALLER-HANDOFF.md) | Historical M6 caller design; its proof/headroom admission rules are superseded by the lifecycle implementation contract. | superseded |
| [M6-IMPLEMENTATION-HANDOFF.md](playback-control/M6-IMPLEMENTATION-HANDOFF.md) | Historical M6 prepared-recipe contract and measurements; use the lifecycle contract for current policy. | superseded |
| [M6-AXIS-CASE-HANDOFF.md](playback-control/M6-AXIS-CASE-HANDOFF.md) | The one measurement M6 waits on, and how to take it. | done |
| [M6-AXIS-CASE-RESULTS.md](playback-control/M6-AXIS-CASE-RESULTS.md) | The 30 Mbit/s axis run, below the throughput floor and superseded the same day by a 40 Mbit/s re-run that admitted the pair. Read the correction at the top. | superseded |
| [M6-CLIENT-REPLACEMENT-CONTRACT.md](playback-control/M6-CLIENT-REPLACEMENT-CONTRACT.md) | Historical v1 client-wire snapshot; the lifecycle contract supersedes its software proof and throughput gates. | superseded |
| [CLIENT-PREPARED-SWITCH-CONTRACT.md](playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md) | The streaming-reliability prepared-switch adapter contract shared by Apple, Android, and web. | built |
| [CLIENT-APPLE-HANDOFF.md](playback-control/CLIENT-APPLE-HANDOFF.md) | The implementation handoff for Apple prepared-switch message adaptation. | built |
| [CLIENT-ANDROID-HANDOFF.md](playback-control/CLIENT-ANDROID-HANDOFF.md) | The implementation handoff for Android prepared-switch message adaptation. | built |
| [CLIENT-WEB-HANDOFF.md](playback-control/CLIENT-WEB-HANDOFF.md) | The implementation handoff for web prepared-switch message adaptation. | built |
| [M6-APPLE-CLIENT-BUILD.md](playback-control/M6-APPLE-CLIENT-BUILD.md) | Historical Apple M6 build brief; current enablement and retry policy lives in the lifecycle status. | superseded |
| [M6-WEB-CLIENT-BUILD.md](playback-control/M6-WEB-CLIENT-BUILD.md) | Historical web M6 build brief; current enablement and first-frame fallback lives in the lifecycle status. | superseded |
| [M6-ANDROID-CLIENT-BUILD.md](playback-control/M6-ANDROID-CLIENT-BUILD.md) | Historical Android M6 build brief; current enablement and retry policy lives in the lifecycle status. | superseded |
| [M6-ANDROID-CLIENT-STATUS.md](playback-control/M6-ANDROID-CLIENT-STATUS.md) | Historical Android implementation record; its qualification prose is not current admission policy. | superseded |
| [M6-APPLE-HARDWARE-ACCEPTANCE.md](playback-control/M6-APPLE-HARDWARE-ACCEPTANCE.md) | Historical hardware procedure; the lifecycle status contains the current finite sweep card. | superseded |
| [M6-WEB-CLIENT.md](playback-control/M6-WEB-CLIENT.md) | Historical web implementation record; its throughput qualification prose is not current admission policy. | superseded |
| [M6-SERVER-PRIME-HANDOFF.md](playback-control/M6-SERVER-PRIME-HANDOFF.md) | Phase 3 — the reserve/prime constraints and the implementation that now attaches the staged VOD worker. | built |
| [QUALITY-SWITCH-CONTINUITY-PLAN.md](playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md) | Why every rung change is still a reopen although the prepared handoff is built and deployed on every side, and the plan (M0–M3) to make a quality change a handoff with the incumbent playing throughout. | open |
| [QUALITY-SWITCH-CONTINUITY-BUILD.md](playback-control/QUALITY-SWITCH-CONTINUITY-BUILD.md) | The build plan for the executing agent: M0 server (`delivery.preparation`, supersession cancels the successor), M2 Apple/Android offer wait + rendezvous, M1 web owner + alignment, M3 measurement; Astra review disposition in §11. | open |
| [QUALITY-SWITCH-CONTINUITY-RESULTS.md](playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md) | The acceptance record for the prepared quality handoff: what each client measures around the commit, the twenty-change bar, the two measurements deliberately not taken and what they would cost, and empty result rows until a device run fills them. | open |
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
| [STATUS-HISTORY.md](streaming/STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [HONEST-MASTER-PLAYLIST.md](streaming/HONEST-MASTER-PLAYLIST.md) | Implementation plan from the 2026-09-20 architecture review: why a transcode's multivariant playlist advertised the source's bitrate, resolution and no codec string, and how each attribute becomes a measured fact about the bytes this session delivers. | open |
| [CODEC-AND-GPU-QUALIFICATION.md](streaming/CODEC-AND-GPU-QUALIFICATION.md) | Implementation plan from the 2026-09-20 architecture review: what it takes to qualify one more codec/GPU graph end to end, why NVENC and VideoToolbox work is conditional on a fleet inventory, and how Q8's three argument questions are each settled on their own evidence. | open |
| [PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md](streaming/PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md) | Implementation plan from the 2026-09-20 architecture review: why DV Profile 5 was refused and the media-origin probe went dead after the Windows refactor; the fix to `output_job_owned` with a portable child test, the full caller audit, and a bounded, killable library scan probe. | built |
| [MEDIA-BODY-BUFFERS.md](streaming/MEDIA-BODY-BUFFERS.md) | Implementation plan from the 2026-09-20 architecture review: the four 4 KiB `ReaderStream` sites, the exact HLS pump acknowledgement contract, a measurement protocol, and the later ack-batching change. | open |
| [ENCODED-VOD-HOLD-AND-RELEASE.md](streaming/ENCODED-VOD-HOLD-AND-RELEASE.md) | Implementation plan from the 2026-09-20 architecture review: SIGSTOP instead of SIGKILL for encoded producers past the ahead horizon, a low-water mark, and the stopped→release transition driven by `Admissions::live_is_waiting`, proven in pure scheduler tests first. | open |
| [FONT-ATTESTATION-AND-BLOCKING-IO.md](streaming/FONT-ATTESTATION-AND-BLOCKING-IO.md) | Implementation plan from the 2026-09-20 architecture review: moving the font/engine `stat` loops off the runtime, then freezing a burn recipe's fontconfig environment so per-segment `fc-list` becomes unnecessary because the inputs cannot change. | open |
| [FFMPEG-SPAWN-UNIFICATION.md](streaming/FFMPEG-SPAWN-UNIFICATION.md) | Implementation plan from the 2026-09-20 architecture review: one producer spawn path for rolling HLS, VOD and progressive remux and one strict progress-line classifier, with the accidental differences closed by tests. | open |
| [ENCODER-RATE-CONTROL-DEFAULTS.md](streaming/ENCODER-RATE-CONTROL-DEFAULTS.md) | Implementation plan from the 2026-09-20 architecture review: per-encoder-family CRF/QVBR vs ABR evidence with the bench harness before any default flips; HDR10's separate policy and VBV bounds kept. | open |
| [TONE-MAP-CHAIN-CORRECTIONS.md](streaming/TONE-MAP-CHAIN-CORRECTIONS.md) | Implementation plan from the 2026-09-20 architecture review: the hardware-download metadata test first, then MaxCLL at scan, explicit `peak=`, gamut before curve, and dither, with an image-comparison protocol. | open |
| [INTERLACE-IN-THE-MEDIA-CONTRACT.md](streaming/INTERLACE-IN-THE-MEDIA-CONTRACT.md) | Implementation plan from the 2026-09-20 architecture review: field order carried into the media contract, deinterlace before scale with a deliberate frame/field policy, the Live TV field-rate bitrate fix, and the reproduced fixture as acceptance. | open |
| [AUDIO-RESOLVED-INDEPENDENTLY.md](streaming/AUDIO-RESOLVED-INDEPENDENTLY.md) | Implementation plan from the 2026-09-20 architecture review: audio negotiated separately from video so a resolution change or burn no longer downmixes compatible multichannel audio, carried through recipe identity, manifests and handoffs. | open |
| [VOD-BFRAMES-TIMELINE-DESIGN.md](streaming/VOD-BFRAMES-TIMELINE-DESIGN.md) | Design decision from the 2026-09-20 architecture review: keep no-reorder deployed; if evidence ever supports B-frames, only signed version-1 offsets with the exact executable presentation-grid contract and per-family/client qualification are admissible. | open |
| [DECODE-FACTS-GATE-AND-FALLBACK.md](streaming/DECODE-FACTS-GATE-AND-FALLBACK.md) | Implementation plan from the 2026-09-20 architecture review: instrumenting the decode-fact gate, amortising immutable-image validation without losing tamper checks, and classifying fallback reasons. | open |
| [TRANSCODE-DECOMPOSITION-PLAN.md](streaming/TRANSCODE-DECOMPOSITION-PLAN.md) | Implementation plan from the 2026-09-20 architecture review: the behaviour-preserving extraction of `transcode.rs`, `hls.rs` and `vodserve.rs` along ownership boundaries, the test-seam census, and the separate registry-unification evaluation. | open |
| [ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md](streaming/ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md) | Why immersive AC-4 and delayed AC-3 broke live starts, what the fixes preserve, and why the 103.1 capture cannot initialize its decoder. | open |
| [ATSC 3.0 live audio repair status](streaming/ATSC3-AUDIO-STARTUP-STATUS.html) | Implementation, single adversarial review, final fast lane, merge and outstanding live acceptance. | open |
| [LIVE-TV-DIRECT-PLAY-AND-SURROUND.md](streaming/LIVE-TV-DIRECT-PLAY-AND-SURROUND.md) | 2026-09-24: why every ATSC 1.0 channel was encoded and every encode was stereo, the copy-first audio track selection, the sink-derived AAC channel claim, Android hardware MPEG-2, and the aspect-ratio report the server could not reproduce. | built |
| [RESUME-ROLLING-PUBLICATION-RCA.md](streaming/RESUME-ROLLING-PUBLICATION-RCA.md) | Why resume stopped working on every client while starting from the beginning still did: which files fall back to the rolling engine, the web client whose MediaSource never opens, and the progress beat that erases a resume point once a startup fails. | open |
| [SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md](streaming/SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md) | Why repeated seeks refuse playback and then reap the incumbent: retired streams retain a producer's whole reservation, and a refused destination poisons the incumbent's control snapshot. | open |
| [SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md](streaming/SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md) | The build order for that repair: one scratch ledger, a writer barrier, exact release grace, growing reservations, ownership boundaries and the evidence each unit owes. | open |
| [Seek scratch reservations status](streaming/SEEK-SCRATCH-RESERVATIONS-STATUS.html) | Live ledger for the effort: unit state, base and compiler, evidence, review, fast lane and deployment. | open |
| [Seek scratch reservations receipt](evidence/seek-scratch-reservations-receipt-2026-09-20.md) | Exact base and compiler, the accounting/release/mechanism proofs, which regression covers which acceptance row, what already failed before the change, and what is still owed. | open |
| [FFmpeg HLS write boundary receipt](evidence/ffmpeg-hls-write-boundary-receipt-2026-09-24.md) | The last whole-ceiling reservation removed: FFmpeg's HLS muxer uploads through a loopback endpoint that grants every piece before it lands, the legacy copy hole, the inverted grant wait, and the proofs. | open |
| [MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md](streaming/MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md) | Why one valid MKV was denied immutable VOD, why its rolling fallback violated AVFoundation's playlist-update deadline, and the packet-duration plus sliding-HLS repair contracts. | open |
| [MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md](streaming/MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md) | The build contract for the reviewed MKV-duration and sliding-HLS repair: startup admission, packet evidence, publication scheduling, object grace, and acceptance. | open |
| [MKV-DURATION-AND-SLIDING-HLS-STATUS.md](streaming/MKV-DURATION-AND-SLIDING-HLS-STATUS.md) | Live package, compiler, review, qualification, and production-authorization ledger for the MKV-duration and sliding-HLS repair. | open |
| [APPLE-TV-FORWARD-SKIPS-STATUS.md](streaming/APPLE-TV-FORWARD-SKIPS-STATUS.md) | Live implementation, review, focused-proof, fast-lane and merge ledger for cumulative rolling-publication pacing and Apple TV position continuity. | open |
| [CONTENT-ANALYSIS-REPAIR-STATUS.md](streaming/CONTENT-ANALYSIS-REPAIR-STATUS.md) | Live implementation ledger for selected-video completion, bounded retry, exact-identity repair, review, and promotion. | open |
| [CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md](streaming/CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md) | Why complete video indexes were labelled incomplete: fleet evidence, selected-stream duration, timeout policy, and recovery constraints. | open |
| [CONTENT-ANALYSIS-FAILURES-ADVERSARIAL-REVIEW.md](streaming/CONTENT-ANALYSIS-FAILURES-ADVERSARIAL-REVIEW.md) | Independent design review that found retry expiry and lost video identity before implementation. | done |
| [CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md](streaming/CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md) | Build contract for selected-video completion, typed diagnostics, bounded retry cycles, exact-identity recovery, migrations, tests, and rollout. | open |
| [HEVC-SAMPLE-ENTRY-STATUS.md](streaming/HEVC-SAMPLE-ENTRY-STATUS.md) | Live execution ledger for the HEVC sample-entry admission repair: implementation, evidence, review, and promotion state. | open |
| [SAFARI-DIAGNOSIS-AND-FIX.md](streaming/SAFARI-DIAGNOSIS-AND-FIX.md) | Why HEVC-in-MP4 admission fails on Safari, the decoder evidence behind the repair, and the accepted review findings. | open |
| [HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md](streaming/HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md) | Executable contract for source sample-entry facts, compatible packaging, client admission, downgrade safety, and focused acceptance. | open |
| [HEVC sample-entry qualification receipt](evidence/hevc-sample-entry-qualification-2026-09-16.md) | Exact reviewed head, focused commands and counts, F1–F10 disposition, rollout order, and physical-evidence limits for PR #337. | built |
| [TCL-PROBE-MISMATCH-RCA-AND-FIX.md](streaming/TCL-PROBE-MISMATCH-RCA-AND-FIX.md) | Why one added E-AC-3 Atmos profile refused a whole movie, what shipped to admit it, and why report equality is the wrong source-verification contract. | built |
| [Probe compatibility replay](evidence/probe-compatibility-replay.py) | Runs the deployed and shipped source-probe comparators side by side, on a synthetic pair or on two real FFprobe documents. | built |
| [STREAMING-RELIABILITY-IMPLEMENTATION.md](streaming/STREAMING-RELIABILITY-IMPLEMENTATION.md) | Two-wave reliability effort: quality preservation, truthful recovery, first-play preparation, task ownership and finite qualification. | open |
| [STREAMING-SHARED-INDEX-HANDOFF.md](streaming/STREAMING-SHARED-INDEX-HANDOFF.md) | Sol work package for exact shared Dolby Vision indexes and bounded first-play measurements. | open |
| [STREAMING-WEB-RECOVERY-HANDOFF.md](streaming/STREAMING-WEB-RECOVERY-HANDOFF.md) | Sol work package for truthful web stall evidence, recipe-preserving recovery and native parity. | open |
| [VOD-PRESENTATION-PLAN.md](streaming/VOD-PRESENTATION-PLAN.md) | Every title is a film, not a broadcast — the programme. | open |
| [VOD-PRESENTATION-PLAN-REVIEW.md](streaming/VOD-PRESENTATION-PLAN-REVIEW.md) · [response](streaming/VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md) | Six contracts that were not buildable, and the answers to them. | done |
| [VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md](streaming/VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md) | How to build the plan without breaking the live path. | open |
| [VOD-PRESENTATION-M0-REVIEW-BRIEF.md](streaming/VOD-PRESENTATION-M0-REVIEW-BRIEF.md) | Three normative sentences re-decided before M1. | done |
| [VOD-M0-ISSUES.md](streaming/VOD-M0-ISSUES.md) | Every issue M0 found. | done |
| [VOD-M2-QUESTIONS.md](streaming/VOD-M2-QUESTIONS.md) | The M2 questions and how each was ruled. | done |
| [VOD-M3-HANDOFF.md](streaming/VOD-M3-HANDOFF.md) | Serving from the plan — the first code on the live path. | built |
| [VOD-M4-HANDOFF.md](streaming/VOD-M4-HANDOFF.md) | Web adoption and release acceptance. | built |
| [VOD-CUTOVER.md](streaming/VOD-CUTOVER.md) | Historical VOD-only cutover; superseded by the retained first-play fallback. | done |
| [VOD-ENCODING.md](streaming/VOD-ENCODING.md) | How immutable transcode and subtitle-burn VOD produce and verify the bytes named by a playlist. | built |
| [VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md](streaming/VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md) | Restoring the seek-storm case that CI disabled 2026-08-26. | open |
| [VOD-STALL-ACCEPTANCE-HANDOFF.md](streaming/VOD-STALL-ACCEPTANCE-HANDOFF.md) | Restoring the bandwidth-recovery case; blocked on a device measurement. | open |
| [VOD-STEADY-ACCEPTANCE-HANDOFF.md](streaming/VOD-STEADY-ACCEPTANCE-HANDOFF.md) | Restoring the steady-play case that CI disabled 2026-08-26. | open |
| [STREAMING-RELIABILITY-REVIEW.md](streaming/STREAMING-RELIABILITY-REVIEW.md) | Keeping the stream alive while its future changes. | done |
| [STREAMING-RELIABILITY-STATUS.md](streaming/STREAMING-RELIABILITY-STATUS.md) | Review, repair and promotion status of that effort. | open |
| [WEB-HLS-STARTUP-RECOVERY-STATUS.md](streaming/WEB-HLS-STARTUP-RECOVERY-STATUS.md) | Implementation, review, qualification, and promotion status for delayed-manifest web startup recovery. | open |
| [WEB-HLS-STARTUP-RECOVERY-IMPLEMENTATION.md](streaming/WEB-HLS-STARTUP-RECOVERY-IMPLEMENTATION.md) | Build contract for unloaded-manifest recovery, bounded startup retries, and cause-correct decoder reporting. | open |
| [WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md](streaming/WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md) | Free Fall playback repair: loader ownership, stable init identity and scoped drift verdicts, binary refusals, and causal Auto quality. | open |
| [WEB-HELD-SEEK-STALL-RCA.md](streaming/WEB-HELD-SEEK-STALL-RCA.md) | Why one held Right Arrow became two client commits but one source change, how a refilled presentation wait spent recovery too early, and the live implementation status. | open |
| [NATIVE-HLS-STARTUP-RCA.md](streaming/NATIVE-HLS-STARTUP-RCA.md) | Why native Safari turned a temporarily unavailable playlist into a codec failure and source-rescan refusal. | open |
| [NATIVE-HLS-STARTUP-IMPLEMENTATION.md](streaming/NATIVE-HLS-STARTUP-IMPLEMENTATION.md) | Build contract for native-HLS readiness, bounded reload, joinable source preparation, and three-client parity. | open |
| [Native startup replay](evidence/native-startup-replay.cjs) | Replays the native-HLS startup controller against delayed publication without private media or a running server. | open |
| [STREAMING-RELIABILITY-HANDOFF.md](streaming/STREAMING-RELIABILITY-HANDOFF.md) | The remaining work, for the next streaming agent. | open |
| [STREAMING-CONTINUATION-HANDOFF.md](streaming/STREAMING-CONTINUATION-HANDOFF.md) | The continuation-session workflow, branch state, and integration queue for streaming reliability. | open |
| [PLAYBACK-CAPS-V2-PLAN.md](streaming/PLAYBACK-CAPS-V2-PLAN.md) | Highest deliverable grade, negotiated rather than guessed. | open |
| [PLAYBACK-CAPS-V2-M0.md](streaming/PLAYBACK-CAPS-V2-M0.md) | The M0 measurements taken on lab4, 2026-08-30. | done |
| [MEDIA-BADGES-PLAN.md](streaming/MEDIA-BADGES-PLAN.md) | Making the play menu tell the truth about HDR and Dolby Vision. | built |
| [M5A-CLIENT-BADGE-HANDOFF.md](streaming/M5A-CLIENT-BADGE-HANDOFF.md) | The `DV P7 → DV P8` badge in the Apple and Android clients. | built |
| [M5A-VERIFICATION-ON-LAB4.md](streaming/M5A-VERIFICATION-ON-LAB4.md) | The container-truth check that has to run on real media. | open |
| [M5B_STATUS.md](streaming/M5B_STATUS.md) | Permanent Dolby Vision Profile 7 conversion — what merged. | built |
| [M5-VERIFICATION-PROMPT.md](streaming/M5-VERIFICATION-PROMPT.md) | Fleet verification: the first converted stream a browser ever plays. | open |
| [ANDROID-DV-CONVERSION-RCA-AND-FIX.md](streaming/ANDROID-DV-CONVERSION-RCA-AND-FIX.md) | Why a capable Lenovo received the HDR base instead of converted Profile 8.1, and the bounded repair. | open |
| [ANDROID-DV-CONVERSION-IMPLEMENTATION.md](streaming/ANDROID-DV-CONVERSION-IMPLEMENTATION.md) | Live build and acceptance record for request-scoped Profile 7 conversion, native-DV progressive packaging, and Android HLS retention. | open |
| [DV-DELIVERY-FINDINGS.md](streaming/DV-DELIVERY-FINDINGS.md) | Why Dolby Vision titles arrive as HDR10 or lower. | open |
| [DV-DISK-CONVERSION-DIAGNOSIS.md](streaming/DV-DISK-CONVERSION-DIAGNOSIS.md) | Why the on-disk conversion never completes, and a proposal. | open |
| [QUEUE-REPAIR-VERIFICATION-PROMPT.md](streaming/QUEUE-REPAIR-VERIFICATION-PROMPT.md) | Did the fragment-index queue come back, and stay back? | open |
| [DECODER_SELECTION_AND_RECOVERY_PLAN.md](streaming/DECODER_SELECTION_AND_RECOVERY_PLAN.md) | An explicit decoder choice and recovery path for each producer. | open |
| [DECODER_SELECTION_RECOVERY_STATUS.md](DECODER_SELECTION_RECOVERY_STATUS.md) | The live execution ledger for that plan: what is merged, what was tested, and what remains unsafe. | open |
| [DECODER-EFFORT-HANDOFF.md](streaming/DECODER-EFFORT-HANDOFF.md) | Picking the decoder effort up: what blocks it, what is left, how to work here, and the traps it already fell into. | open |
| [DECODER-M8-HANDOFF.md](streaming/DECODER-M8-HANDOFF.md) | Running the remaining hardware, workload, latency, concurrency, and three-client evidence after the decoder implementation merged. | open |
| [AVI_VIDEOTOOLBOX_REVIEW_DECISION.md](streaming/AVI_VIDEOTOOLBOX_REVIEW_DECISION.md) | The VideoToolbox decode fix to build, and the follow-up it requires. | open |
| [VideoToolbox live-TV repair status](streaming/LIVE-TV-VIDEOTOOLBOX-STATUS.html) | Current branch, implementation, review, validation and merge progress for the ATSC 1.0 caption repair. | open |
| [ATSC 1.0 VideoToolbox root cause and repair](streaming/LIVE-TV-VIDEOTOOLBOX-ATSC1-ROOT-CAUSE-AND-FIX.md) | Reproduction, current-main implementation, review dispositions and final local validation for caption-triggered encoding failures. | built |
| [VideoToolbox file-caption follow-up](streaming/VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md) | The reproduced DVR/VOD failure left outside the live workaround, with caption-policy and cache-identity acceptance work. | open |
| [Fable's ATSC 1.0 review](reviews/LIVE-TV-VIDEOTOOLBOX-ATSC1-FABLE-REVIEW.md) | Supplied independent reproduction and blockers against the original stale-base candidate. | done |
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
| [STATUS-HISTORY.md](cluster/STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [CLOCK-SKEW-GUARD-DESIGN.md](cluster/CLOCK-SKEW-GUARD-DESIGN.md) | Design decision from the 2026-09-20 architecture review: how the existing signed peer timestamp becomes a conservative offset interval without widening the 30 s/5 s auth windows, which actions refuse, and what fleet evidence must precede implementation. | open |
| [STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md](cluster/STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md) | Implementation plan from the 2026-09-20 architecture review: why ten store modules never run the three-voter lane, and what a `?N` validator plus a discarded-result audit would catch. | built |
| [HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md](cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md) | Implementation plan from the 2026-09-20 architecture review: who owns the hiqlite fork, and which dependency edges pull two crypto backends and an ML stack into every build. | open |
| [CLUSTER-BACKUP-AND-RESTORE.md](cluster/CLUSTER-BACKUP-AND-RESTORE.md) | Implementation plan from the 2026-09-20 architecture review: a portable, verifiable backup of an activated cluster and a restore procedure that resets identity and fences the old cluster, with RPO/RTO measured and drills for one-node and majority loss. | open |
| [RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md](cluster/RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md) | Implementation plan from the 2026-09-20 architecture review: measuring snapshot build time first, sizing the log threshold and storage floor from it, and the single-logical-cut requirement for any off-writer snapshot. | open |
| [REPLICATED-WRITE-RATE-HYGIENE.md](cluster/REPLICATED-WRITE-RATE-HYGIENE.md) | Implementation plan from the 2026-09-20 architecture review: stopping the watched-outbox's unconditional 1 Hz raft write on every voter and the takeover loop's idle consistent reads while keeping the atomic replicated claim. | open |
| [BOUNDED-REPLICA-READS-ROLLOUT.md](cluster/BOUNDED-REPLICA-READS-ROLLOUT.md) | Implementation plan from the 2026-09-20 architecture review: flipping `bounded_replica_reads` as a consistency-policy change with per-route coverage, fewer consistent reads per request, and no ordinary-auth cache. | open |
| [SQLITE-READ-PATH-AND-QUERY-PLANS.md](cluster/SQLITE-READ-PATH-AND-QUERY-PLANS.md) | Implementation plan from the 2026-09-20 architecture review: moving genuinely read-only store methods off the writer mutex, splitting `user_for_token`, `EXPLAIN QUERY PLAN`-justified indexes, keyset pagination, and the corrected search predicate. | open |
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
| [TRANSPORT-RECOVERY-CI-IMPLEMENTATION-HANDOFF.md](cluster/TRANSPORT-RECOVERY-CI-IMPLEMENTATION-HANDOFF.md) | Build order for independent voter and learner execution, durable diagnostics, phase timings, and separately reported CI roles. | open |
| [TRANSPORT-RECOVERY-RESOURCE-BASELINE.md](cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md) | Why that lane never passed under the per-cycle ceiling: the measurement. | done |
| [TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md) | The campaign-floor contract that replaced it, and the two options not taken. | built |
| [CLUSTER_PAGE_LATENCY_REVIEW.md](cluster/CLUSTER_PAGE_LATENCY_REVIEW.md) | Evidence for why Home, Activity and Settings were slow in a cluster. | done |
| [CLUSTER_PAGE_LATENCY_FIX_PLAN.md](cluster/CLUSTER_PAGE_LATENCY_FIX_PLAN.md) · [review](cluster/CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md) | Restore quorum truth, then unblock first paint. | built |
| [WAL_GENERATION_REPAIR_PLAN.md](cluster/WAL_GENERATION_REPAIR_PLAN.md) · [review](cluster/WAL_GENERATION_REPAIR_PLAN_REVIEW.md) | Making snapshot compaction invalidate every stale reader cache. | built |
| [MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md](cluster/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md) | Making admission an authorization decision. | open |
| [LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md](cluster/LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md) | Why a live learner waited fifteen idle minutes. | open |
| [ACTIVITY_PEER_READ_FIX_PLAN.md](cluster/ACTIVITY_PEER_READ_FIX_PLAN.md) | Coalescing peer-read bursts and reporting failures truthfully. | built |
| [DVR-ACTIVITY-ZERO-GENERATION-STATUS.md](cluster/DVR-ACTIVITY-ZERO-GENERATION-STATUS.md) | Why remote Activity rejected a healthy recorder's initial serving epoch, and the repair's review and promotion state. | open |

---

## clients/ — Apple, Android, and the web player

The native clients and the web UI: parity trackers, the input contract every
player obeys, subtitles and overlays, layouts and themes.

| File | Answers | |
|---|---|---|
| [STATUS-HISTORY.md](clients/STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md](clients/NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md) | The build plan the adaptive-quality design produced: settle the four policy disagreements in the web first, type the reopen cause on the wire only in the change that first sends one, add the Swift and JVM runners of the shared fixture, then one adapter at a time — each one disabled until its own shaped-network trace beats its own baseline. | open |
| [NATIVE-ADAPTIVE-QUALITY-DESIGN.md](clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md) | Implementation plan from the 2026-09-20 architecture review: what adaptive quality on the Apple and Android clients would have to be — one shared policy, five named kinds of evidence, and the shaped-network trace that would let it be switched on. | open |
| [WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md](clients/WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md) | Implementation plan from the 2026-09-20 architecture review: hls.js worker on, `recoverMediaError` inside the existing attempt fences, a global error reporter with redaction, and local seeking for rolling HLS within the retained published range. | open |
| [WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md](clients/WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md) | Why an unlanded Safari VOD seek hit the 8 s stall rule before its fragment retry, and the reviewed 20 s local-seek fallback proposal. | open |
| [WEB-VOD-SEEK-IMPLEMENTATION-STATUS.md](clients/WEB-VOD-SEEK-IMPLEMENTATION-STATUS.md) | Current progress, evidence owed, and merge state for the web VOD seek deadline repair. | open |
| [WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md](clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md) | Implementation plan from the 2026-09-20 architecture review: `tsc --checkJs` ratcheted from a baseline over the served script order, then splitting `play()` and `attachHls()` along their seams, plus the small W9 fixes each with its guardrail. | open |
| [APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md](clients/APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md) | Implementation plan from the 2026-09-20 architecture review: asset-derived display criteria through the active window on tvOS, interruption and route-change ownership in all three players, and the device tests that prove either. | open |
| [APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md](clients/APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md) | Implementation plan from the 2026-09-20 architecture review: an immutable per-continuation `Attempt` snapshot that keeps the nine epochs' scopes, one typed item-event observer for all three stacks, poll-to-KVO conversions that keep their deadlines, and staged strict concurrency. | open |
| [NATIVE-LIBRARY-PAGING.md](clients/NATIVE-LIBRARY-PAGING.md) | Implementation plan from the 2026-09-20 architecture review: paging large libraries on demand on Apple and Android while preserving merged multi-library order and defined search behaviour for unloaded titles. | open |
| [ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md](clients/ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md) | Implementation plan from the 2026-09-20 architecture review: `Display.Mode` selection on TV (Media3 has no ALWAYS strategy), and sizing the playback byte budget from measured heap classes rather than a fixed floor. | open |
| [ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md](clients/ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md) | Implementation plan from the 2026-09-20 architecture review: background pause and audio foreground service as one contract, one player builder with role overrides, live-window recovery, audio-sink error classification, and status-gated node failover. | open |
| [ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md](clients/ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md) | Implementation plan from the 2026-09-20 architecture review: a scoped short-lived grant instead of the account bearer for external players, the DataStore excluded from both backup rule files, dispatcher separation after a trace, and shipping the release variant. | open |
| [WATCH-AND-BROWSE-IMPLEMENTATION.md](clients/WATCH-AND-BROWSE-IMPLEMENTATION.md) | Approved movie/episode watch browser, retained web host, task ownership, tests and pending native/real-media gates. | open |
| [WATCH-VIEW-LAYOUT.md](clients/WATCH-VIEW-LAYOUT.md) | What the web watch view shows beside and below the picture: the collapsible chapter rail with on-request thumbnails, the media ledger whose chips pick tracks, one Close, and the Developer switch behind the thumbnails. | built |
| [APPLE-CLIENT-PARITY.md](clients/APPLE-CLIENT-PARITY.md) | What the Apple client has, what it lacks, and which build proved it. | live |
| [ANDROID-CLIENT-PARITY.md](clients/ANDROID-CLIENT-PARITY.md) | The same, for Android. | live |
| [PLAYBACK-INFO-REDESIGN.md](clients/PLAYBACK-INFO-REDESIGN.md) | Playback info hierarchy, measurement semantics and qualification. | open |
| [PLAYBACK-INFO-MISSING-FIELDS-RCA.md](clients/PLAYBACK-INFO-MISSING-FIELDS-RCA.md) | Why clients omit output facts or display source dimensions for a converted stream, with reviewed provenance rules. | open |
| [PLAYBACK-INFO-IMPLEMENTATION-HANDOFF.md](clients/PLAYBACK-INFO-IMPLEMENTATION-HANDOFF.md) | Build sequence for output metadata, safe client collectors, attachment fencing, package ownership, and acceptance. | open |
| [330-calm-library-pages.md](apple-builds/330-calm-library-pages.md) | Apple build 162 quiet Home and open media details. | built |
| [359-apple-pause-resume.md](apple-builds/359-apple-pause-resume.md) | Apple return-to-picture owner, deadline, and advisory enablement. | built |
| [playback-info-redesign.md](apple-builds/playback-info-redesign.md) | Apple build 161 playback-info changes. | open |
| [328-native-stall-parity.md](apple-builds/328-native-stall-parity.md) | Apple release note for recipe-preserving native stall recovery. | open |
| [PLAYER-INPUT-CONTRACT.md](clients/PLAYER-INPUT-CONTRACT.md) | One routing table every client obeys — which key does what, in which state. | live |
| [PLAYER-INPUT-CONTRACT-PLAN.md](clients/PLAYER-INPUT-CONTRACT-PLAN.md) | How that contract was implemented. | built |
| [PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md](clients/PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md) | What the physical devices did on 2026-09-02. | done |
| [PLAYBACK-SURFACE-CONTRACT.md](clients/PLAYBACK-SURFACE-CONTRACT.md) | Why an error overlay sits over a playing picture on every client, and the one fault contract that ends it (v2, ruled). | live |
| [PLAYBACK-SURFACE-CONTRACT-REVIEW.md](clients/PLAYBACK-SURFACE-CONTRACT-REVIEW.md) | Adversarial review of PR #274: factual audit, recovery and ownership counterexamples, and acceptance gaps. | done |
| [PLAYBACK-SURFACE-CONTRACT-REVIEW-RESPONSE.md](clients/PLAYBACK-SURFACE-CONTRACT-REVIEW-RESPONSE.md) | Finding-by-finding disposition of that review — what v2 of the contract changed and why. | done |
| [PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md) | The six-PR build plan for the surface contract: exact interfaces, owner sites, fence, acceptance commands. | open |
| [APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md](clients/APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md) · [design review](clients/APPLE-PAUSE-RESUME-HANDOFF-REVIEW.md) · [status](clients/APPLE-PAUSE-RESUME-STATUS.md) | Bounded return to a moving picture after Apple pause: incident evidence, reviewed contract, and live implementation/promotion ledger. | open |
| [PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md](clients/PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md) | Hand-off for a Mac: compile and test M2 and M5's Swift, which has never seen a compiler. | open |
| [PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md](clients/PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md) | The same for M3 and M5's Kotlin: Gradle, the mutations, the device runs. | open |
| [PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md](clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md) | M4: the four §7 recipes on an Apple TV, an iPhone and an Android TV, and what counts as a pass. | open |
| [PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md](clients/PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md) | M6's gate: the one measurement that decides whether the Android remux-seek landing gets built at all. | open |
| [WEB-UI-USABILITY-AUDIT.md](clients/WEB-UI-USABILITY-AUDIT.md) | Approved twelve-part web usability audit and acceptance criteria. | open |
| [WEB-UI-IMPLEMENTATION-STATUS.md](clients/WEB-UI-IMPLEMENTATION-STATUS.md) · [status page](clients/WEB-UI-STATUS.html) | Progress, decisions, review and final fast-lane evidence for the web UI implementation. | open |
| [UI-NAVIGATION-AUDIT.md](clients/UI-NAVIGATION-AUDIT.md) | Why the player felt different on every client, anchored at `file:line`. | done |
| [CLIENTS-CODE-REVIEW.md](clients/CLIENTS-CODE-REVIEW.md) · [assessment](clients/CLIENTS-CODE-REVIEW-ASSESSMENT.md) | Capable players that under-ask the server — findings, and what to trust. | done |
| [CLIENTS-REMEDIATION-PLAN.md](clients/CLIENTS-REMEDIATION-PLAN.md) | Restoring trust, then raising the quality ceiling. | built |
| [APPLE-NATIVE-SUBTITLES-PLAN.md](clients/APPLE-NATIVE-SUBTITLES-PLAN.md) · [handoff](clients/APPLE-NATIVE-SUBTITLES-HANDOFF.md) | Native text subtitles on Apple: the road, and what shipped. | built |
| [PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) | Why a 79.5 GB read blocked a start when a title needed PGS burn-in, what the 2026-09-21 incident actually was, and the three fixes — all merged; the overlay gate waits on a two-device check. | open |
| [PGS-SUBTITLE-START-PATH-RCA-REVIEW.md](clients/PGS-SUBTITLE-START-PATH-RCA-REVIEW.md) | Adversarial review of the RCA at `5c605768`: §5.3's web regression refuted, the index pass proven to be one full demux (so Fix C rides on it for free), answers to all seven questions, six rulings. | done |
| [SUBTITLE-CLUSTER-EXTRACTION-PLAN.md](clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) | v2 implementation contract and M0–M5 execution log for clustered text and PGS extraction, queueing, hydration and backfill; fleet evidence pending. | built |
| [SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md](clients/SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md) | Seven findings and dispositions that shaped the v2 contract. | done |
| [SUBTITLE-CLUSTER-EXTRACTION-STATUS.md](clients/SUBTITLE-CLUSTER-EXTRACTION-STATUS.md) | Current K-09 milestone, review and merge qualification status; fleet evidence pending. | built |
| [SUBTITLE-RELIABILITY-ASSESSMENT.md](clients/SUBTITLE-RELIABILITY-ASSESSMENT.md) | Why subtitles still fail on every client after the rework landed — three symptoms root-caused at `c9e4edf4`, with confidence labels. | done |
| [SUBTITLE-RELIABILITY-HANDOFF.md](clients/SUBTITLE-RELIABILITY-HANDOFF.md) | The build order for the repairs: six milestones, exact contracts, non-goals, acceptance. | open |
| [SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md](clients/SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) | Eight cases for the session that has the devices — the hardware evidence none of this has yet. | open |
| [PGS_OVERLAY_PLAN.md](clients/PGS_OVERLAY_PLAN.md) | Dolby Vision-safe PGS subtitle overlay. | open |
| [PGS-OVERLAY-M0-FEASIBILITY.md](clients/PGS-OVERLAY-M0-FEASIBILITY.md) | The feasibility evidence for M0. | open |
| [PGS-OVERLAY-REVIEW-ASSESSMENT.md](clients/PGS-OVERLAY-REVIEW-ASSESSMENT.md) | Accepted findings, and the re-review requested. | done |
| [APPLE-PGS-OVERLAY-ACCEPTANCE.md](clients/APPLE-PGS-OVERLAY-ACCEPTANCE.md) | One iPad Pro run, one decidable acceptance record. | open |
| [UI-LAYOUTS-PLAN.md](clients/UI-LAYOUTS-PLAN.md) | Four layouts and five themes, proposed for review. | done |
| [UI-LAYOUTS-REVIEW.md](clients/UI-LAYOUTS-REVIEW.md) | Ship a smaller, honest first slice. | done |
| [UI-LAYOUTS-IMPLEMENTATION.md](clients/UI-LAYOUTS-IMPLEMENTATION.md) | The accepted slice, as built. | built |
| [UI-LAYOUTS-STATUS.md](clients/UI-LAYOUTS-STATUS.md) | Ground truth for what of that slice is proven. | open |
| [UI-LAYOUTS-G3-DECISION.md](clients/UI-LAYOUTS-G3-DECISION.md) | Did the layout abstraction pay for itself? | done |
| [WEB-SHELL-LAYOUT.md](clients/WEB-SHELL-LAYOUT.md) | Where the web app's sixty-five files are, what each one holds, where its code used to be in `index.html`, and the rules a new file has to obey. | live |
| [WEB-SHELL-SPLIT-PLAN.md](clients/WEB-SHELL-SPLIT-PLAN.md) | How the 23,901-line web `index.html` became a multi-file app with no build step, and the byte-identity gate that proved nothing else changed. | built |
| [WEB_LAYOUT_CONTAINMENT_STATUS.md](clients/WEB_LAYOUT_CONTAINMENT_STATUS.md) | Live delivery status of web layout containment. | open |
| [OFFLINE-VIEWING-PLAN.md](clients/OFFLINE-VIEWING-PLAN.md) · [review](clients/OFFLINE-VIEWING-REVIEW.md) | One-tap, app-managed downloads — the plan and its review. | built |
| [EBOOK-READER-PLAN.md](clients/EBOOK-READER-PLAN.md) | plurx reads what Curator acquires. | open |
| [CLIENT-DEPLOY-PROMPT.md](clients/CLIENT-DEPLOY-PROMPT.md) | Getting a merged build onto the phones and the Apple TVs. | live |
| [DVR-HARDWARE-VERIFICATION-PROMPT.md](clients/DVR-HARDWARE-VERIFICATION-PROMPT.md) | Hand-off for the LAN: the guide tier, and the end-to-end recording pass no cloud session can run. | open |

---

## server/ — the HTTP layer, scanning, images, auth and observability

The daemon's request path and its background workers, as distinct from what
is streamed (`streaming/`) and what is replicated (`cluster/`).

| File | Answers | |
|---|---|---|
| [DETAIL-READS-AND-STORAGE-AVAILABILITY.md](server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md) | Implementation plan from the 2026-09-20 architecture review: why an item-detail page stalls on an unmounted share, and how the index badge is decided without loading the index. | open |
| [TELEMETRY-BACKPRESSURE.md](server/TELEMETRY-BACKPRESSURE.md) | Implementation plan from the 2026-09-20 architecture review: what bounds playback telemetry when the node-local writer is slow, and why the metrics survive retention being turned off. | open |
| [PLEX-FACADE-PAGING.md](server/PLEX-FACADE-PAGING.md) | Implementation plan from the 2026-09-20 architecture review: how the Plex façade answers a 5,000-item section without an N+1, and what must be proven before the media-session route cache is touched. | open |
| [OBSERVABILITY-BASELINE.md](server/OBSERVABILITY-BASELINE.md) | Implementation plan from the 2026-09-20 architecture review: what plurx publishes about its own HTTP surface and its panics, and which metric is each release-evidence number. | open |
| [HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md](server/HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md) | Implementation plan from the 2026-09-20 architecture review: header-read and per-group handler deadlines the listener lacks, precompressed web assets with correct negotiation, hashed `hls.min.js` and sidecars, and the security headers — as three PRs. | built |
| [SCAN-AND-ENRICHMENT-HYGIENE.md](server/SCAN-AND-ENRICHMENT-HYGIENE.md) | Implementation plan from the 2026-09-20 architecture review: a bounded blocking walker for the scanner and real deadlines on the TMDB/AniList clients and the join request. | built |
| [IMAGE-SERVING-AND-DERIVATIVES.md](server/IMAGE-SERVING-AND-DERIVATIVES.md) | Implementation plan from the 2026-09-20 architecture review: verified digests instead of per-request hashing, separate local-read and peer-fetch permits, `ETag`s, and size variants added without reverting `original` backdrops. | open |
| [AUTH-HARDENING.md](server/AUTH-HARDENING.md) | Implementation plan from the 2026-09-20 architecture review: bounded logout admission that keeps the two-phase revocation fence, a lockout-resistant login throttle, and token expiry as a product decision with its client consequences. | open |

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
| [STATUS-HISTORY.md](ci/STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md) | Implementation plan from the 2026-09-20 architecture review: a PR-level regression field bound to the merged tree that replaces per-commit receipts without deleting one, the narrowed corrective rule, the text-contract pruning protocol, the STATUS.md split, and the release-tag restart with `sha-` images kept as deploy identity. | open |
| [ARCHITECTURE-DOC-RECONCILIATION.md](ci/ARCHITECTURE-DOC-RECONCILIATION.md) | Implementation plan from the 2026-09-20 architecture review: every wrong number in ARCHITECTURE.md verified against its constant, the one PR that rewrites it with named constants, the DVR reversal as a dated decision, a `doc_versions.py` extension, and the 46 index rows whose doc header disagrees. | built |
| [RUST-TEST-EXECUTION-POLICY.md](ci/RUST-TEST-EXECUTION-POLICY.md) | Implemented plan from the 2026-09-20 architecture review: Paul's option (a) decision, the blocking PR unit/Clippy lane, baseline repairs, focused-core and known-red guards, playback-policy fixes, and bounded timing evidence. | built |
| [SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md) | Implementation plan from the 2026-09-20 architecture review: systemd and Compose limits each with its validation step, child priorities and OOM adjustment, CI on the shipped ffmpeg 8, the corrected release profile, and parser fuzz targets. | open |
| [RIPWIRE-PILOT.md](ci/RIPWIRE-PILOT.md) | Measured query cost, source/fixture omissions, and completed opt-in adoption verdict. | open |
| [RIPWIRE.md](ci/RIPWIRE.md) | Explicit setup, bounded navigation, output meanings, and coverage limits. | live |
| [RIPWIRE-STATUS.md](ci/RIPWIRE-STATUS.md) | Ripwire implementation, measured evidence, decisions, and promotion progress. | open |
| [CI_TEST_OVERHAUL_PLAN.md](ci/CI_TEST_OVERHAUL_PLAN.md) | Fast failures, selective evidence, safe reuse. | open |
| [CI_EXECUTION_ACCELERATION_PLAN.md](ci/CI_EXECUTION_ACCELERATION_PLAN.md) · [review](ci/CI_EXECUTION_ACCELERATION_REVIEW.md) | Persistent caches, native packaging, exact sharding. | open |
| [AGENT-COMPILE-LOOP.md](ci/AGENT-COMPILE-LOOP.md) | A compiler for a checkout that has none. | live |
| [AI-HARNESS-ASSESSMENT.md](ci/AI-HARNESS-ASSESSMENT.md) | Why agent navigation and dependable feedback should precede new orchestration, and where Ripwire fits. | open |
| [AI-HARNESS-FABLE-ASSESSMENT.md](ci/AI-HARNESS-FABLE-ASSESSMENT.md) | Fable's assessment: fix the compile loop, test evidence, and prompt drift before navigation tooling; positions on Gemini's and Codex's proposals. | open |
| [AI-HARNESS-IMPLEMENTATION-PLAN.md](ci/AI-HARNESS-IMPLEMENTATION-PLAN.md) | Ten milestones, in that order, with exact contracts and acceptance checks: agent-check, tests out of the hotspots, prove-fix, fences, guides, swarm/ alignment, status fragments, PR ledger, Ripwire pilot, first extraction. | open |
| [RIPWIRE-IMPLEMENTATION-HANDOFF.md](ci/RIPWIRE-IMPLEMENTATION-HANDOFF.md) | CLI-first build contract for bounded repository navigation, benchmark evidence, validation integration, and rollout limits. | open |
| [FORGEJO-MAIN-IMAGE-HANDOFF.md](ci/FORGEJO-MAIN-IMAGE-HANDOFF.md) | Publishing the main image from Forgejo. | open |
| [RUNNER-DISK.md](ci/RUNNER-DISK.md) | What fills a runner, what bounds it, how to reclaim it. | live |
| [MEDIA1-RUNNER-ORPHANED-PROCESSES.md](ci/MEDIA1-RUNNER-ORPHANED-PROCESSES.md) | Why effort preflight leaked stopped children on media1, and how cleanup is proved. | open |
| [NYNUC-RUNNER-ORPHANED-SLEEP-PROCESSES.md](ci/NYNUC-RUNNER-ORPHANED-SLEEP-PROCESSES.md) | Evidence for stopped orphaned sleeps in the nynuc runner cgroups and the safe cleanup boundary. | open |

---

## features/ — whole capabilities, planned end to end

| File | Answers | |
|---|---|---|
| [SUBTITLE-DOWNLOADS-IMPLEMENTATION.md](features/SUBTITLE-DOWNLOADS-IMPLEMENTATION.md) | Online subtitle search, durable captions, playback integration and optional automatic acquisition. | open |
| [STATUS-HISTORY.md](features/STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION.md](features/DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION.md) | Implementation plan from the 2026-09-20 architecture review: why a series rule missed an airing ten days out and why one slow recording disk stalls the shared tuner; the scheduler's full-guide view and per-sink owned writers behind bounded queues. | open |
| [LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START.md](features/LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START.md) | Implementation plan from the 2026-09-20 architecture review: one graced settings observation per node instead of a consistent read per session per second, one shared `PeerTransport`, warm start without the serial prefix wait, and a retry owner for failed cleanup. | open |
| [LIVE-TV-SHARED-TRANSPORT.md](features/LIVE-TV-SHARED-TRANSPORT.md) | Implementation plan from the 2026-09-20 architecture review: how three viewers on one channel come to cost one tuner — a `(device, channel)` transport fanning to bounded consumers — and why `CLOSED-CAPTIONS` is advertised only per graph proven with a captioned fixture. | open |
| [SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md](features/SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md) | Why reference show S split and changed files can lose watch state; Fable-reviewed directory lookup proposal, movie coverage, hint correction and catalogue repair. | open |
| [SCAN-IDENTITY-IMPLEMENTATION.md](features/SCAN-IDENTITY-IMPLEMENTATION.md) | Sol's three-PR build contract for scan ownership, directory matching, guarded series hints and atomic catalogue repair, with backend tests and release acceptance. | open |
| [SCAN-IDENTITY-STATUS.md](features/SCAN-IDENTITY-STATUS.md) | Live base, branch, milestone, evidence, review and promotion ledger for the scan-identity effort. | open |
| [SCREENSHOT-TOUR.md](features/SCREENSHOT-TOUR.md) | What do native phone, tablet, browser, Live TV, recording, and library layouts look like with public-safe demo data? | live |
| [HDHOMERUN-LIVE-TV-PLAN.md](features/HDHOMERUN-LIVE-TV-PLAN.md) | One tuner, every plurx client. | open |
| [HDHOMERUN-LIVE-TV-STATUS.md](features/HDHOMERUN-LIVE-TV-STATUS.md) | What is built and what is proved on a real FLEX 4K. | open |
| [LIVE-TV-ORIGINAL-QUALITY-IMPLEMENTATION.md](features/LIVE-TV-ORIGINAL-QUALITY-IMPLEMENTATION.md) | Preserve the broadcast when the player can use it, convert only incompatible tracks, and track the effort to main. | built |
| [LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md](features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md) | The eight decisions recording needed — guide horizon, series matching, tuner reserve, capture format, library kind, reminder channels, cell actions, webhook policy — each with what it costs and what it forecloses. | built |
| [LIVE-TV-DVR-STATUS.md](features/LIVE-TV-DVR-STATUS.md) | What recording and reminders do, the four properties the design turns on, what they deliberately do not do, and the hardware pass that is still unproved. | live |
| [LIVE-TV-DVR-IMPLEMENTATION.md](features/LIVE-TV-DVR-IMPLEMENTATION.md) | The executable plan for recording and reminders: transports and sinks, the ten-step owner loop, attempt-file recovery, the recordings library kind, and the reviewer's eleven findings with where each one landed. | built |
| [LIVE-TV-DVR-AND-REMINDERS-REVIEW.md](features/LIVE-TV-DVR-AND-REMINDERS-REVIEW.md) | The eleven recorder, airing, transport, reminder, and recovery findings required before the DVR implementation. | done |
| [LIVE-TV-DVR-VISIBILITY-STATUS.md](features/LIVE-TV-DVR-VISIBILITY-STATUS.md) | DVR foundation progress plus the web fidelity follow-up: programme recording panels, Activity cards/details, Saved cards and executable browser evidence. | open |
| [DVR-VISIBILITY-IMPLEMENTATION.md](features/DVR-VISIBILITY-IMPLEMENTATION.md) | Build contract for programme recording indicators, authoritative Activity telemetry, event history, and Saved navigation. | open |
| [DVR-VISIBILITY-REVIEW.md](features/DVR-VISIBILITY-REVIEW.md) | Adversarial review of DVR visibility, including the blank-list response defect and the design dispositions. | done |
| [LIVE-TV-GUIDE-AND-UI-PLAN.md](features/LIVE-TV-GUIDE-AND-UI-PLAN.md) | The Live TV page rebuilt — list and grid views, the guide feed, fullscreen and picture-in-picture on every client. | open |
| [LOCAL-LIBRARY-SEARCH.md](features/LOCAL-LIBRARY-SEARCH.md) | Default local search, reusable classification, optional embedded semantic search, and validation evidence. | open |
| [LIBRARY-CHANNEL-SUBJECT-MATCHING-IMPLEMENTATION.md](features/LIBRARY-CHANNEL-SUBJECT-MATCHING-IMPLEMENTATION.md) | Subject matching implementation contract and live P1–P4 progress, compiler evidence, review and merge status. | open |
| [LIBRARY-CHANNELS-IMPLEMENTATION.md](features/LIBRARY-CHANNELS-IMPLEMENTATION.md) | First-release contract for subject schedules, deterministic publication, finite-media playback, and authoring on every client. | open |
| [Channel subject Apple notes](apple-builds/313-library-channel-subjects.md) | Built | What changed in Apple channel subject authoring? |
| [LIBRARY-CHANNELS-STATUS.md](features/LIBRARY-CHANNELS-STATUS.md) | Where Library channels is, what is proved, and what remains before promotion. | open |
| [LIBRARY-CHANNELS-PLAYBACK-REPAIR.md](features/LIBRARY-CHANNELS-PLAYBACK-REPAIR.md) | Repair progress, native-muxer diagnostic, and deployed channel acceptance evidence. | open |
| [WEEKLY-REVIEW-REMEDIATION-STATUS.md](features/WEEKLY-REVIEW-REMEDIATION-STATUS.md) | Which September 3–9 security, recovery, and playback findings were fixed, and which remain separately scoped capabilities. | done |
| [LIVE-TV-NATIVE-LAYOUTS-STATUS.md](features/LIVE-TV-NATIVE-LAYOUTS-STATUS.md) | Three native TV presentations, compact mobile browsing, and the exact implementation evidence. | open |
| [LIVE-TV-NATIVE-LAYOUTS-IMPLEMENTATION.md](features/LIVE-TV-NATIVE-LAYOUTS-IMPLEMENTATION.md) | Build contract for three selectable TV presentations and the compact iOS and Android Live TV layout. | open |
| [LIVE-TV-NATIVE-LAYOUTS-PROPORTIONS-REVIEW.md](features/LIVE-TV-NATIVE-LAYOUTS-PROPORTIONS-REVIEW.md) | Why the Apple TV, Google TV and phone Live TV screens have the wrong proportions — five causes with line anchors, the numbers that fix them, and the renders. | open |
| [LIVE-TV-PROPORTIONS-IMPLEMENTATION.md](features/LIVE-TV-PROPORTIONS-IMPLEMENTATION.md) | The two-PR build plan for the approved TV and phone proportions — constants per platform, file ownership, acceptance screenshots. | open |
| [LIVE-TV-GUIDE-AND-START-RELIABILITY.md](features/LIVE-TV-GUIDE-AND-START-RELIABILITY.md) | Why the guide is empty after every deploy and why Live TV says "wait 90 seconds" with the tuner idle — the diagnosis with fleet evidence, and the durable-guide + ask-the-server-instead-of-waiting fix. | open |
| [LIVE-TV-RELIABILITY-IMPLEMENTATION.md](features/LIVE-TV-RELIABILITY-IMPLEMENTATION.md) | The plan Opus builds from Paul's four rulings — exact interfaces for the durable guide, the event-driven loop, `request_id`, retire/resume, stray eviction and the barrier removal on all three clients; milestones with acceptance checks and the reviewer's attack list. | open |
| [LIVE-TV-RELIABILITY-STATUS.md](features/LIVE-TV-RELIABILITY-STATUS.md) | How far the reliability lane has got: milestone by milestone, the deviations from the plan and why, and the evidence recorded for each. | open |
| [LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md](features/LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md) | Why every Live TV start plays a few seconds and then freezes on every client — the measured `-hls_init_time 1` cadence jump, the uniform-1 s-segment fix that keeps the start as fast as the tuner — and the tvOS fullscreen surface redesign: the reveal-layer focus trap, the new band, Info, waiting and paused states, renders, and the two-PR plan. | done |
| [LIVE-TV-START-STALL-AND-TVOS-SURFACE-IMPLEMENTATION.md](features/LIVE-TV-START-STALL-AND-TVOS-SURFACE-IMPLEMENTATION.md) | The plan Sol builds, v2 after Astra's review — three PRs on one lane: uniform 1 s segments, the listed-media publish gate, progress as the newest listed segment, the rebuilt graph probe, one Android line, the `threshold + 1` lag budget, the controller's `waiting`/behind-live publishers, the band, focus rules, the Info ledger, tests, milestones with acceptance, and the reviewer's attack list. | built |
| [SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md](features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md) | What is built, reviewed, and proved for the settings navigation and Developer-page redesign. | open |
| [SETTINGS-NAVIGATION-AND-DEVELOPER-IMPLEMENTATION.md](features/SETTINGS-NAVIGATION-AND-DEVELOPER-IMPLEMENTATION.md) | Approved build contract for settings navigation, advisory Developer requirements, and explicit feature controls. | open |
| [HOMEVIDEO-PLAN.md](features/HOMEVIDEO-PLAN.md) | Home video and photos — the `home` libraries. | built |
| [INTEGRATION-PLAN.md](features/INTEGRATION-PLAN.md) | plurx's side of the Curator pipeline. | built |
| [WINDOWS-PORT-PLAN.md](features/WINDOWS-PORT-PLAN.md) | Decisions and acceptance contract used to build the native `plurxd.exe`. | built |
| [WINDOWS-PORT-STATUS.md](features/WINDOWS-PORT-STATUS.md) | What is built, blocked, and proved for the native Windows server effort. | open |

---

## reviews/ — pull-request review records

| File | Answers | |
|---|---|---|
| [ARCHITECTURE-REVIEW-2026-09-20.md](reviews/ARCHITECTURE-REVIEW-2026-09-20.md) | The end-to-end architecture review of 2026-09-20, revision 3 (adversarial assessment applied, Astra's independent review merged) — ranked findings for performance, stability, video quality and design, ten verified do-first items, and the sequencing. | open |
| [ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md](reviews/ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md) | The nine full area reports behind that review, unrevised, with every finding's quoted evidence and each area's "already good" list; the main document wins where they disagree. | open |
| [ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md) | Checks all 82 consolidated entries and 128 appendix findings against the source, with corrections, per-finding verdicts and evidence limits; revision 2 of the review applies its dispositions. | done |
| [ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md](reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) | The single shared status for every implementation plan from the review, across Claude, GPT and OpenRouter sessions — the claim protocol, the model/session identity rules, and one row per plan. | live |
| [ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md](reviews/ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md) | Current queue, next action and evidence locations for the assigned GPT architecture-review build and fleet pass. | open |
| [ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md](reviews/ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md) | The four-node `f600d2823` deployment receipt, preliminary fleet readings, failed Chrome D3 trace and remaining evidence for each architecture-review workboard row. | open |
| [ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md](reviews/ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md) | Read-only L-01, L-02, C-03, S-04, S-05 and K-06 baselines on the deployed fleet, with the exact acceptance gaps. | open |
| [ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md](reviews/ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md) | Dated read-only K-02, C-05, C-08, P-02 and S-11 fleet measurements, collector windows and exact acceptance limits on current main. | open |
| [Architecture review implementation status](reviews/ARCHITECTURE-REVIEW-2026-09-20-STATUS.html) | Live searchable view of the canonical architecture-review workboard, with a read-only same-origin Forgejo overlay for mapped open plan PRs and exact-head CI state; API failures leave every board row visible. | live |
| [SECURITY-ASSESSMENT-2026-09-13.md](reviews/SECURITY-ASSESSMENT-2026-09-13.md) | Which trust boundaries need work across the server, cluster, clients, media supply chain, and delivery process. | open |
| [WEEKLY-ARCHITECTURE-SECURITY-IMPLEMENTATION.md](reviews/WEEKLY-ARCHITECTURE-SECURITY-IMPLEMENTATION.md) | Bounded implementation handoff for the September architecture, security, recovery, and playback findings. | open |

The remaining files are one record per reviewed PR, each naming the verdict
and findings against a specific head commit. They are kept because other docs
cite them; none describes current behavior.

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
