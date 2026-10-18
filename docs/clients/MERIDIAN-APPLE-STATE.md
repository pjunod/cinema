# Apple state inventory — Meridian destinations and retained lifetimes

**Status:** open; M0 source inventory complete; M3 host/presentation migration
and native qualification remain due. **Inspected:** 2026-10-17.
**Source:** `85ae74eb6980602f4689506a7b3ff33edb912290`.

Companion to [the M0 evidence](MERIDIAN-M0.md) and
[canonical implementation ledger](MERIDIAN-IMPLEMENTATION.md). This records the
read-only Apple exploration, then specifies the reviewed adapter work still
required. Line references below are source evidence at that SHA; they are not
runtime assertions. Source paths are relative to `clients/apple/Sources/`.

## Navigation roots — retaining the coordinator alone is insufficient

| Surface | Verified owner |
|---|---|
| Application | `PlurxApp.swift`:69–75 owns AppModel, navigation, playback, remote and invitation StateObjects. |
| iPhone/iPad | `HomeView.swift`:101–155 has coordinator-bound Home/Libraries/Search stacks and implicit Live TV/Recordings/Channels/Downloads/Settings stacks. |
| iPad adaptation | `HomeView.swift`:81–88 applies `.sidebarAdaptable` to existing iOS tabs on iOS 18+. |
| tvOS | `HomeView.swift`:91–95 has one outer NavigationStack bound to the selected tab path, with seven roots inside TV tabs. It does not already have independently mounted per-tab TV stacks. |
| Semantic navigation | `RemoteNavigationCoordinator.swift`:40–41 owns selectedTab and `[HomeTab:[Route]]`; :187–216 synchronizes scope and invalidates destination/modal returns. |

[`HomeTab`](../../clients/apple/Sources/HomeView.swift) has Home, Libraries,
Live TV, Recordings, Channels, Search, Downloads and Settings (:3–12).
Downloads is only in iOS chrome. Visibility and feature fallback belong to
`HomeTab.isVisible`, `visibleSelection` and `keepSelectionVisible`
(:19–32,67–77). The finite [`Route`](../../clients/apple/Sources/PlurxApp.swift)
enum (:98–104) has collection, item, sharedLibraries, sharedLibrary and sharedItem.
Apple has no general Activity/admin root in this inventory; recording activity
is nested under DVR/Live TV.

## Persistent hosts — one stack identity for both layouts

Retain one destination host common to Standard and Meridian. Every root,
NavigationStack and sheet/cover presenter keeps its lifetime when geometry,
theme or available width changes. The selected tab and bound paths continue to
belong to the existing coordinator.

- iOS retains all eight current roots, including the five implicit stacks.
- tvOS first migrates the outer-stack composition to persistent per-destination
  stack nodes common to both layouts. Bind them to the existing coordinator
  paths and retain destination-based Settings/DVR pushes in their own node.
- A viable OS17 seam uses public UIKit containment, persistent child
  UIHostingControllers and unchanged SwiftUI stack roots. Standard/native
  chrome and Meridian bottom/rail/panel chrome select those same children.
- Prototype controller/view identity, implicit history, Back, appearance
  forwarding and covers before product geometry. A retained View/AnyView value
  alone does not establish retained SwiftUI state.
- Inactive roots leave focus, hit testing and accessibility. Actual tab
  activation may forward existing lifecycle changes; layout/width changes must
  not trigger disappearance/stop handlers or alter mounted stack identity.

The project targets iOS/tvOS **17.0** (`project.yml`:10–12).
Installed SDK headers declare `UITabBarController.setTabBarHidden(_:animated:)`
and `isTabBarHidden` as **18.0+** (`UITabBarController.h`:134–140 in the installed
TV simulator SDK; the iPhone SDK agrees). That API alone cannot cover the
supported minimum. Persistent public containment avoids relying on unsupported
hiding. Minimum-deployment compilation and current/17-runtime identity journeys
are distinct evidence; M0 has only installed 26.5 runtimes.

