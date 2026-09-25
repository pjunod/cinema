# Subtitle cluster extraction — implementation status

**Status:** built — M0–M5 implementation complete; PR #507 merge qualification in progress; fleet and device evidence pending · **Updated:** 2026-09-24 · **Board:** K-09 · **Branch:** `plan/K-09` · **PR:** [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) · **Base:** `dafadf043`

The [v2 plan](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) is the contract. Its
Execution log holds the evidence for each milestone. This page is the short
progress view for the one implementation PR.

| Milestone | State | Evidence / next step |
|---|---|---|
| M0 · nuc3 experiments E1–E5 | complete | E1–E3 passed under the documented E1b cue-identity choice; E4 cost and E5 failure behavior are recorded in [§6.1](SUBTITLE-CLUSTER-EXTRACTION-PLAN.md#61-results--2026-09-24-m0-cue-identity-decision). E5 requires M1 to reject a decode-error truncation even when cue and framecrc counts agree. |
| M1 · text ride-along and VTT consumer | `53b8b6fe2` + `b18173eb5` + `fd7356074`; PR gate | Typed text probe, ASS double map, E5 decoder-error veto, VTT and styled burn store consumers, named tests. |
| M2 · schema v46 and queue lifecycle | `53b8b6fe2` + `b18173eb5` + `fd7356074`; PR gate | SQLite and Hiqlite migrations, deterministic foreground join/repair, publication rows and named store cases. |
| M3 · publications and hydration | `53b8b6fe2` + `b18173eb5` + `fd7356074`; PR gate | Merge-safe local publish, portable sampled digest and local inode binding, authenticated bounded peer route, replicated node A → B lookup scenario, named tests. |
| M4 · worker, playback join and Developer switch | `53b8b6fe2` + `b18173eb5` + `fd7356074`; PR gate | No-index worker, foreground self-claim, bounded VTT and burn flights, typed text/PGS progress, named tests. |
| M5 · idle backfill and advisory controls | `53b8b6fe2` + `b18173eb5` + `fd7356074`; PR gate | Eligible-file store query, exclusive idle discovery, telemetry, manual switches and advisory Developer probes, named tests. |

**Review and merge:** The sole adversarial review by
`agent:/root/adversarial_review` requested four P1 and two P2 fixes: the
HLS and overlay peer paths, the active-job wait bound, partial-ready repair,
cross-node ride skip and cancel-at-publish fencing. All six are fixed in
`b18173eb5`; repair epochs now survive terminal-request pruning. The first
local `make unit` run found SQL contract and fixture errors, fixed in
`fd7356074`; their focused reruns pass. Unchanged scratch-upload and telemetry
tests were red on the macOS run, so the pinned Linux/FFmpeg 6 PR gate is the
merge qualification. Pinned Rust compile, formatting and Clippy pass on the
reviewed code, and the UI baseline produced 78 captures without page
or console errors. The plan's §6.2 fleet and device checks require a later
deployment and are not claimed by this implementation PR.

**Anchor correction for M3:** §8 was checked against `f600d2823` (all 21
anchors) and rechecked after merging `dafadf043`; its ten changed paths do not
touch an anchor source. `object_version` is host-local; the portable sampled digest is
`FragmentIndexSourceObservation.source_sha256`. The plan's §3.10 identity
contract now records that correction.
