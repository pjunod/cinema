# Rust worker controller — verify the post-claim lifecycle

**Status:** built; independently reviewed production controller; amendments and real-output fixtures implemented. Complete qualification and landing are tracked in [PR #1260](http://forge.lan:3000/noirr/plurx/pulls/1260). **Updated:** 2026-10-22 · **Owner:** Codex coordinator · **Branch:** `codex/admission-rust-refinement`. Coordinator owns integration, qualification and merge.

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

Native capability calibration established the tool route before implementation. The exact staged production controller compiled with the expected six-target inventory. Inventory compilation is not proof acceptance. The decision considers actual Rust semantics, proof scope, macOS arm64 support, installation/build costs and trusted adapters. No production feature gate, watchdog or testing-framework replacement belongs to this work.

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
- These are implementation-stage facts. Current review and focused runtime results are recorded below; complete six-target/three-control acceptance and landing remain tracked in PR #1260.

## 7. Accepted design and remaining boundary

Extract the existing post-claim acquire → bind → process/interrupt → ordered cleanup decisions into a pure, allocation-free Rust controller. The driver retains real tokens, leases, futures, locks and RAII guards and executes the returned effects. Preserve every existing test fault point relative to its operation. Do not change claim reconciliation, SQL, semaphore policy, the biased select or heartbeat behavior.

Preserve the source's signed `i64` timestamps/delays and post-release timestamp sampling with an explicit release-completed event. Use an explicit one-shot initialization transition; combine refusal timestamps with their outcome events so each transition consumes one complete event. Check all reachable prefixes and terminal paths. The finite acyclic protocol bounds accepted transitions; stale inputs do not imply eventual termination, and asynchronous waits are outside that bound. No assumed liveness conclusion or unchecked replacement body is admissible.

Six proof obligations cover totality/determinism, process gating, cleanup/resource retention, refusal and interruption settlement, saturating time arithmetic, and stale re-entry. Three strict mutation controls must exhibit assertion counterexamples for cleanup order, bind gating and overflow. Compiler errors, missing harnesses and unwind failures cannot count as successful mutation detection.

Retain the prior unchanged TLA model results at their original source identity. This slice leaves the durable source-I/O enabledness/fairness gap open and does not claim SQL correctness, Tokio/OS fairness, external-operation termination, or a cluster-wide deadlock theorem. Driver-to-effect correspondence is reviewed and tested, rather than machine-proved here.

- Source reconciliation corrected two illustrative design details before integration: release must finish before sampling the settlement clock, and the real timestamps/delays use signed `i64`, including signed saturation. The controller follows source behavior; proofs must quantify the actual types.

## 8. Integrated source and qualification plan

Production extraction commit `a14a269c354f17677098f6886d4effb631e1568b` passed pinned daemon all-target compilation and the normal workspace Clippy/format/syntax hook before integration as `c742287a8`. This source-integration checkpoint preceded runtime/proof qualification; subsequent results are recorded below.

The proof adapter stages complete production Rust files byte-for-byte into an isolated zero-dependency package and checks hashes before and after analysis. Its standalone manifest is a recorded packaging envelope, avoiding unrelated workspace dependency resolution. No second controller implementation is maintained. Ordinary production Rust compilation remains separate evidence.

The main fast lane gains an affected-input proof job with checksum-pinned native Kani and a scoped nightly; its result is required by the promotion gate. The complete registry remains six positive harnesses and three semantic controls. Strict per-target receipt reuse must preserve the original candidate and log while rerunning only missing, failed or invalidated targets. Final Fable review precedes affected qualification.

## 9. Independent review disposition

Fable 5.1 reviewed candidate `58a10429c57847ea4539ac763a01cb837d4de156` in task `t_e69efaca`, run 19, and **approved with amendments**. It found no defect in the production controller or driver operation order. It did not execute tests or proofs.

- A1: pin exact interruption effects in T4: loss/pressure revoke, cancel and join; normal completion joins only.
- A2: require outcome-producing effects to be last in their effect list, matching the driver's event handoff.
- A3: validate the result parser against an actual pinned-tool positive log and state a nonvacuity/reachability policy. Specification assertions must succeed; only unmarked compiler panic/expect checks may be unreachable. Preserve strict semantic counterexample attribution.
- A4: record actual first-run provisioning/solver timings and tool availability. Raise a timeout explicitly if needed; do not weaken proof bounds.
- A5: update this ledger to the delivered review and next acceptance steps.

The lower-model proof lane owns A1–A3; the coordinator owns A4/A5, exact-candidate qualification and merge. The controller and driver remain frozen. Fable requires another review only if their DAG or operation order changes. One post-review native positive target calibrates the parser against the real tool; the first full six-positive/three-control qualification is reserved for the Linux CI job.

A1/A2 proof assertions and A3 explicit parser policy are integrated without changing the production controller/driver. The real pinned T2 output is retained as a path-redacted fixture with its original log hash and exact transformation provenance. The five runtime schedules passed; full package acceptance and landing are tracked in PR #1260.

### Calibration and runtime results — first candidate

The five affected worker schedules passed on `1e4556f614bf6d6272896579a972782153d993ea` (1.87 seconds execution, 335 seconds cold compilation). The count/name wrapper returned a bookkeeping failure because its expected namespace omitted `state::`; the saved original log was validated against all five actual full names and the exact 5/0 result. No passing test was replayed.

The first native T2 calibration reported 242 successful checks and one unreachable check in 9.393 seconds solver time. Its aggregate acceptance remained false: the strict parser rejected a formatting-related unreachable overflow check, and ordinary Rust assertion macros had lost their custom messages in Kani's no-std analysis. The root-cause repair uses the pinned Kani assertion primitive with literal labels and checked result guards in the proof module. It does not loosen the parser or change production Rust. A second T2 calibration is necessary because those solver inputs changed.

Runtime retention is bound to the actual tested candidate. All ordinary production source/manifests remain unchanged; the proof module is excluded from normal compilation by `cfg(kani)`, confirmed by retained compiler dependency information. A source bridge records that distinction instead of relabelling the old execution as a new binary run.

### Literal-label calibration and exact compiler guard

The repaired T2 harness on `ecb9de29ffaa2b293c41168369a26a701335bacb` produced 246 successful properties, five successful literal specification assertions, and one unreachable compiler-formatting guard (7.391 seconds solver time). The original strict-parser rejection is retained. Reinterpreting that same immutable log with the repaired parser passes this single target without running the solver again; aggregate package acceptance remains false until all six targets and three controls qualify.

The only added unreachable exception is the exact `core::fmt::Arguments::from_str` left-shift guard at `library/core/src/fmt/mod.rs:820:38`, with its full emitted property name, description, location suffix and actual scoped rust-src SHA-256 checked against the manifest pin. It is compiler panic-message formatting, outside the production controller. A failing or undetermined guard, another source pin/location, or an unreachable T1–T6 specification assertion still rejects qualification. Default safety and unwinding checks remain enabled.

The committed real-output fixture preserves every non-path byte, including the tool's whitespace; its provenance states the original candidate, source/tool identities and path-only redactions. Parser-resume tests require exact solver inputs, command, tool identities and complete current-parser success before retaining a formerly rejected log, and preserve its original rejection. The final package run records native Linux provisioning and per-target solver timings in the PR evidence.

This phase's merge-time qualification and landing receipt live in [PR #1260](http://forge.lan:3000/noirr/plurx/pulls/1260). This ledger records the proof boundary and implementation decisions; the PR records the current candidate, required gate, retained runtime evidence and remaining work. No clustering test is removed.

### Current-main integration and evidence retention

Main advanced to `2838e05ad8bf1ed30f9c88db59caa7821bc3c087` during qualification preparation. The merge preserves both status entries and both source-map updates; all three current mapping validators report no source drift. Pinned Rust 1.97.1 all-target daemon compilation on the integrated source passed in 116 seconds.

The broad 1,491-file runtime identity no longer matches because upstream added playback-cache behavior. A narrower source inspection confirms that the tested worker/controller, fixture constructors, scan path and manager/admission helpers retain their exact bodies. The five fixtures construct managers directly and do not execute the changed AppState/cache-worker initialization. Their passes remain attributed to `1e4556f61`, with this explicit narrower bridge rather than a claim that a new binary ran. All 15 parser/CI-contract tests also passed (0.026 seconds); their exact runner, workflow and fixture inputs remain unchanged by main integration.

### First Linux qualification and retained solver evidence

Run 5811 (repository index 5782) analyzed candidate `6fb7996f2b44c98ed0c33a2eb0695a42c023d766`. T1–T5 passed strict qualification and all three controls produced their intended semantic assertion counterexamples. T6's solver reported success for all eight specification assertions, but aggregate acceptance correctly stayed false: the parser rejected an additional unreachable standard-library slice-equality loop-increment check. The original artifact, rejection and logs are preserved.

The repair pins that exact Effect slice-equality property, add-overflow description, `library/core/src/slice/cmp.rs:136:13` location and actual scoped rust-src SHA-256. This code is instantiated by generic Result equality but unreachable in the checked T6 comparison. Specification assertions still require the assertion property class and `SUCCESS`; no reachable overflow, undetermined result or unreachable specification assertion is accepted. A separate parser-source attestation records this additional check without changing or normalizing the solver's tool identity. Neither Rust nor proof source changed. Offline strict reparse of the immutable T6 log passes; complete Linux qualification remains a separate runner/gate result.

The first native Linux runner took 653.377 seconds: 558.304 seconds were reported solver time, leaving 95.073 seconds combined provisioning, identity checks, metadata/harness compilation and parsing overhead. An isolated provisioning timer was not recorded, so the residual is not represented as provisioning alone. The scoped exact nightly was available and all targets completed within their 600-second budget. T3 positive/control dominated at 269.013/220.643 seconds.

The CI adapter recovers the latest same-PR proof artifact, including a failed attempt's partial successes. It authenticates repository/PR/run/job/upload identity, bounds the ZIP and extracts only receipts, logs and archived source into an owned directory. The existing runner still decides reuse independently through source/tool/command/log/mutation checks and complete current-parser success. Artifact origin and original rejection remain in the output; incompatible or unavailable evidence requires fresh targets. Flat artifact layout is preserved. This is general evidence reuse, with no run or PR number embedded in the code. Delta review and final qualification are recorded in PR #1260.

### Final CI evidence review

Fable 5.1 task `t_ef8b9487`, run 20, reviewed the evidence adapter at `f0f35e2748cd18429cf6cba6f960a15baf69bba3` and approved with amendments. The coordinator integrated the lower-model fixes: actual Git bytes now bind the prior and current runner, workflow and tool manifest; reuse requires identical invocation digests or an exact, source-visible reviewed compatibility entry for the historical commit. The retained origin records those digests and the review basis. Empty or overlapping resume directories are rejected before tool setup, and same-PR/event filtering precedes detailed history requests. Conservative archive limits remain unchanged.

The compatibility entry for `6fb7996f2b44c98ed0c33a2eb0695a42c023d766` records the coordinator's historical-invocation attestation and the actual review chain; it does not claim Fable reviewed that exact intermediate commit. Such entries are review decisions, not solver theorems. Manual local resume additionally assumes a trusted result producer. The PR is ready for its new synchronize run; final qualification must show authenticated run 5811 origin and all nine retained target results before merge. Production Rust, proof bodies and tool manifest remain unchanged by these amendments.