## Complete destinations and their presentation state

| Destination | Existing owner/state | Meridian placement and selected lifetime strategy |
|---|---|---|
| Home | `HomeView.swift`:327–397 owns native scroll/shelf positions and remote shelf realization; hubs remain AppModel-owned. | Phone Home; expanded/TV rail. Retain root, path and shelves; change feature geometry inside the owner. |
| Libraries overview | `HomeView.swift`:518–624 owns grouping menu/native scroll; model owns grouping/libraries/previews. | Phone Libraries; tablet/TV Libraries plus source entries. Retain overview root and saved grouping. |
| Local library/category | `Route.collection`; `LibraryView.swift`:20–64 owns LibraryGridCoordinator StateObject, sort/filter/query, expanded/selected group, choices, bookmarks, focus, jump state, nonce and visible indices/columns. | Retain exact LibraryView identity. Different explicit source selection alone replaces Libraries history. |
| Expanded library group | `LibraryView.swift`:202–205,456–473,548–556 owns item-bound destination, scope token, item bookmark, tools/content focus and return-to-tools state. | Retain the implicit destination and grid owner; Back returns to its group opener. |
| Item detail (movie/show/season/episode/book/photo/etc.) | `Route.item`; `DetailView.swift`:681–714 owns detail/play/series contexts, episode attempt, selected version, track/file choices, menus/about, readers/download and TV focus latch. Ancestors use value links at :323. | Retain DetailView/presenters; compact/wide composition at :1094–1113 can replace scroll surfaces, so restore identity bookmarks when those surfaces change. |
| Shared libraries | `Route.sharedLibraries`; `SharedLibrariesView.swift`:27–31,80–85 owns assignments/history, rows/recent, loading/errors/generations. | Libraries/More on phone, Shared entry on tablet/TV under existing policy. Retain its branch. |
| Shared source/root/children | `Route.sharedLibrary`; `SharedLibrariesView.swift`:146–158,279 owns query, nonce, accumulator/cursor and request/load generations. | Retain full shared identity, query, scroll and child browsing. Do not collapse it to a local library ID. |
| Shared detail/player | `Route.sharedItem`; `SharedLibrariesView.swift`:230–239,316–327 owns preparation task/lifetime, playbackPlan, remotePreplay, detail/menu state. | Retain iOS sheet/tvOS cover owner; existing shared authorization and player controller remain owners. |
| Live TV | `LiveTvView.swift`:1477–1502,2097–2217 owns guide scroll/anchor/focus coordinator, query/browse/favorite/protected filters, guide time/channel/programme/header focus, fullscreen/chrome/panels/PiP. | Phone More; expanded/TV rail. Retain root, presenters and controller; layout must not trigger stop/release at :2496–2499. |
| Recordings | `DvrRecordings.swift`:1246–1278; panel state :677–700 owns chip/manual fields, panel, confirmations, opener/order/restoration generation. DvrController owns data/pages. | Phone More; expanded/TV rail with existing badge. Retain implicit stack. |
| Recording detail/activity | Destination links at `DvrRecordings.swift`:933–946,979–984,1255–1257,1300–1302; detail confirmation/focus :1354–1363; Play emits Route.item at :1479. | Preserve nested history and exact Back caller. Activity remains the existing DVR/Live TV child. |
| Library channels | `LibraryChannels.swift`:832–852 owns editor/seed, personal playback/return channel, programme opener/focus, fullscreen/chrome/More/PiP; controller data :317–320. | Phone More; expanded/TV rail. Retain root; :986–989 stops controller on disappearance unless fullscreen. |
| Downloads/readers | iOS only; `DownloadsView.swift`:5–11,93–109 owns offline playing/reading/profile deletion presenters and List state. | Phone More; tablet rail/More. Retain managers and readers; no new TV Downloads entry. |
| Search | `SearchView.swift`:6–14 owns query/results/loading/errors/retry/bookmark/focus/nonce; scroll restoration :74–82. | Phone Search; expanded/TV search entry/control. Header search selects this owner, not a second query pipeline. |
| Settings/account | `SettingsView.swift`:49–68 owns TV selected/focused/last category, sign-out/dialog/download decisions and independent preferences. | More/account or rail. Preserve implicit stack and every destination-based link below. |
| Remotes/devices | `SettingsView.swift`:309; `RemoteDeviceSettingsView.swift`:5–14 owns confirmation, opener/order/restoration state; invitation settings are nested. | Settings/More. Retain confirmation and its valid opener. |
| Help/report | `SettingsView.swift`:317–323; `DiagnosticReportView.swift`:365,392,434–436,608–621 owns report NavigationStack, Advanced destination, iOS share sheet and root/player-specific presenters. | Settings/More action. Retain DiagnosticReportModel and correct presenter. No synthetic Help route. |
| Connection/login/recovery | `PlurxApp.swift`:127–180; QR sheet at `AuthViews.swift`:212. | Existing RootView phase flow remains outside the layout shell. |

