# Web HLS startup recovery — implementation status

**Status:** locally qualified · hosted promotion queued · **Updated:** 2026-09-14 · **Base:**
`28ae8163b52545730f4c65916bb2c8757eabb993` · **Branch:**
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
| S01 · reproduce both defects | qualified | Shipped-helper coverage separates unloaded-manifest and established-stream recovery; actual vendored-loader coverage and the delayed-manifest shipped-page browser check pass in the fast lane | Complete |
| S02 · bounded manifest recovery | qualified | One attachment owns 40 s cold / 20 s seek time, one shared corrective credit, explicit stock manifest policy, a 16-send final boundary, pause/resume rules, final-send ownership, and loader destruction | Complete |
| S03 · truthful diagnosis | qualified | Failure evidence is attachment/resource/ordinal-bound and size-limited; terminal bodies preempt every retry; manifest, media, decoder, and presentation evidence are separate and telemetry omits capability URLs | Complete |
| S04 · server phase attribution | qualified | Exact init inspection preserves pending, invalid, unsupported, unavailable, and producer/session terminal outcomes; storage uncertainty is distinct from absence; incomplete required HEVC records are invalid; the five-second outer bound remains | Complete |
| S05 · qualification and promotion | hosted gate queued | The single adversarial review is complete, all nine findings are addressed, current main is integrated, and the local fast lane is green on code-and-test candidate `d226f44d`; PR #311 is ready | Pass the exact status-only descendant through the Main promotion gate, then merge |

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
| Rust 1.97.1 compiler and lint | passed | Pinned `cargo check -p plurxd --all-targets --locked` and `cargo clippy -p plurxd --all-targets --locked -- -D warnings`; formatting also passes |
| Adversarial implementation review | complete | One pass against `3c3687a1`: four P1 and five P2 findings; no second review requested or run |
| Review corrections | complete | `38b423c9` through `daf858c6` fix terminal retry precedence, established retry ownership, target-position presentation proof, actual loader/browser coverage, resource-scoped evidence ordering, unavailable storage classification, required HEVC record classification, bounded typed bodies, sanitized episode telemetry, and exact response-delivery settlement |
| Focused Rust regressions | passed | `cargo test -p plurxd --bin plurxd web_hls_startup_ --locked`: 3 passed, 0 failed, 2,232 filtered |
| Web fast lane | passed | `make web-check` on merged candidate `d226f44d`: shipped policy/controller contracts, 26/26 Settings sections, 63/63 surface cases, 484 contrast pairs with no new failures, and actual vendored hls.js playback in Headless Chrome 152 |
| Browser recovery receipt | passed | Eight typed manifest refusals, 9 manifest requests total, first observed position 0.758 s; fixture SHA-256 `a708543b1bb974a7cbb2889e9039c79eba95e44b5551354bbec5b7faf5ff4915`; hls.js 1.6.16 |
| Policy and operations | passed | History and validation catalogs clean; 198 validation tests (1 skipped), 356 operations tests, and 4 documentation-index tests pass |
| Main promotion gate | queued | PR #311 is ready; its next status-only synchronization starts the exact-head hosted Forgejo receipt |
| Main merge | pending | Merge is allowed only after that exact hosted head is green |
