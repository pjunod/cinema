# Media body buffers — size the read, then, separately, the acknowledgement

**Status:** M1 merged. §5.1 before/after measured 2026-09-24 (§5.1.1).
Decision 1 taken on Paul's behalf and his to overturn: the shared read is
128 KiB, and `TCP_NODELAY` is set on accepted connections, which removed the
HLS p50 regression (§5.1.2, Decision 6). M2 pending ·
**Executes:** §2.4, C1, F-core-1, F-stream-8, §5.1 item 3 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Implemented:** 2026-09-21 against `main` @
`882862e8`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [PLAYBACK.md](../PLAYBACK.md) (how a segment reaches a client)
and [STREAMING-RELIABILITY-IMPLEMENTATION.md](STREAMING-RELIABILITY-IMPLEMENTATION.md)
(the delivery accounting the HLS pump exists to keep honest). Read §2.2 in
full before touching `hls.rs`: the pump is not a buffer, it is the thing
that proves a byte left the server before the byte is counted or the object
is marked complete. M1 changes one number at four sites and measures. M2 is
a later milestone in this same plan PR that changes how often the pump asks
for that proof, and it must keep every behaviour §2.2 lists. It remains
pending until M1 has the deployment and measurement evidence in §5.2.

The original instruction here was: "if M1 seems to need a change in `hls.rs`
beyond the constructor argument, stop and flag it." It did, and this is the
flag. Raising the read size alone also raises the granularity of the pump's
delivery proof, because the two were the same number (§2.2 item 1). M1
therefore also splits one storage read into fixed-size acknowledgement
pieces, so the read size can move without the proof moving with it.

## 1. Objective

1. Media bodies read storage in 128 KiB units instead of 4 KiB, at the four
   sites the review names, so an 80 Mb/s direct play costs ~80 blocking-pool
   hops per second per viewer instead of ~2,500, and a 4 MB segment crosses
   the pump ~32 times instead of ~1,000. (M1 shipped 256 KiB, half those
   counts; Decision 1 moved it to 128 KiB on the §5.1.1 measurement.)
2. The change is measured — throughput, resident memory under concurrent
   viewers, and tail latency of segment responses — before it is credited,
   with the protocol written down so the number is reproducible.
3. Later, and separately, the HLS pump acknowledges a batch of bytes rather
   than every chunk, without changing what the acknowledgement proves.

## 2. Contract today

Re-verify line numbers at build time; they are from `88a3957a`. Versions:
tokio 1.53.1, tokio-util 0.7.18, hyper 1.10.1, axum 0.8.9.

