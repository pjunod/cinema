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
| S01 · reproduce both defects | implemented, untested | Shipped-helper coverage drives unloaded-manifest and established-stream recovery separately; the contract asserts the vendored loader's final-send seam | Deferred fast lane rejects `startLoad` on an unloaded manifest and playlist-only decoder diagnosis |
| S02 · bounded manifest recovery | implemented, untested | One attachment owns 40 s cold / 20 s seek time, one shared corrective credit, explicit stock manifest policy, a 16-send final boundary, pause/resume rules, and loader destruction | Review accepts ownership/cancellation; deferred controller and policy contracts pass |
| S03 · truthful diagnosis | implemented, untested | Failure evidence is attachment/ordinal-bound; auth and typed terminal bodies preempt stock retry; manifest, media, and presentation exhaustion have different copy and the startup surface has only Close/Try again | Review accepts precedence; generated surface contract and browser contracts pass |
| S04 · server phase attribution | implemented, untested | Exact init inspection returns fenced `startup_timeout`, `hls_init_invalid`, `hls_init_unsupported`, or `init_inspection_unavailable`; producer/session terminal responses survive; five-second outer bound remains | Rust compile and deferred focused HLS tests pass |
| S05 · qualification and promotion | deferred | No test result is claimed during implementation | One adversarial review, addressed findings, exact-head fast lane, then merge |

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
| Adversarial implementation review | not run | Runs once after the complete candidate is frozen |
| Review corrections | not run | Every accepted finding will name its correcting commit |
| Fast lane | not run | Runs only after review corrections on the exact PR head |
| Main merge | not run | Merge is allowed only if the reviewed head and green fast-lane head are identical |
