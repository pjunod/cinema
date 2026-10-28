# Features (Live TV and DVR) — status history

**Status:** done · records moved verbatim from `STATUS.md` on 2026-09-24

**Moved here from [STATUS.md](../../STATUS.md) on 2026-09-24**, verbatim, by
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
M6. Each section keeps its original heading under the date it was first
recorded in `STATUS.md`; relative links are re-based to this folder and
nothing else changed. Newest first. These are records: a section here
describes the state on the day it was written, and
`tests/operations/test_status_pr_claims.py` keeps holding it to the same
merged-pull-request rule it held in `STATUS.md`.

## 2026-10-04 · Live TV station logos on Apple TV, iOS and Android

**Branch `fix/live-tv-station-logos-native`, issue
[#815](http://forge.lan:3000/noirr/plurx/issues/815).** Paul reported
2026-10-04 that the Apple TV Live TV guide has no station logos while the web
guide does. Cause: #755 put the rule that picks a channel's logo in the web
page (`pages/live-tv.js`) instead of the shared guide reducer and
`tests/playback/live-tv-guide-cases.json` — the one contract all three guides
reproduce — so the native clients, which already decode `image_url`, were
never asked to draw it and no suite could see the gap. The rule is now
`station_logo` in that fixture (lineup-id match, cold guide, 38 address
cases; a textual `https://` + plain-host rule so three languages agree),
answered by `PlurxLiveTv.stationLogoUrl`, Apple's and Android's
`LiveTvGuideReducer`. Apple (build 209) and Android (versionCode 146) draw it
in list rows, grid headers, programme details, the picture badge and the
fullscreen identity, with the callsign until the image decodes. Native
artwork loads on its own connection with no bearer, cookies or HTTPS→HTTP
redirects (Android's app-wide Coil loader adds the bearer to every host).
Adversarial review: no blockers; its five should-fix findings are
addressed (scheme respelled for Coil, Android failure backoff, Apple lock
kept off suspension points, grid headers keep their width without artwork,
wiring pinned by source tests). Not yet on hardware.

## 2026-09-28 · Android Live TV fullscreen on tablets: every box gets its own player view

**[PR #606](http://forge.lan:3000/noirr/plurx/pulls/606), merged 2026-09-28 as `cfa61cb3e`; Android 138 installed on the Lenovo TB322FC, Google TV Streamer, Pixel 11 Pro XL, Pixel 10 Pro Fold and razr ultra 2025; VERIFIED on the TB322FC and the Streamer. Still owed: the TCL 9445X and the Xiaomi 25019PNF3C, which were not reachable over wireless adb.**
Paul reported 2026-09-28 that fullscreen Live TV on the tablets still shows
the small inline picture in a mostly black screen. #509's relayout + surface
rebind *is* on `main` (re-landed by #546 after the push-mirror rewind, in
every build since 129), so this was the fix not working, not a lost commit.
The one PlayerView was moved between the wide browser's picture box and the
fullscreen box with `movableContentOf`, and its SurfaceView kept its creation
geometry. Each host box now composes its own PlayerView on the shared
ExoPlayer and unbinds on release; phones are unchanged (same slot, just a
resize). `LiveTvPlayerSurfaceTest` fails against `main` and passes here;
`testDebugUnitTest` (799/0), `lintDebug` and `assembleDebug` green;
`history-audit` ok. Open as [PR #606](http://forge.lan:3000/noirr/plurx/pulls/606).
The adversarial review found no blocker and confirmed the release/bind order
against Media3 1.10.1 and Compose's apply order; it added PlayerView's API 34
SurfaceView sync workaround for the boxes that resize in place, an honest
KDoc (one black frame per swap, no PiP host on the wide layout) and a
tighter source pin. The fast lane went green (Rust gate included) and the
merge carries the `Regression-Test:` trailer. Hardware, driven over wireless
adb from lab3 with screenshots and `uiautomator` bounds: on the TB322FC the
fullscreen SurfaceView is `[0,97][3040,1807]` and the picture fills it at
16:9; exit returns it to the inline box `[44,492][1753,1453]`; five
fullscreen/exit toggles from the watch pane and three from the Guide preview
kept the tuner (`1 tuner in use`, no stop message); the swap while paused
shows the held frame in both boxes. The Streamer (API 34) plays, fills and
returns the same way. The release build also exposed that
`scripts/sign-android-release` could not read apksigner 37's per-scheme
signer lines (the pinned image has build-tools 37); that landed in the same
PR with three unit tests. Android 138 is a release-signed build; 137 was
never installed anywhere.


## 2026-09-28 · Live TV said "all slots are busy" with every tuner idle

**[PR #585](http://forge.lan:3000/noirr/plurx/pulls/585), merged 2026-09-28 as `2694db665`; not yet deployed — the GPT deploy/verify prompt is in the project doc.**
Paul reported 2026-09-27 (web and iOS) that channels intermittently refuse
with *All Live TV slots are busy*; the FLEX 4K had four idle tuners each
time. The owner's own log named the cause — `tuner_capacity` with
`cause=transcode capacity is temporarily unavailable: background encoding
did not yield within 5.0s` — and the RCA
([LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md](../streaming/LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md))
found four layers: the subtitle backfill held the whole software encoder
pool while walking candidate jobs; eight zombie `subtitle_extract` rows each
cost that walk a full 30 s ambiguity wait because the claim trigger's
definite `no longer claimable` abort was surfaced as an error and nothing
retired the row; a live start that waited out the 5 s window with only
background work in the way was **refused, by design**; and the Live TV layer
labelled an encoder refusal a tuner one. Fixed at each layer: the store
side landed first on `main` as #588 (claim/candidate precondition, bounded
reconcile, guarded settled trigger, SQLite v84 / cluster v62) and this
branch dropped its own copy at the merge; the backfill claims before it
takes the pool; a `Priority::Live` start is admitted over background
ownership after the window (hardware within the cap, software forced, one
WARN + `plurx_transcode_background_overrun_total{pool}`); and the refusal is
`encoder_capacity` with its own copy on all three clients, pinned by the
shared start-cases fixture (Android 134, Apple 196).
One adversarial review round (five findings, all taken): the take over a
stuck permit is now bounded by live usage, the settled-trigger guard the
review asked for is the one #588 shipped, the subtitle pre-check is the
admission's own predicate, and the tests reach the arms they name.
**Decision for Paul to look over:** admitting a viewer over a stuck
background permit reverses the ruling OPERATIONS.md carried ("absence after
five seconds means that worker is stuck rather than permission to start
beside it"); the RCA §3 argues why. Next (GPT): deploy the three voters then lab3,
confirm the eight zombie rows retire on the first upkeep pass, tune 6.1
under backfill load twenty times from web and iPhone, screenshot the
`encoder_capacity` copy, and install Android 134 / Apple 196.
commit as the work it describes; a stale entry here is a bug. Newest effort
first.


## 2026-09-28 · Apple TV Live TV navigation: every press reversible, every control reachable

**[PR #589](http://forge.lan:3000/noirr/plurx/pulls/589) merged to `main`
2026-09-28 as `f400c0ea2`, Apple build 195; not yet installed on any device.**
Paul reported 2026-09-27 that Live TV navigation on the Apple TV
was close to broken: hard to reach anything, and a move often did not reverse.
Two independent reads of `LiveTvView.swift` agreed on the causes, and one
was worse than reported: Info and More on the fullscreen pills did nothing at
all, because their sheets hung off a root that was already presenting the
cover. Fixed in one Apple PR: the cover owns every sheet it can open; one
`FocusTarget` key per view (the toolbar and the pills no longer share
`.guide`/`.channels`/`.more`); Up from the guide's first row goes to the
stage's Watch / the temporary guide's Close / Over picture's Close, and Down
from those returns to the cell the grid held; Right past the last programme
and Left from the header page the window and land on the same row; Up from
the header column reaches the paging chips, which are never `.disabled`;
channel rows are not disabled during a tune (focus used to jump to the
toolbar); the detail region beside the list is moved by name so Left from
the picture returns to the row you came from; the Guide pill opens on the
channel playing and closing it restores the browse view and the pill; leaving
the cover restores page focus from `onDismiss`; a programme sheet returns to
its cell. The restore coordinator applies from `onChange` against the current
view instead of the task's stale copy, and no longer cancels on an
engine-driven arrival.
[LIVE-TV-APPLE-TV-NAVIGATION.md](LIVE-TV-APPLE-TV-NAVIGATION.md)
has the focus graph and the §1 table that doubles as the device checklist.
Verified: `make apple-test` (iOS 674 + tvOS 690, 0 failures) and the full
fast lane, including the promotion gate. Not verified: anything with a remote
in hand — the device pass is `~/Downloads/kit 2/APPLETV-LIVE-TV-NAVIGATION-PHYSICAL-VERIFICATION-PROMPT.md`.


## 2026-09-13 · Live TV start, stall, and tvOS surface has landed on main

**[#301](http://forge.lan:3000/noirr/plurx/pulls/301), merge
`04cbb2e44` into `main`; final adversarial findings were folded and
[Main promotion gate run 2005](http://forge.lan:3000/noirr/plurx/actions/runs/2005)
passed on the exact merged-base candidate.** The server, Android, and Apple
task PRs are merged and their focused automated evidence is green. The
four-case documentation-index regression passes for this closeout.
Paul explicitly directed promotion without waiting for the HDHomeRun, Apple TV,
Android-phone, and web physical results. Those results and the authenticated
tvOS simulator screenshots remain unclaimed; the exact hand-off prompts stay
in the task PR bodies. The promotion adds no feature gate and does not edit the
forbidden input-routing, remote-adapter, fixture, wire-shape, or timeout seams.

## 2026-09-13 · Apple Live TV now distinguishes a stall and owns its fullscreen surface

**[#300](http://forge.lan:3000/noirr/plurx/pulls/300), from
`codex/live-tv-start-stall-apple` into `effort/live-tv-start-stall`;
implementation complete, adversarial review and focused qualification green,
physical evidence pending.** The promotion ships Apple build 154, which
publishes a debounced stall-only waiting state, truthful behind-the-edge and
buffered measurements,
and fullscreen-owned status copy. The tvOS fullscreen surface is now the
reviewed telemetry strip, programme band, waiting tile, paused state, and
five-action focus row; its reveal layer cannot take focus while controls or the
guide are present. Info is a fixed two-column snapshot ledger with programme,
channel, delivery, signal, and summed AVPlayer access-log facts. The iPhone
surface, Live TV input routing, remote adapter, playback-surface fixture, wire
shapes, timeouts, and feature-gate state are unchanged. The exact Apple TV,
HDHomeRun, phone, and web physical prompt is in the PR body; that evidence
remains pending, and Paul later explicitly directed promotion without it as
recorded above. The review's three findings
are closed: Info dismissal yields focus back to Pause, the corrective
fullscreen commit has its durable source-to-test anchor, and the iOS fullscreen
branch is restored unchanged. `make apple-build` compiled both schemes and
the nine named tvOS tests executed on the Apple TV 4K simulator with nine
passes, zero failures, and zero skips. `make history-check` passes with this
PR's corrective fullscreen commits anchored to their regressions.

## 2026-09-13 · Android now lets each HLS playlist choose its live hold-back

**PR #299, from `codex/live-tv-start-stall-android` into
`effort/live-tv-start-stall`; implementation complete, review and
focused qualification green.** The Android
player no longer imposes an absolute four-second target offset on every live
playlist. Media3 now derives the target from the playlist while the existing
eight-second ceiling remains in force, so a newly published two-segment live
window starts at its first listed segment. The named JVM regression pins both
the built configuration and the production call site. The Android-device
first-minute check remains the physical hand-off and ships with the server
candidate qualification; no buffer duration, input route, wire shape, phone
surface, timeout, or feature gate moved.
The adversarial review's two findings are closed: the test now reads the
configuration from a built `MediaItem`, and the corrective commit has its
durable `tests/client-fixes.toml` source-to-test anchor. The exact named JVM
test passed on the installed Android toolchain; the device evidence is still
pending.

## 2026-09-13 · Live TV starts now use a stable one-second cadence

**[#298](http://forge.lan:3000/noirr/plurx/pulls/298), from
`codex/live-tv-start-stall-server` into `effort/live-tv-start-stall`;
implementation complete, physical evidence pending.** The server half now has
the reviewed 24-entry uniform one-second HLS
window, a two-listed-segment / two-target-duration publication barrier,
progress at the newest listed segment, startup counts in its timeout message,
and a graph probe built by the production command path. The two-node harness
requires two entries in its first playlist, serves every listed name and rolls
the full 24-entry window. `scripts/live-tv-hardware` measures first-list and
answer times, durations, target duration and bitrate; `--copy` selects the real
HEVC/AC-3 route and `--via` records the non-owner relay outcome under the
runtime's existing 35 s public deadline. HDHomeRun and client evidence remains
the physical hand-off. The ten named S1/S2 regressions, all four two-node
cluster cases, the hardware-script syntax check, and pinned Rust 1.97.1
check/Clippy/format are green. `make history-check` passes, and this lane's
four corrections have their own `live-tv.integration` evidence mapping. No
timeout, signed request, route, client input contract, or feature gate moved.

## 2026-09-13 · plurx records now, and tells you before a programme starts

**[#294](http://forge.lan:3000/noirr/plurx/pulls/294), titled `WIP:`.**
Built on `effort/live-tv-dvr` as one PR to `main`, mine to merge once the
qualification run on the merged tree is green. Executes
[LIVE-TV-DVR-IMPLEMENTATION.md](LIVE-TV-DVR-IMPLEMENTATION.md)
v2 — the plan Astra reviewed on 2026-09-13, eleven findings, all folded in.
Status and evidence:
[LIVE-TV-DVR-STATUS.md](LIVE-TV-DVR-STATUS.md).

Every guide cell on every client now offers **Record · Record series ·
Remind me** beside Watch. The owner writes the tuner's bytes straight to a
`.ts` under the DVR root with a sidecar of guide metadata; the file becomes an
item in a `recordings` library that any node serves through the ordinary VOD
path. A reminder reaches an overlay on every open client, a local notification
on a phone, and one webhook POST.

Four things carry the design, and each was a defect in the plan's first draft:

- **An airing is `(channel, start)` for its whole life.** Expansion runs every
  fifteen seconds, so insert-if-absent over an index covering *every* state is
  what stops a rule resurrecting a recording the viewer skipped.
- **One channel is one tuner.** Back-to-back airings share a single tuner GET
  and produce two files, with the overlapping padding written to both.
- **An attempt is its own file.** A capture that loses its worker resumes into
  `.a2.part`, so a fenced predecessor still draining cannot corrupt it; the
  gap is recorded and the recording finishes `partial`, never silently `done`.
- **A viewer refused a tuner is told what holds it.** `tuner_capacity` carries
  the recordings by channel and offers to stop one — and their own idle
  session is evicted first, so they are never told a recording took a tuner
  they were holding themselves.

The plan pins on two things — a guide that survives a restart, and the owner
evicting a viewer's own stray before answering a capacity refusal — that were
not on `main` when this branch started, so it built both, and only those two.
The reliability effort (#281) has since merged its own versions, so the merge
into `main` keeps **its** implementations and this branch keeps only what is
genuinely DVR: the fortnight horizon the recording rules need, the programme
identity they match on, and the tuner accounting that lets a recording hold a
transport.

Nothing is gated — `dvr.enabled` is a plain setting, and the Developer tab's
six rows are advisory. Nothing has touched the tuner yet: the hardware pass is
the only unproved step, and §8 of the plan carries its prompts.
## 2026-09-13 · Live TV: the empty guide and the "wait 90 seconds" refusal, diagnosed

**Diagnosis in [#273](http://forge.lan:3000/noirr/plurx/pulls/273); Paul
ruled 2026-09-13 (doc §7): cache the guide, the client never guesses, and a
possibly-held tuner is never a reason to refuse a viewer — Opus builds it from
[docs/features/LIVE-TV-RELIABILITY-IMPLEMENTATION.md](LIVE-TV-RELIABILITY-IMPLEMENTATION.md)
— Astra reviewed it 2026-09-13 (eight findings, all accepted and folded in,
plan §8); lane `effort/live-tv-reliability`.** Both complaints trace to a client guessing at something
the owner knows. The guide is memory-only on the owner, its first refresh
after a restart is a full 20 minutes away (the first loop tick runs before
the serving fence admits the node, is *skipped*, and the skip path sleeps the
whole interval — media1's metrics show exactly one skip at boot and the first
success 20 minutes later), a refresh cannot run until a client has read the
lineup, and the web never re-asks while Apple and Android re-ask 20 minutes
after *they* opened. `start_outcome_unknown` is never sent by the server: it
is the client's start barrier finding a marker it holds for the whole session,
so every tab close, tvOS suspension or mid-stream deploy costs the next open
90 s from the moment Live TV is opened, long after the owner reaped the
session at 45 s idle. Eight starts today, zero failed on the owner.

The fix in [docs/features/LIVE-TV-GUIDE-AND-START-RELIABILITY.md](LIVE-TV-GUIDE-AND-START-RELIABILITY.md):
a durable owner-local guide under the cache root with an event-driven loop
and a server-published `next_refresh_at` the clients poll on; and a public
`request_id` (the marker the clients already persist) plus
`DELETE /live-tv/starts/{id}`, so a press retires whatever the last start
produced and starts afresh in one round trip to the owner — which already
keys and tombstones starts by request id on the internal leg — with the
owner evicting a viewer's own stray before ever answering `tuner_capacity`.
The client-side barrier is deleted, not rewritten; opening Live TV asks the
owner to resume the last stream if it is still live, and otherwise waits for
the viewer to pick a channel.

## 2026-09-12 · Live TV gets the web page's proportions on Apple TV and iPhone

**Built for issue #267; iOS and tvOS compile green and the full simulator
suite is green at 970 cases on the lab Mac.** tvOS resolves the semantic text
styles two to two and a half times larger than iOS does — `.subheadline` is
38 pt there and `.title2` 57 — so a channel row ran the width of the screen
and three bands of chrome left the live preview a fifth of it. Live TV now
sizes itself through one explicit scale and spends one 48 pt toolbar row.

The arrangement follows the rule the web page follows: lists are tall and
narrow, grids are wide. On now is a 620 pt column beside a large picture that
is itself a focus target, so Select on it is Fullscreen and "Return to live"
left the toolbar. Guide is a 302 pt stage over a full-width grid whose slot
width is derived from the width the grid actually got — a hard-coded 300 pt
filled 58% of a 1920 pt screen and could never fit two hours — and the fixed
`rows * rowHeight + 54` frame that pushed the last rows off the bottom is
gone. Earlier / Now / Later became chips in the grid header; the status banner
became a channel count in the toolbar plus one muted line along the bottom of
the content. The adversarial review rejected the four seconds the spec asked
for: a toast cannot repeat itself when the same failure happens twice, and it
hid every cleanup message on a phone.

The Layout menu offers Preview and Over picture. A stored `channel_browser`
still decodes and keeps its raw value; it renders as Preview, because the two
only ever differed in which browse view they opened with. On iPhone the
picture is full-bleed with its chips on it, and the six-line now bar, the
always-visible search field and the filter toggles are gone — four to five
channel rows are visible while a channel plays instead of one.

Nothing about playback, the lease, the input contract or enablement changed.
Apple build 146.

## 2026-09-12 · Live TV gets the web page's proportions on Google TV and Android phones

**Built for issue #267; `:app:compileDebugKotlin` and `lintDebug` green and
the whole JVM unit suite green at 553 cases, including four new proportion
and arrangement contracts.**
The Google TV screen spent about 180 dp on chrome — a Back button, a
`headlineMedium` title, a title line, a status line and a seven-item
`FlowRow` carrying a 260 dp search field — before any content, and then drew
the guide with the phone's single `LiveTvGridMetrics`: 160 dp slots and 56 dp
rows, which are 320 and 112 px on a television. Six channels filled the
screen and the grid showed barely an hour.

Live TV now sizes itself through one explicit `LiveTvTypography` scale on
television and spends one toolbar row of 28 dp minimum — a fixed 24 dp clipped
its own labels as soon as the television's font scale moved. `LiveTvGridMetrics.forTelevision`
derives the slot width from the width the grid actually got, so two hours fit
any panel; Earlier / Now / Later are chips in the grid header, and the search
field is a dialog the viewer asks for. On now is a 310 dp column beside a
picture that is a focus target — Select on it is Fullscreen, so "Return to
live" left the toolbar. The Layout menu offers Preview and Over picture, and a
stored `channel_browser` renders as Preview.

On the phone the picture is full-bleed with its chips and its
picture-in-picture and fullscreen actions on it, followed by one 56 dp caption
and one 48 dp toolbar. `LiveTvNowBar`, the filter row and the always-visible
text field are gone, so four to five channel rows are visible while a channel
plays instead of two.

Nothing about playback, the lease, the input policy or enablement changed.
Android versionCode 89.
