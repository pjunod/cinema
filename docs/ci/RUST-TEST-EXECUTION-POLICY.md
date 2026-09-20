# Rust test execution policy — where the suite runs, and what red means

**Status:** ready for review · **Executes:** §2.2 / §4.8 / F-build-1 /
F-hist-7 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the
ruling this document asks Paul to revisit), [VALIDATION.md](../VALIDATION.md)
(how paths select evidence) and
[CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) (the earlier lane
design). Read review §2.2, §4.8 and §7.1, then assessment correction 10 and
rows `2.2`, `F-build-ops-codehealth-1`, `F-hist-7`, then this document. §3
presents the options with their measured and estimated cost; **§7.1 records
the decision as Paul's** — this document recommends, it does not decide.
M1 (measure) runs before the decision; M2 implements whichever option he
picks; M3–M6 are independent hygiene items that hold under any option.

The standing instruction: **if a step seems to require changing
`make unit`'s definition, the `plurx-cluster-check` exclusion, the cluster
lanes' features, or the draft-gate semantics of `main-fast-lane.yml`, stop
and flag it.** This plan changes *when* the existing lane runs, not what it
is.

**Correction to the review:** two additions, one narrowing.
(1) `ci.yml`'s `publish_main` job (`ci.yml:1452-1473`) runs only when
`github.event_name == 'push' && github.ref == 'refs/heads/main'`. Since
`3cd127e2` (2026-09-10) `ci.yml` has no `push: branches` trigger, so the
`sha-<12hex>` image publication that OPERATIONS.md §"The fleet registry"
calls automatic has had no automatic trigger for ten days; only a manual
dispatch on `main` produces one. Option (b) below must widen that condition
to `schedule` if the schedule is to restore image publication, and
the ledger/release-tags plan (not yet written; see the work board)
depends on this. (2) The policy is pinned by text contracts:
`tests/operations/test_evidence_workflows.py:14-22` asserts `ci.yml` has no
`schedule:` and no `branches: [main]` trigger, and
`tests/operations/test_contracts.py:1692-1745` pins the fast lane's Rust
step to `make effort-rust-check` + `make hiqlite-vendor-clippy`. Either
option changes those tests in the same PR; that is the policy change made
visible, not a workaround. (3) The review's "no Rust test runs anywhere on
any trigger" is exact for CI; the tracked pre-commit hook and AGENTS.md's
focused-regression duty do run tests, on the contributor's machine, which
is enforcement by convention.

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

Done means: the decision is recorded in §7.1 with a date, M2's PR is
merged under it, and M3–M6 are merged.

---

## 2. Contract today

Re-verify at build time.

### 2.1 What runs where

| Trigger | Workflow | Rust content |
|---|---|---|
| PR to `main`, ready (not draft) | `main-fast-lane.yml:110-130` `rust_compile` | `make effort-rust-check` (`Makefile:76-77`: `fmt-check`, `spike-lock-check`, `cargo check --workspace --locked --all-targets`) + `make hiqlite-vendor-clippy` (`:156-159`, vendored lib only) |
| `v*` tag, `workflow_dispatch` | `ci.yml:6-9` | `check` job (`:260-291`): `make ci-rust-gate` = `fmt-check spike-lock-check lint test-socket-permission-check` + `cargo test --workspace --locked --exclude plurx-cluster-check --no-fail-fast` + `vodencode-restart-check`, on `ffmpeg-6` runners; cluster lanes; `publish_main` (push-to-main only) |
| `v*` tag, dispatch | `lint.yml:3-6` | `make fmt-check lint` (workspace Clippy), badge |
| weekly cron | `rust-audit.yml` | advisories only |
| commit (local) | `scripts/pre-commit` → `precommit-check` (`Makefile:88-90`) | `validation-lint fmt-check lint` + `js-check` — Clippy, no tests |

`make unit` (`Makefile:44-46`) is `spike-lock-check test-socket-permission-check`
then `cargo test --workspace --exclude plurx-cluster-check --no-fail-fast`
then `vodencode-restart-check` (two `#[ignore]`d real-FFmpeg restart
fixtures, serial). `make test-full` (`:50-54`) adds
`plurx-core/hiqlite-contract-tests,plurxd/cluster-integration-tests`.

### 2.2 The stale comment

`lint.yml:15-16`: `# Release-tag and manual badge workflow. Main pull
requests run Clippy in main-fast-lane.yml after the one adversarial
review.` They do not; the fast lane runs the vendored crate's Clippy only.

### 2.3 The ruling

DEVELOPMENT_PIPELINE.md's opening block, quoted so the conflict is visible:

> **Workflow correction, 2026-09-10, amended 2026-09-13:** Main-bound pull
> requests use only main-fast-lane.yml: open as draft, obtain exactly one
> adversarial review, address it, then mark ready. Marking ready starts the
> lane; returning the PR to draft cancels it; draft PRs allocate no jobs.
> […] Full CI runs only by manual dispatch or an explicit release tag.
> Effort compiler checks and evidence proofs are manual. Merge does not
> build or deploy an image. Runtime-test schedules are disabled; the weekly
> dependency audit remains.

**Decider: Paul.** The review's §7.1 adds that Paul's 09-17 instruction
made the fast lane "the one place tests run", which conflicts with the
09-10 text that makes it compile-only — either reading leaves no automatic
Rust test execution, and the 09-10 record's assumption that "a batch
process picks up full-suite failures" has no producer (review §1 item 1,
§2.2).

### 2.4 Measured timings (DEVELOPMENT_PIPELINE §4, one crate, one session)

| Step | Cold | Warm |
|---|---:|---:|
| `cargo check -p plurxd --all-targets` | ~4 min | ~35 s |
| `cargo clippy -p plurxd --all-targets -- -D warnings` | ~3 min | ~3 min |
| `cargo test -p plurxd --bin plurxd` (1,504 tests) | ~4 min | ~2 min |

