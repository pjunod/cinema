# Apple TV navigation — release contract

**Status:** open — split from the [audit](APPLE-TV-NAVIGATION-AUDIT.md) for the docs size budget.

## 7. Navigation contract — make regressions fail before release

### 7.1 One answer for each remote action

| Context | Direction / Select | Back | Focus entry and return |
|---|---|---|---|
| Root tabs | Native tab movement; Down enters content | Platform root behavior | Return to last surviving target in selected tab |
| Content grid/shelf | Spatial neighbors; Select activates focused item | Pop one destination | Restore item by identity, not array index |
| Library tools | Left from the first content column enters tools; Right returns to bookmark | Pop destination, never a surprise scroll-to-top | At most six Left presses for a six-column grid |
| Jump index | Select realizes and focuses the chosen group's first item | Close index to opener | No scroll-only success |
| Choice sheet | Up/Down choices; Select commits exactly once | Cancel only this sheet | Current selection on entry; exact opener on exit |
| Confirmation | Focus begins on safe Cancel/Keep | Cancel only | Original row; next/previous surviving neighbor if removed |
| Text entry | Native keyboard owns input | Close keyboard first | Text field, then its results on Down |
| Hidden playback controls | First direction/Select reveals only; Play/Pause acts once | Existing player contract's exit/return-browser behavior | Visible transport receives focus after realization |
| Visible playback controls | Native spatial movement; timeline uses existing scrub contract | Close sheet, cancel scrub, hide chrome, then leave on later presses | No duplicate action from one input |
| Info/help sheet | Focusable text permits reading; actions remain visible | Close sheet | Restore opener; changing mode keeps that mode focused |
| Background update | Retain existing focused identity | Unchanged | If removed: next → previous → visible recovery action |

Do not bind an ordinary navigation press to tuning, deleting or changing a
setting. Preserve the existing
[player input contract](PLAYER-INPUT-CONTRACT.md) and server/session ownership.
The proposed library left-edge tools path supplements spatial browsing; it
does not turn Up in the middle of a grid into a jump to the top.

### 7.2 Shared UI ownership with small, testable boundaries

1. **One TV presentation host per visible screen.** It owns menu/sheet stack,
   opener identity, first-focus selection, Back and return fallback. Use a
   native sheet/fullscreen presentation as appropriate; ensure covered views
   cannot receive focus. Existing authorization, modal source-generation and
   presentation-fencing logic must survive this work.
   Keep the mounted presentation's focus state local to that presentation;
   returning a logical key to the covered browser cannot focus player controls.
2. **One identity per actual control.** Native focus, companion navigation,
   accessibility identifiers and test assertions refer to the same stable
   target. A logical requested key is not proof of actual focus.
3. **Restore after realization, cancel on newer intent.** Scroll lazy targets
   into existence before requesting focus; acknowledge actual focus. Use a
   presentation/route generation and explicit completion, not a fixed sleep
   or repeated focus-stealing timer. If the opener disappeared, use a defined
   visible fallback. A real subsequent remote press cancels an old restore.
4. **One playback chrome family.** Share presentation/input behavior across
   VOD, Live TV, library channels and shared playback; keep their separate
   media controllers. No new transport rewrite is required.
5. **One control style family.** Readable text/foreground pairs, distinguish
   selected/focused/disabled, reserve focus clearance, respect Reduce Motion.
   Default styles are allowed only after TV focus/contrast acceptance.

