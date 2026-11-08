# Attempt root — who owns pre-Winner storage through final destruction?

**Status:** open · scoped task merged; broader inventory open · 2026-11-07.
Companion to the [canonical ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) and
[owned containment](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md). The
[ordinary collector](ADAPTIVE-PLAYBACK-ORDINARY-COLLECTOR.md) owns native finite
cleanup. This document covers original attempt allocations and reply copies.
Full M0, inventory completeness and production trials remain open.

## Paid records precede original allocation and capture

The separate attempt-support kind installs before listener publication.
External service ownership keeps shutdown authority; the transcode manager
receives a removable client. Ordinary and attempt support remain additive
under the same census/ledger transaction and retain their original kind,
runtime, generation and postcommit-release identities.

The [core attempt root](../../crates/plurx-core/src/adaptive_resources/runtime/finalization_support/attempt_root.rs)
has 64 original-attempt records. Slot admission precedes the Entry allocation,
channels and captured task inputs. Admission and sealing share one lock.
Coordinator/control slots are claimed before creating duplicate channels or
spawning another task. Every Entry reference uses opaque original custody;
original JoinHandles survive cancelled observers and retries.

Typed generic custody controls actual Arc/header/payload destruction. It
supplies no reachable-heap or peak authority. Fixed private partitions and the
concrete daemon's actual-capacity preflight define the adapter's requested
storage. Wire length alone cannot bound decoded String or Vec capacities.

| Each original attempt; 64 records | Requested partition |
|---|---:|
| Entry, outer header and destruction wrapper | 16 KiB |
| Original coordinator F | 512 KiB |
| Original control F | 256 KiB |
| Request envelope heap | 256 KiB |
| Each of two claim copies | 32 KiB |
| Planning snapshot heap | 2 MiB |
| Original reply DTO | 96 KiB |
| Original fence | 96 KiB |

| Each independent response; 64 records | Requested partition |
|---|---:|
| Decoded reply DTO | 96 KiB |
| Parsed preview | 32 KiB |
| Serialized wire backing | 256 KiB |
| Origin derivatives and activation captures | 4 MiB |

These fixed partitions reserve 512,757,760 bytes on the audited 64-bit layout
(about 489 MiB), including the separate response-header allowance. Exact root/state/storage
metadata and a dedicated 64 KiB standing-service partition are additional.
Every capture and service F must fit its own partition. Complete construction,
allocator, executor/task-cell and captured-manager graph peaks remain Unknown.
The original 16 MiB prepared-recipe partition stays M-owned after construction;
producer, rendition, client and ordinary-closing storage keep separate budgets.

The origin partition bounds the actual decoded recipe, a fixed audited set of
activation DTO copies and the activation F separately from serialized wire
storage. Its copy counts, capacity preflights and output lifetime are under
source review and native qualification; the allowance alone supplies no proof
that an untracked derivative or task result has closed.

## Actual Entry destruction releases the original P/M fence

The root record holds the original P/M application lease outside the Entry
being destroyed and outside its destructor's unwinding frame. Actual Arc
uniqueness permits consuming the original header and dropping its payload.
Only successful destruction releases that lease and permits slot reuse.
A destructor panic or failed original join retains Unknown custody and charge.

R finishing has a separate dependency: native R retirement waits for its
application lease. Root custody first prepays the surviving Entry/header,
terminal metadata and applicable Fs. Actual R-owned rich payloads and original
tasks close before that R lease releases. Only the terms actually transferred
can leave an earlier R projection. The order is:

```text
fund surviving original attempt storage under the root
  -> close actual R application owners
  -> native R retirement and original Store close
  -> remove terminal registry ownership
  -> destroy actual Entry header and payload
  -> release original P/M application fence
```

A registry removal or terminal flag cannot substitute for allocation closure.
Shutdown retries retain the same original root and use the same continuous
deadline while outstanding attempts and response storage drain.

## Response funding follows the original serialized backing

Every original or replay response obtains its own copy permit before cloning.
Explicit consuming handoffs retain that permit; full-DTO dereferencing and
unfunded cloning are unavailable. Serialization has a separate bounded budget.

Core keeps the original serialized Bytes while transport consumers hold
aliases. Successful `try_into_mut` recovers that original allocation only when
those aliases are gone; core drops the recovered backing before returning the
response record to Idle. The standing actor can retry this real uniqueness
check; a timer supplies no destruction proof. Attempt and response records
therefore have separate lifetimes.

## Qualification records 31 distinct scoped passes

The task is based on actual effort `66461276151bc3b3eb5e614d6d006ff96a2d0da4`.
Its tree matches the prior B stack base; the original adoption-sequence fix and
`any_support()` recovery guard coexist unchanged. The 31 distinct Rust cases
have passed across recorded source scopes: 20 core/original-task cases at
`77abdda0`, five origin cases at `76c4595f`, four real native cases at `f26c6ea0`,
and two RPC cases rerun at `fc8bd0d5`. Reruns add no distinct credit. The final source corrections
have independent review. Strict Clippy, workspace/all-targets compilation,
the affected RPC rerun and full formatting pass at `fc8bd0d5`; normal hooks
and the manual effort gate passed before task [#1429](http://forge.lan:3000/noirr/plurx/pulls/1429)
merged at `4dabcb100`. Gate 6675 passed all eight jobs on `340b73b8`;
candidate/landing tree `b8dd2379` and all 32 regression trailers match.
The exact committed checks and normal hooks passed. Full qualification scope
and the compile-profile boundary remain recorded in the task PR.

The native fixtures initialize the same ordinary collector before attempt
support as production startup. They exercise stored-snapshot P/M work before
real R; foreign and refused-Adopt fences; original-deadline retirement; and
adopted route publication. An escaped original attempt reference holds M after
R/Store close until actual Entry/header destruction. Core tests cover original
reference destruction, destructor failure, response aliases, shutdown retry and
support-kind separation. RPC cases preserve the original grant when reply
admission refuses and permit already-owned cancellation during fresh sealing.

Dynamic reply backing stays core-owned through transport aliases. Failed
publication has an exact static 68-byte JSON body, preserving status and wire
format while allowing actual root release while static body bytes remain live. Its strict
pointer regression uses one real static array; the earlier const-literal
pointer failure remains a failed run, not evidence of a heap copy. The first
native fixture refusal remains recorded too; adding actual ordinary startup
preserved admission guards and original deadlines.

Earlier standalone `e722bc9f` and `de3711bb` compiler passes do not qualify the
combined tree. All ten family-completeness adapters, production owned-worker
containment, complete allocator/executor/native peaks, real trials,
current-main qualification and Main promotion remain open. No deployment.
