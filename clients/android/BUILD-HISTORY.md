# Android build history through build 207

What each Android build changed, moved verbatim on 2026-11-15 from the status
blockquote of [README.md](README.md), where every Android change prepended a
paragraph at the same line. Builds after 207 are numbered at release and
described by the [changelog](../../CHANGELOG.md); nothing is added here.

Build `207` (v0.8.0) — prepared Dolby Vision handoffs retain
the executing choice and film position after optional Auto failures, and
the control playhead stays in title time for streams that began mid-title.

Build `206` — the ebook reader accepts the same supported server origins as the session.

Build `205` — direct Dolby Vision playback receives the
original-file enhancement report, and playback controls show when Cinema is
tone mapping. Native `DYN` requires a selected Dolby Vision decoder on a Dolby
Vision display; pending or fallback playback explains why it is inactive.
Server tone mapping requires current processing evidence and is shown
separately from native Dolby Vision. Physical Lenovo verification remains open.

Build `203` — the rendezvous never precedes a prepared
successor's own start, so a processed upgrade staged at the window its trial
already produced is met where it begins. Build 203 changes only the version
code so Google Play can deliver a fresh install to existing testers.

Build `199` — a prepared successor primes and serves
untunneled, so a Dolby Vision-enhanced upgrade no longer takes the
television's hardware A/V sync from the player on screen.

Build `197` — releases builds 195–196 through the
internal-testing channel: no app change; the channel's release guard needs a
version commit after the last Android source change.

Build `196` — Playback Info shows the server's
real reason a Dolby Vision enhancement was skipped or is still qualifying.

Build `195` — the HEVC decoder envelope is the one
Media3's own decoder selector reports, rather than a re-derivation of it.

Build `194` — the HEVC decoder envelope stops at the
vendor's declared performance points, the bound Media3's decoder selector
applies, so the hvc1 probe holds and processed Dolby Vision (HDR10-E) can
upgrade playback on decoders such as the Google TV Streamer's.

Build `193` — a prepared switch hands MediaSession the successor only
after its decoded frame and reports its seam (`prepared_seam`) with decoder-counter frame
and audio-underrun evidence, once per commit that began, with whether an ordinary reopen
actually ran in its window; a create refusal no retry can outlive
(`transcode_height_unavailable`, `transcode_scratch_exhausted`, `replacement_expired`) ends the
start on its own surface row, and `media_session_overloaded` is retried; every session create tells the server what its start
sequence has left (`X-Plurx-Startup-Remaining-Ms`); media waits retire with their settled
or superseded destination intent, matching the shared playback-surface contract. Native qualification
is tracked in PR #1378; physical acceptance remains open.

Build `188` — progressive captions discover stopped operations before explicit Resume
and show current nonretryable range failures. Discovery creates no demand.

Build `185` — the player shows the server's Dolby
Vision enhancement: one teal `FEL` / `RPU` / `DYN` pill per active
enhancement after the dynamic-range chip, and an "Enhancement" info row
with one line per enhancement (why it is or is not in use). A processed
HDR10 delivery shows them only while a current effective-processing report
holds. JVM tests only; not yet installed on hardware.

Build `174` — Meridian adds independent layout/theme
choices, native phone/tablet/TV navigation and cinematic browsing. Focused
production-host emulator evidence is recorded with the implementation PR;
physical-device and complete playback acceptance remain open.

