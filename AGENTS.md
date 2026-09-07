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
describes (`playback-control/`, `streaming/`, `cluster/`, `clients/`,
`performance/`, `ci/`, `features/`, `reviews/`, `archive/`).

A document you add or move belongs in the subject folder for its work and
needs a row on that index **in the same commit**;
`tests/operations/test_docs_index.py` fails the build otherwise, and the same
test refuses any reference in the repo to a `docs/` path that does not exist.

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
- Treat `Effort development gate` as blocking. It proves policy, formatting,
  static web contracts, and affected Rust/Apple/Android compilation; it does
  not make the branch releasable.
- Run the smallest focused regression for changed behavior locally and record
  that command in the task pull request. The effort workflow deliberately
  defers the full suites.
- Commit effort tasks with `PLURX_EFFORT_COMMIT=1 git commit ...`. The tracked
  hook then runs fast policy plus compile-only Rust evidence instead of the
  full commit profile; never use that override for an ordinary `main` change.
- When the effort is complete, freeze task merges, merge current `main` into
  the effort, and open `effort/<project>` into `main`.
- Merge only after `Main promotion gate` passes on the current candidate and
  the qualification receipt exists. If `main` or the effort moves, qualify the
  new tree again.

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
