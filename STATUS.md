# Status — what the agent is working on and where it stands

**Updated:** 2026-09-28 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## The durable queue refused every job for a day: retention cap counted finished work

**Branch `fix/durable-queue-retention-pressure`, PR __PR__; not yet deployed — the GPT deploy/verify prompt is in the project doc.**
Paul reported 2026-09-28 that Content analysis was not advancing and the page
showed `pipeline version unavailable`. Those rows are a side effect of today's
image builds flipping jellyfin-ffmpeg 8.1.2 → 8.1.3 → 8.1.2 (the Dockerfile
installs `jellyfin-ffmpeg8` unpinned; each deploy under the other digest fails
the requests queued under the first). The stall itself was the new durable
queue: `background_jobs` had exactly 10,000 rows, 8,890 of them terminal
(6,219 from Saturday's embedding backfill), admission refuses at 10,000
regardless of state, and terminal rows were only retired after seven days —
so since 2026-09-27 ~09:00 UTC every enqueue answered `queue_full`. Fragment
builds bounced as `queue_full_or_busy` (fence 100–250 on the same rows, zero
fragment jobs queued or running, last `ready` 2026-09-27 05:11 UTC), library
scans logged `QueueFull` 163× per library, subtitle extraction could not admit
new demand. It would not have self-healed before 2026-10-04, and the receipt
table (11,947 of 16,384) would have closed it again days later.
Fix: the same family of rule the attempt table already had (that one fires one
page under its cap; these fire eight), applied to retained rows and internal
receipts — upkeep compacts the oldest terminal details once the table holds
≥ 8,976 rows, and the oldest terminal *internal* receipts once the receipt
table holds ≥ 15,360 (never user-scoped, identity-retaining — which is every
fragment interest — or of an active job), a page per pass. Internal producers
re-derive demand from their domain tables (embeddings from
`background_embeddings`, subtitles from `analysis_requests`), so a compacted
internal receipt does not re-run finished work. Shipped as replicated schema
**v63** / SQLite **v85** (a drop-and-recreate of the maintenance trigger, same
shape as v62). Two SQLite tests and one three-voter contract test drive each
cap through refusal → one page → reopened admission → receipt survival →
convergence (the replicated one from the exact v62 predecessor), and a fourth
pins the SQL literals to the constants and the migration's trigger to the
shared schema's.
Evidence and the ffmpeg-flip analysis:
[DURABLE-WORK-QUEUE-STATUS.md](docs/cluster/DURABLE-WORK-QUEUE-STATUS.md).
**Operator follow-ups after deploy:** press **Retry this page** on the
Attention filter for the 228 `pipeline version unavailable` rows that carry
nynuc's current digest (they are tombstones until reopened); pin
`jellyfin-ffmpeg8` in the Dockerfile so a rebuild cannot change the engine
digest (separate issue).

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

## P-03: the regression ledger stops growing, and releases get a weekly cadence

**Merged by #489 (`995b60f3e`); phase B switched on 2026-09-25 by branch
`ci/p03-enforce` with Paul's approval** — the boundary is `448e803da`, and a
corrective pull request now needs a resolving `Regression-Test:` line.
Executes [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
under Paul's two 2026-09-23 decisions. Built: the `Regression-Test:` field
(checked before merge in the tree the merge will produce), the landing-commit
audit that replaces new `validation/regressions.d/` fragments past a boundary
commit, `scripts/release-cut`, and a Monday release-readiness run that tags a
merged release only from a green gate. The boundary is set (phase B); the
first weekly tag still waits for `publish_main` to have a trigger (P-01). This page is now the
newest sections plus an index; older sections moved verbatim into each
folder's `STATUS-HISTORY.md`. The execution log in the plan is the record.

## PGS subtitles stopped blocking the start path

Three pieces, from one report: *Bad Boys: Ride or Die* would not play on the
TCL tablet on 2026-09-21, and the overlay the viewer saw was the least
interesting of three failures that night. Diagnosis, measurements and the plan:
[docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md),
reviewed by Fable (APPROVE WITH CHANGES, folded in).

The measurement that explains all of it: file 5208 is a **79.5 GB** remux whose
PGS subtitle packets are interleaved across 116 minutes, so extracting one
track read the whole film — **402 s to produce 18,866 bytes**, which is just
the array at 198 MB/s.

- **#437** (`5c48ed5ab`, merged) — the cluster replacement gate is reclaimed on
  evidence, not on a clock: a hold that declared itself abandoned, or one past
  a 120 s ceiling, loses its player. Fixed the refusal the viewer quoted.
- **#445** (`5c605768`, merged) — a session start no longer awaits a full-film
  demux, and a pending sidecar is a named `startup_timeout` rather than a
  codeless 503 no client retries. Bounded for the start path **only**: offline
  restore, the VTT endpoint and package production keep their unbounded wait,
  because for them a slow success must stay a success.
- **#447** (`883cf4d42`, merged) — the PGS overlay is offered per caller rather
  than per node. `/decision` narrows on the server switch **and** the caller's
  own `subtitle_overlays` claim, so a client that cannot paint a bitmap is not
  offered a PGS default and is told the truth that selecting one burns the
  video. **This is what makes the gate safe to flip.** Before it, turning the
  gate on would have burned web direct-plays: the server stamped the PGS track
  `default`, and the web applies the server's default 400 ms after open, which
  for a bitmap track means a burn — or, on HDR, a degraded notice instead of a
  subtitle. Two deliberate limits, both from the adversarial review: **no caps
  document at all** falls back to the switch alone, because the legacy query is
  a mixed-fleet path both native clients reach on any 400/404/405 and reading
  silence as a refusal would send a capable client off to re-encode a whole
  film; and the `overlay` field on the track keeps the server's own answer,
  because it describes what this process can deliver rather than what this
  caller can paint. Item detail keeps answering `false`: it has no capabilities
  document, and the web's detail surface does not narrow the default by a
  renderer. `tests/validation/test_caps_wire_conformance.py` pins the field
  name across all four ports, because `DeviceCaps` has no
  `deny_unknown_fields`, so a misspelled claim is silently dropped rather than
  refused — costing a needless burn and, on an HDR source, the grade with it.

- **#453** (`1d21b184e`, merged) — overlay seeks and failures, on both native
  clients against one shared fixture (`tests/playback/pgs-overlay-cases.json`).
  Four bugs fixed: Apple's 1 s tick cancelled its own in-flight load, so any PNG
  slower than a second never arrived; Apple re-raised a notice every second
  after one failed window; Android left a finished cue on screen after a seek
  into the refresh margin; Android read every refused manifest as "empty". The
  server now answers a failed preparation with a typed
  `pgs_overlay_prepare_failed` instead of a 503 both clients kept polling for
  ten minutes; only capacity is a 503. Apple build 179, Android 119.
- **Fix C — every PGS track rides the fragment-index pass**, which already reads
  the whole file. Designed in #451 (§6 of the RCA, three adversarial rounds with
  ffmpeg experiments, each overturning something load-bearing), built as three
  PRs:
  - **#456** (`8962c0c66`, merged) — the store at
    `<cache>/runtime/subtitle-source-v1/` and its two readers. The overlay uses
    a stored `.sup` instead of demuxing; the burn path derives its `.mks` from
    it in a fraction of a second (with `-copyts` and **no** `-start_at_zero`,
    which would have moved every cue early) and answers "nothing to burn"
    without reading 79.5 GB for a track with no cues. MPEG-TS sources keep
    today's extraction.
  - **#466** (`cf5666876`, merged; first merged as #460, see below) — the
    producer: one `tee` output after the index's `pipe:1` with a `sup` slave
    and a `framecrc` companion per track, `onfail=ignore`, and a mandatory
    `null` sentinel, so the worst case is no subtitles and never no index; the
    index's argv, bytes, cache key and retry rules are unchanged, measured.
    Per-track verdicts from byte arithmetic, a persistent latch, a behavioural
    startup self-test, a local-disk and free-space gate, and one Developer
    switch (`subtitles.stored_sources`) that turns off producer and readers.
  - **#463** (`9236de83a`, merged) — attribution: the analysis row says the pass is also
    keeping N PGS tracks, by title, with bytes and a link to the switch, on both
    indexers; a Maintenance card shows the store, what is running and why.

**Forge anomaly, 2026-09-23 13:48 UTC.** Forgejo reported #460 merged as
`6a9a6a5a2`, but the server-side reflog shows `refs/heads/main` moved to it and
was set back to `8962c0c66` one second later by an internal "update by push".
Main never kept the merge. The same head was re-landed as #466. #460 had been
retargeted from its stacked base to `main` just before merging, which is the one
thing it did differently from every merge that stuck — worth avoiding
(merge stacks bottom-up and open the upper PR against `main` fresh) until the
cause is known.

**Deployed to all four nodes 2026-09-23 19:28 UTC** (`v0.3.0-3626-gfad591a46`, each node's own build report; the ride-along self-test passed on ffmpeg 8.1.2-Jellyfin). The first `deploy.yml` run called nuc4 and m6 "already at" that build while their containers were a day old, because their checkouts had moved without a rebuild; `-e force=true` rebuilt them, and `pjunod/ansible#4` now rebuilds whenever the running image's revision label differs from the checkout. **The mobile apps and the overlay switch are not done**: devices need Apple 179 / Android 119, then
the overlay's
enablement check — two devices, one Android and one Apple, one playing a DV
title and one an HDR10 title, a seek each way — before the gate
(`subtitles.pgs_overlay`) is turned on, and the Developer and Maintenance page
layout goldens, which need `scripts/ui-baseline --self-host --update` on a
machine with Playwright (the golden is also stale on `main` for unrelated
routes).

## Older efforts — where each one now lives

Sections older than those above moved verbatim on 2026-09-24, 2026-09-25, 2026-09-26 and 2026-09-27
into the status history of their subject folder. One row per section, newest first.

| First recorded | Effort | Now in |
|---|---|---|
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
