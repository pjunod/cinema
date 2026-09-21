# Native Live TV layouts — implementation status and evidence

**Status:** open — proportions shipped on both native clients; the physical
walkthrough is the only unproved step · **Base:** Forgejo `main` at
`ebe2b100` · **Updated:** 2026-09-12

Companion to
[LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) (the existing
guide and playback contract) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review fast
lane) — this is the short answer to *what has shipped in this effort, what is
being built, and what remains unproved*.

## Proportions — the second correction, issue #267

Paul reported the shipped layouts as wrong in proportion rather than wrong in
behaviour: Apple TV channel rows "three feet long", the live preview "tiny
like an afterthought", mobile "still not very good", against a web page that
is "nearly perfect". The review found five causes, all in the views, and one
rule fixes them: **lists are tall and narrow; grids are wide.**

| Cause | Correction |
|---|---|
| tvOS resolves the semantic text styles 2–2.5× larger than iOS — `.subheadline` 38 pt, `.title2` 57 pt | one explicit scale per client: `LiveTvType` (30 / 22 / 20 / 18, badges 16) and `LiveTvTypography` (15 / 11 / 10 / 9 sp, badges 8) |
| Three bands of chrome before any content — a status banner, 7–10 66 pt buttons, the tab bar on Apple; ≈180 dp of Back, headline, title, status and a 7-item row on Android | one toolbar — 48 pt on Apple, 28 dp minimum on Android (a fixed 24 dp clipped its own labels at any font scale above 1.0). The banner became one status line; Earlier / Now / Later became chips in the grid's own header; the search field became a dialog; Return to live went away because the picture is a focus target whose Select is fullscreen |
| The preview was sized by a height fraction and then aspect-fit inside a width fraction — 437 × 246 pt, 23% of the width | On now is a 620 pt / 310 dp list column beside a picture that takes the width the list leaves |
| The guide's slot width was a constant — 300 pt × 3 + 210 filled 58% of a 1920 pt screen, and Android drew the phone's 160 dp slots and 56 dp rows on a television | slot width is derived from the width the grid is actually given: `(contentWidth − inset − channelColumn) ÷ 4`. Two hours fit any panel, and the fixed `rows × rowHeight + 54` frame that pushed the last rows off the bottom is gone |
| Phones stacked a six-line now bar, a status line, a filter row and an always-visible search field above the list | full-bleed 16:9 picture with its chips and its PiP/fullscreen actions on it, one 56 pt caption, one 48 pt toolbar, then the web `.lt-row` |

The Layout menu collapses to **Preview** and **Over picture**. The stored
`channel_browser` value still decodes and is never rewritten; it renders as
Preview, because the two only ever differed in which browse view they opened
with. No new persisted state, no reducer, lease or input-contract change, no
server change, and no gate: the Developer-tab enable and its advisory
readiness reasons remain the only runtime control.