### 2.1 Before M1, four readers used the default and two were already sized

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

   **The proof granularity, stated in bytes.** One acknowledgement covers
   exactly one `DrivenLocalChunk`, so the chunk size *is* the resolution of
   everything that follows it: the most an abandoned body can over-credit,
   and the largest object a single body poll can make look complete. Before
   M1 that bound was 4 KiB by accident — `ReaderStream`'s undocumented
   default was also the chunk size, because the pump sends one chunk per
   read. M1 makes it 4 KiB on purpose, as
   `MEDIA_BODY_ACK_GRANULARITY` (§3.1), and holds it there while the read
   grows to 256 KiB. Fusing the two instead would have set the bound at
   256 KiB, and every media object at or below that — `init.mp4`, subtitle
   segments, audio-only and low-bitrate renditions (a 2 s AAC segment at
   128 kb/s is ~32 KiB) — would arrive as a single chunk: one poll from a
   client that then disconnected would count the whole object to the
   delivery meter, renew the playback lease, and move the fetched-segment
   frontier that pacing and scratch reclamation read. Items 1 to 3 of this
   section are only true at the granularity this constant sets.
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
/// One blocking-pool hop per read; tokio::fs caps a read at 2 MiB.
pub(crate) const MEDIA_BODY_READ_BUFFER: usize = 128 * 1024;
```

M1 shipped this at 256 KiB, matching the fragment-index and offline transfer
paths. Decision 1 (§7) lowered it to 128 KiB after §5.1.1 measured the two
sizes equal on throughput and latency and 128 KiB cheaper in memory.

The four constructors become `ReaderStream::with_capacity(reader,
MEDIA_BODY_READ_BUFFER)`. M1 also adopts the constant in `offline.rs` and
`internal_media.rs`; their value was already 256 KiB, so this removes two
duplicate spellings without changing their behaviour.

**Second constant: hold the proof granularity where it was.** The two HLS
sites also need the pump's chunk size pinned, or the line above moves it
64× (§2.2 item 1):

```rust
// media_sessions.rs, beside MEDIA_BODY_READ_BUFFER
/// Bytes of a media body proved delivered per downstream acknowledgement.
pub(crate) const MEDIA_BODY_ACK_GRANULARITY: usize = 4 * 1024;
```

Both pump loops keep their single `reader.next()` per iteration and then
hand the result downstream in `MEDIA_BODY_ACK_GRANULARITY` pieces, one
`DrivenLocalChunk` and one acknowledgement each, counting and completing per
piece exactly as before. `Bytes::split_to` is a refcount bump on the buffer
the read already filled, so this costs no copy and no extra allocation, and
it does not touch the blocking-pool hop count — the thing §1 exists to
reduce. The acknowledgement rate is unchanged from before M1 (at 80 Mb/s,
~2,560/s either way); only the storage read count falls, by 32× at
128 KiB (64× at the 256 KiB M1 first shipped).
`driven_local_body`, the channel capacity, both deadlines, the terminal and
the ownership set are untouched.

One consequence inside `SegmentDelivery`: `note_read(bytes, elapsed)`
conflated two different units — bytes delivered (now per acknowledgement)
and a storage read's rate (now per read). It splits into `note_delivered`
and `note_storage_read`, with `note_read` kept as the composition of the two
for the buffered init/probe callers, where one read *is* the whole body. The
rolling pump calls `note_storage_read` once per read, with the read's real
size and duration, after the first piece of that read is acknowledged — so
the rate stays honest and a chunk the consumer never took still reports
nothing.

The slow-read threshold (§2.2 item 7): the honest comparison is bytes per
second, not seconds per read. Change the guard in `note_read` to normalise:
`elapsed >= SEGMENT_WAIT_EVENT_MIN && bytes as f64 / elapsed.as_secs_f64() <
SEGMENT_STALL_BYTES_PER_SECOND` with `SEGMENT_STALL_BYTES_PER_SECOND = 1 MiB/s`
(a 4 KiB read taking 250 ms is 16 KiB/s; a 256 KiB read taking 250 ms is
1 MiB/s and healthy on a NAS). Without this the warning would fire on
ordinary spinning-disk reads after M1. This is the one line outside the
constructors, and it is why M1 is "one number" plus one guard, not one
number.

### 3.2 M2 — batch the acknowledgement, keep what it proves

Later milestone in this PR, with its own tests. The change is in the pump
loop only: instead of one `DrivenLocalChunk` per read, accumulate reads into
a batch of at most
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
- **Read size, acknowledgement size and channel capacity are three separate
  decisions** (F-core-1). They were not separate in the code: with a channel
  of capacity 1 and one acknowledgement per read, the read size silently
  *was* the acknowledgement granularity, so "M1 changes the first only" was
  not achievable as written. M1 separates them —
  `MEDIA_BODY_READ_BUFFER` for storage, `MEDIA_BODY_ACK_GRANULARITY` for the
  proof, `LOCAL_MEDIA_BODY_CHANNEL_CAPACITY` for the channel — and then
  changes only the first. M2 changes the second; neither touches the third.
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

Code: §3.1. Tests: the existing pump tests in `hls.rs` and the
`serve_file_range` tests in `stream.rs` must pass unchanged. They are the
acceptance for this milestone, not a formality — an earlier draft of this
line claimed the change was "invisible to them by construction", and it was
not: three of them failed, and the `grep -n
"driven_local_body\|StreamedBodyTerminal"` recipe this section used to give
did not select any of the three, because they drive the pump through
`segment()` and `vod_segment_response()` rather than by name. Run the whole
`hls` test module, not a name filter. The three that pin the proof
granularity, and the assertion in each that moves if it changes:

| Test (`http::hls::tests::`) | Pins |
|---|---|
| `an_abandoned_segment_body_is_recorded_as_response_dropped` | a 64 KiB segment's first chunk is a strict prefix; the drop is `response_dropped` and credits only that prefix |
| `an_abandoned_vod_body_counts_nothing_it_did_not_hand_over` | two chunks of a 512 KiB VOD object are less than the object; the meter equals exactly what was taken |
| `vod_stream_finalizer_commits_only_exact_live_response_bodies` | one poll of a 64 KiB object leaves `fetched_segment` at `None` and the lease untouched |

Two tests are added:

- `http::hls::tests::a_media_body_is_proved_in_acknowledgement_units_not_storage_read_units`
  — names the decoupling directly: the first chunk of a 64 KiB object is
  `MEDIA_BODY_ACK_GRANULARITY`, exactly that is credited, and the frontier
  stays `None`. It fails if the chunk size is ever refused to the read size
  again.
- `transcode::tests::a_large_read_at_the_event_boundary_emits_no_storage_stall_warning`
  — the normalised slow-read guard through its observable: a 256 KiB read in
  250 ms emits no `segment_delivery_wait`; a 4 KiB read in the same 250 ms
  emits one, and the event names the 4 KiB read. The predicate-level test
  `segment_storage_stall_signal_is_normalized_by_read_size` is kept, but it
  calls `storage_read_is_slow` directly and passes with the guard in
  `SegmentDelivery::note_storage_read` reverted, so it is not the pin.

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
# concurrent (group A, large): 8 viewers, resident memory sampled at 1 Hz
PID=$(pidof plurxd)
( while sleep 1; do awk '/VmRSS/{print systime(), $2}' /proc/$PID/status; done ) &
sampler=$!
pids=()
for v in $(seq 8); do
  curl -s -o /dev/null -H "Authorization: Bearer $TOKEN" \
    "$BASE/api/v1/files/$FILE/direct" &
  pids+=("$!")
done
for curl_pid in "${pids[@]}"; do wait "$curl_pid"; done
kill "$sampler"; wait "$sampler" || true
# concurrent (group B, small): the allocation shape this change actually
# introduces. ReaderStream::with_capacity allocates its BytesMut eagerly at
# construction, and tokio then sizes its blocking read buffer to match, so
# every serve_file_range response reserves ~512 KiB whatever it carries —
# including photo originals, Plex parts, /download and the one- or two-byte
# probe ranges players open a file with. Group A measures the case where
# 256 KiB is proportionate; only this group measures the case where the
# reservation is entirely overhead, scaled by request count. Decision 1
# (§7) is settled by both, not by group A alone.
PHOTO=<item id of a photo whose original is a few hundred KiB or less>
( while sleep 1; do awk '/VmRSS/{print systime(), $2}' /proc/$PID/status; done ) &
sampler=$!
for round in $(seq 20); do
  pids=()
  for v in $(seq 64); do
    curl -s -o /dev/null -r 0-1 -H "Authorization: Bearer $TOKEN" \
      "$BASE/api/v1/files/$FILE/direct" &
    pids+=("$!")
    curl -s -o /dev/null -H "Authorization: Bearer $TOKEN" \
      "$BASE/api/v1/items/$PHOTO/photo" &
    pids+=("$!")
  done
  for curl_pid in "${pids[@]}"; do wait "$curl_pid"; done
done
kill "$sampler"; wait "$sampler" || true
# HLS segments: create one session and measure 200 successful local GETs.
SESSION=$(curl -fsS -X POST \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"playback_id\":\"s02-buffer-bench\",\"request_id\":\"s02-$(date +%s)\",\"height\":2160,\"start\":0}" \
  "$BASE/api/v1/files/$FILE/hls/sessions" | jq -r .session_id)
mapfile -t segments < <(curl -fsS "$BASE/api/v1/hls/$SESSION/index.m3u8" |
  awk '/^[^#].*\.(ts|m4s)$/{print}')
test "${#segments[@]}" -gt 0
for n in $(seq 0 199); do
  segment="${segments[$((n % ${#segments[@]}))]}"
  curl -fsS -o /dev/null -w '%{time_total}\n' \
    "$BASE/api/v1/hls/$SESSION/$segment"
done | sort -n | awk '{a[NR]=$1} END{print "p50",a[int(NR*.5)],"p95",a[int(NR*.95)],"p99",a[int(NR*.99)]}'
curl -fsS -X DELETE "$BASE/api/v1/hls/$SESSION" >/dev/null
```

