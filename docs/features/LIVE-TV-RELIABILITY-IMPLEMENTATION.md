# Live TV reliability — the implementation plan

**Status:** ready for adversarial review, then build · **Executes:** Paul's
four rulings in [LIVE-TV-GUIDE-AND-START-RELIABILITY.md §7](LIVE-TV-GUIDE-AND-START-RELIABILITY.md#7-four-rulings-from-paul-2026-09-13--what-they-supersede) ·
**Written:** 2026-09-13 · **Against:** `main` at `a124876` · **Lane:**
`effort/live-tv-reliability` (exists, at `a124876`)

Read the diagnosis first — it says *why* every line below exists and carries
the fleet evidence. This document is the *what*, exactly: the interfaces,
the file:line seams, the tests that prove each piece, and the order. Work
milestone by milestone (§5); each milestone is one task PR into the lane,
opened `WIP:`, reviewed once adversarially, findings implemented, then the
smallest focused regression recorded in the PR body (AGENTS.md 49–52 —
full suites run once, at the lane's promotion). Every file:line is against
`a124876`; re-verify at build time, the file moves.

**Standing instruction.** If a step seems to require any of these, stop and
flag it rather than doing it: changing the `DeviceAuth` policy; changing a
*signed internal wire shape* (`LiveTvStartRequest`, `LiveTvStartRequestV2`,
`LiveTvActivateRequest`, `LiveTvStopRequest` — new *paths* are fine, new
fields on those bodies are not, `deny_unknown_fields` turns them into 400s
on an older owner mid-rollout); changing `CAPABILITY_IDLE_TIMEOUT`,
`PROVISIONAL_TIMEOUT`, `STARTUP_TIMEOUT` or `STARTUP_FEEDING_TIMEOUT`;
persisting a capability, token or tuner URL on any client; or re-introducing
any client-side refusal to start.

---

## 1. Objective — the behaviour a finished effort proves

1. **The guide survives a deploy.** Restart the owner; `GET /live-tv/guide`
   answers the previous guide as `stale` with its true age within seconds
   of the process serving, and `plurx_live_tv_guide_age_seconds` is never
   `NaN` on an owner that has ever refreshed.
2. **The first refresh after a restart is a minute away, not twenty.**
   `guide_refresh_total{outcome="skipped"}` stops growing on the owner
   after boot; a `fresh` guide is served within `GUIDE_COLD_LINEUP_RETRY`
   (60 s) of serving admission with no client having opened Live TV.
3. **Clients see the guide when it arrives.** Open Live TV on web, Apple
   and Android within a minute of a deploy: the grid fills without leaving
   the page or touching Settings, within 45 s of the owner's refresh.
4. **`start_outcome_unknown` no longer exists** — not in the server, not in
   any client string table, not in any fixture. The text in Paul's
   screenshot cannot be rendered.
5. **Opening Live TV after an unclean end is either the same stream back or
   the channel list.** Kill the Apple TV app while watching, reopen within
   45 s: the picture is back with no press. Reopen after two minutes: the
   channel list, and the first press plays.
6. **A stray never refuses a viewer.** With `max_sessions` = 4 and four
   sessions of which one is the viewer's own with no keepalive for ≥ 15 s,
   a new start from that viewer is admitted and the stray is cancelled;
   `tuner_capacity` is answered only when every slot is live and none is
   the viewer's.
7. **Every session end is on the box.** One INFO line per ended session
   with channel, reason, duration and tuner bytes;
   `plurx_live_tv_session_ends_total{reason=…}` broken out by reason.

---

## 2. Where the code is — the seams this plan touches

| Piece | File and anchor (at `a124876`) | What is there today |
|---|---|---|
| Guide document | [`crates/plurxd/src/live_tv/guide.rs` 134–171](../../crates/plurxd/src/live_tv/guide.rs) | `LiveTvGuide` + `unavailable()` |
| Guide cache | [`crates/plurxd/src/live_tv.rs` 1480–1632](../../crates/plurxd/src/live_tv.rs) | `GuideCache`, `GuideCacheState`, `CachedGuide`, `read`/`store`/`invalidate` |
| Guide refresh loop | live_tv.rs 3206–3252; spawned at [`main.rs` 2223](../../crates/plurxd/src/main.rs) | ticks at boot, `skipped` → 20 min |
| Cold lineup rule | live_tv.rs 2833–2853 (`cached_lineup` 2933–2944) | refuses with `cold_lineup` |
| Lineup snapshot cache | live_tv.rs 1354–1470 (`SnapshotCache::get_or_refresh`, write at 1438); called from `local_snapshot` 1804–1840 | the only lineup source |
| Relay memory | live_tv.rs 79 (`RELAY_GUIDE_MEMORY`), 3123–3134 | 60 s, any answer |
| Manager construction | live_tv.rs 1735–1770; callers [`state.rs` 910–917](../../crates/plurxd/src/state.rs), live_tv.rs 5861–5868 (test) | no store path |
| Guide metrics | live_tv.rs 1246–1330 (`observe_guide`, `guide_prometheus`) | age from an `Instant` |
| Public start | [`http/live_tv.rs` 330–401](../../crates/plurxd/src/http/live_tv.rs) (`PublicLiveTvStart`, `start_session`; `request_id` minted at 375; protocol gate 360–366) | ingress mints the id |
| Ingress→owner start | http/live_tv.rs 24–26 (budgets), 646–722 (`owner_start`), 885–921 (`owner_stop`) | 24 s total, no cleanup on timeout |
| Owner start / registry | live_tv.rs 1040–1055 (`LiveTvRequestKey`), 1186–1240 (`LiveTvRegistry`, `request_session`), 2048–2246 (`start_local_inner`; capacity at 2195–2206), 2247–2320 (`activate_local`; document at 2312–2319), 2691–2704 (`stop_local`), 2822–2848 (`retire_session`, tombstone) | keyed idempotency on the internal leg only |
| Internal peer routes | live_tv.rs 34–41 (path consts), [`http/internal_live_tv.rs`](../../crates/plurxd/src/http/internal_live_tv.rs) 226–248 (`stop` — the pattern), [`http/mod.rs`](../../crates/plurxd/src/http/mod.rs) 503–512 (registration), 650–657 (maintenance allowlist), 893–905 (access-log redaction) | `stop` is the template |
| Typed errors | [`http/error.rs`](../../crates/plurxd/src/http/error.rs) 52–90 (`typed`, `typed_detail`); http/live_tv.rs 1131–1200 (`peer_error`, `api_error`) | `typed_detail` already carries extra flat fields |
| Serving fence | [`serving_fence.rs`](../../crates/plurxd/src/serving_fence.rs) 224–226 (`subscribe`), 127–130 (`admit`) | watch channel exists; live_tv holds only `ServingAuthority` |
| Web | [`web/live-tv.js`](../../crates/plurxd/src/web/live-tv.js) 8–41 (`RETRYABLE_CODES`, `errorView`), 125–193 (`StartBarrier`), 195–275 (`Lease`); [`web/index.html`](../../crates/plurxd/src/web/index.html) 14815–14890 (barrier wiring, `LIVE_TV_DEFINITIVE_REFUSALS` 14829, `LIVE_TV_LEASE`), 14939 + 15577–15590 (`loadLiveTvGuide`), 15676 (keepalive timer), 15785, 15798 (storage listeners) | |
| Apple | [`LiveTv.swift`](../../clients/apple/Sources/LiveTv.swift) 459 (string), 533 (untyped POST → unknown), 629–707 (`LiveTvStartBarrier`, `LiveTvFileBarrierStore`), 722–800 (`LiveTvLease`); [`LiveTvView.swift`](../../clients/apple/Sources/LiveTvView.swift) 18/76 (lease creation), 120–140 (heartbeat), 199–221 (`startGuideRefresh`), 1304–1310 (background stop) | build `146` (`project.yml` 18) |
| Android | [`LiveTvLease.kt`](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvLease.kt) 23–32 (`DEFINITIVE_START_REFUSALS`), 58–95 (barrier), 100–175 (lease); [`LiveTvPlayer.kt`](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt) 45, 88, 181 (5 s heartbeat), 365–383, 409; [`LiveTvApi.kt`](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt) 306, 384, 404–427, 450 | `versionCode = 89` (`build.gradle.kts` 48) |
| Shared fixtures | `tests/playback/live-tv-guide-cases.json` (consumed by `tests/web/live-tv.test.js` 702–705, `LiveTvTests.swift` 476/552 via `project.yml` 121/143, `LiveTvGuideTest.kt`); `tests/playback/player-input-contract.json` `live` section → `scripts/player-contract-table --embed` → `playback-policy.js` 56–60 | the model for the new fixture and timings |
| Docs/tests that gate | `docs/API.md` line 17 ("plurx has 187 routes", checked by `tests/operations/test_api_doc_routes.py` 243/270); `docs/README.md` index (`tests/operations/test_docs_index.py`); `tests/client-fixes.toml` + `validation/regressions.d/` (history-check, for every `fix(...)` client commit); `make apple-build-bump`; `clients/android/README.md` build claim | |

---

## 3. Contract — exact interfaces

### 3.1 The guide document carries `next_refresh_at`

`guide.rs` 134–152, add after `refresh_error`:

```rust
    /// When the owner's loop next intends to refresh, unix seconds. Clients
    /// poll on this rather than on a cadence of their own. Absent only when
    /// the loop has not run yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_refresh_at: Option<i64>,
```

Every literal construction gains `next_refresh_at: None`: `unavailable()`
(guide.rs 155–168), `fetch_guide` (live_tv.rs 3003–3013), and the two test
literals at live_tv.rs 7895 (`guide_with`) and 8157. The compiler finds
them; there are exactly four.

### 3.2 The persisted guide file

Path: `<runtime cache root>/live-tv/guide.json`. `runtime_cache` is the
`Dirs` field destructured at state.rs 823 and is still owned at 910 (it is
moved into `AppState` at 940), so `LiveTvManager::new` gains a seventh
parameter and state.rs 910–917 passes `runtime_cache.join("live-tv")`; the
test constructor at live_tv.rs 5861–5868 passes `root.join("live-tv-guide")`.
Do not put it under `data_dir` (main.rs 993–1030 keeps the authoritative
root and every cache root disjoint by census) and do not add a census
entry: a child of an already-managed persistent root needs none.

```rust
/// The on-disk shape. `fetched_at` is wall-clock, which is what makes the
/// age meaningful across a restart; the in-memory `observed` instant is
/// reconstructed from it on load.
#[derive(Serialize, Deserialize)]
struct PersistedGuide {
    version: u32,          // PERSISTED_GUIDE_VERSION = 1; anything else is ignored
    generation: i64,       // settings generation it was matched under
    fetched_at: i64,       // unix seconds
    guide: LiveTvGuide,    // the public document, verbatim — no DeviceAuth by construction
}
```

Load rules (`GuideCache::load_persisted(path) -> Option<CachedGuide>`):
unreadable or wrong `version` → `None` + one `tracing::warn!`;
`age = unix_seconds().saturating_sub(fetched_at).max(0)` (clamped — a
clock step at boot must not discard a good copy); `age > GUIDE_STALE_TTL`
→ `None` (left on disk; the next success overwrites it);
`observed = Instant::now().checked_sub(age).unwrap_or_else(Instant::now)`
(a machine up for less time than the guide's age has no such instant; the
copy is then served as if fetched at boot — fresher than it is, never
staler). Write rules: serialise → `guide.json.tmp` → `rename`, inside
`spawn_blocking`, **after** the state mutex is released; failure is a
`warn!`, never an error to the refresh. `invalidate()` removes the file
(`NotFound` is fine).

### 3.3 `GuideCache` changes

```rust
struct GuideCache {
    state: tokio::sync::Mutex<GuideCacheState>,
    refresh_admission: Arc<tokio::sync::Semaphore>,
    refresh_cancel: StdMutex<Option<(LiveTvConfig, CancellationToken)>>,
    next_sequence: AtomicU64,
    persist: Option<PathBuf>,                 // NEW: None in Default, Some in with_store
    wake: Arc<tokio::sync::Notify>,           // NEW: shared with SnapshotCache (§3.4)
    observed_generation: AtomicI64,           // NEW: i64::MIN until first observe
}
struct GuideCacheState {
    cached: Option<CachedGuide>,
    published_sequence: u64,
    last_error: Option<(i64, String)>,
    next_refresh_at: Option<i64>,             // NEW
}
impl GuideCache {
    fn with_store(dir: PathBuf) -> Self;                 // loads `<dir>/guide.json`
    fn load_persisted(path: &Path) -> Option<CachedGuide>;
    async fn persist_guide(&self, cached: &CachedGuide);
    async fn discard_persisted(&self);
    fn wake(&self);                                      // notify_one — a permit, not a broadcast
    fn observe_generation(&self, generation: i64);       // wakes iff previous != MIN && previous != generation
    async fn note_next_refresh(&self, at: i64);
}
```

`read()` sets `guide.next_refresh_at = state.next_refresh_at` on **both**
branches — an `unavailable` answer still says when to ask again.
`store()` on `Ok` clones the new `CachedGuide` out, drops the lock, then
`persist_guide`. `Default` stays what it is (tests use it).

`LiveTvManager::new` builds `GuideCache::with_store(guide_store)` and, after
the `Arc` exists, calls `manager.adopt_persisted_guide()`:
`state.try_lock()` (uncontended at construction), and if `cached` is
`Some`, `metrics.observe_guide_at(cached.observed, total_programmes)` and
`publish_guide_titles(&cached.guide)`. `observe_guide_at` is a new sibling
of `observe_guide` (live_tv.rs 1268–1274) taking the instant explicitly —
the age gauge must report the copy's real age, not a fresh fetch.

`observe_config` (live_tv.rs 1782) gains one line after
`cancel_if_config_changed`: `self.guide_cache.observe_generation(config.generation)`.

### 3.4 The refresh loop wakes on events

**The lineup-fill wake is signalled from the manager, not from inside
`SnapshotCache`** — `SnapshotCache::get_or_refresh` (1369) has no reference
to the manager, and `self.guide_cache` does not exist there (this was the
first thing the compiler said about a draft that tried). Give
`SnapshotCache` a `lineup_filled: Arc<tokio::sync::Notify>` and clone the
same `Arc` into `GuideCache.wake` at construction; at the write site
(1438) compute `was_cold` *before* replacing `state.snapshot`:

```rust
let was_cold = state
    .snapshot
    .as_ref()
    .is_none_or(|cached| cached.generation != generation);
state.snapshot = Some(CachedSnapshot { generation, observed, snapshot: snapshot.clone() });
if was_cold {
    self.lineup_filled.notify_one();
}
```

The loop's new signature and body (replaces 3206–3252; main.rs 2223 passes
`state.serving.subscribe()` — the same receiver `serving_fence_loop` takes
at main.rs 2192):

```rust
pub(crate) async fn guide_refresh_loop(
    self: Arc<Self>,
    mut serving: tokio::sync::watch::Receiver<crate::serving_fence::ServingState>,
    shutdown: CancellationToken,
) {
    let mut delay = guide::GUIDE_REFRESH_INTERVAL;
    let mut serving_open = true;
    loop {
        if let Ok(config) = self.config().await {
            // … invalidation block unchanged (3210–3218) …
            if ours && config.guide_fetches() && self.serving.admit().is_some() {
                // §5.3 of the diagnosis: one bounded lineup read when nothing has
                // been read yet. `local_snapshot` is the existing entry point; its
                // own CONNECT_TIMEOUT (2 s) bounds a switched-off tuner.
                if self.cached_lineup(&config).await.is_empty() {
                    let _ = self.local_snapshot(&config, false, false).await;
                }
                // … refresh_guide_with_cancel + match, unchanged (3221–3243) …
            } else {
                delay = guide_skip_delay(ours && config.guide_fetches());
                self.metrics.observe_guide_refresh(config.guide_source, "skipped");
            }
        }
        self.guide_cache
            .note_next_refresh(unix_seconds().saturating_add(delay.as_secs() as i64))
            .await;
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
            _ = self.guide_cache.wake.notified() => {}
            changed = async {
                if serving_open { serving.changed().await } else { std::future::pending().await }
            } => { if changed.is_err() { serving_open = false; } }
        }
    }
}

/// The owner with a source that is merely not admitted yet is seconds from
/// being admitted; every other skip (not the owner, no source) changes only
/// with a settings save, which wakes the loop itself.
fn guide_skip_delay(owner_with_source: bool) -> Duration {
    if owner_with_source { guide::GUIDE_COLD_LINEUP_RETRY } else { guide::GUIDE_REFRESH_INTERVAL }
}
```

A fence *loss* also fires `changed()`; the tick then sees `admit()` is
`None`, records `skipped`, and sleeps 60 s. That is cheap and correct.

### 3.5 Relay memory on a non-owner node

```rust
const RELAY_UNAVAILABLE_GUIDE_MEMORY: Duration = Duration::from_secs(10);   // beside RELAY_GUIDE_MEMORY, live_tv.rs 79

/// How long an ingress may repeat the owner's answer without asking again.
fn relay_guide_memory(guide: &LiveTvGuide, now: i64) -> Duration {
    if guide.freshness == GuideFreshness::Unavailable {
        return RELAY_UNAVAILABLE_GUIDE_MEMORY;
    }
    match guide.next_refresh_at {
        Some(next) => RELAY_GUIDE_MEMORY.min(Duration::from_secs(next.saturating_sub(now).max(0) as u64)),
        None => RELAY_GUIDE_MEMORY,
    }
}
```

`relayed_guide` (3124–3130) compares against `relay_guide_memory(guide,
unix_seconds())` instead of the constant. Nothing else on the relay path
changes; `next_refresh_at` rides through `clipped()` untouched because
`clipped` clones the whole document.

### 3.6 The public start body carries the client's `request_id`

http/live_tv.rs 330–333 becomes:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicLiveTvStart {
    #[serde(default)]
    playback: Option<LivePlaybackRequest>,
    /// The client's durable identity for this start — 32 lower-case hex
    /// characters (128 bits), the marker id the clients already generate.
    /// Replaying it joins the same owner session instead of taking a second
    /// tuner. Absent from an older client; the ingress then mints one.
    #[serde(default)]
    request_id: Option<String>,
}
```

Validation at the ingress: `^[0-9a-f]{32}$`, else `400 invalid_request`.
`start_session` (375) uses it verbatim as `LiveTvStartRequest.request_id`
when present — **no internal wire change**: the field already exists on
the signed v1/v2 bodies, its content was always an opaque string, and the
owner key (`LiveTvRequestKey`, 1040–1055) already includes `user_id`. When
absent, `uuid::Uuid::new_v4()` as today. Advertise it: `start_protocols:
vec![1, 2]` at live_tv.rs 1960 becomes `vec![1, 2, 3]`, and the public
snapshot exposes it exactly as `2` is exposed today (the `owner_protocols`
read at http/live_tv.rs 360–366 is the model). Clients send `request_id`
and use the routes in §3.10 only when the snapshot lists `3`.

Replay semantics are the owner's existing ones and are **not** widened:
the same ingress under the same serving generation joins the in-flight
session or gets its tombstone (`request_session` 1208–1234); a different
ingress or a bumped generation is a different key and therefore a fresh
start. Paul's ruling makes that acceptable (the stray reaps at 45 s, or is
evicted under §3.7); the client never replays across ingresses anyway.

### 3.7 Owner registry — retired ids and same-viewer stray eviction

```rust
const MAX_RETIRED_PER_USER: usize = 8;                              // beside MAX_TERMINAL_TOMBSTONES, 144
const RETIRED_TTL: Duration = TERMINAL_TOMBSTONE_TTL;               // 60 s
/// A session of the requesting viewer whose last keepalive is older than
/// this is a stray, and is cancelled to admit that viewer's new start when
/// the slots are full. Three missed 5 s heartbeats.
const STRAY_EVICTION_IDLE: Duration = Duration::from_secs(15);

struct LiveTvRegistry {
    // … existing fields …
    /// (user_id, request_id) → when it expires. Per user, at most
    /// MAX_RETIRED_PER_USER, oldest evicted. NOT counted against
    /// MAX_TERMINAL_TOMBSTONES: any signed-in viewer could otherwise fill
    /// the shared cap with 256 retire calls and refuse every start for a
    /// minute.
    retired: HashMap<(i64, String), tokio::time::Instant>,
}
```

`request_session` (1208): after `prune_terminals()`, also prune `retired`
by time; if `(request.user_id, request.request_id)` is retired → `Err(LiveTvError::Conflict("the live-TV request id was retired"))`
(surfaces as `409 settings_conflict` through the existing `Conflict` arm of
`api_error` — do not add a code; a client that hits this made a programming
error, not a user-facing state).

Lookup helpers on `LiveTvRegistry` (all O(sessions), fine — the map holds
≤ 4 live sessions and ≤ 256 tombstones):

```rust
fn session_for_public_request(&self, user_id: i64, request_id: &str) -> Option<Arc<LiveTvSession>>;
   // scans `requests` keys for key.user_id == user_id && key.request_id == request_id
fn tombstone_for_public_request(&self, user_id: i64, request_id: &str) -> Option<&LiveTvTerminalTombstone>;
   // scans `terminals` values by entry.request.{user_id, request_id}
fn retire(&mut self, user_id: i64, request_id: String, now: Instant);
   // insert with now + RETIRED_TTL; evict the user's oldest past MAX_RETIRED_PER_USER
```

The capacity branch at 2195–2206 becomes:

```rust
if registry.sessions.len() + registry.terminals.len() >= MAX_TERMINAL_TOMBSTONES { /* unchanged */ }
let live = registry.sessions.values().filter(|s| !s.cancel.is_cancelled()).count();
if live >= usize::from(config.max_sessions) {
    // Paul's ruling: a possibly-held tuner is never a reason to refuse a
    // viewer. This viewer's own stray — no keepalive for STRAY_EVICTION_IDLE,
    // or a request id they have since retired — is cancelled first.
    let stray = registry.sessions.values()
        .filter(|s| s.request.user_id == request.user_id && !s.cancel.is_cancelled())
        .filter(|s| {
            let state = s.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            now.duration_since(state.last_touch) >= STRAY_EVICTION_IDLE
        })
        .min_by_key(|s| s.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last_touch)
        .cloned();
    match stray {
        Some(stray) => { stray.cancel.cancel(); self.metrics.observe_stray_eviction(); }
        None => return Err(LiveTvError::Capacity(format!("all {} plurx Live TV session slots are in use", config.max_sessions))),
    }
}
```

A cancelled session stays in `sessions` until its worker exits (a few
hundred ms — `kill_on_drop`, live_tv.rs 4280/4497); the new start's tuner
GET follows a lineup refresh and lands after it. If the HDHomeRun still
reports every tuner busy, the start fails with the owner's existing typed
`tuner_unavailable`, which the client renders with `retry: "now"` — the
next press finds the slot free.

### 3.8 Owner operations: retire and resume

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveTvRetireOutcome { Stopped, Ended, Retired }

#[derive(Serialize, Deserialize)]
pub(crate) struct LiveTvResumeAnswer {
    pub(crate) outcome: LiveTvResumeOutcome,        // live | ended | retired
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<LiveTvActivated>,    // present iff outcome == live
}

impl LiveTvManager {
    /// Stop whatever this viewer's request produced, and make sure nothing
    /// can be produced from it later.
    pub(crate) async fn retire_local(&self, user_id: i64, request_id: &str) -> LiveTvRetireOutcome {
        // 1. live session for (user, id)  → cancel_and_wait(vec![session]) (ignore Err) → Stopped
        // 2. tombstone for (user, id)     → Ended
        // 3. neither                      → registry.retire(user_id, request_id, now) → Retired
        // In all three cases the id is also inserted into `retired`, so a POST that
        // was in flight when the client gave up gets Conflict, never a tuner.
    }

    /// The same document the original activation answered, for a session
    /// that is still live and still this viewer's.
    pub(crate) async fn resume_local(&self, user_id: i64, request_id: &str) -> LiveTvResumeAnswer {
        // live session, state.activated, phase == Active, !cancel.is_cancelled()
        //   → state.last_touch = now; Live + document built exactly as at 2312–2319
        //     (channel_with_source_format, output, delivery, playlist_url, live: true)
        // tombstoned → Ended;  otherwise → Retired (and registry.retire(...))
    }
}
```

`resume` deliberately does not admit a `Provisional` (published, not yet
activated) session: activation is the ingress's job inside the original
start, and a start whose activation never completed is one the ingress
already stopped (http/live_tv.rs 386–390) or that the provisional timeout
will reap.

### 3.9 Internal peer paths — `retire` and `resume`, modelled on `stop`

```rust
pub(crate) const RETIRE_PATH: &str = "/_internal/v1/live-tv/retire";    // live_tv.rs 34–41
pub(crate) const RESUME_PATH: &str = "/_internal/v1/live-tv/resume";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvRetireRequest {     // and LiveTvResumeRequest, same three fields
    pub(crate) expected_owner_node_id: String,
    pub(crate) user_id: i64,
    pub(crate) request_id: String,
}
```

Handlers in `internal_live_tv.rs`, copied from `stop` (226–248):
`authorize(&state, &headers, &body, RETIRE_PATH)`, parse, `expected_owner_node_id
!= node_id → 409`, call the local op, `signed_json_response` with the
outcome/answer body. Register in `http/mod.rs` next to `STOP_PATH`
(503–512) with `DefaultBodyLimit::max(1_024)`. Add both to the maintenance
allowlist `matches!` at 650–657 — a retire must be admitted during
maintenance and drain exactly as `STOP_PATH` is, or it is refused precisely
when it is needed.

Ingress side (`http/live_tv.rs`, beside `owner_stop` 885–921):
`owner_retire(state, config, user_id, request_id) -> Result<LiveTvRetireOutcome, ApiError>`
and `owner_resume(...) -> Result<LiveTvResumeAnswer, ApiError>`; local
when `config.owner_node_id == state.node_id`, else `PeerTransport` POST with
`deadline_after(CONTROL_EXCHANGE_DEADLINE)` (5 s), `ExactRequestAndResponse`.
An owner that answers **404** to either path is an older build: map to
`ApiError::typed_detail(503, "owner_unavailable", "the tuner owner does not
retire starts yet", {"owner_decided": false, "retry": "now"})` — a typed
answer that proves nothing about the tuner, which is exactly what the
client's rule in §3.16 needs it to be.

### 3.10 Public routes

| Method | Path | Auth | Answer |
|---|---|---|---|
| `DELETE` | `/api/v1/live-tv/starts/{request_id}` | bearer (`AuthUser`) | `200 {"outcome": "stopped" \| "ended" \| "retired"}` |
| `POST` | `/api/v1/live-tv/starts/{request_id}/resume` | bearer | `200 {"outcome": "live", "session": <LiveTvActivated>}` or `200 {"outcome": "ended" \| "retired"}` |
| `GET` | `/api/v1/live-tv/starts/{request_id}` | bearer | `200 {"state": "starting" \| "active" \| "ended" \| "retired" \| "unknown", "code"?: <terminal code>}` — never a capability |

All three validate `request_id` as `^[0-9a-f]{32}$` (400 otherwise) and
answer `503 live_tv_disabled` when Live TV is off, exactly as
`guide_document` does (http/live_tv.rs 72–86). Registration in
`http/mod.rs` 90–119 beside the session routes. Three more places, each
with a test that fails without it:

- **Redaction** (mod.rs 893–905): add `"starts"` to the marker list with
  `index > 0 && segments[index - 1] == "live-tv"` — a request id is a
  handle that *derives* a capability through replay and resume, so the
  access log must not carry it.
- **Maintenance eligibility** (mod.rs 665–690): the `DELETE` and the
  `POST …/resume` are admitted during maintenance exactly as
  `DELETE /api/v1/live-tv/sessions/{capability}` is (the DELETE arm at 678–684).
- **`docs/API.md`**: three rows in the Live TV table (around 1825–1827)
  and "plurx has 187 routes" → `190` on line 17;
  `test_api_doc_routes` (`test_no_documented_path_is_invented` 243, count
  270) fails until both are right. These rows land in the **same PR** as
  the routes (M3), never earlier.

### 3.11 The typed error envelope says what to do next

Every Live TV error leaves `api_error()` (http/live_tv.rs 1140–1200) as
`ApiError::typed_detail` with two extra flat fields — `typed_detail`
already exists (error.rs 67–90) and writes `code`/`message` last, so an
older client reading only those two is unaffected:

| `LiveTvError` variant → code | `retry` | `owner_decided` |
|---|---|---|
| `Disabled` → `live_tv_disabled` | `never` | `true` |
| `InvalidConfig` / `InvalidResponse` → `invalid_settings` / `live_tv_protocol_unready` | `later` | `true` |
| `DeviceUnavailable` → `device_unavailable` | `later` | `true` |
| `OwnerUnavailable` → `owner_unavailable` — **from the owner's worker** (`ensure_session_fence`, 4022–4027) | `now` | `true` |
| `OwnerUnavailable` — **minted by the ingress** (fence refusal 341–347, mismatched/invalid response 691–710, `peer_error` 1131–1138) | `now` | **`false`** |
| `Capacity` → `tuner_capacity` | `later` | `true` |
| `TunerUnavailable` → `tuner_unavailable` | `now` | `true` |
| `ChannelNotFound` → `channel_not_found` | `never` | `true` |
| `DrmUnsupported` / `CodecUnsupported` | `never` | `true` |
| `StartupTimeout` → `startup_timeout` | `later` (this channel; nothing for the others) | `true` |
| `StreamFailed` → `stream_failed` | `now` | `true` |
| `Conflict` → `settings_conflict`, `SourceFormatChanged` | `now` | `true` |
| `CapabilityExpired` → `capability_expired` | `now` | `true` |

Mechanically: `api_error(error)` becomes `api_error_from(error, Decided::Owner)`
and the three ingress-minted sites call it with `Decided::Ingress`; the
`retry` column is a `match` on the variant in one place. `node_maintenance`,
`node_removal_fenced` and `learner_route_ineligible` are minted outside
this module by the request gate; they stay as they are — the client rule
(§3.16) treats any typed body without `owner_decided` as **not** owner
decided, which is the safe default for every code this table does not
list.

### 3.12 Session ends are logged and counted by reason

`retire_session` (2822–2848), after the tombstone is inserted:

```rust
tracing::info!(
    channel = %session.channel.guide_number,
    reason = error.code(),
    duration_s = session.started.elapsed().as_secs(),
    tuner_bytes = session.tuner_bytes.load(Ordering::Acquire),
    user = session.request.user_id,
    "Live TV session ended"
);
```

No capability, no tuner URL, no `DeviceAuth` in the line (the existing
`DeviceAuth`-absence tests at 8347–8394 gain one assertion over a captured
end line). `LiveTvMetrics.ended: AtomicU64` becomes
`ends: StdMutex<BTreeMap<&'static str, u64>>` keyed by `error.code()` (a
clean release — `CapabilityExpired` from `stop_local` — counts under
`released`, distinguished by the `cleanup` path that produced it), and the
Prometheus block at 3583–3585 emits one `plurx_live_tv_session_ends_total{reason="…"}`
line per key, plus `plurx_live_tv_stray_evictions_total` from §3.7.

### 3.13 Ingress budgets

http/live_tv.rs 24–26: `START_EXCHANGE_ATTEMPT` 17 s → **20 s**,
`START_EXCHANGE_TOTAL` 24 s → **36 s** (`STARTUP_FEEDING_TIMEOUT` 30 s + 6 s
of activation and relay slack; the 45 s client POST timeouts on all three
clients stay above it). In `owner_start` (713–721), when the loop exhausts
its attempts, call `owner_retire(state, config, request.user_id,
&request.request_id)` before returning the error — the only handle the
ingress has is the request id, and this closes the published-but-lost
window the diagnosis §3.6 describes. Ignore its result.

### 3.14 The live contract table carries the client timings

`tests/playback/player-input-contract.json`, `live.timings` (the object
`scripts/player-contract-table --embed` renders into `playback-policy.js`
56–60 as `LIVE_CONTRACT_TIMINGS`):

```json
"guide_poll_unavailable_s": 30,
"guide_poll_min_s": 15,
"guide_poll_after_next_refresh_s": 5,
"retire_liveness_probe_ms": 250,
"retire_orphan_after_keepalives": 3,
"start_replay_attempts": 1
```

with a `notes` entry per key in the same style as the existing three.
`channel_coalesce_ms` reaches Apple as `LiveTvInputRouting.channelCoalesceMilliseconds`
and Android as `LiveTvInputPolicy.CHANNEL_COALESCE_MS`; the new keys reach
them by the same route, and the existing fixture assertion that pins those
two to the JSON is extended to the six.

### 3.15 One fixture for start outcomes: `tests/playback/live-tv-start-cases.json`

Same shape as `live-tv-guide-cases.json` (`schema_version`, `title`,
`documented_in`, `ruled`, `notes`) plus:

```json
"answers": [
  {"body": {"code": "tuner_unavailable", "retry": "now", "owner_decided": true},
   "render": "tuner_unavailable", "offer_retry": true, "keep_hint": false},
  {"body": {"code": "owner_unavailable", "retry": "now", "owner_decided": false},
   "render": "owner_unavailable", "offer_retry": true, "keep_hint": true},
  {"body": {"code": "node_maintenance"},                       "render": "owner_unavailable", "offer_retry": true, "keep_hint": true},
  {"body": {"code": "startup_timeout", "retry": "later", "owner_decided": true},
   "render": "startup_timeout", "offer_retry": true, "keep_hint": false},
  {"transport": "timeout",                                     "render": "no_answer", "offer_retry": true, "keep_hint": true, "replay": true},
  {"transport": "http", "status": 404, "body": {"error": "not found"},
                                                               "render": "no_answer", "offer_retry": true, "keep_hint": true, "replay": true},
  {"transport": "http", "status": 400, "body": {"code": "invalid_request"},
                                                               "render": "invalid_request", "offer_retry": false, "keep_hint": false}
],
"resume": [
  {"body": {"outcome": "live", "session": {"session_id": "ltv1.x.y", "live": true, "...": "..."}}, "then": "reattach"},
  {"body": {"outcome": "ended"},   "then": "clear_hint_wait"},
  {"body": {"outcome": "retired"}, "then": "clear_hint_wait"},
  {"transport": "timeout",         "then": "keep_hint_wait"}
]
```

Rule the fixture encodes, and each client's reducer implements as a pure
function: **a hint is kept unless the body is typed with
`owner_decided: true`, or is a typed 4xx the ingress decided before
reaching an owner (`invalid_request`, `admin_required`, `invalid_settings`,
`live_tv_disabled`)**; `render` is the copy key; `replay` is whether the
lease replays the POST once with the same id. Consumers: `tests/web/live-tv.test.js`
(beside the guide cases at 702), `LiveTvTests.swift` (+ two `project.yml`
resource entries at 121/143), `LiveTvStartCasesTest.kt` (new, beside
`LiveTvGuideTest.kt`).

### 3.16 Clients — what goes, what comes

**Common shape, all three.** The persisted thing is a *hint*:
`{request_id: <32 hex>, touched_at: <unix ms>}` — never a capability,
never a token (the invariant at live-tv.js 125–127 stays true and its
comment moves onto the hint store). `Lease.start`:

```
 open Live TV (controller created)
   hint present → POST /live-tv/starts/{id}/resume
       live    → attach the returned session exactly as a fresh start would (playlist, heartbeat, watching = channel)
       ended | retired | typed 4xx from the ingress → forget hint; channel list
       anything else → keep hint; channel list
 press
   hints present and orphaned (see web) → DELETE /live-tv/starts/{id}   (fire and forget; forget on any typed 2xx)
   new id → persist hint → POST …/sessions {playback, request_id}
       typed 2xx → acquired; hint stays until DELETE /sessions confirms (then forget)
       no typed answer → replay POST once, same id
       still none → render no_answer, keep hint; the next press repeats
   typed error → render per fixture; keep_hint per fixture
 heartbeat (5 s, existing) → touch hint (touched_at = now)
 release (DELETE /sessions/{cap} 2xx/404/410) → forget hint
```

`start_outcome_unknown` is deleted from every string table and every
classification set; `RETRYABLE_CODES` (live-tv.js 8–17) and the three
definitive sets (index.html 14829, LiveTv.swift 746, LiveTvLease.kt 23–32)
are replaced by the fixture's `retry`/`keep_hint` reducer.

**Web** (`live-tv.js`, `index.html`; new asset rule: none — it all fits in
`live-tv.js` and `index.html`):
- `StartBarrier` (live-tv.js 128–193) → `StartHints` over the same
  `localStorage` prefix (`plurx_live_tv_hint_v1:<id>` → `touched_at`):
  `list()`, `remember(id)`, `touch(id)`, `forget(id)`. Boot `sync()` at
  index.html 14824 becomes the open-time resume in `openLiveTv` (the route
  handler around 14939), guarded so it runs once per document.
- **Liveness before retiring** (the multi-tab rule): a `BroadcastChannel("plurx-live-tv")`
  is opened at document boot; a document that owns a live session answers
  `{alive: id}` to `{who: id}`. Before `DELETE /starts/{id}` the pressing
  document posts `{who}` and waits `retire_liveness_probe_ms`; an answer,
  or `touched_at` newer than `retire_orphan_after_keepalives × 5 s`,
  leaves the hint alone and renders the existing "another tab is watching"
  refusal. Web Locks would be cleaner and is unavailable on the LAN HTTP
  origin this runs on (`randomUUID` is not either — 14819).
- `LIVE_TV_LEASE.start/release/keepalive` (14847–14887): body gains
  `request_id`; the `hold` on keepalive (14884) becomes `touch`; the
  `hold` on release (14875) goes; `confirm` after the unload DELETE stays
  and is now harmless if it never runs.
- `loadLiveTvGuide` (15577–15590) becomes the body of a controller-owned
  loop scheduled from `next_refresh_at` per §3.14
  (`max(next_refresh_at + 5 s, now + 15 s)`, `now + 30 s` when
  `unavailable`), cancelled on route change beside `LIVE_TV.timer`.
- `errorView` (live-tv.js 19–41): drop the `start_outcome_unknown` row; add
  `no_answer` ("The server did not answer. Press the channel again.") and
  read `retry` for the button.

**Apple** (`LiveTv.swift`, `LiveTvView.swift`):
- Delete `LiveTvStartBarrier` (629–676) and the `start_outcome_unknown`
  string (459) and the untyped-POST throw (533 → `LiveTvFailure(code:
  "no_answer")`). Keep `LiveTvFileBarrierStore`'s file mechanics as
  `LiveTvStartHintStore` writing `{request_id, touched_at}` as JSON to the
  same path (`live-tv-start.hint`; delete a leftover
  `live-tv-start.pending` on first run).
- `LiveTvLease` (722–800): `start` implements the press flow; new
  `resumeIfRecent() async -> LiveTvStarted?` implements the open-time
  step; `releaseCurrent` forgets the hint on success.
- `LiveTvView.swift`: the controller's load (where `startGuideRefresh` is
  started, ~199) calls `resumeIfRecent()` first and, on `live`, enters
  playback through the same path `watch()` uses after a successful start
  (the block at 120–140). `startGuideRefresh` polls on `next_refresh_at`.
  The background stop at 1304–1310 is unchanged.
