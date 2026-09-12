# Playback buffer observability — implementation status

**Status:** live rolling-status review addressed; fast lane pending · **Updated:**
2026-09-12 · **Promotion:**
[PR #263](http://192.168.4.7:3000/noirr/plurx/pulls/263), merge `eaecb199`

This is the live execution ledger for the additive playback buffer and delivery
instrumentation that follows the
[playback lifecycle implementation](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md). It
records what is built, what each measurement means, and which qualification
evidence exists. Unknown and stale observations are never recorded as zero.

## Progress

| Package | State | Current evidence | Next action |
|---|---|---|---|
| O01 measurement contract | complete | the generated cross-client field contract names source read, anchored server ready, server HTTP wait/delivery, client loaded media and presentation separately; missing readiness is `0.0 s` while unknown is `Unavailable` | none |
| O02 server publication | complete on `main` | rolling applies the achieved origin once and stops at pruned/gapped media; VOD begins at the accepted playhead/seek entry and requires init plus contiguous materialized entries; the bounded wait pool publishes per-session count, oldest age and segment | none |
| O03 Apple presentation | complete on `main` | Apple info/debug rows consume the shared fields; AVPlayer loaded ranges and film-clock age stay separate; visible waits name presentation, client-loaded media and server HTTP-wait state without inferring a cause | none |
| O04 Android presentation | complete on `main` | Media3 reports attached-player loaded runway and presentation age; server samples are identity-fenced and age-stamped; rows and visible waiting copy use the shared vocabulary | none |
| O05 web presentation | degraded on current `main` | the client rows are present, but live rolling status requests deterministically return `503 response_state_changed`; client-loaded and presentation facts remain visible while server readiness, HTTP wait and delivery facts remain unavailable | repair O07, qualify once, then verify a live remux sample |
| O06 promotion | complete | task PR [#264](http://192.168.4.7:3000/noirr/plurx/pulls/264) merged at `35d40a2d`; combined PR #263 passed review and run 1903, then merged to `main` at `eaecb199` | none |
| O07 live rolling-status repair | review addressed | [PR #270](http://192.168.4.7:3000/noirr/plurx/pulls/270) types `status` as exact-attempt `SessionStatus`; its one adversarial review found that the first regression fixture bypassed actor authorization and did not pin hold semantics, so the corrected HTTP proof now uses actor-managed transcode and remux sessions before media, while held, and after media, with mutation-sensitive actor snapshots unchanged | run the current-head fast lane, merge, then verify the deployed endpoint returns `200` |

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
| client task `6f800466` | `node tests/playback/web-policy.test.js`; `node tests/playback/player-input-contract.test.js`; `node tests/web/player-dom.test.js` | passed |
| client task `6f800466` | pinned Android JDK 25 / SDK 37: `:app:compileDebugKotlin`, `:app:assembleDebugAndroidTest`, and `:app:testDebugUnitTest --tests tv.plurx.app.player.PlaybackInfoContractTest --tests tv.plurx.app.player.PlaybackTelemetryTest` | passed; instrumentation sources compiled; focused JVM classes passed |
| client task `6f800466` | iOS simulator build plus the two new `AppleClientTests`; tvOS simulator build | passed; 2 tests, 0 failures; both Apple platforms compiled |
| client task `465a3aac` | Effort development gate [run 1895](http://192.168.4.7:3000/noirr/plurx/actions/runs/1877) | passed; policy preflight, Rust compile and aggregate gate green |
| effort merge `35d40a2d` | PR [#264](http://192.168.4.7:3000/noirr/plurx/pulls/264) | merged into `effort/buffer-observability` |
| combined candidate `83693106` | one adversarial review | four findings; all addressed before the final validation phase |
| exact head `91eb2546` | [main fast-lane run 1903](http://192.168.4.7:3000/noirr/plurx/actions/runs/1885) | passed; all eight jobs, including Android and `Main promotion gate`, succeeded |
| merge `eaecb199` | PR [#263](http://192.168.4.7:3000/noirr/plurx/pulls/263) | merged the unchanged qualified head to `main` |

## Decisions made without waiting

- This is ordinary observability and is visible through the existing stats
  preference. It needs no Developer capability switch or qualification gate.
- The existing two-second server status cadence and player progress sampling
  are the telemetry budget. The implementation will not add a recovery owner,
  watchdog, media-file scan, or independent polling loop.
- Apple TV presentation is the first UI priority, while labels and measurement
  meanings remain shared with Android and web.
- Focused regressions required by the repository workflow ran before task
  pushes. The full suites stayed deferred; broad validation was confined to
  the final post-review phase, with retries limited to demonstrated failures.
- The playback-rewrite session was the final integrator. It combined this
  additive effort before the one review and promoted one main candidate,
  avoiding competing instrumentation and rewrite PRs.
