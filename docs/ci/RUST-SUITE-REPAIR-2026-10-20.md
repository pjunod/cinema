# Rust suite repair — preserve the 25 failures and qualify their corrections

**Status:** open — implementation and local verification in progress; Linux
qualification remains owed. **Owner:** Codex. **Branch:**
`codex/full-pr-suite-m1`, based on `effort/full-pr-suite` at `c1c344132`
and carrying M0 dependency `16d1a5003` locally; neither task PR is merged.

Companion to [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md)
(the current execution policy) and [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)
(integration rules). This ledger owns the next milestone after baseline
PR #1237: correct the 25 ordinary Rust failures without weakening their
assertions, then execute the affected cohort repeatedly and the complete
ordinary Rust and serial media cells. Work uses an independent clone; the
user checkout is untouched. No rollout or automatic PR-policy cutover is
part of this milestone.

## 1. The original failed run remains failed

Frozen source `86f1385005b27122e8a8d3a3d5bd9501eb2fdb16`, Forgejo internal
run 5667, executed 36 ordinary native binaries: 6,970 cases, comprising
6,896 passes, 25 failures and 49 ignored cases. Seven doctest targets were
separate. Native binary bodies took 4,577.60 seconds, dominated by daemon,
Core and ordinary Store. M0's flat parser was incomplete because known
child-process output appeared inside parent stdout. Its original failure
and parser verdict are retained, not replaced by a green receipt.

The exact 25 identities are committed in
`validation/rust_repair_cases.json`. Their sorted target/name digest is
`22bdbf49112084882ac4b135287d28290257be96b274be04791695b741a2c5c2`;
the qualification controller rejects substitutions and fewer than ten
predeclared repetitions. A pass from a later repetition cannot erase an
earlier failure.

## 2. Corrections retain the original behavioral contracts

| Original failures | Cause and correction | Required protection |
|---|---|---|
| Nine Store census, import and migration cases | The already-landed v113 annotations schema was missing from source/authority inventories and historical fixture rewinds. Register the actual tables, five reads and write transaction; preserve annotation data across upgrade/reopen; validate the exact schema before rewinding historical fixtures. | Keep the unchecked-read ceiling, private-schema refusal, append-only migrations and exact table assertions. Negative rewinds must leave populated or malformed annotations untouched. |
| Process launcher census | The already test-gated child module used implicit path resolution that the source census did not associate with its sibling file. | Declare the existing test module's explicit path; keep its Linux test gate and the production scanner unchanged and prove adjacent production spawns remain visible. |
| Processed descriptor planning case | An SDR fixture no longer satisfied the processed-source eligibility contract. | Supply an eligible DV fixture with typed audio evidence; retain descriptor counter assertions and add ineligible controls. |
| Recovery refusal census | A new processed-contract refusal lacked its recovery classification. | Classify it as refusal; do not admit a retry. |
| Rolling publication frontier | A real-time-paced copy fixture measured below the existing 1.00x capacity requirement. | Pace actual input at 1.25x; retain real publication, fetch, EOF and frontier assertions and the production capacity threshold. |
| Serving fence | A finite publisher could exit while the test performed real decodes. | Use an owned live publisher, bounded cleanup, and real-media panic/cancellation controls. Preserve the original live-child and decoded-media assertions. |
| Replicated browse authority bar | Setup proof preceded fixture writes, so it did not necessarily cover the echo being tested. | Establish an observable applied-index/proof precondition before each route; invoke each handler once. Stop the sampler and prove an actually expired proof requires authority fallback. |
| Media seam census | Processed-reuse tests replaced the real driver with a test-only early return. | Exercise the real driver with typed ordering/observation hooks; preserve real task and slot ownership assertions, including teardown races and panic/cancellation. |
| Nine Source/HTTP cases | Linux baseline failed during actual Source startup; local execution has not reproduced it. Failure-only diagnostics now preserve the typed Store/startup refusal and first HTTP response. | Do not increase deadlines, retry assertions or claim a repair from a local pass. Diagnose the Linux/full-suite result before closure. |

## 3. Qualification binds actual execution to the source and inventory

The manually dispatched `.github/workflows/rust-repair.yml` requires an
exact source SHA. It uses the pinned Rust 1.97.1 CI image and media toolchain,
compiles once, discovers exact native identities, and records executable
hashes before and after execution. The complete M0 dependency is carried unchanged from PR #1237 because
that task is not yet merged into the effort. Its workflow remains available as the historical
diagnostic command; the new repair workflow owns M1 acceptance.

The declared sequence is compile, inventory, static checks (including the
21 evidence/controller controls), ten rounds of
the affected Core/daemon cohort, the complete ordinary Rust cell, and the
two exact serial media cases. Each round runs the inventoried real
binaries with 22 test threads and all original failed identities. Eight additional
regressions are listed separately, including five actual-task panic,
cancellation and registration-teardown controls. Three of those controls
reside in the existing Linux-only processed-reuse module; a Mac result
cannot qualify them. The full cell also declares 22 threads;
the baseline resource receipt reported 22 logical and affinity CPUs.
This is recorded execution context, not a reduction in the case universe.
The qualification job declares one Cargo build job to avoid concurrent
large compiler processes; native execution still uses 22 test threads.
The pinned CI build profiles and debug-symbol settings remain unchanged.
Every phase retains its original native return code and process cleanup
receipt. An unsafe cleanup blocks later execution. A safe failed body may
collect later diagnostic evidence, but the final result remains red.

