# HEVC sample-entry admission — implementation status

**Status:** software candidate qualified locally; promotion gate pending · **Updated:** 2026-09-16 ·
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
| S01 · source fact and bounded recovery | focused evidence green | parser, SQLite/Hiqlite round trips, 257-row pagination, and exact stale-snapshot fencing passed | observe bounded production completion after server rollout |
| S02 · validated caps and shared admission | focused evidence green | nullable/empty/exact-list validation and packaging-only Auto/Original verdicts passed in the 59-test playback module | no software action pending |
| S03 · progressive web probes and downgrade protection | focused evidence green | 157 web-policy tests cover file-only tier/tag/PQ evidence, holes below ceilings, settled snapshots, and no downgrade | capture real Safari/Chrome answers when available |
| S04 · Apple serialization and propagation | compile and focused evidence green | build 165; iOS/tvOS compile; five focused iOS capability/request tests passed | physical iOS/tvOS playback remains pending |
| S05 · execution guard and refusal | focused evidence green | actual progressive tags, `requires_hls`, typed 409, cold-index policy, and pre-admission create refusal passed | physical minimal-hvcC delivery remains pending |
| S06 · documentation, evidence, review, and promotion | preflight correction ready | API, playback inventory, routing catalog, Developer advisory, qualification receipt, one adversarial review, and 280 focused checks are complete; the first remote preflight's missing history mappings and ownership count are corrected | require the rerun exact-head `Main promotion gate`, then merge the unchanged green head |

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
| S03/S05 compile and syntax | `cargo check -p plurxd --all-targets --locked`; `scripts/js-check crates/plurxd/src/web/index.html`; Node syntax checks passed |
| Apple build | build 165 claimed; `make apple-build` passed for iOS and tvOS after rerunning outside the filesystem sandbox required by SwiftUI macros |
| Unit and integration tests | one post-review focused window: 280 final checks passed; full suites intentionally deferred |
| Browser capability observations | pending; no real-browser claim |
| Physical Apple playback | pending; no device claim |
| Adversarial review | two findings; tier-complete progressive evidence and explicit copy-HLS transport admission fixed in `1092acda` |
| Focused runtime window | 280 final checks passed; exact commands and corrections are in the qualification receipt |
| Fast lane | first exact-head run stopped at historical-evidence preflight; corrective mappings, Apple anchor, and the reviewed 511 → 512 startup-task inventory are ready for rerun |

## Open acceptance — software first, devices honestly pending

The [software qualification receipt](../evidence/hevc-sample-entry-qualification-2026-09-16.md)
names exact commands, executed counts, Apple build results, the PR head/base,
F1–F10 disposition, and both follow-up safeguards. Safari, Chrome, physical
iOS, and physical tvOS results remain pending. Missing hardware evidence does
not become a fabricated pass and does not weaken the server-side safety
contract.
