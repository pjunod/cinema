# Raspberry Pi status — existing Plurx on a Pi 5

**Status:** building · **Updated:** 2026-10-06

Companion to the [implementation plan](RASPBERRY-PI-IMPLEMENTATION.md). This
page records software progress separately from physical-device acceptance.
The existing daemon and web player remain the product.

[Draft PR #832](http://192.168.4.7:3000/noirr/plurx/pulls/832) batches all
implementation commits. It stays draft until implementation and the single
adversarial review are complete.

## Current work

| Work | State | Evidence / next action |
|---|---|---|
| Independent clone | ready | `/private/tmp/plurx-pi-agent`, branch `codex/raspberry-pi`, base `cca4a09b997171ba91136b396f7d484097305fd0` |
| Compiler | baseline passed | Rust 1.97.1; `cargo check --locked -p plurxd --all-targets`, exit 0 in 1m46s before Rust edits |
| Implementation contract | ready to build | Source audits of server decoder inventory and browser media predictions |
| Server decoder integration | implemented; compiler passed | Sol 6.1; separate 8/10-bit graph proof, operational capabilities, shared HLS/VOD transfer |
| Existing browser player | implemented; validation pending | Sol 6.1; delivered tuple and fenced nullable predictions; syntax/type checks passed |
| Deployment | implemented; validation pending | Sol 6.1; `deploy/pi-player`, safe launcher ownership, bounded report, six regression cases written |
| Adversarial review | running | Complete implementation frozen; one independent agent review before the fast lane |
| Fast lane | not run | Run after review fixes, retain passing evidence, rerun failed checks only |
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

No test or physical-device acceptance has been recorded yet. Baseline
compiler evidence above is compilation only. The final ledger will identify
candidate SHA, commands, results and outstanding limits.

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
