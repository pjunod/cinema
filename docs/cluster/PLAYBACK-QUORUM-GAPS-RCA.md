# Playback dies on brief quorum gaps — root cause and proposed fix

**Status:** **open** — approved for implementation (v3.1), in progress
on `effort/playback-quorum-gaps`. Fable design review 2,
re-check: "approved with conditions"; both conditions are folded into
§4.5 row 1 and §4.2 (1). Fable reviewed in place of Astra, at Paul's
direction. · **Reviews so far:** Fable 5.1 adversarial review (9 findings) ·
Astra `gpt-6-astra` (changes requested; answered Q1–Q4) · Fable 5.1 design
review 2 (changes requested, narrow: V1–V3; R3, C3, C4 and §4 approved) ·
**Owner:** Claude (Opus 5.5), session
`session_01BCWLA2so4gEn4DTVZieugb` · **Verified against:** `main @ df27550d1`
· **Written:** 2026-11-01 · **Goes in:** `docs/cluster/` with an index row

> **Who reads this:** the build agent works through §5 one milestone at a
> time. §6 lists what must not change, and §9 maps every review finding to
> where it was addressed.

Companion to `docs/cluster/README.md`, which describes how the quorum and
serving fence are built. This document covers *why playback became
unreliable*, as measured on the fleet from 2026-10-30 01:44 to 2026-11-01
02:35 UTC, and the fix for each cause.

## 1. What Paul saw

At 22:20 EDT on 2026-10-31 (02:20 UTC on 2026-11-01), an Apple TV was
playing through media1. An iPad Pro then started a second title, also
through media1. The Apple TV froze to buffering, then errored. The iPad never
started; it errored out instead. Failures had also been frequent and
seemingly random on the days before.

| Client | Title (file) | Session server | What happened |
|---|---|---|---|
| Apple TV | Taylor Tomlinson: Quarter-Life Crisis (92) | lab6 | `seg00277.m4s` answered **503 after 3755 ms** at 02:20:43; client reported `server wait state unavailable` at 02:20:55 |
| iPad Pro | The Beekeeper (93) | media1 | Started at 02:20:42 on temporary live HLS (`vod_index_pending: foreground_preempted`). The producer died at 02:20:56 with `copy segmenter reader failed: writing seg00002.m4s: database error: CheckIsLeaderError`. The `master.m3u8` requested at 02:20:42 answered **503 after 55070 ms** (`init_inspection … response_publication_timeout`), and the client then showed `Server returned 410` |

## 2. Evidence, and how strong it is

The raw logs are on each node at `~/work/plurxd-<host>.log`, taken with
`docker logs plurxd` at 02:2x UTC on 2026-11-01. Claims are graded as
Astra asked:

| Claim | Strength |
|---|---|
| C0 (a starved voter) triggered tonight's elections | **Probable trigger**. Measured load, swap, PSI, sampler lateness and the onset of vote requests all coincide; not reproduced |
| C1, C3, C5 are failure mechanisms | **Supported by source** at `df27550d1`, and matched to log lines |
| C2 turns a short fence into a client error | **A deliberate contract today** (`control.rs:1571-1574`), confirmed in source |
| C4: why cache writes failed at the ceiling | **Unresolved** between two mechanisms (§3.4) |
| The Apple TV's 3755 ms 503 | **Not attributed**. The response code was not logged (§3.2) |
| 13 of media1's 74 self-fences | **Unexplained** |

**The starved voter.** The voters are media1 (raft node 1), lab6, and lab4
(raft node 6, the leader). lab3 is a learner.

