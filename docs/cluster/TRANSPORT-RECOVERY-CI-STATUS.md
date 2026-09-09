# Transport recovery CI — implementation status and evidence

**Status:** qualifying M2 · **Owner:** Codex · **Updated:** 2026-09-08

Companion to
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) and
[TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md)
(what the retained leak statistic asserts) — this page says what has actually
been built, reviewed, and proved. It is updated with each milestone so a green
checkbox means retained evidence, not intent.

## Current position — M1 is complete and the old campaign is paused

The effort begins at remote `main`
`9fcd151c98485707d2dc28195adf9c0c1e4a6d4c`, the same source reviewed by the
implementation handoff. Work is isolated on `effort/transport-recovery-ci`;
reviewable milestones use `codex/transport-recovery-ci-m*` branches and the
effort development gate.

Forgejo run
[#1034](http://192.168.4.7:3000/noirr/plurx/actions/runs/1034) remains the
timing baseline: about 123 seconds before the campaign, about 100 minutes for
voter warmup plus 20 recoveries, then a resource-envelope failure. The newer
main run
[#1100](http://192.168.4.7:3000/noirr/plurx/actions/runs/1100) had not reached
a qualification verdict; its old combined campaign was cancelled on
2026-09-08 at the operator's request. The older queued run
[#1098](http://192.168.4.7:3000/noirr/plurx/actions/runs/1098) was cancelled at
the same time. No smoke or partial run is recorded as qualification.

**How to read it:** M1–M5 development stays on the compile-and-policy effort
lane, so it does not start the old four-hour campaign. M6 deliberately restores
and runs the full voter and learner qualification once the final candidate is
frozen.

## Milestones — evidence closes the checkbox

- [x] **M0 — establish the base.** Rust 1.97.1 was invoked explicitly because
  Homebrew shadows the repository pin with 1.98.0. `cargo fmt --all --check`,
  package check, and package Clippy with denied warnings passed before edits.
- [x] **M1 — independent role reports.** Typed plan, strict CLI, shared
  validation, compatible aliases, and canonical assembly pass focused tests
  and one real Linux warmup-plus-one smoke for each role.
- [ ] **M2 — durable diagnostics.** Checkpoints, timings, failure classes,
  cancellation, and owned-child cleanup evidence.
- [ ] **M3 — split CI.** Independent voter and learner jobs, retained aggregate
  job ID, current-run artifacts, and exact receipt integration.
- [ ] **M4 — frozen candidate semantics.** Promotion runs survive unrelated
  superseding events and stale candidates cannot issue a receipt.
- [ ] **M5 — measured optimization.** Three-cycle, same-runner comparisons
  decide whether any profile or hashing change is retained.
- [ ] **M6 — current-tree qualification.** One attempt produces both 20-cycle
  role reports, the canonical artifact, and the transport and promotion
  receipts.

## Source map — one owner per contract

| Area | Responsibility in this effort |
|---|---|
| `crates/plurx-cluster-check/src/transport_recovery.rs` | Role runner, report validation, assembly, journals, and timings |
| `crates/plurx-cluster-check/src/lib.rs` | CLI dispatch and process lifecycle plumbing |
| `benchmarks/` | Closed role-report schema; canonical schema remains version 2 |
| `Makefile` | Contracts, role, smoke, and compatible full targets |
| `.github/workflows/ci.yml` | Independent jobs, aggregate evidence, and promotion cancellation policy |
| `validation/` | Same-run, first-attempt, exact-candidate receipts |
| `tests/operations/` and `tests/validation/` | Static scheduling, schema, receipt, and stale-candidate regressions |
| Maintained docs | Commands, output interpretation, budgets, and freeze procedure |

## M1 evidence — both roles now finish independently

Commit `b9547a2b03bc5bd5a499a5437b51575ef1c5f359` was archived without
repository metadata or credentials and built in the Rust 1.97.1 Linux image
on `nynuc`. One shared execution ID, `m1-linux-b9547a2b`, produced two closed
smoke reports:

| Role | Warmup | Measured cycles | Recovery | Envelope | Report SHA-256 |
|---|---:|---:|---:|---|---|
| Voter | 1 | 1 | 258,310 ms | Recorded, not asserted | `70653f81105b001c0a4d35960cce5270e7df8f792c25e15cdc89c7beeda92d3b` |
| Learner | 1 | 1 | 241,880 ms | Recorded, not asserted | `d03bf574e40b4aedbcf9eb7d6b752a21911855f43670333353fa474633ba78a3` |

Both roles used separate persistent source clusters, replaced node 4, moved
acknowledged writes through the existing external TLS writer, validated the
exact installed snapshot, shut down their children, passed the closed role
schema and shared semantic validator, and published atomically. The learner
completed admission before its warmup and measured cycle. These are M1 smoke
results only; neither can assemble or stand in for 20-cycle qualification.

The first acceptance attempt exposed two harness-boundary defects. Whole-
millisecond event ages sampled in separate processes could invert causal order
by one millisecond, and the continuous writer could advance the current
snapshot after the purge boundary had already been satisfied. The correction
allows only two milliseconds of cross-process timestamp quantization and
records the specific snapshot boundary whose purge was awaited; exact
transferred snapshot identity, bytes, digest, and transport completion remain
separate mandatory evidence. Focused package tests now contain 51 cases.

The node's 31 GiB `/tmp` tmpfs filled during the initial cold link. The exact
source-only rerun used `/var/tmp`, which had 108 GiB available. This is runner
capacity evidence, not a product or campaign failure.

## M2 implementation — local checks complete, Linux acceptance running

The common role runner now writes create-new `events.jsonl` and
`cycles.jsonl` journals plus an atomic terminal `summary.json` outside the
temporary cluster root. Records include named phase and learner-admission
subphase timings, every resource-sampling attempt, source and replacement
process identities, synchronized completed-cycle evidence, classified failure
context, and bounded cleanup results. A passing role report is still withheld
until orderly teardown and diagnostic summary publication succeed.

SIGINT, SIGTERM, and the optional role runtime limit now interrupt the active
role, drop its in-flight protocol exchange, and enter a 30-second cleanup path
that targets only registered PID/start-time identities. External cancellation
suppresses the second role in the compatible paired wrapper; an ordinary role
failure still does not. The pinned package check, formatting, Clippy with
warnings denied, and 58 focused transport-recovery tests pass locally. M2
remains open until both three-cycle Linux smokes and the deliberate bounded
failure produce the retained evidence listed above.

## Guardrails — this effort changes observability, not production transport

The 1,500-second recovery deadline, 60-second resource cleanup horizon,
three-second sampling interval, stability rule, image sizes, 20-cycle role
qualification, writer checks, and resource allowances remain unchanged. A
short smoke proves recovery and integrity only; it cannot assemble the
canonical artifact or qualify a candidate. Production heartbeat ownership and
transport lifecycle work remain a separate, measured follow-up.
