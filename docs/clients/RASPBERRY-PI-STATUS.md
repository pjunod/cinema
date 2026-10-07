# Raspberry Pi status — existing Plurx on a Pi 5

**Status:** merged; physical Pi acceptance in progress · **Updated:** 2026-10-07

Companion to the [implementation plan](RASPBERRY-PI-IMPLEMENTATION.md). This
page records software progress separately from physical-device acceptance.
The existing daemon and web player remain the product.

[PR #832](http://forge.lan:3000/noirr/plurx/pulls/832) batches all
implementation commits. Its checks and PR description carry the live final
validation receipt and merge result. Physical acceptance remains separate.

## Current work

| Work | State | Evidence / next action |
|---|---|---|
| Independent clone | ready | `/private/tmp/plurx-pi-agent`, branch `codex/raspberry-pi`, base `cca4a09b997171ba91136b396f7d484097305fd0` |
| Compiler | baseline passed | Rust 1.97.1; `cargo check --locked -p plurxd --all-targets`, exit 0 in 1m46s before Rust edits |
| Implementation contract | implemented | Source audits of server decoder inventory and browser media predictions |
| Server decoder integration | implemented; compiler passed | Sol 6.1; separate 8/10-bit graph proof, operational capabilities, shared HLS/VOD transfer |
| Existing browser player | implemented; tests passed | Sol 6.1; delivered tuple and fenced nullable predictions; web unit inventory, syntax and types pass |
| Deployment | implemented; tests passed | Sol 6.1; `deploy/pi-player`, safe launcher ownership, bounded report; all six regressions pass |
| Adversarial review | complete; both findings addressed | Reviewed `dfa775112`; Sol repaired P1 fallback and P2 transforms, coordinator inspected the changes |
| Validation | targeted checks passed; CI promotion incomplete | 292 validation methods, 729 operations methods, web inventory and all 10 named Rust regressions have passing evidence; one optional operations test skipped |
| Physical Pi 5 | testing | 16 GB Pi 5, Raspberry Pi OS Trixie; 4K60 Wayland HDMI connected; real Main/Main10 decode-transfer probes passed |

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

[Run 4222](http://forge.lan:3000/noirr/plurx/actions/runs/4222) tested
`35d90bc4db1feb061250ae918987446660271d7f`. History and regression-field checks
passed. Validation unittest ran 292 methods: 289 passed, three failed. Commit
`19764e7a5` repaired an assertion's old FFmpeg argument location and two ownership
counts; targeted invocations passed all three failed methods. Playback code did
not change. Successful siblings were retained.

Subsequent checks used that production source, with documentation-only link
scrubbing required by the operations contract:

| Command / coverage | Result |
|---|---|
| `make operations-check` plus failed-method continuation | 729/730 methods passed across retained runs; one optional upstream Hiqlite comparison skipped without its external source fixture |
| `make web-unit-check` plus unpassed-command continuation | Entire recipe passed; after a sandbox socket refusal, the existing `PLAYBACK_LAB_TEST_FILTER` retained 23 shaping passes and ran the remaining 95, followed by the unrun recipe suffix |
| `cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::decoder_inventory::tests::request_` | 3 passed |
| `cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection request_` | 5 passed |
| `cargo test --locked -p plurxd --bin plurxd -- browser_predictions_do_not_claim_decoder_identity request_decode_software_encode_has_one_frozen_software_startup_successor` | 2 passed |

The operations continuation reran only 11 unsuccessful methods. Socket/process
fixtures passed with the needed permissions. The Linux janitor timeout case
needs GNU `timeout`, absent on the Mac; a source-only archive executed that one
method on Linux in 2.054 seconds, passed, and its temporary extraction was
removed. No repository credential was transferred. The infrastructure-name
scrubber corrected two new status links and eight pre-existing links in three
other documents. It changed no runtime configuration.

**Promotion remains incomplete.** Main's fast lane bundles Python tests and
repeats overlapping web tests; it lacks the effort lane's per-test pass receipts.
The failed run was cancelled after its result to release an unused queued job.
The PR is draft to avoid replaying successful tests. Paul's reconciliation of
that gate with the requested once-only policy is pending. No full Rust unit
suite or Windows/Apple/Android CI compilation is claimed by the targeted local
evidence, and no green CI status has been manufactured. The PR description
holds the live receipt and final merge disposition.

All 20 named PR regressions have passed. No physical-device acceptance has been
recorded; server/browser/HDMI behavior still needs the exact Pi runtime.

The Pi FFmpeg source confirms its DRM SAND transfer uses planar `yuv420p`
or `yuv420p10le`; treating every 10-bit DRM frame as P010 would be wrong.
The implementation probes the actual transfer path before selecting it.
Browser support remains separate: an installed Chromium build must expose
HEVC through its own kernel/Mesa integration.

## Implementation evidence

The three implementation tasks were built by `gpt-6.1-sol` agents. The
coordinator integrated their changes without changing the user's checkout.

- Rust 1.97.1 daemon all-target compilation passed on the integrated source.
- Core decoder-selection compilation and the five request-selection regressions
  passed. The full workspace Clippy hook with denied warnings passes.
- JavaScript syntax and TypeScript ratchet checks pass; the existing type
  diagnostic count decreased from 515 to 513.
- Python deployment syntax and all six operation regressions pass.
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
Both now pass; no unit execution preceded review. The existing actor still owns
one in-process retry; its separately budgeted classified-health wrapper does
not introduce another attempt or a new watchdog.

## Physical acceptance — 2026-10-07

Paul supplied a Pi 5 for cinema testing. Tests use an independent clone and
isolated data/browser profiles; the existing desktop and system media tools
are retained. The merge of PR #832 is complete. Its later coverage build was
killed by SIGKILL before test execution; that does not establish a Pi defect.

Initial observed runtime: Raspberry Pi OS Trixie reference 2026-10-06, kernel
`6.18.50+rpt-rpi-2712`, ARM64, 16 GB memory; FFmpeg
`7.1.5-0+deb13u1+rpt2`, Chromium `154.0.8037.92`, Mesa `26.2.2`.
The active Wayland display is 3840x2160 at 59.982 Hz. Initial thermal status
reported no throttling. These are fixture observations, not minimum versions.

Both 8-bit and 10-bit HEVC completed the exact production request-decode,
hardware download and software H.264 encode graph in under 0.2 seconds for
two 160x120 frames. Driver diagnostics identify V4L2 stateless HEVC, DRM
frames and the expected downloaded planar depth. The actual OS diagnostic
uses `swfmt rpi4_8`/`swfmt rpi4_10`, whereas the original parser required
`swfmt=`. The parser is being corrected at the existing inventory authority;
the earlier equals form is retained and the remaining proof requirements
are unchanged. Captured-log positive and negative regressions are written,
with unit execution reserved for the reviewed candidate.

Native daemon, browser playback, seeking, sustained concurrency, subtitles
and HDR acceptance are in progress or pending; small FFmpeg probes alone do
not satisfy those rows. All changes and evidence will be batched in one PR.
