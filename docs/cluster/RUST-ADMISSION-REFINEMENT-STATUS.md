# Rust admission refinement — prove the production start protocol

**Status:** open; source and tool feasibility established, design advice pending · **Updated:** 2026-10-22 · **Owner:** Codex coordinator · **Branch:** `codex/admission-rust-refinement` · **PR:** opening; coordinator owns implementation integration, qualification and merge.

Companion to [worker refinement](WORKER-REFINEMENT-STATUS.md), which delivered two complete model workers and selected Rust conformance. This phase starts a machine-checked implementation proof; it does not claim complete Rust simulation or cluster-wide deadlock freedom.

## 1. Scope — a production protocol, not a parallel implementation

The inspected source is `f09e6f732e3e3db8b422fcdacda6165716c7c77b`.
The actual path obtains local heavy/CPU admission, confirms a durable queue claim, acquires a domain lease, binds the queue/domain authority, and only then starts processing. Refused claims drop their candidate admission without claiming, yielding or acquiring a domain. Refused binds release the acquired domain, yield, retire and finally drop admission. Claim/release replies can be ambiguous; a timeout does not establish rollback.

Authoritative source-reader serialization is implemented by transactional SQL and reservation triggers. A second Rust copy of that SQL is not an implementation proof. Select a small deterministic controller whose decisions the production driver actually executes. Keep real tokens, RAII guards, locks, asynchronous operations and durable Store behavior in their existing ownership layer unless the design identifies a necessary architectural extraction.

## 2. Proof obligations — bind each claim to its actual scope

The intended first milestone proves initialization and one-step refinement of the actual production core into an explicit admission/start contract. Processing must require confirmed claim, acquired domain and accepted bind; refusal/ambiguity must select the correct next action; stale events must not restart processing; required ownership must survive until its cleanup boundary. The exact theorem, reachable-state invariant, reference relation and supported events are frozen after Fable's design advice.

The proof must analyze the actual shared Rust body and reject skipped targets, unchecked substitute bodies, unjustified loop/unwind truncation and assumed conclusions. A copied implementation with matching examples does not qualify. Ordinary pinned Rust compilation and proof-tool analysis are separate evidence, each tied to the exact source.

Fair-execution transfer, Tokio/OS scheduling, Store/SQL semantics and external completion remain explicit obligations. Local step safety does not discharge them. The prior model deliberately permits more claim overlap than the implementation; restricting enabledness requires a fairness argument before transferring its liveness result. This phase must state whether it resolves that gap or leaves it open.

## 3. Tool feasibility — select a bounded native route

Candidate tools are Kani over the real Rust body, Verus with an enforced production-body identity mechanism, and Aeneas/Charon translation to Lean. The selected frontend and all runtime dependencies must be pinned, checksummed and available on the intended platform. The ordinary build remains Rust 1.97.1. A proof frontend's different compiler cannot substitute for that build.

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
- Fable design task `t_51737056` is queued; no design approval, implementation proof, unit qualification or merge is claimed yet.
