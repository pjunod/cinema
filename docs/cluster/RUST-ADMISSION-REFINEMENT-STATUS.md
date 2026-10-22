# Rust admission refinement — prove the production start protocol

**Status:** open; design selected; production controller implementation in progress · **Updated:** 2026-10-22 · **Owner:** Codex coordinator · **Branch:** `codex/admission-rust-refinement` · **PR:** opening; coordinator owns implementation integration, qualification and merge.

Companion to [worker refinement](WORKER-REFINEMENT-STATUS.md), which delivered two complete model workers and selected Rust conformance. This phase starts a machine-checked implementation proof; it does not claim complete Rust simulation or cluster-wide deadlock freedom.

## 1. Scope — a production protocol, not a parallel implementation

The inspected source is `f09e6f732e3e3db8b422fcdacda6165716c7c77b`.
The actual path obtains local heavy/CPU admission, confirms a durable queue claim, acquires a domain lease, binds the queue/domain authority, and only then starts processing. Refused claims drop their candidate admission without claiming, yielding or acquiring a domain. Refused binds release the acquired domain, yield, retire and finally drop admission. Claim/release replies can be ambiguous; a timeout does not establish rollback.

Authoritative source-reader serialization is implemented by transactional SQL and reservation triggers. A second Rust copy of that SQL is not an implementation proof. Select a small deterministic controller whose decisions the production driver actually executes. Keep real tokens, RAII guards, locks, asynchronous operations and durable Store behavior in their existing ownership layer unless the design identifies a necessary architectural extraction.

## 2. Proof obligations — bind each claim to its actual scope

The intended first milestone proves initialization and one-step refinement of the actual production core into an explicit admission/start contract. Processing must require confirmed claim, acquired domain and accepted bind; refusal/ambiguity must select the correct next action; stale events must not restart processing; required ownership must survive until its cleanup boundary. The selected theorem is local safety of the production-consumed post-claim lifecycle controller: every reachable prefix respects process gating and resource ownership; every terminal path emits ordered cleanup with admission dropped last. Arbitrary stale events are rejected without effects. This is not a fair simulation theorem. Entry requires a confirmed claim and retained admission, which remain driver/Store preconditions.

The proof must analyze the actual shared Rust body and reject skipped targets, unchecked substitute bodies, unjustified loop/unwind truncation and assumed conclusions. A copied implementation with matching examples does not qualify. Ordinary pinned Rust compilation and proof-tool analysis are separate evidence, each tied to the exact source.

Fair-execution transfer, Tokio/OS scheduling, Store/SQL semantics and external completion remain explicit obligations. Local step safety does not discharge them. The prior model deliberately permits more claim overlap than the implementation; restricting enabledness requires a fairness argument before transferring its liveness result. This phase must state whether it resolves that gap or leaves it open.

## 3. Tool feasibility — select a bounded native route

Selected tool: Kani 0.68.0 over the same ordinary Rust step body called by the production driver. A scoped native macOS arm64 calibration passed a symbolic-input check, detected an intentionally false assertion and rejected insufficient loop unwinding. Those are tool capability checks, not Plurx proof acceptance. The proof compiler is nightly-2026-08-21; CBMC is 6.11.0. The selected frontend and all runtime dependencies must be pinned, checksummed and available on the intended platform. The ordinary build remains Rust 1.97.1. A proof frontend's different compiler cannot substitute for that build.

A short source-bound feasibility spike precedes implementation. The decision considers actual Rust semantics, proof scope, macOS arm64 support, installation/build costs and trusted adapters. No production feature gate, watchdog or testing-framework replacement belongs to this work.

## 4. Ownership — one coherent reviewable protocol slice

| Surface | Owner | Boundary |
|---|---|---|
| Real start/cleanup controller and production call sites | Lower-model source implementer | Driver must consume decisions; preserve guards, tokens, timing and refusal distinctions |
| Proof harness/specification, pinned verifier runner and source binding | Lower-model proof implementer | Work against the frozen core interface; no competing call-site edits |
| Architecture advice and sole final independent adversarial review | Fable 5.1 | Advice/review only; independent final context after implementation is frozen |
| Ledger, indexes, integration, receipts, PR and merge | Codex coordinator | Own shared files and exact-candidate qualification |

