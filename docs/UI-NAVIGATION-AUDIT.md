# UI navigation audit — why the player feels different on every client

**Status:** findings settled, rulings taken · **Snapshot:** every anchor below
is `file:line` at commit `18886477` (main, 2026-09-01) — verify with
`git show 18886477:<path>` because later commits have shifted line numbers ·
**Written:** 2026-09-02 · **Leads to:** [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md)
(the rule) and [PLAYER-INPUT-CONTRACT-PLAN.md](PLAYER-INPUT-CONTRACT-PLAN.md)
(the build).

Companion to [CLIENTS.md](CLIENTS.md) (which client runs where) — this is
*what each player does with a press today, where the three disagree, and why
fixing them one report at a time never converges*. The reports that
prompted it: "the seek bar can't really be used", "navigating through the
buttons selects it and makes it skip forward or back", "the playback UI is
different on everything when it should be the same". All three are true and
all three come from the same five mechanisms in §1. Nothing here is a
design proposal; the proposal is the contract.

---

## 1. Five mechanisms account for every report

**M1 — the same press means "move focus" for four seconds and "seek"
afterwards.** On tvOS the chrome hides 4 s after the last focus change
(`clients/apple/Sources/PlayerView.swift:682`, `:908-926`, `:959`) and on
Android TV 3.8 s after the last `poke()`
(`clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt:918-923`).
While hidden, every directional press *seeks* — ±10 s left/right, ±30 s
up/down — and only then reveals: tvOS routes the hidden surface's
`onMoveCommand` straight to `.seek` (`PlayerView.swift:770`, `:86-87`,
`:1186-1194`); Android consumes all four directions on the bare surface
(`PlayerScreen.kt:335-338`, `:974-981`). Nothing on screen marks the
boundary, so a viewer who pauses to read the row and presses Right again
has just skipped 10 s and lost their place in the row (focus lands on
Play/Pause: `PlayerView.swift:1178-1184`; `PlayerScreen.kt:953-957` then
`:1259-1260`). Both behaviours are pinned as intended by tests
(`clients/apple/Tests/AppleClientTests.swift:2312-2324`;
`clients/android/app/src/test/java/tv/plurx/app/player/PlayerPolicyTest.kt:19-29`),
which is why they survived every patch.

**M2 — the seek bar sits in the same row as the buttons, and horizontal
input on it is a seek.** Android's transport row is
`[⟲10][▶][⟳10] 0:00 [———slider———] 1:59:00` (`PlayerScreen.kt:1339-1362`).
The slider is Material3's `Slider` with no `steps`
(`:1457-1461`); a focused Material slider consumes DPAD left/right to move
its value by 1 % of the range and fires `onValueChangeFinished` on key-up,
which the screen maps to a committed seek (`:1459-1460` → `:1110-1111` →
`Controller.kt:586-618`, a server session reopen on transcode and remux).
So arriving on the bar from `⟳10` and pressing Right once is a 72 s jump on
a two-hour film, and no horizontal press ever leaves the bar. On tvOS the
bar is a hand-routed focus stop (`PlayerView.swift:1894-1958`) that only
seeks once *engaged* by Select (`:1939-1942`) — but engagement's only
visual is the 8 pt bar growing to 12 pt (`:1931`) inside a focus ring the
theme test caps at 1.5 pt (`Theme.swift:262-266`,
`AppleClientTests.swift:6806`), and while engaged Left/Right never leave
the bar (`:90-97`). The web bar is a `<div role="slider" tabindex="0">`
(`crates/plurxd/src/web/index.html:3039`) whose own handler knows only
Home/End (`:9061-9064`); arrows reach the global handler, which seeks and
`preventDefault()`s whenever the target is not a `<button>` (`:10786`), so
on any browser where arrows move focus the bar is a one-way door.

**M3 — three different chrome-state machines.** Auto-hide delay: tvOS 4 s,
Android 3.8 s, web 2.6 s (`PlayerView.swift:682`; `PlayerScreen.kt:920`;
`index.html:8869`). What holds the chrome: tvOS — scrubbing, stream change,
engaged bar, *focus resting on* audio/subs/quality but not on any other
button (`PlayerView.swift:1048-1056`, `:1118-1128`); Android — any open
panel, never while paused (`PlayerScreen.kt:919`); web — an open `#pmenu`,
never while paused (`index.html:8874-8875`). What does *not* hold it: tvOS
hides under the failure view and the stats panel and moves focus to the
invisible reveal surface (`PlayerView.swift:762`, `:1160-1164`), after which
the next press seeks a failed player. Where focus goes on reveal: tvOS and
Android always Play/Pause (`PlayerView.swift:1182`; `PlayerScreen.kt:1259`);
web nowhere — the player never takes focus (`index.html:7236-7266`), so the
detail page's Play button behind the modal keeps it and the next Space
re-launches playback through `PLAY_OPEN_GATE` (`:7237`, `:10782`).

