# Android state inventory — Meridian M0 route and lifetime evidence

**Observed source SHA:** `85ae74eb6980602f4689506a7b3ff33edb912290`
**Status:** open; M0 source inventory complete; M4 presentation-state migration
required; no implementation or runtime qualification claimed.
**Written:** 2026-10-17 · **Scope:** Android phone, tablet and TV.

Companion to the canonical
[Meridian implementation contract](MERIDIAN-IMPLEMENTATION.md).
This artifact records the existing navigation, presentation and playback
owners, then proposes the adapters M4 needs. Re-verify cited symbols and line
numbers against the M4 base before editing.

All source paths below are relative to the inspected isolated worktree. No source files, tracked documents, branches or
Git metadata were changed for this inventory. No builds, tests, emulator runs,
screenshots or physical-device journeys were performed by this subtask.

Ripwire was attempted first and refused because its `target/ripwire` cache
write was not permitted. Inspection continued with targeted `rg`, outlines
and source ranges. Its heuristic refusal is not evidence of an absent owner.

## 1. Existing navigation — one controller shared by every form factor

Android has one linear NavController. There is no separate TV destination
field, Libraries overview, or preserved-primary-stack registry.

[MainActivity.kt](../../clients/android/app/src/main/java/tv/plurx/app/MainActivity.kt)
creates the sole controller at line 206, reads the current
`NavBackStackEntry.destination.route` at line 215, and owns the NavHost at line
254. TV adaptations happen inside the same registered screen destinations.

The existing lifetimes are:

- Activity-owned AppViewModel, theme and AppRoot: `MainActivity.kt:111–118`.
- MainNav-owned controller, RemoteNavigationCoordinator and profile-bound DVR
  controller: `MainActivity.kt:206–239`.
- Remote scope follows the exact current entry ID; Back is entry-bound:
  `MainActivity.kt:212–220`.
- [BackNavigation.kt](../../clients/android/app/src/main/java/tv/plurx/app/BackNavigation.kt)
  lines 6–11 implement `NavController.popBackStackFrom(entry)`. It refuses
  stale-entry callbacks and root pops. Preserve this discipline.
- [AppViewModel.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt)
  owns Home data, preferences and feature state at lines 134–150. Its single
  active library pager is bound to profile, library IDs and sort at lines
  760–792.
- [Layout.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/Layout.kt)
  lines 14–22 classify TV from UI mode, otherwise Expanded at available width
  of at least 600 dp. Phone and tablet do not use a separate navigation owner.

### Destination inventory and Meridian placement

Every registration below is in `MainActivity.kt`. Placement is proposed M4
presentation, not an implemented route change.

| Existing NavHost route | Verified registration | Meridian placement |
|---|---|---|
| `home` | 255 | Home |
| `library/{ids}/{name}` | 273–287 | Libraries branch; individual local sources in expanded navigation |
| `search` | 290–296 | Search |
| `detail/{id}` | 299–317 | Retain originating branch and detail history |
| `reader/{itemId}/{fileId}` | 321–333 | Detail-owned EPUB reader |
| `pdf-reader/{fileId}/{expectedSize}` | 336–348 | Detail-owned PDF reader |
| `photo/{id}` | 351–359 | Detail-owned photo viewer |
| `shared-libraries` | 362–365 | More on phone; direct destination in expanded/TV navigation |
| `sharing-settings` | 367–370 | Settings → Sharing |
| `settings` | 373–382 | More/account → Settings |
| `live-tv?channel={channel}` | 385–404 | More on phone; direct expanded/TV destination |
| `recordings` | 407–415 | More/direct expanded/TV destination |
| `recording/{id}` | 418–428 | Recordings/Live TV child |
| `recording-activity` | 431–437 | Recordings activity child |
| `library-channels?seedId={seedId}&seedKind={seedKind}&seedTitle={seedTitle}` | 440–458 | More/direct expanded/TV destination; retain detail-seeded creation |
| `remote-devices` | 461 | Settings → Remotes & devices |
| `developer` | 462–465 | Settings → Developer |
| `live-tv-settings` | 467–470 | Settings → Live TV |
| `downloads` | 472–479 | More on phone/tablet; retain existing TV visibility policy |
| `offline-book/{id}` | 482–490 | Downloads-owned reader |
| `offline/{id}` | 493–501 | Downloads-owned player |
| `player/{itemId}/{fileId}/{startMs}?audio={audio}&subtitle={subtitle}&returnChannel={returnChannel}` | 504–554 | Existing playback owner; suppress browsing chrome |

