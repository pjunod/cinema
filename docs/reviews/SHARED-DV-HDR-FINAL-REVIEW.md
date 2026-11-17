# Shared DV/HDR — final review and owner disposition

**Status:** one P1 corrected and compiled; runtime qualification pending.
**Owner:** GPT/Codex · **Promotion:** [#1732](http://forge.lan:3000/noirr/plurx/pulls/1732)
**Reviewed candidate:** `efe5400d4faf5dec7a4a8fbf63e93b1a4991e1fa`
**Main base:** `32fa105eed4194d85f3cbc97794b6864fb99d2ad`

The single independent adversarial pass requested changes for one blocker.
M1 was previously reviewed; this pass covered the integrated M2/M3/M4 work,
FF8 colour assertion, current-main interactions and client fixture corrections.
The independent review ran no tests or builds. Its finding and acceptance
limits are retained below; the owner disposition is separate from its verdict.

## P1 — Activity and Stop must reach learner and maintenance owners

The Operations roster includes committed learners, but the outer capacity
policy rejected Activity and processed Stop before their signed handlers.
Work on those owners could not be discovered or remotely stopped; maintenance
also closed the control path precisely while existing work was draining.

**Owner disposition:** corrected in the same change as this record. Both
`learner_route_eligible` and `maintenance_route_eligible` admit exact GET
`/_internal/v1/activity-snapshot` and exact POST
`/_internal/v1/activity-source-processed-stop`. Member authentication and
signed responses remain in their handlers. Other methods, suffixes and new
work routes retain the existing closed policy.

Coverage is deliberately split:

- `http/tests/activity_routes.rs::committed_learner_and_maintenance_owner_admit_signed_activity_and_stop`
  joins a real learner and uses the complete router before/after durable
  maintenance. It checks signed snapshot (200) and signed Stop (404), unsigned requests (401),
  and rejected wrong-method/new-work requests. The404 proves routing and
  acknowledgment, not an active renderer's settlement.
- `transcode/tests/source_actor.rs::admin_stop_of_a_source_processed_session_settles_and_ends_the_start`
  separately owns the actual Source operation, child and reader barrier and
  checks that Stop waits for physical settlement.

Both tests are authored and their targets compile; execution results belong
to final qualification.
No second adversarial pass or independent approval of later edits is claimed.

## Independent review scope and remaining acceptance

No other actionable code blocker was established in the inspected Source
operation/root custody, principal scoping, receiver successor/client rendezvous,
Activity/Stop authorization, grade/profile/init projection, boot facts,
cluster facade and bounded-capability fixture changes.

Genuine FEL backend/root-reuse isolation, pending-verdict backend behavior,
measured successor lead, D1's 20 device/output switches with frame/audio
readings, deployment and real shared-title proof remain open. Synthetic FF8
metadata and the lifecycle renderer do not establish those outcomes. The
canonical acceptance ledger is the
[shared DV/HDR plan](../streaming/SHARED-LIBRARY-DV-HDR-PLAN.md) and
[processed sharing plan](../streaming/SHARED-DV-PROCESSED-PLAN.md).

## Baseline census correction after CI

The main-bound runtime census exposed an existing mismatch in main: HTTP
metrics referenced Hiqlite five times while its pin allowed three. The two
writer-reply observation references now live behind the cluster facade,
which returns its own counter rows. The metrics substate snapshots them per
request without store or membership I/O; labels and output stay unchanged.
The pin and census ceiling remain unchanged.

The original reviewer inspected only this adapter delta and found no concrete
blocker. This was a targeted closure in the final review, not a second review
of the effort. The existing vendor row-count assertion is preserved.
Exact-head compilation and runtime qualification remain required.

The counter-adapter check exposed rustc E0275 proof-depth exhaustion through
the existing Source actor/runtime Send/Sync container graph, without a
non-Send value. The unchanged parent passed the same check. The original
reviewer recommended one bounded compiler limit of 256, preserving ownership,
and exact-candidate all-target Clippy; a further overflow requires investigation
rather than another automatic increase. No runtime stack or ownership rule
changes with this compiler setting.

## Focused runtime-repair closure

A focused review of the runtime repair delta found no additional concrete code
blocker. This closes the new edits without repeating the whole-effort review.
The Activity allocator charges actual serialized ancillary bytes rather than
reserving maximum space for absent families. It preserves the 256 KiB ceiling,
existing family bounds and Stop identities, reserves a fitting delivery before
analysis, then fills the remaining wire budget. The saturated regression keeps
maximum JSON-escaped titles and checks all families together.

Fixture and structural-inventory repairs retain production admission and privacy
checks. The Source offer case still requires overlapping ordinary and affirmed
Starts; failure-stage diagnostics do not waive its unresolved 503. These repairs
have not yet compiled or passed their failed-only runtime checks. Main promotion,
deployment and real-title/device acceptance remain outstanding.

## Corrected predecessor integration

The effort integrates corrected candidate809be33c (tree8fa71cbb) by normal
merge. Its upstream source reviews and formal repair remain separate evidence.
The integration resolves the metrics projection overlap by retaining one
cluster-owned fixed-label projection, without a redundant Cluster method or
MetricsState counter copy. Both the exact rendered-row and all11-source-row
assertions remain.

The unchanged ownership scanner and module roots measure the union of both
parents:1176 task shapes,104 method spawns,2271 timers,361 Command constructors
and951 launch-shaped methods. Each equals the sum of the parent counts minus
their common base. This is static ownership inventory, not process execution.

The actual main source retains both uniquely mapped startup/worker anchors.
The source-pin rebind preserves both parents' scoped provenance and explicitly
excludes timed runtime/Store shutdown and physical cleanup from the finite
models' guarantees. Models/configuration bytes are unchanged. Independent targeted merge-delta review approved source6561385de8 without
an additional blocker. Static compilation and affected validation remain
pending; no new model execution or runtime qualification is claimed by this
integration.
