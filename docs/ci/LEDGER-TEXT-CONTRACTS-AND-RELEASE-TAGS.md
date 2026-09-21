# Regression ledger, text contracts and release tags — what the process buys, and what it costs

**Status:** ready for review · **Executes:** §4.4 / §4.5 / F-hist-8 / F-hist-9 /
F-build-9 / F-build-15 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [VALIDATION.md](../VALIDATION.md) (what the points and checks
mean), [RELEASING.md](../RELEASING.md) (the cut procedure this changes),
[OPERATIONS.md](../OPERATIONS.md) (the fleet registry and the `sha-` rollback
tags) and [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) (the
lane every check here runs in). Read review §4.4, §4.5 and §7.4/§7.7, then
assessment rows `4.4`, `4.5`, `F-hist-8`, `F-hist-9`,
`F-build-ops-codehealth-9` and `F-build-ops-codehealth-15`, then this
document. Board id **P-03**.

Work it in milestone order. M1 measures, M2–M4 replace the per-commit receipt
with a PR-level field **before** anything stops being written, M5 prunes text
contracts one at a time, M6 moves status prose, M7 restarts releases.
**Two decisions in §7 are Paul's** (the retirement authorisation and the
release cadence); this document recommends and does not decide.

The standing instruction: **if a step seems to require deleting a file under
`validation/regressions.d/`, weakening `tests/operations/test_docs_index.py`
or `tests/operations/test_status_pr_claims.py`, or changing what
`main-fast-lane.yml`'s `preflight` job gates, stop and flag it.** This plan
stops the ledger *growing*, keeps every existing receipt, and adds checks to
the lane rather than removing them.

---

**Correction to the review:** five, one of which changes a milestone.

1. **`publish_main` cannot fire on any trigger `ci.yml` declares.**
   `ci.yml:6-9` is `push: tags: ["v*"]` plus `workflow_dispatch`, and
   `publish_main`'s condition (`ci.yml:1471-1473`) is `github.event_name ==
   'push' && github.ref == 'refs/heads/main'`. A tag push has
   `refs/tags/v*`; a dispatch is not a push. So the `sha-<12hex>` image that
   [OPERATIONS.md](../OPERATIONS.md) §"The fleet registry" calls the product of
   "every successful Forgejo `main` run" has had **no producer at all** since
   `3cd127e2` (2026-09-10). §4.5's "keep the `sha-` receipts as the deploy
   identity" is therefore not a status quo to preserve — it has to be restored
   first, which is
   [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) §3.1(b)'s
   `publish_main` widening. M7 depends on it.
2. **There *is* a server-side release gate; it is the weekly cadence that
   does not exist.** §4.5 says "nothing equivalent exists for the server".
   `make release-check` (`Makefile:1584-1592`) refuses a dirty tree, an
   already-existing `v$(VERSION)` tag, a missing `## [$(VERSION)]` changelog
   section, and then runs `scripts/validate run --profile ci --all --strict`;
   `.github/workflows/release-readiness.yml` runs it. What is missing is the
   schedule: that workflow is `workflow_dispatch:` only, and
   `tests/operations/test_contracts.py:3081` **asserts** `schedule:` is absent
   from it. [RELEASING.md](../RELEASING.md):178 nevertheless calls it "the
   weekly release-readiness workflow". Option (a) in §3.5 is that sentence
   made true, and it changes that assertion in the same PR.
3. **The ledger's measured size differs from §4.4's numbers.** At `0f02b7ea`,
   with a history shallow since 2026-08-18: 3,954 non-merge commits; 1,015
   touching `validation/regressions.d/` of which **724 touch nothing else**;
   856 fragment files, 6,771 lines, **791 of them eight lines or fewer**, 139
   carrying `ignore = true`. §4.4's 1,240 / 502 are a different range. The
   conclusion is unchanged and the commands are in §2.1 so the next reader
   re-measures instead of quoting.