The API call is deliberate: `scripts/bench run` deletes its session on exit,
so a `--print-session` switch would need new lifecycle ownership rather than
the nominal twenty-line output change. The 45-second fixture also has fewer
than 200 segment names, so the loop samples 200 successful responses over its
real playlist instead of timing 404s after the fixture ends. Also record
blocking-pool activity with `ps -o nlwp -p $PID` peak during the concurrent
run; the shipped build does not enable Tokio's unstable runtime metrics.

Report in the PR body, before/after: single-viewer MB/s (3 runs each),
peak VmRSS with 8 large viewers (group A), peak VmRSS with 128 concurrent
small-object requests in flight (group B), HLS p50/p95/p99, peak thread
count. The acceptance is not a target number — it is that all five are
reported and that p99 and both peak-RSS figures did not get worse. Group B
is the one with veto power over 256 KiB: if peak RSS there scales at roughly
half a megabyte per concurrent small request, Decision 1 falls to 128 KiB or
the small-object paths stop sharing the constant.

Acceptance: run the four focused commands in §6, then `make unit`; record the
four lab4 measurement groups in the PR body.

#### 5.1.1 Measured 2026-09-24

Run on nuc3, not lab4, with three release builds of `main` @ `886fc8bd4`.
The builds differ only in `MEDIA_BODY_READ_BUFFER`: 4 KiB (which is the
pre-M1 behaviour at the measured sites), 128 KiB and 256 KiB. There were
three trials in rotated order, every group ran in a fresh process, media was
page-cache warm, and client and server shared loopback. nuc4 was not usable
for this: it already runs an image that contains M1 (`99d4abf8c`), so it has
no "before" side, and it is a production node whose load nothing here
controls. The host was a shared build host with a load average of 1 to 9,
not the idle lab4 this section asks for. Peak RSS is `VmHWM` after a
`clear_refs` reset, because group A finishes in under a second and the 1 Hz
sampler never recorded a sample. Method, conditions, the harness, every raw
number and the HLS diagnostic are in
[media-body-buffers-m1-measurement-2026-09-24.md](../evidence/media-body-buffers-m1-measurement-2026-09-24.md).
Ranges are over the three trials:

