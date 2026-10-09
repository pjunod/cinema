# Preparation buffer pressure — adversarial review of the repair contract

**Status:** approved to build after corrections · **Reviewed:** 2026-10-08 · **Source:**
`079960dae` · **Scope:** implementation plan, before Rust implementation.

Companion to [the incident and implementation ledger](../streaming/PREPARATION-BUFFER-PRESSURE-RCA-AND-IMPLEMENTATION.md).
This review checks whether another agent can implement the proposed repair
without inventing its ownership and failure semantics. It does not authorize
production changes. The parent investigator owns the plan, index and backlog.

## Verdict — approved to build after F1–F3 corrections

The causal explanation agrees with the inspected source. Preparation release
removes the exclusion while source segments remain charged, and a viewer can
only evict its own rendition. The immutable ordinary/private accounting domain
added during review prevents the mutable preparation option from deciding byte
ownership. The plan also correctly requires copy and encoded regressions,
ordinary-pressure coverage, exact incarnation fencing and cancellation-safe
settlement.

The initial findings below concerned concrete missing design, not a claim that
the plan endorsed unsafe behavior. Its invariants prohibited these failures;
the added §§3.1–3.3 now specify how ownership must satisfy them. No P0 issue was
found. All three findings are resolved at the design level. Retain the findings
and acceptance cases for the actual implementation review.

## F1 · P1 — specify the reservation handoff after assembly consumes it

**Evidence:** In
[`retained.rs`](../../crates/plurxd/src/vod/retained.rs),
`reserve` replaces the preparation's cap with zero after charging the retained
wire bytes (lines 1269–1273). `expose_prepared` removes the preparation entry
(line 1447). `PreparationAllowance::release` later removes that same reservation
in [`copy_preparation.rs`](../../crates/plurxd/src/vod/copy_preparation.rs)
(lines 548–562). Retained collection releases `artifact.charge` when its
directory is removed (`retained.rs`, lines 1597–1604); it does not inspect the
preparation's original hard links.

**Failure:** Keeping the current allowance alive after exposure does not keep
capacity reserved: its reservation is already gone. If temporary source unlink
fails and the retained artifact is subsequently collected, the remaining source
inode has neither a retained charge nor a preparation capacity reservation.
Conversely, retaining the full cap alongside the complete retained charge can
double charge one hard-linked body and reject a preparation that should fit.
Init, identity and playlist bytes also need owners; media-only counter
conservation is insufficient.

**Required correction:** Specify the exact capacity representation and atomic
handoff for active preparation, assembly in flight, exposed artifact and private
cleanup. Name which owner pins the retained artifact until temporary media links
are gone, which capacity backs residual nonlinked metadata, and what happens on
assembly failure before every link exists. Keep the reservation or equivalent
charge until the last physical owner is settled. State the admission equation
and count bound, and avoid reacquiring unreserved capacity only after a failure.

**Acceptance:** With a near-full retained budget, fail source unlink after
successful exposure, force retained collection and attempt another preparation.
Outstanding source bytes remain backed, collection cannot prematurely release
their capacity, and eventual retry releases each charge once. Repeat with
partial link creation and failure before exposure. Assert init/identity/playlist
ownership and no artificial full-body double charge on successful handoff.

## F2 · P1 — install the private owner before cancellable construction

**Evidence:**
[`serve/create.rs`](../../crates/plurxd/src/vod/serve/create.rs)
reserves capacity and calls `resolve_rendition` with only an allowance nonce at
lines 127–136. It then awaits the lease snapshot at lines 139–142. The
`CopyPreparation` is created at line 164 and the drop guard only at line 199.
[`shared.rs`](../../crates/plurxd/src/vod/shared.rs) constructs and installs a
rendition, charges adopted media and starts its driver before returning
(lines 418–449). Construction uses a detached owner. The private key is a hash
including the nonce (`serve/create.rs`, lines 428–439), not an on-disk ownership
record that a new process can recover by decoding the directory name.

**Failure:** A request can be cancelled after private rendition installation
but before `PreparationRun` exists. A design that only schedules retirement from
that guard misses this interval. Passing a private/ordinary tag without its
capacity owner still permits an unbacked private directory. A restart also loses
the in-memory domain, so the test row promising safe startup reconciliation
needs a defined discovery and adoption rule.

**Required correction:** Pass the immutable private domain and owned capacity
into the detached construction transaction before its first filesystem await.
Specify who retires an installed-but-unclaimed rendition on every early return,
and how cancellation before installation transfers or releases ownership.
Cover metadata and initialization writes, not just segment publication. Define
how startup identifies and accounts for leftover private scratch before new
background admission, including scratch from the old implementation. If complete
historical classification cannot be proved, document a conservative bounded
reconciliation rule instead of guessing from a hash.

