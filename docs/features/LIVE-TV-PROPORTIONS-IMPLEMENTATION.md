# Live TV proportions — the build plan for the TV and phone layouts Paul approved

**Status:** **executed and merged 2026-09-12** · **Executes:** the fix spec in
[LIVE-TV-NATIVE-LAYOUTS-PROPORTIONS-REVIEW.md](LIVE-TV-NATIVE-LAYOUTS-PROPORTIONS-REVIEW.md)
§3, approved by Paul 2026-09-12 · **Base:** `main` at `10f2afe60` (Apple
build 144 · Android versionCode 87) · **Written:** 2026-09-12


> **Built.** Apple PR #268, Android PR #269 and the status update PR #271
> merged on 2026-09-12; `main` at `311683bc`, Apple build 146, Android
> versionCode 89, issue #267 closed. Three things changed during the build and
> the shipped code, not this document, is the record: the status banner became
> a persistent one-line status rather than a four-second toast (a toast cannot
> repeat itself when the same failure happens twice, and it hid every cleanup
> message on a phone); the Android television toolbar is `heightIn(min = 28.dp)`
> rather than a fixed 24 dp, which clipped its own labels; and the Apple slot
> width also subtracts the grid's own 16 pt inset. What shipped, and what it
> did not prove, is in
> [LIVE-TV-NATIVE-LAYOUTS-STATUS.md](LIVE-TV-NATIVE-LAYOUTS-STATUS.md).

Companion to the review (why the screens are wrong and the numbers that fix
them) and to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how a
PR reaches `main` since 2026-09-10) — this is *what to edit, in what order,
and what proves it*. Read the review's §2–§3 once; the renders are
`../mockups/live-tv/proportions-tv-proposed-on-now.png`,
`proportions-tv-proposed-guide.png`, `proportions-tv-proposed-over-picture.png`
and `proportions-phone-proposed-on-now.png` beside it.

This is two independent pull requests — Apple (tvOS + iOS) and Android (TV +
phone) — that share nothing but this document, so two sessions can build them
in parallel with the file ownership in §4. It is view code and constants
only. If a step seems to need the guide reducer, the lease, the input
contract, the server, or a new persisted setting, stop and flag it: that is
outside this plan and almost certainly a misreading. Budget: each PR is one
to two days of focused work, not a milestone programme.

## 1. Objective and the one rule

Give the picture and the channel list the web page's proportions on every
native client, without touching what plays or how the remote is routed.

**Lists are tall and narrow; grids are wide.** Everything in §3 follows from
it: On now puts a 620 px list column beside a large picture; Guide puts a
30%-wide picture over a full-width grid; the fullscreen guide keeps its
opaque lower panel. Text on the ten-foot surfaces gets an explicit scale
(30 · 22 · 20 · 18 px, badges 16) instead of tvOS semantic styles, because
`.subheadline` is 38 pt and `.title2` is 57 pt there.

## 2. What does not change — guardrails

- **No gates.** The existing Settings → Developer *HDHomeRun Live TV*
  enable/disable with its met/unmet readiness rows stays the sole runtime
  control, unchanged. No build flag, environment variable, allowlist,
  rollout check, or "new layout" toggle is added; every viewer gets the new
  layouts on install.
- **No reducer, lease, guide-fetch or input changes.** `LiveTvGuideReducer`,
  `LiveTvLease`, `LiveTvInputRouting` / `LiveTvInputPolicy`, the generated
  input tables, `player-input-fence` and `tests/ui-structure.golden` are
  untouched. `gridLayout` already takes `pxPerSlot` as a parameter — pass a
  different number, do not change the function.
- **No new persisted state.** The saved layout key (`plurx.liveTvLayout` /
  `liveTvLayout` DataStore), the browse view and the phone guide choice keep
  their keys and raw values; §3.1 aliases one value at read time.
- **No web changes, no theme changes, no navigation redesign, no DVR.**
- **No server or Store changes**, so no `ci-store` lane and no cluster suite
  is involved.

## 3. Contract — the numbers, per platform

Design px at 1920 × 1080. tvOS points = px. Android TV dp = px ÷ 2.
Re-verify every line anchor below against the branch you build on;
they are from `10f2afe60`.

