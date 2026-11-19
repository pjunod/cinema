# Native callback delivery — Fable review and dispositions

**Status:** findings addressed; implementation drafted, qualification pending · **Reviewed:** 2026-11-02 ·
**Runtime source:** `8a212be7021f` · **Review source:**
[NATIVE-CALLBACK-DELIVERY-RCA.md](NATIVE-CALLBACK-DELIVERY-RCA.md)

Companion to the [native callback RCA](NATIVE-CALLBACK-DELIVERY-RCA.md) and
[implementation status](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md). This file
records the review findings and what the implementation must carry. It does
not claim code, runtime acceptance or an upstream WebKit filing.

## Review evidence and root cause

Fable verified the packet's 24 checksums, seven imported documents against
`0a8fc8c626ed`, 47 source-excerpt ranges against `8a212be7021f`, and five
Node 22 reproductions against their recorded outputs. The `queuePlaybackFrame`
guard and `renewingSeek` path match the RCA. WebKit source at
`WebKit-7625.2.8.11.9` shows native gathering begins on empty-to-nonempty
request-list transition, stops when its final pending request is canceled, and
the one-shot AVFoundation observer can clear before returning without a frame.
The private live traces and measurements were accepted as reported; Fable did
not compile Rust claims or re-review M1/M3 proposal sections.

The root cause holds: a native frame request registered during positioning
remains pending, and `id != null` prevents renewal after the media position is
ready. The a5 cancel-to-empty and a6 rearm-only controls show cause and effect,
with the packet's disclosed changes preserved. The proposed owner cohort was
more complicated than the cause requires.

## Findings and dispositions

| Finding | Required disposition | State |
|---|---|---|
| P2-1 · Multi-owner cohort protocol | Replace independent native registrations with one per-element multiplexer, at most one native request, and fan-out to subscribers. Keep subscriber-local generation fences and proof floors. Incumbent and spare get separate multiplexers. Remove blocked-cohort, nonparticipating-owner and uncoordinated-listener outcomes and acceptance cells. | Revised RCA and implementation drafted; final review and qualification pending. |
| P2-2 · Progress evidence delays repair | Replace the old active repair gate (`seeking=false`, `readyState >= 3`, plus independently advancing clock/decoded observations) with event-driven renewal once per discontinuity: seek start, `emptied`/source attach, `loadeddata`; at first qualified readiness, active playback uses `!seeking && readyState >= 3`. Preserve a separate paused path. Keep clock/decoded evidence only for bounded observation-unavailable diagnosis. No timer. | Revised RCA and implementation drafted; final review and qualification pending. |
| P2-3 · Client rejects deferred companion | Acceptance includes both the client binding change that permits an initial family without the companion and client consumption of the versioned family update, as well as server deferral. | AC3 RCA and implementation drafted; final review and qualification pending. |
| P3-1 · MSE scope | Scope the renewal contract to native-src/AVFoundation. MSE remains unproven until an MSE seek acceptance cell is added. | Accepted; MSE behavior explicitly unknown. |
| P3-2 · Upstream WebKit report | Prepare a minimal bug report, but do not file it without explicit authorization for the external submission. Keep the application repair independent. | Prepared/unfiled; external filing not authorized. |
| P3-3 · Duplicate heavy-source rule | Consolidate `decode.rs::preferred_backend` and `transcode/mod.rs::heavy_source` into one shared source-complexity predicate, including cropped UHD dimensions and 3840×2076 Main10. | AC3 RCA and implementation drafted; final review and qualification pending. |
| P3-4 · Separate PR sequence | Fable's smallest-first separate-PR recommendation is superseded by the user's instruction to batch the playback repair work into one main PR. Preserve the proposed commit/work order inside that batch. | One batched PR planned; not opened until the main candidate is ready. |

## Implementation and qualification boundary

The implementation owner is Codex/root on `codex/playback-startup-fixes`, based
on `bd213c819`. The eight work lanes, owner, current state and qualification
sequence are tracked in the
[implementation status](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md). Implementation drafts are recorded in that ledger. This review disposition
does not claim a passing test, merged PR or runtime qualification.
Adversarial review belongs on the completed main merge candidate; then run the
fast lane and each requested test once against that exact candidate, retrying
only failed tests.
