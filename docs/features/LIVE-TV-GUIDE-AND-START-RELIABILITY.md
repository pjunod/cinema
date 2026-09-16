# Live TV reliability — why the guide is empty and why it says "wait 90 seconds"

**Status:** diagnosis complete, fix ruled on by Paul 2026-09-13 (§7), building on `effort/live-tv-reliability` ·
**Written:** 2026-09-13 · **Against:** `main` at `a124876` · **Evidence:**
media1's `/metrics` and container log, 2026-09-13 00:29 UTC (§10)

Companion to [LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md)
(what the guide feed and the page are) and
[HDHOMERUN-LIVE-TV-PLAN.md](HDHOMERUN-LIVE-TV-PLAN.md) (the tuner contract
and the start barrier as first designed). This document is *why both
misbehave on every client after every deploy, and the change that ends it*.
Both symptoms come from the same shape of decision: something the server
knows is guessed at by a client with a timer. The fix in each case is to
let the client ask.

Every file:line below is against `a124876`; re-verify at build time.

---

## 1. What you see, and what is actually happening

| You see | What is true at that moment |
|---|---|
| Live TV opens with channel numbers and callsigns but no programmes, on every client, until you go to Settings → Live TV and press *Refresh now* | The owner (media1) restarted — every deploy restarts every node — and the guide lives only in its memory. Its refresh loop ran once before the node was admitted to serve, was skipped, and went to sleep for the full 20 minutes. The web page asked once at open and never asks again; Apple and Android ask again 20 minutes after *they* opened. Nobody asks at the moment the data arrives. |
| "Start response was lost. Wait 90 seconds for any unclaimed tuner session to expire, then select the channel again." — with the tuner idle | Nothing is wrong with the server or the tuner. A marker file the client wrote when you last watched is still there, because the last session ended without a confirmed DELETE (tab closed, Apple TV suspended the app, the server was redeployed mid-stream). The client cannot ask the server what became of that session, so it refuses to start anything for 90 seconds from the moment you *open Live TV* — long after the server reaped the old session at 45 seconds idle. |

The tuner was never the constraint in either case.
`plurx_live_tv_starts_total{outcome="failed"}` on the owner is **0** across
eight starts today (§10); every 90-second wait you have hit was decided on
the client, and the one server answer that legitimately leaves the client
unsure (§3.4) lasts under a second.

Paul's rulings on the fix are in §7; the build is tracked in
[STATUS.md](../../STATUS.md).

---

## 2. The guide — four independent reasons it is empty, and they compound

### 2.1 The guide exists only in the owner's memory, so every deploy blanks it

`GuideCache` ([live_tv.rs 1480–1632](../../crates/plurxd/src/live_tv.rs))
is a `tokio::sync::Mutex<GuideCacheState>` with `cached: Option<CachedGuide>`
and nothing behind it. `CachedGuide.observed` is a `tokio::time::Instant` —
monotonic, process-scoped — which is why it *cannot* be persisted as written.
The plan that built it said so on purpose: guide plan §5.5, "No durable guide
cache. Memory on the owner; a restart refetches." The reason given there is
correct as far as it goes — the replicated `settings` table is not a cache —
but the consequence is that the fleet's deploy habit (`ansible` → every node
`reset --hard` + rebuild + restart; all four nodes restarted together at
23:40 UTC today, §10) empties the guide fleet-wide several times a week.

### 2.2 The first refresh after a restart is twenty minutes away

`guide_refresh_loop` ([live_tv.rs 3206–3252](../../crates/plurxd/src/live_tv.rs))
is spawned from [main.rs 2223](../../crates/plurxd/src/main.rs) and runs its
first tick immediately. The tick refreshes only when
`ours && config.guide_fetches() && self.serving.admit().is_some()`
(line 3220). At boot the serving fence has not admitted the node yet — media1's
log shows `serving authority recovered from a fresh quorum watermark` **9 s**
after the container started (23:40:12 → 23:40:21). So the first tick takes the
`else` branch at 3244–3247: it records `outcome="skipped"` and sets
`delay = GUIDE_REFRESH_INTERVAL` — **20 minutes**. The 60-second
`GUIDE_COLD_LINEUP_RETRY` exists for the cold case, but only on the error
path (3233–3237); the skip path never reaches it.

The metrics prove the timeline rather than just the code:
`guide_refresh_total{outcome="skipped"} 1`, `{outcome="ok"} 2`, and
`guide_age_seconds 526` at 00:28:40 — one skip at 23:40, one success at
≈ 00:00, one at ≈ 00:20. Between 23:40 and 00:00 every client showed
"The programme guide has no data yet". The three non-owner nodes show the
same skip count per tick (`skipped` 2–3) because they are not the owner,
which is correct, and reveals that the loop wakes only every 20 minutes.

### 2.3 A refresh cannot run until a client has read the lineup

`refresh_guide_with_cancel` reads the lineup **only** from the snapshot
cache (`cached_lineup`, 2841 and 2933–2944) and returns `cold_lineup`
without fetching when it is empty (2849–2853). The comment explains the
rule — "A guide refresh must never be a reason to talk to the tuner" — which
was written to stop the 20-minute loop hitting a switched-off tuner. But the
snapshot cache is also memory-only, so after a restart the loop cannot refresh
until *someone opens Live TV*, and even then only on its next wake-up. The
first viewer after a deploy is, by construction, the one who sees the empty
grid, and the refresh that would fill it is gated on their visit.

### 2.4 The clients do not come back for it

