# Player input physical verification — device results from 2026-09-02

Companion to
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (the behavior contract)
and
[PLAYER-INPUT-CONTRACT-PLAN.md](PLAYER-INPUT-CONTRACT-PLAN.md) (the physical
verification script) — this records what the connected devices did.

**Result:** 21 PASS · 13 FAIL across 34 required device/item rows.

## Scope — the tested tree and devices

The run used source commit
`a9e45c078e694d562fd5e60b9b69da9dc8f7de94`. Android ran versionCode 67.
That Apple source produced build 112; the build 111 named by the script was
not available from the tested tree. The signed release tvOS build was restored
after the temporary DEBUG-only probes.

The connected surfaces were Apple TV · Google TV · Pixel Fold · iPhone ·
Chrome on macOS · iPad Safari. Remote and keyboard input was injected through
the platforms' device automation paths. Item 16 therefore verifies external
keyboard routing, but not Bluetooth transport pairing itself.

FAIL means either the device contradicted the fixture or the required live
fixture/device automation was unavailable. The note distinguishes those cases;
an unavailable fixture is not evidence that the behavior works.

## Results — every required row is explicit

| Item | Device | Result | What happened |
|---:|---|:---:|---|
| 1 | Apple TV / Siri Remote | PASS | With chrome hidden, Left, Right, Up, Down, and Select revealed chrome without changing position. |
| 2 | Apple TV / Siri Remote | FAIL | Fixture: timeline Left/Right preview; Select commits; Back/Up/Down cancel and leave. Preview, Select, Back, and Up worked, but Down moved focus to skip-forward while leaving the pending scrub active. |
| 3 | Apple TV / Siri Remote | PASS | Repeats stepped 10 s, 30 s at repeat 5, and 60 s at repeat 10. After release, the next step returned to 10 s. |
| 4 | Apple TV / Siri Remote | PASS | Back precedence matched: pending cancel, info close with focus restored, chrome hide, then exit when hidden. |
| 5 | Apple TV / Siri Remote | PASS | Playing chrome hid after 4 s. Paused, menu, Standard panel, and pending-scrub chrome remained visible. Reveal restored focus. |
| 6 | Apple TV / Siri Remote | PASS | The Mini strip and chrome both disappeared after about 4 s. |
| 7 | Apple TV / Siri Remote | PASS | A real 404 stream produced the failure UI. Every direction stayed within retry controls, and Menu exited. |
| 8 | Apple TV / Siri Remote | PASS | The Standard header showed `No server-side session`, not `No stalls`. |
| 9 | Apple TV / Siri Remote | PASS | A DEBUG-injected intro interval let Up focus the marker. After expiry, Up focused the real progress control and the app did not crash. The live catalog had no intro interval. |
| 1 | Google TV / D-pad | PASS | All five hidden-chrome keys revealed controls without seeking. |
| 2 | Google TV / D-pad | PASS | Timeline preview, commit, cancel, and row-leave behavior matched. |
| 3 | Google TV / D-pad | FAIL | Fixture: 10 s → 30 s at repeat 5 → 60 s at repeat 10, then reset. The device automation path emitted isolated key events and never entered repeat acceleration. |
| 4 | Google TV / D-pad | PASS | Back precedence matched the four required states. |
| 5 | Google TV / D-pad | FAIL | Fixture: never auto-hide while paused or with a pending scrub. Paused chrome collapsed, and pending-scrub chrome hid after about 6 s. |
| 8 | Google TV / D-pad | PASS | The Standard header showed `No server-side session`, not `No stalls`. |
| 9 | Google TV / D-pad | FAIL | The live catalog had no skip-intro fixture, so the marker-expiry route could not be exercised. |
| 10 | Google TV / D-pad | FAIL | The live catalog had no unknown-duration or live-recording fixture, so this route could not be exercised. |
| 11 | Google TV / D-pad | FAIL | Fixture: FF and REW are `ignore` while a scrub is pending. REW was ignored, but FF changed playback state. |
| 12 | Google TV / D-pad | FAIL | Fixture: the first D-pad press moves focus on Library, Search, and Settings. Library and Settings consumed the first press to establish the anchor. |
| 13 | Google TV / D-pad | FAIL | Fixture: the keyboard cannot take D-pad input before Select begins editing. The keyboard took D-pad input before Select. Clear did answer one Select. |
| 14 | Pixel Fold / touch | PASS | Tapping the picture hid and then revealed chrome without toggling play/pause. |
| 15 | Pixel Fold / lock screen | FAIL | Fixture: ±10 s and play/pause controls work. The media session remained `PLAYING`, but Android exposed no Plurx media card or notification controls. |
| 16 | Pixel Fold / external keyboard | PASS | Injected external-keyboard arrows moved focus from Back to Playback position. No Bluetooth keyboard was paired. |
| 17 | iPhone / touch | PASS | Tapping the picture toggled chrome without changing playback. Drag release committed a changed playback position. |
| 18 | iPhone / touch | PASS | Tapping outside Standard and Debug closed each panel without activating the transport control underneath. |
| 19 | iPhone / lock screen | FAIL | Fixture: ±10 s and play/pause are available, and skips are ignored during an on-screen pending scrub. The lock screen exposed no media buttons, so neither the basic controls nor the pending-scrub branch could pass. |
| 20 | Chrome / keyboard | PASS | Seek-bar Left/Right previewed, Enter committed, Escape canceled without closing the player, and Tab-away canceled. |
| 21 | Chrome / mouse and keyboard | FAIL | Fixture: Escape during a click-drag cancels and leaves the player open. Escape left the player open but committed the drag, moving playback from 47:13 to 19:36. |
| 22 | Chrome / keyboard and network | PASS | J followed by Right and Enter produced exactly one HLS session reopen: the committed seek. |
| 23 | Chrome / keyboard | PASS | Up and Down did not seek. |
| 24 | Chrome / keyboard | PASS | Menu arrows moved within the menu; Escape closed it and restored gear focus. Escape closed the info panel without disturbing the menu behind it. |
| 25 | Chrome / keyboard | PASS | Classic-layout posters and episode rows were tab stops and Enter opened them. Header-search focus stayed in the field while results arrived. |
| 26 | Chrome / keyboard | FAIL | The live catalog had no photo fixture, so lightbox Escape and focus restoration could not be exercised. |
| 27 | iPad Safari / touch | FAIL | The physical XCTest runner timed out while enabling automation mode, so picture-tap behavior could not be exercised. |

## How to read this — product failures and verification gaps

The direct behavior regressions are Apple TV item 2 · Google TV items 5,
11, 12, and 13 · Android item 15 · iOS item 19 · Chrome item 21. These are
device observations that contradict the fixture.

Google TV items 3, 9, and 10 · Chrome item 26 · iPad item 27 are verification
gaps. They remain FAIL in this qualification because missing repeat semantics,
content, or automation cannot be converted into a passing claim.

## Evidence — retained outside the repository

Screenshots, UI hierarchies, device logs, and XCTest result bundles were
captured under `/private/tmp/plurx-player-input-evidence/` on the validation
host. They are intentionally not committed because result bundles contain
machine-specific metadata and the fixture was requested as a documentation
commit, not an artifact import.

Key retained artifacts include the Android paused and pending-scrub captures,
the Pixel Fold lock-screen capture, the Chrome drag-cancel failure capture,
and the iPhone/iPad `.xcresult` bundles.
