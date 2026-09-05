# HDHomeRun Live TV status — what is built and what is proved

**Status:** M5 complete and merged. **ATSC 1.0 and unprotected ATSC 3.0 both play end to end on a real FLEX 4K.** The hardware pass found three real defects and all three are fixed; what plurx cannot open is protected ATSC 3.0, by design · **Effort:** `effort/hdhomerun-live-tv` ·
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
| M5 hardware acceptance | **run, and passed for ATSC 1.0** | [#32](http://192.168.4.7:3000/noirr/plurx/pulls/32) | real FLEX 4K over an antenna: 6.4 s to a fetchable segment of a 15 s budget, 4.004 s segments, 1080i MPEG-2/AC-3 in and H.264 720p/AAC out, 2 sessions then `tuner_capacity`, the device itself confirming the tuners held and freed, 0 bytes left. One defect found: ATSC 3.0 always `startup_timeout` |
| M5 documentation | merged | [#33](http://192.168.4.7:3000/noirr/plurx/pulls/33) | Live TV now appears in FEATURES §4a, ROADMAP, ARCHITECTURE §3a, PLAYBACK, OPERATIONS (runbook + problems + ports), CHEATSHEET §5, SECURITY, both client-parity docs, REQUIREMENTS §3a and the README, whose live-TV non-goal is narrowed to "no DVR" rather than deleted |
| M5 defects found by hardware | **all three fixed and merged** | [#39](http://192.168.4.7:3000/noirr/plurx/pulls/39) · [#40](http://192.168.4.7:3000/noirr/plurx/pulls/40) · [#42](http://192.168.4.7:3000/noirr/plurx/pulls/42) | a 10-bit source meeting a pinned 8-bit profile, a flat startup budget that gave one message to two unrelated situations, and an 8 MiB probesize spent on tuner stream. Each with coverage executed against the unfixed implementation |
| M5 promotion | ready for ATSC 1.0; ATSC 3.0 open | — | the household numbers are measured and in the documentation. What is still open: ATSC 3.0 startup latency on a weak mux, the two-node cases (see the control result below), Android television instrumentation, and physical client playback |

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


| 2026-09-05 | `scripts/live-tv-hardware` against a real HDHomeRun FLEX 4K (`HDFX-4K`, firmware `20260326`, four tuners) on a real antenna, from a host on the same LAN | `make live-tv-hardware-check DEVICE=<ipv4> TUNERS=2` | **passed for ATSC 1.0.** 55 channels, 52 playable, 3 DRM-flagged (all ATSC 3.0). Channel 6.1: **6.4 s** from asking to a fetchable segment against the 15 s budget — 6.42 s of it inside the start request and 0.006 s waiting after it; **4.004 s** segments; source read straight off the tuner is MPEG-2 Main 1080 interlaced with AC-3 5.1, published output is H.264 720p with AAC stereo in MPEG-TS; signal 96–100% strength, 83–93% quality, 100% symbol; the device's own `/status.json` named the tuner (`tuner3`) and the channel (`6.1`) it was on. Two sessions filled the configured limit, the third answered `tuner_capacity`, and the device confirmed exactly two tuners (`tuner0`, `tuner3`) held and then free. A DRM-flagged channel was refused `drm_unsupported`. 0 bytes of live scratch survived release, and a tuner another household client held throughout (`tuner2`) was never touched. Only one property was not exercisable: a four-tuner device has no configurable `live_tv_max_sessions` above its own count, since the setting is validated to 1..=4 |
| 2026-09-05 | the same device, channel 157.1 (ATSC 3.0) | `make live-tv-hardware-check DEVICE=<ipv4> --channel 157.1` | **failed: `startup_timeout`.** Diagnosed rather than assumed. The channel is listed `ready` by plurx's own sanitized lineup. Reproducing plurx's exact producer arguments (`spawn_live_ffmpeg`) by hand against the live stream published its **first segment at 18.1 s**, then seven segments without trouble; the ATSC 1.0 control channel on the same device and host published at **7.04 s**. `STARTUP_TIMEOUT` is 15 s, so the budget — not a codec — is what refuses it. Ruled out: the codec (HEVC Main 10 1080 + AC-3, both decoded), transcode cost (that source encodes to 720p at **2.37× realtime** in software on the same machine, against 5.87× for MPEG-2), and the device (first byte in 2.77 s). Separately, ATSC 3.0 channels 108.1 and 135.1 return **zero bytes** from the device within 10 s, which is reception |
| 2026-09-05 | hardware-script defects the first real run exposed | second and third runs | two, both fixed in the same PR. (1) Startup was measured from after the start request returned, reporting **0.007 s** while the request itself took 6.4 s — the one number nobody wants; it now measures from asking, and reports both halves. (2) Attribution matched the device's `TargetIP` against this host's address, which never matches behind NAT; it now falls back to "idle before this run, busy now", says which signal it used, and accepts `--host-address`. A third addition came from the run: the broadcaster's own codecs are now read straight off the tuner, because the published segment only proves what the graph produced |

| 2026-09-05 | the same device, chasing channel 157.1 (ATSC 3.0, HEVC Main 10 1080 + AC-3) | three separate diagnoses | **three real defects, all fixed and merged.** (1) The live filter chain pinned no pixel format, so a 10-bit broadcast reached libx264 — which is handed `-profile:v high` — and x264 refused with "high profile doesn't support a bit depth of 10", so FFmpeg exited before publishing. Any 10-bit live source was affected. Reproduced deterministically from a 4-second capture, with and without the conversion, plus the same pair against an 8-bit MPEG-2 capture to show the conversion costs that path nothing. Fixed in [#39](http://192.168.4.7:3000/noirr/plurx/pulls/39). (2) `STARTUP_TIMEOUT` was flat, so it gave one sentence to a channel quietly working and to two ATSC 3.0 channels that accept the connection and deliver **zero** bytes — and it expired before the encoder error above could be reported at all. Fixed in [#40](http://192.168.4.7:3000/noirr/plurx/pulls/40); extending it is how the encoder error was found, as `stream_failed: FFmpeg exited with exit status: 1`. (3) `-probesize` was 8 MiB of *tuner stream*: measured 7.06 s to first segment against 5.55 s at 2 MiB and 5.54 s at 512 KiB on the antenna's strongest channel, and roughly 23 s of wall time on its own on the 2.8 Mbps ATSC 3.0 mux. Fixed in [#42](http://192.168.4.7:3000/noirr/plurx/pulls/42) |
| 2026-09-05 | the same device and channel, on the fully merged tree | `make live-tv-hardware-check DEVICE=<ipv4> --channel 157.1` | one run answered `503 tuner_unavailable` — the **device's** own refusal, relayed immediately and correctly typed, which is what the budget change was for. **That was transient tuner contention, not the mux failing**, and reading it as the latter was wrong: a later run with every tuner idle got 2.36 s to first byte and 3.3 Mbps from the same channel |
| 2026-09-05 | the same device, channel 157.1, all three fixes merged | `make live-tv-hardware-check DEVICE=<ipv4> --channel 157.1` | **ATSC 3.0 PLAYS.** Source read straight off the tuner is HEVC **Main 10** 1080 with AC-3 5.1; the published output is H.264 720p with AAC stereo in MPEG-TS; playable **8.997 s** after asking against the 15 s budget (8.988 s of it inside the start request); 4.004 s segments; the device named the tuner (`tuner0`) and the channel (`157.1`) it was on at 87% strength and 51% quality; the tuner was attributed to this run, released, and 0 bytes of scratch survived. All three fixes were needed: without the pixel-format conversion FFmpeg exits on the 10-bit source, without the byte-aware budget the failure is an unreadable timeout, and without the 2 MiB probesize the probe alone outlasts the budget on a 3 Mbps mux |
| 2026-09-05 | ATSC 3.0 channels 108.1 and 135.1 | direct reads with every tuner idle, no Range header | **the earlier "reception" conclusion was wrong.** Both return **zero bytes after a flat 10.0 s** while the lineup reports **98% signal quality** — byte-for-byte the same behaviour as DRM-flagged 106.1 on the same antenna, and nothing like a weak signal. So those muxes are protected content the device declines to hand over, whether or not the lineup carries a DRM marker. The control that settles it: 6.1 answered in 0.36 s at 7.2 Mbps and 157.1 in 2.36 s at 3.3 Mbps in the same sweep |
| 2026-09-05 | `crates/plurxd/tests/live_tv_two_node.rs` on **two** lab hosts — nuc3, and nynuc (16 cores, 60 GB) in the pinned 1.97.1 image, both able to bind 80 and 5004 | `cargo test --features cluster-integration-tests --test live_tv_two_node` | **executed, and all three cases failed on both — environmentally, replicated.** On nynuc node B got as far as "serving authority recovered from a fresh quorum watermark" and the cases still failed after 298 s. The control failed **identically on both hosts**: the repo's own `cluster_activity` panics at `cluster_activity.rs:303` with `daemon readiness timed out`, 1 of 2 cases passing, while CI's `cluster_daemon` job is green on its self-hosted runner. Two independent hosts and a shared pre-existing baseline failure is as far as this can be taken without that runner.** The container binds both ports and sees itself on a private address the runtime accepts, so the fixture device is viable; the daemons then flood `hiqlite ... no such table: watched_outbox` and the cases fail after 300 s. **The control settles it**: the repo's own `cluster_activity` in the same container fails with `daemon readiness timed out` (1 of 2 cases passing), while CI's `cluster_daemon` job on its self-hosted runner is green. So this environment cannot host multi-daemon cluster tests, and that is a fact about the container rather than about the new cases. They still need a host like CI's runner, and are still unproven |

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

1. **~~Finish ATSC 3.0~~ — done.** Unprotected ATSC 3.0 plays end to end: 8.997 s to a fetchable segment against the 15 s budget, HEVC Main 10 in and H.264 720p out, on a 51%-quality mux. Protected ATSC 3.0 is out of scope and stays out: plurx has no licensed DRM path, and the two unflagged channels that will not open behave exactly like the flagged ones while reporting 98% signal quality. Rerun `make live-tv-hardware-check DEVICE=<ipv4> --channel <an ATSC 3.0 number>` after any antenna work, and if a healthy ATSC 3.0 mux still misses the budget, that is real evidence for a further change rather than the marginal-signal noise measured here.
2. **Run the two-node cases somewhere that can host two daemons.** `make live-tv-two-node-check`. They have now been *executed* once, in a container that could bind 80 and 5004, and all three failed — but the control proves the environment: the repo's own `cluster_activity` fails there too with `daemon readiness timed out`, while CI's `cluster_daemon` job is green on its self-hosted runner. So the place to run them is a host of that class, and their assertions remain unproven.
3. **Android television instrumentation.** The AOSP Android TV system image enforces adb authorization, which a headless container cannot grant, so the television half of `LiveTvUiTest` needs one of the arm64 AVDs on the Mac.
4. **Physical client playback.** The server side is proved against the real device; no physical iPhone, Apple TV, phone or Google TV has played from it, so whether AVPlayer and Media3 hold a 4 s-segment live window on real hardware is still open.
5. **Promotion.** Only after 1–4.

## Known limits — honest until evidence changes them

- HDHomeRun HTTP behavior differs by model and firmware; the hardware pass
  must record `discover.json`, sanitized lineup facts, headers, and FFmpeg
  result from the actual device. `scripts/live-tv-hardware` now exists to do
  exactly that and has not been pointed at a device. Firmware that does not
  serve `/status.json` costs the strongest assertion in it — that the device
  itself agrees which tuners plurx holds — and it reports that as
  `not_exercisable` rather than skipping quietly.
- The startup latency and segment length in the documentation are now
  **measurements** from one FLEX 4K on one antenna, not specifications. The two
  concurrent sessions remain a default: two were exercised on a four-tuner device,
  and three or four concurrent live sessions have not been.
- **ATSC 3.0 is listed as playable and always refused.** Measured on a FLEX 4K: an
  ATSC 3.0 channel publishes its first segment at 18.1 s against a 15 s
  `STARTUP_TIMEOUT`, so every start answers `startup_timeout` even though the channel
  plays once started. Not a codec limit — HEVC Main 10 and AC-3 both decode, and that
  source transcodes at 2.37× realtime in software. The lineup advertised `AudioCodec:
  AC4` for that mux while the stream carried AC-3, so AC-4 decoding remains untested
  rather than proved absent.
- Reception is still not decoding: two ATSC 3.0 channels on the test antenna return
  zero bytes from the device itself, which no amount of software fixes.
- Owner failure ends the current live session. Reconfiguration remains possible
  while disabled; re-enable requires confirmed old-owner cleanup or explicit
  physical fencing, followed by readiness. Automatic takeover is out of scope.
- An external HDHomeRun client can win a tuner after plurx checks capacity.
  Runtime `503` remains normal and actionable.