Verified source entry points:
[HomeView](../../clients/apple/Sources/HomeView.swift),
[LibraryView](../../clients/apple/Sources/LibraryView.swift),
[DetailView](../../clients/apple/Sources/DetailView.swift),
[SharedLibrariesView](../../clients/apple/Sources/SharedLibrariesView.swift),
[LiveTvView](../../clients/apple/Sources/LiveTvView.swift),
[DvrRecordings](../../clients/apple/Sources/DvrRecordings.swift),
[LibraryChannels](../../clients/apple/Sources/LibraryChannels.swift),
[DownloadsView](../../clients/apple/Sources/DownloadsView.swift),
[SearchView](../../clients/apple/Sources/SearchView.swift),
[SettingsView](../../clients/apple/Sources/SettingsView.swift).

## Destination links and presenters — these outlive chrome changes

| Push / presenter | Existing owner and line evidence |
|---|---|
| Settings → Playback readiness / Live TV settings / Sharing | `SettingsView.swift`:164 / :267 / :279 |
| Settings → Remotes / Developer | `SettingsView.swift`:309 / :314 |
| Sharing → Developer / Management | `SharedSharingSettingsView.swift`:11,19 |
| Developer → saved invitation choices | `LiveTvDeveloperView.swift`:29 |
| Invitation recovery → Settings/Remotes | `InvitationSettingsView.swift`:104,111–116 |
| Report → Advanced diagnostics | `DiagnosticReportView.swift`:434–436 |
| Sharing endpoint/assignment/recipient editors | Independent sheet NavigationStacks at `SharedSharingManagementView.swift`:75–77. Secret draft retires on disappearance at :82; layout cannot detach this subtree. |
| Root invitation and companion sheets | `PlurxApp.swift`:237,244; companion QR/text state at `RemoteCompanionViews.swift`:107–111,146 |
| Local detail player / iOS readers | `DetailView.swift`:820–872; player controller/PiP/lifecycle StateObjects at `PlayerView.swift`:696–698; shared controller/PiP at :3737–3748 |
| Current TV Theater Home hero | **Owns its playback presentation** at `TvHomeLayout.swift`:60–62,165–177. Before conditional Meridian Home geometry, move presenter variables to one persistent Home presentation owner; retain PlayContext and PlayerView flow. |
| Live TV root/cover sheets | `LiveTvView.swift`:2505–2569,2917; activity/touch-recording sheets contain independent stacks |
| Channel editor/player/fullscreen/programme/layout/More | `LibraryChannels.swift`:913–959,1106; editor recipe/query/preview/save/step state :1257–1295 |
| Player menus | `PlayerView.swift`:2475,2490,2702,2726,2756,4009 |
| PDF reader | `PDFReaderView.swift`:187–198 owns page/search/save state |

## Selection and focus — separate cancel from destination commitment

Use four semantic operations through the existing selected-tab/path owner:

