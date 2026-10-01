# Raft fault testing review — close false-result paths before building

**Status:** independent review completed; findings addressed in the proposal
by its author; no second independent approval · **Written:** 2026-09-30 ·
**Original verdict:** request changes before implementation

Companion to [the implementation contract](RAFT-FAULT-TESTING-IMPLEMENTATION.md).
An independent agent reviewed the initial draft against actual repository
source at `acfa667ecc929bf4790752451360c6b462798b23`. This file records its
findings and the author's subsequent dispositions. It is a review record,
not implementation evidence or a second execution ledger.

The reviewed draft's SHA-256 was
`636e986e64ad6aaeec6bf74385bfa6426f0cf1295dc9eebac736527357b40680`.
Line references below refer to that draft; current section references point
to the amended contract. Review consisted of source/document inspection.
No Rust compilation or runtime scenarios ran, and the reviewer did not
independently repeat the external raft-rupee inspection.

## 1. Verdict — retain the scope and correct four contracts

The reviewer judged the bounded three-task approach and unchanged ordinary
fast lane appropriate. The proposal respects the no-product-gates requirement.
The fixed corpus, existing validation targets and unchanged workflow YAML
are useful boundaries. Four findings required changes; none requires a
simulation platform or another implementation milestone.

| ID | Severity | Finding | Original draft | Disposition |
|---|---|---|---|---|
| R1 | P1 | A no-quorum timeout can desynchronize the child protocol. | Lines 183, 245 | Addressed in §4.2, §5.3 and M2 acceptance. |
| R2 | P2 | One absent witness does not establish observed isolation throughout the fault workload. | Lines 228–237 | Addressed in §5.2 and its negative control. |
| R3 | P2 | Per-key reads cannot prove the promised complete key-set comparison. | Lines 272–277 | Addressed in §6 using existing local dumps. |
| R4 | P2 | Existing helper cleanup cannot substantiate the promised ownership/reaping evidence. | Lines 183, 361–367 | Addressed in §8 and M2's explicit scope. |

## 2. R1 — preserve request/response alignment after timeout

**Finding:** ordinary `PutSetting` gets a 12-second controller timeout, but
the production retry envelope can last approximately 15.4 seconds. Existing
`WriteWithoutQuorum` receives 17 seconds for precisely this reason.
`NodeProcess::request` and `read_response` use untagged response lines; a
controller timeout does not cancel the child's operation or consume its
eventual reply. That reply could satisfy the wrong later request.

**Evidence:** [harness library](../../crates/plurx-cluster-check/src/lib.rs),
`WRITE_WITHOUT_QUORUM_RESPONSE_TIMEOUT`, `Request::response_timeout`,
`NodeProcess::request`, and `NodeProcess::read_response`.

**Author amendment:** use the existing `WriteWithoutQuorum` request and its
deadline, recording its fixed key as an explicit namespace exception in
the fresh database. A decoded quorum error permits planned recovery;
unrelated errors fail. Any timeout or framing uncertainty poisons the
channel and ends the case with cleanup. Do not issue another command on
that stream. Add a delayed-reply regression. No multiplexed protocol is
introduced.

## 3. R2 — observe partition effects over the workload

**Finding:** a healthy asynchronous follower can briefly lack a newly
acknowledged write. One absent witness before the remaining workload could
pass even when the intended partition is ineffective.

**Evidence:** [Raft client](../../vendor/hiqlite/src/network/raft_client.rs)
and [Raft server](../../vendor/hiqlite/src/network/raft_server.rs) check the
existing partition control, but the original proposed oracle did not
observe its effect across the fault interval.

**Author amendment:** require witness absence over a minimum two-second
sampled interval, after fault-phase writes and immediately before healing.
The final pre-heal probe checks all eight fault-phase keys. A failed read
cannot count as absence. Add a negative control where the witness appears
during the interval. State the limit: evidence covers those observations,
not every internal instant or complete transport isolation.

## 4. R3 — compare actual complete sets with independent expectations

**Finding:** querying only issued keys cannot detect an extra key created
by a misaddressed write, even if every expected value exists. The proposed
observation did not match its stronger complete-set claim.

**Evidence:** existing `Request::Dump` in the
[harness](../../crates/plurx-cluster-check/src/lib.rs) returns the local
database dump; no new RPC is needed.

**Author amendment:** extract the entire test namespace from each node's
returned settings dump, including the registered no-quorum key. Reject
unexpected keys and missing voters as well as lost or changed data. Hash
the returned filtered values; do not assume the separately queried whole
database digest is an atomic counterpart to the dump. Retain point reads
for witnesses. Clarify that missing data after an observed acknowledgement
is a failure, while a lost response leaves an ambiguous operation.

## 5. R4 — retain ownership before awaiting readiness

**Finding:** `ClusterProcesses::kill_all` discards per-child errors, and
startup or dynamic spawn can fail before returning/registering the child
in the controller's cluster. Best-effort `kill_on_drop` is not evidence
that a process was reaped successfully.

**Evidence:** [harness library](../../crates/plurx-cluster-check/src/lib.rs),
cluster startup, `spawn_node`, and `kill_all`.

**Author amendment:** M2 explicitly owns narrow helper changes to register
children immediately after spawn, retain ownership through setup errors,
and record termination/reaping outcomes individually. Preserve the first
scenario error separately from cleanup failures. Test failure during
readiness and failure during cleanup, in addition to cancellation. Re-run
existing helper lifecycle regressions when shared behavior changes.

## 6. Clarifications — preserve portability, model independence and provenance

| Review clarification | Amendment |
|---|---|
| `cluster-check` already runs on Darwin and Linux; the draft's platform contract was ambiguous. | Initial process corpus is explicitly Linux-only. Existing Darwin checks remain; parent platform dispatch reports this added substep as not applicable and creates no success artifact. Linux evidence remains required for this effort. |
| WAL legal-operation semantics and reader lifetime need to be explicit. | Specify front/suffix/full-purge cutoffs and preserve one reader/memo across mutations; do not rebuild away the stale-generation condition. |
| Embedded SHA alone is not clean-source provenance. | Record actual executable SHA-256, reuse existing source verification patterns, and link the existing qualification receipt rather than create a new attestation system. |
| “Lost acknowledgements” was ambiguous. | Distinguish missing data after observed success from a missing response after dispatch. |

All dispositions are author reconciliation of the independent findings.
The revised implementation document remains a proposal for human review;
neither this record nor the documentation checks assert runtime correctness
or independent approval of the amended text.
