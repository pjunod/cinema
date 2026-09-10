# AI harness implementation plan — the loop, the evidence, the drift, then the tools

**Status:** ready to build · **Executes:** the order in
[AI-HARNESS-FABLE-ASSESSMENT.md](AI-HARNESS-FABLE-ASSESSMENT.md) §3 with the
recommendations from Codex's assessment (its `AI-HARNESS-ASSESSMENT.md`,
still untracked in `ci/` when this was written) folded into the milestones
they belong to · **Written:** 2026-09-10 · **Author:** Fable.

Read the assessment first for *why* the order is what it is; this document
is *what to build*. Work milestone by milestone: each milestone is one
main-bound pull request (draft → exactly one adversarial review → findings
addressed → ready → `fast-lane` → merge), except where a milestone says it
is several PRs. Every milestone ends with an acceptance check that is a
command or an observable fact. The standing instruction: if a step seems to
require changing product behaviour, weakening a rule in
[AGENTS.md](../../AGENTS.md), or adding a new mandatory pre-merge gate that
this plan does not name, stop and flag it instead of doing it.

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the
process every milestone here flows through), [VALIDATION.md](../VALIDATION.md)
(the catalog M4 and M5 extend), and
[AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md) (the recipe M1 turns into a
command).

## 1. Objective

Make it cheap for an agent — fleet worker, Codex session, Claude session,
whichever model — to change plurx *correctly*: get a trustworthy compile and
test answer in minutes from wherever it runs, produce tests that prove the
change, read only the guidance that applies to the subsystem it is in, and
never learn a repository rule from a red CI check. Then, with the file sizes
halved and the guides in place, measure whether navigation tooling buys
anything on the residual cost.

Ten milestones, in the order the assessment argues for:

| # | Milestone | Attacks | Source |
|---|---|---|---|
| M0 | Baseline counters | measurement | Codex §"Priorities" measures table |
| M1 | `scripts/agent-check` — one local command | compiler access, paperwork-in-CI | Fable §3.1, Codex priority 4 |
| M2 | Inline tests out of the four hotspots | file size, merge conflicts | Fable §3.2, Codex priority 5 (prerequisite) |
| M3 | `prove-fix` in the loop, reviewer defect catalog, PR-scoped mutants | tests that prove nothing | Fable §3.3 |
| M4 | Two fences: replicated-SQL placeholder order, Store boundary | recurring mechanical defects | Fable §3.4, Codex priority 6, Gemini §2.1 |
| M5 | Subsystem guides, prompt subtraction, `guide =` in the catalog, exemplar lines | context tax, doc drift | Fable §3.5, Codex priorities 2–3, Gemini §1.1/§4.2 |
| M6 | `swarm/` reads `AGENTS.md`; a test that prompts match policy | harness drift | Fable §3.6, Codex "audit overlapping root instructions" |
| M7 | Status and changelog as per-PR fragments | bookkeeping conflicts | Fable §3.7 |
| M8 | PR template: change ledger and evidence block; suppression advisory | drift summaries | Codex "drift summaries", Gemini §5 |
| M9 | Ripwire pilot against `rust-analyzer scip`, measured | navigation | Codex priority 1, Gemini §4.1 |
| M10 | First cohesive extraction from `transcode.rs` by co-change | concentrated responsibility | Codex priority 5 |

## 2. Contract — what exists today, copied from the tree

Re-verify each of these against the file named at build time; `main` moves
about a hundred commits a day and any of them can have moved.

### 2.1 Make targets and scripts the milestones compose

From `Makefile` (line numbers at `4d05857f`):

| Target | Runs | Used by |
|---|---|---|
| `fmt-check` | `cargo fmt --all --check` | M1 |
| `effort-rust-check` | `cargo check --workspace --locked --all-targets` (after `fmt-check spike-lock-check`) | M1 — this is what `main-fast-lane.yml` `rust_compile` runs |
| `lint` | `cargo clippy --workspace --all-targets -- -D warnings` | M1 |
| `history-check` | `scripts/history-audit --report target/validation/history.json` | M1 (2 m cold, 18 s warm — walks the whole history) |
| `operations-check` | `python3 -m unittest discover -s tests/operations -p 'test_*.py'` | M1, M6, M7 |
| `validation-lint` | `scripts/validate lint` | M1, M4, M5 |
| `validate-staged` | `scripts/validate run --profile commit --staged` | M1 |
| `web-check` | the three `node tests/playback/*.test.js` suites | M1 when web files changed |
| `ci-rust-gate` | `cargo test --workspace --locked --exclude plurx-cluster-check --no-fail-fast` | full sweep only — **not** M1 |

`scripts/validate` (`validation/runner.py`) selects points with exactly one
of `--all`, `--staged`, `--changed-from REF`, `--paths …`, `--point ID`, and
has subcommands `lint`, `list`, `plan --profile P`, `run --profile P
[--strict] [--fail-fast] [--artifact-dir D] [--no-report]`. The catalog loader
(`load_catalog`, `validation/runner.py:133`) builds `Point(id, title,
contract, paths, checks, depends_on)` with `item.get(...)`, so **an unknown
key on a `[[points]]` table is ignored, not rejected** — M5's `guide =` field
needs its own lint clause.

`scripts/prove-fix` (already written, nothing runs it):

```bash
scripts/prove-fix [--command 'CMD {filter}'] [--repo DIR] BASE TEST_FILTER PATH...
# restores PATH... from BASE in a clone of --repo, runs the test command with
# TEST_FILTER, and passes only if the retained test FAILS without the fix.
# Default command: cargo test --workspace {filter}. '-' means no filter.
# Swift: scripts/prove-fix --command 'make apple-test' origin/main - clients/apple/Sources/PlayerView.swift
```