4. **Narrowing the corrective rule is not cost-free.** §4.4 says the
   assessor's `^fix(` rule gives "same conclusion". It gives a much smaller
   audited set: `ISSUE_RE` (`history.py:21-35`) matches **1,483** subjects,
   `^(fix|perf)[(:]` matches **938**, so **558** commits leave the audit —
   including `feat(scan): add bounded identity repair` and
   `ci: restore complete watch integration preflight`. That is the escape the
   assessment warns about, and §3.2 documents it rather than denying it.
5. **`RELEASING.md` still works `0.2.7` through its examples** while
   `Cargo.toml:21` is `0.3.0`. Cosmetic, but it is the procedure someone
   follows at 1 a.m.; M7 fixes it in passing.

---

## 1. Objective

Board id **P-03**. Three separate things, each with its own end state:

1. **The regression ledger stops growing** — a per-PR field bound to the
   merged tree replaces per-commit short-SHA receipts, and no receipt is ever
   deleted. Rebasing a branch stops producing re-mapping commits.
2. **The text-contract suite shrinks only where something stronger exists** —
   one test per PR, each with a demonstrated runtime replacement, with a named
   keep list that is never touched.
3. **Releases resume** — semantic tags with a `CHANGELOG` rollover, the
   `sha-<12hex>` image kept as deploy identity, and a written path from "this
   node reports build X" to "these changelog entries".

Done means: `make history-check` is green under the merge-trailer audit with
`validation/regressions.d/` frozen; `STATUS.md` is under 400 lines with every
retired section owned by a per-effort doc; a semantic tag exists on `main`
newer than `v0.3.0`; and [RELEASING.md](../RELEASING.md) describes what the
repository actually does.

---

## 2. Contract today

Re-verify at build time. Every number below is a command.

### 2.1 What the ledger costs, measured

```sh
git log --no-merges --format=%H | wc -l                      # 3954
git log --no-merges --format=%H -- validation/regressions.d | wc -l   # 1015
ls validation/regressions.d/*.toml | wc -l                   # 856
cat validation/regressions.d/*.toml | wc -l                  # 6771
grep -l 'ignore = true' validation/regressions.d/*.toml | wc -l       # 139
git log --no-merges --format=%s -- tests/operations | grep -cE '^fix' # 138
wc -l STATUS.md                                              # 2689
```

724 of those 1,015 commits change nothing but the ledger (each commit's
`diff-tree --name-only` is entirely under `validation/regressions.d/`). Eight
carry `remap`/`rename` in the subject — the rebase tax the fragment naming
rule (`history.py:151-160`: a file must be named after its first mapped
commit) makes unavoidable while the key is a short SHA.

### 2.2 How a commit becomes "corrective"

`validation/history.py:21-35`, `ISSUE_RE`. It matches a subject prefix set
(`^fix`, `^perf(`, `address`, `harden`, `route`, `scope`, …), a body-word set
(`keep`, `bound`, `remove`, `preserve`, `truth`, `free each`, …) and three
whole-subject forms. A `docs:` commit whose subject contains "keeps" is
corrective. `discover_issues` (`:399-445`) then collects each matching
commit's paths, the catalogue points those paths select, and whether the diff
added anything test-shaped (`TEST_ADDITION_RE`, `:36-40`).

`audit_history` (`:452-608`) is the gate. A corrective commit passes if it has
a functionality point **and** direct test evidence, or a
`validation/regressions.d/<sha>-<slug>.toml` entry naming points and checks
that are evidence for those points, or a `tests/client-fixes.toml` anchor.
Past `client_fixes.enforce_after`, a corrective commit touching `crates/`
**must** have an explicit mapping or anchor — direct test evidence is no
longer enough (`:583-596`).

The fragment format, one entry per file so two branches never collide
(`:227-233`):

```toml
version = 1

[[coverage]]
commits = ["0022fc43"]
points = ["validation.framework"]
checks = ["history-regressions"]
reason = "…"
```

`0022fc43-activity-ledger.toml` is the self-referential class §4.4 names: a
ledger-only commit mapped to the validation framework so that the check which
demanded it is satisfied by it.

### 2.3 Where the checks run

`main-fast-lane.yml:80-106`, the `preflight` job, on every non-draft PR into
`main`:

