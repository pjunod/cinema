# FFmpeg HLS write boundary — receipt

**Status:** built and reviewed, pending fast lane · **Written:** 2026-09-24 ·
**Completes:** R4 / unit C of the
[seek scratch RCA](../streaming/SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md) for
direct and transcoded FFmpeg output ·
**Earlier receipt:** [units A, B and the copy half of C](seek-scratch-reservations-receipt-2026-09-20.md)

The 2026-09-20 promotion left one writer outside the scratch grant: FFmpeg's
own `-f hls` muxer. Transcodes, and the copy sessions that use the muxer
instead of the segmenter, had no Rust hook before a write, so the only honest
reservation was the whole per-session ceiling (2 GiB + 64 MiB). This change
gives the muxer an owned write boundary and removes the last whole-ceiling
admission. It also closes a hole the earlier receipt did not name, and fixes
an inverted deadline in the copy writer.

## 1. Mechanism — the muxer uploads, the daemon writes

`-method PUT` makes FFmpeg's HLS muxer upload every object over HTTP instead
of opening the file itself. Each rolling session binds a loopback listener
(`crates/plurxd/src/scratch_put.rs`) and gives FFmpeg
`http://127.0.0.1:<port>/<token>/<lane>` as its output base. The argument
builders are unchanged apart from the write mode
(`plurx_core::transcode::hls_write_args`): a directory keeps `temp_file`, an
upload endpoint gets `-method PUT` and drops `temp_file`, and every other
muxer flag is identical.

The receiver reads each body one piece at a time and authorizes each piece
against the session's ledger allocation, through the same
`copyseg::WriteGrants` the segmenter uses, before it writes it. A refused
grant stops the receiver reading that socket. FFmpeg's unwritten bytes wait
in kernel socket buffers, not in the scratch directory, and the receiver asks
the flow controller for an evaluation immediately (`request_flow`), which
holds the producer through the existing suspend path.

This is the "owned output boundary" candidate from the earlier receipt. It
was rejected there because it seemed to require routing transcoded output
through the fragmented pipe and `copyseg`, which would change the media
pipeline. `-method PUT` provides the same boundary with the muxer unchanged.

| Property | How it holds |
|---|---|
| Objects are byte-identical to file output | The muxer is the same code with the same flags; only the I/O protocol differs. Proven with real FFmpeg for MPEG-TS transcode output and fMP4 copy output with an `EXT-X-MAP` init segment. |
| Absent-or-complete objects | The receiver writes `<name>.<n>.tmp` and renames it into place when the body ends, as `temp_file` did. A body that ends early is removed, never renamed. |
| A playlist never names a missing object | FFmpeg opens one connection per object and does not wait for replies, so arrival order is not publication order. A playlist that names an object not yet in place is held as the lane's pending version and renamed by the commit that completes it; nothing waits on a timer, so the final `ENDLIST` playlist cannot be dropped. An older playlist never replaces a newer one: an EVENT playlist only grows, so the rank is `(entries, ENDLIST)`. |
| A replaced attempt cannot overwrite its successor | Each producer attempt has its own lane. Lane 0 is the initial attempt, lanes 1 and 2 the transcode's colour-safe and software-decode retries, lane 1 the copy path's legacy retry. Lanes only move forward. Every retry path retires the reaped attempt's lane before it clears the directory and waits for that lane's uploads to finish refusing, which they do at once: a body waiting on the socket wakes when its lane is retired. Every lane below the writing one is refused for good, including a replaced attempt whose first upload reaches the receiver after its successor's. The lane switch, the lane check before a rename, the rename and its bookkeeping share one lock, and a replaced lane stops spending grants at its next piece. |
| A starved writer holds its producer | A writer waiting on a refused grant marks the allocation starved. The flow evaluation holds a starved allocation, and the manager's reaper runs that evaluation as soon as a writer starts or stops waiting instead of at its 15 s repair pass. Without this, FFmpeg blocked on the socket would trip its 10 s progress deadline. The copy segmenter's own wait uses the same mechanism. |
| A failed write stops publication | FFmpeg does not read replies, so a write the lane needed failing (disk error, or a session that could never publish waiting out its 120 s) is recorded on the lane, and the publication clock fails the session with it. A replaced lane's failure is discarded. |
| Retirement's writer barrier covers these writes | Every request registers a scratch writer before its first byte and holds it to the rename. A fenced allocation refuses new requests, and a request waiting for a grant wakes and gives up at the fence. |
| FFmpeg's exit is not the end of its writes | A finished upload can still be queued on the socket after the process exits. The completion probe drains the endpoint before it reads the playlist, leaving the read time inside the actor's deadline, so the ENDLIST check sees what file output would have had in place at reap. A loopback `connect` returns only once the kernel has queued the connection, so after exit the set is closed, and the drain accepts every queued one directly through a duplicate of the listening socket. An exited producer's remaining pieces are admitted without waiting on the budget: they are bounded by the socket buffers it filled, they are charged, and refusing them could only fail a finished title. |
| No foreign writes | Loopback only, a 128-bit random token per session compared in constant time, names restricted to the muxer's own object names (`.ts`, `.m4s`, `.mp4`, `.m3u8`, `.vtt`, no leading dot, no separators), PUT only, a 10 s limit on the request head, and conflicting body framing refused. The token is redacted from every logged argument list and every FFmpeg log line, and FFmpeg is started with an empty `http_proxy` so an inherited proxy never sees the uploads. |

