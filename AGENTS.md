# Contributor workflow

The repository is private because trusted self-hosted runners execute its
workflows. GitHub branch protection is unavailable on the current account
plan, so every contributor and coding agent must enforce the merge convention
below.

## Where the documents are

[docs/README.md](docs/README.md) indexes every document under `docs/`: what
question each one answers, and whether it is live, open, built, or done. Read
it instead of listing the directory — the root holds the eighteen maintained
reference documents, and everything else lives in the folder for the work it
describes (`playback-control/`, `streaming/`, `server/`, `cluster/`, `clients/`,
`performance/`, `ci/`, `features/`, `reviews/`, `archive/`).

A document you add or move belongs in the subject folder for its work and
needs a row on that index **in the same commit**;
`tests/operations/test_docs_index.py` fails the build otherwise, and the same
test refuses any reference in the repo to a `docs/` path that does not exist.

## Where the web app is

`crates/plurxd/src/web/index.html` is a 97-line shell of markup and tags. The
app itself is the sixty-six files
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

## Rust compile loop

At the start of any session that may change Rust, establish a working compiler
loop before editing. If the checkout host cannot run the repository-pinned
Rust 1.97.1 toolchain, use the source-only cloud loop in
[docs/DEVELOPMENT_PIPELINE.md](docs/DEVELOPMENT_PIPELINE.md#4-compile-before-ci-send-source-to-the-compiler-never-credentials).
Do not wait for CI to discover type errors, moved values, missing struct fields,
test failures, or denied lints.

- Archive committed source with `git archive`; transfer neither `.git` nor a
  repository credential to the cloud container.
- Keep the cloud `target/` warm and run check, clippy, formatting, and the
  focused tests needed by the change before pushing.
- After applying the verified patch to the current intended base, archive that
  exact branch and run the loop again. Results against the older source snapshot
  are not evidence for a branch whose base moved.
- Verify `rustc --version` rather than trusting the default `cargo`; an unpinned
  toolchain is not equivalent evidence. Remove stale source extractions when
  the session's fixed writable allowance gets tight.

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

## Prove it locally before you push

If your session has the clone on one machine and `cargo` on another — the
usual shape for a coding agent here — set up the compile loop in
[docs/ci/AGENT-COMPILE-LOOP.md](docs/ci/AGENT-COMPILE-LOOP.md) **before** writing
Rust, not after a gate rejects something. `git archive` carries source to a
toolchain without carrying a credential, and it puts `cargo check`, `clippy
-D warnings`, the unit suite and `rustfmt` inside ten minutes.

The rule it exists to enforce: **do not use CI as a compiler.** A branch
pushed to discover whether it builds costs fifteen to forty minutes and
reports one error; the same branch checked locally reports all of them at
once. Re-verify against current `main` after porting a change, or the suite
you ran describes a snapshot rather than the branch.
