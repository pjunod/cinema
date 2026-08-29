# Contributor workflow

The repository is private because trusted self-hosted runners execute its
workflows. GitHub branch protection is unavailable on the current account
plan, so every contributor and coding agent must enforce the merge convention
below.

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