- `make apple-build-bump` → `147`; `LiveTvTests.swift` gains the start-cases
  and resume-cases fixtures; `tests/client-fixes.toml` rows for every
  `fix(...)` commit (the history-check gate).

**Android** (`livetv/`):
- Delete `LiveTvStartBarrier` (LiveTvLease.kt 58–95) and
  `DEFINITIVE_START_REFUSALS` (23–32); `LiveTvFileBarrierStore` becomes
  `LiveTvStartHintStore` (JSON `{request_id, touched_at}` via the same
  `AtomicFile`). `LiveTvLease` (100–175): press flow, `resumeIfRecent()`,
  `heartbeatMarker()` → `touchHint()` (in-memory + one write per 60 s at
  most — the 2026-09-05 review's ANR finding stands: no fsync on every
  heartbeat).
- `LiveTvApi.kt`: 306 string deleted; the five `start_outcome_unknown`
  throws (384, 404, 410, 427, 450) become `no_answer`; `start(...)` body
  gains `request_id`; new `retire(id)`, `resume(id)`.
- `LiveTvPlayer.kt` 88: after `startGuideRefresh`, `resumeIfRecent()`;
  365–383 poll on `next_refresh_at`; `versionCode` 89 → **90** and
  `clients/android/README.md`'s build claim to match.

