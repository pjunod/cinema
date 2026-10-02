# ADR-J0-1 — retain passive VOD routes after reader idle reap

**Status:** proposed; physical reproduction retained, recovery policy unproved · **Date:** 2026-10-02 EDT · **Deciders:** Jellyfin compatibility implementation and task review.

Companion to [the build contract](JELLYFIN-COMPATIBILITY-BUILD.md) (required
behavior) and [execution status](JELLYFIN-COMPATIBILITY-STATUS.md) (measured
results) — this answers which lifetime must survive a foreign client's long
pause. It does not qualify the release or mark J0 complete.

## Context — delivery works; durable resurrection currently fails

The physical Android TV client rendered native encoded fMP4 from an isolated
Plurx daemon. It paused for 343.445 seconds and sent 22 paused progress reports
to the reference server. The probe forwarded no synthetic native control and
no progress-to-reader touch. Native operator activity became empty and the
log confirmed the 300-second idle reap. Resume and a seek beyond the buffer
requested init and playlist bytes, but native HTTP returned 410
`media_session_ended`.

The [sanitized observation](jellyfin/androidtv-connection-observation.json)
retains timing, real client requests and mutation boundaries. At source
`9c5d6bc98`, `VodServe` removes an idle reader without a tombstone, but
`Transcode::renewable_session_ids` then omits its session. The owner loop's
stale-settlement path ends the durable route. Public resurrection requires an
active route. The isolated reader-transition regression bypasses that owner
integration and cannot prove this scenario.

The producer must still relinquish its admission and working set while idle.
Authentication, terminal release, replacement, drain and owner epochs must
continue to fence media publication. No foreign progress report proves a
rendered frame or authorizes prepared native actions.

## Decision — propose a bounded route grant separate from the reader

Add an internal, server-owned passive VOD lifetime policy beside the existing
per-create VOD-only policy. Its lightweight grant keeps the existing durable
route eligible for owner lease renewal after reader idle reap; it holds no
producer, rendition reader, encoder admission or resource reservation.

The facade's authenticated current-play presence may renew only this grant.
Successful native media delivery may renew it too. Neither operation touches
an idle reader or fabricates `ControlRequestV1`, rendered-frame evidence or
presentation health. The native reader retains its existing 300-second clock.
A subsequent real init/playlist/fragment request resurrects the same active
route through the existing publication and adoption fences.

Proposed bounds, to prove in the spike:

| Limit | Meaning |
|---|---|
| 600 seconds without valid presence or successful delivery | Expire the lightweight grant and let normal terminal cleanup run; no unlimited orphan route |
| One current activated delivery per stable player identity | Renegotiation retires the predecessor grant as well as the reader |
| 64 active grants per native user; 4,096 per daemon | Refuse new activation at admission; never evict another active grant to accept it |
| Existing owner lease/deadline and publication epoch | A grant does not override lease loss, ambiguous renewal or owner transition |

Only trusted compatibility service ingress selects this policy. The native
HTTP create body must not expose a client-controlled retention knob. Bind the
policy and bounds into native request identity, durable recipe and worker
start/relay envelopes. Presence must resolve the authenticated binding to the
exact current native incarnation/owner epoch, so delayed traffic cannot
renew a retired or superseded play.

Grant expiry, terminal Stop, replacement, drain and feature disable must use
the existing native release/publication fences. Token logout follows the
build contract's existing reader-grant semantics; it does not manufacture a
new route or bypass expiry. Missing owner state after restart or failover is
an honest renegotiation/refusal under the first release's pinned-owner scope,
not a promise of transparent failover. A cached recipe alone cannot revive a
terminal route.

## Options considered — preserve the resource and evidence boundaries

| Option | Complexity | Resource cost | Consequence |
|---|---|---|---|
| Keep the reader alive with synthetic control/progress touches | Low | Paused producer/admission can remain held | Rejected: conceals idle reaping and fabricates native evidence |
| Raise the reader TTL globally | Low | Extends every native client's paused working set | Rejected: changes native behavior and still has a later failure boundary |
| Recreate a new route automatically from a stale alias | Medium | May allocate after terminal or superseded traffic | Rejected: weakens terminal publication and player identity fences |
| Keep a bounded, opt-in lightweight route grant | Medium/high | Bounded metadata and ordinary owner renewals | Selected for the spike: resource reclamation and real-request resurrection remain distinct |

## Consequences — the owner integration must be proved

This adds a native service seam rather than an alternate playback ledger.
Existing native creation remains on its existing lifetime policy. The facade
continues to use the existing durable owner route and publication fencing.
Grant inventory, expiry and last real frontier must be available to the owner
renewal loop without requiring a live rendition. A renewal ambiguity fences
both grant and reader. New active-grant admission must be bounded before any
worker allocation, and worker abort must relinquish the grant.

The J0 estimate increases by a native owner-lifecycle spike and its race tests;
freeze the revised whole-effort estimate only after the Infuse and recovery
experiments finish. Do not advance J4 on a document-only disposition.

## Action items — proof before acceptance

1. Reproduce the failure in an integrated owner-loop/HTTP regression, with a
   controllable clock, rather than relying on a reader-only test.
2. Implement the smallest opt-in service spike and prove reader/admission
   reclamation while the exact durable route remains active.
3. Prove expiry without presence, current-play presence, delayed predecessor
   presence, terminal Stop, disable/drain, release races and renewal ambiguity.
4. Repeat the physical pause beyond 300 seconds and fetch outside the buffer;
   decode new bytes under the same native session, then verify terminal 410.
5. Carry and test the policy through worker start and relay envelopes, update
   the build contract, and retain the focused regression in the task PR.
