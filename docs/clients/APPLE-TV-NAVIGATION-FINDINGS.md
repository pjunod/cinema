# Apple TV navigation — findings F01–F36

**Status:** open — split from the [audit](APPLE-TV-NAVIGATION-AUDIT.md) for the docs size budget; the audit's §8 ledger is canonical.

## 3. Findings — browse, menus and details

### F01 · P1 · N/S — a visible choice panel does not own native focus

**Evidence:** [openModal](../../clients/apple/Sources/RemoteNavigationCoordinator.swift#L308)
sets the logical modal and requests nil focus. [RemoteControlModifier](../../clients/apple/Sources/RemoteNavigationViews.swift#L48)
only sets its binding true for a matching request; it does not make covered
controls unfocusable. [RemoteChoicePanel](../../clients/apple/Sources/RemoteNavigationViews.swift#L155)
registers order on appear but selects no initial target. The
[library overlay](../../clients/apple/Sources/LibraryView.swift#L102) leaves
its base active. The native reproduction above demonstrates the result.

**Reach:** View/Sort/Filter; dashboard grouping; pre-play Audio/Subtitles/Version;
local player tracks/quality; Live TV Guide time/Captions; shared player choices.
Reusing the component proves shared exposure, not identical symptoms on each.

**Fix:** one native modal presenter for this family. Present above the current
owner, make the covered surface ineligible for native focus and input, move
focus to the selected choice (otherwise the first enabled choice), then restore
the opener after dismissal. Keep companion commands routed through that same
presentation. `focusScope` or `focusSection` alone is not a modal barrier.
**Proof:** open each menu from its real opener; the first native focus after
presentation is inside, every direction stays inside, and Select cannot open
a background movie. Rendering R02.

### F02 · P1 · N — menu controls can be visually blank

**Evidence:** the native capture shows red fills without readable Rows/Grid/Close
labels. The panel uses `.buttonStyle(.bordered)` and inherits the red tint.
[Theme.swift](../../clients/apple/Sources/Theme.swift#L117) already documents
the same tvOS tint/label problem and provides `TVReadableButtonStyle`.

**Fix:** use a shared TV choice-row style that owns both foreground and fill;
show selection with a checkmark and focus with a distinct high-contrast outline.
Apply it to Close too. **Proof:** screenshot and visual inspection of every
choice focused/unfocused/selected/disabled in all themes and appearances on
supported tvOS versions. An accessibility name alone does not pass. R02.

### F03 · P1 · R — Back can be delivered outside the menu

**Evidence:** [RemoteChoiceExitAdapter](../../clients/apple/Sources/PlayerRemoteAdapter.swift#L144)
is installed on the panel. F01 leaves native focus in the underlying sibling;
that panel is not the focused view's ancestor. Thus its exit handler cannot be
assumed to receive Back. The exact resulting pop/hide/exit differs by host and
has not been reproduced here.

**Fix:** the top presented owner handles Back once. Close its menu first,
without also popping the page or hiding/exiting the player. **Proof:** Back
immediately after opening and after every choice position; repeat with rapid
presses during presentation and dismissal. R02.

### F04 · P2 · S/R — Jump scrolls content but does not hand focus to it

**Evidence:** [LibraryView](../../clients/apple/Sources/LibraryView.swift#L65)
changes `selectedGroup` and calls `scrollTo`; it does not request the first
item's focus. The selected-group indicator also does not follow manual
scrolling. The existing jump test uses iOS taps, not a TV remote.

**Fix:** Select on Jump to M scrolls to and focuses M's first available item
after realization. Preserve a separate last index choice for returning to the
index. Clear or update the marker when sort/filter removes the group.
**Proof:** choose an offscreen group, press Select then Right; focus moves among
that group's items, not along the alphabet. Repeat after manual scrolling. R01.

### F05 · P2 · R — long-library escape and restoration lack a TV contract

**Evidence:** collection/group grids have no explicit physical focus bookmark
or bounded route from deep content to tools. They rely on native geometry and
retained view state; semantic remote restoration is a separate mechanism.
[LibraryGroupView](../../clients/apple/Sources/LibraryView.swift#L304) reloads
its coordinator on arrival. Sort/query/filter can replace the visible targets.
This does not prove every Back restoration fails, but the current tests do not
establish it works.

**Fix:** bookmark item identity, group and scroll location. Add a persistent
Library tools target at the grid/shelf's left boundary: at most six Left presses
in a six-column grid reach tools at any depth. Up still means the preceding
row; Back still pops the destination. Tools return to the saved item. When an
item disappears, choose the next surviving neighbor, then previous, then the
empty-state action. **Proof:** item 1/200/416, View all, detail, playback, sort,
filter and refresh all have explicit landing assertions. R01.

### F06 · P2 · S — season episode cards have no details action on TV

**Evidence:** [EpisodeCard](../../clients/apple/Sources/Components.swift#L320)
has a single television Button mapped to Play. The copy-to-detail link exists
only in the non-TV branch. From that season shelf there is no direct episode
details route to inspect its full description or choose tracks before playing.
Searching separately is a workaround, not a replacement for that route.

**Fix:** preserve Select-to-play if that remains the product contract; add a
visible shelf-level “Episode options” action tied to the focused episode, with
Details, Audio/Subtitles and Start over. Do not require a hidden long press.
**Proof:** reach episode details from its season without starting playback;
Back restores the exact episode. R03.

### F07 · P2 · S — truncated synopses cannot be expanded

**Evidence:** [tvPlayableContent](../../clients/apple/Sources/DetailView.swift#L1508)
limits synopsis to two lines; the TV series header limits it to five. Neither
has a Read more action. The movie body contains track facts/editions/children,
not a complete synopsis destination.

**Fix:** an About action opens the full synopsis in a readable scrolling sheet,
with explicit focusable text regions on TV. **Proof:** read the last paragraph
of a long synopsis and close back to About. R03.

### F08 · P2 · R — long action labels can exceed the fixed detail hero

**Evidence:** [TV playable hero](../../clients/apple/Sources/DetailView.swift#L1497)
has a 980-point copy area, fixed height and clipping. Its second HStack holds
full Audio and Subtitles summaries plus Version, without adaptive wrapping.
Long language/codec/track names can compete with controls and hide choices.

**Fix:** short stable action labels with selected values on secondary lines;
put Versions and detailed track facts in the same options sheet. Bound the
visible hero copy, not the reachability of its actions. **Proof:** two-line
title, long synopsis, resume/start-over, multiple versions and longest track
names simultaneously, at 1080p and 4K. R03.

### F09 · P2 · S — Home and Libraries failures offer no TV Retry

**Evidence:** [HomeDashboard and LibrariesDashboard](../../clients/apple/Sources/HomeView.swift)
render `ContentUnavailableView` on `homeError`. Refresh gestures are iOS-only;
the empty library text says “pull to refresh” on TV too. Foreground refresh
exists but is an indirect recovery route.

**Fix:** Retry next to the error, preserve any usable cached shelves, and use
TV-specific empty-state copy. **Proof:** disconnect/recover the fixture; Retry
restores content without app relaunch or leaving the app. R04/R10.

### F10 · P2 · S/R — search failures require editing, and refresh removes results

**Evidence:** [SearchView](../../clients/apple/Sources/SearchView.swift#L58)
shows an error without Retry; `.task(id: query)` runs again only when the query
changes or the view task restarts. `searching` replaces the results with a
spinner. Its TV search field scrolls with results. Native focus behavior when
an arriving query removes the focused card remains unverified.

**Fix:** fixed search field, keep previous results with a loading status until
the replacement arrives, explicit Retry/Clear, and identity-based restoration.
**Proof:** empty query, no matches, error then Retry unchanged, out-of-order
responses and Back from a deep result. R05.

## 4. Findings — playback, Live TV and channels

### F11 · P1 · S — diagnostics floats above video outside the controls

**Evidence:** [Live TV fullscreen](../../clients/apple/Sources/LiveTvView.swift#L2392)
and [library-channel fullscreen](../../clients/apple/Sources/LibraryChannels.swift#L936)
attach Report a problem as an unconditional top-trailing overlay. It is not
conditioned on chrome visibility and has no place in those focus targets.
The channel player also puts Return/Pause/Stop in the same corner. Local VOD
instead includes reporting in its control row and failure view; the unauthenticated
root has a separate bottom-trailing button. This is not one global button on
every ready screen, but several independently placed copies.

**Fix:** Help → Report a problem in Settings; More → Report a problem in every
player; a secondary action on actual failure panels; contextual Help on
connection screens. No persistent report button over healthy video. Capture
the incident promptly when selected and retain the existing privacy/export
flow. **Proof:** after idle, no report or unrelated control is visible or
focusable; reveal → More → Report → Close returns to More with playback intact.
R06/R09/R10.

### F12 · P1 · S/R — library channels uses a separate fullscreen control system

**Evidence:** [channelFullscreenSurface](../../clients/apple/Sources/LibraryChannels.swift#L1005)
embeds interactive `VideoPlayer` plus permanent Return/Pause/Stop controls.
It has no integration with the finite/live custom input adapters. Watch from
start adds another permanent Return to channel button above `PlayerView`.
Duplicate control ownership is source-confirmed; exact AVKit/custom focus
collisions require device verification.

**Fix:** share the TV playback chrome/presentation contract while preserving
the library channel's existing session/controller. Return to channels is a
visible-chrome action; leaving fullscreen retains the live channel; Stop is
explicit. Watch-from-start's return action belongs in the player's controls.
**Proof:** same hidden/reveal/menu/Back behavior for local, live, channel and
shared playback, including transitions back from personal playback. R06.

### F13 · P2 · S — future library-channel programmes are buttons with no action

**Evidence:** [channelList](../../clients/apple/Sources/LibraryChannels.swift#L1054)
sets `focusedProgrammeId` on Select, then tunes only if the programme is on
now. Future/past cards have no explanatory action. The “Focused programme”
accessibility value reflects the last selection, not native focus movement.

**Fix:** Select opens programme details for non-current programmes, with timing
and applicable actions; current programmes can retain Watch. Observe actual
focus for focus labels. **Proof:** future programme Select changes visible UI
and explains availability, without tuning the current programme accidentally.
R07.

### F14 · P2 · S — channel layout changes by an undisclosed cycle

**Evidence:** [library-channel toolbar](../../clients/apple/Sources/LibraryChannels.swift#L886)
cycles three layout strings every press. The label describes the current
layout, not a choice list or the next action. Live TV uses a layout panel.

**Fix:** one layout picker with named choices and a checkmark, shared with the
Live TV presentation vocabulary. Restore the Layout opener on close. **Proof:**
choose each layout directly, cancel without changing it, retain the programme.
R02/R07.

### F15 · P2 · R — the Live TV toolbar can exhaust its width

**Evidence:** [liveToolbar](../../clients/apple/Sources/LiveTvView.swift#L2778)
puts On now/Guide/Recordings, a variable summary, Favorites, Search (including
the query), Layout, Guide time, Captions and More in one 48-point HStack.
The two new text buttons use default styling rather than the compact TV style.
There is no fitting alternative or scrolling toolbar.

**Fix:** keep On now/Guide, Search and More at the top. Put guide-time controls
beside the guide, captions in playback options, layout/favorites in the
appropriate menu; keep recordings reachable through the existing destination.
Show full values in menus, not in growing toolbar labels. **Proof:** longest
query/status and recording count together at both TV output sizes. R07.

### F16 · P2 · S/R — new Live TV controls bypass its focus bookkeeping

**Evidence:** Guide time and Captions have semantic `.remoteControl` keys but
no cases in [FocusTarget.isToolbar](../../clients/apple/Sources/LiveTvView.swift#L2048).
They do not update `lastToolbarFocus`, which `returnToToolbar()` uses. Entering
content after one of these can return to an older toolbar control. The exact
native path needs reproduction.

**Fix:** register every toolbar action with the shared focus owner, or remove
it from that toolbar as in F15. No separate native and semantic identity lists
that can silently diverge. **Proof:** leave and re-enter content from every
toolbar control; restore that exact control. R07.

### F17 · P2 · R — reminder actions are not part of fullscreen focus ownership

**Evidence:** [ReminderOverlay](../../clients/apple/Sources/ReminderOverlay.swift)
is an independently focusable set of Watch/Record/Dismiss buttons; its expiry
removes them. It has no focus restoration and its comment relies on platform
Back dismissal. Live fullscreen's keyed reveal/pill controls and Back reducer
do not have a reminder state. Do not assume the comment proves dismissal.

**Fix:** noninteractive reminder toast while chrome is hidden; reveal exposes
a named reminder action. Opening its action sheet freezes chrome auto-hide;
expiry closes the sheet safely and restores its opener/fallback. Do not steal
focus on arrival. **Proof:** reminder appears/expires while scrubbing, in a
menu, focused on Dismiss and during playback; one press causes one action.
R06/R07.

## 5. Findings — recordings, settings, support and shared media

### F18 · P2 · S/R — recording confirmation appears away from its opener

**Evidence:** [DvrRecordingsPanel](../../clients/apple/Sources/DvrRecordings.swift#L689)
inserts delete/stop confirmation above the scrolling content, with no focus
handoff. `delete_file_required` can reveal it after a server round trip while
the viewer remains deep in the list. Detail stop confirmation is another inline
HStack. Reading it visually does not prove the remote reaches it.

**Fix:** one native confirmation sheet, Cancel/Keep as initial focus, explicit
recording title and consequence; close to the same row or its neighbor after
deletion. **Proof:** confirm/cancel from the final row in a long list, delayed
response, long title, repeated Select and Back during dismissal. R08.

### F19 · P2 · S — deleting a series rule has no confirmation

**Evidence:** [ruleRow](../../clients/apple/Sources/DvrRecordings.swift#L978)
places Delete next to ordering and enable controls; it calls `deleteRule`
directly. A navigation mistake can remove a recording rule.

**Fix:** put secondary row actions in an options sheet and confirm rule
removal. Preserve fast non-destructive enable/reorder actions. **Proof:** one
stray Select cannot delete a rule; Cancel preserves it. R08.

### F20 · P2 · S — manual recording asks TV users for an internal channel ID

**Evidence:** [manualRecording](../../clients/apple/Sources/DvrRecordings.swift#L1005)
requires a nonempty “Channel ID” TextField. No lineup picker explains or
supplies that identifier. TV time choices are limited to Now/5/15 minutes and
30/60/90/120-minute durations despite copy promising an exact time range.

**Fix:** searchable channel-number/name picker and clearly labeled start and
duration choices; either support exact scheduling or describe the actual
limits. **Proof:** schedule with only the remote and displayed channel names,
without copying an internal ID from another app. R08.

### F21 · P2 · R — DVR rows mix unstyled links with strongly styled actions

**Evidence:** saved/schedule/activity links in
[DvrRecordings.swift](../../clients/apple/Sources/DvrRecordings.swift#L910)
use `.buttonStyle(.plain)` without a local TV focus treatment, while adjacent
DVR buttons use a custom plate. Long rule rows contain five separate actions.
Whether the runtime supplies an adequate focus cue for each plain link must
be visually checked.

**Fix:** one clear focus treatment; make the whole title row a details target,
with secondary actions in its details/options surface. Label reorder actions
“Move earlier/later” rather than only triangles. **Proof:** a viewer can
identify the selected title/action from a screenshot and reach every action
with no diagonal navigation guess. R08.

### F22 · P2 · S — Help and routine settings sit below a long mixed form

**Evidence:** [SettingsView](../../clients/apple/Sources/SettingsView.swift)
is one scrolling Form covering playback, appearance, administration, sharing,
account, remotes, Developer, Help and About. Help is near the bottom. The
connection root solves discoverability by floating reporting elsewhere.

**Fix:** TV category navigation with Playback, Appearance, Live TV, Sharing,
Account, Remotes, Help, Developer and About; each preserves its focused setting.
Place Help among the first visible category choices. Connection errors expose
Help alongside Retry without a permanent unrelated overlay. **Proof:** Help
and a playback preference have bounded routes independent of readiness text
length; Back returns to the category. R09/R10.

### F23 · P2 · S — reporting is a diagnostics workbench before it is a TV flow

**Evidence:** [DiagnosticReportView](../../clients/apple/Sources/DiagnosticReportView.swift#L326)
contains capture, category, description, privacy, export, reproduction,
connection checks, administrator snapshot, history and events in one Form.
Moving events after actions was a useful previous repair, but the primary
phone/ZIP handoff still competes with many advanced actions. Settings Help
still advertises sending to support, while the current ordinary UI exports ZIPs.

**Fix:** capture immediately, then show “Save on phone”, a concise privacy
summary and Done. Keep advanced checks/history on separate routes. Keep local
export explicit, preserve cancellation/revocation and saved-draft behavior,
and describe the actual ZIP attachment flow. **Proof:** capture → QR → Close
from every player; export is never automatic; existing reporting tests remain
valid and gain exact opener restoration. R09.

### F24 · P2 · S/R — shared playback has a different TV interaction model

**Evidence:** [SharedLibraryDetailView](../../clients/apple/Sources/SharedLibrariesView.swift#L292)
opens a sheet containing [SharedPlayerView](../../clients/apple/Sources/PlayerView.swift#L3780).
That view places a noninteractive video surface, status text, a permanent
control HStack and Close in a VStack. It does not attach the local player's
physical Play/Pause/Back adapter. The controls can be selected individually,
but equivalence of physical transport keys is not established.

**Fix:** reuse TV chrome and input presentation while retaining the shared
controller's authorization and command ownership. Keep Source visible in
details/options; do not route shared commands through local playback APIs.
**Proof:** physical Play/Pause, reveal, Back, menu and return behavior matches
the other TV players; revocation still stops the authorized session. R06/R11.

### F25 · P2 · S — shared menu restoration names unregistered openers

**Evidence:** [SharedPlayerControls.openChoices](../../clients/apple/Sources/PlayerView.swift#L3905)
uses `shared:quality`, `shared:audio`, `shared:subtitles` as return keys. Those
opener buttons have accessibility identifiers but no matching `.remoteControl`
registration. Closing the panel requests a focus key no opener observes.

**Fix:** unify control identity and restoration registration, then verify
actual focus acknowledgment after dismissal. **Proof:** Quality/Audio/Subtitles
Select or Back lands on its exact opener, including busy settlement. R02/R11.

### F26 · P2 · S — remote registration reset acts immediately

**Evidence:** [RemoteDeviceSettingsView](../../clients/apple/Sources/RemoteDeviceSettingsView.swift#L34)
resets TV registration/revokes all paired phones on one Select, without a
confirmation. Per-device revocation is also immediate. This is consequential
UI next to routine management controls.

**Fix:** explicit confirmation showing the affected device(s), default Cancel;
post-action focus returns to the remaining list or Refresh. **Proof:** a single
Select only opens confirmation; Back cancels; rapid repeat does not cascade.
R09.

### F27 · P2 · S/R — focus presentation varies, and reduced motion is untested

**Evidence:** [Theme.swift](../../clients/apple/Sources/Theme.swift) uses a
four-point focus ring for readable buttons, a one-point ring for player icons,
and different lift/glow treatments for posters and shelf actions. New menus
bypass those styles. These styles contain unconditional scale animations and
no explicit Reduce Motion branch; resulting device accessibility behavior has
not been verified. Selection and focus can both use the same accent.

**Fix:** common focus tokens per control size, persistent checkmark for selected
values, clear disabled state, and a reduced-motion variant that retains the
focus outline without lift/animation. **Proof:** focus remains distinguishable
from selection in every theme, dark/light, Reduce Motion and VoiceOver. R01–R11.

### F28 · P2 · S/R — informational pages need a deliberate readable focus path

**Evidence:** track facts and recording history include long runs of plain
Text inside scrolling content. They do not all use the explicit `.focusable()`
reading treatment already present in
[PlaybackInfoPanel](../../clients/apple/Sources/PlaybackInfoPanel.swift#L197)
and diagnostic event previews. At couch distance, missing a reachable scroll
anchor can make below-fold text effectively inaccessible. Separately,
PlaybackInfoPanel resets focus to Close on every mode change, moving it off
the Standard/Details/Debug choice the viewer just selected.

**Fix:** focusable text blocks where scrolling requires them; keep focus on the
selected info mode, use Close only for initial entry if that is the agreed
rule. **Proof:** reach the final history/facts line and switch info modes
without being moved to Close. R03/R08/R09.

### F29 · P1 · S — current release evidence does not enforce TV journeys

**Evidence:** [LibraryRowsNavigationTests](../../clients/apple/UITests/LibraryRowsNavigationTests.swift#L1)
is entirely `#if os(iOS)`. [TVPhysicalInputTests](../../clients/apple/UITests/TVPhysicalInputTests.swift)
contains useful settings/playback/captions/paging tests, but requires a signed-in
physical device and named media. Its library test still expects Category/Library
segmented buttons although the current dashboard uses a Group by choice panel.
[project.yml](../../clients/apple/project.yml#L239) puts TV UI tests only in the
separate physical scheme; [make apple-test](../../Makefile#L1882) runs the ordinary
unit-test schemes. The effort and ordinary fast lanes compile Apple code but
do not drive focus journeys. Earlier layout docs explicitly left moving focus
and hardware acceptance open.

**Fix:** the enforceable contract in §7. Compile, reducer tests and static
screenshots must never be reported as navigation acceptance. Update stale
physical tests to the actual UI before treating their existence as protection.
**Proof:** deliberately remove modal focus entry in a control branch; the
required simulator journey fails and prevents qualification. R12.

### F30 · P1 · N/S — returning to a root tab steals focus into its content

**Evidence:** the shipping-route fixture test returned from Movies to Libraries,
focused its root tab, moved Left to Home, then Right to Libraries. The next
native focus was Movies' See All button rather than the Libraries tab. No Down
or Select was issued. `synchronizeRoute()` requested the stored content target
on a tab change, interrupting native horizontal tab navigation.

**Fix:** retain the content bookmark, but do not request it while switching
root tabs. Down enters content through native spatial navigation. Returning
from a destination within the same tab still restores its opener. **Proof:**
all enabled tabs in both directions after visiting content, plus detail Back
restoration and feature-removal fallback. This new finding comes from the
implementation's whole-app tests, not the original source-only audit.

### F31 · P1 · N — hidden playback paints a white focus box over video

**Evidence:** after build 239 was installed, the owner reported a white rounded
box centered on playing video which disappeared when controls appeared. The
retained native hidden-video capture contains the same box. The earlier visual
review missed it: native focus assertions passed without checking whether the
focus target painted visible chrome. The new screenshot assertion fails on the
old implementation with 148 bright pixels in the solid-black fixture's center.

**Fix:** the shared reveal target keeps its native Button and remote routing,
but uses an explicit label-only ButtonStyle. tvOS PlainButtonStyle paints a
focused bezel even when its label is clear and focus effects are disabled.
Movies, Live TV, library channels and Shared playback all use this target.

**Proof:** each of the four existing native playback journeys now checks the
actual screenshot pixels after asserting native hidden-target focus, then
continues through reveal, More/report and exact Back/session restoration.
The black MP4 and HLS fixtures make visible control chrome a measurable
failure. Earlier 26-journey receipts remain input evidence; they do not prove
this missing visual assertion. All four corrected journeys pass, and the hidden movie and Live captures were
visually checked. The corrected build 239 is installed on the paired Apple TV.
Final promotion stays held until the correction and the updated gate pass.

### F32 · P0 · N — full Live TV guide stalls and loses visible focus

**Evidence:** the owner supplied a physical-TV photograph of the 77-channel
lineup, reported invisible focus and sluggish navigation, then clarified that
the guide nearly froze the entire UI. A native shipping-route fixture with 77
channels reproduces a tens-of-seconds accessibility stall on guide entry. The
eager grid realizes every channel/programme control, while scroll geometry
changes update the grid. The two-channel playback fixture did not exercise
that load. The photograph also shows a large idle preview, clipped bottom rows,
a selected-segment background extending into the safe area, and a timeline
still anchored to the time the app opened.

**Implemented repair:** realize guide rows lazily, use a vertical TV
viewport with explicit time paging, remove per-scroll origin publication on TV,
keep one focus binding per cell and toolbar control, and route each direction
from the control receiving it. Toolbar Down/Select records its explicit origin;
transient native arrivals cannot overwrite that bookmark. Up through programme
actions and closing a choice sheet restore their respective exact openers. The first lazy-row trial eliminated the long entry stall but
exposed native focus moving before the outer custom direction handler; that
failed trial is retained. Use a white focus outline without zoom, a compact
detail stage with no idle preview, and clock-following until the viewer
explicitly pages away from Now.

**Validation:** the final isolated native run passes all three Live TV journeys.
The 77-channel fixture mixes half-hour and two-hour programmes and traverses
every row down and back without changing the vertical time anchor. Every step
requires the actual focused programme to fit inside the viewport within two
seconds, with a three-second bound including the press and native queries. The
last programme's action sheet restores its cell. Guide time closes to its exact
opener, and Down into content followed by Up returns to that same toolbar
control. The full traversal takes 122.63 seconds; all three journeys take
286.87 seconds. First/last-row and toolbar-return renders were inspected.
All 1,099 Apple TV unit tests and iOS simulator compilation pass.

The first saved-account physical traversal passed on the real 77-channel
lineup (126.99 seconds), with visible focus throughout and Back returning to
the root tab. Later faster-input checks caught a redundant scroll resetting
the viewport, and mixed-duration checks prompted the stable time anchor. The
final guide leaves completed native moves alone, reserves an opaque heading
outside the scroll content, and keeps channel labels inside their own column. Physical screenshots exposed
clipped diagnostic badges; TV guide rows now show station identity only, with
technical formats retained in stream information.
The required suite now includes both full-lineup guide journeys. Failed
attempts remain retained; neither missing tests nor skipped journeys qualify.

**Physical acceptance:** the final saved-account device repeat passes all
77 channels down and up, visible focus, a three-second response bound per
step, no future-programme drift and Back to the root tab (137.94 seconds).
This final build also removes codec badges clipped inside the channel column;
its first/last-row captures were inspected on the physical TV. Additional fullscreen-guide open/Back and Up/Close checks caught requests
being sent to the covered browser instead of the mounted player focus owner.
The player controls and guide Close now own those requests inside the cover.
The extended Live playback journey passes both Back and Up/Close, reporting,
a two-second bound for Back-to-hidden-video (shorter than auto-hide), and
return to the original channel with exactly one tuner session. Its retained
native session takes 123.53 seconds. More now explicitly owns its Back; relying
on native sheet dismissal had left the next playback Back unhandled. The combined 28-journey repeat passes with zero failures and zero skips on
clean source `0c52e02868687c845da2ff2b0a46095fc42c3e21` (2,249.15 seconds).
The receipt verifies unchanged source across the run and preserves the native
result bundle and portable screenshot exports. The refreshed `d2abab249` integration
candidate retained identical Apple source; its current-main Rust compilation,
Clippy and focused gate contracts pass. Mandatory promotion remains open,
as does the remaining physical accessibility/gesture acceptance matrix.

### F33 · P2 · N/S — playback hide countdown outlives a new presentation or failure

The single adversarial review of candidate `d2abab249` found two remaining
ownership gaps. Live Recording activity did not suspend the four-second hide
timer or restore its exact opener. A library-channel error arriving while
controls were already visible did not invalidate that timer; its completion
could hide the failure and recovery controls.

The shared countdown now rechecks eligibility after waiting and rejects
cancelled requests. Live activity participates in the same presentation
ownership and dismissal restoration as the other cover sheets, with Play as
the fallback when the activity indicator disappears. Channel error changes
invalidate the current countdown and keep the failure visible.

**Focused evidence:** a deterministic unit injects a real controller failure
while the countdown is suspended; another exercises a newer presentation,
cancellation and unchanged idle state. Both pass. The expanded native Live
journey leaves Recording activity open beyond five seconds, then requires
actual focus on its opener. It fails on the unchanged production baseline
and passes on the correction (109.29 seconds), including the existing guide,
report and Back paths. The channel playback smoke also passes (90.74 seconds).
The actual activity-return capture was inspected. All 1,101 tvOS unit tests
pass with zero failures; the iOS simulator target also builds. The earlier 28-journey
receipt predates this follow-up; final qualification must run the expanded
journey against the promotion candidate. Build 253 includes these corrections
and is installed and launched on the paired Apple TV. They are integrated
with the guide correction in the same task candidate. Gate 5287 passed every
other job but exhausted its thirty-minute server compile allowance without a
compiler diagnostic; the effort compiler now has the same sixty-minute
allowance as the main compiler, with all checks retained.

The first full follow-up repeat exposed a timing failure between activity
return and the Guide pill; its failed receipt and captures are retained.
An isolated trace passed and showed the countdown restarting on actual focus
changes. The harness now checks already-settled native focus immediately,
rather than spending XCTest's first polling interval on every assertion; it
retains the same focus predicate, failure timeout, screenshots and deliberate
idle deadlines. The expanded Live journey passes with that harness (89.37
seconds). A complete clean-source repeat remains required before promotion.

### F34 · P1 · N/S — playback defaults compete with directional focus

Two full follow-up runs fail intermittently after activity or fullscreen-guide
return: the requested next pill never retains focus, then the idle timer hides
the controls. Both failed receipts, logs and captures remain retained. The
immediate-focus assertion improvement alone does not close this failure.

The instrumented repeat isolates a double movement: one Right press moves from
Guide to Channels and then Info within 37 milliseconds. Every actual change
restarts the countdown, which hides four seconds after the final change with
the same generation. This rules out a stale countdown in that reproduction.
The shared playback region had a default preference that followed observed
focus, plus a parent feedback path that could request that focus again.
[Apple documents the default-focus priority](https://developer.apple.com/documentation/swiftui/view/defaultfocus(_:_:priority:));
changing its priority to automatic still failed the stronger native repeat.

The candidate removes the changing default preference, requests entry once,
and ignores explicit requests already equal to the region's actual focus.
Presentation restoration remains explicit. The native Live journey alternates
Right/Left four times after guide closure and requires actual Channels/Guide
focus on every move before proceeding. The first diagnostic run passes this
sequence and the complete playback journey (99.22 seconds). Temporary tracing
has been removed. Two uninstrumented Live repeats pass (98.35 and 97.39
seconds), and the first library-channel repeat passes (73.49 seconds). The
second channel repeat exposes the separate browser gap below before playback
opens. The subsequent clean full run at `b40d88e4c` still fails the first Left
after guide closure. Removing the default preference and ignoring equal writes
are therefore insufficient. The next candidate removes the observation-to-parent
callback entirely: only explicit presentation intents may update the requested
focus key. The strengthened native journey now requires twenty Right/Left pairs.
Removing that callback alone also fails at the fourth Left in the longer
repeat. The current candidate therefore assigns explicit neighbours to every
mounted Live transport control through the existing remote adapter, routing
from the receiving control rather than a potentially advanced focus binding.
Each direction invalidates stale remote intent and resets idle, including row
edges; Activity has explicit Up/Down routes. Two forty-move native repeats
pass, including subsequent reporting and session retention. The final journey
also exercises repeated input at both row edges for longer than the idle
deadline. Complete-candidate validation remains required.
The failed full receipt, focused trials and cancelled gate 5310 are retained. The explicit-owner
candidate `101fc89f3` subsequently passes the ordered full run's Live journey
(146.64 seconds), including forty consecutive moves, five seconds of input at
each row edge, reporting and retained playback. Its 1,101-unit suite has zero
failures/skips. A later reverse-root-tab check samples route existence before
the native transition settles: focus is Libraries while the accessibility tree
still reports Live TV about a quarter-second after the press. The forward check
already allows five seconds; the isolated follow-up applies that same bounded
route check in reverse, preserving actual focus and no-Select assertions. The
failed run remains retained: 27 of 28 journeys pass, with no skips. The corrected
root traversal passes twice (54.22 and 53.75 seconds) on unchanged source, with
actual focus and route checks in both directions. Complete-candidate qualification
remains open.

### F35 · P1 · N — Favorite has no Down destination in a short channel row

The repeated channel journey exposes a browser failure before fullscreen
playback: Favorite sits at the far right of the card, while two short programme
buttons and Watch live end far to its left. Eight Down presses leave actual
focus on Favorite. The retained hierarchy and capture show the gap; this is
not a playback hide-timer failure.

Watch live now fills the card width on TV, using the shared readable button
style. Its focus area gives Down a destination below Favorite even when the
programme strip is short. Touch layout is unchanged. The native playback
setup now explicitly focuses Favorite, presses Down once and requires Watch
live to own actual focus before tuning. Two complete native channel playback
repeats pass (78.11 and 73.56 seconds), and the full-width Watch focus rendering
was inspected. iOS compilation and the 1,101-unit suite pass on the combined
candidate. Complete app qualification remains open.

### F36 · P2 · N — final synopsis focus outline meets the scroll clip

The coordinating visual review of the complete run finds the final paragraph's
white focus outline clipped at the bottom of the reading viewport. The final
text is readable and Back restores About, but a passing focus assertion alone
does not establish a complete visible outline. This view also serves Shared
About.

The reading stack now reserves eight points inside its scroll viewport. A native
regression requires every focused paragraph, plus four points of outline
clearance, to fit within that viewport. The strengthened assertion fails on the
prior layout (39.23 seconds). With clearance, the local journey passes in 46.99
seconds and the complete Shared journey in 73.60 seconds. The coordinating
agent inspected the new final-paragraph capture: all four sides of the focus
outline are visible, with space above Close. The prior screenshot and negative
result are retained. This follow-up still needs complete-candidate qualification.
R03/R11.