### 3.17 Developer card rows (advisory)

`guide_readiness` (http/live_tv.rs 209+) gains four `LiveTvReadinessCheck`
rows — `guide_age` (age + `fetched_at`), `guide_next_refresh`,
`guide_persisted` (path, bytes, generation, or "none"), `guide_last_error`
— rendered by `liveTvGuideReadyView` (index.html 16098) and the native
Developer views. Met/unmet colouring, never a gate.

---

## 4. Guardrails — do not do these

1. **No `DeviceAuth` at rest, in the persisted guide, in logs, in the
   session-end line, in any new error string.** The guide document never
   carried it; the file is that document verbatim; the tests at 8347–8394
   are extended to the file and the log line.
2. **No new field on a signed internal body.** New *paths* with new bodies
   only (§3.9). An older owner mid-rollout answers 404 to a new path and the
   ingress turns that into a typed, not-owner-decided answer (§3.9); it
   would answer 400 to a new field and take the whole start down.
3. **No change to `CAPABILITY_IDLE_TIMEOUT` (45 s), `PROVISIONAL_TIMEOUT`
   (40 s), `STARTUP_TIMEOUT`, `STARTUP_FEEDING_TIMEOUT`.** They are what make
   "recent" mean something and a stray cheap.
4. **No client-side refusal to start, ever again.** No monotonic wait, no
   quarantine, no "try again in N seconds" that the client computed. A hint
   that cannot be retired is kept and the start proceeds.