| Step | Command |
|---|---|
| Audit corrective-history evidence | `make history-check` → `scripts/history-audit` |
| Catalog and static contracts | `make validation-lint`; `tests/validation` |
| CI, deploy and shipping contracts | `make operations-check` → `tests/operations` |
| Shared player input contract | two Node tests |

That job is the merge-time hook this plan needs: the `pull_request` event
checks out the **merge candidate**, not the branch head, so a check running
there is already looking at the merged tree. M2 re-verifies that on a real run
before anything depends on it.

### 2.4 The text-contract surface

`tests/operations/test_contracts.py` is 3,579 lines and 64 tests in one class.
Its reads are 14 × `ci.yml`, 6 × `scripts/ui-baseline`, 6 × `Dockerfile`,
5 × `Makefile`. 209 commits have touched it. The review's example is real:

```python
self.assertEqual(dockerfile.count("&& apt-get clean"), 2)   # :1077
```

The two the assessment insists on keeping do something a link checker cannot.
`tests/operations/test_docs_index.py` enforces **inventory** — every tracked
Markdown file under `docs/` appears in `docs/README.md`, with five exempted
record directories (`:24`). `tests/operations/test_status_pr_claims.py`
enforces **status truth** — no status page pairs a merged `#N` with an
in-flight phrase, judged against `main`'s own landing commits in two shapes
(`Merge pull request #N` and `title (#N)`, `:87-90`), matched per joined
paragraph because the two real defects were line-wrapped (`:26-30`).

### 2.5 Release state

`Cargo.toml:21` is `0.3.0`; the newest tag is `v0.3.0` (2026-08-31);
`git describe --tags` reads `v0.3.0-2979-g0f02b7ea`; 2,237 non-merge commits
sit past the tag; `CHANGELOG.md`'s `[Unreleased]` is 340 lines. The build
stamp is `git describe --tags --always --dirty=-dirty`
(`crates/plurxd/build.rs:117-119`), surfaced on `GET /api/v1/server`,
`GET /api/v1/system` and `plurx_build_info{version,build}`
(`crates/plurxd/src/http/system.rs:5037-5039`). `scripts/registry-push:24-26`
builds `sha-${FULL_SHA:0:12}` and moves `:main`; the Forgejo cleanup rule
keeps the ten newest (OPERATIONS.md §"Rollback by digest-bearing tag"). Per
correction 1, nothing calls that script automatically.

---

## 3. Change

### 3.1 A PR-level regression field, bound to the merged tree

The field, repeatable, one test per line:

```text
Regression-Test: crates/plurxd/src/transcode.rs::the_producer_releases_on_late_arrival
```

`<path>::<name>` — a repository-relative path and a test function name. Not a
SHA, so a rebase cannot invalidate it.

**Two carriers, because they answer different questions.**

*Pre-merge*, the PR description carries it. The fast lane reads
`${{ github.event.pull_request.body }}` into an environment variable — payload
data, no API call and no credential — and `validation/regression_field.py`,
run from `preflight`, checks against the **checked-out merge candidate**:

1. every `<path>` exists in that tree (`Path.is_file`);
2. `<name>` occurs in that file adjacent to a test marker (`fn <name>` under
   `#[test]`/`#[tokio::test]`, `func test…`, `@Test`, `it(`/`test(` for the
   Node suites) — the same marker set `TEST_ADDITION_RE` already uses, lifted
   into one shared module so the two cannot drift;
3. `<name>` is in the set the fast lane actually executes. That set is derived
   from the lane's own step list, not hardcoded; **under today's compile-only
   Rust gate the only executable set is the Node and Python preflight suites**,
   so a Rust path is accepted with a printed warning until
   [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) M2 lands
   option (a) or (c). That dependency is named in §7, not hidden;
4. if the PR's commit range contains a corrective commit under §3.2's rule and
   the body carries no field, the job fails with the subject it objected to.

*At merge*, the landing commit carries the same line as a trailer. `main` is
never force-pushed, so a landing commit is immutable and so is its tree —
which is the exact-tree binding the assessment requires. `history.py` gains
`audit_merge_regressions`:

- collect landing commits on `main` past a boundary, using
  `test_status_pr_claims.py:87-90`'s two shapes so squashed PRs are covered as
  well as merge commits;
