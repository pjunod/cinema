# playback-control/ — the playback control rewrite

The index of `docs/playback-control/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

Explicit demand, one owner per stream, prepared handoffs. The largest single
thread in the repo: a protocol plan, milestone contracts and implementation
receipts. Start at the current rewrite remainder, then the lifecycle coverage
map; older milestone files are historical contracts, not a fresh missing-work
list.

| File | Answers | |
|---|---|---|
| [STATUS-HISTORY.md](STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [REPLACEMENT-GATE-SUPERSESSION-RCA.md](REPLACEMENT-GATE-SUPERSESSION-RCA.md) | Why an abandoned replacement held its player key forever, the lab6 evidence, and the supersedable gate that fixes it. | built |
| [CALLBACK-ROOT-CAUSE-AND-FIX.md](CALLBACK-ROOT-CAUSE-AND-FIX.md) | Why Safari cold resume lost frame callbacks, the lifecycle fix, controlled evidence, and questions for Fable review. | open |
| [MACOS-HDR-ACCEPTANCE-FOLLOWUPS-2026-10-12.md](MACOS-HDR-ACCEPTANCE-FOLLOWUPS-2026-10-12.md) | Seven retained Mac HDR follow-ups, completed bounded physical/current-owner evidence, reviewed repairs and task gates; final frozen-tree promotion remains pending and broader device/optical limits stay explicit. | open |
| [F6 authority/media controls](evidence/f6-authority-media-20261015.json) | Controlled real-media brief/sustained authority loss and current F3 owner regressions; physical accounting remains separate. | built |
| [Paused VOD seek repair receipt](evidence/paused-vod-seek-20261012.json) | F1 stopped-loader reproduction, failed first harness, actual paused forward/backward target frames, regressions and retained Air rolling-reopen failure; Ordinary Air hls.js HDR10-to-SDR rerun passes; native HLS and the broader matrix remain separate. | open |
| [October 13 Air HDR paused-reopen receipt](evidence/air-hdr-paused-20261013.json) | Actual Metal paused target/reopen proof, exact surface repair qualification/landing, 58½-minute CPU control, preserved failures and verified restoration. | done |
| [F2 decoded-alignment repair receipt](evidence/f2-decoded-alignment-20261013.json) | Original refusals, both qualified landings, installed HDR/SDR proofs, immutable archives and final zero-resume/clipboard restoration; broader audio/matrix remain separate. | done |
| [F7 Hable reference-white repair receipt](evidence/f7-reference-white-20261015.json) | Pinned 203-versus-100-nit diagnosis, neutral and nine held-film controls, bounded shader correction, unchanged HLG/default controls and verified private source/evidence archive; full shipping package and bounded physical Air comparison recorded separately, final promotion pending. | open |
| [F4 compressed Matroska parser receipt](evidence/f4-compressed-matroska-parser-20261015.json) | Why did unchanged compressed subtitle tracks refuse SDR startup, and what does the repaired sealed parser prove? | built |
| [October 15 physical playback receipt](evidence/hdr-physical-playback-20261015.json) | What did the corrected Air Metal/CPU and genuine AV1 cold/warm playback runs show, and which immutable archives preserve the failures and cleanup? | **built** |
| [F3 current-owner lifecycle receipt](evidence/f3-real-lifecycle-20261015.json) | Do owned family charges, scheduled pins, idle readers and browser teardown settle under the current lifecycle policy, and what remains unknown about the original incident? | built |
| [PLAYBACK-LIFECYCLE-COVERAGE.md](PLAYBACK-LIFECYCLE-COVERAGE.md) | Playback states and transitions, buffer handoffs, communication contracts, existing test anchors, and open acceptance gaps. | open |
| [PLAYBACK-REWRITE-REMAINDER.md](PLAYBACK-REWRITE-REMAINDER.md) | Executed Sol handoff: the finite B01–B05 remainder, its boundaries, and the work promoted through PR #263. | done |
| [PLAYBACK-LIFECYCLE-IMPLEMENTATION.md](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md) | Completed S01–S09 implementation contract: code entry points, lifecycle/buffer contracts, compiler commands, deferred regression coverage, advisory Developer settings and batched fast-lane delivery. | built |
| [PLAYBACK-LIFECYCLE-STATUS.md](PLAYBACK-LIFECYCLE-STATUS.md) | Completed execution ledger for the lifecycle implementation: package state, compilation, deferred tests, review, qualification, promotion and cleanup. | done |
| [PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md](PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md) | Completed execution ledger and live measurement contract for playhead-anchored server readiness, HTTP delivery, client loaded ranges, and presentation progress. | built |
| [Retained playback observations](../evidence/playback-lifecycle-observation-2026-09-11.json) | Sanitized September 11 Apple TV observations used by the lifecycle audit; diagnostic evidence, not a runnable test fixture or acceptance result. | done |
| [PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) | The whole design: explicit demand, one owner, prepared handoffs. | open |
| [PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) | What is actually built and merged, milestone by milestone. | open |
| [PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md) | The numbered work items the milestones execute. | open |
| [PLAYBACK-CONTROL-PROTOCOL-REVIEW.md](PLAYBACK-CONTROL-PROTOCOL-REVIEW.md) | Twelve contracts corrected before any code was written. | done |
| [PLAYBACK-CONTROL-PROTOCOL-M1.md](PLAYBACK-CONTROL-PROTOCOL-M1.md) · [review](PLAYBACK-CONTROL-PROTOCOL-M1-REVIEW.md) | M1: the fenced, observable control path. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md](PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md) · [review](PLAYBACK-CONTROL-PROTOCOL-M2-WEB-REVIEW.md) | M2: the browser shadow reporter. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-ANALYSIS.md](PLAYBACK-CONTROL-PROTOCOL-M3-ANALYSIS.md) · [review](PLAYBACK-CONTROL-PROTOCOL-M3-ANALYSIS-REVIEW.md) | M3: operator-facing analysis control and status. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-LEASE-ACTOR.md](PLAYBACK-CONTROL-PROTOCOL-M3-LEASE-ACTOR.md) | M3a: the rolling lease actor that replaced the split control mutex. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md](PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md) | M3b: explicit demand lease and producer pacing. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md](PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md) | M3c1: the attempt-fenced delivery ledger. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-PRODUCER-EVENTS.md](PLAYBACK-CONTROL-PROTOCOL-M3-PRODUCER-EVENTS.md) | M3c2: producer progress and process-exit events. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md](PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md) | M3c3: who owns the terminal event, and the orderly client release. | built |
| [PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md) | M4: one producer deadline, and the watchdog it removes. | built |
| [REMAINING-ROADMAP-HANDOFF.md](REMAINING-ROADMAP-HANDOFF.md) | M5.5 through M9 — the rest of the rewrite, scoped. | open |
| [M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md) | M5: moving recovery authority off the clients. | built |
| [M5-FLEET-ACCEPTANCE.md](M5-FLEET-ACCEPTANCE.md) | How to accept M5 on the real fleet rather than a branch. | open |
| [M5-FLEET-RESULTS-18886477.md](M5-FLEET-RESULTS-18886477.md) | M5 fleet run at `18886477` — protocol partially accepted. | done |
| [M5-FLEET-RESULTS-943D8A9A.md](M5-FLEET-RESULTS-943D8A9A.md) | M5 fleet run at `943d8a9a` — deployed, acceptance not established. | done |
| [M5-FLEET-RESULTS-C571A50D.md](M5-FLEET-RESULTS-C571A50D.md) | M5 fleet run at `c571a50d` — deployed, acceptance blocked. | done |
| [M5.5-PREPARATION-FEASIBILITY-SPIKE.md](M5.5-PREPARATION-FEASIBILITY-SPIKE.md) | Can a client hold two pipelines at once? The spike that decides. | open |
| [M5.5-SPIKE-EXECUTION-HANDOFF.md](M5.5-SPIKE-EXECUTION-HANDOFF.md) | Running that spike on real hardware, without Paul. | open |
| [M5.5-STAGED-GENERATIONS-HANDOFF.md](M5.5-STAGED-GENERATIONS-HANDOFF.md) | A successor stream that exists without being current. | open |
| [M6-CALLER-HANDOFF.md](M6-CALLER-HANDOFF.md) | Historical M6 caller design; its proof/headroom admission rules are superseded by the lifecycle implementation contract. | superseded |
| [M6-IMPLEMENTATION-HANDOFF.md](M6-IMPLEMENTATION-HANDOFF.md) | Historical M6 prepared-recipe contract and measurements; use the lifecycle contract for current policy. | superseded |
| [M6-AXIS-CASE-HANDOFF.md](M6-AXIS-CASE-HANDOFF.md) | The one measurement M6 waits on, and how to take it. | done |
| [M6-AXIS-CASE-RESULTS.md](M6-AXIS-CASE-RESULTS.md) | The 30 Mbit/s axis run, below the throughput floor and superseded the same day by a 40 Mbit/s re-run that admitted the pair. Read the correction at the top. | superseded |
| [M6-CLIENT-REPLACEMENT-CONTRACT.md](M6-CLIENT-REPLACEMENT-CONTRACT.md) | Historical v1 client-wire snapshot; the lifecycle contract supersedes its software proof and throughput gates. | superseded |
| [CLIENT-PREPARED-SWITCH-CONTRACT.md](CLIENT-PREPARED-SWITCH-CONTRACT.md) | The streaming-reliability prepared-switch adapter contract shared by Apple, Android, and web. | built |
| [CLIENT-APPLE-HANDOFF.md](CLIENT-APPLE-HANDOFF.md) | The implementation handoff for Apple prepared-switch message adaptation. | built |
| [CLIENT-ANDROID-HANDOFF.md](CLIENT-ANDROID-HANDOFF.md) | The implementation handoff for Android prepared-switch message adaptation. | built |
| [CLIENT-WEB-HANDOFF.md](CLIENT-WEB-HANDOFF.md) | The implementation handoff for web prepared-switch message adaptation. | built |
| [M6-APPLE-CLIENT-BUILD.md](M6-APPLE-CLIENT-BUILD.md) | Historical Apple M6 build brief; current enablement and retry policy lives in the lifecycle status. | superseded |
| [M6-WEB-CLIENT-BUILD.md](M6-WEB-CLIENT-BUILD.md) | Historical web M6 build brief; current enablement and first-frame fallback lives in the lifecycle status. | superseded |
| [M6-ANDROID-CLIENT-BUILD.md](M6-ANDROID-CLIENT-BUILD.md) | Historical Android M6 build brief; current enablement and retry policy lives in the lifecycle status. | superseded |
| [M6-ANDROID-CLIENT-STATUS.md](M6-ANDROID-CLIENT-STATUS.md) | Historical Android implementation record; its qualification prose is not current admission policy. | superseded |
| [M6-APPLE-HARDWARE-ACCEPTANCE.md](M6-APPLE-HARDWARE-ACCEPTANCE.md) | Historical hardware procedure; the lifecycle status contains the current finite sweep card. | superseded |
| [M6-WEB-CLIENT.md](M6-WEB-CLIENT.md) | Historical web implementation record; its throughput qualification prose is not current admission policy. | superseded |
| [M6-SERVER-PRIME-HANDOFF.md](M6-SERVER-PRIME-HANDOFF.md) | Phase 3 — the reserve/prime constraints and the implementation that now attaches the staged VOD worker. | built |
| [CONTINUOUS-QUALITY-STATUS.html](CONTINUOUS-QUALITY-STATUS.html) | Current continuous-quality build progress, retained evidence and ownership dependencies. | open |
| [CONTINUOUS-FOREGROUND-ADMISSION.md](CONTINUOUS-FOREGROUND-ADMISSION.md) | Why background indexing refused continuous starts, and the bounded foreground admission repair. | open |
| [CONTINUOUS-QUALITY-BUILD.md](CONTINUOUS-QUALITY-BUILD.md) | Merged continuous-quality implementation and source-bound acceptance: safe failed changes, shared audio, bounded JIT production, browser campaigns, Safari LAN verifier repair and remaining physical-device evidence. | open |
| [APPLE-AUTO-LOW-LINK-IMPLEMENTATION.md](APPLE-AUTO-LOW-LINK-IMPLEMENTATION.md) | Apple 260 metadata timeouts before and after the 3 Mb/s cliff, installed-method label correction, the sign-in/server-picker product bug, and the session-safety prerequisite for any new physical proxy run. | open |
| [CONTINUOUS-QUALITY-REVIEW.md](CONTINUOUS-QUALITY-REVIEW.md) | Adversarial findings and dispositions for the continuous-quality build contract. | open |
| [Native prepared playback correction](../apple-builds/994-cadence-unavailable.md) | Full-width launch IDs, refused-transfer cleanup and resolving staged tracks before exposure; combined native qualification remains open. | open |
| [CONTINUOUS-QUALITY-MATRIX.md](CONTINUOUS-QUALITY-MATRIX.md) | Which source/lifecycle/pressure rows have current, older or missing evidence, and how to read the focused lifecycle helper. | open |
| [QUALITY-SWITCH-CONTINUITY-PLAN.md](QUALITY-SWITCH-CONTINUITY-PLAN.md) | Why every rung change is still a reopen although the prepared handoff is built and deployed on every side, and the plan (M0–M3) to make a quality change a handoff with the incumbent playing throughout. | open |
| [QUALITY-SWITCH-CONTINUITY-BUILD.md](QUALITY-SWITCH-CONTINUITY-BUILD.md) | The build plan for the executing agent: M0 server (`delivery.preparation`, supersession cancels the successor), M2 Apple/Android offer wait + rendezvous, M1 web owner + alignment, M3 measurement; Astra review disposition in §11. | open |
| [QUALITY-SWITCH-CONTINUITY-RESULTS.md](QUALITY-SWITCH-CONTINUITY-RESULTS.md) | The acceptance record for the prepared quality handoff: what each client measures around the commit, the twenty-change bar, the two measurements deliberately not taken and what they would cost, and empty result rows until a device run fills them. | open |
| [M7-REMAINDER-HANDOFF.md](M7-REMAINDER-HANDOFF.md) | M7: subtitle readiness, bounded materialization, seek coalescing, burn-join, prewarm. | open |
| [M7-R-M3-CLAUDE-HANDOFF.md](M7-R-M3-CLAUDE-HANDOFF.md) | Finish M2, then build seek coalescing. | open |
| [M7-M1-LARGE-MKV-OBSERVATION.md](M7-M1-LARGE-MKV-OBSERVATION.md) | Large-MKV readiness and bounded subtitle publication, observed. | done |
| [M7-M1-M3-BLOCKER-REVIEW.md](M7-M1-M3-BLOCKER-REVIEW.md) | Why the M7 remainder is not accepted yet. | open |
| [M7-M4-BURN-JOIN-DESIGN-REVIEW.md](M7-M4-BURN-JOIN-DESIGN-REVIEW.md) | The burn-join design: approve, with one contract correction. | done |
| [MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md) | Marker-destination prewarm — the consumer its metric waits for. | open |
| [CONTENT-ANALYSIS-INDEX-HANDOFF.md](CONTENT-ANALYSIS-INDEX-HANDOFF.md) | Exact skip markers, an explicit queue, and its instrumentation. | open |