| Figure | 4 KiB (before) | 128 KiB | 256 KiB (merged) |
|---|---|---|---|
| Single-viewer direct play, MB/s (9 runs, median) | 373 | 3341 | 3172 |
| Group A, 8 large viewers: RSS growth, MiB | 1.0–1.3 | 9.3–10.9 | 14.3–16.1 |
| Group B, 128 small requests per round: RSS growth, MiB (6 runs, median) | 44.8 | 41.9 | 53.1 |
| HLS 200 GETs p50 / p95 / p99, ms | 24–25 / 62–65 / 65–72 | 48–50 / 51–58 / 51–60 | 48–50 / 50–57 / 51–58 |
| Peak threads, group A / group B | 46–47 / 60–69 | 35–40 / 38–43 | 29–35 / 37–38 |

Against the acceptance above:

- All five figures are reported.
- **p99 did not get worse.** It improved by 7 to 21 ms.
- **Both peak-RSS figures got worse**, so M1 is **not accepted as the
  acceptance is written**.
- Group A grew about 1.8 MiB per concurrent large body at 256 KiB, over the
  4 KiB build. That is above the ≈1 MiB budget in §2.1. 128 KiB grew about
  1.1 MiB per body.
- **Group B's veto is not reached.** Group B memory is mostly per-connection
  cost, 33 to 59 MiB even at 4 KiB. At matched concurrency, the worst run
  put 256 KiB about 0.27 MiB per in-flight request above 4 KiB, about half
  the veto line.
