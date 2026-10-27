# Copy start cleanup — accepted decisions and implementation review

**Status:** open · bounded design reviewed; implementation and qualification active.
**Updated:** 2026-10-26.

Companion to the [resource ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) and
[delivery checklist](ADAPTIVE-PLAYBACK-DELIVERY.md). PR #1293 targets effort
PR #1248. Source is committed and reviewed through ordinary Git. No behavioral
or mutation tests have run. Completed-candidate independent review and fixes
must precede those tests; the bounded design review grants no final acceptance.

The user supplied a review attributed to Claude Opus 5.5, dated 2026-10-26,
against implementation `46dc2805b9b6688b5052494605250898a6b105c7` and document
`8b6331e5634eb315626e60103d58a96b347501a2`. The reviewer reported checking all
27 original manifest hashes and Tokio 1.53.1 source, with no compilation or
execution. That original request and manifest remain available at `8b6331e`.
The supplied review's SHA-256 is
`dc9df50930130a30adee9ec91a52e62bf06392509d25e8bff677dc7439505672`.
The current [source manifest](ADAPTIVE-PLAYBACK-COPY-START-REVIEW-SOURCE.json)
pins implementation `5267b8721312b86e205ac99217c20a159b0fec4a` for the next review; it is source identity, not
execution or approval evidence.

**Original** means the same retained instance: Child/ChildJob, JoinHandle,
source or pipe cell, latch, graph custody and named operation. Reopening a
path, recreating a command, matching IDs or launching a replacement task does
not establish that ownership. Trusted fixed software/helper topology is an
assumption, never proof of native closure.

## 1. Accepted state boundaries and native witnesses

One cleanup driver selects a per-state native witness. Its selector is not a
proof; each constructor checks the sealed graph's retained inventory.

| State and boundary | Required native evidence | Emitted disposition |
|---|---|---|
| S0: acquired source/output prefix; process entry not consumed | `FiniteVodNeverRootClosure`, plus actual source and acquired prefix closure | Root/task NeverAcquired; an acquired prefix uses `ScratchAssetsV3`, never NeverAcquired |
| S1: process entry consumed; launcher returned no retained Child | No absence or close witness; `uncertain_launch` and full R remain held | No invented root/task close or settlement |
| S2: original Child/Job retained; parser not registered | `CopyRootOnlyClosure`: original wait/status/job, checked unclaimed pipes, matching fixed image, no registered writer | Root closed, `VodOutputTaskNeverAcquiredV1`, original sources/prefix closed |
| S3: parser registered; one-shot latch never delivered | Original handle joins naturally with positive `CopyTaskTerminal::NeverEntered`; runtime delivery bit is false; checked reader/pipe closure | Acquired `VodOutputTaskV1`, after the exact task acquisition observation commits |
| S4: latch delivered; parser entry permitted | Original handle joins with `Entered(summary)`, delivery bit is true; original root waited and pipes closed | Same acquired task closed-owner fact; success/content is separate |

S2 also requires the runtime task selector to be absent and the task member
not invoked. An armed member with no observation may use NeverAcquired: the
actual typed transaction and validator permit this. Arming alone is not parser
registration. A missing runtime selector beside a registered graph writer is
an identity mismatch, not registration absence.

S1 has no same-boot restart settlement authority. An unbound S2 root identity
also grants no new root fact: native cleanup can proceed through its Child,
but full R stays held without the required committed identity. These are
conservative dispositions of unresolved authority, not claims of recovery.

## 2. Latch, terminal and emitted task semantics

The start latch is one `oneshot` channel. Sending consumes its sole sender;
the receiver is the task's first await, with only destructuring beforehand,
no blocking work and no receiver-close path. Sealed cleanup drops an unsent
sender under graph membership and persists `DroppedUnsent` or
`TakenBeforeSeal` exactly once before joining. Retry cannot reclassify it.
Cleanup never aborts the task to obtain nonentry.

Only that receiver's drop-without-send error publishes `NeverEntered` and
notifies waiters. An entered task publishes `Entered(summary)`. Panic,
abandonment, a poisoned slot, missing terminal or delivery mismatch stays
Unknown. No default summary stands in for a positive nonentry terminal.

The nonentry distinction remains internal. Existing `VodOutputTaskV1` means
the original registered handle joined and every inventoried owner closed
under a non-nil `join_operation_id`; it proves neither parser entry nor output
nor success. No schema/version change is required. A future persisted reader
that requires the entry bit would need a new fact version. Output evidence
remains the separately checked exact scratch head.

## 3. Cancellation preserves original operations and witnesses

The existing caller deadline bounds each borrowed cleanup attempt. Timeout
cancels the waiter, retaining the original handles, operations and charge.

1. Seal graph entry and the application gate. Resolve captured Hold, Arm,
   SourceRoot and CopyTask operations under their original IDs before closing
   owners those acknowledgments require. An unresolved outcome stays held.
