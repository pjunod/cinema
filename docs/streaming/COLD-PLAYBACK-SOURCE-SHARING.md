# Cold playback source sharing — path policies and domain-wide decisions

**Status:** proposed · **Written:** 2026-11-02 EST ·
**Owns:** M1 steady sharing in the cold-start proposal, not implemented policy.

Companion to [the canonical RCA/proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md)
and [the supplied review/dispositions](COLD-PLAYBACK-STARTUP-FABLE-REVIEW.md).
This expands M1's sharing contract so a rolling-only watermark rule cannot
silently govern long direct/progressive bodies. The common ledger, immediate
foreground overlap, pause/cleanup ownership and integrity rules remain in
§5 of the canonical proposal.

Rolling owners use validated contiguous producer lead relative to accepted
film demand. Initial qualification candidates are `low = required steady
reserve`, `high = low + two publication targets`. Preserve the steady reserve.
Above high, offer a sharing opportunity; below low, prioritize refill;
between thresholds retain the previous state. Pace banked copy production
through the actor's owned hold/budget mechanism, not an assumed runtime edit
of FFmpeg's fixed readrate. Exclude deliberate holds from unconstrained
capacity evidence.

**Nonrolling bodies have a separate service policy.** Progressive and direct
ranges transition from first-source-read priority to `transfer_service` after
the first bounded input read/mux fragment; no first-presentation or film-lead
signal is assumed. Use finite byte-service grants against cold source supply
and foreground source-rate floors, with a reserved background service quantum.
Qualification candidates use 5 s service epochs and at least 1 MiB of background
progress per epoch when supply exceeds the foreground floor plus that quantum.
Unknown rate floors use a bounded weighted byte-service class (initial 2:1
foreground/background weight), not permanent startup priority; its media
continuity target remains unqualified until measured. Coordinate grants in
the shared ledger at epoch/owner transitions, never per packet. A grant
records its finite epoch, byte allowance and minimum foreground service;
compute it against all current owners on every required domain. If a measured
cold-supply floor is available, protect the sum of foreground input floors
plus the background quantum. Otherwise the weighted service class remains
explicitly unqualified for media continuity until its runtime cells pass.

Slow consumers/socket backpressure become source-idle only after outstanding
reads and prefetch settle; body/descriptor custody remains owned. No source
occupancy is charged for an idle connection merely holding materialized bytes.
Before a new read, atomically register active/refill demand and signal background
yield; start through the immediate-overlap rule. Completed/cancelled ranges
settle only their exact custody. Seeks bind a new source position/generation;
range retries do not reset the viewer clock or renew an unbounded startup
exemption. Clients declared to lack film telemetry use the transfer service class,
including legacy rolling delivery. Temporarily missing telemetry from a
telemetry-capable owner retains refill/lease-expiry behavior; it is not
silently downgraded to a different class.

**Domain admission is a conjunction, not one viewer's permission.** A background
resume requires every relevant current foreground owner to permit sharing:
no startup/refill owner; every rolling owner is in sharing state with fresh lead above low;
every transfer owner has a current service grant protecting its floor; idle
owners have confirmed source quiescence. Apply this across all resource keys,
mixed paths and nodes. Stale high lead never overrides another owner; missing
rolling observations mean refill, while missing transfer telemetry uses its
bounded service class. An atomic resume transaction revalidates the complete
owner/generation/observation set and available occupancy before granting the
background slot. New demand or a low transition revokes the opportunity and
signals pause; a racing foreground start uses accounted overlap rather than
waiting for reap.

Preserve separate waiters on shared preparation and all overlapping readers.
Measure service opportunities, hash/scanner progress and pause age. Sufficient
supply cells must demonstrate continuing preparation during long rolling,
progressive and direct-range sessions; absent film telemetry is not a waiver.
Test high-lead plus starting/refilling owners, remote and stale observations,
slow/idle bodies, changing ranges, and mixed path types. Physically insufficient
capacity cannot guarantee both uninterrupted playback and background progress;
show that condition and retain the user's existing capacity policy. Keep
`fragment_worker_idle` CPU/GPU-only; source service belongs to this arbiter.

## Acceptance — exercise conflicting owners and real progress

The rolling sharing state begins at high, remains between watermarks, and
ends at low; `share_allowed` requires that state plus a fresh lead above low.
Before background admission/resume, atomically check every current generation
and every required domain, including observation versions/expiry and slot
capacity. A high owner alone never grants service for a low/startup owner.

Required behavioral and runtime cases:

- A high-lead viewer plus a starting/refilling viewer, local and remote;
  background stays paused until both allow service.
- A stale high observation, missing telemetry, new generation and teardown;
  none grants service for a successor or silently removes an active owner.
- Rolling plus progressive/direct-range owners, including unknown rate floors;
  use the conjunction of their different service conditions.
- Long progressive and long direct-range transfers with sufficient supply;
  background checkpoint/scan progress must occur while transfers continue.
- Slow consumers, idle connections, resumed reads, finite/changed ranges and
  seek/retry storms; preserve read custody and the original user clock.
- Adequate versus insufficient supply; report foreground service, background
  quanta, early stalls, hold time and owner/epoch transitions separately.

These are build/qualification requirements. A proposed 5 s epoch or 2:1 weight
is not an accepted production default, and a quick refusal is not permitted
as the way to satisfy them. M4 must retain any failed class as unqualified.
