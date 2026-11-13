# Storage domains — implementation and qualification status

**Status:** open · **Owner:** Codex · **Updated:** 2026-11-13

Companion to [the implementation plan](STORAGE-DOMAINS-DEFERRED-APPLY-PLAN.md).
This page records what is built and what still needs evidence. A saved
configuration becomes effective only after both capacity and exact job
ownership remain valid. Saving itself never waits for runtime state.

| Milestone | State | Evidence or next action |
|---|---|---|
| Atomic pending schema and migration | Implemented; unqualified | v131, both backends, strict shape verification |
| Save/apply and consistent diagnostics | Implemented; unqualified | Final review and focused store contracts pending |
| HTTP and existing scheduler integration | Implemented; unqualified | Admin-only save; voter applies; no new task/timer |
| Settings status and pending values | Focused regressions pass | 89 cases passed initially; corrected the remaining fixture expectation and reran that case successfully |
| Adversarial review | Findings addressed | Protected edits made during GET/PUT; reviewer accepted targeted disposition; atomic ownership logic had no blocker |
| Focused regressions and fast lane | Running | Rust focused checks use the shared external cache; retry only failed checks |
| Merge and deployment | Pending | Merge only qualified candidate; operator deploys |

## Decisions and limits

1. **No feature toggle.** This corrects configuration lifecycle; the pending
   status reports progress rather than controlling enablement.
2. **Running jobs retain exact resource keys.** A mapping change waits for
   jobs in affected libraries, including paused jobs, because older nodes
   re-derive keys during renewal and claim acknowledgement.
3. **Custody retains its charge.** Bare `source_io` still conflicts with every
   named domain, so such readers need not block a bare-to-named change. A
   reader holding a disturbed named key continues to block application.
4. **One atomic application.** The replicated command and trigger settle the
   pending revision and applied rows together. Older-node changes supersede
   pending revisions rather than silently overwriting them.

## Execution log

| Date | Owner | Result |
|---|---|---|
| 2026-11-13 | Codex | Claimed implementation after plans PR #1563 merged; schema and UI delegated to lower-cost agents; tests deferred until final review. |

| 2026-11-13 | Codex | Integrated current main after offload claimed v130; pending domains moved to v131, with ordered strict fixture rewind. Affected schema/store checks are requalified on this tree. |
