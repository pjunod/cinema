# Contributor workflow

The repository is private because trusted self-hosted runners execute its
workflows. GitHub branch protection is unavailable on the current account
plan, so every contributor and coding agent must enforce the merge convention
below.

## Working efficiently — read this first

The tree is over a million lines and the docs run to 19 MB, so how you look
decides what a session costs. Every agent working here, Claude or GPT:

- **Navigate with Ripwire first.** `scripts/ripwire find '<words>'` returns
  about a dozen candidate signatures with paths and lines (~4 KB);
  `scripts/ripwire body '<file>:<symbol>'` prints one function;
  `callers` / `impact` show who depends on it; `pr-context --base origin/main`
  maps a diff for review (it refuses a dirty tree — commit or stash first).
  Its first stderr line is a JSON receipt; refusals also arrive on stderr with
  a nonzero exit, so don't discard stderr wholesale.
  `make hooks` installs it (`scripts/ripwire setup` does it alone). Its graph
  is heuristic — an empty answer does not prove absence — so confirm in
  source before you conclude. [docs/ci/RIPWIRE.md](docs/ci/RIPWIRE.md).
- **Then search, then outline, then read.** `rg -l <pattern>`, then
  `rg -n -m 5 --max-columns 200 <pattern> <file>`; the root `.ignore` keeps
  evidence, archives, vendored and minified files out (`--no-ignore-dot` to
  include them). `scripts/outline <file>` (or `--top`) lists declarations with
  line numbers; read only the range you need. Several source files exceed
  10,000 lines — never read one whole.
- **Docs: read the head, not the body.** A plan or status document's opening
  section says where it stands; read past it only for the part you need.
- **Keep command output small.** `git diff --stat` before `git diff`,
  `git log --oneline -n 20`, `| head -c 4000` on anything unfamiliar, and
  `scripts/quiet <command>` for builds, test suites, ansible and gradle — it
  keeps the full log under `target/quiet/` and prints only failures and the tail.
  For a red CI run, `scripts/ci-failures [<sha> | --pr N]` prints just the
  failing jobs' failure lines instead of whole job logs; `--wait SECONDS`
  waits quietly for running jobs and prints only the final state.
- **Append without reading.** `STATUS.md` and the project backlog carry an
  `<!-- … insert … -->` anchor line; add new entries just below it with an
  edit instead of reading the whole file first. In `CHANGELOG.md` read only the
  `## [Unreleased]` section (`sed -n '/^## \[Unreleased\]/,/^## \[[0-9]/p'`)
  and add to its matching `###` list — an entry outside that section is never
  released.
- **Delegate exploration** that spans many files to a sub-agent that returns a
  conclusion, and start a fresh session per pull request rather than carrying
  one long context through several. Claude sessions run exploration and
  mechanical sub-agents on Sonnet or Haiku and keep Opus or Fable for design
  and adversarial review.
- **Review the diff, not the repo.** An adversarial review gets the diff, the
  touched files' outlines and the relevant plan's head.

## Before you push: paperwork

`make paperwork` (`scripts/paperwork`) checks, in about ten seconds, the
bookkeeping rules CI enforces — doc index rows, Apple/Android release
counters, infra names, `docs/API.md` routes, status-page shape, and the
`Regression-Test:` lines a `fix(`/`perf(` commit needs in the pull request
description — and prints the fix under each failure. `make hooks` runs it on
every push. Forgejo runs it as the first job of every pull request, drafts
included, and nothing else starts until it passes, so skipping it only costs
you the wait. [docs/ci/PAPERWORK.md](docs/ci/PAPERWORK.md) lists the rules.

## Where the documents are

[docs/README.md](docs/README.md) is a short router: the reference documents
that describe plurx as it is today, and one row per subject folder. Each
folder keeps its own index, `docs/<area>/README.md` (`playback-control/`,
`streaming/`, `server/`, `cluster/`, `clients/`, `performance/`, `ci/`,
`features/`, `reviews/`, `archive/`), saying what question each document
answers and whether it is live, open, built, or done. Read the router, then
only the folder index for the area you are working in — never list `docs/`.