- for each, read `Regression-Test:` trailers with
  `git interpret-trailers --parse`;
- for each trailer, `git cat-file -e <landing>:<path>` and then check the name
  against `git show <landing>:<path>` — **the blob as it was at that merge**,
  not as it is today;
- a corrective landing commit with no trailer, or a trailer whose path or name
  does not resolve in its own tree, is an error.

**The ledger of merge commits → tests is generated, not tracked.**
`scripts/history-audit --report` already writes
`target/validation/history.json`; it gains a `merges` array of
`{landing, pull, tests[], resolved}`. Nothing new is committed per merge, which
is the whole point.

**One tracked exception file**, `validation/merge-errata.toml`, append-only,
one row per landing commit whose trailer is wrong: `{commit, reason}`. A
landing commit cannot be amended, so a typo that reaches `main` is permanent;
the pre-merge check in step 1–3 above exists precisely so that typos are
caught while they can still be fixed. Expect this file to stay near-empty; if
it does not, the pre-merge check is broken and that is the signal.

**Retirement, and why nothing is deleted.** The assessment's requirement —
"define durable exact-tree evidence and merge enforcement" before retiring
receipts — is honoured by never retiring one:

| Phase | `regressions.d/` | Merge trailers |
|---|---|---|
| A (M2–M3) | required, unchanged | checked, advisory |
| B (M4, Paul's call) | **frozen**: the loader refuses a new fragment whose lead commit is newer than the boundary | required past the boundary |
| C (steady state) | read-only evidence for the pre-boundary range, forever | the only evidence for the post-boundary range |

The directory stops growing. The 856 existing fragments keep auditing the
2,000-odd commits they were written for, because nothing else ever will.

### 3.2 Corrective = `fix(` / `perf(` prefix only

`ISSUE_RE` becomes `^(fix|perf)(\([^)]*\))?[:!]` — Conventional Commit
prefixes, nothing else. Measured at `0f02b7ea`: 1,483 subjects → 938.

**The accepted escape, stated plainly and documented in the tree.** 558
commits stop being audited. Some of them are genuinely corrective
(`feat(scan): add bounded identity repair` carries a repair;
`ci: restore complete watch integration preflight` restores a gate). A
behaviour fix mislabelled `chore(` or `refactor(` now passes the audit without
evidence. This is accepted because the alternative — a regex that matches
"keeps" — makes every prose commit a corrective commit and produced the
self-referential receipt class in §2.2. Three mitigations, each cheap:

- AGENTS.md's focused-regression rule gains one sentence: *if the change
  alters behaviour a user could observe, its subject is `fix(` or `perf(`.*
- `preflight` prints the PR title's prefix alongside the field check, so a
  reviewer sees `feat(` on a PR whose body describes a repair.
- The prefix set lives in one constant with the escape written above it, so
  the next person to widen it reads why it is narrow first.

### 3.3 Prune text contracts one at a time, each with a replacement

The protocol, one PR per test, no exceptions and no line target:

1. State the invariant the test encodes, in one sentence, in the PR body.
2. Build the replacement and prove it distinguishes the production change with
   `scripts/prove-fix` — the replacement fails when the production line is
   restored from a base revision, and the text contract fails the same way.
   Two checks failing on the same revert is the demonstration that they cover
   the same invariant.
3. Delete the text assertion in the same PR, naming the replacement.

**Never pruned** (security posture, plus the two the assessment names):
`test_external_actions_are_immutable_and_checkout_drops_credentials` (:180),
`test_live_tv_ffmpeg_clears_inherited_environment` (:202),
`test_private_origin_fetches_use_only_step_scoped_credentials` (:210),
`test_fleet_registry_variable_always_carries_its_fallback` (:3046),
`test_forgejo_workflows_do_not_declare_ignored_github_permissions` (:3142),
`test_ci_jobs_use_the_intended_runner_trust_boundary` (:3175),
`test_workflows_have_no_hosted_runner_escape` (:3448), and the whole of
`test_docs_index.py` and `test_status_pr_claims.py`.

**First candidate, and the only one this plan pre-authorises.**
`test_docker_build_frees_each_ffmpeg_download_before_the_next`'s final line,
`assertEqual(dockerfile.count("&& apt-get clean"), 2)` (:1077). The invariant
is "the shipped image carries no apt lists"; the replacement is an assertion
in `scripts/container-smoke` that `/var/lib/apt/lists` is empty in the built
image. The three ordering assertions above it stay until the smoke check also
proves the second download cannot be resident at the same time as the first.
Every later candidate is proposed in its own PR against this protocol.

### 3.4 Status prose out of `STATUS.md`

`STATUS.md` is 2,689 lines and 50 `##` sections. New shape:

- the newest ten sections stay in full;
- every older section moves **verbatim**, under a dated heading, into the
  subject folder's status document (most exist already — the `features/`,
  `streaming/`, `playback-control/` and `cluster/` folders each have one);