`python3 -m validation.mobile_versions` reads `PLURX_VALIDATION_MODE`
(`all` | `changed-from`), `PLURX_VALIDATION_BASE`, and
`PLURX_VALIDATION_MERGE_TARGET` (`validation/mobile_versions.py:384–386`);
`main-fast-lane.yml` `mobile_version` calls it with mode `changed-from`,
base = the PR's merge base, merge target `origin/$BASE_REF`.

`python3 -m validation.ci_scope` computes the affected-surface scope,
including `docs_only` (`validation/ci_scope.py:213 is_docs_only`).

`scripts/require-test-count` wraps `cargo test`; with
`PLURX_EXPECT_TEST_COUNT=N` and `--exact` it fails when the run does not
report exactly `N` tests. M2 uses it.

`scripts/player-input-fence` is the model for M4: a Python script with a
`TOKENS` table of spellings and a docstring stating the invariant; wired as
`[[checks]] id = "player-input-fence"` with `profiles = ["commit", "ci",
"full", "nightly"]`, `requires = ["python3"]`, `missing = "fail"`,
`timeout_seconds = 30` (`validation/points.toml:79–86`).

### 2.2 The fast lane, exactly

`.github/workflows/main-fast-lane.yml` (Forgejo runs it) on
`pull_request` `labeled` with `fast-lane`: `scope` (`validation.ci_scope`)
→ `mobile_version` → `preflight` (`make history-check`, `make
validation-lint` + `tests/validation`, `make operations-check`, then either
the two `node` contract tests or, when `docs_only`, `scripts/js-check` +
`node --check` over the web JS) → `rust_compile` (`make effort-rust-check`,
`make hiqlite-vendor-clippy`) → `web_compile` → `apple_compile` (`make
apple-build`) → `android_compile` (`make android`) → `promotion_gate`
(`Main promotion gate`). M1 reproduces `mobile_version`, `preflight` and
`rust_compile` locally and nothing else.

### 2.3 The hotspots and their test regions

| File | Lines | `mod tests` begins | `#[test]`/`#[tokio::test]` | `#[cfg(test)]` blocks above `mod tests` |
|---|---:|---:|---:|---:|
| `crates/plurxd/src/transcode.rs` | 43,584 | 25,200 (`pub(crate) mod tests {`) | 272 | 205 |
| `crates/plurxd/src/playback_control.rs` | 29,135 | 14,311 | 261 | 146 |
| `crates/plurxd/src/http/hls.rs` | 23,834 | 11,798 | 183 | 45 |
| `crates/plurxd/src/http/mod.rs` | 13,659 | 1,043 | 127 | 2 |

`transcode.rs:34670` already does `include!("vodencode_manager_tests.rs");`
and `vodserve.rs:8106` does `include!("vodencode_tests.rs");` — the crate has
a sibling-file convention for tests, via `include!`. M2 uses `mod tests;`
with a directory instead (rust-analyzer resolves it; `include!` bodies are
opaque to it), and may migrate the two `include!` sites to match in the
same PRs. `crates/plurxd/src` has 2,071 test attributes in total (the M2
baseline for `require-test-count`).

### 2.4 The harness files

`swarm/config.json` (`schema_version: 3`): `project.quality_command` is
`"make validate"`; `requirements.executables` is `["gh"]` with a `gh auth
status` check; `queue.adapter` is `"github-issues"`; roles map to
`swarm/lead.txt` (53 lines), `swarm/builder.txt` (42), `swarm/reviewer.txt`
(18), `swarm/troubleshooter.txt` (36); `swarm/project.txt` (187 lines) is
the project prompt, `swarm/core.txt`, `apple.txt`, `android.txt` are
territory prompts. `builder.txt` and `core.txt` open with "Always read
README.md, docs/ARCHITECTURE.md, and docs/VALIDATION.md" (24 KB + 34 KB +
64 KB). `project.txt` names `https://github.com/pjunod/plurx`, says
"`main` requires a pull request to be up to date before it merges", and
"Use separate full clones only. Never create a git worktree." (the last one
stays — it is about worker isolation, not the merge policy).

### 2.5 The bookkeeping files

`STATUS.md` (1,332 lines): H1, an `**Updated:** YYYY-MM-DD` line, then
`## <effort title>` sections, newest first, each opening with a bold verdict
sentence. `tests/operations/test_status_pr_claims.py` reads the three
`STATUS_PAGES` (`STATUS.md`, `docs/STATUS.html`,
`docs/playback-control/PLAYBACK-CONTROL-STATUS.md`) and refuses a merged PR
described as open. `CHANGELOG.md` is Keep-a-Changelog with `## [Unreleased]`
→ `### Fixed` / `### Added` / … → `- **bold lead.** prose` bullets.
`validation/regressions.d/` holds 705 per-commit evidence files — the
existing precedent for "one file per change, folded by a tool".

### 2.6 The catalog

`validation/points.toml`: 26 `[[points]]`, 33 `[[checks]]`. Point keys in
use: `id`, `title`, `contract`, `paths`, `checks`, `depends_on`. Every
governed path (`[settings] audit_paths`, which includes `crates/**`,
`scripts/**`, `tests/**`, `validation/**`) must match some point's `paths`
or `validation-lint` fails — so any new file under `crates/` or `scripts/`
in this plan is added to a point in the same commit.

### 2.7 The placeholder-order class

`validate_parameter_order(sql)` at `crates/plurx-core/src/store/hiqlite.rs:3992`
is a **runtime** check: hiqlite binds by rusqlite's numeric index while
SQLite assigns `$N` indexes by first appearance, so a statement whose first
placeholder is not `$1` (or that skips a number) is refused when executed.
It is called from one wrapper at `:3985`. The class has five evidence files
in `validation/regressions.d/` (`4da3bbde-…-placeholders`,
`9e1e792e-shared-cache-parameters`, `c2d0725b-p3-replicated-contracts`,
`35352d40-peer-authority-coverage`, `b6890aa4-merge-resolution-fixes`) —
five times shipped, five times found at runtime or in review, never at
build time.

