# Media write-stall guard — a connection nobody is draining is closed

**Status:** ready to build · the guard is **disabled by default** until M3's client
evidence is in (§4 Decision 2) · **Executes:** a gap found comparing plurx's
transport with Silo's on 2026-09-26; adjacent to review §2.5 (listener
timeouts) in
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md),
which the header timer already closed, and to
[MEDIA-BODY-BUFFERS.md](MEDIA-BODY-BUFFERS.md), which closed the read-size
half · **Written:** 2026-09-26 against `main` @ `e680849f` · **Reviewed:**
adversarial review 2026-09-26, verdict *reject as written*; 15 findings
folded in, ledger in §9

Companion to [MEDIA-BODY-BUFFERS.md](MEDIA-BODY-BUFFERS.md) (how a media
body is read and acknowledged) and [PLAYBACK.md](../PLAYBACK.md) (how a
segment reaches a client). This document is the other half of the same
question: what happens when the bytes are ready and the *client* stops
taking them. Read §2 before deciding anything is missing: the HLS pump
already has two timers, and this plan deliberately does not add a third
there.

The shape is borrowed from Silo's `RollingDeadlineWriter`
(`silo-server/internal/httpstream`, read 2026-09-26): a deadline on the
transport write that moves forward on progress, so the contract is "must
make progress every *window* seconds", never "must finish within N". Their
scar is worth carrying over verbatim: their first version was a fixed
`WriteTimeout: 120s` on the whole server, and it killed every healthy
multi-gigabyte stream at T+120 s.

## 1. Objective

1. A media response whose peer has stopped reading is closed after a bounded
   window, on every delivery path — direct play and ranges, progressive
   `stream.mp4`, HLS segments and playlists, offline package transfers,
   relayed peer bodies — from one place, so no path can be forgotten.
2. Only a peer that is *not draining* is reaped. The exact floor is the
   kernel's, not ours (§3.2): a client that takes roughly a third of the
   socket send buffer within the window is making progress; one that takes
   less is indistinguishable from one that is gone.
3. Idle keep-alive between requests, long-polls the server is slow to
   answer, and paused producers are all untouched: no write is pending in
   any of those, so the guard is not armed.
4. Every reap is attributable: a `tracing` line with the peer, the route
   that was being served, and how long the write was pending, plus a
   `/metrics` counter and a Developer-tab item whose requirements say what
   has to be true before turning it on is safe. Silence is never evidence of
   safety.

## 2. Contract today

Re-verify line numbers at build time; they are from `e680849f`. Versions:
hyper 1.10.1, hyper-util 0.1.20, tokio 1.53.1.

### 2.1 What exists — and why it does not close the socket