5. **No capability, token, channel id or tuner URL persisted on a client.**
   The hint is `{request_id, touched_at}` and nothing else. `resume` hands
   the capability back over the authenticated channel; the client holds it
   in memory as today.
6. **No auto-tune on open.** `resume` rejoins a session the viewer already
   owns; if the owner says `ended`/`retired`, the client shows the list and
   waits. `plurx_live_tv_last` still only pre-selects.
7. **No guide in the replicated store, no guide relay beyond the existing
   ingress memory.** One file on the owner.
8. **No lineup read from the guide path except the single cold read in
   §3.4.** A warm-but-expired snapshot never triggers a device read from
   the loop.
9. **No second tuner GET, no DVR, no reminders** — the guide plan's §5.1,
   §5.3, §5.4 stand.
10. **Do not delete the 60 s terminal tombstone or fold `retired` into it.**
    They have different caps for the reason in §3.7.

---

## 5. Milestones — task PRs into `effort/live-tv-reliability`

Server milestones and client milestones are on different CI lanes; keep
them in separate PRs. The Rust focused command for every server PR is
`cargo test -p plurxd --bin plurxd live_tv::` (nuc3: ~6 min cold, ~1 min
incremental; the cloud container works too, see `docs/ci/AGENT-COMPILE-LOOP.md`).