There is no general Activity route. Android exposes recording activity. Help
is the diagnostic dialog reached from Settings
(`ui/SettingsScreen.kt:275–277`), not a separate NavHost destination.

### Visibility, disabled routes and external entry

[HomeScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/HomeScreen.kt)
lines 94–99 derive Live TV, Recordings, Library channels and Shared libraries
visibility from `features`. Hidden sections also leave semantic remote order
at lines 106–110. Routes remain registered so reminder/external entry can
reach their own disabled state; the source comment states this at lines 89–92.

Downloads is omitted from the existing TV header at lines 522–525. Local
library ordering comes from existing Home/library order; grouped Home
collections are derived at lines 549–574. There is no pin/reorder persistence
to reuse.

[LiveTvSettingsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvSettingsScreen.kt)
lines 35–38 explicitly keep settings reachable with administrator authority
enforced by the server and readiness advisory. The new shell must preserve
that distinction.

Reminder channel entry uses launchSingleTop and consumes the request once
(`MainActivity.kt:243–250`). Invitation entry is a separately owned dialog
after the NavHost (`MainActivity.kt:558–563`).

There is no existing automatic primary-stack/source fallback owner to reuse.
The current Home feature policy hides controls and leaves destination
disabled states registered. M4 must explicitly implement the plan's visible
fallback when a selected local source disappears, without granting access or
inventing a different data owner.

## 2. Presentation owners — preserve composition or migrate explicit state

The table records source evidence. “Remembered” means ordinary Compose
`remember`, not guaranteed retention after a destination composition is
disposed. `rememberSaveable` and list-state savers retain their supported
values through NavHost state restoration, but do not preserve arbitrary
controllers, WebViews, effects or ordinary remembered objects.

