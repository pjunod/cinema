# DVR scheduler guide view and sink isolation — the scheduler reads the whole guide, and one slow disk stalls one recording

**Status:** implementation and sole adversarial-review fixes complete in draft
PR #407; broad qualification and fleet evidence pending · **Executes:** L1 /
F-ltv-1 and L10 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Started from:** `main` @ `94e36750` ·
**Revalidated after merging:** `main` @ `882862e8`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read [ARCHITECTURE-REVIEW-2026-09-20.md §3.4](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
rows L1 and L10 first, then the assessment rows L1 and F-ltv-1 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)
— every "required disposition" there is a guardrail or an acceptance check
below. The design the DVR was built to is
[LIVE-TV-DVR-IMPLEMENTATION.md §3.2 and §3.5](LIVE-TV-DVR-IMPLEMENTATION.md);
this plan changes how the scheduler *reads* and how a sink *writes*, not what
either means. The work board's whole-plan rule supersedes the earlier
milestone-PR wording: M1–M3 are logical commits in one draft PR into `main`.
Every `file:line` is against `88a3957a` — re-verify
at build time. If a step seems to require changing the public guide JSON
shape, the 2 MiB public response bound, the attempt-file (`O_EXCL`) rule, the
per-chunk serving-fence check, or the `(channel_id, airing_start)` identity,
stop and flag it rather than doing it.

**Correction to the review:** L1 says the guide is "cloned + JSON-serialised
twice per 15 s tick". It is three times on the owner: `dvr_expand`
(`dvr.rs:753`), `dvr_reconcile` (`:849`) and `reminder_tick` (`:2284`), all
through `local_guide` → `GuideCache::read` → `clipped`, and `clipped`
serialises once per bound-check pass (up to eight passes on an oversized
guide), so "twice" is the floor. L1 also implies "series rules only see the
first day"; how many days survive depends on the lineup — the assessment's
correction stands, and the test below is written against a matching late
airing, not a day count. The persisted `guide.json` is loaded without
re-normalisation (`live_tv.rs:2093-2135`), which matters for the binary
search in §3.2.

## 1. Objective

1. The DVR scheduler, reconciler and reminder sweep match rules against the
   owner's **full** cached guide for the whole `DVR_SCHEDULE_DAYS_MAX` (14
   day) horizon. A rule that matches an airing 13 days out schedules it,
   whatever the guide's byte size.
2. The 2 MiB bound (`MAX_GUIDE_RESPONSE_BYTES`) is applied exactly where a
   body leaves the process: the public `GET /live-tv/guide` handler and the
   internal owner→ingress relay. Nowhere else.
3. A guide read for scheduling costs one `Arc` clone under the guide mutex and
   no JSON serialisation; a per-channel window clip is a binary search over
   the sorted programme list.
4. One DVR sink's slow or failing destination cannot delay the shared tuner
   reader or a sibling sink by more than the shared `Bytes` handoff into a
   bounded queue. The failing sink ends *its* attempt with a named reason;
   the healthy sink and the tuner continue.
5. Nothing about ownership changes: one tuner GET per channel, the serving
   fence checked per chunk, `O_EXCL` attempt files, generation fencing on
   `drain_before`, and stop/failover waiting for writers to settle.

## 2. Contract today

Re-verify each of these at build time.

### 2.1 The scheduler's read path (L1)

`dvr_expand` (`crates/plurxd/src/live_tv/dvr.rs:753-761`):

```rust
let guide = self
    .local_guide(
        live_tv,
        GuideWindow {
            start: now,
            end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
        },
    )
    .await;
```

`dvr_reconcile` does the same at `:849-857` with `start: now - 3600`, and
`reminder_tick` at `:2284-2291`. `local_guide` (`live_tv.rs:3979-3988`) is:

```rust
if config.owner_node_id != self.node_id || config.guide_source == GuideSource::Off {
    return LiveTvGuide::unavailable(config.guide_source, window, None);
}
self.guide_cache.read(config, window).await
```

