# streaming/ — delivery: VOD, Dolby Vision, capabilities, reliability

Delivery, Dolby Vision, capabilities and playback reliability. The
[docs index](../README.md) defines status words and documentation rules.
Add each document's row here in the same commit.

Start owners: [plan §6](PLAYBACK-START-PLAN.md#6-lane-status).

| File | Answers | |
|---|---|---|
| [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md) | Owner, merges, validation and acceptance. | open |
| [PLAYBACK-START-BUILD.md](PLAYBACK-START-BUILD.md) | Phase 1 build contract (nine lanes). | open |
| [PLAYBACK-START-A1-B1-BUILD.md](PLAYBACK-START-A1-B1-BUILD.md) | A1/B1 contract: one claim, sparse copy, v3. | open |
| [PLAYBACK-START-PLAN-ASTRA-REVIEW.md](PLAYBACK-START-PLAN-ASTRA-REVIEW.md) | Astra's plan review; dispositions in plan §7. | done |
| [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md) | Effort plan: rulings, lanes, reviews, status. | open |
| [PLAYBACK-START-HISTORY.md](PLAYBACK-START-HISTORY.md) | Historical review dispositions. | live |
| [SHARED-LIBRARY-PLAYBACK-RCA.md](SHARED-LIBRARY-PLAYBACK-RCA.md) | Shared-start timeouts, A1 fix, five fleet errors. | open |
| [SHARED-LIBRARY-PLAYBACK-STATUS.md](SHARED-LIBRARY-PLAYBACK-STATUS.md) | PR #1472 ownership, implementation, retained evidence and merge prerequisites. | built |
| [SHARED-SUBTITLE-CUSTODY-AND-START-503.md](SHARED-SUBTITLE-CUSTODY-AND-START-503.md) | S5 Source-owned caption windows, custody initialization race and follow-up qualification. | done |
| [PLAYBACK-PUBLICATION-OVERLAY-STATUS.md](PLAYBACK-PUBLICATION-OVERLAY-STATUS.md) | Cold publication and Resume fault repair: review and merge ledger. | open |
| [HDR10E-PREPARATION-AND-WEB-RECOVERY.md](HDR10E-PREPARATION-AND-WEB-RECOVERY.md) | Preparation, native observation and rewatch resume repairs; review and acceptance. | open |
| [DV-TITLE-SCAN.md](DV-TITLE-SCAN.md) | How DV titles are scanned. | open |
| [PLAYBACK-CACHE-BUDGET.md](PLAYBACK-CACHE-BUDGET.md) | Shared playback cache: reservations, eviction and protected readers. | built |
| [WEB-PREPARED-SEEK-INCIDENT.md](WEB-PREPARED-SEEK-INCIDENT.md) | October 20 ROG seek failure: subtitle cancellation, Auto repair and qualification. | open |
| [BROWSER-HDR-RESTART-LIFECYCLE-RCA.md](BROWSER-HDR-RESTART-LIFECYCLE-RCA.md) | Close/Resume HDR downgrade, lost DV cancellation, Safari startup intent corruption, lifecycle ownership and remaining stale-session evidence. | open |
| [TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md](TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md) | Why TCL Auto rejects Android capability counts, the catalog read-budget failure, and the reviewed plan for one catalog per create with legacy-client compatibility. | open |
| [TCL-CATALOG-REPAIR-STATUS.md](TCL-CATALOG-REPAIR-STATUS.md) | Implementation decisions, commit milestones, compile and review evidence, and remaining physical TCL acceptance. | open |
| [COLD-PLAYBACK-AC3-RETEST-RCA.md](COLD-PLAYBACK-AC3-RETEST-RCA.md) | AC3 retest, Resume and startup repair; Fable client-binding and shared-classifier dispositions. | open |
| [COLD-PLAYBACK-SOURCE-SHARING.md](COLD-PLAYBACK-SOURCE-SHARING.md) | Source-sharing evidence and bounded M1 handoff. | open |
| [COLD-PLAYBACK-STARTUP-ASTRA-REVIEW.md](COLD-PLAYBACK-STARTUP-ASTRA-REVIEW.md) | Historical Astra startup RCA review and dispositions. | done |
| [COLD-PLAYBACK-STARTUP-FABLE-REVIEW.md](COLD-PLAYBACK-STARTUP-FABLE-REVIEW.md) | Historical Fable review of the consolidated startup proposal. | done |
| [COLD-PLAYBACK-STARTUP-INCIDENT-20261102.json](COLD-PLAYBACK-STARTUP-INCIDENT-20261102.json) | Credential-free incident timings and observations. | done |
| [COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md) | Cold startup root cause, proposal and 2 s ready / 5 s supported cold acceptance. | open |
| [NATIVE-CALLBACK-WEBKIT-REPORT-DRAFT.md](NATIVE-CALLBACK-WEBKIT-REPORT-DRAFT.md) | Sanitized WebKit report and minimal native-HLS reproduction; prepared, not filed. | open |
| [NATIVE-CALLBACK-DELIVERY-RCA.md](NATIVE-CALLBACK-DELIVERY-RCA.md) | Native callback root cause and revised per-element multiplexer repair contract. | open |
| [NATIVE-CALLBACK-FABLE-REVIEW.md](NATIVE-CALLBACK-FABLE-REVIEW.md) | Fable P2/P3 dispositions; implementation drafted, final review pending. | open |
| [PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md) | Eight-lane owner, implementation, PR and final-candidate qualification ledger. | open |
| [PLAYBACK-STARTUP-ASTRA-IMPLEMENTATION-REVIEW.md](PLAYBACK-STARTUP-ASTRA-IMPLEMENTATION-REVIEW.md) | Astra's final implementation review of frozen candidate `2bc1e54e1`; seven findings and the request-changes verdict. | done |
| [PLAYBACK-STARTUP-ASTRA-DISPOSITIONS.md](PLAYBACK-STARTUP-ASTRA-DISPOSITIONS.md) | Source dispositions and outstanding review and qualification evidence for Astra's seven startup findings. | open |
| [PLAYBACK-STALL-RECOVERY-IMPLEMENTATION.md](PLAYBACK-STALL-RECOVERY-IMPLEMENTATION.md) | Bounded October 1 repair: periodic quorum-loss diagnosis, startup/retirement/recovery correctness, VOD delivery truth, agent/Opus review dispositions, and one fast-lane PR. | open |
| [PLAYBACK-START-FAILURES-2026-10-13.md](PLAYBACK-START-FAILURES-2026-10-13.md) | Standalone reader contention, unpublished native attachment, stale inventory/error ownership and separate Apple TV audio evidence; fixes built, deployment and physical acceptance open. | open |
| [BAD-BOYS-APPLE-TV-INTERRUPTIONS-RCA.md](BAD-BOYS-APPLE-TV-INTERRUPTIONS-RCA.md) | Five Apple TV remux interruptions from deploy restarts, quorum fences, rolling retirement and whole-file re-attestation (fixed). | open |
| [HEATED-RIVALRY-S1E5-QUORUM-RCA-AND-FIX.md](HEATED-RIVALRY-S1E5-QUORUM-RCA-AND-FIX.md) | S1E5 outage: WAL correlations and SQL reproduction establish the missing lookup index; reviewed proposal covers migrations, cadence, and web recovery. | open |
| [Heated Rivalry quorum build status](HEATED-RIVALRY-S1E5-QUORUM-STATUS.html) | Current implementation phase, review, validation, PR, decisions, and cleanup for the quorum repair. | open |
| [MEDIA-COMPATIBILITY-CATALOG.md](MEDIA-COMPATIBILITY-CATALOG.md) | Which 52 media-condition families have incident, reproduction or synthetic evidence, where their fixtures and regressions live, and which samples and player checks are still missing? | open |
| [STATUS-HISTORY.md](STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [PREPARATION-BUFFER-PRESSURE-RCA-AND-IMPLEMENTATION.md](PREPARATION-BUFFER-PRESSURE-RCA-AND-IMPLEMENTATION.md) | October 8 Apple TV freeze from full-title preparation; merged ownership repair and postmerge checks, deployment/device acceptance open. | open |
| [HONEST-MASTER-PLAYLIST.md](HONEST-MASTER-PLAYLIST.md) | M1/M2 landed; bounded M3/M4, retained VOD M5 and copy/encoded preparation implemented. M3/M4 review, unseen-tail publication and device acceptance remain open. | open |
| [MACOS-VIDEO-PROCESSING-DESIGN.md](MACOS-VIDEO-PROCESSING-DESIGN.md) | Proposed native Mac acceleration: shared-memory cost model, VideoToolbox/Metal processing, Jellyfin dependency preservation, P5 hardware-decode experiment, contracts and measured acceptance. | open |
| [MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md) | Build handoff: package proof, milestone owners, regressions, rollout and independent codec extensions. | open |
| [MACOS-METAL-HDR-ACCEPTANCE-2026-10-12.md](MACOS-METAL-HDR-ACCEPTANCE-2026-10-12.md) | Accepted five-part HDR10 Metal/CPU comparison; broader playback follow-ups remain open. | open |
| [Metal HDR evidence manifest](evidence/macos-metal-hdr-20261012/evidence-manifest.json) · [all text receipts](evidence/macos-metal-hdr-20261012/text-receipts.tar.gz) | Checksum-indexed captures, neutralized logs and API/CI/landing receipts; screenshots retained privately on PR #1038. | done |
| [MACOS-VIDEO-PROCESSING-STATUS.md](MACOS-VIDEO-PROCESSING-STATUS.md) | Completed supported implementation and hardware qualification; final QSV closeout PR, exact evidence, decisions and separate broader acceptance. | done |
| [MACOS-VIDEO-PROCESSING-EVIDENCE.md](MACOS-VIDEO-PROCESSING-EVIDENCE.md) | Processing, subtitle/font, Linux SDK and AMD evidence, dependency identities and retained failures. | open |
| [Active-font release and API evidence](evidence/macos-video-20261008/font-active-e9fd-evidence.tar.gz) | Matched release/symbols and actual native subtitle delivery while inactive font configuration changes; performance remains separate. | open |
| [Linux SDK and compiler evidence](evidence/macos-video-20261008/linux-sdk-and-compile-evidence.tar.gz) | Hash-bound SDK generation, exact-source Linux compilation and isolated runtime dependency evidence; shipping and hardware acceptance remain open. | open |
| [Linux producer and sealed-parser build evidence](evidence/macos-video-20261008/linux-producer-parser-build-evidence.tar.gz) | Full-featured Bookworm producer and matching static AC-4 parser build receipts; package and hardware/API acceptance remain open. | open |
| [Linux package audit evidence](evidence/macos-video-20261008/linux-package-audit-evidence.tar.gz) | Preserved producer/parser identities, exact runtime-base package additions and complete ELF closure; hardware and daemon API qualification remain open. | open |
| [Merged-source compiler evidence](evidence/macos-video-20261008/merged-c10-compile-evidence.tar.gz) | Exact current-main integration checks on Mac and Linux, with the bounded hook continuation retained; no unit or runtime qualification claim. | open |
| [Merged Mac release and API evidence](evidence/macos-video-20261008/final-c10-release-api-evidence.tar.gz) | Exact merged release/symbols and focused subtitle/caption API delivery; interrupted timing results remain unqualified. | open |
| [Complete subtitle performance evidence](evidence/macos-video-20261008/final-c10-subtitle-performance-evidence.tar.gz) | Eight uninterrupted CPU/GPU complete-graph comparisons with frame/timestamp validation, exact timing/child-CPU medians and measurement limits. | open |
| [Intel strict-P5 evidence](evidence/macos-video-20261008/intel-strict-p5-evidence.tar.gz) | Intel raw strict graphs (VA-API and QSV encode), the foreign-Media-SDK and 64×64 QSV-proof findings, fixed-daemon normal-API windows on Intel and AMD, and the build record; no NVIDIA or deployment claim. | open |
| [QSV shipping dependency evidence](evidence/macos-video-20261008/qsv-shipping-dependency-evidence.tar.gz) | Exact installer and both shipping-stage builds preserve audited providers while removing the competing Debian Media SDK; paired Intel hardware acceptance is retained below. | done |
| [Intel QSV shipping qualification](evidence/macos-video-20261008/intel-qsv-shipping-qualification-evidence.tar.gz) | Corrected shipping image passes all 32 raw controls and forced-QSV ordinary-API delivery; exact historical-daemon control, runtime identities and cleanup retained. | done |
| [MACOS-VIDEO-PROCESSING-REVIEW.md](MACOS-VIDEO-PROCESSING-REVIEW.md) | Original proposal review and active continuation implementation review: independent coverage, concrete findings, owner repairs and re-review disposition. | open |
| [CODEC-AND-GPU-QUALIFICATION.md](CODEC-AND-GPU-QUALIFICATION.md) | Codec/GPU corpus, typed output contract, VUI/scoring, rolling-output census and separate graph/device acceptance. | open |
| [S11-INTERNAL-ROLLING-CELLS-2026-10-02.md](S11-INTERNAL-ROLLING-CELLS-2026-10-02.md) | Sanitized source/digest ledger for 16 internal rolling cells, failed attempts and public/film/device qualification limits. | done |
| [PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md](PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md) | DV Profile 5 refusal and dead media-origin probe; `output_job_owned` repair, caller audit and bounded library scan probe. | built |
| [MEDIA-BODY-BUFFERS.md](MEDIA-BODY-BUFFERS.md) | Storage-read sizing and implemented HLS batch coordination with unchanged 4 KiB delivery proof; ownership, regression contracts and remaining throughput evidence. | open |
| [MEDIA-WRITE-STALL-GUARD.md](MEDIA-WRITE-STALL-GUARD.md) | HLS pump timers free handles, not sockets; transport write-stall guard, disabled by default with Developer prerequisites. | open |
| [DV-STRIP-TRIAL-PROBE.md](DV-STRIP-TRIAL-PROBE.md) | Two-second per-file `dovi_rpu=strip` trial, unit-type fallback for unreadable SEI and `probe_json` verdict consumed by argv builders. | open |
| [ENCODED-VOD-HOLD-AND-RELEASE.md](ENCODED-VOD-HOLD-AND-RELEASE.md) | SIGSTOP hold past encoded ahead horizon, low-water release via `Admissions::live_is_waiting`, scheduler tests first. | open |
| [FONT-ATTESTATION-AND-BLOCKING-IO.md](FONT-ATTESTATION-AND-BLOCKING-IO.md) | Move font/engine `stat` loops off runtime and freeze burn-recipe fontconfig identity. | open |
| [FFMPEG-SPAWN-UNIFICATION.md](FFMPEG-SPAWN-UNIFICATION.md) | Unified HLS/VOD/remux spawn path and strict progress-line classifier. | open |
| [ENCODER-RATE-CONTROL-DEFAULTS.md](ENCODER-RATE-CONTROL-DEFAULTS.md) | Per-encoder CRF/QVBR vs ABR evidence before default changes; HDR10 policy and VBV remain separate. | open |
| [TONE-MAP-CENSUS.md](TONE-MAP-CENSUS.md) | Operator census of each GPU/encoder graph against the CPU chain on authored PQ signals; feeds ruling R2. | built |
| [TONE-MAP-CHAIN-CORRECTIONS.md](TONE-MAP-CHAIN-CORRECTIONS.md) | Implementation plan from the 2026-09-20 architecture review: the hardware-download metadata test first, then MaxCLL at scan, explicit `peak=`, gamut before curve, and dither, with an image-comparison protocol. | open |
| [INTERLACE-IN-THE-MEDIA-CONTRACT.md](INTERLACE-IN-THE-MEDIA-CONTRACT.md) | Field order contract, deinterlace/frame policy, Live TV field-rate bitrate fix and fixture acceptance. | open |
| [AUDIO-RESOLVED-INDEPENDENTLY.md](AUDIO-RESOLVED-INDEPENDENTLY.md) | Implementation plan from the 2026-09-20 architecture review: audio negotiated separately from video so a resolution change or burn no longer downmixes compatible multichannel audio, carried through recipe identity, manifests and handoffs. | open |
| [AUDIO-DOWNMIX-SYNTHETIC-QUALIFICATION.md](AUDIO-DOWNMIX-SYNTHETIC-QUALIFICATION.md) | Shipped Jellyfin 8.1.3 per-side synthetic downmix measurements, clipping and AAC limiter margin; no real-content, listening, device or output enablement claim. | open |
| [AUDIO-DOWNMIX-REAL-CONTENT-QUALIFICATION.md](AUDIO-DOWNMIX-REAL-CONTENT-QUALIFICATION.md) | Real-content stereo downmix measurement (nine film windows): the incumbent fold clipped and AC-3/E-AC-3 applied stored levels; the float matrix plus −4 dBFS limiter that ships, and its encoded-VOD join proof. | open |
| [VOD-BFRAMES-TIMELINE-DESIGN.md](VOD-BFRAMES-TIMELINE-DESIGN.md) | Strict presentation-grid implementation and optional signed-offset software B-frames; no-reorder default, per-family/client qualification and remaining evidence. | open |
| [DECODE-FACTS-GATE-AND-FALLBACK.md](DECODE-FACTS-GATE-AND-FALLBACK.md) | Implementation plan from the 2026-09-20 architecture review: instrumenting the decode-fact gate, amortising immutable-image validation without losing tamper checks, and classifying fallback reasons. | open |
| [TRANSCODE-DECOMPOSITION-PLAN.md](TRANSCODE-DECOMPOSITION-PLAN.md) | Built extraction and all three removal consumers; delegated lifecycle/seam decisions and retained evidence. Optional registry evaluation is deferred; S-05 physical acceptance remains open. | built |
| [ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md](ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md) | Why immersive AC-4 and delayed AC-3 broke live starts, what the fixes preserve, and why the 103.1 capture cannot initialize its decoder. | open |
| [ATSC 3.0 live audio repair status](ATSC3-AUDIO-STARTUP-STATUS.html) | Implementation, single adversarial review, final fast lane, merge and outstanding live acceptance. | open |
| [LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md](LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md) | Why idle tuners reported busy: subtitle backfill blocked admission; pool policy and refusal classification fixes. | built |
| [LIVE-TV-DIRECT-PLAY-AND-SURROUND.md](LIVE-TV-DIRECT-PLAY-AND-SURROUND.md) | 2026-09-24: why every ATSC 1.0 channel was encoded and every encode was stereo, the copy-first audio track selection, the sink-derived AAC channel claim, Android hardware MPEG-2, and the aspect-ratio report the server could not reproduce. | built |
| [RESUME-ROLLING-PUBLICATION-RCA.md](RESUME-ROLLING-PUBLICATION-RCA.md) | Why resume failed across clients: rolling-engine fallback, unopened web MediaSource and progress update erasing position after failed startup. | open |
| [PLAYBACK-STARTUP-LATENCY-M2-20261002.json](PLAYBACK-STARTUP-LATENCY-M2-20261002.json) | Candidate floor rejection arithmetic and bounded generated hls.js startup sweep; continuity/native qualification state. | open |
| [PLAYBACK-STARTUP-LATENCY-M0-20261002.md](PLAYBACK-STARTUP-LATENCY-M0-20261002.md) | Native Safari preparation/publication incident and review-reported index backlog, with evidence limits. | done |
| [PLAYBACK-STARTUP-LATENCY-M0-20261002.json](PLAYBACK-STARTUP-LATENCY-M0-20261002.json) | Sanitized retained phase receipt for the native Safari incident; no successful presentation claim. | done |
| [PLAYBACK-STARTUP-LATENCY-REVIEW-20261002.md](PLAYBACK-STARTUP-LATENCY-REVIEW-20261002.md) | Supplied proposal review, single final implementation review and conservative admission disposition. | done |
| [HDR-CONTENT-LIGHT-INDEX.md](HDR-CONTENT-LIGHT-INDEX.md) | Why changing HDR light-level metadata forced rolling startup, and the indexed-copy metadata repair. | open |
| [PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md](PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md) | Phase origins, incident replay, index/resource observations, transport limits, and the A-M0 startup lab with its decision rule. | open |
| [PLAYBACK-STARTUP-LATENCY-BUILD.md](PLAYBACK-STARTUP-LATENCY-BUILD.md) | Sol handoff: clone, CI/CD rulings, milestones, one reviewed PR, fast lane, merge, cleanup. | open |
| [PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md](PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md) | Reviewed runtime build contract for the measured 14.2-second start, index availability, qualified publication policy and cross-client acceptance. | open |
| [STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md](STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md) | Reviewed rev 6 handoff: 8 s encoded rolling target, fewer create-path quorum reads and operator tone-map census before changes. | open |
| [PLAYBACK-STARTUP-PATH-FINDINGS.md](PLAYBACK-STARTUP-PATH-FINDINGS.md) | Play-to-first-frame waits across routes: fixed 40 ms consistent-read ACK stall and web scrim, plus ranked remaining waits and owners. | open |
| [PLAYBACK-STARTUP-LATENCY-REVIEW.md](PLAYBACK-STARTUP-LATENCY-REVIEW.md) | Independent adversarial plan review, three findings and verified dispositions. | open |
| [Playback startup build status](PLAYBACK-STARTUP-LATENCY-STATUS.html) | Live Sol build status: phase, PR, evidence, review, lane, decisions. | open |
| [SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md](SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md) | Why repeated seeks refuse playback and then reap the incumbent: retired streams retain a producer's whole reservation, and a refused destination poisons the incumbent's control snapshot. | open |
| [WEB-SEEK-LATENCY-RCA-AND-FIX.md](WEB-SEEK-LATENCY-RCA-AND-FIX.md) | October 2 VOD loader deadlock, false seek-presentation evidence and native rolling reopen latency: controlled causal evidence, proposed repair and adversarial review. | open |
| [Web seek incident evidence](../evidence/web-seek-rca-2026-10-02.json) · [mechanism replay](../evidence/web-seek-mechanism-replay.cjs) | Sanitized live seek traces, same-session loader intervention and executable incident-source mechanisms. | done |
| [SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md](SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md) | The build order for that repair: one scratch ledger, a writer barrier, exact release grace, growing reservations, ownership boundaries and the evidence each unit owes. | open |
| [Seek scratch reservations status](SEEK-SCRATCH-RESERVATIONS-STATUS.html) | Live ledger for the effort: unit state, base and compiler, evidence, review, fast lane and deployment. | open |
| [Seek scratch reservations receipt](../evidence/seek-scratch-reservations-receipt-2026-09-20.md) | Exact base and compiler, the accounting/release/mechanism proofs, which regression covers which acceptance row, what already failed before the change, and what is still owed. | open |
| [FFmpeg HLS write boundary receipt](../evidence/ffmpeg-hls-write-boundary-receipt-2026-09-24.md) | The last whole-ceiling reservation removed: FFmpeg's HLS muxer uploads through a loopback endpoint that grants every piece before it lands, the legacy copy hole, the inverted grant wait, and the proofs. | open |
| [FFMPEG-SCRATCH-SINK-IMPLEMENTATION.md](FFMPEG-SCRATCH-SINK-IMPLEMENTATION.md) | Historical design proposal for the owned FFmpeg write boundary; use the receipt above for the implemented mechanism and proof. | done |
| [MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md](MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md) | Why one valid MKV was denied immutable VOD, why its rolling fallback violated AVFoundation's playlist-update deadline, and the packet-duration plus sliding-HLS repair contracts. | open |
| [MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md) | The build contract for the reviewed MKV-duration and sliding-HLS repair: startup admission, packet evidence, publication scheduling, object grace, and acceptance. | open |
| [MKV-DURATION-AND-SLIDING-HLS-STATUS.md](MKV-DURATION-AND-SLIDING-HLS-STATUS.md) | Live package, compiler, review, qualification, and production-authorization ledger for the MKV-duration and sliding-HLS repair. | open |
| [APPLE-TV-FORWARD-SKIPS-STATUS.md](APPLE-TV-FORWARD-SKIPS-STATUS.md) | Live implementation, review, focused-proof, fast-lane and merge ledger for cumulative rolling-publication pacing and Apple TV position continuity. | open |
| [APPLE-TV-FORWARD-SKIPS-RCA.md](APPLE-TV-FORWARD-SKIPS-RCA.md) | Why rolling media advancing faster than playback made Apple TV skip forward, with production and AVPlayer replay evidence. | done |
| [APPLE-TV-FORWARD-SKIPS-IMPLEMENTATION.md](APPLE-TV-FORWARD-SKIPS-IMPLEMENTATION.md) | Historical build contract for cumulative publication pacing, protected playback position and focused regressions; use the status above for actual delivery evidence. | done |
| [CONTENT-ANALYSIS-REPAIR-STATUS.md](CONTENT-ANALYSIS-REPAIR-STATUS.md) | Live implementation ledger for selected-video completion, bounded retry, exact-identity repair, review, and promotion. | open |
| [CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md](CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md) | Why complete video indexes were labelled incomplete: fleet evidence, selected-stream duration, timeout policy, and recovery constraints. | open |
| [CONTENT-ANALYSIS-FAILURES-ADVERSARIAL-REVIEW.md](CONTENT-ANALYSIS-FAILURES-ADVERSARIAL-REVIEW.md) | Independent design review that found retry expiry and lost video identity before implementation. | done |
| [CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md](CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md) | Build contract for selected-video completion, typed diagnostics, bounded retry cycles, exact-identity recovery, migrations, tests, and rollout. | open |
| [HEVC-IN-BAND-PARAMETER-SETS.md](HEVC-IN-BAND-PARAMETER-SETS.md) | Why a chunk-encoded WEB-DL copies with pink and green blotches, why PR #535 never shipped, and the per-file census that keeps redefined parameter sets in band. | open |
| [HEVC-SAMPLE-ENTRY-STATUS.md](HEVC-SAMPLE-ENTRY-STATUS.md) | Live execution ledger for the HEVC sample-entry admission repair: implementation, evidence, review, and promotion state. | open |
| [SAFARI-DIAGNOSIS-AND-FIX.md](SAFARI-DIAGNOSIS-AND-FIX.md) | Why HEVC-in-MP4 admission fails on Safari, the decoder evidence behind the repair, and the accepted review findings. | open |
| [HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md](HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md) | Executable contract for source sample-entry facts, compatible packaging, client admission, downgrade safety, and focused acceptance. | open |
| [HEVC sample-entry qualification receipt](../evidence/hevc-sample-entry-qualification-2026-09-16.md) | Exact reviewed head, focused commands and counts, F1–F10 disposition, rollout order, and physical-evidence limits for PR #337. | built |
| [TCL-PROBE-MISMATCH-RCA-AND-FIX.md](TCL-PROBE-MISMATCH-RCA-AND-FIX.md) | Why one added E-AC-3 Atmos profile refused a whole movie, what shipped to admit it, and why report equality is the wrong source-verification contract. | built |
| [Probe compatibility replay](../evidence/probe-compatibility-replay.py) | Runs the deployed and shipped source-probe comparators side by side, on a synthetic pair or on two real FFprobe documents. | built |
| [STREAMING-RELIABILITY-IMPLEMENTATION.md](STREAMING-RELIABILITY-IMPLEMENTATION.md) | Two-wave reliability effort: quality preservation, truthful recovery, first-play preparation, task ownership and finite qualification. | open |
| [STREAMING-SHARED-INDEX-HANDOFF.md](STREAMING-SHARED-INDEX-HANDOFF.md) | Sol work package for exact shared Dolby Vision indexes and bounded first-play measurements. | open |
| [STREAMING-WEB-RECOVERY-HANDOFF.md](STREAMING-WEB-RECOVERY-HANDOFF.md) | Sol work package for truthful web stall evidence, recipe-preserving recovery and native parity. | open |
| [VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) | Film-based VOD programme; §13 tracks the F1/N2 shared-zero, byte-fallback and entry-scoped landing repair separately from device/fleet acceptance. | open |
| [VOD-PRESENTATION-PLAN-REVIEW.md](VOD-PRESENTATION-PLAN-REVIEW.md) · [response](VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md) | Six contracts that were not buildable, and the answers to them. | done |
| [VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md](VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md) | How to build the plan without breaking the live path. | open |
| [VOD-PRESENTATION-M0-REVIEW-BRIEF.md](VOD-PRESENTATION-M0-REVIEW-BRIEF.md) | Three normative sentences re-decided before M1. | done |
| [VOD-M0-ISSUES.md](VOD-M0-ISSUES.md) | Every issue M0 found. | done |
| [VOD-M2-QUESTIONS.md](VOD-M2-QUESTIONS.md) | The M2 questions and how each was ruled. | done |
| [VOD-M3-HANDOFF.md](VOD-M3-HANDOFF.md) | Serving from the plan — the first code on the live path. | built |
| [VOD-M4-HANDOFF.md](VOD-M4-HANDOFF.md) | Web adoption and release acceptance. | built |
| [VOD-CUTOVER.md](VOD-CUTOVER.md) | Historical VOD-only cutover; superseded by the retained first-play fallback. | done |
| [VOD-ENCODING.md](VOD-ENCODING.md) | How immutable transcode and subtitle-burn VOD produce and verify the bytes named by a playlist. | built |
| [VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md](VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md) | Restoring the seek-storm case that CI disabled 2026-08-26. | open |
| [VOD-STALL-ACCEPTANCE-HANDOFF.md](VOD-STALL-ACCEPTANCE-HANDOFF.md) | Restoring the bandwidth-recovery case; blocked on a device measurement. | open |
| [VOD-STEADY-ACCEPTANCE-HANDOFF.md](VOD-STEADY-ACCEPTANCE-HANDOFF.md) | Restoring the steady-play case that CI disabled 2026-08-26. | open |
| [STREAMING-RELIABILITY-REVIEW.md](STREAMING-RELIABILITY-REVIEW.md) | Keeping the stream alive while its future changes. | done |
| [STREAMING-RELIABILITY-STATUS.md](STREAMING-RELIABILITY-STATUS.md) | Review, repair and promotion status of that effort. | open |
| [WEB-HLS-STARTUP-RECOVERY-STATUS.md](WEB-HLS-STARTUP-RECOVERY-STATUS.md) | Implementation, review, qualification, and promotion status for delayed-manifest web startup recovery. | open |
| [WEB-HLS-STARTUP-RECOVERY-IMPLEMENTATION.md](WEB-HLS-STARTUP-RECOVERY-IMPLEMENTATION.md) | Build contract for unloaded-manifest recovery, bounded startup retries, and cause-correct decoder reporting. | open |
| [WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md](WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md) | Free Fall playback repair: loader ownership, stable init identity and scoped drift verdicts, binary refusals, and causal Auto quality. | open |
| [WEB-HELD-SEEK-STALL-RCA.md](WEB-HELD-SEEK-STALL-RCA.md) | Why one held Right Arrow became two client commits but one source change, how a refilled presentation wait spent recovery too early, and the live implementation status. | open |
| [NATIVE-HLS-STARTUP-RCA.md](NATIVE-HLS-STARTUP-RCA.md) | Why native Safari turned a temporarily unavailable playlist into a codec failure and source-rescan refusal. | open |
| [NATIVE-HLS-STARTUP-IMPLEMENTATION.md](NATIVE-HLS-STARTUP-IMPLEMENTATION.md) | Build contract for native-HLS readiness, bounded reload, joinable source preparation, and three-client parity. | open |
| [SAFARI-SEEK-IMPLEMENTATION.md](SAFARI-SEEK-IMPLEMENTATION.md) | Build contract for native seek evidence, viewer preparation, shared I/O admission, reproducible runtime inputs, and bounded rolling coverage. | open |
| [SAFARI-SEEK-STATUS.md](SAFARI-SEEK-STATUS.md) | Current Safari seek implementation, validation, review, and promotion state. | open |
| [Native Safari seek fixture evidence](../evidence/safari-seek-native-2026-09-29.json) | Served playlists, media generator identity, and native buffered/seekable/landing snapshots for four synthetic variants. | done |
| [Native startup replay](../evidence/native-startup-replay.cjs) | Replays the native-HLS startup controller against delayed publication without private media or a running server. | open |
| [STREAMING-RELIABILITY-HANDOFF.md](STREAMING-RELIABILITY-HANDOFF.md) | The remaining work, for the next streaming agent. | open |
| [STREAMING-CONTINUATION-HANDOFF.md](STREAMING-CONTINUATION-HANDOFF.md) | The continuation-session workflow, branch state, and integration queue for streaming reliability. | open |
| [PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md) | Highest deliverable grade, negotiated rather than guessed. | open |
| [PLAYBACK-CAPS-V2-M0.md](PLAYBACK-CAPS-V2-M0.md) | The M0 measurements taken on lab4, 2026-08-30. | done |
| [MEDIA-BADGES-PLAN.md](MEDIA-BADGES-PLAN.md) | Making the play menu tell the truth about HDR and Dolby Vision. | built |
| [M5A-CLIENT-BADGE-HANDOFF.md](M5A-CLIENT-BADGE-HANDOFF.md) | The `DV P7 → DV P8` badge in the Apple and Android clients. | built |
| [M5A-VERIFICATION-ON-LAB4.md](M5A-VERIFICATION-ON-LAB4.md) | The container-truth check that has to run on real media. | open |
| [M5B_STATUS.md](M5B_STATUS.md) | Permanent Dolby Vision Profile 7 conversion — what merged. | built |
| [M5-VERIFICATION-PROMPT.md](M5-VERIFICATION-PROMPT.md) | Fleet verification: the first converted stream a browser ever plays. | open |
| [ANDROID-DV-CONVERSION-RCA-AND-FIX.md](ANDROID-DV-CONVERSION-RCA-AND-FIX.md) | Why a capable Lenovo received the HDR base instead of converted Profile 8.1, and the bounded repair. | open |
| [ANDROID-DV-CONVERSION-IMPLEMENTATION.md](ANDROID-DV-CONVERSION-IMPLEMENTATION.md) | Live build and acceptance record for request-scoped Profile 7 conversion, native-DV progressive packaging, and Android HLS retention. | open |
| [HEVC-COLOR-FIX-STATUS.html](HEVC-COLOR-FIX-STATUS.html) | Current implementation, decision, review and merge status for the HEVC color repair. | open |
| [HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md](HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md) | Why copied HEVC turns pink and green, the matched-pixel reproduction, and the implemented proof-before-stripping containment with an unrestricted Developer override. | open |
| [HEVC-COLOR-CORRUPTION-REVIEW.md](HEVC-COLOR-CORRUPTION-REVIEW.md) | Independent adversarial review of the HEVC color diagnosis and implementation, with objections, amendments and remaining delivery gates. | open |
| [DV-P5-APPLE-START-REPAIR.md](DV-P5-APPLE-START-REPAIR.md) | Why a P5 Apple TV start fails before publication, the matching-tuple header repair, per-sample rolling guard, adversarial findings and qualification limits. | built |
| [DV-DELIVERY-FINDINGS.md](DV-DELIVERY-FINDINGS.md) | Why Dolby Vision titles arrive as HDR10 or lower. | open |
| [DV-SELECTION-PATHS.md](DV-SELECTION-PATHS.md) | Current DV delivery, enhancement, decoder/encoder, proof and fallback decisions; source/test anchors and acceptance limits. | live |
| [DV_HDR_PROCESSING_PLAN.md](DV_HDR_PROCESSING_PLAN.md) | FEL-aware DV to HDR10 and Profile 7 to 8.1 proposal: quality measurements, controls and fallbacks. | open |
| [DV_HDR_PROCESSING_BUILD.md](DV_HDR_PROCESSING_BUILD.md) | Sol 6.1 implementation handoff: backend feasibility, integration contracts, benchmarks and qualification. | open |
| [DV_HDR_PROCESSING_REVIEW.md](DV_HDR_PROCESSING_REVIEW.md) | Adversarial findings and dispositions for the DV processing proposal and build handoff; cleared for M0 feasibility only. | done |
| [DV_HDR_PROCESSING_FEASIBILITY.md](DV_HDR_PROCESSING_FEASIBILITY.md) | Sol 6.1 M0 reuse findings: existing DV/FEL renderers, P8.1 recipe, metric checks and unproven boundaries. | open |
| [DV_HDR_M1_CONTRACTS.md](DV_HDR_M1_CONTRACTS.md) | Approved additive M1 type, frame-coverage, graph and recovery-episode contracts; the initial slice kept the production registry empty. | open |
| [DV_HDR_PROCESSING_STATUS.md](DV_HDR_PROCESSING_STATUS.md) | Processing and acceptance ledger. | open |
| [HDR-ENHANCEMENT-SOURCE-BUDGET.md](HDR-ENHANCEMENT-SOURCE-BUDGET.md) | Source budgets and coded limits. | open |
| [DV-FOLLOWUPS-HANDOFF.md](DV-FOLLOWUPS-HANDOFF.md) | What is left after the HDR10-E work, for any agent: the Pixel HLS/Animals branch, FEL→8.1 for DV displays, display-targeted PR A, and small open items, with branches, files, tests and merge lines. | open |
| [DM-PRECISION-STATUS.md](DM-PRECISION-STATUS.md) | Shader PQ precision repair, rejected CM v2.9 affine experiment, ownership and merge evidence. | done |
| [DISPLAY-TARGETED-DV-BUILD.md](DISPLAY-TARGETED-DV-BUILD.md) | Current A–F implementation and source-pinned qualification matrix, retained checkpoint history, remaining effort promotion, and physical accuracy acceptance. | in progress |
| [DISPLAY-TARGETED-DV-QUALIFICATION-HISTORY.md](DISPLAY-TARGETED-DV-QUALIFICATION-HISTORY.md) | Historical display-target source checkpoints and qualification records, retained verbatim with original attribution. | historical |
| [DISPLAY-TARGETED-DV-TRIM-CONTRACT.md](DISPLAY-TARGETED-DV-TRIM-CONTRACT.md) | Superseded stub: the never-merged Annex B.3 RGB trim candidate, replaced by the closed-form display management of the build plan's phase 2. | superseded |
| [DV-ENHANCEMENTS-NOT-YET-BUILT.md](DV-ENHANCEMENTS-NOT-YET-BUILT.md) | Which Dolby Vision enhancements are possible but unbuilt (HDR10+ from L1, DV→SDR trims, MEL detection, P5, more routes and profiles, pipeline performance) and what each involves. | open |
| [DV_HDR_REPAIR_LOG.md](DV_HDR_REPAIR_LOG.md) | Running log of the fleet HDR repair (`effort/dv-hdr-repair`): reproduced failures, repair rounds R1 onward, acceptance still owed. | open |
| [DV fixture recipes](../evidence/dv-processing-2026-10-09/dv-fixture-reproduction.tar.gz) · [archive receipt](../evidence/dv-processing-2026-10-09/fixture-archive-receipt.json) | Reproduce and verify the synthetic acceptance sources; generated movies and private media are excluded. | built |
| [DV_HDR_BACKEND_CONTROLS.md](DV_HDR_BACKEND_CONTROLS.md) | Reproduce reviewed synthetic libplacebo and DoViBaker pixel controls; exact build evidence and limits. | open |
| [Parsed metadata and authoring controls](DV_HDR_AUTHORING_CONTROLS.md) | Reproduce parsed-RPU rendering and synthetic reconstructed-base authoring; distinguish completed mechanics from timing, nonlinear metadata and conformance gaps. | open |
| [DV_HDR_DECODED_LAYER_CONTROLS.md](DV_HDR_DECODED_LAYER_CONTROLS.md) | Reproduce reviewed encoded BL/EL/RPU association, B-frame timing, stale-metadata refusal and accepted-frame rendering controls. | open |
| [DV_HDR_TIMELINE_CONTROLS.md](DV_HDR_TIMELINE_CONTROLS.md) | Reproduce bounded VFR, real seek/preroll and timestamp-epoch controls with stored-duration, lifecycle and failure-status evidence. | open |
| [DV_HDR_RPU_REUSE_CONTROLS.md](DV_HDR_RPU_REUSE_CONTROLS.md) | Reviewed explicit mapping reuse, cache/reset refusals, input-bound execution and retained-prerequisite replay. | open |
| [DV_HDR_COMBINED_CONTROLS.md](DV_HDR_COMBINED_CONTROLS.md) | Reviewed finite P7/FEL source-to-HDR10 chain, actual output/timing, numerical limits and remaining movie integration. | open |
| [FEL-P81-RECONSTRUCTION.md](FEL-P81-RECONSTRUCTION.md) | Reconstructed Profile 8.1: admission, truthfulness, proofs, route recipe, per-title qualification. | open |
| [DV_PROCESSING_TOOLS.md](DV_PROCESSING_TOOLS.md) | Build and run the bounded streaming FEL reconstruction and P8.1 authoring helpers; remaining serving limits. | open |
| [DV_HDR_NONIDENTITY_CONTROLS.md](DV_HDR_NONIDENTITY_CONTROLS.md) | Reproduce reviewed affine FEL reconstruction and P8.1 repeated-reshape controls, separate HDR10-base checks and exact replay provenance. | open |
| [DV-DISK-CONVERSION-DIAGNOSIS.md](DV-DISK-CONVERSION-DIAGNOSIS.md) | Why the on-disk conversion never completes, and a proposal. | open |
| [QUEUE-REPAIR-VERIFICATION-PROMPT.md](QUEUE-REPAIR-VERIFICATION-PROMPT.md) | Did the fragment-index queue come back, and stay back? | open |
| [DECODER_SELECTION_AND_RECOVERY_PLAN.md](DECODER_SELECTION_AND_RECOVERY_PLAN.md) | An explicit decoder choice and recovery path for each producer. | open |
| [DECODER-EFFORT-HANDOFF.md](DECODER-EFFORT-HANDOFF.md) | Picking the decoder effort up: what blocks it, what is left, how to work here, and the traps it already fell into. | open |
| [DECODER-M8-HANDOFF.md](DECODER-M8-HANDOFF.md) | Running the remaining hardware, workload, latency, concurrency, and three-client evidence after the decoder implementation merged. | open |
| [AVI_VIDEOTOOLBOX_REVIEW_DECISION.md](AVI_VIDEOTOOLBOX_REVIEW_DECISION.md) | The VideoToolbox decode fix to build, and the follow-up it requires. | open |
| [VideoToolbox live-TV repair status](LIVE-TV-VIDEOTOOLBOX-STATUS.html) | Current branch, implementation, review, validation and merge progress for the ATSC 1.0 caption repair. | open |
| [ATSC 1.0 VideoToolbox root cause and repair](LIVE-TV-VIDEOTOOLBOX-ATSC1-ROOT-CAUSE-AND-FIX.md) | Reproduction, current-main implementation, review dispositions and final local validation for caption-triggered encoding failures. | built |
| [VideoToolbox file-caption follow-up](VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md) | Caption SEI and held-clock root causes, corrected normal VOD preservation, the initial-padding oracle and remaining lifecycle qualification. | open |
| [APPLE-PACING-HOLD-FREEZE-ROOT-CAUSE.md](APPLE-PACING-HOLD-FREEZE-ROOT-CAUSE.md) | Why Apple and web HLS froze on a pacing hold, and the repair contract. | built |
| [STUTTER-4K.md](STUTTER-4K.md) | 4K copy-path stutter: what it is, what it isn't, what to try next. | open |
| [SEGMENTER-PLAN.md](SEGMENTER-PLAN.md) | GOP-aware segmenting — zero boundary drops on the copy path. | built |
| [DISPLAY-AWARE-AUTO-QUALITY-PLAN.md](DISPLAY-AWARE-AUTO-QUALITY-PLAN.md) | How Auto should preserve smooth original playback, choose display-appropriate intermediate transcodes, and recover quality safely across Android, Apple and web. | open |
| [DISPLAY-AWARE-AUTO-QUALITY-REVIEW.md](DISPLAY-AWARE-AUTO-QUALITY-REVIEW.md) | Adversarial findings on the automatic-quality plan, their disposition, and remaining qualification risks. | open |
| [DISPLAY-AWARE-AUTO-QUALITY-OPUS-HANDOFF.md](DISPLAY-AWARE-AUTO-QUALITY-OPUS-HANDOFF.md) | The independent Opus review request and the evidence it must challenge before implementation. | open |
| [DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.md](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.md) | Lifecycle wrapper for the independent Opus review; links to its byte-identical source artifact and reconciliation. | done |
| [DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.txt](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.txt) | Supplied independent Opus review, preserved byte-for-byte: four P1 and ten P2 findings and source evidence. | done |
| [ADAPTIVE-QUALITY.md](ADAPTIVE-QUALITY.md) | The design for bandwidth-aware streaming. | live |

## Notes moved from the front of the docs index

DV final bounded serving evidence: [HTTP summary](../evidence/dv-processing-2026-10-09/http-summary.json) and [artifact manifest](../evidence/dv-processing-2026-10-09/http-manifest.json) — retained local synthetic results and failed attempts; full promotion evidence belongs to PR #968.
