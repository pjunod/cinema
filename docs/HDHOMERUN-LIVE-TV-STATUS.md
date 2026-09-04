# HDHomeRun Live TV status — what is built and what is proved

**Status:** M2 Astra findings implemented; final focused verification pending · **Effort:** `effort/hdhomerun-live-tv` ·
**Updated:** 2026-09-04 · **Historical issue:**
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
| Forgejo main synchronization | review pending | [#7](http://192.168.4.7:3000/noirr/plurx/pulls/7) | merge `f087f9d8`; later main changes still need incorporation |
| M2 live HLS and cluster relay | review fixes in progress; merge held | pending Forgejo PR | earlier focused checks passed; second review found unresolved owner recovery, cleanup, response bounds, metrics, and behavioral-test gaps |
| M3 web client | implementation staged | — | controller and browser tests are ready to resume after the M2 review closes |
| M4 Apple and Android | not started | — | — |
| M5 docs, hardware, promotion | not started | — | — |

## Current work — close the remaining M2 review findings

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
behavioral tests. These changes are unqualified until they compile and pass
focused tests. An explicitly selected Astra agent is independently reviewing
the owner-transition design. The earlier review labelled “Astra” selected a
reviewer role without changing its model; it is retained as an additional
review, not evidence that Astra ran.

The actual Astra whole-M1/M2 review found three further issues: an in-flight
start could cross shutdown, owner control responses were not authenticated,
and unsupported source decoders lacked a typed error. The working fix closes
registry insertion permanently at shutdown, authenticates bounded owner
control responses (including playlists), constructs activation URLs locally,
and recognizes missing-decoder diagnostics in bounded stderr capture.
The latest completed local focused run passed 40 Live TV tests plus the
cluster Activity aggregation test. The signature-tampering regression passed
separately. Local lint and policy checks passed; the exact committed tree still
needs Rust 1.97.1 verification. No current-commit approval
or merge qualification is claimed.

Commit `3945a713` contains the owner/recovery/cleanup hardening. Astra's closure
review accepted those findings and identified optional audio as a final gap:
the first profile now requires detectable video and audio. A new real-MPEG-TS
regression exercises the production FFmpeg arguments. Its Mac run could not
execute FFmpeg because Homebrew's x265 dylib is missing; the isolated compiler
image now includes FFmpeg for the pinned-toolchain verification. The user's
Homebrew installation was not modified.

Next is compilation and tests of the completed fixes, adversarial approval,
the Forgejo effort gate, and task merge.
Forgejo is the only mutable remote after the repository migration. Repository
access is confirmed through the host SSH agent; the supplied deploy key
remains scoped to node access.

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