- `STATUS.md` keeps a one-line row per moved section: date, one sentence, and
  a link to the document that now owns it.

**The guard must follow the prose.** `test_status_pr_claims.py:42-46`'s
`STATUS_PAGES` is three entries. Every document that receives moved prose is
added to it in the same PR, or the check's reach shrinks silently while the
diff looks like a tidy-up — which is the failure mode the test's own docstring
describes. A new operations test asserts `STATUS.md` is under 400 lines and
that `STATUS_PAGES` contains every document `STATUS.md`'s index rows link to.

### 3.5 Release tags, changelog rollover, and node → changelog

**The decision is Paul's** (§7.2). Both options assume correction 1 is fixed
first, because both want `sha-` images to exist.

**(a) Weekly tag from a green scheduled run.** `release-readiness.yml` gains
`schedule: cron: '0 6 * * 1'`; `test_contracts.py:3081`'s
`assertNotIn("schedule:", readiness)` becomes an assertion of that cadence in
the same PR. A green run is the evidence for a tag; the tag then fires
`ci.yml`'s full sweep once a week instead of once a deploy.

**(b) A `v0.3.x` tag per fleet deploy.** Honest about cost: every deploy pays
one full `ci.yml` sweep including the cluster lanes.

**Either way**, `scripts/release-cut` (new, Python, tested in
`tests/operations`) does the mechanical half and refuses the unsafe half:

1. refuse an empty `[Unreleased]`;
2. rewrite `## [Unreleased]` to `## [Unreleased]` + `## [X.Y.Z] — <date>` and
   append the two link definitions `RELEASING.md` step 3 asks for;
3. bump `Cargo.toml`'s `[workspace.package] version` and run `cargo build` so
   `Cargo.lock` follows;
4. run `make release-check` and print its `git tag -a` line rather than
   running it — tagging stays a human act, as RELEASING.md step 5 requires.

**`sha-<12hex>` stays the deploy identity.** Nothing in `deploy/.env`,
`make docker-image-up` or the rollback procedure changes. A release tag is a
communication artefact; the immutable image is the rollback artefact. Both
exist, and OPERATIONS.md says which answers which question.

**Node → changelog.** `GET /api/v1/server` already returns
`{version, build}` with `build` from `git describe`. Once tags resume, a node
reports `v0.3.4-12-gabc1234`: the prefix names the `CHANGELOG.md` heading to
read, and a nonzero commit count means also reading `[Unreleased]`.
OPERATIONS.md gains that as a three-line procedure beside the existing
`curl … | jq '{version, build}'`, and `plurx_build_info{version,build}` is
named as the fleet-wide form of the same question.

---

## 4. Guardrails (non-goals)

- **Do not delete a `regressions.d` fragment.** §3.1's phase table freezes the
  directory; the pre-boundary range has no other evidence. A deletion is a
  silent loss of the audit's history.
- **Do not retire per-commit receipts before the trailer audit is green on a
  real merge.** The assessment's condition, and M3's acceptance is exactly it.
- **Do not replace `test_docs_index.py` or `test_status_pr_claims.py` with a
  Markdown link checker.** A link checker proves neither inventory nor status
  truth; §2.4 says what each enforces and §3.4 extends the second rather than
  narrowing it.
- **Do not prune a text contract to a line-count target.** One test, one PR,
  one demonstrated replacement (§3.3). "The suite is too big" is not a reason.
