# Shared DV/HDR — final review and owner disposition

**Status:** one P1 corrected; compilation and runtime qualification pending.
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

Both tests are authored; execution results belong to final qualification.
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
