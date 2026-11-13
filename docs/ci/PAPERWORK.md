# Paperwork — the bookkeeping rules, checked first

**Status:** live. Owner: `scripts/paperwork`. Added 2026-10-17.

Most CI failures on a pull request here are not about code. A doc was added
without its index row, a client change shipped without a release-counter
bump, a real host name slipped into the tree, a `fix(` commit had no
`Regression-Test:` line. Each is a rule with a test behind it, and each used
to be learned by failing a lane 15–40 minutes after pushing — after the
compile fan-out had already been spent.

`scripts/paperwork` runs those rules in about ten seconds, prints the fix
beside every failure, and runs in three places so nobody has to remember it:

| Where | How it runs | When it blocks |
|---|---|---|
| Your checkout | `make paperwork` | never — it reports |
| Your push | pre-push hook from `make hooks` | the push (bypass: `git push --no-verify`) |
| Forgejo | first job of the main fast lane, the effort lane, and `paperwork.yml` for pull requests into effort branches; drafts included | every other job: they `needs:` it, and the gates require it to be `success`. Derivable failures are fixed on the branch instead (below) |

The CI job is the guarantee; the hook only moves the failure earlier.

## What fixes itself

Some rules have exactly one correct answer, so the lane writes it instead of
failing. When paperwork fails in the fast lane or in `paperwork.yml`, the job
runs `scripts/paperwork-fix --ci`, and locally `make paperwork-fix` does the
same to your working tree without committing:

| Rule | The fix it applies |
|---|---|
| Apple release counter | `make apple-build-bump` above the merge target (every generated surface, the branch's fragments) |
| Android release counter | `versionCode` one above the merge target's, and the README status line's build |
| Real host names | `scripts/scrub-infra-names --write`, kept only in Markdown files |
| Missing index row | a row in `docs/<area>/README.md` with the doc's `# ` title and its `**Status:**` word (`open` if none); improve the wording later if you like |

In the lane the fix is committed as `chore(paperwork): apply derivable
bookkeeping fixes` with a `Paperwork-Fix: <run URL>` trailer, paperwork runs
again on it, and the commit is pushed to the PR branch. Guards:

- every changed file is Markdown, or a release file whose changed lines differ
  only in numbers; nothing is added or deleted, otherwise nothing is committed;
- a head that is already a `Paperwork-Fix:` commit is never fixed again, so a
  fix that does not converge fails instead of looping;
- only same-repository PRs, and only when the checkout is the event's head;
- the push (`scripts/paperwork-fix --push`) is leased to the head the run
  checked: if the branch moved at all meanwhile (your new commit, or a
  force-push that dropped one), it is refused and your push's run takes over;
  it goes only to an ordinary work branch, never `main`, the base or a
  `refs/…` head, and its token reaches git through the environment, never a
  command line, with hooks off.

The push uses the run's own token, which starts no workflow; the job then
fails saying the fix is on the branch and any push runs the lane on it.
`git pull --rebase` before your next push.

Do not expose a persistent `PAPERWORK_PUSH_TOKEN` to these workflows: they
execute PR-controlled scripts and can be edited by the PR. Seamless publication
requires a dedicated repository-write bot behind a separately trusted publisher,
with the credential outside this repository's PR Actions secrets. That publisher
must independently verify the generated bookkeeping edits, exact parent and
current open same-repository PR head before a leased push, reject protected and
train branches, and prove that exactly one replacement lane starts. This trusted
publisher is not implemented here; adding a secret alone is not a safe setup.

Rules that need a decision (a `Regression-Test:` line, a functionality point,
a doc split, a status move) still fail, with the fix printed under them. A
session that never ran `make hooks` still meets the rules within a minute of
opening its pull request, before any compiler job is allocated.

## The rules

`scripts/paperwork --list` prints this table from the script itself.

| Rule | Checked by | The fix |
|---|---|---|
| Every governed file maps to a functionality point | `scripts/validate lint` | map the file in `validation/points.toml` |
| A `fix(`/`perf(` commit carries `Regression-Test: <path>::<test>` in the PR description | `validation.regression_field` (locally a reminder; in CI against the description) | add the line; whoever merges copies it into the landing commit (`--landing-lines`) |
| Client changes raise the Apple and Android release counters | `validation.mobile_versions` (changed-from the merge base) | `make apple-build-bump`; raise Android `versionCode` and its README status line |
| Every doc under `docs/` has an index row; no reference names a missing `docs/` path | `test_docs_index` | add the row to the doc's folder index in the same commit |
| The tree names no real host, address, home or SSH user | `test_infra_names` | `scripts/scrub-infra-names --write`, or an allow-list row with its reason |
| `docs/API.md` lists every route | `test_api_doc_routes` | add or delete the route entry |
| Build claims are generated, not hand-edited | `test_apple_build_claims`, `test_mobile_build_claims` | `make apple-build-bump`; a user-visible change also gets a `docs/apple-builds/` fragment |
| `STATUS.md` stays short and indexes what moved out | `test_status_page_shape` | move the oldest sections per [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md) §3.4 |
| No status page calls a merged PR open | `test_status_pr_claims` | update the quoted prose |
| Doc `**Status:**` headers make possible claims | `test_doc_status_audit` | correct the header |
| Every Node web test is in a lane | `test_web_test_inventory` | add the file to `make web-unit-check` |
| Every `scripts/` Python file parses | `test_script_syntax` | fix the syntax error |
| Docs stay under 40 KB; the shrink-only list only shrinks (running `*_LOG.md` logs, STATUS-HISTORY files, the project backlog and tool-maintained docs are outside it) | `test_doc_size_budget` | move a status doc's dated log into a sibling `*_LOG.md`, or split the doc; never add to the allowlist |
| A new web file is registered everywhere; Developer cards graduate | `tests/web/asset-*.test.js`, `settings-sections.test.js` (only when web files changed) | file + `WEB_ASSETS` row + shell tag + [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) row, one commit |

The corrective-history audit (`make history-check`; in the PR lane it fails
only on the PR's own gaps, [CI-THROUGHPUT.md](CI-THROUGHPUT.md) §2a) and the
operations suite (~110 s) stay in the preflight job. Paperwork is the subset
that fails for a missing piece of bookkeeping rather than a behavior.

## Batching is unaffected

Paperwork checks the range from the merge base to the head, so a pull request
that batches many commits to run the unit suites once is checked once, and a
corrective commit anywhere in it is listed by name.

## Adding a rule

A new bookkeeping rule goes in `TEST_RULES` in `scripts/paperwork` with its
one-line fix, and in the table above. Keep the whole run under about fifteen
seconds: a slow check belongs in preflight, not here.
