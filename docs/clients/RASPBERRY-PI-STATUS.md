# Raspberry Pi status — existing Plurx on a Pi 5

**Status:** building · **Updated:** 2026-10-06

Companion to the [implementation plan](RASPBERRY-PI-IMPLEMENTATION.md). This
page records software progress separately from physical-device acceptance.
The existing daemon and web player remain the product.

## Current work

| Work | State | Evidence / next action |
|---|---|---|
| Independent clone | ready | `/private/tmp/plurx-pi-agent`, branch `codex/raspberry-pi`, base `cca4a09b997171ba91136b396f7d484097305fd0` |
| Compiler | running baseline | Rust 1.97.1 verified; daemon all-target check before Rust edits |
| Implementation contract | ready to build | Source audits of server decoder inventory and browser media predictions |
| Server decoder integration | source audit | Sol 6.1; V4L2 stateless request decoding, no fictitious Pi encoder |
| Existing browser player | source audit | Sol 6.1; delivered-stream capability prediction and honest telemetry |
| Deployment | pending | Existing Linux installer and sandboxed fullscreen browser |
| Adversarial review | pending | One review when the complete PR is ready for main |
| Fast lane | not run | Run after review fixes, retain passing evidence, rerun failed checks only |
| Physical Pi 5 | unavailable | Host/user requested; no hardware, HDR or concurrent-playback claim |

## Standing decisions

1. **Use an independent clone.** The user's checkout is not a workspace for
   this effort. The initially requested managed worktree was unused and
   queued for archival.
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

No test or physical-device acceptance has been recorded yet. The final
ledger will identify candidate SHA, commands, results and outstanding limits.