**M4 — the back key has a different precedence on every client.** tvOS
Menu: engaged bar → stats → hide chrome → exit (`PlayerView.swift:961-972`),
so an engaged bar under a Mini stats strip takes four presses to leave.
Android BACK: panel → hide chrome → exit (`PlayerScreen.kt:322-326`,
`:742-751`). Web Escape: always closes the whole player, menu or stats open
or not (`index.html:10784`); nothing closes `#pmenu` except choosing
(`:9575-9627`, no Escape, no click-outside).

**M5 — the playback-info panel has no shared row list.** The three
clients agree on the mode names Mini · Standard · Debug and on nothing
else (`PlayerView.swift:2216-2230`; `PlayerScreen.kt:306-310`;
`index.html:3039`). §4 has the matrix; the short version is that the same
datum wears four labels, two units and three formats depending on which
screen you open, and each client omits rows the others consider basic.
Navigation inside it is M3 and M4 again: tvOS parks focus on the invisible
reveal surface 4 s after the panel opens (`PlayerView.swift:1160-1164`),
Android TV swallows the first Select and re-shows the transport under the
panel on every D-pad press (`PlayerScreen.kt:987-991`, `:1002-1009`), and
the web closes the whole player on Escape (`index.html:10784`).

Everything in §3 and §4 is one of these five wearing a different coat.

---

## 2. The three players side by side

| | tvOS | iOS | Android TV / phone | Web |
|---|---|---|---|---|
| Row grammar | one row: transport · time · bar · time · options (`PlayerView.swift:1291-1301`) | wide: same row + PiP + ✕; narrow: bar row above transport row (`:1661-1694`) | ≥700 dp: transport · time · slider · time; <700 dp: transport / slider / times (`PlayerScreen.kt:1338-1387`); options in a *top* row (`:1262-1284`) | top bar of `.pbtn` + bottom `#ptransport` with the bar mid-row (`index.html:3039`) |
| Seek bar | custom focusable capsule, click-to-engage, 10 s steps (`:1894-1958`) | SwiftUI `Slider`, seek on release (`:1696-1717`) | Material3 `Slider`, 1 %/press, seek on key-up (`:1447-1482`) | custom div, pointer seek on release, Home/End only (`:9046-9065`) |
| Hidden-chrome L/R | seek ±10 (`:770`) | n/a | seek ±10 (`:335-336`) | seek ±10 (`:10786`) |
| Hidden-chrome U/D | seek ±30 (`:65-66`) | n/a | seek ±30 (`:337-338`) | seek ±30 (`playback-policy.js:747-760`) |
| Auto-hide | 4 s | 4 s | 3.8 s | 2.6 s |
| Hide while paused | yes (`:1048-1056` ignores `isPlaying`) | yes | no (`:919`) | no (`:8875`) |
| Play/Pause key with chrome up | **dropped** — only handler is on the hidden surface (`:771`); no `MPRemoteCommandCenter` on tvOS (`PlayerController.swift:5505`) | works via remote commands | works (`:983-986`) | Space/K unless a button is focused (`:10785`) |
| Back precedence | engaged → stats → hide → exit | ✕ exits | panel → hide → exit | Escape exits, always |
| Options set | audio · subs · quality · autoplay · info (`:1728-1737`) | same + PiP · ✕; narrow folds into "More" (`:1853-1890`) | tracks · settings (quality, sync, auto-skip, autoplay) · info · PiP (`:1270-1283`) | audio · subs · quality · sync · stats · auto-skip · autoplay · AirPlay · close; bottom row repeats audio/CC + info + PiP + fullscreen (`:3039`) |
| Focus on reveal | Play/Pause always | n/a | Play/Pause always (`Controls` recomposed, `:1081`) | none — never takes focus |
| Menus trap focus | system `Menu` (yes) | popover (yes) | **no** — scrim is `canFocus=false` with no `focusGroup`; controls stay composed under it (`Controller.kt:1625-1641`; `PlayerScreen.kt:2606-2617`) | **no** — focus never enters `#pmenu`; no `aria-expanded` |
| Offline player | same view | same view | **stock Media3 controller** (`OfflinePlayerScreen.kt:87`, `useController = true`) — different buttons, key map, timeout | n/a |
| Tests pinning input | routing table rows (`AppleClientTests.swift:2269-2357`) | auto-hide predicate (`:5498-5530`) | seek map + back precedence (`PlayerPolicyTest.kt:12-39`); slider vertical exit (`PlayerControlsFocusTest.kt:53-89`, asserts nothing about seeking) | `seekDeltaSeconds` only (`tests/playback/web-policy.test.js:1658-1662`); no test opens the player |

