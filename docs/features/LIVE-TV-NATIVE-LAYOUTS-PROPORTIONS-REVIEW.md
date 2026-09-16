# Live TV native layouts — why the TV and phone screens look wrong, and the numbers that fix them

**Status:** review + fix spec — **built and merged 2026-09-12** · **Reviews:** the layouts
shipped by [LIVE-TV-NATIVE-LAYOUTS-STATUS.md](LIVE-TV-NATIVE-LAYOUTS-STATUS.md)
(Apple build 144 / Android versionCode 87, `main` at `10f2afe60`) · **Written:** 2026-09-12


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

Companion to the implementation plan above (what was asked for) and
[LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) (the web design
everyone agrees works) — this is *why the native screens came out with the
wrong proportions, and exactly what to change*. Renders live in
[`../mockups/live-tv/`](../mockups/live-tv/) as `proportions-<surface>-<state>.png`
(for example [proportions-tv-proposed-on-now.png](../mockups/live-tv/proportions-tv-proposed-on-now.png))
and on the design canvas "Live TV Proportions"; every "as built" render is drawn from the constants in
the source, not from a device screenshot, so §6 says what still has to be
confirmed on the Apple TV and the Google TV Streamer.

Read §2 first — it is the whole diagnosis in five lines. §3 is the build
spec; a builder executes it milestone by milestone (§5) and stops to flag
anything that would need the guide reducers, the lease, or the input contract
to change, because none of this touches them.

## 1. What the renders show

The web page (`crates/plurxd/src/web/index.html`, CSS at lines 407–482) has
one 40 px toolbar, a stage where the picture is 2/3 of the width at a real
16:9, a 1/3 show-info column, and a channel list of 78 px rows in 13.5 px
type. The native TV pages carry the same data through the same reducers and
arrive at this:

| Surface, default layout (Guide + preview) | Picture | Chrome above the content | Channel rows on screen | Row width |
|---|---|---|---|---|
| Web, 1200 px wide, List view | 800 × 450 · 67% of width | 40 px | ~6 of 78 px beside the picture | 366 px column |
| Apple TV, On now | **437 × 246 pt · 23% of width, 5% of the screen** | status banner 62 + toolbar 66 + tab bar 140 = 268 pt | **3½** of 125 pt | **1720 pt, edge to edge** |
| Apple TV, Guide | same 437 × 246 pt | 268 pt (toolbar now 10 buttons) | 5 of 82 pt, rows 6–7 below the screen edge | grid 1110 pt, right 35% empty |
| Google TV, On now | **197 × 111 dp (394 × 222 px) · 20% of width** | Back + headline + title + message + toolbar ≈ 180 dp | **2½** of ~85 dp | 928 dp, edge to edge |
| iPhone 15 Pro, On now, playing | 361 × 203 pt (fine) | 318 pt *between* the player and the list | **≈1** of ~80 pt | — |
| Android phone (412 × 915 dp), playing | 380 × 214 dp (fine) | ≈ 470 dp incl. the player | **≈2** | — |

**How to read it:** the picture Paul called "an afterthought" is 5% of the
Apple TV screen and 4% of the Google TV screen; the web gives it 34% of the
page. The "channels three feet long" are rows whose only right-hand content
is a star and a play glyph 1.5 m from the channel name on a 65" panel. The
phones are not a proportion problem at all — the player is the right size —
they are a *stacking* problem: five things sit between the picture and the
list, so the list is one row tall while anything plays.

## 2. Why it came out this way — five causes, all in the view code

1. **tvOS semantic text styles are 2–2.5× their iOS sizes, and the views use
   iOS-shaped styles.** `LiveTvChannelRow` sets the title in
   `.subheadline` ([LiveTvView.swift:507](../../clients/apple/Sources/LiveTvView.swift)),
   the details title in `.title2` (line 1526), copy in `.callout` (1532,
   1534) and the status banner in `.callout` (1107). On iOS those are
   15 / 22 / 16 pt. On tvOS Apple's Dynamic Type table makes them
   **38 / 57 / 31 pt**, `.body` 29 and `.caption` 25 (re-verify with
   `UIFont.preferredFont(forTextStyle:)` on the tvOS simulator — the
   ratios are the point, not the last digit). The spec asked for 28–32 px
   titles and 22–24 px row copy; every row is 60–80% over. The one place
   the code names a size, the format badges at `size: 9` (line 402), goes
   the other way and is unreadable from a sofa.