Supporting lanes integrate into one main-bound PR for this single protocol slice. They do not independently merge overlapping product changes. Freeze interface ownership before parallel coding; review the integrated diff once it is ready to merge.

## 5. Acceptance — review, qualify, land and clean up

1. Record source map, knowledge coverage and verified compiler loop before Rust edits.
2. Obtain Fable's finite design advice and demonstrate the selected tool can analyze the intended actual Rust body.
3. Implement the production-consumed core and exact-source proof with explicit remaining assumptions.
4. Freeze the candidate for a fresh independent Fable review; address findings.
5. Run the smallest affected production regressions, complete proof targets and strict negative controls. Preserve successes on unchanged inputs; retry failed or genuinely invalidated cases only. Run existing required promotion gates.
6. Merge the qualified tree with regression references, preserve compact evidence and remove owned clones/builds after verified landing.

Retain prior TLC results as claims about their actual unchanged models/configurations. Changed Rust source requires new implementation correspondence and applicable regressions; do not relabel old receipts as execution on new Rust.

## 6. Execution — source and compiler facts

- Source exploration confirms local physical admission and transactional durable admission are separate gates; token-mutex to publication-read-lock order and refusal/ambiguity paths must be preserved.
- Engineering-knowledge queries report coverage `none` at snapshot `a1e4279ddec7b9cf98f9d2ecaef337f6cd1ab54d072a9b5aa6248998520ec9c1`. That means unknown coverage, not absent obligations. Requery actual proposed changes before review/push.
- Pinned `rustc 1.97.1` and `cargo check -p plurxd --all-targets` passed on the source baseline in 182 seconds before edits. This is compile evidence, not test/proof qualification.
- Ripwire initially refused because its binary was absent; scoped source ranges supplied navigation. `make hooks` subsequently installed the normal hooks and Ripwire in the owned clone.
- Fable 5.1 produced the design report on task `t_51737056`, run 17. The run exhausted its eight-turn budget after writing the report; the coordinator accepts its concrete advice with the clarifications below. This is design advice, not final independent review.
- GPT-6.1 Sol owns the new zero-dependency `plurx-worker-control` crate and production driver conversion in an isolated builder clone. Proof tooling is a separate lower-model lane after the interface is stable.
- No implementation proof, post-change unit qualification or merge is claimed yet.

## 7. Accepted design and remaining boundary

Extract the existing post-claim acquire → bind → process/interrupt → ordered cleanup decisions into a pure, allocation-free Rust controller. The driver retains real tokens, leases, futures, locks and RAII guards and executes the returned effects. Preserve every existing test fault point relative to its operation. Do not change claim reconciliation, SQL, semaphore policy, the biased select or heartbeat behavior.

Preserve the source's post-release timestamp sampling with an explicit release-completed event. Use an explicit one-shot initialization transition; combine refusal timestamps with their outcome events so each transition consumes one complete event. Check all reachable prefixes and terminal paths. The finite acyclic protocol bounds accepted transitions; stale inputs do not imply eventual termination, and asynchronous waits are outside that bound. No assumed liveness conclusion or unchecked replacement body is admissible.

Six proof obligations cover totality/determinism, process gating, cleanup/resource retention, refusal and interruption settlement, saturating time arithmetic, and stale re-entry. Three strict mutation controls must exhibit assertion counterexamples for cleanup order, bind gating and overflow. Compiler errors, missing harnesses and unwind failures cannot count as successful mutation detection.

Retain the prior unchanged TLA model results at their original source identity. This slice leaves the durable source-I/O enabledness/fairness gap open and does not claim SQL correctness, Tokio/OS fairness, external-operation termination, or a cluster-wide deadlock theorem. Driver-to-effect correspondence is reviewed and tested, rather than machine-proved here.
