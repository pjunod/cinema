# Progressive subtitle setup repair — install flow and merge state

**Date:** 2026-10-25
**Status:** live · **Updated:** 2026-10-25
**Owner:** Codex /root
**PR:** [#1283](http://forge.lan:3000/noirr/plurx/pulls/1283), branch `codex/subtitle-setup-repair` → `main`. The PR description and required checks are authoritative for current qualification and landing state.

This page tracks the setup repair as it moves from a reproduced failure to a reviewable, mergeable change. The broader P27 translation route and its acceptance limits remain in the [completion ledger](PROGRESSIVE-SUBTITLE-TRANSLATION-LEDGER.md).

## Current state

The live reproduction on the serving Linux node showed Install ending in **Invalid model operation response**. The server omitted the `purpose: "translation"` discriminator required by the setup client. A later refresh showed that the model download completed and was cached; its test was queued at `waiting_for_playback_capacity`. The download therefore succeeded, but the model was not qualified. An earlier model operation on another worker failed with `output_invalid`; its cause remains open and could involve storage rather than invalid speech.

The local repair adds the missing purpose field and an HTTP route regression. Setup now suggests the supported model without saving it, labels the action **Download and test**, explains failures and wait states, preserves model/language edits across catalog failure, refreshes an old status clock before a new action, and prevents duplicate active work. A controlled local browser fixture reached Ready using a simulated worker; that is UI fixture evidence, not production proof.

The scheduling cause is also repaired locally: model tests repeatedly tried to acquire the heavy-background semaphore instead of retaining a queued turn. Both generation and translation model tests now wait in that existing semaphore’s FIFO queue under their renewable job ownership and cancellation token. They publish `waiting_for_capacity` while waiting, reserve CPU only after acquiring the gate, and recheck foreground admission. Resource guards release CPU before handing over the heavy slot. Current background work finishes before the queued model test runs; this is fair handoff, not priority preemption.

| Work | Owner | State |
|---|---|---|
| HTTP response, translation setup and shared admission | Codex /root | Implemented; adversarial review found no actionable issues |
| Offline generation setup wording/status | GPT-6 Luna | Implemented; adversarial review found no actionable issues |
| Status page and indexes | GPT-6 Luna | Updated; root maintains merge state |
| Final adversarial review, fast lane and merge | Codex /root | Review complete; final qualification and landing tracked in #1283 |

## Scope and evidence

The repair covers installation and test setup behavior. Native translation, multiple audio streams, remux/encode delivery, and progressive same-language generation stay outside this change. The existing first-route acceptance remains bounded to direct web playback, one audio stream, and explicit spoken language.

UI, type, and paperwork checks passed before the current workflow was adopted. Current-main Mac Rust compilation fails on unrelated Linux-only `dv_runtime` imports. A Linux test attempt was stopped when the user requested that tests wait until after the final adversarial review. The complete candidate received an independent adversarial review with no actionable findings. Final qualification receipts and landing are recorded on the linked PR.

## Merge path

1. Implementation and the independent adversarial review are complete.
2. Run the named Rust regressions once during final qualification, plus the required fast lane. Retain passes and rerun only failed checks.
3. Merge the current candidate only when required checks are green; record the landing on the PR.

The linked PR carries the live owner, gate progress and eventual landing receipt. This page keeps the implementation and evidence boundaries stable across that workflow.
