# Async driver and Store refinement — parked

**Status:** open — parked 2026-11-02 by Paul; awaits a reduced-model redesign.
**Owner:** unassigned (last: Claude, session `01BCWL9xgAD2CNRAHWxAaDWU`).
**Work preserved:** annotated tag `archive/async-store-refinement-2026-11-02`
(head `1e05f136e`), branch `codex/async-store-refinement`, and a restore-tested
git bundle attached to [PR #1263](http://forge.lan:3000/noirr/plurx/pulls/1263).

## What it was

An exhaustive refinement proof connecting the production Rust library-worker
driver to actual Store SQL outcomes for two workers, over seven contexts and
eleven TLC outcome families. Because the state space exceeded one job, it ran
as a resumable "paged" session on a dedicated runner: units of at most
1800 s, persistent catalog checkpoints, native artifact transport, and a Main
promotion gate that authenticates the completed session.

The product fix that travelled with it — library workers run through one
shared driver, so ready work beats pressure ticks while queue and domain loss
keep priority — landed on its own, without the proof, in the pull request
that added this page.

## Why it is parked

An offline census on 2026-11-02 ran the same exploration as the proof (the
paged `expand_paged_predecessor` without catalog writes, in-memory oracle,
Store cap lifted; the first 717 expansions matched the disk-catalog path
exactly, and the total was within 0.7% of run 6115 at its 296,431-expansion
cut). It did not converge in about two hours:

| Contexts | Reached (each) | Growth per BFS level | Extrapolated total (each) |
|---|---|---|---|
| stable, weak_admission, no_claim_win | ≥ 4.97M Products, 3.65M Expansions | about ×1.35, falling slowly | about 3e7–7e8 Products |
| base, withheld_join, old_poll_policy, drop_before_join | ≥ 4.12M Products at level 12 | about ×2.6 | about 1e8 to above 1e12 |

Measured on the proof runner, a paged unit costs about 290 s fixed plus
0.40 s per expanded predecessor in the unoptimized test build the proof uses
(an `opt-level = 2` build was 3–4× faster per predecessor and about 4.6×
slower to compile cold). Exhaustive qualification would therefore need
thousands of units and months of runner time, beyond any immutable
`maximum_units` a session can admit. The size is a property of the model, so
it will not become runnable later without changing the model.

## State at parking

- **Works against real jobs on the dedicated proof runner, or a faithful offline replay of them:** source
  registration bound to the PR merge base, the capability probe trio,
  admission, and the first unit (driver graph, Init, expansion, checkpoint
  export and packaging). Defects found on the way were fixed at the root with
  regression tests: Forgejo 16 run/artifact API shapes, runner identity, grant
  plan identity and lease naming, measurement freshness, unit-0 admission,
  bytecode in the source extraction, the writer-epoch schema type, three
  failing store tests, and an fsync-bound export scratch database.
- **Fixed, not re-run:** the two second-unit (resumed expand) defects.
- **Never exercised:** semantic verification, SCC/monitor checks,
  per-component TLC, backend qualification, the three-batch transport
  conformance case, the completion gate, and landing.

## Restarting it

1. Reduce the model before anything else: merge contexts with identical
   behaviour (each class above produced identical level sequences) and
   abstract the unbounded values (monotonic clocks, wall/monotonic anchors,
   Store growth) so the space closes. Measure the reduced space offline first.
2. Restore the work: `git fetch` the tag or the bundle from PR #1263 into a
   plurx clone, then integrate current Main.
3. Size the session plan from the measured space, then follow the
   qualification runbook preserved on the branch.

Findings with the full numbers: the project note
`claude/pr1263-qualification-scale-finding-2026-11-02.md`.