## 3. Non-goals — do not do these

- **No new mandatory pre-merge gate** anywhere in this plan except the two
  M4 fences, which are `commit`-profile checks in the catalog like
  `player-input-fence`. `prove-fix` (M3) is a builder-contract step and a
  reviewer expectation, not a workflow job. The mutation run stays on the
  sweep. The suppression check (M8) is advisory output. Reason: the whole
  point is fewer minutes waiting on CI; a plan about that must not add any.
- **No refactor of `transcode.rs` by responsibility before M2 and M9.**
  Reason: 42% of it is fixtures and no co-change data exists yet; a split
  chosen by eye is the "arbitrary pieces" Codex warns about.
- **No separate architect/implementer model split, no immutable-test phase,
  no file-count cap on a PR, no termination on cross-domain reads** (Gemini
  §1.3, §2.3, §3). Reason: assessment §4.
- **No committed symbol outlines or copied "golden" code files.** Reason:
  stale by the next commit at this velocity; the guide's exemplar line is a
  link into live code.
- **No Ripwire installation before M2 and M5 have merged.** Reason: the
  pilot must measure the residual navigation cost, not the cost of reading
  fixtures.
- **No change to the one-review rule, the draft rule, the effort-lane rule,
  or the separate sweep.** They are Paul's; this plan flows through them.
- **Do not edit `~/code/plurx` or any worker's clone from another worker.**
  Own clone, own branch, PR.

## 4. Milestones

Each milestone: **What** · **Files** · **Exactly** (the contract) ·
**Acceptance** (runnable). Estimates are for one agent with a warm compile
loop; "PR" means a main-bound draft PR through the standard process.

### M0 — Baseline counters (½ day, 1 PR)

**What:** the four numbers every later milestone is judged by, collected
from data the repository already produces, before anything changes.

**Files:** `scripts/harness-metrics` (new, Python), `ci/HARNESS-METRICS.md`
(new, generated table + the date and `main` sha it was taken at), a row in
`docs/README.md` for the metrics doc, and the script added to the
`ci.operations` point's `paths` (or whichever point owns `scripts/ci-*`
today — check `scripts/validate list`).

**Exactly:**

```bash
scripts/harness-metrics --forgejo http://192.168.4.7:3000 --repo noirr/plurx \
    --token-file ~/code/plurx-agent/github_token --since 2026-08-27 --weeks 2
# prints, per ISO week:
#   fast_lane_failures{bucket=paperwork|compile|real}   from /api/v1/repos/{r}/actions/runs
#       (workflow main-fast-lane.yml, conclusion=failure; bucket by the first
#        failed job: mobile_version|preflight → paperwork; *_compile → compile;
#        promotion_gate with all needs green → real)
#   review_findings{class=proof|other}                  from PR review comments whose
#       body matches ^\s*\d+[.)]  (the numbered-findings convention) — class=proof
#       when the finding text matches the M3 catalog keywords (mutation|reverted|
#       survives|zero assertions|reads .* as text|fixture .* impossible)
#   merge_laps                                          count of "Update branch by
#       merge" merge commits per merged PR (commits on the PR with two parents
#       whose second parent is on main), mean and p90
#   tokens_per_task                                     from the harness session
#       import if present under .swarm/ (skip with a note when absent)
```

Output is Markdown; the script never writes to the repo itself — the PR
commits its output by hand so the number is reviewable. Forgejo 16.0.3 has
no rerun API and its `actions/runs` ids differ from the web UI numbers
(`AGENT-COMPILE-LOOP.md` and the CI docs carry the details); resolve jobs
via `/actions/runs/{id}/jobs`.

**Acceptance:** `ci/HARNESS-METRICS.md` exists with one row per week
for the two weeks before this PR's base, all four counters filled or marked
`n/a (reason)`; `make operations-check` green; the script runs in under
60 s against the live Forgejo.

### M1 — `scripts/agent-check`: the fast lane, locally, in one command (1–2 days, 1 PR)

**What:** one script an agent runs before pushing that answers the same
questions the fast lane will ask, in the same order, and prints an evidence
block the PR description can carry verbatim.