| Client | Asks for the guide | Asks again | Anchor |
|---|---|---|---|
| web | once, when the page renders | **never** for the life of the page | `loadLiveTvGuide` called only at [index.html 14939](../../crates/plurxd/src/web/index.html); the function at 15577 has no timer |
| Apple | on open | every **20 min** from *its* open, regardless of what it got | `startGuideRefresh`, [LiveTvView.swift 199–221](../../clients/apple/Sources/LiveTvView.swift) |
| Android | on open | every **20 min** from *its* open | `startGuideRefresh`, [LiveTvPlayer.kt 365–383, 409](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt) |
| non-owner node (ingress relay) | relays the owner's answer | remembers whatever it got for **60 s** — an `unavailable` answer included | `RELAY_GUIDE_MEMORY` [live_tv.rs 79](../../crates/plurxd/src/live_tv.rs); `remember_relayed_guide` is called for every `Ok`, [http/live_tv.rs 122–133](../../crates/plurxd/src/http/live_tv.rs) |

Twenty minutes is the *owner's* refresh cadence copied into the clients
("matching the owner's own refresh cadence", LiveTvView.swift 195). It is the
right number when the client already has a fresh guide and the wrong number
when it has none: the two situations are not distinguished anywhere.

### 2.5 The timeline, end to end

```
 23:40:12  every node restarts (deploy)          guide cache: EMPTY, lineup: EMPTY
 23:40:12  owner's guide loop, tick 1            serving not admitted → "skipped" → sleep 20 min
 23:40:21  serving authority recovered           (nobody wakes the loop)
 23:4x     you open Live TV on the Apple TV      GET channels → lineup fills
                                                 GET guide → unavailable → "no data yet"
                                                 client re-asks at 00:0x (20 min from open)
 00:00:12  owner's guide loop, tick 2            lineup warm → refresh OK, 1709 programmes
 00:0x     Apple re-polls                        guide appears — if you are still on the page
           web                                   still empty until you navigate away and back
```

Anything you did in the first 20 minutes — including *Refresh now*, which
calls `POST /live-tv/guide/refresh` and does work, but only as an admin and
only on the owner node ([http/live_tv.rs 184–190](../../crates/plurxd/src/http/live_tv.rs))
— was you doing the loop's job by hand.

---

## 3. The 90-second wait — a client-side timer standing in for a question

### 3.1 What the message is

`start_outcome_unknown` is never sent by the server. It is minted by each
client's **start barrier** when it holds a persisted marker it cannot resolve:

| Client | Barrier | Marker | Deadline |
|---|---|---|---|
| web | `StartBarrier`, [live-tv.js 128–189](../../crates/plurxd/src/web/live-tv.js) | `localStorage` keys `plurx_live_tv_pending_v1:<random>` | `performance.now() + 90000` on every `sync()` that sees a key it has not seen ([147](../../crates/plurxd/src/web/live-tv.js)) |
| Apple | `LiveTvStartBarrier`, [LiveTv.swift 629–676](../../clients/apple/Sources/LiveTv.swift) | file `live-tv-start.pending` (Caches on tvOS, Application Support elsewhere) | `ContinuousClock.now + 90 s` in `init` if the file exists ([640](../../clients/apple/Sources/LiveTv.swift)) |
| Android | `LiveTvStartBarrier`, [LiveTvLease.kt 58–95](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvLease.kt) | `AtomicFile` marker via `LiveTvFileBarrierStore` | `elapsedRealtime() + 90_000` in `init` if pending ([65](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvLease.kt)) |

