# Finalization support ADR — independent custody before a real collector

**Status:** Proposed · fixture-only stage 1 source-qualified; task landing pending ·
**Date:** 2026-11-04 · **Deciders:** adaptive M0 implementation and independent
source review. Companion to [owned containment](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md)
and the [canonical resource ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md).
The [resource plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md) remains normative.

## Context — the finalizer cannot refund its own frame

The current finite closing future can still own its key and rich closing state
when R is refunded. A future production collector must retain and positively
join the original closing/abandon future before returning that capacity. The
collector itself needs independent custody before it can own work across R/S
refund. This stage creates that private accounting and lifetime boundary.

Stage 1 has no production profile constructor, pool allocator, standing actor,
new native role, disk claim or trial permit. Its fixture exercises actual
payload destruction and the original task handle. It does not close the
current R refund gap or establish complete R/S, pre-Winner or M0 ownership.
Production playback has no installed support hold and keeps its prior policy.

## Decision — one private support hold per runtime

1. **Keep support separate from native claims.** The private ledger entry adds
   its fixed requested storage allowance to `used`. It cannot yield a native
   reservation, retained role, spawn latch, physical receipt, adapter coverage
   or complete peak claim. Native P/M and `MemoryClaim` spellings stay intact.
2. **Require actual fresh Known RAM under the ledger lock.** The runtime seam
   refreshes capacity first. Installation then checks the original epoch,
   owner and ledger identity, a nonfuture `Some(observed_at)` no older than one
   second, and sufficient Known RAM. An unchanged capacity epoch is not a
   freshness witness. The producer's serialized-cold Unknown-RAM exception
   does not apply; support creation refuses `InsufficientEvidence` instead.
3. **Commit census and accounting together.** Lock order is census → runtime
   support slot → ledger. All identity, arithmetic, capacity, duplicate, trial,
   drain, native-overage and fixed-slot checks precede insertion or allocation.
   Context methods do not re-enter the census. Destructors and lost selectors
   only set deferred atomics; they never acquire these locks.
4. **Preserve Unknown aggregate peaks.** The allowance is requested structural
   storage, never a complete memory/executor bound. Support has separate
   diagnostics; producer counters and OS process roots stay producer facts.
   Its logical record/task slots do not represent kernel tasks. Both fit paths
   and direct/runtime trial paths consult support. Installation itself refuses
   more than one already active producer, using active memory claims even when
   those producers have zero OS roots. One conservative producer may coexist;
   a second refuses while support's complete peak is Unknown.
5. **Retain the original runtime through actual destruction.** Installed
   custody intentionally pins the original runtime, journal and daemon lock.
   Losing a selector cannot allow same-process reopening while old objects
   survive. Ephemeral support is not reconstructed from native SQL claims.

| Alternative | Why this stage declines it |
|---|---|
| Reuse PlanningMetadata | The exact probe P/M exception would acquire unrelated authority. |
| Reuse retained residency | Native residency requires the original proof-bearing R→S transition. |
| Reserve another producer | Support would falsely count as an encoder and obscure its independent Unknown peak. |
| Let the closing future refund itself | Its own frame and retained graph can survive the refund. |

## Lifecycle — Closing owns the barrier independently of dynamic storage

```text
ReservedUnallocated ──▶ Active ──▶ Closing ──▶ Closed
          │                │          │
          └── never allocated ─────────┘
                           └──────────┴────▶ UnknownHeld
```

Only core's proof that allocation never became possible permits an unallocated
close. Active Drop, cancellation, deadlines, missing selectors and failed joins
never refund. A failed original join is sticky; its completed handle is not
polled again and original custody remains charged.

Before detaching dynamic state, the inline runtime slot enters Closing and
retains the original identity, census token, charge and independent runtime
pin. Replacement is blocked throughout destruction and final commit. An
external shutdown caller joins the original task; the standing task cannot
join itself. The caller's compact shutdown frame belongs to independent root
bookkeeping, outside the support storage being destroyed.

The synchronous destruction helper runs outside every census/slot/ledger lock.
It requires sealed registration, positive original join with outputs and
handles destroyed, and unique original State and payload ownership. Escaped
payload aliases are opaque wrappers around the actual payload allocation;
there is no exposed Weak. Failed uniqueness restores the same original State
Arc under Closing and keeps the charge. The helper destroys the actual
payload, State, arena/handle storage and their Arc allocations/headers before
returning only a compact scalar identity proof.

Positive join and handle destruction prove destruction of the original F and
its output. They do not prove deallocation of Tokio task-cell backing: runtime,
completion or waker references may retain it. That executor/allocator storage
remains explicitly Unknown and needs the production audit. The unique State
and payload Arc/header destruction proof does not extend to executor backing.