and `GuideCache::read` (`live_tv.rs:2257-2289`) holds the
`tokio::sync::Mutex<GuideCacheState>` guard (`state`) across
`cached.guide.clipped(&window)` at `:2278`. `LiveTvGuide::clipped`
(`live_tv/guide.rs:222-269`) clones the whole document, `retain`s each
channel's programmes to the window (linear), then loops up to eight times
serialising the document with `serde_json::to_vec` and popping the
furthest-out rows until the encoding is `<= MAX_GUIDE_RESPONSE_BYTES`
(`2 * 1024 * 1024`, `guide.rs:54`). The public handler calls the same
`local_guide` (`http/live_tv.rs:137`), and the internal relay handler calls
it with `refresh_window(config.guide_hours)` (`http/internal_live_tv.rs:78-79`)
and the requesting ingress bounds the body at `MAX_SNAPSHOT_BYTES`
(`2 * 1024 * 1024`, `live_tv.rs:54`) — so the relay depends on the clip too.

`GuideCache::owner_copy` (`live_tv.rs:2295-2304`) already exists ("what the
DVR scheduler matches rules against") and returns an unclipped `clone()`; the
scheduler does not call it — only the incremental refresh does (`:4189`).

Sorting invariant: `normalise_programmes` (`guide.rs:281-`) sorts by
`(start, end)` and removes overlaps for every fetched or merged page; the
persisted file is adopted without it (`live_tv.rs:2093-2135`).

Horizon inputs: `DVR_SCHEDULE_DAYS_MAX: i64 = 14` (`plurx-core/src/dvr.rs:31`);
`live_tv.guide_hours` default `24`, range `4..=336` (`live_tv.rs:86-88`);
`GUIDE_MAX_PROGRAMMES_PER_CHANNEL = 800` (`guide.rs:51`). The guide only
holds `guide_hours` ahead, so a day-13 airing exists only when
`guide_hours >= 312`.

### 2.2 The fan-out's write path (L10)

`pump_tuner_fanout` (`dvr.rs:1914-2045`), per chunk, after the per-chunk
serving-fence check (`:1953-1960`):

```rust
let sinks = transport.live_sinks();
...
for sink in sinks {
    if now < sink.window.0 || now >= sink.window.1 { continue; }
    let mut handle = sink.file.lock().await;                       // outside the timeout
    let Some(file) = handle.as_mut() else { continue; };
    match tokio::time::timeout(TUNER_READ_TIMEOUT, file.write_all(&bytes)).await {
        Ok(Ok(())) => { /* bytes, observation, delivered */ }
        Ok(Err(error)) => { /* capture_interrupted disk_write_failed; sink.cancel.cancel() */ }
        Err(_) => { /* capture_interrupted disk_write_timeout; sink.cancel.cancel() */ }
    }
}
```

`TUNER_READ_TIMEOUT = 30 s` (`live_tv.rs:105`). `DvrSink.file` is
`tokio::sync::Mutex<Option<tokio::fs::File>>` (`dvr.rs:115`); `stop_sink`
(`:1499-1529`) cancels, then takes the lock to `flush` + `sync_all`.
`close_transport` (`:1596-1619`) cancels the transport and every sink and
joins the worker under `SESSION_DRAIN_TIMEOUT` (5 s, `live_tv.rs:177`).
`dvr_recover` (`:644-736`) opens attempt `row.attempt + 1` for a row with no
live sink while `now < capture_end - DVR_MIN_USEFUL_S` and
`attempt < DVR_ATTEMPTS_MAX` (8), recording the gap. Attempt files are
`<base>.a<N>.part` opened with `create_new(true)` (`:1426-1435`);
`concatenate_attempts` (`:2071`) joins them in order.

Sequential consequences today: a sink whose `write_all` blocks for 30 s
holds the loop, so the tuner body is not read for 30 s (the HDHomeRun keeps
sending; reqwest buffers, then TCP backpressure), and every sibling sink
misses 30 s of bytes without any event saying so. The `sink.file.lock()`
before the timeout means a `stop_sink` holding the lock through a slow
`sync_all` stalls the loop with no bound at all.

## 3. Change

### 3.1 An immutable full-guide view for internal consumers

`CachedGuide.guide: LiveTvGuide` becomes `guide: Arc<LiveTvGuide>`
(`live_tv.rs:2007`). `GuideCache` gains:

```rust
/// The owner's full cached guide for this generation, unclipped, unbounded
/// by bytes, shared by reference. `None` when nothing usable is cached.
async fn view(&self, generation: i64) -> Option<GuideView>;

pub(crate) struct GuideView {
    pub(crate) guide: Arc<LiveTvGuide>,   // immutable; a refresh replaces the Arc
    pub(crate) fetched_at: i64,
    pub(crate) age: Duration,
    pub(crate) freshness: GuideFreshness, // Fresh | Stale by the same rule as read()
}
```

`view` takes the mutex only to clone the `Arc` and compute the age; the
existing `generation` and `GUIDE_STALE_TTL` filters stay exactly as in
`read`. `owner_copy` is reimplemented on `view` (it returns
`(*view.guide).clone()` for the one caller that mutates). `LiveTvManager`
gains `local_guide_view(&self, config) -> Option<GuideView>` with the same
owner/`GuideSource::Off` guard as `local_guide`, so a non-owner node still
sees "no guide" and the DVR loop's `guide_fetches()` checks are unchanged.

Why an `Arc` and not a longer-lived clone: a refresh that lands mid-tick
replaces `state.cached`; the tick keeps the snapshot it started with, which
is exactly the concurrent-refresh property the assessment asks the test to
prove, and a `store()` never waits on a scheduler.

### 3.2 Per-channel window clip by binary search, no byte bound

`LiveTvGuideChannel` gains:

```rust
/// Programmes touching `[start, end)`. Requires the sorted, non-overlapping
/// order `normalise_programmes` establishes; `assert_sorted` is checked on
/// adoption, so a persisted file cannot violate it silently.
pub(crate) fn programmes_in(&self, window: &GuideWindow) -> &[LiveTvProgramme] {
    let first = self.programmes.partition_point(|p| p.end <= window.start);
    let last = self.programmes.partition_point(|p| p.start < window.end);
    &self.programmes[first..last.max(first)]
}
```

Reason for the invariant check: `partition_point` on an unsorted slice
returns a legal but wrong index, and the persisted-load path
(`live_tv.rs:2093-2135`) adopts whatever `guide.json` says. `store()` and
the persisted load both run `normalise_programmes` when
`programmes.windows(2).any(|w| w[1].start < w[0].end)` finds a violation
(a warn-level log, then normalise, never a refusal — the guide is a cache).

`dvr_expand`, `dvr_reconcile` and `reminder_tick` switch from
`local_guide(...)` to `local_guide_view(...)` and iterate
`channel.programmes_in(&window)`. `into_some_if_populated` semantics are
kept: `dvr_reconcile` treats `None` view **or** an empty `channels` as "I
cannot tell", never "no" (the comment at `dvr.rs:842-847` explains why a
settings save must not withdraw every row). `rule_matches` and
`recording_from_programme` take `&LiveTvProgramme` already; no signature
change.