### 3.1 Layout menu — two entries, three stored values

| Stored raw value | Menu label after this plan | Renders |
|---|---|---|
| `guide_preview` | **Preview** (default) | §3.3 when browse = On now · §3.4 when browse = Guide |
| `guide_overlay` | **Over picture** | §3.5 |
| `channel_browser` | *(not shown)* — read as `guide_preview` | as Preview |

Apple: `TvLiveLayout` ([LiveTv.swift:175–188](../../clients/apple/Sources/LiveTv.swift))
keeps all three cases so an existing `@AppStorage` value still decodes; add
`var presented: TvLiveLayout { self == .channelBrowser ? .guidePreview : self }`,
use `presented` in `tvBrowseRegion` and the layout sheet, and list only
`.guidePreview` and `.guideOverlay` in the sheet with the labels above.
Android: the same in `TvLiveLayout.fromStorage` and the Layout dropdown.
Reason: "Guide + preview" and "Channel browser" only ever differed in which
browse view they opened with; once On now is side-by-side in both, a third
entry is a choice with no consequence.

### 3.2 Toolbar — one row, 48 px

`[ On now | Guide ]` · "12 channels · 1 of 2 tuners" (20 px, muted) · spacer
· `★ Favorites` · `⌕ Search` · `Layout` · `More`. Buttons 48 px tall, 20 px
horizontal padding, 10 px radius, 22 px semibold labels; the segmented
control is the same height with a 1 px `outline` border and the active
half on `surfaceHi`.

| Moves out of the toolbar | Goes to |
|---|---|
| Status banner (`statusMessage`, LiveTvView.swift:1105–1119) | channel count into the toolbar; `live.message` shown as a 4 s toast at the bottom of the content area, except playback/cleanup messages, which appear as the details panel's footer line (§3.3) |
| **Return to live** (1294) | the picture is a focus target; Select on it sets `fullscreen = true`; a "Select · Fullscreen" hint (18 px, 75% white) sits in its bottom-right corner |
| **Earlier / Now / Later** (1265–1275; Android 716–720) | three chips (`‹` · `Now` · `›`, 16 px, 6 px radius) in the grid header's channel column (§3.4) |
| Android search `OutlinedTextField` (260 dp, LiveTvScreen.kt:724–730) | a `Search` button that opens an `AlertDialog` with the text field — the shape Apple already has (LiveTvView.swift:1061–1071) |

Apple: add `TVReadableButtonStyle(prominent:compact:)` — `compact` uses
`padding(.horizontal, 20).padding(.vertical, 10)`, `minHeight: 48`,
`Font.system(size: 22, weight: .semibold)` ([Theme.swift:144–146](../../clients/apple/Sources/Theme.swift)
is the non-compact pair); every Live TV toolbar button passes `compact:
true`; sheets keep the old style. Android: `TvTextButton` /
`TvButton` ([TvFocus.kt:117–171](../../clients/android/app/src/main/java/tv/plurx/app/ui/components/TvFocus.kt))
gain a `compact: Boolean = false` that applies
`defaultMinSize(minHeight = 24.dp)` and `contentPadding = PaddingValues(10.dp, 5.dp)`
with an 11 sp label; Live TV passes `compact = true` on television.

### 3.3 On now — list column beside the picture (both TV layouts named Preview)

Content box = safe width (1760 tvOS pt / 880 Google TV dp) × height below
the toolbar.

