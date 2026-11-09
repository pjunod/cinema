# Shared-library playback — implementation and merge status

**Status:** open · **Updated:** 2026-11-09 · **Owner:** Codex integrator

Companion to [the root-cause analysis](SHARED-LIBRARY-PLAYBACK-RCA.md), which
owns the design and accepted decisions. This page records PR #1472's delivery
state. The larger [playback-start effort](PLAYBACK-START-PLAN.md) owns A1.

## Current state

PR **#1472**, branch `claude/shared-library-standalone-fixes`, remains draft.
The resumed source is `0b3c0dfc3`; the relay work and prior Opus adversarial
review fixes are integrated. No passing fast-lane result is claimed.

The implementation covers NULL-principal consumers (§4), committed membership
(§6), RPC attribution (§7 step 2), Source admission (§8/S12), poster retries
and snapshots (§9), the HEVC index argv (S4), owned abandonment (S10), and
startup budgets and retries across the relay (S6/S7/S11).

S5 was reverted: independent subtitle extraction lacked Source custody.
S1–S3, S8 and S9 remain A1 work. Native Apple/Android startup headers require
a client release. The RX ring and deployment remain separate operations work.

## Evidence and remaining work

| Stage | State |
|---|---|
| Compiler | Pinned Rust 1.97.1 verified on the existing build host and in a resource-capped build container; warm target retained. |
| Prior review | PR body records one Opus adversarial review and applied fixes; the API has no formal review or attached report. Preserve that attribution; do not claim a new independent review. |
| Existing focused tests | Latest retained run: 143 passed, two failed; pinned fixtures: four passed, three failed. The final scope-status commit postdates those results. |
| Core | Retained focused run: 27 passed; separate replicated-store run: five passed. Applicability remains bounded to the source and features actually exercised. |
| Counterfactuals | Prior evidence covers NULL principals and Source admission. Other counterfactual execution is not established. |
| Paperwork | Prior CI failed for missing `Regression-Test:` lines in the PR body. |
| Next | Repair the remaining fixture/production failures, compile final source, complete merge prerequisites, run the final fast lane, rerun failures only, merge on green. |

The fixture investigation uses a separate owned clone. User-owned source is
not a build or scratch workspace. No watchdog, feature gate or authorization
cache is introduced by the resumption.