2. **The page stacks chrome that the web page never draws.** The status
   message is a permanent full-width banner (1105–1119); the toolbar is
   seven `TVReadableButtonStyle` buttons at `minHeight: 66` with 28 pt
   horizontal padding ([Theme.swift:144–146](../../clients/apple/Sources/Theme.swift))
   and grows to ten in Guide view (1264–1276); the TabView adds ~140 pt
   above all of it. On Android the outer `Column` draws Back, a
   `headlineMedium` "Live TV", `state.title` and `state.message` before the
   browser even starts ([LiveTvScreen.kt:323–330](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt)),
   then a `FlowRow` of seven controls including a 260 dp text field
   (710–755). ~270 pt / ~180 dp are spent before any content.
3. **The picture is sized by a height fraction, then aspect-fit inside a
   width fraction.** Apple: the top strip is `geometry.size.height * 0.28`
   (1441) and the picture gets `geometry.size.width * 0.4` (1439) with
   `.aspectRatio(16/9, contentMode: .fit)` (1485), so the height wins:
   246 pt tall → 437 pt wide, floating in a 688 pt box. Android:
   `weight(0.34f)` of what is left after the chrome (867) → 111 dp tall
   → 197 dp wide inside a 456 dp black box (871). The spec's "roughly the
   lower two-thirds for the guide" was honoured; the preview paid for it.
4. **"Guide + preview" with the On-now list is the worst pairing, and it is
   the default.** `browse` defaults to `.list` (924) and `tvLayout` to
   `.guidePreview` (925), so the first thing a viewer sees is the top strip
   plus a *full-width* `List` (1584–1621) whose row is an `HStack` with a
   `Spacer` pushing a star and `play.circle` to the far edge (525–529) and a
   progress bar pinned to `maxWidth: 220` (517) — stranded in the middle of
   a 1720 pt row. Android's `LiveTvBrowserRow` is a `fillMaxWidth()`
   `TextButton` (963–967), so its text is accent-coloured link text across
   928 dp with a full-width progress bar. The Channel browser layout, which
   puts the list in a 34% column beside the picture (1462–1472; Android
   789–817), is the arrangement a list wants — it just is not the default.
5. **The grid uses phone-shaped metrics on the TV.** Apple's tvOS branch of
   `LiveTvGridMetrics` (354–357) hard-codes 300 pt per half hour × 3 slots +
   210 = 1110 pt, so the grid fills 58% of the width and the rest is empty;
   `tvBrowseContent(rows: 7)` then gives it a fixed frame of 7 × 82 + 54 =
   628 pt (1578) into ~430 pt of remaining height, so the VStack overflows
   the bottom of the screen (confirm on device — SwiftUI does not clip a
   fixed frame, it draws past the safe area). Android has one
   `LiveTvGridMetrics` object with no form-factor branch
   ([LiveTvGuideUi.kt:66–73](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvGuideUi.kt)):
   56 dp rows are 112 px on a TV, 50% taller than the spec's 76–88 px.

The web page avoids all five by construction: CSS pixels are the same
size everywhere, the stage is a grid with the picture as the 2fr column,
and the row is a `52px minmax(0,1fr) auto` grid whose right column holds a
star and nothing else.

## 3. The fix — one geometry for both TVs, one for both phones

Design px at 1920 × 1080. tvOS points are these numbers as-is; Android TV
dp are these numbers ÷ 2 (the Streamer and every 1080p Google TV report
320 dpi). Content box: 64 px side insets, so 1792 wide on Google TV and
1760 on tvOS (whose 80 px overscan inset is already applied by the system).

### 3.1 The rule that replaces the three-layout menu

**Lists are tall and narrow; grids are wide.** So the arrangement follows
the browse view, not a separate layout choice:

```
 On now  (list)                          Guide  (grid)
 ┌──────────┬──────────────────────┐    ┌────────┬────────────────────────┐
 │ list     │ picture 16:9         │    │ picture│ focused programme      │
 │ 620 px   │ 1116 × 628           │    │ 30% w  │ title · time · synopsis│
 │ 72 px    ├──────────────────────┤    ├────────┴────────────────────────┤
 │ rows     │ title · time · left  │    │ ‹ Now ›  8:00   8:30   9:00  9:30│
 │ × 11     │ synopsis · UP NEXT   │    │ 6 rows × 74 px, 2 h visible     │
 └──────────┴──────────────────────┘    └─────────────────────────────────┘
```