A document you add or move belongs in the subject folder for its work and
needs a row on that folder's index **in the same commit**;
`tests/operations/test_docs_index.py` fails the build otherwise, and the same
test refuses any reference in the repo to a `docs/` path that does not exist.

Substantial proposals and unfinished projects also need a row in
[the project backlog](docs/features/PROJECT-BACKLOG.md). Update that row with
the canonical project ledger when work is claimed, built, qualified, deferred
or superseded; distinguish missing implementation from missing acceptance.

## Engineering knowledge pilot

For the six selected routing rules and the Python receipt boundary, use the
[repository knowledge skill](.agents/skills/engineering-knowledge/SKILL.md).
Start investigations with `scripts/knowledge context --path <file>`; changes
and reviews also use `scripts/knowledge obligations --base origin/main --worktree`.
Inspect returned coverage, source identity and unresolved claims. Maintain
source-backed decisions and obligations in the same change. The
[pilot status](docs/features/ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md) records
actual implementation and evidence; an empty answer proves no absence.
PR acknowledgment remains advisory during the observation window, while
invalid supplied IDs fail. This is scoped engineering metadata, not a blanket
requirement to link every file or a substitute for existing receipt validators.

## Where the web app is

`crates/plurxd/src/web/index.html` is a 97-line shell of markup and tags. The
app itself is the seventy-three files
[docs/clients/WEB-SHELL-LAYOUT.md](docs/clients/WEB-SHELL-LAYOUT.md) maps —
open that before grepping the shell for a function that is not in it. Adding a
file means the file, a row in `WEB_ASSETS`, a tag in the shell and a row in
that table, all in one commit; three tests fail otherwise. They are plain
scripts in one global scope, so served order is load order, and `export` /
`import` / `module.exports` do not belong in any of them.

## The public mirror names no real infrastructure

The repository is pushed to a public mirror, so hosts, LAN addresses, home
directories and SSH users are written with neutral names: `media1`,
`lab1`–`lab6`, `maca`/`macb`, `nas`, `jump1`, `10.42.x.y`, `forge.lan:3000`,
`~` and `operator@`. `scripts/scrub-infra-names` holds the mapping and its context
rules; `--write` rewrites the tree, `--check` lists what is left, and
`tests/operations/test_infra_names.py` runs the check in
`make operations-check`. A literal that truly must stay goes in
`scripts/scrub-infra-names.allow.toml` with its reason. Tools that need the
real lab take it from a git-ignored `scripts/*.fleet.json` (or a path given
on the command line) shaped like the committed `*.fleet.example.json`.

## Optional features and the Developer tab

