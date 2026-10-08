# Rust test execution policy — where the suite runs, and what red means

**Status:** built — implementation merged; historical evidence limitations
accepted, current non-ignored failure repair remains open · **Executes:** §2.2 / §4.8 / F-build-1 /
F-hist-7 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Implemented:** 2026-09-21 against `main` @
`882862e8`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the
current ruling), [VALIDATION.md](../VALIDATION.md)
(how paths select evidence) and
[CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) (the earlier lane
design). Read review §2.2, §4.8 and §7.1, then assessment correction 10 and
rows `2.2`, `F-build-ops-codehealth-1`, `F-hist-7`, then this document. §3
presents the options with their measured and estimated cost; **§7 records
Paul's decisions.** The 2026-09-20 option (a) implementation and its original
receipts remain historical below. Paul's 2026-10-07 amendment in `a3162e446`
supersedes its lane placement: ready Rust compiles all targets and runs
workspace/vendor Clippy. Full `ci.yml` qualification remains manual or
release-tag triggered; configured coverage independently executes workspace
units on `main` pushes. No recurring runtime schedule was added. M1's missing Forgejo
comparison is an accepted infrastructure deviation; its actually executed
source-only fallback remains relative sizing evidence. M2's unrecovered
throwaway-unit negative is an accepted historical gap, not an executed pass
(§7.1). This does not close the current non-ignored failure obligation.

The standing instruction: **if a step seems to require changing
`make unit`'s definition, the `plurx-cluster-check` exclusion, the cluster
lanes' features, or the draft-gate semantics of `main-fast-lane.yml`, stop
and flag it.** This plan changes *when* the existing lane runs, not what it
is.

**Historical correction to the review (2026-09-20):** two additions, one narrowing.
(1) `ci.yml`'s `publish_main` job (`ci.yml:1452-1473`) runs only when
`github.event_name == 'push' && github.ref == 'refs/heads/main'`. Since
`3cd127e2` (2026-09-10) `ci.yml` has no `push: branches` trigger, so the
`sha-<12hex>` image publication that OPERATIONS.md §"The fleet registry"
calls automatic has had no automatic trigger for ten days; only a manual
dispatch on `main` produces one. Option (b) below must widen that condition
to `schedule` if the schedule is to restore image publication, and
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
depends on this. (2) The policy is pinned by text contracts:
`tests/operations/test_evidence_workflows.py:14-22` asserts `ci.yml` has no
`schedule:` and no `branches: [main]` trigger, and
`tests/operations/test_contracts.py:1692-1745` pins the fast lane's Rust
step to `make effort-rust-check` + `make hiqlite-vendor-clippy`. Either
option changes those tests in the same PR; that is the policy change made
visible, not a workaround. (3) The tracked pre-commit hook runs catalog lint,
Rust formatting/Clippy and embedded JavaScript syntax, **not tests**;
AGENTS.md's focused-regression duty is separate contributor evidence.
The historical trigger finding is not the current policy: §2.1 and §7.1
record the later lane amendment and configured coverage producer.

---

## 1. Objective

1. Paul decides, with measured cost in front of him, whether `make unit`
   and workspace Clippy join the main fast lane, whether `ci.yml` runs on a
   schedule against `main`, or both.
2. Whatever he decides, the following are true afterwards: the
   `lint.yml` comment tells the truth; a focused `plurx-core` command
   cannot silently compile out the replicated-store tests again; the two
   red Node tests are green or quarantined for a stated reason; and a
   known-red inventory exists that is empty or lists only `#[ignore =
   "reason"]` tests.

Done means: the decision is recorded in §7 with a date, this plan PR is
merged under it, and M1–M6 evidence is recorded in the Execution log.

**2026-10-08 disposition:** the source implementation is built, not a claim
of whole-plan Done. §7.1 distinguishes historical evidence and accepted gaps
from the current non-ignored failure queue; an empty catalog cannot make a
failing test green.

---

## 2. Contract today

Re-verify at build time.

### 2.1 What runs where