| Layer | Timer | Where | What it bounds |
|---|---|---|---|
| Connection | `header_read_timeout` 15 s (`HEADER_READ_TIMEOUT`, `main.rs:2989`) | `serve_http`, `main.rs:2901-2905` | Time to read request headers; also bounds keep-alive idle (`keep_alive_idle_is_bounded_by_the_header_timer`, `main.rs:3958`) |
| Connection, HTTP/2 | `keep_alive_interval` / `keep_alive_timeout` 20 s / 20 s (`main.rs:2909-2910`) | same | Dead-peer detection by PING, HTTP/2 only (whether hyper's server PINGs while idle is not verified from source) |
| HLS body | `MEDIA_BODY_NO_PROGRESS_TIMEOUT` 30 s (`media_sessions.rs:115`) | pump task, `http/hls/segment.rs:753`, `:844` | Upstream *or downstream* no-progress: the pump fails the body if the receiver does not accept a chunk for 30 s |
| HLS body | `MAX_ADMITTED_MEDIA_BODY_LIFETIME` 300 s (`media_sessions.rs:62`) | `segment.rs:733`, `:1372`, `response.rs:480` | Total lifetime of one admitted segment/playlist body |
| Direct play | — | `serve_file_range`, `http/stream.rs:3077-3157` | nothing |
| Progressive remux | — | `remux`, `http/stream.rs:3347`, body `unfold` at `:3577` | nothing (the body has cancellation on authority loss, not on peer silence) |
| Offline package | — | `http/offline.rs:1195-1215` | nothing; no `Range` handling on that route either |

There is no `tower_http` `TimeoutLayer` (tower-http features are `trace`,
`fs`, `cors`). `axum::serve` appears only in test fixtures (about twenty
across `crates/plurxd/src`); production is the hyper-util loop below.

**The HLS pump's 30 s timer does not close the socket.** It fails the
`StreamedBodyTerminal` and returns from the pump task, which releases the
file handle and the channel. But the body it feeds (`driven_local_body`,
`http/hls/response.rs:391`) only surfaces that error when hyper next polls
it, and hyper does not poll a response body while its write buffer is full.
Verified in hyper 1.10.1 source (crate downloaded during review):

- `proto/h1/dispatch.rs:349-378` — when `!conn.can_buffer_body()` the
  dispatcher does `ready!(self.poll_flush(cx))` and never reaches
  `body.poll_frame`;
- `proto/h1/io.rs:267-304` — `Buffered::poll_flush` is what calls
  `io.poll_write_vectored` (Queue strategy) or `io.poll_write` (Flatten);
  **that** call is the one that returns `Pending` when the peer's receive
  window is closed. `TcpStream::poll_flush` itself is a no-op `Ready` and is
  only reached once `write_buf.remaining() == 0`, so it cannot falsely
  register progress;
- `proto/h1/dispatch.rs:166-175` — `poll_loop` calls `poll_write` then
  `poll_flush` on *every* poll of the connection task, so a timer waker
  fired from inside the io wrapper re-enters the write and the wrapper gets
  to return its error;
- `proto/h1/dispatch.rs:123-140` — `poll_catch` turns that error into the
  connection future's `Err`; the dispatcher and `body_rx` drop, the remux
  `unfold` state drops, and `process_guard` reaps ffmpeg.

So the pump timer is a *file-handle* bound, and a good one; it is not a
connection bound. The same reasoning is why a body-level timer is the wrong
tool for direct play: a `Sleep` armed inside `poll_frame` wakes the task,
but the dispatcher will not re-poll the body until the buffer drains, so
the timer cannot return an error. Only the transport write can. These four
line references are the load-bearing evidence; re-check them on any hyper
bump.

### 2.2 What the kernel does on its own

For a peer that is *gone* (power-pulled tablet, dropped Wi-Fi) **while the
server has bytes to send**, Linux retransmits the unacknowledged send
buffer under `tcp_retries2` (default 15; the kernel documents ≈ 924 s as
the lower bound, so 15–30 min in practice) and then errors the socket;
hyper sees the write error and the connection ends. Today's leak per
abandoned direct play is therefore one file handle, one connection task,
≈ 1 MiB of buffers ([MEDIA-BODY-BUFFERS.md](MEDIA-BODY-BUFFERS.md) §2.1)
and, for a progressive remux, one blocked ffmpeg child, for up to half an
hour. Cheap individually; the point of the guard is that it is bounded by
*us*, at a number we chose, rather than by a sysctl.

For a peer that is gone **while the server has nothing to send** (a paced
remux waiting on ffmpeg, a 30 s playlist long-poll, a paused SIGSTOPped
VOD producer) nothing bounds the socket at all: plurx sets no
`SO_KEEPALIVE` anywhere in `crates/` (verified), and this guard does not
change that, because there is no pending write to time. That case is
bounded by session leases, not sockets, and is out of scope here.

For a peer that is *alive and paused* the kernel does nothing: the send
buffer stays full, ACKs keep arriving, the connection is healthy and idle.
This is the case §2.3 and §4 have to get right, because closing it is a
behaviour change the clients see.

### 2.3 How each client actually behaves after a mid-body close

This table was wrong in the first draft; the review read the client code.
Every row below cites the handler that runs, and the budget it has.

| Path | Client | What the code does today | Resumes? |
|---|---|---|---|
| Direct play / range | AVPlayer, Media3, Safari and Chrome `<video src>` | Platform loaders re-request with `Range:` from the byte cursor; `serve_file_range` answers `206` (`http/stream.rs:3096-3128`) | Expected yes; platform behaviour, **confirm on device in M3** |
| Progressive `stream.mp4` | web | `transport.js:665-722` is the `<video>` error handler. It returns with no action if `playbackIsReal()` (`:686`); it rescues codes 3/4 *into a transcode*; a code 2 network error falls to `stopPlayerForExhaustion()` and the "Playback failed" overlay (`:712-720`). `player.js:1057-1059` says network fatals are "deliberately left alone". The only reopen-at-position is the persistent-stall recovery in `measurements.js:568-608`, and it is **one-shot** per item (`:572-580`: "One automatic recovery was already tried"). `stream_mp4` appears to ignore `Range` (range parsing exists only in `serve_file_range`; not verified against the handler body — check at build). | **No, not reliably.** Whether a truncated chunked body surfaces as a `waiting` stall or as `MEDIA_ERR_NETWORK` is browser behaviour not verified here; the second pause on one title is the failure case either way |
| Progressive | Android | `onPlayerError` (`Controller.kt:811`) → `playbackErrorAction` (`PlaybackPolicy.kt:62-80`): a transport error on established SDR playback is `Fail`; established HDR gets one `RetrySameHDRDelivery`. The stall watchdog `restartAt(position, "stall")` (`:2093`) bails on `STATE_ENDED` (`:2075`) — and a truncated body with no error *is* ENDED — and is one-shot (`sessionlessStallRecoveryUsed`, `:2080-2093`). Media3's own loader retry sends `Range:` and gets a `200` from byte 0. | **No** |
| Offline package | Apple `OfflineDownloadManager.swift:439` (`URLSessionConfiguration.background`) | Phone sleeps mid-download → stops ACKing → reaped → `nsurlsessiond` resumes with `Range:` → `http/offline.rs` has no range handling → `200` from byte 0 | **No; restarts from zero, repeatedly** |
| HLS segment | hls.js, AVPlayer, Media3 | Retry the segment URL; segments are ≤ 64 MiB and a window-long stall inside one is never legitimate | yes, nothing new |
| Relayed peer body | the serving node | The relaying node's downstream pump already fails at `MEDIA_BODY_NO_PROGRESS_TIMEOUT` (`segment.rs:842-861`) and drops the peer response; the serving node's guard is a backstop behind it, ordered correctly | yes, nothing new |

Three rows say **No**. That is why Decision 2 changed between drafts: the
guard ships off, and turning it on has prerequisites the Developer tab
lists.

### 2.4 Deployments behind a reverse proxy

`deploy/cluster-routing/README.md:5-6` supports Caddy, nginx, Traefik and
ingress-nginx in front of plurxd. nginx with the default
`proxy_buffering on` buffers a paused client's response to disk (up to
`proxy_max_temp_file_size`, 1 GB by default), so plurxd's writes stay
`Ready` and the guard never arms; and `remote` in any log line is the proxy
hop, not the viewer. The guard is meaningful on bare-LAN deployments, which
is Paul's, and is documented as such (§6).

