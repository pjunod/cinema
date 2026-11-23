# Cluster facade boundary — private mechanisms across two crates

**Status:** built — adopted boundary, implemented and independently reviewed.
**Owner:** Codex. **Date:** 2026-11-17.

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811). Earlier branch
names and checkpoint receipts below are historical. Fleet acceptance remains
after Paul’s deployment.

Companion to the repository's cluster facade build contract. This decision accompanies [the final-stage plan](CLUSTER-FACADE-SLICE-7-BUILD.md),
committed before source edits. Its preparatory decision and checkpoint
evidence remain historical; use the promotion record for qualification and
landing.

## Context

The daemon facade lives in `plurxd`, while MembershipManager lives in
`plurx-core::cluster::membership`. The intended final state removes raw
AppState handles, reaches an honest zero daemon bypass census, and makes
the manager's role/roster methods module-private. Rust cannot grant private
access to a selected module in a different crate. Simply changing these
methods to pub(super) breaks the facade and external startup/cluster-check
consumers. Renaming public methods would not create the intended boundary.

Source exploration found existing reusable domain interfaces: the applied
RosterDirectory/RosterSnapshot, ClusterJobAuthority, applied standing and
heartbeat admission projections, clock proof/leadership/guard types, and
claim-bound revocation leases. Their consistency and freshness differ; a
bridge must preserve those differences, including errors and unknowns.

## Proposed decision

Introduce an explicitly public, private-handle core ClusterAccess aggregate
and purpose-specific children. Its construction remains at selected-backend
or startup composition. It exposes no raw manager getter, Deref, generic
callback into the manager, or arbitrary SQL escape. The daemon facade owns
this aggregate and gives features narrower daemon capabilities.

The bridge supplies coherent domain boundaries rather than public renamed
manager methods:

- A local observation identifies deployment mode, applied standing, heartbeat
  admission and maintenance sources explicitly. Do not claim these separate
  sources are one atomic or linearizable snapshot. Preserve actual effective
  member/voter behavior, including unavailable and standalone distinctions.
- A directory factory reuses the stamped roster and purpose projections,
  preserving ordering, limits, route learner eligibility and time-dependent
  reachability. Existing uncapped/non-Raft hostname/origin rules require
  source verification before reuse; use a bounded purpose service when the
  directory does not load the necessary evidence.
- An owned job-authority interface preserves the existing fresh learner and
  singleton decisions. Active-voter authority remains a separate fresh
  decision, not a cached voter bit.
- A clock protocol child keeps guard identity, leadership, applied changes,
  proved roster, local hint and final authenticated-request roster together.
- A revocation exclusion child keeps acquisition, apply acknowledgement,
  claim-bound roster and exact release ownership together.

After all consumers migrate, the selected raw role/roster methods become
pub(super) without changing their internal algorithms. Core cluster
descendants may still call them. External startup fixtures use the clock
protocol; cluster-check uses the appropriate directory/origin contracts.
Compile those actual consumers instead of treating a source search as proof.

The daemon removes raw AppState fields and closes bootstrap, shutdown,
backup, settings-change and job composition through owned capabilities.
Census classification distinguishes exact proven non-raw receivers/DTOs
from handles. Unknown receivers remain counted; retain and strengthen
negative alias, clone, destructure, shorthand, macro and new-file controls.
Keep explicit role/roster detection when raw methods cease to be public.
No file exemption or global method-name exception is added to claim zero.

## Alternatives and trade-offs

| Option | Ownership/privacy | Cost and consequence |
|---|---|---|
| Public high-level core bridge, selected here | Raw methods become private; daemon owns the bridge and its source fence guards its use. Rust still cannot restrict the public bridge to one foreign module. | Reuses existing observations/protocols; requires explicit external consumer migration and core/daemon/cluster-check compile coverage. |
| Keep raw methods public and enforce only daemon source rules | Can remove current bypasses but leaves the intended raw-method privacy end state unmet. | Smaller immediate diff; relies more heavily on a heuristic source guard. |
| Move the entire daemon facade/fence/lifecycle into core | Could place mechanisms under one crate visibility boundary. | Much wider architecture and ownership changes, including server-specific admission/lifecycle, risking C2/C7 and unrelated owners. |

Public renamed raw equivalents, raw-manager getters and blanket census
exclusions are rejected because they do not strengthen the boundary.

## Delivery and acceptance

Split the residual work into reviewable sequential ownership groups: incoming
auth/admission; job/backup/applied-change capabilities; bootstrap/core bridge;
then final AppState privacy and semantic census zero. Cluster.rs has one
integration owner. Preserve current C7 behavior after its actual landing and
keep B3/C1/source-reader mechanisms with their existing owners.

Each group has plan-first source changes, a pinned compiler loop, focused
behavioral regressions, one adversarial complete-diff review and exact-head
gate before effort landing. Final evidence must show empty measured pins,
zero ceiling, compiler-enforced AppState privacy, strengthened census
controls, actual external API compile coverage, fresh main integration and
main-promotion qualification. No deployment or fleet acceptance is inferred.