| PR | Scope | Evidence | State |
|---|---|---|---|
| [#268](http://forge.lan:3000/noirr/plurx/pulls/268) | Apple — `LiveTvView.swift`, `LiveTv.swift`, `Theme.swift`, `LiveTvTests.swift`, build 146 | `make apple-test` on the lab Mac: **970 cases, exit 0**, across the iOS and tvOS destinations | merged to `main` at `ebe2b100` |
| [#269](http://forge.lan:3000/noirr/plurx/pulls/269) | Android — `LiveTvScreen.kt`, `LiveTvGuideUi.kt`, `LiveTvApi.kt`, `TvFocus.kt`, `SettingsScreen.kt`, versionCode 89 | pinned build image: `testDebugUnitTest` + `lintDebug`, **553 tests, 0 failures** | open on the fast lane |

Each PR took exactly one adversarial review before it was marked ready. Both
reviews found a focus blocker that no test could have caught, and both are
answered in full on the PR:

- **Apple.** The guide's new paging chips were focusable inside the remote
  adapter with no focus key of their own. `onMoveCommand` consumes every
  direction it is handed and the adapter discards the `apply` result, so a
  chip could not be left in any direction — and the inline page has no root
  adapter to answer Back with. Every focusable inside the adapter now owns a
  key; `movePagingFocus` walks the chips, hands `up` to the toolbar and
  enters the grid on `down`.
- **Android.** `Over picture` drew the picture `fillMaxSize` as a focus
  target, so its focus rect was the whole screen: nothing in the 260 dp guide
  panel was below, left or right of it, and it also always won Down from the
  toolbar. The picture now takes a focus modifier that stops above the panel.

Both are anchored in `tests/client-fixes.toml` against the arrangement
contracts that would catch them again — contracts that assert the wiring, not
the constants: the grid draws only from the dimensions it is handed, the
television call sites measure before they size, the fixed grid frame and the
deleted chrome bands are gone, and the picture is a bounded focus target.

**Still unproved:** every claim above is simulator, JVM and lint evidence. The
Apple TV + Siri Remote and Google TV Streamer + D-pad walkthrough is the
review document's §7 prompt and has not run; the first thing to check on the
physical Apple TV is whether the guide's bottom rows are reachable, which was
the unverified half of the fifth cause.

**Three judgement calls made without Paul**, recorded so they can be reversed:
the status banner became a persistent one-line status rather than the
specified four-second toast (a toast cannot repeat itself when the same
failure happens twice, and it hid every cleanup message on a phone); the
`Over picture` layout keeps the 48 pt toolbar above it (hiding it would leave
Search, Layout and More reachable from nowhere); and the Live TV Developer
enable was left exactly as it is — it already carries prose requirements plus
a readiness check that renders met/unmet rows and does not gate Enable, and
giving it structured rows like *Library channels* has would need a new
`live_tv` item in the server's `developer.rs`, which is a server change and a
different CI lane.

## Progress — three packages, then one promotion

| Package | State | Evidence | Remaining |
|---|---|---|---|
| Shared input, metadata, and preference contracts | verified | bounded FFmpeg input parser/cache; cross-client DTO and badge cases; generated input tables; pinned Rust compile plus 3 focused server and 39 web checks green | promotion gate |
| Apple TV and iOS | verified | all three saved layouts, anchored/pinned guide, scoped input, compact schedule/grid, Apple build 130; iOS and tvOS compile green; 28 focused Live TV tests green on each platform | physical Siri Remote walkthrough remains unproved; promotion gate |
| Google TV and Android | verified | all three saved layouts around one movable player, stable UTC-anchor D-pad navigation, false-on-delegate input, compact schedule/grid, Android build 79; Kotlin compile and 35 JVM checks green; real D-pad case green on Google TV Streamer | broader physical walkthrough remains unproved; promotion gate |
| Integrated promotion to `main` | candidate finalized | Forgejo PR [#229](http://forge.lan:3000/noirr/plurx/pulls/229); exactly one Astra adversarial review of head `f5e67fe9`; all findings addressed; final focused pass green; no feature gates; full suites reserved for the separate sweep | the remote workflow still launched the legacy full qualification on a ready effort PR, so that run was canceled; the candidate adds the required label-triggered compile-only lane before obtaining its current-head verdict |

## Apple TV navigation follow-up — one focus owner per press

Forgejo issue
[#235](http://forge.lan:3000/noirr/plurx/issues/235) tracks a physical
Apple TV regression reported after build 130 shipped. The correction is Apple
build 134 after integrating current `main`. Rapid Siri Remote moves
can be overwritten by the guide's asynchronous restore pass, and moving above
the first guide row can leave both the grid and toolbar claiming focus. The
default tvOS button tint also paints the More and Layout sheet labels and
their backgrounds the same red, which makes inactive actions unreadable.

The correction gives delayed Guide and On now restoration revocable ownership
tickets: new remote input, a toolbar focus, or leaving the region invalidates
the queued write before it can touch `FocusState`. The guide adapter emits one
ordered boundary transfer that clears the grid before asking the toolbar to
take focus. Focusable channel headers now live in the vertically scrolling
rows while cancelling only the horizontal offset, and On now uses a native
focus-scrolling list. Every Live TV television action owns an explicit
foreground/background pair. One focused regression pass will be recorded
here before the fix is promoted.

## Contract — presentation moves, playback does not

> **Superseded in part by the proportions correction above.** The three-entry
> layout menu described here shipped and then collapsed to **Preview** and
> **Over picture** in PRs #268 and #269; `channel_browser` is still a valid
> stored value and still decodes, but it renders as Preview. Everything else
> in this section still holds.

The effort shipped `Guide + preview`, `Guide over picture`, and `Channel
browser` together on both television clients. The default is `Guide +
preview`; the choice saves locally per device and never starts, stops, or
retunes the current session. A temporary guide over fullscreen also leaves
the saved layout unchanged.

Phone and tablet clients keep their existing `On now`, `Guide`, and
`Favorites` choices. They receive a compact player and browsing treatment,
not the television layout selector. The existing Live TV Developer switch
and its met/unmet readiness reasons remain the sole runtime enable surface.
Readiness is advisory: no build flag, device allowlist, rollout gate, or
hidden eligibility test is added.

## Evidence — claims are attached to exact trees

| Date | Tree | Check | Result |
|---|---|---|---|
| 2026-09-09 | `main` at `4cef0da7` | `rustup run 1.97.1 rustc --version` | pinned compiler available: `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| 2026-09-09 | `main` at `4cef0da7` | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 1m 22s as the pre-edit compiler baseline |
| 2026-09-09 | initial status-page change | `python3 -m unittest tests.operations.test_docs_index` | 4 passed in 0.925s before the instruction to reserve all further tests for the final integrated candidate; this check will not be repeated during implementation |
| 2026-09-09 | implementation worktree | `rustup run 1.97.1 cargo check -p plurxd --all-targets` | passed in 31.24s; retained metadata tests were compiled but not executed |
| 2026-09-09 | implementation worktree | `make apple-build` | iOS and tvOS compile-only builds passed; no simulator tests ran |
| 2026-09-09 | implementation worktree | `./gradlew --no-daemon :app:compileDebugKotlin` | passed in 20s; no JVM or instrumentation tests ran |
| 2026-09-09 | implementation worktree after D-pad wiring | `./gradlew --no-daemon :app:compileDebugKotlin` | passed in 9s; main sources only, with the retained real-KeyEvent instrumentation case neither compiled nor executed by this command |
| 2026-09-09 | pre-review integrated tree on current `main` | `rustup run 1.97.1 cargo fmt --all -- --check` and `cargo clippy -p plurxd --all-targets -- -D warnings` | passed; Clippy completed in 1m 13s |
| 2026-09-09 | pre-review integrated tree on current `main` | `make apple-build` | exact-tree iOS and tvOS compile-only builds passed in 13s |
| 2026-09-10 | post-review remediation worktree | `xcrun swiftc -parse clients/apple/Sources/*.swift` and `make apple-build` | Swift parse plus iOS and tvOS compile-only builds passed; no simulator tests ran |
| 2026-09-10 | post-review remediation worktree | `./gradlew :app:compileDebugKotlin` | Android main-source compile passed in 2s; no JVM or instrumentation tests ran |
| 2026-09-10 | post-review remediation worktree | pinned Rust `fmt --check`, `cargo check -p plurxd --all-targets`, and Clippy with denied warnings | passed; retained regression tests compiled but did not execute |
| 2026-09-10 | merged-base final pass | focused Rust source-format tests and `tests/web/live-tv.test.js` | 3 Rust tests and 39 web checks passed |
| 2026-09-10 | merged-base final pass | Android `tv.plurx.app.livetv.*` JVM tests | 34 passed; one equal-value boxed `Int`/`Long` assertion failed, was corrected to the reducer's UTC `Long`, and its sole targeted retry passed — 35/35 covered |
| 2026-09-10 | Google TV Streamer, Android 14 | `LiveTvGuideFocusTest` real D-pad event instrumentation | first attempt lost its Compose activity when the sleeping display stopped it; after waking the device, 1/1 passed and normal sleep behavior was restored |
| 2026-09-10 | Apple simulators, OS 26.5 | focused `LiveTvTests` | iOS 28/28 passed; tvOS passed 26 initially and the two corrected stale source-contract assertions on their sole retry — 28/28 covered |
| 2026-09-10 | PR #236 review-remediation worktree | `xcrun swiftc -parse` plus iOS and tvOS compile-only builds | passed after addressing the single review; no simulator tests ran |
| 2026-09-10 | PR #236 merged-base source tree `fcc2c2e5` | iOS and tvOS compile-only builds, then the one focused tvOS `LiveTvTests` simulator pass | compile passed; 31/31 tests passed with no retry; no other suite ran |
| 2026-09-10 | PR #236 fast-lane attempt 1770 | policy preflight | stopped before compile because the history ledger still named Apple build 133 and omitted both corrective client commits; anchored both source/test pairs, updated the build claim to 134, and confirmed `make history-check` locally |

Compilation and static contracts are retained here as they pass. The focused
Google TV D-pad path has physical-device evidence; the complete Google TV and
Apple TV/Siri Remote walkthroughs still need the release build and are not
replaced by simulator claims.

## One adversarial review — disposition

The required single review ran in Astra task
`01a088d3-145d-7ea3-af07-4240406efcef` against draft head `f5e67fe9`. No
second review or approval pass will be requested. The author verified the
remediation through source inspection, compile/static checks, and the final
focused test pass recorded above.

| Findings | Resolution |
|---|---|
| Web adapter used obsolete presentation names and retained stale merged facts | aligned it to the generated `browser` / `fullscreen_*` contract, delegated native activation, and made a missing status observation clear the previous one |
| Android could sleep during playback, leave fullscreen during a tune, lose the picture on an empty filter, or let delegated activity age out the controls | restored `keepScreenOn`, distinguished a busy tune from a completed stop, retained the movable player under Clear filters, and rearmed the idle timer for native focus/activation |
| TV guide reachability, paging, protected/empty focus, selected-programme details, and layout-switch restoration were incomplete | exposed Guide in every layout; added Earlier, Now, and Later over the fetched window; made protected headers and empty rows focusable but harmless; hoisted channel/programme/UTC-anchor state and restores it after composition |
| Apple mobile, settings, exit, and timer behavior had presentation gaps | added a no-guide Watch fallback, used one `@AppStorage` layout source for Settings and Live TV, switched the selected Home tab on Leave, and rearmed/suspended fullscreen hiding as focus and panels change |
| Expired measured source facts could survive in server or client snapshots | the session clears its cached lineup observation before applying a fresh session observation; web, Apple, and Android expire at 20 minutes or programme end and fall back to honest lineup facts |

The review's build-number question originally distinguished Apple build
**129** from Forgejo issue **227**. Current `main` subsequently claimed build
129, so the promotion correction re-claimed this branch as Apple build
**130**; 227 remains the issue number in the build-note filename and its
`Issue:` field.

## Apple TV navigation follow-up — adversarial review disposition

The required single Astra review ran in task
`01a088d3-145d-7ea3-af07-4240406efcef` against PR
[#236](http://forge.lan:3000/noirr/plurx/pulls/236) draft head `898bf2b3`.
It required changes; no second review or approval pass will be requested.

| Finding | Author resolution |
|---|---|
| Guide and On now restore tasks could reclaim focus after newer input or a move to the toolbar | added one revocable restore-ticket coordinator shared by both paths; explicit entry advances the request, while navigation, focus loss, and toolbar ownership invalidate older work |
| Focusable channel headers were drawn in an overlay that could not make the guide scroll to off-screen rows | moved the buttons into each scrolling row and compensate only for horizontal motion, leaving vertical reveal under the scroll view's focus ownership |
| The regression checks exercised destinations and source strings, not restore ownership or the boundary handoff | added coordinator behavior coverage for stale tickets, passive refresh after exit, a newer explicit entry, and the ordered `clearGrid` then `focusToolbar` effects; retained only the visual/container source contracts |

The author compile-checked the remediated shared sources for both iOS and tvOS,
then ran the one focused simulator pass on the current-base source tree:
31/31 tvOS `LiveTvTests` passed without a retry. No broader suite ran. The
remaining automated evidence is the label-triggered compile-only fast lane.

## Decisions made while Paul is away

| Decision | Why | Revisit when |
|---|---|---|
| Keep the approved package order on one effort branch | metadata and input contracts are the seam both native implementations consume; one integrated branch minimizes promotion overhead | a package requires an independently releasable server compatibility step |
| Use the existing runtime Live TV enable control only | it already exposes configuration and readiness; another activation mechanism would be a hidden gate by a different name | never, unless the product-level enable contract changes explicitly |
| Add the missing [`fast-lane` workflow](../../.github/workflows/main-fast-lane.yml) in this PR | the remote workflow ignored label events and started full suites, which could not produce the required gate without violating the resource budget; the new lane reuses the proven effort compile jobs and verifies the current head and base before promotion | once the separate full-suite scheduler replaces the legacy PR fan-out completely |

## Non-goals — this effort stays bounded

No DVR, recording, rewind, scheduled tuning, new tuner provider, new decoder,
4K delivery promise, surround passthrough promise, app-wide navigation
redesign, theme engine, or background scan is part of this work. Source facts
describe what the tuner delivered; they do not claim the player preserved
that format after transcoding.
