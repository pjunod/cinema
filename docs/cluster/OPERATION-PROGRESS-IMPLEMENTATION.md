# Operation progress — source waits to a repeatable progress argument

**Status:** built, independently approved and formally qualified; delivery tracked by PR #1249 · **Date:** 2026-10-21 · **Owner:** Codex coordinator.

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

## 5. Selected design and model ownership

[The source wait map](../../verification/operation-progress/WAIT-MAP.md) and 45 wait anchors cover 19 files; the Tokio lockfile pin makes 46 package anchors across 20 files. Fable 5.1 supplied design advice and a correcting addendum: expiry revokes authority but both heartbeat cancellation and expiry still await renewal. Cleanup depends on the actual lock queue and returning dependencies; assuming a lease deadline directly frees the lock would be unsound.

The new `verification/operation-progress` package preserves the original pilot. Sol owns its model/configurations/mapping and contract. Luna owns the shared runner, focused runner tests, existing CI workflows and validation point paths. A separate Luna source tracer owns the wait map and source-anchor data. Coordinator alone owns these plan/status files, indexes, backlog and PR.

The model represents one selected request's full operation and a bounded competing worker on the same domain, rather than asserting starvation freedom for arbitrary jobs. Separate properties cover durable request usefulness, observed reconciliation, attempt cleanup and resource release. A yield is not useful completion. Lock request/grant, held resources, dependency completion and timeout/late-commit outcomes are distinct steps. Fairness does not assume completion of the operation or release of a held application lock. Cancellation cleanup is covered; graceful daemon shutdown is excluded because the inspected worker loops do not receive the shutdown token.

The fixed package has two positive configurations, four deliberate non-progress controls and three normal reachability witnesses. Controls cover an added reverse lock edge, continued heartbeat without worker progress, withheld external return and lost settlement without expiry recovery. Normal witnesses cover success, committed-but-lost acknowledgement reconciliation and cooperative cancellation cleanup. TLC deadlock detection stays enabled; local stuck operations may require temporal checks because unrelated transitions can continue. No event horizon, symmetry or state quotient is planned.

Smoke uses the same small positive and success-witness inputs as full qualification. Final evidence may combine current-input CI smoke receipts with individual receipts for remaining configurations, avoiding a second execution of a configuration that already passed. No new runtime behavior change is planned without a concretely identified source defect.

Fable approved the complete candidate without blocking findings. The status ledger records twelve comments and their dispositions. The clarification batch changes documentation/registry metadata only; the reviewed TLA and configuration bytes remain frozen. Effort compile-only mode now skips toolchain provisioning and both package smokes, while still validating both source mappings.

Qualification completed all nine expected formal outcomes and the affected Python batch (1,862 methods) in CI run 5724. The status ledger distinguishes the two exhaustive positives from controls/witnesses and links the live promotion/landing receipt.
