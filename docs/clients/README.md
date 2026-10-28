# clients/ — Apple, Android, and the web player

The index of `docs/clients/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

The native clients and the web UI: parity trackers, the input contract every
player obeys, subtitles and overlays, layouts and themes.

| File | Answers | |
|---|---|---|
| [LINUX-PLAYER-CINEMA-UI.md](LINUX-PLAYER-CINEMA-UI.md) | Cinema-style native controls, scoped library metadata and Next, quality replacement ownership, package qualification and ROG installation ledger. | open |
| [LINUX-PLAYER-PLAN.md](LINUX-PLAYER-PLAN.md) | How Dolby Vision plays on Linux through plurx: a web handoff to plurx Player (an mpv launcher), the server playback handoff (§3.1), Profile 7 FEL composed in the player (reconstructed Profile 8.1 from the server for clients that cannot compose), the closed-form mapping in a private mpv, packaging, and the milestones with their acceptance. | open |
| [HDR-DISPLAY-PEAK.md](HDR-DISPLAY-PEAK.md) | Android/TV platform reports, browser peak choices, session boundaries and the synthetic PQ comparison; physical qualification pending. | open |
| [LINUX-PLAYER-PLAN.md](LINUX-PLAYER-PLAN.md) | How Dolby Vision reaches Linux through plurx: mpv with a launcher, the playback handoff, the caps the player claims, Profile 7 FEL composed in the player, reconstructed P8.1 for other clients (M2), and the milestones. | open |
| [FRAME-DROP-RECORDER.md](FRAME-DROP-RECORDER.md) | Automatic web incident capture, bounds, privacy, retrieval and attribution limits; production acceptance pending. | built |
| [AVATAR-WEB-PLAYBACK-STATUS.md](AVATAR-WEB-PLAYBACK-STATUS.md) | Avatar Safari presentation faults, FEL encoder-selection root cause, physical evidence and review state. | open |
| [Apple TV library row clipping](../apple-builds/1245-tvos-library-row-clipping.md) | Complete movie/show posters and metadata fit while browsing; the preamble scrolls and compact tools retain Back/Down focus. Physical Apple TV acceptance pending. | built |
| [SYSTEM-SETTINGS-STATUS.md](SYSTEM-SETTINGS-STATUS.md) | Approved System layout, backend capability/package facts, representative comparison and source-bound rendering; Linux hardware acceptance remains open. | built |
| [SAFARI-PLAYBACK-ACTIVATION.md](SAFARI-PLAYBACK-ACTIVATION.md) | Safari gesture-bound media preparation, immediate permission-refusal ownership, independent review and focused regression evidence, and pending physical acceptance. | open |
| [Meridian source identity](../apple-builds/1195-meridian-source-identity.md) | Choosing another source from the Meridian panel opens it and focuses its tools; Apple native TV job limit 120 minutes. Physical Apple TV acceptance pending. | built |
| [Retained Apple resume position](../apple-builds/1181-retain-resume-position.md) | Interrupted native resume seeks retry once; failed preparation, Retry and Close retain the saved destination. Physical iPad acceptance pending. | built |
| [Restore Standard Apple layouts](../apple-builds/1178-restore-standard-apple.md) | Restores native iPad/Apple TV tabs and original TV Theater styling after Meridian changed Standard; existing themes stay unchanged. | built |
| [APPLE-TV-NAVIGATION-AUDIT.md](APPLE-TV-NAVIGATION-AUDIT.md) | October 15 whole-app TV navigation source audit, native menu failure reproduction, 36 findings, implementation ledger and mandatory navigation acceptance contract. | open |
| [APPLE-TV-NAVIGATION-FINDINGS.md](APPLE-TV-NAVIGATION-FINDINGS.md) | The 36 Apple TV navigation findings (F01–F36): evidence, fix and proof for each. | open |
| [APPLE-TV-NAVIGATION-CONTRACT.md](APPLE-TV-NAVIGATION-CONTRACT.md) | The Apple TV navigation release contract: remote actions, ownership, required journeys and qualification. | open |
| [Apple TV navigation renderings](apple-tv-navigation/index.html) | Twelve proposed screen/acceptance plates and actual synthetic native failure captures; designs, not implemented fixes. | open |
| [Android playback repair status](ANDROID-PLAYBACK-REPAIR-STATUS.md) | Merged playback repairs, validation and remaining physical-device acceptance. | built |
| [Ready-video recovery build note](../apple-builds/888-ready-video-recovery.md) | Apple build claim and viewer-visible black-picture recovery changes for PR #888. | built |
| [APPLE-BLACK-VIDEO-IMPLEMENTATION.md](APPLE-BLACK-VIDEO-IMPLEMENTATION.md) | Avatar and HDR10 black-video investigation, verified false-frame evidence defects, item-bound presentation repair and physical acceptance limits. | open |
| [Growing-resume preparation build note](../apple-builds/914-growing-resume-preparation.md) | Apple build claim and bounded unpublished-session preparation repair for PR #914. | built |
| [APPLE-GROWING-RESUME-PREPARATION.md](APPLE-GROWING-RESUME-PREPARATION.md) | Why Naked Gun resumed through a premature preparation timeout, the exact server/client timing mismatch, bounded readiness repair, and remaining native-error evidence limits. | built |
| [RASPBERRY-PI-IMPLEMENTATION.md](RASPBERRY-PI-IMPLEMENTATION.md) | Build contract for existing Plurx server and web HDMI playback on Pi 5: request decoding, browser evidence, deployment and acceptance. | open |
| [RASPBERRY-PI-INSTALLATION.md](RASPBERRY-PI-INSTALLATION.md) | Implemented Docker-default Pi setup, selectable native/systemd, runtime/browser ownership and bounded acceptance; final CI pending. | built |
| [RASPBERRY-PI-STATUS.md](RASPBERRY-PI-STATUS.md) | Existing Plurx on Pi 5: completed software and bounded physical acceptance, retained history and pending final CI. | built |
| [Native library index scrolling](../apple-builds/823-ios-library-index-scroll.md) | Why index taps resolved to their own buttons, and the distinct row-target repair. | open |
| [Native grouped library rows](../apple-builds/821-native-library-rows.md) | Apple release note for native grouped browsing, jump navigation, and saved Rows/Grid. | built |
| [GROUPED-LIBRARY-ROWS-STATUS.md](GROUPED-LIBRARY-ROWS-STATUS.md) | Approved grouped library browsing contract, progress, review, and merge evidence. | open |
| [Live TV cluster resource](../apple-builds/537-live-tv-cluster-resource.md) | Apple build 193 durable cluster start intents and protocol negotiation; validation limits. | built |
| [Native policy runners](../apple-builds/634-native-policy-runners.md) | Apple 202 / Android 140 pure Auto-quality ports and shared JSON test runners; no native controller enablement or device acceptance. Removed 2026-10-04 (next row). | done |
| [Remove native policy runners](../apple-builds/634-remove-native-policy-runners.md) | Why the uncalled Swift/Kotlin `decideRung` ports, their runners and the Rust `decide_auto_transition` reducer were deleted, and why the shared fixture stays. | built |
| [Native seek completion deadline](../apple-builds/473-native-seek-deadline.md) | Apple recovery for seeks that never complete, cancellation and late-callback ownership. | built |
| [Live TV station startup](../apple-builds/568-live-tv-session-url.md) | Apple build 189 repair for valid station URLs rejected as expired sessions; regression and delivery limits. | built |
| [STATUS-HISTORY.md](STATUS-HISTORY.md) | Includes the 2026-09-24/25 sections moved on 2026-10-05. Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md](NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md) | The build plan the adaptive-quality design produced: settle the four policy disagreements in the web first, type the reopen cause on the wire only in the change that first sends one, add the Swift and JVM runners of the shared fixture, then one adapter at a time — each one disabled until its own shaped-network trace beats its own baseline. | open |
| [NATIVE-ADAPTIVE-QUALITY-DESIGN.md](NATIVE-ADAPTIVE-QUALITY-DESIGN.md) | Implementation plan from the 2026-09-20 architecture review: what adaptive quality on the Apple and Android clients would have to be — one shared policy, five named kinds of evidence, and the shaped-network trace that would let it be switched on. | open |
| [ANDROID-SHAPED-NETWORK-MEASUREMENT-PROTOCOL.md](ANDROID-SHAPED-NETWORK-MEASUREMENT-PROTOCOL.md) | A-04's manual acquisition procedure for the four named Android clients and both profiles: real proxy controls, six-metric acquisition gaps, identity, bounded execution, restoration and honest incomplete receipts. | open |
| [WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md](WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md) | Implementation plan from the 2026-09-20 architecture review: hls.js worker on, `recoverMediaError` inside the existing attempt fences, a global error reporter with redaction, and local seeking for rolling HLS within the retained published range. | open |
| [WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md](WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md) | Why an unlanded Safari VOD seek hit the 8 s stall rule before its fragment retry, and the reviewed 20 s local-seek fallback proposal. | open |
| [WEB-VOD-SEEK-IMPLEMENTATION-STATUS.md](WEB-VOD-SEEK-IMPLEMENTATION-STATUS.md) | Current progress, evidence owed, and merge state for the web VOD seek deadline repair. | open |
| [WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md](WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md) | Implementation plan from the 2026-09-20 architecture review: `tsc --checkJs` ratcheted from a baseline over the served script order, then splitting `play()` and `attachHls()` along their seams, plus the small W9 fixes each with its guardrail. | open |
| [APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md](APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md) | Implementation plan from the 2026-09-20 architecture review: asset-derived display criteria through the active window on tvOS, interruption and route-change ownership in all three players, and the device tests that prove either. | open |
| [APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md](APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md) | Implementation plan from the 2026-09-20 architecture review: an immutable per-continuation `Attempt` snapshot that keeps the nine epochs' scopes, one typed item-event observer for all three stacks, poll-to-KVO conversions that keep their deadlines, and staged strict concurrency. | open |
| [NATIVE-LIBRARY-PAGING.md](NATIVE-LIBRARY-PAGING.md) | Implementation plan from the 2026-09-20 architecture review: paging large libraries on demand on Apple and Android while preserving merged multi-library order and defined search behaviour for unloaded titles. | open |
| [ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md](ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md) | Android TV display-mode selection, actual-granted-heap containment for every buffer role, isolated measurement provenance and the still-open three-device allocation/HDMI qualification. | open |
| [ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md](ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md) | Implementation plan from the 2026-09-20 architecture review: background pause and audio foreground service as one contract, one player builder with role overrides, live-window recovery, audio-sink error classification, and status-gated node failover. | open |
| [ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md](ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md) | D-03 plan and 2026-09-25 approval ruling: the server grant exists, external book handoff remains open, and the mobile clients use in-app readers; also covers backup exclusions, dispatcher evidence, and signed release deployment. | open |
| [WATCH-AND-BROWSE-IMPLEMENTATION.md](WATCH-AND-BROWSE-IMPLEMENTATION.md) | Approved movie/episode watch browser, retained web host, task ownership, tests and pending native/real-media gates. | open |
| [WATCH-VIEW-LAYOUT.md](WATCH-VIEW-LAYOUT.md) | What the web watch view shows beside and below the picture: the collapsible chapter rail with on-request thumbnails, the media ledger whose chips pick tracks, one Close, and the Playback switch behind the thumbnails (Developer until 2026-09-28). | built |
| [APPLE-CLIENT-PARITY.md](APPLE-CLIENT-PARITY.md) | What the Apple client has, what it lacks, and which build proved it. | live |
| [ANDROID-CLIENT-PARITY.md](ANDROID-CLIENT-PARITY.md) | The same, for Android. | live |
| [ANDROID-DOUBLE-BACK-RCA-AND-FIX.md](ANDROID-DOUBLE-BACK-RCA-AND-FIX.md) | Why rapid Back taps can empty Android navigation, the proposed entry ownership guard, emulator regression evidence, and review and release acceptance. | open |
| [ANDROID-DOUBLE-BACK-IMPLEMENTATION-HANDOFF.md](ANDROID-DOUBLE-BACK-IMPLEMENTATION-HANDOFF.md) | Sol's build and delivery contract: recover the rebased candidate, finish the Media3 lint opt-in, preserve six regressions, validate, and prepare review and landing evidence. | open |
| [ANDROID-DOUBLE-BACK-STATUS.md](ANDROID-DOUBLE-BACK-STATUS.md) | Current branch, milestones, validation, review, PR, and physical acceptance for the Android double-Back fix. | open |
| [PLAYBACK-INFO-REDESIGN.md](PLAYBACK-INFO-REDESIGN.md) | Playback info hierarchy, measurement semantics and qualification. | open |
| [PLAYBACK-INFO-DIMENSIONS-STATUS.md](PLAYBACK-INFO-DIMENSIONS-STATUS.md) | Live build ledger for frame, aspect and provenance repairs across web, Apple and Android. | live |
| [PLAYBACK-INFO-DIMENSIONS.md](PLAYBACK-INFO-DIMENSIONS.md) | Source handoff for the playback dimension and aspect repair, with evidence rules and acceptance cases. | open |
| [PLAYBACK-INFO-MISSING-FIELDS-RCA.md](PLAYBACK-INFO-MISSING-FIELDS-RCA.md) | Why clients omit output facts or display source dimensions for a converted stream, with reviewed provenance rules. | open |
| [PLAYBACK-INFO-IMPLEMENTATION-HANDOFF.md](PLAYBACK-INFO-IMPLEMENTATION-HANDOFF.md) | Build sequence for output metadata, safe client collectors, attachment fencing, package ownership, and acceptance. | open |
| [330-calm-library-pages.md](../apple-builds/330-calm-library-pages.md) | Apple build 162 quiet Home and open media details. | built |
| [582-seek-observability.md](../apple-builds/582-seek-observability.md) | Apple build194 seek terminal beacons and live viewing method. | open |
| [590-acceptance-focus.md](../apple-builds/590-acceptance-focus.md) | Apple build197 physical TV focus navigation correction and acceptance provenance. | open |
| [465-library-filter-and-focus-regressions.md](../apple-builds/465-library-filter-and-focus-regressions.md) | Query, watch-filter and page-arrival focus regression batch (written as Apple200 / Android137; Apple 206 on `main`); source and physical qualification. | open |
| [602-native-live-tv-settings.md](../apple-builds/602-native-live-tv-settings.md) | Apple200 and Android137 native Live TV server settings leave Developer for Settings → Live TV. | open |
| [465-adaptive-library-prefetch.md](../apple-builds/465-adaptive-library-prefetch.md) | Apple199 and Android136 native two-row prefetch, diagnostics and profile tooling batch; qualification and physical evidence. | open |
| [594-live-tv-caption-and-capacity.md](../apple-builds/594-live-tv-caption-and-capacity.md) | Apple198 and Android135 Live TV caption choices and explicit capacity alternatives; qualification and physical acceptance. | open |
| [359-apple-pause-resume.md](../apple-builds/359-apple-pause-resume.md) | Apple return-to-picture owner, deadline, and advisory enablement. | built |
| [playback-info-redesign.md](../apple-builds/playback-info-redesign.md) | Apple build 161 playback-info changes. | open |
| [328-native-stall-parity.md](../apple-builds/328-native-stall-parity.md) | Apple release note for recipe-preserving native stall recovery. | open |
| [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) | One routing table every client obeys — which key does what, in which state. | live |
| [PLAYER-INPUT-CONTRACT-PLAN.md](PLAYER-INPUT-CONTRACT-PLAN.md) | How that contract was implemented. | built |
| [PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md](PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md) | What the physical devices did on 2026-09-02. | done |
| [PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) | Why an error overlay sits over a playing picture on every client, and the one fault contract that ends it (v2, ruled). | live |
| [PLAYBACK-SURFACE-CONTRACT-REVIEW.md](PLAYBACK-SURFACE-CONTRACT-REVIEW.md) | Adversarial review of PR #274: factual audit, recovery and ownership counterexamples, and acceptance gaps. | done |
| [PLAYBACK-SURFACE-CONTRACT-REVIEW-RESPONSE.md](PLAYBACK-SURFACE-CONTRACT-REVIEW-RESPONSE.md) | Finding-by-finding disposition of that review — what v2 of the contract changed and why. | done |
| [PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md) | The six-PR build plan for the surface contract: exact interfaces, owner sites, fence, acceptance commands. | open |
| [APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md](APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md) · [design review](APPLE-PAUSE-RESUME-HANDOFF-REVIEW.md) · [status](APPLE-PAUSE-RESUME-STATUS.md) | Bounded return to a moving picture after Apple pause: incident evidence, reviewed contract, and live implementation/promotion ledger. | open |
| [PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md](PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md) | Hand-off for a Mac: compile and test M2 and M5's Swift, which has never seen a compiler. | open |
| [PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md](PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md) | The same for M3 and M5's Kotlin: Gradle, the mutations, the device runs. | open |
| [PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md](PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md) | M4: the four §7 recipes on an Apple TV, an iPhone and an Android TV, and what counts as a pass. | open |
| [ANDROID-DV-LEVEL-CENSUS-PROMPT.md](ANDROID-DV-LEVEL-CENSUS-PROMPT.md) | For the session with the Android devices: dump each decoder's Dolby Vision profile/level pairs so a server-side level check is built only if some device needs it. | open |
| [PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md](PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md) | M6's gate: the one measurement that decides whether the Android remux-seek landing gets built at all. | open |
| [WEB-UI-USABILITY-AUDIT.md](WEB-UI-USABILITY-AUDIT.md) | Approved twelve-part web usability audit and acceptance criteria. | open |
| [WEB-UI-IMPLEMENTATION-STATUS.md](WEB-UI-IMPLEMENTATION-STATUS.md) · [status page](WEB-UI-STATUS.html) | Progress, decisions, review and final fast-lane evidence for the web UI implementation. | open |
| [UI-NAVIGATION-AUDIT.md](UI-NAVIGATION-AUDIT.md) | Why the player felt different on every client, anchored at `file:line`. | done |
| [CLIENTS-CODE-REVIEW.md](CLIENTS-CODE-REVIEW.md) · [assessment](CLIENTS-CODE-REVIEW-ASSESSMENT.md) | Capable players that under-ask the server — findings, and what to trust. | done |
| [CLIENTS-REMEDIATION-PLAN.md](CLIENTS-REMEDIATION-PLAN.md) | Restoring trust, then raising the quality ceiling. | built |
| [APPLE-NATIVE-SUBTITLES-PLAN.md](APPLE-NATIVE-SUBTITLES-PLAN.md) · [handoff](APPLE-NATIVE-SUBTITLES-HANDOFF.md) | Native text subtitles on Apple: the road, and what shipped. | built |
| [PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) | Why a 79.5 GB read blocked a start when a title needed PGS burn-in, what the 2026-09-21 incident actually was, and the three merged fixes. The September 24 two-device overlay check passed; current evidence is reconciled in the subtitle completion ledger. | built |
| [PGS-SUBTITLE-START-PATH-RCA-REVIEW.md](PGS-SUBTITLE-START-PATH-RCA-REVIEW.md) | Adversarial review of the RCA at `5c605768`: §5.3's web regression refuted, the index pass proven to be one full demux (so Fix C rides on it for free), answers to all seven questions, six rulings. | done |
| [SUBTITLE-CLUSTER-EXTRACTION-PLAN.md](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) | v2 implementation contract and M0–M5 execution log for clustered text and PGS extraction, queueing, hydration and backfill; fleet evidence pending. | built |
| [SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md](SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md) | Seven findings and dispositions that shaped the v2 contract. | done |
| [PARALLEL-SUBTITLE-RANGES-STATUS.md](PARALLEL-SUBTITLE-RANGES-STATUS.md) | Distributed playback text windows: bounded-seek evidence, implementation and verification status. | built |
| [SUBTITLE-CLUSTER-EXTRACTION-STATUS.md](SUBTITLE-CLUSTER-EXTRACTION-STATUS.md) | Current K-09 milestone, review and merge qualification status; fleet evidence pending. | built |
| [SUBTITLE-RELIABILITY-COMPLETION.md](SUBTITLE-RELIABILITY-COMPLETION.md) | Current cross-client subtitle defects, measured engine recovery, and reconciled remaining subtitle work. | built |
| [Subtitle qualification receipts](evidence/subtitles-20261009/final-qualification.tar.gz) | Sanitized local case/source hashes, initial failures and repaired app evidence, native compilation, and reproducible subtitle drivers; CI monitoring is recorded separately. | built |
| [SUBTITLE-MAC-COMPOSITION-EVIDENCE.md](SUBTITLE-MAC-COMPOSITION-EVIDENCE.md) | Bounded native-processing then CPU subtitle corpus, active-cue seek, EOF and cancellation observations, package/source identity and explicit reproducer. | done |
| [Mac subtitle composition receipt](SUBTITLE-MAC-COMPOSITION-EVIDENCE.json) | Sanitized package/input/driver hashes and analytic measurements for the bounded E2 graph acceptance. | done |
| [SUBTITLE-RELIABILITY-ASSESSMENT.md](SUBTITLE-RELIABILITY-ASSESSMENT.md) | Why subtitles still fail on every client after the rework landed — three symptoms root-caused at `c9e4edf4`, with confidence labels. | done |
| [SUBTITLE-RELIABILITY-HANDOFF.md](SUBTITLE-RELIABILITY-HANDOFF.md) | The build order for the repairs: six milestones, exact contracts, non-goals, acceptance. | built |
| [SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md](SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) | Eight cases for the session that has the devices — the hardware evidence none of this has yet. | open |
| [PGS_OVERLAY_PLAN.md](PGS_OVERLAY_PLAN.md) | Dolby Vision-safe PGS subtitle overlay. | built |
| [PGS-OVERLAY-M0-FEASIBILITY.md](PGS-OVERLAY-M0-FEASIBILITY.md) | The feasibility evidence for M0. | open |
| [PGS-OVERLAY-REVIEW-ASSESSMENT.md](PGS-OVERLAY-REVIEW-ASSESSMENT.md) | Accepted findings, and the re-review requested. | done |
| [APPLE-PGS-OVERLAY-ACCEPTANCE.md](APPLE-PGS-OVERLAY-ACCEPTANCE.md) | One iPad Pro run, one decidable acceptance record. | open |
| [UI-LAYOUTS-PLAN.md](UI-LAYOUTS-PLAN.md) | Four layouts and five themes, proposed for review. | done |
| [MERIDIAN-IMPLEMENTATION.md](MERIDIAN-IMPLEMENTATION.md) | Meridian web/Apple/Android layout and theme: visual contract, native navigation ownership, Sol build packets, review and rendered acceptance ledger. | open |
| [MERIDIAN-M0.md](MERIDIAN-M0.md) | Meridian M0 platform references, route/state-owner map, lifetime adapters, semantic tokens, toolchains and reproducible design gallery. | open |
| [MERIDIAN-APPLE-STATE.md](MERIDIAN-APPLE-STATE.md) | Every Apple Meridian destination, implicit/bound stack, sheet/player lifetime, retained-host adapter and focused acceptance gap. | open |
| [MERIDIAN-ANDROID-STATE.md](MERIDIAN-ANDROID-STATE.md) | Every Android Meridian destination, presentation/player lifetime, branch/selection migration and focused acceptance gap. | open |
| [UI-LAYOUTS-REVIEW.md](UI-LAYOUTS-REVIEW.md) | Ship a smaller, honest first slice. | done |
| [UI-LAYOUTS-IMPLEMENTATION.md](UI-LAYOUTS-IMPLEMENTATION.md) | The accepted slice, as built. | built |
| [UI-LAYOUTS-STATUS.md](UI-LAYOUTS-STATUS.md) | Ground truth for what of that slice is proven. | open |
| [UI-LAYOUTS-G3-DECISION.md](UI-LAYOUTS-G3-DECISION.md) | Did the layout abstraction pay for itself? | done |
| [JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md](JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md) | Whole Jellyfin/Emby effort: first clients, phased scope, estimates and exclusions. | open |
| [TV-REMOTE-AND-COMPANION-PLAN.md](TV-REMOTE-AND-COMPANION-PLAN.md) | TV remote and HDMI-CEC input, automatic phone/tablet remote discovery, pairing, cross-platform control, and the Linux/Pi, Mac, and Windows implementation sequence. | open |
| [TV-REMOTE-AND-COMPANION-REVIEW.md](TV-REMOTE-AND-COMPANION-REVIEW.md) | Independent adversarial review of the TV remote plan: controller authorization, command expiry, desktop CEC availability, and the tvOS feasibility gate. | open |
| [TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) | Build-ready Sol 6.1 packets, file ownership, native/desktop/background delivery, parent review, and batched-merge handoff. | open |
| [TV-REMOTE-NATIVE-INVITATIONS-BUILD.md](TV-REMOTE-NATIVE-INVITATIONS-BUILD.md) | Native notification enrollment, cold-launch taps, existing reminder/service ownership, Android resident networking and focused acceptance. | open |
| [TV-REMOTE-API.md](TV-REMOTE-API.md) | Implemented v1 HTTP DTOs, header proofs, owner routing, pairing/control lifecycle, storage migration and limits for receiver and companion clients. | open |
| [TV-REMOTE-INVITATIONS-API.md](TV-REMOTE-INVITATIONS-API.md) | Exact opt-in phone consent, background poll/tap, broker ticket and durable invitation lifecycle contract; first storage slice implemented, transport qualification pending. | open |
| [TV-REMOTE-PROTOCOL.md](TV-REMOTE-PROTOCOL.md) | Exact remote authority, node ownership, command/state wire types, receiver freshness, pairing and endpoint contracts. | open |
| [TV-REMOTE-AND-COMPANION-STATUS.md](TV-REMOTE-AND-COMPANION-STATUS.md) | Packet progress, parent review, compilation and physical acceptance evidence, without treating docs as shipped functionality. | open |
| [TV-REMOTE-OPERATIONS.md](TV-REMOTE-OPERATIONS.md) | Set up a TV receiver and paired phone, recover control, and record physical acceptance without treating in-progress packets as shipped support. | open |
| [JELLYFIN-SHARED-SERVICES.md](JELLYFIN-SHARED-SERVICES.md) | J1 shared login/token authority, token-only revocation and complete native watch operations; remaining compatibility lifecycle work. | open |
| [JELLYFIN-PLAY-BINDINGS.md](JELLYFIN-PLAY-BINDINGS.md) | J1 bounded negotiation metadata, live source incarnations, native route references, terminal retention and remaining adapter fences. | open |
| [JELLYFIN-COMPATIBILITY-BUILD.md](JELLYFIN-COMPATIBILITY-BUILD.md) | Infuse/Android TV build contract after three reviews: protocol spike, player identity, VOD fallbacks, watch fencing and acceptance. | open |
| [JELLYFIN-DURABLE-IDENTITIES.md](JELLYFIN-DURABLE-IDENTITIES.md) | J1 permanent opaque mappings, deterministic deletion retirement, both-backend migrations and import proof. | open |
| [JELLYFIN-PROTOCOL-FOUNDATION.md](JELLYFIN-PROTOCOL-FOUNDATION.md) | J1 pure tick/ID/credential forms, source replacement guard and the remaining service/storage boundary. | open |
| [JELLYFIN-COMPATIBILITY-REVIEW.md](JELLYFIN-COMPATIBILITY-REVIEW.md) | Sanitized first Opus review and historical author disposition; amended by the re-review. | done |
| [JELLYFIN-COMPATIBILITY-REREVIEW.md](JELLYFIN-COMPATIBILITY-REREVIEW.md) | Opus approvals and R1–R8 / S1–S4 dispositions: duration, restart attestation, cold-node artwork and trigger coverage. | done |
| [JELLYFIN-COMPATIBILITY-STATUS.md](JELLYFIN-COMPATIBILITY-STATUS.md) | J0/J1 integration, pinned reference/client evidence, and J2 connection/catalog work with remaining qualification gates. | open |
| [JELLYFIN-PASSIVE-VOD-ROUTE-LIFETIME.md](JELLYFIN-PASSIVE-VOD-ROUTE-LIFETIME.md) | J0 physical idle-reap failure and proposed bounded passive route grant; recovery proof still required. | open |
| [J0 baseline manifest](jellyfin/baseline-manifest.json) | Pinned server/schema/client provenance and synthetic fixture hashes; physical acceptance tracked separately. | open |
| [Docs publication evidence](jellyfin/docs-publication-receipt.json) | PR #744 exact-head/base docs fast-lane outcomes; not implementation or final release qualification. | done |
| [Android TV connection observation](jellyfin/androidtv-connection-observation.json) | Physical Android reference playback, controlled VTT/fMP4 normalization and native delivery spike; production adapter acceptance remains open. | open |
| [Infuse connection observation](jellyfin/infuse-connection-observation.json) | Physical Apple TV reference login, direct MKV, explicit transcode, carrier/profile differences and long-pause seeking. | open |
| [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) | Where the web app's sixty-five files are, what each one holds, where its code used to be in `index.html`, and the rules a new file has to obey. | live |
| [WEB-SHELL-SPLIT-PLAN.md](WEB-SHELL-SPLIT-PLAN.md) | How the 23,901-line web `index.html` became a multi-file app with no build step, and the byte-identity gate that proved nothing else changed. | built |
| [WEB_LAYOUT_CONTAINMENT_STATUS.md](WEB_LAYOUT_CONTAINMENT_STATUS.md) | Live delivery status of web layout containment. | open |
| [OFFLINE-VIEWING-PLAN.md](OFFLINE-VIEWING-PLAN.md) · [review](OFFLINE-VIEWING-REVIEW.md) | One-tap, app-managed downloads — the plan and its review. | built |
| [EBOOK-READER-PLAN.md](EBOOK-READER-PLAN.md) | plurx reads what Curator acquires. | open |
| [CLIENT-DEPLOY-PROMPT.md](CLIENT-DEPLOY-PROMPT.md) | Getting a merged build onto the phones and the Apple TVs. | live |
| [DVR-HARDWARE-VERIFICATION-PROMPT.md](DVR-HARDWARE-VERIFICATION-PROMPT.md) | Hand-off for the LAN: the guide tier, and the end-to-end recording pass no cloud session can run. | open |
| [Live TV startup and captions repair status](LIVE-TV-START-FEEDBACK-STATUS.html) | Review findings, final checks, merge and live acceptance for web startup feedback and captions controls. | open |

## Notes moved from the front of the docs index

Mobile UI work: [Apple build 159 notes](../apple-builds/317-ui-usability.md) · [Mobile and TV usability audit](MOBILE-UI-USABILITY-AUDIT.md) — **open**.

Native follow-up: [Phone, tablet and TV layout fixes](NATIVE-LAYOUT-FOLLOWUP.md) — **open**.

Library-page revision: [Quiet Home and open media details](CALM-LIBRARY-PAGES.md) — **built**; web Home and item page since restored to their originals.
