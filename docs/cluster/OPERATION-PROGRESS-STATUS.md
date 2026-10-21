# Operation progress — wait mapping and model delivery

**Status:** implemented, independently approved and formally qualified; live delivery and main gate are recorded in PR #1249 · **Updated:** 2026-10-21 · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **PR:** [#1249](http://forge.lan:3000/noirr/plurx/pulls/1249); reviewed and formally qualified; current promotion and landing state live in the PR.

Companion to [the implementation plan](OPERATION-PROGRESS-IMPLEMENTATION.md). This phase extends the [completed bounded pilot](FORMAL-VERIFICATION-PILOT-STATUS.md) with a source-backed wait map and a finite-state model that permits repeated execution. It does not yet establish operation progress or cluster deadlock freedom.

## Ownership and current work

| Work | Owner | State |
|---|---|---|
| Waits, resources and completing tasks | GPT-6 Luna | 45 wait anchors across 19 files committed; plus the Tokio lockfile pin gives 46 package anchors across 20 files |
| Property, fairness and abstraction design | Fable 5.1 | Advice and source-corrected addendum delivered; no coding or final review |
| Repeated-operation model and contract | GPT-6.1 Sol | Integrated; all nine outcomes qualified, with reviewed model/configuration bytes unchanged |
| Checker and existing CI integration | GPT-6 Luna | Committed and integrated; affected unit batch passed, including parser regressions |
| Final independent adversarial review | Fresh Fable 5.1 reviewer | APPROVE on `3da7e0a34`; no blockers; all 12 comments dispositioned |
| Qualification, PR and merge | Codex coordinator | PR #1249 owns live promotion/landing; all nine formal outcomes and 1,862 Python methods have passing evidence |

Persistent work uses agent-owned clones. One accidental tooling write to the operator checkout was reversed with explicit approval and an exact captured patch; the affected file has no remaining diff. Subsequent editing is confined to explicit owned paths. The initial integration base was `c7d78b119b88fa6523824fdde5e84bc21e088a3d`; the live PR records the current integrated main. The Hermes `plurx-formal` board records live task ownership and comments.

The source map is [WAIT-MAP.md](../../verification/operation-progress/WAIT-MAP.md). No concrete application lock cycle was established in the inspected path; the model will exercise a deliberate cycle as a coverage control. Filesystem return and unknown lower-layer dependencies remain explicit premises. Lease expiry revokes authority but still drains renewal; it is not itself a cleanup deadline.

## Evidence and remaining acceptance

A pre-review development calibration explored 13,514 states in 2.778 seconds on its recorded earlier model hash. A subsequent TTL fallback correction received static parsing only. All nine configurations now have valid results; CI run 5724 also completed 1,862 Python methods with no fixture errors. The main promotion and landing receipt are maintained in the linked PR. The prior pilot's results retain their original scope. New acceptance requires reviewed wait edges, explicit external premises, repeated-execution progress checks, deliberate deadlock/livelock controls, interpretable receipts, final independent review dispositions and the current candidate's affected main gate. Passing evidence is retained only where its governed inputs remain unchanged.

## Decisions and limits

The selected operation is a bound `library_scan` / `metadata_refresh` worker from claim through domain lease, bind, finite processing, fenced insert, replicated apply/reply and release, with renewal, cancellation and shutdown interactions. Source tracing will identify external I/O and backend assumptions. A continuing heartbeat alone does not count as useful progress. No arbitrary cluster theorem, automatic Rust refinement, product feature gate, watchdog or test deletion is authorized by this phase.

## Candidate and review boundary

The delivered package has a 422-line model, two positives, four temporal non-progress controls and three normal witnesses. One selected request, one full worker pipeline and one optional domain competitor are explicit finite bounds; the contention case uses zero faults and does not establish per-job starvation freedom. The single-worker positive permits one modeled fault and two retries. Cyclic retries, renewal and time observation have no lifetime event limit.

Both original and new package source mappings pass offline checks. The original pilot's model, configurations, mapping and toolchain bytes are unchanged. The shared checker now selects the package explicitly; the existing main/effort/full workflows preserve separate source impact and receipts. Fable reviewed the complete model/configuration/contract and tooling diff plus source excerpts, approving with no blockers. Index/backlog rows and unmodified classifier portions were outside its stated coverage. Tests follow the comment batch.

## Independent review and comment dispositions

Fresh non-author Fable 5.1 (`claude-fable-5-1`), Hermes task `t_09b0b514`, approved source `3da7e0a340d935dc8e884774511c6aa4bc068512` against `c7d78b119b88fa6523824fdde5e84bc21e088a3d`, model `5afb443975ec202d60eda641bdb2b0d29c7355111a43f96cf1529a82675fae99`. Its full report and explicit coverage limits will accompany qualification evidence. Approval is static review, not a test result.