Apple's [focus guidance](https://developer.apple.com/design/human-interface-guidelines/focus-and-selection/)
explains why visible, predictable focus is central to television input.
Its [focusSection documentation](https://developer.apple.com/documentation/swiftui/view/focussection%28%29/)
and [default-focus API](https://developer.apple.com/documentation/swiftui/view/prefersdefaultfocus%28_%3Ain%3A%29)
provide building blocks. Our inference from the code and reproduction is that
those building blocks need shared ownership and actual UI verification;
adding a focus section to an arbitrary overlay is not sufficient.

### 7.3 Required tests measure the journey, not just whether controls exist

Build a deterministic synthetic API fixture and a tvOS simulator UI scheme.
It must use the shipping shell/routes/views. The small direct-entry harness
from §1 is a bug reproduction, not that eventual whole-app suite.

| Required journey | Assertion that prevents false success |
|---|---|
| Every root destination and Back | Target actually has native focus; expected route and tab remain selected |
| Movies 416 / 5,000, Rows and Grid | Tools reachable within stated movement bound at first/middle/final item |
| View, Sort, Filter and Group by | Immediate in-modal focus; no background focus or selection in any direction |
| Modal Select and Back | Correct value, single dismissal, exact opener restored; no underlying navigation |
| Jump A/M/Z and year/resolution groups | Target card visible **and focused**, repeated jump works |
| Grid → detail → playback → Back | Original item/group/scroll identity retained; no jump to first movie |
| Season episode → options → details | No playback started merely to inspect information |
| Search keyboard/results/error | Keyboard Back returns to field; unchanged-query Retry works; result return retained |
| Live list/guide/detail/fullscreen | Channel/programme anchor preserved across all layouts and close paths |
| Every playback kind, chrome hidden | No report/control overlay visible; first direction reveals without seek/tune |
| Report from player → close | Report owns focus; transport continues as intended; focus returns to More |
| Reminder arrival/expiry | No steal; no vanished-focus dead end; no double handling of Back |
| DVR final row → confirm/cancel/delete | Safe initial focus; no cross-row deletion; neighbor restoration |
| Settings category → picker → close | Exact setting restored, selected and focused states distinguishable |
| Shared choices/prepare/revocation | Same UI grammar; commands still use shared authorization and controller |
| Refresh removes item/control | Defined neighbor/recovery target, no nil focus loop |
| Long text / blank art / disabled / loading / failure | Every visible action reachable, focus unclipped, content readable |

Use `XCUIRemote` presses, `hasFocus` (or a verified exact native cell proxy),
route/value assertions, bounded press counts and screenshots. Never pass a test
because `.exists` or `.isHittable` is true. Never use a generic 80-press search
to prove a route that should take two presses. Tests should assert the intended
sequence and fail where it diverges. Separate setup navigation from the path
being measured. Do not force focus into the destination under test.

Keep the pure input/restore reducers and their tests, plus lightweight wiring
checks that enumerate presentations and actual focus targets. Those checks
supplement native journey tests; grep assertions cannot prove reachability.
Use negative controls: break focus entry, remove an opener, add a floating
button, and verify each guard catches its intended failure.

### 7.4 Make qualification mandatory where the code can break navigation

**Policy scope:** the owner's main-only restriction applies only to mobile app
builds intended for physical-device installation, and those installations.
iOS simulator/emulator and CI/test builds not intended for physical-device
installation, along with non-mobile work such as tvOS, follow the normal
repository workflow. There is no hold on branch-based native test builds.
Follow focused native checks before pushing, the effort gate before task
merges, and qualification before promotion. Never repurpose an unmerged-source
mobile test build for physical-device installation.

**Development evidence:** each behavioral fix needs its focused regression
and required `Regression-Test:` field in the PR and landing message. Pure-logic
checks support this evidence but do not substitute for native focus tests.

**Required native coverage:** shared menu, focus modifier, theme, root shell
or playback-chrome changes require the full critical tvOS journey set.
Per-screen changes need the changed journey plus common menu/Back smoke.
Current CI does not provide this guarantee; add these native journeys to the
appropriate pre-merge gates under the normal tvOS workflow.

**Promotion and release gate:** bind the full native simulator run and receipt
to the exact candidate source SHA and app build. A failing or missing required
journey blocks promotion or release. If the candidate or its base moves,
qualify the new tree again. Global components cannot be exempted by a narrow
changed-file classifier. Since branch protection is unavailable, automation
must validate the source-bound receipt and the merge/release operator must
enforce it. A passing gate does not itself authorize deployment. Any mobile
build intended for physical-device installation must meet the source rule above.

**Before a TV release:** short physical Siri Remote acceptance on the actual
candidate: slow presses, swipes, held directions, rapid Back, keyboard, 1080p/
4K output, suspend/resume, VoiceOver, Reduce Motion and couch-distance contrast.
Record app build, source SHA, OS, fixture/server state, exact route, observed
focus and screenshots/video. Simulator evidence does not replace this step.

**Nightly:** full synthetic matrix and long-list/update stress. Preserve failed
runs and distinguish flakes from passes. A rerun cannot erase a new navigation
failure; fix or explicitly diagnose it before release. This document does not
create a scheduled automation.

### 7.5 Ownership prevents another gradual deterioration

A named maintainer owns the TV navigation contract and shared components.
Every PR adding/moving/removing a focusable element answers: entry target,
directional neighbors, Select action, Back owner, dismissal target, async
removal fallback, empty/error state, and its native UI regression. A required
review checklist and test manifest make this reviewable; “looks fine” is not
evidence. Security/admin routes remain physically operated where required.

Do not add a Developer toggle for these correctness repairs. Do not hide
working controls because qualification is missing. Missing qualification
blocks a release claim, not the viewer's saved choice.