| Element | tvOS pt | Google TV dp |
|---|---|---|
| List column width · row gap | 620 · 4 | 310 · 2 |
| Row height · corner radius · padding | 72 · 10 · 0 × 14 | 36 · 5 · 0 × 7 |
| Row grid | `84 · 1fr · auto`, 14 gap | `42 · 1fr · auto`, 7 gap |
| Station chip | 84 × 40, 18 bold muted on `surfaceHi`, 6 radius | 42 × 20, 9 sp |
| Primary / secondary / tertiary | "7.1 WABC" 22 semibold · programme 20 · "until 8:30 PM" 18 muted (right column, under the star) | 11 / 10 / 9 sp |
| Progress bar | 3 px, `outline` track, `accent` fill, under the text column | 2 dp |
| States | focused: 3 px `onBg` ring + `surface` fill · playing: 1 px `accent` ring + `surfaceHi` + "● LIVE" 14 px accent after the name · protected: opacity 0.55, secondary line "Protected · not playable" | same |
| Removed from the row | `Spacer`, `play.circle`, `lock` glyph (LiveTvView.swift:525–529); the `maxWidth: 220` on the bar (517); Android's `TextButton` wrapper and accent-coloured text (LiveTvScreen.kt:963–983) | |
| Picture | width = contentWidth − 620 − 24; height = width × 9/16, capped so the details fit (details ≥ 120); top-left: chip · "7.1 WABC" 20 · LIVE pill; 4 px progress line along its bottom edge; focusable | same ÷ 2 |
| Details under the picture | title 30 semibold + "8:00–8:30 PM · 18 min left" 20 muted, one line · synopsis 20, 2 lines · `UP NEXT` 16 bold + next three "8:30 Kitchen Table" 18 muted · badges 16 at the line's right end · footer 18 muted (playback/cleanup message when there is one) | 15 / 10 / 8 / 9 sp |

Apple: this is `tvChannelList` inside the `.channelBrowser` branch of
`tvBrowseRegion` (1462–1472) with the fixed `0.34` replaced by the 620 pt
column and the `livePicture` / `focusedProgrammeDetails` / `channelSchedule`
stack reworked to the table; the `.guidePreview` branch with `browse ==
.list` now renders the same view. `LiveTvChannelRow` (475–536) becomes the
row above on tvOS (keep the iOS branch, §3.7). Android: `LiveTvOnNowList` +
`LiveTvBrowserRow` (907–983) restyled, used by the `GuidePreview` branch
(865–883) as well as `ChannelBrowser` (789–817), with `weight(0.34f)` →
`width(310.dp)`.

### 3.4 Guide — stage over grid

| Element | tvOS pt | Google TV dp |
|---|---|---|
| Stage height | 302 | 151 |
| Picture | 537 × 302 (16:9), left; chip · channel · LIVE top-left; "Select · Fullscreen" bottom-right; focusable | 268 × 151 |
| Details (right of the picture, 24 gap) | eyebrow "FOCUSED · 7.1 WABC · 8:30–9:00 PM" 14 bold accent · title 30 semibold · synopsis 20 × 3 lines · badges 16 · footer 18 muted "Now playing: City Beat · 18 min left · Select the picture to watch fullscreen" | 7 / 15 / 10 / 8 / 9 sp |
| Grid header | 34 tall: `‹ Now ›` chips in the channel column, then ticks 18 muted with a 1 px `outline` left rule per slot | 17 |
| Channel column | 200: 64 × 34 chip + number 20 semibold over callsign 16 muted | 100 |
| Slot width | `(contentWidth − 200) / 4` → 390 at 1760; **derived, never a constant**; `visibleSlots = 4` (2 h per page; paging moves 2 h) | `(contentWidth − 100.dp) / 4` |
| Row | 74; cells inset 3 × 5, 6 radius, 22 px title; airing `surfaceHi`; playing 1 px accent ring; focused 3 px `onBg` ring | 37 |
| Now line | 2 px `bad`, header bottom → last row | 1 dp |
| Rows visible | 6 (34 + 6 × 74 = 478); the grid takes the remaining height — **delete** the fixed `.frame(height: rows × rowHeight + 54)` (LiveTvView.swift:1578) | 6 |

Apple `LiveTvGridMetrics` (346–371), tvOS branch:

```swift
#else
static let rowHeight: Double = 74
static let channelColumnWidth: Double = 200
static let visibleSlots = 4
/// Slot width follows the screen: a hard-coded 300 left 35% of a 1920 pt
/// screen empty and could never fit 2 h.
static func pxPerSlot(contentWidth: Double) -> Double {
    (contentWidth - channelColumnWidth) / Double(visibleSlots)
}
#endif
```

