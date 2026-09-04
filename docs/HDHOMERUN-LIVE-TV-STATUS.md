# HDHomeRun Live TV status — what is built and what is proved

**Status:** design · **Effort:** `effort/hdhomerun-live-tv` · **Updated:**
2026-09-04 · **Live issue:** [#902](https://github.com/pjunod/plurx/issues/902)

Companion to [HDHOMERUN-LIVE-TV-PLAN.md](HDHOMERUN-LIVE-TV-PLAN.md) (the
contract and ordered work) and [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md)
(the task-PR and final qualification rules) — this is the one-page answer to
*where is the effort, what passed, and what decision was made without Paul?*

## Progress — task PRs earn their checkmark

| Milestone | State | PR | Evidence |
|---|---|---|---|
| M0 plan and adversarial review | ready for PR | pending | adversarial re-review approved; fast lane passed |
| M1 device/settings/lineup | not started | — | — |
| M2 live HLS and cluster relay | not started | — | — |
| M3 web client | not started | — | — |
| M4 Apple and Android | not started | — | — |
| M5 docs, hardware, promotion | not started | — | — |

## Current work — design before bytes

The current branch is `codex/hdhomerun-plan`, based on
`effort/hdhomerun-live-tv`. Architecture and protocol pre-reviews rejected
adapting the finite-file VOD engine and rejected per-node tuner capacity. A
third adversarial review found three critical and eleven high/medium gaps. The
plan now closes them with a proxy-free one-GET byte pump, configuration/quorum
fencing, idempotent provisional start/activation, owner-authoritative lineup,
mixed-version capability proof, independent producer watchdog, exact FFmpeg
graph readiness, typed bounded resources, `omit_endlist`, no-feature-gate
tests, client recovery, and per-PR review/status gates. The reviewer approved
the amended plan with no remaining findings.

## Evidence — exact commands and trees

| When | Tree | Command | Result |
|---|---|---|---|
| 2026-09-04 | `main` at `48615baf` | `rustc --version` | `1.97.1 (8bab26f4f 2026-07-14)` |
| 2026-09-04 | `main` at `48615baf` | `cargo check -p plurxd --all-targets --locked` | passed in source-only compiler loop |
| 2026-09-04 | staged `codex/hdhomerun-plan` | `make validate-staged` | 115 catalog, 1,294 history, and 160 operations checks passed after review fixes |

## Decisions made while the owner is away

| Decision | Why | Revisit when |
|---|---|---|
| One manually configured private IPv4 device | Docker/VLAN broadcast discovery cannot be promised from the data-plane namespace | a diagnostic discovery helper can prove its namespace and interfaces |
| One explicit tuner-owner node with signed relay | prevents four cluster nodes from advertising sixteen leases on four tuners | replicated fenced tuner leases exist |
| Default two concurrent sessions | FLEX/CONNECT 4K has only two ATSC 3-capable tuners and other clients may compete | hardware evidence supports a different household policy |
| H.264/AAC at 720p by default | broad client compatibility and realistic realtime encoding | 1080p/2160p fixtures pass on the selected owner |
| DRM and captions unsupported | plurx has no licensed DRM path; captions lack end-to-end proof | a lawful DRM path or 608/708 fixture exists |
| All first-party clients are in scope | a living-room feature is not complete as a web-only API | owner explicitly narrows the product surface |

## Known limits — honest until evidence changes them

- HDHomeRun HTTP behavior differs by model and firmware; the hardware pass
  must record `discover.json`, sanitized lineup facts, headers, and FFmpeg
  result from the actual device.
- ATSC 3.0 commonly needs HEVC and AC-4. Device reception does not prove the
  installed FFmpeg can decode either.
- Owner failure ends the current live session. The next Watch may start after
  the owner returns or the configured owner changes; automatic takeover is not
  part of this effort.
- An external HDHomeRun client can win a tuner after plurx checks capacity.
  Runtime `503` remains normal and actionable.
