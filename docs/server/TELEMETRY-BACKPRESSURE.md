# Telemetry backpressure — one bounded queue, one supervised writer, and counters that survive retention being off

**Status:** ready for review · **Executes:** C15 (§3.3.4) from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **C-06**. Read §2 first: it quotes `emit_with_network` as it is,
names the two consistent reads it performs per event, and shows that the
writer it feeds is one `Mutex<Connection>` shared with the fragment
indexer — which is the contention the finding is really about. Then build
§5 in order: M1 (the queue and the writer) is the structural change and
everything else attaches to it; M2 (cached settings) cannot land first
because it needs a task to own the refresh; M3 (counters out of the
retention branch) is two lines and one test but must come **after** M1 so
that the metric is recorded on the emit side of the queue; M4 (policy,
drops, drain) closes it. One draft PR per milestone into `main` under the
fast lane. Every `file:line` is from `0f02b7ea`; re-verify by function
name.

**If a step seems to require replicating a playback event, making `emit`
async or fallible at its call sites, letting a terminal outcome be
coalesced or dropped ahead of a sample, blocking shutdown on a wedged
sidecar, or coupling the retention switch to the network-prior switch,
stop and flag it.**

**Correction to the review:** two.

1. C15 has **no row in the assessment.** It is a revision-3 addition from
   Astra's review (§0, "Added from Astra's review"), and the assessment was
   written against the first draft. The required dispositions are §3.3.4's
   acceptance sentence — "inject a slow/failed writer during playback;
   queue depth and memory stay bounded, playback stays responsive, drops
   are counted; retention, shutdown and network-prior opt-ins remain
   independent" — plus the remedy line in the C15 row. §4 answers each.
2. The review cites `telemetry.rs:336-360`. At `0f02b7ea` the function
   begins at `:336` and the body runs to `:373`; the settings reads are
   `:342-359`, the metrics/insert branch `:361-367`, the prior branch
   `:368-372`. More importantly the review's "individual insert on the
   serialised node-local writer" understates it: on the hiqlite backend the
   *settings* reads are `query_consistent_map` (`hiqlite.rs:3453-3461`),
   so each event costs **two leader round trips before it writes anything**
   — and on the client-ingest path there is already one `tokio::spawn`
   (`http/system.rs:882`) before `emit_with_network` spawns a second. The
   per-event cost is therefore two tasks, two consensus reads and one
   blocking-pool hop behind a contended mutex.

---

## 1. Objective

1. `emit` never spawns a task, never reads a setting, and never touches the
   store. It records the bounded metric and enqueues, and it returns in
   time that does not depend on storage.
2. Exactly one supervised writer drains a bounded queue in batches, so the
   sidecar's single connection is acquired once per batch instead of once
   per event.
3. The effective retention and network-prior settings are read on a bounded
   schedule, not per event, and the operator's own change on this node takes
   effect at once.
4. `/metrics` counters are produced whether or not raw events are retained.
   Turning retention off stops rows landing on disk; it does not blind the
   node.
5. When the queue is full, what is dropped is chosen, counted and visible —
   and a terminal outcome is never the thing that loses.
6. Shutdown drains for a bounded time and then gives up, counting what it
   gave up.
7. None of it leaves the node.

## 2. Contract today

Re-verify at build time.

### 2.1 `emit_with_network`

`crates/plurxd/src/telemetry.rs:336-373`:

```rust
pub(crate) fn emit_with_network(
    store: Arc<dyn Store>,
    event: PlaybackEvent,
    network: Option<NetworkIdentity>,
) {
    tokio::spawn(async move {
        let telemetry_enabled = store
            .get_setting(keys::TELEMETRY_RETAIN_DAYS)
            .await.ok().flatten()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(keys::TELEMETRY_RETAIN_DEFAULT_DAYS) > 0;
        let priors_enabled = if network.is_some() {
            store.get_setting(keys::PLAYBACK_NETWORK_PRIORS)
                .await.ok().flatten()
                .is_some_and(|value| value.trim() == "1")
        } else { false };

        if telemetry_enabled {
            METRICS.record(&event);                                   // :362
            if let Err(error) = store.record_playback_event(&event).await {
                tracing::warn!(%error, event = %event.event, "recording playback telemetry failed");
            }
        }
        if priors_enabled {
            if let Some(observation) = prior_observation(&event, network.as_ref()) {
                if let Err(error) = store.observe_network_prior(&observation).await { … }
            }
        }
    });
}
```

