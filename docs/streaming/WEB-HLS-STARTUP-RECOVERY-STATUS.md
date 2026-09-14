# Web HLS startup recovery — implementation status

**Status:** implementation in progress · **Updated:** 2026-09-14 · **Base:**
`11ca0573a33fe41214c7837690348d8290318127` · **Branch:**
`codex/web-hls-startup-recovery`

Companion to [PLAYBACK.md](../PLAYBACK.md) (the shipped delivery contract),
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) (the evidence vocabulary), and
the [playback lifecycle map](../playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md)
(recovery ownership) — this page records what has been built, reviewed,
qualified, and promoted for delayed-manifest startup recovery. The attached
implementation contract remains the design authority until it is committed
with the implementation.

The work uses an isolated Forgejo clone under `/private/tmp`; no existing
developer checkout supplies source or validation evidence. Rust work uses the
repository-pinned Rust 1.97.1 toolchain. Tests remain deferred until the
candidate has received its one adversarial implementation review and every
accepted finding is addressed; compile-only feedback may run earlier because
the contributor contract forbids using CI as a compiler.

## Delivery ledger — one substantial main-bound pull request

| Work order | State | Current evidence | Exit condition |
|---|---|---|---|
| S01 · reproduce both defects | review-corrected, untested | Shipped-helper coverage separates unloaded-manifest and established-stream recovery; actual vendored-loader coverage and a delayed-manifest shipped-page browser check are wired into the fast lane | Exact-head controller and browser evidence pass |
| S02 · bounded manifest recovery | review-corrected, untested | One attachment owns 40 s cold / 20 s seek time, one shared corrective credit, explicit stock manifest policy, a 16-send final boundary, pause/resume rules, final-send ownership, and loader destruction | Exact-head controller and policy contracts pass |
| S03 · truthful diagnosis | review-corrected, untested | Failure evidence is attachment/resource/ordinal-bound and size-limited; terminal bodies preempt every retry; manifest, media, decoder, and presentation evidence are separate and telemetry omits capability URLs | Generated surface, controller, and browser contracts pass |
| S04 · server phase attribution | review-corrected, compile-passed | Exact init inspection preserves pending, invalid, unsupported, unavailable, and producer/session terminal outcomes; storage uncertainty is distinct from absence; incomplete required HEVC records are invalid; the five-second outer bound remains | Focused HLS tests pass |
| S05 · qualification and promotion | fast lane pending | The single adversarial review is complete and all nine findings are addressed by `38b423c9` | Exact-head fast lane, then merge |

## Standing decisions — safety evidence advises but never gates

1. **No feature gate will be added.** Startup recovery is correctness work on
   the existing web HLS path, not an optional code path.
2. **Developer settings reports readiness without controlling it.** The new
   always-on recovery card reports the loader seam, bounded policy, init
   inspection, and presentation evidence. It has no switch and nothing reads
   the card to decide whether recovery runs.
3. **The existing recovery owner remains authoritative.** The loader adapter
   may admit, abort, and report transport work; it may not reopen sessions,
   select quality, or present an error on its own.
4. **The server's five-second publication authority bound remains fixed.**
   Cold readiness is recovered by the client budget, not by weakening owner
   fencing or guessing codec metadata.

## Qualification ledger — evidence belongs to the exact candidate

| Evidence | State | Receipt |
|---|---|---|
| Rust 1.97.1 compiler baseline | passed | `cargo check -p plurxd --all-targets --locked` on base `11ca0573`; one pre-existing `AttemptChild::new` dead-code warning |
| Adversarial implementation review | complete | One pass against `3c3687a1`: four P1 and five P2 findings; no second review requested or run |
| Review corrections | complete | `38b423c9` fixes terminal retry precedence, established retry ownership, target-position presentation proof, actual loader/browser coverage, resource-scoped evidence ordering, unavailable storage classification, required HEVC record classification, bounded typed bodies, and sanitized episode telemetry |
| Fast lane | not run | Runs only after review corrections on the exact PR head |
| Main merge | not run | Merge is allowed only if the reviewed head and green fast-lane head are identical |