| Trigger | Workflow | Rust content |
|---|---|---|
| PR to `main`, ready (not draft) | `main-fast-lane.yml` `rust_compile` | pinned Rust 1.97.1 / Ubuntu 24.04 / FFmpeg 6; `make effort-rust-check`; `make hiqlite-vendor-clippy`; `make lint`; no full Rust unit execution |
| `v*` tag, `workflow_dispatch` | `ci.yml` | `check`: `make ci-rust-gate` plus workspace Rust/SQLite tests and serial restart fixtures, on `ffmpeg-6` runners; cluster lanes; `publish_main` only on manual `main` dispatch without promotion inputs |
| `v*` tag, dispatch | `lint.yml:3-6` | `make fmt-check lint` (workspace Clippy), badge |
| weekly cron | `rust-audit.yml` | advisories only |
| commit (local) | `scripts/pre-commit` → `precommit-check` (`Makefile:88-90`) | `validation-lint fmt-check lint` + `js-check` — Clippy, no tests |
| push to `main`, manual retry | `coverage.yml` | configured coverage measures the source it checks out; a failed run retains the last successful badge/date, not a green current suite |

Re-verified at `8e242787c` on 2026-10-08: the ready Rust job has a 60-minute
ceiling and `CARGO_BUILD_JOBS=1`. These are compile/Clippy bounds, not test
execution or fresh cold/warm timings. Python/Node preflight is a separate
receipt-producing lane; applicable per-PR successes are retained.

`make unit` (`Makefile:44-46`) is `spike-lock-check test-socket-permission-check`
then `cargo test --workspace --exclude plurx-cluster-check --no-fail-fast`
then `vodencode-restart-check` (two `#[ignore]`d real-FFmpeg restart
fixtures, serial). `make test-full` (`:50-54`) adds
`plurx-core/hiqlite-contract-tests,plurxd/cluster-integration-tests`.

### 2.2 The lint comment

`lint.yml` now says that its badge workflow is release-tag/manual and that
ready main pull requests run workspace Clippy in `main-fast-lane.yml` after
the one adversarial review. The implementation makes that statement true.

### 2.3 The ruling

The historical opening ruling in DEVELOPMENT_PIPELINE.md:

> **Workflow correction, 2026-09-10, amended 2026-09-13:** Main-bound pull
> requests use only main-fast-lane.yml: open as draft, obtain exactly one
> adversarial review, address it, then mark ready. Marking ready starts the
> lane; returning the PR to draft cancels it; draft PRs allocate no jobs.
> […] Full CI runs only by manual dispatch or an explicit release tag.
> Effort compiler checks and evidence proofs are manual. Merge does not
> build or deploy an image. Runtime-test schedules are disabled; the weekly
> dependency audit remains.

**Historical decision context:** the review's §7.1 adds that Paul's 09-17 instruction
made the fast lane "the one place tests run", which conflicts with the
09-10 text that makes it compile-only — either reading leaves no automatic
Rust test execution, and the 09-10 record's assumption that "a batch
process picks up full-suite failures" has no producer (review §1 item 1,
§2.2).

**Current ruling:** Paul's 2026-10-07 [test-lane amendment](../DEVELOPMENT_PIPELINE.md)
in `a3162e446094238ce2e17bafdebf96a68ef5a3f3` keeps full Rust/SQLite units
outside the ready lane. Named focused regression evidence and affected
compile/Clippy checks remain merge prerequisites. Preflight resolves
`Regression-Test:` anchors in the merge candidate; it does not execute
arbitrary commands from a PR body. Keep applicable per-PR positives; retry
only failed, new or actually invalidated checks. For this architecture
continuation, the coordinator hands reviewed, merge-ready work to the
batching owner; this reconciliation neither
authorizes a worker to merge `main` nor creates a new runtime schedule.

### 2.4 Measured timings

| Step | Cold | Warm |
|---|---:|---:|
| `cargo check -p plurxd --all-targets` | ~4 min | ~35 s |
| `cargo clippy -p plurxd --all-targets -- -D warnings` | ~3 min | ~3 min |
| `cargo test -p plurxd --bin plurxd` (1,504 tests) | ~4 min | ~2 min |

These are `-p plurxd` numbers on the cloud loop, not the `high-cpu` runner
and not the workspace.

#### P-01 measurement, 2026-09-21