`METRICS.record` at `:362` is **inside** the retention branch. That is the
"disabling retention disables the metrics" defect, and it is one line.

### 2.2 The two settings reads are consensus reads

`crates/plurx-core/src/store/hiqlite.rs:3453-3461`:

```rust
async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
    let sql = "SELECT value FROM settings WHERE key = $1";
    validate_sql(sql)?;
    let rows = self.client()
        .query_consistent_map::<SettingValueRow, _>(sql, params!(key))
        .await?;
    Ok(rows.into_iter().next().map(|row| row.value))
}
```

`get_setting_pair` (`:3476-3497`) exists and answers both keys in **one**
consistent read. Nothing calls it from here today.

### 2.3 The writer is one mutex, shared with the fragment indexer

`record_playback_event` on hiqlite is `self.telemetry.record(event.clone())`
(`hiqlite.rs:3247-3249`), and `NodeLocalTelemetry::with_conn`
(`store/telemetry.rs:526-540`) is:

```rust
let conn = Arc::clone(&self.conn);
tokio::task::spawn_blocking(move || {
    let guard = conn.lock()
        .map_err(|_| StoreError::Task("telemetry sidecar mutex poisoned".to_owned()))?;
    operation(&guard)
})
.await
.map_err(|error| StoreError::Task(error.to_string()))?
```

The same `self.conn` serves `put_fragment_index`, `fragment_index`,
`record_fragment_index_typed_outcome`, `put_rendition_plan` and the prior
writes (`:607-700`). The module header (`:1-9`) states why these three
subjects share one node-local file. So a telemetry burst and an index build
contend for one lock, which is exactly the "compete with fragment-index
work on the same serialised store" in §3.3.4. On the standalone backend the
insert is `SqliteStore::with_conn` on the main database
(`sqlite/telemetry.rs:10-14`) — same shape, different file.

### 2.4 Who produces events

Eight call sites (`grep -rn "telemetry::emit" crates/plurxd/src`):
`state.rs:6034, 8842`; `vodserve.rs:2560, 2600`; `http/hls.rs:5824`;
`transcode.rs:6932, 9502`; and the client ingest at `http/system.rs:1001`
through `emit_client_playback_event` (`:987-1001`). The ingest path already
spawns its own task first (`:882-903`) so it can join session truth before
emitting.

`http/hls.rs:5824` is the **terminal** producer. Its own comment
(`:5801-5810`) states the contract this plan must keep: "a telemetry write
that fails must not turn a terminal the viewer's client is waiting on into
an error. Losing the row is worse than not having it only if the
alternative is losing the terminal."

### 2.5 The two settings

`crates/plurx-core/src/store/mod.rs:1584-1601`:

```rust
/// Node-local playback telemetry retention, in days. Missing means
/// [`TELEMETRY_RETAIN_DEFAULT_DAYS`]; `0` disables both writes and pruning.
pub const TELEMETRY_RETAIN_DAYS: &str = "telemetry.retain_days";
/// Opt-in node-local network history used to seed Auto quality. Missing
/// and every value other than `"1"` are off.
pub const PLAYBACK_NETWORK_PRIORS: &str = "playback.network_priors";
pub const TELEMETRY_RETAIN_DEFAULT_DAYS: i64 = 30;
```

Both are written by the settings handler at `http/system.rs:3471-3485`,
through `put_setting` — a replicated write on hiqlite, so a change made on
one node reaches the others by replication and not by notification.

## 3. Change

### 3.1 Shape

```text
 producers (8 call sites)                       one writer task
 ───────────────────────                        ───────────────
 emit(store, event)                             loop {
   │                                              batch = recv_many(64, 200ms)
   ├─ METRICS.record(&event)   ◀── always          coalesce samples
   │                                               if retain { one txn insert }
   └─ sink.try_send(Job{class,event,network})      if priors { fold observations }
        │        │                                 refresh settings if > 30 s
        │        └─ full ▶ drop policy (§3.4)    }
        ▼                                             │
   mpsc::channel(1_024)  ─────────────────────────────┘
        │
        └── 128 slots reserved for Terminal
```

