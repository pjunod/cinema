# Rust suite repair — preserve the 25 failures and qualify their corrections

**Status:** integration refresh in progress after merge authorization on
2026-10-21. Frozen `0f7b6325` passed run 5713: 6,928 native passes, zero
failures and 49 unchanged ignores; seven doctest targets, 400 cohort bodies,
20 writer bodies and both serial media cases also passed with qualified
cleanup. Those receipts remain evidence for that exact older source.
Effort development gate 5728 refused stale effort/main ancestry before
compilation. Main `00c74cd8` is integrated in `6a6d55099`; effort gate 5733
then passed history/catalog checks and correctly refused the operation-progress
model's old whole-file writer pin. The reviewed metadata refresh preserves
the existing application/reply/timeout abstraction, model, configurations
and guards; [the wait map](../../verification/operation-progress/WAIT-MAP.md)
records its scope. Both package mapping checks and 39 focused formal
runner/CI controls pass; TLC has not been rerun for this refresh. Both failed
gates and old formal receipts retain their original verdicts and source
bindings. Fresh exact-source compiler, focused,
full native and development-gate acceptance remain required before merge;
any new TLC qualification claim needs a new input-bound formal receipt.
Current receipts are recorded in
[task PR #1244](http://forge.lan:3000/noirr/plurx/pulls/1244).
The every-PR rollout remains a later milestone. **Owner:** Codex. **Branch:**
`codex/full-pr-suite-m1`, carrying the M0 dependency `16d1a5003`; neither
old successful outcomes nor the failed development gate are replaced.

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
| Nine Source/HTTP cases | Full 22-thread execution reached the existing three-second replicated Store timeout during startup. The controlled four-thread full body passes the same nine cases and waiter-cancellation control unchanged; failure-only diagnostics preserve the first typed refusal and HTTP response. | Keep the timeout and unknown-outcome semantics. The old-source comparison supports contention as a trigger, but fresh execution must qualify the newer repair candidate. |
| Newly surfaced K06 clock case, outside the original 25 | Full run 5679 constructed a clock observer before its actual applied-membership watch was ready. | Share the original 45-second startup deadline with a wait for both real nodes to observe the same committed learner membership. Construct the manager once; preserve every authorization, stale-heartbeat, fresh-heartbeat, removal and cleanup assertion. Add this case to the additional repeated cohort. |

## 3. Qualification binds actual execution to the source and inventory

The manually dispatched `.github/workflows/rust-repair.yml` requires an
exact source SHA. It uses the pinned Rust 1.97.1 CI image and media toolchain,
compiles once, discovers exact native identities, and records executable
hashes before and after execution. The complete M0 dependency is carried unchanged from PR #1237 because
that task is not yet merged into the effort. Its workflow remains available as the historical
diagnostic command; the new repair workflow owns M1 acceptance.

The declared sequence is compile, inventory, static checks (including the
30 evidence/controller controls), ten rounds of
the affected Core/daemon cohort, the complete ordinary Rust cell, and the
two exact serial media cases. Each round runs the inventoried real
binaries with 22 test threads and all original failed identities. Fifteen
additional regressions are listed separately, including five actual-task
panic, cancellation and registration-teardown controls. Three of those
controls reside in the existing Linux-only processed-reuse module; Mac
results cannot qualify those three. K06, Source waiter-cancellation, four
rolling retention/publication cases and the simulated capacity case are
also repeated. The declared cohort is 40 cases (14 Core and 26 daemon),
requiring 400 bodies over ten rounds; the original 25 identities remain
unchanged. The static phase separately requires 20 vendor transaction
bodies: two cancellation controls repeated ten times. These are declared
requirements, not claimed execution. The complete ordinary cell admits four simultaneous
cases; focused cohort invocations remain explicitly at 22 threads and the
two serial cases remain at one. Internal test runtimes and scenario races
are unchanged. The baseline and controlled comparison both reported 22
logical and affinity CPUs. This bounds competing real-node/media fixtures,
without dropping cases or increasing their deadlines. The qualification
job keeps one Cargo build job to avoid concurrent large compiler processes.
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
| Ten Linux cohort repetitions after stack correction | On exact `9971299d6`, all 330 executions passed: 250 original-failure bodies and 80 controls. Every native exit was zero, cleanup qualified, and strict accounting found no gaps. This receipt remains bound to that source. The expanded candidate now declares 400 fresh cohort bodies; this older 330-body receipt cannot satisfy that requirement. |
| Full Core on `9971299d6` | 1,943 passed, one failed and five ignored in 1,388.11s. The sole failure was the previously passing, unchanged K06 fixture: `Internal("clock observation lacks applied membership")` at manager construction. An additional SQL-writer panic appeared in its captured output. The original ten Core failures passed in this full body. The run remains red. |
| Isolated K06 before correction | Passed in 19.97s with the real Hiqlite backend and default stack; the known shutdown-handler panic was matched by the existing strict catcher. An isolated pass does not repair the full-run readiness race. |
| K06 correction, local native checks | The complete unchanged assertion sequence passes in 19.89s after the real-watch readiness wait. The expanded Core cohort (all original ten plus four additional cases) passes all 14 in 22.54s, with default stack and 22 test threads. Strict helper/controller/document controls pass 27 cases; independent review confirms the single original deadline and one constructor invocation. |
| Full ordinary Rust on `9971299d6`, run 5679 | The command finished in 4,395.719s (73m16s), exit 101, with safe cleanup. Strict original accounting conservatively refused interleaved Tray parent/child output. A separately hashed secondary parse using the corrected parser accounts for 36 native binaries: 6,918 passed, ten failed, 49 ignored (6,977 total), plus seven doctest targets. The original red/incomplete receipt is untouched. Exactly 18 original cases passed and seven Source/HTTP originals failed. |
| Full daemon failure receipts | 4,373 passed, nine failed, 40 ignored in 2,102.96s. Five original HTTP cases and two original Source status cases failed; a new Source waiter-cancellation case and the new serving-fence cancellation control also failed. Source startup receipts identify the unchanged three-second replicated Store timeout at activation writes; one first authority refusal occurred earlier. The late writer replies include actual successful transactions and a guarded SQL refusal. Full-load Source closure remains open. |
| Serial media on `9971299d6` | Both complete exact bodies passed with safe cleanup: VFR command 3.722s, bitmap command 3.235s. |
| Tray accounting correction | Preserve interleaved parent verdicts, including failures; remove only the exact source-bound child header, outcome and footer. Ambiguous/unknown children, missing parents, changed owners and mismatched counts remain refusals. Secondary reparse is complete and still red with ten outer failures. |
| F6 cancellation controller correction | Construct the actual fixture and satisfy the original ten-second real-media precondition before transferring it to the observer. Retain the receipt before transfer; the existing 15-second handshake observes actual ownership. Always cancel/join and check cleanup before propagating a handshake failure. All seven focused serving-fence cases pass in 18.06s with default stack. The failed old control did not reach its cleanup assertions. |
| Dropped transaction reply correction | Two real-writer regressions reproduce the old success/error reply panics deterministically, then pass after late reply delivery stops panicking the writer. Ten pairs pass with strict exact-body accounting. Actual commit, rollback and later writes are verified; strengthened regressions also require persisted applied-log metadata after real shutdown and reopen. Neither the Store deadline nor unknown-outcome semantics changes. Hiqlite generic patch 27 remains pending upstream tracking. The next Linux static phase repeats and accounts for both bodies ten times. |
| Follow-up local native cohort | All 14 Core cases passed in 22.96s and all 18 platform-applicable daemon cases passed in 58.87s, including every original failed identity and the new waiter case. Default stack and 22 test threads were retained. Three additional controls remain Linux-only. Workspace strict Clippy passed in 214s; 47 evidence/workflow/ledger/docs checks passed with one existing skip. |
| Cancellation boundary | The writer reply receiver belongs to Raft state-machine application, separately from the public client's timeout waiter. Failed-fixture teardown can abandon it after a timeout. The logs plus deterministic regression prove an internal abandoned-reply defect; they do not prove a client timeout alone directly cancels the SQL writer's waiter. |
| Completed controlled resource comparison, run 5698 (internal 5727) | Old-source `ffbed0dd3` with four ordinary test threads finished in 2,563.040s (42m43s), exit 101, safe cleanup. Secondary strict accounting covers all 6,977 native cases: 6,927 passed, one failed, 49 ignored. All original 25 and the Source waiter control pass. The sole failure is `rolling_real_put_complete_ts_attaches_exact_new_owner`. Original red/incomplete evidence remains untouched; [§4.1](#41-four-threads-reduce-measured-cost-but-the-comparison-remains-red) records scope and resource limits. |
| K06-only candidate disposition | `fef12d2f5` passed exact-source Linux all-target Clippy in 370s; run 5691 was cancelled while queued with no test execution after the older full run reproduced Source failures. The fresh follow-up qualification receipt is recorded in task PR #1244; previous source identities cannot substitute for it. |
| Independent review | Earlier schema, proof and task-ownership changes reviewed. The old-source comparison now passes all original 25; one separate rolling case still fails. Follow-up writer, parser and F6 corrections, all repeated controls and complete current-source execution remain acceptance requirements. |
| Published fixture checkpoint `c81f3af25` | Local 44-case rolling stress and 50 declared new-rolling bodies passed as detailed in §4.2. Source-only Linux all-daemon-target Clippy passed in 277s. Normal commit hooks passed in 53s, including 42.16s Clippy; paperwork passed 14/14 checks with 57 regression fields. These local/compiler/bookkeeping receipts do not replace exact-source native qualification. |
| Qualification run 5710 (internal 5739), task 22223 | Failed in 80s during native GTK package setup: the pinned Debian snapshot returned HTTP 503 for three required archives despite `Acquire::Retries=3`. No Cargo compilation or test body executed. Original failed setup logs and artifacts remain retained. |
| Qualification run 5711 (internal 5740), task 22224 | Failed in about 32s at the same native dependency setup, with the same three snapshot archives unavailable. No Cargo compilation or test body executed. This second setup failure supplies no source or test verdict. |
| Exact-head effort gate | Pending. |

### 4.1 Four threads reduce measured cost, but the comparison remains red

Run 5698 (internal 5727) executed `ffbed0dd310a7131c009ba5a43a38462c18e5f40`
on `codex/full-pr-suite-contention`. Its only committed difference from
`9971299d699c9d5b202e533afff41bbacf7aa4bc` is the ordinary full-run
`RUST_TEST_THREADS=4` setting in the repair workflow. The `crates/` and
`vendor/` Git tree objects match exactly; assertions, scenario concurrency
and deadlines are unchanged. Both jobs report 22 logical and affinity CPUs.
The focused cohort remains explicitly at 22 test threads. This older source
predates the K06 readiness, abandoned-reply and F6 cancellation follow-ups;
its outcomes cannot qualify those corrections on the newer repair branch.

All 36 native binaries reconcile exactly under the reviewed parser from
`8e2a8cb64`: 6,927 passed · one failed · 49 ignored, totaling 6,977.
Seven selected doctest targets remain separate: the Core target's one case
passed; the other six contained zero cases. All original 25 passed, including
all nine Source/HTTP cases; the additional Source waiter-cancellation case
also passed. Core reported 1,944 passes and five ignores; ordinary Store
reported 270 passes; daemon reported 4,381 passes, one failure and 40 ignores.
The sole failed body was
`transcode::tests::rolling_real_put_complete_ts_attaches_exact_new_owner`,
which timed out at its existing 30-second inventory precondition. It did
not reach the attachment assertion. No correction or successful rerun of
that fixture is claimed here.

| Ordinary full-cell measure | 22 threads, run 5679 | Four threads, run 5698 | Change |
|---|---|---|---|
| Native command wall time | 4,395.719s | 2,563.040s | −41.7% |
| Core reported body time | 1,388.11s | 919.56s | −33.8% |
| Ordinary Store reported body time | 860.40s | 385.32s | −55.2% |
| Daemon reported body time | 2,102.96s | 1,228.26s | −41.6% |
| Sampled cgroup CPU time | 58,747.551 CPU-s | 6,960.619 CPU-s | −88.2% |
| Sampled system CPU time | 50,274.329 CPU-s | 2,768.788 CPU-s | −94.5% |
| Sampled user CPU time | 8,473.221 CPU-s | 4,191.831 CPU-s | −50.5% |
| Mean consumed CPUs within sampled interval | 13.377 | 2.724 | −79.6% |
| Peak sampled process RSS sum | 6.347 GB | 3.621 GB | −42.9% |
| Peak sampled cgroup memory current | 6.721 GB | 10.506 GB | **+56.3%** |

**How to read the resource comparison:** counters cover only each job's
cgroup during the ordinary command, including Cargo/observer overhead.
Five-second samples use the first and last observations strictly inside
that command, omitting 4.162s at the 22-thread boundaries and 7.293s at the
four-thread boundaries. CPU differences are cumulative counter deltas over
those sampled intervals. Peaks are sampled, and process RSS sums may count
shared pages more than once. Both windows recorded zero OOM and CPU
throttling increments. Cgroup memory moved opposite process RSS; these
receipts lack `memory.stat`, so they do not establish a cause or an overall
memory reduction. External host load and cache state were not controlled.
One comparison supports inter-test contention as a Source startup-timeout
trigger; it does not establish a distribution, p95 or current-source
qualification.

**Provenance and retained verdicts:** the original native command exited
101 with qualified process cleanup. Its original parser conservatively
refused an inherited Tray child frame containing interleaved parent output.
The separately hashed secondary parse preserves that parent outcome and
accounts for all 36 binaries and seven doctest targets with zero gaps;
`native_passed` remains false. Both original red/incomplete receipts remain
unchanged. Executable hashes match for 35 of 36 binaries; the daemon byte
hash differs despite identical committed Rust sources and the same native
basename. The evidence therefore claims source identity, not identical
bytes for every executable or an explanation for that difference.

The retained artifact is `3826-rust-continuation-rust-final-5727-1`, under
source `ffbed0dd3`; the comparison artifact is
`3783-rust-continuation-rust-final-5708-1`, under `9971299d6`. Ignored local
receipts `rust-repair-resource-threads4-secondary-native-evidence.json`,
`rust-repair-resource-threads22-secondary-native-evidence.json` and
`rust-repair-thread-resource-comparison-summary.json` retain raw-log,
inventory, original verdict, parser, policy and resource-sample SHA-256s,
exact original-25 outcomes, both daemon hashes and sample boundaries.
`reparse-thread-resource-comparison.py` reproduces those derived receipts
from the retained artifacts. These files are secondary audit products,
not replacement CI receipts.

### 4.2 Rolling fixture preconditions remain under qualification

The local 44-case rolling stress run with 22 test threads and
`RUST_MIN_STACK` unset finished with 41 passes and three failures in 39.10s
(`env--u-RUST_MIN_STACK-20261021-135432.log`). The complete-PUT case passed;
two adjacent retained-copy/integrity cases timed out before inventory, and
the separate simulated 1.2x capacity case measured 1.198 under stress. Those
results remain failed; no new passing receipt is claimed.

The three positive retention fixtures used an unpaced producer despite
requiring optional capture to succeed. Their follow-up applies the existing
`d3_copy_session` fixture precondition: `HLS_READRATE=1` and
`HLS_BURST_SECS=0`. This prevents the default initial burst from consuming
the entire short fixture while capture must retain its committed objects.
The original 30-second inventory deadline and attachment, integrity,
retained-body and new-owner assertions remain unchanged. This repairs a
missing fixture guarantee; run 5698's inventory timeout alone does not prove
the optional-capture gate was busy.

The separate busy-capture negative now explicitly checks that retention is
not abandoned before capture against its actually held gate, and that this
capture immediately marks it abandoned. Zero initial burst retains the
original paced setup and 20-second ordinary-publication deadline. The
required result remains ordinary publication while optional retention is
refused. These four rolling cases are now additional cohort entries; they
do not replace any original failure.

The capacity simulation mixed Tokio's paused clock with the sampler's
unpaused `std::Instant`: a real extra millisecond can make its intended
`1,200 × 1,000 / 1,000` speed sample become `1,200 × 1,000 / 1,001`, or
1.198 after integer scaling. The fixture now uses the existing real sampler's
`note_out_time_at` with explicit simulation timestamps `(0, 0)`,
`(1,000, 1,200)` and `(1,000 + 2 × RECENT_SAMPLE_MAX_GAP_MS, 2,400)`.
Both exact `Some(1.2)` assertions and the complete 30-minute simulation
remain unchanged. No production sampler, threshold or rounding tolerance
changes. This fifth rolling case brings the declared additional list to 15
and the full cohort to 400 bodies; the separate vendor controls add 20.
Local follow-up passes all 44 rolling cases together with 22 threads in
20.34s (`env--u-RUST_MIN_STACK-20261021-135904.log`). All 50 exact bodies
in ten declared rounds of the five new rolling entries also pass, taking
142s total (`env-PYTHONPATH-.--opt-homebrew-bin-python3.12-20261021-140114.log`).
The local receipt retains native identities, per-round headers/footers,
filtered counts, executable and source hashes; no ignored or zero-match
body qualifies. `RUST_MIN_STACK` is unset. The 52 evidence/workflow/vendor
ledger/docs checks pass with one existing skip. These Mac checks do not
replace complete current-source Linux qualification, which remains owed.

### 4.3 Two package setup failures leave qualification open

Published source `c81f3af2581ee79b2eea35c18aaff60fe5d3ec35` has completed the
local fixture checks and Linux all-daemon-target Clippy above. Runs 5710
and 5711 are terminal setup failures, not running qualifications. Their
`rust-native-deps` action could not fetch three exact native GTK dependency
archives from the pinned Debian snapshot, even after APT's three retries.
Both stopped before Cargo compilation, inventory or native test execution.
The ignored source-bound evidence directory retains
`setup-failed-task-22223.log`, `setup-failed-task-22224.log`, their SHA-256
receipts, step metadata and always-uploaded diagnostic artifacts. Earlier
red native runs and incomplete accounting receipts remain unchanged.

Isolated setup now passes in the same immutable image,
`sha256:3d84b711936233da699308d0301ac1e91f12e76c6b928967c41817e841f5e855`,
with the same `20260928T000000Z` snapshot. Changing only the recognized APT
source prefix from `http://snapshot.debian.org/` to
`https://snapshot.debian.org/` fetched the signed package index (9.4 MB in
one second). Normal APT then installed all 142 GTK/DBus dependency archives
and OpenSSL without a failed fetch. `pkg-config` confirmed GTK 4.8.3,
DBus 1.14.10 and OpenSSL 3.0.22. No archive fallback was needed or executed.
`apt-tls-integration.json` binds that observation to the image, snapshot and
four hashed logs: index update, GTK/DBus installation, OpenSSL installation
and library version checks. The disposable container had no host mounts or
transferred credentials. This successful installation is one observation;
it does not guarantee future snapshot availability.

The selected setup patch is transport-only: `prefer_snapshot_https` rewrites
recognized snapshot HTTP prefixes in APT source files while preserving the
snapshot date, package selection, trust configuration and file mode. It
rejects source symlinks. A Docker-only action guard confines the setup step
to the CI container. The original APT install with `Acquire::Retries=3`,
signature verification and package hash verification remains unchanged.
The uncommitted generic archive installer is excluded from the landing.
The four focused TLS controls raise the declared static Python controls
from 26 to 30 and the broader inherited checks from 52 to 56. All 56
combined checks pass with one existing skip. The exact final helper and all
three composite-action scripts also pass inside that immutable image:
the source step changes one source file, the OpenSSL probe compiles and
runs, and the GTK/DBus probes report their actual versions. The integration
receipt retains the final helper/action hashes and per-step log hashes.
The owned scratch container is stopped and removed. Fresh exact-source
native qualification remains owed.

This work changes neither Rust sources nor the pinned image, snapshot date,
package versions, compiler profile or test deadline. The execution contract
remains four ordinary test threads, 22-thread cohort cells, 400 declared
cohort bodies, 20 vendor transaction bodies, the 6,977-case ordinary
inventory plus seven separate doctest targets, and both exact serial media
bodies. The setup follow-up must preserve those requirements before any
acceptance or every-PR rollout claim.

Closure requires every original failure to execute and pass, all declared
repetitions and new ownership controls to pass, exact complete full-cell
accounting, and bounded cleanup. Replicated Store, topology, Apple and other
baseline lane failures remain separately owned by the full-suite project;
this Rust repair alone cannot declare the complete repository green or
enable the every-PR cutover.
