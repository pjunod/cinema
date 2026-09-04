# Web layout containment — live delivery status

**Status:** fast validation complete; pull request assembly in progress ·
**Updated:** 2026-09-04

This page tracks the systemic fix for intrinsic-width content escaping its
owning card or viewport. The immediate report was a long DTS-HD audio label on
the item page, but the contract applies to every shipped fractional grid
track, including reader and responsive layouts.

## Outcome — content, not labels, owns the failure boundary

Every fractional content column now has an explicit zero minimum. Long track
chips may wrap, but they may not resize the grid, card, or document. The rule
is retained by a source-wide static contract and a Chromium stress assertion
that injects both an ordinary long label and an unbroken token.

## Delivery — one ordinary main-bound pull request

| Stage | State | Evidence |
|---|---|---|
| Root cause | Complete | Bare `1fr` resolved to an intrinsic `auto` minimum |
| Systemic implementation | Complete | Main UI, responsive grids, and both readers use zero-floor content tracks |
| Focused regression | Complete | Static contract plus post-capture Chromium containment stress |
| Fast validation | Complete | Six local checks green; 2,761 unit tests green on pinned Linux |
| Adversarial review | Pending | Independent correctness, test, and maintainability reviews |
| Full PR qualification | Pending | One full run after every review finding is resolved |
| Merge | Pending | Only after the current PR candidate is green |

## Decisions — defaults taken without blocking delivery

- Use one ordinary `codex/web-layout-containment` branch targeting `main`.
  This is one independently shippable correction, not a multi-task effort.
- Enforce the zero-floor rule across all shipped CSS instead of patching only
  the visible audio row. This prevents the same intrinsic sizing defect from
  moving to another page or breakpoint.
- Keep adversarial content out of image goldens. The browser assertion runs
  after capture, so it adds containment evidence without meaningless visual
  baseline churn.
- Build from a fresh GitHub clone. No existing developer checkout is used for
  commits, validation artifacts, pushes, or merge operations.
- Run Rust with the installed 1.97.1 toolchain explicitly. The Mac default is
  1.95.0 and is not valid evidence for this repository.
- Move only the unit execution to a source-only Linux container. The Mac's
  FFmpeg links to a removed `libx265.216` and its sandbox refuses local socket
  binds; changing the developer machine would broaden this correction without
  improving the product.

## Evidence — commands that must describe the final tree

Fast development evidence:

```bash
make validate-plan
CARGO='rustup run 1.97.1 cargo' make validate-staged
cargo test --workspace --exclude plurx-cluster-check --no-fail-fast
```

The selected local lane passed its validation, history, operations, input,
static web, and membership checks. Its browser checks declared the missing
Python Playwright prerequisite and deferred to the PR runner. The local Rust
gate passed formatting and Clippy before fixture execution found the broken
host FFmpeg and denied socket binds. The exact staged source tree was archived
without `.git`, SHA-256 verified after transfer, and its unit lane completed
on Rust 1.97.1 with 2,761 passed, zero failed, and three intentionally ignored.

Final qualification runs once after review findings are fixed:

```bash
make validate-full
gh pr checks <number> --watch
```

If either command exposes a defect, the failing focused check becomes the
development loop. The full qualification is restarted only when the candidate
tree is stable again; evidence from an older commit is never reused.