* **Enforced by code:** every byte on the disk passed a grant first; the
  ledger arithmetic is the same that already bounds the segmenter.
* **Bounded but not by the ledger:** kernel socket memory while a writer is
  starved. It is at most the socket buffers of the connections FFmpeg opens
  before the hold suspends it, and the receiver requests that hold on the
  first refusal rather than waiting for the next scheduled evaluation.
* **Measurements, not bounds:** the startup allowance and the envelope,
  unchanged from the earlier receipt. A transcode sizes both from its own
  output rate, video plus audio.

## 2. What else changed

**The legacy copy writer was already under-reserved.** Copy sessions were
admitted on the smaller startup allowance whether or not the segmenter was
cutting them. A copy the segmenter cannot cut, a cluster takeover, and the
legacy retry a segmenter session keeps in reserve all ran FFmpeg's muxer with
no grant boundary, so their only bound was the flow-control envelope the
earlier receipt itself describes as a measurement. They now upload through
the same endpoint.

**The copy writer's grant wait was inverted.** `SessionDir::authorize_write`
armed its 120 s give-up for a session that *had* published and let a session
that never could wait forever, the reverse of its own comment. A published
session was failed after two minutes of an ordinary hold. The deadline now
applies only before the first playlist, and it counts polls rather than wall
time.

**A directory walk could forget landed bytes.** `ScratchLedger::observe_used`
zeroed the landed-since-last-walk figure on every walk, and the segment GC
called it with a derived figure that was never a walk. A write that landed
while a walk was running, or before a deletion, could be authorized again.
A walk now subsumes only what had landed before it began, the GC path moves
the measurement without touching landed bytes, and each piece is flushed
before it counts as landed. This also affected the copy segmenter.

**`RollingScratchSizing::SessionCeiling` is test-only.** No producer is
admitted with the whole ceiling any more; the variant remains so the
admission tests can model the historical reservation.

## 3. Decisions taken without Paul

* **No capability fallback.** The endpoint needs FFmpeg's `http` output
  protocol. Both engines in the published image have it (checked in the
  running `plurxd` container on `nynuc`: jellyfin-ffmpeg 8.1.2, the default
  engine, and distro FFmpeg 5.1 both list `http` under output protocols), and
  a fallback to file output would reintroduce the whole-ceiling path this
  removes. A build without network protocols fails
  every transcode start with "Protocol not found". The Developer tab states
  the requirement.
* **Windows gets the same boundary.** It is loopback TCP and ordinary file
  writes. It is built for Windows but has no native runtime receipt.
* **Direct to `main` as one pull request.** One task, one owner of every file
  it touches, so AGENTS.md's ordinary-change path applies rather than an
  effort branch.

## 4. Regression evidence

Pinned toolchain, `nuc3`:

```bash
rustup run 1.97.1 rustc --version        # 1.97.1 (8bab26f4f 2026-07-14)
cargo test -p plurxd --bin plurxd scratch_put_
cargo test -p plurxd --bin plurxd scratch_charge_
cargo test -p plurx-core --lib upload_output
```

