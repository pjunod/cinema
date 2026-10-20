# Formal verification pilot — status

**Status:** M0 in progress; no formal result or milestone accepted · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **Base:** `3808640c79` · **PR:** [#1241](http://forge.lan:3000/noirr/plurx/pulls/1241) · **Merge readiness:** not ready

The pilot is in an agent-owned clone. The stable TLC 1.7.4 artifact and Temurin JDK 21.0.12+8 pins are prepared and verified. TLC help runs on the local macOS ARM64 JDK. This is toolchain feasibility evidence only; no protocol specification, model-check result or backend acceptance is claimed.

Fable task `t_7622df11` has resumed through an isolated copy of the existing
credential-routing fix. The installed route misclassified an identity-linked
API credential and recorded false local exhaustion. A direct API check
returned HTTP 200 as `claude-fable-5-1`; only that credential's local failure
state was reset. No shared installation was changed. M0 model work is running;
no model result is claimed.

The build proceeds as one related batch for a single PR to `main`. When that PR is complete, the coordinator requests the directed final adversarial Fable 5.1 review, fixes its findings, and runs the affected fast lane once. Only failed or invalidated evidence is rerun. Model checking, compile checks and static inspection are available during development; no unit suites run before the final review. The pilot adds no product feature gate.

The pinned Rust 1.97.1 compile-only baseline is running in an agent-owned
build directory. GPT-6 Luna is implementing the explicit checksummed
toolchain bootstrap. The draft PR allocates bookkeeping checks only; no
pre-review unit or conformance suites have been started.


## Current implementation and acceptance

| Scope | Owner/model | State | Evidence still owed |
|---|---|---|---|
| Toolchain pin/bootstrap | GPT-6 Luna | Committed `bf3c9c537`; fetch/resolve feasibility passed | Final synthetic tests and review |
| Pinned compiler loop | Codex coordinator | Rust 1.97.1 core `hiqlite-store` all-target check completed | Changed-source checks if Rust tests are added |
| Protocol/model | Fable 5.1 | Pinned worker resumed; M0 in progress | M0 freeze, M1 exploration/controls |
| Runtime diagnosis | GPT-6 Luna and designated control session | Resolved with existing dependencies and isolated committed fix | Complete protocol/model artifacts |
| Backend/runner/CI | Fable contract plus bounded builder | Not started | M2/M3 implementation, final qualification |
| Review/fast lane/merge | Coordinator and designated Fable reviewer | Not ready | Complete PR, review fixes, once-per-candidate evidence |

The authoritative blocking workflow on this source is
`main-fast-lane.yml`, whose preflight budget is 15 minutes. The older full
`ci.yml` is manual. M3 must provision/run the tool in the actual affected
fast-lane preflight rather than assuming manual full CI protects a merge.