### 5.1 M0 — contracts and fixtures (docs + tests, no behaviour)

- `tests/playback/live-tv-start-cases.json` (§3.15), the six `live.timings`
  keys (§3.14) + `scripts/player-contract-table --embed`, this document's
  and the diagnosis's index rows already in `docs/README.md`.
- **Accept:** `python3 -m unittest tests.operations.test_docs_index` green;
  `node --test tests/web/live-tv.test.js` green with the new fixture
  *loaded but no cases asserted yet* (they assert from M4); `make web-check`
  green after the embed.

### 5.2 M1 — the guide survives and the loop wakes (server)

§3.1–§3.5, §3.17's server half. New unit tests in `live_tv.rs`'s test
module, beside `the_guide_cache_serves_fresh_then_stale_then_nothing…`
(7976): `a_persisted_guide_survives_a_restart_at_its_real_age`,
`a_persisted_guide_past_the_stale_window_is_not_adopted` (and garbage on
disk is ignored), `the_guide_document_says_when_the_owner_comes_back`,
`a_skipped_tick_on_the_owner_comes_back_in_a_minute_not_twenty`,
`an_ingress_forgets_an_unavailable_answer_quickly_and_a_real_one_by_the_owners_clock`,
`a_settings_generation_change_wakes_the_guide_loop_once`, and one over the
lineup wake (`get_or_refresh` from cold notifies; from warm does not).
- **Accept:** the focused suite green; `docs/API.md` guide row mentions
  `next_refresh_at`; on nynuc after deploy, `plurx_live_tv_guide_age_seconds`
  is a number within 10 s of `/readyz` and `skipped` does not increment
  past 1.

