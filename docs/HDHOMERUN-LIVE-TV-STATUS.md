# HDHomeRun Live TV status — what is built and what is proved

**Status:** backend, web, Apple and Android merged; M5 acceptance scripts and documentation merged; **the hardware pass has not run** · **Effort:** `effort/hdhomerun-live-tv` ·
**Updated:** 2026-09-05 · **Historical issue:**
[#902](https://github.com/pjunod/plurx/issues/902)

Companion to [HDHOMERUN-LIVE-TV-PLAN.md](HDHOMERUN-LIVE-TV-PLAN.md) (the
contract and ordered work) and [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md)
(the task-PR and final qualification rules) — this is the one-page answer to
*where is the effort, what passed, and what decision was made without Paul?*

## Progress — task PRs earn their checkmark

| Milestone | State | PR | Evidence |
|---|---|---|---|
| M0 plan and adversarial review | merged | [#904](https://github.com/pjunod/plurx/pull/904) | review approved; effort gate passed; merge `e3f05f2e` |
| M1 device/settings/lineup | merged | [#906](https://github.com/pjunod/plurx/pull/906) | attack-review findings fixed; re-review approved; effort gate passed; merge `b15d241a` |
| Forgejo main synchronization | merged | [#7](http://192.168.4.7:3000/noirr/plurx/pulls/7) | Astra approved `c4b66e1d`; fresh gate run 61 passed; merge `e8095916` |
| M2 live HLS and cluster relay | merged | [#13](http://192.168.4.7:3000/noirr/plurx/pulls/13) | Astra approved `fc164045`; exact pinned fmt/check/clippy and 47 focused tests passed; effort gate run 85 passed; merge `f2773ca8` |
| M3 web client | reviewed; merged with M4 Apple; effort PR pending | — | 18 focused tests; `70607d14` passed pinned static checks, decoded H.264/AAC media acceptance, and 72 structural captures against the committed baseline |
| M4 Apple | merged | [#17](http://192.168.4.7:3000/noirr/plurx/pulls/17) | Astra approved `8cfe8cc5`; iOS/tvOS compile and 14 focused tests pass on each simulator; effort gate 95 passed; merge `fc9c8f45` |
| M4 Android | reviewed; effort PR open and green | [#23](http://192.168.4.7:3000/noirr/plurx/pulls/23) | dedicated live player and Developer controls; 15 focused tests and both instrumentation tests pass on an API 36 phone emulator; exact JDK 25 / SDK 37 build passes; adversarial review closed a shared critical double-allocation path. Television instrumentation is still outstanding |
| M5 endless-source acceptance | merged | [#28](http://192.168.4.7:3000/noirr/plurx/pulls/28) | `scripts/live-tv-endless` drives ten real window rollovers against an endless FFmpeg source: one tuner GET, window never past six, 29.9 MB peak scratch, orphan expiry in 65 s, zero bytes left |
| M5 carried-forward review closeout | merged | [#30](http://192.168.4.7:3000/noirr/plurx/pulls/30) | 25 web tests, 16 Android unit tests, 4 instrumentation tests (2 `LiveTvUiTest`, 2 `LiveTvFileBarrierStoreTest`) pass |
| M5 two-node acceptance | merged | [#31](http://192.168.4.7:3000/noirr/plurx/pulls/31) | three cases written and compiled clean behind `cluster-integration-tests`; `make live-tv-two-node-check`. **Never executed** — no reachable host can bind port 80 for the fixture device and run two daemons |
| M5 hardware acceptance | merged; never pointed at a device | [#32](http://192.168.4.7:3000/noirr/plurx/pulls/32) | `scripts/live-tv-hardware` plus `make live-tv-hardware-check DEVICE=<ipv4>`; argument, address, bound and unreachable-device paths exercised. **The hardware path has never run** |
| M5 documentation | merged | [#33](http://192.168.4.7:3000/noirr/plurx/pulls/33) | Live TV now appears in FEATURES §4a, ROADMAP, ARCHITECTURE §3a, PLAYBACK, OPERATIONS (runbook + problems + ports), CHEATSHEET §5, SECURITY, both client-parity docs, REQUIREMENTS §3a and the README, whose live-TV non-goal is narrowed to "no DVR" rather than deleted |
| M5 promotion | blocked on hardware | — | the household numbers the docs are waiting for come from `make live-tv-hardware-check`, and it has not run |

## Current work — finish client acceptance and effort gates

M0 and M1 are merged to the effort. M2 now has the always-compiled foreground
admission path, one-open tuner-to-FFmpeg pump, bounded six-segment live HLS
scratch, owner-bound capabilities, provisional-start activation, signed
cluster relay/control, serving and settings-generation fences, idle/progress
timeouts, activity, and metrics. Playback remains runtime-disabled until an
administrator follows the safety checklist in Settings → Developer; there is
no Cargo feature or build variant to unlock.

The first exact-diff adversarial review rejected M2 with 11 findings. The
working fix now reserves Live TV's scratch namespace from the finite-media
sweeper, gives Live TV restart/shutdown/owner-transition drains a confirmed
physical-cleanup boundary, limits tuner creation to committed voters, binds
activation to its source and serving generation, publishes playlist bytes and
their segment inventory atomically, bounds local and relayed resource reads,
sanitizes transport logs, corrects metrics, and deinterlaces only interlaced
frames. A new loopback fixture proves one tuner GET through HLS publication,
activation, byte serving, and process/socket/scratch/registry cleanup.

The second M2 review and combined-effort review requested changes. Work now
covers monotonic signed drain proofs, recovery when the prior owner is lost,
cancellation during startup, confirmed cleanup failures, bounded file streaming,
expiring terminal errors, truthful metrics, cluster Activity, and stronger
behavioral tests. These changes passed focused qualification. An explicitly
selected Astra agent independently reviewed the owner-transition design.
The earlier review labelled “Astra” selected a
reviewer role without changing its model; it is retained as an additional
review, not evidence that Astra ran.

The actual Astra whole-M1/M2 review found three further issues: an in-flight
start could cross shutdown, owner control responses were not authenticated,
and unsupported source decoders lacked a typed error. The working fix closes
registry insertion permanently at shutdown, authenticates bounded owner
control responses (including playlists), constructs activation URLs locally,
and recognizes missing-decoder diagnostics in bounded stderr capture.
Candidate `4baf64bb` passed 41 Live TV tests on pinned Rust 1.97.1, including
the real video-only MPEG-TS regression with FFmpeg 5.1.9. The signature-tampering
test passed with the `hiqlite-store` feature; two route-matrix and two live
admission regressions passed. Pinned formatting, workspace check, denied-lint
Clippy, and no-default daemon compilation passed. Astra approved this exact
candidate and tree `a0946a83561816923f74d4efd5f5245f3e9daacf` with no findings.
This is focused task evidence, not final release qualification.

Commit `3945a713` contains the owner/recovery/cleanup hardening. Astra's closure
review accepted those findings and identified optional audio as a final gap:
the first profile now requires detectable video and audio. A new real-MPEG-TS
regression exercises the production FFmpeg arguments. Its Mac run could not
execute FFmpeg because Homebrew's x265 dylib is missing; the isolated compiler
image now includes FFmpeg for the pinned-toolchain verification. The user's
Homebrew installation was not modified.

PR #7 merged after actual Astra approval and its fresh effort gate. PR #13's
policy check identified eight stale task/process ownership counts. Astra
independently attributed every new site; the ledger now names those owners,
and all 115 validation tests pass. The ledger does not waive the ownership
check. Candidate `fc164045` passed its exact pinned compiler loop and fresh
effort gate run 85, then merged as `f2773ca8`.

Web structural acceptance produced 72 desktop/mobile captures across all three
layouts with no console/page errors. Actual Astra then found that a typed lost
owner response and a wall-clock adjustment could bypass uncertain-start
quarantine. The working fix uses monotonic deadlines and keeps ambiguous starts
blocked for 90 seconds. Native Apple has the same fixes, an app-wide uncertainty
barrier across profile switches, explicit live controls, and the runtime-only
Developer card. Token-free ownership markers now persist before POST and during
active playback, surviving page/app restart until confirmed release. Immutable
web marker keys prevent expiry from deleting another tab's fresh recovery wait;
marker IDs work on LAN HTTP. The real-HLS browser fixture now proves advancing
decoded frames, explicit controls, channel switch, active owner/producer/expiry
failures, paused release and reload quarantine after deliberately lost unload
DELETE. It does not claim unload delivery is guaranteed or substitute a fake
reaper for the backend's orphan cleanup. Apple merged after its exact review
and effort gate. Android is implemented with rendered-frame progress because
Media3 live-window position can move backward during healthy playback; 14
focused tests pass. Native UI and hardware playback acceptance remain.

Browser access has resumed after the Mac was unlocked. Forgejo is the only
mutable remote after migration; no authentication bypass or credential
extraction was attempted. Forgejo's documented AGit workflow opened and updated
PR #13 through existing SSH access. The supplied deploy key remains scoped to
node access.

## Evidence — exact commands and trees

| When | Tree | Command | Result |
|---|---|---|---|
| 2026-09-04 | `main` at `48615baf` | `rustc --version` | `1.97.1 (8bab26f4f 2026-07-14)` |
| 2026-09-04 | `main` at `48615baf` | `cargo check -p plurxd --all-targets --locked` | passed in source-only compiler loop |
| 2026-09-04 | staged `codex/hdhomerun-plan` | `make validate-staged` | 115 catalog, 1,294 history, and 160 operations checks passed after review fixes |
| 2026-09-04 | `b1bf0375` | PR [#904](https://github.com/pjunod/plurx/pull/904) | opened to the effort branch; exact-diff review requested |
| 2026-09-04 | `8e6eaafa` | adversarial PR review | approved; no actionable diff findings |
| 2026-09-04 | `a79c4927` | PR [#904](https://github.com/pjunod/plurx/pull/904) | current-head review approved; effort gate passed; merged as `e3f05f2e` |
| 2026-09-04 | `807d8318` | PR [#906](https://github.com/pjunod/plurx/pull/906) | Rust 1.97.1 fmt/check/clippy and focused tests passed; effort gate passed |
| 2026-09-04 | `807d8318` | first M1 adversarial review | one critical, five high, and two medium findings; merge held |
| 2026-09-04 | `bc7305ae` | M1 exact source archive `ca09afb…` | Rust 1.97.1 fmt/check/clippy; core cluster and replicated/SQLite generation suites; 16 Live TV and 2 route-matrix tests passed |
| 2026-09-04 | `bc7305ae` | M1 final adversarial review and effort gate | approved with no findings; merged as `b15d241a` |
| 2026-09-04 | working M2 tree | `cargo test -p plurxd live_tv --locked` | 22 passed, including one-GET streaming, bounded scratch, capability, redaction, and quorum cases |
| 2026-09-04 | working M2 tree | focused route and foreground-admission tests | 4 passed; maintenance/learner routing and cancellation-safe live admission proved |
| 2026-09-04 | working M2 tree | default clippy and no-default check | passed; Live TV is present in both builds |
| 2026-09-04 | `dd528728` archive `a2d9da92…b4304f02` | exact Rust 1.97.1 source-only loop | format, workspace check, workspace clippy `-D warnings`, no-default compile, 22 Live TV, 2 route-matrix, and 2 admission tests passed |
| 2026-09-04 | M2 adversarial review at `5637efba` | correctness, security, lifecycle, and test attack review | changes requested: 1 critical, 4 high, and 6 medium findings; merge held |
| 2026-09-04 | current adversarial-fix tree | `cargo test -p plurxd live_tv -- --nocapture` | 30 passed, including the one-GET fake-tuner/fake-FFmpeg lifecycle, source-generation binding, and confirmed cleanup fixture |
| 2026-09-04 | `80f63b7b` archive `385f478c…c7b88d` | exact Rust 1.97.1 source-only loop | format, workspace check, workspace clippy `-D warnings`, no-default compile, 30 Live TV, 2 route-matrix, relay-ceiling, voter-role, and 2 admission tests passed |
| 2026-09-04 | authoritative Forgejo | fetch and intended-base check | `main` is `2727ead0`; task base `effort/hdhomerun-live-tv` is `b15d241a`; GitHub is fetch-only historical context |
| 2026-09-04 | `c4b66e1d` | exact Astra review; pinned fmt/workspace check/workspace clippy | passed; PR #7 updated; includes main `4ce33f95` |
| 2026-09-04 | `4baf64bb`, source archive SHA-256 `51dd11bc220d079c39e2aca28961c23c77f521ea441f26d15f8f02fd97e16464` | Rust 1.97.1 fmt/workspace check/clippy; no-default check; `cargo test -p plurxd live_tv --locked`; route_matrix; live_admission | passed: 41 + 2 + 2 focused tests; no-default compilation retains existing unrelated dead-code warnings |
| 2026-09-04 | `4baf64bb` | `cargo test -p plurx-core --features hiqlite-store --lib live_tv_drain_response_signature --locked` | 1 passed; a previous filter without the feature ran zero and was not counted |
| 2026-09-04 | `4baf64bb` | actual Astra exact-candidate review | approved, no actionable findings; real rollover and two-node fault acceptance remain M5 |
| 2026-09-04 | physical tuner discovery | sanitized `discover.json`, no tuner stream opened | FLEX 4K, HDFX-4K, device `10AF300E`, four tuners, firmware `20260326`; not playback proof |
| 2026-09-05 | `70607d14`, the web branch merged with effort `fc9c8f45` | Rust 1.97.1 `cargo fmt --check`, `cargo check --workspace --locked --all-targets`, workspace Clippy `-D warnings`, `cargo test -p plurxd app_shell_loads_the_live_tv_controller_before_its_route` | passed; 1 focused test. The merge commit itself passed the effort hook: `history-check`, `validation-lint`, `operations-check` (160 tests) and `effort-rust-check` |
| 2026-09-05 | `70607d14` | `python3 scripts/live-tv-browser --self-host` | passed; decoded H.264/AAC frames advance, pause/resume, fullscreen stop, channel switch, navigation, typed refusal, late-start cleanup, active owner loss, producer failure, capability expiry, paused budget, visibility event and lost-unload reload quarantine; ten starts, nine confirmed releases, one deliberately orphaned start blocks replacement; no account token on capability routes and zero VOD/account writes |
| 2026-09-05 | `70607d14` | `scripts/ui-baseline --self-host` compared against the committed golden | passed; 72 captures, 6,600 structural facts, no console or page errors, 408 s. The `--update` golden generated during the `36090562` run was **not** adopted: it recorded one extra `div.row > button.ghost.sm` on the Live TV route that a clean run of the merged tree does not reproduce, so the committed baseline stands |
| 2026-09-05 | adversarial review of `70607d14` | release/renew symmetry in the client lease | one finding, fixed here: `Lease.status()` lacked the `releasing` guard that `keepalive()` already carries, so a heartbeat status poll issued while its own DELETE was in flight could renew the lease the client was dropping — the suite already treats a status request as lease-renewing. Covered by a regression test proven to fail without the guard |
| 2026-09-05 | adversarial review of `7fe573aa` (web) and `4192b97f` (Android) | whole-diff review of both client task PRs | three defects fixed, listed below. Both clients shared the critical one, and it sat in code an earlier review had approved |
| 2026-09-05 | `9c536174` | start-failure classification | **critical**: the ambiguous-outcome test was a denylist of four codes, so any other typed failure — including `tuner_unavailable`, whose own message admits a tuner "may be unavailable", and any code a newer server adds — cleared the durable marker and admitted an immediate retry, letting one viewer hold two physical tuners. Now an allowlist of failures decided before a tuner can open; everything else arms the barrier |
| 2026-09-05 | `9c536174` | `Lease.status()` / `keepalive()` generation check | the generation was captured *after* `stop()` had already incremented it, so the check compared a value to itself, and `releasing` is only set two microtasks later. A poll entered in that synchronous window still renewed the lease being dropped. Both now compare against the generation that owns `current`. The earlier `656ceb00` guard was real but incomplete, and its test skipped exactly this window |
| 2026-09-05 | `9c536174` | `channelView` | a denylist of two known-protected shapes rendered an enabled "Watch live" for any unfamiliar `support` value. Now allowlists the one playable state |
| 2026-09-05 | Android `da19e7fa` with its instrumentation fixture | isolated JDK 25 / Android SDK 37 image: `:app:assembleDebug`, `:app:assembleDebugAndroidTest`, `:app:testDebugUnitTest --tests tv.plurx.app.livetv.LiveTvTest` | passed; 14 unit tests, 0 failures. This is the exact-source Android verification the plan required, run off the Mac |
| 2026-09-05 | `LiveTvUiTest` on an API 36 x86_64 phone emulator | `:app:connectedDebugAndroidTest -Pandroid.testInstrumentationRunnerArguments.class=tv.plurx.app.livetv.LiveTvUiTest` | passed; 2 tests, 0 failures. The two earlier 10-second timeouts were **fixture faults, not product faults**: the loopback socket was bound to `InetAddress.getLoopbackAddress()` instead of the literal `127.0.0.1` the client dials, and `/api/v1/live-tv/readiness/refresh` was answered with `{}`, a body that cannot decode into `LiveTvReadiness`. The fixture now reports its bound address, accepted connections, every request, unexpected paths, fixture faults and the semantics tree instead of timing out silently |
| 2026-09-05 | `LiveTvUiTest` initial-focus expectation | phone-profile semantics dump | the Back control was observed `Focused = 'false'`. Initial focus is the 10-foot navigation contract, not something the product owes a touch screen, so the assertion is now scoped to television `uiMode` and the phone profile asserts reachability instead. **The television half has not been run**: the AOSP Android TV system image enforces adb authorization, which a headless container cannot grant, so it needs one of the arm64 AVDs on the Mac |
| 2026-09-05 | Android start-failure classification | `LiveTvLease.start` | **critical**: the ambiguous-outcome test was a denylist of four codes, so any other typed failure — including any code a newer server adds — called `barrier.confirm()` and deleted the durable marker, admitting an immediate retry that could hold a second physical tuner. Now an allowlist of failures decided before a tuner can open. Covered by `unrecognisedStartFailureKeepsTheDurableMarker`, executed against the unfixed code and observed to fail |
| 2026-09-05 | Android heartbeat | `LiveTvLease.heartbeatMarker` | it called `barrier.arm()` every five seconds, which is an `AtomicFile` rename + write + `fsync` + unlink **on the main thread** during live video — dropped frames and an ANR candidate. The durable marker is already on disk for the whole session, so the write changed nothing an observer could see; the heartbeat now refreshes only the in-process deadline |
| 2026-09-05 | Android watchdog coverage | `LiveTvTest` | the test cited as evidence for the sliding-window fix destructured its regressing-position list into `_` and never used it, feeding a monotonically increasing count instead. It would have passed against the position-based implementation it claimed to rule out. The list is gone, the comment now says what the test does prove, and it is renamed `renderedFrameCountRenewsOnChangeAndFrozenVideoExpires`. **The position-versus-frames choice at the LiveTvPlayer call site remains unproven** until a fake player drives that heartbeat |

| 2026-09-05 | `scripts/live-tv-endless` against a real endless FFmpeg source | ten playlist window rollovers, bounded scratch, one tuner GET, idle orphan expiry, resource cleanup | **passed** — 10 rollovers (media sequence 0 → 60, 4.0 s segments), 66 distinct segments actually read, published window never exceeded 6, peak live scratch 29.9 MB, **one** tuner GET for the whole session; an orphaned capability with no DELETE expired 65 s after its last read and left 0 bytes of scratch and no surviving encoder. The fixture is a real HDHomeRun-shaped device on ports 80 and 5004 at this host's own private address; no household tuner was opened |


## Decisions made while the owner is away

| Decision | Why | Revisit when |
|---|---|---|
| One manually configured private IPv4 device | Docker/VLAN broadcast discovery cannot be promised from the data-plane namespace | a diagnostic discovery helper can prove its namespace and interfaces |
| One explicit tuner-owner node with signed relay | prevents four cluster nodes from advertising sixteen leases on four tuners | replicated fenced tuner leases exist |
| Default two concurrent sessions | FLEX/CONNECT 4K has only two ATSC 3-capable tuners and other clients may compete | hardware evidence supports a different household policy |
| H.264/AAC at 720p by default | broad client compatibility and realistic realtime encoding | 1080p/2160p fixtures pass on the selected owner |
| Runtime enablement only; no code feature gate | one binary must expose the same capability everywhere while unsafe activation stays explicit and reversible | never; this is the product contract |
| No timeout-only owner recovery | elapsed time cannot prove the old FFmpeg process/tuner socket closed; require authenticated drain or a separate exact admin physical-fencing attestation while disabled | a physically enforced fenced lease mechanism exists |
| One tuner HTTP GET and one FFmpeg process per session | retries can double-lease physical tuners and split lifecycle ownership | a device-native resumable lease protocol exists |
| DRM and captions unsupported | plurx has no licensed DRM path; captions lack end-to-end proof | a lawful DRM path or 608/708 fixture exists |
| All first-party clients are in scope | a living-room feature is not complete as a web-only API | owner explicitly narrows the product surface |
| Separate Apple and Android task PRs within M4 | each platform gets an independently reviewed, compiled and tested candidate; neither blocks review of the other | both merge before final promotion |

## What remains

1. **Run the hardware pass.** `make live-tv-hardware-check DEVICE=<tuner ipv4> TUNERS=2` on a machine on the tuner's network. It writes `target/live-tv-hardware/hardware.json` with the device's real tuner count, real time to a playable segment against the 15 s budget, real segment length, signal strength and quality, the codec that survived the graph, and the device's own `/status.json` account of which tuners plurx held and that they came back. Every "pending the hardware pass" in the documentation points here.
2. **Run the two-node cases.** `make live-tv-two-node-check` on a Linux host that can bind ports 80 and 5004. They compile and have never executed; expect to debug the harness as well as the product on the first run.
3. **Android television instrumentation.** The AOSP Android TV system image enforces adb authorization, which a headless container cannot grant, so the television half of `LiveTvUiTest` needs one of the arm64 AVDs on the Mac.
4. **Promotion.** Only after 1–3.

## Known limits — honest until evidence changes them

- HDHomeRun HTTP behavior differs by model and firmware; the hardware pass
  must record `discover.json`, sanitized lineup facts, headers, and FFmpeg
  result from the actual device. `scripts/live-tv-hardware` now exists to do
  exactly that and has not been pointed at a device. Firmware that does not
  serve `/status.json` costs the strongest assertion in it — that the device
  itself agrees which tuners plurx holds — and it reports that as
  `not_exercisable` rather than skipping quietly.
- The default two concurrent sessions, the real startup latency and the real
  segment length in the documentation are **defaults and budgets, not
  measurements**. Nothing in this tree has measured them against a tuner.
- ATSC 3.0 commonly needs HEVC and AC-4. Device reception does not prove the
  installed FFmpeg can decode either.
- Owner failure ends the current live session. Reconfiguration remains possible
  while disabled; re-enable requires confirmed old-owner cleanup or explicit
  physical fencing, followed by readiness. Automatic takeover is out of scope.
- An external HDHomeRun client can win a tuner after plurx checks capacity.
  Runtime `503` remains normal and actionable.