## 3. Design

### 3.1 One wrapper on the accepted stream

```
 listener.accept()
        │
        ▼
 TcpStream ──▶ StallGuardedIo { inner, window, pending_since, sleep, observer }
        │
        ▼
 TokioIo::new(guarded) ──▶ hyper serve_connection_with_upgrades
```

`StallGuardedIo<T: AsyncRead + AsyncWrite + Unpin>` lives in a new
`crates/plurxd/src/http/stall_guard.rs`, ≈ 150 lines with tests:

```rust
pub(crate) struct StallGuardedIo<T> {
    inner: T,
    /// Zero disables the guard: every method is a plain delegate.
    window: Duration,
    /// `Some` from the first `Pending` write until the next `Ready` write.
    pending_since: Option<tokio::time::Instant>,
    /// Allocated on the `None → Some` transition, dropped on `Ready`.
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    observer: Arc<dyn StallObserver>,
}
```

- `poll_write`, `poll_write_vectored`, `poll_flush`, `poll_shutdown`
  delegate to `inner`. On `Ready` they clear `pending_since` and drop
  `sleep`; if the pending run lasted > 5 s they emit the `debug` line
  `write stall cleared after {secs}s` (M3 reads this). On `Pending` they set
  `pending_since = Some(now)` and arm `sleep` at `now + window` if not
  already armed, then poll `sleep`: elapsed →
  `Ready(Err(io::Error::new(TimedOut, "peer took no bytes for {window:?}")))`
  after `observer.reaped(...)`; otherwise `Pending` with both wakers
  registered.
