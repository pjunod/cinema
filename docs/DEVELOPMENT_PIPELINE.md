# Development pipeline — fast effort branches, deliberate qualification

**Status:** accepted and implemented · **Decider:** Paul · **Written:**
2026-08-29

Companion to [VALIDATION.md](VALIDATION.md) (how changed paths select evidence)
and [RELEASING.md](RELEASING.md) (how a qualified main commit becomes a
release) — this is how a large project moves without putting the full test
suite in every task's critical path. Work milestone by milestone on one
`effort/<project>` branch. If a shortcut would let an effort reach `main`
without one complete qualification run, stop: fast development is the choice;
untested promotion is not.

## 1. Objective — stop treating every task as a release candidate

The ordinary main-bound workflow is designed to decide whether one pull
request can ship. A large project asks a different question while its pieces
are still moving: can the next piece integrate without breaking compilation?
Running cluster campaigns, simulators, emulators, browsers, containers, and
cross builds after every answer turns a 20-minute gate into hours of serial
waiting.

The effort train separates those questions:

```text
 task branches
      │ pull requests
      ▼
 effort/<project> ──▶ Effort development gate
      │                 policy · compile · static web
      │                 no release suites
      │
      │ final pull request to main
      ▼
 frozen candidate ──▶ Main promotion gate
      │                 every CI surface · exact-tree receipt
      ▼
     main ───────────▶ existing release/tag process
```

## 2. Branch roles — one temporary integration line

| Branch | Receives | Gate | Lifetime |
|---|---|---|---|
| `codex/<task>` or another task branch | One reviewable milestone | Local focused evidence, then the effort gate on its PR | Delete after merge |
| `effort/<project>` | Task PRs for one large project | Compile-only development gate | Delete after the project reaches `main` |
| `main` | Ordinary PRs and fully qualified efforts | Affected-surface gate for ordinary PRs; full fan-out for `effort/**` | Permanent |

Staging is an environment, not a branch. A second permanent integration branch
would accumulate its own merge debt and eventually become another mainline.
The effort branch supplies source isolation; qualification records the exact
candidate tree.

## 3. Develop the effort — compilation is blocking, release evidence is not

Create the integration branch from current `main`, then aim every project task
PR at it:

```bash
git switch main                         # start from the shipped integration line
git pull --ff-only                      # do not build the effort on stale main
git switch -c effort/<project>          # one temporary branch for the project
git push -u origin effort/<project>     # make it available as a PR base

gh pr create --base effort/<project>    # task branches target the effort
PLURX_EFFORT_COMMIT=1 git commit        # compile-only pre-commit mode
```

The [`effort-ci.yml`](../.github/workflows/effort-ci.yml) gate runs only:

| Surface | Blocking evidence | Deliberately deferred |
|---|---|---|
| Every task | History, validation-catalog, CI/deploy/shipping contracts | Release builds and container smoke |
| Rust | `rustfmt` plus `cargo check --workspace --locked --all-targets` | Unit, SQLite, cluster, daemon, and recovery tests |
| Web | Embedded JavaScript and theme static contracts | Browser layout and playback acceptance |
| Apple | iOS and tvOS compilation | XCTest and physical-device acceptance |
| Android | Debug application compilation | JVM tests, lint, emulator tests, and physical-device acceptance |

The aggregate result is named `Effort development gate`. Every new task push
cancels its superseded run because only the latest task tree can merge. Missing
scope information fails open into all compile surfaces.

**Compilation being blocking is a reason to compile locally, not a reason to
let the gate do it.** A session whose checkout has no toolchain can still have
one in about ten minutes — see
[AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md) — and the local run reports
every error at once where the gate reports the first.

**Corrective changes still need a focused local proof.** Run the smallest test
that rejects the old behavior and name it in the task PR. CI intentionally does
not rediscover or expand that command in the effort lane; the history audit
keeps the regression mapping honest, and final qualification executes the
retained suite. This is a working convention rather than a GitHub-enforced
rule while the private repository has no branch-protection feature.