- **HLS p50 doubled.** Per-segment timing shows a bimodal ~50 ms mode, and
  the diagnostic records one hypothesis for it, not yet tested: Nagle and
  delayed ACK, since `plurxd` never sets `TCP_NODELAY`. It is loopback-only
  evidence and not a §5.1 acceptance figure.

128 KiB matches 256 KiB on throughput and latency within run-to-run spread,
at about two thirds of the group A growth. This record left Decision 1 (§7)
to Paul. It was then taken on his behalf, as 128 KiB, and §5.1.2 records the
follow-up measurement.

#### 5.1.2 Decision 1 follow-up, measured 2026-09-24

Two more runs on nuc3 with the same harness and conditions as §5.1.1: fresh
process per group, `VmHWM` after `clear_refs`, page-cache warm, loopback,
three trials in rotated order, a shared build host (load average 1 to 10).
The harness now also records peak established connections in group B, and
how many of the 200 timed HLS GETs took 40 ms or more. The raw record is in
the same [evidence file](../evidence/media-body-buffers-m1-measurement-2026-09-24.md).

**The Nagle test.** The HLS p50 hypothesis in §5.1.1 was tested, not
assumed. Three release builds of `7eea547fe` were compared: 4 KiB, 128 KiB,
and 128 KiB plus `TCP_NODELAY` on every accepted connection. The last is the
`dbcb1168f` change, applied as a patch. Ranges are over the three trials:

| Figure | 4 KiB | 128 KiB, Nagle on | 128 KiB, `TCP_NODELAY` |
|---|---|---|---|
| Single-viewer direct play, MB/s (9 runs, median) | 361 | 2665 | 2886 |
| Group A: RSS growth, MiB | 1.0–1.2 | 10.2–10.9 | 9.2–10.5 |
| Group B: RSS growth, MiB (median) / peak connections | 34.0–38.4 (37.9) / 78–101 | 37.9–48.5 (48.3) / 74–104 | 36.7–44.8 (44.6) / 68–97 |
| HLS p50 / p95 / p99, ms | 26–27 / 60–66 / 67–71 | 49–51 / 52–58 / 52–58 | **15–16 / 18 / 19–20** |
| HLS GETs of 200 at ≥ 40 ms | 17, 34, 17 | 124, 118, 111 | **0, 0, 0** |
| Peak threads, group A / group B | 45–47 / 57–66 | 32–39 / 39–46 | 31–36 / 40–44 |

The hypothesis holds on loopback. With Nagle on, 128 KiB puts 56 to 62 % of
segment fetches in the ~50 ms delayed-ACK mode. With `TCP_NODELAY` none are
there, and every percentile is below the 4 KiB build's p50. Throughput,
memory and thread counts did not move beyond run-to-run spread. So
`TCP_NODELAY` is in this PR (Decision 6).

**The final state against the before side.** One release build of
`dbcb1168f`, the final code (128 KiB with `TCP_NODELAY`), was compared with
the 4 KiB build above in a separate run of the full protocol:

| Figure (§5.1) | 4 KiB (before) | Final: 128 KiB + `TCP_NODELAY` |
|---|---|---|
| Single-viewer direct play, MB/s (9 runs, median) | 373 | 2747 |
| Group A, 8 large viewers: wall s | 0.67–0.70 | 0.10 |
| Group A: RSS growth, MiB | 0.9–1.2 | 9.0–11.5 |
| Group B, 128 small requests per round: wall s | 3.08–3.22 | 2.08–2.69 |
| Group B: RSS growth, MiB (median) / peak connections | 32.3–38.9 (33.2) / 68–89 | 38.5–47.0 (38.7) / 93–102 |
| Group B: failed requests | 0 | 0 |
| HLS 200 GETs p50 / p95 / p99, ms | 25–26 / 61–64 / 66–76 | **15 / 18 / 19–20** |
| HLS GETs of 200 at ≥ 40 ms | 28, 21, 15 | 0, 0, 0 |
| Peak threads, group A / group B | 44–47 / 58–60 | 30–35 / 38–45 |

Against the §5.1 acceptance:

- All five figures are reported.
- **HLS p99 improved**, from 66–76 ms to 19–20 ms. p50 and p95 improved
  too. The p50 regression §5.1.1 recorded is gone.
- **Group A peak RSS is still worse than 4 KiB**, by 9.0 to 11.5 MiB for 8
  bodies. That is 1.0 to 1.3 MiB per concurrent large body, at the ≈1 MiB
  budget in §2.1. The 256 KiB build was about 1.8 MiB per body. Any read
  above 4 KiB reserves more per open body, so no size that delivers §1 can
  meet "not worse" literally. Decision 1 accepts this cost at 128 KiB.
- **Group B is worse in the median**, 33.2 → 38.7 MiB. The final build also
  ran at higher peak concurrency here (93–102 connections against 68–89), and
  group B memory follows concurrency (§5.1.1). The veto (about half a
  megabyte per concurrent small request) is not reached: even crediting all
  5.5 MiB to the read size, it is well under 0.1 MiB per request.

So on these numbers M1 plus Decision 1 meets the acceptance except the
literal "not worse" for peak RSS. That remainder is the cost Decision 1
accepts, and Paul can overturn it.

### 5.2 M2 — acknowledgement batching

Same plan PR, after the M1 candidate has been exercised on media1 for a week
with no `segment_delivery` regressions in the telemetry (§6). Code: §3.2.
Tests all run against `driven_local_body` plus a pump built from a `Cursor`
reader, so they run in `make unit` without files:

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

- Focused M1 is not sufficient on its own and never was; run
  `cargo test -p plurxd --bin plurxd` whole. The focused filters, one name
  per invocation, are `direct_range`, `driven_local_body`,
  `a_media_body_is_proved_in_acknowledgement_units_not_storage_read_units`,
  `a_large_read_at_the_event_boundary_emits_no_storage_stall_warning`,
  `segment_storage_stall_signal_is_normalized_by_read_size`,
  `segment_delivery_counts_reads_and_names_incomplete_storage`, the three
  proof-granularity tests tabled in §5.1, and, since Decision 1,
  `the_shared_media_read_is_128_kib_and_the_delivery_proof_stays_4_kib` and
  `accepted_http_connections_have_nagle_disabled`. Run the six M2 tests by name when
  that milestone becomes eligible.
- Lane: `make unit` before promoting the one plan PR. The implementation
  session did not run it while P-01 was repairing that lane; this is pending
  evidence, not an implied pass.
- Fleet evidence after M1 deploy: the existing `segment_delivery_*` telemetry
  rows (`transcode.rs:8884` `emit`) already classify cuts
  (`storage_unexpected_eof`, `transport_stall`, `transport_lifetime`). GPT
  prompt: "On media1, one week after PR <n> deployed, run the telemetry
  query for `segment_delivery_*` events grouped by `cut_class` for the seven
  days before and after the deploy timestamp; also `journalctl -u plurxd |
  grep -c 'stalled on storage'` for both windows. Report both tables." A
  rise in `transport_stall` or in the stall warning is the rollback signal.
- Rollout: one draft PR owns the whole plan; milestones remain separate
  commits and execution-log rows. No setting and no gate: the buffer size is
  a constant with its reason; making it operator-tunable would be a knob
  without a reading to set it by. No recipe identity, cache digest or schema
  is touched.
- Rollback: revert; nothing persists.

## 7. Decisions and pending evidence

1. **Use 128 KiB.** *Decided 2026-09-24 on Paul's behalf, and his to
   overturn.* M1 shipped 256 KiB provisionally, to match the two
   pre-existing file-backed paths, and left the size to the concurrent-memory
   measurement. §5.1.1 measured both sizes. They are indistinguishable on
   direct-play throughput (3341 against 3172 MB/s median) and HLS latency.
   128 KiB costs about two thirds of 256 KiB's per-stream memory in group A
   (9.3–10.9 against 14.3–16.1 MiB for 8 bodies) and less in group B
   (median 41.9 against 53.1 MiB). The reservation is eager, so every open
   body pays it whatever it carries. Taking the smaller size costs nothing
   measurable and saves memory on every body. The constant is shared, so
   the offline transfer and internal fragment-index paths also move from
   256 to 128 KiB. Neither was in the measured harness, and both still read
   32× fewer times than a 4 KiB default. `MEDIA_BODY_ACK_GRANULARITY` stays
   4 KiB and stays a separate constant (Decision 5). A test pins the read
   size and the acknowledgement unit separately, so a change to either is a
   deliberate edit. §5.1.2 has the final-state measurement. To overturn:
   restore `256 * 1024` and that test's expected value. Nothing persists.
