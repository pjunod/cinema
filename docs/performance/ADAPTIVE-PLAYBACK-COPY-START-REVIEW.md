# Copy start cancellation — design decisions before implementation

**Status:** open · revised after owner review · **Written:** 2026-10-26.

Companion to the [adaptive playback ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md)
and [delivery checklist](ADAPTIVE-PLAYBACK-DELIVERY.md). This asks for bounded
copy-specific design decisions. No partial-start adapter is implemented or
qualified by this document. Behavioral tests remain unrun pending review.

Source is committed on `effort/adaptive-playback-resources`, draft PR #1248,
at `41ad7bebad6c1597ff0bbb0d82849bd13f128278`. The [source manifest](ADAPTIVE-PLAYBACK-COPY-START-REVIEW-SOURCE.json)
pins the relevant file hashes. Read that commit rather than unrelated main or
an uncommitted worktree; normal Git fetch/show supplies every cited file.
This source pin is an ancestor of the continuing effort and remains stable.

**Original** means the same retained instance: Child/ChildJob, JoinHandle, pipe
cell, source cell, latch, graph custody and named operation. Matching IDs,
reopened paths, recreated commands or replacement tasks are insufficient.

Assume trusted fixed software and its known helper topology, as recorded by C1
in the delivery checklist. This limits the threat model; it does not prove any
process, descriptor, job or application owner closed.

## 1. Review the pinned implementation

Short `physical_graph…` and `runtime/…` paths are under
`crates/plurx-core/src/adaptive_resources/`; `process/…` is under
`crates/plurx-core/src/`; `codec_v2…` is under `src/resource_custody/`.

### Existing definitions and proposed names

The following definitions exist in the included commit:

| Definition | Source location |
|---|---|
| `CopyCancellationClosure`, `CopyCancelProcess` | `crates/plurx-core/src/adaptive_resources/runtime/operations/copy_close.rs:10–18` |
| `CopyFixedImage` | `crates/plurx-core/src/adaptive_resources/physical_graph/copy_vod.rs:24` |
| `FiniteVodNeverRootClosure` | `crates/plurx-core/src/adaptive_resources/physical_graph/finite_vod.rs:188` |
| `CopyTaskSummary` | `crates/plurx-core/src/adaptive_resources/physical_graph/copy_vod.rs:135` |

`CopyRootOnlyClosure`, a root-entered/no-child witness and a typed `NeverEntered`
terminal are **proposals**, not existing definitions. Their absence is part of
the decision. The existing no-root token's software-oriented name does not
permit substituting a software image/launch/writer proof for copy evidence.

### Current launch wiring is blocked before the proposed partial states

`runtime/operations/copy_vod.rs:412–421` passes a named acquisition into
`launch_root_role`. The named-arm branch at `runtime/operations.rs:1171–1179`
requires `role.finite_vod`, while copy installation rejects that field and
installs `role.copy_vod` (`operations/copy_vod.rs:174–190`). The pinned source therefore
returns `State` at that branch. The entered-root cases below are required
acquisition boundaries to support when that wiring is repaired; the packet does
not claim current copy execution reaches them.

## 2. Four partial states with explicit entry and exit boundaries

These states partition the process/task start sequence. Prefix ownership must
also remain recorded when a later state is reached. A journal acknowledgment may
be pending independently of the native step; retain its exact operation.

### S0 — output prefix acquired, process root not entered

**Entry:** a bootstrap prefix owns filesystem roots/jobs/observations, while the
process-entry slot has not been consumed. **Exit:** process-root entry begins.
The retained source cells remain part of the bundle throughout.

Current no-root cleanup rejects bootstrap/scratch/root activity
(`runtime/operations/copy_close.rs::prepare_copy_never_root_close`). Bootstrap
retirement also expects the joined copy process path. It cannot label this
acquired prefix `NeverAcquired`.

**Required decision:** which copy-specific process-nonentry witness remains
valid alongside an acquired prefix, and how its actual bootstrap arm, closed
head and `ProducerFiniteClosure` justify source/scratch settlement. Do not
assume the existing whole-graph no-root constructor already accepts this state.