A destructor panic leaves the inline Closing/runtime pin and numeric charge
UnknownHeld even when the detached State no longer exists. A successful proof
is retained in the slot if final validation refuses. Final census → slot →
ledger commit checks original identity, row and subtraction before clearing
the hold. It extracts the runtime pin and drops that pin outside locks. Native
retirement, retained conversion and capacity changes preserve the additive
hold. Startup reconstruction creates a fresh ledger; it cannot reset a live
support hold. Empty native entries do not make the runtime drained.

## Fixture profile and diagnostics — allowance is not a complete bound

The only constructor is compiled for tests: 64 KiB of requested storage, one
logical record slot and one logical task slot. Size checks cover the concrete
payload/state wrappers and actual wrapped future. Runtime/executor allocator
and kernel-thread contributions remain Unknown; the fixture does not certify
CPU zero or `KnownMemoryPeak`. A production layout/frame audit must replace
this fixture evidence before a production constructor exists.

`finalization_support` is omitted from snapshots when absent. When present,
its requested bytes and logical slots describe the fixed allowance; phase
shows ReservedUnallocated, Active, Closing or UnknownHeld. Aggregate managed
memory and unaudited task peaks remain Unknown. `held`, producer-unknown counts
and process-root counts do not silently acquire support owners.

## Qualification and next work

Reviewed source tree `dbf4b24b140688bcf93a340851b24c5d0dd9579c` passes fourteen
new policy/lifecycle fixtures, zero ignored. These include freshness/Unknown
RAM, zero-root producer cohorts, actual State/payload aliases, canceled
waiters, original self-join refusal followed by external close, repeated
JoinError, epoch exhaustion before factories, wrapper-size refusal before
factories and destructor panic.

The same source passes 24 existing scoped cases: seven ledger/accounting,
eleven claim/headroom, five R→S accounting and one original-observer join.
Six older ledger fixtures fail identically on this tree and exact pre-change
`4a40c1700a92f4c1b3262ea40310f90c167e8545` (tree `dfd42a69`):

| Existing ledger fixture | Same failure on both sources |
|---|---|
| `adaptive_overlap_is_atomic_for_four_and_six_roles` | Positive overlap refuses `InsufficientEvidence`. |
| `shared_retained_role_is_counted_once_and_never_rebound` | Positive missing-video overlap refuses `InsufficientEvidence`. |
| `reservation_rechecks_all_dimensions_and_epochs_atomically` | CPU HardLimit precedence assertion. |
| `uncertain_drop_holds_trial_until_original_cleanup_reconciles` | TrialAlreadyHeld precedence assertion. |
| `unknown_bounds_and_overflow_never_become_spare_capacity` | ArithmeticOverflow precedence assertion. |
| `overlap_rejects_seventh_role_third_family_and_overflow_without_mutation` | ArithmeticOverflow precedence assertion. |

All six test names and assertion messages were compared against the actual
base execution: seven passed, six failed, zero ignored on each source. These
are six ledger exceptions, separate from the earlier seven operation-book
exceptions in the canonical ledger. No passing evidence is claimed for them;
the governing refusal and their assertions remain unchanged.

Later source tree `f6a904e17161781c80952c372e8aeafe1449d116` passes the
private census-registration linearization and additive-support R→S cases,
zero ignored, plus pinned formatting. These are two additional distinct
source-scoped passes; the fourteen-case evidence above retains its DBF source
boundary. The full workspace/all-targets check passes on F6. After the tiny
Notify destructor-lint cleanup, tree
`c993ba1cacfe511b63c3cac10dd14d9bbe764424` passes pinned formatting and strict
workspace/all-targets Clippy; the affected positive actual-close and actual
destructor-panic cases both pass again, zero ignored. These reruns add no new
distinct cases. The Notify layout observation is tied to locked Tokio 1.53.1;
executor task-cell backing remains Unknown.

There are sixteen new and 24 existing distinct source-scoped passes in this
task, with the six baseline exceptions above. Final intended-base integration,
exact-candidate qualification and task landing remain pending.

Before production construction, audit the actual pool records, keys, status,
slot selectors, standing finalizer frame and transient workspace. Keep every
per-R ticket graph charged to R/S until actual native/application settlement;
consume and discard its empty ticket before publishing terminal status.
Replace or independently account transient pending-close vectors across
refund. Then qualify original closing/abandon future joins, full inventory,
pre-Winner ownership, broader adapters and complete producer/task bounds.
Real trials and Main promotion remain open. This task deploys nothing.

Source: [ledger support](../../crates/plurx-core/src/adaptive_resources/finalization_support.rs),
[census bridge](../../crates/plurx-core/src/adaptive_resources/owner_inventory/support.rs),
[runtime custody](../../crates/plurx-core/src/adaptive_resources/runtime/finalization_support.rs).