`requestedHours` stays 6 (one 2 h page plus the server's hour of backfill
plus slack). Thread `geometry.size.width − 2 × padding` from
`tvBrowseRegion` into the two `LiveTvGuideGrid(...)` call sites (1355–1377,
1549–1577) and the temporary-guide one (1893); the iOS branch keeps `160 /
56 / 128 / 8`. Android: `LiveTvGridMetrics`
([LiveTvGuideUi.kt:66–73](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvGuideUi.kt))
gains

```kotlin
data class LiveTvGridDimensions(val slotWidth: Dp, val rowHeight: Dp, val channelColumnWidth: Dp)
fun LiveTvGridMetrics.forTelevision(contentWidth: Dp) =
    LiveTvGridDimensions((contentWidth - 100.dp) / 4, 37.dp, 100.dp)
val LiveTvGridMetrics.phone get() = LiveTvGridDimensions(160.dp, 56.dp, 132.dp)
```

and `LiveTvGuideGrid` takes a `dimensions` parameter; the television call
sites wrap the grid in `BoxWithConstraints` to supply `maxWidth`.

### 3.5 Over picture — same panel, new grid

Fullscreen video; opaque lower panel 520 pt (260 dp) tall with a 22 px
header line ("Guide · Focused: 9.1 WWOR · Overnight Movie · 9:00 PM" ·
spacer · `★ Favorites` · `Close`, compact buttons) and the §3.4 grid at 5
rows. The upper header stays as built (chip · title 30 · LIVE · meta 20 over
the existing gradient; the `.title3` at 1805 becomes `LiveTvType.title`).
The panel never auto-hides; Back/Close dismiss; the saved layout is
untouched. The temporary fullscreen guide (Apple 1878–1900; Android
1129–1169) uses the identical panel.

### 3.6 Type — one enum per client

```swift
enum LiveTvType {
    #if os(tvOS)
    static let title     = Font.system(size: 30, weight: .semibold)
    static let primary   = Font.system(size: 22, weight: .semibold)
    static let cell      = Font.system(size: 22)
    static let secondary = Font.system(size: 20)
    static let tertiary  = Font.system(size: 18)
    static let badge     = Font.system(size: 16, weight: .bold)
    static let eyebrow   = Font.system(size: 14, weight: .bold)
    #else
    static let title = Font.headline, primary = Font.subheadline.weight(.semibold),
               cell = Font.caption, secondary = Font.caption, tertiary = Font.caption2,
               badge = Font.system(size: 9, weight: .bold), eyebrow = Font.caption2.weight(.bold)
    #endif
}
```

Every `.font(...)` in `LiveTvView.swift` and `LiveTvFormatBadges` goes
through it; the iOS values are today's, so phones do not move. Android:
`object LiveTvTypography` with `title 15 sp · primary 11 sp · secondary
10 sp · tertiary 9 sp · badge 8 sp` for `FormFactor.Television` and the
current Material styles otherwise, applied in `LiveTvFocusedProgramme`,
`LiveTvBrowserRow`, `LiveTvGuideGrid`, `LiveTvOverlay`. Badges: the `size:
9` at LiveTvView.swift:402 becomes `LiveTvType.badge`.

### 3.7 Phones — stop stacking, reuse the web row

| Band | iOS (pt) | Android (dp) | Content |
|---|---|---|---|
| Nav bar | 44 (`.navigationBarTitleDisplayMode(.inline)`) | `TopAppBar` 64 | "Live TV" · search icon (reveals the existing `.searchable` field / a search `AlertDialog`) · more icon (Refresh channels · Hide protected · Retry cleanup while cleanup is unconfirmed · Leave) |
| Picture | full width × 9/16 (keep the `min(…, height × 0.4)` rule at 1013; drop the horizontal padding around it) | `heightIn` stays; full width | "● LIVE · 7.1 WABC" chip top-left; PiP and Fullscreen 30 pt icon buttons top-right; tap = toggle chrome, as today |
| Progress | 3 pt line | 3 dp | replaces the `ProgressView` in `nowBar` |
| Caption | 56 | 56 | title 15 semibold + "8:00–8:30 PM · 18 min left · Next: …" 12 muted (one line, ellipsis) · mute · info · stop as 32 pt icon buttons |
| Toolbar | 48 | 48 | segmented `On now · Guide · Favorites` (13 pt) · channel count 12 muted |
| List | the rest (≈ 4½ rows at 393 × 852) | ≈ 5 rows | the web `.lt-row`: min 78, `52 · 1fr · auto`, chip 52 × 32 (10.5 bold), name 13.5 bold, programme 13.5, 3 bar, "until 8:30 PM · Next: …" 12 muted, star right |

