# Playback buffer observability — implementation status

**Status:** implementation in progress · **Updated:** 2026-09-12 · **Base:**
`10f2afe6` · **Effort:** `effort/buffer-observability`

This is the live execution ledger for the additive playback buffer and delivery
instrumentation that follows the
[playback lifecycle implementation](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md). It
records what is built, what each measurement means, and which qualification
evidence exists. Unknown and stale observations are never recorded as zero.

## Progress

| Package | State | Current evidence | Next action |
|---|---|---|---|
| O01 measurement contract | decided | Astra confirmed the five-stage model and the authoritative server-ready definition; no separate implementation document is pending | encode the shared field contract and status types |
| O02 server publication | complete on task branch | rolling applies the achieved origin once and stops at pruned/gapped media; VOD begins at the accepted playhead/seek entry and requires init plus contiguous materialized entries; the bounded wait pool publishes per-session count, oldest age and segment | merge `codex/buffer-server-telemetry` into the effort after its development gate |
| O03 Apple presentation | pending | AVPlayer contiguous loaded-range and existing two-second status cadence identified | add Apple TV-first Buffering / Delivery rows and waiting copy |
| O04 Android presentation | pending | Media3 buffered position, status cadence and playback-info renderer identified | add the shared rows, stale identity fence and waiting copy |
| O05 web presentation | pending | browser buffered ranges, playback-quality clock and fenced status poll identified | add the shared rows, sample ages and waiting copy |
| O06 promotion | pending | independent Forgejo clone and effort branch published | merge reviewable tasks into the effort, freeze, integrate current main, review once, run fast lane, merge |

## Measurement contract

The normal stats/info surface uses these separate facts:

1. **Source read:** measured source availability or read activity only. A
   configured input pace is policy, not source activity; unsupported readings
   stay unavailable.
2. **Server ready:** contiguous complete media that the active recipe and
   generation can read, beginning at the latest accepted absolute film-time
   playhead or pending seek target. Coverage must contain that anchor. A known
   missing anchored segment is zero; evicted or unobservable coverage is
   unavailable. A later ready island is debug context, not runway at the
   playhead.
3. **HTTP delivery:** server-observed completed response bytes/rate and active
   server wait. Response completion is not proof that the client received,
   demuxed, decoded, or retained the media.
4. **Client loaded:** the contiguous native/browser loaded range ahead of the
   current attached playhead. Prepared-successor ranges never enter the current
   player's number.
5. **Presentation:** player clock/frame progress and the age of its last
   advance. Loaded media alone never attributes why presentation stopped.

Production actual, production target, paced/running state and resource wait
remain separate producer facts. The target is not displayed or interpreted as
a buffer. Rolling readiness comes from complete published segments plus the
retained window, with `media_origin_ms` applied once. VOD readiness comes from
bounded manifest metadata beginning at the entry containing the accepted
anchor; global completion, planned entries and non-contiguous cache islands do
not count.

## Qualification ledger

| Candidate | Command or gate | Result |
|---|---|---|
| `10f2afe6` | `rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| `10f2afe6` | `cargo --version` | `cargo 1.97.1 (c980f4866 2026-06-30)` |
| server task | `cargo fmt --all -- --check`; `cargo check -p plurxd --locked --all-targets`; `cargo clippy -p plurxd --locked --all-targets -- -D warnings` | passed |
| server task | three exact Rust regressions: origin/retention readiness, wait-pool observation, VOD disjoint far-seek readiness | 3 passed; 0 failed |
| remaining task branches | focused regressions and affected compilation | not run |
| effort candidate | Effort development gate | not run |
| current-main candidate | one adversarial review | not requested |
| reviewed candidate | fast lane / Main promotion gate | not run |

## Decisions made without waiting

- This is ordinary observability and is visible through the existing stats
  preference. It needs no Developer capability switch or qualification gate.
- The existing two-second server status cadence and player progress sampling
  are the telemetry budget. The implementation will not add a recovery owner,
  watchdog, media-file scan, or independent polling loop.
- Apple TV presentation is the first UI priority, while labels and measurement
  meanings remain shared with Android and web.
- Focused regressions required by the repository workflow run before task
  pushes. The full suites stay deferred; the fast lane runs once, after the
  single adversarial review is addressed on the final candidate.