Read across a row and the "different on everything" report writes itself.
Read down the Android column and the seek-bar report does. The reason no
single fix has stuck is that each cell was set independently, by a
different PR, with a test that pins the cell rather than the row.

---

## 3. Per-client findings

Ranked within each client. "Proven" means a test in the tree executes the
path; "source" means the behaviour is plain in the code; "inferred" means it
depends on framework semantics no test here exercises.

### 3.1 Apple (tvOS unless noted)

1. **Hidden-chrome directional press seeks** (`PlayerView.swift:761-776`,
   `:770` → `:1186-1194` → `:86-87`). HIGH. Proven
   (`AppleClientTests.swift:2312-2324`). Mechanism M1.
2. **Play/Pause remote button is inert whenever chrome is visible**
   (`:771` is the only `onPlayPauseCommand`; `PlayerController.swift:5505`
   is `#if os(iOS)`). HIGH. Source. Second press within 4 s of the first
   does nothing.
3. **Auto-hide runs under the failure view and the stats panel and steals
   focus** — `shouldAutoHideControls` consults neither `failed` nor
   `showStats` (`:1048-1056`); `hideControls` then parks focus on the
   invisible reveal surface (`:1160-1164`), which renders whenever
   `!controlsVisible` regardless of `failed` (`:762` precedes `:787`).
   HIGH. Source. `testPlaybackInfoStaysVisibleAfterControlsAutoHide`
   (`:5532-5540`) proves the panel survives, nothing tests where focus went.
4. **Engaged bar is a horizontal focus trap with sub-perceptual feedback**
   (`:1939-1942`, `:1931`, `:90-97`; ring capped at 1.5 pt by
   `AppleClientTests.swift:6806`). HIGH. Routing proven (`:2276-2310`), UX
   inferred. Mechanism M2.
5. **Menu precedence chain reaches four presses** (`:961-972`). MEDIUM.
   Source. Mechanism M4.
6. **`DetailView` re-grabs focus to Play/Resume 120 ms after every
   appearance** (`DetailView.swift:764-784`) — returning from an episode
   or the player yanks focus off the shelf card you came from. MEDIUM.
   `.task` lifecycle inferred.
7. **Season episode cards on tvOS play instead of opening detail**
   (`Components.swift:313-322`, `:76-78`). MEDIUM. Proven
   (`AppleClientTests.swift:6575-6626`). Deliberate, but it removes a
   route every other surface has.
8. **Chrome-hold depends on which button focus rests on** — held on
   audio/subs/quality, not on Play/Pause, Skip, Autoplay or Stats
   (`:1118-1128`). LOW. Source. Mechanism M3.
9. `progressRightControl` and `playbackOptionGroup` disagree on PiP's
   condition (`:1961-1964` vs `:1730`). LOW. Moot on tvOS.
10. `DetailView.playbackActions` (`DetailView.swift:2405-2436`) is dead —
    reachable only from a tvOS branch `content()` never takes (`:971-976`).
    LOW.
11. iOS stats Debug backdrop swallows the chrome-toggle tap; Standard does
    not (`PlayerView.swift:2788-2800`). LOW.

Settled and not worth re-auditing: the un-engaged visible bar never seeks
on Left/Right — it routes to `skipForward` / the right neighbour
(`:90-97`, proven `:2276-2310`). `onMoveCommand` on a focused view
suppresses the system focus move (inferred from the routing's explicit
`.focus(...)`/`.ignore` outcomes and the "up should remain inert" test at
`:2336`); there is no double-fire. Step sizes 10/10/30/30 proven at
`:2269-2274`. Relative seeks accumulate (`PlayerController.swift:1991-1998`,
proven `:2239-2267`).

### 3.2 Android

1. **The slider is a horizontal focus trap that seeks on every press**
   (`PlayerScreen.kt:1457-1475`; nothing focusable follows it, `:1361`,
   `:1484-1491`; no `focusProperties` routing anywhere in the file). HIGH.
   Source; the 1 %-per-press step is Material3 library behaviour, not
   visible in this tree, corroborated by the author's own vertical-only
   guard and the test name `verticalDpadLeavesTheProgressSliderWithoutSeeking`
   (`PlayerControlsFocusTest.kt:53`). Mechanism M2.
