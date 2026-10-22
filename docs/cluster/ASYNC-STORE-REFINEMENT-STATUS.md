# Async driver and Store refinement — connect implementation steps to conditional progress

**Status:** open; source mapping and Fable design advice in progress. **Updated:** 2026-10-22 · **Owner:** Codex coordinator · **Branch:** `codex/async-store-refinement`. PR creation follows the initial design checkpoint; the coordinator owns integration, qualification, merge and cleanup.

Companion to [the production controller proof](RUST-ADMISSION-REFINEMENT-STATUS.md) and [the two-worker refinement](WORKER-REFINEMENT-STATUS.md). PR #1260 merged the exact-body controller safety proof as `1f30b4791476acb5d290c2f8e6ed38d07051f31d`. This phase addresses the asynchronous driver, durable Store admission and the separate fairness/enabledness gap. It does not presume a whole-cluster deadlock theorem.

## 1. Contract — actual implementation steps need a checked relation

The existing controller proves post-claim local safety. Its outcomes are environmental inputs, while the real driver owns guards, futures, locks, heartbeats and Store calls. A descriptive action table and selected successful schedules do not prove that every source execution simulates the two-worker model.

The design must identify a state relation, initialization obligation and source-step or stuttering obligation over the selected actual operation. Bind every executable source component, Store statement and model artifact to its inspected identity. Distinguish mechanically proved properties, finite exhaustive SQL/conformance results, reviewed correspondence and external premises. A second handwritten implementation cannot substitute for a proof of the production code.

## 2. Fairness — admission reachability changes the progress obligation

The old model deliberately permits simultaneous same-domain library claims. Production has separate node-local physical admission and a durable source-reader reservation gate that can refuse the second claim. Such refusal need not mutate the queue or acquire/yield a domain. The supported library-work enqueue route does not supply the live-demand waiter that would make that second reader eligible.

The new relation must account for these enabledness differences, reservation lifetime and request ownership. State exactly which weak or strong fairness assumptions apply to which actions and why a fair source trace supplies them. Time advancement, stable demand, eventual Store/quorum availability and external completion must remain visible environmental premises wherever they are required. Assuming the desired progress conclusion is inadmissible. Refusal/retry stuttering and a never-returning external wait must not be mistaken for a proved protocol deadlock or a completed progress proof.

## 3. Ownership — lower models build; Fable advises and reviews

| Surface | Owner | Boundary |
|---|---|---|
| Driver/Store source map and implementation | Lower-model source lane | Actual production boundaries; preserve guard, cancellation, join and transaction semantics |
| Refinement relation, fairness obligations and proof tooling | Lower-model proof lane | Mechanically check the selected relation; expose residual premises and strict negative controls |
| Design advice and final adversarial review | Fable 5.1 | Advice/review only; no implementation |
| Ledger, shared indexes, integration, receipts and merge | Codex coordinator | Exact-candidate qualification and visible PR state |

Concrete file ownership follows the source map and design advice before overlapping implementation begins. This is one coherent main-bound operation/proof slice; work will be batched into one PR.

## 4. Acceptance — review before one affected qualification batch

1. Establish the ordinary Rust 1.97.1 compiler loop before editing Rust and inspect scoped knowledge coverage.
2. Map the actual waits, Store linearization/ambiguity points and existing model assumptions; obtain Fable design advice.
3. Implement the reviewed source/refinement/fairness contract and record exact source bindings and remaining assumptions.
4. Freeze the candidate for independent Fable adversarial review; address required findings.
5. Run the smallest affected regressions and complete selected formal checks. Preserve passing evidence only on unchanged or explicitly reviewed compatible inputs; rerun failed or invalidated cases only. Require the current-candidate Main promotion gate.
6. Publish compact qualification evidence, merge the qualified tree with regression references, verify the landing tree and remove task-owned clones/builds.

Existing clustering tests remain. No runtime watchdog, product gate or optional-feature switch belongs to a proof or internal correctness change.

## 5. Current execution — starting facts and unresolved decisions

The fresh owned clone starts at main `1f30b4791476acb5d290c2f8e6ed38d07051f31d`. A lower-model source reader owns the bounded wait/Store/fairness investigation. Rust 1.97.1 is verified and the all-target daemon compiler loop is running. Fable's finite design brief will use the actual source report; its findings will determine the implementation architecture and precise theorem scope. No new tests or solver qualification have run, and no async/Store/fairness proof is claimed yet.
