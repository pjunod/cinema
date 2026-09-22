# Resume never plays: a rolling copy session at a non-zero offset never publishes

**Status:** open; reproduced and localised to the web client's media
attachment, the progress ratchet fixed in PR #438, the remaining startup
failure not yet fixed ·
**Written:** 2026-09-22 ·
**Evidence base:** m6 `v0.3.0-3135-g9deb58a2e`, live reproduction from the
shipped web client

Companion to
[SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX](SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md)
— which this document partly exonerates — and to the
[playback reference](../PLAYBACK.md).

## 1. The symptom, and the asymmetry that names it

The item page offers "Resume 30:32", the client sends `start_seconds: 1832.0`,
and playback begins at zero — or does not begin at all. Starting the same file
from the beginning works. Resume is the only broken case, which is why it
presents as "everything starts over".

Nothing is wrong with the watch state or the clients. The position is stored,
`GET /items/:id` returns it, every client renders its Resume affordance from
it, and the play request carries it. ffmpeg is even given the right seek.

## 2. What the node did

Reproduced from the shipped web client against m6 on file 6455 at 30:32, with
the client's progress beats blocked so nothing was destroyed:

```
WARN  VOD prerequisite unavailable; using temporary live-HLS recovery
      file_id=6455 refusal="vod_index_pending"
INFO  copy-video HLS ffmpeg args: … -noaccurate_seek -ss 1832.000
      -readrate_initial_burst 90.0 -readrate 2.00 … mode="segmenter"
INFO  started actor-owned prepublication copy session … producer_attempt=1
WARN  client[Chrome] hls_manifest_dispatch … manifest request 1 runway=0.0s
      (then 2, 3, 4 over the next 24 s)
INFO  validated copy init before first media handoff … description_count=0
WARN  transcode ffmpeg process ended … encoder="copy" elapsed_s=38
ERROR copy segmenter reader failed: writing a final segment: the copy session
      was retired while waiting for scratch capacity
      … counts=copy segmenter: segments 21 · clean cuts 19 · fragments 57
WARN  client[Chrome] stall: The stream playlist could not be loaded.
      [The playlist did not load before the startup deadline.]
```

21 segments — about 126 s of media — in 38 s of wall time, against a 48 s
first-publication runway. The client never received a parseable playlist.

## 3. Which files are affected, and why it started when it did

`create_session_inner` (`transcode.rs:19404`) tries the immutable VOD path
first and falls back to the rolling live-HLS recovery engine on a typed
`vod_index_pending` refusal. A file with a complete cluster fragment index is
served the whole title from zero and the client seeks into it, which works. A
file whose index is still pending gets the rolling engine, which trims the
stream to the seek — and that is the broken path.