`validation/rust_repair_evidence.py` checks exact identities, native headers,
footers, ignore disposition and filtered counts. Its only nested-output
protocols are the existing Core restore diagnostics and two inherited Tray
children, bound to the reviewed source-file hashes. Changed owners,
unclosed blocks, unrelated outcomes, missing/duplicate sections or mismatched
counts refuse qualification. Offline parsing of the M0 log accounts for its
25 actual failures; it does not alter that run's original verdict.

The controller allows 60 minutes for the repeated cohort and 90 for full
ordinary Rust, with bounded compile/static/serial phases and a 360-minute
job envelope. A timeout is incomplete failed evidence. These ceilings are
not performance estimates or permission to drop cases.

## 4. Evidence and remaining acceptance

| Check | Result |
|---|---|
| Pinned workspace compiler loop before edits | Passed; source matched the effort Rust tree. |
| Source/HTTP original nine, joint local run with 22 threads | Nine passed in 59.73 seconds; Linux baseline cause remains unresolved. |
| Strict parser controls | 15 passed, including malformed/foreign child output and omitted/failed native bodies. |
| Workflow contract controls | Six passed, including exact original-case binding, ten-round declaration and unsafe cleanup blocking. |
| Combined new/inherited harness, inventory and docs checks | 133 passed in 13.896 seconds with owned process-group observation available. The restricted sandbox attempt correctly refused unknown cleanup observations. |
| Core focused native regressions | 14 passed in 6.91 seconds, including the original ten Core failures and annotation preservation/refusal controls. |
| First daemon repair cohort on Mac | 15 passed, two failed in 63.45 seconds. The added proof-expiry control exposed four actual authority reads; seam census reached six further Record sites after the fake driver was removed. Three Linux-only controls were absent on this host, not passed. The final daemon rerun below verifies those corrections. |
| Local Linux compiler setup | Pinned 1.97.1 verified. Generic image lacks unrelated tray GTK libraries. A baseline two-job affected-target check recorded an OOM kill in the shared 7.747-GiB Docker VM; lower build concurrency is required. The same baseline affected-target check then passed in 80 seconds with one build job. No candidate test outcome is inferred from that setup failure. |
| Final local workspace check | All targets passed in 1 minute 56 seconds; two subsequent fixture-only diagnostic-hook installation calls are included in the Clippy compile below. |
| Final workspace Clippy | All targets passed with `-D warnings` in 1 minute 33 seconds, including the final fixture hook calls and redundant-borrow cleanup. |
| Linux affected-target Clippy | Committed `5a277dbdf` passed all Core/daemon production and test targets with `-D warnings` in 6m48s. The 7.747-GiB local VM required one job, no incremental cache and no debug symbols; its earlier two compiler OOM kills remain setup failures. Local FFmpeg is 7.1.5; final CI requires 8. |
| Final daemon native regressions | 17 platform-applicable cases passed in 61.34 seconds on committed `5a277dbdf`, including all original 15 daemon failures. With Core, all 25 original identities pass locally. Three new Linux-only driver controls remain owed. |
| Replicated annotations/import contracts | Both actual `dyn Store` annotation and populated v14 three-voter import/parity cases passed in 20.02 seconds, with `cluster-read-cost-validation,hiqlite-contract-tests`. |
| First Linux qualification, run 5675 on `b10f7c6c7` | Compile passed in 1,084.352s, inventory in 6.710s and static checks in 753.591s. Inventory: 36 native binaries, 6,977 cases, 49 ignored. Rust 1.97.1 and Jellyfin FFmpeg 8.1.3 verified. All ten Core rounds passed 13 cases; all ten daemon rounds aborted with a browse-test stack overflow. The 301-second cohort is failed and incomplete, with safe process-group cleanup. No Source/HTTP outcome is inferred from an aborted binary. |
| Browse default-stack correction | Heap-own each real handler future before nested accounting helpers, and separately box the unchanged expiry sequence. Preserve the sampler abort/join, real expiry, one request and exact `[0,4,0]` assertion. The complete test passes locally with `RUST_MIN_STACK` unset in 10.45s; fresh Linux qualification remains owed. Earlier focused Mac commands used an 8-MiB stack and did not expose the default-stack failure. |
| Local Linux native code generation | Two affected-daemon native build attempts exceeded the shared VM memory limit, including one job with incremental/debug symbols disabled. Neither yielded test outcomes; the two additional OOM kills remain setup failures. |
| Ten Linux cohort repetitions after stack correction | Pending. |
| Full ordinary Rust and exact serial media cells | Pending. |
| Independent review | Store schema/preservation, native accounting, browse proof, real publisher cleanup and real driver teardown reviewed; no remaining blocker. |
| Exact-head effort gate | Pending. |

Closure requires every original failure to execute and pass, all declared
repetitions and new ownership controls to pass, exact complete full-cell
accounting, and bounded cleanup. Replicated Store, topology, Apple and other
baseline lane failures remain separately owned by the full-suite project;
this Rust repair alone cannot declare the complete repository green or
enable the every-PR cutover.
