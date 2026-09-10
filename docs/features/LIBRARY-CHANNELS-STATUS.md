# Library channels status — what is built and what remains

**Status:** M0 complete · implementation in progress · **Effort:**
`effort/library-channels` · **Updated:** 2026-09-09 · **Base:** `4cef0da7`

Companion to [FEATURES.md](../FEATURES.md) (what Plurx supports),
[PLAYBACK.md](../PLAYBACK.md) (finite-media delivery), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) — this is the short answer to *where is Library channels, what is
proved, and what remains?*

## Progress — one frozen candidate will receive the expensive gate

| Milestone | State | Evidence |
|---|---|---|
| M0 isolated base and compiler | complete | clean independent clone at `4cef0da7`; Rust 1.97.1 baseline `cargo check -p plurxd --all-targets` passed in 1m14s |
| M1 recipes, schedules, and durable storage | built; integration compile passed | one normalized evaluator and deterministic clock/order implementation; SQLite v55 and Hiqlite v35 entities, authorization-at-write, idempotent definitions, bounded staging, guarded publication, catalogue projection, and import census |
| M2 API and playback purpose | built; integration compile passed | bounded authenticated CRUD/preview/guide/resolve routes, pinned-occurrence finite-HLS starts, durable purpose binding, and following-mode start/history isolation |
| M3 web | built; integration compile passed | responsive browse/guide, three-step resumable editor, preview and management actions, fenced following playback, watch-from-start/return, and advisory Developer enablement |
| M4 Apple | built; iOS and tvOS compile passed | native now/next guide and adaptive phone/tablet/television layout; iPhone/iPad preview and authoring; fenced finite-HLS following with pause-boundary rejoin and ordinary watch-from-start; build 129 |
| M5 Android | built; Android compile passed | native now/next guide; phone/tablet authoring; adaptive Google TV layout; fenced finite-HLS following with pause-boundary rejoin and ordinary watch-from-start; versionCode 79 |
| M6 promotion | queued | documentation, one adversarial review, review fixes, fast lane, and merge |

## Current decision — the merged Live TV guide is the UI seam

Forgejo `main` already contains the 2026-09-08 Live TV guide and client
layout work. Library channels will reuse its guide presentation and preserve
its tuner controller as a separate source adapter. There is no parallel
layout implementation and no combined tuner/library transport.

The host default is Rust 1.98.0, but Rust 1.97.1 is installed through
`rustup`. Every Rust compile command for this effort explicitly uses the
pinned toolchain so Homebrew's default cannot silently become evidence.

## Enablement — explicit and advisory

Library channels are compiled into every ordinary build. An administrator
gets an enable control in Settings → Developer together with live readiness
facts: authoritative store availability, catalogue media with a successful
video probe, and client/server compatibility. Those facts explain likely
failure; they never override the administrator's choice or hide the feature.

The switch controls new resolve/session admission, not discovery or authoring.
This keeps the empty page and editor available while an administrator inspects
or repairs readiness, and it preserves a disabled channel definition without
pretending the readiness verdict has authority over the explicit switch.

## Promotion rule — review once, qualify once

Implementation commits accumulate on `effort/library-channels`. Only the
complete main-bound draft receives one adversarial agent review. The author
addresses that review without requesting a second pass, marks the pull request
ready, applies `fast-lane`, and merges only when the current head has a green
Main promotion gate. Full unit and device sweeps remain owned by the separate
test-maintenance process.

## Known limits — do not read queued work as shipped behavior

- No implementation milestone after M0 is claimed until its code is committed.
- Physical two-device and television-input observations require the named
  devices and are recorded honestly if unavailable during this effort.
- Library channels never gate on a tuner, remote metadata service, AI key, or
  readiness verdict; only ordinary authentication and media permissions apply.
