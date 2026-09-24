# Observability baseline — RED metrics on matched routes, logs a machine can read, and a release-evidence metric set with owners

**Status:** ready for review · **Executes:** C10 / F-core-12 /
F-build-ops-codehealth-12 and the §4.9 release-evidence list from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(assessment rows C10, F-core-12, F-build-ops-codehealth-12 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **C-08**. Read §2 first: it quotes the one HTTP layer that
exists, the logging init as it is, the vendored `tracing-subscriber`
default that puts ANSI escapes into journald, and the house pattern every
metric in this tree follows — fixed atomic cells rendered to text, never a
metrics library. Then build §5 in order. M1–M4 are four small independent
PRs and may run in parallel sessions; **M5 is different** — it is a
mapping and a set of definitions, it produces one table and several small
metric additions, and it is the part §4.9 and §5.3 actually asked for.
Do not start M5 until M1 has landed, because two of its rows are satisfied
by M1's series. One draft PR per milestone into `main` under the fast
lane. Every `file:line` is from `0f02b7ea`; re-verify by function name.

**If a step seems to require an unbounded metric label (a raw URI path, a
session id, a file id, a user id, a title, a node id), a `/metrics` scrape
that reads the Store, an access log at INFO on media routes, logging a
request id a caller supplied without validating it, or a panic hook that
replaces rather than chains the default, stop and flag it.**

**Correction to the review:** three precisions, none of which changes the
finding.

1. C10 says "access log at DEBUG". More precisely: there is no access log
   at all. `TraceLayer::new_for_http()` (`http/mod.rs:630-641`) is given a
   custom `make_span_with` and **nothing else** — no `on_response`, no
   `on_failure`. What exists is a span whose default `on_response` logs at
   `DEBUG`; the span's fields are method, redacted target and version, and
   no status or latency is recorded anywhere. So "no RED metrics" is right
   and "access log at DEBUG" understates it.
2. C10 says "task panics never reach `logbuf`". True, and the reason is
   that `grep -rn "panic::set_hook" crates/plurxd/src` is empty:
   there is no hook to reach it with. The assessment's narrowing is the
   accurate statement — framework panics still reach stderr, and what is
   missing is the in-memory buffer and a counter.
3. The ANSI claim is confirmed against the vendored source, not inferred.
   `tracing-subscriber-0.3.23/src/fmt/fmt_layer.rs:739-743`:
   `let ansi = cfg!(feature = "ansi") && env::var("NO_COLOR").map_or(true,
   |v| v.is_empty());` — no TTY detection anywhere, so a unit under
   systemd gets escape sequences unless the operator sets `NO_COLOR`.

---

## 1. Objective

1. Every HTTP request is counted and timed against its **matched route
   template**, with bounded method and status labels, and response-header
   latency is a different series from body completion.
2. Every request carries an id, minted here or adopted from a caller only
   after validation, present on the span and echoed to the client.
3. Logs can be emitted as JSON, and never carry ANSI escapes into a
   non-terminal.
4. A panic anywhere reaches `tracing::error!` with redaction, a bounded
   backtrace, a reporting path that cannot itself panic, and the default hook
   still running after it — and increments a bounded counter. (Corrected by
   the #461 review: this said "a recursion guard", but std never re-enters a
   panic hook — a nested panic aborts the process — so no guard can protect
   anything; see §3.4.)
5. The eight §4.9 release-evidence measurements each name a real series:
   the one that exists, or the one this plan creates, with labels,
   denominator, observation interval and an owner.
6. §5.3's rule — "the exit criterion adds a counter read off `/metrics` on
   the fleet" — has a written definition an effort can fill in, and one
   worked example.

## 2. Contract today

Re-verify at build time.

### 2.1 The only HTTP layer

`crates/plurxd/src/http/mod.rs:626-645`, the tail of `router()` (`:82`):

```rust
        .nest("/api/v1", api)
        .merge(plex_routes)
        .fallback(web::fallback)
        // Never put capability credentials or query-string tokens in a span.
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    target = %safe_trace_target(request.uri()),
                    version = ?request.version(),
                )
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            cluster_capacity_gate,
        ))
        .with_state(state)
```

`cluster_capacity_gate` is added last, so it is the **outermost** layer; it
refuses learner-ineligible, fenced and maintenance requests with 503
(`:940-963`). Anything that must count those refusals has to sit outside
it.

`safe_trace_target` (`:965-983`) is the existing redaction: it replaces the
segment after `media`, `hls`, `publication`, `sessions` and `starts` with
`[REDACTED]` and **drops the entire query string**, with the reason stated
in the comment — "Intentionally omit the entire query rather than trying to
enumerate every present and future spelling of an access token." That rule
is inherited by everything in this plan.

There are ~190 `.route(...)` registrations in `router()` and its
sub-routers, of which 14 are the Plex façade's absolute paths
(`:452-465`); the methods actually used are GET, POST, PUT, PATCH and
DELETE. Count them at build time rather than trusting this number — M1's
`the_label_space_is_closed` test makes the count an assertion.

### 2.2 Logging init

`crates/plurxd/src/main.rs:1534-1561`:

```rust
fn init_logging() -> logbuf::LogBuffers {
    let logs = logbuf::LogBuffers::default();
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_env("PLURX_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            tracing_subscriber::fmt::layer().with_filter(filter_fn(|metadata| {
                !logbuf::is_cluster_target(metadata.target()) || *metadata.level() <= Level::WARN
            })),
        )
        .with(logbuf::BufferLayer(Arc::clone(&logs.general)).with_filter(…))
        .with(logbuf::BufferLayer(Arc::clone(&logs.cluster)).with_filter(…))
        .try_init()
        .ok();
    logs
}
```

No `.with_ansi(…)`, no format switch, no panic hook. `logbuf::LogBuffer`
(`logbuf.rs:75-118`) is a **bounded ring** with `tail(min_level, limit)`,
which is why §3.3 refuses to raise the access log to INFO: one segment
request per second per viewer would evict everything else from the ring
within a minute, and the ring is what Settings → System → Logs shows.

### 2.3 The metric house pattern

There is no `prometheus` crate. Every metric in the tree is a fixed array
of `AtomicU64` rendered to text. `StoreOperationMetrics`
(`store/hiqlite.rs:629-737`) is the reference implementation: a
`[StoreOperationCell; 9]` indexed by `class.index() * OUTCOMES + outcome.index()`,
rendered as `plurx_store_operation_seconds_bucket{class="…",outcome="…",le="…"}`
with `_sum` and `_count`, plus a `plurx_store_operations_total` counter over
the same cells. `PlaybackMetrics` (`telemetry.rs:28-289`) is the same shape
for playback.

`/metrics` (`http/system.rs:4991-5070`) concatenates those strings and
**must not touch the Store** — pinned by
`prometheus_scrape_has_no_store_operation` (`:5483-5490`), which reads the
handler's own source text. Everything this plan adds obeys that.

### 2.4 What already exists of §4.9's list

`rg 'plurx_[a-z_]+' crates/plurxd/src -o | sort -u` gives ~185 names. The
ones that matter here:

| §4.9 measurement | Closest existing series | Verdict |
|---|---|---|
| first-frame p50/95/99 | `plurx_ttff_ms{method}` histogram, 8 buckets 100→30 000 ms (`telemetry.rs:9, 137-175`) | **Exists.** Needs one more dimension and finer high buckets |
| seek-to-moving-picture | — | **Missing** |
| stalled seconds per playback hour | `plurx_stalls_total{kind}` counts *events*; `plurx_suspended_seconds_total` is **encoder** suspension, a different thing | **Missing** (numerator and denominator) |
| failed starts per attempt | `plurx_sessions_total{encoder}`, `plurx_live_tv_starts_total{outcome}`, `plurx_cache_serves_total{result}` | **Partial**: Live TV has outcomes, finite playback does not |
| replacement failure rate | `plurx_playback_preparation_decisions_total`, `_staged_total{outcome}`, `_cancelled_total{reason}`, `_counterfactual_total` (`playback_control.rs:14198-14260`) | **Exists.** Needs the ratio named, not new series |
| admission wait | `plurx_vod_blocked_gets_waiting` (gauge, `waitpool.rs:216`), `plurx_vod_blocked_get_refusals_total`, `plurx_media_session_takeover_seconds` | **Missing**: gauges and a takeover histogram are not admission wait |
| actual vs reserved scratch | — | **Missing**, and owned elsewhere (§3.5) |
| bytes per watched minute | `plurx_live_tv_relay_bytes_total`, `plurx_offline_transfer_bytes_total` | **Missing** for ordinary playback |

## 3. Change

### 3.1 M1 — RED on the matched route

```rust
// Built once at startup from the router's own route list, so the label
// space is closed: a request that matches no route gets one shared label
// and cannot mint a new series.
struct RouteTable { templates: Vec<&'static str>, index: HashMap<&'static str, usize> }

const UNMATCHED: &str = "<unmatched>";     // no MatchedPath extension
const FALLBACK: &str = "<fallback>";       // web::fallback matched
```

The label is `axum::extract::MatchedPath`, read from the request
extensions after `next.run(request)` — a *template* such as
`/api/v1/items/{id}`, never the concrete URI. That is what bounds the
cardinality, and it also means no redaction is needed for the metric
label: a template contains no id and no token. `safe_trace_target` stays
in charge of the *span*, where the concrete target still appears.

Series, all emitted only for cells that have been observed (a node serves
a few dozen of the ~190 routes, so the scrape stays small):

```text
plurx_http_requests_total{route,method,status="2xx|3xx|4xx|5xx|other"}
plurx_http_request_seconds_bucket{route,le}   + _sum + _count
plurx_http_body_seconds_bucket{route,le}      + _sum + _count
plurx_http_bodies_total{route,outcome="complete|aborted"}
```

Cardinality arithmetic, stated so nobody has to trust it: 192 route labels
(190 templates + `<fallback>` + `<unmatched>`) × 7 methods (the five in
use, plus `head` and `other`) × 5 status classes = 6 720 possible counter
cells, 54 KB of atomics, of which a real node populates a few hundred. The
two histograms are labelled by **route only** — 192 × 10 lines — because a
per-method histogram triples the exposition for a distinction that method
already makes in the counter.

**Header latency versus body completion** (F-core-12, verbatim: "distinguish
response-header latency from streaming-body completion").
`plurx_http_request_seconds` stops when the handler returns its
`Response` — that is the number an operator wants for `/api/v1/items/{id}`
and the number a two-hour direct play would otherwise ruin.
`plurx_http_body_seconds` is recorded by a `http_body_util` wrapper around
the response body, on last frame or on drop, and is the number that
describes delivery. `plurx_http_bodies_total{outcome}` separates a body
that finished from one whose consumer went away, because on media routes a
dropped body is the normal end of a seek and must not read as an error.

Buckets: request `0.005, 0.025, 0.1, 0.5, 1, 5, 30, +Inf`; body
`0.1, 1, 10, 60, 600, 3600, +Inf`. Two scales because the two
distributions are two orders of magnitude apart, which is the whole reason
they are separate series.

**Placement.** The layer is added **after** `cluster_capacity_gate`, so it
is outermost and counts the 503s the gate produces
(`LEARNER_ROUTE_INELIGIBLE_JSON`, `NODE_REMOVAL_FENCED_JSON`,
`NODE_MAINTENANCE_JSON`). A refusal is a request, and a node refusing
everything with an uncounted 503 is the failure mode this metric exists to
show.

### 3.2 M2 — request ids

`tower_http::request_id::{SetRequestIdLayer, PropagateRequestIdLayer}`
with a custom maker:

```rust
const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_REQUEST_ID: usize = 64;

// An id a caller supplied is accepted only if it is a short, boring token.
// Anything else is replaced, not rejected: the id is a correlation aid, and
// refusing a request over it would make a log field load-bearing for
// service. Never log the discarded value — a caller that puts a bearer in
// this header must not get it written to journald.
fn adopt_or_mint(incoming: Option<&HeaderValue>) -> HeaderValue {
    match incoming.and_then(|v| v.to_str().ok()) {
        Some(value)
            if !value.is_empty()
                && value.len() <= MAX_REQUEST_ID
                && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
        {
            HeaderValue::from_str(value).unwrap_or_else(|_| mint())
        }
        _ => mint(),   // uuid v4, simple form
    }
}
```

The id is added as a field on the existing `http_request` span, so every
log line inside a request already carries it through `tracing`'s span
context, and it is propagated to the response so a client (or the web
app's error reporter) can quote it. It is **not** a metric label — that
would be unbounded, which is the single most common way a metrics system
is destroyed.

### 3.3 M3 — log format and ANSI

```rust
// PLURX_LOG_FORMAT=json|text (default text). An environment variable and
// not a replicated setting: this is a property of how this process's
// stdout is consumed, exactly like PLURX_LOG beside it, and a node whose
// journald is scraped by Loki needs it while its neighbour may not.
enum LogFormat { Text, Json }
```

`init_logging` builds the fmt layer twice — once `.json()`, once the
default — behind the switch, and both get:

```rust
.with_ansi(std::io::stdout().is_terminal())
```

The reason, quoted in the code: `tracing-subscriber`'s `Layer::default`
turns ANSI on whenever the `ansi` feature is compiled and `NO_COLOR` is
unset (`fmt_layer.rs:739-743`), with no TTY check, so a systemd unit gets
escape bytes in journald today. `is_terminal()` is the check the library
does not do. Under `json` the flag is forced off regardless, because escape
sequences inside a JSON string field are not a colour, they are a parse
hazard.

The two `logbuf::BufferLayer`s are unaffected — they never format with
ANSI.

**Access logging** stays at DEBUG for successful requests, and gains one
`on_response` that logs at **WARN** for 5xx only, with route template,
status, latency and request id. The reason is `logbuf::LogBuffer`'s bounded
ring (§2.2): an INFO access log on a node serving one HLS segment per
second per viewer evicts the general ring in under a minute, and that ring
is the only log an admin can read from the product. The RED counters carry
the volume; the ring carries the exceptions.

### 3.4 M4 — the panic hook

```rust
thread_local! { static IN_HOOK: Cell<bool> = const { Cell::new(false) }; }
const MAX_PANIC_MESSAGE: usize = 512;
const MAX_BACKTRACE_FRAMES: usize = 32;

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic raised *inside* this hook — a poisoned logbuf mutex, a
        // formatter that panics on third-party payload text — would
        // otherwise recurse until the stack is gone. One guard, checked
        // first, and the default hook still runs.
        let recursed = IN_HOOK.with(|flag| flag.replace(true));
        if !recursed {
            let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
            let message = redact_bounded(panic_message(info), MAX_PANIC_MESSAGE);
            PANICS.record(subsystem_of(location.as_deref()));
            tracing::error!(
                target: "plurxd::panic",
                location = location.as_deref().unwrap_or("unknown"),
                backtrace = %bounded_backtrace(MAX_BACKTRACE_FRAMES),
                "{message}"
            );
            IN_HOOK.with(|flag| flag.set(false));
        }
        previous(info);   // deliberate chaining — stderr and abort behaviour unchanged
    }));
}
```

Four properties the assessment asked for by name:

- **Redaction.** `redact_bounded` is the generalisation of the existing
  `redact_operator_text` (`http/cluster_operations.rs:2495`), which already
  replaces the whole message when it contains `authorization`, `bearer`,
  `token`, `secret`, `password`, `signature`, `api_key`, `username`, the
  join-token prefix, or a path separator, and truncates to 512 characters.
  M4 lifts that function into a shared module and uses it for both callers
  rather than writing a second one — a panic payload is third-party text
  (F-core-12: "including panic hooks and third-party panic text").
- **Bounded backtrace.** `MAX_BACKTRACE_FRAMES = 32`, captured only when
  `RUST_BACKTRACE` is set, each frame's text redacted and truncated.
- **Recursion protection.** The thread-local guard above.
  **Corrected by the #461 review — this bullet and the sketch's comment were
  wrong.** std never re-enters a panic hook: a panic raised while one is
  running is `MustAbort::PanicInHook`, which prints "panicked while processing
  panic" and aborts the process before anything unwinds. The guard's `else`
  branch can never run, and no `catch_unwind` anywhere can intercept the
  abort. What actually protects the process is that the reporting path is
  panic-free by construction (payload only downcast to `&str`/`String`, no
  third-party `Debug`/`Display`, `char`-wise truncation, `get` instead of
  indexing, string-only `tracing` fields). The shipped hook has no guard;
  `panics.rs` documents each step, and
  `a_panic_inside_the_reporting_path_aborts_the_process` pins the abort.
- **Deliberate default-hook chaining.** `previous(info)` always runs, at
  the end, outside the guard. The process's abort/unwind behaviour does not
  change.

`plurx_panics_total{subsystem}` where `subsystem` is derived from the
panic location's first two path components (`crates/plurxd/src/http` →
`http`, `crates/plurx-core/src/store` → `store`) against a fixed
allowlist of the ~12 module names, falling back to `other`. Bounded by
construction; a panic in a dependency reports `other`.

**What this is not.** F-build-ops-codehealth-12 is explicit: "A panic hook
reports failure; it does not ensure a detached task's lifecycle effects are
repaired." A task that panics mid-publication still leaves whatever it
left. M4 makes that visible and counts it; repairing it belongs to the
owning efforts — `spawn` supervision is named in
[TRANSCODE-DECOMPOSITION-PLAN.md](../streaming/TRANSCODE-DECOMPOSITION-PLAN.md)
and in [TELEMETRY-BACKPRESSURE.md](TELEMETRY-BACKPRESSURE.md)'s writer
watchdog, and this plan does not claim either.

### 3.5 M5 — the release-evidence metric set

The mapping. **Segmentation rule first, because it governs every row:**
Prometheus carries the *bounded* series; the node-local sidecar carries the
fine segmentation. `playback_events` already stores `method`, `encoder`,
`height`, `ua`, `session_id`, `file_id` and an `extra` JSON per event
(`store/telemetry.rs:24-51`) and is queryable through
`GET /api/v1/system/playback-events`. So a release comparison segmented by
client × transport × codec × grade × hardware is a **query against those
rows**, and the `/metrics` series carry at most two dimensions each. Trying
to put five dimensions into Prometheus labels would be the unbounded-label
mistake §4.9 itself warns against ("bounded labels and no content
identifiers").

| # | Measurement | Series | Labels | Denominator | Status |
|---|---|---|---|---|---|
| 1 | First frame p50/95/99 | `plurx_ttff_ms` | `method`, **+`client`** | `_count` | Exists; M5 adds `client` (the bounded class `http::network::identity` already derives) and two buckets above 30 s, because a p99 that lands in `+Inf` is not a p99 |
| 2 | Seek to moving picture | `plurx_seek_to_picture_ms` **(new)** | `method` | `plurx_seeks_total{method}` **(new)** | Client-reported event `seek_resumed`, ms from the seek command to the first presented frame. Needs a client change on three platforms — M5 lands the server side and the metric stays at zero until clients emit it, which is honest and visible |
| 3 | Stalled seconds per playback hour | `plurx_stalled_seconds_total` **(new)** | `kind` (the existing four) | `plurx_watched_seconds_total{method}` **(new)** | `plurx_stalls_total` counts events and stays. **Do not reuse `plurx_suspended_seconds_total`** — that is encoder ahead-window suspension (`telemetry.rs:196-203`), not a viewer stall, and conflating them would make a healthy pacing decision look like a defect |
| 4 | Failed starts per attempt | `plurx_start_outcomes_total` **(new)** | `method`, `outcome="ok\|refused\|failed\|cancelled"` | its own `ok+refused+failed+cancelled` | Live TV keeps `plurx_live_tv_starts_total{outcome}`; this is the finite-playback twin, so the ratio is one query on either surface |
| 5 | Replacement failure rate | `plurx_playback_preparation_staged_total{outcome}` ÷ `plurx_playback_preparation_decisions_total` | existing | existing | **No new series.** M5's work here is to write the exact expression down, in [OPERATIONS.md](../OPERATIONS.md), so two people compute it the same way |
| 6 | Admission wait | `plurx_admission_wait_seconds` **(new)** | `pool="vod_blocked_get\|encode_permit\|probe_gate\|image_materialize"` | `_count` | Four named pools, each an existing semaphore or wait pool. The existing gauges (`plurx_vod_blocked_gets_waiting`) say how many are waiting, never how long |
| 7 | Actual vs reserved scratch | `plurx_scratch_bytes{kind="reserved\|actual"}` | `kind` | — | **Not built here.** The reservation accounting belongs to the seek-scratch repair effort (review §3.3.5, C16). M5 reserves the name and the two label values so that effort and this one do not invent two spellings |
| 8 | Bytes per watched minute | `plurx_delivered_bytes_total{method}` **(new)** ÷ `plurx_watched_seconds_total{method}` | `method` | row 3's denominator | One counter, reusing row 3's denominator — which is why rows 3 and 8 must land in the same PR |

Rows 2 and 3's denominators are the same kind of thing and are the reason
the set works at all: §1's "the six named fleet incidents were each found
by a human" is a story about counters with no denominator. A stall count
without watched seconds cannot distinguish a bad release from a busy week.

### 3.6 M5 — how an effort's exit counter is defined (§5.3)

§5.3 asks that every future effort's exit criterion add "a counter read off
`/metrics` on the fleet — with its event, bounded labels,
expected-demand denominator, observation interval and alert owner defined".
M5 writes that as a five-field block that goes in the effort's own plan,
and adds the template to [OPERATIONS.md](../OPERATIONS.md) beside the
existing metric tables:

```text
Exit counter
  Event       what increments it, in one sentence, naming the code path
  Series      plurx_<name>{<label>="<values>"}   — every value enumerated
  Denominator the series that says how much demand there was; "none" is
              only legal for a gauge whose absolute value is the claim
  Interval    how long the fleet must be observed before the number counts
  Threshold   the value that means the effort worked, decided BEFORE the
              observation
  Owner       the person who is paged, or who checks, when it moves
```

Worked example, for the week item "`output_job_owned` pipes both streams"
(review §2.1) — chosen because it is the one where a counter would have
caught the defect the day it shipped:

```text
Exit counter
  Event       every `require_dovi_renderer` decision (transcode.rs:14322)
  Series      plurx_dovi_proofs_total{result="proved|refused|error"}
  Denominator the same series' sum — the decision is its own demand
  Interval    7 days on media1, which sees Profile 5 material weekly
  Threshold   result="refused" is under 5 % of the total. Before the fix it
              was 100 %, and nothing anywhere said so for eight days
  Owner       Paul; checked at the weekly review against the pinned status
              page
```

The point of the threshold-before-observation rule is that "the number
looks fine" is not a test. The 100 % figure above was available on the
fleet from the day of the 09-14 deploy; what was missing was a series and
somebody expecting a value.

### 3.7 Dependencies this needs, and which are not there yet

Checked at `0f02b7ea` in `crates/plurxd/Cargo.toml`:

| Needed by | Crate | State today |
|---|---|---|
| M1's body wrapper | `http-body-util` | **dev-dependency only** (`:81`) — must be promoted to a real dependency |
| M2's uuid minting | `uuid` | **dev-dependency only** (`:83`; workspace `Cargo.toml:102`, `features = ["v4"]`) — promote, or mint 16 hex bytes from `getrandom`, which *is* a real workspace dependency (`Cargo.toml:89`) |
| M2's layers | `tower-http` `request-id` feature | **not enabled**: `features = ["trace", "fs", "cors"]` (`:43`) — add `"request-id"`, or write the ~40-line layer by hand |
| M3's TTY check | `std::io::IsTerminal` | in std since 1.70; nothing to add |
| M3's JSON layer | `tracing-subscriber` `json` feature | **not enabled**: workspace `Cargo.toml:60` is `features = ["env-filter"]` — add `"json"` |

Each is a one-line manifest change and each grows the release binary
slightly; the M2 alternative (hand-written layer, `rand`-minted id) exists
precisely so that adding a feature to `tower-http` is a choice rather than
a requirement. Whichever is taken, say which in the PR body — the
dependency surface of the shipped binary is the kind of thing that grows
by accident.

### 3.8 Settings

None. `PLURX_LOG_FORMAT` is an environment variable beside the existing
`PLURX_LOG`, because it describes how this process's stdout is consumed
and that is per node, not per cluster. Nothing here is a replicated
setting and nothing appears in Settings → Developer.

## 4. Guardrails (non-goals)

- **Matched routes and bounded method/status labels** (F-core-12,
  verbatim). §3.1 uses `MatchedPath` templates from a table built at
  startup, with two reserved labels for the two ways a request can have no
  template; the arithmetic is written out so the bound is checkable, not
  asserted.
- **Response-header latency distinguished from body completion**
  (F-core-12, verbatim). Two series, two bucket scales, and a third
  counter separating a completed body from an abandoned one so a seek does
  not read as an error.
- **Request ids are sanitised** (F-core-12, verbatim). §3.2 validates
  charset and length, replaces rather than rejects, and never logs a
  discarded value.
- **ANSI defaults are confirmed from source, not assumed** (F-core-12:
  "The cached tracing-subscriber source confirms ANSI defaults on unless
  NO_COLOR disables it"). §2 quotes `fmt_layer.rs:739-743`; §3.3 adds the
  TTY check the library omits.
- **Redaction is preserved, including third-party panic text** (C10,
  F-core-12). `safe_trace_target` is untouched and still governs the span;
  the panic hook reuses `redact_operator_text`'s rule set rather than
  writing a weaker second one.
- **Recursive logging is avoided** (F-core-12: "avoid recursive logging").
  ~~A thread-local guard, checked before any tracing call in the hook.~~
  Corrected by the #461 review: a guard cannot do this, because std aborts
  on a panic inside the hook instead of re-entering it. Recursion is avoided
  by std; the process survives only if the reporting path does not panic,
  which is how it is built (§3.4).
- **The default hook is chained deliberately** (F-build-ops-codehealth-12).
  `previous(info)` always runs; stderr output and abort behaviour do not
  change.
- **A panic hook is not a repair** (F-build-ops-codehealth-12, verbatim:
  "it does not ensure a detached task's lifecycle effects are repaired").
  Said in §3.4, with the efforts that own the repair named.
- **`/metrics` still reads no Store.** Every series added here is an
  atomic array rendered to text (§2.3); the existing
  `prometheus_scrape_has_no_store_operation` test
  (`http/system.rs:5483`) stays green and is an acceptance check on every
  milestone.
- **No content identifiers in labels** (§4.9, verbatim). Route templates,
  bounded enums and a fixed subsystem allowlist. Session ids, file ids,
  user ids, titles, paths and node ids appear in none of them — the same
  rule [OPERATIONS.md](../OPERATIONS.md) already states for cluster
  metrics.
- **The five-dimension segmentation §4.9 asks for lives in the sidecar,
  not in Prometheus.** §3.5 says so explicitly and names the query
  surface, because the alternative is the label explosion the same
  paragraph forbids.
- **Not in scope:** an exporter, a dashboard, a tracing backend, OTLP, or
  changing `EnvFilter`/`PLURX_LOG` semantics. Also not in scope: the
  clients' side of rows 2 and 3 — three client plans own emitting
  `seek_resumed` and stall durations, and M5 lands the server half so
  their exit counters have somewhere to land.

## 5. Milestones

### 5.1 M1 — RED middleware (`core/http-red-metrics`)

1. `http-body-util` promoted from dev-dependency to dependency (§3.7);
   `RouteTable` built from the router's registrations; the middleware,
   added outermost (after `cluster_capacity_gate`); the four series in
   §3.1; rendered into `/metrics` beside `process_metrics`
   (`http/system.rs:5000-5011`).
2. Tests (`http/mod.rs`'s existing test module, which already builds a
   router and calls it):
   `a_matched_route_is_labelled_by_template_not_by_uri` (request
   `/api/v1/items/42`, assert the label is `/api/v1/items/{id}` and that
   `42` appears nowhere in the exposition);
   `an_unmatched_path_uses_one_shared_label` (100 random paths → one
   series);
   `a_capacity_gate_refusal_is_counted_as_503`;
   `header_latency_and_body_latency_are_separate_series` (a handler that
   returns fast and streams slowly; assert the request histogram's sum is
   small and the body histogram's is large);
   `an_abandoned_body_counts_aborted_not_error`;
   `the_label_space_is_closed` (enumerate the table, assert it equals the
   router's route count plus two).
3. `prometheus_scrape_has_no_store_operation` still green.

Acceptance: `cargo test -p plurxd http::` green;
`curl -s $HOST/metrics | grep -c '^plurx_http_requests_total'` is in the
low hundreds, not thousands, after a browse session;
`curl -s $HOST/metrics | grep plurx_http | grep -E '[0-9a-f]{8}-[0-9a-f]{4}'`
finds nothing (no id ever reached a label).

### 5.2 M2 — request ids (`core/http-request-ids`)

1. The manifest change §3.7 names (`tower-http` `request-id` plus a uuid
   source, or the hand-written layer); `adopt_or_mint`,
   `SetRequestIdLayer` + `PropagateRequestIdLayer`, the span field.
2. Tests:
   `a_valid_inbound_id_is_adopted_and_echoed`;
   `an_oversized_or_exotic_id_is_replaced_and_not_logged` (assert the
   discarded value is absent from the log buffer);
   `every_response_carries_an_id`;
   `the_id_is_not_a_metric_label` (grep the exposition after a request
   with a known id).

Acceptance: `cargo test -p plurxd http::` green;
`curl -si $HOST/api/v1/server | grep -i x-request-id` prints a header;
`curl -si -H 'x-request-id: aaaa$(cat /etc/passwd)' $HOST/api/v1/server`
returns a minted id, and the daemon log contains neither the header value
nor any part of it.

### 5.3 M3 — log format and ANSI (`core/log-format-and-ansi`)

1. `"json"` added to the workspace `tracing-subscriber` features
   (`Cargo.toml:60`); `PLURX_LOG_FORMAT`, `with_ansi(is_terminal())`, the
   5xx WARN `on_response`; [OPERATIONS.md](../OPERATIONS.md) gains the
   variable beside `PLURX_LOG`.
2. Tests:
   `json_format_emits_parsable_lines` (capture the writer, `serde_json`
   every line);
   `ansi_is_off_when_stdout_is_not_a_terminal` (injected `is_terminal`);
   `a_five_hundred_logs_once_at_warn_with_the_route_template`;
   `a_two_hundred_does_not_log_at_info` (the ring-eviction guardrail, as a
   test).

Acceptance: `cargo test -p plurxd` for the logging module green;
`PLURX_LOG_FORMAT=json ./plurxd … 2>&1 | head -5 | python3 -c 'import
sys,json; [json.loads(l) for l in sys.stdin]'` exits 0;
`journalctl -u plurxd -n 50 --output=cat | grep -c $'\e\['` prints 0 on a
node running the new build.

### 5.4 M4 — panic hook (`core/panic-hook`)

1. `redact_bounded` lifted from `redact_operator_text`
   (`http/cluster_operations.rs:2495`) into a shared module, with
   `cluster_operations` switched to it; `install_panic_hook` called from
   `init_logging`'s caller before any task spawns; `plurx_panics_total`.
2. Tests:
   `a_panic_reaches_the_log_buffer_with_its_location`;
   `a_panic_payload_containing_a_bearer_is_redacted`;
   `a_panic_inside_the_hook_does_not_recurse` (a formatter that panics) —
   shipped as `a_panic_inside_the_reporting_path_aborts_the_process`, because
   what a panicking formatter inside the hook actually does is abort the
   process (§3.4), plus `hostile_payloads_are_reported_without_a_second_panic`
   for the property that keeps that from happening;
   `the_previous_hook_still_runs`;
   `the_subsystem_label_is_from_the_allowlist` (a synthetic location
   outside it reports `other`);
   `redact_operator_text_behaviour_is_unchanged` (pin the existing rule
   set through the lifted function).

Acceptance: `cargo test -p plurxd panic` and `cargo test -p plurxd
cluster_operations` green; `make unit` green once before un-WIP;
a deliberate panic in a `#[ignore]`d test shows in
`GET /api/v1/system/logs` and moves `plurx_panics_total`.

### 5.5 M5 — the release-evidence set and the exit-counter definition (`core/release-evidence-metrics`)

Three PRs, because the rows have different shapes:

1. **M5a** — rows 1, 5: `plurx_ttff_ms` gains `client` and two buckets
   (60 000, 120 000); the replacement-failure expression written into
   [OPERATIONS.md](../OPERATIONS.md). No new families.
2. **M5b** — rows 3, 8, and row 4:
   `plurx_watched_seconds_total{method}`, `plurx_stalled_seconds_total{kind}`,
   `plurx_delivered_bytes_total{method}`, `plurx_start_outcomes_total{method,outcome}`,
   all fed from events the server already sees, plus the `OPERATIONS.md`
   rows that say how to read each ratio.
3. **M5c** — rows 2, 6: `plurx_seek_to_picture_ms{method}` +
   `plurx_seeks_total{method}` (server side only; zero until clients
   emit), `plurx_admission_wait_seconds{pool}` at the four named pools;
   the §3.6 exit-counter template and its worked example into
   `OPERATIONS.md`. Row 7 is a reservation of names only — **no code**.

Tests per PR: one that the new families render with exactly their
enumerated label values and no others; one that each ratio's numerator and
denominator move together on a synthetic event stream; and
`prometheus_scrape_has_no_store_operation` on all three.

Acceptance: `cargo test -p plurxd telemetry` and `cargo test -p plurxd
http::` green; `curl -s $HOST/metrics | grep -E
'plurx_(watched_seconds|stalled_seconds|delivered_bytes|start_outcomes|admission_wait|seek_to_picture)'`
shows every family after one play and one seek; every added family has a
row in [OPERATIONS.md](../OPERATIONS.md)'s metrics section, checked by
`python3 -m pytest tests/operations/`.

## 6. Verification and rollout

### 6.1 Lanes

Per milestone the focused `cargo test` above, then `make unit` once before
un-WIP. `make validate-staged` before every push. M3 and M5 edit
[OPERATIONS.md](../OPERATIONS.md), so `python3 -m pytest tests/operations/`
must be green on those. No client or Node gate is affected.

### 6.2 Rollout

M1–M4 are independent and safe to deploy in any order; take them to `lab1`
one at a time and read `/metrics` after each, because the failure mode of a
metrics change is a scrape that got large or slow rather than a crash.
Check the exposition size explicitly: `curl -s $HOST/metrics | wc -c`
before and after M1, reported in the PR body. M5's three PRs go to the
fleet together with M5b last, since its `OPERATIONS.md` rows describe
families M5a and M5c introduced. Nothing here changes recipe identity,
cache digests or schema; rollback is the previous `sha-` image, and a
scraper that has learned the new series sees them go absent.

### 6.3 What only the fleet can prove — GPT prompt

```text
On media1 and lab1, with the build carrying PRs <M1..M4 numbers>:
1. Exposition size and scrape cost, on each host:
     curl -s -o /dev/null -w 'bytes=%{size_download} time=%{time_total}\n' \
       http://<host>:32400/metrics
   Run it five times per host after at least an hour of normal use and
   paste all ten lines. If any scrape exceeds 2 MB or 500 ms, say so.
2. Label hygiene — this must return nothing on both hosts:
     curl -s http://<host>:32400/metrics | grep -E \
       'route="[^"]*[0-9a-f]{8}-|session|token|/mnt/|file_id='
   Paste the command's output (expected: empty) and its exit status.
3. ANSI in journald, on each host:
     journalctl -u plurxd -n 200 --output=cat | grep -c $'\e\['
   Expected 0. Report the number.
4. JSON mode on lab1 only: set PLURX_LOG_FORMAT=json in the unit's
   environment, restart, and paste the first five log lines. Confirm each
   parses as JSON. Then revert the variable and restart.
5. Request ids: curl -si http://lab1:32400/api/v1/server | grep -i
   x-request-id — paste it. Then send a deliberately bad one:
     curl -si -H 'x-request-id: ../../etc/passwd AAAA...(80 chars)' \
       http://lab1:32400/api/v1/server | grep -i x-request-id
   Paste the response header, then grep the last 50 journal lines for
   "passwd" and confirm it is absent.
6. RED sanity: browse the web app for two minutes, then
     curl -s http://lab1:32400/metrics | grep plurx_http_requests_total | wc -l
   and paste the ten highest-count series.
7. Play one title, seek twice, stop it, then paste every line matching
     curl -s http://lab1:32400/metrics | grep -E 'plurx_http_body_seconds|plurx_http_bodies_total'
Report exact values. Do not restart anything except in step 4.
```

### 6.4 What only the fleet can prove for M5 — GPT prompt

```text
On lab1 and media1, with the build carrying the C-08 M5 PR (plan/C-08-2):
1. Families and size, on each host:
     curl -s http://<host>:32400/metrics | grep -cE \
       '^plurx_(ttff_ms|seek_to_picture_ms|seeks_total|stalled_seconds_total|watched_seconds_total|delivered_bytes_total|admission_wait_seconds)'
     curl -s -o /dev/null -w 'bytes=%{size_download} time=%{time_total}\n' \
       http://<host>:32400/metrics
   Paste both numbers per host. Flag any scrape over 2 MB or 500 ms.
2. Label hygiene — must print nothing on both hosts (paste output and exit
   status):
     curl -s http://<host>:32400/metrics | grep -E \
       '^plurx_(ttff_ms|seek_to_picture_ms|seeks_total|stalled_seconds_total|watched_seconds_total|delivered_bytes_total|admission_wait_seconds)' \
       | grep -vE 'method="(direct_play|remux|transcode|unknown)"|kind="(supply|decode|network|other)"|pool="(vod_blocked_get|encode_permit|image_materialize)"'
3. Watched time, on lab1, in Chrome: save
     curl -s http://lab1:32400/metrics | grep -E 'plurx_(watched_seconds_total|delivered_bytes_total|ttff_ms_count)'
   then play one title for 5 minutes, pause for 1 minute, seek forward
   10 minutes, play 2 more minutes, stop. Repeat the grep and paste both.
   Expected: plurx_watched_seconds_total{method=<the method the stats
   overlay shows>} rose by about 420 (not 480 — the pause and the seek do not
   count; report the exact delta), delivered_bytes for that method rose, and
   plurx_ttff_ms_count{method=...,client="chrome"} rose by 1.
4. Native clients: play any title for 2 minutes on the Apple TV and 2 minutes
   on the Android device against lab1, then paste the same grep. Expected:
   plurx_watched_seconds_total{method="unknown"} rose by about 240 (natives
   do not name their method yet), and plurx_ttff_ms_count gained one each
   under client="apple" and client="android". If either landed under
   client="other", paste that client's User-Agent from the lab1 journal.
5. Admission wait: on lab1 start a VOD title in Chrome and seek far ahead
   twice, then paste
     curl -s http://lab1:32400/metrics | grep 'plurx_admission_wait_seconds_count'
   Report the three counts; vod_blocked_get should be non-zero.
Report exact values. Restart nothing.
```

## 7. Open questions

1. **`client` as a `plurx_ttff_ms` label (row 1).** The class comes from
   `http::network::identity`, whose vocabulary this plan has not
   enumerated. M5a's first step is to read it and write the values down; if
   it is open-ended (derived from a User-Agent rather than mapped to a
   fixed set), the label must be dropped and the segmentation left to the
   sidecar rows. Do not ship an unbounded label to get a nicer graph.
2. **Where `plurx_watched_seconds_total` is incremented.** It is the
   denominator for two rows and must not double-count a session served by
   two nodes or a viewer who leaves the tab open on a paused player.
   Candidate source: the delivery accounting the HLS pump already keeps.
   M5b must name the exact call site and defend it in the PR body; if no
   single site can answer honestly, the metric is not ready and rows 3 and
   8 wait.
3. **Body-latency accounting for range requests.** A direct play answered
   as 206 in ten chunks is ten bodies. Whether
   `plurx_http_body_seconds{route="/api/v1/media/{id}"}` should describe
   one range or one viewing is a modelling choice; M1 takes "one response
   body" because that is what the layer can see, and §7 records that the
   number is per-response, so nobody reads it as time-to-watch.
4. **Whether the 5xx WARN line duplicates existing error logging.** Many
   handlers already log their own failures. M3 should check for
   double-reporting on the three or four noisiest error paths before
   landing, and drop the layer's line for those targets if it is
   redundant — a bounded ring cannot afford to say things twice.
5. **Row 7's owner.** `plurx_scratch_bytes{kind}` is reserved here and
   built by the seek-scratch repair effort. If that effort lands a
   different name first, this document is the one that changes, not the
   metric — and §3.5's row should be updated in the same PR.

### 7.6 Decisions taken while executing, 2026-09-23

This plan was written against `0f02b7ea`. Five things it says are no longer
true at `1d21b184e`, and one thing it asked for turned out to be a hazard.
Each is a deviation from the plan as written and is recorded here rather than
left for a reader to discover from the diff.

1. **K-04 landed a matched-route latency histogram after this plan was
   written, so §3.1's `plurx_http_request_seconds` was not built.**
   `http_store_attribution` (`http/mod.rs`) times `next.run(request)` by
   bounded `route_group` and serving role and renders
   `plurx_http_route_seconds`. That timer stops when the handler returns its
   `Response`, which is exactly response-header latency. Adding a second
   header-latency histogram would have been the same measurement under a
   second name. F-core-12's distinction is still delivered, and is now the
   distinction between `plurx_http_route_seconds` (header) and the new
   `plurx_http_body_seconds` (delivery).
2. **The route label is `route_group`, not the `MatchedPath` template.** §3.1
   specified 192 template labels built from the router's own route list. Axum
   exposes no route list at runtime, so that table would have to be maintained
   by hand — and `http_route_group` is already this repository's one
   exhaustive inventory of registered templates, held closed by
   `registered_routes_reach_every_non_other_attribution_family` and by the
   unclassified-pattern assertion in the route inventory test. A second table
   would be a second spelling of the same fact with no test holding the two
   in step. **The cost is real and is not hidden**: a 5xx on one item route
   and a 5xx on another are one series, and §3.1's two reserved labels
   (`<unmatched>`, `<fallback>`) collapse into the existing `other`. The 5xx
   access line carries the redacted target and the request id, and that is
   what takes an operator from a series to a request.
3. **The 5xx access line is throttled to one per route group per second.**
   §3.3 asked for an `on_response` that logs every 5xx at WARN. On a fenced
   node, a learner outside its eligible routes, or a node in maintenance,
   `cluster_capacity_gate` refuses **every** request with 503 — so "every 5xx"
   is one ring entry per request at full request rate, and the ring is the
   only log the product can show. That is the same eviction hazard §3.3 cites
   as its reason for refusing an INFO access log, arriving through the door it
   left open. The line now carries `also_suppressed`, the number of lines it
   stands for, and the counters remain exact.
4. **§3.7's dependency table is stale in two rows.** `uuid` is a real
   dependency of `plurxd` at this commit, not a dev-dependency, so M2 needed
   no manifest change for it and no hand-rolled id source. `http-body-util`
   is still dev-only, but M1 does not need it: the body wrapper implements
   `http_body::Body` directly so it can delegate `size_hint`, and `http-body`
   is named as a workspace dependency instead — a crate already compiled in
   the tree under hyper and axum, so the build gains nothing new. The one
   genuinely new crate in the shipped binary is **`tracing-serde 0.2.0`**,
   pulled in by `tracing-subscriber`'s `json` feature, and it is recorded in
   `THIRD-PARTY-NOTICES.md`.
5. **Open question 4 — does the 5xx WARN line duplicate existing error
   logging? Partly, and it stays.** `ApiError::Internal` already logs the
   failure detail at ERROR (`http/error.rs`), with no route, no status, no
   latency and no request id; the access line has all four and not the detail.
   They are complementary halves of one report, so a 500 raised through
   `ApiError::Internal` costs two ring entries. The alternative — dropping the
   access line for that path — would leave every 5xx that does **not** come
   from `ApiError::Internal` (a panic turned into a 500, a typed
   `ServiceUnavailable`, the capacity gate's refusals, hyper's own errors)
   with no line at all. The throttle in point 3 is what bounds the cost.
6. **Open question 3 — body accounting for range requests — is answered as
   §7 anticipated and is written into the operator documentation.**
   `plurx_http_body_seconds` is per **response body**, so a direct play
   answered as ten range requests is ten observations, not one viewing.
   `docs/OPERATIONS.md` says so where the metric is described, so nobody reads
   it as time-to-watch.

**M5 was not started.** It is the largest milestone and the plan splits it
into three PRs of its own; two of its open questions (§7.1 the `client` label's
vocabulary, §7.2 where `plurx_watched_seconds_total` is incremented without
double-counting) are research this session did not do, and §7.2 says plainly
that if no single call site can answer honestly the metric is not ready. M1's
series exist now, which is the dependency M5 was waiting on.


### 7.7 Decisions taken while executing M5, 2026-09-24

M5 was built as **one** draft PR on `plan/C-08-2`, not the three PRs §5.5
describes: the work board's rule 4 makes milestones logical commits inside
one plan PR, and this session was asked for one. Every premise §3.5 rests on
was re-read at `936157b4b` first; the branch starts at `7939a3f1e` (which
already carries #482 and #487 through integration #493) and was merged with
`origin/main` at `b1de09647` before it was pushed, and none of the files
cited below changed between `936157b4b` and `b1de09647`. What changed against
the plan is below, and each item is a deviation from the plan as written.

**What was re-verified, by function name (lines at `936157b4b`).**
`PlaybackMetrics` is still the fixed-array house pattern (`telemetry.rs:225`,
`record` `:259`, `render` `:354`); `plurx_ttff_ms` still had eight buckets
ending at 30 s (`:17`). Every first-party client still sends a stall's
duration in `ms` (web `recordWaitStall` in `web/player/measurements.js`,
Android `ControllerPlaybackTelemetry.sampleStall`, Apple
`ApplePlaybackStallLog`), so row 3's numerator needed no client change.
`/metrics` (`http/system.rs:5290`) and `prometheus_scrape_has_no_store_operation`
are unchanged, and nothing here touches the `format!` in that handler: every
new family renders inside `crate::telemetry::prometheus()`. The replacement
counters of row 5 are where §2.4 said (`playback_control.rs:14304-14373`).
The worked example's `require_dovi_renderer` is now at `transcode.rs:15132`,
not `:14322`, and `plurx_dovi_proofs_total` **does not exist**; OPERATIONS.md
labels the example illustrative.

1. **§7.1 — the `client` label — is answered: bounded, and taken from the
   header.** `http::network::client_class` maps a `User-Agent` onto seven
   literals (`chrome`, `safari`, `firefox`, `edge`, `apple`, `android`,
   `other`) and returns nothing else; it now returns `&'static str`, the
   vocabulary is `telemetry::CLIENT_CLASSES`, and
   `every_client_class_is_in_the_bounded_metric_vocabulary` holds the two
   together. The label comes from the requester's header, never from the
   beacon's free-text `ua` field, and is derived beside — not from —
   `network::identity`, which is `None` for an IPv6 peer. Cost: the
   `plurx_ttff_ms` family grows from 44 lines to 364. **A query of the old
   shape (`plurx_ttff_ms_count{method="remux"}`) now returns seven series**;
   `sum by (method)` restores it.
2. **§7.2 — where watched seconds are counted — is answered: the live
   progress beat (`http::watch::progress`).** It is the one signal every
   first-party player sends every few seconds while open, playing or paused,
   and each beat reaches exactly one node. `telemetry::WatchLedger` compares
   a beat with the previous beat for the same viewer and item on the same
   node and credits the smaller of the position's advance and the wall time
   between them; a pause, a stall, a rewind, an advance over twice the wall
   time plus 2 s (a seek), or a gap over 120 s credits nothing. So the sum
   is never more than wall time per viewer and item per node, and a viewer
   who moves nodes starts a fresh baseline instead of being counted twice.
   The ledger is per process (`AppState::watch_ledger`) and capped at 4,096
   entries; a viewer arriving at a full ledger is not tracked rather than
   evicting a live one. Offline replays (`recorded_at`) and Plex-compatible
   `/:/timeline` clients are not counted. **The method is what the beat
   names**: the beat gains an optional `method`, validated against the
   playback vocabulary and never stored. The web player sends it in this PR;
   Apple and Android do not yet, so their seconds are `unknown`. Row 3 (the
   sum) is complete; row 8's per-method split is not, until they do.
3. **Row 4 — failed starts per attempt — is not built.** No single server
   site sees a finite-playback start attempt *and* its outcome: a direct play
   has no create call (its start is a stream of range GETs the direct-play
   registry collapses), an HLS create sees only server-side refusals, and a
   start that fails in the client's decoder reaches the server only as a
   client beacon (`playback_failed`, `stream_rejected`, `hls_fatal`) that is
   not paired with an attempt the server counted. The honest build is an
   attempt-keyed outcome — the beacons already carry `attempt` — and that is
   a design of its own, not a counter. `plurx_start_outcomes_total{method,
   outcome}` is reserved in OPERATIONS.md so it cannot be spelled twice, and
   `tests/operations/test_release_evidence_metrics.py` fails the day it is
   rendered without a reading row.
4. **Row 6 has three pools, not four.** The probe gate's wait is already
   `plurx_decode_facts_phase_seconds{phase="gate_wait"}` (S-13 M0,
   `decode_facts.rs` `get_or_probe_inner`); a second series for the same
   wait would repeat §7.6.1's mistake. The three: `vod_blocked_get`
   (`waitpool::RegisteredWait::wait`, timed from pool admission),
   `encode_permit` (the lifetime of an `admission::LiveWait` guard, which
   covers the VOD encoder queue, the rolling start queue and the software
   capacity retry), and `image_materialize` (`ArtworkCoordinator::
   derive_permit`). One observation per wait, however it ended.
5. **Row 2's denominator needed a definition the plan did not give.**
   `plurx_seeks_total{method}` counts seeks that *ended*: `seek_resumed`
   (with `ms`, which also feeds the histogram) plus `seek_abandoned`
   (superseded, or the player stopped first). Counting only `seek_resumed`
   would have made the denominator equal to the histogram's own `_count`.
   Both events are a client contract no client implements yet; the families
   render at zero until one does.
6. **Row 8's two sources differ by at most one chunk.** HLS (rolling and
   VOD) and progressive remux credit the delivery meter, which counts a piece
   after the downstream has taken it; a direct play has no meter, so its
   body counts each chunk as it is yielded to the connection. Each
   `crate::meter::Meter` now carries its method from construction.
7. **Row 5's expression, written exactly:**
   `plurx_playback_preparation_staged_total{outcome="refused"}` ÷
   `plurx_playback_preparation_decisions_total{outcome="prepare"}`. §3.5's
   "staged ÷ decisions" would have divided by every decision including the
   fallbacks, which never try to stage.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 — RED on the matched route | [PR #461](http://192.168.4.7:3000/noirr/plurx/pulls/461) | `plurx_http_requests_total{route_group,method,status}`, `plurx_http_body_seconds{route_group}` and `plurx_http_bodies_total{route_group,outcome}`, recorded by a layer outside `cluster_capacity_gate` so its 503s are counted. Header latency was **not** re-implemented: K-04's `plurx_http_route_seconds` already is it (§7.6.1). The route label is `route_group`, not the template (§7.6.2). Six tests, each shown failing with its change reverted. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 — request ids | [PR #461](http://192.168.4.7:3000/noirr/plurx/pulls/461) | `x-request-id` adopted when it is at most 64 characters of `[A-Za-z0-9_-]` and minted otherwise, on the request, on the `http_request` span and on the response; never a metric label. Hand-written layer, no `tower-http` feature added; `uuid` was already a real dependency (§7.6.4). Three tests, one of which asserts the discarded value reaches neither the response nor the log. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 — log format and ANSI | [PR #461](http://192.168.4.7:3000/noirr/plurx/pulls/461) | `PLURX_LOG_FORMAT=json\|text`; `with_ansi` set explicitly from `std::io::stdout().is_terminal()` and forced off under JSON. **Confirmed on the fleet before the change**: `docker logs plurxd` on nuc4 carries `ESC[2m` / `ESC[31m` escape bytes in every line, so the plan's §2.3 claim was not only true of the vendored source but true of a running node. The 5xx access line is throttled (§7.6.3). Four tests; the ANSI one asserts the flag is load-bearing in both directions. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 — panic hook | [PR #461](http://192.168.4.7:3000/noirr/plurx/pulls/461) | `crates/plurxd/src/panics.rs` and `crates/plurxd/src/redact.rs`; `redact_operator_text` lifted out of `http/cluster_operations.rs` unchanged and reused for panic payloads; ~~recursion guard~~ (removed by the #461 review: it could never fire — see §3.4), opt-in path-free bounded backtrace, chained previous hook, `plurx_panics_total{subsystem}` over a fixed 13-name allowlist. Five tests, one of which installs the real hook and panics for real. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M5 — release-evidence set | — | **Not started**, deliberately. See §7.6. (Superseded by the M5 row of 2026-09-24 below.) |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | #461 review, P1 — body outcome | [PR #461](http://192.168.4.7:3000/noirr/plurx/pulls/461) | **The M1 row's claim that `plurx_http_bodies_total` separates a finished body from an abandoned one was false for fixed-length bodies**, i.e. for every direct play range and HLS segment: hyper drops a `Content-Length` body unpolled once the declared bytes are written, and a stream body is not `is_end_stream()` until polled to `None`, so every one was counted `aborted`. `MeasuredBody` now records the declared length (the `Content-Length` header, else the exact size hint; zero for HEAD/1xx/204/304) and counts yielded data bytes, and classifies a drop as `complete` once the declared length is yielded. Pinned by `a_fully_delivered_body_counts_complete_over_a_real_connection_whatever_its_framing` (real `axum::serve` listener, raw HTTP/1.1 socket, chunked and `Content-Length` and HEAD) and `a_fixed_length_body_is_complete_at_its_declared_length_and_aborted_short_of_it`; both shown failing with the production hunk reverted. OPERATIONS.md's "a healthy node aborts bodies constantly" removed. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | #461 review, P2 — panic hook | [PR #461](http://192.168.4.7:3000/noirr/plurx/pulls/461) | The recursion guard and every claim that it protected the process are gone (§3.4 corrected in place). std aborts on a panic inside a hook; the reporting path is now documented step by step as panic-free by construction, reaches the counter and label with `get`, and has no `catch_unwind` because none could work. The stand-in `a_nested_report_is_suppressed_by_the_guard` is replaced by two child-process tests: `a_panic_inside_the_reporting_path_aborts_the_process` (the real hook behind a panicking log writer → SIGABRT and std's nested-panic message) and `hostile_payloads_are_reported_without_a_second_panic` (a payload whose `Debug` and `Display` panic, a non-string, an empty and a multi-byte payload cut at 512 characters all come out as log lines). The guard's removal changes no behaviour — it never fired — so there is no revert to show failing; the abort test pins std's behaviour, and the hostile-payload test was shown catching a byte-slicing truncation in `redact_bounded` (the child aborts, exit 134). Fix commits `7611d398` (P1) and `20700f81` (P2). |
| | | | | | `needs:` the §6.3 fleet observations. Nothing in this branch has been deployed or scraped on a node; the exposition-size, label-hygiene, journald-ANSI-after, JSON-mode and RED-sanity steps are all unrun. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M5 — release-evidence set | draft PR on `plan/C-08-2` (number in the board row) | Rows 1, 2, 3, 5, 6, 7 and 8 built as one PR (premises read at `936157b4b`, merged with `origin/main` at `b1de09647`); **row 4 is not built** (§7.7.3) and its name is reserved. `plurx_ttff_ms` gains `client` (seven closed values from the request's own `User-Agent`) and 60 s / 120 s buckets; `plurx_seek_to_picture_ms{method}` + `plurx_seeks_total{method}` (zero until clients send `seek_resumed` / `seek_abandoned`); `plurx_stalled_seconds_total{kind}` from the stall `ms` all three clients already send; `plurx_watched_seconds_total{method}` from live progress beats (§7.7.2), the web player now naming its method; `plurx_delivered_bytes_total{method}` from the delivery meter and the direct-play body; `plurx_admission_wait_seconds{pool}` on three pools (§7.7.4). The row-5 expression, the row-7 reservation, the per-family reading rows and the §3.6 exit-counter template are in `docs/OPERATIONS.md`, held by `tests/operations/test_release_evidence_metrics.py`. Rust tests, each shown failing with its production hunk reverted on nuc3: `a_ttff_beacon_is_labelled_by_the_requesters_client_class` (handler passes no class → the `client="firefox"` bucket never moves; and again with `record_from` ignoring the class), `ttff_is_labelled_by_client_class_and_has_buckets_above_thirty_seconds` (bucket revert → no `le="60000"`; class revert → `safari` bucket 0 ≠ 1), `release_evidence_families_render_exactly_their_enumerated_labels` (`seek_abandoned` arm removed → `seeks_total{method="unknown"}` 1 ≠ 2), `stalled_and_watched_seconds_move_together_on_a_synthetic_session` (stall-`ms` credit removed), `live_progress_beats_credit_watched_seconds_to_the_named_method` (beat call removed), `a_meter_credits_its_method_in_the_delivered_bytes_family`, `a_live_wait_is_timed_into_the_encode_permit_pool_when_it_ends`, `an_admitted_wait_is_timed_into_the_vod_blocked_get_pool`, `a_derive_permit_wait_is_timed_into_the_image_materialize_pool` (each recording call removed). `watched_time_credits_only_plausible_forward_play_and_never_more_than_wall_time` pins the ledger's rules and `every_client_class_is_in_the_bounded_metric_vocabulary` the label bound; both are new code with nothing to revert to. Web: `a beat names the delivery method it is playing through` (`tests/web/progress-never-presented.test.js`) fails with `method` dropped from the beat. |
| | | | | | `needs:` the §6.4 M5 fleet observations (families and size, label hygiene, watched seconds against a scripted Chrome session, the two native clients landing under `unknown` with their own `client` class, a non-zero `vod_blocked_get` wait). Nothing in this branch has been deployed or scraped. Client-bound follow-ups, not this plan's code: Apple and Android naming `method` on progress beats, and all three clients emitting `seek_resumed` / `seek_abandoned`. |
