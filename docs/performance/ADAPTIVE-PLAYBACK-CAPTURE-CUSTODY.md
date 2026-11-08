# Capture native custody — which originals survive a cancelled observer?

**Status:** open · native/writer implementation under qualification · 2026-11-07.
Companion to the [canonical ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md) and
[owned containment](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md). This continuation
covers original local recording files, their native operations and writer
joins. Complete CaptureSink inventory, full acquisition, M0 and trials remain
open.

## Original files and jobs outlive observers

The [resident owner](../../crates/plurxd/src/live_tv/dvr/capture_owner.rs)
registers original work before storage-marker or recording-file effects.
Capture, finalizer and transport tables each have 64 slots. Capacity refusal
precedes native acquisition. These fixed counts supply no complete allocator,
constructor, executor or kernel peak authority.

The original restart admission remains in a root record outside the Entry
being destroyed. Actual outer Entry aliases retain that record. The root
consumes the original Arc/header and destroys its payload before releasing
admission or reusing the slot. A failed join, destructor or uncertain close
retains the same original state and charge.

Production recording I/O uses an owned standard-file adapter. Its file cell
exists before open and is locked before the original opening operation;
a successful File is published directly into that cell before later fallible
work. Original blocking-operation JoinHandles and results remain discoverable
outside the writer's unwinding frame. Observer cancellation and observational
timeouts cannot replace those original operations.

Storage-marker read/write/sync and recording-file operations have distinct
original identities. Positive original joins and a single checked close precede
file closure. An uncertain numeric close is not retried. Native write-copy
accounting follows actual Vec capacity and remains debited through the original
payload/frame destruction. Generic lifetime guards and elapsed time are not
native close evidence.

## Completion consults the original local work

Disabled-recording, recovery and common finalization paths consult all original
local attempts, including work absent from the live-sink registry. An expired
SQL lease cannot substitute for their actual close. A same-root recording and
epoch fence atomically blocks new capture admission while finalization runs.
The fence itself remains represented outside its observer.

Stopping seals demand before a late acquisition can publish a writer. Root
cleanup waits for the live acquisition's actual completion/drop before taking
its polling authority. It preserves the same cleanup future across observation
timeouts. Positively closed sinks can be pruned; an old raw sink cannot hide a
new unattached acquisition. Deletion also stops and waits for original local
writers before obtaining its owned marker-identity fence.

Transport resource authority excludes the manager/root and metrics registry
backedges. The original transport JoinHandle remains in a resident task cell;
a cancelled closer or failed join cannot publish `closed`. This narrow repair
does not complete the whole LiveTransport family.

Main keeps an independent original root before background entry. Shutdown seals
its first continuous deadline and still runs existing registry, session and
orphan draining when capture shutdown remains unresolved. Later observers do
not renew that deadline. Losing the external root preserves actual unresolved
records, files, handles and admissions rather than asserting closure.

## Qualification remains source-bound

The peer-reviewed Rust source is `65652f4c`, based on the attempt task. Pinned
formatting and compiler integration fixes remain exact successor snapshots.
The first compile exposed five adapter errors; the fixes include the remaining
deletion-marker lookup and stop ordering. Workspace/all-target checking passed
`8dc5b34d`.

On exact runtime source `c80f8360`, all 19 new cases passed: 17 original-owner
cases plus two concurrent/dropped-observer cases. All 40 affected existing DVR
cases and two existing session/orphan cases also passed, zero failed/ignored.
The confirming 17-case rerun adds no distinct credit. Distinct scoped total: 61.
The tests cover real pipe writes, actual expired Capture records and a positive
post-close path as well as panic, uncertainty and alias retention.

Strict Clippy required a transparent transport-task alias and descriptive
fixture expectations in place of unwraps; the catalog
needed both new source paths. Strict Clippy and full formatting pass on exact successor `7ed0b926`.
Reversing only the transparent alias and fixture panic diagnostics, then using
the pinned formatter, reproduces all four runtime-source hashes exactly.
Normal hooks, exact committed checks, paperwork and the manual effort gate
remain required.

## Remaining inventory boundaries are explicit

SQL Claim remains inline in the attach caller. A dropped observer can leave a
late Store commit and temporary durable capacity hold after native-root cleanup;
it cannot launch the later capture-file/tuner work from that dropped caller.
Owning that original coordinator/result and completing its restart SQL drain
remain pre-M/application work.