### 3.2 M1 — the bounded queue and the supervised writer

```rust
pub(crate) enum EventClass { Terminal, Lifecycle, Sample }

struct Job {
    class: EventClass,
    event: PlaybackEvent,
    network: Option<NetworkIdentity>,
}

const QUEUE: usize = 1_024;
const TERMINAL_RESERVE: usize = 128;
const BATCH: usize = 64;
const BATCH_WINDOW: Duration = Duration::from_millis(200);
```

`QUEUE = 1_024`: a `PlaybackEvent` with its `extra` JSON (the block built
at `http/system.rs:938-983` is ~1 KB of scalars) is a few kilobytes at
worst, so the ceiling is single-digit megabytes — small enough to be
uninteresting and large enough to absorb a 200 ms sidecar stall at the
fleet's observed event rate. That rate is currently unmeasured; M1 ships
`plurx_telemetry_enqueued_total` so the next revision of this constant has
a number behind it.

`BATCH_WINDOW = 200 ms`: the ceiling on how long a terminal row waits
before it is on disk. Terminals are what an operator reads after a
restart (`http/hls.rs:5801-5804`), so the window is the durability lag
this plan accepts, stated rather than implied.

The sink lives in `AppState`; `emit`/`emit_with_network` keep their exact
signatures and stay infallible and synchronous, so no call site changes
beyond dropping the now-unused `store` argument where the sink already
holds it. **`emit` keeps taking `Arc<dyn Store>`** in M1 even though the
sink owns one — removing the parameter touches eight call sites and two
test helpers and is cosmetic; do it in M4 or not at all, so M1's diff is
the mechanism only.

**The writer.** One `tokio::spawn` started in `boot()` beside
`spawn_background_loops` (`main.rs:1483`), holding `Arc<dyn Store>`, the
receiver, and the cached settings cell. It uses
`Receiver::recv_many(&mut buf, BATCH)` with a `BATCH_WINDOW` timeout,
writes the batch through one new store call, then loops.

**One batch, one connection lease.** New trait method beside
`record_playback_event`:

```rust
/// Persist several events in one connection lease. The sidecar's single
/// mutex is the contended resource (store/telemetry.rs:526), so the win
/// is one acquisition per batch, not fewer statements.
async fn record_playback_events(&self, events: &[PlaybackEvent]) -> Result<u64, StoreError>;
```

Both backends implement it as one `with_conn` that opens a transaction,
reuses one prepared statement over `crate::store::telemetry::insert`, and
commits. `record_playback_event` stays and delegates to a one-element
slice, because the admin test surface and the sidecar tests use it.

**Supervision.** The writer's `JoinHandle` is held by a small watchdog
task. If the writer returns or panics, the watchdog logs at `error` with
the reason and restarts it, at most `WRITER_RESTARTS_PER_HOUR = 6` in a
rolling hour. Past that the sink enters `degraded`: `Sample` and
`Lifecycle` jobs are refused at the door and counted
(`reason="writer_degraded"`), `Terminal` jobs are still queued against the
reserve so that a node whose sidecar is broken still records *what
happened* as far as it can, and the condition warns once every ten minutes.
Six restarts an hour is the line between "a transient lock poisoning" and
"this node's sidecar is broken"; unbounded respawn would turn a poisoned
mutex into a hot loop.

### 3.3 M2 — cached effective settings with a bounded refresh

```rust
struct EffectiveSettings { retain_days: i64, priors: bool, read_at: Instant }
const SETTINGS_REFRESH: Duration = Duration::from_secs(30);
```

Held in an `arc_swap`-style `RwLock<Arc<EffectiveSettings>>` on the sink.
Seeded once in `boot()` **before the listener accepts**, so the first event
is never decided by a default. Refreshed by the writer task, at most once
per `SETTINGS_REFRESH`, through **`get_setting_pair(TELEMETRY_RETAIN_DAYS,
PLAYBACK_NETWORK_PRIORS)`** (`hiqlite.rs:3476`) — one consistent read for
both keys instead of two per event. A failed refresh keeps the previous
value, increments
`plurx_telemetry_setting_refresh_failures_total`, and never blocks a
batch: a store that cannot answer must not also stop the writer.