### 5.3 M2 — the clients poll on the owner's clock (web · Apple · Android)

§3.16's guide-loop items only (`loadLiveTvGuide` loop, `startGuideRefresh`
×2). No barrier changes yet.
- **Accept:** `make web-check`; `make apple-test` on `mba`;
  `./gradlew testDebugUnitTest lintDebug`; Apple `147`, Android `90` (or
  bump once more in M4 — one bump per client per lane is enough: do it in
  the last client PR). Manual: open Live TV < 60 s after a deploy on each
  client; the grid fills without touching Settings.

### 5.4 M3 — `request_id`, retire, resume, stray eviction, budgets, logging (server)

§3.6–§3.13. Tests: registry-level cases for replay-join, replay-conflict,
retired → `Conflict`, per-user retired cap (a 9th evicts the oldest; 256
retires from one user never refuse another user's start), retire on a
live/tombstoned/unknown id, resume on a live/provisional/ended/retired id
(provisional → not `live`), stray eviction (full slots + own idle session
→ admitted and cancelled; full slots + own *fresh* session → `Capacity`;
full slots + another user's idle session → `Capacity`), ingress 404 →
`owner_decided: false`, redaction of `starts/<id>`, maintenance eligibility
of the two public routes, and the `DeviceAuth`-absence assertion over an end
line.
- **Accept:** focused suite green; `test_api_doc_routes` green with the
  three rows and `190`; `plurx_live_tv_starts_total{outcome="recovered"}`
  increments on a replayed public POST in a two-node cluster-check
  scenario or the single-node integration test, whichever is cheaper to
  add beside the existing Live TV cases.

### 5.5 M4 — the barrier removal, from one fixture (web · Apple · Android)

§3.16 press/open/release flows, the fixture assertions from M0, the string
and set deletions. `fix(...)` commits need `tests/client-fixes.toml` rows
(history-check) — pin the call site (`Lease.start` / `LiveTvLease.start`),
not an extracted helper.
- **Accept:** `grep -r start_outcome_unknown crates clients tests docs
  --include='*.{js,html,swift,kt,json,md}'` returns only the diagnosis
  document; all three fixture suites green; on each platform an
  instrumented test proves a hint left by a killed process is retired in
  the background and the press is not delayed. Physical: kill the Apple TV
  app while watching, reopen within 45 s — the same stream is back with no
  press; reopen after two minutes — the channel list, first press plays.

### 5.6 M5 — Developer rows, status doc, release counters, deploy

§3.17 client halves; a `LIVE-TV-RELIABILITY-STATUS.md` beside this file with the
before/after metrics from nynuc; STATUS.md entry; `make apple-build-bump`,
Android `versionCode` + README claim (if not done in M4); lane promotion
(the one full run); Paul deploys.

---

## 6. What the reviewer should attack

The diagnosis document's own review found twenty things; these are the
ones that survive into this plan and the new surfaces it adds:

1. **The lineup-fill wake.** Is `was_cold` computed before the write, and
   is the `Notify` the same instance the loop awaits? A `notify_waiters`
   here loses a wake that lands mid-refresh; `notify_one` stores it.
2. **`checked_sub` on `tokio::time::Instant`.** Under `tokio::time::pause()`
   in tests the clock starts near zero; a persisted age of an hour has no
   instant before it. The fallback (served as fetched-at-boot) must be the
   *conservative* direction and the test must not assert an age it cannot
   produce under a paused clock.
3. **Replay across a fence bump.** `LiveTvRequestKey` includes
   `source_serving_generation`; a blip between POST and replay makes a
   second session. Paul accepted a stray; confirm §3.7's eviction catches
   it under full capacity and the idle reap catches it otherwise.
4. **Retire during an in-flight start on the owner.** `retire_local` must
   insert into `retired` *before* looking for the session, so a POST racing
   the retire either finds `Conflict` or a session that is about to be
   cancelled — never a fresh admission.
5. **`resume` on a session another document of the same user is actively
   watching.** Two devices with separate hint stores cannot collide (their
   ids differ). Two web tabs can: the liveness probe in §3.16 must run
   before `resume` too, not only before `DELETE`.
6. **Eviction picks the oldest `last_touch`, under the registry lock, with
   per-session `StdMutex` locks inside the iterator.** Lock order is
   registry → session state everywhere else in `live_tv.rs`; confirm no
   path holds a session state lock while taking the registry.
7. **Redaction marker `starts` matches `library-channels/starts`? any other
   route?** Check the segment rule with the preceding `live-tv` segment.
8. **`api_error_from(…, Decided::Ingress)` on every ingress-minted site,
   and nowhere the owner decided.** Grep every `ApiError::typed(` and
   `api_error(` in `http/live_tv.rs`.
9. **The Apple hint store on tvOS lives in Caches** (LiveTv.swift 682–690:
   tvOS may purge it). A purged hint is a lost resume, not a safety issue;
   confirm the code treats a missing file as "no hint".
10. **`start_protocols` gating.** A new client against an old owner must
    send neither `request_id` nor use the new routes; the snapshot's
    protocol list is the only signal. Confirm the ingress strips
    `request_id` when the owner does not list `3`, as it strips `playback`
    for `2` (http/live_tv.rs 364–366).
11. **The `retire_orphan_after_keepalives` rule on the web uses wall
    clock.** A clock step makes a live tab look orphaned and its session
    gets retired: the other tab sees `capability_expired` and its next
    press plays. Annoying, not unsafe — but say so in the code.
12. **Budgets.** `START_EXCHANGE_TOTAL` 36 s with two 20 s attempts:
    confirm the second attempt is still bounded by the total (the
    `min` at 675) and that the retire-on-exhaustion uses a fresh 5 s
    deadline, not the exhausted one.

---

## 7. Physical verification — the prompt for the GPT session at Paul's Mac

> plurx Live TV reliability, lane `effort/live-tv-reliability`, Apple build
> 147 / Android 90. Install both on the Apple TV, the iPhone and the Google
> TV from the lane head with `scripts/ship-physical`. Then, on each device:
> (1) with the fleet freshly deployed, open Live TV within a minute — the
> guide must fill in without touching Settings; note how long it took;
> (2) start a channel, press Home, force-quit the app, reopen within 45 s —
> the same channel must be playing with no press; (3) start a channel,
> force-quit, wait two minutes, reopen — the channel list, and the first
> press must play; (4) confirm the words "Wait 90 seconds" cannot be
> produced by any sequence you try. Report each as pass/fail with the
> device, the build number from Settings → About, and a screenshot of any
> failure.