`LiveTvGuide::clipped` keeps its byte bound and becomes the **only** place
`MAX_GUIDE_RESPONSE_BYTES` is applied. Its `retain` is replaced by
`programmes_in(..).to_vec()` per channel so the public path gets the same
binary search. It is called by `GuideCache::read` (public handler and
relay) and by the ingress relay memory (`http/live_tv.rs:140,149`), all of
which produce HTTP bodies. `read` is changed to take the `Arc` under the
lock and run `clipped` **after** dropping the guard, so a 2 MiB
serialisation never holds the guide mutex against a refresh or another
reader.

### 3.3 Per-sink owned writer with a bounded byte queue (L10)

```
 tuner body ─▶ pump_tuner_fanout ─┬─▶ sink A: try_send(Bytes) ─▶ queue (≤ DVR_SINK_QUEUE_BYTES) ─▶ writer task A ─▶ <base>.a1.part
                (one task, one       │
                 fence check/chunk)  └─▶ sink B: try_send(Bytes) ─▶ queue (≤ DVR_SINK_QUEUE_BYTES) ─▶ writer task B ─▶ <base>.a1.part
```

`DvrSink.file` is replaced by:

```rust
pub(crate) struct DvrSink {
    ...
    /// The only handle the fan-out has on the file: a bounded byte queue.
    /// `None` after the writer has been asked to close.
    writer: std::sync::Mutex<Option<SinkQueue>>,
    /// Joined by stop/failover so an attempt is never concatenated while a
    /// write to it may still be in flight.
    writer_task: std::sync::Mutex<Option<tokio::task::JoinHandle<SinkWriterOutcome>>>,
}

struct SinkQueue {
    tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    /// Bytes admitted and not yet reported written. Bounded by
    /// DVR_SINK_QUEUE_BYTES, so N sinks cost at most N × that, and the
    /// buffers themselves are the shared refcounted tuner chunks.
    queued_bytes: Arc<AtomicU64>,
}

/// ≈ 3.3 s of a 19.4 Mbit/s ATSC 1.0 mux. Long enough to ride out one
/// filesystem hiccup, short enough that a dead NAS is declared within a
/// few seconds instead of after 30.
const DVR_SINK_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
/// Chunk slots; the byte bound is the one that matters, this stops a
/// pathological stream of 1-byte chunks from allocating unbounded slots.
const DVR_SINK_QUEUE_CHUNKS: usize = 512;
```

