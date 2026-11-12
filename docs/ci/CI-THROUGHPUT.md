# Fast-lane throughput: superseded runs, self-fixing paperwork, the Rust gate

**Status:** built · Added 2026-11-12 (PR #1550) · Companion to
[PAPERWORK.md](PAPERWORK.md) and [RUST-GATE-IMAGE.md](RUST-GATE-IMAGE.md)

Plain terms for any agent (Claude, GPT, OpenRouter) or person working a pull
request into `main`.

## Before you push

```sh
make paperwork          # the bookkeeping rules CI enforces, ~10-60 s, with the fix under each failure
make paperwork-fix      # applies the fixes that are derivable (below) to your working tree; review, commit
scripts/paperwork --body-file <description.md>   # also checks your PR description's Regression-Test: lines
```

`make hooks` runs `make paperwork` on every push. After the lane has pushed a
`chore(paperwork)` commit to your branch (below), `git pull --rebase` before
your next push.

## What changed, and why

Forgejo, 2026-11-02..12: 359 fast-lane PR runs, 34% success, 53% failure,
13% cancelled; the median PR needed 3 lane runs, p90 9. Most failures were not
about code.

### 1. A new push cancels the PR's in-flight run

The lane's concurrency group was per PR with `cancel-in-progress: false`, so a
push queued behind the obsolete run, and the obsolete run then failed its own
head guard anyway (33 of the 117 analysed preflight failures). It is now
`cancel-in-progress: true`. What a cancelled run had already proved:

| Job | After a cancel |
|---|---|
| paperwork, scope | rerun (about a minute) |
| preflight Python/Node units | successes from earlier *finished* runs are adopted as before; successes the cancelled run earned are not (it publishes no final journal) and run again |
| fast Rust gate | reruns; the runner-local Cargo cache it had filled stays warm |
| Apple, Android, web | rerun; native receipts only adopt other PRs' merged sources, so a superseded head never reused them |
| worker controller proof | reruns, resuming from the evidence of earlier finished runs only |

The receipt rules treat a cancelled preflight as a terminal job, so a cancel
never wedges a PR ([PYTHON-UNIT-PR-RECEIPTS.md](PYTHON-UNIT-PR-RECEIPTS.md)).

### 2. Derivable paperwork fixes itself

The `paperwork` job (fast lane, and `paperwork.yml` for PRs into effort
branches) runs `scripts/paperwork`. When it fails it runs
`scripts/paperwork-fix --ci`, which applies only fixes whose correct output is
derivable, commits them as `chore(paperwork): apply derivable bookkeeping
fixes` with a `Paperwork-Fix:` trailer, re-runs paperwork, and pushes the
commit to the PR branch. [PAPERWORK.md](PAPERWORK.md) lists what is fixed and
the guards. The PR description and title are read from the event payload, not
an environment variable: a long description used to stop the runner from
starting the step (`argument list too long`).

Paperwork failures, 62 job logs of the 65 failed `paperwork` jobs (a run can
fail several rules):

| Rule | Runs | PRs | Now |
|---|---|---|---|
| mobile release counters (mostly: main claimed the same number first) | 23 | 5 | fixed by the lane |
| `Regression-Test:` line missing for a `fix(`/`perf(` commit | 19 | 12 | author; message names the commits and the command |
| real host names in the tree | 9 | 3 | fixed by the lane in Markdown; code is left to the author |
| file without a functionality point | 8 | 3 | author |
| doc over 40 KB | 4 | 3 | author |
| doc without an index row | 4 | 1 | fixed by the lane (row from title and Status header) |
| step never started: description too long for an env var | 3 | 3 | fixed (read from the event file) |

Preflight failures, 117 job logs of the 127 failed preflight jobs:

| Cause | Runs | PRs | Now |
|---|---|---|---|
| superseded run refused by the head guard | 33 | 28 | gone: cancelled instead (§1) |
| `history-check`: a landing already on `main` lacks `Regression-Test:` lines or lost an anchor | 31 | 15 | proposed change below |
| rolling-producer ownership inventory count moved | 17 | 15 | author (reviewed allowlist count) |
| `validation-lint` exited 1 with nothing in its log | 9 | 9 | not diagnosed |
| package/toolchain download 5xx or timeout | 6 | 3 | infrastructure |
| formal smoke: TLC timed out | 5 | 5 | infrastructure |
| other unit/contract tests | 14 | 13 | author |
| a re-run job attempt refused by receipts | 2 | 2 | by design: dispatch fresh runs |

### Proposed, not done here

- **`history-check` judges `main`, not the PR.** 31 failures across 15 PRs
  came from two or three landings on `main` that lacked their lines; every PR
  opened after them was red until an errata row landed, and nothing the PR
  author did could fix it. Proposal: in the PR lane, report history problems
  that predate the PR's merge base as a warning and fail only on the PR's own
  commits; keep the full audit on `main` pushes, where a breakage belongs to
  whoever landed it.
- **Description edits do not start the lane** (`edited` is not a trigger, by
  design: status edits would restart it). After adding a `Regression-Test:`
  line, push any commit (`git commit --allow-empty -m 'chore: rerun lane'`).

### 3. The Rust gate

Pinned image, cgroup-sized parallelism and a single type-check; it stays on
the nonvoter compile pool because the idle high-CPU runners do not apply the
job's CPU, PID and swap limits. Details and timings:
[RUST-GATE-IMAGE.md](RUST-GATE-IMAGE.md).
