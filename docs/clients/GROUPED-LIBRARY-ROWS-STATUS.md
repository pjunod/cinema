# Grouped library rows — implementation and acceptance status

**Status:** building · **Updated:** 2026-10-04 · **Branch:**
`codex/grouped-library-rows` · **Pull request:** pending

Companion to [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md), which maps the web
application. This page records the approved grouped-row library design,
implementation progress, decisions, review, and merge evidence.

## The browsing contract

Movies, shows, and other libraries offer Rows and Grid as normal view choices.
Rows group the filtered library by its selected sort: title initial, year,
recently-added period, recording year, or resolution. Each group scrolls
horizontally; the page scrolls vertically. A jump index reaches each group,
and View all opens one group as a paginated grid. The choice is remembered.
Existing cards, watch filters, library scope, title search, artwork, and item
navigation remain shared with the existing library view.

Rows must cover the entire loaded library, not the first display page. Loading
and failures must identify incomplete results. Arriving pages must preserve
row scroll and keyboard focus. Pointer, touch, and keyboard users must all be
able to reach the last item in a group. Empty and unknown metadata groups are
handled explicitly, without invented dates or years.

## Progress

| Step | State | Evidence / next action |
|---|---|---|
| Isolated checkout | done | Own clone under `/private/tmp`; user checkouts untouched. |
| Compiler and hooks | ready | Local Rust 1.97.1 verified; tracked commit hook installed. |
| Shared grouping and row presentation | building | Existing loader and card renderer; no server change expected. |
| Responsive controls and navigation | pending | Desktop and mobile browser; native scope awaiting clarification. |
| Regression coverage | pending | Author during implementation; execute after review. |
| Adversarial agent review | pending | One review when the complete PR is ready to merge. |
| Fast lane | pending | After review corrections; rerun only failed checks. |
| Merge and cleanup | pending | Carry regression fields into landing message; delete task branch and temporary build files. |

## Decisions

1. **One PR with normal commits.** This is one library-browsing change, built
   in an independent clone. Implementation, coverage, and documentation land
   together, without task PRs or unrelated edits.
2. **The existing sort drives groups.** No separate grouping setting that can
   contradict sorting, and no Developer gate for an ordinary view option.
3. **Tests follow the final review.** Paul explicitly confirmed on 2026-10-04
   that this overrides the earlier before-push regression timing. Formatting,
   syntax checks, and compiler/lint checks continue during development.
4. **No playback changes.** This work changes catalog presentation only and
   reuses item navigation and the existing metadata API.

## Review and validation evidence

Not run yet. Record exact commands and results here after the adversarial
review and corrections. A pending row is not a passing result.
