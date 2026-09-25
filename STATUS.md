# Status — what the agent is working on and where it stands

**Updated:** 2026-09-24 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Live TV: direct play first, 5.1 stays 5.1

**Merged (#492, #501) and deployed to nynuc, m6, nuc4 and nuc3 as
`v0.3.0-3881-gf600d2823`; #509 (tablet fullscreen) is the client follow-up.**
Paul's three reports of 2026-09-24 — Live TV "transcodes no matter
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

**On the fleet:** a Chrome session on 157.1 against the deployed nynuc comes
back HEVC copied + AAC-LC 48 kHz with `channel_configuration = 6` in the
init segment's AudioSpecificConfig and `channelcount = 6` in `mp4a` — the
value that was 0 before #501. GPT's round-1 devices: the Google TV Streamer
direct-plays (Remux) 6.2, 6.1 and 157.1; the Lenovo TB322FC and TCL 9445X
tablets showed the fullscreen picture undersized in the top-left corner. That
is the client: the PlayerView is one View subtree moved between the inline
and fullscreen boxes with `movableContentOf`, and a moved View keeps its last
measured size. #509 (Android 125) forces a layout pass through the subtree
whenever its host box resizes and re-binds the SurfaceView to the player
after that pass. Still to read on hardware: the tablets after 125, a ~5 s
freeze on the Google TV Streamer at 38–43 s on 6.2's copy route (round 1,
once), and the Apple build (182, signing failed on the shipping Mac).

Verified: focused Rust tests (53) + clippy `-D warnings` + fmt on nuc3;
`make apple-test` on mba (1330 cases); Android unit tests in the pinned image
(745); `tests/web/live-tv.test.js`. Hardware, this branch's `plurxd` against
the real tuner from nuc3: 6.2 encodes to `704×480 SAR 40:33 DAR 16:9`;
157.1 with a copy envelope comes back **copy/copy** — HEVC + the AC-3 5.1
simulcast in MPEG-TS, no decoder running. One adversarial review pass, eight
findings folded (Android now reads the HDMI sink's PCM channel count, described
tracks never win, `und` is no language, the track is mapped by PID).

## P-03: the regression ledger stops growing, and releases get a weekly cadence