- On lab6, load average was 50 on 12 cores, swap was 8191 of 8191 MiB, and
  PSI memory `full avg300` was 11.6 %. The source was another agent
  session's container, `claude-subs-build`. It ran `CARGO_BUILD_JOBS=6 cargo
  clippy -p plurxd --all-targets`: six `clippy-driver` processes of
  2.3–4.6 GB each, with no cap.
- lab6's sampler logged `tick_lateness_ms: 2718`. lab6 made 32 vote requests
  between 02:00 and 02:35, and none in the 48 h before.
- The raft term had held at **18242 for 48 h**. It then went 18242 → 18250 →
  18264 between 02:20 and 02:32. Each election left every node without a
  leader for about 4 s (`CheckIsLeaderError: has to forward request to:
  None, None`).
- The heartbeat is 800 ms and the election timeout is 2.4–4.0 s
  ([`migration.rs:160-164`](../../crates/plurx-core/src/cluster/migration.rs)).

**media1 fences itself without any election.** media1 logged `serving
authority expired; mutable media is self-fenced` 74 times in 48 h. lab6 logged
it 13 times and lab3 7. Outages ranged from 25 to 4000 ms, with a median of
285 ms and a p90 of 545 ms. In 72 of the 74, the latest sampler outcome was
`accepted`.

| Fences | Sampler phase at expiry | Reading |
|---|---|---|
| 46 | `local_publication`, `apply_lag_entries: Some(N>0)` (199 at 02:03:27) | C1: a fresh watermark had landed, but it was not yet applied |
| 15 | `request_round_trip`, in flight 508–538 ms | C1b: the next sample was slower than the 500 ms left on the lease |
| 13 | `request_round_trip`, in flight ≤ 25 ms | Unexplained |

**What each class cost on media1 over 48 h:** 90 HLS 503s · 78 cache writes
refused for `shared playback cache budget or filesystem reserve exhausted` ·
30 relayed-playlist 502s · 26 `reader_failed` playlist refusals · 4
copy-producer deaths (2 at the cache budget, 1 on `CheckIsLeaderError`, 1
pipe ended) · 1 VOD producer death at the cache budget.

## 3. Root causes

### 3.1 A healthy follower loses serving authority (C1, C1b)

**C1.** `ServingFence::refresh`
([`serving_fence.rs:992`](../../crates/plurxd/src/serving_fence.rs)) counts
authority as current only when the **newest** watermark has
`apply_lag_entries == Some(0)`. Watermarks are sampled every 500 ms, and
each grants a 1 s lease from the moment its sample started
([`migration.rs:5438-5440`](../../crates/plurx-core/src/cluster/migration.rs)).
`publish_watermark_at` (`:6416-6424`) overwrites W(n) when W(n+1) lands.

If the node catches up to W(n) only after that overwrite, it never observes
zero lag for W(n). The retained proof keeps its deadline, which is 1 s
after the start of the watermark that was current at the last zero-lag poll
(`:6035`). So a node that has applied everything committed as of a
confirmation less than 1 s old still fences itself. In practice the node has
about 500 ms to catch up, not 1 s.

**C1b.** The lease is 1 s, the refresh 500 ms, and the round-trip timeout
750 ms (`QUORUM_WATERMARK_TIMEOUT`). One sample slower than 500 ms therefore
lets the lease lapse while that sample is still in flight. The sampler's
interval uses `MissedTickBehavior::Skip` (`:6594`), so requests do not even
start at least 500 ms apart.

### 3.2 The HTTP fence 503s streams that are already running (C2)

Running media work outlives a loss of authority by up to 5 s
(`SERVING_FENCE_SESSION_GRACE`, `serving_fence.rs:207`). The outage budget
is owned by `TranscodeManager::serving_fence_loop` (`control.rs:1593`). HTTP
fails fast in two places:

- **At the router.** `mutable_media_serving_gate`
  ([`http/mod.rs:3144-3190`](../../crates/plurxd/src/http/mod.rs))
  answers `503 serving_fenced` at once for every `requires_authority` path,
  including every `/hls/` request for an existing session.
- **Inside a handler** (Astra R1). A request that passed the router can hit
  a fence at the publication boundary. `authorize_response_publication`
  rejects at `.admit()` (`transcode/manager/publication.rs:25`), and a loss
  between authorization and commit is rejected by `commit_guard_before`
  (`:599`). Both become `503 response_state_changed` (`http/hls/response.rs:210`).

Failing fast is deliberate today. `control.rs:1571-1574` makes keeping
sessions safe because "nothing they hold is served while the node is
fenced". AVPlayer treats a 503 on a playlist or segment as fatal
(`CoreMediaErrorDomain -16849`).

*Apple TV attribution:* the 3755 ms 503 on lab6 could be any of three:

- the router gate running on a runtime stalled for seconds;
- the inside-handler path above;
- a relay.

The response code was not logged, so M0 logs it.

### 3.3 A producer dies because a settings read needs a leader (C3)

Every object a rolling producer writes reaches
`playback_cache::reserve`
([`copyseg.rs:403`](../../crates/plurxd/src/copyseg.rs)) →
`PlaybackCacheBudget::refresh`. That re-reads `cache.max_gb` from the
**replicated Store** every `SAMPLE_AGE` (1 s), under the `operations` mutex
(`playback_cache.rs:396-411`). `make_room` and the sweep call
`refresh(true)` as well (`:764`, `:774`).

During an election the read fails and the reservation fails with it. The
producer then ends `ReaderFailed` (`copyseg.rs:1101`). A read that hangs
instead of failing costs `IO_TIMEOUT` (10 s), and every writer on the node
pays it. The VOD sink and `scratch_put` reach the same path.

### 3.4 Foreground writes fail when the shared cache is at its ceiling (C4)

The cache was at its 50 GiB ceiling (`remaining_budget=0`).
`reserve_owned` (`playback_cache.rs:520-526`) refuses in two cases:

- **(a)** `remaining(device) < bytes` after `make_room` found nothing it
  could evict. This is the likelier case.
- **(b)** `state.dirty` was set. That happens when another task's
  reservation is dropped before it settles (`:831-841`), or an eviction is
  cancelled (`:850-859`).

The sweep report is not logged with the refusal, so the logs cannot tell
the two apart. M0 adds it.

### 3.5 `master.m3u8` waits 55 s for a producer that has died (C5)

`exact_hls_context_before` (`http/hls/context.rs:21`) waits for the init
object through `segment_for_publication_before`. That wait is a 100 ms sleep
poll (`publication.rs:1899-1913`, `:1965-1974`). In the `!available`
branch (no served snapshot yet, which was the iPad's state) it never checks
for a terminal producer.

Two existing checks don't cover this case:

- `Session::failed` deliberately excludes failures after publication
  (`rolling/session.rs:1927-1930`).
- `published_producer_ended_before` (`:1931`) needs an exact attempt, and
  returns nothing for several actor states.

`coherent_path_producer_attempt() == None` can mean a healthy replacement is
under way (`:1605`). `Missing(None)` is read as `SessionGone` during
inspection (`context.rs:262`), so it is the wrong answer for a producer
failure.

### 3.6 The trigger: build load on a voter (C0)

Nothing stops an agent from running an uncapped container on a voter. The
runner fleet (`validation/runner-fleet.toml`) keeps heavy CI off voters, but
ad-hoc agent containers bypass it. The stall was memory and swap, not CPU.
[`deploy/docker-compose.yml`](../../deploy/docker-compose.yml) sets no
resource keys for plurxd.

```
 uncapped build on voter (C0)
        │ swap full, 2.7 s runtime stalls
        ▼
 voter times out leader → election storm (term +22)
        │ ~4 s with no leader, every node
        ├──▶ cache reserve's Store read fails (C3) ──▶ producer dies
        ▼
 serving fence expires on every node ◀── also every ~40 min on media1 alone (C1, C1b)
        │
        ▼
 router gate or publication boundary 503s running streams (C2) ──▶ AVPlayer error
        │
 master.m3u8 polls a dead producer (C5) ──▶ 55 s spinner, then 410
