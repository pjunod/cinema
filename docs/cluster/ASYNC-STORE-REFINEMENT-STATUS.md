# Async driver and Store refinement — conditional progress of a selected operation

**Status:** built, unqualified; review amendments built; final delta review and qualification pending. **Updated:** 2026-10-22 · **Owner:** Codex coordinator · **Branch:** `codex/async-store-refinement` · [Draft PR #1263](http://forge.lan:3000/noirr/plurx/pulls/1263).

Companion to [the production controller proof](RUST-ADMISSION-REFINEMENT-STATUS.md) and [two-worker refinement](WORKER-REFINEMENT-STATUS.md). This phase connects production async control and actual SQL transitions to a finite two-worker model. The first independent Fable review covered the production driver and adapter. The Store and tooling review continuations are complete with required amendments. Those amendments are built; final delta review, graph closure, regressions and solver qualification remain pending. It does not establish complete fair Rust simulation or cluster-wide deadlock freedom.

## 1. Implementation — source-derived control and durable transitions

Production library workers consume the shared `plurx-worker-control::library_driver` interpreter through a thin Tokio environment adapter. One owned work future retains its publisher and domain reference through interruption; cancellation precedes joining that same future, then publisher drop, local domain release attempt, queue settlement/retirement and physical admission drop. The existing lifecycle/proof bodies are unchanged. The shared selector polls queue loss, domain loss, work, then pressure ticks. A ready tick cannot hide a ready work result; no concrete Tokio starvation incident is claimed from a coarse polling schedule.

Driver inventory invokes actual `drive`: 48 reachable control states, 52 accepted edges and 572 explicit rejections across 13 reply classes. Those are inventory counts, not temporal acceptance. Control takes reply tags without numeric payloads; tag independence of the unchanged lifecycle guards is a reviewed composition lemma, not a new all-integer machine proof. Runtime assertions and representative arithmetic tests cannot replace that lemma or the existing arithmetic proof.

The Store harness calls actual validated methods, SQL, migrations and triggers. A sequential savepoint/replay explorer checks full-row identity, rollback, operation preimages and independent invariants. Source, compiled migrations and installed schema are bound. Its 15 core cases, sampled backend history and 124 predicate rows are defined but unexecuted. The exact Rust input registry contains 33 commands, including three new real retirement conformance cases and the existing cancellation-revision case. Actual closure counts and runtime remain unknown. The sampled history requires completed markers for memory SQLite, file SQLite and real three-voter Hiqlite; its fault deliberately discards an applied acknowledgement and is not a real transport-timeout injection.

## 2. Domain — exact concrete bounded input protocol

Domain D contains two selected jobs with paired requests/waiters, one shared root, two nodes with one physical permit each, and a foreign domain holder. There are at most two successful claims per job, one renewal per claim and one domain acquisition per owner. Four selected attempt identities and two mismatch identities are distinct. Allocator exhaustion is outside D because the attempt budget is reached first. There are no demand re-arrivals.

All integers remain concrete. Only opaque generated job UUIDs are renamed. Clocks advance through actual current/captured authority-expiry neighbours, without a numeric quotient or fixed horizon. Ordinary inputs use the latest captured revision per claim, matching queue/domain owners, library Yield/Retry/Cancel settlements and ownership-conditioned node-domain acquisition. Historical revisions, wrong owners and Fail/Stop are separate conformance partitions. Every input restriction and cutoff is explicit; closure is over D only. A 20,000-state cap aborts without a closed receipt.

The coordinator selected this narrower domain because first-appearance/rank normalization has no demonstrated preservation argument for increment, MAX, stale identities and clock gaps. Generated SQL tables are source-derived semantics, not independent oracles for themselves. Catalogue item publication remains an opaque operation/conformance boundary.

## 3. Fairness — separate cleanup from selected success

[The exact contract](../../verification/async-driver-refinement/CONTRACT.md) defines these conditional checks:

| Claim if qualification succeeds | Required boundary |
|---|---|
| Finite product safety | Actual driver/SQL graphs, explicit environment and independent raw-row/transition assertions |
| Attempt cleanup | `[]WithinEpisodeCapacity => AttemptCleanup`; matching SQL inputs remain available, with named runtime dispatch/application/reply/return and work/join return fairness |
| Both fixed requests complete | Stable node/job pairing, recurring physical opportunity, strongly fair admission and a separate durable claim-win premise; available successful operations/work before budgets or leases are exhausted |
| Nonvacuity | A reachable witness with both durable requests freshly done and both local physical admissions dropped |

Selected success still permits an applied completion acknowledgement to be lost and mapped to `WorkFailed`. The retained captured token admits a refused `SettleRetry` self-edge after durable completion; that branch is present in the actual Store graph and does not itself create a missing-edge cleanup exemption.

`GuardBeforeRevoke` is a structural consistency check of the modeled ordering, not independent evidence about adapter lock service. Task fairness alone does not prove branch polling, mutex service, backend return or child termination. Arbitrarily persistent fair errors do not imply successful work. The stable-success configuration excludes clock advance, owner removal and foreign acquisition; finite demand cannot establish infinitely many fresh completions. Safety retains cutoff states even where conditional cleanup does not apply.

`NoTwoSourceReaders` counts durable records with state running and queue expiry after the sampled time; it does not bound physical readers while expired work awaits cancellation or join. A local queue handle can outlive atomic last-request retirement. Local admission drop or domain release does not establish durable expiry, quiescence or rollback of a committed call.

## 4. Residual assumptions — this is not a complete Rust simulation proof

The finite relation does not discharge Tokio/OS scheduling, lock service, child/process return, filesystem behavior, SQLite atomicity/visibility or Raft/Hiqlite internals. Source capture/application races, error-to-reply classifiers, current-token replacement and publication-lock granularity remain reviewed or conformance contracts. Full-row state hashes depend on the Rust producer/replay checks; compact projections cannot independently reconstruct hidden rows. These boundaries must remain in receipts and any result summary.

Current Hiqlite insertion retries allocator-identity conflicts at most four times; a lost-reply timeout does not take that retry branch. The three publication clock samples and backend time-check differences remain outside this common-SQL graph's publication boundary. Local revocation after commit is not evidence that the durable call rolled back.

## 5. Ownership and delivery — review, qualify, merge, clean

| Surface | Owner |
|---|---|
| Shared driver, Tokio adapter and selector regressions | GPT-6.1 Sol source lane |
| Actual-SQL graph, conformance and sampled backend history | GPT-6.1 Sol Store lane |
| Temporal relation, strict runner and receipt transport | GPT-6.1 Sol proof lane |
| Mechanical old-proof staging, CI wiring and contract fixtures | GPT-6 Luna |
| Design advice and fresh final adversarial review | Actual Fable 5.1; advice tasks `t_257e8e98`, `t_2be26f79`, receipt-transport advice `t_ba162cc8` |
| Integration, ledgers, qualification, PR, merge and cleanup | Codex coordinator, chat `01a15aca-f39a-7ca3-9899-bf5bced43755` |

Baseline main is `1f30b4791476acb5d290c2f8e6ed38d07051f31d`. Commits `8e199e43c` and `196763515` contain driver/staging and Store/CI work. Ordinary Rust 1.97.1 daemon/Store target compilation and normal workspace formatting/Clippy/syntax hooks pass. Knowledge coverage is unknown, with no curated matches; an empty result proves no absence. No new tests, Store graph traversal, TLC or Kani qualification have run. The new module changes the old proof source envelope, so old six-target/three-control receipts cannot silently qualify it.

Independent Fable review part 1 (`t_96f689aa`, run24) was conditional with incomplete coverage. It found no blocking defect in the inspected production driver/adapter, identified the retirement modeling gap, and requested explicit boundaries. Continuations `t_533096c5` (Store and retirement advice) and `t_b8675492` (tooling and receipt trust) inspect frozen commit `c37c4d610`. Lower models implement corrections.

Retirement now has explicit local-token state, heartbeat drain, three wall samples and separate dispatch/application/reply/observation stages for its first settlement, logical resolution and optional second settlement. Acknowledged settlement clears the local token; completion retains it while clearing durable ownership. Retirement Yield preserves the actual `worker_retired` value. Error returns end the finite local chain without certifying durable release.

The logical resolution API remains a conformance boundary for its two internal SQL awaits. For `Some(fence)`, its dispatch-time field is unused by source guards and queries; the representative dispatch time uses a reviewed noninterference lemma, with all three real captures retained. Captured `now_ms` versus later application-time races remain outside the closed relation: imported primitives require their sample to equal the graph application time. Conditional cleanup exposes that restriction through `WithinEpisodeCapacity`; it is not an arbitrary Rust trace theorem.

Fable completed Store (`t_533096c5`) and tooling (`t_b8675492`) baseline coverage. The original refusal-edge criticism was corrected: existing canonical comparisons already established those self-edges. The fixed candidate adds independent checker predicates and hashes all vendored and compiled asset inputs before reuse. Legacy source maps retain stable IDs and explicitly retain their narrower retirement boundary.

Remaining: freeze and independently review the amendment diff, run affected cases once with valid-pass retention and failed/invalidated-only retries, require the current-candidate Main promotion gate, merge with regression references, verify the landing tree and remove task-owned clones/builds after preserving compact evidence. CI generates graphs and provisions pinned Java/TLC after compilation in the existing Rust lane; preflight cannot consume later graph outputs.

Existing clustering tests remain. This phase removes or replaces none, and adds no product gate, watchdog or optional switch.

Authenticated same-PR receipt restoration is implemented through the existing transport with legacy defaults preserved. Restored logs are reparsed; leaf/Store/daemon source scopes are checked against path dependencies. TLC reuse binds semantic model inputs and tools, while current provenance is refreshed separately. Stable working paths survive by authenticated artifact hydration, not an assumed persistent container cache. Compact fresh run snapshots exclude builds, scratch and TLC state databases. Fable receipt-design task `t_ba162cc8`/run23 is complete; baseline independent review coverage is complete; final amendment review remains pending.
