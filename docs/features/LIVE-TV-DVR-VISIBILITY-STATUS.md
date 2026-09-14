# DVR visibility status — make capture state explain itself

**Status:** implementation in progress · **Branch:** `effort/dvr-visibility` ·
**Base:** Forgejo `main` at `28ae8163` · **Updated:** 2026-09-14

Companion to [LIVE-TV-DVR-STATUS.md](LIVE-TV-DVR-STATUS.md) (what the shipped
recorder already does),
[LIVE-TV-DVR-IMPLEMENTATION.md](LIVE-TV-DVR-IMPLEMENTATION.md) (the recorder
and scheduler contract), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review fast
lane) — this is the short answer to *what is being changed, what is already
proved, and what remains before the DVR visibility effort reaches `main`?*

## Outcome — recording evidence belongs wherever the programme appears

The effort makes a recording visible in Live TV, Activity and a permanent
Recordings destination on web, Apple and Android. Labels follow recorder
evidence: a request is not a write, a running clock is not saved media, and
an unreachable owner is `Status unavailable`, never a false zero.

The existing recorder, scheduler, household permissions, reminder controls,
shared channel transports and finite-media playback path stay in place. The
new work adds bounded observation, durable lifecycle history and one shared
presentation contract around them.

## Progress — one integrated candidate, one promotion run

| Package | State | Evidence or next boundary |
|---|---|---|
| S00 isolated base and compiler | complete | independent clone at `28ae8163`; pinned Rust 1.97.1 workspace check passed before Rust edits |
| S01 blank-list repair | built; final evidence pending | web, Apple and Android decode the real `{rows, next}` envelope, preserve stale rows and expose independent Load more controls; regression fixtures written but not run |
| S02 capture observation and overview | built; compile passed | successful-write sampling, five-second rate window, bounded peer observations, shared Activity/overview projection and `/api/v1/dvr/overview`; focused fixtures written but not run |
| S03 lifecycle ledger and attention | built; compile passed | append-only SQLite v59 / replicated v39 migrations; atomic and owner-fenced events, bounded pages/pruning, recovery-gap provenance, legacy outcomes, per-user acknowledgments and section/watermark-stable attention traversal; no tests run yet |
| S04 web layouts and controller | built; compile passed | one scope-aware poller feeds chrome, Live TV and Recordings; exact-airing player context, canonical `#/recordings`, six task tabs, mobile detail, real event history and Activity projection |
| S05 Apple clients | built; source type-check passed | additive overview/event/attention decoders; one foreground profile controller; iOS/tvOS Recordings root, capture activity, immediate details, history, manual/skipped/rules/reminders and exact-airing player/guide context; device builds remain pending because this host exposes no simulator runtime |
| S06 Android clients | built; compile passed | additive overview/event/attention decoders; one lifecycle-owned profile controller; phone/Google TV Recordings root, capture activity, immediate detail/history, manual/skipped/rules/reminders, exact-airing context and named stop/delete confirmations; physical D-pad evidence remains pending |
| S07 review and promotion | ready to start | integration audit complete; merge current `main`, obtain one adversarial code review, address it, mark the draft PR ready and let the fast lane run once |

## Decisions made while implementing

1. **Use one effort branch and one main-bound PR.** The user asked for larger
   batches and one final review/test cycle, so reviewable packages land as
   normal commits on `effort/dvr-visibility`; there are no per-package CI
   runs.
2. **Treat readiness as advice.** DVR remains a plain enable setting. The
   Developer settings section explains each prerequisite and whether this
   process can observe it, but no readiness result blocks enablement.
3. **Use the current workflow, not the superseded label instruction.** Since
   2026-09-13, marking a main-bound draft ready starts `main-fast-lane.yml`;
   the removed `fast-lane` label cannot be applied.
4. **Reserve tests for the frozen candidate.** Compile-only feedback may run
   while building because repository policy forbids using CI as a compiler.
   Focused and fast-lane tests run only after the adversarial review is
   addressed.
5. **Keep the television manual timer operable without a date picker.** tvOS
   does not ship SwiftUI's `DatePicker`, so its existing channel-and-time task
   uses explicit start-delay and duration choices; iOS keeps exact date/time
   selection. Both submit the same server contract.
6. **Use one bounded schedule window on every new surface.** Upcoming defaults
   to the twelve hours before and after now, caps the window at 24 hours, and
   owns a cursor separate from Saved and Needs attention. The legacy 14-day
   query remains compatible for older clients.

## Current compile evidence — no test lane spent

- `rustup run 1.97.1 cargo check -p plurxd --all-targets` passes on the
  integrated Rust tree.
- Android `:app:compileDebugKotlin` passes; its two volume-icon deprecation
  warnings predate this effort.
- Direct Swift type-checks pass for both `arm64-apple-ios17.0` and
  `arm64-apple-tvos17.0`. The full Xcode build still stops in asset compilation
  because this host has no simulator runtimes; no Swift error was emitted.
- `scripts/js-check` accepts both shipped inline script blocks.

No unit or UI test command has run. The shared web/Apple/Android fixture and
focused Rust cases remain deliberately unexecuted until the one adversarial
review is addressed, as requested.

## Evidence limits — green source is not a hardware claim

The final fast lane can prove policy, static contracts and affected Rust,
web, Apple and Android compilation. It cannot prove tuner writes, physical
Siri Remote or Google TV focus, shared storage on every node, or two-client
convergence. Those observations remain explicit follow-up evidence; this
page will not turn a simulator, fixture or compiler result into a device
claim.
