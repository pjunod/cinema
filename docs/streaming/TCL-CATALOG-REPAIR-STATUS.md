# TCL catalog repair — implementation and evidence

**Status:** building · **Updated:** 2026-10-02

Companion to [the reviewed RCA](TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md).
This page records implementation, decisions and evidence separately from physical
playback acceptance. Work uses an isolated clone and the current Forgejo main
(`bfdc4930b`) as its base. The original checkout is untouched.

## 1. Delivery — commits batched for one main review

The user superseded per-task test runs: build with compile, Clippy and formatting
checks; obtain one adversarial agent review when the main PR is ready; address
findings, then run the fast lane once. Rerun failed checks only, unless a code
change invalidates a prior passing result. No deployment or saved-switch change
is authorized by implementation. Integration uses `effort/tcl-catalog-repair`.

| Milestone | State | Evidence |
|---|---|---|
| Preserve RCA and source replay | done | Copied only this effort's three untracked files |
| Pinned Rust loop | done | Rust 1.97.1; current-main all-target compile passed |
| Typed diagnostics and accounting | building | Diagnostic commit retained before bound repair |
| One catalog per create | pending | Canonical caps and recipe/worker identities retained |
| Android compaction and shared contract | pending | No legacy blanket 400 |
| Snapshot, generation and budgets | pending | Parameters recorded before dependent changes |
| Partial selection through existing owners | pending | No new watchdog/retry owner |
| Review and fast lane | pending | No tests claimed yet |
| Physical TCL/Streamer and cluster timing | outstanding | Exact TCL body/count not captured |

## 2. Decisions — evidence required before dependent changes

Bound, negotiated contract, startup/reserve numbers and generation scope are
being resolved from current code and fixtures. No placeholder is acceptance.
RF1 consistent-read latency remains a separate investigation.

## 3. Acceptance — planner evidence does not close device playback

Retain the diagnostic commit for a gated diagnostic deployment. Confirm actual
TCL and Streamer wire counts on unchanged clients; a TCL count at or below 16
falsifies B1 attribution. Deployment, cold/warm Auto first-frame and sustained
playback, Safari fallback and settled-cluster p95/p99 remain outstanding until
measured against recorded client/server revisions.