Finalizer assembly/output, unlink work, inherited scratch publication, the
whole transport graph and other-node/shared-storage expiry are not certified
by this local fence. Whole rich payloads, constructors, allocator/executor
cells, complete native/kernel peaks and census-family completeness remain
Unknown or unbridged. No full-family acceptance, real-trial eligibility,
current-main qualification, Main promotion or deployment is claimed.

## Earlier backlog checkpoint

The prior backlog description is preserved below as historical source-bound
status; its former pending steps do not override current qualification.

**Selected bridge/direct applications built; static P/M: 63 scoped passes; original finite driver: 13 daemon passes; opaque gates: five daemon passes; effort #1248 draft; V1–V5 OPEN.** #1375 merged after gate 6437.

[Canonical ledger](../performance/ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md): 66 distinct source-scoped
passes and seven baseline exceptions. The [direct-selector follow-up](../performance/ADAPTIVE-
PLAYBACK-OWNED-CONTAINMENT.md#direct-retirement-follow-up--original-selectors-replace-permanent-
aliases) pins the blocker, seven actual-read/three affected daemon passes and #1377 candidate
gate/landing. The [ordinary collector](../performance/ADAPTIVE-PLAYBACK-ORDINARY-COLLECTOR.md)
merged as #1392 after gate 6512 with 48 source-scoped passes. [Retained S
exit](../performance/ADAPTIVE-PLAYBACK-RETAINED-EXIT.md) has 14 scoped passes; #1419 merged after
gate 6591. [Pre-P attempt ownership](../performance/ADAPTIVE-PLAYBACK-ATTEMPT-ROOT.md) has 31 scoped
Rust passes; final checks pending. The [Proposed support ADR](../performance/ADAPTIVE-PLAYBACK-
FINALIZATION-SUPPORT.md) scopes fixture-only independent finalization custody; complete production
peak audit and full R/S closure remain open. M0/inventory, producer bounds, trials, current-main
integration and Main promotion remain open. No deployment.

## Earlier canonical checkpoint

Draft effort #1248 includes merged task PRs #1293, #1338, #1360–#1365, #1368,
#1371 and #1375. The latter landed at `edb5b79f` after eight green jobs in gate
6437; #1368/#1371 passed gates 6411/6422. [Owned containment](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md)
keeps the earlier 44 cases separate from 51 selected-bridge core passes on
production-identical `6ee93330`/`ca85f07f` and three daemon passes on `ca85f07f`.
Seven unchanged overlap failures remain reproduced baseline exceptions.

[PR #1377](http://forge.lan:3000/noirr/plurx/pulls/1377) records the exact-candidate
gate and landing for the built direct-retirement amendment. Exact `7658db4c` execution
reproduced three core and two daemon ticket-retention failures. The
[original-selector follow-up](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md#direct-retirement-follow-up--original-selectors-replace-permanent-aliases)
records the bounded repair and two new actual-read cases at source tree
`4cb874ba`: 21 focused core and three affected daemon cases pass. Distinct source-scoped total: 66.
Exact-candidate checks are recorded in that task PR.
The earlier 51-case qualification retains its original source boundary.
The [direct application continuation](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md#direct-applications-retain-original-workers-and-whole-payloads)
records 17 scoped passes for original-worker and rich-payload reference custody; full M0 stays open.

The [static P/M continuation](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md#static-pm-counts-actual-controllers-entries-and-inline-futures)
now counts the actual original controller, inline encoded pass and escaped
entry aliases, with core custody of the same heartbeat and compact independent
cleanup. Its 63 distinct source-scoped passes are recorded there. The pre-M
census span and all family coverage remain unbridged.

[Finite driver custody](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md#finite-vod-keeps-the-original-driver-through-every-join)
records 13 scoped daemon passes, zero ignored. Opaque build gates preserve
original mutexes; rich R/S and pre-Winner whole-F closure remain open.

The [finite ledger](ADAPTIVE-PLAYBACK-DELIVERY.md#finite-lifecycle-and-restart-qualification)
and [policy ledger](ADAPTIVE-PLAYBACK-PRODUCTION-POLICY.md) retain their earlier
reader/lifecycle/restart and fourteen refusal/service results. #1365's
controlled two-node and exact-owner publication evidence remains in the
[delivery log](ADAPTIVE-PLAYBACK-DELIVERY.md). None establishes naturally
selected performance or M0–M7 acceptance.

Still open: full inventory/M0, M3a/C2 native and allocator bounds, production
trials, V1–V5, broader adapters/platforms, current-main integration and Main
promotion. The actor-application lease regression and selected-static FFmpeg
`-display_rotation` decision remain separate open items. No deployment.