2. **Three player UIs in one app** — online custom `Controls`
   (`:1234-1392`, `useController = false` at `:1018`); offline stock
   Media3 controller (`OfflinePlayerScreen.kt:87`); and the online
   transport reflows at 700 dp of *content width* (`:1338`), not on
   `FormFactor` (`ui/Layout.kt:14-23`), so phone-landscape, tablet and TV
   each differ. Panels disagree on dismissal chrome: Tracks has no close
   button (`Controller.kt:1602-1743`), Settings an ArrowBack "Close"
   (`:2620-2625`), Info an `X` (`:1788-1794`). HIGH. Source.
3. **Settings panel can crash on open when the stored quality is not in the
   menu** — `RequestInitialFocus(initialFocusRequester)` is unconditional
   (`:1507`) while the requester attaches only to the row whose quality
   equals `preferences.playbackQuality` (`:1517-1521`); `qualityOptions`
   drops rungs the ladder lacks (`PlaybackPolicy.kt:131-148`), so a stored
   1080p on a 720p source leaves the requester unattached and
   `requestFocus()` throws — the crash `Common.kt:236-237` and
   `HomeScreen.kt:145` already document. `TrackMenu` guards this correctly
   (`Controller.kt:1740-1742`). HIGH impact, source-confirmed path.
4. **Skip-marker button unreachable by D-pad while chrome is hidden** —
   composed regardless of `controlsVisible` (`:1066-1079`) while the
   hidden surface consumes all four directions (`:974-981`) and CENTER only
   reveals (`:988-990`). MEDIUM-HIGH. Source.
5. **Hidden-surface Up/Down seek ±30 s** (`:337-338`, proven
   `PlayerPolicyTest.kt:19-29`), with `ACTION_DOWN` auto-repeat (`:973`).
   MEDIUM. Mechanism M1.
6. **Auto-hide ignores an in-progress touch scrub** — the timer effect has
   no `scrubbing` key (`:918-923`) and `onScrub` never `poke()`s
   (`:1109-1110`); a drag longer than 3.8 s disposes the slider under the
   finger (`:1081`). MEDIUM. Whether Compose still delivers
   `onValueChangeFinished` on disposal is library behaviour; if not,
   `scrubbing` sticks and `positionMs` freezes (`:890`).
7. **Panels do not trap focus** (`Controller.kt:1625-1641`;
   `PlayerScreen.kt:2606-2617`; `Controls` stays composed beneath,
   `:1112-1113`). MEDIUM. Source.
8. **Focus resets to Play/Pause on every reveal** (`:1081`, `:1259-1260`),
   so the ≥3-press route to the top-row buttons has to fit in 3.8 s.
   LOW-MEDIUM. Source. Mechanism M3.
9. **BACK hides instead of exiting** (`:742-751`, proven
   `PlayerPolicyTest.kt:12-16`) — on TV it strands focus on the hidden
   surface where the next Left/Right seeks. LOW-MEDIUM. Mechanism M4.
10. Search field on TV opens the keyboard on focus (`SearchScreen.kt:93-104`)
    unlike `AuthTextField`'s select-to-edit (`AuthScreens.kt:299-355`). LOW.
11. No initial focus on Home/Library/Search/Settings/Downloads; only Detail
    (`DetailScreen.kt:264`) and the player request it. LOW.
12. Detail back button draws no focus ring (`SafeAreas.kt:58`). LOW.
13. Info mode chips are `Text.clickable{}.focusable()` (`:2049-2050`), the
    doubled-target pattern `Common.kt:101-107` removed from `PosterCard`.
    LOW.
14. `PlayerControlsFocusTest` does not assert its own name — the callbacks
    are `{}` and nothing counts scrubs (`:66-68`). LOW.

Settled: Down on the slider does not also seek −30 s — `playerSeekDeltaMs`
returns null while chrome is visible (`:333`) and the slider's preview
handler consumes vertical first (`:1467-1471`). `onScrub` is preview-only;
the seek is `onScrubEnd` (`:1110-1111`). The Home shelf focus graph is
pinned by `ShelfFocusTest` and is the one part of Android navigation built
the way the contract wants everything built.

### 3.3 Web

1. **The seek bar captures all four arrows once focused** — `#pseek` is
   `tabindex="0"` (`index.html:3039`), handles only Home/End itself
   (`:9061-9064`), and the global handler seeks with `preventDefault()` for
   any non-button, non-input target (`:10786`). Arrows cannot leave it;
   only Tab can. HIGH. Source. Mechanism M2. (Arrows are *not*
   double-handled: the bar is a div, so exactly one `nudge()` per press;
   the problem is capture, not duplication.)
