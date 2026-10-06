# Shared ingress custody — ownership across a cluster hop

**Status:** accepted design; implementation and qualification open  
**Date:** 2026-10-06  
**Decision owner:** Root, coordinating the Source and receiver builders under
Paul's instruction to resolve architectural causes.

This decision extends the [implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md).
The [status page](SHARED-LIBRARIES-STATUS.md) records actual integration and
evidence. It does not replace the contract's physical closure requirements.

## Context

A request can arrive at a cluster node other than the Source producer or B
receiver owner. Forwarding it to that owner is necessary, but the internal
response writer is not the outer ingress writer. The internal response may
finish while the ingress still has bytes queued to its accepted client
connection. Joining only the owner's internal writer therefore cannot prove
that all Source/B writes have closed before End succeeds.

The same cause exists on both sides of sharing. Duplicating a monitor or
adding a timeout would not create the missing closure evidence. A durable
owner assignment likewise cannot manufacture a physical actor or writer.

## Decision

Use one node-local accepted-driver custody mechanism, with principal-bound
durable obligations for Source and B playback:

1. Capture the actual accepted connection's private identity, closure observer,
   hard-cancel and graceful-drain controls at ingress. Register its exact
   obligation with the physical owner before exposing forwarded bytes.
2. Authenticate the internal request as a current cluster member and bind the
   obligation to principal, session incarnation/epoch, ingress node and boot,
   and registration identity. Routing metadata never supplies physical proof.
3. Seal registration when retirement starts. The owner requests closure of
   every outstanding ingress obligation and waits for the exact acknowledgement
   produced after the captured driver actually closes. Supersession drains;
   revocation/deletion can hard-close. Source End and final retirement follow
   those joins.
4. Retry lost registration/closure replies with the same identity. Missing
   registry entries, an unreachable ingress, a new process boot and elapsed
   deadlines are unresolved states, not successful closure receipts.
5. Deduplicate by accepted connection and session, not by HLS segment. Bound
   active obligations per route/node. A principal registration ordinal is
   separate from physical driver identity: an existing keepalive connection
   can register for a later playback. Retain a per-node registration high-water
   mark so reclaiming a closed slot cannot let a late retry reopen it. Check
   exact active replay before rejecting older ordinals. An unresolved register
   holds that principal's pending reservation, including after physical close;
   it neither blocks unrelated principals nor admits a later ordinal until
   reconciled. No registry mutex is held across the network exchange.

Status/control responses also need ownership if their accepted writes carry
the session binding. The existing actor/retirement owners and change signals
own this work; there is no independent playback watchdog. An End request on
a driver it must close returns a closing/unresolved answer first, permitting
that response to finish and the connection to drain. Only a later exact retry
can report confirmed End after actual closure; graceful retirement cannot
wait indefinitely for its own response-held connection.

## Options considered

| Option | Complexity and cost | Result |
|---|---|---|
| Forward through the existing media relay only | Small code change and one internal hop | Rejected: internal EOF does not prove outer accepted-writer closure |
| Add timeout-based release or infer closure from missing owner state | Small implementation, uncertain physical lifetime | Rejected: admits premature settlement and loses cleanup custody |
| Principal-bound durable obligations plus shared actual-driver close RPC | Adds persistence, exact retry state and one bounded close exchange per active ingress connection | Selected: preserves the existing End proof across a cluster hop |
| Refuse all non-owner ingress | Smallest safe interim behavior | Retained while implementation is incomplete; does not satisfy the cluster routing contract |

## Persistence and compatibility

Custody is a dedicated adjunct to session ownership, not a generic settings
blob. It has no deletion cascade that can erase an unresolved writer debt
when a stale route is removed. Both Store backends need guarded registration,
seal and acknowledgement behavior, with current-member capability checks.

The existing replicated v71 is reserved for Source-principal installation;
it cannot be reused as an ordinary additive migration. The selected migration
plan advances ordinary baseline v70 to v72 and installed Source v71 to v73;
Source installation then advances v72 to v73. The frozen Source layout marker
v71 remains distinct from the current committed schema. Exact predecessor
shape and membership/floor checks apply before mutation. Legacy v71 must have
no held Source bindings, starting requests, non-ended Source routes or Source
preparations in the same atomic migration guard. Original owners must settle
those obligations before upgrade; lease expiry is insufficient. This is a
coordinated transition, with no claim of rolling compatibility. The migration
is integrated in `69876dfce`; runtime upgrade qualification remains open.

Restore/import must explicitly retain or fence old custody without adopting a
prior boot or synthesizing a receipt. Supported restore still disables sharing
and requires re-pairing. A restart with no genuine retained closure evidence
can leave a diagnostic stranded obligation. Interrupted restore can retain
admission capacity debt until actual old-owner teardown or fencing is proved;
re-pairing alone does not erase that debt. Availability must not silently
override that boundary.

## Consequences and acceptance

The design makes cleanup retryable after response loss and owner failure, and
keeps Source/B transport ownership consistent. It adds bounded durable state
and can require operator reconciliation after an ingress disappears without
closure evidence. It does not promise seamless adoption of a dead producer.

Implementation must cover current-owner routing, lost register and close
replies, owner-crash persistence, old-boot replay, quota reclamation, concurrent
retirement and an unreachable ingress refusing End. Actual HTTP/2 backpressure
must demonstrate that no End receipt precedes outer driver closure. Regression
definitions and test-target compilation occur during development; execution
is deferred to main promotion under Paul's explicit test policy.
