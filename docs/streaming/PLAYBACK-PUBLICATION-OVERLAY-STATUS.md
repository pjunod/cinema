# Playback publication and overlays — root-cause repair ledger

Companion to [the startup implementation ledger](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md) and [the playback surface contract](../clients/PLAYBACK-SURFACE-CONTRACT.md) — this records the deployed acceptance failures, their corrections, and the PR's progress toward merge.

**Status:** Implementation in progress. **Owner:** Codex/root; Sol owns the web lifecycle patch. **Branch:** `codex/playback-publication-overlay`. **PR:** Not opened yet. **Base:** `d367abf3d`. **Updated:** 2026-11-03.

## Acceptance found two distinct ownership defects

Cold native HLS started in 10.659 seconds despite a complete first segment after 0.801 seconds. The first public snapshot waited about seven seconds longer: its 16-second reload coverage was added to a consumed frontier advancing before the client could attach. Both the NAS and client file page caches were empty before the run. The reload coverage is an immutable HLS promise; early production and unpublished time must not create fictitious consumption.

Expired-session web Resume also retained a `media_waiting` fault over advancing video. An attached-media event inherited the pending seek's intent; ordinary presentation could then never retire it. The fault became blocking on pause. Native Safari's short retained-session Resume delivered a frame callback in 55 ms with 352 native deliveries during the bounded run; those observations do not establish long-duration acceptance or clear the expired-session defect.

## Corrections and ownership

| Work | Owner | State | Required invariant |
|---|---|---|---|
| Initial publication frontier and clock | Codex/root | In progress | Requested position, not pre-attachment elapsed time, owns initial coverage. Keep complete cuts, immutable reload duration, exact-attempt admission and steady reserve. |
| Media-wait and destination-fault lifetime | Sol | In progress | Attached presentation retires only attached waits; destination faults retire on actual destination proof or supersession. Predecessor movement cannot forgive an unresolved destination. |
| Regression evidence and main fast lane | Codex/root | Pending review | One completed adversarial review, fixes, then focused regressions and one fast-lane run. Retry only failed or invalidated checks. |
| Main merge and post-merge jobs | Codex/root | Pending | Carry regression fields into landing commit; record observed post-merge checks. Maintainer owns deployment. |

## Decisions and limits

There are no new watchdogs, startup timeout increases, feature gates, or browser-route substitutions. The existing 16-second target duration cannot safely be lowered from a few observed opening GOPs: it also bounds later complete segments and determines native reload behavior. First-frame latency after this accounting repair requires deployed measurement; this ledger must not claim a two-second guarantee from unit or compile results.

## Validation and review receipts

Not run yet. Tests are deferred until the complete candidate has had its adversarial review. Static syntax and pinned compiler checks may run during implementation. Private runtime receipts retain the measured sequence and cleanup; committed documentation uses neutral infrastructure names.
