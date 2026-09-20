# Live TV session fence, peer transport and start — one settings read per node, one HTTP client per node, no fixed wait on a warm channel, and no zombie in the registry

**Status:** ready for review · **Executes:** L2 / F-ltv-2, L3 / F-ltv-3,
L6 / F-ltv-6 and L9 / F-ltv-11, F-ltv-12, F-ltv-14 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Against:** `main` @ `88a3957a`

Read the review's §3.4 rows L2, L3, L6 and L9 first, then the assessment rows
L2, L3, L6, L9 and F-ltv-2/3/6/11/12/14 plus correction 9 and correction 13
in [ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)
— each disposition is a guardrail or an acceptance check here. The session
lifecycle this plan touches is described in
[LIVE-TV-RELIABILITY-IMPLEMENTATION.md §3.7–§3.13](LIVE-TV-RELIABILITY-IMPLEMENTATION.md)
and the ownership model in [HDHOMERUN-LIVE-TV-PLAN.md](HDHOMERUN-LIVE-TV-PLAN.md).
Four items, four milestones, one draft PR each into `main` under the fast
lane; M1 and M2 are independent, M3 depends on nothing here but is the
riskiest, M4 is three small changes in one PR. Every `file:line` is against
`88a3957a` — re-verify at build time. If a step seems to require changing a
signed internal wire shape (`LiveTvStartRequest`, `LiveTvResourceRequest`,
`LiveTvDrainRequest`), the capability string format (`ltv1.<node>.<uuid>`,
`capability_owner` at `live_tv.rs:6916`), `validate_start_config`'s four
checks, the serving fence, or the `O_EXCL`/`env_clear` rules, stop and flag
it.

**Corrections to the review:**