| Comment | Disposition |
|---|---|
| C1 · dependency drift | Retain whole-file lockfile pin; document deliberate mapping review on dependency changes. |
| C2 · temporal translator | Confirmed by both completed positive configurations; no syntax rewrite needed. |
| C3 · local revocation abstraction | Disclose omitted late-commit timing; local revocation is not a durable backend check. |
| C4 · effort compile-only | Explicitly name the existing pilot behavior change below. |
| C5 · shape invariants | Label by-construction consistency guards separately from implementation evidence. |
| C6 · release limit | Name a modeled limit; all-five-failures and TTL fallback are unexercised at current budgets. |
| C7 · receipt governance | Document the narrower formal executable input set and separately visible dirty paths. |
| C8 · wider retry budgets | Registered bounds only; widening requires a reviewed invariant/action change. |
| C9 · anchor counts | 45 wait anchors/19 files, plus one Tokio lockfile row: 46 package anchors/20 files. |
| C10 · cancellation | Scan-phase cooperative witness only; no all-await or shutdown claim. |
| C11 · global deadlock | Always-enabled Tick prevents a global deadlock result; temporal properties provide the actual check. |
| C12 · item timeouts | WorkFault abstracts attempt control flow; no intermediate catalogue rollback claim. |

The effort workflow now applies `compile_only` to toolchain fetch and both package smokes, including the existing pilot smoke that previously ran in that mode. Both offline mappings still run. This is an explicit compile-only CI behavior change; normal main qualification continues to run affected smokes.

## Qualification started after review and clarifications

The first paperwork batch passed 13 of 14 rules (76 of 77 methods); the status page length alone failed. Removing two surplus blank lines fixed it, and only `StatusPageShapeCase.test_status_md_stays_short` reran and passed. The remaining local paperwork passes are retained.

`contention.cfg` passed complete exploration. The first lock-cycle control produced the correct temporal lasso, but the inherited runner rejected TLC's annotated `Back to state` line. Luna fixed the strict matcher and added positive/malformed-format regressions; only the failed control reran. The passing contention raw log was verified against identical model/config/source/mapping hashes and reclassified with the final runner without repeating TLC. The parser change touches only temporal-violation recognition, leaving the positive path unchanged.

The four controls and reconciliation/cancellation witnesses then completed with their expected named outcomes. These are seven locally qualified configurations, including retained contention; CI completed the remaining positive and success witness on identical governed inputs. C2's temporal-translator concern is settled by the completed positive contention run. Full traces, original parser rejection, retry and retention receipt are preserved with qualification evidence. No product runtime source changed.

The source tracer inspected the actual lasso states for all four controls: the added token/publication lock cycle, scanner scheduling starvation despite continuing heartbeat, a busy walker retaining admission, and a durable running row after local cleanup without expiry. It also inspected witness final states: committed result plus terminal waiter plus reconciliation/cleanup, and cancelled cleanup with the request still pending. The first parser-rejected trace remains explicitly incomplete; the fixed rerun is the counted evidence.

## Qualified results and their limits

The nine required outcomes comprise **two exhaustive positives, four deliberate temporal failures and three normal reachability witnesses**. Qualifying TLC execution totals **13.431 seconds** across local macOS and CI Linux runs, with separate pinned platform JDK hashes and common TLC/model/configuration/source hashes. This total excludes provisioning, wrapper overhead, the original parser-rejected invocation and development calibration.

| Positive | Generated / distinct / queued | Depth | TLC seconds |
|---|---|---|---|
| Selected request progress | 51,143 / 13,514 / 0 | 122 | 3.808 |
| Domain contention, cleanup/retirement only | 22,549 / 5,260 / 0 | 64 | 1.882 |

The first positive establishes eventual durable result and eventual attempt cleanup under the stated finite model and dependency premises. The contention positive establishes selected-attempt cleanup/retirement, not per-job starvation freedom. The controls and witnesses verify the model's sensitivity and reachable paths; they are not additional exhaustive positives. Always-enabled Tick makes global deadlock detection vacuous: temporal progress properties are the actual acceptance checks.

Existing tests remain. There is no cluster-wide deadlock theorem, Rust refinement, Raft-liveness proof, numeric deadline or general graceful-shutdown guarantee. Whole-file dependency pins deliberately require mapping review after lockfile changes. Normal main qualification uses affected smokes; effort compile-only validates both mappings while skipping toolchain fetch and both smokes.

The PR carries the exact current candidate, authenticated CI artifacts, formal receipt/input equivalence, full independent review and final landing receipt. Keeping that live state in the PR avoids claiming a future merge hash in this immutable source snapshot.