2. **Use 1 MiB/s for the slow-read rate.** At that rate a roughly 20 MiB 4K
   segment already takes 20 seconds to read, close to the 30-second no-progress
   budget. The boundary test makes the intended classification explicit;
   media1 measurements can still justify an adjustment before M2.
3. **Create the measurement session through the existing API.** A
   `scripts/bench --print-session` option that leaves a session alive would
   introduce a second lifecycle owner. The explicit POST/DELETE pair above is
   reproducible and keeps the benchmark helper out of this transport change.
4. **Use process thread count, not unavailable runtime metrics.** Neither the
   Dockerfiles nor the build configuration enable `tokio_unstable`, so the M1
   protocol records `ps -o nlwp -p $PID` peak.
5. **Decouple the acknowledgement unit from the read unit now, rather than
   accept a coarser proof.** The alternative was to let the proof granularity
   rise to 256 KiB with the read, restate the §2.2 bound at that figure, and
   re-derive the three tests in §5.1 against it. Rejected on two grounds.
   First, the cost of decoupling is nil in the dimension this plan exists to
   improve: splitting a `Bytes` is a refcount bump, the acknowledgement rate
   is exactly what it was before M1, and the 64× reduction in blocking-pool
   hops is untouched. Second, the coarser proof is not a weaker statement
   about a rare case, it is a wrong statement about the common one — every
   `init.mp4`, subtitle segment and audio-only segment is under 256 KiB, so
   the objects a client is most likely to open and abandon are exactly the
   objects that would be credited and committed on one poll. Growing the
   three fixtures past 256 KiB so the assertions go green was never an
   option: it would have deleted the only coverage naming the behaviour
   while leaving the behaviour changed.
6. **Set `TCP_NODELAY` on every accepted HTTP connection.** *Decided
   2026-09-24 on measurement; outside the letter of M1, and recorded here
   because M1's measurement found it.* `plurxd` had never disabled Nagle's
   algorithm. The HLS p50 doubled with the larger read while p99 improved,
   and §5.1.1 recorded a hypothesis: Nagle holding a body's trailing short
   write until the peer's delayed ACK, 40 ms minimum on Linux. §5.1.2 tested
   it with two 128 KiB builds that differed only in the socket option. With
   `TCP_NODELAY`, p50/p95/p99 fell from 49–51 / 52–58 / 52–58 ms to
   15–16 / 18 / 19–20 ms, and fetches at 40 ms or more fell from 353 of 600
   to none. Throughput, memory and thread counts were unchanged within
   spread. hyper already coalesces a response's head and body writes, so
   the option does not fragment responses. It is set in the `TcpListener`
   acceptor that feeds `serve_http`, the only production HTTP listener. A
   failure to set it is logged at debug, and the connection is still
   served. `accepted_http_connections_have_nagle_disabled` pins it.
   Loopback is not a client network. Whether the same p50 shift existed on
   real client links, and how much this changes there, is not measured. The
   post-deploy telemetry check in §6 is the place it would show.

---

## Execution log