This makes today's "Guide + preview" and "Channel browser" the same thing,
which is the honest outcome: they only ever differed in which browse view
they opened with. **Recommendation:** collapse the Layout menu to two
entries — *Preview* (the two arrangements above) and *Over picture*
(fullscreen video, opaque lower panel) — and keep the saved-per-device
semantics. If the three names must survive for continuity, then
"Guide + preview" with On now selected must render the side-by-side
arrangement; that is the minimum change and it is what the renders show.
This is a product call: it is flagged here, not decided.

### 3.2 Toolbar — one row, 48 px

`[ On now | Guide ]` segmented · muted "12 channels · 1 of 2 tuners" ·
spacer · `★ Favorites` · `⌕ Search` · `Layout` · `More`. Labels 22 px
semibold, buttons 48 px tall with 20 px horizontal padding, 10 px radius.

- The status banner leaves the page. The channel count goes into the
  toolbar as above; errors and cleanup states become a transient toast
  (or the existing message inside the details panel's footer line when it
  is playback-related); "Cached lineup (Ns old)" belongs in More → Refresh
  channels as its subtitle.
- **Return to live** leaves the toolbar: the picture itself is a focus
  target and Select opens fullscreen (a "Select · Fullscreen" hint sits in
  its corner). That is one fewer 260 pt button and the natural gesture.
- **Earlier / Now / Later** move into the grid's header row, in the channel
  column, as three small chips (see 3.4). They belong to the grid, not the
  page, and they were what pushed the Guide toolbar to ten buttons.
- Apple: `TVReadableButtonStyle` gets a `compact` variant (padding 20 × 10,
  `minHeight: 48`, `Font.system(size: 22, weight: .semibold)`); the
  existing style stays for sheets. Android: `TvTextButton` gets
  `defaultMinSize(minHeight = 24.dp)` and `contentPadding = 10.dp × 5.dp`
  on television; the 260 dp `OutlinedTextField` becomes a Search button
  that opens the existing search sheet/dialog, as the Apple client already
  does.

### 3.3 On now — list column beside the picture