2. **The player modal takes no focus and hides nothing** — no `focus()` in
   `play()` (`:7236-7266`), no `role=dialog`/`aria-modal`/`inert` (the only
   dialog semantics in the file are the connect-QR dialog, `:3893`), and
   `#app` precedes `#modal` (`:3038-3039`). The clicked Play button keeps
   focus behind the modal; Space/Enter re-launches (`:10782`); Tab walks
   the covered page. HIGH. Source. Mechanism M3.
3. **Escape closes the whole player with a menu or stats open**
   (`:10784`; no Escape or click-outside on `#pmenu`, `:9575-9627`). HIGH.
   Source. Mechanism M4.
4. **Header search loses focus while typing** — `#q` debounces 300 ms into
   `location.hash` (`:3786`, `:14471`) → `render()` → `viewSearch`
   (`:4791`) → `layoutChrome` replaces `#app.innerHTML` (`:3754`, `:13909`,
   `:14437`) and the input you were typing in is destroyed; no `focus()`
   restores it. HIGH for keyboard users. Source.
5. **Focus survives auto-hide, invisibly** — `.idle` is `opacity:0` only
   (`:335`, `:364`); the wake listener is on `#player` (`:9071`), so a Tab
   whose target is outside it does not un-hide; Space on an invisible
   focused `.picon` still fires. MEDIUM. Source. Mechanism M3.
6. **Clicking the seek bar never focuses it** — `pointerdown`
   `preventDefault()` (`:9051`) suppresses the focus change. MEDIUM.
7. **Modifier keys unchecked** — Ctrl/Cmd+F → fullscreen, Ctrl+I → stats,
   Ctrl+C → cycle subtitles, Ctrl/Cmd+K → play/pause (`:10783-10788` test
   `e.key` only). MEDIUM. Source.
8. **Steps and hints disagree** — ↑/↓ ±30 s vs ←/→ ±10 s
   (`playback-policy.js:747-760`) while the transport offers only ±10 and
   no hint mentions 30; `pbaudio`'s "(a)" hint has no handler (`:3039`);
   `f` is not repeat-gated unlike the rest (`:10787`); <520 px drops the
   ±10 buttons (`:402`). LOW-MEDIUM. Source.
9. **No focus return or dialog semantics on the player, the lightbox or
   the edit dialog** — `closePlayer` re-renders the item page (`:10764`),
   `closeLightbox` (`:4109-4113`) and `closeEdit` (`:5905`) restore
   nothing; the edit dialog has no Escape. The connect-QR dialog
   (`:3884-3919`) is the well-behaved model. MEDIUM. Source.
10. **Classic-layout posters and every `.eprow` are mouse-only** —
    `card()` (`:4035`), `homeCard()` (`:4046`, `:4050`) and `episodeRow()`
    (`:5380`) emit `<div onclick>`; only catalog/theater enhance `.poster`
    (`:13794-13802`, `:14880-14887`) and nothing enhances `.eprow`. Classic
    is the silent fallback for an unknown layout preference (`:3025-3026`).
    MEDIUM for TV browsers. Source.
11. Stats radios have `role="radio"` but no arrow-key group behaviour
    (`:3039`, `:9991`) — arrows there seek. LOW.
12. Escape's fullscreen guard reads only `document.fullscreenElement`
    (`:10784`) although `isFullscreenAnywhere()` exists (`:9009-9012`).
    LOW, plausible.

Settled: one player serves all three web layouts — no layout-specific
player CSS exists (only `[data-theme=terminal]` recolours, `:1219-1226`).
`surfaceClass()` detects `"tv"` (`:3000-3011`) and every layout claims the
`tv` surface (`:2993-2995`) but nothing changes for it: there is no
spatial-navigation engine in the web client at all. Keyboard seeks are
coalesced 350 ms against a frozen base (`:8992-9002`) and auto-repeat is
ignored; drag seeks once on release (`:9055-9058`); non-direct seeks
restart the server stream (`:9295-9323`). The documented `ui-baseline`
harness never opens the player (`UI-LAYOUTS-STATUS.md:44-49`).

---

## 4. The playback-info panel

Opened from `info.circle.fill` in the option group on Apple
(`PlayerView.swift:1836-1850`; the "More" popover row on compact iPhone,
`:1853-1890`), from the top-right `Info` button on Android
(`PlayerScreen.kt:1276-1278`), and from `ⓘ Stats` in the top bar or the `i`
key on the web (`index.html:3039`, `:10783` — beside an unrelated `ⓘ`
"Title info" button that opens the synopsis strip). Three modes on every
client, the same top-right ledger design since 2026-08-23, and that is
where agreement ends.

