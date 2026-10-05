# Grouped library rows — implementation and acceptance status

**Status:** implemented and locally validated · **Updated:** 2026-10-05 · **Branch:**
`codex/grouped-library-rows` · **Pull request:** [#821](http://192.168.4.7:3000/noirr/plurx/pulls/821)

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
| Shared grouping and row presentation | implemented | Shared loader/card renderer; all five sorts; no server changes. |
| Responsive controls and navigation | implemented | All three web layouts, touch rows, keyboard navigation, jump index and group grids. |
| Regression coverage | passed | Seven unit/keyboard checks and eight browser cases have passed on the final source. Only failed browser cases were rerun. |
| Adversarial agent review | addressed | Three P2 findings on `1affd2904`: Theater controls, group viewport navigation, index focus. All corrected with regression coverage. |
| Fast lane | tracked on PR | [PR #821](http://192.168.4.7:3000/noirr/plurx/pulls/821) carries the current candidate verdict and final evidence. |
| Merge and cleanup | tracked on PR | The PR records its landing commit and cleanup receipt after all required jobs pass. |

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

Formatting, pinned workspace Clippy, JavaScript syntax, the TypeScript
baseline, source-shape lint, and theme contrast lint passed during development.
After review corrections, these focused commands passed:

```bash
node --test tests/web/library-rows.test.js tests/web/calm-library.test.js tests/web/nav-keyboard.test.js
# Seven reported checks, plus the existing incremental-grid assertions.

node --test tests/web/library-rows.browser.cjs tests/web/library-layout.browser.cjs
# Five browser cases passed initially; three fixture assumptions needed correction.

node --test --test-name-pattern='rows preserve cards|View all opens|row filters, sorts' tests/web/library-rows.browser.cjs
# Incremental scroll/focus passed; two remaining cases required fixture corrections.

node --test --test-name-pattern='View all opens|row filters, sorts' tests/web/library-rows.browser.cjs
# Both remaining browser cases passed. All eight now have passing evidence.
```

Browser commands used the bundled Playwright module via `PLAYWRIGHT_MODULE`.
The fixture corrections account for CSS scroll snapping, give the expanded
group enough content to exercise vertical navigation, and measure library
containment independently of the existing Classic header overflow at 768 px.
No production code changed between these browser runs. Full unit suites were
not run locally. Fast-lane results will be recorded on the PR before merge.

## Scope and interaction choices

This PR implements the responsive web app shown in the approved renders,
including mobile browsers. Native Apple and Android screens are outside this
PR; no response to the optional scope question had arrived when work began.

Rows is the default; `plurx_library_view` remembers Rows/Grid per browser.
The existing page size controls Grid and group expansion, not row contents.
Title groups follow the server's article-stripped `sort_title`; non-A–Z keys
share `#`. Year and recording date use years; resolution uses existing tiers.
Recently added uses Today, Previous 6 days, Earlier this month, then calendar
months. Missing metadata goes into named Unknown groups; future added dates
are identified separately. TV years are series years, not episode air dates.

Each row initially mounts at most 40 cards. Approaching its horizontal end
mounts another 40. Arrow keys move between posters; Home/End reach a row's
first/last poster. View all opens the complete group in the existing grid,
and All rows restores its horizontal position. Counts identify partial loads
and request failures offer Retry. Grouping never caps membership to Grid's
page size. Merged category ordering follows the server's shared sort fixture.


## Adversarial review — 2026-10-05

One independent agent reviewed `1affd2904` without running tests. Its verdict
was request changes for three P2 findings:

| Finding | Correction | Regression |
|---|---|---|
| Theater toolbar omitted Rows/Grid and retained an inert page size. | All three layout shells use the same view and page-size controls. | Cross-layout desktop/mobile browser case. |
| View all retained a late row's vertical offset in the new grid. | Expansion scrolls to its heading; returning jumps to the originating row. | Late-letter expansion and return browser case. |
| A new group replaced every jump-index button and lost focus. | Index buttons reconcile by group identity, preserving focus and scroll. | Delayed page with a focused index button. |

No second review is requested. Validation follows these corrections.

## Candidate handoff — 2026-10-05

Review corrections and focused passing evidence are committed in `b45b81409`.
The PR is ready for merge validation; `main` remained at `bf0bb6acf` through
this local acceptance pass. Forgejo's API title change cleared draft status
without emitting a ready-for-review run, so this documentation update supplies
the normal ready-PR synchronization event. It changes no tested source.
The [PR check list](http://192.168.4.7:3000/noirr/plurx/pulls/821) is the
source of truth for the resulting candidate's gate and landing status.
