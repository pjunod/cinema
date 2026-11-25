# The requested lane — how a pull request lands on `main`

**Status:** built (`.github/workflows/main-fast-lane.yml`, `validation/lane_request.py`; the requesting side is `ci-fleet lane` in [noirr/ci-fleet](http://forge.lan:3000/noirr/ci-fleet)) · **Updated:** 2026-11-25 · **Owner:** Claude session for the batch-lane change

Any agent — Claude, GPT, OpenRouter — or person can follow this page. It
replaces the merge train, which is retired.

## Why

Every push to a ready PR used to start the whole lane. Measured over the
merge-train period, the heavy jobs ran about 4.3 times per PR: 36% of runs
were cancelled by the next push and 44% failed. The requested lane splits the
lane in two: pushes get a cheap gate, and the heavy lane runs once, for the
head you mean to land, when you ask for it.

## How a session lands

1. **Push freely.** Every push to a ready PR (opened, synchronize, reopened,
   ready for review) runs the **cheap gate**: `paperwork`, scope, the fast Rust
   fmt/Clippy gate, web syntax and type checks, and the Android compile. A draft
   runs `paperwork` only. The cheap gate has no `Main promotion gate`.
2. **When the head is ready, request the lane once:**
   `ci-fleet lane request --pr N --head SHA` (or add the label `lane` to the
   PR by hand). That label event starts one **full run** for that head:
   preflight, the worker controller proof, the Rust unit fan-out, the Apple
   compile and native TV journeys, the Node unit suite, every cheap job again,
   and the `Main promotion gate`.
3. **Wait for it:** `ci-fleet lane wait --pr N --head SHA`.
4. **Land it:** `scripts/land <pr> --head <sha>`, never the raw merge API. The
   script requires a green `Main promotion gate` on that exact head; one green
   requested run is the receipt, even if a later cheap or label event on the
   same head posted a newer `skipped` gate. It lands with the description (its
   `Regression-Test:` lines) and `Reviewed-on:` in the landing commit and reads
   the landing commit back.

**After a failure**, fix it, push, and request again on the new head. To rerun
the same head (a flake, a runner outage), request again on that head: ci-fleet
removes the `lane` label and adds it back. Receipts make the rerun
incremental: Python, web, native and Rust unit passes already recorded for the
same inputs are adopted, not repeated
([PYTHON-UNIT-PR-RECEIPTS.md](PYTHON-UNIT-PR-RECEIPTS.md),
[RUST-UNIT-FANOUT.md](RUST-UNIT-FANOUT.md)).

**Never add or remove labels in a loop.** Each label change is a run.

## What the lane does with each event

| Event | Runs |
|---|---|
| push / open / reopen / ready for review | the cheap gate |
| draft, or converted to draft | `paperwork` only |
| label change, PR ready, labels include `lane` | the full run and the promotion gate |
| label change, labels lack `lane` | nothing (every job is skipped, `paperwork` included) |

`validation/lane_request.py` reads the event payload in the scope job and
writes `lane_requested`; every heavy job and the promotion gate require it to
be `true`. Forgejo (17.0.1, Gitea 1.22-compatible) sends every pull request
label change as payload action `label_updated` (`label_cleared` when the last
label goes), matched by `types: [labeled]`, and its payload has no `label`
field — only `pull_request.labels`. The module accepts `labeled` too, for a
GitHub-shaped payload. The receipt modules bind a prior run only when it was a
`pull_request` run of `main-fast-lane.yml` for that PR and head; a label event
is one, so its passes are adoptable like a push's
(`validation/main_preflight_adoption.py` `PR_EVENT_ACTIONS`).

## Batches

ci-fleet may answer several `lane request`s with one run. Its bot account
(`ci-fleet-lane`) builds a branch `lane-batch/b-<id>` from `main` with each
member head merged in (`git merge --no-ff`, so every member head is an
ancestor of the batch head), opens it as a pull request into `main`, and adds
`lane`: the batch is an ordinary requested run, and the receipt modules bind
it like any other pull request's. When that run's `Main promotion gate` is
green, ci-fleet posts on each member head a status
`main fast lane / Main promotion gate (batch)` whose description reads
`ci-fleet batch b-<id>: run <run id> of #<batch pr> at <batch head> green`
(optionally `· with #<n> …` and `· interaction`), then closes the batch pull
request unmerged.

`scripts/land` counts that status only after tracing it
(`validation/landing.py` `verify_batch`): the description parses exactly;
`ci-fleet-lane` posted it; the run is a `pull_request` run of
`main-fast-lane.yml` for the batch pull request on the batch head and
concluded `success`, with exactly one `Main promotion gate` job that
succeeded; the batch pull request is from `lane-batch/b-<id>` into `main` at
the batch head (closed is fine); and `refs/pull/<batch pr>/head`, fetched,
is the batch head and has the member head as an ancestor. A batch status that
fails any check is ignored, neither a pass nor a failure, and land prints one
line saying why. A solo green gate on the head still passes as before.

Sessions do nothing different: `ci-fleet lane request` and `ci-fleet lane
wait` as before, then `scripts/land`. A member's verdict may come from a
batch.

## Known costs

- **A push cancels an in-flight requested run.** The concurrency group is one
  run per PR, `cancel-in-progress`. That is correct: the run's head is stale,
  and its gate would refuse the moved head anyway. Request again on the new
  head.
- **Any other label change while `lane` is present re-triggers the run.**
  Forgejo cannot say which label changed, so adding or removing an unrelated
  label is a label event that still carries `lane`: it cancels the running
  requested run and starts another. Receipts make that rerun incremental, but
  it is still a rerun; leave other labels alone while a requested run is going.
- **Removing `lane` starts nothing.** When other labels remain, the removal is
  a label event in the same concurrency group and cancels an in-flight
  requested run. When `lane` was the only label, Forgejo sends
  `label_cleared`, which matches no trigger and cancels nothing, so
  `ci-fleet lane leave` also cancels the requested run through the API.

## Manual source gate

An authenticated `Manual-Source-Gate: <run id>` line in the PR description
(an actual completed `ci / Main promotion gate` for this head,
[RUST-UNIT-FANOUT.md](RUST-UNIT-FANOUT.md)) switches the duplicated jobs and
the request off; `scripts/land --source-gate-run` verifies that run instead
(`validation/manual_source.py`).