- **Do not touch the keep list in §3.3.** Those encode security and
  cross-file policy that no runtime test observes.
- **Do not treat production counters as the replacement for a regression
  test.** They supplement (review §5.3, assessment row 4.4); a counter reports
  the field, a test refuses the merge.
- **Do not add a `push: branches: [main]` trigger** to make `publish_main`
  reachable. That is the ruling DEVELOPMENT_PIPELINE.md records; the bounded
  alternative is the schedule in
  [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) §3.1(b).
- **Do not make a release tag the deployment qualification.** The `sha-` image
  with its startup-budget proof remains what a voter runs (assessment row
  F-build-ops-codehealth-15).
- **Do not move status prose into a document the status guard does not
  read.** §3.4's second paragraph.
- **No in-code feature gate anywhere in this plan.** Nothing here needs a
  switch; if a step seems to, it is a settings key surfaced in
  Settings → Developer, and that is a flag to raise, not a decision to take.

---

## 5. Milestones

One PR per milestone, draft into `main` under the fast lane, except M5 which
is one PR per pruned test.

### 5.1 M1 — measure, change nothing

Run §2.1's commands on `main` and record the output under a dated heading in
§2.1. Add two: the wall-clock of `make history-check` on the `general` runner
(it walks the whole history on every PR), and the count of landing commits on
`main` whose subject matches `^(fix|perf)[(:]` in the last 30 days — the
volume the trailer requirement will actually carry.

Acceptance: §2.1 carries a second table with those two numbers and a Forgejo
run URL for the timing.

### 5.2 M2 — the field, its checker, advisory in the lane

`validation/regression_field.py` per §3.1 steps 1–4, wired into
`main-fast-lane.yml`'s `preflight` with `continue-on-error: true`. In the same
PR, re-verify on a real run that the `pull_request` checkout is the merge
candidate: the step prints `git rev-parse HEAD` and its two parents, and the
PR body records that they are the branch head and `main`'s tip.

Acceptance: `python3 -m validation.regression_field` with a body naming a real
`<path>::<name>` exits 0; with a misspelt name it exits 1 naming the file it
searched; with a path absent from the tree it exits 1; a run URL shows the
merge-candidate parents.

### 5.3 M3 — the merge-trailer audit

`audit_merge_regressions` in `validation/history.py` per §3.1,
`validation/merge-errata.toml` (empty, with a header comment stating that a
row is permanent), the `merges` array in the audit report, and the boundary
setting. Advisory for this PR: report, do not fail.

Acceptance: `make history-check` green; `scripts/history-audit --report
target/validation/history.json` and `jq '.merges | length'` returns the number
of landing commits past the boundary; a synthetic landing commit in a fixture
repository with a trailer naming a deleted path is reported as an error;
`python3 -m unittest discover -s tests/validation` green.

### 5.4 M4 — narrow the rule, freeze the directory (**Paul's authorisation**)

`ISSUE_RE` → `^(fix|perf)(\([^)]*\))?[:!]` with §3.2's comment above it;
`load_coverage` refuses a fragment whose lead commit is not an ancestor of the
boundary; the trailer audit becomes an error rather than a report; AGENTS.md
and [VALIDATION.md](../VALIDATION.md) gain the subject-prefix rule and the
escape.

Acceptance: `make history-check` green on `main`; `scripts/history-audit`
prints a corrective-commit count within 5 of 938; adding a new fragment file
for a post-boundary commit fails with the message naming the trailer instead;
removing a trailer from a fixture landing commit fails the audit; a full week
passes with no `remap` commit in `git log --format=%s -- validation/`.

### 5.5 M5 — first text-contract prune

`scripts/container-smoke` gains the apt-lists assertion; `test_contracts.py`
loses line 1077 only. The PR body carries the two `scripts/prove-fix`
transcripts §3.3 step 2 requires.

Acceptance: `make operations-check` green; `scripts/container-smoke` fails on
an image built from a `Dockerfile` with the second `apt-get clean` removed,
and the PR body shows it failing; `git diff --stat` touches exactly two files.

### 5.6 M6 — `STATUS.md` split

Move every section older than the newest ten, add the index rows, extend
`STATUS_PAGES`, add the two new assertions.

