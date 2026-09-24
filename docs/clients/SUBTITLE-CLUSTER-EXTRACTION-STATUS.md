# Subtitle cluster extraction — implementation status

**Status:** claimed · **Updated:** 2026-09-24 · **Board:** K-09 · **Branch:** `plan/K-09` · **Base:** `f600d2823`

The [v2 plan](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) is the contract. Its
Execution log holds the evidence for each milestone. This page is the short
progress view for the one implementation PR.

| Milestone | State | Evidence / next step |
|---|---|---|
| M0 · nuc3 experiments E1–E5 | pending | Run FFmpeg 8.0.1 synthetic-source experiments and record results in plan §6.1. E1/E1b failure is a STOP. |
| M1 · text ride-along and VTT consumer | pending | Begins only after M0 passes. |
| M2 · schema v46 and queue lifecycle | pending | SQLite and replicated store contracts. |
| M3 · publications and hydration | pending | Portable source identity and merge-safe hydration. |
| M4 · worker, playback join and Developer switch | pending | Existing client pending budgets remain. |
| M5 · idle backfill and advisory controls | pending | Default off until rollout evidence. |

**Review and merge:** pending. The plan's §6.2 fleet and device checks require a
later deployment and are not claimed by this implementation PR.