Deleted: `nowBar` (LiveTvView.swift:1734–1767) and `LiveTvNowBar`
(LiveTvScreen.kt:1004–1051); the iOS `actionBar` (1218–1240) and the
`filterBar` toggle row (1253–1254); Android's Back button, `headlineMedium`
title, `state.title`, `state.message` rows (323–330), the filter `FlowRow`
(464–487) and the always-visible `OutlinedTextField` (488–494). Pause stays
on the picture tap and in fullscreen. The phone Guide keeps the
selected-channel schedule; its channel picker and *Grid* toggle share one
44 pt row under the toolbar, and schedule rows are 44 pt `time 12 muted ·
title 14 · NOW` lines (today: `.headline` buttons with two lines,
1406–1418; Android 619–640).

## 4. File ownership — two sessions, no overlap

| PR | Owns | Never touches |
|---|---|---|
| `codex/live-tv-proportions-apple` | `clients/apple/Sources/LiveTvView.swift`, `LiveTv.swift` (the `presented` alias only), `Theme.swift` (the compact style only), `clients/apple/Tests/LiveTvTests.swift`, `clients/apple/project.yml` (build bump), `docs/apple-builds/<n>-live-tv-proportions.md` | anything under `clients/android`, the web tree, `LiveTvGuide.swift`, `LiveTvInputRouting.swift`, `PlayerRemoteAdapter.swift` |
| `codex/live-tv-proportions-android` | `clients/android/.../livetv/LiveTvScreen.kt`, `LiveTvGuideUi.kt`, `ui/components/TvFocus.kt` (the compact flag only), `livetv` tests, `app/build.gradle.kts` (versionCode) | anything under `clients/apple`, the web tree, `LiveTvGuide.kt`, `LiveTvInputPolicy.kt`, `LiveTvKeyAdapter.kt`, `LiveTvPlayer.kt` |
| docs PR (this file's status update) | `docs/features/LIVE-TV-NATIVE-LAYOUTS-STATUS.md`, `docs/README.md` row | code |

Both PRs edit `STATUS.md`'s top entry; take the conflict when the second
one merges rather than serialising the work.

## 5. Milestones

Each PR follows the workflow correction in
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md): open as draft (a
`WIP:` title on Forgejo), obtain exactly one adversarial review, address it,
mark ready — which is what starts the lane, there being no label since
2026-09-13 — and merge. Compile and focused proofs are manual
and happen *before* the PR opens — the lane is not a compiler.

### M1 · Apple — tvOS then iOS, one PR

1. `LiveTvType` (§3.6) and the compact button style (§3.2); replace every
   `.font(` in the file. Compile both platforms on `mba` with
   `make apple-build` (~15 s warm) before anything else.
2. Toolbar (§3.2): banner → toast + count; Return to live → focusable
   picture; Earlier/Now/Later → grid header chips.
3. `LiveTvGridMetrics` (§3.4) with width-derived `pxPerSlot`; delete the
   fixed grid frame; stage-over-grid for `browse == .guide`.
4. Side-by-side On now (§3.3) for both Preview states; row restyle;
   `TvLiveLayout.presented` (§3.1) and the two-entry sheet.
5. Over-picture panel (§3.5).
6. iOS (§3.7): delete `nowBar`/`actionBar`/toggle row; picture overlay
   chips + caption + toolbar; the web row; schedule rows.
7. Tests, in `LiveTvTests.swift` beside the existing 31: `pxPerSlot(
   contentWidth: 1760) == 390` and `window` spans 4 slots on tvOS; the
   `presented` alias maps `channel_browser` → `guide_preview` and the
   stored raw value round-trips unchanged; a source-contract assertion that
   `LiveTvView.swift` contains no `.subheadline`, `.title2`, `.title3` or
   `.callout` (the three semantic styles that inflate on tvOS — same
   mechanism as the existing source contracts in that file). Bump with
   `make apple-build-bump` and write the build note.