| Existing owner | State and presentation to preserve |
|---|---|
| Home | Saveable initial-focus claim and visible-shelf requesters: `ui/HomeScreen.kt:190–204`; vertical scroll: 242–245; per-shelf LazyRow states: `ui/components/Common.kt:222–225`. Shelf order, grouping picker and explicit up/down chain are interdependent (`HomeScreen.kt:167–189,214–240`). Preserve the chain and mounted rows. Grouping remains a preference (291–306). |
| Local library | Saveable sort/filter/query/Rows-Grid/expanded group/jump group: `ui/LibraryScreen.kt:116–126`; pager/loading/filtered groups: 118–157; grid and rows scroll owners: 120–121; initial Back focus: 166–167. Expanded group is an owned fullscreen Dialog with its own grid, token, realization and native-window registration (269–330). Underlying keyed rows deliberately remain composed. |
| Search | Saveable query; ordinary remembered results/loading/error: `ui/SearchScreen.kt:65–68`; search nonce, grid scroll and semantic realizer: 69–81; debounce effect: 83–105; television-only field focus: 107–115. A layout change must not recreate its text nonce. |
| Local detail | Load/refresh and series-playback resolution: `ui/DetailScreen.kt:124–148`; remembered starting episode, per-file track choices and selected version: 193–205; semantic play owner: 208–219; LazyColumn scroll and remote realization: 220–245. Place these above Standard/Meridian visual branches. |
| Shared libraries | Separate internal stack: `ui/SharedLibrariesScreen.kt:27–40,43–77`. It owns Sources → Library → Detail/children, route-specific scopes, opener map, restoration generation and internal Back. Source-list scroll: 102; source-local query, accumulator, generation and list state: 177–205; detail list: 299. |
| Shared detail/preplay/player | `ui/SharedLibrariesScreen.kt:232–249` owns playback plan, preplay file, opener, incumbent player, playing reference and preparation/autoplay fences. Embedded PlayerScreen and SharedRemotePreplay replace the detail body (274–297). Keeping MainNav alone does not preserve this subtree's owners. |
| Settings | Remembered language selections, sign-out dialogs, release-status load and device SettingsStore: `ui/SettingsScreen.kt:85–103`; sign-out dialog: 110–128; focus: 134–135; scroll: 225. Compact/expanded changes move controls into different arrangements (233–244). Preserve root composition and scroll owner. |
| Sharing settings/management | Local management destination branch: `ui/SharedSharingSettingsScreen.kt:23–24`; summaries/draft: 25–28,68–72; scroll: 53. `ui/SharedSharingManagementScreen.kt:23–40` owns secret drafts, import/export selection, invitations and edit/confirmation state; confirmation dialog: 135–137. Back/disposal deliberately clears secrets (54–60), so layout must not cause disposal. |
| Live TV | Application-scoped LiveTvPlayer plus screen locals: `livetv/LiveTvScreen.kt:130–170`. Search, fullscreen, persisted browser/layout choices, selected channel, guide time/window, focus target, favorites/protected filters, programme detail, overlays, captions/info/more and PiP live here. DVR sheet/chip/confirmation and one-shot notification tune: 202–207,310–314. Disposal/stop stops playback unless retained for PiP (322–324). |
| Live TV TV alternatives | Browse, TvLiveLayout, guide/list focus/time remain screen locals (`livetv/LiveTvScreen.kt:140–150`); TV on-now list owns list state, requester map and pending requester (1986–2010). These are alternatives within Live TV, not primary destinations. Preserve the input reducer (409–460) and fullscreen Back order (325–333). |
| Live TV overlays | Recording sheet: `livetv/LiveTvScreen.kt:982`; programme sheet: 993; stop/delete confirmations: 1029,1045; captions: 1086; info sheet: 1095. New navigation-panel Back must arbitrate above existing owned overlays without consuming the same event twice. |
| Recordings | `livetv/DvrUi.kt:580–584` owns selected chip, stop/delete confirmations, clock and Back focus. Dialogs: 648,664; list owners: 356,402,458,553. Activity owns clock/focus/list (689–706). Recording detail owns confirmation/focus plus controller selection (735–739), file list (777) and stop dialog (791). Shared DVR controller stays MainNav-owned. |
| Library channels | Controller, editor/creation, saveable TV layout and programme focus: `librarychannels/LibraryChannelsScreen.kt:94–101`; polling: 103–109; disposal stops playback: 110. TV guide-preview/guide-over-picture/channel-browser alternatives: 121–156; editor sheet: 164–175. Editor recipe/draft/search/preview/step state spans 290–334; persistent draft/request identity is intentional. List scroll: 229; editor scroll: 409. |
| Downloads | Profile-delete confirmation: `ui/DownloadsScreen.kt:78–85`; list: 141. Records remain VM/store-owned. |
| EPUB reader | Native WebView and frozen server origin: `ui/ReaderScreen.kt:77–84`; Back requests the web reader's close operation (87–95); disposal destroys reader/WebView (186–198). Do not recreate AndroidView during shell changes. |
| Offline EPUB reader | Book-keyed WebView/error: `ui/OfflineBookReaderScreen.kt:116–117`; Back: 127; disposal destroys the WebView: 270–280. |
| PDF reader | PDF-object disposal and Back: `ui/PdfReaderScreen.kt:130–135`; page index: 137; scroll: 192; per-page zoom/pan: 212–213. |
| Photo viewer | No current zoom/scroll state; double-tap is reserved: `ui/PhotoScreen.kt:20–33`. Preserve route and existing Back. |
| VOD player | Plan/presentation target/revision/generation/quality/tracks/intent are item/file-keyed remembered state: `player/PlayerScreen.kt:506–539`. Controller is remember(plan) (823–843), with panel/control focus/last focus/opener locals (870–906). Back is player-owned (1290); disposal releases controller (1366–1370). Layout must not enter those lifetime keys. |
| Offline player | Player is download-keyed: `player/OfflinePlayerScreen.kt:51`; control/info/focus locals: 52–67; Back uses input policy: 210–213; disposal records progress and releases player: 226–244. |
| App overlays | Invitation dialog follows NavHost (`MainActivity.kt:558–563`), followed by RemoteRootOverlay (565). DiagnosticReportHost belongs to AppRoot (194). TV pairing dialog, phone companion surface and suggestions: `remote/RemoteCompanionViews.kt:52–95`. Diagnostic dialog explicitly restricts companion actions: `diagnostics/DiagnosticReportScreen.kt:37–49`. Keep these owners outside shell variations. |
| Live TV settings / Developer | Remembered admin state, Back focus and scroll: `livetv/LiveTvSettingsScreen.kt:42–48`, `livetv/LiveTvDeveloperScreen.kt:36–42`. Admin state includes unsaved fields at `livetv/LiveTvAdminState.kt:70–82`; factory: 201. |
| Remotes & devices | Own revision and scroll: `remote/RemoteCompanionViews.kt:210–214`. |