The writer task owns the `tokio::fs::File` (still opened `create_new` by
`attach_sink` before the sink is published) and loops: `rx.recv()` →
`timeout(TUNER_READ_TIMEOUT, file.write_all(&bytes))` → on `Ok` subtract
from `queued_bytes`, bump `sink.bytes`, call `successful_write` (the
observation and `delivered` accounting move here unchanged; `delivered`
stays the transport's counter). On `Err` or timeout it records the same
`capture_interrupted` event with the same reason codes
(`disk_write_failed`, `disk_write_timeout`), cancels the sink, drains and
drops the queue, and returns `SinkWriterOutcome::Failed(reason)`. On
channel close (`stop_sink`) it `flush`es, `sync_all`s and returns
`SinkWriterOutcome::Closed { bytes }`.

The fan-out per chunk becomes, for each in-window live sink:
`queue.tx.try_send(bytes.clone())`; on `Full` or when
`queued_bytes + len > DVR_SINK_QUEUE_BYTES`, it records
`capture_interrupted` with a **new** reason code `disk_write_backlog`,
cancels the sink and drops the queue. No await on any sink; the loop's only
awaits are the tuner read and the fence check, as in `pump_tuner_stream`.

Overflow policy, stated: a sink that cannot keep up **ends its attempt**;
it does not skip chunks inside one attempt file. A silently dropped chunk
would produce an MPEG-TS discontinuity inside a file that claims to be
continuous; an ended attempt produces a gap the row and sidecar already
know how to name, and `dvr_recover` opens the next attempt on the next tick
while `now < capture_end - DVR_MIN_USEFUL_S`. "Rolls its own attempt" is
therefore the existing recovery path, not a new one.

Cancellation policy, stated: `sink.cancel` closes the queue (the sender is
dropped); the writer finishes the write it is in — a blocked `write(2)` on
a stalled mount cannot be interrupted from user space, and the timeout
future being dropped does not unblock the pool thread — then exits.
`stop_sink` and `close_transport` await `writer_task` under
`SESSION_DRAIN_TIMEOUT`; on timeout the attempt is marked *unsettled*
(`DvrSinkObservation.phase = "settling"`) and `finish_row` **does not
concatenate** that attempt on this tick; the next `dvr_tick` retries the
join, so an attempt file is never read while a write to it may still land.
`begin_finishing` and the event-drain ordering (`:1531-1559`) are
unchanged.

Fencing, unchanged: the per-chunk `serving.is_current(...)` check stays
ahead of every `try_send`; `drain_before` still closes transports by
`generation`; `close_transport` still cancels sinks before joining the
worker; attempt numbers stay per-sink and files stay `O_EXCL`.

### 3.4 Metric

`plurx_dvr_sink_failures_total{reason}` with `reason` ∈
`{disk_write_failed, disk_write_timeout, disk_write_backlog}` — three
labels, all fixed strings, emitted from the writer task and the fan-out.
Reported next to the existing Live TV block in `LiveTvMetrics::prometheus`
(`live_tv.rs:~4740`). No settings key changes; no recipe identity or cache
digest is involved.

## 4. Guardrails (non-goals)

- **The public and relay bodies stay bounded at 2 MiB.** The assessment's
  disposition is "retain full scheduler input and bounded public
  responses"; `clipped` keeps the bound and both HTTP handlers keep calling
  it. Do not raise `MAX_GUIDE_RESPONSE_BYTES` or `MAX_SNAPSHOT_BYTES` as a
  shortcut.
- **No new wire shape.** `LiveTvGuide` is still both the public document
  and the relayed one; `GuideView` never leaves the process.
- **The scheduler never fetches.** `view` reads the cache; the refresh loop
  is the only fetcher (guide.rs module doc, `live_tv.rs:1931-1936`).
- **"Unavailable" still means "cannot tell".** `dvr_reconcile` and the
  reminder sweep act on `None`/empty by doing nothing; the `Withdrawn`/
  `Stale` transitions require a populated view.
- **`(channel_id, airing_start)` identity is untouched.** The binary search
  reads `start`/`end`; it never moves one. The persisted-file normalisation
  in §3.2 shortens an earlier row's `end`, never a later row's `start`
  (the `normalise_programmes` rule).
- **One tuner transport per channel.** L10 does not add a second reader,
  a per-sink tuner GET, or a copy of the chunk per sink; queues hold
  refcounted `Bytes`.
- **No in-file gap.** Overflow ends the attempt (§3.3); it never drops a
  chunk and keeps writing the same file.
- **Concurrency is added only after overflow and cancellation are defined**
  (§3.3), as L10's remedy requires; the writer task and the queue land in
  the same PR as those definitions.
- **Not in this plan:** L4's viewer sharing of the transport (its own
  design doc), the guide's fetch strategy, `DVR_TICK`, retention, sidecar
  content.