### 4.1 What each client shows

Condensed from the three builders — Apple `standardSections`/`debugSections`
(`PlayerView.swift:3161-3644`, with a *different* Standard on tvOS
`:3392-3512` and iOS `:3514-3644`), Android `PlaybackInfoDetails`
(`PlayerScreen.kt:2197-2380`), web `updateStats` (`index.html:10443-10664`).
`✓` present · `–` absent · `L:` same datum, different label · `H` in the
header only · `N` in the notes strip.

| Datum | Web Mini / Std / Dbg | Apple Mini / Std / Dbg | Android Mini / Std / Dbg | Mismatch |
|---|---|---|---|---|
| Method | ✓ / – / L: Delivery method | ✓ / H (tvOS) or ✓ (iOS) / ✓ | ✓ / H / ✓ | Web Standard has no Method row. Vocabulary differs: Apple "Remux · HLS", Android "Direct stream · remux", web "Transcode · nvenc" (`PlayerController.swift:1556-1563`; `PlayerScreen.kt:2452-2457`; `index.html:10102-10104`) |
| Position | ✓ / – / ✓ | ✓ / H / ✓ | ✓ / H / ✓ | tvOS says "1:02 of 2:03" (`:2876`), everyone else "/" ; web Debug appends "· 1.5×" |
| Reason | – / N / N | – / N / N | – / N / N | joiner is " · " in Standard and "; " in Debug on all three (`:3415` vs `:3200`; `:2207` vs `:2283`; `:10616` vs `:10472`) |
| Container | – / ✓ / ✓ | – / ✓ / ✓ | – / – / – | **Android never shows it** |
| Source audio | – / N / N | – / – / N | – / ✓ / ✓ | Apple Standard has no audio row |
| Decoding resolution | ✓ / ✓ / ✓ | ✓ / L: Output / ✓ | inside "Video" / N / N | three labels for one datum |
| Dynamic range | ✓ / N / N | ✓ / N / N | – / L: Range / L: Range | Android Mini lacks it |
| Buffer runway | ✓ "N.N s" / ✓ / ✓ | ✓ "N.N s ahead" / **–** / ✓ | ✓ "m:ss ahead · N%" / ✓ / ✓ | **Apple Standard has no buffer row**; Android formats as clock + percent (`PlayerScreen.kt:1578`) |
| Dropped frames | – / ✓ / ✓ | – / – / – | – / L: Frames N / L: Frames N | AVPlayer has none — legitimately absent on Apple |
| Player stalls | – / – / ✓ (**under Server**) | pill / pill / ✓ (Now decoding) | – / – / – | Android has none; web files it under SERVER (`index.html:10308`) |
| Subtitles | – / ✓ / **–** | – / ✓ (tvOS: Now decoding; **iOS: Server**, `:3634-3642`) / ✓ | – / ✓ / ✓ | Web Debug drops it |
| Delivery rate | ✓ / L: Delivery (Server) / ✓ (Network) | ✓ / L: Delivery (Server) / ✓ (Server) | ✓ / L: Delivery (Server) / ✓ (Network) | same datum under Server in Standard and Network in Debug, label alternating, on all three |
| Delivered bytes | – / – / ✓ Delivered | – / L: **Transferred** (tvOS Std, `:3509`) / L: Delivered | – / – / L: Transferred | on Apple Debug **Transferred is a different number** — client access-log bytes (`:3301`) |
| Status | pill / ✓ / **–** | – / ✓ (tvOS) or – (iOS) / ✓ only when no session | pill / ✓ / ✓ | Web Debug has no Status; wording "Holding buffer" / "Holding" / "Held" / "No server session" / "No server-side session" / "Waiting for server details" (`PlayerScreen.kt:2241` vs `:2330`; `PlayerView.swift:3405`) |
| Encoder | – / ✓ / **–** | – / ✓ / ✓ | – / ✓ / ✓ | web Debug folds it into Delivery method |
| Encode speed | – / ✓ / ✓ | – / ✓ / ✓ | – / L: Encode / ✓ | |
| Server ahead | – / L: Ahead / ✓ (+held · release · suspends inlined) | – / L: Buffer ahead / ✓ | – / L: Ahead / ✓ | **four labels**; web inlines the hold clause into the value (`index.html:10268-10270`), the others note it |
| Ahead bytes · Produced · Pacing · Held · Hold reason · Suspend count · Request idle · Last request · Playlist · Published/Fetched end | – / – / Produced only | – / – / ✓ all | – / – / ✓ all (+ Progress idle, Fetched segment, First retained) | web Debug lacks the server-side ledger the natives have |
| Playback mode · Switched · Auto log · Frame rate · Hitches · Buffer target · Decoder · Demand window · Started in · Control timing | ✓ Debug | – | – | web-only |
| Waiting reason · Buffer empty · Likely to keep up · Buffer full · Requests · Transfer time · Downloaded media | – | ✓ Debug | – | Apple-only |
| File · decoding Audio | – | – | ✓ | Android-only |
| Health pill | server state | **stall count** (`:2588-2593`) | server state | one pill, two meanings |

