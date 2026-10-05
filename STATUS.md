# Status — what the agent is working on and where it stands

**Updated:** 2026-10-04 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Live TV station logos on Apple TV, iOS and Android

**Branch `fix/live-tv-station-logos-native`, issue
[#815](http://192.168.4.7:3000/noirr/plurx/issues/815).** Paul reported
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

## Apple TV: new HEVC WEB-DLs refused with 503 — ffmpeg 8 repeats the sample description

**Branch `fix/ffmpeg8-repeated-hevc-descriptions`.** Paul reported
2026-10-04 that *Taylor Tomlinson: Prodigal Daughter* (file 91) and
*Tom Segura: Teacher* (file 106) error out on the Apple TV
(`NSURLErrorDomain -1008`, underlying HTTP 503). Neither has a VOD index, so
both play through live-HLS copy, and every attempt ended in `copy segmenter
rejected the stream shape: … this stsd has 2 HEVC sample entries`; the
legacy-muxer fallback wrote the same init and `master.m3u8` refused it (503).
Cause: jellyfin-ffmpeg 8's MOV muxer appends a sample description whenever a
packet carries new-extradata side data that differs byte-wise from the current
one, and `extract_extradata` (needed for Matroska sources with a bare 23-byte
`hvcC`) attaches it on every parameter-set keyframe. Reproduced on six of six
2160p sources on nynuc: two descriptions identical outside `hvcC`, the same
`hvcC` header and the same VPS/SPS/PPS modulo a PPS trailing zero (only SEI
arrays differ), no `tfhd` sample-description index, `trex` default 1. The fix
collapses such provably decoder-equivalent descriptions to the first, in
`fmp4::collapse_equivalent_hevc_sample_entries`, called by both served-init
builders (`promote_from`, `promote_hevc_parameter_sets`), by the copy
segmenter on the init, and by VOD's `InitIdentity` before it digests a
generation's muxer init; descriptions that differ in anything else keep the
typed `MultipleHevcSampleEntries` refusal. The kept `hvcC` is reduced to its
VPS/SPS/PPS arrays: ffmpeg 8's extraction copies the leading keyframe's SEI
(decoded-picture hashes included), so without that every VOD generation
started at a different keyframe would have refused as muxer drift once these
titles get an index. Verified on captures: a real 8.1.3 copy pipe publishes a
one-description init end to end, and four generations of file 91 started at
0/300/900/1800 s collapse to byte-identical inits (opt-in tests).
Not addressed here: file 91's fragment index also fails on a separate
`Non-monotonic DTS` muxer error, and file 6710's `410 Gone` in the same log
window is unrelated.

## Content analysis stopped: the queue's receipt bound is the next cliff after #608

**Branch `fix/queue-receipt-pressure`, [PR #610](http://192.168.4.7:3000/noirr/plurx/pulls/610), CI running; the retained-row half is
already on `main` as [#608](http://192.168.4.7:3000/noirr/plurx/pulls/608)
(`29358ce5`), not yet deployed — the GPT deploy/verify prompt is in the
project doc.** Paul reported 2026-09-28 that Content analysis was not
advancing with `pipeline version unavailable` on every row. Two things, and
the one on the page was the smaller. The stall: `background_jobs` held
exactly 10,000 rows (8,890 finished, 6,219 from Saturday's embedding
backfill), admission refuses at 10,000 regardless of state, finished rows
retired only after seven days — every enqueue since 2026-09-27 ~09:00 UTC
answered `queue_full`; fragment requests bounced as `queue_full_or_busy`
(fences of 100–250 on the same rows, zero fragment jobs queued or running,
last `ready` 2026-09-27 05:11 UTC). Two sessions diagnosed it in parallel;
#608 landed first with enqueue-path eviction plus upkeep from 9,000
(replicated v63 / SQLite v85), and this session's
[#605](http://192.168.4.7:3000/noirr/plurx/pulls/605) was closed as
superseded. What #608 leaves: `background_job_waiters` stood at 11,947 of
16,384 with 10,775 succeeded receipts that only expire after seven days, so
the same refusal was days away one table over. This branch compacts the
oldest settled *internal* receipts a page per upkeep pass from 15,360
(never user-scoped, identity-retaining — every fragment interest — or of an
active job), as replicated **v64** / SQLite **v86**, generated from v63's
upkeep trigger and pinned to it by test.
The rows on the page: today's image builds flipped jellyfin-ffmpeg
8.1.2 → 8.1.3 → 8.1.2 (the Dockerfile installs `jellyfin-ffmpeg8` unpinned),
which changes the fragment-index engine digest, and each deploy under the
other digest failed everything queued under the first — 278 rows, 228 of
them tombstones on nynuc until **Retry this page** is pressed after the
deploy. Filed as [#604](http://192.168.4.7:3000/noirr/plurx/issues/604).

## Android Live TV fullscreen on tablets: every box gets its own player view

**[PR #606](http://192.168.4.7:3000/noirr/plurx/pulls/606), merged 2026-09-28 as `cfa61cb3e`; Android 138 installed on the Lenovo TB322FC, Google TV Streamer, Pixel 11 Pro XL, Pixel 10 Pro Fold and razr ultra 2025; VERIFIED on the TB322FC and the Streamer. Still owed: the TCL 9445X and the Xiaomi 25019PNF3C, which were not reachable over wireless adb.**
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
`history-audit` ok. Open as [PR #606](http://192.168.4.7:3000/noirr/plurx/pulls/606).
The adversarial review found no blocker and confirmed the release/bind order
against Media3 1.10.1 and Compose's apply order; it added PlayerView's API 34
SurfaceView sync workaround for the boxes that resize in place, an honest
KDoc (one black frame per swap, no PiP host on the wide layout) and a
tighter source pin. The fast lane went green (Rust gate included) and the
merge carries the `Regression-Test:` trailer. Hardware, driven over wireless
adb from nuc3 with screenshots and `uiautomator` bounds: on the TB322FC the
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

## Live TV said "all slots are busy" with every tuner idle

**[PR #585](http://192.168.4.7:3000/noirr/plurx/pulls/585), merged 2026-09-28 as `2694db665`; not yet deployed — the GPT deploy/verify prompt is in the project doc.**
Paul reported 2026-09-27 (web and iOS) that channels intermittently refuse
with *All Live TV slots are busy*; the FLEX 4K had four idle tuners each
time. The owner's own log named the cause — `tuner_capacity` with
`cause=transcode capacity is temporarily unavailable: background encoding
did not yield within 5.0s` — and the RCA
([LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md](docs/streaming/LIVE-TV-SLOTS-BUSY-OVER-BACKGROUND-RCA.md))
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
beside it"); the RCA §3 argues why. Next (GPT): deploy the three voters then nuc3,
confirm the eight zombie rows retire on the first upkeep pass, tune 6.1
under backfill load twenty times from web and iPhone, screenshot the
`encoder_capacity` copy, and install Android 134 / Apple 196.
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Apple TV Live TV navigation: every press reversible, every control reachable

**[PR #589](http://192.168.4.7:3000/noirr/plurx/pulls/589) merged to `main`
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
[LIVE-TV-APPLE-TV-NAVIGATION.md](docs/features/LIVE-TV-APPLE-TV-NAVIGATION.md)
has the focus graph and the §1 table that doubles as the device checklist.
Verified: `make apple-test` (iOS 674 + tvOS 690, 0 failures) and the full
fast lane, including the promotion gate. Not verified: anything with a remote
in hand — the device pass is `~/Downloads/kit 2/APPLETV-LIVE-TV-NAVIGATION-PHYSICAL-VERIFICATION-PROMPT.md`.

## Silo comparison: two implementation plans and one device census, no code

**Branch `docs/silo-comparison-plans`, [PR #563](http://192.168.4.7:3000/noirr/plurx/pulls/563), draft; not merged, nothing deployed.**
Paul asked 2026-09-26 how plurx compares with Silo (github.com/Silo-Server),
then for implementation docs on the two things worth taking and a census for
the third. Both plans went through an adversarial agent review that pulled
hyper's source and ran ffmpeg to check them; both came back *reject as
written* and were rewritten — the write-stall guard now ships off by default
because the web and Android progressive paths do not resume after a mid-body
close, and the DV probe changed from "broken → transcode" to "broken → strip
by unit type" once the review showed the failing unit is a malformed SEI,
not the RPU. Round two: approve with changes, applied.
[MEDIA-WRITE-STALL-GUARD.md](docs/streaming/MEDIA-WRITE-STALL-GUARD.md) ·
[DV-STRIP-TRIAL-PROBE.md](docs/streaming/DV-STRIP-TRIAL-PROBE.md) ·
[ANDROID-DV-LEVEL-CENSUS-PROMPT.md](docs/clients/ANDROID-DV-LEVEL-CENSUS-PROMPT.md).
Next: Paul's call on the DV plan's Decision 1 and the stall guard's §8; the
DV plan's M0 evidence step needs the media.

## P-02 M3: a priority class for every child process

**Branch `plan/P-02-m3`, draft pull request; not merged, nothing deployed.**
Paul's go of 2026-09-25: every child starts through one launcher, playback,
VOD and Live TV as realtime (nice 5) and scans, probes and extraction as
background (nice 15), with Activity → Processes and a pidfd-safe Stop for
admins, and `/metrics` counts children by class. A source census and
clippy's `disallowed-methods` keep every spawn on the launcher. After the
review (comment 4817) the class is the caller's: the probes and extractions
a session start or a viewer's `/subs` request waits on (held source probe,
decode-fact probes, the Profile 5 pixel proof, burn and text-track
extraction) run realtime, the same work from warm-ups, offline packages and
the pre-transcode pass background, and `/metrics` counts spawns by class and
purpose; each `spikes/` workspace has its own `clippy.toml`. Outstanding:
the realtime cadence measurement on a busy media host (post-merge), and M4
(unit hardening plus `OOMScoreAdjust=-500`), whose steps are written.
Record: §3.2.2 of [the P-02 plan](docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md).

## P-02: four parser fuzz targets, one of which found a way to abort plurxd; the release profile measured

**Branch `plan/P-02-2`, [PR #510](http://192.168.4.7:3000/noirr/plurx/pulls/510), merged 2026-09-25; not yet deployed.**
The second pass of
[SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](docs/ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md)
took the buildable remainder that needs no lab host and no decision of
Paul's. **M8:** the four fuzz targets of §3.6 (`fmp4_reader`, `rpu_rewrite`,
`nfo_parse`, `epub_facts`) in their own package `fuzz/parsers/`, each with a
generated seed corpus (`scripts/fuzz-seeds`), a nightly `parser-fuzz`
matrix job on the PGS campaign's budget, and `scripts/fuzz-campaign` writing
executions and corpus growth into each job's summary so a target that stops
finding edges shows.

`rpu_rewrite` found, within its first ten thousand executions, one way for
a Dolby Vision RPU to take the daemon down and two to unwind the converting
task, all in `dolby_vision` 3.4.0: an allocation sized from an unbounded
ue(v) count (a ~25.8 GB `Vec::with_capacity` from eight changed bytes of the
real Profile 7 fixture, which aborts the process), an `unimplemented!()` two
bits from any valid RPU, and an `unreachable!()` on any level 8/9/10 block
with an unlisted length. The parser runs inside `plurxd` on every sample of
a converted disc remux. The crate is now vendored under
`vendor/dolby_vision` with refusals in place of those
([PLURX-PATCH.md](vendor/dolby_vision/PLURX-PATCH.md); five patches after
the adversarial review), its bit reader `bitvec_helpers` beside it for two
Exp-Golomb overflows, `dvconvert` refuses any RPU over 64 KiB before
parsing, each fuzz input is a fixture with a test in `dvconvert`, and
refusals report the parse error's whole chain rather than "CM v4.0".

**M6's release-profile half, measured on nuc3 and not shipped:** PR 1 as
written (`debug = "line-tables-only"`, `strip = "none"`) makes `plurxd` a
420 MiB binary (+427 %) for a fully symbolicated backtrace; `strip =
"debuginfo"` gives named frames without lines at +36 % (+11.5 % gzipped);
packed split debuginfo is 230 MiB plus a 177 MiB `.dwp`. `plurxd
diagnostic-panic` (hidden) is the check; `Cargo.toml` keeps main's profile
and **which one ships is Paul's** (plan §7 Q6). M3 stays blocked on Paul's
spawn-seam decision, M4 on the lab1 matrix. Board row P-02 records it.
## Watch view: menus and Playback info escape the picture; the picture goes under the header

**Branch `fix/watch-popovers-escape-picture`, pull request open as a draft;
not merged, nothing deployed.** Paul's report of 2026-09-24 on the new web
watch view: the Playback info readout ran past the bottom of the picture and
was cut off there, the subtitle menu ran past its top so half of it could not
be chosen, and scrolling the page slid the picture over the main navigation.
One cause: the slot player's host was fixed at z-index 90 with a clip-path to
its own box. It now sits under the page's chrome (z-index 10) and clips
nothing; `positionMenu` / `positionStats` bound each popover to the viewport
minus the stuck chrome (`watchPopoverBounds`) and re-run on every scroll
frame. Record: [docs/clients/WATCH-VIEW-LAYOUT.md](docs/clients/WATCH-VIEW-LAYOUT.md)
(“The picture under the page; the menus and the readout over it”). Browser
acceptance extended with a thirty-track menu, the Diagnostics readout and a
scrolled-under-the-header check; passes on fine and coarse pointers.
Native clients untouched.

## Live TV: direct play first, 5.1 stays 5.1

**Branch `fix/live-tv-direct-play`, draft pull request; not merged, nothing
deployed.** Paul's three reports of 2026-09-24 — Live TV "transcodes no matter
what", audio is "unnecessarily downmixed to stereo", and every channel shows
the same aspect ratio — worked against the real lineup on the FLEX 4K
(59 ATSC 1.0 MPEG-2/AC-3 channels, 10 ATSC 3.0 HEVC/AC-4 channels).
Record: [docs/streaming/LIVE-TV-DIRECT-PLAY-AND-SURROUND.md](docs/streaming/LIVE-TV-DIRECT-PLAY-AND-SURROUND.md).

- **Transcoding:** every client sent `interlaced: false` and no client claimed
  `mpeg2video`, so all 59 ATSC 1.0 channels were encoded on every client. Android
  TV now claims its hardware MPEG-2 decoder and interlaced input (television UI
  mode only), so ATSC 1.0 is copied there; Apple and the web cannot decode MPEG-2
  in HLS and keep the encode — the one necessary case. On ATSC 3.0 the picture
  was already copied; the audio was converted from the fragile AC-4 track even
  though 157.x carries an **AC-3 5.1 simulcast in the same programme**. The
  owner now probes every audio stream and selects the track the player can copy
  (`LiveDeliveryPlan.audio_track`), and the AC-3-in-fMP4 rule switches the
  container to MPEG-TS when the player claims that pair instead of converting.
- **Stereo:** all three clients claimed `aac: max_channels 2`, and an AC-4 track
  whose layout the probe had not seen was folded to stereo before a frame
  existed. The AAC claim is now the sink's real channel count (floored 2, capped
  5.1) on Apple, Android and the web, and the encode negotiates its layout with
  `aformat=channel_layouts=` under that ceiling — `-ac` is gone from live
  commands, so stereo stays stereo, 7.1.4 folds to 5.1, unknown is decided by
  the first frame.
- **Aspect ratio: not reproduced on the server.** Real 6.2 captures (704×480,
  SAR 40:33, 480i) through the exact QSV, VAAPI and x264 producer chains on nynuc
  all publish SAR 40:33 / DAR 16:9. `scripts/live-tv-hardware` now records the
  segment's SAR/DAR. Open until the client showing it is named.

**Deployment finding (same day):** the layout list led with `5.1(side)`, which
the AAC encoder signals as ADTS configuration 0 plus a PCE; browsers read that
as an audio track with no channels and never start, so Safari and Chrome sat
black on 157.1. Fixed: the list is `5.1|stereo|mono`, the harness asserts a
positive header channel count and gains `--surround`.

Verified: focused Rust tests (53) + clippy `-D warnings` + fmt on nuc3;
`make apple-test` on mba (1330 cases); Android unit tests in the pinned image
(745); `tests/web/live-tv.test.js`. Hardware, this branch's `plurxd` against
the real tuner from nuc3: 6.2 encodes to `704×480 SAR 40:33 DAR 16:9`;
157.1 with a copy envelope comes back **copy/copy** — HEVC + the AC-3 5.1
simulcast in MPEG-TS, no decoder running. One adversarial review pass, eight
findings folded (Android now reads the HDMI sink's PCM channel count, described
tracks never win, `und` is no language, the track is mapped by PID).

## Older efforts — where each one now lives

Sections older than those above moved verbatim on 2026-09-24, 2026-09-25, 2026-09-26, 2026-09-27, 2026-09-28 and 2026-10-04
into the status history of their subject folder. One row per section, newest first.

| First recorded | Effort | Now in |
|---|---|---|
| 2026-09-24 | P-03: the regression ledger stops growing, and releases get a weekly cadence | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-22 | PGS subtitles stopped blocking the start path | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-22 | The full Rust suite and the release build are clean again | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-22 | Resume stopped working on every client — reproduced, half fixed | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-21 | An abandoned replacement held its player's key — reported, diagnosed, fixed | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-20 | Architecture review, revision 3 — Astra's review merged | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-20 | Implementation plans for the architecture review, and one work board for every vendor | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-20 | Architecture review, revision 2 — after the adversarial assessment | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-20 | End-to-end architecture review — ten verified do-first items, ranked | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-19 | The web app is a tree, and the bytes are the same ones | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-17 | The twenty red tests, and the three live defects three of them were reporting | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-17 | A held source is compared on its media facts, not its reporter's schema | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-17 | `make install` is one command on every platform | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-16 | The web item page is back to its pre-#317 layout | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-13 | Live TV start, stall, and tvOS surface has landed on main | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Apple Live TV now distinguishes a stall and owns its fullscreen surface | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Android now lets each HLS playlist choose its live hold-back | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Live TV starts now use a stable one-second cadence | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | plurx records now, and tells you before a programme starts | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Apple's notice strip was a dead end, and two rows of the readiness card were false | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | Every class in the playback surface contract can now be drawn on Apple | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | The Android playback surface has no unreachable sources left | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | The web half of the playback surface contract is reachable, and four rulings are closed | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | `make web-check` is green, and `rust-gate` has actually been run | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-13 | The playback surface contract — built, both clients compiled, unverified on hardware | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | Open rulings — the playback surface contract | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | The player input fence was red on `main`, on two doc comments | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | Live TV: the empty guide and the "wait 90 seconds" refusal, diagnosed | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-12 | Live TV gets the web page's proportions on Apple TV and iPhone | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-12 | Live TV gets the web page's proportions on Google TV and Android phones | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-09 | Fresh Dolby Vision recovery converts, and tvOS can arm Live TV starts | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-08 | The transport-recovery campaign asserted a property the system does not have | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-08 | Phase 3 is buildable today, and the first answer to that question was wrong | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-08 | The axis receipt said do not widen, and the fleet widened anyway — correctly | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-08 | A prepared commit hands the viewer a session that is refused from its first request | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-08 | The third client speaks the protocol, on a platform the server will not use it for | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-07 | Apple viewers were paying for encoders nobody told them about | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-07 | The CI fleet filled up because the bound was behind a flag nobody set | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-07 | Settings put the operator on the login page, and the cause was a tombstone | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-05 | A deploy that refused itself over an unmaintainable pair | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-04 | Streaming reliability is under end-to-end review and repair | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | The fragment-index queue built nothing for three days | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | Source attestation was hashing whole films to answer a question about their inode | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | Dolby Vision Profile 7 on the web — what was actually left | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | The ✕ on an iPhone could not leave a film | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-03 | Activity's Now playing row, read as a card | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-03 | Nothing played on the web, and every fallback was terminal | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | The picker says which machine again | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-02 | The player input contract, reviewed and finished | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-02 | M7 M4 burn-join and current-main corrections | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | M7 R-M3 — one playback owns one subtitle window | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | Apple pacing-hold freeze — the hold that vetoed its own recovery | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | Everything a read-only cluster member could not do | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-01 | Artwork repair-fence claim flake (same CI job) | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-01 | Exact-count cluster assertions (the `v0.3.0` release blocker) | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