### S1 — process-root entry consumed, no Child returned or retained yet

**Entry:** acquisition and selected `entered=true` precede the spawn call in
`physical_graph.rs:388–396`. **Exit:** a returned child/job is retained (S2), or a
definitive no-child launch outcome is retained and cleaned up.

The flag and spawn attempt share an uninterrupted graph-lock scope; this is
not an extra asynchronous cancellation yield. They are **not an all-or-nothing
acquisition**: the spawn attempt can fail after entry was consumed. A launcher
error can contain no Child/Job (`process/mod.rs:126–131`); nevertheless
`retain_launch_failure` sets
`uncertain_launch=true` (`physical_graph.rs:515–525`). The no-root constructor
refuses acquired/uncertain state (`physical_graph/finite_vod.rs:490–514`).

**Required decision:** a separate witness for entered-but-definitively-no-child,
including whether the launcher must preserve stronger outcome provenance. No
returned Child, an empty roots collection or a generic spawn error proves no
child existed. Uncertain spawn/attach/resume stays Unknown with full charge.
With no known child there is no handle on which to invent a kill/wait receipt.

### S2 — child/job retained, parser not registered

**Entry:** the retained Child/ChildJob is installed. **Exit:** the parser's
JoinHandle and pipe/work ownership are registered (S3).

Copy start sets its entry flag, then awaits source observations before starting
the task (`runtime/operations/copy_vod.rs:423–448`); task arming adds another await
before registration (`:499–520`). Thus this differs from S1. Current runtime
join requires a task and committed task observation (`:581–591`), and the copy
process witness requires a joined parser. No copy root-only witness exists.

**Required cleanup:** seal new entry; request kill of the retained root/job;
wait that root; check-close its unclaimed stdout/stderr cells; dispose its
Child/ChildJob and close the actual source/prefix owners. If a positive retained
exit status already exists, use it. Do not depend on SIGPIPE/EPIPE or reader drop
to terminate a child. The software root-only adapter is a reference for these
primitives, not copy authority.

### S3 — parser registered, start latch never released

**Entry:** the task, prepared work, pipe readers, image and sole start sender are
registered. **Exit:** successful latch delivery permits parser entry (S4), or
sender-drop is observed and the task is naturally joined without entry.

Registration uses `tokio::sync::oneshot::channel`
(`physical_graph/copy_vod.rs:359–378`). The sole sender is removed with
`Option::take` (`:384–403`); `release_native_entry` owns it by value and performs a
synchronous consuming `send` (`runtime/operations.rs:394–424`). The required
nonentry contract is:

- No cloned/replacement sender and no receiver-side `close` operation.
- The receiver is the task's **first await** (`copy_vod.rs:220`). Only prepared
  input destructuring precedes it; parser work and diagnostic draining start
  after successful delivery (`:223`). Resources already exist before delivery.
- Cancellation seals entry, takes/drops the unsent sender and joins the retained
  task. It **never aborts the task** to obtain a nonentry witness and never sends
  the latch merely to make cleanup progress. A panic/lost outcome stays Unknown.
- For a live root: request kill, wait the retained root, join the task and then
  check-close its pipes. Keep the work/image/source/prefix owners until their
  actual disposal is established.

Currently a receive error returns without a terminal summary (`:220–221`);
join then rejects missing/abandoned terminal state (`:459–471`). A proposed
`NeverEntered` result must be written only on that precise receiver-error path,
notify its waiter and remain distinct internally from entered success/failure.
The emitted-record question is explicitly open in section 4.

### S4 — latch delivered and parser entered

This is the existing full joined path's reference case, not a fifth missing
partial adapter. The retained parser and root must join, checked pipe closure
must succeed, and source/prefix owners must close before process settlement.
A completed/failed parser summary is not interchangeable with S3 nonentry.

## 3. Cleanup attempts are bounded; deadlines do not release charge