30 s is the staleness this plan accepts for a *peer's* change. A retention
toggle is an operator action, and half a minute of continued recording
after it is indistinguishable from immediate to everyone except a test.

**The local change is immediate.** The settings handler
(`http/system.rs:3471-3485`) calls `state.telemetry.invalidate_settings()`
after the two `put_setting` calls return, which clears `read_at` so the
next batch re-reads before it writes. This is what keeps the existing
`disabled_telemetry_proof` behaviour (`http/mod.rs:6763-6790`) correct: the
test sets `telemetry_retain_days: 0` and then asserts the next event is not
readable back. Without the poke it would race a 30 s window. The poke is
local only — a peer's change still takes up to 30 s, and §7.2 asks whether
that is worth a notification.

### 3.4 M4 — coalesce, drop, count, drain

**Classification** is a pure function beside `PlaybackMetrics::record`, so
it is testable without a store:

```rust
fn classify(event: &PlaybackEvent) -> EventClass {
    // The four names `durable_outcome` can produce (playback_control.rs:
    // 13960-13983) — the statements of what happened that hls.rs:5801 says
    // are the thing most worth explaining after a restart.
    match event.event.as_str() {
        "control_terminal" | "control_retry_resource"
        | "control_hold_withheld" | "control_action_suppressed" => EventClass::Terminal,
        "ttff" | "session_start" | "stall" | "stall_recovery"
        | "suspend" | "resume" => EventClass::Lifecycle,
        // Periodic snapshots: `producer_candidates` (state.rs:6039),
        // `producer_pass` (state.rs:8849), the VOD samples at
        // vodserve.rs:2560 and transcode.rs:6932/9502.
        _ => EventClass::Sample,
    }
}
```

Two rules make this list maintainable rather than a copy that rots. First,
an event whose `level` is `"error"` is `Terminal` regardless of its name —
`durable_outcome` sets `level: "error"` for `ControlTerminal`
(`playback_control.rs:13961-13964`) and the client ingest path already
escalates `level == "error"` to `tracing::error!`
(`http/system.rs:840-845`), so the field is already load-bearing. Second,
M4's first step is a test that enumerates `durable_outcome`'s match arms
and asserts every one of them classifies `Terminal`, so adding a fifth arm
without adding it here fails the build rather than silently demoting a
terminal to a sample.

**Admission.** `Terminal` may use the whole queue. `Lifecycle` and
`Sample` are refused once `len() > QUEUE - TERMINAL_RESERVE`. The reserve
is what "room reserved for terminal outcomes" means in the remedy.

**Coalescing** happens in the writer, over the batch it has just taken, not
in the queue: consecutive `Sample` jobs with the same
`(session_id, event)` collapse to the newest, and the collapsed ones count
as `reason="coalesced"`. A sample is a snapshot of a session's state, so
the newest one is the true one; a `Lifecycle` or `Terminal` is a statement
about a moment and is never collapsed. Coalescing in the writer rather
than at enqueue keeps `emit` free of a map lookup.

**Drops are counted, with the reason:**

| Metric | Labels |
|---|---|
| `plurx_telemetry_enqueued_total` | `class="terminal\|lifecycle\|sample"` |
| `plurx_telemetry_dropped_total` | `reason="queue_full\|coalesced\|writer_degraded\|shutdown"` |
| `plurx_telemetry_written_total` | `outcome="ok\|error"` |
| `plurx_telemetry_queue_depth` | none (gauge) |
| `plurx_telemetry_batch_size` | none (histogram; 1,2,4,8,16,32,64,+Inf) |
| `plurx_telemetry_batch_seconds` | none (histogram; 0.001,0.01,0.05,0.25,1,5,+Inf) |
| `plurx_telemetry_setting_refresh_failures_total` | none |

Three classes × one label each: twelve series in total, all fixed. No
session ids, no file ids, no user ids, no paths — the same rule
[OPERATIONS.md](../OPERATIONS.md) §"Health & metrics" already states for
the cluster counters.

**Shutdown.** `const DRAIN: Duration = Duration::from_secs(2);` On the
drain signal the sink refuses `Sample`, the writer finishes the batch it
holds and drains the queue for at most `DRAIN`, then counts the remainder
as `reason="shutdown"` and returns. Two seconds sits inside the 10 s
restart-preparation window `main.rs:1505-1512` already opens before Live TV
shutdown, so this adds no wall time to a deploy. It is bounded because an
unbounded drain against a wedged sidecar would hold the process open
exactly when an operator is trying to restart it.

