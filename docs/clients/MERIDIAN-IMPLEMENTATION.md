# Meridian — a selectable Cinema layout and theme across web, Apple and Android

**Status:** implemented across web, Apple and Android; GPT-6 Astra findings
resolved and scoped UI checks passed. Physical-device acceptance remains open.
**Written:** 2026-10-17 · **Source baseline:** `e586725544f0`.
**Owner:** coordinating chat; implementation will be assigned to GPT-6.1 Sol.
**Integration branch:** `effort/meridian`.

This is the canonical implementation and acceptance ledger for Meridian.
Read this head, then the milestone being executed. The user requested the
current Plex layout/navigation/theme, named **Meridian**, on the web and native
Apple/Android clients, rendered previews, an adversarial plan review, and a
separate Sol 6.1 implementation chat supervised by the coordinating chat.
The coordinator writes/reviews the plan and reviews delivery; Sol builds.

Companions: [web shell map](WEB-SHELL-LAYOUT.md),
[TV navigation contract](APPLE-TV-NAVIGATION-CONTRACT.md),
[player input contract](PLAYER-INPUT-CONTRACT.md), and
[development pipeline](../DEVELOPMENT_PIPELINE.md). Re-verify symbols below
against the task's actual base. A source or test gap is a work item, never
permission to silently reduce the native scope or substitute a mockup for a
running application.

## 1. Outcome and reference baseline

Provide independent **Layout: Meridian** and **Theme: Meridian** choices in
Cinema's existing Appearance settings. Preserve current defaults and every
existing layout/theme. The product remains Cinema; no Plex logo, label,
third-party service entry, or runtime dependency is introduced. Historical
compatibility identifiers remain unchanged.

### 1.1 Reference evidence and deliberate adaptations

Inspected 2026-10-17:

| Reference | What it establishes | Meridian interpretation |
|---|---|---|
| [Web interface overview](https://support.plex.tv/articles/200484203-interface-overview/) and [web customization](https://support.plex.tv/articles/customizing-plex-web/) | Search/top controls, left navigation, personal libraries and Home shelves. Search-index excerpts were available; direct support-page fetch was denied. | Desktop top search/account bar and collapsible library sidebar; no claim of pixel-exact measurement from an inaccessible page. |
| [Navigation update, 2026-08-17](https://forums.plex.tv/t/navigation-updates-coming-to-preview-users/941751) and its [published image](https://global.discourse-cdn.com/plex/original/4X/0/4/a/04ae5cc2162dda71f4e2f055d1ab5a5a5d030973.jpeg) | Official TV preview/beta returns primary navigation to the left; favorite libraries become top-level sources. Image visually inspected: narrow floating icon rail, expanded rounded dark panel, white selected pill, cinematic backdrop, poster shelves. | TV left rail that expands into a focus-owning panel. Treat this as the latest published TV direction, not proof that every stable client has the same version. |
| [Mobile navigation](https://support.plex.tv/articles/navigating-the-mobile-apps/) | Published search excerpt describes primary navigation and secondary navigation within sources; direct fetch denied. | Phone bottom destinations, library selector, source-local controls; preserve native platform ergonomics. |

M0 must obtain any further accessible official reference needed to settle a
visual detail and record its URL/date/platform. Do not claim access to the
user's Plex installation or infer unseen screens. No download/repackaging of
Plex artwork, icons, fonts or source code. Use Cinema assets and clearly
labeled fixture titles/art for previews.

### 1.2 What makes this distinct from Catalog

Catalog already has a sidebar and Amber palette. Meridian must add the
collapsible navigation model, top search/account composition, immersive
feature/detail treatment, modern rounded focus/selection treatment and
native counterparts. Renaming Catalog or adding another gold palette alone
does not meet the request. Preserve the existing `plex` → `catalog` storage
migration in the web theme engine; never redirect it to Meridian.

## 2. Appearance and navigation contract

### 2.1 Independent persistent preferences

| Surface | Existing contract | New contract |
|---|---|---|
| Web | `plurx_layout`, `plurx_theme`, `plurx_appearance`; `LAYOUTS`, `THEMES` in [theme.js](../../crates/plurxd/src/web/core/theme.js) | New `meridian` entries; same keys, fallbacks and before-first-paint application. |
| Apple | `ViewerTheme`, `ViewerAppearance`; `plurx.theme`; separate `plurx.tvHomeLayout` | `ViewerTheme.meridian`; new `ViewerLayout` with `standard` and `meridian`, stored as `plurx.layout`, published by AppModel and exposed as Layout. Absent/unknown values resolve to Standard. |
| Android | `ThemeId`, `ViewerPreferences`, SettingsStore; no app-layout setting | `ThemeId.Meridian`; `ViewerLayout.Standard/Meridian`, storage strings `standard`/`meridian`; new device-local app-layout setting `viewer_layout`. Absent/unknown values resolve to Standard. |

The new native storage names are proposed contracts, not claims about existing
keys. M1 checks for collisions before adding them. Keep changes to the actual
existing store abstraction rather than a second preference store. Read/write
tests must cover upgrade with no new key, unknown key, restart and independent
changes to layout, palette, appearance, poster size, Rows/Grid and grouping.
Do not sync the new preference through the server or overwrite sibling fields.

Meridian owns Home presentation when selected. Apple's existing TV Home
Classic/Theater preference remains stored and resumes its exact effect under
Standard; hide its inactive picker under Meridian and explain that Meridian
provides its own Home composition. Do not overwrite that preference. Live TV
layout, library Rows/Grid and grouping remain independent and effective.

Changing layout keeps the current route, query/filter/sort, navigation history,
selected library and any running playback session. Preserve scroll and focused
item by identity where the view survives; otherwise restore the corresponding
visible control after realization. Layout/theme changes must never create a
playback request or reconstruct a native navigation/playback owner.

This applies to **implicit native state as well as coordinator paths**. M0
must inventory each platform's bound paths, implicit stacks, destination-based
links, sheet/player owners and view-local query/filter/scroll state. In Apple
HomeView, only Home/Libraries/Search iOS paths are coordinator-bound today;
Live TV, Recordings, Channels, Downloads and Settings use implicit stacks.
Settings also uses destination-based NavigationLinks. Retaining the coordinator
alone does not retain those routes. M3 must keep their existing mounted stack
identity across shell/size changes, or first move the affected state to a
shared owner with focused migration tests. Separate Standard/Meridian branches
that each instantiate their own navigation-stack subtree are prohibited.
M0 records the chosen lifetime strategy per destination; unsupported native
chrome-hiding APIs are not a strategy. Any required state migration is part of
M3, confined to the affected presentation state, and reviewed before rendering
the replacement shell.

### 2.2 Visual specification to freeze in M0

The following are implementation targets, not measurements of Plex pixels.
The visual table describes **Meridian layout plus Meridian theme together**.
Layout owns geometry, placement, shapes and interaction; theme owns colors.
No Meridian layout CSS selector or native layout branch may hardcode charcoal,
white or gold. Existing themes retain their own palette in the new layout.

| Element | Meridian treatment |
|---|---|
| Canvas | Dark charcoal, layered neutral panels, white hierarchy, restrained warm gold for actions/progress; light variant uses pale neutral surfaces and darker gold. System appearance remains supported. |
| Sidebar | Desktop 232 px expanded / 72 px compact; TV equivalent in native points/dp with ten-foot safe area. Rounded panel, icon + label destinations, clear section separators, selected pill and separate visible focus ring. No hover-only expansion. |
| Header | Desktop 64 px search/account/Appearance row. Compact layouts keep search and account reachable without horizontal overflow. |
| Home | One static feature from existing playable Home data with Resume/Play and Details; Continue watching, Next up, Recently added and existing library shelves below. No autoplay trailer, slideshow timer or new recommendation request. |
| Library | Title/source identity, existing sort/filter/view controls, poster grid/rows and paging; selected source clearly highlighted in navigation. |
| Detail | Readable backdrop gradient, poster/identity, metadata, primary play/resume and existing secondary actions; versions, episodes and children remain accessible. No metadata or playback-decision reimplementation. |
| Phone | Bottom Home / Libraries / Search / More; More contains existing allowed ancillary destinations. Library selector and library-local controls sit above content. Safe-area insets and keyboard must not cover controls. |
| Tablet | Left rail at expanded width; compact mode follows phone. Size-class/window changes must preserve the same route and data owners. |
| TV | Compact rail while browsing; explicit activation/left-edge transition opens panel. Selected destination and physical focus are visibly different. Back closes panel before leaving the route. |

Desktop breakpoint targets: <600 px compact phone; 600–1099 px compact rail;
≥1100 px expanded sidebar. Native implementations use available width/size
class and input capability, not device-model strings. M0 may refine sizes
after rendering, documenting the final tokens before M2–M4 start.

Proposed **Meridian theme** core palette (freeze after machine contrast checks
in M0; these values do not belong to the Meridian layout):

| Token | Dark | Light |
|---|---|---|
| background / surface / raised | `#141416` / `#1e1e21` / `#29292d` | `#f5f4f1` / `#ffffff` / `#eae8e3` |
| primary text / muted | `#f5f5f6` / `#aaaab2` | `#202024` / `#606068` |
| accent / on-accent | `#e5a00d` / `#171205` | `#805600` / `#ffffff` |
| selected fill / ink | `#f5f5f6` / `#202024` | `#29292d` / `#ffffff` |

Use these semantic roles in shared controls on all platforms; names below are
the web CSS contract and native adapters must expose equivalent roles:

| Role | Meridian theme | Other themes' default mapping |
|---|---|---|
| `--selected-bg` / `--selected-ink` | Selected fill/ink above | Active text / background tokens |
| `--focus-ring` | Active accent, validated against adjacent selected surfaces | Active accent; choose an active-theme contrasting outline pair when one ring cannot meet 3:1 on both neighbors |
| `--action-bg` / `--action-ink` | Accent / on-accent above | Existing accent / button-ink tokens |
| `--hero-scrim` / `--hero-ink` | Active background / text tokens | Active background / text tokens; opacity/gradient geometry belongs to layout |

Resolve these roles from the active palette on every theme/appearance change,
including fallback/cleanup when leaving Meridian, so custom values cannot leak
into another theme. M0 must specify any two-tone focus-ring treatment needed
for dark ink against bright selection and dark panels. Hero scrim opacity must
provide the required contrast over the lightest/darkest fixture artwork;
increase the scrim rather than switching to fixed white text in layout code.

All body, muted, button, status and action text must meet 4.5:1 on their actual
surfaces. Interactive outlines/focus indicators must meet 3:1 against adjacent
surfaces; selection is also indicated structurally/semantically. Measure
composited hero text with scrims, not only flat theme tokens. Respect Reduce
Motion, Reduce Transparency/high contrast where available, text scaling and
44 pt/48 dp touch targets. Web tokens that the existing checker covers must
all pass without new allowlist exceptions. Native tables need equivalent
tested foreground/background pairs, including disabled and selected states.

### 2.3 Destination identity and ownership

Build a destination projection from the existing route inventory and permission
rules, keyed by stable semantic destination/library IDs. Home, Libraries,
Search, Shared libraries, Live TV, Recordings, Library channels, Activity,
Settings/account, Downloads/readers where available, Remotes and Help remain
reachable. Do not assume platform inventories are identical. M0 records each
platform's existing route → Meridian placement; missing routes are blockers.

Local libraries are top-level sidebar entries where space permits, with a
scrollable Libraries/More entry for the rest. Stable ordering follows Cinema's
existing library order. New pin/reorder persistence is out of scope; the visual
reference's favorites do not justify a new server feature. Source removal or
feature disable resolves through existing route policy and a visible fallback.
Admin destinations remain admin-only; a different shell cannot grant access.

TV panel opening transfers actual focus into it and excludes covered content
from interaction. Panel outcomes have distinct ownership:

| Outcome | Route/stack result | Actual focus result |
|---|---|---|
| Cancel/Back/Escape | Leave current route and stack untouched. | Restore the opener only if it still belongs to the visible active scope; otherwise next item → previous item → visible recovery control. |
| Select current primary destination | Preserve its current stack; no duplicate push, pop-to-root or refresh merely from selection. | Close panel to that destination's remembered visible target or designated first control. |
| Select different primary destination | Activate its preserved stack using the existing destination owner. | Cancel all old-scope restoration requests; realize and focus the new active destination's remembered target or designated first control. Never return to an inactive mounted tab. |
| Select a library source | Web uses the existing library hash route/history and suppresses duplicate navigation to the same route. Native: same library owning the Libraries stack retains that stack; a different library replaces only the Libraries branch with Libraries overview → selected library root, with no chain of previously selected libraries. Other destination stacks remain unchanged. | After committed source identity and content realization, focus the current source target or its first usable control; no old-source focus restore. |

M0 records the exact existing stack owners and adapters implementing this table
for each platform, including disabled/deleted-source recovery. A current
destination selection differs from explicit source selection; do not implement
both with one unconditional reset helper. Focus completion is generation-bound
and cancelled by newer navigation or input. Left from content must
not override the existing library-tools path: consume local spatial movement
and tools first, then expose a labeled navigation control at that boundary.
Do not bind an ordinary direction to selecting a route. Escape/Back dismisses
the topmost owned overlay once. Hardware and companion remote inputs share
the same semantic destination and focus model. Hidden controls remain outside
the focus graph. Preserve the existing playback reveal/Back contracts.

## 3. Implementation seams and file ownership

These are entry points, not permission to read entire large files. Use Ripwire,
then outline and narrow ranges; exclude recovery copies from conclusions.

| Packet / owner | Files and required seams |
|---|---|
| M1 web preferences / Sol | [core/theme.js](../../crates/plurxd/src/web/core/theme.js), [core/theme-menu.js](../../crates/plurxd/src/web/core/theme-menu.js); registry, independent storage and picker. |
| M2 web shell / Sol | [layouts/renderers.js](../../crates/plurxd/src/web/layouts/renderers.js), new Meridian renderer, [app.css](../../crates/plurxd/src/web/app.css), [index.html](../../crates/plurxd/src/web/index.html), [http/web.rs](../../crates/plurxd/src/http/web.rs), shell-map table. Reuse catalog/theater compositions and shared card/rail/grid helpers without coupling Meridian to their DOM mutations. |
| M1/M3 Apple / Sol, sequential | [Models.swift](../../clients/apple/Sources/Models.swift), [SettingsStore.swift](../../clients/apple/Sources/SettingsStore.swift), [AppModel.swift](../../clients/apple/Sources/AppModel.swift), [Theme.swift](../../clients/apple/Sources/Theme.swift), [SettingsView.swift](../../clients/apple/Sources/SettingsView.swift), [HomeView.swift](../../clients/apple/Sources/HomeView.swift), [RemoteNavigationCoordinator.swift](../../clients/apple/Sources/RemoteNavigationCoordinator.swift), [TvHomeLayout.swift](../../clients/apple/Sources/TvHomeLayout.swift), shared library/detail components only as needed. |
| M1/M4 Android / Sol, sequential | [ViewerPreferences.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/ViewerPreferences.kt), [SettingsStore.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/SettingsStore.kt), [AppViewModel.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt), [SettingsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SettingsScreen.kt), [Theme.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/theme/Theme.kt), [MainActivity.kt](../../clients/android/app/src/main/java/tv/plurx/app/MainActivity.kt), [HomeScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/HomeScreen.kt), shared library/detail components only as needed. |
| M5 evidence / Sol; review / coordinator | Existing web/native tests, capture harnesses and source-bound evidence. Sol owns release counters and delivery-ledger updates. Coordinator owns review findings/dispositions; serialize edits to this document. |

Web keeps the `chrome(active, inner)` and whole-body Home/item seams, but library
remains `{shell, items, count}` with per-region fallback. Preserve `#main`,
`#libbody`, `#libpager`, `#librail`, `#libcount`, the window scroll owner,
incremental paging, Home asynchronous slots, `navChanged`, `libsChanged`,
`stickyFloor`, search focus capture/restore, `wireSearchInput` and activity
polling. Add no duplicate polling, fetches or event listeners on shell changes.
Register any new asset in file, WEB_ASSETS, shell and shell-map row together;
classic global scripts still require served order and `"use strict"`.

Apple keeps RemoteNavigationCoordinator's selected tab and per-tab paths,
plus the implicit stacks/destination links inventoried under §2.1.
Do not replace it or use a layout-dependent `.id` on the navigation owner.
Preserve notifications/deep links, feature-change fallback, DVR observation
and refresh lifecycle. iOS/iPadOS and tvOS require actual app-shell work;
adding a tvOS hero enum alone is insufficient. macOS web use is covered;
a new native macOS app is not in scope.

Android keeps MainNav's NavController, entry-owned remote scopes and
`popBackStackFrom(entry)` discipline. Compose the shell around the existing
NavHost without resetting its back stack. Extend `ViewerColors` with explicit
button ink/on-primary mapping for Meridian: hardcoded white on gold is a known
contrast trap. Preserve all other palettes' behavior and player black surfaces.

## 4. Build packets and review checkpoints

Use one effort with reviewable task branches based on its current head.
Shared owners above make the independent-main exception inapplicable.
No production code changes before plan findings are addressed and coordinator
dispatches Sol. Each packet ends with a compact review handoff: exact commit,
diff stat, affected outlines, regression commands/results and limitations.

| Packet | Work | Acceptance / handoff |
|---|---|---|
| M0 — reference, route map, visual contract | Confirm source baseline/toolchains, enumerate destinations and all bound/implicit presentation-state owners, specify their lifetime/migration and selection adapters, validate semantic tokens, produce desktop/phone/TV design renders using Cinema components or clearly labeled prototypes. Freeze dimensions/tokens and document remaining adaptations. | Coordinator compares against platform-specific references, verifies every existing destination has a placement and viable state-preservation strategy, and shows renders to user. Hard internal checkpoint before M2–M4; no extra user approval required to continue. |
| M1 — independent preferences/palette | Add web and native Meridian identities, persistence and theme values; picker exposure only with a working layout implementation in that platform's task candidate. Split implementation commits if useful, but do not land inert selectable layout entries. | Upgrade/unknown-ID/independence tests and contrast checks pass; existing defaults unchanged. |
| M2 — web | Implement responsive shell, Home/detail presentation and library composition with existing data/playback ownership. Integrate M1 web slice atomically. | Actual shipping-asset browser journeys pass; desktop/mobile screenshots; Classic/Catalog/Theater controls retain behavior. |
| M3 — Apple | Implement iPhone/iPad/tvOS shell, Home/library/detail adaptations and semantic remote destinations. Integrate M1 Apple slice atomically. | iOS/tvOS builds plus focused unit/UI journeys; native screenshots with actual focus and Back evidence. |
| M4 — Android | Implement phone/tablet/TV shell and Home/library/detail adaptation with preserved NavHost. Integrate M1 Android slice atomically. | JVM/lint and instrumented phone/tablet/TV journeys; native screenshots, D-pad and Back evidence. |
| M5 — integrated review | Complete cross-layout/theme/settings matrix and gallery, review diff adversarially, address findings, run affected integrated checks. | Coordinator verifies exact commit and independently inspects renders/tests; no unresolved P1/P2 correctness issue. |
| M6 — promotion | Freeze task merges, integrate current main, requalify resulting candidate and prepare main-bound PR under repository rules. | Required current-candidate promotion gate and qualification receipt; implemented, merged, simulator-qualified and physical-accepted states reported separately. |

The coordinator authorized concurrent GPT-6.1 Sol implementation for M2/M3/M4
on isolated `codex/meridian-web`, `codex/meridian-apple` and
`codex/meridian-android` branches and worktrees. Each native worker owns its
platform files, tests and release counters; web owns web files and the current
shared document changes. The coordinator integrates the shared ledger. This
supersedes the original sequential packet policy without allowing concurrent
edits to a shared file. Fresh GPT-6 Astra review examines each actual PR head
and findings are repaired before integration. Implementation does not wait for
CI; workers provide native screenshots and evidence at checkpoints. Required
merge and promotion gates still apply.

## 5. Tests and rendered evidence

### 5.1 Behavioral matrix

| Area | Required cases |
|---|---|
| Persistence | Fresh install/upgrade/unknown ID/restart; layout and theme changed separately; Standard retains Apple Home choice; Rows/Grid, grouping and Live TV preferences unaffected. |
| Navigation | Every existing allowed destination; correct active library; deep link from cold/warm start; Back preserves entry owner; repeated shell switches; source removed/feature disabled/admin versus viewer. |
| Content | Home hubs arrive independently; empty/error/loading; one and many libraries; long titles; photos/books; movie, show, season, episode, missing file and multiple versions. |
| Playback | Existing Play/Resume and Details work; return retains source/item; switch theme/layout while playback exists without another start/session; fullscreen/overlay Back still owned by player. |
| Input | Mouse, keyboard Tab/Shift-Tab/Escape, touch, native D-pad and companion inputs; panel open/select/cancel/restore; hardware focus asserted, not merely a requested ID. |
| Geometry | Web 390, 768, 1024, 1440/1936 px; phone narrow 320 dp/pt and large text; tablet portrait/landscape/split width; TV 1920×1080 capture; no horizontal overflow or clipped focused cards. |
| Orthogonality | Meridian layout × every platform's shipped theme; Meridian theme × every shipped layout; light/dark/system transitions; each poster size; Standard controls as regression checks. Full exhaustive unit/token checks plus representative actual-shell visual journeys. |

Review-derived mandatory journeys (assert behavior, not only registry entries):

1. On Apple, enter nested Settings, a Recording detail and a library detail;
   switch layout and cross compact/expanded geometry. The same route, stack
   depth, scroll/filter state and subsequent Back destination must survive.
   Cover coordinator-bound paths, implicit stacks and destination-based links.
2. On both TV platforms, panel cancel, current destination, different
   destination, specific library, rapid selection then Back and opener removal
   each assert physical focus in the active destination and one transition per
   Back. Repeat the same semantic journeys with companion input.
3. Render Meridian layout with two visibly different existing themes in both
   appearances and Meridian theme with each existing layout. Assert actual
   computed/rendered selected, focused, action and hero roles against the
   active palette and contrast thresholds, including switching away from
   Meridian to catch leaked overrides.

### 5.2 Existing checks to extend and run

Web: [library-layout.browser.cjs](../../tests/web/library-layout.browser.cjs)
serves real assets through fixture API responses and supports
`LIBRARY_SCREENSHOTS`. Extend it to Meridian and add shell-switch/route/focus
journeys. Run asset-order/load/layout, layout-containment, theme-family,
syntax and contrast checks through `scripts/quiet make web-check`. Run
`scripts/ui-baseline` for the integrated server; inspect new portable golden
entries deliberately instead of accepting unrelated baseline drift.

Apple: extend `Tests/TvHomeLayoutTests.swift`, preference tests and shipping
UI journeys under `clients/apple/UITests`. The existing
`TVNavigationTestCase.openLibrary` assumes `app.tabBars.buttons["Libraries"]`;
adapt test navigation to stable semantic destinations for both layouts.
Assertions require actual native `hasFocus`, bounded remote presses and exact
opener restoration. Screenshot-only `ImageRenderer` cards are not shell proof.

Android: extend `ViewerPreferencesTest`, `ThemeLayoutTest`, `BackNavigationTest`,
Home feature-visibility/narrow-width tests and remote navigation tests. Existing
dark-card-only theme tests cannot establish a new shell. Instrument the actual
phone/tablet/TV NavHost, not a second demo implementation. Assert physical
D-pad focus and once-only Back, including stale-entry callbacks.

Commands, selecting the smallest relevant tests per task and retaining full
logs under `target/quiet`:

```bash
scripts/quiet make web-check                  # JS, contracts and palettes
scripts/quiet make apple-build                # iOS and tvOS compilation
scripts/quiet make apple-tv-navigation-test   # native remote journeys
scripts/quiet make android-test              # JVM units and lint
scripts/quiet make android-instrumentation-build  # emulator app + test APKs
make paperwork                              # bookkeeping before every push
```

Select actual simulator/emulator destinations explicitly and retain their
runtime/build identifiers. Record focused xcodebuild/Gradle filters and counts
in each packet. The Android instrumentation runner uninstalls packages: use
only a disposable emulator for this effort. Unmerged mobile builds are for
simulators/emulators, not installation on personal physical devices. Missing
runtime capacity is a named acceptance gap; continue independent work but do
not call the corresponding client qualified.

### 5.3 Gallery the user can inspect

M0 delivers labeled design renders; M2–M5 replace them with screenshots of the
running implementation. Minimum final gallery: web Home/library/detail and
Appearance (desktop plus phone), Apple iPhone/iPad/TV Home and library/detail,
Android phone/tablet/TV equivalents, expanded/collapsed TV navigation with
focus visible, and dark/light paired samples. Include long-title/empty and
non-admin examples in evidence even if not all enter the summary contact sheet.

Each capture records source commit, platform/runtime, viewport or device,
layout/theme/appearance, route, fixture/live status and reproduction command.
Use local output files under `target/meridian/`; link them with absolute paths
in chat and a compact gallery index. Keep source references, render scripts
and machine-checkable manifests reviewable; do not commit bulky binaries.
Use the app's real render path. Mark every prototype and synthetic fixture;
do not present a web mockup as iOS/Android acceptance. Show renders inline to
the user at the design checkpoint and after implementation review.

## 6. Integration, validation and boundaries

Establish a pinned Rust 1.97.1 compiler loop before editing the web asset table
or any Rust. The coordinator observed default local `rustc` **1.98.0**, which
does not satisfy that requirement, and separately verified the installed
rustup toolchain reports **1.97.1 (8bab26f4f 2026-07-14)**. Select the rustup
shim ahead of Homebrew and verify the effective compiler in the worktree;
the repository toolchain file pins 1.97.1. Use that toolchain or the
[source-only loop](../ci/AGENT-COMPILE-LOOP.md); never transfer `.git` or
credentials to a compiler host. Check, Clippy `-D warnings`, formatting and
focused affected tests must run on the exact intended base before pushing.
Likewise establish Apple/Android compiler and disposable runtime availability
early, rather than discovering native acceptance blockers after the web is done.

Create each `codex/meridian-*` task branch from current `effort/meridian`, PR
back to that effort, normal commits/hooks, focused behavior regression and
manual **Effort development gate** before every task merge. This gate does
not substitute for native or Rust unit evidence. Behavior subjects use `fix(`
or `perf(` per AGENTS; carry valid `Regression-Test: <path>::<test>` lines in
PR descriptions and landing messages, including `MergeMessageField` for
Forgejo API merges. Use installed configured forge tooling; no credential
printing. New/moved docs need index entries in the same commit. Apple/Android
source changes need their prescribed build counters and release bookkeeping.

Promotion requires current-main integration, current-candidate qualification
receipt and the required main promotion/affected-surface gate. Never merge a
red or stale candidate. Review status and findings before declaring completion.
No production deployment or physical-device installation is implied by
implementation; prepare reviewable changes and report these separately.

Guardrails: no server API/schema/recommendation or library-pinning project;
no playback transport rewrite; no simultaneous rewrites of existing layouts;
no replacing feature/permission policy with view-local copies; no new feature
gate. Layout is a permanent user preference in Appearance, not a temporary
Developer switch. Acceptance status belongs in this ledger.

## 7. Review dispositions and delivery ledger

Independent adversarial review completed 2026-10-17 by
`meridian_adversarial_review`; verdict was request changes with three P2
findings. The coordinator accepted all three and amended the plan before
dispatch. No product code or native acceptance is claimed by this disposition.

| Finding | Disposition / contract correction | Verification required |
|---|---|---|
| R1 · P2 · Apple implicit navigation can disappear even if the coordinator survives | Accepted. §2.1 now inventories implicit stacks/destination links and requires a persistent stack identity or explicit state migration before replacing chrome; duplicated conditional stack trees are prohibited. M0 must choose and justify per-destination lifetime strategy. | §5.1 journey 1 covers nested Settings, Recording and library routes, geometry/layout changes and subsequent Back. |
| R2 · P2 · Cancel and destination commitment cannot restore the same opener | Accepted. §2.3 now separates cancel, current primary, different primary and explicit source selection, defines native/web stack semantics and cancels old-scope focus requests. | §5.1 journey 2 asserts actual active-scope focus and once-only Back for hardware and companion inputs. |
| R3 · P2 · Palette ownership was ambiguous under independent layout/theme choices | Accepted. §2.2 assigns colors to theme and geometry to layout, defines selected/focus/action/hero semantic roles, fallback mappings and cleanup on theme change. | §5.1 journey 3 checks rendered/computed roles and contrast across distinct palettes/appearances and switching away. |
| C1 · Reference fidelity checkpoint | Retained as a hard M0 checkpoint. TV preview evidence cannot establish desktop/mobile fidelity; record platform references and label adaptations before implementation. | Coordinator inspects M0 route/state-owner map and render gallery before releasing M2–M4. |

| Packet | State | Commit / evidence / remaining work |
|---|---|---|
| Plan | Reviewed; findings addressed | Coordinator authored against `e586725544f0`; R1–R3 accepted and corrected; M0 checkpoint still owed. |
| M0 | Delivered; Astra approved | [Visual contract](MERIDIAN-M0.md), [Apple state inventory](MERIDIAN-APPLE-STATE.md) and [Android state inventory](MERIDIAN-ANDROID-STATE.md) arrive through PR #1170 at `3e04ef30471a588d3214f4f2de594303bb4b37cd`, reviewed by GPT-6 Astra with no actionable P1/P2 findings; coordinator approved the rendered direction. Remote lint run 5391 timed out after 601 seconds during dependency/crate checking; local pinned lint and UI checks passed. The broader Effort gate remains unsatisfied. |
| M1 web + M2 | Delivered; Astra approved | PR #1172 merged into the effort at `addde63e0`. GPT-6 Astra approved exact product head `5839b8235`: phone search and companion source-history findings resolved, no remaining actionable findings. Eleven browser journeys and 43 focused UI units passed; 22 clean-head shipping-asset captures live under `target/meridian/web/`. Main integration preserves current subtitle playback assets. |
| M1 native + M3/M4 | Implemented; Astra cleared | Apple final `c3f59d528` follows PR #1174 through #1176; Android final `1c7ec8b18` follows PR #1173 through #1177. Exact reviewed production blobs are retained. Latest affected Apple iPad tests 3/3 and extended native TV focus journey passed; Android production UI tests 7/7 on each phone, tablet and TV plus focused JVM/lint passed. Physical-device and oldest-supported-runtime qualification remain open. |
| M5 | Scoped UI evidence and Astra review complete | Web 11 browser journeys and 66 focused UI units passed against current main. Native simulator/emulator receipts cover the affected state, playback, search, source and focus fixes; each platform retains source-bound captures. No remaining material Astra findings. |
| M6 | Main delivery | The user authorized all reviewed Meridian PRs and final fixes to merge after scoped lint/syntax and UI checks without waiting for broader CI. Physical qualification and broad promotion-gate evidence are separate and are not claimed. |


### Web checkpoint validation policy

The human requested prompt product delivery, proper PRs and GPT-6 Astra review,
with lint/syntax and UI-related checks only plus required bookkeeping. No
unrelated full backend unit suite or broad compiler/unit CI lane is dispatched.
Web's retained shell/main seam keeps current page generation and mounted library,
preplay, reader and playback owners while updating navigation. Other palettes
retain their original tokens; Meridian resolves safe action/muted roles from
the active palette, including a derived accent shade when an existing mid-tone
accent has no safe text ink. The temporary M0 dependency does not satisfy a
merge gate or claim native qualification.
