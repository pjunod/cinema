# Copy start cleanup — accepted decisions and implementation review

**Status:** open · source fixes implemented; current behavioral failure under repair.
**Updated:** 2026-10-29.

Companion to the [resource ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) and
[delivery checklist](ADAPTIVE-PLAYBACK-DELIVERY.md). Task PR #1293 targets effort
PR #1248. Source is committed and shared through ordinary Git. The current
implementation `b1f2794d79b1012714a6de0644c72df44f10e9e1` passed macOS and Linux
compiler checks; its first 206-case native run stopped after 31 passes and one
static fixture failure. All five registered-planning entry/cancellation cases
passed. The full result and receipt hashes are in the ledger. Further diagnostic
execution is in progress; no final implementation acceptance is claimed.

The user supplied a review attributed to Claude Opus 5.5, dated 2026-10-26,
against implementation `46dc2805b9b6688b5052494605250898a6b105c7` and document
`8b6331e5634eb315626e60103d58a96b347501a2`. The reviewer reported checking all
27 original manifest hashes and Tokio 1.53.1 source, with no compilation or
execution. That original request and manifest remain available at `8b6331e`.
The supplied review's SHA-256 is
`dc9df50930130a30adee9ec91a52e62bf06392509d25e8bff677dc7439505672`.
The current [source manifest](ADAPTIVE-PLAYBACK-COPY-START-REVIEW-SOURCE.json)
pins implementation `b1f2794d79b1012714a6de0644c72df44f10e9e1`.
It records exact compiler scope and the current failed behavioral run, while
preserving the supplied reviews at their original pins. It grants no approval.

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
actual syscall EBADF rather than checked-close result loss (R9),
every possible cleanup await/certainty combination (R19), and actual wait failure/non-ESRCH driver
signal failure (R20). R11 now authors real drain refusal of the original latch.
R21 covers a committed settlement with its original reply delayed, not an
unknown storage transaction outcome. At that source-review checkpoint none of these bodies or controls had run; section14 records later execution.

## 5. Earlier published source and review boundary

The earlier published source is `9903421c871c0146b8030bc88c51340403ba8134` in ordinary Git and task PR #1293, against effort
`5cf30d1470eb40ca0359eb2372e8a46dc447cc53` with main `77e9858277813456bb94e855ec62ed3e9fd65af8` merged. The current manifest hashes previous
dependencies and changed Rust/JavaScript/Cargo files, including both isolated
spike lockfiles. Later documentation/manifest commits do not change that code
pin. Read the task diff and section7 for the implemented dispositions.

The supplied implementation review examined5267/c108, as recorded in section6;
the original bounded design review remains at46dc/8b6331e. Neither review is
silently transferred to changed source. The compiler receipts below establish
only their exact source and environment. The user manages Opus review because
Fable quota is exhausted; no Hermes or external API review is dispatched.
The user has since authorized required behavioral tests; actual incomplete results and repairs are in section14. Native workload/model/mutation qualification remains open.
Do not infer acceptance of V1–V5/R1/M0–M7 or complete native/allocator bounds.

## 6. Supplied implementation review and policy decision

The user supplied a source-only Opus 5.5 implementation review of code
`5267b8721312b86e205ac99217c20a159b0fec4a`, base
`48eccfd748741bab47d945e68c7713a4c213ea60`, and this document at `c1085312`.
SHA-256: `97f0d42b524143dced18f03975acae890106292d265a21a1a8107c954359fccc`.
Its verdict is **not ready** until B1–B4 and C1–C2 are fixed and their
regressions authored. C3–C4 regressions are also required. Nothing was compiled
or executed by that review. Section7 records the subsequent source fixes and exact compiler
evidence; the historical verdict is preserved and grants no new approval.

