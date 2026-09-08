# Transport recovery CI — implementation status and evidence

**Status:** building M1 · **Owner:** Codex · **Updated:** 2026-09-08

Companion to
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) and
[TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md)
(what the retained leak statistic asserts) — this page says what has actually
been built, reviewed, and proved. It is updated with each milestone so a green
checkbox means retained evidence, not intent.

## Current position — M0 is complete and the old campaign is paused

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
- [ ] **M1 — independent role reports.** Typed plan, strict CLI, shared
  validation, compatible aliases, and canonical assembly.
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

## Guardrails — this effort changes observability, not production transport

The 1,500-second recovery deadline, 60-second resource cleanup horizon,
three-second sampling interval, stability rule, image sizes, 20-cycle role
qualification, writer checks, and resource allowances remain unchanged. A
short smoke proves recovery and integrity only; it cannot assemble the
canonical artifact or qualify a candidate. Production heartbeat ownership and
transport lifecycle work remain a separate, measured follow-up.
