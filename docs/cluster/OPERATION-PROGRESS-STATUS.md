# Operation progress — wait mapping and model delivery

**Status:** implementation complete; final independent Fable review pending · **Updated:** 2026-10-21 · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **PR:** not opened; implementation and independent review precede qualification.

Companion to [the implementation plan](OPERATION-PROGRESS-IMPLEMENTATION.md). This phase extends the [completed bounded pilot](FORMAL-VERIFICATION-PILOT-STATUS.md) with a source-backed wait map and a finite-state model that permits repeated execution. It does not yet establish operation progress or cluster deadlock freedom.

## Ownership and current work

| Work | Owner | State |
|---|---|---|
| Waits, resources and completing tasks | GPT-6 Luna | 45 source anchors across 19 files committed; map interpretation corrections being applied |
| Property, fairness and abstraction design | Fable 5.1 | Advice and source-corrected addendum delivered; no coding or final review |
| Repeated-operation model and contract | GPT-6.1 Sol | Integrated; final static parse and 46 mapped anchors pass; no final qualification yet |
| Checker and existing CI integration | GPT-6 Luna | Committed and integrated; static Python parsing complete, unit checks await final review |
| Final independent adversarial review | Fresh Fable 5.1 reviewer | Complete candidate frozen for review; verdict pending |
| Qualification, PR and merge | Codex coordinator | Pending review and fixes |

Persistent work uses agent-owned clones. One accidental tooling write to the operator checkout was reversed with explicit approval and an exact captured patch; the affected file has no remaining diff. Subsequent editing is confined to explicit owned paths. The integration base is `c7d78b119b88fa6523824fdde5e84bc21e088a3d`. The Hermes `plurx-formal` board records live task ownership and comments.

The source map is [WAIT-MAP.md](../../verification/operation-progress/WAIT-MAP.md). No concrete application lock cycle was established in the inspected path; the model will exercise a deliberate cycle as a coverage control. Filesystem return and unknown lower-layer dependencies remain explicit premises. Lease expiry revokes authority but still drains renewal; it is not itself a cleanup deadline.

## Evidence and remaining acceptance

A pre-review development calibration explored 13,514 states in 2.778 seconds on its recorded earlier model hash. A subsequent TTL fallback correction received static parsing only. No final model, runtime or unit qualification is claimed. The prior pilot's results retain their original scope. New acceptance requires reviewed wait edges, explicit external premises, repeated-execution progress checks, deliberate deadlock/livelock controls, interpretable receipts, final independent review dispositions and the current candidate's affected main gate. Passing evidence is retained only where its governed inputs remain unchanged.

## Decisions and limits

The selected operation is a bound `library_scan` / `metadata_refresh` worker from claim through domain lease, bind, finite processing, fenced insert, replicated apply/reply and release, with renewal, cancellation and shutdown interactions. Source tracing will identify external I/O and backend assumptions. A continuing heartbeat alone does not count as useful progress. No arbitrary cluster theorem, automatic Rust refinement, product feature gate, watchdog or test deletion is authorized by this phase.

## Candidate and review boundary

The delivered package has a 422-line model, two positives, four temporal non-progress controls and three normal witnesses. One selected request, one full worker pipeline and one optional domain competitor are explicit finite bounds; the contention case uses zero faults and does not establish per-job starvation freedom. The single-worker positive permits one modeled fault and two retries. Cyclic retries, renewal and time observation have no lifetime event limit.

Both original and new package source mappings pass offline checks. The original pilot's model, configurations, mapping and toolchain bytes are unchanged. The shared checker now selects the package explicitly; the existing main/effort/full workflows preserve separate source impact and receipts. Final review receives the diff, outlines, model, contract and relevant source waits. Tests follow review and fixes.