```

## 4. Proposed fixes

One effort branch, one commit per milestone. No new watchdogs, and no timer
whose job is to hide a symptom. Each fix makes an existing contract hold
where it does not today.

### 4.1 C1 — any applied, live confirmation proves authority

**Retention.** Each node keeps a bounded ring of quorum confirmations
(`K = 6`) inside `PassiveRaftMetricsAtomics`, under the existing seqlock.
Every entry is in the **current generation**: same term, same leader, same
local observation epoch, same local-read protocol, and not invalidated.

- **Adding.** Publishing a watermark appends it to the ring.
- **Clearing.** A generation change, an invalidation, `record_error`, or a
  regressed watermark clears the ring.
- **Full ring.** When the ring is full, the oldest entry is dropped. This
  fails closed: dropping an entry can only lose eligibility, never grant it.
  A debug assertion and tests check that, at both the 500 ms and the
  250 ms cadence, the dropped entry has already expired. With `Skip` and a
  single in-flight attempt, at most 3 entries are unexpired at 500 ms and
  at most 5 at 250 ms (Fable review 2), so `K = 6` covers both.

**Eligibility.** `eligible_serving_proof_at` (`migration.rs:5979`) keeps its
zero-lag rule first. Failing that, it returns a proof anchored on the
**newest** ring entry that meets every condition below:

- `applied_index ≥ committed_index`;
- younger than `QUORUM_WATERMARK_LEASE`, measured from the entry's own
  start;
- passes every binding and freshness check that `bounded_replica_state_at`
  applies to the current watermark.

The proof keeps that entry's original deadline. `ServingFence::refresh` ORs
`eligible_serving_proof().is_some()` into `authority_is_current` for the
local-binding case, so nothing that is ready today becomes unready.

**Contract amendment (Astra Q3).** Rule 4 of `decide_authority`
(`serving_fence.rs:905-918`) is amended:

> A fenced node may become ready again by having applied a still-live,
> quorum-confirmed index of the current generation. No lease is extended.
> Eligibility is deliberately widened from "applied the newest confirmation"
> to "applied any live confirmation".

The doc comment and the module docs change with the code. The
bounded-replica read rules are unchanged.

**Expected effect, not a guarantee.** At the 500 ms cadence the catch-up
window grows from about 500 ms to up to the full 1 s lease. Because the
sampler uses `MissedTickBehavior::Skip` and allows only one attempt in
flight, the gap between confirmations is not fixed, so neither is the
window.

**C1b deferred (Astra Q2).** The cadence stays at 500 ms. After M1 and M3
ship, the fences are re-measured. If faster sampling is still needed, the
follow-up is serial 250 ms with this same ring. `K = 6` already covers the
`0 / 250 / 500 ms` case: an entry is kept until its own lease lapses.
Overlapping requests are out of scope.

### 4.2 C2 — the manager owns the outage outcome, and HTTP observes it

**One owner (Astra R2).** `TranscodeManager::serving_fence_loop` already
owns the outage budget through its long-lived `SessionGrace`. It now also
publishes the result on a `watch` channel:

```rust
pub(crate) struct OutageResolution {
    loss_generation: u64,
    state: OutageState, // Pending { expires_at } | Recovered { generation } | Expired
}
```

The loop writes `Pending` when it observes a loss. It writes `Recovered`
or `Expired` when `resolve_loss` returns. HTTP waits are read-only: they
never construct a `SessionGrace`, so cancelling a request cannot reset or
spend the budget, and repeated flapping is accounted for once, in the
manager.

`expires_at` is not computable from `SessionGrace` today. Its fields are
private, and `remaining` and `lost_at` are locals of `resolve_loss`
(`serving_fence.rs:261-268`). It gains one named API,
`SessionGrace::deadline_for_loss(lost_at) -> Instant`, computed exactly as
`resolve_loss` computes its bound. `resolve_loss` uses it as well, so the
two cannot drift apart. A request that arrives four seconds into an outage
therefore waits about one second.

**Wait predicate (Fable V2).** `.admit()` already fails during the
pending-loss window (`serving_fence.rs:862-863`). That is before
`loss_generation` is bumped (`:870`) and before the watch the manager reads
is sent (`:874`). So a waiter can still see the *previous* resolution,
`Recovered { generation == admitted }`. The wait therefore completes only
when **`resolution.loss_generation > admitted_generation` and
`state != Pending`**. A router-edge waiter compares against the loss
generation it read on arrival. A stale `Recovered` never consumes the
retry.

**Where HTTP waits (Astra R1).**

1. **At the router.** The gate is a `Router::layer`
   (`http/mod.rs:2198-2205`), so `MatchedPath` is available, and the hold
   is keyed on it. It covers **every GET route template under
   `/api/v1/hls/{session}` except `starts_mutable_media`**
   (`:2142-2178`): `index.m3u8`, `video.m3u8`, `status`, `quality-family`,
   `subs/{index}/index.m3u8`, `subs/{index}/{segment}`,
   `{role}/{rendition}/{kind}/{object}` and `{segment}`. The list is
   enumerated in code next to the routes, so a new route has to be
   classified explicitly. Matching on the template, not on a `/hls/`
   substring, means Live TV
   (`/api/v1/live-tv/sessions/…`, whose grace `live_tv.rs` owns) is never
   held against a resolution the manager doesn't own. Such a GET, if it is
   not a `starts_mutable_media` admission and arrives while the node is
   fenced, waits for the current loss generation to resolve. It waits until
   `min(request deadline, expires_at)`.
   - `Recovered`: the request runs as a fresh arrival.
   - `Expired`, or the deadline passes: the 503 `serving_fenced` it gets
     today.

   `POST …/control` is **not** held at the router or replayed: it fails fast
   as it does today, so a control handler never runs twice.
2. **At the publication boundary — buffered responses only (Fable V3).**

   **New rejection.** `MediaResponsePublicationRejection::AuthorityLost
   { admitted_generation }` is raised only when serving authority caused
   the refusal. Every other cause stays `StateChanged`. Three sites raise
   it:
   - the three `.admit()` calls: `publication.rs:25-28`, and
     `authorize_playlist_error_publication` at `:449-461`;
   - the `require_publication_authority!` macro, which today conflates
     "generation moved" with "release gate released". It is split so the
     two report separately;
   - `commit_guard_before` (`serving_fence.rs:176-182`). It returns why it
     refused, generation moved or deadline passed, instead of a bare
     `None`. Its other caller, `vodserve.rs:1078`, maps both back to its
     current `None`, so its behaviour is unchanged.

   No caller of the publication API exists outside `http/hls`.

   **Where it is handled.** Only the **buffered** response paths handle
   `AuthorityLost`:
   - the init/playlist path at `http/hls/response.rs:849`;
   - the buffered segment paths at `http/hls/segment.rs:1334` and `:1495`;
   - playlist-error publication.

   They wait as in 1. On `Recovered` the helper **abandons the first
   authorization and authorizes again from the top**: a fresh `.admit()`,
   then the existing checks of the exact session `Arc`, producer attempt,
   route and prepared object. Everything after `.admit()` (the first-media
   handoff at `publication.rs:235`, actor authorization at `:248`, delivery
   tracking) then runs as a *second request after the first was
   abandoned*. The code already tolerates that case, because clients
   abandon requests all the time; M2 tests it explicitly. No stale
   authorization or prepared handle is reused.

   **Streamed commits are excluded.** The streamed commit path runs
   detached, after the body has been sent (`http/hls/response.rs:319-345`).
   There is nothing left to re-serve, and re-running
   `commit_generation_metadata` there would be wrong, so it keeps today's
   behaviour.

   **Relation to the existing reclassification loop.** The authority wait
   finishes inside the authorize helper, before it returns. So the loop in
   `http/hls/playlist.rs:399-431` and `response_publication_state_changed()`
   see the same non-authority rejections they see today. When the wait
   ends `Expired` or hits the deadline, the helper returns the router's
   existing `serving_fenced` 503, not `response_state_changed`, so the
   client can tell the two apart.

   **Limits.** At most one retry per recovered loss, all inside the original
   request deadline. If the object no longer re-validates (attempt replaced,
   session retired), the existing rejection for that state is returned.
   Control handlers never take this path.

**What a held request may do after `Recovered`.** It behaves exactly like a
request that arrived just after recovery:

- an expired rolling session is already marked unservable before its
  asynchronous removal (`control.rs:1524`), and is refused by the existing
  checks;
- a held segment or playlist request **may** resurrect a reaped VOD
  attachment from its durable route (`segment.rs:460`), as any
  post-recovery arrival may, because that happens under the restored
  authority. After `Expired` no held request resurrects anything.

Nothing is served while the node is fenced, and every response is admitted
against the generation that came back. That is the invariant
`control.rs:1571-1580` relies on, and it still holds.

**Scope.** This covers responses owned by `TranscodeManager` (rolling and
VOD HLS). Live TV (`live_tv.rs:3536`) and progressive remux
(`stream.rs:3820`) own their own grace and keep today's behaviour; they are
a follow-up once this lands.

### 4.3 C3 — no Store call under `operations`, on any path

A dedicated settings task, started beside the sweep loop, reads
`cache.max_gb` every `SAMPLE_AGE` (1 s, the current update latency) and
publishes it to a `watch`. Both `refresh(false)` and `refresh(true)` (the
latter used by `make_room` and the sweep) read only that cached value. No
Store call happens under `operations`.

| State | Behaviour |
|---|---|
| Before the first successful read | Reservations fail exactly as they do today (fail closed) |
| Read fails or hangs | The last value is kept, with a WARN at most once a minute |
| Known ceiling of zero | Preserved and enforced |
| Ceiling updated | Takes effect within about 1 s, as today |

*Rejected:* backoff in hiqlite's `leader changed` helper. It slows every
Store call, and the media path would still depend on quorum.

### 4.4 C4 — measure, then at most one re-inventory

M0 adds the sweep report (`evicted`, `protected`, `remaining`, dirty
revision) to the refusal log. M4 reproduces the refusal and picks the fix:

- **(b) confirmed:** re-inventory **once**, keeping the dirty revision taken
  before the retry. If the revision changed again during the retry, fail
  closed with today's error. Outstanding destructors are not serialised by
  `operations`, so a single retry can be raced.
- **(a) confirmed:** reported back for a decision. It is not patched here.

**M4 outcome (measure first).** Both refusals reproduce deterministically in
unit form, and M0's log now names which one happened (`cause` =
`inventory_dirty` for (b), `no_room` for (a), with the sweep report). What a
local reproduction cannot say is which one the fleet hit: the 48 h logs carry
`remaining_budget=0`, which points to (a), but only a fleet log with `cause`
settles it. So no C4 fix ships here. M7's correlation reads the `cause`
field; if it shows (b), the one-retry fix above is the follow-up, and if (a),
the eviction policy goes back to Paul.

### 4.5 C5 — the init wait recognises a terminal producer by exact attempt

**Which snapshot fields cannot do this (Fable V1).** v2 named fields that
cannot detect "this exact attempt ended before its first served playlist":

- `terminal` is the session-level `RollingTerminalCause`
  (`playback_control.rs:6610-6616`), not per attempt.
- `producer_ended_with_proposal` requires `producer_media_published`
  (`:9751-9753`), which is false in exactly the C5 case.
- `decision_reason` comes from `pending_decision.or(last_decision)`
  (`:9740-9743`). It has no attempt binding, and it includes `Retry`
  decisions (`:6444-6458`).

**Fields used instead.** The actor snapshot gains two fields:

- `failed_attempt_decision: Option<{ failed_attempt, kind, reason }>`,
  projected from the actor's `ProducerDecision.failed_attempt`
  (`:6447`, `:6453`) and its decision kind;
- `terminal_prepublication_failure: bool`, projected from the actor's
  existing `has_terminal_prepublication_failure()` (`:9708-9717`).

Both are read under the same snapshot, so they are coherent with
`delivery.producer_attempt`.

**Classification.** In both sleep branches of
`segment_for_publication_before`, the wait classifies the exact attempt
`A` that the request's owner capability is bound to:

| Actor state | Result |
|---|---|
| `(terminal_prepublication_failure && delivery.producer_attempt == A)` **or** `failed_attempt_decision == { A, non-Retry kind }`. The first disjunct covers the `failure_applied` and `executor_lost` failures that carry no decision (`playback_control.rs:9708-9717`) | `SegmentPublication::Failed`, carrying `PlaylistError::ProducerEnded(reason)` stamped with **attempt A**; the reason is the decision's when there is one, otherwise the terminal failure's own kind |
| `failed_attempt_decision.failed_attempt == A` with kind `Retry`, or a replacement in progress (`replacing_child`, `coherent_path_producer_attempt() == None`) | Keep waiting; a successor is coming |
| `delivery.producer_attempt > A` (superseded) | The existing owner-transition rejection for the predecessor-bound request; the successor is never failed |
| Otherwise (running) | Keep waiting |

**Attempt stamping.** `PlaylistPublicationError::for_session`
(`transcode/response.rs:123-129`) stamps the *current* attempt, which is
wrong for a superseded one. A `for_attempt(A)` constructor is added, and
this path uses it.

`Missing(None)` is never used for a producer failure. Objects already
published keep their current serving rules. The 100 ms poll stays (Astra
Q4).

### 4.6 C0 — isolation enforced by cgroups and verified at runtime

The compose keys are mitigation only. CPU shares are relative weights, and
memory reservations are soft limits. What actually isolates plurxd is a
cgroup boundary that every other container on a voter falls under:

- **`plurx.slice`.** plurxd's compose service sets
  `cgroup_parent: plurx.slice` and **also** `mem_reservation: 3g`. A
  slice's `MemoryLow` reaches the container's scope only through the
  container's own `memory.low`, or through `memory_recursiveprot`, which is
  not assumed (Fable review 2). The slice has `MemoryLow` = 3 GiB and
  `CPUWeight=1000`.
- **`docker-other.slice`.** The dockerd default
  (`daemon.json "cgroup-parent": "docker-other.slice"`, with the `.slice`
  suffix the systemd driver requires) places every other container on a
  voter here. The slice has `MemoryHigh` = 50 % and `MemoryMax` = 70 % of
  host RAM, `MemorySwapMax` = 2 GiB and `CPUWeight=50`.
  - **No `IOWeight`.** It only works with BFQ or io.cost, and those are
    not assumed. The check script reports the IO scheduler instead.
  - **Siblings are bounded too.** This also bounds sonarr and the sentinel
    services on media1. Their steady RSS is measured before the limits are
    applied, and the limits are sized so they never bind on them.
- **Host values.** lab6 has 28 GiB; media1 60 GiB; lab4's RAM is read at
  install time, because its shell could not be reached tonight.
- **Agent builds** on voters go through a `scripts/voter-build` wrapper that
  adds per-container `--memory` = `--memory-swap` = 8 GiB, `--cpus` 4 and
  `-j4`. AGENTS.md and `docs/ci/AGENT-COMPILE-LOOP.md` require it.

**How it is verified, not just declared.** `scripts/voter-isolation-check`
runs on each voter and prints the effective values:

- plurxd's container cgroup path and its `memory.low`;
- the cgroup `memory.max` and `memory.swap.max` of every other running
  container;
- the slice settings from `systemctl show`.

The script also flags any container whose cgroup sits outside
`plurx.slice` or `docker-other.slice`, because an explicit `--cgroup-parent`
escapes the default. Any mismatch makes it exit non-zero. M7 runs it. A YAML
test is not offered as evidence.

**Paul's call.** Setting the dockerd default needs a dockerd restart on each
voter. Done in order, it doesn't interrupt anything:

1. Set `"live-restore": true` in `daemon.json`, if it is not already set.
   With it on, restarting dockerd keeps running containers up. The script
   checks the current value first.
2. Restart dockerd. Containers that are already running keep their old
   cgroup until they are recreated.
3. Recreate plurxd. That is a normal deploy, so it is Paul's.

Without the default, only containers started with `--cgroup-parent` are
bounded, and an ad-hoc agent container can still starve a voter.

## 5. Build plan

Branch `effort/playback-quorum-gaps`. `fix(...)` commits carry their
`Regression-Test:` line and a `validation/regressions.d` row. Every
acceptance check includes a revert: the old line is put back, and the new
test must fail.

| # | Milestone | Acceptance |
|---|---|---|
| M0 | Instrumentation | Failed playback responses log the error code, loss generation, owner kind and attempt. Cache refusals log the sweep report. The `serve_http` drain logs which listener it is |
| M1 | C1 ring (K=6) and amended rule 4 | Tests: (a) W(n) lags, W(n+1) lands, apply reaches W(n) → eligible, with W(n)'s original deadline; (b) **Astra's 0/250/500 ms case**: the first entry stays eligible until its own lease expires; (c) a generation change, invalidation or regression clears the ring; (d) a full ring drops only expired entries at both 500 ms and 250 ms, and fails closed otherwise; (e) bounded-replica reads still refuse on lag. Revert → (a) and (b) fail |
| M2 | C2 manager-owned resolution, router hold, publication-boundary retry | Tests: arrival while fenced recovers → 200; **arrival 4 s into an outage waits about 1 s**; **flapping across several requests spends one budget**; **cancelling a request does not reset the budget**; **loss after router admission** and **loss between authorization and commit** both recover with fresh authorization; **attempt replaced or session retired during the wait** → the existing rejection; **recovery concurrent with retirement** → refused; **a stale `Recovered` from the previous loss does not consume the retry** (loss observed through `.admit()` before the generation bump); **abandon-then-reauthorize after a first-media handoff** behaves as a second request; a streamed commit is not retried; a fenced-then-expired response carries `serving_fenced`, not `response_state_changed`; a Live TV path is never held, while a `subs/` path and a child-rendition object are held; `POST /hls/sessions` and `POST …/control` → immediate 503 |
| M3 | C3 settings task | Tests: a hung settings read **concurrent with a reservation that must evict** → the reservation proceeds on the cached ceiling; no first read → fail closed; a known zero ceiling is enforced |
| M4 | C4 measure, then fix | Deterministic reproduction of (a) or (b). If (b): one retry, and fail closed when the dirty revision changes again |
| M5 | C5 exact-attempt terminal check | Tests: a failure after init and segments but before the first served playlist → `ProducerEnded` **stamped with that attempt** within 1 s (paused clock); an **`executor_lost`-only failure (no decision)** also fails within 1 s; a `Retry` decision keeps waiting and **a healthy replacement completes**; a predecessor's death does not fail the successor; already-published objects behave as before |
| M6 | C0 slices, wrapper, check script, docs | `voter-isolation-check` passes on a lab host after setup, reading a non-zero `memory.low` for plurxd; it fails while a deliberately uncapped container, or one with an explicit foreign `--cgroup-parent`, is running |
| M7 | Fleet proof (server-side) | Scripted HLS clients on a lab node, under a capped build and the existing raft fault tooling. The log correlation must show, for every failed response: error code, loss generation, producer outcome and listener. Leader restarts and dockerd restarts are **Paul's call** |

After that, Paul's lifecycle: a draft PR, an adversarial review, fixes,
the fast lane once (rerunning only failed tests), then merge. Paul deploys.

## 6. Guardrails — do not do these

- **Do not lengthen `QUORUM_WATERMARK_LEASE` or the election timeouts.**
  The lease must stay below the election floor (`migration.rs:7671`).
- **Do not hold new admissions or replay control handlers.**
- **Do not create a `SessionGrace` per request.** The manager's grace is
  the only outage budget.
- **Do not reuse a pre-loss authorization after recovery.** Authorize again
  from the top.
- **Do not add per-symptom watchdogs or a retry loop around a producer.**
- **Do not change the bounded-replica read lag rule.**
- **Do not kill or pause other sessions' containers as a fix.**

## 7. Astra's answers, as they stand in this plan

| Q | Astra | In this plan |
|---|---|---|
| Q1: hold running-session requests? | Yes, if R1/R2 are met; new admissions stay fail-fast | §4.2 |
| Q2: C1b cadence? | Defer; if needed later, serial 250 ms with matching retention | Deferred; the ring already supports 250 ms (§4.1) |
| Q3: wider apply-lag tolerance? | Yes, within the original deadline; amend the recovery contract | Rule 4 amended explicitly (§4.1) |
| Q4: C5 poll or watch? | Keep the poll; fix the terminal semantics | §4.5 |

## 8. Still unexplained

- **lab4 (the leader)** runs build `206-gac69a35ee`. It accepts the deploy
  key, but its login shell never starts, so its logs were not read.
- **`plurxd: shutdown complete`** is logged on media1 and lab3 at each leader
  loss (02:20:53, 02:25:29) while the container keeps running. M0
  identifies the listener.
- **lab6's `hiqlite::split_brain_check`** warns every minute.
- **13 of the 74 media1 fences** remain unclassified.

## 9. Review record

| Source | Finding | Where addressed |
|---|---|---|
| Fable 1 | C5 is a poll with no terminal check | §3.5, §4.5 |
| Fable 2 | C1 covers 46 of the 74 fences | §2, C1b |
| Fable 3 | Wider tolerance; rule 4 | §4.1 |
| Fable 4 | Apple TV 3755 ms not from the immediate gate | §2, §3.2, M0 |
| Fable 5 | A dirty loop under `operations` stalls writers | §4.4: one retry |
| Fable 6 | (a) is likelier than (b) | §3.4, M4 |
| Fable 7 | Store read on the hot path | §4.3 |
| Fable 8 | 503-while-fenced is deliberate; one policy | §3.2, §4.2 |
| Fable 9 | Memory, not CPU; wrong levers | §4.6 |
| Astra R1 | Losses inside the handler | §3.2, §4.2 (2), M2 |
| Astra R2 | One budget owner; "the session exists" is not enough; VOD resurrection | §4.2, M2 |
| Astra R3 | Retention must match cadence | §4.1 ring, M1(b) |
| Astra R4 | Typed failure, exact attempt, healthy replacement | §4.5, M5 |
| Astra §3 C3 | `make_room` `refresh(true)`; hung read during eviction; zero ceiling; latency | §4.3, M3 |
| Astra §3 C4 | Preserve the dirty revision | §4.4 |
| Astra §3 C0 | Limits must be verified at runtime | §4.6, M6 |
| Astra §4 | Evidence grading; correlate in M7 | §2, M0, M7 |
| Fable rev. 2 V1 | Named snapshot fields cannot detect the C5 case; attempt stamping | §4.5 (new fields, `for_attempt`), M5 |
| Fable rev. 2 V2 | Stale `Recovered` visible during the pending-loss window; `expires_at` not computable | §4.2 wait predicate, `deadline_for_loss`, M2 |
| Fable rev. 2 V3 | Streamed commits; third `.admit()`; conflated causes; reclassification loop | §4.2 (2), M2 |
| Fable rev. 2 R3 | K=4 too small at 250 ms | K=6, M1(d) |
| Fable rev. 2 C0 | `MemoryLow` reach, `live-restore`, escapes, siblings, `IOWeight` | §4.6, M6 |
| Fable rev. 2 low | `/hls/` substring parks Live TV paths | §4.2 (1): route template |
| Fable re-check cond. 1 | `executor_lost`/`failure_applied` failures have no decision | §4.5 row 1 disjunction, M5 |
| Fable re-check cond. 2 | Router hold template list was incomplete | §4.2 (1) enumerated via `MatchedPath`, M2 |

## 10. Build record: where the code differed from this plan

Recorded by the build agent on `effort/playback-quorum-gaps` (M0–M6).

| Plan said | Code had | What was built |
|---|---|---|
| §4.2 (1) hold list | Two more GET reads of an existing session: `master.m3u8` and `{role}/{rendition}/index.m3u8` | Both are held. The list is `HLS_SESSION_ROUTES` in `http/hls/fence.rs`, and a source test fails on an unclassified route |
| §4.2 (1) "request deadline" | The router has no request deadline | The hold is bounded by the grace from arrival and by the outage's `expires_at` |
| §4.2 (2) three `.admit()` calls | Two in `publication.rs`; the third is session registration | Both publication calls raise `AuthorityLost` |
| §4.2 (2) `commit_guard_before` has one other caller | Seven more (`create.rs`, `control.rs`) | Its `Option` signature stays; `commit_guard_or_refusal_before` says why, for publication commits only |
| §4.5 `terminal_prepublication_failure: bool` | A bool cannot carry the reason when no decision exists | `Option<&'static str>` with the kind (`executor_lost`, `failure_applied`) |
| §4.5 "both sleep branches" | Three sleeps | The two before any object opens. The third waits on an attempt that has published, which `published_producer_ended_before` already covers |
| §5 `validation/regressions.d` rows | Refused past the merge-errata boundary (`validation/history.py`, phase B) | `Regression-Test:` lines in the pull request instead |

Not covered: a pre-publication segment request (not `master.m3u8`) whose
producer dies still ends `response_state_changed`, because
`authorize_playlist_error_publication` publishes `ProducerEnded` only with the
published-producer proof. With no running `serving_fence_loop`, a held
request fails fast, as it did before the hold.
