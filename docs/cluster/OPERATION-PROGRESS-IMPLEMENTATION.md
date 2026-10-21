# Operation progress — source waits to a repeatable progress argument

**Status:** implementation started; design and source map pending · **Date:** 2026-10-21 · **Owner:** Codex coordinator.

[The status page](OPERATION-PROGRESS-STATUS.md) records current ownership, evidence and delivery. This is the next phase after [the bounded formal verification pilot](FORMAL-VERIFICATION-PILOT-PROPOSAL.md); its safety checks remain unchanged unless an explicit reviewed correction requires otherwise.

## 1. Establish the operation's real dependencies

Map the bound library worker from durable claim to domain acquisition, binding, finite item processing, fenced publication, replicated commit/reply and release. Include renewal, cancellation and shutdown. Every edge records its source anchor, awaited resource or event, resources held while waiting, the completing task and any dependency back toward the waiter. Distinguish examined code from external or unexamined implementation. Empty search results establish no absence claim.

## 2. Define useful progress without assuming away a cycle

Fable provides design advice on useful completion, stable-environment premises, legitimate idle/terminal states and fairness at independently scheduled dependency actions. The model must allow repeated execution without an artificial event horizon. Stable quorum, eventual communication and finite external processing are assumptions to state and justify at the source boundary. Lock release and response processing under investigation cannot be assumed as unconditional eventual success.

A finite-state abstraction is acceptable only with an explicit correspondence argument and limits for recycled identities, relative time and bounded in-flight work. No symmetry or state quotient is implicit. Distinguish absence of global deadlock, per-operation progress and starvation freedom.

## 3. Build one reviewable delivery

Lower-model builders implement the separate progress model and its focused tooling. Reuse the pinned TLC/JDK and established fail-closed receipt semantics where practical. Preserve the original pilot's executable inputs and existing tests. Add the progress package to affected-source selection and the existing preflight, with bounded runtime and durable traces. Concrete file ownership is assigned before parallel edits. This phase is a single reviewable delivery PR rather than separately promoted implementation tasks.

Controls must demonstrate detection of a modeled resource cycle and continuing transitions without useful completion; normal witnesses must show the intended operation and recovery paths are reachable. Exact controls, positive configurations and resource budgets follow source tracing and design advice. If a real defect is found, identify its root cause and make an architectural correction; do not add a timer or watchdog to hide it.

## 4. Review, qualify and merge

Freeze the complete candidate and give a fresh Fable 5.1 session the diff, touched-file outlines, wait map, property definitions and plan head for one adversarial review. Lower models address findings. Then run the affected fast lane and focused formal checks once, retain successful results on unchanged inputs, and rerun only failures or invalidated checks. Runtime changes require a working pinned compile loop before editing Rust.

The PR identifies coordinator ownership, current stage, review findings, qualification evidence and exact remaining merge conditions. Merge only with the current candidate's required main gate green. Record final claims and limitations, retain compact evidence, and remove expendable owned workspaces and caches after delivery. Deployment is outside this phase.