Units disagree even where the rows match: bitrate is `"%.2f Mb/s"` always
on Apple (`:3859-3861`, so "0.80 Mb/s"), "12.3 Mb/s" / "800 kb/s" on the
web (`:4800`), "12.3 Mbps" on Android (`PlayerScreen.kt:2551-2555`); bytes
are `ByteCountFormatter` decimal on Apple, decimal "%.2f GB" on Android,
and **1024-based** on the web (`:4801`) — the same `delivered_bytes` reads
differently on two of three clients. Resolution has spaces around × on tvOS
Standard only (`:3421`).

### 4.2 Using and navigating it

| | Apple | Android | Web |
|---|---|---|---|
| Initial focus | Done (Standard/Debug, `:2804-2815`); **none in Mini** (`:2767`) | ✕ (`PlayerScreen.kt:1665-1666`), not re-requested after a mode switch | none — `toggleStats` never focuses (`:9999-10017`) |
| Mode selector | capsule buttons in the header (`:2431-2454`) | `Text.clickable{}.focusable()` chips (`:2034-2055`) | `role="radio"` buttons with no arrow-key group behaviour (`:3039`, `:9991`) |
| Transport while open | tvOS disabled unless Mini (`:805`); iOS Debug backdrop swallows every tap outside the panel (`:2788-2800`) so the hidden transport cannot be recalled | hidden on open (`:1115`); **any D-pad press re-shows it under the panel and shrinks the panel** (`:1002-1009` → `:1625-1629` → `:1687`) | live; auto-hides beneath the panel after 2.6 s |
| First Select on TV | works | **swallowed** — the root handler consumes CENTER while controls are hidden (`:987-991`) | n/a |
| Scrolling by remote | every section box and the notes strip are focusable (`:3003-3009`, `:3099`) | **impossible** — `verticalScroll` with no focusable children (`:1904`, `:2088-2122`); Debug SERVER has up to 18 rows | browser scroll focus; body `innerHTML` rebuilt every 1 s kills text selection (`:10665`) |
| Auto-hide under the panel | fires at 4 s and moves focus to the invisible reveal surface (`:1048-1056`, `:1160-1164`); Left/Right then seek | suspended while any panel is open (`:918-923`) | chrome hides; panel stays (`:335-377`) |
| Back / Menu / Escape | closes the panel (`:965-966`) | closes the panel (`:322-326`) | **closes the player** (`:10784`) |
| Focus after close | Play/Pause (`:1178-1184`), not the info button | Play/Pause (`:1091`, `:1169`) | unchanged, wherever it was |
| Mode remembered | no (`:716`) | no (`:671`) | yes, `localStorage` (`:9976-9994`) |
| Refresh | 0.5 s position, 2 s status | 0.5 s rebuild of the whole details model (`:888-893`, `:1551-1616`) | 1 s `innerHTML` (`:10011`) |
| Live re-layout | numeric-alignment and tvOS dense-column verdicts flip per render (`:2369-2381`) | `alignEnd`/`twoUp` flip when a value crosses 12 chars (`:1979-1981`) | per-row `txt` class and `.onecol` width flip per tick (`:10374-10376`, `:10670`) |

Ranked, the ones that make it "a disaster to use": tvOS losing focus to the
reveal surface at 4 s (then a swipe seeks the film) · Android TV's swallowed
first Select and the transport popping up under the panel on every press ·
Android TV's unscrollable Debug ledger · web Escape closing playback · no
client returning focus to the button that opened it.

### 4.3 What pins it today

Apple: three mode labels (`AppleClientTests.swift:6857-6862`), the
overlay-survives-auto-hide predicate (`:5532-5540`), the tvOS canvas
constants (`:6817-6855`); the ledger types are `private`
(`PlayerView.swift:2253-2401`) so the row list is unpinnable as written.
Android: `PlaybackInfoOverlayTest.kt:39-110` pins about eight Standard
labels and the ✕ initial focus; nothing for Mini or Debug. Web: nothing
references `statsov`, `updateStats` or `setStatsMode` outside `index.html`.
No client has a test that pins the full row list of any mode, and nothing
pins parity. That is why the matrix in §4.1 exists — and why the contract
carries a second fixture for it.

