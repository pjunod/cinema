# Media body buffers — size the read, then, separately, the acknowledgement

**Status:** ready for review · **Executes:** §2.4, C1, F-core-1, F-stream-8,
§5.1 item 3 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Companion to [PLAYBACK.md](../PLAYBACK.md) (how a segment reaches a client)
and [STREAMING-RELIABILITY-IMPLEMENTATION.md](STREAMING-RELIABILITY-IMPLEMENTATION.md)
(the delivery accounting the HLS pump exists to keep honest). Read §2.2 in
full before touching `hls.rs`: the pump is not a buffer, it is the thing
that proves a byte left the server before the byte is counted or the object
is marked complete. M1 changes one number at four sites and measures. M2 is
a later PR that changes how often the pump asks for that proof, and it must
keep every behaviour §2.2 lists. If M1 seems to need a change in `hls.rs`
beyond the constructor argument, stop and flag it.

## 1. Objective

1. Media bodies read storage in 256 KiB units instead of 4 KiB, at the four
   sites the review names, so an 80 Mb/s direct play costs ~40 blocking-pool
   hops per second per viewer instead of ~2,500, and a 4 MB segment crosses
   the pump ~16 times instead of ~1,000.
2. The change is measured — throughput, resident memory under concurrent
   viewers, and tail latency of segment responses — before it is credited,
   with the protocol written down so the number is reproducible.
3. Later, and separately, the HLS pump acknowledges a batch of bytes rather
   than every chunk, without changing what the acknowledgement proves.

## 2. Contract today

Re-verify line numbers at build time; they are from `88a3957a`. Versions:
tokio 1.53.1, tokio-util 0.7.18, hyper 1.10.1, axum 0.8.9.

### 2.1 The four `ReaderStream::new` sites, and the two that already size

| Site | Function | Reader | Body |
|---|---|---|---|
| `http/stream.rs:2940` | `serve_file_range` (`/api/v1/files/{id}/direct`, `/download`, Plex parts, photos), `206` arm | `fh.take(count)` | `Body::from_stream` |
| `http/stream.rs:2957` | `serve_file_range`, `200` arm | `fh` | `Body::from_stream` |
| `http/hls.rs:13402` | `vod_segment_response_before` | `take(ready.file, len)` | pump (§2.2) |
| `http/hls.rs:14012` | `segment_local_before` | `opened.file.take(opened_len)` | pump (§2.2) |
| `http/internal_media.rs:116` | fragment-index blob | `with_capacity(file, 256 * 1024)` | `Body::from_stream` |
| `http/offline.rs:1192` | `hls_stream_response` | `with_capacity(file.take(bytes), TRANSFER_STREAM_BUFFER)` where `TRANSFER_STREAM_BUFFER = 256 * 1024` (`offline.rs:33`) | `Body::from_stream` |

`ReaderStream::new` uses `DEFAULT_CAPACITY = 4096`
(`tokio-util-0.7.18/src/io/reader_stream.rs:8`), documented as "not part of
the semver contract". `poll_next` reserves `capacity` when the internal
`BytesMut` is empty and calls `poll_read_buf` once per item. `tokio::fs::File`
reads through `blocking.rs:75`: `max_buf_size = min(dst.remaining(),
DEFAULT_MAX_BUF_SIZE)` with `DEFAULT_MAX_BUF_SIZE = 2 MiB`, one
`spawn_blocking` per read. So the unit of blocking work is exactly the
`ReaderStream` capacity, up to 2 MiB. The `.take(n)` wrapper only clamps
the last read.

Resident cost per viewer after the change is not "one 256 KiB buffer": the
`Bytes` handed to hyper, the `BytesMut` the stream reserves for the next
read, tokio's blocking `Buf` for the in-flight read, and (HLS) the one chunk
parked in the pump channel are each up to 256 KiB. Budget **≈1 MiB per
active body** as the upper bound and measure the real figure (§5.1).

### 2.2 The HLS pump — the contract M2 must keep

Both HLS sites build the same structure (`hls.rs:13402-13556` for VOD,
`14012-14150` for rolling). Constants: `LOCAL_MEDIA_BODY_CHANNEL_CAPACITY =
1` (`hls.rs:9900`); `MAX_ADMITTED_MEDIA_BODY_LIFETIME = 300 s`
(`media_sessions.rs:62`); `MEDIA_BODY_NO_PROGRESS_TIMEOUT = 30 s`
(`media_sessions.rs:84`).

