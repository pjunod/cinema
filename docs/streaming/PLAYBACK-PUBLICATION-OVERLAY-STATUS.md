# Playback publication and overlays — root-cause repair ledger

Companion to [the startup implementation ledger](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md) and [the playback surface contract](../clients/PLAYBACK-SURFACE-CONTRACT.md) — this records the deployed acceptance failures, their corrections, and the PR's progress toward merge.

**Status:** open — source repair implemented; deployment acceptance pending.

**Progress:** Adversarial review finished; its regression-fixture finding is addressed. This branch’s pull request description is the live record for compiler results, retained per-test evidence, the main fast lane and landing.

**Owner:** Codex/root; Sol owns the web lifecycle patch. **Branch:** `codex/playback-publication-overlay`. **PR:** See the pull request for this branch; its description records the current state. **Integration base:** `3812516dc` (the reviewed patch has no overlapping upstream changes). **Updated:** 2026-11-03.

## Acceptance found two distinct ownership defects

Cold native HLS started in 10.659 seconds despite a complete first segment after 0.801 seconds. The first public snapshot waited about seven seconds longer: its 16-second reload coverage was added to a consumed frontier advancing before the client could attach. Both the NAS and client file page caches were empty before the run. The reload coverage is an immutable HLS promise; early production and unpublished time must not create fictitious consumption.

Expired-session web Resume also retained a `media_waiting` fault over advancing video. An attached-media event inherited the pending seek's intent; ordinary presentation could then never retire it. The fault became blocking on pause. Native Safari's short retained-session Resume delivered a frame callback in 55 ms with 352 native deliveries during the bounded run; those observations do not establish long-duration acceptance or clear the expired-session defect.

## Corrections and ownership

| Work | Owner | State | Required invariant |
|---|---|---|---|
| Initial publication frontier and clock | Codex/root | Implemented | Requested position, not pre-attachment elapsed time, owns initial coverage. Keep complete cuts, immutable reload duration, exact-attempt admission and steady reserve. |
| Media-wait and destination-fault lifetime | Sol | Implemented | Attached presentation retires only attached waits; destination faults retire on actual destination proof or supersession. Predecessor movement cannot forgive an unresolved destination. |
| Regression evidence and main fast lane | Codex/root | Tracked in the PR | One completed adversarial review, fixes, then focused regressions and one fast-lane run. Retry only failed or invalidated checks. |
| Main merge and post-merge jobs | Codex/root | Tracked in the PR | Carry regression fields into landing commit; record observed post-merge checks. Maintainer owns deployment. |

## Decisions and limits

There are no new watchdogs, startup timeout increases, feature gates, or browser-route substitutions. The existing 16-second target duration cannot safely be lowered from a few observed opening GOPs: it also bounds later complete segments and determines native reload behavior. First-frame latency after this accounting repair requires deployed measurement; this ledger must not claim a two-second guarantee from unit or compile results.

## Validation and review receipts

Astra review preceded test execution. Pinned formatting passed. Initial bookkeeping passed 14 checks; its one lifecycle-header assertion passed on a focused retry after wording correction. The PR records final pinned compiler output and the 26-case Rust journal; its main fast lane owns the web regression IDs and retains successes across failed-only retries. Private runtime receipts retain measured startup/Resume and cleanup; committed documentation uses neutral infrastructure names.

## Implementation — the clock starts at its owner

The actor now retains the first accepted media-body commit time for each producer attempt. Init/metadata, publication and rejected predecessor commits cannot set it. Replacement resets it. Legacy publication adds elapsed time from this event to the requested position relative to media origin. An unpublished prefix has no elapsed consumption, including copy lead before a Resume destination. Explicit publication uses observed position without extrapolation until the actor has accepted post-publication presentation progress; after that the latest accepted observation owns extrapolation. The first snapshot continues to cover its immutable native reload window and every response retains normal init, complete-object and attempt admission. First-snapshot logs now name lease mode, demand sequence, presentation phase and media-commit state.

Web media waits now explicitly distinguish the attached element from an executed, unresolved seek on that same element and attachment. The actual settlement owner emits `intent_settled` after target proof (or established paused positioning), and the `media_waiting` source specifically retires on that event or supersession. Other buffering sources retain their existing contracts. Deduplication includes intent; paused and retired-element events cannot raise waits.

The regressions replay late waiting after the old early settlement event, predecessor movement, actual target proof, supersession and the expired-session Play/reopen seam. Publication regressions retain the 16-second target, reject insufficient post-position coverage despite seek lead, simulate seven seconds of unpublished time, distinguish observed position from presentation extrapolation, and test delivery timestamp ownership across replacement.

## Adversarial review — one fixture correction

Astra reviewed the full candidate `e66d81b2f` against `d367abf3d` before test execution. One P1 finding identified a new test fixture attempting producer replacement after publication through a constructor that correctly refuses `PlaylistPublished`. The correction splits published body-commit timing from an unpublished replacement fixture, then publishes the successor before accepting its media. The existing admission fence remains intact. No additional actionable product defect was found. The private review receipt records the exact reviewed source and this disposition; it is not runtime acceptance.