There are no feature gates. An optional or unfinished feature gets an
explicit switch in Settings → Developer **only where manual enable/disable
has a meaningful purpose** (Paul clarified 2026-09-30). Advisory readiness
never disables the switch, rejects its Save, or overrides the saved choice.
Internal correctness fixes and automatic infrastructure do not acquire
gratuitous toggles merely because implementation or acceptance is unfinished;
their Developer entries can explain read-only facts and remaining evidence.
Developer is where a feature waits while it is not fully active or not fully
tested, and each card says what it is waiting on (`devGraduation` in
`web/pages/settings-developer.js`; `tests/web/settings-sections.test.js`
enforces it). When the feature is done it leaves Developer — to its proper
settings section if a permanent on/off makes sense, otherwise the advisory
card (and any temporary toggle) is removed and the feature is simply on.
The rule and the current audit are in
[SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md](docs/features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md#developer-lifecycle--every-card-graduates).

## Rust compile loop — do not use CI as a compiler

At the start of any session that may change Rust, establish a working compiler
loop before editing. A branch pushed to discover whether it builds costs
fifteen to forty minutes and reports one error; checked locally it reports all
of them at once. If the clone and `cargo` are on different machines — the usual
shape for an agent here — use the source-only loop in
[docs/ci/AGENT-COMPILE-LOOP.md](docs/ci/AGENT-COMPILE-LOOP.md) and
[DEVELOPMENT_PIPELINE.md §4](docs/DEVELOPMENT_PIPELINE.md#4-compile-before-ci-send-source-to-the-compiler-never-credentials).

- Archive committed source with `git archive`; transfer neither `.git` nor a
  repository credential to the toolchain host.
- Keep its `target/` warm and run check, clippy `-D warnings`, rustfmt and the
  focused tests the change needs before pushing.
- After porting the patch to the current intended base, archive that exact
  branch and run the loop again; results against an older snapshot are not
  evidence for a branch whose base moved.
- Verify `rustc --version` (pinned 1.97.1) rather than trusting the default
  `cargo`. Remove stale source extractions when disk gets tight.

## Large efforts

- Integrate a multi-task project on one temporary `effort/<project>` branch.
- Base each reviewable task branch on the current effort and open its pull
  request back into that effort.
- Dispatch `Effort development gate` by hand before merging a task into the
  effort, and do not merge on a red run. `.github/workflows/effort-ci.yml`
  runs only on `workflow_dispatch`: nothing fires it on a task pull request,
  so it gates a merge only when someone runs it first. It proves policy,
  formatting, static web contracts, and affected Rust/Apple/Android
  compilation; it does not run the Rust, Apple or Android unit suites and
  does not make the branch releasable.
- Run the smallest focused regression for changed behavior locally and record
  that command in the task pull request. The effort workflow deliberately
  defers the full suites.
- **If the change alters behavior a user could observe, its subject is `fix(`
  or `perf(`.** The corrective-history audit narrows to those two prefixes past
  its boundary commit, so a behavior fix labelled `chore(` or `refactor(` is
  not audited at all and its regression is not recorded anywhere. That is an
  accepted escape and this rule is what answers it; see
  [VALIDATION.md](docs/VALIDATION.md) "The boundary, and what replaces a
  fragment past it".
- Name the regression in the pull request description, one per line:
  `Regression-Test: <path>::<test name>`. The fast lane checks it against the
  tree the merge will produce. Whoever merges puts the same lines in the
  landing commit's message — `python3 -m validation.regression_field
  --body-file <description> --landing-lines` prints them — because once
  `validation/merge-errata.toml` sets its boundary, `make history-check`
  reads them from the landing commit, and a corrective landing without them
  turns every later pull request red until an errata row records it.
  `.forgejo/default_merge_message/MERGE_TEMPLATE.md` prefills Forgejo's web
  merge form with the description, so a web merge left as prefilled carries
  them. The API does not: `POST …/pulls/<N>/merge` without
  `MergeMessageField` lands the title line alone (Forgejo drops the
  template's body on that path), so an API merge must pass the lines in
  `MergeMessageField`.

- A focused `plurx-core` regression that covers replicated storage must use
  `make unit-core` or pass `--features hiqlite-store`. Bare
  `cargo test -p plurx-core --lib` is not evidence for `store/hiqlite*` code.
- Commit normally on every branch. The tracked hook runs only catalog lint,
  Rust formatting and Clippy, and embedded JavaScript syntax; it does not run
  tests or compile-only effort evidence. Run the smallest focused regression
  and the affected compile checks before pushing.
- When the effort is complete, freeze task merges, merge current `main` into
  the effort, and open `effort/<project>` into `main`.
- Merge only after `Main promotion gate` passes on the current candidate and
  the qualification receipt exists. If `main` or the effort moves, qualify the
  new tree again.

**The one bounded exception.** A multi-task project may branch each task from
`main` and merge it there instead, when its implementation plan says so *and*
names the file ownership per task, because an effort branch buys serialised
integration and there is nothing to serialise when no two tasks can touch the
same file. Everything else in this section still applies to such a project —
focused regression per task, the tracked hook, the gate — and the plan carries
the ownership table that makes the exception safe. The playback surface
contract ran this way through eleven task pull requests
([§8 and §9 of its plan](docs/clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md));
ruled 2026-09-13, and recorded here rather than left as two documents that
disagree.

Ordinary independent changes may continue to target `main` and use its
affected-surface validation. The complete commands and rationale live in
[docs/DEVELOPMENT_PIPELINE.md](docs/DEVELOPMENT_PIPELINE.md).