- **`is_write_vectored` delegates to `inner`.** Without it the wrapper
  inherits tokio's default `false`, hyper's `Buffered` picks the copying
  `Flatten` strategy (`h1/io.rs:60-64`) and memcpys every 128 KiB body chunk
  into its own buffer before writing — a regression on exactly the path
  being guarded that no wrapper-only test would notice. M1 has a test that
  proves hyper reaches the wrapper through `poll_write_vectored`.
- `poll_read` delegates unchanged. Reads are the header timer's business.
- The error surfaces as hyper's connection error, logged at `debug` today
  (`main.rs:2950-2952`); §3.3 raises the stall case to `warn` with context.

Why the transport and not the body: §2.1. Why one wrapper and not one per
route: every media route, present and future, is covered without being
asked. A JSON response can also trip it if it is megabytes long and the
peer stops reading; that is correct, not collateral.

Why the timer only runs while a write is pending: a `Pending` write means
the kernel could not take one more byte — the peer's receive window is
closed. That is the one observable that means "not draining". Time between
writes (a slow producer, a paused SIGSTOPped VOD encoder, a long-poll) is
the server's, not the peer's, and is bounded elsewhere.

Upgraded connections (`serve_connection_with_upgrades`) would inherit the
guard; there are no WebSocket or SSE routes in plurxd today (grep is
empty), so this is recorded for whoever adds one. Graceful shutdown is
unaffected: a stalled connection already only delays `graceful.shutdown()`
to the 5 s drain cap (`main.rs:2959-2977`). The
`TERMINAL_PROJECTION_SAFETY_WINDOW` const-assert (`media_sessions.rs:106-111`)
is untouched because the guard only ever shortens a body's life; for the
same reason the setting has no maximum.

### 3.2 The window, and the floor it implies

`playback.write_stall_secs`, **default 0 (off)**, minimum 30 when set.
Silo's 180 s is the recommended value once §4 Decision 2's prerequisites
are met, and is what M3 tests with.

The wording "one byte every few seconds is progress" from the first draft
was wrong. Linux reports writability (`POLLOUT`) only when free send-buffer
space is at least half of what is queued (`sk_stream_is_writeable`), so
tokio does not see `Ready` until roughly a third of `sndbuf` has drained.
With a 4 MiB autotuned buffer and a 180 s window the floor is ≈ 1.3 MiB per
window ≈ **60 kbit/s**; at the 30 s minimum it is ≈ 350 kbit/s. Silo
documents its floor the same way (`rolling_deadline.go:41`). Any real
viewer is orders of magnitude above this; a client below it is one that has
stopped.

**Where the value lives.** `serve_http` has no store
(`main.rs:2894-2898`), and a store read per accept is a DB hop and a
failure mode on the hot path. The window becomes a field of `HttpTimeouts`
(`main.rs:2829-2835`) as `write_stall: Arc<AtomicU64>` (seconds), seeded at
startup from the setting and updated by the settings `PUT` in
`http/system.rs` (the `HLS_BURST_SECS` write at `system.rs:3148` is the
pattern). The wrapper loads it at accept, so a change applies to new
connections without a restart and never moves a live deadline. This also
lets the existing `timeout_test_server(HttpTimeouts)` harness
(`main.rs:3808-3829`) drive M2's test.

### 3.3 Attribution