4151 of 6201 files were indexed at the time of writing. Files cross between the
two paths as the backfill runs, so the affected set moves: file 6640 served
rolling at 22:17 and 00:33 and VOD at 01:03 the same night, after its first
artifact was built at 23:30. `637ec781` (2026-09-19, "prove MKV duration from
packet timeline") unblocked index completion for a cohort of MKVs that
previously always failed `index_completion_unverified`, which is what put the
backfill — and this failure — in motion around the reported date.

## 4. The scratch line is a fence, not a budget refusal

`the copy session was retired while waiting for scratch capacity` is
`copyseg.rs:377-382`, reached on `grants.fenced()`. A genuine budget refusal
takes the deadline exit at `:355-367` and says `rolling_insufficient_capacity:
… the global budget did not free any in 120s`, which does not appear. The fence
is set only by `begin_retirement`/`begin_release`, and retirement kills the
child first (`transcode.rs:4186-4204`) — which is what makes `finish()` run at
all. The message therefore describes the end of the session, not its cause.

The arithmetic says the same. `rolling_startup_bytes` for the 11 Mb/s 1080p
source: `gate_ms = max(48 000, 12 000)`, `startup_ms = 64 000`, media
`(11e6/8)·64·3.0 = 264 000 000`, envelope `max((11e6/8)·5, 64 MiB) =
67 108 864`, grant **331 108 864 B ≈ 315.8 MiB** against an 8 GiB budget — 25
such sessions fit. The reservation is byte-identical for a 0-start and an
1832-start: the whole scratch subsystem is offset-blind. The admission-refusal
line that would prove a full budget (`transcode.rs:27386`, carrying
`holders = …`) never appeared.

Raising `hls_scratch_max_bytes` does not work around this — `authorize_write`
refuses a fenced entry before it consults the cap
(`scratch_ledger.rs:601-603`) — and
[SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX](SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md)
§ forbids raising it to mask a defect.

## 5. The ratchet: one failed resume destroys the resume point

While the player waits for a playlist that never arrives it keeps beating
progress. `reportProgress` computed `bookOffset + (offset + currentTime)·1000`;
with the element at `readyState 0` and a VOD or direct timeline's `offset` of
0, that beat is **position 0**, posted every 5 s and accepted. Six of them in
one 26-second failed startup in the reproduction. In the wild it took one title
from 33:17 to 5 seconds and another to 0 — so the damage outlives the session
that caused it, and a viewer sees "resume is broken for everything" long after.

Fixed in PR #438: a zero beat requires a witness — the current attachment
having reached a timeline — and a positioned beat requires none. The witness is
scoped to the attachment because a stall retry and a quality reopen both reuse
the player object while resetting the element and zeroing `offset`.

## 6. The playlist does publish. The web client never opens its MediaSource.

The section this replaces assumed the server never published. A second,
instrumented reproduction disproved that. Session
`6a49150d-…` for file 6629 at 8:41, read off the node while the client sat
there:

```
t=03:35:39  http=200 bytes=406
#EXTM3U / #EXT-X-VERSION:7 / #EXT-X-TARGETDURATION:16 / #EXT-X-MEDIA-SEQUENCE:0
#EXT-X-MAP:URI="init.mp4" / #EXT-X-START:TIME-OFFSET=0
#EXTINF:2.833000, seg00000.m4s / #EXTINF:6.584000, seg00001.m4s / …
t=03:35:56  http=200 bytes=437      ← still growing
```

A valid, growing rolling playlist. The client fetched it four times — and
**never requested `init.mp4` or a single segment.** hls.js state at that
moment:

```
levels[0].details : live, startSN 0, endSN 9, 10 fragments, totalduration 61.1 s
streamController  : state IDLE, startPosition -1, lastCurrentTime -1
currentLevel      : -1        started: true        media: the element
bufferController.mediaSource.readyState : "closed"    sourceBuffers: 0
```

`URL.createObjectURL` and the `HTMLMediaElement.src` setter were instrumented
before the attach. Exactly one MediaSource is created, by hls.js
`onMediaAttaching`, and assigned to the element — no second blob, no race, one
attachment (`_mediaAttachmentOrdinal: 1`), one session. **The MediaSource is
created and assigned and then never reaches `open`,** so no SourceBuffer is
ever created, so the stream controller has nowhere to put media and stays
IDLE. `hls.startLoad()` and `hls.resumeBuffering()` called by hand change
nothing; a subsequent `video.play()` returns
`AbortError: The play() request was interrupted by a call to pause()` and the
controller goes to `STOPPED`.

`attachHls` calls `pausePlaybackInternally(video)` before attaching, and
`handlePlaybackTransportEvent` turns a pause into `wantsPlayback = false` plus
`pauseHlsStartup(p)` unless the internal-pause token bookkeeping cancels it.
That is the shape the evidence points at, and it is where to look next.

**Caveat, stated because it changes what the evidence proves:** the Resume
button was clicked programmatically, which is not a user activation. The
element was muted, so autoplay was permitted and `paused` did go false, but the
pause/play interleaving under a synthetic click is not identical to a real one.
The `closed` MediaSource, the zero fragment requests and the IDLE controller are
solid; the exact trigger needs one reproduction with a real click and the
console attached.

Server-side arithmetic for completeness: first publication needs
`end_list || end_ms >= budget.desired_end_ms` (`transcode.rs:7349`) with
`desired_end_ms = consumed_end_ms + 48 000` (`:6548`) and
`consumed_end_ms = consumed_absolute_ms − media_origin_ms` (`:6543`). That
conversion is offset-correct, and the first reproduction produced ~126 s of
media in 38 s, so the budget was not the obstacle there either.

## 7. Two defects found in the PR #389 scratch code

Neither caused this incident; both are wrong on their own terms.

**A — the grant-wait deadline is inverted.** `copyseg.rs:348-350`:

```rust
// Only a session that has never published can wait forever for
// nothing, so only that one gets a deadline.
let deadline = self.started.then(|| Instant::now() + GRANT_WAIT_BUDGET);
```

`started` means the playlist is already on disk (`:262-265`, set at `:431`),
so the deadline lands on the session that **has** published and the
never-published session waits forever — the inverse of the comment, of
`GRANT_WAIT_BUDGET`'s doc comment, and of the requirement never to hold forever
waiting for a client that has no playlist to drain. Under budget pressure it
turns a classified 503 into an indefinite hang.

**B — retirement fences the writers it then waits for.** `begin_retirement`
(`scratch_ledger.rs:396-415`) sets `writers_fenced` and returns a barrier for
the already-registered writers, while `authorize_write` refuses every fenced
allocation (`:601-603`). A copy writer's remaining work is to write the final
segment and `ENDLIST`, so it can only fail: the tail of every killed copy
session is lost, and every retirement is reported in scratch-capacity language.

**C — stale comment**, `transcode.rs:1800-1809`: a pre-publication session does
now contribute to `global_live_bytes` (`global_flow_bytes`, `:27555`).