Executing sessions append one row per milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M1 | `ed98c6ab` / [#410](http://192.168.4.7:3000/noirr/plurx/pulls/410) | Shared 256 KiB capacity at all six file-backed readers; rate-normalized the slow-read signal. Pinned 1.97.1 check and six focused regressions passed. Needs: lab4 before/after throughput, peak RSS/thread count, HLS p50/p95/p99, and the repaired fast-lane `make unit` evidence. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M2 | pending in [#410](http://192.168.4.7:3000/noirr/plurx/pulls/410) | Needs: M1 candidate deployed on media1 for one week with no `segment_delivery` regression, then the §5.1 lab4 protocol re-run. No acknowledgement-batching code has been written. |
| 2026-09-22 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 review disposition | [#410](http://192.168.4.7:3000/noirr/plurx/pulls/410) | Addressed the sole adversarial review. The 256 KiB read had carried the pump's delivery-proof granularity up with it; `MEDIA_BODY_ACK_GRANULARITY = 4 KiB` now pins the proof where it was (§2.2 item 1, §3.1, Decision 5) and the three delivery-accounting tests that failed at the merged head pass again for that reason. Added `a_media_body_is_proved_in_acknowledgement_units_not_storage_read_units` and `a_large_read_at_the_event_boundary_emits_no_storage_stall_warning`; §5.1 now measures small-object concurrency (group B) as well as large. Still needs: the lab4 groups A and B before/after, and `make unit`. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 §5.1 measurement | evidence PR (this row's commit) | Before/after measured on nuc3 with three release builds of `886fc8bd4`, identical except `MEDIA_BODY_READ_BUFFER` at 4/128/256 KiB, and three rotated trials. nuc4 was only inspected: it already runs M1 (`99d4abf8c`), so it has no before side, and it carries production load. Results (§5.1.1, raw record in `docs/evidence/media-body-buffers-m1-measurement-2026-09-24.md`): single-viewer direct play 373 → 3172 MB/s median; HLS p99 65–72 → 51–58 ms (met) and p50 24–25 → 48–50 ms; group A RSS growth 1.0–1.3 → 14.3–16.1 MiB; group B median 44.8 → 53.1 MiB, with the veto not reached. Acceptance **not met as written**: both peak-RSS figures grew. Decision 1 (256 or 128 KiB) is waiting on Paul. The §5.1 photo URL is now the real route, `/api/v1/items/{id}/photo`. `make unit`: #454's integration record ran its `cargo test --workspace --exclude plurx-cluster-check` half on the integrated tree (4051 passed, 0 failed). Its `vodencode-restart-check` half is not recorded there. Still owed: Paul's Decision 1, a lab4 re-run only if the quiet-host condition is wanted, and M2's media1 week. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Decision 1 + HLS p50 | `7eea547fe`, `dbcb1168f` / [#487](http://192.168.4.7:3000/noirr/plurx/pulls/487) | **Decision 1 taken on Paul's behalf; he can overturn it:** the shared read is now 128 KiB (`7eea547fe`). `MEDIA_BODY_ACK_GRANULARITY` is unchanged at 4 KiB. The new `the_shared_media_read_is_128_kib_and_the_delivery_proof_stays_4_kib` pins both values. The pin fails with the read reverted to 256 KiB (`left: 262144, right: 131072`). Defining the acknowledgement unit from the read stops the test binary compiling. Re-coupling the pump split still fails `a_media_body_is_proved_in_acknowledgement_units_not_storage_read_units` (`left: 65536, right: 4096`). The HLS p50 Nagle hypothesis was tested on nuc3 (§5.1.2): 128 KiB with and without `TCP_NODELAY`, three rotated trials. p50 went from 49–51 to 15–16 ms, with nothing else moving, so `dbcb1168f` sets it on accepted connections. `accepted_http_connections_have_nagle_disabled` pins it and fails with the call removed. Final state against 4 KiB, full §5.1 re-run: direct play 373 → 2747 MB/s median; HLS p50/p95/p99 25–26 / 61–64 / 66–76 → 15 / 18 / 19–20 ms; group A growth 0.9–1.2 → 9.0–11.5 MiB, which is 1.0–1.3 MiB per body, at the §2.1 budget; group B median 33.2 → 38.7 MiB, at higher concurrency, far from the veto. Gates: fmt, clippy `-D warnings`, `cargo test -p plurxd` (2717 passed, 0 failed, 11 ignored), history-check, validation-lint, validation unittests, operations-check, spike-lock-check, all exit 0 (PR body). Still owed: Paul's confirmation or reversal of Decision 1, M2's media1 week, and the §6 post-deploy telemetry for both changes. |
