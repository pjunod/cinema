# Development pipeline — fast effort branches, deliberate qualification

> **Workflow correction, 2026-09-10, amended 2026-09-13:** Main-bound pull
> requests use only
> [main-fast-lane.yml](../.github/workflows/main-fast-lane.yml): open as draft,
> obtain exactly one adversarial review, address it, then mark ready. Marking
> ready starts the lane; returning the PR to draft cancels it; draft PRs
> allocate no jobs. The `fast-lane` label is gone — it was an opt-in on top of
> the draft gate, and on 2026-09-13 three lanes merged without it, so the
> gate never ran for any of them and the third was carrying a fix for a
> regression the first had already shipped to the fleet. Draft alone still
> buys what the label was for: a PR opened early costs nothing until it is
> ready. Full CI runs only by manual dispatch or an explicit release tag.
> Effort compiler checks and evidence proofs are manual. Merge does not
> build or deploy an image. Runtime-test schedules are disabled; the weekly
> dependency audit remains. These rules supersede the older automatic
> qualification and post-merge instructions below pending their rewrite.

> **Test-lane amendment, 2026-10-07, superseding 2026-09-20:** The ready
> main-bound fast Rust job compiles every target and runs workspace and
> vendored Hiqlite Clippy on the pinned FFmpeg 6 runner surface. The full
> Rust unit and SQLite suites run in manually dispatched
> [ci.yml](../.github/workflows/ci.yml), outside the blocking PR lane.
> Run the smallest focused regression locally before pushing and record its
> command in the PR, with each `Regression-Test:` anchor on its own line.
> Preflight validates those anchors in the merge candidate; it does not
> execute commands from the PR body. Policy, affected compile checks, and
> named regression evidence remain prerequisites for merge. This keeps a
> focused change from waiting for the entire Rust suite; no periodic schedule
> was added.

> **Coverage amendment, 2026-09-30:** [coverage.yml](../.github/workflows/coverage.yml)
> refreshes the dated unit coverage badge on pushes to `main`, independently
> of full CI. It also accepts manual retries on `main`. Coverage runs serialize
> and measure current `main` when they start; failed runs retain the last
> successful badge and its UTC measurement date.


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
git commit                              # lint/syntax hook; no tests or effort compile
```

The [`effort-ci.yml`](../.github/workflows/effort-ci.yml) gate runs only:

| Surface | Blocking evidence | Deliberately deferred |
|---|---|---|
| Every task | History, validation-catalog, CI/deploy/shipping contracts | Release builds and container smoke |
| Rust | `rustfmt` plus `cargo check --workspace --locked --all-targets` | Unit, SQLite, cluster, daemon, and recovery tests |
| Web | Embedded JavaScript and theme static contracts | Browser layout and playback acceptance |
| Apple | iOS and tvOS compilation | XCTest and physical-device acceptance |
| Android | Debug application compilation | JVM tests, lint, emulator tests, and physical-device acceptance |

The aggregate result is named `Effort development gate`. Receipt-bearing
attempts serialize without automatic cancellation so successful Python units
cannot lose their evidence. [Once-per-PR Python receipts](ci/PYTHON-UNIT-PR-RECEIPTS.md)
retain the original test/run/source attribution; retries execute only failed
or newly discovered IDs. They do not replace current-source compilation,
history, catalog or static checks. Missing scope information enables all
compile surfaces; ambiguous PR receipt identity refuses unit execution.

**Compilation being blocking is a reason to compile locally, not a reason to
let the gate do it.** A session whose checkout has no toolchain can still have
one in about ten minutes — see
[AGENT-COMPILE-LOOP.md](ci/AGENT-COMPILE-LOOP.md) — and the local run reports
every error at once where the gate reports the first.

**Corrective changes still need a focused local proof.** Run the smallest test
that rejects the old behavior and name it in the task PR. CI intentionally does
not rediscover or expand that command in the effort lane; the history audit
keeps the regression mapping honest, and final qualification executes the
retained suite. This is a working convention rather than a GitHub-enforced
rule while the private repository has no branch-protection feature.

Run `make hooks` once after adopting this pipeline. The same hook runs on every
branch and stops after catalog lint, Rust formatting and Clippy, and embedded
JavaScript syntax. It does not replace the focused regression or compile checks
required before an effort push, and ordinary changes still need their affected
validation before a pull request.

## 4. Compile before CI: send source to the compiler, never credentials

The device VM that owns the private clone may not have a usable Rust toolchain.
The cloud container has `cargo`, `rustc`, `rustup`, registry access, and the
repository-pinned Rust 1.97.1 toolchain, but it does not need the repository's
history or credentials. A compiler needs source, not history. Move a source
archive across that boundary and keep the credential on the device.

This loop is working and reproducible in about ten minutes cold. It turned the
M7 M3 work from a blind, push-to-compile cycle into merged-quality evidence in
one sitting. The previous alternative cost about 15 minutes for the fast gate
or 30–40 minutes for the full fan-out on every attempt, and each attempt often
revealed only the next type error, test failure, or denied lint.

### Start the loop before editing

From the committed base in the device VM clone, create a source-only archive:

```bash
git archive --format=tar.gz \
  -o "$HOME/mnt/plurx-agent/_src.tgz" HEAD # source only; no .git or credential
