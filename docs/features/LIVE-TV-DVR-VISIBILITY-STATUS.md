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
| S01 blank-list repair | built; final evidence pending | web, Apple and Android decode the real `{rows, next}` envelope; web preserves stale rows and exposes Load more; regression fixtures written but not run |
| S02 capture observation and overview | built; compile passed | successful-write sampling, five-second rate window, bounded peer observations, shared Activity/overview projection and `/api/v1/dvr/overview`; focused fixtures written but not run |
| S03 lifecycle ledger and attention | built; compile passed | append-only SQLite v59 / replicated v39 migrations; atomic and owner-fenced events, bounded pages/pruning, legacy provenance and per-user acknowledgments; no tests run yet |
| S04 web layouts and controller | built; commit checks pending | one scope-aware poller feeds chrome, Live TV and Recordings; exact-airing player context, canonical `#/recordings`, six task tabs, mobile detail, real event history and Activity projection |
| S05 Apple clients | queued | tolerant decoders, one foreground controller, Recordings navigation and focus-safe details |
| S06 Android clients | queued | tolerant decoders, lifecycle-aware controller, Recordings navigation and stable D-pad focus |
| S07 review and promotion | queued | merge current `main`, obtain one adversarial code review, address it, mark the draft PR ready and let the fast lane run once |

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

## Evidence limits — green source is not a hardware claim

The final fast lane can prove policy, static contracts and affected Rust,
web, Apple and Android compilation. It cannot prove tuner writes, physical
Siri Remote or Google TV focus, shared storage on every node, or two-client
convergence. Those observations remain explicit follow-up evidence; this
page will not turn a simulator, fixture or compiler result into a device
claim.
