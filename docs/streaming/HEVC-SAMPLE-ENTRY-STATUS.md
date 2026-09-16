# HEVC sample-entry admission — implementation status

**Status:** implementation in progress · **Updated:** 2026-09-16 ·
**Base:** `df3721320a8efe5967d95ce331cc30ccf6f0e3ea` · **Branch:**
`codex/hevc-sample-entry`

Companion to the externally supplied HEVC sample-entry implementation contract
— this is the live execution ledger for source-tag recovery, capability
validation, playback admission, compatible delivery, client propagation, and
promotion. A compile result is not recorded as a test result, and physical
browser or Apple evidence remains pending until it is actually observed.

## Work packages — one coherent promotion

| Package | State | Current evidence | Next action |
|---|---|---|---|
| S01 · source fact and bounded recovery | source complete; compiled | one normalized first-playable-video parser; SQLite and Hiqlite schema/write/read/import/publication paths; separately leased 256-row stored-probe backfill with exact source/probe fencing | retain the written regressions for the final focused run |
| S02 · validated caps and shared admission | source complete; compiled | nullable/empty/exact-list semantics flow through caps and profile; invalid present claims receive typed `invalid_capabilities` before fallback; packaging-only mismatch selects Remux with truthful reasons | retain the written decision and HTTP regressions for the final focused run |
| S03 · progressive web probes and downgrade protection | not started | existing HEVC probes and POST-to-GET fallback identified | implement after the server wire contract is stable |
| S04 · Apple serialization and propagation | not started | Apple capability and request surfaces identified | claim one build and compile iOS/tvOS after request propagation is complete |
| S05 · execution guard and refusal | not started | segmented and progressive copy builders remain separate | verify the actual output tag, then add `requires_hls` and typed refusal |
| S06 · documentation, evidence, review, and promotion | not started | this ledger is indexed with the implementation | update API/playback docs, obtain one adversarial review, run the fast lane once, then merge a green unchanged head |

## Current decisions and boundaries

1. **Build from a separate clone.** The original checkout was 488 commits
   behind and dirty; no file from that checkout is edited or swept into this
   branch.
2. **Preserve the reviewed design.** The relevant diff from review base
   `3129ce993` to current `main` contains sanitization-only changes, so no
   source, capability, or delivery contract needs substitution.
3. **Do not make compatibility optional.** The repair activates only when a
   client sends the explicit additive capability field. Developer settings
   will describe rollout readiness as advice; it will not provide a switch
   that can disable source admission or transport safety.
4. **Defer runtime suites to the frozen candidate.** Pinned compile, format,
   lint, and syntax checks may run while building. Focused and fast-lane tests
   run after the one adversarial review, immediately before promotion.
5. **Roll servers before clients.** Old servers can silently ignore an unknown
   v2 field. Every serving node must enforce the field before web refresh or
   Apple publication.

## Evidence ledger — claims stay literal

| Evidence | Result |
|---|---|
| Clean Forgejo base | `df3721320a8efe5967d95ce331cc30ccf6f0e3ea` |
| Reviewed source anchor | `3129ce993`; relevant current-main changes are cosmetic only |
| Rust compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| S01/S02 compile | `cargo check -p plurx-core --all-targets --locked`; Hiqlite feature compile; `cargo check -p plurxd --all-targets --locked` passed |
| Unit and integration tests | deferred; not run |
| Browser capability observations | pending; no real-browser claim |
| Physical Apple playback | pending; no device claim |
| Adversarial review | pending until the merge candidate is frozen |
| Fast lane | pending until review findings are addressed |

## Open acceptance — software first, devices honestly pending

The final software receipt must name exact commands, executed counts, Apple
build results, the PR head/base, F1–F10 disposition, and both follow-up
safeguards. Safari, Chrome, physical iOS, and physical tvOS results will be
recorded only if those environments are available. Missing hardware evidence
does not become a fabricated pass and does not weaken the server-side safety
contract.
