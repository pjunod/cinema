# Meridian M0 — reference, state ownership and design evidence

**Status:** open; M0 evidence prepared for coordinator review, product work not
started. **Inspected:** 2026-10-17. **Source base:** `85ae74eb6980` on
`effort/meridian`. **Implementation owner:** chat
`01a14cba-e609-7f71-93af-db0b2b797db3`.

Companion to [the implementation and acceptance ledger](MERIDIAN-IMPLEMENTATION.md).
This records what M0 inspected, the proposed lifetime adapters and the reproducible
render contract. It does not qualify any native client. The source inventories
below describe the inspected base; line numbers refer to that base.

## Reference interpretation — each platform has its own evidence

All references were rechecked on **2026-10-17**. The interface uses Cinema's
existing icon/glyph assets and original synthetic titles/vector artwork. No
reference art, fonts, icons or third-party service destinations are packaged.

| Platform / reference | Observed evidence | Frozen interpretation and limits |
|---|---|---|
| Web: [Interface Overview](https://support.plex.tv/articles/200484203-interface-overview/) (page last modified 2025-04-08), [Customizing Plex Web](https://support.plex.tv/articles/customizing-plex-web/) | Current search-index text exposes left source navigation and the top search/account/action bar. Direct support fetch returned 403. | Expanded source sidebar, explicit collapse button, search/account header, source-local controls and shelves. Dimensions below are Cinema targets; no pixel measurement or claim to the user's installation. Existing library order; no new pin/reorder feature. |
| Phone: [Navigating the Mobile Apps](https://support.plex.tv/articles/navigating-the-mobile-apps/) | Search-index content describes bottom source navigation, a library selector and source-local secondary controls. Direct fetch returned 403. | Home/Libraries/Search/More uses Cinema destinations; native sources remain accessible above library content. This is a deliberate adaptation of the published pattern, not a claim that the reference has those exact four labels. |
| TV: [Navigation Updates](https://forums.plex.tv/t/navigation-updates-coming-to-preview-users/941751) (published 2026-08-17), [published image](https://global.discourse-cdn.com/plex/original/4X/0/4/a/04ae5cc2162dda71f4e2f055d1ab5a5a5d030973.jpeg) | Announcement directly fetched; image visually inspected in the browser. Preview Apple TV/Roku and Android TV beta return primary navigation to the left. Image shows a compact floating rail, rounded expanded source panel, selected pill and cinematic content. | Native TV rail/panel with actual focus ownership and Cinema sources. The render deliberately shows selected Home and focused Movies separately. This is the latest inspected published direction, not proof of all stable-client versions. |
| Tablet | No separate tablet screenshot was measured. | Available-width adaptation of the native phone destinations: rail at expanded width, bottom destinations at compact width, persistent owners across transitions. iPad and Android folds are acceptance targets, not inferred screenshots. |

## Frozen dimensions and semantic roles

Layout geometry stays independent from the active palette. Candidate theme
values live in [tokens.json](../../scripts/meridian/tokens.json); the preview
resolves role aliases from the selected palette for every render. Production
M1 must do the same on every theme/appearance transition and remove stale
optional variables when leaving a theme.

| Geometry token | Frozen target |
|---|---|
| Web expanded / compact source rail | 232 / 72 CSS px; expanded at ≥1100 px, compact at 600–1099 px |
| Web header / phone bottom navigation | 64 px / at least 76 px including the preview inset; production adds actual safe-area inset |
| Phone width | <600 CSS px; native available-width policy rather than device model; 44 pt / 48 dp minimum controls |
| Home feature | Single existing playable Home item, static composition; desktop 375 px illustrative height; phone art above copy; no new fetch or timer |
| TV composition | 1920×1080 review capture; 48 px left / 60 px bottom capture-safe margin; 72 px collapsed panel, 330 px expanded panel, 58 px navigation rows in this capture |
| Native TV logical units | Map the capture through actual runtime scale: initial tvOS 72 pt rail / 330 pt panel / ≥58 pt rows; Android 1080p density-2 target 56 dp rail / 280 dp panel / ≥48 dp rows. Safe area follows runtime insets. Final logical dimensions must be checked in native captures rather than copied as pixels. |
| Shape / focus | Rounded source panel 24 px; selected pill; 3 px inner gap plus 3 px outer ring (TV capture 4 + 4 px), geometry owned by layout |
| Hero scrim | At least 94% active background beneath **every text bounding box**, increasing to opaque as needed. Geometry may fade toward artwork only beyond the text zone. No fixed white hero ink. |

| Semantic role | Meridian | Other active themes |
|---|---|---|
| selected fill / ink | Dark `#f5f5f6` / `#202024`; light `#29292d` / `#ffffff` | Active text / background |
| action fill / ink | Active accent / on-accent (`--btn-ink`) | Existing accent / existing button ink |
| hero scrim / ink | Active background / active text | Active background / active text |
| focus outer / gap | Active accent / active background | Same mapping initially; validate each palette, choose an active-theme contrasting pair if necessary |
| disabled text | Full-opacity muted ink on the actual neutral surface, with disabled semantics and visible unavailable wording | Preserve each theme's existing mapping, with equivalent state checks |
| status / prose | Explicit full-opacity values in candidate token table | Existing palette values; layout must not substitute Meridian status colors |

A single gold outline fails against bright selection. The frozen two-tone
indicator puts the active-background gap beside the selected/action fill and
the accent ring outside it against the neutral surface. The gap and outer
ring each have a tested neighboring surface. Selection also has an explicit
pill and semantic selected/current state; color alone is insufficient.

The repository checker passes **50/50** candidate text/action pairs at 4.5:1,
without an allowlist. The preview audit adds selected/disabled-equivalent,
two-tone focus, worst-case hero compositing and actual screenshot-pixel hero
measurements. The initial complete candidate matrix minimum is **4.667:1**;
this includes status colors. Core text/muted/accent minima remain approximately
**6.28:1 dark / 5.09:1 light**. These values are evidence for this candidate,
not acceptance of each shipped control or every existing theme.

## Web — preserve the current router and presentation owners

Verified entry points: [router.js](../../crates/plurxd/src/web/router.js):57–138,
[chrome.js](../../crates/plurxd/src/web/core/chrome.js):9–73,
[theme-menu.js](../../crates/plurxd/src/web/core/theme-menu.js):30–89 and
[library-grids.js](../../crates/plurxd/src/web/layouts/library-grids.js):335–437.
The web route table below covers the router's complete explicit dispatch;
unknown hashes retain its current Home fallback.

| Existing route / presentation | Meridian placement / lifetime |
|---|---|
| `#/` | Home; source shelves remain Home-owned asynchronous slots |
| `#/library/:id`, `#/category/:key` | Individual source entries; Libraries opens the existing Home library overview/section, not a duplicate server request |
| `#/search/:query` | Header Search / phone Search; query remains in hash, existing input debounce and focus capture survive |
| `#/item/:id` | Existing detail child; preserve `NAV_ORIGIN`, version/track choices and Back/history |
| `#/shared`, `#/shared/library/:import/:server/:epoch/:library`, `#/shared/item/:import/:server/:epoch/:library/:item` | Shared libraries via expanded navigation or More; preserve full source reference and library `?q=` query. Source identities are not local library IDs. |
| `#/live-tv` | Direct expanded/TV entry, phone More; existing guide/player/overlay state and disabled-state route policy |
| `#/recordings…` | Direct expanded/TV entry, phone More; existing recordings detail/activity routing and `DVR_PAGE` ownership |
| `#/library-channels` | Direct expanded/TV entry, phone More; existing channel editor/playback owner |
| `#/activity`, `#/analysis` | Activity / its Analysis tools; existing job inspectors and polling |
| `#/settings/:section`, `#/settings`, legacy `#/admin` | Admin Settings; all existing sections below remain inside their existing frame |
| `#/settings/developer` for viewers | Account → Device preferences, using existing viewer policy; do not hide this behind the admin Settings link |
| `#/remote` | Account / More → Phone remote; existing grant/connection owners |
| `#/read/:item/:file` | Existing reader, reached through detail; retain reader mount/WebView bridge |
| Photo lightbox | Existing grid-owned `PHOTO_SET`, `LB_AT`, `LB_OPENER`; body-level modal remains outside shell replacement |
| Help, diagnostics, server QR, sign-out, pairing | Existing account dialogs/owners; no synthetic Help route |

Settings' manifest at `pages/settings.js`:104–118 has Libraries, Metadata,
Playback, Live TV, Analysis, Maintenance, Users, System, Cluster, Sharing,
Integrations and Developer. Preserve its current admin/viewer/cluster rules,
`SETTINGS_DATA` aggregate, in-flight map, section navigation and unsaved inputs.
Do not rebuild its form merely to move navigation.

**Chosen M2 adapter.** Add a presentation-only shell recomposition at the
existing layout seam. Capture the current rendered model at `layoutView` and
library shell/region seams, route identity and realization generation. Move
rather than discard live DOM owners (forms, reader, current player/surface,
library region mounts and current loaded rows). Home/detail presentation may
recompose from their retained current models; preserve independent Home slots
and refresh completions in the same generation. Library moves `#libbody`,
`#libpager`, `#librail`, `#libcount`, browse controls and row nodes into the new
shell, retaining the `LIB_VIEW.draw` closure and loaded accumulator. Preserve
window scroll, per-row scroll, current page/group, query/filter/sort and focused
item identity. Fresh route/data loads still use the existing router.

This adapter is required because current `setLayout()` calls `render()`;
`libraryView()` resets `LIB_PAGE_AT`, `LIB_SCOPE`, `LIB_FIND`, `LIB_GROUP` and
row state. `render()` also calls `closePlayer({routeLeave:true})` when `WATCH`
exists, retires shared resource owners and replaces page-scoped timers. Merely
saving `window.scrollY` does not meet the new contract. M2 must prove an active
playback session, nested Settings fields and filtered/grouped/paged libraries
survive the switch with no additional start request or duplicate listener.

**Web selection.** A current destination/source compares canonical hashes and
does nothing; another source commits the existing library hash once, using
browser history. Navigation panel cancel restores only a connected, visible
opener; committed selection invalidates its old restoration ticket. Keep
`CinemaRemote` scope/generation ownership and topmost player/reader/modal Back.
Use `featureVisible()` and existing role checks; shell projection adds no rights.

## Native adapters — preserve owners before rendering new chrome

The [Android inventory](MERIDIAN-ANDROID-STATE.md) enumerates every existing
Android destination, local/implicit stack, presentation and playback owner. The
[Apple inventory](MERIDIAN-APPLE-STATE.md) does the same for iPhone/iPad/tvOS.
These are implementation requirements for M3/M4; their native lifetime proofs
remain due. Adding an enum or retaining a coordinator by itself is insufficient.

| Platform | Chosen adapter / required migration |
|---|---|
| Apple iPhone/iPad/tvOS | One persistent native destination host shared by Standard and Meridian, with a stable child hosting controller per existing stack and the same shared AppModel/coordinator. Standard/native chrome and Meridian rail/bottom/panel surround those children. Geometry changes reframe the same child. Preserve implicit NavigationStacks, destination links, local sheets/covers and their view identity. No layout-dependent `.id`, duplicate conditional stack subtree or unsupported chrome-hiding fallback. |
| Apple supported 17.x | Project targets iOS/tvOS 17. Native `setTabBarHidden` is 18+, so it cannot be the only strategy. Prototype the persistent child-controller host using supported UIKit containment on 17 before production chrome; keep the hosting controller and root stack alive while swapping its surrounding chrome. Native tests must verify covers, focus environments and nested implicit Back. |
| Android phone/tablet/TV | Keep the single existing MainNav `rememberNavController`, providers, DVR controller, NavHost and app overlays at fixed composition sites. A responsive shell changes modifiers/slots around that NavHost. There is no separate TV destination field in the inspected source; `entry.destination.route` belongs to this controller. |
| Android primary branches | Existing controller is linear and has no Libraries overview or per-primary saved branches. Add a real Libraries overview at the same controller plus reviewed branch bookkeeping/nested graph save/restore. Move ordinary `remember` state needed across branch suspension into an entry-owned state holder/Saver first; `restoreState=true` alone does not retain it. No second navigation controller. |

**Primary selection.** Current primary selection closes the panel while retaining
its current path. A different primary activates its preserved branch. Apple uses
the current selected-tab owner plus the retained implicit stack hosts; Android
uses the same NavController's reviewed branch adapter. All chrome, deep-link and
companion selection paths call the same semantic adapter. Explicit library
selection is distinct: same source retains Libraries history; different source
replaces only Libraries with overview → selected root, keyed by stable IDs.
Other branches remain unchanged. Source deletion invalidates only that source
and returns to the existing visible overview/recovery control under current
feature/permission policy.

**TV focus and Back.** Opening a panel transfers real focus into it and removes
covered content from interaction/focus. Cancel may restore its visible opener;
current-destination selection targets that branch's remembered visible item;
different-destination/source selection cancels the old restoration and targets
the newly realized active scope. Ticket identity includes entry/tab, committed
source, navigation generation and input/focus revision. Try remembered target,
next item, previous item, then visible recovery control. Never focus an inactive
mounted tab. Consume local spatial movement and library tools before exposing
the labeled navigation boundary. Back closes exactly the topmost owned overlay,
then panel, then the existing entry/stack; playback reveal/Back stays player-owned.
Hardware and companion inputs use that model, with actual native focus asserted.

Android `enterMenu()` currently refuses restricted routes, including Settings,
Recordings, Channels and Downloads. M4 needs an explicitly safe chrome panel
capability over those routes while retaining their restricted content policy;
blind reuse either fails navigation or over-grants companion actions. This is a
presentation capability change with focused tests, not a permission bypass.

## Reproduce and read the gallery

Source harness: [preview.cjs](../../scripts/meridian/preview.cjs),
[preview.html](../../scripts/meridian/preview.html),
[tokens.json](../../scripts/meridian/tokens.json). Outputs are ignored artifacts
under `target/meridian/m0/`; bulky PNGs are not committed.

```bash
# Point these variables at existing installed packages if they are not on Node's search path.
PLAYWRIGHT_MODULE=<installed-playwright> SHARP_MODULE=<installed-sharp> \
  node scripts/meridian/preview.cjs
scripts/contrast-check --from-json scripts/meridian/tokens.json
# Open target/meridian/m0/gallery.html, or inspect contact-sheet.png.
```

The harness emits 14 labeled capture pages and PNGs: desktop Home dark/light,
library/detail; phone Home dark/light, library, More and narrow 320 px; tablet;
TV collapsed/expanded; Terminal-dark and Classic-light palette composition
studies. `semantic-states.png` covers selected/focused/action/disabled samples;
`desktop-home-{dark,light}-art-{0,255}.png` shows the actual hero text rendered
over black/white fixture extremes. `manifest.json` records exact source commit,
renderer SHA-256, Chromium version, viewport, layout/theme/appearance, route,
synthetic status, computed role values, geometry and contrast results. Temporary
screenshots used for pixel sampling hide text only while measuring the artwork
and scrim; final visible-text screenshots remain separate.

A passing run means zero script errors, horizontal overflow, clipped hero
actions or vertical phone navigation at the sampled sizes, and all 126 contrast
checks meeting their assigned 4.5:1/3:1 thresholds. It does **not** prove route
interactions, actual native focus, screen-reader/text-size behavior, every theme
combination, or playback preservation. The prototype uses Cinema icon/glyph
assets; body/render code is a labeled composition study, not shipping UI.

## Toolchains, gaps and next packet

| Tool/runtime | M0 observation | Meaning |
|---|---|---|
| Rust | Explicit rustup `rustc 1.97.1 (8bab26f4f 2026-07-14)`; Cargo 1.97.1 | Available pinned compiler. Default Homebrew compiler is unsuitable; select rustup first for hooks/builds. No product Rust change in M0. |
| Apple | Xcode 27.1 build 27A9275; iPhone simulator SDK 27.1, TV simulator SDK 27.0; XcodeGen available | Available compiler/SDKs; not an app build receipt. |
| Apple runtimes | iOS 26.5 build 23F77, tvOS 26.5 build 23L470; iPhone 17 family, iPad family and Apple TV simulator definitions enumerate | Runtime discovery passes outside restricted sandbox. No new app installation or native test run in M0. Supported 17.x needs separate native compatibility proof; only 26.5 runtimes are installed. |
| Android | JBR/JDK 21.0.11, Gradle 9.7.1 (offline version works), Kotlin 2.4.0 in Gradle; SDK platforms 35/36/37.0 | Available compiler runtime; app compileSdk is 37. No app compile receipt. |
| Android emulators | Emulator 37.1.11.0; Hypervisor.Framework acceleration check returns 0; API36 phone, fold and 1080p TV AVDs enumerate | Disposable targets are available. No active ADB device or new boot/instrumentation evidence. Sandbox-only emulator probe misleadingly reported missing NEON; unrestricted probe passes. |
| Browser | Bundled Playwright/Chromium; exact version in generated manifest | Reproducible HTML prototype evidence only. |
| Navigation tools | Ripwire attempted first; sandbox cache write refused, then unrestricted call reports binary absent | Targeted `rg`/outlines/source ranges used. No empty Ripwire result was interpreted as absence. |

Required review items: validate the proposed native host/branch migrations;
review Apple Settings → Sharing → Shared Libraries route-registration gap in the
inventory; qualify all platform-specific destination policies; and inspect the
rendered compositions, especially contrast/focus and compact geometry. Remaining
M2–M5 acceptance includes production-shell interaction journeys, real native
captures, full palette/layout orthogonality, large text/accessibility, source
removal, playback continuity, physical remote focus and supported OS behavior.
Physical-device acceptance remains distinct from simulator evidence.

**Recommended next task after coordinator clearance:** M1 web slice + M2 shell
as one reviewable `codex/meridian-web` candidate, including presentation-only
switch preservation regressions and all four web asset registrations. Keep native
M1 slices paired with their M3/M4 implementations. Before each native chrome
rewrite, prove the persistent-host/branch migration through the mandated nested
Settings/Recording/library and TV focus/Back journeys. M0 itself stops at the
coordinator's internal checkpoint; it does not authorize self-release into M2.