2. For S2/S3 and cancelled S4, request termination through the original root
   before a blocking join. Natural parser failure or stderr-read failure also
   signals the same registered child **before awaiting the other branch** of
   the parser/stderr join. A post-join kill alone cannot unblock open stderr.
3. Naturally join the same parser; close pipes with checked native closes;
   wait the same Child and retain its job/status. A failed kill, including an
   exited-child race, is not termination proof: observe the original wait.
4. Join original bootstrap/source jobs and close source parents. Preserve
   acknowledged source-root observations across retries after parent close;
   never require reopening or re-reading an already-closed parent.
5. Mint and retain exactly one per-state witness without a cancellable gap
   after minting. Close the acquired output prefix against its actual head.
6. Snapshot the five actual acknowledged envelopes and run the complete
   existing envelope and settlement validators before consuming one-shot
   witnesses. Length-count every snapshot and the aggregate settlement before
   their allocating validators. Their known requested storage is included
   before R; allocator, executor and native aggregate peaks remain Unknown.
   Retain the same settlement operation across cancelled reply waits, then
   destroy the rich bundle before releasing R. Every remaining application alias blocks
   capacity return.

The parser/sink never takes graph membership while its original join holds
that lock. The runtime releases the bundle lock before parser join, because
sink publication needs that bundle. This is a source invariant and an authored
regression obligation, not a proven execution result.

Checked-close failure and lost terminal/native authority are permanent holds.
A failed termination request remains retryable advice while original handles
exist. Metadata preflight rejection is a separate retryable hold: the same
settlement ID, witnesses and charge remain; only a successful full preflight
clears it. Immutable-invalid metadata can therefore remain held and retried;
this is not a recovery claim. Maintenance skips only permanent native holds. The Resource ownership card
shows the retained reservation's charge and typed reason; unavailable inventory
stays unavailable, and restarting alone does not release the charge.

## 4. Evidence and remaining review obligations

The implementation uses one `prepare_failed_copy_close` driver and
`NeverRoot` / `RootOnly` / `Joined` witnesses. It reuses retained custody flags
for retry rather than adding a parallel staged closure state machine. Native
proof remains whole-state inventory evidence; no per-stage refund is added.

Regression bodies cover actual retained owners, latches, source observations,
root-only closure, alias barriers, launcher uncertainty, terminal/pipe failure,
lock order, cancellation and the original settlement reply. The post-commit
reply fixture delays the actual worker's original reply after COMMIT and lock
release; it is not an injected unknown storage outcome. A checked-close
result-loss fixture is not a forced EBADF or permission to close/retry an old
numeric descriptor. Authored cases and mutation controls confer no execution
credit until run after completed-candidate independent review.

Keep R1–R22 qualification open for explicit coverage review, focused behavioral
execution and required old-code mutations. Private allocator/executor/native
aggregate peaks, shared storage authority, provisional recovery, broader
adapters, and the remaining V1–V5 / R1 / M0–M7 acceptance stay open. The design
review does not close those obligations.


The authored-source map leaves these cases explicitly incomplete: actual
post-root budget refusal (R8), actual syscall EBADF rather than checked-close
result loss (R9), cancellation at the literal take-start mutex wait (R10),
caller abort while an entered parser is pending (R15), every possible cleanup
await/certainty combination (R19), and actual wait failure/non-ESRCH driver
signal failure (R20). R11 now authors real drain refusal of the original latch.
R21 covers a committed settlement with its original reply delayed, not an
unknown storage transaction outcome. None of these bodies or controls ran.

## 5. Next independent implementation review

Review committed source `5267b8721312b86e205ac99217c20a159b0fec4a` through ordinary
Git and task PR #1293 into effort PR #1248. Use effort `48eccfd748741bab47d945e68c7713a4c213ea60`
as the task diff base. Historical `8b6331e` retains the original bounded design
request and manifest. The current source manifest includes original dependencies
and changed Source/private-constructor/product files; review the task diff too.
The compiler receipts cover the code pin above. Publication adds status/review
text, the source manifest and a source-backed client-history anchor afterward;
no Rust/Cargo changes or behavioral/mutation execution follow that code pin.

Check the S0–S4 boundaries, original latch/handle identity, kill/join/wait
ordering, same-operation journal replay, complete preflight before witness
takes, rich-bundle destruction and last application-owner release. Challenge
Source P/M construction and report lifetime, the factory restart admission's
unresolved Drop behavior, and direct actor → original ticket custody while
actual readers remain. Dropping the redundant actor application reference
must not become native-close evidence or a refund.

Report concrete blockers with source locations, missing regression cases and
any mismatch between implementation and the accepted decisions. Preserve
Unknown where authority or measurements are absent. The earlier availability
file probes, same-boot S1 recovery, shared storage/provisional work, broader
adapters and complete native/allocator bounds remain explicit open scope.
Do not run behavior or mutation tests as part of this source review. Return
whether the examined implementation can proceed to the identified focused
qualification after findings are fixed; do not label the whole adaptive
playback effort accepted from a bounded review.