## 5. Milestones

### 5.1 M1 — `GuideView`, binary-search clip, scheduler on the view (L1)

Files: `live_tv.rs` (`CachedGuide`, `GuideCache::{read,view,owner_copy,
store}`, persisted load, `local_guide_view`), `live_tv/guide.rs`
(`programmes_in`, `clipped`, sortedness check), `live_tv/dvr.rs`
(`dvr_expand`, `dvr_reconcile`, `reminder_tick`).

Tests (all in `live_tv.rs`'s and `dvr.rs`'s existing `mod tests`):

- `the_scheduler_sees_an_airing_the_public_clip_drops`: build a guide with
  `guide_with(64, 700, 200)`-scale rows so that `serde_json::to_vec` of the
  14-day window exceeds `MAX_GUIDE_RESPONSE_BYTES`; place one programme with
  a distinctive title at `now + 13 * 86_400` on channel 63 (the furthest
  channel, so it is among the first rows the clipper pops); a rule matching
  that title; run `dvr_expand`; assert a `Rule` row exists with
  `airing_start == now + 13 * 86_400`. Also assert `local_guide(...)` with
  the same window does **not** contain that programme — the test must
  demonstrate the clipped/unclipped difference, not trust the file size
  (§8 of the review).
- `already_scheduled_late_rows_survive_reconciliation_of_an_oversized_guide`:
  same fixture, a `PENDING` rule row for the day-13 airing; run
  `dvr_reconcile`; assert it is still `PENDING`, not `Withdrawn`.
- `a_refresh_landing_mid_tick_does_not_change_the_snapshot_the_tick_uses`:
  take a `view`, call `guide_cache.store(generation, seq+1, &Ok(other))`,
  assert the held `Arc` still has the old `fetched_at` and rows, and a new
  `view` has the new ones; `Arc::ptr_eq` on the two is `false`.
- `programmes_in_matches_the_linear_window_predicate` (property-style,
  `live_tv.rs`):
  for random sorted non-overlapping rows and random windows,
  `programmes_in` equals the `retain` predicate's result; and an unsorted
  persisted document is normalised on adoption (assert the warn path via
  the returned rows being sorted).
- `clipped_runs_outside_the_guide_mutex` is covered structurally: `read`
  clones the `Arc` and every cache-derived scalar, explicitly drops the
  guard, and only then calls `clipped`. The concurrent timing form was not
  retained because it would assert scheduler timing rather than lock
  ownership and could pass or fail with unrelated runtime load.

Acceptance: `cargo test -p plurxd dvr_expand` and
`cargo test -p plurxd guide` green; `make unit` green; the day-13 test
fails on `88a3957a` when the scheduler edit is reverted (verify by
reverting `dvr_expand` locally once, note the failure text in the PR body).

### 5.2 M2 — per-sink writer task, bounded queue, overflow and settle (L10)

Files: `live_tv/dvr.rs` (`DvrSink`, `SinkQueue`, writer task,
`pump_tuner_fanout`, `attach_sink`, `stop_sink`, `close_transport`,
`finish_row` settle check), `live_tv.rs` (metric line).

Injection seam for tests: `attach_sink` opens the file and hands the
writer task a `Box<dyn AsyncWrite + Send + Unpin>`; production passes the
`tokio::fs::File`, tests pass the write half of `tokio::io::duplex(n)`
whose read half is drained slowly or not at all. `LiveTunerInput` is a
`BoxStream`, so the tuner side is an in-memory stream of fixed 188×N-byte
chunks at a set cadence. No trait, no `cfg(test)` field on the shipping
struct (§4.2 of the review).