```text
  spawned pump task (owns file, delivery tracker, authorization, permit)
  ┌────────────────────────────────────────────────────────────────────┐
  │ loop:                                                              │
  │   select! biased { body_deadline → fail; sender.closed → return;   │
  │                    progress_deadline → fail; reader.next() }       │
  │   send DrivenLocalChunk { bytes, accepted: oneshot }  (cap 1)      │
  │   select! biased { accepted_rx → ok; body_deadline → fail;         │
  │                    downstream_deadline → fail; sender.closed }     │
  │   delivery.note(len) / note_read(len, elapsed); delivered += len   │
  │   if delivered == len → settle_streamed_response_completion(...)   │
  └────────────────────────────────────────────────────────────────────┘
          │ mpsc(1)                                     ▲ oneshot ack
          ▼                                             │
  driven_local_body (unfold over receiver)  ── recv → recheck fences → ack → yield Bytes
```

The properties, each with the line that implements it:

1. **Acceptance before accounting.** `delivery.note(bytes_len)`
   (`:13527`) and `delivery.note_read(bytes_len, read_elapsed)` (`:14136`)
   run only after `accepted_rx` resolves `Ok(())`, and the body side sends
   that ack (`:10008`) only after it has re-checked `body_deadline` and
   `terminal.signal` *after* `recv` (`:9998-10007`). A chunk taken by the
   body while the deadline was firing is never counted. `note` feeds the
   session's delivery meter (`meter.rs:63`), which is why the comment at
   `:13519-13526` says a meter advanced at read time "would measure the
   disk".
2. **Final byte.** `delivered == len` triggers
   `settle_streamed_response_completion` (`:13530-13542`, `:14138-14150`)
   exactly once, with `completion.take()`; on the rolling site it also
   requires `delivery.finish()` to return `true`. A short read
   (`None` with `delivered != len`) is `UnexpectedEof` and never completes.
3. **Dropped consumer.** `sender.closed()` is in both selects; the pump
   returns without failing the terminal, so a client that went away is
   `response_dropped`, not a transport error (`transcode.rs:8933`
   `cut_class`).