Build `173` — Authenticated current-account requests
echo bounded read-after receipts across the advertised voter cohort;
unknown writes retire the floor (#1049). Sustainable processed HDR10 Auto
selection is implemented on the HDR repair effort. Source qualification and
physical acceptance remain open for the composed client.

Private incident capture, protected account
storage, local ZIP sharing, resumed support delivery and physical-only TV
phone approval are implemented. Debug app and regression-source compilation
pass; release, runtime/privacy/resource and physical acceptance remain open.

Build `165` — A continuous quality change cancelled
before it reached its owner is reported retained, so it no longer holds Auto
for the rest of the title (#978).
Build `164` — Continuous Auto costs rungs by their link
rate and never holds a rung the link cannot carry (#994); its
capability-probe build passed the Android TV series.
Build `162` — Home hides the Live TV, Recordings,
Library channels and Shared libraries entries (and their remote-navigation
stops) while the server has that section switched off (`GET /api/v1/features`);
not yet installed on hardware.
Build `161` — Developer graduation pass (Enable Live TV
is the first card of Settings → Live TV; Prepared replacement is in Playback;
the release startup profile status is in About); not yet installed on hardware.
Build `160` — Continuous-quality acceptance repairs
(Auto decides again after a viewer quality choice; shared-audio EOF evidence).
Build `159` — Shared-library playback and controls;
not yet installed on hardware.
Build `152` — Prepared-video geometry and playback-panel contrast repairs
from PR #915; hardware visual acceptance remains pending.
Build `150` — Shared-library playback and controls.
Build `149` — Live TV station logos: the list rows,
grid headers, programme details, picture badge and fullscreen overlay draw
the guide's station artwork from the shared `station_logo` rule, fetched
without the account token. Not yet installed on hardware.
Build `148` — a display-mode wait that times out
withdraws its `preferredDisplayModeId` request while it still owns the
window, so a late HDMI switch cannot land mid-playback (needs
`display_mode_match` on; not yet observed on a television), and the
uncalled Auto-quality policy port and its JVM runner are deleted; released
continuous audio outputs are retired again (since build 145 they were kept
and could exhaust the 16-output bound).
Build `147` — a seek pressed while a prepared quality
handoff has switched but not yet rendered is held and runs once on its first
frame (or on the picture an Auto rollback restores) instead of being dropped
until the 8-second target deadline reopens; a rollback reopen lands at the
seek's destination, not the playhead. Not yet on a device.
Build `146` — grouped library rows, a visible letter/year
index, expanded groups with Retry, and landscape home-media cards. Focused
phone and TV emulator checks passed; physical-device acceptance is unmeasured.
Build `145` — continuous quality: manual and Auto
quality changes adapt inside the one player, with owned outputs and release
fences around a prepared replacement, on top of current main's recovery
parity and audio-focus handover. Production and test sources compile;
physical-device transitions remain unmeasured.
Build `144` — decoder envelopes compact conservatively
while preserving crossing limits, profiles and display grades. The bounded
contract is negotiated with the server; no evidence is truncated. Physical
TCL/Streamer acceptance remains outstanding.
Build `143` — no Android source change; the number was claimed with the
server's bounded catalog discovery and canonical recipe dispatch
(`56e52c50e`).
Build `142` — the 10/30-second seek controls use directional arrow icons
(`4f25ab713`); no physical production install is claimed.
Build `141` was claimed twice: on main for the 10/30-second seek controls
(`bdff3314b`), and on the architecture effort (`1ffaae2a4`), where buffer
roles retain their existing ceilings
and clamp to the actual process heap; local diagnostics name the actual Coil
cache bound. Larger incumbent allocation and three-device memory/HDMI
qualification remain open. No physical production install is claimed.
A paused transcoding session the server
retires after its three-minute pause grace no longer surfaces "Playback
stopped"; Play reopens at the saved position (not yet on a device). Build
138's per-box Live TV player view (fullscreen fills tablets) was accepted
on the Lenovo TB322FC and the Google TV Streamer 2026-09-28, TCL 9445X
still owed.
Build 137 — the tuner, programme guide, recording and Library channel
settings leave Developer for Settings → Live TV, as on the web; the
Developer Enable Live TV card shows its live readiness rows.
Build 136 — measured library prefetch, bounded debug/Live diagnostics and
release profile tooling.
Build 135 adds actual Live TV caption choices and explicit watchable
channel actions; its eight focused regressions passed.
Build 134 names an encoder refusal as the encoder being busy.
Build 133 — cluster Live TV intents and advisory Developer enablement.
Build 131 adds in-app PDF/EPUB reading. Build 129 restores
the Live TV picture to the fullscreen box on tablets after a push-mirror
rewind erased its earlier merge. Build 128 rotates the signed release key
while preserving data, with exact-artifact reinstall.
Build 127 separates playback source, stream, and
display dimensions; build 126 preserves Back navigation history; build 125
preserves native viewer parity across phone, foldable, and TV. Build 124
settles a progressive remux seek after later rendered video
crosses the requested position, even if its first frame lands slightly early;
background playback cannot settle that pending seek. PGS tracks display as
Overlay in the player. Build 121 lands on the login screen with the server's sentence when a
sign-in expires after its idle window, instead of leaving an HTTP 401 on Home,
and gives a system interruption (a call, another app taking audio) its own
hold notice that survives the hold timer and clears when the system resumes.
Build 120 stops walking the whole ingress list for playback failures
no other node would answer differently: a 4xx on a segment or playlist is
terminal, and only a connection-level failure or a 5xx tries a peer. It also
excludes the credential DataStore from Auto Backup and from device-to-device
transfer, and gives the release variant a signing config that fails rather
than falling back to the debug key, so the APK the server serves is
release-signed and not `debuggable`. Build 110 executes a server-required copy-HLS remux on initial play,
seek, reopen, track change, recovery and prepared handoff, so Profile 7 → 8.1
conversion never falls back into the progressive copy path that cannot run
it. Older servers omit the requirement and retain progressive behavior.
Build 103 restores the original Home cards, media shelves, library
grouping and TV focus navigation while retaining all item and player improvements.
Build 102 introduces a quieter library Home and an open Media section
with quality/container badges and collapsed searchable track lists.
Build 101 keeps unknown presentation stalls on the selected recipe:
the bounded repair no longer claims decoder starvation or sends the legacy
quality-reduction ticket, while actual Media3 errors retain their existing
transport and compatibility ladders. Build 100 groups playback information
into Overview, Details and Diagnostics, with a labeled Compact view and
explicit missing player dimensions.
Build 99 adds adaptive tablet/TV Live TV panes, retained recording
overlays and controls that grow with text. Build 98 adds permanent Recordings, capture activity, lifecycle
history, reminders and advisory DVR readiness across phone and TV. Build 97
adds channel subjects, resumable matching progress and named
Include/Exclude decisions; saving never waits for inference.
 Build 96 makes every guide cell open its actions sheet: branching on
whether the programme was on the air sent a live cell straight to Watch
and returned, so Record and Record series — which the sheet has always
offered for an on-air programme — were reachable only on a future cell.
Build 95 lets the playlist choose Live TV's target offset instead of
forcing a four-second absolute offset. Build 94 adds recording and reminders to the guide: every cell offers
Record, Record series and Remind me beside Watch, the grid marks what is
already asked for from one marks read per visible window rather than a lookup
per cell, a Recordings row sits on Home and plays finished captures through
the ordinary VOD path, and armed reminders are mirrored into `AlarmManager`
so a sleeping phone still fires.
Build 93 gives every playback-surface source this client owns a raise
site. `STATE_BUFFERING` after start is the contract's `media_waiting` and is
drawn as `buffering`, debounced by the class's own 350 ms; the separate
`isPlaybackWaiting` spinner that used to draw beside it is gone, so one thing
decides what covers the picture. Opening a stream is `client_preparing`. A
prepared successor abandoned after failing, a control reporter that has given
up, and session-status polling that stopped answering each record a
`surface_log_only` line and move nothing. A playlist or segment 503 is a "not
yet" only when its body carries a code the contract lists for that row. What a
stall, a reopen or a failure DOES is unchanged — no threshold, budget,
detector or ladder moved. Build 92 adds the two bounded recovery steps this
client owns under
the playback surface contract (M5). A session create the server answers with
"still building" is re-posted under the same request identity after 1 s, 2 s
and 4 s, bounded by an absolute sixty seconds from the first attempt; a
session that arrives after that is released rather than attached. Both the
ladder and its deadline run only on a cold start — a create over a picture
the viewer is watching is a refused change, and stopping that player after a
minute is the outcome the contract forbids outright. A `BEHIND_LIVE_WINDOW`
error on a finite timeline seeks back to where the picture was and prepares
again, once per attach, instead of failing; Media3's
`seekToDefaultPosition()` is a live-edge policy and is not used, and a live
item keeps the failure it has today. Build 91 makes Live TV starts recoverable
rather than refusable: the 90-second start barrier is gone, a press persists a
`{request_id, touched_at}` hint the owner can retire, a start that gets no
answer replays once with the same id and says so instead of locking the
screen, reopening the app rejoins a session the viewer never meant to leave,
and the guide polls on the owner's own `next_refresh_at`.
Build 90 makes the playback failure surface a projection of the
player: one `PlaybackSurface` StateFlow fed by a pure reducer that runs the
shared contract fixture, typed faults with a class and two identities instead
of an `onError` string, the recovery owner stopping ExoPlayer before it raises
a full-screen prompt, refusal bodies read from session creates and from
Media3's `InvalidResponseCodeException`, an opaque failure view, and a SURFACE
section in Playback debug. Build 89 gives Live TV the web page's proportions on Google TV and on
the phone: one explicit ten-foot type scale, one 24 dp toolbar, a guide whose
slot width is derived from the width it is given so two hours always fit, a
310 dp On now column beside a focusable picture whose Select is fullscreen,
and a full-bleed phone player over one caption and one toolbar. The Layout
menu offers Preview and Over picture; a stored `channel_browser` still
decodes and renders as Preview. Build 84 includes the required Live TV protocol version in every start request. Build 83 sends the active Live TV playback envelope, preserves
compatible broadcast tracks, and reports the selected delivery. Build 82 uses the canonical Library-channel collection route while
the server preserves the first native spelling during rollout. Build 81 adds
Library channel browsing and authoring plus fenced
finite-HLS following; Library-channel readiness remains advisory in Settings
→ Developer and never overrides the explicit enable. Build 80 makes sign-out
revoke the captured server session within a five-second bound, clear the
exact local credential even during cancellation, and leave a newer login
untouched. Build 79 adds three saved Google TV layouts around one Media3 player,
six-hour guide browsing, compact phone schedules, exact-once D-pad delegation,
and measured source-format details. Build 78 labels Live TV channel format and reports live tuner signal
metrics while a channel plays. Build 77 enables prepared replacement by default while keeping its
readiness requirements advisory. Build 76 completes bounded commit → end settlement for prepared
replacements and retains the predecessor until final settlement. Build 75
gives Live TV a programme guide: channel rows say what is on
now with a bar to its end and what is next, a phone or tablet can switch to a
half-hour grid, and picture-in-picture keeps playing when the app goes to the
background rather than releasing the tuner. Build 74 speaks the M6
prepared-replacement protocol: it declares the vocabulary, accepts a
`prepare`, primes a second pipeline against the playhead, reports readiness
and commits — off by default, behind an enable in Settings' Developer section.
Build 73 carries the streaming-reliability effort's client half; it adds no
viewer-visible behaviour of its own. Build 70 gives
the library, search and settings screens a starting
focus and puts every layout's cards and episode rows in the D-pad's reach.
Build 66 retries the selected native subtitle once when its demanded
cache window becomes ready, without replacing video. Build 57 skips the
unchanged-recipe retry when the server has
already ruled that recipe out, which is the only rung its verdict
licenses skipping. Build 46 accepts the temporary live-HLS recovery presentation while
immutable VOD indexes are backfilled. Build 44
retries a stalled stream through another cluster node's
ingress without moving the account origin or spending the codec-compatibility
ladder; only a transport failure moves, and an `https` session will not fall
back to an `http` sibling. Build 43 quotes native reader item/file identifiers
so JavaScript preserves the full signed 64-bit route across the WebView
handoff. Build 41
compacts the shared Mini, Standard, and Debug playback-info
modes, keeps Debug inside every supported screen, and colors fields that need
attention. Build 40 introduced the three shared modes and live server-session
diagnostics. Build 39
restores a saved session's Home shell before checking server
reachability, so an Android process recreation cannot strand resume on an
empty loading surface while its server is unavailable. Build 38 stops
software-only decoder claims above 1080p from selecting direct play while
preserving the server's 2160p SDR transcode output.
Build 37 reports codec-specific decoder-height ceilings, acknowledges
an already-SDR forced-subtitle plan explicitly, and persists redacted failures
that happen before Media3 starts. Build 36 consumes Cinema's server-owned ebook format/action registry
so detected-but-external formats cannot acquire a false **Read** or offline
action. Build 35 displays bounded book author metadata and exact work-linked
editions on browse and detail surfaces. Build 34 adds Cinema-managed offline EPUB reading on phones and
tablets: a profile/revision-scoped original and bounded publication are
published atomically in app-private storage, then served through a synthetic
HTTPS WebView origin with no account bearer or network fallback. Build 33
combined bounded stall recovery for adaptive playback with in-app online
EPUB reading on phones and tablets with
an isolated, same-origin WebView, memory-only authentication, cross-device
locator resume, and no Android/Google TV reader action. Build 31 visibly
hedges estimated credits markers while preserving
exact chapter-derived labels. Build 30 adds the revision-bound ebook reading-state wire models and
authenticated API routes without exposing a TV reader. Build 28 makes the
detail screen list every audio and subtitle track
with the server's own default markers and its five-state preferred-language
verdict, and lets the viewer pick both — including Off — before pressing play;
the choice reaches the first `/decision` and any burn-in cost is disclosed
beforehand. Build 26 adds first-class audiobook details and playback through
the shared audio player and progress path; physical-device acceptance remains
pending. Build 17 added app-managed offline viewing on phones and tablets;
build 18 carried the playback-compatibility fallback, build 19 made a
restored completed transfer release its server-side package and quota, and
build 20 retries temporary PGS extraction capacity instead of abandoning the
subtitle selection, build 21 coalesces hidden-control seeks, restores
position feedback, and pins media-origin consumption, build 22 restores
on-device Compose instrumentation on Android 17 with Espresso 3.7.0, build
23 reapplies the saved download-network policy before restoring an
interrupted transfer after process death, build 24 reports authenticated
TTFF, six-second buffering stalls, and playback errors, and build 25 moves
Android 14+ offline transfers to persisted user-initiated data-transfer jobs.
Their sockets and DNS are bound to the job's granted network; a system or Task
Manager stop stays **Paused by system — tap Resume**. A transfer created on
build 24 has no UIDT registration to inherit, so after upgrading it needs one
foreground Resume tap before unattended reboot recovery can be claimed. The
default-off `pgs-v1` application overlay remains available: Android
fetches authenticated, server-decoded PNG compositions, schedules them on the
source timeline, and draws them above the unchanged Dolby Vision, HDR, or SDR
video. Picture in Picture is unavailable while the overlay is active until
physical-device behavior is accepted.
HDHomeRun Live TV has dedicated live controls and Settings → Live TV
runtime enablement, readiness and exact previous-owner recovery. Its durable
token-free marker prevents duplicate starts after interrupted cleanup or app
death. DRM, DVR, captions and guide scheduling are not supported.
General server administration remains in the web app; the viewing, discovery, and
playback surfaces are native here. The capability matrix is in
[Android client parity](../../docs/clients/ANDROID-CLIENT-PARITY.md).