Each reap emits, at `warn`, target `plurxd::http::stall_guard`: `remote`,
`window_secs`, `pending_secs`, and the last request line seen on the
connection — method and path, **query string dropped** (tokens travel in
it) — captured by the per-accept `map_request` closure (`main.rs:2942-2946`,
built once per connection, so an `Arc<Mutex<Option<String>>>` shared with
the observer is correct; `TowerToHyperService` clones the service per
request, so the capture has to be that shared `Arc`, not a closure local).
When plurx is behind a proxy `remote` is the proxy; the line notes it when
the peer address is loopback. There is no trusted-upstream setting to
consult, and this plan does not add one.

`/metrics`: `plurx_http_write_stall_closed_total` (counter).

Developer tab (`http/developer.rs`, builder `readiness` at `:107`): item
`write_stall_guard`, `setting: Some("write_stall_secs")` — the
`SettingsUpdate` **API field**, not the store key; `developer.rs:78-86`
records that the first item to carry a store key passed its test by writing
nothing — `enabled: Some(window > 0)`. Requirements, advisory only (the module's
`an_unmet_prerequisite_does_not_block_the_switch` stays green):

| id | title | status today | evidence |
|---|---|---|---|
| `web_progressive_reopen` | Web reopens a progressive remux at position after a network error | `Unmet` | the web build number once `transport.js` handles code 2 by reopening, else "web build N: network errors stop playback" |
| `android_progressive_reopen` | Android restarts a progressive remux at position after a transport error or unexpected end | `Unmet` | Android versionCode once fixed |
| `offline_range` | Offline package downloads resume by `Range` | `Unmet` | met once `http/offline.rs` honours `Range` |
| `device_evidence` | M3 pause/resume run recorded | `Unmet` | link to §7 M3 results |
| `reaps` | Connections closed since boot | `Met` when enabled, `Unobservable` when off | the counter |

The item exists so the Developer tab says, in one place, what has to be
true before the default moves, and whether it is — never blocking the
switch.

### 3.4 HTTP/2 is not covered, and that is acceptable on the LAN

Under HTTP/2 a stalled *stream* exhausts its flow-control window; hyper
stops polling that body and the TCP write never goes `Pending`, so the
guard does not fire for one stalled stream on a multiplexed connection. No
plurx client negotiates h2 (no `H2_PRIOR_KNOWLEDGE` / `allowsHTTP2` in
`clients/`; reqwest over plain http is h1), but the auto builder does
accept h2c prior knowledge (`h2_keepalive_is_configured`, `main.rs:3989`, proves it) and a
Caddy or Traefik upstream can be configured to speak it — so behind such a
proxy the guard is reachable-but-blind, in addition to §2.4. Recorded so
nobody adds a per-stream timer here without knowing this was considered.

## 4. Decisions taken on Paul's behalf — his to overturn

1. **Transport-level guard, not per-body timers.** Reason in §2.1: only
   the write can close the socket. Trade-off: the guard cannot distinguish
   routes, so the client-side prerequisites in §3.3 apply to every path at
   once.
2. **Default off; the Developer item lists what makes turning it on safe.**
   The first draft shipped 180 s on and called the client fixes a
   follow-up. The review showed the web and Android progressive paths and
   the offline route do not resume (§2.3), so "on" would turn a pause into
   an error on three paths. Off-by-default with an advisory readiness list
   is the enable pattern this repo already uses; nothing is gated in code.
   The client fixes are named as requirements, not built here.
3. **180 s recommended, 30 s minimum.** Silo's number; ≈ 60 kbit/s floor
   (§3.2). M3 records the longest pending write seen per client during an
   unpaused play; if any exceeds 60 s the recommendation moves.
4. **No change to the HLS pump.** Its 30 s / 300 s timers stay; they bound
   file handles and pump tasks, which the connection guard does not.
5. **Offline `Range` support is a requirement, not part of this PR.** It
   is a separate route with its own tests; naming it here keeps this PR one
   thing.

## 5. Non-goals

- No total-lifetime cap on connections or bodies. That is the Go
  `WriteTimeout` mistake; a 40 GB direct play is allowed to take a day.