### 3.5 M3 — counters independent of retention

`METRICS.record(&event)` moves out of the retention branch and onto the
**emit** side, before the enqueue, in `emit_with_network` itself. Two
consequences, both wanted: a dropped event still counts, so
`plurx_ttff_ms` and friends describe what the node observed rather than
what it managed to store; and the metric is recorded on the caller's
thread, which is a handful of relaxed atomic adds
(`telemetry.rs:61-152`) and cannot block.

This is the one change that makes the three switches genuinely
independent:

| Switch | What it governs after this plan |
|---|---|
| `telemetry.retain_days` | Rows in `playback_events`, and the prune pass. Nothing else. |
| `playback.network_priors` | The `network_priors` fold only. |
| shutdown drain | How long the writer is given, and nothing about either switch. |
| (none) | `/metrics` counters — always on, bounded, node-local. |

## 4. Guardrails (non-goals)

- **Bounded queue and bounded memory** (§3.3.4). `mpsc::channel(1_024)`
  with a stated per-job size; `plurx_telemetry_queue_depth` makes the
  bound observable; M4's acceptance asserts memory does not grow under a
  writer that never returns.
- **Playback stays responsive** (§3.3.4). `emit` does no I/O, takes no
  lock that a store can hold, and returns after a `try_send`. M1's test
  asserts a bound on `emit`'s wall time with a writer that sleeps forever.
- **Drops are counted** (§3.3.4). Four reasons, one counter, never a
  silent discard. A refusal also never produces an error at a call site —
  `emit` stays infallible, because `http/hls.rs:5801-5810` says a
  telemetry failure must not become a viewer-facing error.
- **Terminal outcomes have reserved room** (C15 remedy). 128 slots
  `Lifecycle`/`Sample` cannot touch, and `Terminal` is never coalesced.
  Even in `degraded` the reserve still accepts terminals.
- **Retention, shutdown and network priors stay independent** (§3.3.4).
  The table in §3.5 is the contract, and M3/M4 each add a test that toggles
  one and asserts the other two are unchanged.
- **Counters do not depend on raw retention** (C15). §3.5.
- **It stays node-local** (C15 remedy). Nothing here writes through the
  replicated API. `store/telemetry.rs:1-9` explains why these rows are
  node-local; the batch insert is on the same sidecar connection, and the
  only replicated reads left are one `get_setting_pair` per 30 s.
- **The sidecar mutex is shared, and this plan reduces its acquisitions
  rather than claiming to remove the contention.** One lease per batch
  instead of one per event is the whole claim; fragment-index writes still
  take the same lock. Separating the two connections is a different change
  with its own transactional questions and is named in §7.1, not smuggled
  in here.
- **`spawn_blocking` is still not cancellable.** A batch whose insert
  blocks in SQLite holds a blocking-pool thread until it returns. The
  queue bounds what accumulates behind it; the drain bounds what shutdown
  waits for; nothing here interrupts the syscall.
- **No feature gate.** No new setting either: every bound is a constant
  with its reason beside it, and the two existing settings keep their exact
  meanings.

## 5. Milestones

### 5.1 M1 — bounded queue and supervised batching writer (`core/telemetry-queue`)

1. `EventClass`, `Job`, `TelemetrySink` with the queue and the counters;
   `emit`/`emit_with_network` rewritten to record-then-`try_send`;
   the writer task and its watchdog started in `boot()`;
   `record_playback_events` on the trait and both backends.
2. Tests (`telemetry.rs`'s module plus `store/telemetry.rs`'s):
   `emit_returns_without_waiting_for_the_writer` (a store whose
   `record_playback_events` sleeps 10 s; assert 1,000 `emit` calls finish
   under 50 ms in total);
   `the_queue_never_exceeds_its_capacity` (same store; assert depth ≤ 1,024
   and that `plurx_telemetry_dropped_total{reason="queue_full"}` rises);
   `a_batch_takes_one_connection_lease` (count `with_conn` entries through
   a test hook; 64 events, one lease);
   `a_panicking_writer_is_restarted_and_logged`;
   `six_restarts_in_an_hour_degrades_the_sink_and_keeps_terminals`.