**Files:** `scripts/agent-check` (new, Bash 3.2-clean because Paul's Mac
runs it too; delegates to Python for anything non-trivial),
`docs/ci/AGENT-COMPILE-LOOP.md` (gains a "the one command" section at the
top and keeps the manual recipe as the fallback), `AGENTS.md` (the "Rust
compile loop" section names the command), `swarm/builder.txt` and
`swarm/core.txt` COMPLETION sections (name the command, see M6), the
script added to the owning point's `paths`.

**Exactly:**

```bash
scripts/agent-check [--base REF] [--no-tests] [--no-clippy] [--evidence FILE]
#   --base REF      merge base to diff against (default: merge-base HEAD origin/main,
#                   after `git fetch origin main` — the base MUST be fetched fresh,
#                   the device VM's origin/main has been days stale)
#   --no-tests      compile-only (what the fast lane does); default runs the
#                   affected commit-profile checks too
#   --no-clippy     skip `make lint` (for a mid-edit loop; the final run never skips)
#   --evidence FILE write the evidence block to FILE as well as stdout
```

Steps, each printed with its wall-clock and exit status; the script stops
at the first failure unless `--keep-going`:

1. **Toolchain.** `rustc --version` must report `1.97.1` (from
   `rust-toolchain.toml`). If `cargo` is absent, print the archive-and-ship
   recipe from `AGENT-COMPILE-LOOP.md` and exit 2 — never silently skip
   compilation. If the repository path is on a FUSE mount (`stat -f -c %T`
   reports `fuseblk`/`osxfuse`/`macfuse`, or the `$HOME/mnt/` prefix), print
   "run this from a local clone or worktree — the mount is ~2,000× slower
   on directory walks" and exit 2.
2. **Scope.** `python3 -m validation.ci_scope` against `--base`; record
   `docs_only`, the changed paths, and the affected crates.
3. **Format.** `make fmt-check`.
4. **Compile.** `make effort-rust-check` (skipped when `docs_only`).
5. **Clippy.** `make lint` unless `--no-clippy`.
6. **Paperwork** — the fast lane's `mobile_version` and `preflight`, exactly:
   - `PLURX_VALIDATION_MODE=changed-from PLURX_VALIDATION_BASE=$BASE
     PLURX_VALIDATION_MERGE_TARGET=origin/main python3 -m validation.mobile_versions`
   - `scripts/history-audit --report target/validation/history.json`
     restricted to `$BASE..HEAD` — add a `--since REF` flag to
     `history-audit` in this PR (it walks the whole history today: 2 m cold);
     the fast lane keeps the full walk.
   - `make validation-lint` and `python3 -m unittest discover -s
     tests/validation -p 'test_*.py'`
   - `make operations-check`
   - when web files changed: `make web-check`; when `docs_only`:
     `scripts/js-check`.
7. **Affected tests** (unless `--no-tests`): `scripts/validate run --profile
   commit --changed-from $BASE`, then `cargo test -p <crate>` for each crate
   with a changed `.rs` file, `--exclude plurx-cluster-check`.
8. **Evidence block**, always printed last:

```text
agent-check  2026-09-10T18:42:11Z
rustc 1.97.1 (…)          cargo 1.97.1
base   origin/main@<sha>  head <sha>  tree <tree-sha>
scope  docs_only=false    crates=plurxd,plurx-core   web=false apple=false android=false
fmt-check            ok   2s
effort-rust-check    ok   4m12s
lint                 ok   3m40s
mobile_versions      ok   1s
history-audit        ok   3s   (--since origin/main)
validation-lint      ok   2s
operations-check     ok   9s
validate --changed   ok   1m05s   points=playback.control,streaming.hls
cargo test -p plurxd ok   2m10s   1908 passed
RESULT ok
```

The tree sha is `git rev-parse HEAD^{tree}` — the PR template (M8) asks for
this block, and a reviewer compares its tree sha to the PR head's. Evidence
against a different tree is not evidence.

**Acceptance:** on a branch with a deliberate `rustfmt` violation, a
deliberate type error, and an un-bumped Apple build counter, three
successive runs fail at steps 3, 4 and 6 respectively with the same message
the fast lane would give; on the corrected branch `scripts/agent-check`
prints `RESULT ok` and the PR's fast lane is green on the first attempt.
`make operations-check` gains `tests/operations/test_agent_check.py`
asserting the script's step list equals the fast lane's job list read from
`main-fast-lane.yml` (so the two cannot drift silently).

### M2 — Inline tests out of the four hotspots (2–3 days, 4 PRs)

**What:** move each hotspot's trailing `mod tests` into a sibling file and
its scattered `#[cfg(test)]` helpers into a `test_support` module. Tests
are moved, never edited. One PR per file, in this order: `http/mod.rs`
(92% tests, the cheapest proof of the recipe), `http/hls.rs`,
`playback_control.rs`, `transcode.rs`.

**Files (per file `X.rs`):** `X.rs` → keeps production code; `X/tests.rs`
(new, the body of `mod tests`); `X/test_support.rs` (new, the `#[cfg(test)]`
items that were interleaved above `mod tests`). For `http/mod.rs` the
directory already exists: `http/tests.rs` and `http/test_support.rs`.
Catalog `paths` entries that name the file by basename (e.g.
`crates/plurxd/src/{cachekeep,…,transcode}.rs` in `cluster.auth`) gain the
directory glob `crates/plurxd/src/transcode/**` in the same commit or
`validation-lint` fails.

**Exactly:**

```rust
// at the old `#[cfg(test)] pub(crate) mod tests { … }` site in transcode.rs:
#[cfg(test)]
pub(crate) mod tests;            // body moved verbatim to transcode/tests.rs
#[cfg(test)]
pub(crate) mod test_support;     // the 205 interleaved cfg(test) items
#[cfg(test)]
pub(crate) use test_support::*;  // so tests.rs and any production-adjacent
                                 // cfg(test) call sites compile unchanged
```

`transcode/tests.rs` begins with `use super::*;` exactly as the inline
module did. A `#[cfg(test)]` block that is an `impl` on a *production* type stays where
it is — an inherent `impl` cannot move without widening visibility — and is
counted in the PR description; a `#[cfg(test)] impl` on a test-only type
(check whether `HlsDeliveryFixture` at `transcode.rs:25094` is one) moves
with its type. The two
`include!` sites (`transcode.rs:34670`, `vodserve.rs:8106`) become
`mod vodencode_manager_tests;` / `mod vodencode_tests;` under the same
`#[cfg(test)]` in the transcode and vodserve PRs respectively, and the
files move into the matching directories.

Recipe, so the reviewer can check moves rather than content:

```bash
git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change \
    origin/main...HEAD -- crates/plurxd/src/transcode.rs crates/plurxd/src/transcode/
# every removed hunk in transcode.rs must appear as a moved block; a hunk that
# is neither moved nor the three-line mod declaration above is a finding.
PLURX_EXPECT_TEST_COUNT=<count before> scripts/require-test-count test -p plurxd --exact
# the count before is taken on the base sha with the same command.
```

**Acceptance (per PR):** `require-test-count --exact` passes with the base
count; `scripts/agent-check` `RESULT ok`; `wc -l` on the production file is
within 200 lines of (old total − old test region − moved helpers); the
`--color-moved` diff shows no non-moved content hunk; the review records
the count of `impl`-internal `#[cfg(test)]` items left in place. After all
four: the sweep's `make ci-rust-gate` is green on the first run.

### M3 — `prove-fix` in the loop, a defect catalog for the reviewer, mutants on the sweep (2 days, 1 PR + 1 sweep change)

**What:** three things that turn "tests are green" into "tests prove the
change".

**Files:** `ci/REVIEW-DEFECT-CATALOG.md` (new; index row),
`swarm/reviewer.txt` (points at it — see M6 for who edits `swarm/`),
`swarm/builder.txt` + `swarm/core.txt` COMPLETION (name `prove-fix`),
`AGENTS.md` "Paperwork is part of the change" (one bullet: a corrective
change's PR carries a `prove-fix` line), `.github/pull_request_template.md`
(M8 adds the field; M3 adds one line under the checklist),
`.github/workflows/validation-nightly.yml` (the mutants step).

**Exactly:**

1. **Builder contract.** A corrective change (anything that would need a
   `validation/regressions.d/` entry) runs, and pastes into the PR:

   ```bash
   scripts/prove-fix origin/main '<test filter>' <production paths…>
   # → "prove-fix: <filter> FAILS at origin/main without <paths>; PASSES at HEAD"
   ```

   A `prove-fix` that cannot fail (the retained test passes without the
   fix) is a blocker for the author, before review.

2. **Reviewer catalog.** `ci/REVIEW-DEFECT-CATALOG.md` lists, with the
   PR number where each was found, the classes a reviewer checks for
   explicitly before reading for correctness:

   | Class | Repository example | What to run |
   |---|---|---|
   | Mutation survivor — a wrong implementation passes | #141 `+7s` final-EXTINF survives 362 tests; #137 wrong-child; #131 `duration_mismatch`; #84 9 of 17 | revert or negate the changed condition, rerun the filter |
   | Test pins a helper, not the call site | seven production lines reverted green (`feedback: tests must pin the call site`) | `prove-fix` with the *call-site* file as the path |
   | Runner ran nothing | the synchronous `test()` passing async bodies (cluster panel) ; web tests reading JS as text (#727/#729) | count started vs finished; grep the test for `await`/execution |
   | Assertion no mutation can fail | #794 paused-Activity count; #796 executor tests with the commit gate deleted | ask, per assertion, which mutation it catches |
   | Fixture makes the failure impossible | #830 "abandoned window publishes nothing" with no producer in the fixture | open the gate, rerun |
   | Source-grep assertion | #814 Apple substring broken by a reformat | prefer a behavioural pin; if a grep must stay, pin a token not a line |
   | Placeholder order in replicated SQL | #700 → #852; five `regressions.d` entries | M4 fence; until then, read every `$N` |
   | `Ok(_)` folding a refusal into "unreachable" | Activity fan-out; cluster-operations fan-out (#806) | grep the diff for `Ok(_)` arms on HTTP results |

   The reviewer prompt's REVIEW CONTRACT gains one line: "Check the
   catalog classes first; name the class in each finding."

3. **Mutants on the sweep.** The nightly step
   (`validation-nightly.yml:194–221`, `cargo mutants --timeout 300` over
   files changed in seven days) moves to the manually dispatched sweep and
   is scoped to *functions* changed since the sweep's last sha:

   ```bash
   git diff <last-sweep-sha>...HEAD > target/sweep.diff
   cargo mutants --timeout 300 --output target/mutants --in-diff target/sweep.diff
   ```

   Survivors are written to the job summary and posted as one comment on
   each merged PR whose diff they fall in (the PR number comes from the
   merge commit subject). Advisory: nothing turns red.

**Acceptance:** the catalog exists and every row's example resolves to a
PR or a doc in the repo; `swarm/reviewer.txt` references it; a corrective
PR opened after this milestone carries a `prove-fix` line (M0's
`review_findings{class=proof}` is the trend to watch over the following
four weeks); `cargo mutants --in-diff` runs on the sweep and its summary
lists survivors by function.

### M4 — Two fences (1 day, 1 PR)

**What:** build-time checks for the two mechanical classes that have
shipped repeatedly, in the `player-input-fence` shape.

**Files:** `scripts/sql-placeholder-fence` (new), `scripts/store-boundary-fence`
(new), two `[[checks]]` in `validation/points.toml`, the checks added to the
`cluster.auth` point (and whichever point owns `crates/plurx-core/src/cluster/**`),
both scripts added to a point's `paths`, `docs/VALIDATION.md` rows.

**Exactly:**

```python
#!/usr/bin/env python3
"""Every `$N` placeholder in a replicated statement appears in ascending
first-appearance order, because hiqlite binds by rusqlite's numeric index
while SQLite numbers `$N` by first appearance (see
`validate_parameter_order` in crates/plurx-core/src/store/hiqlite.rs)."""
# Scans: crates/plurx-core/src/store/hiqlite*.rs, crates/plurx-core/src/cluster/*.rs
# Extracts every Rust string literal (plain and raw, r#"…"#) containing `$` + digit,
# runs the SAME algorithm as validate_parameter_order (port it: quotes, [brackets],
# `--` and `/* */` comments skipped), and fails with file:line and the offending
# statement's first three placeholders when first appearance is not 1,2,3,…
# Allowlist: a literal preceded on the previous line by
#   // sql-placeholder-fence: allow — <reason>
```

```python
#!/usr/bin/env python3
"""The `Store` trait is the only durable-state boundary: `hiqlite::` and
`rusqlite::` are named only under crates/plurx-core/src/store/** and the
listed exceptions, so SQLite and clustered operation cannot diverge."""
# Scans every tracked *.rs outside crates/plurx-core/src/store/.
# Fails on `hiqlite::` or `rusqlite::` (use paths or qualified calls) except in:
#   crates/plurx-core/src/cluster/membership.rs   (raft membership needs the client)
#   crates/plurx-core/src/cluster/migration.rs
#   crates/plurx-core/src/error.rs                (error conversion)
#   crates/plurx-cluster-check/**                 (the harness drives hiqlite directly)
#   crates/plurx-core/tests/store_contract.rs, crates/plurxd/tests/cluster_activation.rs
#   crates/plurxd/src/logbuf.rs                   (a log target string, not a use)
# — the list is the census taken 2026-09-10; re-take it at build time with
#   git grep -lE '\b(rusqlite|hiqlite)::' -- 'crates/**/*.rs' | grep -v plurx-core/src/store/
# and confirm each survivor is a legitimate exception before writing it in.
```

Catalog entries mirror `player-input-fence` exactly (`profiles` all four,
`requires = ["python3"]`, `missing = "fail"`, `timeout_seconds = 30`).

**Acceptance:** each fence has a mutation proof in
`tests/validation/test_fences.py`: a temp copy of a store file with `$4`
before `$3` fails the placeholder fence with the right file:line; a temp
`.rs` outside `store/` containing `hiqlite::Client` fails the boundary
fence; both pass on `main`; each runs in under 10 s; `make validation-lint`
green with the new checks wired.

### M5 — Subsystem guides, prompt subtraction, `guide =` in the catalog (3 days, 2 PRs)

**What:** the local guidance Codex and Gemini both ask for, paid for by
removing the global always-read set from the prompts. PR 1 adds the guides
and the catalog field; PR 2 (with M6) changes the prompts.

**Files:** six guides, one per hotspot subsystem, beside the code:
`crates/plurxd/src/transcode/GUIDE.md`, `…/playback_control/GUIDE.md`,
`…/http/hls/GUIDE.md`, `…/http/GUIDE.md`, `…/state/GUIDE.md` (create the
directory if M2 did not), `crates/plurxd/src/web/GUIDE.md`; `guide =` on the
owning `[[points]]`; `validation/runner.py` (`Point` gains `guide: str | None`;
`lint` verifies the path exists); `docs/VALIDATION.md` (the field);
`docs/README.md` "Find it fast" (one row: "which guide for this file? →
`scripts/validate list`"). Each guide path is added to its point's `paths`
so `validation-lint` owns it.

**Exactly — the guide, ≤ 80 lines, this skeleton and nothing else:**

```markdown
# <Subsystem> — guide for changing it

**Owns:** one paragraph. What this subsystem is responsible for and what it
deliberately is not (e.g. transcode owns producer lifecycle and rung
selection; it does not own delivery — that is http/hls).

**Enters at:** the 3–8 functions or handlers where work arrives, as links:
`fn start_session` (transcode.rs:L), `POST /api/v1/…` (http/mod.rs:L).

**Invariants a change must keep:** numbered, each one sentence with the
reason, each with the test or fence that would catch a violation.
1. A prepared successor is STAGED, never primed — `stage_prepared_successor`
   only stages; the pointer moves on acknowledgement. Caught by
   `test_control_wire_conformance`.

**Copy this:** one or two links to live code that is the approved shape for
the common change here, with one line on where the pattern stops applying.

**Evidence:** the exact commands: `cargo test -p plurxd <filter>`,
`scripts/validate run --point <id>`, the fence(s), the sweep suite that
covers it.

**Functionality point:** `<id>` in validation/points.toml.
```

Generate nothing into the guide. When a fact in it is a line number, it is
a link the docs test can check; when it is an invariant, it names the test
that enforces it, so a guide cannot claim more than the suite does. The
M6-class facts from the assessment (§2 item 3) seed the transcode and
playback_control guides.

**Catalog field:**

```toml
[[points]]
id = "playback.control"
guide = "crates/plurxd/src/playback_control/GUIDE.md"   # new; lint: must exist
```

**Prompt subtraction (lands with M6):** in `swarm/builder.txt` and
`swarm/core.txt`, "Always read README.md, docs/ARCHITECTURE.md, and
docs/VALIDATION.md" becomes "Read `AGENTS.md`. Then run `scripts/validate
plan --paths <the issue's paths>` and read the `guide` of each point it
names. Read `docs/ARCHITECTURE.md` only when the issue crosses a subsystem
boundary; `docs/VALIDATION.md` only when adding a check or a point." The
same edit in the Codex/Claude root instructions if they restate it.

**Nested instruction files:** Codex reads `AGENTS.md` up the directory
chain from its launch directory, Claude Code reads nested `CLAUDE.md` files
when it touches files beneath them; neither is verified to load a file
beside `transcode.rs` in a real fleet run. Do **not** rely on it: the guide
is reached through the prompt and the catalog. A one-line
`crates/plurxd/src/CLAUDE.md` saying "read the GUIDE.md beside the file you
are changing" may be added after a real run shows it loads (Codex's
"verify effective instruction loading" point).

**Acceptance:** `scripts/validate lint` fails when a `guide` path does not
exist and passes on the branch; every guide is ≤ 80 lines and its
line-number links resolve (extend `test_docs_index` to guides under
`crates/`); the question "is the prepared handoff staged or primed?" is
answered by `playback_control/GUIDE.md` alone; M0's `tokens_per_task`
(where the harness records it) drops after PR 2 — record before and after
in `HARNESS-METRICS.md`.

### M6 — One source of truth for the process, with a test (1 day, 1 PR — Lead territory)

**What:** `swarm/` stops restating the merge policy and stops describing
GitHub; a test keeps it that way.

**Files:** `swarm/project.txt` (shrinks), `swarm/config.json`,
`swarm/builder.txt`, `swarm/core.txt`, `swarm/reviewer.txt`,
`swarm/lead.txt`, `tests/operations/test_swarm_prompts.py` (new),
this plan's §2.4 (update the facts).

**Exactly:**

- `swarm/project.txt` keeps: purpose and authority, the claim protocol,
  ownership, invariants, quality-and-completion. It **deletes** every
  sentence about the merge policy and replaces them with one paragraph:
  "The merge policy is `AGENTS.md` and `docs/DEVELOPMENT_PIPELINE.md`; read
  them, they are not restated here. In their terms: a task PR targets its
  `effort/<project>` branch or `main`; a main-bound PR opens as a draft
  with a `WIP:` title, gets exactly one adversarial review, addresses it,
  drops `WIP:`, takes `fast-lane`, and merges on a green `Main promotion
  gate`." It replaces `https://github.com/pjunod/plurx` with the Forgejo
  remote and deletes "`main` requires a pull request to be up to date
  before it merges" (Forgejo's "Update branch by merge" is the equivalent
  and `merge-change`'s empty-sync rule stays as written). "Never create a
  git worktree" stays.
- `swarm/config.json`: `project.quality_command` becomes
  `"scripts/agent-check --no-tests"` (the fast lane, locally — M1);
  `requirements.executables` drops `gh` and the `gh auth status` check
  unless the queue adapter still needs GitHub (if the fleet's issue queue
  is still on GitHub, say so in a `_note` key and leave it —
  this plan does not move the queue); `queue.adapter` is changed only if
  swarmdeck has a Forgejo adapter (check `~/code/swarmdeck` before editing;
  if not, flag and leave).
- Role prompts: the M5 read list; COMPLETION names `scripts/agent-check`
  and `scripts/prove-fix`; `reviewer.txt` names the M3 catalog.
- `tests/operations/test_swarm_prompts.py` asserts: no `github.com`,
  `gh auth`, or `gh pr` in any `swarm/*.txt`; `config.json`'s
  `quality_command` names a `scripts/` file or `Makefile` target that
  exists; every glob in a TERRITORY section matches at least one tracked
  file (a `git ls-files` census, so a fence like `docs/APPLE-*.md` that
  matches nothing fails); no `swarm/*.txt` contains the phrases "exactly
  one adversarial", "fast-lane", or "draft" except in the one
  paragraph that points at `AGENTS.md` (the policy has one home).

**Acceptance:** the new test fails on `main` before the edit and passes
after; `make operations-check` green; the fleet's next task PR shows the
new read list in its transcript (Lead confirms once).

### M7 — Status and changelog as per-PR fragments (2 days, 1 PR)

**What:** the two files every PR edits at the top become generated from
per-PR fragment files, so two PRs never conflict on them and the merge-lap
count drops.

**Files:** `status.d/` (new, repo root — beside `STATUS.md`),
`changelog.d/` (new), `scripts/fold-status` (new), `scripts/fold-changelog`
(new), `STATUS.md` and `CHANGELOG.md` (become generated, with a header
line saying so and the generator's name), `tests/operations/test_status_pr_claims.py`
(reads fragments as well as the pages), `tests/operations/test_fragments.py`
(new), `AGENTS.md` "Paperwork is part of the change" (the fragment replaces
the edit), `docs/RELEASING.md` (the fold at release), a `[[checks]]` for
`test_fragments` if it is not already covered by `operations-contract`.

**Exactly:**

```text
status.d/2026-09-10-fable-harness-plan.md
---
title: AI harness implementation plan
date: 2026-09-10
prs: [<n>]
state: open | built | merged | deployed
---
**Decided and written; nothing built.** <the section body exactly as it
would have gone under `## <title>` in STATUS.md>

changelog.d/<first-commit-prefix>-<slug>.md
---
section: Fixed | Added | Changed | Removed
---
- **<bold lead.>** <prose>
```

`scripts/fold-status` writes `STATUS.md` = header + fragments newest-first
by `date` then filename; `scripts/fold-changelog` writes the `[Unreleased]`
section from `changelog.d/`, grouped by `section`, and `make release`
(`docs/RELEASING.md`) moves the fragments into the version heading and
deletes them. Both folds are run by the fast lane's `preflight` (one
step: fold, then `git diff --exit-code STATUS.md CHANGELOG.md` — the PR
must have committed the folded result, so the rendered pages never lag; a
conflict on the rendered page is resolved by re-running the fold, never
by hand). `docs/STATUS.html` is generated by the same fold from the same
fragments (today it is hand-edited and churns on every PR — 329 commits).

**Acceptance:** two branches from the same base each adding a fragment
merge into `main` without conflict, in either order; `test_status_pr_claims`
still refuses a merged PR called open, reading the fragment; `preflight`
fails when a fragment is added without re-folding; M0's `merge_laps` is
re-measured two weeks after merge.

### M8 — PR template: change ledger, evidence block, suppression advisory (½ day, 1 PR)

**What:** the drift summary Codex and Gemini both want, at the size that
gets filled in.

**Files:** `.github/pull_request_template.md`, `.github/workflows/main-fast-lane.yml`
(one advisory step in `preflight`), `swarm/builder.txt` (M6 owns the edit).

**Exactly — added to the template above the promotion checklist:**

```markdown
## Change ledger
- **Reused:** <existing helpers, types, contracts this change calls; "none" is an answer>
- **Introduced:** <new abstractions, dependencies (Cargo.toml/package.json/build.gradle), new patterns — and one line on why an existing one did not fit>
- **Evidence:** <paste the `scripts/agent-check` block; for a corrective change add the `scripts/prove-fix` line; name any suite you did NOT run and why>
- **Open questions:** <decisions that are Paul's, or none>
```

The `Evidence` block is the task record Codex asks for (objective, base,
decisions, files, unresolved questions, compiler identity, source snapshot).
The `preflight` step:

```bash
git diff origin/${BASE_REF}...HEAD -U0 | grep -nE '^\+.*(#\[allow\(|eslint-disable|@Suppress\(|@SuppressLint|noqa)' \
  && echo "::notice::new lint suppressions above — reviewer, look at each" || true
```

Advisory only: it never fails the job (non-goal 1). Whether to make it a
finding is the reviewer's.

**Acceptance:** the template renders on a new PR; a PR that adds
`#[allow(dead_code)]` shows the notice in its `preflight` log and the job
stays green.

### M9 — Ripwire pilot, measured against `rust-analyzer scip` (2 days, 0 PRs to `main` until the result says so)

**What:** Codex's priority 1, run after M2 and M5 so it measures the
residual navigation cost. Ripwire is a local zero-dependency CLI/MCP with
name-based call edges (its README: dynamic dispatch, callbacks and
macro-generated call sites contribute no edge); Kotlin is not among its 16
languages; JS embedded in `index.html` is unverified.

**Files:** `ci/RIPWIRE-PILOT.md` (new, the result), nothing else on
`main` unless the pilot passes.

**Exactly:**

1. In a scratch clone at a pinned `main` sha, run `rust-analyzer scip .`
   (`rustup component add rust-analyzer` on the pinned 1.97.1 toolchain;
   install the `scip` CLI to query the index) and Ripwire's index over the workspace. Record
   versions and wall-clock.
2. For twenty symbols — ten chosen from `playback_control.rs` and
   `http/hls.rs` by highest fan-in in the SCIP index, ten from the M2 test
   files chosen at random — compare `callers`/references: precision and
   recall of Ripwire against SCIP as ground truth. Record the closure and
   macro call sites SCIP finds that Ripwire does not.
3. Run Ripwire over `crates/plurxd/src/web/index.html` and report whether
   the embedded JS yields symbols at all; run it over `clients/android`
   and report the (expected) absence.
4. Four representative tasks from the assessment §6.5, each run twice on
   the same sha by the same model, with and without the Ripwire MCP:
   a placeholder-order fix, a three-client control change, a `transcode.rs`
   change needing one function and its callers, and a corrective change
   with `prove-fix`. Record the Codex measures table: correct completion,
   missed consumers, human intervention, elapsed and cost, navigation
   reads, failure category.

**Acceptance:** `RIPWIRE-PILOT.md` carries the numbers and a one-paragraph
verdict; Ripwire enters `swarm/config.json` as an MCP for builders only if
correct completion is not worse and navigation reads or cost improved on
at least three of the four tasks. Otherwise the verdict records what
`rust-analyzer scip` gives for free and the pilot closes.

### M10 — First cohesive extraction from `transcode.rs`, chosen by co-change (3–5 days, 1 effort branch)

**What:** with tests out (M2) and callers known (M9's SCIP index), pick the
one responsibility whose functions change together and rarely with the
rest, and move it to its own module behind the interface it already has.

**Exactly:**

```bash
# co-change census on the post-M2 file: which functions are edited in the same
# commits? (function attribution from `git log -L :<fn>:crates/plurxd/src/transcode.rs`
# over the last 90 days, or from SCIP ranges intersected with `git log -p`)
scripts/cochange crates/plurxd/src/transcode.rs --since 90.days   # new script, this milestone
# → clusters of functions with their co-change ratio and their external callers
```

Choose the cluster with the highest internal co-change and the fewest
external callers; that is the seam. Move it to `crates/plurxd/src/transcode/<name>.rs`
with `pub(super)` items and no signature changes; `git diff --color-moved`
proves the move; the M5 guide gains an "Enters at" line for the new module.

**Acceptance:** `require-test-count --exact` unchanged; `agent-check` ok;
the new module's external callers equal the census count; the next four
weeks of `git log --name-only` show commits touching the new module
without touching `transcode.rs` (the extraction was real) — recorded in
`HARNESS-METRICS.md`.

## 5. Order and dependencies

```
M0 baseline ──▶ M1 agent-check ──▶ M2 tests out (4 PRs) ──▶ M9 Ripwire pilot ──▶ M10 extraction
                     │                                            ▲
                     ├──▶ M3 prove-fix + catalog + mutants        │
                     ├──▶ M4 fences                               │
                     └──▶ M5 guides ──▶ M6 swarm/ + prompts ──────┘
                                              │
                          M7 fragments ◀──────┤   (M7 and M8 are independent of
                          M8 PR template ◀────┘    each other; both after M6 so
                                                   the prompts change once)
```

M1 first because every later acceptance check says `agent-check ok`. M3,
M4 and M5 can run in parallel on separate branches by separate builders —
their file territories do not overlap (M3: docs + workflow; M4: scripts +
catalog checks; M5: guides + runner). M6 waits for M1, M3 and M5 because
it edits the prompts once with all three changes. M7 and M8 both touch the
template and the fast lane; sequence them. M9 needs M2 and M5 merged; M10
needs M9's index.

## 6. Measurement — what "it worked" means

Re-run `scripts/harness-metrics` every two weeks and append to
`ci/HARNESS-METRICS.md`. The numbers that decide the next investment:

| Counter | Baseline (M0) | Target after | Interpreting it |
|---|---|---|---|
| `fast_lane_failures{paperwork}` per week | fill at M0 | 0 after M1 + two weeks | any paperwork failure after M1 is an `agent-check` gap — fix the script, not the agent |
| `fast_lane_failures{compile}` per week | fill at M0 | 0 after M1 | same |
| `review_findings{class=proof}` per PR | fill at M0 | halved after M3 + four weeks | still high → the catalog is not being read, or `prove-fix` is not being run; check transcripts before adding process |
| `merge_laps` p90 | fill at M0 | ≤ 1 after M7 | laps that remain are real conflicts in code, which is fine |
| `tokens_per_task` (where recorded) | fill at M0 | down after M5/M6 | up → the guides were added without the subtraction |
| production lines in the four hotspots | 110,212 total | ≈ 52,000 after M2 | a mechanical fact, but it is the one that changes what an agent reads |
| Ripwire pilot table | — | M9 | see M9 acceptance |

A compact context that misses necessary work is not a saving (Codex);
correct completion stays the primary outcome on every row.

## 7. What this plan does not decide

Three things are Paul's to rule on when they come up, and the milestones
are written so they can be built without the answer:

1. Whether the fleet's issue queue moves from GitHub to Forgejo (M6 leaves
   `queue.adapter` alone unless swarmdeck already has the adapter).
2. Whether Ripwire is adopted (M9 recommends, Paul decides).
3. Whether the suppression notice (M8) ever becomes a finding class in the
   reviewer catalog rather than an advisory line.