Tests (`dvr.rs` `mod tests`):

- `a_slow_sink_never_delays_the_tuner_reader_or_its_sibling`: two sinks
  on one transport, sink B's duplex read half never drained; feed 64 chunks
  of 64 KiB at 10 ms; assert every chunk reached sink A (`bytes == 4 MiB`)
  within 1 s, the tuner stream was consumed at its own cadence (the
  in-memory stream records the instant each chunk was pulled; max gap
  < 100 ms), sink B is cancelled with `reason_code == "disk_write_backlog"`
  exactly once, and `plurx_dvr_sink_failures_total{reason="disk_write_backlog"}`
  is 1.
- `a_failing_sink_records_one_named_interruption`: an injected writer error
  proves one `capture_interrupted` event and one fixed-cardinality
  `disk_write_failed` increment. Attempt-2 creation remains exercised by the
  existing recovery path; the plan's fleet prompt is the acceptance proof for
  a real failed mount, durable `gap_s`, and `.a2.part` together.
- `queues_stay_bounded_under_a_stalled_writer`: one sink, writer blocked;
  feed 32 MiB; assert `queued_bytes <= DVR_SINK_QUEUE_BYTES` at every step
  and the process's retained chunk count never exceeds
  `DVR_SINK_QUEUE_CHUNKS + 1`.
- `stop_waits_for_the_writer_and_never_concatenates_an_unsettled_attempt`:
  writer stalled in a write; `stop_sink` returns after
  `SESSION_DRAIN_TIMEOUT` with the sink `settling`; `finish_row` leaves
  `.a1.part` in place and the row in `Recording`/finishing; release the
  writer; next tick concatenates.
- `a_fenced_transport_still_cancels_every_sink_before_exiting`: flip the
  serving generation mid-stream; assert each sink's `cancel` is set before
  `pump_tuner_fanout` returns `OwnerUnavailable`, and each writer task has
  joined (no write lands after the fence).
- `two_overlapping_recordings_get_identical_bytes_in_their_overlap`:
  windows `[0, 120)` and `[100, 220)` on one transport; the bytes written
  between 100 and 120 are byte-identical in both files (the existing
  correctness property the sequential loop had — keep it).

Acceptance: `cargo test -p plurxd pump_tuner_fanout` and
`cargo test -p plurxd sink` green; `make unit` green; PR body quotes the
slow-sink test's measured max tuner-pull gap.

### 5.3 M3 — fleet observation (docs + metrics, no code beyond a status line)

Add the metric to [OPERATIONS.md](../OPERATIONS.md)'s metrics table and a
row to [LIVE-TV-DVR-STATUS.md](LIVE-TV-DVR-STATUS.md) recording the L1
finding and its fix date. The documentation index must be green. The GPT
prompt in §6.3 remains explicitly pending until a fleet-capable session can
produce its byte counts and booleans; absence of that evidence does not turn
an unrun observation into a passing result.

## 6. Verification and rollout

### 6.1 Commands

| Milestone | Focused | Gate |
|---|---|---|
| M1 | `cargo test -p plurxd dvr_expand`, `cargo test -p plurxd guide` | `make unit` |
| M2 | `cargo test -p plurxd pump_tuner_fanout`, `cargo test -p plurxd sink` | `make unit` |
| M3 | `python3 -m pytest tests/operations/test_docs_index.py` | — |

Post-base-merge Rust-tree evidence at `da578b7c` used Rust 1.97.1:
`cargo fmt --all -- --check`, `cargo check -p plurxd --all-targets`, and
`cargo clippy -p plurxd --all-targets -- -D warnings` are green; the 19-test
`live_tv::dvr::tests::` module plus the three exact guide-view regressions are
green; and `python3.12 -m unittest tests.operations.test_docs_index` runs the
same four index contracts green. The broad `make unit` qualification is
deliberately deferred until P-01 lands, per the coordinating constraint; it is
not represented here as passing evidence.

### 6.2 Rollout

One whole-plan draft PR into `main` under the fast lane, as the work board now
requires. No feature switch: M1 changes an internal read, M2 changes an internal write path;
neither has a user-visible mode to toggle, and Paul refuses in-code gates.
Deploy to the tuner owner (`media1`) first with the ansible playbook, watch
`plurx_dvr_sink_failures_total` for one recording, then the rest of the
fleet. Rollback is the previous image tag (OPERATIONS.md §308 immutable
`sha-<12hex>` tags).

