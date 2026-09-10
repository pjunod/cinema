# Native Live TV layouts — implementation status and evidence

**Status:** implementation and one adversarial review complete; final verification pending · **Effort:**
`effort/live-tv-native-layouts` · **Base:** Forgejo `main` at `20057416` ·
**Updated:** 2026-09-10

Companion to
[LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) (the existing
guide and playback contract) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review fast
lane) — this is the short answer to *what has shipped in this effort, what is
being built, and what remains unproved*.

## Progress — three packages, then one promotion

| Package | State | Evidence | Remaining |
|---|---|---|---|
| Shared input, metadata, and preference contracts | built | bounded FFmpeg input parser/cache; cross-client DTO and badge cases; generated input tables; pinned Rust compile green | final single focused test run |
| Apple TV and iOS | built | all three saved layouts, anchored/pinned guide, scoped input, compact schedule/grid, Apple build 129; iOS + tvOS compile green | final focused tests; physical Siri Remote unavailable |
| Google TV and Android | built | all three saved layouts around one movable player, stable UTC-anchor D-pad navigation, false-on-delegate input, compact schedule/grid, Android build 79; Kotlin compile green | final focused tests; physical Google TV unavailable |
| Integrated promotion to `main` | review addressed | draft Forgejo PR [#229](http://192.168.4.7:3000/noirr/plurx/pulls/229); exactly one Astra adversarial review of head `f5e67fe9`; all findings addressed on the author branch; no feature gates; full suites reserved for the separate sweep | commit the review fixes, one focused test pass, ready + `fast-lane`, green promotion gate, merge |

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
| 2026-09-09 | implementation worktree | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 31.24s; retained metadata tests were compiled but not executed |
| 2026-09-09 | implementation worktree | `make apple-build` | iOS and tvOS compile-only builds passed; no simulator tests ran |
| 2026-09-09 | implementation worktree | `./gradlew --no-daemon :app:compileDebugKotlin` | passed in 20s; no JVM or instrumentation tests ran |
| 2026-09-09 | implementation worktree after D-pad wiring | `./gradlew --no-daemon :app:compileDebugKotlin` | passed in 9s; main sources only, with the retained real-KeyEvent instrumentation case neither compiled nor executed by this command |
| 2026-09-09 | pre-review integrated tree on current `main` | `rustup run 1.97.1 cargo fmt --all -- --check` and `cargo clippy -p plurxd --all-targets -- -D warnings` | passed; Clippy completed in 1m 13s |
| 2026-09-09 | pre-review integrated tree on current `main` | `make apple-build` | exact-tree iOS and tvOS compile-only builds passed in 13s |
| 2026-09-10 | post-review remediation worktree | `xcrun swiftc -parse clients/apple/Sources/*.swift` and `make apple-build` | Swift parse plus iOS and tvOS compile-only builds passed; no simulator tests ran |
| 2026-09-10 | post-review remediation worktree | `./gradlew :app:compileDebugKotlin` | Android main-source compile passed in 2s; no JVM or instrumentation tests ran |
| 2026-09-10 | post-review remediation worktree | pinned Rust `fmt --check`, `cargo check -p plurxd --all-targets`, and Clippy with denied warnings | passed; retained regression tests compiled but did not execute |

Compilation and static contracts are retained here as they pass. Physical
Apple TV/Siri Remote and Google TV/D-pad walkthroughs will be recorded with
the exact device and build; absence of either device will be recorded as a
limit, never replaced by a simulator claim.

## One adversarial review — disposition

The required single review ran in Astra task
`01a088d3-145d-7ea3-af07-4240406efcef` against draft head `f5e67fe9`. No
second review or approval pass will be requested. The author verified the
remediation through source inspection and compile/static checks; the final
focused tests remain deliberately batched into one run.

| Findings | Resolution |
|---|---|
| Web adapter used obsolete presentation names and retained stale merged facts | aligned it to the generated `browser` / `fullscreen_*` contract, delegated native activation, and made a missing status observation clear the previous one |
| Android could sleep during playback, leave fullscreen during a tune, lose the picture on an empty filter, or let delegated activity age out the controls | restored `keepScreenOn`, distinguished a busy tune from a completed stop, retained the movable player under Clear filters, and rearmed the idle timer for native focus/activation |
| TV guide reachability, paging, protected/empty focus, selected-programme details, and layout-switch restoration were incomplete | exposed Guide in every layout; added Earlier, Now, and Later over the fetched window; made protected headers and empty rows focusable but harmless; hoisted channel/programme/UTC-anchor state and restores it after composition |
| Apple mobile, settings, exit, and timer behavior had presentation gaps | added a no-guide Watch fallback, used one `@AppStorage` layout source for Settings and Live TV, switched the selected Home tab on Leave, and rearmed/suspended fullscreen hiding as focus and panels change |
| Expired measured source facts could survive in server or client snapshots | the session clears its cached lineup observation before applying a fresh session observation; web, Apple, and Android expire at 20 minutes or programme end and fall back to honest lineup facts |

The review's build-number question required no code change: **129** is the
Apple build counter. **227** is the Forgejo issue number recorded in the
build-note filename and its `Issue:` field.

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