---

## 5. Navigation outside the player

The shells are sound and differ for platform reasons: iOS is a `TabView` of
five `NavigationStack`s, tvOS one `NavigationStack` around a four-tab
`TabView` (`HomeView.swift:35-38`, `:44-83`); Android is Navigation-Compose
in `MainActivity.kt:84-229`; the web is a hash router (`index.html:15313-15344`)
whose every route rebuilds `#app.innerHTML`. What is *not* platform-specific
and should be uniform:

| Concern | Apple | Android | Web |
|---|---|---|---|
| Initial focus on a screen | Detail → Play/Resume, re-grabbed on every appearance (`DetailView.swift:764-784`) | Detail → primary action, TV only (`DetailScreen.kt:264`); Home/Library/Search/Settings none | none anywhere (16 `focus()` calls, none on route change; only the analysis page restores, `:11181`) |
| Focus after a dialog closes | popover/`Menu`: system restores | `popBackStack()`; panels: nothing | nothing (`:10764`, `:4109-4113`, `:5905`) |
| Shelf ↔ row focus graph | `focusSection` on rails and the tv hero/header (`Components.swift:22-40`, `:583`, `:700`; `DetailView.swift:1383`, `:1588`) | explicit `up/down` chain on Home, pinned by `ShelfFocusTest` (`HomeScreen.kt:118-137`; `Common.kt:194-278`) | n/a — no spatial nav |
| Back affordance on a pushed screen | system Menu; iOS custom chevron with nav bar hidden (`DetailView.swift:737-763`) | `SafeBackButton`, no focus ring (`SafeAreas.kt:58`) | `pageHead` `←` link (`:4582-4590`) |
| Select on a season episode card | **plays** (tvOS) | opens detail | opens detail (`.eprow` is mouse-only) |
| Text entry on TV | inline `TextField` (`SearchView.swift:15-36`) | `AuthTextField` select-to-edit; Search does not use it | n/a |

These are worth normalizing in the same arc but are not the source of the
reports; the plan sequences them last.

---

## 6. Why one-at-a-time patching cannot converge

Every input decision lives where the event arrives. tvOS has a real routing
table (`TVPlayerRemoteRouting`, `PlayerView.swift:77-107`) but only two
views consult it; the other eleven controls are system-driven and the
Menu/Play-Pause keys are handled in three other places. Android's
`playerSeekDeltaMs`/`playerBackAction` (`PlayerScreen.kt:322-341`) are the
right idea and cover two of eleven inputs; the slider's keys are the
library's, the panel keys are Compose's. The web has one global `keydown`
(`:10776-10789`) that decides by *the tag name of whatever happens to be
focused* — a `<div>` is a seek target, a `<button>` is not.

So the same question — "what does Right do right now?" — has four answers
per client and no two clients agree on the answer for the same state. A
test that pins one cell (`testTVHiddenControlsKeepDirectionalSeeking`,
`PlayerPolicyTest.longVerticalSteps`, `seekDeltaSeconds("ArrowUp") == 30`)
turns the cell into a load-bearing wall the next fix has to route around.
That is the whack-a-mole: the moles are cells in a table nobody wrote down.

The fix is to write the table down once, make each client's reducer the
*only* place that answers the question, and test all three reducers
against the same rows. That is
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md).

---

## 7. Rulings taken on 2026-09-02

1. **A directional press on hidden chrome only reveals it.** Seeking is via
   the visible timeline, the ±10 s buttons, and FF/REW media keys. This
   reverses the tested tvOS and Android behaviour in M1 on purpose.
2. **The 10-foot seek bar is its own row and is preview-then-commit.**
   Left/Right move a preview position (no seek, no network), Select
   commits, Back cancels, Up/Down cancel and leave. Android's Material
   slider and tvOS's click-to-engage bar are both replaced.
3. **The ±30 s vertical seek is dropped everywhere.** Vertical is
   navigation between rows.

4. **The info panel gets one row list.** The union in
   `tests/playback/playback-info-fields.json`, duplicate labels retired,
   platform-only rows marked, one unit spelling, one pill meaning.

Still open, with the recommended default in the contract and the plan:
the canonical options set (§2 "Options set" row) — recommended
`audio · subtitles · quality · settings · info · [pip] · [close]` with
autoplay, auto-skip and audio sync inside `settings`; and whether tvOS
season episode cards keep play-on-select (§3.1 item 7).