Mixed-fleet safety: the guide wire shape is unchanged; a new owner relays
the same JSON to an old ingress and vice versa. DVR state rows and attempt
files are unchanged; an old owner taking over from a new one reads the
same `.aN.part` files.

### 6.3 What only the fleet can prove — GPT prompt

```
On media1 (the HDHomeRun owner), with Live TV guide_hours >= 312 and a
DVR series rule that matches a programme airing 10+ days out:
1. Record the size of `<runtime cache root>/live-tv/guide.json` (the
   root Settings → Developer → Live TV reports as the guide store) and
   `curl -s -H "Authorization: Bearer $T" \
     'http://10.42.0.11:8080/api/v1/live-tv/guide?hours=336' | wc -c`.
2. From the same guide, find one airing >= 10 days out whose title the
   rule matches. Report: is it present in the curl body (public clip)?
   Is a DVR row for it present in `GET /api/v1/dvr/schedule?days=14`
   within one DVR tick (15 s) of enabling the rule?
3. Repeat step 2 before and after deploying the M1 build. Before: expect
   the public body to omit it AND the row to be absent (the defect).
   After: expect the public body may still omit it AND the row is present.
Report the four booleans and the byte counts; do not paraphrase.
```

```
On media1 with two recordings scheduled to overlap on one channel and the
DVR root on the NAS mount:
1. During the overlap, pause the NAS export for 20 s
   (`ssh nas 'sudo exportfs -u 10.42.0.11:/volume1/dvr'`, then re-export).
2. Report from /metrics before and after:
   plurx_dvr_sink_failures_total{reason=...} per reason.
3. Report from `GET /api/v1/dvr/recordings/<id>` for both rows: attempt
   count, gap_s, state. Expect: both rows rolled to attempt 2 with a gap
   (both sinks share the mount, so both fail) — the point is that the
   tuner transport did not fail: the Live TV owner log must show no
   "the HDHomeRun stream stopped producing bytes" line in the window.
4. Repeat with one recording on the NAS root and one on local disk (set
   dvr.root, schedule, then change it back — two roots need two runs).
   Expect only the NAS row to roll; the local row's gap_s stays 0.
```

## 7. Open questions

1. `DVR_SINK_QUEUE_BYTES = 8 MiB` is sized to an ATSC 1.0 mux. ATSC 3.0
   channels on the FLEX 4K can carry more; if a healthy NAS write burst
   exceeds 3 s in the fleet, raise it with the measurement in the PR. Not a
   setting — a viewer never tunes it.
2. Whether the `settling` phase should surface on the Activity card
   (probably: "finishing — waiting for the disk") is a
   [DVR-VISIBILITY-IMPLEMENTATION.md](DVR-VISIBILITY-IMPLEMENTATION.md)
   question; this plan only adds the phase string.
3. The relay's `MAX_SNAPSHOT_BYTES` equals `MAX_GUIDE_RESPONSE_BYTES` by
   coincidence of value, not by construction. M1 adds a `const_assert!`
   (or a unit test) that they are equal, because the relay's correctness
   depends on the owner's clip fitting the ingress's bound.

---

## Execution log

Executing sessions append one row per milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | #407 / `9a4b83af` | Full cached guide is an immutable `Arc` view; scheduler, reconciliation and reminders use binary window slices while HTTP clipping stays bounded. Day-13 and snapshot-focused regressions green on Rust 1.97.1. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | #407 / `46a1aeec`, `f2127e1a` | Per-sink 8 MiB/512-chunk queues and owned writers isolate backlog/failure, retain attempt settlement, and emit three fixed reasons. Focused slow/failing/fenced/overlap/settling regressions and scoped Clippy are green. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 | #407 / `d7118e91` | Metric/operator/status contracts recorded; all four docs-index contracts green. Fleet prompts remain `needs: media1 336-hour guide and mixed NAS/local sink interruption evidence`. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | Sole review findings | #407 / `6837d56b` | Comment #3174 resolved: finalization fences later attempts and joins every writer through the exact row attempt; sink-local failure evidence suppresses a second `worker_lost` event; the day-13 reminder consumer now has executable full-guide coverage. All 19 DVR tests and the three exact new regressions are green on Rust 1.97.1; broad unit remains pending. |