**Branch `plan/P-03`, draft pull request; not merged, nothing deployed.**
Executes [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
under Paul's two 2026-09-23 decisions. Built: the `Regression-Test:` field
(checked before merge in the tree the merge will produce), the landing-commit
audit that replaces new `validation/regressions.d/` fragments past a boundary
commit, `scripts/release-cut`, and a Monday release-readiness run that tags a
merged release only from a green gate. Nothing is in force yet: the boundary
is set by a one-line follow-up once this lands, and the first weekly tag
waits for `publish_main` to have a trigger (P-01). This page is now the
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

## The full Rust suite and the release build are clean again

`PR #443`, branch `fix/red-suite-2026-09-22`. Nothing here changes runtime
behaviour except one allocation at daemon startup; nothing to deploy for it.

The fast lane (`make unit`) was green on `main`; the red was all in what only
`make test-full` builds, which no CI lane runs any more.

- **Release/Docker build warning.** `AvcCLocation.entry` and `.ancestors` in
  `plurx-core/src/fmp4.rs` are read only by the `fixtures` builders, so every
  release build warned they were never read. `expect(dead_code)` outside that
  cfg.
- **plurx-core lib aborted on a stack overflow** with `hiqlite-store` on, in two
  join tests, taking every later test in the binary with it.
  `select_daemon_store` awaited its join, reopen and activation branches
  inline, so its future carried all of them (9,984 bytes; 496 boxed). Branches
  are boxed now, and `select_daemon_store_future_stays_small` holds it under
  2 KiB.
- **14 hiqlite store contracts** failed in their fixtures: the downgrade
  helpers stopped at schema v34, so replaying v40/v42/v43/v44 collided with
  their own `ADD COLUMN`s, and v42's index over `video_identity` blocked the
  v27 rewind. One shared reversal list now walks back v44..v40. Two stale
  expectations were updated with it (the v42 index split; 46 to 52 import
  tables).
- **`live_tv_two_node`** (4 cases) needs to bind ports 80 and 5004, and fails
  by design on a host that cannot. With `cap_net_bind_service` on nuc3 all
  four pass. Not a code defect.

## Resume stopped working on every client — reproduced, half fixed

`PR #438`, branch `fix/resume-progress-zero-clobber`, **merged, NOT deployed**.
RCA: [docs/streaming/RESUME-ROLLING-PUBLICATION-RCA.md](docs/streaming/RESUME-ROLLING-PUBLICATION-RCA.md).

Reported 2026-09-22: "resume doesn't work for anything. it all just starts at
the beginning now. It's been that way at least a day or two." Apple TV, iOS and
web; the item page offers "Resume 13:04" and playback begins at zero.

The watch state and the clients are innocent. The position is stored, the API
returns it, the Resume affordance is drawn from it, the play request carries it
and ffmpeg is given the seek. What breaks is the **rolling live-HLS recovery
path**, taken whenever a file's cluster fragment index is still pending
(`vod_index_pending`); a file with a complete index is served the whole title
and resumes correctly. 4151 of 6201 files are indexed, and the backfill that
`637ec781` unblocked on 09-19 keeps moving files across that boundary — which
is why this arrived everywhere at once on the reported date.

Localised from the shipped web client with the node instrumented. The server
publishes a valid, growing playlist; the client fetches it and **never requests
`init.mp4` or a single segment**, because hls.js's MediaSource is created,
assigned to the element, and never reaches `open` — no SourceBuffer, stream
controller IDLE, then the startup deadline and a server-side retirement. The
`retired while waiting for scratch capacity` line that this chases is a fence
raised after the session is already dead, not a budget refusal; the reservation
is 315.8 MiB against an 8 GiB ledger and the subsystem is offset-blind.

What #438 fixes is the ratchet, not the startup: while the player waits,
`reportProgress` posts position 0 every five seconds over the saved resume
point — eight of them in one reproduction — so a single failed startup destroys
the resume position permanently. A zero beat now needs a witness, the current
attachment having reached a timeline; a positioned beat needs none, which keeps
a predecessor's close-time save intact.

Still open: why the MediaSource never opens (the evidence points at
`attachHls`'s internal pause reaching `handlePlaybackTransportEvent` as a
viewer pause and stopping hls startup, and wants one reproduction with a real
click); the inverted grant-wait deadline at `copyseg.rs:348`, which applies the
bound to the session that *has* published; retirement fencing the writers it
then waits for, which loses the final segment and `ENDLIST` of every killed
copy session and misreports every retirement as a capacity problem; and 30
titles carrying `watched = 1` below the threshold, which suppresses their
resume outright.

## An abandoned replacement held its player's key — reported, diagnosed, fixed

`PR #437`, branch `fix/replacement-gate-supersession`, **merged, NOT deployed**.

Reported from the Android client on the TCL tablet, 2026-09-21 ~18:50 ET,
playing *Bad Boys: Ride or Die*: `transcode capacity is temporarily
unavailable: another replacement for this player is still being committed`,
over a Retry button that could not clear it.

**Orphaned**, not a commit in flight. On m6 the cluster replacement gate for
that player was held by the detached cleanup of a request that had answered
the viewer 503 six seconds earlier, and nothing in the tree ages, expires or
force-releases that registry. What wedged the hold inside the start was a
402-second subtitle sidecar extraction awaited under the gate with no timeout.
Node evidence, anchors and the mechanism:
[docs/playback-control/REPLACEMENT-GATE-SUPERSESSION-RCA.md](docs/playback-control/REPLACEMENT-GATE-SUPERSESSION-RCA.md).

A key is now reclaimed only from a hold that can be proved not to need it —
one that has declared itself abandoned, or one past a 120 s ceiling — never on
the cooperative window, because the work under this gate routinely takes tens
of seconds and the client's retry ladder re-posts into it. The refusal itself
became a typed, `Retry-After`-carrying 503 (`transcode_capacity_pending`) so
the ladder all three clients already carry waits it out instead of showing a
terminal overlay quoting an internal sentence.

Still open, all in the RCA's §4: `ensure_burn_file` is still awaited under the
gate unbounded; `Drop for StartedSessionGuard` still releases only after a full
retirement rather than after the fence; and a hold wedged before it registers
anything still has nothing to fence. Neither client half has run on hardware.

## Implementation plans for the architecture review, and one work board for every vendor

**46 handoff plans** under `docs/{streaming,server,cluster,features,clients,ci}/`
(index rows in [docs/README.md](docs/README.md)), one per finding or per
shared mechanism, each executing named ids of
[the review](docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md) with the
adversarial assessment's dispositions as guardrails, the current code copied
into a contract section, and a runnable acceptance check per milestone.
The remaining twelve landed in a second pass the same day, so **all 46
board rows have a document**. The second pass corrected the review in
several places worth knowing before Astra reviews: the encoded-VOD master
already emits output geometry (only the rolling path does not) and the
hard-coded codec string is `avc1.640034`, not `.640028`; `publish_main` is
unreachable on `ci.yml`'s current triggers, so no `sha-` rollback image has
been produced automatically since 2026-09-10; half the clock-skew exchange
already exists (`x-plurx-cluster-time-ms`) and the peer-auth windows already
assume ≤5 s; there are 29 discarded store results, not 13; removing the
cryptr `s3` edge does not remove the second `reqwest`, and renaming the fork
is actively expensive because the patch stack substitutes by registry name;
`files_for_items` does not exist and there are no recorded Kodi fixtures;
there is no HTTP access log at all; and the review's "unreplicated SQLite
mode" is the one recovery boot, so ARCHITECTURE's 1-voter sentence is
incomplete rather than wrong.

**[docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md](docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md)
is the only shared status.** The plans will be executed by Claude, GPT and
OpenRouter sessions concurrently, so the board defines the claim protocol in
vendor-neutral terms: a claim is a draft PR that edits the row (the push is
the atomic step), every row and every commit carries the exact **model
identifier** and **session id** (`Agent-Model:` / `Agent-Session:` trailers),
statuses are a fixed vocabulary, stale rows are reclaimable after seven days
with a note, and every plan ends with an **Execution log** table the
executing session fills per milestone. Writers' corrections to the review
found while reading the code are recorded at the top of each plan (for
example: `live_tv.rs:7403` already pipes stderr; `bounded_process::output` is
the better primitive for scan probes than the one the review named; the guide
is cloned three times per DVR tick, not two). Astra reviews the plans next.

## Architecture review, revision 3 — Astra's review merged

**[docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md](docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
revised in place again.** Astra's independent review was written against the
first draft; revision 3 keeps revision 2's corrected remedies and merges
everything Astra added that the first draft had not found, each re-verified in
the tree: an unbounded, unkillable scan probe (`scan/probe.rs:190-215`, C12);
decode-fact lookups that hash three executable-sized inputs under a one-permit
gate before consulting the cache and fall back to catalogue facts on any error
(C13); item-detail badges that unpack whole fragment indexes and `stat` every
media path with no deadline (C14); telemetry that spawns a task and a
consistent settings read per event with no bounded queue (C15); DVR fan-out
that writes sinks sequentially with the lock outside the timeout (L10); HDR10
HEVC output limited to software and QSV by design (Q12); no native adaptive
quality and dormant stall-ticket plumbing (§3.8); and two policy tests red on
`main` — `web-policy.test.js:6007` (a stale call count) and
`web-control.test.js:3164` under Node 22 — both reproduced here (§4.8).
Astra's interlace experiment was re-run on this container's ffmpeg 6.1.1 with
identical counts: the current CPU chain emits 90/90 combed frames tagged
progressive. The seek-scratch reservation finding (C16) is a tracking item
for the existing repair. §9 records what was run and what was not. No code
changed.

## Older efforts — where each one now lives

Sections older than those above moved verbatim on 2026-09-24 into the
status history of their subject folder. One row per section, newest first.

| First recorded | Effort | Now in |
|---|---|---|
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