Run `make hooks` once after adopting this pipeline so the installed hook has
the effort mode. The environment flag is intentionally explicit: ordinary
changes still use the full commit profile, while effort tasks replace that
local suite with policy, formatting, operations contracts, and compile-only
Rust evidence. Do not set it for a change that will target `main` directly.

## 4. Qualify once — freeze, sync, fan out, record

When the project is complete:

1. **Freeze task merges.** Qualification proves one tree. Any later task makes
   the receipt stale and starts a new candidate.
2. **Merge current `main` into the effort.** Do not rebase the shared effort
   branch: rewriting every task commit creates avoidable review and recovery
   work.
3. **Open `effort/<project>` into `main`.** The main workflow recognizes that
   head/base pair and replaces normal path selection with the complete fan-out.
4. **Require `Main promotion gate` to pass.** For a qualification, skipped is
   not success: Rust, cluster, browser, Apple, Android, cross-build, and
   container jobs must all report `success`.
5. **Read the qualification record.** The workflow retains the PR head, base,
   tested merge SHA, exact Git tree, workflow ref, job results, and run identity
   for 14 days. Cross-built binaries and their SHA-256 sidecars remain
   available for one day.
6. **Merge only while the candidate is current.** If `main` moved after the
   run, merge it into the effort and qualify again. Otherwise merge the PR and
   let the ordinary main/tag process continue.

```bash
git fetch origin main
git switch effort/<project>
git merge --no-ff origin/main              # preserve the shared effort history
git push
gh pr create --base main --head effort/<project>
gh pr checks <number> --watch            # wait for Main promotion gate
gh run download <run-id> \
  -n effort-qualification-<number>       # inspect the exact-tree record
jq . qualification-receipt.json         # every recorded job must be success
gh pr merge <number> --merge             # merge only the still-current candidate
```

The receipt is evidence, not access control. GitHub does not currently expose
branch protection or rulesets for this private repository's account plan. Paul
therefore owns the final invariant: no effort PR merges while its promotion
gate is pending, red, or stale. If repository protections become available,
configure `Main promotion gate` on `main` and `Effort development gate` on the
`effort/**` branch pattern; the conditional jobs remain implementation details
behind those two aggregates.

## 5. A failed qualification returns to development

A red qualification does not turn every task back into a release candidate.
Reopen the effort, fix the named failure with its focused command, and use the
compile-only task lane again. When the tree settles, run one new complete
qualification. Do not use a green job from an older candidate as proof for the
new tree; [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) defines the
content-fingerprint work required before selective result reuse can be trusted.

Main pushes still run the exhaustive workflow in the background. A newer main
push cancels an older main run because the newer tree contains it; tags and
merge-group evidence are never cancelled. Post-merge validation may find a
defect, but it is no longer a wait point for starting the next task.

## 6. Guardrails — what the fast lane does not claim

- **It does not make an effort branch releasable.** A compile result says the
  pieces fit syntactically; it says nothing about playback, recovery, clients,
  containers, or replicated durability.
- **It does not replace focused development tests.** The agent changing a
  behavior still runs the regression it is writing or preserving.
- **It does not create a permanent staging branch.** Staging will consume a
  qualified artifact when deployment automation owns that environment.
- **It does not reuse stale evidence.** Every qualification currently runs all
  surfaces. Reuse lands only after the fingerprint plan proves zero stale
  matches.
- **It does not expose self-hosted runners to public code.** The repository
  remains private; runner trust boundaries are unchanged.

## 7. Acceptance — prove the scheduler before using it

```bash
python3 -m unittest tests.validation.test_qualification \
  tests.validation.test_runner              # scope + exact-tree receipt
python3 -m unittest tests.operations.test_contracts \
                                             # workflow and runner contracts
make effort-rust-check                       # compile without executing Rust tests
git diff --check                             # no malformed patch or documentation
```

Observable GitHub acceptance:

- a PR into `effort/<project>` reports `Effort development gate` and no slow
  release-suite jobs;
- the same effort opened into `main` runs every main-workflow surface;
- a green qualification uploads `effort-qualification-<pr>` and both release
  binary artifacts with SHA-256 sidecars;
- an ordinary task PR into `main` retains affected-surface selection.