The exact text on the iPhone — "The start response was lost. Wait 90
seconds for any unclaimed tuner session to expire before trying again." —
is [LiveTv.swift 459](../../clients/apple/Sources/LiveTv.swift), rendered
under a fully populated guide (Paul's screenshot, 2026-09-13): the two
symptoms are independent, and nothing was lost on that press — the app
refused on a marker left by the previous session.

The design is documented in HDHOMERUN-LIVE-TV-PLAN.md 291–298 and is
explicit about the trade: "This is conservative: … a restart may wait even
when the old session has already expired." That sentence is the symptom.

### 3.2 The marker is held for the whole session, so any unclean end taxes the next open

The marker is written before the POST and **kept while you watch**: the
web re-arms it on every keepalive (`LIVE_TV.cleanupMarker=LIVE_TV_BARRIER.hold(...)`,
[index.html 14884](../../crates/plurxd/src/web/index.html)); Apple's `acquired()`
clears only the in-memory deadline and leaves the file ("its disk marker must
survive a crash until DELETE is confirmed", LiveTv.swift 661–665); Android's
`acquired()` likewise ([LiveTvLease.kt 76](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvLease.kt)).
It is removed only after a DELETE that returned. Every ordinary way a Live
TV session ends on a television leaves it behind:

- **Apple TV: Home, or the app is jettisoned.** `scenePhase == .background`
  fires `Task { await live.stop() }` ([LiveTvView.swift 1304–1310](../../clients/apple/Sources/LiveTvView.swift)) —
  an 8-second cleanup request racing tvOS suspension. Lose the race, or get
  terminated later while suspended, and the file stays.
- **Web: closing the tab or the browser while watching.** The unload DELETE
  is sent with `keepalive` so it *reaches* the server, but the `confirm()`
  after the `await` never runs in a document that is gone
  ([index.html 14872–14881](../../crates/plurxd/src/web/index.html)). The
  server releases the tuner; `localStorage` still says "unconfirmed".
- **A deploy while you are watching.** The stream fails, the client sends
  DELETE to a server that is restarting, DELETE fails, and all three clients
  correctly *retain* the marker ("Cleanup is unconfirmed; use Stop to
  retry"). The next open waits 90 s for a session that died with the process.
- **Android: process death** for any reason while watching — same file,
  same result.

Meanwhile the server has already done the right thing: an activated session
with no keepalive is reaped at `CAPABILITY_IDLE_TIMEOUT` = **45 s**
([live_tv.rs 106, 3939](../../crates/plurxd/src/live_tv.rs)); a provisional
one that was never activated at `PROVISIONAL_TIMEOUT` = 40 s (109, 4015).
The tuner is free within a minute. The client's 90 s starts later.

### 3.3 The clock starts when you open Live TV, not when the session died

All three deadlines are *monotonic from the moment the barrier object is
created*: on the web, document boot (`LIVE_TV_BARRIER.sync()` at
[index.html 14824](../../crates/plurxd/src/web/index.html)); on Apple, the
first access to `LiveTvStartBarrier.shared` (a lazy `static let`), which is
`LiveTvLease.init` at [LiveTvView.swift 18 and 76](../../clients/apple/Sources/LiveTvView.swift);
on Android, `LiveTvPlayer`'s
constructor ([LiveTvPlayer.kt 45](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt)).
The wall-clock age of the marker is deliberately ignored (the 2026-09-05
review closed a wall-clock bypass). So a marker from yesterday evening
costs a full 90 s at tonight's first press — the reason it feels like
"every time I open it".

### 3.4 Typed refusals arm it too — and each client has its own list

A typed error *from the owner's worker* is a definitive answer: it is sent
"only after the owner has reaped FFmpeg, released admission, and closed the
tuner response" ([index.html 14825–14828](../../crates/plurxd/src/web/index.html)).
But not every typed body comes from the owner. `owner_unavailable` is also
minted by the *ingress* — for its own serving-fence refusal
([http/live_tv.rs 341–347](../../crates/plurxd/src/http/live_tv.rs)), for an
invalid or mismatched owner response (691–710), and for an unreachable or
timed-out owner via `peer_error` (722, 1131–1138) — and in the last case a
published-but-unanswered session may be holding a tuner for up to 40 s. So
that code genuinely *is* uncertain today, and arming on it is correct with
the tools the client has. What is not correct is what each client does with
the rest, because each decides for itself and the three lists disagree:

| Answer from the server | web ([14829, 14866](../../crates/plurxd/src/web/index.html)) | Apple ([LiveTv.swift 746](../../clients/apple/Sources/LiveTv.swift)) | Android ([LiveTvLease.kt 23–32](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvLease.kt)) |
|---|---|---|---|
| `owner_unavailable` (may be ingress-minted, see above) | **arms** | **arms** | **arms** |
| `startup_timeout` (owner: tuner sent no bytes in 15 s — "no signal") | **arms** | **arms** | **arms** |
| `stream_failed` (owner, after reaping) | clears | **arms** | **arms** |
| `tuner_unavailable`, `device_unavailable`, `codec_unsupported`, `capability_expired` | clears | clears | **arms** |
| any code a newer server adds | **arms** (allowlist) | clears (arms only the four above; a codeless error also arms) | **arms** (allowlist) |
| untyped 4xx | clears | **arms** | **arms** |

Two of the always-arm rows are common in this fleet. The ingress fence
refusal fires on every node restart, on every *other* node: lab4 restarting
at 00:04 produced `serving authority expired; mutable media is self-fenced`
on media1, lab6 and lab3 simultaneously at 00:03:58 for 0.2–0.4 s (§10). A
start pressed inside that window costs 90 s on every client — and the
ingress that refused it *knows* no tuner was touched, because it never
reached the owner. `startup_timeout` is the owner saying, after reaping,
that a channel has no signal right now — and the client answers by refusing
every *other* channel for a minute and a half.

The Apple shape is the one the 2026-09-05 review rejected on the web and
Android (HDHOMERUN-LIVE-TV-STATUS.md rows dated 2026-09-05, "denylist of
four"): an unknown typed code clears the marker there. Three clients, three
contracts, no fixture — the same drift the input contract was written to
end.

### 3.5 The server already has everything the client is guessing at

This is the part that makes the fix a fit rather than a rewrite. The
ingress→owner leg is *already* idempotent:

- `start_session` mints a `request_id` ([http/live_tv.rs 375](../../crates/plurxd/src/http/live_tv.rs))
  and the owner keys sessions by `LiveTvRequestKey { source_node_id,
  source_serving_generation, user_id, request_id }` ([live_tv.rs 1040–1055](../../crates/plurxd/src/live_tv.rs)).
- A replayed key returns the same session without consuming capacity
  (`request_session`, 1204–1234; `starts_recovered` metric), and a session
  that already ended replays its terminal error from a **tombstone** kept
  `TERMINAL_TOMBSTONE_TTL` = 60 s (143–144, 1210–1221).
- `owner_start` retries the same request once inside its 24 s budget
  (646–722), relying on exactly this.
- A replay is admitted only from the same ingress under the same serving
  generation: `request_session` compares the whole `LiveTvStartRequest`
  (1209–1231, 2211–2215) and `activate_local` refuses a signer that is not
  the start's `source_node_id` (2257–2262, 2274–2278). That is the right
  fence for the internal leg and stays.

The public leg has none of it. The client's marker id is random and never
sent; the ingress mints its own `request_id` per POST; a retried public POST
is a *new* request that can take a second tuner. That gap is the whole reason
the 90-second wait exists.

### 3.6 Side finding — the ingress gives up before the owner does

`START_EXCHANGE_TOTAL` = 24 s ([http/live_tv.rs 26](../../crates/plurxd/src/http/live_tv.rs))
while the owner's own start budget once bytes are flowing is
`STARTUP_FEEDING_TIMEOUT` = 30 s (live_tv.rs 101). An ATSC 3.0 channel that
publishes at 25 s (a real FLEX 4K did 18.1 s, live_tv.rs 85–86) makes the
ingress answer `owner_unavailable` for a channel the owner would have
started. The owner side is mostly safe: when the ingress drops the
connection, the owner handler's `StartupWaiter` is dropped and an
*unpublished* session is cancelled outright (`impl Drop for StartupWaiter`,
live_tv.rs 3409–3427); only a session that published in the last instant
and whose answer was lost survives, for ≤ 40 s (`provisional_expired`,
4015–4020). `owner_start`'s timeout path (713–721) has no capability to
stop, so it cannot close that window today. This only bites when the
client's node is not the owner (the Apple TV talks to media1, which *is* the
owner, so today it does not), but it is the same class of defect and §6.6
folds it in.

---

## 4. What a fix has to be, to fit

Four rules, each one a restatement of a decision already on record:

1. **The server owns the facts; a client renders them.** The guide plan's
   §2.1 makes the guide a feed the client draws; the tuner contract makes
   the owner the single authority on sessions. A client-side timer is a
   fact the server should have been asked for.
2. **One contract, one fixture, every client.** The input table
   (`tests/playback/player-input-contract.json` → `PlaybackPolicy.liveContractTiming`)
   is the model: timings and classifications live in a generated table the
   three clients embed, and a test proves they match.
3. **Nothing durable holds a credential.** The barrier persists only random
   marker identities today (live-tv.js 125–127); that stays true. A
   `request_id` is exactly such an identity — it becomes the thing the server
   knows it by.
4. **Attributable from inside the product.** The guide's age, next refresh,
   last error and persisted copy are Developer-card rows, advisory only,
   never a gate.

---

## 5. Fix A — the guide is a durable document the owner keeps current

### 5.1 Persist the owner's guide under the node's cache root (owner-only)

Add one file, `<cache_dir>/live-tv/guide.json`, beside the other
node-local persistent caches (`StorageConfig.cache_dir` — "persistent
node-local artwork, transcode, subtitle, and offline bytes",
[config.rs 73–75](../../crates/plurx-core/src/config.rs)) and **not** under
the authoritative `data_dir`: [main.rs 993–1030](../../crates/plurxd/src/main.rs)
keeps database and cache ownership disjoint by construction, and the new
directory joins that census as a managed persistent path. Written atomically
(`.tmp` + rename) by `GuideCache::store` on every successful refresh, read
once at construction, deleted by `invalidate()`. Contents: the `LiveTvGuide`
document, `fetched_at` (wall-clock, already a field) and the config
`generation` it was matched under — one file, generation inside, so nothing
accumulates. This honours the original reason for §5.5 — it is **not** the
replicated `settings` table, it never leaves the owner, and it carries no
`DeviceAuth` (the guide document never did; the existing `DeviceAuth`-absence
tests at live_tv.rs 8347–8394 cover the file by reading it back through the
same serialiser).

Freshness on boot comes from `fetched_at` against wall-clock, not from a
monotonic `observed`: `age = max(0, now − fetched_at)` (clamped, so a clock
step at boot cannot discard a good copy); served as `fresh` when
`≤ 20 min`, `stale` up to `GUIDE_STALE_TTL` = 6 h, and dropped past that
exactly as today. A deploy no longer blanks the grid; it makes it *stale for
as long as the restart took*, which is the honest state and is drawn as such
(the `freshness != "fresh"` banner at LiveTvView.swift 1380 already exists).

**What changes:** `CachedGuide.observed` becomes derived from `fetched_at`
for age purposes (keep the `Instant` for the in-process refresh cadence);
`GuideCache::default()` gains a `load(path)`; `store()` writes; `invalidate()`
deletes. A generation mismatch on load is discarded exactly as the in-memory
mismatch is today (loop line 3211–3218); `generation` is durable settings
state (live_tv.rs 223), so the comparison means the same thing across a
restart.

### 5.2 The refresh loop wakes on events, and a skip is not a 20-minute sleep

The loop keeps its 20-minute cadence for the steady state and gains three
wake-ups, each of which is an event the process already emits:

| Event | Where it already exists | Effect on the loop |
|---|---|---|
| serving authority admitted | `serving_fence` recovery (the INFO line at 23:40:21); `state.serving.subscribe()` is already handed to `serving_fence_loop` at main.rs 2192 | refresh now if the cache is missing or stale |
| lineup snapshot filled (first `local_snapshot` success under this generation) | the one write of `state.snapshot = Some(CachedSnapshot {…})` at live_tv.rs 1438 | refresh now if the last outcome was `cold_lineup` |
| Live TV settings saved (generation bump) | the loop already re-reads config each tick; `cancel_if_config_changed` at 1782 | invalidate + refresh now |

Implementation: a `tokio::sync::Notify` on `GuideCache` (`wake`, signalled
with `notify_one` so a wake that lands mid-refresh is kept as a permit
rather than lost), and the loop's `select!` at 3251 gains a third arm,
`_ = self.guide_cache.wake.notified()`. `LiveTvManager` holds
`serving.authority()` ([state.rs 914](../../crates/plurxd/src/state.rs)),
not the fence's subscription, so the admission wake is plumbed from the
`serving_fence_loop` subscriber at main.rs 2192 rather than read from the
manager. The **skip** path (3245–3248) sets `delay = GUIDE_COLD_LINEUP_RETRY` (60 s)
rather than the full interval — a node that is not yet admitted, or a source
that is not configured, is a state that changes in seconds, not in twenty
minutes. The 20-minute delay remains the answer after a *successful*
refresh and after a source/network failure with a warm lineup, as today.

### 5.3 The owner reads its own lineup when — and only when — it is cold

The "never a reason to talk to the tuner" rule was written against the
20-minute cadence. Narrow it, with the reason kept: when the snapshot cache
is **empty for this generation** (not merely older than the 30 s TTL), the
loop may call `local_snapshot(&config, false, false)` once per
`GUIDE_COLD_LINEUP_RETRY` before giving up with `cold_lineup`. A tuner that
is switched off answers `device_unavailable` in ≤ 2 s (`CONNECT_TIMEOUT`,
live_tv.rs 57) and the loop returns to its 60 s cold retry — one bounded
probe a minute against an absent device, versus today's "the first viewer
after every deploy sees no guide". A *warm-but-expired* snapshot still never
triggers a device read from the guide path. One visible side effect, stated
so nobody reads it as a regression: with the tuner unplugged, the device
projection and the Developer card's readiness row (live_tv.rs 1826–1830)
now flip to "unavailable" within a minute of boot instead of at the first
viewer's visit — which is the truer answer.

### 5.4 The guide document says when to come back, and the clients obey it

Add `next_refresh_at: i64` (unix seconds) to `LiveTvGuide` — the owner
knows its own next tick, and the ingress relays it unchanged. Clients replace
their hard-coded 20 minutes with:

```
 poll_at = max(next_refresh_at + 5 s, now + 15 s)      when freshness is fresh or stale
 poll_at = now + 30 s                                    when freshness is unavailable
```

The web gets the same controller-owned loop Apple and Android already have
(`startGuideRefresh` pattern; `loadLiveTvGuide` becomes its body and is
cancelled on route change like `LIVE_TV.timer` is). A guide that arrives at
00:00:12 is on every open screen by 00:00:45 instead of at the next
20-minute mark, and a page left open all evening tracks the owner's cadence
without a second timer of its own.

On the ingress, `remember_relayed_guide` keeps an `unavailable` answer for
**10 s** and a real one for `min(60 s, next_refresh_at − now)` — a
non-owner node should neither tell a client "no data" for a minute after
the owner filled in, nor hand out a pre-refresh copy (with its old
`next_refresh_at`) after the owner has moved on.

### 5.5 Developer card rows, advisory

The Live TV Developer card gains four rows from `guide_readiness`:
*Guide age* (`age_seconds`, with `fetched_at`), *Next refresh* (from
`next_refresh_at`), *Persisted copy* (path, size, generation, or "none"),
*Last refresh error* (already in `refresh_error`). Met/unmet colouring, no
gate — Paul's standing rule.

---

## 6. Fix B — a start you can ask about, instead of a start you wait out

### 6.1 The client's marker id becomes the public `request_id`

The public start body gains `request_id: string` — exactly 32 lower-case
hex characters (128 bits; the web marker is already 16 random bytes,
`liveTvMarkerId` at index.html 14818–14821; Apple and Android generate the
same shape), rejected
at the ingress otherwise. The ingress passes it through instead of minting
one (http/live_tv.rs 375). On the owner, the *public* replay key is
`(user_id, request_id)`; the fields compared for a replay are narrowed to
what the viewer chose — `channel_id`, `config_generation`, `playback` — so
that the ingress's own retry (which today compares the whole request,
1209–1231 and 2211–2215) and the activation ownership rule (a signer must be
the start's `source_node_id`, 2257–2262, 2274–2278) are unchanged, and a
replay from a *different* ingress is still `Conflict`. That is deliberate:
a client never replays across ingresses; it resolves (§6.3) and starts
fresh. Everything else at [live_tv.rs 1204–1234](../../crates/plurxd/src/live_tv.rs)
applies as-is: a replay joins the in-flight start (`starts_recovered`), and
a finished start replays its terminal error from the tombstone.

A replayed POST through the same ingress is therefore always safe: it can
never hold a second tuner for the same viewer, which was the entire threat
the 90-second wait defends against.

### 6.2 `DELETE /live-tv/starts/{request_id}` — the fence the client can pull

Public, authenticated as the user who owns the request; the ingress relays
it to the owner over the existing signed peer path (a new
`RETIRE_PATH` beside `STOP_PATH`, http/mod.rs 503–509 and the
`maintenance_route_eligible` allowlist at 596–690 — a retire must be admitted
during maintenance and drain exactly as `DELETE /sessions/{id}` is, or it is
refused precisely when it is needed). Semantics on the owner, and **every
answer is a typed body** — `{"outcome": "stopped" | "ended" | "retired"}`:

```
 request_id known, session live  ──▶ stop it (same path as DELETE /sessions/{id}) ──▶ 200 stopped
 request_id known, tombstoned    ──▶ 200 ended (nothing to stop)
 request_id unknown              ──▶ record a *retired* tombstone for (user, id) ──▶ 200 retired
 any later POST with that id     ──▶ 409 Conflict "request id retired"
```

The retired tombstone closes the last race: a POST that was in flight when
the client gave up cannot create a session *after* the client has been told
there is none. Retired tombstones are **per user, at most 8, oldest
evicted**, and are *not* counted against `MAX_TERMINAL_TOMBSTONES` = 256
(live_tv.rs 144, 2195–2199) — otherwise 256 retire calls from one
signed-in account would refuse every start on the owner for 60 s, which is
a denial of service any viewer could mount. Their TTL is the existing 60 s;
a late POST past that is impossible, because the client's own 45 s POST
timeout plus the ingress's exchange budget have both expired.

`GET /live-tv/starts/{request_id}` → `{state: starting|active|ended|retired|unknown,
code?}` is the read-only sibling for the status overlay and the Developer
card. It never returns the capability: a `request_id` is a handle that
*derives* a capability through replay, so the access log redacts
`starts/<id>` the way it redacts `sessions/<capability>`
([http/mod.rs 893–905](../../crates/plurxd/src/http/mod.rs) gains `starts`
as a marker) and the GET adds nothing to what the holder already has.

`POST /live-tv/starts/{request_id}/resume` is the reattach. For the
requesting user's own request it answers exactly what the original start
answered — the `LiveTvActivated` document, capability included — **if the
session is still live** (activated, producing, not reaped), and touches
`last_touch` so the heartbeat resumes from here; otherwise it answers the
tombstone (`ended`) or `retired`, never a capability. Handing the same user
back the capability they were already issued adds nothing a replayed POST
would not also return, so it is inside the existing policy; it is still
never persisted on the client, and the access log redacts the path.

The new flow is advertised, not assumed: `start_protocols` gains `3`
(live_tv.rs 1960, today `[1, 2]`), and a client uses `request_id` and the
retire route only when the owner snapshot lists it. On a fleet mid-rollout
an old ingress or owner answers the retire route with the unrouted-API
`404 {"error":"not found"}` (http/web.rs 279–286) — untyped, and treated as
**no answer** below, never as "nothing to stop".

### 6.3 The client never waits — it retires, starts, and lets the owner decide

Paul's ruling (2026-09-13): a tuner that *might* still be held by a lost
session is never a reason to refuse a viewer. There are four tuners; use
another one and let the stray get reaped. The client side of the barrier
is therefore deleted, not rewritten. What remains is a **retire hint** —
the persisted `{request_id}` of the last start — used at two moments.

**When Live TV is opened** (the controller is created: the tab on iOS and
tvOS, the screen on Android, the route on the web), before the viewer
touches anything, Paul's second ruling: *"if it was recent, reattach the
stream; if it has been a while, tell the server to kill it and wait for the
user to select something."* The server's own clock decides "recent" — a
session the owner has not reaped is one the viewer had under a minute ago:

```
 open Live TV, a hint exists
   POST /live-tv/starts/{id}/resume
       200 + session document ──▶ reattach: same capability, playlist, heartbeat;
                                  the picture is back without a press
       200 ended / retired    ──▶ clear the hint; show the channel list and wait
       no typed answer        ──▶ keep the hint; show the channel list and wait
```

This is not the "auto-tune on open" the guide plan's §5.8 forbids — no
tuner is taken; a stream the viewer already owns is rejoined. On the web,
a hint owned by another live document (BroadcastChannel answers) is left
to that document.

**When the viewer presses a channel:**

```
 press
   ├─ (best effort, does not block) DELETE /live-tv/starts/{id} for every persisted
   │    hint that no live document owns (web: see below) — frees the slot sooner
   └─ POST /live-tv/channels/{ch}/sessions with a fresh request_id, persisted first
         typed answer            ──▶ play, or show the error with its retry hint (§6.4)
         transport failure       ──▶ ONE replay POST with the same id
                                     (joins the same session, or gets its tombstone)
         still no answer         ──▶ show "the server did not answer", keep the hint;
                                     the next press does all of this again
```

Nothing on the client refuses a start. `start_outcome_unknown`,
`UNRESOLVED_WAIT` and the three barrier classes (live-tv.js 128–193,
LiveTv.swift 629–676, LiveTvLease.kt 58–95) go, and with them the message
in Paul's screenshot.

The owner decides admission, and it now has one more rule than today's
`sessions.len() >= max_sessions → tuner_capacity` (live_tv.rs 2201–2206):
**a viewer's own stray is evicted before the viewer is refused.** When
capacity is full and the requesting user owns a session whose `last_touch`
is older than two keepalive intervals, or whose request id has just been
retired, that session is cancelled and the new start admitted. The normal
45 s idle reap (3939) handles the common case with no capacity pressure at
all — the worst outcome of a lost start is one tuner wasted for under a
minute, which is the trade Paul chose over any viewer ever waiting.

Two rules the flow still depends on:

- **Only a typed body counts as an answer.** An untyped 404 or 5xx, and any
  code the ingress can mint without reaching the owner (`owner_unavailable`,
  `node_maintenance`, `node_removal_fenced`, `learner_route_ineligible`),
  proves nothing about the tuner — those keep the retire hint so the next
  press retires again. The web's untyped-4xx clearing at index.html 14866
  gets this wrong today.
- **The web must not retire another tab's live session.** Web hints are
  per-origin `localStorage`; a retire from tab B would stop tab A. So the
  owning document refreshes `touched_at` on every keepalive (the `hold` at
  index.html 14884 becomes a touch), and before retiring, the pressing
  document posts `{who: id}` on a `BroadcastChannel` (`plurx-live-tv`,
  works on private-LAN HTTP; Web Locks does not) and waits 250 ms for an
  `alive` — a live owner answers and its hint is left alone; a silent hint
  older than three keepalive intervals is an orphan and is retired. Apple
  and Android have one process per store and need neither.

What this changes in practice:

| Situation | Today | After |
|---|---|---|
| Apple TV Home while watching, app killed, reopened within the idle window | 90 s at next open | open → resume → the same stream is back, no press |
| Apple TV Home while watching, app killed, reopened later | 90 s at next open | open → `ended` → channel list; press → start → plays |
| tab closed while watching | 90 s at next open | press → BroadcastChannel silent → retire + start → plays |
| deploy while watching | 90 s at next open | press → retire (`200 retired`: the restarted owner has no such id; FFmpeg was a `kill_on_drop` child of PID 1 — `tokio::process::Child` at live_tv.rs 1101–1106, `kill_on_drop(true)` at 4280/4497; `ENTRYPOINT ["plurxd"]`, Dockerfile 165 — and the tuner's HTTP stream closed with it) + start → plays |
| POST response lost mid-flight | 90 s | one replay POST with the same id → joins the same session |
| fence blip at the moment of pressing (ingress `owner_unavailable`) | 90 s | error shown with "press again"; the next press plays |
| all four tuners busy and one is this viewer's stray | `tuner_capacity` | the stray is evicted, the start is admitted |
| server unreachable | 90 s | the error says so; press again when it is back |

### 6.4 One error table, owned by the server

With no barrier to arm, the typed codes have one remaining job: what the
client draws and whether it offers *Try again*. That decision moves out of
three client lists into the server's typed error envelope — `ApiError::typed`
for Live TV gains `retry: "now" | "later" | "never"` and
`owner_decided: bool` (false for the codes the ingress mints on its own,
§3.4, which is what keeps the retire hint per §6.3). The three per-client
sets (index.html 14829, LiveTv.swift 746, LiveTvLease.kt 23–32) are
deleted, and `tests/playback/live-tv-start-cases.json` carries the cases
all three must render identically, alongside `live-tv-guide-cases.json`.

`startup_timeout` is the owner saying, after reaping, that this channel
sent nothing in 15 s — `retry: "later"` for that channel (a signal fact of
the moment, not of the channel), and nothing at all for the others.

### 6.5 Session ends are logged and attributable

Today no Live TV session start or end is logged at INFO (the only nine
`tracing` sites in live_tv.rs are scratch sweeps, guide failures and
HDHomeRun request failures). Add one INFO line per session end
(`capability` redacted as the HTTP layer already does, `channel`, `reason`,
`duration`, `tuner_bytes`) and a `plurx_live_tv_session_ends_total{reason}`
breakdown beyond `terminal`. When the next "it didn't play" comes in, the
answer should be on the box, not re-derived from client code.

### 6.6 Budgets agree, and the ingress retires what it gives up on

`START_EXCHANGE_TOTAL` becomes `STARTUP_FEEDING_TIMEOUT + 6 s` (36 s) so
the ingress outlasts the owner's own budget, and the client POST timeouts
(45 s on all three today) stay above it. When `owner_start` exhausts its
budget (http/live_tv.rs 713–721) it has no capability to `owner_stop` — it
has only the `request_id` — so it calls the owner's retire path from §6.2
with it, closing the published-but-lost window §3.6 describes. With §6.1
the ingress's retry is keyed on the client's id, so a second ingress attempt
still joins the same session.

---

## 7. Four rulings from Paul (2026-09-13) — what they supersede

The first two come from [LIVE-TV-GUIDE-AND-UI-PLAN.md §5](LIVE-TV-GUIDE-AND-UI-PLAN.md#5-non-goals--guardrails-for-this-effort)
and were right for that effort's scope; the third overrides a review
finding on record; the fourth adds the reattach. Recorded here so nobody re-litigates them:

1. **§5.5 "No durable guide cache" — superseded.** The guide is cached
   (§5.1): node-local under the cache root, owner-only, never the
   replicated `settings` table. The reason behind §5.5 survives; the
   consequence ("a restart refetches", twenty minutes later) does not.
2. **§5.10 "No change to the lease, barrier, keepalive or status
   semantics" — superseded.** "There is a whole client/server protocol, so
   the client should never be guessing at something it could just ask."
   The barrier's client half is removed (§6.3); the owner answers.
3. **A possibly-held tuner is never a reason to refuse a viewer.** "There
   are three other tuners doing nothing. Even if a stream was still there,
   use another available tuner and let the other one get reaped." This
   overrides the 2026-09-05 review rows in HDHOMERUN-LIVE-TV-STATUS.md
   (146, 152) that rated "one viewer could hold two physical tuners" as
   critical: the accepted worst case is one tuner idle for ≤ 45 s, and the
   owner evicts the viewer's own stray first when capacity is actually full.

4. **On relaunch, reattach or kill — never wait.** "Make it tell the
   server to either reattach that stream if it is still recent, or if it
   has been a while, kill the stream … and wait for the user to select
   something." §6.3's open-time step; the owner's idle reap defines
   "recent".

Neither the `DeviceAuth` policy (§5.2) nor "no second tuner GET" (§5.4)
changes — the open-time resume rejoins a stream the viewer already owns.
§5.3's bounded cold-lineup probe is a lineup read, not a stream.

---

## 8. Milestones — task PRs on `effort/live-tv-reliability`, fast lane until the gate

Each PR opens as `WIP:`, gets one adversarial review, and records the
smallest focused regression for what it changed (AGENTS.md 49–52 — the
effort workflow defers the full suites to the lane's promotion gate, which
runs once). Server milestones and client milestones are on different CI
lanes; keep them in separate PRs.

### 8.1 M0 — contracts and fixtures (docs + tests only)

`tests/playback/live-tv-start-cases.json` (every typed code → confirm /
keep / retry), the live contract table gains `unresolved_start_wait_s` and
the two guide poll rules from §5.4, and this document's index row. The
`docs/API.md` rows for the new routes wait for M3: `test_api_doc_routes`
(`test_no_documented_path_is_invented`, line 243) rejects a documented
route that `http/mod.rs` does not register. **Accept:**
`python3 -m unittest tests.operations.test_docs_index` green; the contract
test fails until M1/M3 land (that is the point).

### 8.2 M1 — guide durability and the event-driven loop (server)

§5.1–§5.3 and `next_refresh_at`. **Accept:** a unit test constructs a
`GuideCache` from a written sidecar and serves `stale` with the right age;
a test proves the loop refreshes within `GUIDE_COLD_LINEUP_RETRY` of a
serving-admission wake with no client read; `make unit` green. On the
fleet: after a deploy, `plurx_live_tv_guide_age_seconds` on the owner is
never `NaN` and `guide_refresh_total{outcome="skipped"}` stops growing.

### 8.3 M2 — guide polling on the clients (web · Apple · Android)

§5.4 on all three; the web loop is the new part. **Accept:** open Live TV
< 60 s after a deploy on each client and watch the grid fill without
touching Settings; `make web-check`, `make apple-test` on `maca`,
`testDebugUnitTest` in the container.

### 8.4 M3 — public `request_id`, the starts endpoints, session-end logging (server)

§6.1, §6.2, §6.5, §6.6 and the same-viewer stray eviction from §6.3,
plus the `docs/API.md` rows and route-count bump.
**Accept:** registry unit cases for replay-joins, replay-conflict,
retired-id 409, per-user retire cap, retire on a
live/tombstoned/unknown id, and resume on a live/ended/retired id; `test_api_doc_routes` green; `plurx_live_tv_starts_total{outcome="recovered"}`
increments on a replayed public POST; ingress timeout stops the owner's
provisional session.

### 8.5 M4 — the barrier removal, from one table (web · Apple · Android)

§6.3 and §6.4. Delete the three barrier classes and the three
classification sets; keep the retire hint. **Accept:** the
`live-tv-start-cases.json` fixture passes on all three; a hint left by a
killed process is retired in the background and the start is not delayed,
in an instrumented test on each platform; kill the Apple TV app while
watching and reopen within 45 s — the same stream is back with no press;
reopen after two minutes — the channel list, and the first press plays.

### 8.6 M5 — Developer card rows, status doc, deploy

§5.5; a `LIVE-TV-RELIABILITY-STATUS.md` with the before/after metrics;
Apple build and Android `versionCode` bumps; Ansible deploy, physical
verification prompt for the GPT session.

---

## 9. Non-goals

- **No recording, no guide-driven tuning, no second guide source** — the
  guide plan's §5.1 and §5.3 stand.
- **No change to the 45 s idle reap or the 40 s provisional window.** They
  are the *reason* a lost start is safe to resolve by asking; shortening
  them buys nothing once the client asks.
- **No client-side refusal of any kind.** The retire hint is a courtesy to
  the tuner pool, never a gate; if it cannot be sent, the start goes ahead.
- **No cluster-replicated guide.** The owner serves it; ingress nodes relay
  it. Replicating derived third-party data through raft is cost without a
  reader.
- **No fixing the serving-fence blip here.** That a node restart fences
  every other node for 0.4 s is a cluster behaviour worth its own look; §6.4
  makes it cost a press instead of 90 s, which is all this document needs.

---

## 10. Evidence — media1, 2026-09-13 00:29 UTC

Owner is media1 (`10.42.5.236`); the Apple TV talks to it directly.
Container `plurxd` started `2026-09-12T23:40:12Z`; all four nodes restarted
within three minutes of each other (deploy).

```
plurx_live_tv_enabled 1
plurx_live_tv_starts_total{outcome="created"} 8
plurx_live_tv_starts_total{outcome="recovered"} 0
plurx_live_tv_starts_total{outcome="failed"} 0
plurx_live_tv_session_ends_total{reason="terminal"} 7
plurx_live_tv_sessions{state="active"} 1
plurx_live_tv_guide_refresh_total{source="hdhomerun",outcome="ok"} 2
plurx_live_tv_guide_refresh_total{source="hdhomerun",outcome="skipped"} 1
plurx_live_tv_guide_age_seconds 526
plurx_live_tv_guide_programmes 1709
```

```
2026-09-12T23:40:21Z INFO serving_fence: serving authority recovered from a fresh quorum watermark
2026-09-13T00:03:58Z WARN serving_fence: serving authority expired; mutable media is self-fenced
2026-09-13T00:03:58Z INFO serving_fence: serving authority recovered from a fresh quorum watermark
2026-09-13T00:03:58Z ERROR GET /api/v1/live-tv/sessions/[REDACTED]/segment-000232.ts → 503   (×4, 00:03:58–00:04:01)
```

lab6, lab4 and lab3 (non-owners): `guide_age_seconds NaN`,
`guide_refresh_total{outcome="skipped"}` 2–3 each, one identical fence blip
at 00:03:58 on lab6 and lab3, coinciding with lab4's second restart of the
evening at 00:04 (it also restarted with the others at 23:40). No
Live TV request is logged at INFO on any node; only the `on_failure` lines
above exist, so start latency and end reasons are not recoverable from the
box today (§6.5).