- No read-side stall timer, and no `SO_KEEPALIVE`. Request bodies are
  small; the header timer covers idle; the nothing-to-send dead peer (§2.2)
  is a lease problem.
- No per-route or per-client windows. One number, one place.
- No change to `tcp_retries2` or other sysctls in `deploy/`.
- No reaping of *sessions* — the guard closes a socket; session leases,
  `SESSION_IDLE_TTL` and the rolling lease are unchanged and keep deciding
  when producers die.
- No client fixes in this PR (§4 Decision 5 and §3.3's requirement rows).

## 6. Operator notes (goes into OPERATIONS.md in the same PR)

Off by default. Turn it on (`playback.write_stall_secs`, 180 recommended)
when the Developer tab's *Write-stall guard* item shows its client
prerequisites met, or when you accept that a viewer who pauses a
progressive remux or an offline download for longer than the window will
have to restart it. Symptom if you turned it on early: "playback stopped
after I paused for a while" on Chrome or Android with a remuxed title;
`docker logs plurxd | grep stall_guard` shows the reap. Behind a buffering
reverse proxy the guard sees the proxy, not the viewer, and rarely fires.

## 7. Milestones

### M1 — the wrapper, unit-proven, with hyper in the loop

- `http/stall_guard.rs`: `StallGuardedIo`, `StallObserver`, the `debug`
  cleared-line, lazy `Sleep`.
- Wrapper-only tests over `tokio::io::duplex` with `start_paused`:
  - a write that stays `Pending` past the window returns `TimedOut` and the
    observer sees exactly one reap with the right `pending_secs`;
  - `Pending` then `Ready` at window − 1 s resets, and a later `Pending`
    gets a full window again (the Silo throttle scar: a refresh must never
    inherit a partial window);
  - no write pending for 10 × window produces no reap;
  - `window = 0` is a pass-through, including `is_write_vectored`.
- **Hyper-in-the-loop test:** `hyper_util::server::conn::auto::Builder`
  over a `duplex()` pair wrapped in the guard, a 64 MiB direct-play-shaped
  body, a client that reads 1 MiB and stops. `tokio::io::DuplexStream`
  reports `is_write_vectored() == false`, so wrap it in a test shim that
  returns `true`, or hyper takes `Flatten` and the vectored assertion cannot
  hold. Assert: the reap fires through `poll_write_vectored` (a counting
  observer), the body's `poll_frame` (a counting body wrapper) was not
  called after hyper's buffer filled, and the connection future resolves
  `Err`. This is the test that proves §2.1 and finding 1; M2 repeats the
  vectored assertion over a real `TcpStream`.
- Acceptance: `cargo test -p plurxd stall_guard` green.

### M2 — wired in, attributed, settable

- `TokioIo::new(StallGuardedIo::new(stream, window, observer))` at
  `main.rs:2946-2947`; `write_stall` on `HttpTimeouts`; the setting key
  beside `HLS_READRATE` (`store/mod.rs:1816`) with `MIN_WRITE_STALL_SECS
  = 30`, the `Settings` / `SettingsUpdate` fields in `http/system.rs`
  (`:1663`, `:2449`), the web control in `web/pages/settings-playback.js`,
  and the atomic update from the `PUT`.
- Request-line capture; `warn` on reap; `/metrics` counter; Developer
  readiness item with the five requirement rows; OPERATIONS.md paragraph.
- Integration test on `timeout_test_server(HttpTimeouts)`: with
  `write_stall = 2 s`, a raw HTTP/1.1 client requests a 64 MiB direct play,
  reads 1 MiB, stops; assert the counter increments within 3 s (do not try
  to observe the close from a client that is not reading). Then the same
  client re-requests with `Range: bytes=1048576-` and receives `206`. With
  `write_stall = 0`, no reap within 1 s of the same stall (wall-clock kept
  short for the fast lane).
- Acceptance: fast lane green; both integration cases.

### M3 — device evidence, then the default is confirmed or moved

For the GPT session with the devices (write the prompt when M2 merges, in
the pattern of
[SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md](../clients/SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md)):

1. Set `playback.write_stall_secs = 30` on one node.
2. Direct play on Apple TV, iPhone, Android TV, Android tablet, Safari,
   Chrome: play, pause 60 s, resume, **pause 60 s again, resume**. Expect a
   `stall_guard` reap in the log each time and seamless resume both times.
   The second pause is the case that defeats one-shot recoveries.
3. Progressive `stream.mp4` on Chrome and Android: same double pause. Today
   this is expected to **fail** (§2.3); the result is the evidence the
   client-fix requirements need, not a surprise.
4. Offline download on iPhone: start, lock the phone for 2 min, unlock.
   Expect: restarts from zero today (§2.3); record it.
5. Restore the default. From the `write stall cleared after Ns` debug
   lines, record the longest pending write per client during an unpaused
   10-minute play; if any exceeds 60 s, revisit the 180 s recommendation.
- Acceptance: all results recorded in this section, pass or fail, with
  client build numbers; the Developer item's `device_evidence` row links
  here.

## 8. Open for Paul

Whether the web and Android progressive reopen-at-position fixes, and
offline `Range`, are worth doing at all — they only matter once this guard
is on, and the guard only matters for the leak in §2.2. If the answer is
no, the guard stays a disabled fence and this document records why.

## 9. Review log

Adversarial review 2026-09-26 (Claude, hyper 1.10.1 and hyper-util 0.1.20
source pulled to verify; verdict *reject as written*). Dispositions:

| # | Finding | Disposition |
|---|---|---|
| 1 | `is_write_vectored` not forwarded → hyper falls to `Flatten`, memcpy per chunk | **Accepted.** §3.1 bullet; M1 hyper-in-loop test |
| 2 | "read once per connection from the store" impossible in `serve_http`; store hop on accept | **Accepted.** `HttpTimeouts.write_stall: Arc<AtomicU64>` updated from the settings PUT, §3.2 |
| 3 | §2.3 cited comments, not handlers; web and Android progressive do not reopen; `stream_mp4` ignores `Range`; one-shot recoveries | **Accepted, decisive.** §2.3 rewritten from the handlers; Decision 2 flipped to default-off with advisory requirements; M3 double-pause |
| 4 | Offline package transfers covered, no `Range`, background `URLSession` resumes from zero | **Accepted.** §2.1/§2.3 rows, requirement `offline_range`, Decision 5 |
| 5 | Reverse proxies buffer and hide the viewer | **Accepted.** §2.4, §6, log annotation |
| 6 | "one byte every few seconds is progress" false; floor is `sndbuf`-derived | **Accepted.** Objective 2 and §3.2 rewritten with the real floor |
| 7 | §2.1 claim right but unevidenced | **Accepted.** Four hyper line references recorded |
| 8 | Reuse `timeout_test_server`; add the hyper-in-loop test; fix test timings | **Accepted.** M1, M2 |
| 9 | Setting plumbing sites | **Accepted.** M2 |
| 10 | Unspecified debug line; lazy `Sleep` | **Accepted.** §3.1 |
| 11 | Citations off by 2–3; `axum::serve` count; `readiness` not `items`; review § wrong | **Accepted.** Corrected throughout; header now says the review's §2.5 is adjacent, not the source |
| 12 | `tcp_retries2` bound; no `SO_KEEPALIVE`; dead-peer-with-nothing-to-send unbounded; JSON claim | **Accepted.** §2.2, §3.1 |
| 13 | Upgrades moot, graceful shutdown, const-assert, relay ordering unstated | **Accepted.** §3.1, §2.3 relay row |
| 14 | h2c reachable behind a proxy; server PING-while-idle unverified | **Accepted.** §3.4, §2.1 table |
| 15 | Observer capture must be the shared `Arc`; `observed` row needs a status | **Accepted.** §3.3 |

Round 2 (same reviewer): *approve with changes* — `setting` must be the API field not the store key (§3.3); no upstream setting exists (§3.3); `DuplexStream` is not vectored (M1); two citations; the `stream_mp4` `Range` claim marked unverified. All applied.