| Test | Proves |
|---|---|
| `scratch_put::tests::scratch_put_ffmpeg_output_is_byte_identical_to_file_output` | Real FFmpeg, MPEG-TS transcode: the same objects, byte for byte, no temporaries left, final playlist in place when `drain` returns. |
| `scratch_put::tests::scratch_put_fmp4_copy_output_is_byte_identical_to_file_output` | Real FFmpeg, fMP4 copy with an `init-e3.mp4` map: the same objects, byte for byte. |
| `scratch_put::tests::scratch_put_a_refused_grant_writes_nothing_past_the_allowance` | A 5 MiB upload against a 2 MiB budget puts at most 2 MiB on the disk and is not committed; it completes once the budget grows. |
| `scratch_put::tests::scratch_put_a_playlist_waits_for_the_objects_it_names` | A playlist naming an in-progress segment is not renamed into place until the segment is; a stale version never replaces a newer one. |
| `scratch_put::tests::scratch_put_a_replaced_lane_cannot_overwrite_its_successor` | A late upload on a replaced lane is refused at commit and leaves no temporary. |
| `scratch_put::tests::scratch_put_an_upload_is_a_scratch_writer_until_it_settles` | An upload registers as a writer; the fence wakes a starved upload and the barrier settles; nothing is committed after the fence. |
| `scratch_put::tests::scratch_put_drain_covers_an_upload_still_queued_on_the_socket` | `drain` returns only after a finished upload is in place. |
| `scratch_put::tests::scratch_put_refuses_anything_that_is_not_an_hls_upload` | Wrong token, path escapes, foreign names, temporaries and non-PUT methods are refused and write nothing. |
| `scratch_put::tests::scratch_put_logs_never_carry_the_upload_token` | Logged argv carries no token. |
| `transcode::tests::scratch_charge_a_transcode_starts_small_and_writes_through_the_grant` | A real transcode through the manager is admitted at under a quarter of the ceiling, runs to `ENDLIST`, and every object its playlist names exists. |
| `copyseg::grant_wait_tests::scratch_charge_grant_wait_gives_up_only_before_first_publication` | The inverted deadline, both directions, bounded so the old code fails rather than hangs. |
| `copyseg::starvation_tests::scratch_charge_a_starved_writer_rings_the_hold_signal_on_both_edges` | Starvation counts every waiting writer and rings the manager when the first starts and the last stops. |
| `scratch_ledger::walk_tests::scratch_charge_a_walk_subsumes_only_what_landed_before_it` | A walk and an unlink without a walk never forget landed bytes. |
| `scratch_put::tests::scratch_put_a_retired_lane_is_refused_before_its_successor_writes` | Retiring a lane refuses its in-flight and later uploads before the retry's first upload arrives. |
| `scratch_put::tests::scratch_put_an_older_lane_never_takes_the_endpoint_back` | A replaced attempt whose first upload arrives after its successor's is refused, and a lane retired before any of its uploads arrived stays retired. |
| `scratch_put::tests::scratch_put_drain_admits_an_exited_producers_tail` | A starved tail is admitted, charged, once the producer has exited. |
| `scratch_put::tests::scratch_put_a_failed_write_is_reported_for_the_lane` | A failed write is recorded for the publication clock; a retired lane's is discarded. |
| `transcode::upload_output_tests::scratch_charge_upload_output_swaps_temp_file_for_put_and_nothing_else` | The write-mode switch changes nothing but `temp_file` → `-method PUT`. |

The production engine was checked separately. In the published image on
`nynuc`, jellyfin-ffmpeg 8.1.2 wrote the same MPEG-TS transcode and fMP4 copy
once to files and once with `-method PUT` to a loopback receiver: 6 of 6 and
5 of 5 objects byte-identical. The CI tests use the build host's FFmpeg 8.0.1.

The `scratch_put_` set was also run 24 times at 16 test threads with eight
copies running at once. That load found a lost-wakeup deadlock in the first
version of `drain`, which handed a lock between the acceptor and the drainer;
the sweep now runs inside the acceptor.

## 5. Adversarial review

Two reviewers, one on the receiver's protocol and concurrency and one on its
integration with the daemon. Their findings are merged below where both
raised the same one. Three were declined, with the reason given.

| Finding | Disposition |
|---|---|
| A starved upload blocks FFmpeg but produces no hold, so the 10 s progress deadline replaces the producer (blocker) | Fixed. Starvation is recorded per allocation, the flow evaluation holds a starved allocation, and the reaper evaluates on both edges. |
| A lane switch could land between an old-lane commit's check and its bookkeeping | Fixed. The switch takes the commit lock. |
| Drain could use the whole classification deadline and fail a finished title | Fixed. Drain gets the deadline less 750 ms, and an exited producer's tail is admitted without waiting. |
| A playlist dropped after 30 s, including the final one; a refused object froze publication silently | Fixed. Pending-playlist promotion replaces the timer, and a failed write fails the session through the publication clock. |
| `observe_used` zeroed landed bytes on every walk and on GC | Fixed, as above. |
| Retries cleared the directory while the reaped attempt's uploads could still land | Fixed. `retire_writing_lane` before every clear. |
| A replaced lane kept spending grants for its whole body | Fixed. Checked at every piece. |
| No head timeout; a silent local connection could hold drain and keep the port listening | Fixed. 10 s head limit, sink close checked before a temporary is created, the listening duplicate closed on drop. |
| Waiters not woken on refusal or fence | Fixed by removing the waiters: nothing waits on playlist order any more, and a starved piece polls the fence. |
| The sweep stopped at the first accept error | Fixed. Aborted connections are skipped, descriptor exhaustion retried. |
| An inherited `http_proxy` would carry the uploads | Fixed. |
| Loose HTTP parsing (duplicate framing, unlimited trailers, `+` in chunk sizes) | Fixed. |
| A bind failure answered 500 | Fixed. It is a retryable capacity error. |
| The grant-wait test would hang, not fail, on the old code | Fixed. Bounded. |
| Stale comments and Developer-tab wording | Fixed. |
| No preflight for FFmpeg's `http` output protocol | Not changed. Both published engines have it; a probe adds a start-path dependency for a build nobody ships. Named on the Developer tab. |
| The sink stays bound on segmenter sessions that may never use it, and on retired sessions until their grace ends | Not changed. It is one idle loopback socket per session, refusing everything once fenced. |
| Lazy binding for the copy path | Not changed, for the same reason. |
| Windows open handles during a retry's clear | Fixed as a consequence of retiring the lane and draining before the clear. |

## 6. Still owed

* **Physical acceptance** on real clients, including a long 4K transcode and
  a seek storm on a transcoded title. Not possible from this session.
* **`tests/ui-structure.golden`** drifts from the Developer card, as it
  already did after the earlier promotion.