1. **Cancel:** leave selected tab and every stack untouched. Restore an opener
   only in the visible active scope; otherwise next → previous → recovery.
2. **Current primary:** preserve its exact path and implicit history; close to
   remembered visible content. Do not invoke `.home`, which clears Home history.
3. **Different primary:** apply existing visibility policy, activate its retained
   destination, cancel old panel restoration, then realize/focus its content.
4. **Explicit source:** project `libraryCollections(grouping:.share)` without
   changing saved grouping. `AppModel.swift`:679–710 establishes `share:<id>`
   and `category:<kind>` identities. Same exact source keeps Libraries history;
   different source replaces only `paths[.libraries]` with the collection root.
   The retained Libraries overview remains beneath it. Category aggregates do
   not count as exact source matches.

Feature fallback exists. Local-source deletion has no equivalent coordinator
reconciliation yet; add visible Libraries-overview recovery, cancel old-source
focus and leave other stacks unchanged.

Current TV tab synchronization requests nil content focus on a changed tab
(`RemoteNavigationCoordinator.swift`:207–216), relying on native chrome.
Meridian needs explicit destination realization/completion. Reuse scope,
generation and native acknowledgement at :326–432 and
`RemoteNavigationViews.swift`:108–151; a requested ID alone is not completion.

Library Left must first enter existing tools (`LibraryView.swift`:355–386);
tools Right restores content (`PlayerRemoteAdapter.swift`:407–423). Put a
labeled navigation opener beyond that boundary. No direction selects a route.
Preserve player-first dispatch (`RemoteClientModel.swift`:338–347) and one Back
for the topmost owner. Companion commands already enter this coordinator
(:348–366), so reuse the semantic adapter.

Restrictions remain: rootScope defaults to restricted beyond
Home/Libraries/Search/Live TV (`RemoteNavigationCoordinator.swift`:91–101), and
DVR explicitly blocks companion content (`DvrRecordings.swift`:743,1430).
Admitting safe panel destinations must not globally unblock content. Persistent
inactive roots must not leave stale blockers registered in the active scope.

## Source gaps and focused proof still due

**Settings → Sharing → Shared Libraries appears unregistered on iOS.**
Settings' stack lacks `.appDestinations()` (`HomeView.swift`:153–155), Sharing
renders `SharedLibrariesEntry` (`SharedSharingSettingsView.swift`:9), and that
emits `Route.sharedLibraries` (`SharedLibrariesView.swift`:6). TV's outer
registration covers it. Verify the actual iOS journey and add a focused route
registration regression during M3 rather than silently dropping the entry.

Extend the following UI preference/navigation tests:

- `Tests/RemoteNavigationTests.swift`:80–96 currently expects nil content focus
  after TV tab changes.
- `Tests/TvHomeLayoutTests.swift`:31–46 covers independent preference persistence.
- `Tests/AppleClientTests.swift`:7554–7576 covers appearance choices/defaults.
- `UITests/TVNavigationTestCase.swift`:33 directly uses
  `app.tabBars.buttons["Libraries"]`; replace setup assumptions with semantic
  helpers supporting both shells.

New journeys must cover nested Settings/Sharing editors, Recording detail,
library detail/expanded group, layout/theme changes, compact/expanded width,
retained query/filter/sort/scroll/depth/Back, and TV cancel/current/different/
source/rapid-selection/opener-removal for hardware and companion inputs. Assert
actual native focus and unchanged playback controller/session identity with no
extra Play/Stop. An ImageRenderer hero snapshot is not native shell acceptance.

Recommended sequential Sol M1/M3 ownership: persistent host, HomeView,
coordinator destination adapter, settings/store/theme and focused Home presenter,
library/detail preservation work. Data, paging, media controllers, transport and
authorization retain their current owners.

Ripwire was attempted first and refused its mandatory cache write under this
read-only exploration scope; targeted `rg`, outlines and narrow source ranges
provided the inventory. No native build, test, emulator or physical-device
qualification is claimed by this source audit.
