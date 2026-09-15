# Live TV reliability — the implementation plan

**Status:** reviewed by Astra 2026-09-13 (R1–R8 accepted, §8), ready to build · **Executes:** Paul's
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
   sessions of which one is the viewer's own, *activated*, with no
   keepalive for ≥ 15 s (or whose request id the viewer has retired), a
   new start from that viewer is admitted and the stray is cancelled; a
   starting or provisional session is never evicted by idleness;
   `tuner_capacity` is answered only when every slot is live and none is
   the viewer's stray.
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
(a machine up for less time than the guide's age has no such instant).
**The age is never lost to that fallback** (review R6): `CachedGuide`
gains `age_offset: Duration` — the clamped wall-clock age at adoption,
zero for a fresh fetch — and every age is `age_offset + now - observed`.
When `checked_sub` fails, `observed = Instant::now()` and `age_offset =
age`; the served `age_seconds`, the `fresh`/`stale`/dropped classification
and the Prometheus gauge all use the effective age, so a copy adopted at
3 600 s reports 3 600 s and expires `GUIDE_STALE_TTL − 3 600 s` later
whichever branch it took. Write rules: serialise → `guide.json.tmp` → `rename`, inside
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
branches — an `unavailable` answer still says when to ask again — and
computes `age = cached.age_offset + now.duration_since(cached.observed)`
everywhere the old code used the bare difference (the stale filter at
1566, `age_seconds` at 1576, the freshness split at 1578).
`store()` on `Ok` clones the new `CachedGuide` out, drops the lock, then
`persist_guide`. `Default` stays what it is (tests use it).

`LiveTvManager::new` builds `GuideCache::with_store(guide_store)` and, after
the `Arc` exists, calls `manager.adopt_persisted_guide()`:
`state.try_lock()` (uncontended at construction), and if `cached` is
`Some`, `metrics.observe_guide_at(cached.observed, total_programmes)` and
`publish_guide_titles(&cached.guide)`. `observe_guide_at(observed, age_offset,
programmes)` is a new sibling of `observe_guide` (live_tv.rs 1268–1274); the
projection stores the offset with the instant and `guide_prometheus`
(1300–1310) adds it — the age gauge reports the copy's real age, not a
fresh fetch, on either branch of the adoption.

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
absent, `uuid::Uuid::new_v4()` as today.

**Negotiation covers the whole path, not just the owner** (review R1). An
owner advertising `3` says nothing about the ingress the client is
talking to: an older ingress rejects the new field (`deny_unknown_fields`
on its `PublicLiveTvStart`) and has none of §3.10's routes. So the signal
the client reads is produced *by the ingress*, as an intersection:

```rust
/// What this binary's public surface accepts: body fields and recovery
/// routes. Protocol 3 = client request ids + /live-tv/starts/*.
const INGRESS_START_PROTOCOLS: &[u8] = &[1, 2, 3];

fn negotiated_protocols(owner: &[u8]) -> Vec<u8> {
    INGRESS_START_PROTOCOLS.iter().copied().filter(|p| owner.contains(p)).collect()
}
```

`LiveTvChannelsResponse` (http/live_tv.rs 52–60) gains
`#[serde(default, skip_serializing_if = "Vec::is_empty")] protocols: Vec<u8>`
= `negotiated_protocols(&owner_snapshot.start_protocols)`; the owner's
`start_protocols: vec![1, 2]` at live_tv.rs 1960 becomes `vec![1, 2, 3]`.
Client rule, all three: send `request_id` and use `/live-tv/starts/*` **iff
the last channels response listed `3`**; a response without the field (an
older ingress) means legacy behaviour — no `request_id`, no retire, no
resume, hints kept for later. The ingress additionally strips `request_id`
when the owner does not list `3` (the same shape as `playback` for `2` at
364–366), which covers a client that read the list before an owner
downgrade.

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
fn sessions_for_public_request(&self, user_id: i64, request_id: &str) -> Vec<Arc<LiveTvSession>>;
   // ALL matches: scans `requests` keys for key.user_id == user_id && key.request_id == request_id.
   // A serving-generation bump between a POST and its replay legitimately makes two
   // (review R5); every public operation is defined over the whole set.
fn tombstone_for_public_request(&self, user_id: i64, request_id: &str) -> Option<&LiveTvTerminalTombstone>;
   // scans `terminals` values by entry.request.{user_id, request_id}
fn retire(&mut self, user_id: i64, request_id: String, now: Instant);
   // insert with now + RETIRED_TTL; evict the user's oldest past MAX_RETIRED_PER_USER
fn is_retired(&self, user_id: i64, request_id: &str) -> bool;
```

The capacity branch at 2195–2206 becomes:

```rust
if registry.sessions.len() + registry.terminals.len() >= MAX_TERMINAL_TOMBSTONES { /* unchanged */ }
let live = registry.sessions.values().filter(|s| !s.cancel.is_cancelled()).count();
if live >= usize::from(config.max_sessions) {
    // Paul's ruling: a possibly-held tuner is never a reason to refuse a
    // viewer. This viewer's own stray — no keepalive for STRAY_EVICTION_IDLE,
    // or a request id they have since retired — is cancelled first.
    // Idle is only meaningful for a session that has a heartbeat to miss
    // (review R4): `last_touch` is set at admission and a starting or
    // provisional session cannot yet be touched by a player, and both are
    // already bounded by STARTUP_*_TIMEOUT / PROVISIONAL_TIMEOUT. They are
    // never evicted for idleness — only a retired request id evicts them.
    let stray = registry.sessions.values()
        .filter(|s| s.request.user_id == request.user_id && !s.cancel.is_cancelled())
        .filter(|s| {
            let state = s.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let idle_active = state.activated
                && state.phase == LiveTvSessionPhase::Active
                && now.duration_since(state.last_touch) >= STRAY_EVICTION_IDLE;
            idle_active || registry.is_retired(request.user_id, &s.request.request_id)
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
    pub(crate) outcome: LiveTvResumeOutcome,        // live | pending | ended | retired
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<LiveTvActivated>,    // present iff outcome == live
}

impl LiveTvManager {
    /// Stop whatever this viewer's request produced, and make sure nothing
    /// can be produced from it later.
    pub(crate) async fn retire_local(&self, user_id: i64, request_id: &str) -> LiveTvRetireOutcome {
        // Under ONE registry lock, in this order (review R5 — the fence goes in
        // before anything is looked at, so a POST racing this call finds
        // Conflict or a session that is about to be cancelled, never a fresh
        // admission):
        //   registry.retire(user_id, request_id, now);
        //   let live = registry.sessions_for_public_request(user_id, request_id);   // all of them
        //   let ended = registry.tombstone_for_public_request(user_id, request_id).is_some();
        // then, lock released:
        //   !live.is_empty() → cancel_and_wait(live) (ignore Err) → Stopped
        //   ended            → Ended
        //   otherwise        → Retired
    }

    /// The same document the original activation answered, for a session
    /// that is still live and still this viewer's.
    pub(crate) async fn resume_local(&self, user_id: i64, request_id: &str) -> LiveTvResumeAnswer {
        // Over ALL matches, collected under the registry lock (review R5):
        //   resumable = activated && phase == Active && !cancel.is_cancelled()
        //   Some resumable → pick the one with the greatest last_touch; touch it;
        //                    answer Live + document built exactly as at 2312–2319
        //                    (channel_with_source_format, output, delivery,
        //                    playlist_url, live: true); every OTHER live match —
        //                    resumable or not — is cancelled outside the lock:
        //                    one public identity, one stream.
        //   none resumable but a Starting/Provisional match → Pending
        //                    (an activation is in flight from an ingress; the
        //                    client keeps its hint, shows the list, and does not
        //                    touch it — the provisional timeout or that ingress
        //                    settles it)
        //   no live match, tombstone present → Ended
        //   nothing        → registry.retire(...) → Retired
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
| `POST` | `/api/v1/live-tv/starts/{request_id}/resume` | bearer | `200 {"outcome": "live", "session": <LiveTvActivated>}` or `200 {"outcome": "pending" \| "ended" \| "retired"}` |
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
`retry` column is a `match` on the variant in one place.

**The remote-owner path carries the same two fields without any change to
the internal wire** (review R2). Internal errors are serialised by
`signed_wire_error` (internal_live_tv.rs 322–339) as `{code, message}` and
parsed by `wire_api_error` (http/live_tv.rs 1020–1045) into a stable code.
Both fields are *functions of where the body came from and what the code
is*, so the ingress produces them at the parse site: a body that arrived
as a **signed owner response** — whether from a new or an older owner —
is `owner_decided: true`, and `retry` is the same `match` on the stable
code (a code `wire_api_error` folds into `owner_unavailable` at its `_`
arm is still owner-decided: the owner answered). `peer_error` (transport
failure, timeout, invalid signature/body) stays `Decided::Ingress`.
`signed_wire_error` is not touched. Closing evidence is a two-node run
(§5.4), not a reducer fixture. `node_maintenance`,
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

### 3.13 One public start deadline, threaded through every stage

`start_session` today runs three stages whose sum nobody bounds:
`owner_snapshot` (`SNAPSHOT_DEADLINE` 25 s), `owner_start`
(`START_EXCHANGE_TOTAL` 24 s over two `START_EXCHANGE_ATTEMPT` 17 s tries)
and `owner_activate` (two `CONTROL_EXCHANGE_DEADLINE` 5 s tries) — a worst
case past the clients' 45 s POST timeout (review R7). Replace the per-stage
sums with one budget:

```rust
const PUBLIC_START_DEADLINE: Duration = Duration::from_secs(40);   // clients time out at 45 s
```

`start_session` takes `let deadline = deadline_after(PUBLIC_START_DEADLINE)`
at entry and passes `remaining(deadline)` into each stage, which uses
`min(its own constant, remaining)`: the snapshot read, each start attempt
(`START_EXCHANGE_ATTEMPT` becomes 20 s; `START_EXCHANGE_TOTAL` goes — the
public deadline is the total), each activation attempt. The owner's
lifecycle constants are untouched. When the budget is exhausted at any
stage after a provisional was issued, or when `owner_start` gives up:

```rust
// Not awaited on the response path — it has its own 5 s deadline and is
// logged either way, so exhausting the public budget never silently
// abandons the retirement.
tokio::spawn(owner_retire_detached(state.clone(), config.clone(), user_id, request_id));
```

and the public answer is `owner_unavailable` with `owner_decided: false`,
`retry: "now"` — the client's next press retires and starts. The
published-but-lost window the diagnosis §3.6 describes closes because the
retire carries the request id, the only handle the ingress ever has.

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
  {"body": {"outcome": "pending"}, "then": "keep_hint_wait"},
  {"transport": "timeout",         "then": "keep_hint_wait"}
],
"protocols": [
  {"channels_response": {},                       "request_id": false, "recovery_routes": false},
  {"channels_response": {"protocols": [1, 2]},    "request_id": false, "recovery_routes": false},
  {"channels_response": {"protocols": [1, 2, 3]}, "request_id": true,  "recovery_routes": true}
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
       pending | anything else → keep hint; channel list
 press
   hints present and orphaned (web: not owned by a live document) → DELETE /live-tv/starts/{id}
                                          (fire and forget; forget on any typed 2xx; a hint that is
                                          NOT retired is simply left — the press still proceeds)
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
- **Liveness decides what to retire, never whether to start** (review
  R3): a `BroadcastChannel("plurx-live-tv")` is opened at document boot; a
  document that owns a live session answers `{alive: id}` to `{who: id}`.
  Before `DELETE /starts/{id}` the pressing document posts `{who}` and
  waits `retire_liveness_probe_ms`; an answer, or `touched_at` newer than
  `retire_orphan_after_keepalives × 5 s`, means that hint is **left
  alone** — and the new start proceeds to the owner regardless. A sibling
  tab keeps its stream (admission is the owner's, with four slots); a tab
  that crashed 10 s ago keeps its hint until a later press or the 45 s
  reap. The "another tab is watching" refusal is deleted with the barrier.
  Web Locks would be cleaner and is unavailable on the LAN HTTP origin this
  runs on (`randomUUID` is not either — 14819).
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
   quarantine, no "try again in N seconds" that the client computed, no
   "another tab is watching". A hint that cannot be retired — or that a
   live sibling document owns — is kept, and the start proceeds to the
   owner's admission.
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
`cargo test -p plurxd --bin plurxd live_tv::` (lab3: ~6 min cold, ~1 min
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
(7976): `a_persisted_guide_survives_a_restart_at_its_real_age` (asserted under
`tokio::time::pause()`, where `checked_sub` cannot represent an hour before
the clock started — the real age must still be served and the expiry must
land `GUIDE_STALE_TTL − age` later; never accept zero),
`a_persisted_guide_past_the_stale_window_is_not_adopted` (and garbage on
disk is ignored), `the_guide_document_says_when_the_owner_comes_back`,
`a_skipped_tick_on_the_owner_comes_back_in_a_minute_not_twenty`,
`an_ingress_forgets_an_unavailable_answer_quickly_and_a_real_one_by_the_owners_clock`,
`a_settings_generation_change_wakes_the_guide_loop_once`, and one over the
lineup wake (`get_or_refresh` from cold notifies; from warm does not).
- **Accept:** the focused suite green; `docs/API.md` guide row mentions
  `next_refresh_at`; on media1 after deploy, `plurx_live_tv_guide_age_seconds`
  is a number within 10 s of `/readyz` and `skipped` does not increment
  past 1.

### 5.3 M2 — the clients poll on the owner's clock (web · Apple · Android)

§3.16's guide-loop items only (`loadLiveTvGuide` loop, `startGuideRefresh`
×2). No barrier changes yet.
- **Accept:** `make web-check`; `make apple-test` on `maca`;
  `./gradlew testDebugUnitTest lintDebug`; Apple `147`, Android `90` (or
  bump once more in M4 — one bump per client per lane is enough: do it in
  the last client PR). Manual: open Live TV < 60 s after a deploy on each
  client; the grid fills without touching Settings.

### 5.4 M3 — `request_id`, retire, resume, stray eviction, budgets, logging (server)

§3.6–§3.13. **Registry unit cases** (live_tv.rs test module): replay-join,
replay-conflict, retired → `Conflict`, per-user retired cap (a 9th evicts
the oldest; 256 retires from one user never refuse another user's start),
`negotiated_protocols` over `[]`, `[1,2]`, `[1,2,3]`, retire over zero /
one / two live matches for one identity (two: both cancelled, and a POST
racing the retire — issued between the fence insert and the cancel —
answers `Conflict`), resume selection (active + provisional → the active
one, the provisional cancelled; provisional only → `pending`; two active →
the greater `last_touch`, the other cancelled; tombstone → `ended`; none →
`retired`), stray eviction at full capacity over five fixtures — own
*active idle* session (evicted, admitted), own *active fresh* session
(`Capacity`), own *healthy 16 s-old starting* session (`Capacity`, never
evicted), own *provisional awaiting activation* session (`Capacity`), and
another user's idle session (`Capacity`) — plus a retired-id eviction of a
provisional session, redaction of `starts/<id>`, maintenance eligibility
of the two public routes, and the `DeviceAuth`-absence assertion over an
end line.

**Two-node cases** in `crates/plurxd/tests/live_tv_two_node.rs` (the
existing ingress/owner harness with a fixture HDHomeRun; runs under
`--features cluster-integration-tests`), because a single node cannot show
any of these (review R2, R7): (a) an owner refusal (`tuner_unavailable`
from the fixture device) reaches the client through the ingress with
`owner_decided: true` and the right `retry`; (b) an owner that is
unreachable yields `owner_unavailable` with `owner_decided: false`;
(c) a replayed public POST through the same ingress joins the same session
(`starts_total{outcome="recovered"}` increments) and a retire through the
ingress stops it; (d) a resume through the ingress hands back the same
capability; (e) a fixture device that publishes late plus a delayed
activation stays inside `PUBLIC_START_DEADLINE`, and a device that never
publishes ends in a retired request with no session left on the owner
(assert elapsed public-request time and the owner's registry afterwards).
The old-ingress/new-owner direction is a *client* decision proven by the
`protocols` fixture cases (§3.15) on all three clients; the
new-ingress/old-owner direction is `negotiated_protocols` plus the
existing `playback`-stripping shape at 364–366.
- **Accept:** the focused suite and the two-node file green;
  `test_api_doc_routes` green with the three rows and `190`.

### 5.5 M4 — the barrier removal, from one fixture (web · Apple · Android)

§3.16 press/open/release flows, the fixture assertions from M0, the string
and set deletions. `fix(...)` commits need `tests/client-fixes.toml` rows
(history-check) — pin the call site (`Lease.start` / `LiveTvLease.start`),
not an extracted helper.
- **Accept:** `grep -r start_outcome_unknown crates clients tests docs
  --include='*.{js,html,swift,kt,json,md}'` returns only the diagnosis
  document; all three fixture suites green; on each platform an
  instrumented test proves a hint left by a killed process is retired in
  the background and the press is not delayed. Physical: §7 — the abrupt
  termination case (no clean release happened) must resume; the clean
  background case must show the list; the long-delay case must not
  resurrect an idle session.

### 5.6 M5 — Developer rows, status doc, release counters, deploy

§3.17 client halves; a `LIVE-TV-RELIABILITY-STATUS.md` beside this file with the
before/after metrics from media1; STATUS.md entry; `make apple-build-bump`,
Android `versionCode` + README claim (if not done in M4); lane promotion
(the one full run); Paul deploys.

---

## 6. What the reviewer should attack

The diagnosis document's own review found twenty things and Astra's
review of this plan eight more (§8); these are the ones that survive into
the contracts as written and the new surfaces they add:

1. **The lineup-fill wake.** Is `was_cold` computed before the write, and
   is the `Notify` the same instance the loop awaits? A `notify_waiters`
   here loses a wake that lands mid-refresh; `notify_one` stores it.
2. **`checked_sub` on `tokio::time::Instant`.** Under `tokio::time::pause()`
   in tests the clock starts near zero; a persisted age of an hour has no
   instant before it. `age_offset` (§3.2) is what makes the real age
   survive that; confirm every reader of `observed` adds it — `read()`,
   the stale filter, the freshness split, the Prometheus gauge — and that
   none of them can be reached with a bare difference.
3. **Replay across a fence bump.** `LiveTvRequestKey` includes
   `source_serving_generation`; a blip between POST and replay makes a
   second session for one public identity. §3.7/§3.8 now define every
   public operation over the whole match set; confirm nothing still calls
   a singular lookup, and that resume's "cancel the others" cannot cancel
   the one it just selected.
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
12. **Budgets.** One `PUBLIC_START_DEADLINE`: confirm every stage takes
    `min(own, remaining)` — the snapshot read included — that the detached
    retire uses a fresh 5 s deadline, not the exhausted one, and that a
    provisional issued in the last second of the budget is retired rather
    than left to the 40 s provisional timeout.
13. **Negotiation.** `protocols` is the *intersection*; confirm an ingress
    never reports `3` from its own constant alone, that the channels
    response is what every client reads before its first start (not the
    readiness document), and that a client caches it per lineup read, not
    per process.
14. **Eviction predicate.** `activated && phase == Active` — confirm
    `Provisional` with `activated == false` (the window between publish and
    activation) is excluded, and that a retired id evicts in every phase.

---

## 7. Physical verification — the prompt for the GPT session at Paul's Mac

Pressing Home first is a *clean* release on Apple (the `scenePhase`
handler at LiveTvView.swift 1304–1310 stops the session, and a confirmed
release forgets the hint), so it must not be the setup for the crash case
(review R8). The abrupt case has to kill the process while it is playing
in the foreground, without giving the app a chance to release.

> plurx Live TV reliability, merged to `main` (PR #281), Apple build 147 /
> Android 90. Install on the Apple TV, the iPhone and the Google TV from
> `origin/main` with `scripts/ship --apple --android`, or `scripts/ship-physical`
> if Ansible is unhealthy. The servers are already on this code. For every case record
> the device, the build from Settings → About, what you did, whether the
> server logged a `Live TV session ended` line before you reopened (media1:
> `sudo docker logs --since 5m plurxd | grep 'Live TV session ended'`), the
> seconds since the last keepalive, and the reopen result; screenshot any
> failure.
> (1) Guide after a deploy: with the fleet freshly deployed, open Live TV
> within a minute — the grid must fill in without touching Settings; note
> how long it took.
> (2) Abrupt termination, recent: start a channel and, while it is playing
> in the foreground, kill the process without backgrounding it — Apple:
> `xcrun devicectl device process terminate` (or Xcode Debug → Stop while
> attached); Android: `adb shell am force-stop tv.plurx.app` is NOT abrupt
> enough (it runs `onStop`) — use `adb shell kill -9 $(adb shell pidof
> tv.plurx.app)`. Reopen within 30 s. Expected: the same channel playing
> with no press, and NO `session ended` line before the reopen.
> (3) Abrupt termination, late: same kill, reopen after two minutes.
> Expected: the channel list (the owner reaped it at 45 s idle — the
> `session ended` line is there), and the first press plays.
> (4) Clean background: start a channel, press Home, wait ten seconds,
> reopen. Expected: the channel list (the release was confirmed — the
> `session ended` line is there), and the first press plays. This is not
> a failure.
> (5) Deploy mid-stream: start a channel on the Apple TV, ask Paul to
> restart media1's plurxd, reopen Live TV after it is back. Expected: the
> channel list, and the first press plays.
> (6) Confirm the words "Wait 90 seconds" cannot be produced by any
> sequence you try.

---

## 8. Review log — Astra, 2026-09-13 (R1–R8), and where each landed

All eight accepted; none disputed or superseded. Each line names the
revised section that carries the correction and the evidence that closes
it.

| # | Finding | Status | Where | Closing evidence |
|---|---|---|---|---|
| R1 | Owner protocol support did not prove ingress support | accepted | §3.6 `negotiated_protocols`, `protocols` on the channels response; §3.15 `protocols` fixture | client fixture cases (old ingress → legacy), `negotiated_protocols` unit test (new ingress / old owner), two-node (new / new) |
| R2 | Remote-owner errors bypassed the envelope | accepted | §3.11 — fields produced at `wire_api_error`, no internal wire change | two-node cases (a), (b) in §5.4 |
| R3 | Web liveness rule reinstated a refusal | accepted | §3.16 web bullet; §4.4 | web tests: dead-fresh hint, live sibling, old orphan — which retires are sent, and the start reaches the server in all three |
| R4 | Idle eviction could cancel a slow start | accepted | §3.7 predicate (`activated && Active`, or retired) | the five capacity fixtures in §5.4 |
| R5 | Retire/resume assumed one session per identity | accepted | §3.7 `sessions_for_public_request`; §3.8 fence-first retire, resume precedence, `pending` | registry cases in §5.4 (two matches; racing POST; active + provisional) |
| R6 | Age fallback made old data look new | accepted | §3.2/§3.3 `age_offset` | `…real_age` test under a paused clock, expiry at `TTL − age` |
| R7 | 36 s did not bound the public start | accepted | §3.13 `PUBLIC_START_DEADLINE`, remaining-budget threading, detached retire | two-node case (e) in §5.4 |
| R8 | Physical test performed a clean release first | accepted | §7 cases (2)–(4); §5.5 | the per-case record in §7 |

Standing constraints re-checked after the revision: no `DeviceAuth` policy
change; no field added to `LiveTvStartRequest`, `LiveTvStartRequestV2`,
`LiveTvActivateRequest` or `LiveTvStopRequest` (the `protocols` field is on
a public response, `request_id` rides an existing signed field, the new
internal bodies are new paths); no lifecycle constant changed
(`START_EXCHANGE_ATTEMPT`/`TOTAL` are ingress exchange budgets, not
lifecycle constants); nothing persisted on a client but
`{request_id, touched_at}`.