Acceptance: `wc -l STATUS.md` under 400; `make operations-check` green
including the new length and coverage assertions; `python3 -m unittest
tests.operations.test_status_pr_claims` green with `STATUS_PAGES` listing
every linked target; `python3 -m unittest tests.operations.test_docs_index`
green (every moved document is indexed).

### 5.7 M7 — releases resume (**Paul's cadence decision**)

`scripts/release-cut` and its tests; the `release-readiness.yml` schedule and
the `test_contracts.py:3081` update if option (a);
[RELEASING.md](../RELEASING.md) corrected to `0.3.x` throughout with the
chosen cadence written into "Cutting a release"; OPERATIONS.md's node →
changelog procedure; the first tag cut.

Acceptance: `make release-check` green on the release PR's merged commit;
`git describe --tags` on `main` reads `v0.3.1` with no commit suffix at the
tag; `curl -s <node>/api/v1/server | jq '{version, build}'` on a deployed node
returns a `build` whose tag prefix names a dated `CHANGELOG.md` heading;
`grep -c '^## \[' CHANGELOG.md` is one higher than before;
`docker buildx imagetools inspect forge.lan:3000/noirr/plurxd:0.3.1` resolves
to a two-platform index.

---

## 6. Verification and rollout

Python gate for M2–M7: `make operations-check` plus
`python3 -m unittest discover -s tests/validation -p 'test_*.py'`. History
gate for M3–M4: `make history-check`. No Rust changes anywhere in this plan,
so no Rust gate beyond what the lane already runs; no device or client change,
so no GPT prompt for M1–M6.

M7 is the exception. The fleet half cannot be proved from here:

> **GPT prompt (M7, fleet).** On media1 and lab1–lab6, after the `v0.3.1` tag
> has published: for each node run
> `curl -fsS http://127.0.0.1:32400/api/v1/server | jq '{version, build}'` and
> `docker inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' $(docker compose -f /opt/noirr/plurx/deploy/docker-compose.yml ps -q plurxd)`.
> Report, per node: the reported `version`, the reported `build`, the image
> revision label, and the `PLURX_IMAGE` line from that node's `deploy/.env`.
> Do not change any node. The question is whether `build`'s tag prefix names
> the changelog section a reader would need, and whether every node's image
> label matches the revision its `build` string claims.

Rollout order: M1 → M2 → M3 → **Paul's §7.1 decision** → M4, then M5 and M6 in
either order, then **Paul's §7.2 decision** → M7. M4 is the only irreversible
step (a frozen directory is a policy other branches immediately depend on);
M7 depends on `publish_main` having a trigger again, which belongs to
[RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) M2.

If Paul declines §7.1, M2, M3, M5, M6 and M7 still land: the trailer audit
runs as a report beside the existing receipts, and the only loss is that the
ledger keeps growing.

---

## 7. Open questions

1. **Retirement authorisation (Paul).** Freeze `validation/regressions.d/` at
   a boundary commit and narrow `ISSUE_RE` to `^(fix|perf)`, accepting the 558
   subjects that leave the audit (§3.2)? M4 does not start without a dated
   answer here.
2. **Release cadence (Paul), review §7.7.** Weekly tags from a green scheduled
   run, or a tag per fleet deploy accepting the full tag CI (§3.5)? Recorded
   here with the date when made; until then `v0.3.0` stands.
3. **Whether a Rust path may be named in the field before the fast lane runs
   Rust tests.** §3.1 step 3 accepts it with a warning. If
   [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) §7.1 is
   answered "keep compile-only", the field's third condition can never be
   satisfied for a Rust test and the warning becomes permanent — which is
   worth knowing before M2 rather than after.
4. **The boundary commit for M4.** The first landing commit after M3 merges is
   the obvious choice; an earlier one would require back-filling trailers onto
   commits that cannot be amended, so it is not one.
5. **How many landing commits per week actually carry a corrective change.**
   M1 measures it. If it is under ten, the trailer costs nothing; if it is
   over fifty, the pre-merge check's failure mode matters more than this plan
   assumes and step 4 of §3.1 should start advisory.

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
