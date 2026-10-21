# Formal verification pilot — status

**Status:** implementation and bounded-source qualification complete · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **Integrated main:** `a06be68b9` · **Delivery and live merge gate:** [PR #1241](http://forge.lan:3000/noirr/plurx/pulls/1241)

M0–M3 artifacts and local qualification are complete. The PR records the current candidate, final CI gate and delivery state; a local result alone does not authorize promotion. GPT-6.1 Sol implemented the model and contract; GPT-6 Luna implemented tooling and mechanical fixes. Fable 5.1 supplied design advice and the independent final adversarial review, with no coding after Paul's clarification. Earlier authorship remains intact. All work used agent-owned clones; the owner's checkout was untouched.

## Results and measured cost

The strict `scripts/check-formal full` invocation on clean source `663b3aa548c636b39e1fbd8384ead577320cdfe7` completed all **16 configurations** in **431.97 seconds**. Model SHA-256 is `96d4e49281e562d43b412acd7c307afa7202b11763c24e511ad90cc3fd99bbaa`. TLC 1.7.4 (internal 2.19), Temurin 21.0.12+8, automatic workers and a 4 GiB Java heap were used. Process RSS can exceed the heap cap.

| Positive configuration | Coverage | Generated / distinct / queued | Depth | Seconds | Peak RSS bytes |
|---|---|---|---|---|---|
| `smoke.cfg` | 1 job; 6 events; S0–S7 | 2477235 / 459224 / 0 | 7 | 5.881 | 1895448576 |
| `baseline.cfg` | 2 jobs; 8 events; all safety seeds; S0–S7 | 152583926 / 18005241 / 0 | 9 | 323.076 | 4638982144 |
| `lifecycle.cfg` | 1 job; 11 events; fresh claim through facade reply; S0–S7 | 44680191 / 5481351 / 0 | 12 | 86.889 | 4484431872 |
| `recovery.cfg` | Conditional L1; separate fair recovery suffix | 151 / 78 / 0 | 9 | 0.848 | 198508544 |

Every positive check finished with zero states queued. Five deliberately defective controls produced their required counterexamples: ignored binding (S2), fresh replay fence (S4), cancellation revival (S5), stalled recovery (L1), and work before binding (S2). Seven normal-model witnesses reached three distinct time samples, four submissions, cancellation/renewal overlap, replay, bind refusal, rollback and displacement. A witness or expected violation is not a positive safety proof. Raw traces, per-configuration counts and exit codes are retained.

The focused Rust slice has **11 unique passing tests**: eight publication cases, the dual-owner contract, replay and cancellation. SQLite and the real three-voter Hiqlite harness executed. Ten results are retained from unchanged code; the only failed case was corrected and rerun alone. Its fixture had recomputed a time-sampled predecessor after installing the prior lease. Reusing the exact committed tuple fixed the fixture without changing production behavior. Contract-feature Clippy with `-D warnings` and pinned Rust 1.97.1 passed. The original failed run remains visible in the evidence.

The first CI preflight completed pinned Linux smoke in about 30 seconds, then refused Python unit discovery before running suites: the new tests used extension-based loader detection for an extensionless script. Luna replaced it with an explicit `SourceFileLoader`. Discovery now imports all tests. This test-loading correction changes no formal/runtime/runner input; the PR carries its actual unit and promotion-gate results.

## Scope, trust and remaining limits

The result covers bound `library_scan` / `metadata_refresh` workers calling `insert_item_fenced`, with two workers, two boots per worker, finite claim identities and the explicit configuration horizons. It checks sampled-time authority, stable binding through renewal, cancellation/displacement, replay and transaction/facade outcomes. It models one queue heartbeat, one domain operation, one displacement and one facade call with up to four allocator-conflict submissions. Domain-only and unfenced routes are classified in the source contract and are not silently given queue authority guarantees.

Safety samples are independent absolute classes0–3, including equality and backward observations; this is not physical commit-time expiry. L1 requires stable demand/ownership, time advancement past eligibility and eventual fair fresh delivery/application. Its `DeliverOldReply` is only a scheduling placeholder: L1 establishes eventual fresh current claim, not old-boot reply rejection. Safety claim/renewal reply guards are separate. Integer queue duration1 does not show ordinary forward-time expiry extension; real conformance covers renewal continuity.

The two-job baseline and single-job lifecycle do not compose into a long two-job lifecycle theorem. Larger ten-event two-job development exploration and the earlier four-worker eight-event timeout are retained as incomplete cost evidence, never passes. The final automatic-worker eight-event run completed; the eleven-event fallback was unnecessary. There is no arbitrary-fault/repeated-publication proof, model-to-Rust refinement theorem, Raft correctness proof, whole-cluster deadlock claim or test-removal authorization. Ordinary TLC deadlock checking is disabled at finite horizons.

Trust includes the reviewed source mapping and abstractions, safety quotient/symmetry argument, pinned checker/JDK, runner/receipt parser, Store transaction semantics and actual backend harness. Source drift detection covers 97 reviewed anchors across 33 files, not completeness of that dependency set. Hiqlite statement-zero rejection remains pinned by a real compatibility test to its observable error text. See [the contract](../../verification/durable-jobs/CONTRACT.md) and [package README](../../verification/durable-jobs/README.md) for exact boundaries and commands.

## Acceptance and durable evidence

| Milestone | Evidence |
|---|---|
| M0 · semantics and tools | Reviewed caller/maintenance/comparison map, tool hashes, explicit clock and binding differences |
| M1 · safety | Completed smoke, two-job baseline, full single-job lifecycle and named S2/S4/S5 defect traces |
| M2 · implementation bridge | Eight real publication cases plus ownership/replay/cancellation regressions; source and fixture equivalence receipts |
| M3 · recovery and reproducibility | Completed L1 and retry-stall counterexample, seven reachability witnesses, strict completion/drift/cleanup controls and measured costs |

Fable's final review of `88f73b08c` requested changes F1/F2. Lower models narrowed the recovery claim without changing TLA/configuration bytes, corrected the archive-test expectation and respected the stricter mapped smoke deadline (C8). [The review record](FORMAL-VERIFICATION-PILOT-REVIEW.md) preserves findings and dispositions; this is not a claim that the reviewer issued an approval verdict.

The PR retains `formal-verification-qualification-663b3aa5.zip`, SHA-256 `322f1de2fe771f8087d07fa4ae463435626da8d42dbab8abdb4c5a6e64106c26`: all 16 formal receipts/logs, inspected traces, runtime pass coverage, original failure/retry, source equivalence and independent review. CI retains its own smoke and promotion artifacts. The PR names the checked and landing candidates and verifies unchanged governed executable inputs for documentation-only bookkeeping. Gate results must be green on the actual candidate before merge. Existing tests stay in place; test reduction needs a separately measured proposal.
