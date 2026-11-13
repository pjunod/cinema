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

`make hooks` runs `make paperwork` on every push. After the trusted publisher has pushed a
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

### 1a. A job retry can erase the run's artifacts

The inspected Forgejo 17 job-rerun implementation shares the run reset in
`services/actions/rerun.go`: retrying one job deletes **all artifacts of that
workflow run**, including artifacts from successful sibling jobs. Selecting a
failed setup job does not preserve previously published test evidence.

`scripts/ci-failures --retry-job JOB_ID` therefore checks the selected job's
whole run and its paginated artifact inventory before requesting a retry. It
requires terminal run metadata and no unexpired artifacts, including ordinary
build data. Missing or inconsistent identities, unknown expiry, failed API
reads and incomplete pagination refuse the retry. `--dry-run` performs the
same checks and sends no retry request.

```sh
scripts/ci-failures --retry-job JOB_ID --dry-run  # inspect artifact safety before a retry
```

When it refuses, start a fresh lane and let authenticated adoption reuse
finished evidence. Keep the artifact-bearing run intact. This is a client
preflight, not an atomic server-side retention guarantee: another actor can
change the run or publish an artifact between the inventory read and the rerun
request. Requiring a terminal run avoids retrying while its jobs can still
publish; it cannot serialize other clients against Forgejo's deletion.

### 2. Derivable paperwork fixes itself

The PR `paperwork` jobs run `scripts/paperwork` and stop on failure before
compiler jobs. They carry no persistent publisher credential and do not commit
or push PR-controlled output. The separate ci-fleet publisher uses pinned policy
in a credential-free sandbox, validates the live same-repository PR identity,
and leases a canonical fix commit to the original head. Its dedicated bot push
starts the replacement main-bound lane. Installation and a real replacement-run
canary remain operational acceptance; effort PRs use the local fixer meanwhile.
[PAPERWORK.md](PAPERWORK.md) describes the boundary and supported edits.

The description and title are read from the event payload, not an environment
variable: a long description used to stop the runner from starting the step
(`argument list too long`).

Paperwork failures, 62 job logs of the 65 failed `paperwork` jobs (a run can
fail several rules):

| Rule | Runs | PRs | Now |
|---|---|---|---|
| mobile release counters (mostly: main claimed the same number first) | 23 | 5 | trusted publisher/local fixer |
| `Regression-Test:` line missing for a `fix(`/`perf(` commit | 19 | 12 | author; message names the commits and the command |
| real host names in the tree | 9 | 3 | trusted publisher/local fixer in Markdown; code stays author-owned |
| file without a functionality point | 8 | 3 | author |
| doc over 40 KB | 4 | 3 | author |
| doc without an index row | 4 | 1 | trusted publisher/local fixer (row from title and Status header) |
| step never started: description too long for an env var | 3 | 3 | fixed (read from the event file) |

Preflight failures, 117 job logs of the 127 failed preflight jobs:

| Cause | Runs | PRs | Now |
|---|---|---|---|
| superseded run refused by the head guard | 33 | 28 | gone: cancelled instead (§1) |
| `history-check`: a landing already on `main` lacks `Regression-Test:` lines or lost an anchor | 31 | 15 | gone: reported, not failed (§2a) |
| rolling-producer ownership inventory count moved | 17 | 15 | author (reviewed allowlist count) |
| `validation-lint` exited 1 with nothing in its log | 9 | 9 | not diagnosed |
| package/toolchain download 5xx or timeout | 6 | 3 | infrastructure |
| formal smoke: TLC timed out | 5 | 5 | infrastructure |
| other unit/contract tests | 14 | 13 | author |
| a re-run job attempt refused by receipts | 2 | 2 | by design: dispatch fresh runs |

### 2a. `history-check` fails a PR only for its own gaps

31 failures across 15 PRs came from two or three landings on `main` that
lacked their lines (run 7556: `db1c581e` landed in `90a1bf97`, #1542); every
PR opened after them was red and nothing its author could do fixed it. The
fast lane's preflight now sets `PLURX_HISTORY_MERGE_TARGET=origin/<base>`
(`scripts/history-audit --merge-target REF` does the same locally). An error
then fails the PR only when it is about a commit not reachable from the base,
or about a file the PR changes against the merge base (the merge ledger, a
`regressions.d/` fragment, `tests/client-fixes.toml` or an anchored source or
test), or about nothing more specific. Everything else is printed under
`history gaps already on origin/main -- reported, NOT failing this change`
and written to `target/validation/history.json` as `inherited`; it needs a
follow-up on `main` (an errata row, or the evidence).

The full failing audit is unchanged wherever no merge target is given: `make
history-check` and `make check` locally, `ci.yml` (release tags and manual
dispatch, which is where `main` itself is audited) and `effort-ci.yml`. No
workflow audits `main` on every push, so the PR lane's report is where a new
gap on `main` first shows up.

### Proposed, not done here

- **Description edits do not start the lane** (`edited` is not a trigger, by
  design: status edits would restart it). After adding a `Regression-Test:`
  line, push any commit (`git commit --allow-empty -m 'chore: rerun lane'`).

### 3. The Rust gate

Pinned image, cgroup-sized parallelism and a single type-check; it stays on
the nonvoter compile pool because the idle high-CPU runners do not apply the
job's CPU, PID and swap limits. Details and timings:
[RUST-GATE-IMAGE.md](RUST-GATE-IMAGE.md).

## Native build-counter boundary

The Apple project’s generated numeric `CURRENT_PROJECT_VERSION` may advance
without changing its reviewed input closure. The boundary validates the exact
reviewed project shape with only that positive integer normalized. Full source
fingerprints and case-input comparisons still include the actual Git blob;
no pass transfers between different build numbers. Added resources, scripts,
paths or other project edits still require a new independent boundary review.
This avoids stale-pin failures after ordinary mobile counter bumps without
weakening receipt provenance.