git rev-parse HEAD                          # record the snapshot being tested
```

The archive is about 25 MB. Stage it through the existing device-to-cloud file
bridge, then extract it into a stable working directory in the cloud container.
Use that same directory across iterations so its warm `target/` survives.
`git archive` contains committed `HEAD`, not uncommitted device-VM edits; either
make the change in the extracted tree or commit the intended branch before
archiving it.

Confirm the compiler before treating any result as evidence:

```bash
rustup show active-toolchain                  # must resolve the repository pin
rustc --version                               # must report 1.97.1
cargo --version                               # do not assume the default is pinned
```

One observed container also had a default `cargo` from Rust 1.95 while rustup
already had 1.97.1 installed. The repository's
[`rust-toolchain.toml`](../rust-toolchain.toml) should select 1.97.1 after
extraction; check it rather than trusting the shell path. A version-specific
lint disagreement is exactly what this loop exists to catch.

For a `plurxd` change, run the package loop that proved the method:

```bash
cargo fmt --all
cargo check -p plurxd --all-targets
cargo clippy -p plurxd --all-targets -- -D warnings
cargo test -p plurxd --bin plurxd
```

Add the smallest focused regression for the changed behavior; these commands
are the broad local backstop, not a substitute for the test that rejects the
old behavior. Dependencies resolve from the registry, while the awkward
dependencies (`hiqlite` and `rust_decimal`) are vendored.

Measured on the proved session:

| Step | Cold | Warm |
|---|---:|---:|
| `git archive` plus staging | ~40 s | ~40 s |
| `cargo check -p plurxd --all-targets` | ~4 min | ~35 s |
| `cargo clippy -p plurxd --all-targets -- -D warnings` | ~3 min | ~3 min |
| `cargo test -p plurxd --bin plurxd` (1,504 tests in that run) | ~4 min | ~2 min |
| `cargo fmt --all` | seconds | seconds |

The exact test count will grow. A different count is not itself a failure;
read the command's exit status and confirm that the expected test target ran.

### Rebase the evidence onto the branch that will be pushed

Main or a shared effort branch can move while the cloud tree is warm. Preserve
the warm build, but do not preserve stale claims:

1. Archive the intended base and keep a pristine extraction of that same
   archive beside the cloud working tree.
2. Build the change in the working extraction and run the package loop plus the
   focused regression.
3. Diff only the changed files against the pristine extraction. Inspect the
   patch before moving it: `cargo fmt --all` can touch a file you did not intend
   to change if the archived tree was not already clean.
4. In the device clone, create the task branch from the current intended base
   (`main` or the current `effort/<project>`) and apply the inspected patch.
5. Archive that branch's `HEAD`, extract it over the cloud working tree, and run
   the checks again. Keeping `target/` makes this exact-tree pass warm rather
   than another cold build.

The second pass is mandatory. In the proving session, the first verification
used `c9c6d18`; the port landed on `1a64fa8`, whose tree contained 20 additional
tests. Only the post-port archive made “1,504 tests pass” a statement about the
branch rather than an older snapshot.

### Read failures as design feedback

This loop found, in one sitting:

- `RateLimited(150)` and then `RateLimited(50)`, because a storm test spaced
  seeks below the server's own 250 ms exchange floor;
- a `control_sequence` field read after `SessionRequest` had already been
  converted;
- all 16 struct literals missing the new field in one compiler run; and
- `dead_code` on the new constant and method but not the field, which showed
  that the design needed a consumer rather than another argument for keeping
  unused code.

These are the failures a source compiler answers in minutes and CI answers one
push at a time. Treat the 250 ms cadence as a useful behavioral finding: it
already bounds how many restarts one storm can request.

### Keep the cloud workspace bounded

An extraction plus warm `target/` occupies a few GB. The container has a fixed
per-session writable allowance, and `df` reports the backing volume rather than
that allowance, so healthy-looking `Avail` output does not prove another write
will succeed. Keep the active extraction and warm target; remove older
extractions instead of accumulating them.

This loop accelerates development evidence. It does not replace the task PR's
required gate, the focused regression recorded in the PR, or the final
exact-tree qualification for an effort.

## 5. Qualify once — freeze, sync, fan out, record

When the project is complete:

1. **Freeze task merges.** Qualification proves one tree. Any later task makes
   the receipt stale and starts a new candidate.
2. **Merge current `main` into the effort.** Do not rebase the shared effort
   branch: rewriting every task commit creates avoidable review and recovery
   work.
3. **Open `effort/<project>` into `main`, then dispatch qualification manually.**
   Full CI is manual or release-tag-only; opening the PR does not allocate its
   runtime sweep. Dispatch [ci.yml](../.github/workflows/ci.yml) on the frozen
   promotion head branch with `promotion_pr`, `promotion_head_sha` and
   `promotion_base_sha` naming that open same-repository PR and both exact tips.
   Authenticated API metadata must agree with the dispatched checkout, and
   current `main` must already be an ancestor of the head. The bound run enables
   every surface, including Windows. It never publishes a package or fleet image;
   leaving the three inputs empty preserves the ordinary manual workflow.
   If resolving the final merge requires a separate branch, name it
   `integration/<project>-into-main`; the workflow treats that narrowly named
   branch as the same qualification candidate.
   Forgejo does not auto-cancel these promotion runs when a newer event arrives;
   it retains the older result for diagnosis. Compile-only effort PRs, ordinary
   main-bound PRs, and superseded `main` pushes still cancel obsolete work, and
   immutable tags do not. This is an explicit event-context expression because
   workflow concurrency is evaluated before the scope job exists. The behavior
   follows Forgejo's
   [workflow concurrency contract](https://forgejo.org/docs/latest/user/actions/reference/#concurrency).
4. **Require `Main promotion gate` to pass.** For a qualification, skipped is
   not success: Rust, cluster, browser, Apple, Android, cross-build, and
   container jobs must all report `success`.
5. **Read the qualification record.** The workflow retains the PR head, base,
   tested merge SHA, exact Git tree, workflow ref, job results, and run identity
   for 14 days. Cross-built binaries and their SHA-256 sidecars remain
   available for one day.
6. **Merge only while the candidate is current.** Immediately before writing
   the receipt, the gate fetches the live head and base refs and requires both
   tips to equal the pull-request event or authenticated manual binding. The
   manual receipt also revalidates the PR's open state and same-repository route.
   A moved effort head or `main` base
   leaves the completed run available for diagnosis but prevents a success
   receipt. Merge current `main` into the effort and qualify the new tree.

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

The freeze is still an operator convention: do not merge another task into the
effort or merge a different pull request into `main` while qualification is
running. The workflow refuses stale evidence; it cannot reserve either branch
or implement a merge queue.

## 6. A failed qualification returns to development

A red qualification does not turn every task back into a release candidate.
Reopen the effort, fix the named failure with its focused command, and use the
compile-only task lane again. When the tree settles, run one new complete
qualification. Do not use a green job from an older candidate as proof for the
new tree; [CI_TEST_OVERHAUL_PLAN.md](ci/CI_TEST_OVERHAUL_PLAN.md) defines the
content-fingerprint work required before selective result reuse can be trusted.
The transport-recovery campaign is stricter: its successful lane receipt and
the aggregate qualification receipt accept only workflow run attempt `1`.
Failure receipts remain available for diagnosis, but **Re-run jobs** cannot
turn that workflow execution's failed 40-cycle campaign into promotion
evidence. Fix the cause and create a new candidate commit before qualifying
again. This is a same-run guarantee, not a repository-wide ledger: closing and
reopening a pull request creates a different workflow run whose attempt number
starts at `1`, so operators must still inspect earlier receipts for the same
candidate SHA.

Main pushes still run the exhaustive workflow in the background. A newer main
push cancels an older main run because the newer tree contains it; tags and
merge-group evidence are never cancelled. Post-merge validation may find a
defect, but it is no longer a wait point for starting the next task.

## 7. Guardrails — what the fast lane does not claim

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

## 8. Acceptance — prove the scheduler before using it

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
