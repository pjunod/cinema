# Formal verification pilot — status

**Status:** M0 interface frozen; M1/M2/M3 implementation active; acceptance pending · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **Base:** `3808640c79` · **PR:** [#1241](http://forge.lan:3000/noirr/plurx/pulls/1241) · **Merge readiness:** not ready

The pilot is in an agent-owned clone. The stable TLC 1.7.4 artifact and Temurin JDK 21.0.12+8 pins are prepared and verified. TLC help runs on the local macOS ARM64 JDK. This is toolchain feasibility evidence only; no protocol specification, model-check result or backend acceptance is claimed.

Fable 5.1 task `t_7622df11` has frozen the seven-configuration model/runner interface and is implementing the protocol and source map. Fable task `t_5136edeb` owns the real backend insertion conformance bridge in a separate clone. GPT-6 Luna owns the bounded runner and strict receipts. The recovered API route is active; no model verdict or backend acceptance is claimed.

The first development smoke exploration was stopped after exceeding its 60-second budget: at the last progress sample it had 65,236,225 distinct states and 37,650,419 queued states. This is **incomplete**, not accepted evidence. The model author must reduce the state representation with documented semantic limits before qualification.

The build proceeds as one related batch for a single PR to `main`. When that PR is complete, the coordinator requests the directed final adversarial Fable 5.1 review, fixes its findings, and runs the affected fast lane once. Only failed or invalidated evidence is rerun. Model checking, compile checks and static inspection are available during development; no unit suites run before the final review. The pilot adds no product feature gate.

The pinned Rust 1.97.1 `hiqlite-store` all-target baseline check completed. The explicit checksummed toolchain bootstrap is committed. The draft PR allocates bookkeeping checks only; no pre-review unit or conformance suites have been started.


## Current implementation and acceptance

| Scope | Owner/model | State | Evidence still owed |
|---|---|---|---|
| Toolchain pin/bootstrap | GPT-6 Luna | Committed `bf3c9c537`; fetch/resolve feasibility passed | Final synthetic tests and review |
| Pinned compiler loop | Codex coordinator | Rust 1.97.1 core `hiqlite-store` all-target check completed | Changed-source checks if Rust tests are added |
| Protocol/model | Fable 5.1, `t_7622df11` | M0 model/runner interface frozen; M1 being built | Source contract, complete exploration and intended controls |
| Backend bridge | Fable 5.1, `t_5136edeb` | M2 author active in isolated clone | Real retained-state and ambiguous-reply conformance |
| Runner/receipts | GPT-6 Luna | Runner and CI contracts integrated; metadata tightening active | Reviewed source/config metadata, observed control output and final synthetic controls |
| CI integration | Codex coordinator | Existing preflights wired for affected smoke and explicit provisioning | Review and final CI qualification |
| Review/fast lane/merge | Coordinator and designated Fable reviewer | Not ready | Complete PR, review fixes, once-per-candidate evidence |

The authoritative blocking workflow on this source is
`main-fast-lane.yml`, whose preflight budget is 15 minutes. The older full
`ci.yml` is manual. M3 must provision/run the tool in the actual affected
fast-lane preflight rather than assuming manual full CI protects a merge.