| Element | Value |
|---|---|
| List column | 620 px wide, 4 px row gap, scrolls vertically; focus stays inside the column |
| Row | 72 px tall; grid `84px · 1fr · auto`, 14 px gaps, 10 px radius; focused = 3 px `onBg` ring + `surface` fill; playing = 1 px accent ring + `surfaceHi` fill + "● LIVE" 14 px |
| Station chip | 84 × 40, 18 px bold `muted` on `surfaceHi` (the web's `.lt-chip`, scaled ×1.6) |
| Primary | "7.1 WABC" 22 px semibold; secondary: programme title 20 px; 3 px progress bar under the text column; right column: star + "until 8:30 PM" 18 px muted |
| Removed from the row | the play glyph (the row *is* the button), the lock glyph (a protected row dims to 0.55 and its secondary line says "Protected · not playable"), the `Spacer` |
| Picture | 1116 × 628 (16:9) at the top of the right column; chip + channel + LIVE top-left; progress line along its bottom edge; focusable |
| Details under the picture | title 30 px semibold + "8:00–8:30 PM · 18 min left" 20 px muted on one line; synopsis 20 px, 2 lines max; `UP NEXT` line 18 px with the next three programmes; format badges 16 px at the right end of that line |

Total height 48 + 16 + 628 + 16 + ~120 = 828 of the ~850 px below the
tvOS tab bar. The renders show 11 rows; the spec's "5–7 rows" was a floor,
not a target.

### 3.4 Guide — stage over grid

| Element | Value |
|---|---|
| Stage | 302 px tall: picture 537 × 302 (16:9, 30% of width) left; details right: 14 px accent eyebrow "FOCUSED · 7.1 WABC · 8:30–9:00 PM", title 30 px, synopsis 20 px × 3 lines, badges 16 px, footer line 18 px muted "Now playing: … · Select the picture to watch fullscreen" |
| Grid header | 34 px: `‹ Now ›` chips (16 px) in the channel column, then time ticks 18 px muted with a 1 px left rule per slot |
| Channel column | 200 px: 64 × 34 station chip + number 20 px semibold over callsign 16 px muted |
| Slot width | **derived**: `(contentWidth − 200) / 4` → 390 px at 1760, 2 h visible per page (`visibleSlots = 4`); paging moves by 2 h |
| Row | 74 px; cells inset 3 px horizontally / 5 px vertically, 6 px radius, title 22 px; airing = `surfaceHi`; playing = 1 px accent ring; focused = 3 px `onBg` ring |
| Now line | 2 px `bad` red, from the header's bottom to the last row |
| Rows visible | 6 (34 + 6 × 74 = 478 of the 488 px left under the stage) |

Constants to change: Apple `LiveTvGridMetrics` tvOS branch → `rowHeight
74`, `channelColumnWidth 200`, `visibleSlots 4`, and `pxPerSlot` becomes a
function of the available width (thread `geometry.size.width` from
`tvBrowseRegion`; the reducer already takes `pxPerSlot` as a parameter, so
nothing below the view changes). Drop the fixed `.frame(height: rows × …)`
in `tvBrowseContent` — the grid takes the remaining height. Android: give
`LiveTvGridMetrics` a television variant (`rowHeight 37.dp`,
`channelColumnWidth 100.dp`, `slotWidth` from `BoxWithConstraints`) and keep
the phone values as they are. The overlay guide (3.5) and the temporary
fullscreen guide use the same component with `rows: 5`.

### 3.5 Guide over picture — unchanged shape, new metrics

Fullscreen video; opaque lower panel 520 px tall (48%) with one 22 px
header line ("Guide · Focused: 9.1 WWOR · Overnight Movie · 9:00 PM",
Favorites and Close at the right) and the §3.4 grid at 5 rows. The top
strip keeps the existing programme header (chip · title 30 px · LIVE ·
meta 20 px) over a gradient. Nothing else changes: the panel never
auto-hides, Back closes it, the saved layout is untouched.

### 3.6 Type scale — name the sizes, never the styles, on tvOS

| Role | tvOS | Android TV | Where it replaces |
|---|---|---|---|
| Programme title (details, fullscreen header) | `Font.system(size: 30, weight: .semibold)` | 15 sp / 600 | `.title2` at 1526, `.title3` at 1805 |
| Row primary, toolbar labels, grid cells | 22 semibold / 22 regular | 11 sp | `.subheadline` 507, `.body` 1528, `.caption` 690 |
| Secondary copy, synopsis | 20 | 10 sp | `.callout` 1532/1534, `.caption` 514 |
| Tertiary: times, "until", ticks, UP NEXT | 18 | 9 sp | `.caption2` 518/652/781 |
| Badges | 16 bold | 8 sp | `size: 9` at 402 |

Put them in one `LiveTvType` enum per client (Apple: `#if os(tvOS)` sizes
vs iOS styles; Android: a `LiveTvTypography` object keyed on `FormFactor`)
so the next screen cannot reach for `.subheadline` again. On the phones
the semantic styles are the right size and stay.

### 3.7 Phones — stop stacking, then reuse the web row

Same shape on iOS and Android; the render is the iPhone.

| Band | Height | Content |
|---|---|---|
| Nav bar | 44 pt / 64 dp | "Live TV" · search icon · more icon (Android: `TopAppBar` with the back arrow; delete the Back `TextButton`, the `headlineMedium`, `state.title` and `state.message` rows at 323–330) |
| Picture | full-bleed 16:9 (221 pt at 393) | "● LIVE · 7.1 WABC" chip top-left; PiP and Fullscreen icon buttons top-right; **no** title/progress text stacked beneath it |
| Progress | 3 pt line under the picture | the web's `.lth-bar` |
| Caption | 56 pt | title 15 semibold + "8:00–8:30 PM · 18 min left · Next: …" 12 muted on the left; mute · info · stop as 32 pt icon buttons on the right. Pause lives on the picture (tap) and in the fullscreen overlay |
| Toolbar | 48 pt | segmented `On now · Guide · Favorites` (13 pt) · channel count |
| List | the rest — 4½ rows at 393 × 852 | the web row verbatim: 78 pt min, `52px · 1fr · auto`, 13.5 / 13.5 / 12 pt, 3 pt bar, star in the right column |

Removed from the page: the 6-line `nowBar` (Apple 1734–1767; Android
1004–1051), the `statusMessage` (→ toast; the tuner-released message
stays because it is actionable, but as a toast), the `actionBar`
"Refresh channels / Retry cleanup" (1218–1240 → More menu; *Retry cleanup*
appears only while cleanup is unconfirmed, as the toast's action), the
Favorites/Hide protected toggle row (→ segmented control / More), the
always-visible search field (Apple `.searchable` already exists — keep it
behind the nav-bar icon; Android: same, replacing the 56 dp
`OutlinedTextField` at 488–494). The iOS player height rule
`min(width × 9/16, height × 0.4)` at 1013 is fine and stays; drop the
`.padding()` around the player so it is full-bleed like the mockup.

Guide on a phone keeps the selected-channel schedule (the right call for a
portrait screen) but the channel picker and the *Grid* toggle share one
row under the toolbar, and schedule rows become 44 pt `time 12 · title
14 · NOW` lines instead of `.headline` buttons with a second line.

## 4. Non-goals — the fence for whoever builds this

- **No reducer, lease, guide-fetch, or input-contract changes.**
  `LiveTvGuideReducer`, `LiveTvLease`, `LiveTvInputRouting` /
  `LiveTvInputPolicy` and the generated input tables are untouched; every
  change in §3 is a view constant or a view arrangement. If something in
  §3 seems to need them, stop and flag it.
- **No new layout state.** The per-device saved layout, the persisted
  browse view, and the focus-restore coordinators stay as they are; the
  side-by-side On now reuses the Channel browser code path rather than
  adding a fourth arrangement.
- **No metadata removed.** Badges, source facts and reception meters keep
  their places (rows, details, Info) at the §3.6 sizes; they change size,
  not presence.
- **No phone layout selector**, per the implementation plan §6.
- **No DVR, no theme work, no app-wide navigation change** — unchanged
  from the plan's own non-goals.

## 5. Milestones — each ends with something you can look at

Superseded by [LIVE-TV-PROPORTIONS-IMPLEMENTATION.md](LIVE-TV-PROPORTIONS-IMPLEMENTATION.md),
which folds these into two PRs with file ownership; kept here as the
original sequencing.

1. **tvOS type + chrome.** `LiveTvType`, compact `TVReadableButtonStyle`,
   status banner out, Earlier/Now/Later into the grid header, Return to
   live replaced by the focusable picture. *Acceptance:* focused tvOS
   `LiveTvTests` green, plus a 1920 × 1080 screenshot from the tvOS 26.5
   simulator (`xcrun simctl io <udid> screenshot`) on `maca` showing one
   toolbar row and no banner.
2. **tvOS geometry.** Side-by-side On now (620 px column, 72 px rows),
   stage-over-grid Guide with width-derived slots, overlay guide on the
   same grid. *Acceptance:* the same screenshot shows ≥ 10 list rows in On
   now and 6 grid rows with a 537 px picture in Guide; the focused-cell →
   toolbar boundary and restore tickets still pass their tests.
3. **Google TV, same two steps.** TopAppBar-free chrome, compact TV
   buttons, television `LiveTvGridMetrics`, `LiveTvBrowserRow` restyled
   as the §3.3 row. *Acceptance:* `tv.plurx.app.livetv.*` JVM tests green,
   `LiveTvGuideFocusTest` on the Streamer green, `adb exec-out screencap`
   at 1080p showing the same counts.
4. **Phones.** iOS then Android per §3.7. *Acceptance:* iOS 28 focused
   tests green; a portrait simulator screenshot with ≥ 4 rows while
   playing on both.
5. **Physical pass.** Apple TV with the Siri Remote and the Streamer with
   its D-pad, both layouts, both browse views, one fullscreen round trip
   each — the walkthrough the status doc still lists as unproved. Hand the
   GPT session the prompt in §7.

Bump Apple `CURRENT_PROJECT_VERSION` and Android `versionCode` once per
client PR, per `validation/mobile_versions`.

## 6. What this review did not do

- No device screenshots were taken. Every "as built" number is computed
  from the constants cited in §2 and from Apple's published tvOS Dynamic
  Type sizes; the tvOS default `.padding()` value and the exact TabView
  height were estimated (20 pt and 140 pt). The conclusions do not depend
  on those two.
- The 628 pt fixed-height guide overflowing the screen (§2.5) is inferred
  from the arithmetic, not observed; it is the first thing to check on the
  Apple TV because it changes whether the bottom guide rows are reachable
  at all today.
- Nothing was compiled or tested; nothing in the repo changed. This
  document and the PNGs are the deliverable.

## 7. Prompt for the GPT session (physical verification, before and after)

> On the Apple TV (media1:32400) and the Google TV Streamer, open Live TV
> and, for each of Layout = Guide + preview and Channel browser, with On
> now and then Guide selected: take a screenshot at native 1080p, count the
> channel rows fully visible, measure the picture's width as a fraction of
> the screen, and note whether the bottom of the guide grid is cut off by
> the screen edge. Then pick the last channel row with the D-pad / Siri
> Remote and report whether focus can reach it. Report the eight
> screenshots and the counts as a table; do not change any settings.