4. **Two deadlines, always advancing.** `body_deadline` is absolute (300 s
   from headers); `progress_deadline`/`downstream_deadline` are 30 s from
   the last loop iteration, capped by `body_deadline`. Both fire whether or
   not hyper is polling, which is the reason the pump is a spawned task and
   not a `Body::from_stream` over the file (F-core-1: "replacing the pump
   … loses its completion/cancellation accounting").
5. **Ownership.** The pump task owns the file handle, the `Arc<Delivery>` /
   `DeliveryTracker`, the `MediaResponseAuthorization`, and the
   `OwnedSemaphorePermit`; they are released when the task returns, not
   when hyper drops the body.
6. **Terminal error propagation.** `StreamedBodyTerminal::fail` records the
   first failure and cancels a token; the body side turns the token into an
   `Err` item (`:9967-9985`).
7. **Slow-read reporting.** `note_read` compares `elapsed` (one
   `reader.next()`) against `SEGMENT_WAIT_EVENT_MIN = 250 ms`
   (`transcode.rs:260`) and emits one "stalled on storage" warning per body.
   **This changes meaning with a larger buffer**: a 256 KiB read at 1 MiB/s
   takes 250 ms, a 4 KiB read at the same rate takes 4 ms. §3.1 handles it.

## 3. Change

### 3.1 M1 — `with_capacity(MEDIA_BODY_READ_BUFFER)` at four sites

One constant, named beside the delivery constants so the two pumps and the
direct-play path agree:

```rust
// media_sessions.rs, beside MAX_ADMITTED_MEDIA_BODY_LIFETIME
/// Bytes requested from storage per read while streaming a media body.
/// One blocking-pool hop per read; tokio::fs caps a read at 2 MiB. Matches
/// the fragment-index and offline transfer paths (256 KiB).
pub(crate) const MEDIA_BODY_READ_BUFFER: usize = 256 * 1024;
```

The four constructors become `ReaderStream::with_capacity(reader,
MEDIA_BODY_READ_BUFFER)`. `offline.rs` and `internal_media.rs` may adopt the
constant in the same PR (their literal is the same value) or stay; no
behaviour changes either way.

The slow-read threshold (§2.2 item 7): the honest comparison is bytes per
second, not seconds per read. Change the guard in `note_read` to normalise:
`elapsed > SEGMENT_WAIT_EVENT_MIN && bytes as f64 / elapsed.as_secs_f64() <
SEGMENT_STALL_BYTES_PER_SECOND` with `SEGMENT_STALL_BYTES_PER_SECOND = 1 MiB/s`
(a 4 KiB read taking 250 ms is 16 KiB/s; a 256 KiB read taking 250 ms is
1 MiB/s and healthy on a NAS). Without this the warning would fire on
ordinary spinning-disk reads after M1. This is the one line outside the
constructors, and it is why M1 is "one number" plus one guard, not one
number.

### 3.2 M2 — batch the acknowledgement, keep what it proves

Later PR, its own tests. The change is in the pump loop only: instead of
one `DrivenLocalChunk` per read, accumulate reads into a batch of at most
`MEDIA_BODY_ACK_BATCH_BYTES = 1 MiB` (or until `reader.next()` would block —
use `poll_next` with `Poll::Pending` as the batch boundary, never a timer),
send the batch as one chunk, and wait for one ack. Then:

- item 1 holds: `note`/`note_read` are called once per *batch* with the
  batch's byte count, still after the ack;
- item 2 holds: the last batch ends at `len`; completion runs when
  `delivered == len` after that ack, and a short read is still
  `UnexpectedEof`;
- items 3–6 are untouched: the selects, deadlines, terminal and ownership
  do not move;
- item 7: `note_read` receives the batch's elapsed time and byte count; the
  normalised guard from §3.1 makes that meaningful.

What M2 must **not** do: send a batch before the reader has produced it
(no speculative sizing), change `LOCAL_MEDIA_BODY_CHANNEL_CAPACITY`, or
move accounting ahead of the ack. `driven_local_body` is unchanged: it
already handles a chunk of any size.

## 4. Guardrails (non-goals)

- **Not `Body::from_stream` for HLS.** F-core-1's disposition: the pump's
  completion/cancellation accounting has no equivalent in a plain body.
- **Read size and channel capacity are separate decisions** (F-core-1).
  M1 changes the first only; M2 changes the ack cadence, not the channel.
- **No claimed magnitude.** F-stream-8: "a 64× larger buffer does not prove
  a 100× throughput win". The hop count is arithmetic; the PR body carries
  the §5.1 numbers or the PR does not merge.
- **Bounded per-viewer memory stays bounded.** The channel stays at 1 and
  the batch cap is a constant; no unbounded `Vec` accumulates reads.
- **Direct play validators (`ETag`/`If-Range`, C11) are not in scope.**
- **Live TV and progressive remux bodies are not touched.** The former
  streams from a producer pipe, the latter (`stream.rs:3320-3350`) from
  ffmpeg stdout through a `BufReader`; neither is a `ReaderStream` over a
  file.

## 5. Milestones

### 5.1 M1 — buffer size at four sites, with a measurement protocol

Code: §3.1. Tests: the existing pump tests in `hls.rs` (`grep -n
"driven_local_body\|StreamedBodyTerminal" crates/plurxd/src/http/hls.rs`
under `mod tests`) and `serve_file_range` tests in `stream.rs` must pass
unchanged — the change is invisible to them by construction. Add one test
in `transcode.rs` for the normalised slow-read guard: a 256 KiB read in
250 ms does **not** warn; a 4 KiB read in 250 ms does.

Measurement protocol — run on `lab4` (the playback-qualification host)
against a locally built `plurxd` pointed at a library with two generated
fixtures from `scripts/bench fixtures` (`4k-hdr10` remux-shaped, ~80 Mb/s;
and a 1080p H.264 direct-play file), before and after the change, same
binary flags, same node, nothing else running. `scripts/bench` has no
direct-play throughput mode, so the protocol is `curl`:

```bash
# direct play, single viewer: bytes/s and blocking-pool cost
TOKEN=… BASE=http://10.42.0.14:32400
FILE=<file id of the 4k-hdr10 fixture>
for i in 1 2 3; do
  curl -s -o /dev/null -w '%{speed_download} %{time_total}\n' \
    -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/files/$FILE/direct"
done
# concurrent: 8 viewers, resident memory of plurxd sampled at 1 Hz
PID=$(pidof plurxd)
( while sleep 1; do awk '/VmRSS/{print systime(), $2}' /proc/$PID/status; done ) &
for v in $(seq 8); do
  curl -s -o /dev/null -H "Authorization: Bearer $TOKEN" \
    "$BASE/api/v1/files/$FILE/direct" &
done; wait; kill %1
# HLS segments, tail latency: 200 sequential segment GETs on a copy session
SESSION=$(scripts/bench run --only 4k-hdr10 --token "$TOKEN" --print-session)
for n in $(seq 0 199); do
  curl -s -o /dev/null -w '%{time_total}\n' \
    "$BASE/hls/$SESSION/seg$(printf %05d $n).ts"
done | sort -n | awk '{a[NR]=$1} END{print "p50",a[int(NR*.5)],"p95",a[int(NR*.95)],"p99",a[int(NR*.99)]}'
```

(`--print-session` does not exist in `scripts/bench` today; add it, or
read the session id from the `Activity` page — say which in the PR.) Also
record `tokio` blocking-pool activity: `plurx_tokio_blocking_threads` if the
runtime metrics are exposed on `/metrics`, otherwise `ps -o nlwp -p $PID`
peak during the concurrent run.

Report in the PR body, before/after: single-viewer MB/s (3 runs each),
peak VmRSS with 8 viewers, HLS p50/p95/p99, peak thread count. The
acceptance is not a target number — it is that the three are reported and
that p99 and peak RSS did not get worse.

Acceptance: `cargo test -p plurxd serve_file_range driven_local_body
note_read` green; `make unit` green; the four numbers in the PR body.

### 5.2 M2 — acknowledgement batching

Separate PR after M1 has been on media1 for a week with no
`segment_delivery` regressions in the telemetry (§6). Code: §3.2. Tests,
all against `driven_local_body` + a pump built from a `Cursor` reader so
they run in `make unit` without files:

| Test | Asserts |
|---|---|
| `a_batch_is_counted_once_after_its_ack` | `delivery.note` called with the batch total, after the body yielded it, not before |
| `the_final_batch_completes_exactly_once` | a 3 MiB body with a 1 MiB cap: three chunks, `settle_streamed_response_completion` once, after the third ack |
| `a_short_source_is_still_unexpected_eof` | reader ends at 2.5 MiB of an advertised 3 MiB: terminal error `UnexpectedEof`, no completion |
| `a_dropped_receiver_ends_the_pump_without_a_failure` | drop the body mid-batch: pump returns, terminal has no error, permit released |
| `the_downstream_deadline_still_fires_across_a_batch` | body stops polling after the first chunk: `downstream_no_progress` at 30 s (paused time) |
| `a_batch_never_waits_for_more_bytes_than_the_reader_has` | reader returns `Pending` after 100 KiB: that 100 KiB is sent as a batch, not held |

Acceptance: those six plus the M1 tests green; the §5.1 protocol re-run on
lab4 with HLS p99 not worse than M1's.

## 6. Verification and rollout

- Focused: `cargo test -p plurxd serve_file_range driven_local_body note_read`
  (M1); the six M2 tests by name.
- Lane: `make unit` on both PRs.
- Fleet evidence after M1 deploy: the existing `segment_delivery_*` telemetry
  rows (`transcode.rs:8884` `emit`) already classify cuts
  (`storage_unexpected_eof`, `transport_stall`, `transport_lifetime`). GPT
  prompt: "On media1, one week after PR <n> deployed, run the telemetry
  query for `segment_delivery_*` events grouped by `cut_class` for the seven
  days before and after the deploy timestamp; also `journalctl -u plurxd |
  grep -c 'stalled on storage'` for both windows. Report both tables." A
  rise in `transport_stall` or in the stall warning is the rollback signal.
- Rollout: one draft PR per milestone under the fast lane. No setting and
  no gate: the buffer size is a constant with its reason; making it
  operator-tunable would be a knob without a reading to set it by. No
  recipe identity, cache digest or schema is touched.
- Rollback: revert; nothing persists.

## 7. Open questions

1. The concurrent-memory bound in §2.1 (≈1 MiB per body) is an upper bound
   from the four buffers in flight; the measured figure decides whether
   256 KiB or 128 KiB is the right constant for a node whose ceiling is
   the blocked-GET cap (`plurx_vod_blocked_get_cap` on `/metrics`) plus
   direct plays. The protocol reports it; the PR picks.
2. Should `SEGMENT_STALL_BYTES_PER_SECOND` be 1 MiB/s? It is the rate at
   which a 2 s 4K segment (≈20 MB) takes 20 s to read — well past the
   30 s no-progress deadline's spirit. A lower number hides real stalls; a
   higher one warns on healthy NAS reads. Measure on media1's NAS mount.
3. `scripts/bench --print-session` or an Activity-page read: which does the
   PR use? Adding the flag is ~20 lines of Python and makes the protocol
   scriptable for M2's re-run.
4. Whether tokio runtime metrics (`tokio_unstable`) are compiled in decides
   whether blocking-pool hops can be counted directly or only inferred from
   thread count. Check `RUSTFLAGS` in the Dockerfile before the M1 run.
