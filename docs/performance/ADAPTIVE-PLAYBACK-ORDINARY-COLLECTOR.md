# Ordinary finite collector — who owns close after native refund?

**Status:** open · bounded native qualification passed; task integration pending · 2026-11-04.
The [canonical ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) tracks effort #1248.
This extends the [support ADR](ADAPTIVE-PLAYBACK-FINALIZATION-SUPPORT.md);
complete M0, affected-work inventory, production containment and trials remain open.

## Installation and ordinary admission

The daemon installs one independent support hold before publishing its listener.
Its lifecycle controller belongs to the external service root. Shared state
retains only a removable client. Failed initialization refuses fresh ordinary
software R; cached static S is checked first. The common finite acquisition
path also covers adaptive preparations, preserving their ordinary R entry.
No new native role, memory claim, peak claim or trial permission is created.

The concrete pool has 256 records and 256 permanent pinned service futures,
polled by one original actor. This is an explicit fixed-capacity choice, not a
universal concurrency proof. Full slots, overlong keys, oversized service
frames, stale generations and failed readiness refuse admission. Each actual
R grant validates the same slot and Ready generation inside core's accounting
transaction and retains its original operation before initialization.

Ready requires the actor's actual first poll after its original JoinHandle is
published. A captured failure guard exists before spawn, including on a closed
executor that never polls. Cancelling a Ready observer preserves that same
root owner and actor. Unexpected failure marks actual support and census
Unknown; no replacement task or optimistic refund repairs it.

## Original gate and native close

The directory adopts the exact original legacy mutex. A key already held by
a record cannot be assigned a second mutex. Same-key restart preserves that
mutex, including a new builder's held guard; changing its key requires actual
Arc uniqueness. Closing lookup returns typed Unavailable before legacy fallback.

A permanent service retains the same producer driver and core-owned native
ticket. AbandonRegistered follows positive joining of that original driver
and installation of its closing owner; it can precede external application
lease release. Native completion still requires actual application closure,
original selectors and the fixed one-role close workspace. Core consumes an
empty, successfully retired ticket before publishing Terminal. A missing
daemon envelope uses core's retained original preparation, not a caller's
NoTask assertion. R-to-S transfer preserves stable ledger/operation/role
identity while core validates the current reservation binding.

Slot observations and envelope destruction guards prevent reuse while old
selectors or allocation headers survive. The daemon drops the actual closing
Arc/header before completing its auxiliary drop guard. A panic or unfinished
guard retains Unknown custody. Cancelling metadata/close observers leaves the
same paid service responsible for cleanup.

## What the structural allowance covers

The private profile computes actual concrete state, payload, record, mutex and
service-future sizes, plus checked construction temporaries. Each slot also
reserves a built-in 16 KiB daemon closing allowance. The daemon checks its
conservative BTree-node/key/wrapper/compatibility allocation audit before R.
Caller-supplied bytes and arbitrary production payloads cannot define this
profile. An audit exceeding the allowance refuses rather than undercharges.

These are structural requested-storage bounds. Allocator internals, arbitrary
service-root graphs, legacy directory backing, task cells, native execution
and the complete construction peak remain explicit source-audit/Unknown
limits. The independently root-owned legacy backing is not counted merely by
its Arc handle. No KnownPeak or complete R/S/pre-Winner inventory follows.

## External shutdown and release

External shutdown first seals admissions, drains sessions through their
original gates, then globally seals directory lookup. Retained admitted S or
unknown closure can honestly prevent shutdown; there is no forced eviction.
The original standing JoinHandle is positively joined before storage closes.
Actual outer payload and all descendant gate Arcs pass one batch get_mut
preflight before any payload prefix is destroyed. A normal refusal preserves
the same outer Arc.

Storage Closed and support Released are distinct. Only successful final
census/ledger commit followed by inline slot clear mints Released. Reopening
consumes that exact directory/support/generation/epoch binding once. Failed
commit, late aliases or stale witnesses retain Closing and its support charge.

## Qualification boundary

The following distinct focused cases pass. The 47 unchanged cases ran on
source tree `34b5537300dd1cb106d86073529cba0284ede91f`; the corrected
cancellation observer case ran on `f5faf3a6f730001d11eb12979aacecc8e3a0f953`.
Their production Rust blobs are identical. The test-only correction replaces
a stale pre-Adopt error expectation with actual Pending/cancel/same-driver
identity checks; the interrupted stale test is not counted as a pass.

| Boundary | Passed |
|---|---:|
| Fresh RAM, cohort and trial-policy support preflight | 3 |
| Original support actor, payload, destructor and close lifecycle | 11 |
| Concrete pool/gate/Ready/Released protocol | 10 |
| Actual finite ticket cleanup and fixed workspace | 8 |
| Real R-to-S identity and retirement readers | 7 |
| Installed daemon collector R lifecycle, restart, cancellation and abandonment | 5 |
| Existing original-driver compatibility, including corrected cancellation | 4 |
| **Distinct source-scoped total** | **48** |

Mechanical lint corrections on source `c8c0c70eb336f708d265353433a988ce4d7132bd`
preserve inline custody, atomic orders and checked increment semantics. The
25 affected pool, ticket and reader cases passed again; they add no distinct
cases to the total. Pinned formatting and strict workspace Clippy pass.

The daemon's audited closing allowance measured 8,749 bytes against the
16,384-byte reservation. Native workspace/all-target checks and pinned
formatting pass; the task PR records strict lint, normal hook and exact
candidate/gate results. Prior support and driver receipts retain their
original source scope. No merge, deployment, full inventory, M0 acceptance or
real-trial readiness is claimed here.
