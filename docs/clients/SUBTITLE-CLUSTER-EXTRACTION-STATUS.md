# Subtitle cluster extraction — implementation status

**Status:** in-progress: M0 continuation and M1 · **Updated:** 2026-09-24 · **Board:** K-09 · **Branch:** `plan/K-09` · **PR:** [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) · **Base:** `f600d2823`

The [v2 plan](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) is the contract. Its
Execution log holds the evidence for each milestone. This page is the short
progress view for the one implementation PR.

| Milestone | State | Evidence / next step |
|---|---|---|
| M0 · nuc3 experiments E1–E5 | cue identity accepted; continuing | E1 passed. E1b's ASS `.mks` has matching cues and payloads but differs byte for byte from the direct burn sidecar. The 7.5 s case and E2–E5 are running. [Exact evidence](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md#61-results--2026-09-24-m0-stopped-at-e1b). |
| M1 · text ride-along and VTT consumer | in progress | Producer and consumer implementation started after accepting §6.1's cue-identity allowance. |
| M2 · schema v46 and queue lifecycle | pending | SQLite and replicated store contracts. |
| M3 · publications and hydration | pending | Portable source identity and merge-safe hydration. |
| M4 · worker, playback join and Developer switch | pending | Existing client pending budgets remain. |
| M5 · idle backfill and advisory controls | pending | Default off until rollout evidence. |

**Review and merge:** pending implementation. No adversarial implementation
review or fast lane has run. The plan's §6.2 fleet and device checks require a
later deployment and are not claimed by this implementation PR.

**Anchor correction for M3:** §8 was checked against `f600d2823` (all 21
anchors). `object_version` is host-local; the portable sampled digest is
`FragmentIndexSourceObservation.source_sha256`. The plan's §3.10 identity
contract needs that correction before implementation.