- L2: the read is `settings_snapshot` → `query_consistent_map` under
  `time_authority_read_with_retry` (`hiqlite.rs:904-970`): two `STORE_TIMEOUT`
  (3 s) attempts 100 ms apart, or a 5 s `AUTHORITY_QUORUM_RECOVERY_BUDGET`
  on quorum loss. So one failed fence costs ≈6.1 s (timeouts) or 5 s
  (quorum) before the session ends — the "~6 s" is that budget, not a
  measured failover. The terminal code is `device_unavailable` ("reading
  live-TV settings: …"), which the clients render as a tuner problem.
- L3: `activity_peers` (`membership.rs:5972-6010`) is a **local** hiqlite
  `query_map` plus an in-process `metrics_db()`, not a leader round trip.
  Still one blocking-pool SQL read and one metrics call per relayed
  request. And there is no exposed "membership epoch" to key a cache on;
  reachability is `now - last_seen_at <= NODE_REACHABLE_WINDOW_MS`
  (30 s, `membership.rs:57,2888`), refreshed by 10 s heartbeats. The cache
  below is keyed by owner node id with a TTL and explicit invalidation, as
  the assessment requires.
- L6: agreed with the assessment — 3 s is a ceiling after the first chunk
  (`collect_live_prefix`, `live_tv.rs:5632-5660`); and the prefix bytes are
  not wasted, they are the first bytes pumped into FFmpeg (`:6315`). The
  cost is the serialisation *prefix → ffprobe → plan → spawn*, not the
  bytes.
- L9: the appendix's F-ltv-11 remedy ("always retire; hand the directory to
  `sweep_orphan_scratch`") would let the hourly sweeper (`live_tv.rs:3903`)
  `remove_dir_all` a directory a still-alive FFmpeg is writing into, and
  would drop `LiveTvProcess` (child + job + encoder admission) with the
  session. Planned against the assessment's version instead.

## 1. Objective

1. **Fence (L2).** On the tuner owner, live sessions share one bounded,
   validated observation of the replicated settings per node: one consistent
   read per second while any session or transport is live, zero when none
   is. Each session still validates owner, generation, enabled and drain
   admission against that observation every second, and a change made on
   any node reaches every session within one interval plus read latency. A
   read failure is a **defined grace** of `FENCE_GRACE_MAX_AGE` (10 s) on
   the last validated observation, after which the session ends with a
   message that names the settings store, not the tuner.
2. **Transport (L3).** Every relayed Live TV request on an ingress uses one
   process-lifetime `PeerTransport`, and the owner's `(node_id, http_base)`
   is resolved at most once per `OWNER_PEER_TTL` (5 s) or per invalidation,
   with per-request signing and response verification unchanged.
3. **Start (L6).** A start on a channel whose full `LiveSourceFacts` are
   cached and fresh spawns FFmpeg on the first tuner chunk; the prefix
   probe runs concurrently as **verification**, and publication waits for
   it. A cold channel behaves as today. No wrong delivery is ever published
   on a warm start.
4. **Retire and observe (L9).** A session whose cleanup fails leaves the
   registry's capacity and tombstone accounting immediately while a
   bounded retry owner keeps the child, its admission and the directory
   until both are confirmed gone. The scratch inventory is one blocking
   scan per tick with every current check kept. An unknown capability
   after an owner restart says "unavailable, press Watch", and says
   "restarted" only when a persisted incarnation proves it.

## 2. Contract today

Re-verify at build time.

### 2.1 The per-second fence (L2)

`run_live_session_inner` observe loop (`live_tv.rs:5469-5475`), with
`SESSION_TICK = 250 ms` (`:134`):

```rust
ticks = ticks.wrapping_add(1);
if ticks.is_multiple_of(4) {
    let owner = manager.upgrade().ok_or_else(...)?;
    ensure_session_fence(&owner, session).await?;
}
```

`ensure_session_fence` (`:5554-5565`):

```rust
if !manager.serving.is_current(session.owner_serving_generation) {
    return Err(LiveTvError::OwnerUnavailable(SERVING_FENCED_MESSAGE.to_owned()));
}
let config = manager.config().await?;
validate_start_config(&config, &session.request, &manager.node_id)
```

`LiveTvManager::config` (`:2740-2747`) is `store.settings_snapshot()`
mapped to `LiveTvError::DeviceUnavailable(format!("reading live-TV
settings: {error}"))`, then `LiveTvConfig::from_snapshot` and
`observe_config` (guide-cache generation observation, source-format
pruning, metrics projection). `validate_start_config` (`:4996-5026`)
checks `validate_static`, `enabled`, `owner_node_id == local && ==
request.expected_owner_node_id`, `generation == request.config_generation`,
`admission_ready()` (`transition_from_owner_node_id.is_empty()`, `:372`).
The same fence runs once before publication (`:5401`) and once at start
(`:5170`). The `authority_lost` arm (`:5478-5485`) polls
`serving.is_current` every 25 ms independently. A settings save on any node
also drives an explicit signed drain to the owner (`drain_owner`,
`http/live_tv.rs:1274-1300`; `drain_before`, `live_tv.rs:3492-3529`), so the
per-second read is the fence for the case where that drain did not arrive.

### 2.2 The ingress relay (L3)

`owner_resource` (`http/live_tv.rs:1060-1150`): `owner_peer(state, &owner)`
per request (`:1076`), then `PeerTransport::new(state.membership.clone())`
at `:1093` (playlist/status/keepalive, `request`) or `:1121` (segment/init/
fragment, `request_stream`). `PeerTransport::new` (`peer_transport.rs:50-58`)
builds a `reqwest::Client` with `redirect(Policy::none())`. `owner_peer`
(`:1356-1386`) calls `state.membership.activity_peers().await` and picks
`peer.node_id == expected && peer.reachable` with `http_base`. Nine
`PeerTransport::new` sites in `http/live_tv.rs` (`:175, 932, 1004, 1093,
1121, 1169, 1241, 1298, 1526`); `MediaSessionCoordinator` already holds one
for its lifetime (`media_sessions.rs:1601`).

### 2.3 The start sequence (L6)

`run_live_session_inner` (`:5154-5327`), strictly serial:
`ensure_session_fence` → scratch dir → `open_tuner_stream` (headers under
`STARTUP_TIMEOUT` 15 s) → `collect_live_prefix` (`SOURCE_PREFIX_BYTES` 8 MiB
or `SOURCE_PREFIX_TIME` 3 s after the first chunk, `:136-137`) →
`probe_live_source` (ffprobe on the prefix file, `SOURCE_PROBE_TIME` 2 s,
`:5781-`) → `resolve_live_delivery` (`:5241`) → `admit_live_tv` for encode
routes (`ADMISSION_WAIT` 5 s) → `LiveTvTranscodePlan::new` →
`spawn_live_ffmpeg` (`:5296`) → `pump_tuner_stream` replays prefix + queued
+ remainder into stdin (`:6306-6360`). The UI cache `source_formats:
StdMutex<HashMap<SourceFormatKey, CachedSourceFormat>>` (`:2655`) keyed by
`{generation, device_id, channel_id}` (`:2619-2623`) holds
`LiveTvSourceFormat` (width, height, scan, audio channels/layout,
observed_at — `:646-659`) with expiry `min(observed_at + 20 min, current
programme end)` (`source_format_expiry`, `:2871-2886`). It is never used to
skip the probe. Mux change after start: `capture_live_stderr` sets
`source_format_changed` on "new video stream" / "new audio stream" /
"parameter change" after the input descriptor (`:6273-6279`), and the
observe loop ends the session with `SourceFormatChanged` (`:5356-5360`).

### 2.4 Cleanup, inventory, lookup (L9)

`run_live_session` (`:5071-5080`):

```rust
if let Some(manager) = manager.upgrade() {
    if let Err(error) = cleanup {
        tracing::error!(kind = %error, "Live TV session cleanup could not remove scratch before its deadline");
        return;
    }
    manager.retire_session(&session);
}
```

`cleanup_session` (`:5124-5152`) fails either when `child.wait()` is not
confirmed within `SESSION_DRAIN_TIMEOUT` (5 s) — in which case
`session.process` **keeps** the child, job and admission — or when
`remove_dir_all` keeps failing for 5 s. The un-retired session stays in
`registry.sessions`, excluded from `live_sessions()` (cancelled) but counted
in `sessions.len() + terminals.len() >= MAX_TERMINAL_TOMBSTONES` (256,
`:3161`), which refuses every start. `sweep_orphan_scratch` (`:3903-3942`)
removes any directory not owned by a registered session or a
`scratch_claims` entry, hourly.

`inspect_scratch` (`:6362-6477`) runs every tick: `tokio::fs::read_dir`,
per entry `tokio::fs::symlink_metadata`, and enforces: non-regular entry →
error; total `> MAX_SESSION_BYTES` (128 MiB) → error; playlist
`> MAX_PLAYLIST_BYTES` → error; unexpected filename → error; `> 1`
temporary file → error; listed `> MAX_LISTED_SEGMENTS` (24) or unlisted
`> MAX_DELETION_LAG_SEGMENTS` (5) → error; then reads and parses the
playlist.

`LiveTvManager::session` (`:4769-4787`): registry hit, else tombstone
(`TERMINAL_TOMBSTONE_TTL` 60 s), else
`CapabilityExpired("live-TV capability expired")` → HTTP 410, `retry: now`.
The capability is `ltv1.<base64url(node_id)>.<uuid v4>` (`:3085-3089`),
three dot-separated parts exactly (`capability_owner`, `:6916-6949`).

## 3. Change

### 3.1 L2 — one observation per node, sessions validate against it

```
                    ┌────────────────────────────────────────────┐
  settings (raft) ─▶│ FenceObserver task (owner, 1 Hz while live) │─▶ watch<FenceObservation>
                    └────────────────────────────────────────────┘        │
                                                                          ▼
   session A tick ──▶ ensure_session_fence ── age ≤ 10 s? ─▶ validate_start_config(obs.config)
   session B tick ──▶ ensure_session_fence ── age >  10 s? ─▶ Err(OwnerUnavailable "settings store unreachable for N s")
```

New on `LiveTvManager`:

```rust
fence: FenceObserver,

pub(crate) struct FenceObservation {
    pub(crate) config: LiveTvConfig,
    /// When this consistent read completed, on the tokio clock.
    pub(crate) observed_at: tokio::time::Instant,
    pub(crate) sequence: u64,
}

struct FenceObserver {
    latest: tokio::sync::watch::Sender<Option<FenceObservation>>,
    /// Set by the observer loop after a failed read; cleared on success.
    last_failure: StdMutex<Option<(tokio::time::Instant, String)>>,
    /// Woken by attach/start so a sleeping observer reads at once.
    wake: Arc<tokio::sync::Notify>,
}

const FENCE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(1);
/// How long sessions may continue on the last validated observation when
/// the consistent read keeps failing. Longer than one authority-read retry
/// budget (≈6 s) so a leader failover does not end healthy streams;
/// shorter than any window in which a replacement owner could be admitted
/// without this owner's own drain proof (admission_ready needs that proof
/// or an administrator's physical-stop attestation); and the serving fence
/// ends the session independently on quorum loss.
const FENCE_GRACE_MAX_AGE: Duration = Duration::from_secs(10);
```

The observer loop runs on every node's manager (it is cheap when idle) and
reads only while `registry.held() > 0`; otherwise it parks on `wake`. Each
iteration: `store.settings_snapshot()` (the same consistent read — the
replicated store stays the source of truth, and remote changes propagate
because *this* read sees them); on `Ok`, `LiveTvConfig::from_snapshot`,
`observe_config` (unchanged side effects, now once per node instead of once
per session), `latest.send_replace(Some(obs))`; on `Err`, record
`last_failure`, keep the previous observation, and count
`plurx_live_tv_fence_reads_total{outcome="failed"}`.

`ensure_session_fence` becomes:

```rust
if !manager.serving.is_current(session.owner_serving_generation) { /* unchanged */ }
let observation = manager.fence.latest.borrow().clone();
match observation {
    Some(obs) if obs.observed_at.elapsed() <= FENCE_GRACE_MAX_AGE => {
        validate_start_config(&obs.config, &session.request, &manager.node_id)
    }
    _ => Err(LiveTvError::OwnerUnavailable(format!(
        "the replicated settings could not be read for {}s; live TV stopped so a \
         disabled or moved tuner cannot keep streaming",
        age_secs
    ))),
}
```

Freshness, defined: an observation is *fresh* when `elapsed <=
FENCE_OBSERVATION_INTERVAL + STORE_TIMEOUT` (4 s — one interval plus one
read), *in grace* up to `FENCE_GRACE_MAX_AGE`, *expired* after. The metric
`plurx_live_tv_fence_observations_total{state}` with
`state ∈ {fresh, grace, expired}` counts what sessions actually validated
against. A `validate_start_config` failure on a fresh or in-grace
observation ends the session exactly as today (same error variants, so
`disabled`, `owner_unavailable`, `settings_conflict` codes are unchanged).

The two out-of-loop fences keep a **direct** read: at start (`:5170`) the
start path has just read config for the public request and the observer may
be parked; before publication (`:5401`) a stale-but-in-grace observation
must not admit a first segment, so it reads directly and fails closed on
error as today. Only the per-second loop fence uses the observation.

Why this is not the withdrawn `watch`: nothing here is fed by local writes;
the watch is fed by the same leader-consistent read the sessions did
themselves, so a save on `lab2` is visible on `media1` within one second
plus read latency, as before. Why "unknown, continue" is not this: the
grace is a bound with a reason, after which sessions end; and the
pre-publication fence never uses it.

### 3.2 L3 — one `PeerTransport`, a bounded owner-peer cache

New in `AppState` (`state.rs:615-`), built once in `AppState::new`:

```rust
/// One HTTP client for every Live TV ingress→owner exchange, plus a short
/// memory of where the owner is. Signing, expected-node binding and
/// response verification stay per request inside `PeerTransport`.
pub(crate) live_tv_peers: Arc<crate::http::live_tv::LiveTvPeers>,

pub(crate) struct LiveTvPeers {
    transport: PeerTransport,
    owner: StdMutex<Option<CachedOwnerPeer>>,
}
struct CachedOwnerPeer { node_id: String, http_base: String, resolved_at: Instant }
/// Same order as PEER_STATUS_CACHE_TTL. One sixth of NODE_REACHABLE_WINDOW_MS,
/// so a peer that stops heartbeating is dropped from here well before the
/// roster itself calls it unreachable.
const OWNER_PEER_TTL: Duration = Duration::from_secs(5);
```

`owner_peer` becomes `LiveTvPeers::owner_peer(&self, membership, expected)`:
cache hit when `node_id == expected && resolved_at.elapsed() < OWNER_PEER_TTL`;
otherwise the existing `activity_peers` resolution, stored on success. The
nine `PeerTransport::new(...)` sites in `http/live_tv.rs` become
`state.live_tv_peers.transport.request(...)` / `request_stream(...)`, with
`expected_node_id`, `auth_mode` and deadlines unchanged.

Invalidation, defined (the assessment's requirement beyond TTL):

| Event | Action | Reason |
|---|---|---|
| `PeerTransportError::Unreachable` or `TimedOut` from the owner | drop the cache entry; the failing request still fails as today | the address or reachability changed; the next request re-resolves |
| `PeerTransportError::InvalidResponse` (signature/body verification) | drop the entry | a key or capability change on the peer, not a topology change |
| `expected` differs from the cached `node_id` | miss | owner moved (a capability names its owner; the cache is per owner) |
| HTTP 404 on an internal path (`peer_error` → "does not serve …") | drop the entry | the peer is an older build; do not keep routing to it as if it served the route |
| `OWNER_PEER_TTL` elapsed | miss | bounds staleness of `reachable`, which the roster derives from 10 s heartbeats |

Nothing is cached about credentials: `sign_*` and verification run per
request from `membership`, exactly as now (`peer_transport.rs:132-`). The
request-time owner check `capability_owner(request.capability())` at
`http/live_tv.rs:1068` is untouched — the cache maps a node id to a base,
it never chooses the owner.

Metric: `plurx_live_tv_owner_peer_total{outcome}` with `outcome ∈ {hit,
resolved, invalidated}`.

### 3.3 L6 — warm start: spawn on the first chunk, verify concurrently, publish only on agreement

The cache extends to the planner's inputs. `CachedSourceFormat` gains
`facts: Option<LiveSourceFacts>` (the complete probe result,
`live_tv_delivery.rs:173-`), recorded from the probe at `:5204` under the
same `SourceFormatKey` and the same expiry (`source_format_expiry`: 20 min
ceiling, clipped to the current programme's end — the programme boundary
is where a mux most often changes format, which is why that clip already
exists). `record_source_format` records both; `channel_with_source_format`
and the UI keep reading the `LiveTvSourceFormat` subset.

Start path, warm case (`facts` present, unexpired, same `generation`,
`device_id`, `channel_id`, and the request's `playback` capability set
resolves to a plan from the cached facts without error):

```
 open_tuner_stream ─▶ first chunk ──┬─▶ spawn_live_ffmpeg(plan_from_cache) ─▶ pump (first chunk onward)
                                    │
                                    └─▶ collect_live_prefix (ceiling unchanged) ─▶ probe ─▶ compare
                                                                                          │
                                   publication gate: inventory publishable AND compare == Agree
                                   compare == Disagree ─▶ kill child, restart cold, same start budget
```

- `resolve_live_delivery(cached_facts, request.playback, policy, support)`
  produces the plan; `admit_live_tv` is taken as today for encode routes.
- FFmpeg is spawned before the prefix ceiling; `pump_tuner_stream` is fed
  a `LiveTunerInput` whose `prefix` is the **first chunk only** and whose
  `remainder` is a `tee`: every chunk goes to stdin and, until the prefix
  ceiling, is also appended to the verification buffer. No byte is delayed
  and no byte is delivered twice.
- Verification: `probe_live_source` on the buffered prefix, then
  `plan_inputs_agree(&cached_facts, &probed)` over exactly the fields the
  planner reads (codec, profile/level, width, height, pixel format, bit
  depth, field order, frame rate, colour/HDR, audio codec, channels,
  layout, sample rate). `Agree` → also refresh the cache entry.
  `Disagree` → `capture_interrupted`-style log line, kill and reap the
  child under `SESSION_DRAIN_TIMEOUT`, drop the cache entry, and continue
  as a cold start with the prefix already collected, inside the same
  `STARTUP_TIMEOUT`/`STARTUP_FEEDING_TIMEOUT` budget (`startup_overdue`
  is unchanged; a warm start that had to fall back has lost at most the
  prefix time it would have spent anyway).
- Publication gate: `record_producer_inventory` may say publishable before
  the probe finishes (2 s budget vs. `STARTUP_LISTED_SEGMENTS` at 1 s
  cadence); publication waits for `Agree`. This is what makes "a later
  stderr demotion cannot undo an already-published wrong delivery" a
  non-issue: nothing is published until the plan is re-proven from the
  bytes of *this* tune. The stderr detector stays as the post-publication
  mux-change guard, unchanged.
- Cold case: unchanged sequence, and the plan's facts are recorded.

Expected saving on a warm channel: the prefix ceiling (≤ 3 s) and the probe
(≤ 2 s) no longer precede the spawn; FFmpeg's own `-probesize 524288` /
`-analyzeduration 1000000` (`:6106-6108`) still applies. The docs' 5.5–7 s
first segment is expected to fall by the prefix time; the fleet number is
§6.3's to measure, not this plan's to claim.

Metric: `plurx_live_tv_starts_total{outcome}` gains no label;
`plurx_live_tv_start_plan_total{source}` with `source ∈ {cold, warm_agreed,
warm_disagreed}` is added. Settings: none. Recipe identity: Live TV has no
cache digest; `LiveDeliveryPlan` is per session and is still resolved by
the pure planner.

### 3.4 L9 — retire from capacity, keep a retry owner; one blocking inventory; honest lookup text

**Cleanup handoff.** `run_live_session` always calls `retire_session`.
When `cleanup` failed it first moves what is still owned into a
manager-held orphan record:

```rust
struct OrphanSession {
    capability: String,
    directory: PathBuf,
    /// Present when the child's exit was not confirmed. Holds the job and
    /// the encoder admission until `try_wait` says it is gone.
    process: Option<LiveTvProcess>,
    claim: ScratchClaim,            // keeps sweep_orphan_scratch away
    since: tokio::time::Instant,
    attempts: u32,
}
orphans: StdMutex<Vec<OrphanSession>>,   // on LiveTvManager
const ORPHAN_RETRY: Duration = Duration::from_secs(5);
const ORPHAN_MAX: usize = 64;
```

A bounded loop (the existing sweeper task, `:3944-3956`, gains a 5 s arm)
retries each orphan: `start_kill` + `try_wait` → when exited, drop
`process` (releasing the admission); then `remove_session_directory`; when
both are done, drop the claim and the record. Orphans count in neither
`held()` nor `sessions.len() + terminals.len()`; they are counted in
`plurx_live_tv_orphans{kind}` with `kind ∈ {process, scratch}`. At
`ORPHAN_MAX` the oldest scratch-only orphan is logged at error and dropped
(the hourly sweeper still sees the directory); a process orphan is never
dropped while its child is unconfirmed — an encoder slot held by a live
process is a fact, not a leak. Shutdown drains orphans under
`SESSION_DRAIN_TIMEOUT` like sessions.

**Inventory.** `inspect_scratch` moves its directory walk into one
`tokio::task::spawn_blocking` using `std::fs::read_dir` +
`symlink_metadata`, returning the same `ScratchInventory` or the same
errors; the playlist read stays bounded (`read_bounded_regular_file`). Every
check in §2.4 is kept verbatim, including the 128 MiB total, the
regular-file rule, the `> 1` temporary-file rule, the deletion-lag bound
and the unexpected-name rule, because a temporary segment grows before the
playlist changes (correction 13). No playlist-mtime gate. Bound: at most
one inventory in flight per session — if the previous blocking scan has
not returned by the next tick (a stalled mount), the tick skips the scan
and the existing `PRODUCER_PROGRESS_TIMEOUT` / `startup_overdue` timers
decide, so a hung `stat` cannot pile up blocking tasks.

**Lookup text.** `LiveTvManager` persists `<runtime cache>/live-tv/
incarnation.json` `{version, incarnation, started_at, last_seen_at}`,
written at construction, touched every 30 s while `held() > 0`, and
`last_seen_at` set at clean shutdown. On construction the previous file (if
any) is read: `prior_seen_at`. `session()` on a registry and tombstone miss
answers:

- `CapabilityExpired("the tuner owner restarted; press Watch to start
  again")` when a prior incarnation exists and `now - prior_seen_at <=
  PROVISIONAL_TIMEOUT + CAPABILITY_IDLE_TIMEOUT + 30 s` (a session could
  have been live at the restart);
- otherwise `CapabilityExpired("this live-TV session is not available on
  the tuner owner; press Watch to start again")` — no claim about expiry.

Code stays `capability_expired`, status 410, `retry: now`, so no client
branch changes; only the sentence does. A stale capability is never
accepted: this changes text, not authority.

## 4. Guardrails (non-goals)

- **The replicated store stays the authority (L2).** No local `query_map`
  for the fence, no watch fed by local writes, no "unknown, continue"
  beyond `FENCE_GRACE_MAX_AGE`; the pre-publication and start fences read
  directly and fail closed. The serving fence is untouched.
- **`validate_start_config` is not weakened.** The four checks run every
  second against the observation; a fresh observation that fails them ends
  the session immediately, as today.
- **No cached credential or address without expiry (L3).** TTL 5 s plus
  the invalidation table; signing and verification per request; the
  capability still names the owner per request.
- **Cold starts are byte-for-byte today's sequence (L6).** The warm path
  is additive; a `Disagree` falls back to it within the same budget.
  Nothing is published before the probe agrees. No SPS/PMT heuristic to
  shorten the prefix — the assessment lists the cases it would have to
  handle (delayed PMT, multiple programmes, discontinuities, encrypted
  tracks) and this plan does not take that on.
- **No wire-shape or capability-format change (L6, L9).** The capability
  stays `ltv1.<node>.<uuid>`; incarnation is server-side state only.
- **Retiring never abandons a child (L9).** `LiveTvProcess` moves to the
  orphan record with its admission; the scratch claim blocks the sweeper
  until the child is confirmed gone.
- **Inventory checks are not reduced (L9 / correction 13).** One blocking
  scan, same predicates, no mtime gate.
- **No `EXT-X-PROGRAM-DATE-TIME` / `EXT-X-START` here.** F-ltv-13's remedy
  was withdrawn (`-hls_start_time_offset` does not exist); PDT is a
  capability item with its own origin question, not part of L9.
- **Not in this plan:** L4 transport sharing (its own design doc), L1/L10
  (DVR plan), the 18 `PeerTransport::new` sites outside `http/live_tv.rs`.

## 5. Milestones

### 5.1 M1 — `FenceObserver` and the graced session fence (L2)

Files: `live_tv.rs` (`FenceObserver`, `LiveTvManager::new`/loops,
`ensure_session_fence`, metrics block).

Tests (`live_tv.rs` `mod tests`, using the existing in-memory store and
`tokio::time::pause`):

- `two_sessions_cost_one_settings_read_per_second`: count
  `settings_snapshot` calls on a counting store wrapper; two live sessions
  for 10 paused seconds → 10 ± 1 reads, not 20.
- `a_remote_disable_reaches_every_session_within_one_interval`: write
  `live_tv.enabled = 0` through a second `Arc<dyn Store>` handle (not via
  the manager); advance 1 s; both sessions end with `live_tv_disabled`.
- `a_failing_read_is_graced_then_fatal_with_the_store_named`: inject read
  failure at t=0; at t=9 s sessions are alive and
  `fence_observations_total{state="grace"}` grew; at t=11 s both end with
  `owner_unavailable` whose message contains "replicated settings" and
  not "HDHomeRun".
- `publication_never_uses_a_graced_observation`: read failing, a session
  reaches publishable inventory; assert it does **not** publish and ends
  with the direct read's error.
- `a_fresh_observation_that_fails_validation_ends_the_session_at_once`:
  bump `live_tv.config_generation`; the session ends with
  `settings_conflict` within one interval.
- Existing `disabling_live_tv_drains_the_session_and_refuses_the_next_one`
  (`tests/live_tv_two_node.rs:989`) still passes unchanged.

Acceptance: `cargo test -p plurxd fence` green; `make unit` green;
`make live-tv-two-node-check` green on a host that can bind 80 and 5004
(feature-gated, not in the fast lane — name the host in the PR body).

### 5.2 M2 — `LiveTvPeers` in `AppState` (L3)

Files: `state.rs`, `http/live_tv.rs` (nine sites, `owner_peer`), metrics.

Tests (`http/live_tv.rs` `mod tests` and `tests/live_tv_two_node.rs`):

- `relayed_requests_share_one_client_and_resolve_the_owner_once_per_ttl`:
  count `activity_peers` calls across 20 relayed playlist requests in
  1 paused second → 1 resolution, 19 hits.
- `an_unreachable_owner_invalidates_the_cached_peer`: first request fails
  `Unreachable`; the next request resolves again (2 resolutions total).
- `a_bad_response_signature_invalidates_the_cached_peer`: the fake owner
  answers with a wrong signature → `InvalidResponse`, entry dropped.
- `an_owner_move_is_a_miss`: capability for owner B after cached A →
  resolution for B.
- Existing `an_ingress_relays_a_capability_it_does_not_own_and_never_takes_the_tuner_over`
  (`live_tv_two_node.rs:809`) and
  `moving_the_owner_hands_the_tuner_to_the_new_node_exactly_once` (`:1050`)
  pass unchanged.

Acceptance: `cargo test -p plurxd owner_peer` green; `make unit` green;
`make live-tv-two-node-check` green (same host note); PR body reports
resolutions per relayed request before/after from the test counter.

### 5.3 M3 — warm start with concurrent verification (L6)

Files: `live_tv.rs` (`CachedSourceFormat`, `record_source_format`,
`run_live_session_inner`, `LiveTunerInput` tee, `plan_inputs_agree`),
`live_tv_delivery.rs` (a `PlanInputs` projection of `LiveSourceFacts` so
"the fields the planner reads" is one list, not a comment).

Tests:

- `plan_inputs_agree_covers_every_field_the_planner_reads`
  (`live_tv_delivery.rs`): for each field of `PlanInputs`, mutating it
  alone flips `Agree` → `Disagree`; a field outside `PlanInputs` mutated
  alone stays `Agree`.
- `a_warm_start_spawns_before_the_prefix_ceiling_and_publishes_only_after_agreement`
  (`tests/live_tv_two_node.rs`, behind `cluster-integration-tests`, whose
  fixture device streams a real ffmpeg-generated TS on port 5004 — there
  is no in-repo fake-ffmpeg script harness for the owner side): record
  the spawn instant relative to the first chunk (< `SOURCE_PREFIX_TIME`)
  from the owner log; with the fixture's probe delayed by an injected
  `ffprobe` wrapper on `PATH` that sleeps 2.5 s, assert no `Provisional`
  before the probe returns although the inventory was publishable at 2 s.
- `a_warm_start_that_disagrees_restarts_cold_and_never_publishes_the_wrong_plan`
  (same harness): seed the cache with facts saying `mpeg2video 1080i`
  while the fixture emits `h264 320x180p`; assert the first child was
  killed and reaped, the published plan is the probed one,
  `start_plan_total{source="warm_disagreed"}` is 1, and total start time
  stays under `STARTUP_FEEDING_TIMEOUT`.
- `a_cold_start_is_unchanged` (same harness): no cache entry → the
  fixture's `opens` counter is 1 and the log order is headers → prefix →
  probe → spawn, as today. The two-node file's existing cases (`:809`,
  `:989`, `:1050`) pass unchanged.
- `an_expired_or_foreign_generation_entry_is_cold`: entry past programme
  end, or with another `generation`/`device_id` → cold.
- `every_tuner_byte_reaches_ffmpeg_exactly_once_on_a_warm_start`: the
  fake stdin receives the concatenation of the tuner chunks, no gap, no
  duplicate.

Acceptance: `cargo test -p plurxd warm_start`, `cargo test -p plurxd
plan_inputs` green; `make live-tv-two-node-check` green on a host that can
bind 80 and 5004 (the fast lane does not run it — say in the PR body where
it ran); `make unit` green; §6.3's fleet measurement recorded in
[LIVE-TV-RELIABILITY-STATUS.md](LIVE-TV-RELIABILITY-STATUS.md) before the
PR leaves draft.

### 5.4 M4 — orphan owner, blocking inventory, honest lookup (L9)

Files: `live_tv.rs` (`OrphanSession`, `run_live_session`, sweeper arm,
`inspect_scratch`, `session()`, incarnation file).

Tests:

- `a_failed_cleanup_retires_the_session_and_keeps_the_child_until_reaped`:
  fake ffmpeg ignores SIGTERM for 8 s; after session end, `registry.sessions`
  no longer holds it, `held()` is unchanged by it, a new start is admitted;
  `orphans{kind="process"}` is 1 until the child exits, then 0; the
  encoder admission permit is released only then.
- `an_orphan_directory_is_never_swept_while_its_child_lives`:
  `sweep_orphan_scratch` during the above leaves the directory.
- `two_hundred_failed_cleanups_do_not_refuse_starts`: loop 300 sessions
  with failing `remove_dir_all` (read-only dir); a start after them is not
  refused with "recovery history is full".
- `inspect_scratch_keeps_every_check_in_one_blocking_scan`: parametrised
  over the six error predicates and the growing-`.tmp` case (a 200 MiB
  `segment-000003.ts.tmp` with an unchanged playlist → 128 MiB error);
  count `spawn_blocking` dispatches per call = 1 (+ the bounded playlist
  read).
- `a_hung_inventory_does_not_stack`: a blocking scan that never returns;
  ticks continue; only one scan task exists; the session ends on
  `PRODUCER_PROGRESS_TIMEOUT`.
- `an_unknown_capability_is_unavailable_unless_a_recent_incarnation_proves_a_restart`:
  no prior file → "not available"; prior file with `last_seen_at` 20 s ago
  → "restarted"; 10 min ago → "not available". All 410 `capability_expired`.

Acceptance: `cargo test -p plurxd orphan`, `cargo test -p plurxd
inspect_scratch`, `cargo test -p plurxd incarnation` green; `make unit`
green.

## 6. Verification and rollout

### 6.1 Commands

| M | Focused | Gate |
|---|---|---|
| M1 | `cargo test -p plurxd fence` · `make live-tv-two-node-check` | `make unit` |
| M2 | `cargo test -p plurxd owner_peer` · `make live-tv-two-node-check` | `make unit` |
| M3 | `cargo test -p plurxd warm_start` · `cargo test -p plurxd plan_inputs` · `make live-tv-two-node-check` | `make unit` |
| M4 | `cargo test -p plurxd orphan` · `cargo test -p plurxd inspect_scratch` · `cargo test -p plurxd incarnation` | `make unit` |

Web fixture: none of these changes a client-visible contract; the error
sentences in M4 are not in `tests/playback/live-tv-start-cases.json`
(codes are), so no Node gate changes. Re-verify at build time.

### 6.2 Rollout

One draft PR per milestone into `main` under the fast lane. No switch:
M1–M4 change server-internal mechanics with no user mode. Order: M2 (lowest
risk) → M1 → M4 → M3. Deploy each with the ansible playbook to the whole
fleet; the owner and ingress roles both change (M1/M3/M4 owner, M2 ingress),
and a mixed fleet is safe because no wire shape changes. Rollback is the
previous immutable image tag.

Metrics to watch after each deploy, on `media1` (owner) and one ingress
(`lab1`): `plurx_live_tv_fence_observations_total{state}` (M1 — `expired`
must stay 0 outside a real outage), `plurx_live_tv_owner_peer_total`
(M2 — `hit` ≫ `resolved`), `plurx_live_tv_start_plan_total{source}` (M3 —
`warm_disagreed` should be rare; if it is not, the cache expiry is wrong),
`plurx_live_tv_orphans{kind}` (M4 — returns to 0), plus the existing
`plurx_live_tv_session_ends_total{reason}` and
`plurx_live_tv_starts_total{outcome}`.

### 6.3 What only the fleet can prove — GPT prompts

```
L2 severity and grace sizing. On the three-voter lab (lab1–lab3) with the
Live TV owner on a follower and two live sessions from two clients:
1. Restart the raft leader's plurxd (`sudo systemctl restart plurxd` on
   the leader). Record from journalctl on the owner the first and last
   "transient replicated authority read failed" lines and their timestamps,
   and whether either session ended (plurx_live_tv_session_ends_total
   delta by reason).
2. Repeat three times. Report the longest gap between a successful
   settings read before and after the restart. If it exceeds 10 s, say so
   — FENCE_GRACE_MAX_AGE is sized on the ≈6 s read budget, not on this
   number, and this number decides whether that is enough.
Before the M1 deploy, expect both sessions to end with device_unavailable
within ~6 s of the restart; after, expect neither to end.
```

```
L6 start latency. On media1 with an ATSC 1.0 channel (Harbor Lights on
2.1) and an ATSC 3.0 channel:
1. Cold: restart plurxd, press Watch, record from the owner log the
   instants of "HDHomeRun stream request", FFmpeg spawn, and the first
   Provisional publication. Three runs each.
2. Warm: stop, wait 10 s, press Watch again on the same channel. Three
   runs each. Report the spawn-to-first-chunk delta and total start time,
   and plurx_live_tv_start_plan_total{source} deltas.
3. Zap across a programme boundary on 2.1 (start 30 s before the hour,
   stop, start 30 s after): expect a cold start (cache clipped to the
   programme end), not warm_disagreed.
Report the nine numbers per channel; do not average.
```

```
L9 orphan behaviour. On media1 with the runtime cache on the NAS-backed
work directory: start a session, then make the scratch directory
undeletable for 20 s (`chattr +i` on the session's live-tv-<uuid> dir),
stop the session. Expect: the session leaves Activity at once, a new start
is admitted immediately, plurx_live_tv_orphans{kind="scratch"} is 1 then
0 within 30 s of `chattr -i`, and the directory is gone. Then restart
plurxd while a session is live and press Play on the stale capability
from the web client: the toast must say "restarted", not "expired".
```

## 7. Open questions

1. `FENCE_GRACE_MAX_AGE = 10 s` is a policy choice made against the read's
   own ≈6 s budget. The first GPT prompt measures the fleet's actual leader
   failover; if it is routinely longer, the choice is between a longer
   grace and a faster failover, and that is Paul's call, not this plan's.
2. Should the warm-start cache be keyed by `playback` capability set as
   well? Today the plan is re-resolved per request from cached *facts*, so
   two different clients on one channel get different plans from one
   cache entry — that is the intended split (facts cached, plan resolved).
   Confirm at review.
3. Whether `incarnation.json` should also carry the last `serving`
   generation so an ingress could tell "owner restarted" from "owner
   fenced" — out of scope unless the L4 design needs it.
4. The 18 `PeerTransport::new` sites outside `http/live_tv.rs` (activity,
   auth revocation, cluster operations, fragment index) are the same
   pattern; several already hold a transport for their lifetime. A
   follow-up census, not this plan.