The intended cold/warm/warm plus media1 experiment could not be completed as
named Forgejo runs. Temporary measurement workflows were committed to this
branch and later removed. Forgejo dispatches
[2382](http://forge.lan:3000/noirr/plurx/actions/runs/2382),
[2384](http://forge.lan:3000/noirr/plurx/actions/runs/2384),
[2386](http://forge.lan:3000/noirr/plurx/actions/runs/2386), and
[2388](http://forge.lan:3000/noirr/plurx/actions/runs/2388) did not reach a
measurement job. The registered fast-lane dispatch
[2392](http://forge.lan:3000/noirr/plurx/actions/runs/2392) did reach
`gha-media1-general-03`, then failed before measurement because that generic
runner had FFmpeg 8 while the lane requires FFmpeg 6. No media1 runner was
online. These URLs are failure evidence, not timing evidence.

With no further dispatch retries, one source-only archive of branch
`401d465f` was measured in an Ubuntu 24.04 container on `media1` (16 CPUs),
then the container and archive were removed. The environment reported Rust
1.97.1 and FFmpeg 6.1.1. This is relative sizing evidence only: it was not a
Forgejo job and no warm or media1 result is claimed.

| Environment | Compile + vendored Clippy | Workspace Clippy | `make unit` | Result |
|---|---:|---:|---:|---|
| `media1`, Ubuntu 24.04 container, source-only cold tree | 155 s | 85 s | ~610 s | green: 2,426 passed, 8 ignored, 0 failed; both serial restart fixtures passed |

The measured Rust work totals about 14 minutes 10 seconds before checkout,
package setup and cache finalization. The implemented 30-minute job timeout
therefore retains substantial bounded headroom. The fast Rust job now pins
Ubuntu 24.04 as its FFmpeg 6 environment while selecting the online generic
high-CPU pool; this corrects the drift exposed by run 2392.

After merging current `main` at `882862e8`, one bounded workspace confirmation
finished in 336 seconds rather than stalling. It exited 2 with 2,417 passed,
4 failed and 9 ignored in the `plurxd` target; the remaining workspace targets
and doc tests passed. The four failures were current-main fixture
reconciliations: publication observation ownership, elapsed startup runway,
the logical retention boundary, and a wall-clock-sensitive post-fetch progress
assertion. Their four exact focused tests passed together after repair. Per the
single-confirmation resource bound, the broad suite was not run a second time;
the source-only container result above remains the timing evidence.

### 2.5 The hiqlite-store blind spot

`e314a5bd` (2026-09-14): `cargo test -p plurx-core --lib` does not enable
`hiqlite-store` (`crates/plurx-core/Cargo.toml:17`), so 282 tests behind
`#[cfg(feature = "hiqlite-store")]` compiled out of a run that was reported
green; three DVR replicated-store defects shipped. `make unit` builds the
workspace, and `plurxd` depends on `plurx-core` with `features =
["hiqlite-store"]` (`crates/plurxd/Cargo.toml:32`), so feature unification
turns the feature on there; the blind spot is the **focused per-crate**
command form, which is exactly the form AGENTS.md asks contributors to run.

### 2.6 The two Node regressions

- `web-policy.test.js` no longer counts Kotlin source matches. The existing
  `BehindLiveWindowRecoveryTest` proves the finite-budget re-arm behaviour,
  while the Node inventory keeps only ownership and call-shape assertions.
- `web-control.test.js` passed unchanged under Node 22.22.2 and 26.8.1 on the
  repaired current tree. The reported runtime split did not reproduce, so
  neither shipped ordering logic nor the 400 ms bound was changed. Both files
  now run in the fast-lane preflight under the pinned Node 22 runtime.

### 2.7 Historical ignored-test receipt

The 2026-09-21 receipt recorded eleven `#[ignore = "…"]` in `crates/`, every one attached to a test and
carrying a reason (600 MB store probe, 620 MiB EPUB proof, two-hour audio
benchmark, the two serial FFmpeg restart fixtures, MiniLM download opt-in,
nightly runner capability, private ATSC capture, macOS VideoToolbox, and
contract-factory spawns). `validation.known_red` lexes Rust source, follows
the module graph and `include!`, and gives each ignore an exact
`source.rs::module::function` identity. Bare, empty, commented, detached,
duplicate, unreachable and ambiguous ignores fail closed; Cargo-list
matching is exact rather than a bare-name suffix match.

That eleven is historical, not a newly measured current inventory.
`validation/known-red.toml` has an empty debt catalog and separate explicit
opt-in fixtures. Neither fact proves that non-ignored current tests pass;
§3.6 still treats a non-ignored red result as a build break, not an entry.

---

## 3. Change

### 3.1 The three options, with cost

These are the original 2026-09-20 decision inputs, not the current ready-lane
instruction. Paul's later amendment and its consequences are in §7.1.

```
                     red merge visible after…    who is told      extra cost per PR
 (a) unit in lane    before merge (blocks)       the PR author    +T_unit + T_clippy
 (b) schedule 4 h    ≤ 4 h + run (~30–40 min)    nobody, unless   0 per PR; 6 runs/day
                                                 M2 adds a target  on the lab runners
 (c) both            before merge; schedule      author + target  as (a) + 6 runs/day
                     catches env drift
```

**(a) `make unit` + workspace Clippy in the fast Rust gate.** Add to
`rust_compile` after `make effort-rust-check`: `make lint` and `make unit`.
Cost estimate, from §2.4: `cargo check` does not pay test codegen or
linking, so "already compiled" is only partly true (assessment 10). Test
codegen+link for the workspace is roughly the `-p plurxd` test number
scaled by the other crates' test volume; `plurx-core`'s store contracts are
the large unknown. Estimate **+4–8 min warm, +10–15 min cold** on
`high-cpu`, plus **~3 min** for workspace Clippy (it does not share
`check`'s artefacts — the `-p plurxd` warm number is the same as cold).
The `rust_compile` job's `timeout-minutes: 10` becomes 30. Exposure: none;
a red test blocks the merge. The ffmpeg question: `make unit` runs the two
restart fixtures against whatever ffmpeg the `high-cpu` runner has;
`ci.yml`'s `check` pins `ffmpeg-6` by runner label and
[SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md)
§3.4 moves that to 8; the fast lane must take the same `./.github/actions/ffmpeg`
step with the same major or the two lanes disagree.

**(b) Scheduled `ci.yml` on `main` every 4 h.** `on.schedule: cron: '0 */4
* * *'`; a job condition on every cluster lane (`cluster_store_legacy`,
`cluster_store_shards`, `cluster_store_shadow`, `cluster_store`,
`cluster_topology`, `cluster_transport_recovery_*`, `cluster_wal`,
`cluster_daemon`, `android_device`, `apple`, `vod_web`) of
`github.event_name != 'schedule'` so the heavy lanes stay out; the
concurrency expression (`ci.yml:14-33`) gains `|| github.event_name ==
'schedule'` in `cancel-in-progress` so a new schedule event supersedes a
running one instead of queueing six behind a stuck runner; a `group` that
includes the event name so a schedule run does not cancel a tag run.
`publish_main`'s condition widens to `(push && main) || schedule` **only if
Paul wants scheduled runs to publish `sha-` images** — a green scheduled
run is exactly the "green run" the release doc's weekly tag wants. Cost: 0
per PR; six `check` + `web_layout` + `android_jvm` runs a day on the lab
runners (~30–40 min each). Exposure: a red merge is visible only at the
next run, and the fleet is deployed by hand from `origin/main`
(OPERATIONS.md §"The fleet registry", CLIENT-DEPLOY-PROMPT §1), so a deploy
in that window ships a red tree. A red run also needs somewhere to go:
without a notification target the "batch fixer" still has no input —
`scripts/publish-badge` on a `badges-unit` branch plus a Forgejo
notification to Paul is the minimum.

**(c) Both.** (a) blocks the merge; (b) catches what only time and
environment drift produce (runner image changes, ffmpeg package moves,
flaky-under-load tests like the exact-count cluster windows RELEASING.md
records) and produces the periodic green run that release tagging can
hang off.

**Decision, 2026-09-20:** Paul selected (a). Workspace Clippy and `make unit`
join the blocking ready-PR fast lane; no runtime-test schedule, scheduled
image publication, or recurring runner load is added. The source-only sizing
run in §2.4 supports a 30-minute timeout, while the first ready-PR run remains
the required Forgejo proof of the implemented lane.

### 3.2 `lint.yml` comment

Replace lines 15–16 with the truth under the chosen option — either "Main
pull requests run workspace Clippy in main-fast-lane.yml (`make lint`)" or
"Main pull requests run no workspace Clippy; the tracked pre-commit hook
does, and this workflow is the tag/dispatch badge."

### 3.3 The hiqlite-store blind spot

Three parts, all option-independent:

- `Makefile`: `unit-core` target = `cargo test --locked -p plurx-core
  --features hiqlite-store --lib` (the lines at `:782-791` already spell
  this for the cluster lanes) and a comment on `unit` stating that
  workspace feature unification is what turns the feature on there.
- AGENTS.md's focused-regression rule names the target: a focused
  `plurx-core` run is `make unit-core` or carries `--features
  hiqlite-store`; a bare `cargo test -p plurx-core --lib` is not evidence
  for anything under `store/hiqlite*`.
- A guard: `scripts/require-test-count` already fails a run whose test
  count differs from `PLURX_EXPECT_TEST_COUNT`; `unit-core` sets it from a
  checked-in floor (`validation/points.toml` check `unit-core` with
  `expect_at_least`), so a future feature drop that compiles out a block
  of tests fails by count. A floor, not an exact number: tests are added
  weekly.

### 3.4 `web-policy.test.js:6007` — behaviour, not a count

Delete the count assertion. Replace with a behaviour test in
`clients/android/app/src/test/.../BehindLiveWindowRecoveryTest.kt` (JVM,
runs in `make android-test`): create `BehindLiveWindowRecovery`, spend the
recovery on a finite item (`recover(1002, live = false, …)` → true, `spent`
true), call `attached()` as the prepared commit does, assert exactly one
new recovery is allowed (`recover` → true once, then false). The Node side
keeps the inventory assertions that establish discovery and ownership —
the `!live && used < 1` predicate text, the seek/prepare call shape, the
`if (behindLiveWindow.recover(` gate — with a comment stating their limit
(§4.8: they establish neither frame continuity nor cancellation cleanup).
The commit-site re-arm is proven by the Kotlin test, not by counting
regex matches.

### 3.5 `web-control.test.js:3164` — diagnose under Node 22

Not a timeout widen. Steps, in order, each recorded in the PR:

1. Instrument: wrap `running` to log which branch settled it — the held
   verdict (`settlePlaybackControlWaiters` matched the waiter), the ask's
   own bound (stubbed timer fired via `h.fire()`), or a reopen — with the
   `sequence` and `generation` of the verdict that settled it.
2. Run under Node 22.22.2 and Node 26 (`nvm` or the pinned image's
   `setup-node` with both versions in a matrix for this one file).
3. If the held verdict settled it on 22 and not on 26: the shipped
   `settlePlaybackControlWaiters` accepted a verdict for a **different**
   request sequence — a real ordering bug in the shipped code that the
   newer runtime's microtask ordering happens to hide. Fix the sequence
   check in the shipped function; the test stays as written.
4. If a stubbed timer fired: the harness's `h.fire()` ordering relative to
   `held.resolve()` differs by runtime; fix the harness so the fire
   happens strictly after `settledPromptly` observes pending (an explicit
   `await` on a promise the stub resolves when the waiter is registered),
   not by lengthening 400 ms.
5. Pin the Node version the fast lane's `web_compile` and `preflight` use
   (`main-fast-lane.yml:99, 189`: `node-version: "22"`) as the version the
   playback tests are asserted under, and add `node
   tests/playback/web-policy.test.js` and `web-control.test.js` to the
   fast lane's preflight step once both are green there — today only
   `player-input-contract.test.js` and `player-dom.test.js` run
   (`:102-105`).

### 3.6 Known-red inventory

`scripts/known-red` (Python, in `validation/`): lexes Rust rather than
grepping text, follows external modules and `include!`, and reconciles the
result with `cargo test --workspace --exclude plurx-cluster-check -- --list
--format terse`. It emits every ignored test with its reason and stable
`source.rs::module::function` identity. Comments and string literals cannot
create attributes; a bare, empty, duplicate, detached or non-test ignore is
an error. Every Cargo name must match exactly once, except the three named
platform/feature-gated identities that may be absent on this host.

Every `validation/known-red.toml` entry uses that full identity and carries
an owner, reason and expiry date. Bare names, suffix matches, duplicate rows
and ambiguous identities are invalid. The rule remains: **the file is empty,
or every entry resolves to exactly one test carrying a reasoned ignore in
source**. A test that is red on `main` and not ignored is a build break, not
an entry. `make operations-check` executes the positive and negative parser,
identity and expiry cases. Node and Python suites are listed in the same file
with the same rule (the two §2.6 tests are the only candidates today, and
M4/M5 make them green rather than entries).

---

## 4. Guardrails (non-goals)

- **Do not decide the policy in this document.** §3.1 recommends; §7.1
  records Paul's choice with a date. Changing `test_evidence_workflows.py`
  before that choice is the policy change smuggled in as a test edit.
- **Do not replace `make unit` with two `cargo test` invocations.** The
  first draft's `cargo test -p plurxd --bin plurxd` is not the workspace
  lane and drops the restart fixtures (assessment 10).
- **Do not run the cluster lanes on the schedule.** They are 40-minute
  jobs on dedicated `ci-store` runners; six times a day starves the PR
  lanes. The job conditions in §3.1(b) are the guard.
- **Do not let a schedule event cancel a tag run**, and do not let two
  schedule runs queue. The concurrency group and expression both change.
- **Do not widen the 400 ms in `settledPromptly`** or add retries to make
  `web-control.test.js` green (review §4.8, explicitly).
- **Do not mark a red test `#[ignore]` to satisfy the inventory** without
  a reason that names the owner and the condition for un-ignoring it. The
  inventory exists to make red visible, not to hide it.
- **Do not change the fast lane's ffmpeg without the runtime plan.** If
  `make unit` joins the lane, its ffmpeg major is the one
  SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE §3.4 chooses, through
  the same action.
- **Do not treat a green scheduled run as deployment qualification.**
  It is evidence about `main` at one commit; the deploy identity is still
  the `sha-` image (../reviews/ARCHITECTURE-REVIEW-2026-09-20.md).
- **Do not add a `push: branches: [main]` trigger** to get images back;
  the ruling says merge does not build, and the schedule is the bounded
  alternative.

---

## 5. Milestones

### 5.1 M1 — measure option (a) on the real runner (no policy change)

A branch adding `make lint` and `make unit` to `rust_compile` with
`continue-on-error: true` and `timeout-minutes: 45`, run three times by
`workflow_dispatch` (once cold after `ci-cargo-cache-prune`, twice warm),
on the `high-cpu` runner. Also time `make unit` alone on media1 once, for
the "what a contributor pays locally" number. Record all four timings in
§2.4 under a dated heading. Not merged; the branch is the measurement.

Acceptance: §2.4 has a second table with cold and warm wall-clock for
`make lint`, `make unit` and the whole `rust_compile` job, each from a
named Forgejo run URL.

**Outcome:** the named acceptance could not be met before merge because the
online high-CPU runner image had drifted to FFmpeg 8 and no media1 runner was
online. §2.4 preserves all failed run URLs and the one explicitly non-Forgejo
fallback timing. This is a documented infrastructure deviation, not invented
cold/warm evidence. The deviation also changed the milestone order: the
current-main unit failures and hang were repaired before measurement so the
timed command could have a meaningful completion signal.

### 5.2 M2 — implement Paul's choice

(a): `rust_compile` gains the two steps and the ffmpeg action; `timeout-minutes`
from M1; `test_contracts.py:1692-1745`'s fast-lane step assertions updated
to the new list; DEVELOPMENT_PIPELINE.md's opening block gets a dated
amendment paragraph. (b): the schedule, the job conditions, the concurrency
change, the `publish_main` widening if chosen, a `badges-unit` badge and a
notification step; `test_evidence_workflows.py:14-22` updated to allow
`schedule:` on `ci.yml` only, with the cadence asserted; the amendment
paragraph. (c): both PRs, (a) first.

Acceptance: (a) — a deliberately failing test on a throwaway PR turns
`Main promotion gate` red and the PR cannot be marked "checks passed"; (b)
— `git log -1 --format=%H origin/main` matches the SHA of the next
scheduled run's checkout and the run's job list contains `check`,
`web_layout`, `android_jvm` and no `cluster_*` job; `make operations-check`
green with the updated assertions.

The literal throwaway failing-unit negative was not recovered. §7.1 accepts
that obsolete historical proof gap under delegated authority, explicitly as
**not executed**; the real bootstrap failure is not substituted for it.

### 5.3 M3 — `lint.yml` comment, `unit-core`, AGENTS.md rule, count floor

Per §3.2 and §3.3.

Acceptance: `make unit-core` runs and prints a test count ≥ the floor;
`PLURX_EXPECT_TEST_COUNT=99999 make unit-core` fails; `grep -n "hiqlite-store"
AGENTS.md` shows the rule; `lint.yml:15-16` reads as the chosen option
says.

### 5.4 M4 — `web-policy.test.js:6007` → Kotlin behaviour test

Per §3.4.

Acceptance: `node tests/playback/web-policy.test.js` exits 0 on `main`;
`make android-test` runs `BehindLiveWindowRecoveryTest` with the new
case; `python3 -m unittest tests.operations.test_contracts` still green
(the file's inventory assertions are text contracts some tests read).

### 5.5 M5 — `web-control.test.js:3164` diagnosed and fixed

Per §3.5, with the PR body stating which of steps 3/4 applied and why.

Acceptance: `node tests/playback/web-control.test.js` exits 0 under Node
22.22.2 **and** Node 26 on the same checkout; the fast lane's preflight
step runs both playback tests.

### 5.6 M6 — known-red inventory

Per §3.6.

Acceptance: `python3 -m validation.known_red` prints the eleven ignored
Rust tests with reasons and "known-red: 0 entries"; `make operations-check`
includes the expiry test; a deliberately added entry naming a non-ignored
test fails it.

---

## 6. Verification and rollout

The procedure below records the original 2026-09-21 implementation. Current
changes follow [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) and §7.1;
this documentation-only reconciliation runs no unit/discovery sweep and
does not repeat successful historical checks.

This plan is one draft PR into `main`; milestones are logical commits and
Execution-log rows. M2 changes the lane itself, so the same PR's first
post-review ready run is its Forgejo acceptance evidence. Node
gate: `node tests/playback/web-policy.test.js` and
`web-control.test.js` for M4/M5. Python gate: `make operations-check` for
M2, M3, M6. Option (a) adds no schedule and no device evidence.

Actual order: claim → repair the current-main unit baseline → M3–M6 → M2 →
bounded M1 fallback → exact-main reconciliation. Repair moved ahead of M1
because a suite with deterministic failures and a reproduced hang cannot
produce a meaningful lane-cost measurement.

---

## 7. Decisions

1. **Execution policy, dated history:** Paul selected option (a) on
   2026-09-20: workspace Clippy and `make unit` in the ready-PR lane. Paul
   superseded that placement on 2026-10-07 in `a3162e446`: ready Rust runs
   all-target compile and workspace/vendor Clippy. Full `ci.yml` qualification
   is manual or release-tag triggered; configured coverage independently
   executes workspace units. The original evidence is retained, not relabelled current.
2. **Schedule, image publication, notification:** no schedule, so these
   option-(b) questions are not applicable to P-01.
3. **Node version:** CI asserts Node 22. Local diagnosis also ran Node 26;
   both playback files passed on the same checkout, so no runtime-specific
   implementation branch was warranted.
4. **Runner environment:** pin Ubuntu 24.04 / FFmpeg 6 in the Rust job and
   keep generic high-CPU selection. Run 2392 proved the host image itself no
   longer supplied the pinned FFmpeg major.

### 7.1 Current policy and acceptance disposition — 2026-10-08

Under the user's delegated authority for routine architecture decisions,
M1's recorded runner-image deviation and M2's obsolete, unrecovered
throwaway-unit negative are accepted historical limitations. Neither is
called an executed pass. No replacement cold/warm or old-policy full-suite
campaign is required. The original authors, decisions and Execution log
remain intact; this disposition can be overturned by the owner.

| Milestone | Historical evidence and exact limit |
|---|---|
| M1 | Failed runs in §2.4 and one source-only sizing receipt; the named Forgejo cold/warm table was not obtained. Accepted infrastructure deviation, not invented timings. |
| M2 | [Display 2413](http://forge.lan:3000/noirr/plurx/actions/runs/2413) / API 2431 failed before Rust because Node was absent. [Display 2415](http://forge.lan:3000/noirr/plurx/actions/runs/2415) / API 2433 genuinely passed at `2114f4e5`, before [#401](http://forge.lan:3000/noirr/plurx/pulls/401) merged as `21eab120`. This proves actual aggregate/bootstrap behavior, not the unrecovered throwaway-unit negative. |
| M3 | Original `53be36f9` receipt: 1,142 core tests and failing 99,999 negative floor. Current `unit-core` still enables `hiqlite-store` and the 1,100 minimum. No fresh core execution claimed. |
| M4–M5 | Original `2f5fc142` receipt records Node 22.22.2/26.8.1 and Android JVM/lint; the finite-recovery case remains. Later web source changes require their own applicable evidence, not an old whole-file green claim. |
| M6 | Original [review 3187](http://forge.lan:3000/noirr/plurx/pulls/401#issuecomment-3187) and [disposition 3194](http://forge.lan:3000/noirr/plurx/pulls/401#issuecomment-3194) record lexical/identity repair, nine parser positives, exact Cargo-list reconciliation and zero debt entries. That is historical parser evidence, not a current full-suite verdict. |

Authenticated historical run 2415 completed at 2026-09-21 06:04:27 UTC,
before #401 merged at 06:05:16. Preflight job 27976/task 10553 actually ran
199 validation and 418 operations tests; Rust job 27977/task 10554 used
Rust 1.97.1/FFmpeg 6.1.1, with core 1,146 passed/0 failed and daemon
2,438 passed/0 failed/8 ignored, plus both serial restart cases passed.
Aggregate job 27982/task 10556 was genuinely successful. These retained
raw outcomes close the ready-head provenance gap without replaying them.

An additional retained **pre-`a3162e446` historical unit-negative** is
[#847](http://forge.lan:3000/noirr/plurx/pulls/847), API run 4301/display
4280, ready attempt 1 at `831da491`: Rust job 43928/task 16528 genuinely
failed units, and aggregate job 43933/task 16569 failed at 2026-10-07
20:59:19 UTC with `Job rust_compile failed`. Its raw unit log SHA-256 is
`e1670d641b325cc45328413a3832452a8d75d6181fe378e11d66deda3b3624f5`;
the 859-byte gate log is
`29f13ca02ad1339e02b0c43adbef5e03736f90dcdf0844f900407a219277c58b`.
This is real old-policy fail-closed evidence, not the literal M2 throwaway
negative, current compile-only acceptance, a repair pass, or new execution.

**Current failure repair is open.** Configured coverage API 4340/display
4319, job 44252/task 16617, checked out `c8884f6` and ended exit 101 at
2026-10-08 00:33:53 UTC: 88 ordinary failed records plus one separate
stack-overflow/SIGABRT test identity make **89 observed adverse IDs**.
The daemon produced no final summary, so this is not a complete census.
Original #847 job 43928 maps 40 of those IDs to actual passes and 49 to
actual failures. Its complete core/daemon summaries contain 51 ordinary
failures in total (core 1; daemon 50), including two additional direct
shared-source cases absent from aborted coverage: `sharing_source_direct_range_matches_local_serve_file_range`
and `sharing_source_direct_symlink_or_resized_file_refuses`. Absence from
that aborted run is not a pass. The overlapping 49 remain a recorded unresolved cohort, not proof
that every later `main` has exactly 49 failures. New repair source or a
passing Darwin diagnostic does not establish the failing Linux result.
Do not hide this queue in the empty catalog or mark whole-plan Done.

---

## Execution log

Executing sessions append one row per milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | Commit / PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-20 | gpt-5.6-sol | agent:/root/p01_builder | Claim | `bab55a7e` / [#401](http://forge.lan:3000/noirr/plurx/pulls/401) | Claimed P-01 as one draft plan PR. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/p01_builder | Baseline repair | `763f05b2`, `9a17df1e` / #401 | Repaired 29 initial failures plus 12 later deterministic fixture failures; the bounded suite reached 2,380 passed / 12 failed before the final focused repairs, and every repaired exact test then passed. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/p01_builder | M3 | `53be36f9` / #401 | `unit-core` ran 1,142 tests; the 99,999 negative floor failed as designed; replicated-store guidance and count-floor contracts added. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/p01_builder | M4–M5 | `2f5fc142` / #401 | Web policy/control passed under Node 22.22.2 and 26.8.1; Android JVM tests and lint passed in 9m10s; no ordering implementation change was justified. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/p01_builder | M6 | `b4fd8c0a` / #401 | Eleven reasoned ignores, zero known-red entries; malformed/non-ignored/expired catalog cases are rejected. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 | `10baad55`, `88f689bd` / #401 | Option (a) implemented with Node playback preflight, workspace Clippy, `make unit`, Ubuntu 24.04 / FFmpeg 6 pin, and a 30-minute bound; no schedule added. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | `f62eb0ab`–`75ed2823` / #401 | Temporary branch instrumentation was committed and removed. Runs 2382/2384/2386/2388 did not measure; run 2392 exposed FFmpeg drift. One bounded source-only fallback measured 155 s compile, 85 s Clippy and ~610 s unit, all green. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Exact-main reconciliation | final reconciliation / #401 | One 336-second workspace confirmation found four fixture failures after merging `882862e8`; all four exact tests then passed together, and Rustfmt plus workspace Clippy passed. No broad retry was spent. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Adversarial review P1 | review fix / [#401 comment 3187](http://forge.lan:3000/noirr/plurx/pulls/401#issuecomment-3187) | Replaced regex and suffix matching with lexical, attached-attribute scanning, stable source/module identities, duplicate rejection and exact Cargo-list reconciliation; executable positives and negatives cover every reported bypass. |
| 2026-10-08 | gpt-6.1-sol | agent:/root/remaining_requirements_audit_sol61 | Current-policy and evidence reconciliation | docs-only continuation on `8e242787c` | Preserves Paul `a3162e446` compile-only ready-Rust policy and original M1–M6 authors/receipts; accepts M1's infrastructure deviation and unrecovered M2 unit-negative as historical limitations, not executed passes. Historical 2413/2415 run/job/raw identities verified; current 89-adverse/49-unresolved cohort remains open. Zero unit/discovery replay, release or deployment; root hands merge-ready work to the batching coordinator. |
