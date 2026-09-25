# Subtitle cluster extraction — implementation status

**Status:** in-progress: M0–M5 authored; adversarial review next · **Updated:** 2026-09-24 · **Board:** K-09 · **Branch:** `plan/K-09` · **PR:** [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) · **Base:** `f600d2823`

The [v2 plan](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) is the contract. Its
Execution log holds the evidence for each milestone. This page is the short
progress view for the one implementation PR.

| Milestone | State | Evidence / next step |
|---|---|---|
| M0 · nuc3 experiments E1–E5 | complete | E1–E3 passed under the documented E1b cue-identity choice; E4 cost and E5 failure behavior are recorded in [§6.1](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md#61-results--2026-09-24-m0-cue-identity-decision). E5 requires M1 to reject a decode-error truncation even when cue and framecrc counts agree. |
| M1 · text ride-along and VTT consumer | `53b8b6fe2`; fast lane pending | Typed text probe, ASS double map, E5 decoder-error veto, VTT and styled burn store consumers, named tests. |
| M2 · schema v46 and queue lifecycle | `53b8b6fe2`; fast lane pending | SQLite and Hiqlite migrations, deterministic foreground join/repair, publication rows and named store cases. |
| M3 · publications and hydration | `53b8b6fe2`; fast lane pending | Merge-safe local publish, portable sampled digest and local inode binding, authenticated bounded peer route, replicated node A → B lookup scenario, named tests. |
| M4 · worker, playback join and Developer switch | `53b8b6fe2`; fast lane pending | No-index worker, foreground self-claim, bounded VTT and burn flights, typed text/PGS progress, named tests. |
| M5 · idle backfill and advisory controls | `53b8b6fe2`; fast lane pending | Eligible-file store query, exclusive idle discovery, telemetry, manual switches and advisory Developer probes, named tests. |

**Review and merge:** UI baseline regenerated from 78 captures without page or
console errors. Pinned Rust compile, formatting and Clippy checks pass for
`plurxd`, `plurx-core` and `plurx-cluster-check`; JavaScript syntax passes.
No adversarial implementation review or fast lane has run. The plan's §6.2
fleet and device checks require a
later deployment and are not claimed by this implementation PR.

**Anchor correction for M3:** §8 was checked against `f600d2823` (all 21
anchors). `object_version` is host-local; the portable sampled digest is
`FragmentIndexSourceObservation.source_sha256`. The plan's §3.10 identity
contract now records that correction.
