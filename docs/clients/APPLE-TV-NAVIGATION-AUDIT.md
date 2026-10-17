# Apple TV navigation — defects, proposed screens and a lasting release contract

**Status (2026-10-17):** repairs built and integrated with current main
(`89d790d2e`); the final guide/playback task (#1138) landed in the effort
after a green effort gate. Local proof on `cd01fb9`: 1,101 tvOS units and
28/28 native journeys with zero failures/skips. Main promotion runs on the
exact integrated candidate. Final saved-account device repeats and the
explicitly remaining physical/dynamic-state acceptance are open (§8).
**Written:** 2026-10-15 · **Baseline:** `9eb7f6738647a7e963b17cfb3a2f77a594863c41`

This is the Apple TV follow-up to the [mobile usability audit](MOBILE-UI-USABILITY-AUDIT.md)
and [native layout work](NATIVE-LAYOUT-FOLLOWUP.md). It covers navigation,
focus, control placement, legibility and recovery across the television app.
The [rendering gallery](apple-tv-navigation/index.html) shows the proposed
screens. This document is the canonical project ledger until implementation
has its own ledger. It is not listed in the [backlog](../features/PROJECT-BACKLOG.md): that file is on the docs size budget, and this ledger carries the state.

The immediate diagnosis is concrete: the app can draw a menu in front of the
library without moving physical focus into it. Subsequent remote presses
continue navigating the movies behind it. Several unrelated screens reuse
that same menu. Separately, diagnostics added permanent buttons above live
video, bypassing the controls' visibility and focus ownership.

The findings (§3–§5) live in [APPLE-TV-NAVIGATION-FINDINGS.md](APPLE-TV-NAVIGATION-FINDINGS.md) and the navigation contract (§7) in [APPLE-TV-NAVIGATION-CONTRACT.md](APPLE-TV-NAVIGATION-CONTRACT.md); section numbers are kept so existing references still read correctly.

## 1. Evidence — what was inspected and what was actually reproduced

**User observations:** the movie View/Sort window is visible but reaching it
requires traversing a library of more than 400 movies; the Report a problem
button remains on screen and is badly placed. The user's report establishes
those experienced failures. It does not establish every other screen's failure.

**Native reproduction:** Xcode 27.0 (`27A266a`), tvOS 26.5 simulator, Apple TV
4K third generation. An isolated source copy uses the shipping `LibraryView`,
menu, coordinator and styles, with a synthetic 416-movie loader and a direct
library entry instead of production authentication. The grid had its first
200 titles loaded at capture. The UI test presses the native remote buttons:

1. Focus View and press Select. The View menu appears.
2. Native focus remains on the View toolbar button behind the menu.
3. Press Down three times. Native focus moves to **A Movie 10 behind the menu**.
4. The assertion that Rows, Grid or Close owns focus fails.

The screenshot also shows three red controls whose text is not visually
legible, although Rows, Grid and Close exist in the accessibility tree.
The first harness build failed on a complex synthetic Swift expression; an
explicit loop corrected that harness error. The second build succeeded and
ran the test to the expected focus assertion failure.

![Actual synthetic native capture: movie focus behind the open View panel](apple-tv-navigation/evidence/modal-focus-behind.png)

[Receipt](apple-tv-navigation/evidence/receipt.json) ·
[focus trace](apple-tv-navigation/evidence/focus-trace.txt) ·
[opening capture](apple-tv-navigation/evidence/modal-opening.png) ·
[reproduction patch](apple-tv-navigation/evidence/synthetic-harness.patch).
The reproduction patch is test scaffolding for an isolated copy, not a product
fix. No production server, user library or physical Apple TV was modified.
The isolated simulator was deleted after retaining the evidence.

**Subsequent owner clarification:** after that test completed, the coordination
chat relayed a build/install restriction whose scope the owner subsequently
clarified: only mobile app builds intended for installation on physical devices,
and those installations, must use committed source merged into `main`.
Simulator/emulator, CI and other builds not intended for physical-device
installation remain under the normal branch review/testing workflow, as does
non-mobile work. Do not reuse an unmerged-source mobile test build for
physical-device installation. No further app build/install was performed. The patch is retained
as historical evidence.

**Limits:** this was not a complete live walkthrough, a production login test,
a test of the full bottom-of-library workaround, or physical Siri Remote
swipe/hold acceptance. The rest is a review of current source and tests.
Absence of an explicit focus modifier alone is not proof that a native control
is broken. Runtime-dependent risks below remain labeled as such. No audit can
honestly promise to have found every possible navigation failure without the
full scenario matrix and physical acceptance.

**Evidence labels:** **N** = reproduced in the native synthetic harness;
**S** = behavior or missing route directly established by source;
**R** = plausible focus/layout failure needing runtime verification.
**P1** blocks or seriously disrupts a primary journey; **P2** harms usability
or leaves a reproducible regression gap. R priorities are proposed severity,
not a claim of a reproduced defect.

## 2. Screen inventory — every family has an explicit disposition

| Surface and states inspected | Current evidence / findings | Remaining acceptance |
|---|---|---|
| Connect, discovery, login, reconnect, loading | Source: auth scaffold, busy/disabled controls, root diagnostics; F11, F22, F26 | Discovery list growth, keyboard open/close, timeout, invalid credentials and recovery with a remote |
| Root tabs, hidden server features, stacked destinations | Native tab reentry steals focus into prior content; source: `HomeView`, path binding and scope synchronization; F05, F29, F30 | Every tab in both directions, feature disabled while selected, repeated Back, suspend/resume |
| Home Classic and Theater, Continue Watching, Next Up, Recently Added, Coming Soon | Source: shelves and Theater hero; F09, F27 | First/last card to adjacent rows, data refresh while focused, hero playback return |
| Libraries dashboard, category/library grouping, shared entry | Source: shared choice panel and shelf headings; F01–F03, F09 | Grouping open/select/cancel; See All from far end of every shelf |
| Movie/show/home-video collection, Rows/Grid, filters, sorting, title narrowing | Native View-menu reproduction; all other states source; F01–F05 | Rows and Grid at 0/1/416/5,000 items; every sort/filter, all poster sizes |
| Alphabet/year/resolution/date groups and View all | Source: scroll-only index and group view; F04–F05 | Jump then Down, return from group/detail, partial paging and disappearing group |
| Movie/video/audiobook details, versions and tracks | Source: hero, actions, body and choice panels; F01–F03, F07–F08, F28 | Long titles/track names, unavailable file, multipart audiobook, resume/start-over |
| Show, season and episode cards | Source: explicit TV Play-only episode card; F06–F08 | Find an episode's details and track/version choices without starting it |
| Search, empty/loading/results/failure | Source: result replacement and missing Retry; F10 | Keyboard cancellation, slow search, returning from detail, result removal |
| Local VOD player, hidden/transport/timeline/scrub/menu/info/failed | Source: input reducer and adapter already exist; F01–F03, F11, F27–F28 | Every state/input pair in the real UI, caption menu, report return, autoplay |
| Live TV On now, guide, all three layouts, filters and programme sheets | Source: substantial existing focus restoration; F01–F03, F15–F17 | Guide edges/time paging, zero channels, protected channel, refresh during focus |
| Live fullscreen, temporary guide, More, Info and diagnostics | Source: existing hide/reveal contract plus external report overlay; F11, F17 | First press only reveals, Back hierarchy, sheet close, reminder expiry |
| Library channels: browse, layouts, programme cards, fullscreen, watch-from-start | Source: separate native VideoPlayer presentation; F11–F14 | No-op future programme, controls disappearing, return to same channel, failures |
| Recordings, upcoming, saved, attention, skipped, rules, reminders, manual | Source: all seven views and row actions; F18–F21 | Long lists, destructive confirmation, rule reordering, empty/error, manual channel selection |
| Recording activity, detail and history | Source: dynamic rows and history; F18, F21, F28 | Recording ends or disappears while focused; read full history with remote |
| Settings, Live TV administration, Developer, Remotes & devices | Source: native menu pickers already used; F22, F26–F27 | Every setting and submenu, long readiness text, authorization failure, reset confirmation |
| Report creation, privacy choices, reproduction, local history and TV ZIP | Source plus existing diagnostic UI tests; F11, F23 | Open from each owner, cancel/close/reopen, QR scan from couch, return to opener |
| Shared sources, libraries, search/page loading, title detail and player | Source: separate browsing/player controls; F24–F25 | Offline source, load more, physical Play/Pause/Back, menu return, interrupted prepare |
| Sharing management and endpoint/assignment/export editors | Source: forms, sheets, confirmation paths and authorization handling | Read-only route review only; real admin changes, keyboard and nested-sheet focus unverified |
| Download/book reader and invitation acceptance screens | iOS-only branches; not TV destinations | No television implementation requested or inferred |

Primary sources are under [Apple Sources](../../clients/apple/Sources).
All numbered findings below link to the relevant file or function. The table
records source review breadth, not a claim of runtime coverage of every row.

## 6. Proposed renderings — consistent interaction before decoration

The [gallery](apple-tv-navigation/index.html) includes twelve annotated design
plates. They are proposals with synthetic titles, not screenshots of repaired
SwiftUI. The native failure captures are labeled separately. The same choices
apply to each screen family, rather than adding a bespoke menu to each page.

| Plate | Proposed experience | Findings addressed |
|---|---|---|
| R01 · Library | Stable page tools, bounded left-edge access, grid position retained | F04–F05, F27 |
| R02 · Choice sheet | Selected choice receives focus; covered content inert; Close/Back restores opener | F01–F03, F14, F25 |
| R03 · Details and episode options | Short action labels, About, explicit episode details/track route | F06–F08, F28 |
| R04 · Home | Familiar tabs/shelves, readable focus, Retry above retained content | F09, F27 |
| R05 · Search | Fixed query and Retry; retained results; predictable result return | F10 |
| R06 · Playback | Quiet picture, one revealable control owner; reporting inside More | F11–F12, F17, F24 |
| R07 · Live and channels | Compact toolbar, guide-time controls by guide, real programme actions | F13–F16 |
| R08 · Recordings | Title-first rows, readable detail/actions, owned confirmation, channel picker | F18–F21, F28 |
| R09 · Settings and Help | Categories; TV-first phone handoff; advanced diagnostics separate | F22–F23, F26–F28 |
| R10 · Connection recovery | Retry/change server/Help together, keyboard returns to opener | F09, F11, F22 |
| R11 · Shared title | Same detail/playback grammar with explicit Source and its own controller | F24–F25 |
| R12 · Navigation acceptance | Visible coverage/release decision example, not a product screen | F29 |

### 6.1 Implementation comparison, October 15

Review the actual native captures alongside the plates. The synthetic fixtures
have blank artwork and deliberately long text; the plates use illustrative
artwork. Neither artwork differences nor passing assertions excuse clipped
labels, invisible focus or an unreachable action. The comparison below records
observed behavior and remaining limits, not pixel-for-pixel equivalence.

| Plate | Implemented behavior / retained native evidence | Disposition |
|---|---|---|
| R01 | Tools remain in a fixed left sidebar. Grid/Rows at 416 and 5,000 titles reach tools within six Left presses and View within three Up presses; Right restores the exact title. | Deliberate layout adjustment: View/Sort/Filter/Jump are together in the sidebar instead of split between top and left controls. Native captures inspected. Rows reserve focus clearance, and Up from the first poster reaches the readable View all action; group Back restores that action. |
| R02 | Selected value owns native focus; directional movement stays in the panel; Back returns to the exact opener. | Native View, Sort, Filter, grouping, channel layout and Shared Quality journeys pass individually. |
| R03 | Local movie About reaches the final reading region and returns to About, then the same movie. Episode details open without starting playback. | Native individual journeys pass; long text uses bounded focusable reading regions. |
| R04 | Failed foreground refresh keeps the shelf, shows Retry and returns focus to the retained title after success. | Native lifecycle journey and actual capture inspected. |
| R05 | Search retains its query through failure/Retry and restores the selected result after detail. | Native journey and final query, Retry and result-return captures inspected; explicit TV text contrast remains readable on the dark background. |
| R06 | Movie, Live, channel and Shared players hide chrome, reveal without unintended activation, put reporting inside More and restore the opener. | Native individual journeys pass. Shared keeps a direct Quality shortcut; its choices use the same owned presentation. This is a functional variation from the illustrative More layout. |
| R07 | Compact Live toolbar; named channel actions; channel layouts use explicit choices; future programmes open information. | Native Live/channel journeys pass individually; Live browse and fullscreen captures inspected. Full-lineup guide paging, toolbar return and fullscreen-guide exit now pass; reminder expiry remains a separate outstanding matrix row. |
| R08 | Title-first recording row spans the content width, opens detail and returns exactly; rule confirmation starts on Keep; manual scheduling uses named channels. | Native journey and actual row/confirmation captures inspected. Destructive commit and disappearing-row stress remain separate from safe cancellation. |
| R09 | Help is a visible category; reporting offers an explicit phone export and Advanced controls. Instructions wrap fully. | Native Settings/report journey and corrected instruction capture inspected. Physical couch-distance QR scanning remains open. |
| R10 | Retry, different server and Help are in the connection form. Help uses secondary styling. Recovery is visible during discovery, and late discovery cannot override a newer retry/server choice. | Native five-second recovery, direct Down through Retry → server choice → Help, report return and saved-sign-in reuse pass individually. Actual recovery captures inspected. |
| R11 | Shared Source remains visible; full-screen playback uses the common remote grammar; About exposes every synopsis region and restores its opener. | Native Shared journey and final-paragraph capture inspected. The existing file-specific detail layout is retained; the plate's hero composition is illustrative. Shared authorization and controller ownership remain separate. |
| R12 | Required native suite rejects missing/skipped tests, records source/app hashes, preserves failures, and blocks the affected Apple promotion paths. | Gate wiring and runner tests pass. Capture exports sanitize artifact-forbidden filename characters and preserve distinct names; the earlier promotion upload rejected a colon-bearing capture. No physical qualification or release is claimed by a simulator receipt. |

## 8. Implementation sequence and open ledger

**Execution scope:** tvOS and iOS simulator/CI checks follow the normal
repository workflow; the physical-device mobile restriction does not block
them. Implementation authorized October 15; T0–T4 are in progress. Native journey
and physical acceptance remain required before completion.

Use one `effort/apple-tv-navigation` integration branch because the work
packages share files. Task branches start at its current tip; follow the existing
effort gate and focused regression requirements. Final promotion follows
[the development pipeline](../DEVELOPMENT_PIPELINE.md). The implementation base
is `a0b8cb72c`; the earlier baseline above remains the historical audit source.
The coordinating agent owns integration, adversarial review and final acceptance;
three GPT-6.1 Sol work sessions implemented bounded packages. They reached
their usage limits; the coordinating agent continues integration and native
validation on `codex/tv-navigation-integration`. Existing task PRs are open;
none of the packages is a qualified release merely because it compiles.

| Package | Scope and principal files | Dependency | Acceptance / state |
|---|---|---|---|
| T0 · Harness and contract | `project.yml`, TV UI fixtures/tests, affected-Apple workflows | None | **Built, expanding:** [PR #1112](http://forge.lan:3000/noirr/plurx/pulls/1112). Shipping app through fixture server and normal login. Strict runner retains failures, rejects empty/skipped execution and binds source and app bundle. Required native gate wiring built; a static guard now requires every fixture journey in the runner and navigation scheme and excludes those fixtures from the physical scheme. Simulator builds use ad hoc signing so Keychain-backed setup is real. Full matrix remains open. |
| T1 · Modal ownership | `RemoteNavigationViews`, coordinator, adapters; every caller | T0 | **Partly proven:** [PR #1109](http://forge.lan:3000/noirr/plurx/pulls/1109). Native View journeys pass at 416 and 5,000 items, including selected Grid entry, directional confinement, Rows commit/reopen and exact opener restoration. Cross-target Jump return and every enabled root tab now pass native journeys. Modal close retains the parent order without retaining authorization, and reissues focus after the covered view returns. The coordinator has 28 passing unit regressions. |
| T2 · Playback presentation | `LiveTvView`, `LibraryChannels`, `PlayerView`, diagnostics | T1 | **Partly proven:** [PR #1110](http://forge.lan:3000/noirr/plurx/pulls/1110). Native future-programme details/Close and current-programme single-session behavior pass; explicit layout change/reopen/Back passes. Library-channel hidden Play/Pause, first-direction reveal, More → Report → Back and session retention now pass. Reports moved inside More. Ordinary movie and Live TV native journeys also pass: hidden picture → reveal → More → Report → Back retains the playback session and returns to the original item/channel. Shared playback now passes initial Pause focus, hidden-picture reveal, selected Quality entry, exact menu/report return and a single retained session. The Shared title also exposes the same focusable About reading panel as local titles, with a native final-paragraph and exact-return assertion. |
| T3 · Browsing and details | `LibraryView`, `HomeView`, `SearchView`, `DetailView`, `Components` | T1 | **Partly proven:** bounded tools sidebar, identity restoration, explicit Jump loading, episode details and complete synopsis, fixed Search query and recovery. Native full synopsis/Back and unchanged-query Search Retry/result return pass. Full Grid/Rows journeys now pass at 416 and 5,000 titles, including Jump A/M/Z/M, bounded tools access and exact content return. Empty and single-title libraries pass. Eleven focused browsing unit regressions and iOS simulator compilation pass; the synopsis has a visible focus cue. A foreground Home refresh failure now retains cached titles, exposes a reachable Retry and restores the retained title when Retry disappears; the native lifecycle journey passes. |
| T4 · Secondary screens and visual consistency | DVR, Settings, diagnostics, remotes, theme, info | T1; serialize shared-file work with T2/T3 | **Partly proven:** native Settings quality selection/Back, Help → phone QR report → Advanced → Done/Back, recording details, safe rule cancellation and named manual channel selection pass. DVR title rows span the content region so Down from a middle category cannot hit a gap. Remote pairing revocation and registration reset now pass safe Cancel entry and exact opener return in an ad hoc signed simulator build. The fixture uses the normal saved-server identity and Keychain registration. DVR title scaling and report instruction wrapping are corrected and visually checked after passing native reruns. Saved-connection recovery now exposes Retry and contextual Help while network discovery continues, rather than hiding all actions behind a spinner. A repeated native run first caught the delayed recovery; the corrected run requires Retry within five seconds and preserves the saved sign-in. Final integrated and physical acceptance remain open. |
| T5 · Whole-app qualification | Journeys, receipt, physical devices, release validation | T2–T4 | **Integrated; promotion in progress, final device repeat open:** F29; all 28 source-bound native journeys and 1,101 tvOS units pass on `cd01fb9` with no failures/skips. Current main merged into the effort on October 17; #1138 landed as `150713637` after effort gate 5329. Earlier saved-account physical checks pass on the 491-title library (build 239) and all 77 guide channels (build 252). The October 17 repeat on build 258 did not reach the product (see the October 17 entry below), so final-candidate device acceptance is not claimed. Gestures, accessibility, couch-distance QR and the listed dynamic-state stress cases remain unaccepted. |

**Integrated development evidence, October 15:** the full 1,037-test tvOS unit
suite and iOS simulator compilation pass again after Shared About and connection
recovery changes. The callback race tests now retain every queued publication rather
than mistaking processing metadata for subtitle readiness; Live TV wiring checks
follow the native reveal adapter and presented focus region. The documentation,
infrastructure-name, modal wiring and native-runner static checks pass (27
tests). Native evidence is retained in external source-bound receipts; these
focused runs are development evidence, not the final whole-app qualification.
The runner now requires 26 native journeys, including Shared playback, Home
foreground recovery, saved-connection recovery and Rows View all/group return. An earlier combined run was
intentionally interrupted after passing six journeys when independent visual
validation exposed additional recovery work; that partial result is retained
and is not a passing suite. A later combined run exposed a test setup defect in Live TV: geometric
snapshot discovery consumed the four-second idle interval without sending a
remote event, so the controls correctly hid before the helper read their
frames. The retained failing trace shows that interval. The replacement
journey sends Right through each of the four named fullscreen controls,
asserts actual focus after each press and then verifies report/Back/session
retention; its focused rerun passes. It uses unique fullscreen identifiers to
exclude similarly named covered controls. The full suite must pass on the
updated candidate; these diagnoses do not turn failed runs into qualification.
The full 25-journey run on `d2c79ac4a` passed with zero failures and zero
skips. Final visual review additionally repaired a blank View all label,
clipped Rows/Live focus outlines, low-contrast TV headings/query text and
Up from a row skipping its group action. The new View all/group-return
journey first reproduced the skip, then passed with a native viewport-clearance
assertion. Rows at 416/5,000 titles, Live and Search focused reruns and iOS
simulator compilation pass with these final corrections. The complete 26-journey run on `d754da0d3` passed: 26 passed, zero failed,
zero skipped, with a clean source/app-bound receipt. All 1,037 tvOS unit tests,
iOS simulator compilation and 27 static contracts passed on that candidate.
Effort development gate 5212 passed. [PR #1127](http://forge.lan:3000/noirr/plurx/pulls/1127)
landed as `3a202fa1a`; its Git tree exactly equals the tested candidate and its
message carries all 15 regression references.

**Physical TV evidence:** a signed build from final implementation candidate `d754da0d3` passed the saved-account
library journey on AppleTV14,1, tvOS 27.0 (24J361), app build 238. The real Movies
collection contained 491 titles. View, Sort and Filter focused the selected
choice immediately; Back restored each opener and the collection. Captured
4K focus outlines and labels were inspected. This was XCUIRemote button
automation; gestures/holds, VoiceOver, Reduce Motion and couch-distance QR
scanning remain unaccepted. UIKit hierarchy and SwiftUI view-update warnings
were retained in the receipt. The final candidate repeat passed in 49.215 seconds with no failures or skips;
do not confuse that scope with whole-device qualification.

**October 17 integration and device attempt:** effort gate 5322 on `cd01fb9`
failed only in `effort Rust compile`. The job ran on a `general, high-cpu`
runner with one Cargo job, produced no output for 59 minutes after
`plurx-support`, and hit its 60-minute deadline. Main had already moved that
job to the larger-memory topology queue (`d1cd7235f`). The re-dispatch was
refused by `validation/qualification.py` because current main was not an
ancestor of the effort tip. Current main was therefore merged into the effort
first (`06fd8e7a7`), then into the task (`6fc7e5c9d`). Conflicts were test
fixtures and documentation only, and main's refinements were kept. The task's
duplicate 60-minute compiler pin was dropped in favour of main's queue. Local
checks on the merged tree: tvOS build-for-testing, 199 focused tvOS tests,
iOS compilation, and 141 operations/validation tests. Gate 5329 then passed
every job, and #1138 landed with its 17 regression references. #1131 was
closed as included.

Build 258 (Debug, team-signed) from that tree was installed on the paired
AppleTV14,1. Both physical runs failed before reaching the product. The
older `TVPhysicalInputTests` playback journeys launched without the
`PLURX_TVOS_ORIGIN` that `PlaybackAcceptanceLaunch` requires, so the app
showed server choice rather than a player. The journeys then pressed Select
regardless. Choosing a server deliberately writes the origin without a
bearer, which signed the device out of its real account, and every later
journey stopped at sign-in. The owner's fresh sign-in was observed in use
before the same sequence repeated it. This is the F29 stale-physical-test
risk. `e80b9574a` makes those journeys require their origin, wait for the
player before any input, and stop at the first failed expectation. No
product code changed. The final saved-account guide/library/playback repeat
is still to be run on a signed-in device and is not claimed here.

The development gate's stale source/commit anchors from task consolidation
were corrected to current code and actual corrective commits. The local
history audit accepted the corrected mapping without an errata waiver;
the fresh effort gate passed on the final candidate. Main promotion still
requires the separate full qualification receipt.

**Non-goals:** no server transport rewrite, removal of diagnostic capture,
feature enablement policy change, new hidden remote gestures, or redesign of
iPhone/iPad. Shared Swift files still need iOS compilation and targeted
regressions to avoid moving the defect to another client.

**Completion:** all confirmed findings repaired or explicitly dispositioned,
all R rows tested and resolved, all inventory routes have source-bound native
journey evidence, physical acceptance is recorded, and mandatory release protection
is running. A report, mockup or successful compile alone closes none of those
implementation/acceptance cells.

## 9. Retained reproduction and report validation

The [historical harness patch](apple-tv-navigation/evidence/synthetic-harness.patch)
records the direct library entry, synthetic loader, UI test and XcodeGen project.
It contains no credentials. Apply/read it only in an isolated source copy.
The underlying UI implementation is unchanged from the baseline; the harness
itself modifies the app entry and data. Following the owner's clarification,
this tvOS simulator harness falls under the normal repository workflow,
outside the restriction on mobile builds for physical-device installation.
This policy correction did not launch a build or installation.

The completed historical test used scheme `NavigationAudit`, tvOS 26.5,
`CODE_SIGNING_ALLOWED=NO`, and `ModalAuditTests.testLibraryModalFocus`.
The successful app build was followed by the expected failing focus assertion.
The initial harness compiler failure was corrected before that run; it is
not counted as reproduction. Retained screenshots and native focus lines
are linked in §1. The full temporary build/result bundle is not a repository
artifact; the patch and small receipt preserve the useful evidence.

**How to read it:** View was physically focused and opened, then the final
assertion failed because movies retained focus under the menu. The gallery's
rendering IDs map to findings, not passing test results.

The gallery is regenerated with its [standard-library renderer](apple-tv-navigation/render.py):

```bash
python3 docs/clients/apple-tv-navigation/render.py
python3 -m unittest tests.operations.test_docs_index tests.operations.test_infra_names
```

These are document/rendering operations, not app builds. The 18 documentation
and infrastructure-name checks passed on October 15. All twelve gallery selections and images loaded in the browser without
JavaScript errors; the 375-pixel layout had no horizontal page overflow.
All twelve plates were visually inspected, including correction of background
focus outlines under proposed modal panels. The index checks also passed with
the new, untracked documentation explicitly included, and the infrastructure
scan found no violations in the combined tracked/new document set.
No application behavior was edited by this audit. Retain the receipt and
captures with the proposal so the eventual repair has a concrete before state.