The user selected the operation-local reservation policy recorded in
[contract history](ADAPTIVE-PLAYBACK-CONTRACT-HISTORY.md#native-uncertainty-admission-policy--2026-10-26):
keep existing charges and restart-drain uncertainty, while allowing unrelated
starts through normal capacity and phase checks. No uncertainty bit is
cleared, no closure is invented, and no charge is created for work that never
reserved resources. Ordinary SQL failures and pre-reservation refusals must
not create native uncertainty.

Required source corrections cover original physical/preparation settlement
independent of Source SQL; exact pre-reservation classification; returning a
never-published Direct owner's original sealed ticket; positive never-root
closure after an actual post-Arm refusal; waking a parser parked at a full
window on retirement; and permanent holds for contradictory post-mint facts.
A known original planning selector must retain resumable cleanup rather than
become a selector-less permanent wait. R15 is now required, not deferred.

C5 remains open: the normal-retirement failed-admission flag also selects
orphan pending-ticket repair. Removing it alone would strand abandoned
cleanup, so its labeling/selection split needs a bounded follow-up. This minor
finding does not authorize hiding any held reservation or changing native
proof. Earlier coverage gaps and V1–V5/R1/M0–M7 remain open. The user's explicit
behavioral-test hold remains in force while these fixes and compiler checks
are completed.


## 7. Implemented review dispositions — 2026-10-28

The source changes below are pinned at `9903421c871c0146b8030bc88c51340403ba8134`. They address the supplied
review's blockers and required regression authorship. This is an implementation
record, not a second independent review or permission to run behavior tests.

| Finding | Implemented disposition and authored regression boundary |
|---|---|
| B1 — Source factory admission versus SQL | Actual matching producer/preparation closure and zero real bodies disarm only this factory guard. SQL transient exhaustion/refusal remains unresolved SQL, independently of native closure. Actual existing-route, expired-start and Store-failure fixtures cover these paths in [Source settlement tests](../../crates/plurxd/src/transcode/source_actor/settlement_tests.rs). |
| B2 — Direct pre-R checks | Pure validation before the first typed preparation is tagged BeforeReservation. Actual prepare and post-R failures retain their original classification; native uncertainty is never inferred from a known pre-R refusal. |
| B3 — fixed planning pre-R checks | Deadline, path/mount and arithmetic validation before the first reservation receive the same precise classification. Tests age the same real observation after first proving freshness. |
| B4 — unpublished Direct start | An unowned Source SQL route cannot trap the worker after native closure. It returns the same sealed ticket through actual worker join, ToSettling and finalizer join; [the open-failure regression](../../crates/plurxd/src/http/direct_start_failure_tests.rs) asserts the original reservation through those boundaries. |
| C1 — post-Arm refusal | Only the matching invoked root with positive never-entry conditions is eligible for the graph's original NeverRoot witness. Actual drain and epoch refusals are authored; uncertain post-entry launch stays held. |
| C2 — full-window parser | Actual retirement/cleanup/logical seal records closing intent and wakes the same progress Notify before natural task join. The closing check precedes transition waiting; ordinary graph seal on normal completion retains publication behavior. No task abort supplies closure. |
| C3 — post-mint contradiction | Fact identity rejection after RootOnly/Joined minting becomes TerminalIdentityMismatch. The same witness and full reservation remain held, and maintenance does not retry native termination. Both actual cleanup-driver cases are authored. |
| C4 — missing selector | The driver is exercised with a missing runtime task selector while the actual graph retains its registered writer. It keeps the original reservation held; absence never becomes NeverAcquired. |
| M2 — original planning owner | A known original planning owner moves into retained resumable cleanup before the first await. Waiter cancellation resumes that cleanup; a genuinely missing owner remains unresolved. |
| Policy2 / M1 | Genuine native uncertainty retains the operation's existing charge and blocks restart drain/commit while ordinary unrelated intake uses normal checks. No uncertainty bit is cleared and no reservation is invented for pre-budget I/O. Earlier availability-I/O ownership remains an explicit limitation. |

R15 is required and authored in
[the real copy runtime fixture](../../crates/plurx-core/src/adaptive_resources/runtime/operations/copy_vod/runtime_tests.rs):
the original entered parser fills its immutable five-file window without reader
acknowledgment, remains pending, then wakes and naturally joins on actual
retirement within a deadline. Actual application aliases still retain R until
released. This covers full-window retirement; caller abort at every possible
cleanup await/certainty combination remains incomplete. No body was executed.

C5 remains deferred: the failed-admission flag used during normal retirement
also selects orphan pending-ticket repair. Removing it alone could strand
cleanup. A later labeling/selection split must preserve that repair path and
all held reservations. R8/R9/R10/R19/R20 gaps, shared storage/provisional work,
private native/allocator/executor peaks and V1–V5/R1/M0–M7 remain open.

**Compiler evidence:** exact source `9903421c871c0146b8030bc88c51340403ba8134` passed
native x86_64 Rust1.97.1 core+daemon all-target+cluster strict Clippy in
375.492s, source stable; log SHA-256
`919eea478ff0ea42843fee5443505c4e82bf6757cb6b8f698accf5fc758fa675`.
Host source `93d9bf1bdb621336fa88c70582d92cc3667daa53` passed the full-workspace
compile/format/locked-spike check in 322.64s and strict
core+daemon Clippy in 341.224s. Only the Linux-only Source
settlement and core Direct fixtures changed afterward. Their parent modules
and Linux cfg guards are unchanged; all 7032 other
host-guard source blobs match the current pin. The changed fixtures are
covered by exact native compilation; no host compilation of those files or
exact-new-source host pass is claimed.
Host log SHA-256 respectively:
`11fadca8157bbbd1097c8caf102dfc97ddd6b56bf6b5b1ae99191d49197fb700` and
`4284e07eba21995cc2234a59f8e9df64c036e0bb3cd1112e007c52a761fa9c48`. Standalone vendored Hiqlite Clippy passed in
6.979s; log SHA-256 `51b8531a346691fcf813bb77ae192a992a37da677df0abf9b7d9607c62c524e7`.
The vendor target can report the existing root Clippy configuration's
unreachable-path warnings; no zero-diagnostic claim is made. All90 task
regression declarations resolve against this source. These are compiler and
source-name checks; no regression body was executed. Fresh current-head
compile-only CI remains required before task integration.

Earlier source `5267b8721312b86e205ac99217c20a159b0fec4a` passed native strict
Clippy in612.846s (log SHA-256
`811cf9dbfee5dd6d355e44ecb9aa1b5880033f47391b50dff6d70b2279ee8489`)
and matching host/Linux-target checks in267.793s/320.812s. Those receipts remain
bound to that earlier source; they are not the current candidate's proof.
CI6082/index6053 failed locked spike metadata before Rust compilation; its
other platform/static passes are historical results, not current-head credit.


Behavioral/native-workload/model/mutation tests remain **NOT RUN** and held by
the user's instruction. Fresh current-head compile-only CI is still required;
no current CI pass, merge, deployment, restart or final acceptance is claimed.


## 8. C5 follow-up — source work, 2026-10-28

The later source change separates original close custody from repair selection.
A private non-Clone claim moves from the preparing future into its actual
returned ticket. A live claim excludes maintenance; cancellation before ticket
exposure or destruction of an unfinished ticket requests repair. Cancellation
of a borrowed finish future keeps custody in the surviving ticket. Repeated
preparation while a ticket lives refuses rather than revoking that claim.

The original role retains the same atomic custody allocation after CopyLaunch
is removed. It owns no native/application/preparation payload. All three close
callers validate that identity before touching the application fence or native
proof. Only the existing successful finalizer marks the control finished; all
native witnesses, original journal IDs, application barriers and permanent
holds remain authoritative. Actual Arc layout, retained-role pointer, ticket
storage and projected async frames are included before reservation.

Three actual lifecycle regression bodies are authored, and the original
COMMIT-reply cancellation case now checks maintenance exclusion. They have not
run. New compiler checks are pending. The earlier compiler pins and passing
compile-only CI6081 at 2c0228f remain historical evidence for that source; the
existing manifest still pins that preceding implementation until new checks
finish. This follow-up adds no independent-review or behavioral acceptance.


The user subsequently directed continuous completion through qualification and
merge, and explicitly authorized required behavioral tests after the fixes.
Earlier execution holds above are historical. No behavior pass, main promotion,
deployment or restart is inferred from that authorization.

## 9. Additional copy boundaries — source work, 2026-10-28

R8 now authors a real post-root parser-budget refusal: a deliberate growth of
the same retained fixture payload exceeds its unchanged reservation, and the
actual adapter refuses before registering a writer. The same root, fixed image
and work remain retained; root-only closure and actual last-alias release gate
the eventual refund. This negative fixture establishes no valid memory profile.

R10 now authors cancellation while the actual `take_copy_start` call waits on
the original membership mutex. An opaque fixture holder neither seals the graph
nor takes its one-shot sender. A boundary gate and one pending poll establish
the literal wait; after caller cancellation, the same task and observation
naturally join as NeverEntered, with acquired-task facts and alias barriers.

Both bodies are authored; execution and mutation validation are pending. The
new mutex capability and boundary gate exist only in test/fixtures builds. R9,
R20 and remaining actual pending joins/outcome certainty remain open.

## 10. First behavioral run — custody startup repair, 2026-10-28

The first native campaign at `2f0a2321831b4ac9b6c08bcf73ede853b3d8acd1`
compiled both test harnesses and discovered all102 selected cases. Its first
case failed before runtime construction: the runner's fresh scratch directory
was group-writable. Only that owned directory's permissions were corrected.
The second attempt reached a production startup defect: custody's full ancestor
walk includes `/`, while the general no-follow directory helper refuses root.
Both attempts executed1 case, passed0 and failed1; neither is qualification.

The repair adds a metadata-only root ancestor inspection and uses it during
installation and retained-chain verification. It preserves the full inode
chain, permission checks, ordinary root-capability refusal and intermediate
symlink refusal. A focused filesystem regression is authored; the repair still
requires exact compilation and native execution. No failed evidence is removed.

## 11. Dormant copy allocation installation — source repair, 2026-10-28

At `78a3a4e3e2f6f36803fdddcf9327f9631c61c0d1`, both strict compiler loops
passed and the core-only campaign discovered64 selected cases. Its first case
passed custody startup, then failed during `prepare_copy_vod` with State.
One case ran, zero passed and one failed; the remaining63 did not execute.

The constructor registers its dormant CopyLaunch before fallible allocation,
so a failed constructor retains the original cleanup selector. It then called
the generic response installer, which intentionally rejects registered copies.
The repair preserves early registration and the generic guard. A private copy
installer validates the same app and cleanup allocations, an unsealed dormant
graph with no acquisition or close state, and absent work/budget, then installs
both original allocations atomically without suspension. It does not require a
prepare acknowledgement or change memory, handle or storage limits. Source
review found no blocker; exact compilation and actual cleanup runs remain due.

## 12. Typed output generation — source repair, 2026-10-28

At `bb2fa576a2cf3af2994c77c1d5107cbe50222852`, both strict compiler loops
passed. The first of64 selected core cases progressed through copy construction
and then received a known rollback while acknowledging the initial typed
Prepare: ScratchV3 requires a nonzero generation, but both real copy and
software builders and their shared fixtures supplied zero. One case executed
and failed; no later case ran. This is not a cleanup or native-closure pass.

Each fresh immutable copy/software owner now starts its typed asset generation
at1, forwarding that same value to its scratch and task plans. The separate
legacy scheduler counter is unchanged. Zero is rejected during pure validation
before family creation or reservation; codec validation is unchanged. Actual
zero-generation regressions check BeforeReservation, empty original accounting
and journal drain. Existing accepted and known-rollback software fixtures keep
their original bounds. Source review found no mismatch; compilation and
behavioral qualification of this repair remain pending.

The same validator/native-workspace audit found that accepted copy/software
fixtures reserved exactly1MiB for both scratch and bootstrap. Retained bootstrap
handles consume that partition, while actual sync/publication adds workspace
and payload. Their scratch partition is now3MiB: bootstrap, maximum1MiB payload
and native workspace. Total original R stays256MiB, and intentional rejection
fields stay unchanged. Production profiles already reserve128MiB scratch memory
with1MiB bootstrap and are unchanged. Actual admission still bounds readers and
other overlaps; this correction establishes no full native/allocator peak.

## 13. Acquired scratch prefix without a process — source repair, 2026-10-28

At `db5f8a3c39a50839d63fc342ce6cd4ff9d8613ee`, both compiler loops passed.
The first of69 selected core cases reached accepted preparation, original
source opens and scratch root/sink acquisition, then failed physical retirement
with State. One case ran and failed; the other68 did not run.

The shared classifier previously accepted only an Unstarted scratch bootstrap
when the producer process never entered. It now distinguishes a RetainedPrefix
from an unarmed placeholder and an observed NeverEntered root. Both copy and
software retain that same prefix for the existing native retirement driver.
Only the exact unarmed placeholder may be removed. No schema, witness, refund,
application-alias barrier or change/next-arm guard is relaxed. Missing native
owners still prevent the driver from issuing a close fact. The existing actual
copy-prefix and software cleanup regressions cover this repair; compilation
and execution of the new source remain pending.

## 14. Actual cleanup qualification and repairs

The first actual native campaigns found constructor and lifecycle failures;
these are failed runs, not acceptance. At 00811689a the incomplete survey
recorded 57 cases: 16 passed and 41 failed. Its next isolated-process fixture
printed both child and parent harness summaries; reporting stopped rather than
crediting an ambiguous count. Later reporting uses the exact outer case's final
anchored summary and retains all nested logs, command exits and source hashes.
A nonzero exit or any failed case still fails the campaign.

The directory lender now validates the entire original inventory before lending
an action's exact subset. Child-only and parent-only jobs retain unused Files;
restoration returns the same originals. Sticky registration markers distinguish
unissued publication/sync from missing issued owners. No absent issued owner
can authorize retirement or transfer, and refusal precedes destructive cleanup.

The survey then exposed a journal invariant violation: child removal kept the
quiescent directory-sync marker while registering another native job. The fold
now requires the actual prior sync, consumes its marker on removal Arm, and
keeps the output validator unchanged. Retirement no longer syncs a removed
child. An already-held role may observe its original prefix without authorizing
fresh entry. Software fixtures now request their actual 24 fps cadence.
These repairs and real missing-owner/checked-close regressions require their
own current-source execution; earlier failures and isolated passes do not
establish a complete candidate pass. The Direct typed-prepare schema rejection,
publication/full-window failures, and broader delivery milestones remain open.

## 15. Earlier published compiler receipts

The following receipts describe the earlier published candidate. New source
requires fresh checks; these results confer no behavioral acceptance.

**Earlier published compiler evidence:** exact source `9903421c871c0146b8030bc88c51340403ba8134` passed
native x86_64 Rust1.97.1 core+daemon all-target+cluster strict Clippy in
375.492s, source stable; log SHA-256
`919eea478ff0ea42843fee5443505c4e82bf6757cb6b8f698accf5fc758fa675`.
Host source `93d9bf1bdb621336fa88c70582d92cc3667daa53` passed the full-workspace
compile/format/locked-spike check in 322.64s and strict
core+daemon Clippy in 341.224s. Only the Linux-only Source
settlement and core Direct fixtures changed afterward. Their parent modules
and Linux cfg guards are unchanged; all 7032 other
host-guard source blobs match the current pin. The changed fixtures are
covered by exact native compilation; no host compilation of those files or
exact-new-source host pass is claimed.
Host log SHA-256 respectively:
`11fadca8157bbbd1097c8caf102dfc97ddd6b56bf6b5b1ae99191d49197fb700` and
`4284e07eba21995cc2234a59f8e9df64c036e0bb3cd1112e007c52a761fa9c48`. Standalone vendored Hiqlite Clippy passed in
6.979s; log SHA-256 `51b8531a346691fcf813bb77ae192a992a37da677df0abf9b7d9607c62c524e7`.
The vendor target can report the existing root Clippy configuration's
unreachable-path warnings; no zero-diagnostic claim is made. All90 task
regression declarations resolve against this source. These are compiler and
source-name checks; no regression body was executed. Fresh current-head
compile-only CI remains required before task integration.

Earlier source `5267b8721312b86e205ac99217c20a159b0fec4a` passed native strict
Clippy in612.846s (log SHA-256
`811cf9dbfee5dd6d355e44ecb9aa1b5880033f47391b50dff6d70b2279ee8489`)
and matching host/Linux-target checks in267.793s/320.812s. Those receipts remain
bound to that earlier source; they are not the current candidate's proof.
CI6082/index6053 failed locked spike metadata before Rust compilation; its
other platform/static passes are historical results, not current-head credit.

## 16. Current registered-planning closure and execution — 2026-10-29

Registered probe and engine tasks now pass their original start latches through
the native-entry fence. Positive receiver-error non-entry requires the same
registered task join, sealed graph absence, original source-parent alias drain,
full borrowed DTO validation and durable settlement. Distinct emitted facts
preserve registered non-entry; panic, loss, mixed entry and overage retain charge.
The current run passed the five corresponding runtime cases. It also passed the
remaining 26 Core cases in its first group, then failed the static fixture's
legacy recipe resolver under its saved zero software budget. This is a real
recorded failure, not acceptance of the selected static route. The full 206-case
candidate remains unqualified until that failure and later results are resolved.

The original source-alias fixture repair clones the existing retained file Arc
without consuming the production collector's one-shot lease. Native/application
cleanup ordering and all production admission checks remain intact. No supplied
Opus review is extended to later source by this execution record.
