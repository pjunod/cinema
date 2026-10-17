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
| Forgejo | first job of the main fast lane, the effort lane, and `paperwork.yml` for pull requests into effort branches; drafts included | every other job: they `needs:` it, and the gates require it to be `success` |

The CI job is the guarantee; the hook only moves the failure earlier. A
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
| A new web file is registered everywhere; Developer cards graduate | `tests/web/asset-*.test.js`, `settings-sections.test.js` (only when web files changed) | file + `WEB_ASSETS` row + shell tag + [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) row, one commit |

The full corrective-history audit (`make history-check`, ~80 s) and the
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