The required cancellation order for a state with a known live child is
**seal/drop an unsent latch → kill → wait root → join any registered task →
checked pipe close → remaining source/prefix closure**. No reader-drop-based
termination assumption is allowed. Decide how to connect this ordering to the
existing helpers rather than claiming it is already implemented: current
`join_copy_vod(abort=true)` requests kill, joins/checks parser pipes, then waits
the root (`runtime/operations/copy_vod.rs:603–607`).

Use the retained cleanup selector's deadline for every awaited step. Timeout,
kill/wait error, uncertain job/pipe close or unresolved journal acknowledgment
returns an incomplete/Unknown outcome while retaining handles, operations and
charges for retry. A kill request is not a verified termination-time bound.
Borrow the retained Child/JoinHandle across cancellable waits; do not move them
into a disposable waiter or launch another cleanup owner.

Existing pipe cells refuse close while a reader lives, preserve poisoned/error
state and check `libc::close` on the retained native pipe
(`physical_graph/finite_pipes.rs:74–105`). They do not retry an old numeric FD.
Root/process waits and helper-topology checks must remain real. No typed
settlement substitutes for any native witness; acquired resources never become
`NeverAcquired` merely because initialization did not finish.

Resolve captured Enrich/SourceRoot/CopyTask operations before consuming source
parents or other owners their acknowledgments need. Validate all fallible DTOs
before consuming one-shot witnesses. Destroy the rich bundle before ledger
release, and return capacity only after all native and application owners are
gone. Private native/allocator/executor peaks remain Unknown.

## 4. Decisions requested from Fable

1. **Root-entry coverage and launch wiring.** Specify the S1 no-child witness and
   legitimate emitted root facts, or identify why that outcome must remain
   Unknown. Include the earlier named-arm copy wiring correction as a prerequisite;
   do not assert atomic spawn or reuse process-never-entered proof after entry.
2. **Cancellation order and retry.** Choose the concrete retained operations for
   kill → wait → task join → checked pipe close in S2/S3, with deadline cancellation
   retaining those operations/handles. Identify any exact missing primitive.
3. **Latch nonentry witness.** Confirm the single consuming one-shot sender,
   first-await, no receiver close, sender-drop and non-aborting-join contract.
   State how a private terminal distinguishes received-drop from panic/abort.
4. **Emitted task disposition and schema.** Use these canonical names:
   `VodActor::RollingCopyV1` is an actor tag (`codec_v2.rs:371`);
   `ClosedFact::VodOutputTaskNeverAcquiredV1` is the no-registered-parser
   disposition (`codec_v2/settlement.rs:241`). They are not interchangeable names.
   S2 may use the latter only with an actual registration-absence witness.
   S3 is an acquired task, including when its acquisition observation is pending.

   Current `ClosedFact::VodOutputTaskV1` contains task identity, join operation
   and closed owners (`settlement.rs:188`), not an entry-state field. Its validator
   requires the matching observation (`:485–495`). Neither that fact nor current
   `CopyTaskSummary` encodes `NeverEntered`. **No-schema-change is not settled.**
   Choose explicitly between (a) an internal-only nonentry distinction with a
   shared emitted closed-owner fact whose documented semantics do not imply
   parser execution, and (b) an emitted nonentry distinction with the necessary
   version/codec/validator changes. Explain what a persisted record can prove.
   Resolve the exact pending registration observation before emission; its
   missing acknowledgment cannot justify pretending a registered task was absent.
5. **Adapter shape.** Compare the prior three private per-state adapters, extended
   with an explicit S1 case/witness, against **one private staged copy closure**
   retaining prefix → root entry → child → registration → latch entry and closing
   acquired steps in reverse dependency order. Ask which is smaller and safer,
   considering duplicated validation, operation retention, one-shot consumption,
   layout/accounting and cancellation transitions. A stage counter alone is not
   evidence. Do not reject the staged option merely as a “new framework”; require
   a reasoned choice and concrete constructor/transition boundaries.

Return source-cited decisions, precise implementation seams and regression
cases for all four states, failed kill/wait/close, ambiguous spawn/journal result,
latch races and last application-owner release. Do not implement changes or run
behavioral tests. The completed implementation still requires independent review
and fixes before behavioral qualification; this packet grants no acceptance.
