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
| S01 · reproduce both defects | inspecting | Current `main` still calls `startLoad(position)` after a fatal unloaded manifest and still maps a successful playlist-only probe to `decoder_failed` | Retained vendored-library and shipped-executor regressions reject both old behaviors |
| S02 · bounded manifest recovery | not started | Current attachment and hls.js ownership paths identified | One attachment owns the absolute deadline, one shared corrective credit, at most 16 manifest sends, and complete cancellation |
| S03 · truthful diagnosis | not started | Current refusal capture and playback-surface fixtures identified | Playlist, media, presentation, decoder, authentication, and terminal causes remain distinct |
| S04 · server phase attribution | not started | `exact_hls_context` and its three publication callers identified | Pending, invalid, unsupported, unavailable, terminal, and authority outcomes map to typed fenced responses |
| S05 · qualification and promotion | deferred | No test result is claimed during implementation | One adversarial review, addressed findings, exact-head fast lane, then merge |

## Standing decisions — safety evidence advises but never gates

1. **No feature gate will be added.** Startup recovery is correctness work on
   the existing web HLS path, not an optional code path.
2. **Developer settings will explain enablement only if a user control is
   needed.** Any prerequisite reading will be met · not met · not observable
   and advisory; it will never override the saved choice.
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
| Adversarial implementation review | not run | Runs once after the complete candidate is frozen |
| Review corrections | not run | Every accepted finding will name its correcting commit |
| Fast lane | not run | Runs only after review corrections on the exact PR head |
| Main merge | not run | Merge is allowed only if the reviewed head and green fast-lane head are identical |
