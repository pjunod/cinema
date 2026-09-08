# Web layout containment — live delivery status

**Status:** live delivery state is recorded on authoritative
[Forgejo PR #4](http://192.168.4.7:3000/noirr/plurx/pulls/4) ·
**Updated:** 2026-09-04

This page tracks the systemic fix for intrinsic-width content escaping its
owning card or viewport. The immediate report was a long DTS-HD audio label on
the item page, but the contract applies to every shipped fractional grid
track, including reader and responsive layouts.

## Outcome — content, not labels, owns the failure boundary

Every formerly bare fractional content track now has an explicit zero minimum.
All shipped fractional tracks must declare a non-intrinsic minimum; deliberate
poster and card grids retain their positive design minimums. Long dynamic text
may wrap, but it may not resize its grid, card, or document. The rule is
retained by a source-wide static contract and Chromium stress assertions that
inject both an ordinary long label and genuinely unbroken track and hostname
tokens.

The sweep also exposed classic's full-bleed hero retaining desktop negative
margins inside smaller mobile padding, widening the page by six pixels on
each side. Main and its hero now share inset variables, so their breakpoint
spacing cannot drift independently.

## Delivery — one ordinary main-bound pull request

| Stage | State | Evidence |
|---|---|---|
| Root cause | Complete | Bare `1fr` resolved to an intrinsic `auto` minimum |
| Systemic implementation | Complete | Every fraction declares a non-intrinsic floor; shrinkable content tracks use zero |
| Focused regression | Complete | Static contract plus post-capture Chromium containment stress |
| Fast development proof | Historical | Pre-review checks green; final tree is re-qualified once after review |
| Adversarial review | Live on PR | Three independent reviews; every finding is blocking until resolved |
| Full PR qualification | Live on PR | One full run after every review finding is resolved |
| Merge | Live on PR | Forbidden until the current candidate is green |

## Decisions — defaults taken without blocking delivery

- Use one ordinary `codex/web-layout-containment` branch targeting `main`.
  This is one independently shippable correction, not a multi-task effort.
- Enforce explicit non-intrinsic fractional floors across all shipped CSS,
  using zero for shrinkable content tracks, instead of patching only the
  visible audio row. This prevents the same intrinsic sizing defect from
  moving to another page or breakpoint.
- Keep adversarial content out of image goldens. The browser assertion runs
  after capture, so it adds containment evidence without meaningless visual
  baseline churn.
- Build from a fresh independent clone, initially fetched from the GitHub
  mirror and then rebased on and pushed to the Forgejo authority. No existing
  developer checkout is used for commits, validation artifacts, pushes, or
  merge operations.
- Run Rust with the installed 1.97.1 toolchain explicitly. The Mac default is
  1.95.0 and is not valid evidence for this repository.
- Initially move unit execution to a source-only Linux container. The Mac's
  FFmpeg links to a removed `libx265.216` and its sandbox refuses local socket
  binds; changing the developer machine would broaden this correction without
  improving the product.
- Final local qualification uses isolated FFmpeg launch wrappers and explicit
  toolchain paths; no Homebrew installation is modified. The PR records the
  current-tree results separately from the historical Linux snapshot.

## Evidence — commands that must describe the final tree

Fast development evidence:

```bash
make validate-plan
CARGO='rustup run 1.97.1 cargo' make validate-staged
cargo test --workspace --exclude plurx-cluster-check --no-fail-fast
```

The pre-review candidate passed its selected validation, history, operations,
input, static web, and membership checks. Its browser checks declared the
missing Python Playwright prerequisite and deferred to the PR runner. The local
Rust gate passed formatting and Clippy before fixture execution found the
broken host FFmpeg and denied socket binds. That source snapshot was archived
without `.git`, SHA-256 verified after transfer, and its unit lane completed on
Rust 1.97.1 with 2,761 passed, zero failed, and three intentionally ignored.
It is historical development evidence only: the exact post-review tree is
re-qualified after the final rebase and is the only evidence used for merge.

Final qualification runs once after review findings are fixed:

```bash
CARGO='rustup run 1.97.1 cargo' make validate-full
# Then watch Forgejo PR #4 → Actions → Main promotion gate.
```

If either command exposes a defect, the failing focused check becomes the
development loop. The full qualification is restarted only when the candidate
tree is stable again; evidence from an older commit is never reused.