**Acceptance:** Deterministically cancel before installation, after installation
and during lease snapshot; fail logical validation and metadata reservation;
verify private ownership, bounded cleanup and no ordinary charge at every exit.
Restart with partial and complete private scratch plus retained hard links and
prove that adoption cannot turn it into ordinary playback pressure or admit new
work against unaccounted leftovers.

## F3 · P1 — define a retry owner that does not inherit purge's error handling

**Evidence:**
[`shared.rs::purge_if_dormant`](../../crates/plurxd/src/vod/shared.rs)
detaches settlement after exact map removal, which is useful cancellation
machinery. However, it ignores `perform_driver_step`'s termination result
(lines 952–959), subtracts residual manifest bytes even after failed unlink
(lines 963–968), ignores identity-file deletion errors (line 979), and then
drops the owner. The plan's instruction to reuse purge machinery must explicitly
exclude these failure semantics for private preparation. Similarly,
[`copy_preparation.rs::fence_cancelled_epoch`](../../crates/plurxd/src/vod/copy_preparation.rs)
ignores its termination result (lines 496–505).

**Failure:** Reusing these helpers wholesale can satisfy prompt retirement of
the registry entry while abandoning a still-writing child or undeleted files.
A detached task alone provides cancellation independence, not retry durability
or a bounded queue. The plan says failures are observable and bounded but does
not yet name the state, retry cadence, admission behavior or terminal success
condition.

**Required correction:** Specify one maintenance-owned cleanup record holding
the exact rendition/epoch, required capacity/artifact leases, producer state,
pending publications and remaining paths. Define a short state machine from
fencing through confirmed termination, unlink and final charge release. Failed
termination must retain the process owner and block release; failed unlink must
retain only the appropriate still-backed claims. State retry work/count limits,
fairness across failed records, shutdown/restart treatment and observability
fields (at least identity, phase, bytes, age and last failure). Keep lock order
explicit so retries cannot deadlock assembly, reader attachment or same-key
construction. Do not hold a synchronous mutex across filesystem/process awaits.

**Acceptance:** Inject termination and unlink failures independently and
together, cancel the caller while settlement runs, and execute multiple
maintenance ticks. The record and charges survive, no later write escapes its
domain, another cleanup can progress, and clearing the fault leads to one final
release and prompt driver notification. A stale cleanup record must leave a
new incarnation's files and counters untouched.

## Checks retained without additional findings

The incident attribution is supported by the source and the investigator's
reported timing plus exact older-job byte reduction. This review did not read
the private raw log or query production, so it independently verifies the code
mechanism rather than certifying the historical raw observations. The plan
correctly distinguishes the observed copy job from the analogous encoded risk.

The small-budget, production-path regression is appropriate: it must demonstrate
live segment progress, not only compare counters. Preserve the explicit
publication-versus-release barrier test; the old writer charges `working_set`
before `PendingFootprint::commit(true)` and ignores its false return
([`generation.rs`](../../crates/plurxd/src/vod/generation.rs), lines 1467–1472).
The immutable domain must apply even during that interval, including init and
identity publication.

The single bounded PR to `main` is consistent with the ordinary-change workflow.
The plan correctly keeps its pre-build plan review separate from the required
adversarial review of the actual draft PR, then ready/affected-surface gate,
regression anchors and exact landing message. It also correctly requires local
Rust 1.97.1 evidence and the affected unit suite instead of treating compile-only
CI as unit coverage. Main movement invalidates evidence about the candidate;
merge remains distinct from deployment and physical Apple TV acceptance.

## Re-review — concrete ownership corrections

The added implementation-plan §§3.1–3.3 address F1 and F3 at the design level.
F1 now requires an atomic retained/scratch accounting transfer, keeps residual
scratch capacity and pins the retained artifact through failed cleanup. F3 now
defines a maintenance-owned record, verified producer settlement before unlink,
bounded retry backoff, inclusion in the artifact count ceiling and operator
diagnostics. Their acceptance cases above remain required evidence in the
implementation review; approval of the contract is not evidence that code
already implements it.

F2 is also resolved by the final §3.2 correction. The owning handle precedes
construction, the marker precedes every private owned-file write, and marker
construction has bounded constructor ownership. Bounded, resumable startup
inventory conservatively reserves legacy unmarked bytes without treating a hash
as private identity. Incomplete or unreadable inventory closes new preparation
admission while leaving in-budget ordinary playback available. Exact verified
adoption transfers ownership; unresolved bytes remain charged and observable.
The plan requires both old preparation and ordinary legacy-cache fixtures.

The internal marker is a justified scope addition. The revised non-goal excludes
a new media format, and the marker contract explicitly preserves public wire and
media formats. There is no remaining scope contradiction.

**Final verdict:** approved to build. The final diff still requires its own
adversarial review and the named failure, race, conservation and playback
regressions. This verdict does not claim implementation, test, deployment or
physical-device acceptance has occurred.
