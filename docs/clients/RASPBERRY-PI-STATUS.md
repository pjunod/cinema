# Raspberry Pi status — existing Plurx on a Pi 5

**Status:** implementation and review complete; static-contract repairs pass; remaining validation pending · **Updated:** 2026-10-06

Companion to the [implementation plan](RASPBERRY-PI-IMPLEMENTATION.md). This
page records software progress separately from physical-device acceptance.
The existing daemon and web player remain the product.

[PR #832](http://192.168.4.7:3000/noirr/plurx/pulls/832) batches all
implementation commits. Its checks and PR description carry the live final
validation receipt and merge result. Physical acceptance remains separate.

## Current work

| Work | State | Evidence / next action |
|---|---|---|
| Independent clone | ready | `/private/tmp/plurx-pi-agent`, branch `codex/raspberry-pi`, base `cca4a09b997171ba91136b396f7d484097305fd0` |
| Compiler | baseline passed | Rust 1.97.1; `cargo check --locked -p plurxd --all-targets`, exit 0 in 1m46s before Rust edits |
| Implementation contract | implemented | Source audits of server decoder inventory and browser media predictions |
| Server decoder integration | implemented; compiler passed | Sol 6.1; separate 8/10-bit graph proof, operational capabilities, shared HLS/VOD transfer |
| Existing browser player | implemented; validation pending | Sol 6.1; delivered tuple and fenced nullable predictions; syntax/type checks passed |
| Deployment | implemented; validation pending | Sol 6.1; `deploy/pi-player`, safe launcher ownership, bounded report, six regression cases written |
| Adversarial review | complete; both findings addressed | Reviewed `dfa775112`; Sol repaired P1 fallback and P2 transforms, coordinator inspected the changes |
| Fast lane | targeted repairs passed | Run 4222 retained 289 passes; all three failed static-contract methods now pass; later stages not run |
| Physical Pi 5 | unavailable | Host/user requested; no hardware, HDR or concurrent-playback claim |

## Standing decisions

1. **Use an independent clone.** The user's checkout is not a workspace for
   this effort. The initially requested managed worktree was unused and
   archived without edits.
2. **Batch commits into one main-bound PR.** Paul's 2026-10-06 instruction
   supersedes the repository's per-task effort PR convention for this work.
   Compile during development; defer test execution until the reviewed
   candidate's fast lane. Preserve regression fields on the landing commit.
3. **No new application or watchdog.** Fix existing decoder selection,
   media reporting and deployment seams. Do not invent device capabilities.
4. **Keep hardware observations separate.** A build, FFmpeg inventory,
   browser prediction and displayed accelerated playback are different facts.
5. **No surprise software installation.** Do not replace the user's browser,
   install third-party binaries, or modify nodes during source development.

## Evidence ledger

[Run 4222](http://192.168.4.7:3000/noirr/plurx/actions/runs/4222) tested
`35d90bc4db1feb061250ae918987446660271d7f`. History and regression-field checks
passed. Validation unittest ran 292 methods: 289 passed, three failed. One
assertion still expected FFmpeg arguments in the inventory instead of their
shared mapping; two ownership ledger counts need to include the new fallback
regression. Later test stages have not run. Preserve the passing evidence and
repair only those failures. All three now pass in targeted invocations; playback
code is unchanged by these assertion/ledger repairs. The main workflow bundles
Python tests without the effort workflow's per-test receipts; its retry granularity
needs reconciliation before another CI run. The PR carries the live validation
and merge receipt.
No physical-device acceptance has been recorded.

The Pi FFmpeg source confirms its DRM SAND transfer uses planar `yuv420p`
or `yuv420p10le`; treating every 10-bit DRM frame as P010 would be wrong.
The implementation probes the actual transfer path before selecting it.
Browser support remains separate: an installed Chromium build must expose
HEVC through its own kernel/Mesa integration.

## Implementation evidence

The three implementation tasks were built by `gpt-6.1-sol` agents. The
coordinator integrated their changes without changing the user's checkout.

- Rust 1.97.1 daemon all-target compilation passed on the integrated source.
- Core `decoder_selection` integration target compilation passed; no tests
  executed. The bare-core compile emits existing feature-dependent dead-code
  warnings; the full workspace Clippy hook with denied warnings passes.
- JavaScript syntax and TypeScript ratchet checks pass; the existing type
  diagnostic count decreased from 515 to 513.
- Python deployment syntax passes. Its six operation regressions are written
  and deferred to the final validation window.
- The static regression-field resolver exposed its missing support for the
  repository's existing `asyncTest` harness. The authoritative marker parser
  now recognizes that exact declaration; a negative-case regression is added.
- Normal pre-commit formatting, catalog lint, Clippy and embedded JavaScript
  syntax checks pass. No hook was bypassed.

## Adversarial review dispositions

The reviewer examined the complete candidate `dfa775112` before test
execution. Repairs stay in the existing media planner and startup owner.

| Finding | Consequence | Disposition |
|---|---|---|
| P2: request hardware frames bypass FFmpeg autorotation | Rotated/mirrored input can change presentation when hardware is selected | Fixed: proven identity transform required for initial and continuation paths; rotated, mirrored and unknown transforms preserve existing software handling |
| P1: startup retry excludes every software encoder | Hardware decoding paired with software encoding cannot recover when device/input initialization fails after boot | Fixed: decoder-aware retry eligibility, frozen software-only successor, retained CPU reservation, existing one-shot owner and software startup allowance |

The review found no further blocking issue in browser diagnostics, launcher
ownership, FFmpeg mapping, planar transfer, qualification boundaries or test
marker parsing. Generic DRM on non-Pi Linux can consume some of the existing
30-second inventory budget; this is a recorded bounded inventory limitation,
not a hardware-support claim. No second review or early unit run is planned.

Repair regressions are
`request_decode_requires_proven_identity_transform_even_for_continuations`
and `request_decode_software_encode_has_one_frozen_software_startup_successor`.
They are written, compiled with the candidate and scheduled with the final
fast lane; no unit execution preceded review. The existing actor still owns
one in-process retry; its separately budgeted classified-health wrapper does
not introduce another attempt or a new watchdog.
