# Formal verification pilot — status

**Status:** M0 in progress; no formal result or milestone accepted · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **Base:** `3808640c79` · **PR:** [#1241](http://forge.lan:3000/noirr/plurx/pulls/1241) · **Merge readiness:** not ready

The pilot is in an agent-owned clone. The stable TLC 1.7.4 artifact and Temurin JDK 21.0.12+8 pins are prepared and verified. TLC help runs on the local macOS ARM64 JDK. This is toolchain feasibility evidence only; no specification, model check, implementation or acceptance result is claimed.

Fable task `t_7622df11` is awaiting route recovery: its pinned Hermes invocation returned an API credits error before producing model work. The coordinator has requested the working route from the designated Fable control session; actual account balance is not inferred from that message. M0 remains open until source mapping, model boundary, tool behavior and feasibility are resolved. M1, M2 and M3 have not been accepted.

The build proceeds as one related batch for a single PR to `main`. When that PR is complete, the coordinator requests the directed final adversarial Fable 5.1 review, fixes its findings, and runs the affected fast lane once. Only failed or invalidated evidence is rerun. Model checking, compile checks and static inspection are available during development; no unit suites run before the final review. The pilot adds no product feature gate.

The pinned Rust 1.97.1 compile-only baseline is running in an agent-owned
build directory. GPT-6 Luna is implementing the explicit checksummed
toolchain bootstrap. The draft PR allocates bookkeeping checks only; no
pre-review unit or conformance suites have been started.
