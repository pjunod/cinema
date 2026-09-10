# Native Live TV layouts — implementation status and evidence

**Status:** building shared contracts and source metadata · **Effort:**
`effort/live-tv-native-layouts` · **Base:** Forgejo `main` at `4cef0da7` ·
**Updated:** 2026-09-09

Companion to
[LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) (the existing
guide and playback contract) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review fast
lane) — this is the short answer to *what has shipped in this effort, what is
being built, and what remains unproved*.

## Progress — three packages, then one promotion

| Package | State | Evidence | Remaining |
|---|---|---|---|
| Shared input, metadata, and preference contracts | building | clean agent clone at current `main`; pinned Rust 1.97.1 compile loop established | source-format observation, cross-client DTOs/badges, generated input contract, focused regressions |
| Apple TV and iOS | queued | existing player, guide reducer, and settings seams identified | three tvOS layouts, persistent player host, scoped remote input, compact touch layouts, Apple build counter and parity record |
| Google TV and Android | queued | existing Media3 player, guide reducer, and settings seams identified | three TV layouts, exact-once D-pad routing, compact touch layouts, Android build counter and parity record |
| Integrated promotion to `main` | queued | one effort branch; no feature gates; full suites reserved for the separate sweep | merge current `main`, exact-head compilation, one Astra adversarial review, address findings, ready + `fast-lane`, green promotion gate, merge |

## Contract — presentation moves, playback does not

The effort ships `Guide + preview`, `Guide over picture`, and `Channel
browser` together on both television clients. The default is `Guide +
preview`; the choice saves locally per device and never starts, stops, or
retunes the current session. A temporary guide over fullscreen also leaves
the saved layout unchanged.

Phone and tablet clients keep their existing `On now`, `Guide`, and
`Favorites` choices. They receive a compact player and browsing treatment,
not the television layout selector. The existing Live TV Developer switch
and its met/unmet readiness reasons remain the sole runtime enable surface.
Readiness is advisory: no build flag, device allowlist, rollout gate, or
hidden eligibility test is added.

## Evidence — claims are attached to exact trees

| Date | Tree | Check | Result |
|---|---|---|---|
| 2026-09-09 | `main` at `4cef0da7` | `rustup run 1.97.1 rustc --version` | pinned compiler available: `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| 2026-09-09 | `main` at `4cef0da7` | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 1m 22s as the pre-edit compiler baseline |
| 2026-09-09 | initial status-page change | `python3 -m unittest tests.operations.test_docs_index` | 4 passed in 0.925s before the instruction to reserve all further tests for the final integrated candidate; this check will not be repeated during implementation |

Compilation and static contracts are retained here as they pass. Physical
Apple TV/Siri Remote and Google TV/D-pad walkthroughs will be recorded with
the exact device and build; absence of either device will be recorded as a
limit, never replaced by a simulator claim.

## Decisions made while Paul is away

| Decision | Why | Revisit when |
|---|---|---|
| Keep the approved package order on one effort branch | metadata and input contracts are the seam both native implementations consume; one integrated branch minimizes promotion overhead | a package requires an independently releasable server compatibility step |
| Use the existing runtime Live TV enable control only | it already exposes configuration and readiness; another activation mechanism would be a hidden gate by a different name | never, unless the product-level enable contract changes explicitly |

## Non-goals — this effort stays bounded

No DVR, recording, rewind, scheduled tuning, new tuner provider, new decoder,
4K delivery promise, surround passthrough promise, app-wide navigation
redesign, theme engine, or background scan is part of this work. Source facts
describe what the tuner delivered; they do not claim the player preserved
that format after transcoding.