3. `record_playback_event` still works and the admin read path
   (`GET /api/v1/system/playback-events`) returns the same rows.

Acceptance: `cargo test -p plurxd telemetry` and
`cargo test -p plurx-core store::telemetry` green;
`grep -n "tokio::spawn" crates/plurxd/src/telemetry.rs` finds only the
writer and its watchdog, not one per event;
`curl -s $HOST/metrics | grep plurx_telemetry_` shows the new families
with the fixed label sets.

### 5.2 M2 — cached effective settings (`core/telemetry-settings-cache`)

1. `EffectiveSettings`, the seed in `boot()`, the 30 s refresh in the
   writer via `get_setting_pair`, `invalidate_settings()` called from
   `http/system.rs:3485`.
2. Tests:
   `settings_are_read_once_per_window_not_once_per_event` (counting store;
   1,000 events, assert ≤ 2 settings reads);
   `a_local_retention_change_takes_effect_on_the_next_batch` (the existing
   `disabled_telemetry_proof` flow, unchanged assertions);
   `a_failed_refresh_keeps_the_last_value_and_counts`;
   `the_seed_runs_before_the_first_event`.

Acceptance: `cargo test -p plurxd telemetry` and the existing
`cargo test -p plurxd -- disabled_telemetry_proof` green;
`grep -n "get_setting(" crates/plurxd/src/telemetry.rs` finds nothing —
the only settings read is the paired one in the writer.

### 5.3 M3 — counters out of the retention branch (`core/telemetry-counters-always`)

1. Move `METRICS.record(&event)` to the emit side, before the enqueue.
2. Tests:
   `metrics_are_recorded_with_retention_off` (set `retain_days = 0`, emit a
   `ttff`, assert `plurx_ttff_ms_count{method="remux"}` rose and
   `playback_events` is empty);
   `a_dropped_event_is_still_counted` (full queue; the metric moves, the
   drop counter moves, no row);
   `network_priors_still_require_their_own_opt_in`.

Acceptance: `cargo test -p plurxd telemetry` green; on a node with
`telemetry_retain_days = 0`, `curl -s $HOST/metrics | grep plurx_ttff_ms_count`
is nonzero after one play, and
`GET /api/v1/system/playback-events?limit=5` is empty.

### 5.4 M4 — coalesce, reserve, drain (`core/telemetry-drop-policy`)

1. `classify` pinned against `playback_control::durable_outcome`'s four
   arms; the terminal reserve; batch coalescing; the drop reasons; the
   bounded shutdown drain wired into the existing drain closure
   (`main.rs:1498-1516`).
2. Tests:
   `every_durable_outcome_arm_classifies_terminal` (the enumeration in
   §3.4);
   `an_error_level_event_is_terminal_whatever_its_name`;
   `a_full_queue_still_admits_a_terminal`;
   `terminals_are_never_coalesced`;
   `consecutive_samples_for_one_session_collapse_to_the_newest`;
   `shutdown_drains_within_its_bound_and_counts_the_rest` (a writer that
   never returns; assert the drain ends at ~2 s and
   `reason="shutdown"` carries the remainder);
   `the_drain_does_not_extend_process_shutdown_beyond_its_bound`.

Acceptance: `cargo test -p plurxd telemetry` green; `make unit` green once
before un-WIP; `systemctl restart plurxd` on `lab1` completes in the same
wall time as before the change (three runs, reported in the PR body).

## 6. Verification and rollout

### 6.1 Lanes

Per milestone the focused `cargo test` above, then `make unit` once before
un-WIP. `make validate-staged` before every push. M4 touches
`main.rs`'s drain closure, so the container smoke lane must be run:
`make container-smoke`. No Node or Python gate is affected; the
`/metrics` families are new, so [OPERATIONS.md](../OPERATIONS.md)'s
"Health & metrics" section gains a row per family in the same PR that adds
it.

### 6.2 The injected-writer acceptance (§3.3.4's sentence)

The unit tests above cover the mechanism. The sentence asks for it *during
playback*, which needs a running node. Two parts:

1. **In-process, deterministic** (M1, M4): a `Store` decorator whose
   `record_playback_events` sleeps or returns `Err`, installed for the
   duration of a test that drives the HLS handler. Assert: queue depth
   bounded, resident growth bounded (compare `Vec` capacities, not RSS),
   the segment handler's latency unchanged within noise, drops counted.
2. **On the fleet** (§6.3): the same condition produced by making the
   sidecar slow for real.

### 6.3 What only the fleet can prove — GPT prompt

```text
On lab1 (10.42.1.11), with the build carrying PR <M4 number>:
1. Start two simultaneous playbacks from two clients (one web, one Apple
   TV) of any two titles. Let both run for two minutes.
2. Record the baseline:
   curl -s http://10.42.1.11:32400/metrics | grep -E 'plurx_telemetry_'
3. Make the sidecar slow. Find the node's active hiqlite directory
   (the path built at crates/plurx-core/src/cluster/migration.rs:579 —
   <active>/telemetry.db) and hold a long write lock on it from a second
   shell:
     sqlite3 <active>/telemetry.db "BEGIN IMMEDIATE; SELECT 1;"
   Leave that shell open for 90 seconds, then type ".quit".
4. During those 90 seconds: seek both players twice each, and note whether
   either picture stalls or the seek takes visibly longer than usual.
   Say exactly what you saw.
5. Every 15 seconds during those 90 seconds run the step-2 command and
   paste the plurx_telemetry_queue_depth and
   plurx_telemetry_dropped_total lines.
6. Also record the process's resident memory each time:
   ps -o rss= -C plurxd
7. After the lock is released, wait 30 s, run step 2 once more, and paste
   it. Then confirm both players are still playing.
8. Finally: systemctl restart plurxd and time it (three runs), and report
   whether the restart took longer than it did before this build.
Report exact values. If queue depth exceeds 1024 at any sample, or RSS
grows by more than 100 MB across the window, stop and paste the last 100
lines of journalctl -u plurxd.
```

## 7. Open questions

1. **A second sidecar connection.** The contention in §2.3 is between
   telemetry writes and fragment-index writes on one `Mutex<Connection>`.
   Batching reduces acquisitions; it does not remove the sharing. Opening a
   second connection to the same SQLite file for telemetry only would, but
   it raises WAL-mode, busy-timeout and cross-table transaction questions
   that this plan is not the place to answer. Decide after M1's
   `plurx_telemetry_batch_seconds` says how long a batch actually waits;
   if the p95 is under 5 ms, there is nothing to fix.
2. **Peer settings changes take up to 30 s.** The local poke (§3.3) covers
   the node the operator used. Whether a cluster-wide notification is worth
   building for a retention toggle is a product call; the alternative — a
   consistent read per event — is what this plan exists to remove.
3. **The terminal event set.** §3.4 names three strings and then says to
   read `playback_control::durable_outcome` instead of trusting them. If
   that function's vocabulary turns out to be open-ended, `classify` should
   key on a field the producer sets rather than on the string, and M4 says
   so in its PR rather than guessing here.
4. **The event rate is unmeasured.** `QUEUE`, `BATCH` and `BATCH_WINDOW`
   are sized by argument, not observation. `plurx_telemetry_enqueued_total`
   and `plurx_telemetry_batch_size` are what a later revision would resize
   them from; until there is a fleet reading, nothing in this plan claims
   the constants are optimal — only that they are bounded and stated.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | Claim | pending | Claimed `plan/C-06` for one four-milestone implementation PR; implementation evidence follows milestone by milestone. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M1 | [PR #434](http://192.168.4.7:3000/noirr/plurx/pulls/434) | Added the 1,024-slot synchronous admission path, single supervised batching writer, one-transaction batch Store contract on both backends, bounded fixed-label queue metrics, boot registration, and an executable 64-row sidecar batch regression. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M2 | [PR #434](http://192.168.4.7:3000/noirr/plurx/pulls/434) | Seeded the paired effective settings before listener acceptance, cached them for 30 seconds, preserved the last good values on refresh failure, and invalidated the cache immediately after either local telemetry setting changes; focused tests cover seed, cache window, and invalidation. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M3 | [PR #434](http://192.168.4.7:3000/noirr/plurx/pulls/434) | Moved bounded playback metric recording ahead of queue admission and retention decisions. The focused regression disables retention, emits TTFF, proves the metric rises, and proves no raw playback row is stored. |