Source entry points for the table:

- [Common.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/components/Common.kt)
- [LibraryScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/LibraryScreen.kt)
- [SearchScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SearchScreen.kt)
- [DetailScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/DetailScreen.kt)
- [SharedLibrariesScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SharedLibrariesScreen.kt)
- [SettingsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SettingsScreen.kt)
- [SharedSharingSettingsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SharedSharingSettingsScreen.kt)
- [SharedSharingManagementScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SharedSharingManagementScreen.kt)
- [LiveTvScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt)
- [DvrUi.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/DvrUi.kt)
- [LibraryChannelsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/librarychannels/LibraryChannelsScreen.kt)
- [DownloadsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/DownloadsScreen.kt)
- [ReaderScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/ReaderScreen.kt)
- [OfflineBookReaderScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/OfflineBookReaderScreen.kt)
- [PdfReaderScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/PdfReaderScreen.kt)
- [PhotoScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/PhotoScreen.kt)
- [PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt)
- [OfflinePlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/OfflinePlayerScreen.kt)
- [RemoteCompanionViews.kt](../../clients/android/app/src/main/java/tv/plurx/app/remote/RemoteCompanionViews.kt)
- [DiagnosticReportScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/diagnostics/DiagnosticReportScreen.kt)
- [LiveTvDeveloperScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvDeveloperScreen.kt)
- [LiveTvAdminState.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvAdminState.kt)

## 3. M4 adapters — required implementation, not M0 acceptance

These decisions are viable implementation directions requiring focused review
and tests in M4. M0 has not implemented or validated them.

1. **Keep the existing NavHost mounted.** Keep MainNav, its NavController,
   DVR controller, remote coordinator, providers, NavHost and app overlays at
   constant call sites. Use one responsive shell with modifier/slot geometry.
   Separate Standard/Meridian or compact/expanded NavHosts would destroy the
   local state and effects inventoried in §2.

2. **Keep state above visual alternatives.** Home, Library and Detail can
   render alternative header/hero/rail composition after resolving state.
   Existing saveable query/filter/sort/list state remains entry-scoped.
   Ordinary remembered state must retain its call site during layout/theme
   changes. A layout-specific key on a destination or player is prohibited.

3. **Project stable semantic destinations.** Add Home, Libraries, Search,
   allowed feature destinations, Settings and local library IDs. All touch,
   native D-pad and companion callbacks call one selection adapter at MainNav.
   Libraries needs a real overview destination. The existing library route's
   display-name argument is not destination identity.

4. **Add branch retention at the existing controller.** There is one linear
   stack today. Preserved primary destinations require reviewed branch
   bookkeeping/nested graph ownership with save/restore navigation. Different
   primary selection activates its preserved chain; selecting the current
   primary is a no-op. Move presentation state required across branch
   suspension into an entry-owned state holder/Saver before relying on it.
   Navigation restoreState does not retain arbitrary ordinary remember state.

5. **Separate Libraries selection from explicit source selection.** Track
   committed source IDs separately from the selected primary. Same source
   preserves its chain. A different explicit source invalidates only the
   Libraries saved branch and creates overview → requested library root.
   Other primary branches survive. Previous source roots must not accumulate.

6. **Route companion Home through the same selection adapter.** The current
   companion Home callback recreates Home and clears its stack
   (`MainActivity.kt:223`). Meridian needs the plan's preserve-current-primary
   semantics. Preserve Standard behavior unless a deliberate change has its
   own regression.

7. **Add an explicitly safe navigation-panel presentation.**
   [RemoteNavigation.kt](../../clients/android/app/src/main/java/tv/plurx/app/remote/RemoteNavigation.kt)
   lines 45–55 refuse enterMenu on blocked routes.
   RemoteRoutes at lines 205–212 map Settings, Recordings, Channels, Downloads
   and other ancillary pages to restricted. Blind reuse of enterMenu cannot
   provide companion panel navigation from every page. A typed chrome
   presentation must preserve the old scope/route/order/opener, expose only
   safe destination actions, and retain existing diagnostics/pairing/content
   restrictions. It must not grant companion actions to restricted content.

