# HDHomeRun Live TV status — what is built and what is proved

**Status:** backend and Apple merged; web acceptance passed; Android qualification in progress · **Effort:** `effort/hdhomerun-live-tv` ·
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
| M4 Android | implemented; final review and native UI acceptance | — | dedicated live player and Developer controls compile; 14 focused tests pass; Astra sliding-window finding fixed using rendered-frame progress; isolated JDK 25/SDK 37 verification pending |
| M5 docs, hardware, promotion | not started | — | — |

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

## Known limits — honest until evidence changes them

- HDHomeRun HTTP behavior differs by model and firmware; the hardware pass
  must record `discover.json`, sanitized lineup facts, headers, and FFmpeg
  result from the actual device.
- ATSC 3.0 commonly needs HEVC and AC-4. Device reception does not prove the
  installed FFmpeg can decode either.
- Owner failure ends the current live session. Reconfiguration remains possible
  while disabled; re-enable requires confirmed old-owner cleanup or explicit
  physical fencing, followed by readiness. Automatic takeover is out of scope.
- An external HDHomeRun client can win a tuner after plurx checks capacity.
  Runtime `503` remains normal and actionable.