These are `-p plurxd` numbers on the cloud loop, not the `high-cpu` runner
and not the workspace.

### 2.5 The hiqlite-store blind spot

`e314a5bd` (2026-09-14): `cargo test -p plurx-core --lib` does not enable
`hiqlite-store` (`crates/plurx-core/Cargo.toml:17`), so 282 tests behind
`#[cfg(feature = "hiqlite-store")]` compiled out of a run that was reported
green; three DVR replicated-store defects shipped. `make unit` builds the
workspace, and `plurxd` depends on `plurx-core` with `features =
["hiqlite-store"]` (`crates/plurxd/Cargo.toml:32`), so feature unification
turns the feature on there; the blind spot is the **focused per-crate**
command form, which is exactly the form AGENTS.md asks contributors to run.

### 2.6 The two red Node tests

- `tests/playback/web-policy.test.js:6007`: `assert.equal((androidController.match(/behindLiveWindow\.attached\(\)/g) || []).length, 2, …)`.
  The tree has one call, inside `attachRecipe` (`Controller.kt:373-382`),
  which the prepared commit reaches (`:3565`). The count is stale; the
  behaviour it meant to pin (budget re-armed at both attach sites) holds.
  Reproduced here: 137 PASS then the `strictEqual`.
- `tests/playback/web-control.test.js:3164`: under Node 22.22.2 (this
  checkout) `settledPromptly(running)` returns `true` where the assertion
  wants `false` ("the in-flight exchange's verdict does not answer this
  ask"); Astra's Node 26.8.1 run passed. `settledPromptly` (`:2749-2755`)
  is four `flush()` microtask pairs, a real 400 ms `setTimeout`, one more
  flush, then `Promise.race`. The harness stubs `setTimeout` inside the
  shipped code (`:2354-2455`, `timers` map), so the ask's own bound cannot
  fire on wall-clock time — `running` settled because the held verdict
  reached the waiter, or because a stubbed timer was fired by `h.fire()`
  ordering, and which one differs by runtime. Reproduced here.

### 2.7 Ignored tests today

Twelve `#[ignore = "…"]` in `crates/` (`grep -rn '#\[ignore' crates/`),
every one with a reason (600 MB store probe, 620 MiB EPUB proof, two-hour
audio benchmark, the two serial FFmpeg restart fixtures, MiniLM download
opt-in, nightly runner capability, private ATSC capture, macOS
VideoToolbox, two contract-factory spawns). One bare `#[ignore]` mention is
a doc comment (`storeprobe.rs:791`), not an attribute.

---

## 3. Change

### 3.1 The three options, with cost

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

**Recommendation:** (c), for the reason the review gives — (b) alone
leaves the deploy window — and because (a) alone gives no periodic run for
`sha-` images and tags to bind to. If the measured cost of (a) exceeds
15 min warm, the fallback recommendation is (b) with `publish_main`
widened and a notification target, plus AGENTS.md's focused-regression
duty upgraded to "run `make unit` before marking ready" as a stated
convention.

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

`scripts/known-red` (Python, in `validation/`): parses `cargo test
--workspace --exclude plurx-cluster-check -- --list --format terse` output
plus `grep -rn '#\[ignore'` and emits a table: every ignored test with its
reason, every test whose name appears in `validation/known-red.toml` with
an owner, a reason and an expiry date. The rule: **the file is empty, or
every entry is a test carrying `#[ignore = "<reason>"]` in source**. A
test that is red on `main` and not ignored is a build break, not an entry.
`make operations-check` gains a test that the TOML's entries all resolve
to ignored tests and none has passed its expiry. Node and Python suites
are listed in the same file with the same rule (the two §2.6 tests are the
only candidates today, and M4/M5 make them green rather than entries).

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

Acceptance: `python3 -m validation.known_red` prints the twelve ignored
Rust tests with reasons and "known-red: 0 entries"; `make operations-check`
includes the expiry test; a deliberately added entry naming a non-ignored
test fails it.

---

## 6. Verification and rollout

Every PR here is a draft into `main` under the fast lane; M2 is the one
that changes the lane itself, so its own run is the first evidence. Node
gate: `node tests/playback/web-policy.test.js` and
`web-control.test.js` for M4/M5. Python gate: `make operations-check` for
M2, M3, M6. Nothing here needs a device or the fleet; the only
fleet-adjacent effect is (b)'s runner load, observed for one week via the
runner janitor's disk and queue reports (RUNNER-DISK.md) before the
cadence is called settled.

Rollout order: M1 → decision → M2 → M3–M6 in any order. If Paul's answer
is "keep compile-only", M2 is replaced by a dated paragraph in
DEVELOPMENT_PIPELINE.md stating that no automatic Rust test execution
exists by decision, that `make unit` before marking ready is the
contributor's duty, and that the `sha-` image publication needs its own
trigger — and M3–M6 still land.

---

## 7. Open questions

1. **Paul's decision on §3.1** — (a), (b), (c), or keep. Recorded here
   with the date when made. Until then, DEVELOPMENT_PIPELINE.md's 09-10
   block is the policy.
2. **Whether scheduled runs publish `sha-` images.** Tied to
   LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md §3.5; if yes, `publish_main`'s
   condition and OPERATIONS.md's "every successful main run" sentence
   change together.
3. **Notification target for a red scheduled run.** Forgejo notification,
   a Slack-shaped webhook, or the badge alone. The badge alone is what
   "nobody is told" looks like.
4. **Node version pin.** The fast lane uses 22; local machines run 26. M5
   decides which is asserted; running both in preflight doubles a
   40-second step and is acceptable if the ordering bug is real.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