8. **Bind restoration to active identity and generation.**
   [RemoteNavigationViews.kt](../../clients/android/app/src/main/java/tv/plurx/app/remote/RemoteNavigationViews.kt)
   lines 36–48 wait for realization and native-focus acknowledgement. Reuse
   that proof. Existing closeMenu merely restores the saved opener
   (`RemoteNavigation.kt:60–64`); Meridian needs visible-opener validation,
   then next item → previous item → visible recovery control. A new
   destination cancels old focus tickets. Store entry/source identity,
   navigation generation and focus-request revision with restoration.

9. **Keep local input and Back ownership.** Library tools and spatial
   movement win before left-edge panel activation. Opening the panel must
   transfer actual focus and exclude covered content from interaction.
   Closing it consumes one Back without invoking entry Back. Existing
   player, Live TV, readers, dialogs and Shared libraries internal Back
   remain their owners.

10. **Resolve invalid selection visibly.** Feature-disable/source-removal
    recovery uses existing authorization and disabled-destination policies.
    A removed explicit local source falls back to the Libraries overview
    with a visible explanation; it cannot restore an old-source focus target.
    This requires M4 behavior and tests because the existing controller has
    no such branch adapter.

## 4. Existing tests — useful components, incomplete shell proof

[BackNavigationTest.kt](../../clients/android/app/src/androidTest/java/tv/plurx/app/BackNavigationTest.kt)
uses a miniature test NavHost at lines 28–38, not production MainNav. Its
regressions remain valuable:

- rapidBackFromMovieKeepsHomeVisible (56)
- secondBackDuringExitAnimationKeepsHomeVisible (64)
- rapidBackFromMovieKeepsItsLibraryVisible (78)
- rapidBackBetweenIdenticalDetailRoutesOnlyPopsOneEntry (86)
- coveredDetailCannotPopPlayerButCanGoBackAfterPlayback (96)
- backAtRootNeverEmptiesNavigation (111)

[ThemeLayoutTest.kt](../../clients/android/app/src/androidTest/java/tv/plurx/app/ui/ThemeLayoutTest.kt)
lines 21–38 render dark poster cards only. This cannot establish independent
layout geometry, phone/tablet/TV shell behavior, light appearance, selected
roles or focus contrast.

[RemoteNavigationTest.kt](../../clients/android/app/src/test/java/tv/plurx/app/remote/RemoteNavigationTest.kt)
covers logical ownership and fences:

| Test | Verified line |
|---|---|
| lazyFocusRequestCannotSelectUntilRealizedNativeAcknowledgement | 11 |
| unknownModalAndRetiredRouteFenceCapturedActionsAndSensitiveState | 29 |
| ownedMenuBackRestoresExactOpenerOrderAfterBackgroundRefresh | 41 |
| detailPlayOwnerCannotSurviveHomeOrOldDisposalClearNewDetail | 52 |
| onlyExactOwnedRoutesCanAdvertiseSemanticControls | 68 |
| programmeDetailsOwnsOnlyWatchCloseAndRestoresExactGuideCell | 75 |
| sharedChildExitRestoresExactOpenerAndRetiredDisposalCannotResetParent | 93 |
| expandedLibraryOwnsRealizationAndBackRestoresExactOpener | 108 |
| retiredExpandedDialogCannotCloseReplacementPresentation | 132 |

[HomeFeatureVisibilityTest.kt](../../clients/android/app/src/test/java/tv/plurx/app/ui/HomeFeatureVisibilityTest.kt)
covers feature decoding/defaults and hidden-section remote keys (17–55).
Preserve and extend it for the projected destination inventory.

Required M4 acceptance still includes the actual production NavHost on
phone/tablet/TV; primary branch switching; nested Settings; Shared browse,
preplay and embedded playback; explicit library replacement; source removal;
feature disable; compact/expanded transitions; live playback while changing
appearance; panel cancel/current/different/source selections; topmost-overlay
Back; rapid selection then Back; removed opener; and companion parity.

Assert physical native focus after realization. Logical requested IDs,
compiler success, mockup captures and coordinator unit tests do not establish
native focus or physical-device acceptance.

## 5. Handoff boundary

M0 delivers this source map. M4 must implement and test the required
presentation-state migration and destination-selection adapter before its
native shell can satisfy the contract. No product behavior, compiler result,
emulator qualification or physical acceptance is implied by this artifact.

The next Android work should begin by extracting or exposing the production
MainNav test seam and specifying branch/state retention tests, while keeping
the existing controller and playback/data ownership. Add preference/palette
identities atomically with the functioning Android layout as required by the
canonical plan; do not land an inert selectable native layout.