*Acceptance:* iOS + tvOS focused `LiveTvTests` green on the OS 26.5
simulators (`make apple-test` on `mba`, ~150 s); a 1920 × 1080 tvOS
simulator screenshot (`xcrun simctl io <udid> screenshot`) against nynuc's
lineup showing, in On now, one 48 pt toolbar row and ≥ 10 list rows beside
the picture, and in Guide, 6 grid rows under a 537 pt picture with 4 time
ticks; an iPhone portrait screenshot showing ≥ 4 rows while a channel plays.
Attach the three screenshots to the PR.

### M2 · Android — Google TV then phone, one PR

1. `LiveTvTypography` and the compact `TvTextButton`/`TvButton` flag.
2. Delete the Back/headline/title/message rows on television; toolbar per
   §3.2 with the search dialog.
3. `LiveTvGridMetrics.forTelevision` + `dimensions` parameter on
   `LiveTvGuideGrid`; `BoxWithConstraints` at the television call sites.
4. Side-by-side On now (`width(310.dp)`), row restyle, `GuidePreview`
   branch rendering it; `fromStorage` alias; two-entry dropdown.
5. Over-picture panel and the temporary fullscreen guide on the new grid.
6. Phone (§3.7): `TopAppBar`, picture chips, caption, toolbar, the web
   row, schedule rows; delete `LiveTvNowBar`, the filter `FlowRow` and the
   text field.
7. Tests in `tv.plurx.app.livetv.*` beside the existing 35:
   `forTelevision(880.dp).slotWidth == 195.dp` and `phone` unchanged;
   `fromStorage("channel_browser") == GuidePreview`; the existing
   `LiveTvGuideFocusTest` real-D-pad case still passes on the Streamer
   (focus targets did not move, only their sizes). `versionCode` 87 → 88.

*Acceptance:* `./gradlew :app:compileDebugKotlin` and the `livetv` JVM
tests green; `LiveTvGuideFocusTest` 1/1 on the Google TV Streamer;
`adb exec-out screencap -p` at 1080p showing the same counts as M1's
tvOS screenshot; a phone screenshot with ≥ 4 rows while playing.

### M3 · Status and the physical pass

Update [LIVE-TV-NATIVE-LAYOUTS-STATUS.md](LIVE-TV-NATIVE-LAYOUTS-STATUS.md)
with the two PR numbers, the build numbers and the screenshots' facts, and
give the GPT session the prompt in the review's §7 for the Apple TV + Siri
Remote and Streamer + D-pad walkthrough. Nothing here deploys; Paul installs
the fleet builds.

## 6. Traps the builder will meet

- **The build-number gate compares against the merge target at merge
  time** (`validation/mobile_versions`): if `main` claims a higher Apple
  build while the PR is open, re-bump before merging.
- **tvOS `List` rows and `.listRowBackground`** fight a fixed-width
  column; use `ScrollView` + `LazyVStack` for the 620 pt list so the row
  width is yours, and keep `.focused($focusedChannelId, …)` on the row —
  the restore coordinator keys on that.
- **The picture as a focus target** must not steal focus from the list on
  appear; give it `.focusable()` with no `prefersDefaultFocus`, and on
  Android a plain `focusable()` box — the `RequestInitialFocus` stays on
  the toolbar.
- **Compose `Column` overflow:** a `fillMaxSize()` child of a `Column` takes
  the *remaining* height, so the 302/151 stage must be a fixed
  `height(151.dp)`, not a weight, or it shrinks when the toolbar wraps.
- **Source-contract tests break on reformatting** (see #814): pin the new
  assertions to whole tokens (`.font(LiveTvType.`) rather than multi-line
  chains.
- **Screenshots need a server:** the tvOS simulator on `mba` can reach
  nynuc:32400; the lineup and guide load without a tuner, so judge the
  list/grid first and tune one channel only if a tuner is free.
