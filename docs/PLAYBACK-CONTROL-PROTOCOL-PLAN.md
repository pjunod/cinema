# Playback control protocol — explicit demand, one owner, prepared handoffs

**Status:** adversarially reviewed implementation handoff · M1, web M2, M3a,
and M3b merged · M3c delivery-event ownership in progress · updated
2026-08-26 against `origin/main` at `5908e838` · wire names, defaults, and
source locations must be re-verified at build time

This plan replaces the temporary growing-HLS recovery engine's inferred
control loop with an explicit client/server protocol. It does not replace the
immutable VOD presentation. VOD remains the preferred delivery method; this
work makes the fallback understandable, bounded, and dependable while also
giving VOD, automatic quality, subtitles, and clustered handoff one shared
source of playback truth.

Companions: [PLAYBACK.md](PLAYBACK.md),
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md),
[ADAPTIVE-QUALITY.md](ADAPTIVE-QUALITY.md),
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md),
[CLUSTERING-PLAN.md](CLUSTERING-PLAN.md), and
[CLUSTER-MEDIA-POOL-PLAN.md](CLUSTER-MEDIA-POOL-PLAN.md). The independent
finding ledger is
[PLAYBACK-CONTROL-PROTOCOL-REVIEW.md](PLAYBACK-CONTROL-PROTOCOL-REVIEW.md).
The current actor-event slice is specified in
[PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md](PLAYBACK-CONTROL-PROTOCOL-M3-DELIVERY-LEDGER.md).

## 0. Decision summary

Build a versioned request/response control protocol over authenticated HTTP.
Each active player periodically reports what it intends to do and what the
playback framework can actually render. The server answers with its producer,
delivery, and lease state plus at most one serialized action. Media requests
remain ordinary HLS requests; the control exchange explains and coordinates
them rather than carrying video bytes.

The temporary live engine is then rewritten as one actor-owned state machine.
The actor owns the child process, demand, producer deadline, playlist catalog,
retention, flow control, and replacement transaction. It receives facts from
HTTP handlers and child-process tasks. Those tasks do not independently
restart, reap, suspend, or supersede a session.

The same protocol coordinates prepared quality, codec, dynamic-range, audio,
subtitle, and node replacements:

```
 client intent/observation              server facts
 position · buffer · render state       producer · delivery · owner lease
 throughput · track choices             materialized range · admission
                \                        /
                 +---- control actor ----+
                            |
                     one chosen action
                            |
             hold · prepare · commit · abort · end
```

The important boundary is ownership. Detectors produce observations or
proposals. Only the session actor may mutate delivery state, and only its
action arbiter may tell a client to replace media. This rule exists because
the present whack-a-mole behavior comes from several locally reasonable
watchdogs taking globally conflicting recovery actions.

## 1. Baseline on merged `main`

### 1.1 The fallback that actually ships

The `live-hls-recovery` Cargo feature is on by default. Public clients still
request `presentation: "vod"`; `TranscodeManager::create_session_inner`
tries VOD first and falls back to `start_live_recovery_session` for these
typed prerequisite failures when `playback.vod_live_recovery` is not `0`:

- `vod_index_pending`;
- `vod_transcode_unavailable`;
- `vod_subtitle_burn_unavailable`;
- `vod_source_unsupported`.

The response reports `vod: false`, and all three clients now accept it. An
explicit legacy presentation remains refused. This distinction matters: the
rewrite must preserve VOD-first selection and the typed fallback boundary. It
must not reintroduce live HLS as a public client choice.

### 1.2 How the growing engine works today

A live recovery create starts copy/remux or transcode production near the
requested offset. The server waits for a publish cushion, serves a growing
playlist, changes that playlist to a retained sliding window later, records
the highest fetched segment, and uses that inferred frontier to pace,
suspend, resume, and prune production. A create for the same
`(user, playback_id)` supersedes the predecessor. Seek, quality, track, and
recovery changes therefore create a new session and timeline.

The server already knows valuable facts: produced and fetched frontiers,
encoder progress/speed, delivered bytes/bitrate/idle time, disk bytes, and
admission state. The clients know different facts: actual playhead,
contiguous playable buffer, render/wait state, chosen tracks, decoder errors,
and whether playback is wanted. The current `/status` poll only returns the
server half. It deliberately does not count as activity, so every important
client fact is inferred from media fetch timing.

### 1.3 Why patching `/status` is insufficient

Making `GET /status` keep a session alive would fix one symptom but retain the
fundamental asymmetry. It would not distinguish pause from network loss, a
full buffer from an abandoned tab, decoder failure from encoder starvation,
or a planned replacement from two watchdogs racing. A control request must
state intent, carry observations, renew an explicit lease, acknowledge
actions, and be sequence fenced.

### 1.4 Existing identity and cluster contracts

The implementation must preserve these merged contracts:

| Identity | Current meaning | New protocol use |
|---|---|---|
| `playback_id` | Stable player-instance supersession key | Groups delivery generations for one client intent |
| `request_id` | Idempotent create attempt | Idempotent successor creation |
| `session_id` | Bearer capability and public HLS URL | One delivery generation and its control URL |
| `incarnation_id` | Durable replicated media-session identity | Non-secret delivery `generation` returned to the client |
| `owner_epoch` | CAS-fenced cluster ownership generation | Control epoch; rejects every stale-owner mutation |
| `client_instance_id` | New random UUID per player-controller lifetime | Separates a restarted controller's sequence space |
| control `sequence` | New monotonic counter | Rejects reordered state within `(generation, owner_epoch, client_instance_id)` |
| `action_id` | New, server-issued UUID | Makes replacement phases idempotent |

`session_id` remains stable during the currently supported same-incarnation
cluster takeover. A recipe change creates a new `session_id`. Later rolling
hard-failover work may also create a successor session instead of splicing a
new producer into a mutable timeline; §10 defines that rule.

`session_id` is never copied into a control body or response field merely as a
generation token. It is a bearer capability already present in the URL.
`incarnation_id` is the non-secret delivery generation. A cluster takeover
keeps that generation and increments `owner_epoch`; a recipe successor gets a
new incarnation and starts at owner epoch 1.

## 2. Goals, success criteria, and non-goals

### 2.1 Goals

1. A healthy playing, paused, seeking, or bounded-background-hold player whose
   renewals arrive never loses its session because the server guessed intent
   from fetch silence.
2. Copy and transcode production follow reported buffer demand without
   unbounded disk growth or oscillating `SIGSTOP`/`SIGCONT` decisions.
3. Exactly one component selects recovery. Multiple observations may arrive,
   but they result in one idempotent action.
4. Auto quality distinguishes network, encoder, and decoder pressure and can
   prepare a successor before cutting over.
5. Recipe changes include resolution, codec, Dolby Vision/HDR/SDR, audio,
   subtitle mode, and node placement without duplicating transition logic.
6. Operators can explain every stall, hold, replacement, and retirement from
   structured state transitions rather than timing inference.
7. VOD sessions use the same control vocabulary where useful without making
   immutable media depend on the control channel.

### 2.2 Measurable success

The final cutover is accepted only when all of these are true:

- no freestanding live-session recovery task can restart or replace media;
- a 30-minute pause with control heartbeats physically delivered retains the
  session while producing no new rolling media;
- losing control heartbeats but continuing media fetches does not reap a
  healthy session;
- losing both renewals expires the playback lease once and releases producer
  admission once;
- scripted network cliffs cause at most one replacement transaction and no
  lower-quality oscillation during its cooldown;
- a failed true make-before-break successor leaves the current stream running
  and playable; a one-slot buffered transfer satisfies its separately measured
  interruption and predecessor-restart contract (§5.3);
- cluster owner fencing prevents two nodes from accepting mutating actions;
- fault-injection tests enumerate every remaining deadline in §7;
- physical web, AVPlayer, and Media3 runs record switch interruption and
  playback-stall p50/p95/p99 separately.

### 2.3 Non-goals

- Do not replace HLS media transport with a custom byte protocol.
- Do not make the control channel a general event bus or chatty telemetry
  firehose.
- Do not put every heartbeat or buffer sample through Raft/Hiqlite.
- Do not make VOD segment availability depend on a live control connection.
- Do not let subtitle, quality, cluster, or decoder subsystems directly tell a
  client to reopen.
- Do not fold library indexing, offline packaging, cache warming, or other
  background jobs into playback sessions. They may yield to foreground
  urgency, but they have different lifetimes.
- Do not promise a literally invisible codec or dynamic-range transition on a
  platform that must reconfigure its decoder or display mode. The protocol
  promises prepared, transactional, position-preserving handoff and measures
  any visible transition.

## 3. Transport and wire contract

### 3.1 Transport decision

Add this capability-authenticated route before the segment catch-all:

```text
POST /api/v1/hls/{session}/control
```

Choose bounded HTTP request/response rather than WebSocket for the first
version because it follows existing ingress routing, authentication, timeout,
and cluster relay behavior; works with web, AVPlayer, and Media3 without a
second connection lifecycle; and naturally retries through node failure. The
server advertises the next interval, so this is explicit control rather than a
fixed diagnostic poll.

The response uses `Cache-Control: no-store`. The request body is capped at
16 KiB before JSON parsing. The handler deadline is shorter than the
advertised heartbeat interval and never waits for media production. Slow work
is represented as state and returned on a later exchange.

WebSocket remains a rejected first implementation, not a forbidden future
transport. It may be added behind the same messages only if measured HTTP
overhead is material. Long polling is also rejected: background timer and
ingress behavior are harder to reason about, while none of the actions need
sub-second delivery.

### 3.2 Additive start response

`StartResponse` gains an optional object so rolling upgrades remain safe:

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct ControlBootstrap {
    pub protocol: String,          // exactly "plurx-playback-control-v1"
    pub url: String,               // /api/v1/hls/{session}/control
    pub generation: String,        // this delivery's non-secret incarnation_id
    pub control_epoch: u64,        // current durable owner_epoch
    pub next_exchange_ms: u32,     // initial recommendation, not a deadline
    pub lease_timeout_ms: u32,     // server's current advertised lease
}

pub struct StartResponse {
    // existing fields unchanged
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlBootstrap>,
}
```

Old clients keep using media requests and the legacy idle behavior during
migration. New clients require `control` only after the server capability
advertises protocol v1; they do not infer support from version strings.

### 3.3 Request schema

The concrete v1 body is deliberately small and bounded:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequestV1 {
    pub protocol: String,
    pub generation: String,
    pub control_epoch: u64,
    pub client_instance_id: String,
    pub sequence: u64,
    pub demand: PlaybackDemand,
    pub position_ms: i64,
    pub buffered_from_ms: Option<i64>,
    pub buffered_through_ms: i64,
    pub playback_rate: f64,
    pub render_state: RenderState,
    pub seek_target_ms: Option<i64>,
    pub observed_download_bps: Option<u64>,
    pub selection: ClientSelection,
    pub capabilities: Option<DynamicCapabilities>,
    pub observation: Option<ClientObservation>,
    pub acknowledgement: Option<ActionAcknowledgement>,
}
```

Normative field rules:

- `protocol` must equal `plurx-playback-control-v1`. An exact value prevents
  silently interpreting a future message with old semantics.
- `generation` must equal the path session's active delivery generation.
  Stale controls against a retiring predecessor do not renew its lease.
- `control_epoch` must equal the durable route's current `owner_epoch`. An old
  epoch returns `409 owner_changed` before mutation or lease renewal, even when
  `session_id` stayed stable through takeover.
- `client_instance_id` is a random UUID created by the controller and bound on
  its first accepted exchange in an epoch. A different instance cannot reset
  the sequence of an active binding.
- `sequence` starts at 1 and strictly increases inside
  `(generation, control_epoch, client_instance_id)`. An equal sequence returns
  the previous action outcome without applying it again and **does not renew
  the playback lease**; a lower sequence returns `409 stale_control`. A newly
  accepted higher sequence is the only control request that renews.
- `demand` is `active`, `hold`, or `end`. `hold` includes intentional pause
  and supported background grace; it renews the lease but requests no new
  production. `end` is terminal and equivalent to an orderly release.
- `position_ms` and buffer endpoints use film time, not session-relative time.
  Negative values are rejected. For a known duration, values greater than
  `duration_ms + max(2_000, target_duration_ms)` are rejected; the allowance
  covers one player/segment rounding interval and is capped at 30 seconds.
- `buffered_from_ms` and `buffered_through_ms` describe the contiguous range
  containing or immediately following the playhead. The client computes this
  from platform ranges. Sending an arbitrary range list is unnecessary for
  production decisions and creates an avoidable unbounded surface.
- `playback_rate` is finite and in the supported `[0.25, 4.0]` range when
  demand is active. Pause is represented by `demand`, not rate zero.
- `render_state` is `starting`, `rendering`, `waiting`, `stalled`, `seeking`,
  `ended`, or `failed`. It is an observation, never permission to replace.
- `seek_target_ms` exists only while seeking and is coalesced by sequence. A
  newer target cancels preparation for the older target.
- `observed_download_bps` is optional because native frameworks do not expose
  equivalent estimators. Server delivery measurements remain authoritative
  for bytes the server actually wrote.
- `capabilities` is sent on the first exchange and whenever output/display
  capabilities change. Omission means unchanged, not unknown.
- client error text is a bounded enum plus at most 512 sanitized bytes. Raw
  URLs, tokens, decoder dumps, and media contents never enter the message.

`ClientSelection` includes quality mode (`auto` or a manual height), audio
track, subtitle track/mode (`off`, `native`, `overlay`, or `burn`), audio
offset, requested codec family, and requested dynamic-range policy. The
server answers the effective recipe; it never treats a client request as proof
that a device can decode or display it.

### 3.4 Response schema

```rust
#[derive(Serialize)]
pub struct ControlResponseV1 {
    pub protocol: String,
    pub generation: String,
    pub control_epoch: u64,
    pub accepted_sequence: u64,
    pub server_time_unix_ms: i64,
    pub lease: PlaybackLeaseView,
    pub delivery: DeliveryView,
    pub effective_selection: EffectiveSelection,
    pub action: ControlAction,
}
```

`PlaybackLeaseView` reports `state`, `renew_after_ms`, and the absolute
`expires_at_unix_ms`. An absolute expiry stays honest on an idempotent replay;
the action outcome remains equivalent while the lease is not extended.
`DeliveryView` reports only facts the controller uses or an
operator needs: presentation, producer state, produced/materialized through,
served/fetched through, delivered bitrate and idle interval, recent producer
speed, contiguous client runway, admission state, hold reason, owner node hash
and owner epoch. Capability tokens and raw node addresses are excluded.

Exactly one `ControlAction` is returned:

```text
none
hold(reason)
prepare_replacement(action_id, successor, switch_at_ms, reason)
commit_replacement(action_id, switch_at_ms)
abort_replacement(action_id, reason)
retry_resource(after_ms, reason)
terminal(code, message)
```

The action object is tagged by `type`; it is never an untyped nullable bag.
An equal request sequence returns equivalent action semantics without a lease
renewal. A client acknowledges `metadata_ready`, `buffer_ready`, `committed`,
`failed`, or `aborted` with the matching `action_id`; `buffer_ready` includes
the successor's film-time `buffered_through_ms`, and `committed` includes
first-frame time. Unknown and completed action IDs are harmless typed
conflicts, not triggers for a new replacement.

### 3.5 Error contract

| HTTP | Code | Meaning | Client behavior |
|---|---|---|---|
| 400 | `invalid_control` | Bounded field validation failed | Stop v1 for this session and log the named field |
| 404 | `session_gone` | No local or durable route exists | Reopen from current film position once |
| 409 | `owner_changed` | Control epoch lost after owner takeover | Re-snapshot under the returned epoch; never retry the old mutation |
| 409 | `stale_control` | Generation, client instance, sequence, or action fence lost | Apply returned current state; do not retry stale state |
| 410 | `session_ended` | Session deliberately ended or superseded | Stop renewing predecessor; follow successor if supplied |
| 410 | `owner_lost` | The owner is gone and its durable recipe can never be taken over | Stop retrying; reopen from the film position the media plane returns |
| 425 | `owner_transition` | Durable route exists and a successor could still claim it | Retry after the bounded response hint |
| 429 | `control_rate_limited` | Client exceeded the per-session control budget | Back off to the returned interval; media remains independent |
| 503 | `control_unavailable` | No eligible owner can currently answer | Keep consuming buffer and retry through another ingress |

The two owner answers are decided by one rule, `classify_owner_loss`, shared
with the media plane: a route no longer authorizing control — publication
fence pending, or lease no longer renewed — is a transition while the takeover
path could still accept it, and loss when it never can. A live lease is a live
owner either way; a committed replacement holds one through its whole
publication window.

Three gates apply that rule — public ingress, the owner side of the relay, and
`verify_authority`'s re-read — because each returns before the next runs. Each
is pinned by a test that drives it.

The media plane answers the same durable state with 503
`media_owner_transition` or 410 `media_owner_lost`, and carries
`film_position_ms`, `film_frontier_ms` and `continuous:false` with both. The
control plane deliberately carries no resume: the client is already fetching
media, so the position reaches it there, and `ControlErrorBody` is
`deny_unknown_fields` — new fields on it would be a mixed-fleet parse hazard
for no gain.

**Rollout property.** `validated_control_relay_response` checks a peer's error
body against a strict `(status, code)` allowlist, so an ingress node that
predates `owner_lost` refuses a newer owner's answer and degrades it to
`control_unavailable` — a retry, which is exactly the behaviour that exists
today. A mixed fleet is therefore never *wrong* here, only temporarily too
soft, and it settles as soon as ingress is upgraded.

No control error destroys the currently buffered player by itself. The client
controller makes that choice only after buffer exhaustion or a terminal
session verdict.

## 4. Server ownership model

### 4.1 One actor per delivery generation

Replace the live `Session` bag of mutexes, atomics, and detached recovery tasks
with a `RollingSessionActor`. The actor exclusively owns:

- lifecycle and playback lease;
- current effective recipe and presentation;
- child process handle and admission permits;
- producer progress and the one producer deadline;
- segment catalog, publication shape, fetched frontier, and reader pins;
- client demand, playhead, buffer runway, and render observations;
- pacing/hold state and disk-ahead budget;
- active replacement transaction;
- failure and retirement verdicts;
- the last accepted control sequence and idempotent response.

HTTP, stderr/progress readers, and child waiters send typed commands with
one-shot replies. They may do bounded I/O, but they cannot mutate actor state
or spawn recovery. The actor loop uses `tokio::select!` over commands, child
events, and its nearest owned deadline.

```
 segment GET ----\
 playlist GET ----> RollingSessionActor ----> producer command
 control POST ----/       |     |             publish/hold/stop
 child progress ----------+     +-----------> one ControlAction
 owner lease event -------+
```

This is the primary cleanup, not an internal refactor for its own sake. The
current `child_transition`, `watchdog_active`, `replacing_child`, failure
atomics, request clock, segment/frontier locks, and independent stall tasks
exist to reconstruct serialization that an actor provides directly.

### 4.2 Normalized state and proposals

All control inputs normalize into these internal values:

```rust
struct PlaybackDemandState {
    wants_media: bool,
    position_ms: i64,
    buffered_through_ms: i64,
    runway_ms: i64,
    render_state: RenderState,
    last_renewal: Instant,
}

struct DeliveryObservation {
    produced_through_ms: i64,
    fetched_through_ms: i64,
    delivered_bps: Option<u64>,
    delivered_idle: Duration,
    producer_speed: Option<f64>,
    producer_progress_at: Option<Instant>,
}

struct ActionProposal {
    source: ProposalSource,
    severity: Severity,
    kind: ProposalKind,
    evidence: EvidenceSnapshot,
}
```

Quality, subtitles, decoder compatibility, cluster drain, node loss, and
producer failure submit `ActionProposal`s. A stable arbiter ranks terminal
safety, buffer preservation, manual intent, severe downgrade, cluster move,
ordinary downshift, track change, and upgrade. It may combine compatible
changes into one successor recipe. It never returns two actions in one
exchange.

### 4.3 Demand-driven production

The actor derives a target production frontier from reported playhead,
contiguous runway, playback rate, producer startup distribution, and a bounded
server reserve. Segment fetches advance an observed fetch frontier and renew
the lease, but no longer pretend to be the playhead.

- Active demand below the low-water runway starts/resumes production.
- Runway above the high-water target holds production and its stall deadline.
- Hold demand stops advancing the target. The process may be suspended for a
  short hold or terminated after a configured dormant threshold while the
  session lease remains valid; the choice is actor state, not a second reaper.
- A seek immediately changes the target, cancels obsolete uncommitted work,
  and preserves already published media until reader pins release it.
- Production and disk budgets are both hard caps. A bad client report cannot
  cause unbounded lookahead because the server clamps the target to the
  configured maximum and known title duration.

Initial low/high watermarks must be derived from existing production and
startup measurements, then exposed as settings with safe bounds. Do not copy
the current 180-second/2-GiB inferred-ahead limits without measurement; the
protocol provides actual runway for the first time.

## 5. Prepared replacement protocol

### 5.1 Transaction

Every recipe or owner change uses the same phases:

```
 current:  PLAYING ================================ RETIRING ===== X
                              |                    ^
 successor:                   RESERVE -> PRIME -> READY -> COMMIT -> PLAYING
                              ^              |       |
                              |              + ACK --+
                         action_id fences every phase
```

1. **Propose.** The arbiter selects one successor recipe and records the
   evidence, reason, and minimum safe boundary.
2. **Stage.** A prepare-only durable start creates at most one successor for
   the predecessor/action. It does not call today's supersession reap and does
   not advance `media_playback_pointers`. The staged row is invisible to
   ordinary current-session lookup until commit.
3. **Reserve and prime.** Admission is requested without releasing the current
   permit unless a single-slot handoff uses §5.3's weaker contract. The
   successor uses an idempotent `request_id` derived from `action_id`, begins
   at an aligned film-time boundary, and publishes enough media for the target
   platform's measured startup need.
4. **Prepare.** The server returns the complete successor `StartResponse` and
   boundary. The client preloads it without destroying the current player,
   acknowledges `metadata_ready`, then acknowledges `buffer_ready` with the
   successor film time it can actually play without another fetch.
5. **Commit.** The server returns `commit_replacement`. The client switches at
   or just after the boundary, preserves film position and playback intent,
   and acknowledges `committed` with observed first-frame time.
6. **Commit durability.** A single CAS moves `media_playback_pointers` from
   the expected predecessor to the staged incarnation and marks the action
   committed. A lost CAS aborts the staged generation; it never reaps a newer
   player generation.
7. **Retire.** The predecessor stops producing immediately after commit but
   retains published media through reader pins plus a bounded retirement
   grace. Late predecessor controls do not renew it.
8. **Abort.** Any failure before commit tears down only the successor. The
   current stream remains authoritative and playable.

The transaction has exactly one terminal outcome. A disconnect does not imply
commit; after the preparation lease expires, the actor aborts the successor
and keeps the current generation if it is still healthy.

### 5.2 What can be transparent

| Change | Expected handoff | Platform caveat |
|---|---|---|
| Resolution/bitrate, same codec and grade | Boundary switch with no lost film time | May be native variant switch in a later multi-rendition master |
| Audio track/offset, compatible container | Boundary switch | Audible discontinuity must be measured |
| Native text subtitle on/off/track | No video replacement | Publish subtitle readiness before advertising selection |
| PGS overlay on/off | No video replacement | Overlay generation is independently prepared and fenced |
| Burned subtitle change | Prepared video successor | Requires new encode recipe |
| Dolby Vision ↔ HDR10 ↔ SDR | Prepared successor | Decoder/display may visibly reconfigure |
| H.264 ↔ HEVC/AV1 | Prepared successor | Most clients require new decoder/player item |
| Planned node drain | Prepared owner successor | Shared VOD can often keep identical media URLs |
| Hard rolling-owner loss | Best-effort successor from client state | Node-local unpublished bytes are lost |

The product term “transparent” means no manual action, no position loss, no
duplicate recovery, and continued old playback until commit. A measured
display-mode blink is reported honestly rather than hidden under that term.

### 5.3 One encoder slot is not make-before-break

True make-before-break is impossible when the only compatible hardware slot is
occupied by the current stream. The actor first tries, in order: a bounded
admission overcommit proven safe for that hardware, an eligible software
bridge, or staying on the current recipe. Only a severe condition may use a
buffered break-before-make permit transfer, and only when:

- the client reports contiguous runway exceeding measured successor startup
  p95 plus a safety margin;
- the current published media remains readable after its producer stops;
- the successor recipe is known eligible on the target node;
- the old recipe remains restartable at the same film-time boundary if the
  successor fails.

The actor stops the old producer, releases its permit, primes the successor
while the client consumes retained buffer, and commits only after readiness.
Failure aborts the successor and attempts an idempotent restart of the old
recipe; the finite retained buffer is not described as a running predecessor.
If the runway condition is not met, stay on the current rung unless the
current stream is already terminal. This action is logged and measured as
`buffered_break_before_make`; its acceptance is bounded interruption and
successful old-recipe restart, not seamless handoff.

### 5.4 Client preparation feasibility gate

“Ready” has three platform-independent meanings:

- `metadata_ready`: successor manifest/item parsed and decoder eligibility
  accepted;
- `buffer_ready`: successor has a reported contiguous film-time range through
  at least the proposed switch boundary plus its measured safety runway;
- `first_frame_ready`: observed only after commit, with wall latency and film
  position.

Asset metadata or an AVPlayer/Media3 ready state alone is not `buffer_ready`.
Before M6 freezes or enables prepared actions, a physical spike must measure:

| Platform | Dual preparation proof | Required fallback |
|---|---|---|
| Web/hls.js | Second detached media pipeline can buffer without destroying current MSE/video or exceeding memory/decoder limits | Server-prime then measured single-player replace |
| AVPlayer | Successor item/asset can acquire playable buffer while current item renders, including tvOS/iOS resource limits | `AVQueuePlayer`/single-item replace path with honest interruption |
| Media3 | Second player/preload manager can buffer on target Android TV/phone hardware without decoder starvation | Single-player media-item replace with honest interruption |

The spike records memory, decoder allocation, network duplication, old-stream
continuity, buffered-through truth, switch position error, and first-frame
latency for same-codec and codec/grade changes. Unsupported dual preparation
does not block the protocol; it selects the platform's measured single-player
fallback and prevents the UI from calling that path seamless.

## 6. Adaptive delivery and other protocol consumers

### 6.1 Automatic quality and output grade

Auto policy evaluates a joined snapshot rather than one throughput number:

| Evidence | Network pressure | Producer pressure | Decoder/display pressure |
|---|---:|---:|---:|
| Server delivered bitrate falls | strong | possible | no |
| Client observed bitrate falls | strong | no | no |
| Producer `recent_speed < 1.0` | no | strong | no |
| Buffer runway falls | result | result | possible |
| Server delivery idle while client waits | possible | possible | no |
| Dropped frames/decoder failure | no | no | strong |
| Server output healthy while render stalls | weak | no | strong |

The controller projects time-to-empty from runway trend and supply rate. A
severe downshift can skip multiple rungs to the highest sustainable recipe;
a mild downshift moves one rung after consecutive evidence. Upgrades require
longer stable evidence, no recent stall, adequate margin for the next rung,
and no transition in flight. Manual height, codec, or dynamic-range choices
are sticky unless the exact choice becomes impossible and the server returns a
typed refusal.

Quality, codec, and grade are one recipe decision. If a 4K Dolby Vision stream
is decode-bound, the arbiter may choose 1080p HDR10 or SDR based on device
capabilities and measured producer eligibility. It does not first downshift,
then independently tone-map, then independently move nodes.

The existing [ADAPTIVE-QUALITY.md](ADAPTIVE-QUALITY.md) thresholds are the
starting evidence, not permanent wire constants. Its destroy-before-ready
switch is superseded by §5. Its manual-choice, one-move-in-flight, asymmetric
down/up, cooldown, and bandwidth-estimate preservation rules remain.

### 6.2 Subtitle generation and selection

The protocol improves subtitles in four different ways:

- **Native text:** the client reports the selected track and forward window;
  the server materializes WebVTT ahead of demand and reports
  `subtitle_ready_through_ms`. A playlist never advertises a cue interval that
  cannot be served.
- **PGS overlay:** selection starts bounded extraction/rendering for upcoming
  cues. A seek cancels obsolete work by sequence/generation. Overlay object
  hashes remain immutable; control only coordinates readiness.
- **Burned subtitles:** selection becomes a successor recipe and joins any
  pending quality, codec, grade, or node change into one replacement.
- **Deselection:** stops future subtitle work without disturbing video
  delivery. It cannot kill a video producer.

Subtitle readiness may propose a hold or replacement, but subtitle code never
owns a playback watchdog.

### 6.3 Audio, seeks, pause, and retention

- Rapid scrubs are coalesced by control sequence. Only the latest seek target
  may reposition a rolling producer or create a successor.
- Native-compatible audio changes prepare the next boundary; incompatible
  changes join a successor recipe.
- Pause is explicit `hold`: retain the lease, stop target advancement, and
  release expensive production after the dormant threshold without pretending
  the viewer abandoned playback.
- Retention uses the client's actual contiguous range plus open reader pins,
  retry allowance, and a hard disk budget. It no longer assumes every AVPlayer
  fetch is 120 seconds ahead of the playhead.

### 6.4 Admission, cache, progress, and diagnostics

- Admission ranks waiting sessions by projected time-to-empty and whether a
  client is actively waiting, while enforcing user/fairness limits.
- Cache warming can use the effective recipe and next likely boundary, but it
  remains background work and never renews a playback lease.
- Watch-progress/scrobble events carry delivery generation and control
  sequence so a retired player's late callback cannot move position backward.
- UI and logs use shared typed states: `preparing`, `network constrained`,
  `producer constrained`, `decoder constrained`, `waiting for subtitles`,
  `switching quality`, `moving node`, and `recovering owner`.

### 6.5 Content-analysis index and exact skip markers

“The index” is one operator-visible content-analysis index with three storage
components, not one Rust struct:

| Component | Scope | Contents |
|---|---|---|
| structural fragment index | node-local, pipeline-versioned | Existing packed video-only fragment rows used for VOD planning/landing |
| timeline annotation set | replicated, source-versioned | Authored/manual/detected intro, recap, credits, and preview boundaries |
| detector feature sidecar | node-local, detector-versioned | Optional bounded audio/video fingerprints or sampled visual features |

This split is required by the existing store. `FragmentIndex` is serialized
into a packed node-local sidecar that contains structural rows; adding a Serde
field would not make it replicated and would invalidate structural indexes on
every detector change. The item and Activity UI nevertheless present the
three components as one analysis generation with explicit per-node coverage.

```rust
pub struct TimelineAnnotation {
    pub kind: AnnotationKind,       // intro | recap | credits | preview
    pub start_ticks: i64,
    pub end_ticks: i64,
    pub timescale: u32,
    pub start_ms: i64,              // normalized convenience value
    pub end_ms: i64,
    pub provenance: AnnotationProvenance,
    pub confidence_millis: u16,     // 0..=1000, not a float
    pub detector_version: String,
    pub manual_override_revision: Option<u64>,
}

pub struct TimelineAnnotationSet {
    pub source_identity: SourceIdentity,
    pub generation_id: String,
    pub version: u32,
    pub annotations: Vec<TimelineAnnotation>,
}
```

Ticks are the authority and use the file timeline. Milliseconds are stored so
clients do not each invent rounding. `start < end <= duration` is enforced,
overlaps of the same kind are normalized, and a marker is keyed by the
current source identity plus detector/manual revision. The seek target is the
marker's exact end time. A rolling producer may start at the clean boundary
before that time, but the player still seeks forward to the exact film time
after attach.

Detection is evidence-ranked:

1. Authored chapters with recognized intro/recap/credits titles provide exact
   authored boundaries.
2. Recurring audio/video fingerprints across episodes may identify shared
   opening and ending sequences. A second boundary-refinement pass must locate
   the frame/audio transition rather than using the coarse fingerprint window.
3. Credits-specific visual/text and audio transition evidence may identify a
   non-recurring film tail after a detector passes its evaluation gate.
4. Duration-only estimates remain compatibility hints with low confidence;
   they are never labeled exact and are not auto-skipped by default.
5. An administrator's manual boundary is `manual` provenance, highest
   confidence, and survives ordinary re-analysis. A separately confirmed
   “discard manual override” action is required before automation can replace
   it.

Automatic analysis can be wrong; precision in the stored timestamp is not
proof that the semantic classification is correct. Clients auto-skip only
authored, manual, or detector results above the configured confidence floor.
Lower-confidence markers may still show a clearly labeled manual button.

Authored chapter persistence is immediately buildable and requires no new
media analysis. Experimental detection is a separate bounded feature pass:
the current fragment indexer maps video only and does not decode audio, frames,
or OCR text. A feature job may share queue/admission scheduling and source I/O
budgets, but it must not be described as free output from the existing pipe.
A series/season correlator compares the small feature sidecars and publishes
annotations without rereading every episode when a new episode arrives.

Detector rollout is opt-in until a labeled fixture corpus and real-series
sample meet recorded precision/recall gates, with false-positive rate weighted
more heavily because a wrong automatic skip is worse than no skip. The exact
thresholds are set from that evaluation, not invented in this document.

The existing `DecisionResponse.markers` wire stays additive: `kind`,
`start_ms`, `end_ms`, `label`, and the compatibility `chapter` field remain.
New provenance, confidence, generation, and detector-version fields are
optional until web, Apple, and Android have migrated.

The decision/start payload returns the persisted annotations. The control
actor also consumes them: when the playhead approaches a marker, or automatic
skip is enabled, it raises the marker end as a likely demand boundary. VOD can
materialize that segment and its selected subtitle window; rolling delivery
can prepare/reposition without waiting for the click. A skipped destination
is a hint, not lease renewal and not permission to seek the client.

### 6.6 Analysis queue, force control, and instrumentation

Replace the current cursor-only background walk with an explicit,
lease-claimed analysis queue. The current `POST /items/{id}/reanalyze` repairs
ffprobe facts synchronously; it is not an index queue and must not be silently
redefined. Add admin-only endpoints with exact scopes:

```text
POST /api/v1/files/{file}/analysis
     { "force": false, "components": ["fragment_index", "skip_markers"] }
GET  /api/v1/analysis/jobs?state=queued,running,failed
GET  /api/v1/analysis/jobs/{job}
POST /api/v1/analysis/jobs/{job}/retry
DELETE /api/v1/analysis/jobs/{job}
```

The replicated work contract uses four records:

```text
analysis_jobs
  job_id · file_id · source_identity · component · target_node_id?
  pipeline_or_detector_version · requested_generation · priority · trigger
  state · attempt · not_before_ms · cancel_requested · created/updated_ms

analysis_attempts
  job_id · attempt · claim_node_id · claim_epoch · claim_expires_at_ms
  phase · started_at_ms · phase_updated_at_ms · terminal_code?

analysis_artifacts
  file_id · source_identity · component · generation · version
  payload_or_digest · state(staged|published|rejected) · published_at_ms

analysis_node_coverage
  file_id · source_identity · pipeline_version · node_id
  fragment_generation · state · verified_at_ms
```

Work identities are different on purpose:

- a structural/feature job is unique on
  `(file_id, source_identity, component, pipeline_version, target_node_id)` so
  every eligible media node can build its own node-local artifact;
- semantic correlation is unique on
  `(file_id, source_identity, detector_version, requested_generation)` and is
  cluster-owned once;
- a non-force duplicate joins the active identity and returns its `job_id`;
  force allocates a new requested generation but still permits only one active
  forced successor per component/file.

Legal durable state transitions are:

```text
 queued -> claimed -> running -> staged -> published
    |         |          |          |
    |         +-------> retry_wait <-+
    |                    |
    +-------> canceled <-+----------> failed

 any nonterminal state -- source identity changed --> stale
```

Claim is a CAS from `queued|retry_wait` with `not_before <= now`, incrementing
`attempt` and `claim_epoch`. Renew, phase summary, stage, publish, fail, and
cancel all require the exact `(job_id, attempt, claim_node_id, claim_epoch)`.
An expired claim returns to `retry_wait`; its old worker cannot stage or
publish. Cancellation sets `cancel_requested` durably, then the owner
cooperatively stops at a safe boundary and fences its partial generation. A
queued job cancels immediately. A published artifact is never deleted by job
cancellation.

Retry policy is typed. Transient I/O, preemption, and lease loss use capped
exponential backoff with jitter and an operator-visible next attempt. Source
unsupported and deterministic validation failure are terminal until source or
pipeline/detector version changes, or an admin explicitly retries. The first
implementation must choose and test numeric attempt/backoff ceilings before
enablement; they are settings with safe maximums, not wire constants.

Artifact publication is compare-and-swap on current source identity and
expected predecessor generation. Bytes/payload are fully written, hashed, and
verified in `staged` state before the replicated pointer changes to
`published`. A stale or failed successor leaves the previous published
generation current.

High-frequency byte/media-time progress is node-local in the worker's bounded
activity registry. The existing peer Activity snapshot supplies it to an
ingress. Only claim renewal, throttled phase summaries, and terminal/publication
changes replicate; this keeps per-fragment progress out of Raft.

The item detail page gets **Analyze now** when any current component is absent
or failed and **Rebuild analysis** when it is current. For an item with
multiple files the UI lists each file and selected components before queueing.
The existing admin Activity page gets an **Analysis** section showing:

- queued, claimed, running, retry-wait, failed, canceled, stale, and ready;
- item/file title, file identity, requested components, priority, trigger,
  owner node, attempt, and claim lease;
- current stage (`probing`, `fragment_index`, `fingerprints`,
  `marker_correlation`, `persisting`, or `verifying`);
- bytes read/total, media time examined/total, fragments indexed, throughput,
  elapsed time, and bounded ETA when calculable;
- the last typed failure, next retry time, produced index/detector versions,
  and whether a current older artifact remains serving;
- force, retry, cancel, and open-title actions with admin authorization.

`force:true` always creates a new analysis generation. It does not delete the
currently serving index first. The old generation remains readable until the
new index and annotations validate and publish atomically; a failed rebuild
therefore cannot turn a playable title into an outage. Duplicate requests for
the same source identity/components join one job unless force explicitly asks
for a new generation.

Foreground VOD prerequisite demand and an administrator force request outrank
recently requested and ordinary background indexing, but all use bounded
fairness and the existing foreground-production preemption rule. In a cluster,
semantic annotations replicate after validation. Fragment bytes and
pipe-specific indexes remain node-local and publish the explicit coverage row
for the node/pipeline version that produced them.

Required analysis telemetry:

- queue depth and oldest age by state, component, priority, and trigger;
- claims, lease loss, retries, cancellations, stale identity, and terminal
  failures by typed reason;
- indexing bytes, media seconds, wall seconds, and throughput histograms;
- current jobs by stage and owner node;
- artifact publication and replacement by index/detector version;
- marker counts by kind, provenance, and confidence bucket;
- per-series correlation candidates, accepted matches, and rejected ambiguous
  matches;
- playback marker offers, manual skips, automatic skips, undo/seek-back, and
  destination-prewarm hit rate.

File paths, media titles, raw fingerprints, and user identities must not be
metric labels. The Activity API may return authorized display names; logs use
file IDs and bounded reasons. A stuck-job alert keys off expired claim lease
and no progress, not an independent task that mutates the job.

## 7. Deadlines and watchdog inventory

The goal is not “zero timers.” Networks, processes, and leases require bounded
failure detection. The goal is exactly named ownership and no overlapping
recovery decisions.

### 7.1 The only playback progress watchdogs that remain

| Name | Owner | Armed only when | Action | Why it must exist |
|---|---|---|---|---|
| `ProducerProgressDeadline` | Server session actor | Demand requires output, producer is running, and it is not deliberately held | Before first publication, retry one validated startup recipe; after publication, fail/propose successor, never replace child in place | A wedged or dead child cannot report its own failure |
| `PlaybackProgressDeadline` | One client controller | Demand is active, app is foreground/eligible, and rendered position is not advancing | Send one `stalled` observation; arbiter chooses recovery | Only the playback framework knows whether decoded media is rendering |
| `SegmentMaterializationDeadline` | VOD materializer actor | A planned immutable segment is demanded but absent and eligible production is assigned | Return typed pending/failure and update producer evidence | Immutable playlists can name bytes that do not exist yet; an HTTP request cannot wait forever |

Each has one owner, explicit arm/disarm conditions, and no independent reopen.
`PlaybackProgressDeadline` reports a fact; it does not destroy or recreate the
player.

### 7.2 Necessary lifecycle timers that are not watchdogs

| Timer | Purpose | Mutation authority |
|---|---|---|
| `PlaybackSessionLease` | Reclaim a session after both control and media renewal stop | Session actor ends once |
| cluster owner lease | Detect/fence a dead media owner | Existing replicated CAS/owner epoch |
| replacement preparation lease | Abort an unacknowledged successor | Session actor aborts successor only |
| retirement grace | Let in-flight predecessor reads finish | Session actor removes after pins/grace |
| HTTP/relay deadline | Bound one network request | Handler returns typed error; no recovery mutation |
| admission wait deadline | Bound one capacity attempt | Actor retains current stream or reports pending |
| control rate window | Bound malformed/chatty clients | Handler throttles; media remains independent |

### 7.3 Current mechanisms and disposition

The rewrite must delete or absorb these current live-engine mechanisms. Source
symbols are the acceptance handle; line numbers will move.

| Current mechanism | Current behavior | Disposition |
|---|---|---|
| `SESSION_IDLE_SECS` + `reap_loop` | Reaps from inferred `last_request` after 60 s | Replace with `PlaybackSessionLease`; valid control and media renew it |
| `FIRST_SEGMENT_GRACE` | Starts hardware fallback when first output is late | Fold into pre-publication `ProducerProgressDeadline` |
| `SOFTWARE_GRACE` | Gives the fallback a second startup window | Same deadline with one explicit retry phase |
| `PROGRESS_STALL` + `WATCHDOG_POLL` | Polls producer timestamp and replaces/kills | Replace with actor-owned deadline reset by progress events |
| `PLAYLIST_WAIT_BUDGET` + polling | Holds playlist across nested fallback budgets | Use published actor state plus an ordinary bounded HTTP deadline |
| `SEGMENT_WAIT` | Waits for named live media | Rolling actor waiter; VOD uses `SegmentMaterializationDeadline` |
| `child_transition` | Serializes kill/replace/terminal races | Delete; actor owns child transitions |
| `watchdog_active` | Elects one detached watcher | Delete; actor owns one deadline |
| `replacing_child` | Hides intentional non-zero exits | Delete; actor state names `Replacing` |
| `retired`/failure atomics | Fence detached tasks after removal | Replace with actor lifecycle enum and generation fencing |
| `last_request` request-kind clock | Infers viewer life | Replace with lease renewals and explicit demand |
| client stall reopen owners | Several call sites recreate sessions | Collapse into one client deadline. The budgets and queues themselves are bounds, not owners — see §7.4 and server action transaction |
| ahead-window flow-control polling | Infers buffer from fetched frontier | Use reported runway, hard caps, and actor target |

No production module may contain a detached `sleep -> inspect -> recover`
loop for a playback generation after the actor migration. A repository check
should enumerate approved deadline constructors and fail on new unowned
watchdog patterns in playback modules.

### 7.4 Client watchdog-by-watchdog disposition

This is the build-time deletion ledger. Source symbols must be re-verified
before each client milestone; any newly found automatic recovery owner is
added here before code changes.

**Re-verified on `main` at `bc504681`, 2026-08-31.** Every symbol below still
exists; none has been deleted. That pass changed ten classifications and added
five owners the ledger had never named — the recon is in
[M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md)
§4. The corrections matter more than the count: **a row that names a bound, a
detector or a serializer as an automatic owner sends a milestone to delete the
wrong thing**, and a row that omits a real owner leaves a recovery nobody asked
the server about.

#### The classification rule

The `automatic?` column decides work, so it needs one rule applied the same way
everywhere:

> **A symbol is automatic when it is the one that mutates delivery** —
> reopens, seeks, changes the recipe, reattaches to another node. A symbol
> that computes a verdict and returns it, counts attempts, or serializes
> requests is *evidence* or a *bound*, and its caller is the owner.

Detectors are therefore uniformly **no**, and the call site that acts on the
detector's verdict is the row that carries the work. This is a change from the
previous ledger, which classified three structurally identical Apple detectors
three different ways. The practical consequence: **name the owner, ask the
server there, and leave the detector alone** — a detector usually holds
evidence the server cannot derive.

| Apple symbol | automatic? | Present mutation | Final disposition |
|---|---|---|---|
| `statusTask` / `startStatusPolling` (`:3016`) | **yes** (changed) | `:3039` calls `observeDeliveryStarvation`, which spawns `retrySameDeliveryAfterStall` — it does own a recovery | Delete `:3039` and its detector. The rest feeds the debug panel and survives to M9 |
| `DeliveryStarvationDetector` (`:741`) | **no** (changed) | Server-truth wedge evidence: returns a verdict, touches no player | Retain as evidence; submit as observation |
| `PlaybackStallDetector` (`:997`) | **no** (changed) | Returns `.nudge` / `.reopen`; touches no player | Retain until the arbiter replaces it, then delete with its tests in one PR |
| `BlackFrameWatchdog` (`:860`) | **no** (changed) | Returns `Bool` from the periodic time observer | Retain: it is the only Profile-5 evidence the server cannot derive |
| `PlaybackRecoveryMonitor` (`:1131`) | yes | Acts on `PlaybackStallDetector`: nudges Play, then reopens | Becomes the one `PlaybackProgressDeadline`. **The `.nudge` arm stays** — it plays already-wanted playback and mutates no delivery |
| `handleBlackFrameDecodeFailure` (`:4384`) | **yes** (added) | Acts on `BlackFrameWatchdog`: walks the compatibility ladder. The most invasive mutation on the platform | Route its verdict through the action ask |
| `retrySameDeliveryAfterStall` (`:3156`) | **yes** (added) | The single funnel every stall owner reaches. Reopens or stops | The M5 ask goes here, **before its first statement** — `:3157` already spends the one same-delivery attempt |
| `SameDeliveryStallRecoveryState` (`:702`) | **no** (changed) | A one-shot flag. `next()` spends it; the caller reopens | Delete after action cutover; the server action transaction is the attempt state |
| `StallReopenBudget` (`:512`) | **no** (changed) | A bound — **and the owner of `userActionSequence`**, the viewer-intent fencing `ControllerStallGuard`'s Apple counterpart delegates to | Delete the bound after cutover; **the fencing sequence is retained and must be moved out first** |
| `RecoveryReopenBudget` (`:810`) | **no** (changed) | A storm bound. `admit()` mutates the budget, never the player | Delete after action cutover |
| `PlayerReopenQueue` (`:438`) | **no** (changed) | Last-writer-wins serializer shared by viewer seeks and stall reopens, split only by `PlayerOpenIntent` | **Delete nothing.** Its automatic half disappears when its callers stop calling `reopen` |
| early-end handler (`observeEnd` `:3512`) | yes | Reopens up to `RecoveryReopenBudget`, then stops | Send typed ended observation; the arbiter chooses one action |
| `handleItemFailure` (`:3598`) | yes | Three-rung ladder: next node (`:3616`), established HDR, compatibility fallback | Send typed failed observation. The ask goes **before rung one** — a source verdict does not change by node |
| `retryEstablishedHDRDelivery` (`:4298`) | yes (added) | Reopens once, then stops with a viewer-visible verdict | Invoked only by a prepared server action |
| `retryWithNextCompatibilityFallback` (`:4323`) | yes (added) | Forces `forceCompatibleHDRBase` / `forceCompatibilityTranscode`, then reopens — changes the recipe without asking | Invoked only by a prepared server action |
| `retryMediaOnNextNode` (`:3684`) | yes (added) | Reattaches through another ingress, bumps `openGeneration`, calls `play()` | Becomes the client half of M8's node-replacement action |
| `awaitItemReady` / `retryAfterReadinessTimeout` (`:4432`, `:4366`) | yes (added) | A 15-second readiness watchdog feeding the same ladder | Folds into the one `PlaybackProgressDeadline` |

| Web symbol | automatic? | Present mutation | Final disposition |
|---|---|---|---|
| `waitTimer` / `persistentWait` (`:3378`, `:3424`) | yes | Times persistent media wait, then reopens or falls back a rung | Becomes the one `PlaybackProgressDeadline`; report observation only |
| `stallTimer` / `armStall` (`:8336`) | **no** (changed) | Arms `stallDiagnose` and nothing else | **Retain.** A timer whose only payload is a viewer prompt is not an automatic owner |
| `stallDiagnose` (`:8350`) | **no** (changed) | Diagnoses and shows *Try again / Force transcode / Close* | **Retain.** Its verdicts are the only thing that tells a viewer an ad-blocker is eating `/hls/*.ts`; a server action can never know that |
| `pollSessionHealth` (`:10126`) | no | Reads `/status` | Replace with the control reporter; M9 deletes it |
| hls.js fatal-error handlers | yes | Destroy/recreate or compatibility-transcode | Normalize the error as a proposal; only the controller applies the returned action |
| Auto ABR sample/controller | yes | Opens a replacement session | Retain as an action proposal using joined control facts; no direct create |
| automatic fallback claim/budgets | no | Prevent recovery collisions | Delete once the server arbiter and action ID are authoritative |
| `handleEnded` / `endedTries` (`:8925`) | yes | Treats an early media end as failure and reopens up to its budget | Send a failed observation now; after cutover only the controller applies the successor action |

| Android symbol | automatic? | Present mutation | Final disposition |
|---|---|---|---|
| `BufferingStallTracker` (`PlaybackTelemetry.kt:263`) | **no** (changed) | A post-hoc reporter. It returns a measurement only once the playhead moves again, and `null` while the playhead is stuck | Retain as evidence |
| the one-second loop → `onStall` (`Controller.kt:497`, `:783`) | **yes** (added) | Creates the successor session through `SessionCreateCoordinator.reopenAfterStall`. **This is the row the table should always have carried** | The M5 ask goes here. Note it cannot fire during an open-ended freeze — see §4.3 of the M5 handoff |
| `startStatusPolling` / `statusPollingJob` (`:1052`) | **no** (changed) | A pure read; `sessionStatus` has no reader outside the details panel | Free deletion whenever the panel is re-sourced — this row has no recovery to take |
| `ControllerStallGuard` (`StallReopen.kt:77`) | **no** (changed) | Mints and compares request versions; delegates its reset to `StallReopenBudget` | Retain as viewer-intent fencing |
| `StallReopenBudget` (`StallReopen.kt:10`) | **no** (changed) | A counter — **and the owner of `userActionSequence`**, which `ControllerStallGuard` delegates to | Delete the counting half after cutover. **`resetForUserAction` is retained fencing and must move first**; `StallBudgetTest.kt` also covers two retained symbols and cannot be deleted with it |
| `SessionCreateCoordinator.reopenAfterStall` (`StallReopen.kt:142`) | yes | Creates the lower/same successor directly, and retries once unbound on a 400 | Invoked only by a prepared server action |
| `onPlayerError` (`Controller.kt:391`) | yes | Node failover (`:398`), then a three-rung ladder via `playbackErrorAction` | Send typed failed observation. The ask goes **before `:398`** |
| `retryMediaOnNextNode` (`Controller.kt:1089`) | yes (added) | Reattaches to the next ingress: `setMediaItem`, `prepare`, `playWhenReady` | Becomes the client half of M8's node-replacement action |
| Media3 end callback (`PlayerScreen.kt:733`) | **no** (changed) | Posts progress and autoplays the next episode or audiobook part | **Nothing for M5 to take.** Android has no early-end reopen — no analogue of web's `endedTries` or Apple's `endAction` |

PGS cue-boundary jobs, UI-hide timers, seek debounce, progress reporting, and
subtitle retry scheduling remain ordinary scheduling timers. They do not infer
playback failure or mutate video delivery and therefore are not watchdogs.
The repository allowlist names their owner and purpose so a future recovery
mutation cannot hide under that classification.

## 8. Playback lease semantics

### 8.1 Renewal

A local generation's playback lease is renewed by either:

- a valid, newly accepted higher-sequence control exchange for the active
  generation and control epoch; or
- a successful playlist, media, subtitle, or init-object request that proves
  the capability is still being consumed.

Diagnostic `GET /status` remains read-only and is deprecated after client
cutover. Equal-sequence replays, invalid, stale, old-epoch, rate-limited, and
predecessor control messages do not renew. Cache warmers and cluster probes do
not renew.

M1 advertises the lifetime the selected engine actually enforces: currently 60
seconds for rolling recovery and 300 seconds for VOD. It does not claim the
new 30-second lease before the actor owns it. M3 may advertise a 2-second
exchange recommendation and 30-second active lease only after measured timer
jitter on all three clients validates those provisional values. Media renewal
prevents a timer-delayed but actively fetching player from being killed.

Foreground pause communicates `hold` and renews normally, so it can remain
paused indefinitely without producing. OS suspension is different: no
protocol can prove liveness while the app sends neither control nor media. A
background callback may request a bounded dormant hold that releases the
encoder and retains only restartable session metadata. Its duration is a
measured setting. If the OS prevents even that callback or outlives the hold,
the server expires safely and the client resumes by VOD resurrection or one
position-preserving reopen. The success claim is therefore scoped to
heartbeats that are physically delivered, not to an indefinitely suspended
process.

### 8.2 State transitions

```text
 Starting -> Active -> Holding -> Active
     |          |          |
     |          +------> Replacing -> Retiring -> Ended
     |                     |  abort
     +-------> Failed <-----+-------> Active

 Active/Holding -- lease expiry --> Retiring --> Ended
```

`end` and `DELETE /hls/{session}` enter the same idempotent retirement path.
The actor stops production, releases permits, refuses new control mutation,
keeps existing object reads pinned, and removes scratch state after the grace.

## 9. Client integration

### 9.1 Shared client-controller contract

Web, Apple, and Android each implement one `PlaybackControlController` beside
the player framework. It owns sequence allocation, exchange scheduling,
playback-progress deadline, current action, successor preparation, and action
acknowledgement. Existing specialized detectors may feed observations during
migration, but they lose authority to reopen.

The exchange is sent:

- on the server's scheduled cadence while active or holding;
- immediately on play/pause, seek start/settle, foreground/background,
  selection change, render stall/failure, and action acknowledgement;
- with coalescing so many player callbacks produce one newest snapshot;
- through alternate configured ingress after a transport failure.

Only one exchange is in flight. A newer local snapshot replaces queued older
state and receives the next sequence. Cancellation is not converted into a
failure or recovery action.

### 9.2 Platform observation mapping

| Protocol fact | Web | Apple | Android |
|---|---|---|---|
| film position | `video.currentTime` | `AVPlayer.currentTime()` plus VOD/live origin mapping | `Player.currentPosition` plus origin mapping |
| contiguous buffer | `TimeRanges` containing playhead | `loadedTimeRanges` containing playhead | buffered position/ranges from Media3 |
| render state | media events + hls.js fatal class | time control status, item status, access log/error log | playback state, `isPlaying`, player error |
| observed bitrate | hls.js estimator | optional access-log rate | optional analytics estimate |
| dropped/decode evidence | playback quality/frame callbacks where available | access/error log summaries | decoder counters/analytics |
| selected output | quality/menu/player model | controller selections | controller selections |

Unknown platform facts are omitted. The server never treats omission as zero.

### 9.3 Migration order

1. Add server protocol and advertise it behind
   `playback.control_protocol_v1`, default off.
2. Add passive client reporters that continue existing recovery behavior.
3. Compare protocol observations with legacy inference in telemetry.
4. Enable lease renewal and demand-driven hold, still leaving old client
   recovery authority in shadow mode.
5. Move each client to one action owner and disable its independent reopens.
6. Enable prepared quality/track replacements.
7. Delete `/status` polling and legacy live watchdogs only after mixed-version
   fleet windows expire.

Server rollback turns off advertisement. New clients then use the retained
legacy behavior; they never send control to an unadvertised endpoint.

## 10. Cluster integration and redundancy

### 10.1 Routing and fencing

Any ingress accepts control, resolves `media_sessions` by `session_id`, and
either handles locally or relays the bounded request to the current owner.
Control is **not** added to the existing read-oriented `RelayResource` enum.
It gets a separate internal mutating endpoint and envelope:

```text
ControlRelayRequest
  session_id · incarnation_id · expected_owner_node_id · expected_owner_epoch
  validated_control_body
```

The peer route uses exact write authorization, not the read authorization used
by playlist/status relay. The ingress builds the envelope only from the
durable route, never client-supplied owner fields. The owner re-reads or
otherwise authoritatively fences the route and proves that its local worker
still matches `(incarnation_id, owner_node_id, owner_epoch)` before any
mutation or renewal. Body and response have dedicated bounds and deadlines.

Ingress resolution is an explicit state machine before relay:

```text
active local owner -> handle with exact epoch
active remote owner -> mutating relay with exact epoch
takeover settling   -> 425 owner_transition + retry hint
expired claimable   -> contest/observe takeover, then 425
terminal/missing    -> 410/404
```

The public handler no longer applies `relay_if_remote`'s current blanket
expired-route `GONE` behavior to control. A stale control epoch is rejected
even if takeover has already installed a healthy owner.

Control samples stay node-local. Owner-lease renewal persists only the current
coarse produced/fetched frontier already needed for takeover. Persist recipe,
session/incarnation identity, owner epoch, active replacement transaction,
and terminal state at low frequency or phase changes. Do not persist every
playhead or heartbeat.

### 10.2 Planned drain

A draining node proposes a node-replacement action. The cluster placer reserves
the target; VOD attaches the same immutable recipe/store where possible;
rolling production primes a new delivery generation at a future film boundary.
The client acknowledges readiness, commits, and the old owner retires. Owner
epoch/CAS prevents the drained node from acting after handoff.

### 10.3 Hard owner loss

The surviving ingress has only the absolute snapshot in the request currently
arriving; earlier samples were relayed to the former owner and are not assumed
durable. It combines that current snapshot with the durable recipe/route and
acquires ownership with the existing expired lease CAS:

- **VOD:** resurrect the immutable handle or redirect the client to an
  equivalent successor; requested segment identity is film-addressed.
- **Rolling fallback:** do not splice an unrelated producer into an EVENT URL
  unless exact playlist/timeline continuity is proven. Prefer a successor
  session at an aligned film-time boundary and return it through the current
  control exchange. The client consumes retained buffer while it prepares.
- **No control snapshot:** fall back to persisted watch position and fetched
  frontier, then require a normal reopen. Do not guess transparency.

The current typeless-sliding same-session takeover remains a compatibility
path until successor-generation failover is complete. It must not be expanded
to VOD or EVENT sessions.

### 10.4 Split-brain rule

Only the owner with the current replicated `owner_epoch` may issue a mutating
action or produce new bytes. A stale owner may serve already-open immutable
read handles during retirement grace, but it cannot renew playback, accept an
acknowledgement, advance a replacement, or publish media. Every action log
includes hashed session identity, incarnation, and owner epoch.

## 11. Persistence and observability

### 11.1 Durable changes

Add replacement staging before M6, not in the later cluster-handoff milestone.
Today's create path reaps the predecessor and advances the playback pointer,
so it cannot be reused for preparation. The durable model records:

```text
media_replacement_actions
  action_id PRIMARY KEY
  user_id · playback_id
  predecessor_incarnation · successor_incarnation
  expected_pointer_incarnation · reason · phase
  switch_at_ms · created_at_ms · expires_at_ms · updated_at_ms

UNIQUE active successor per (user_id, playback_id)
```

Prepare-only activation writes the staged successor without reaping or
changing `media_playback_pointers`. Phase transitions require the exact action
and predecessor/successor pair. Commit CASes the current pointer from
`expected_pointer_incarnation` to the staged successor once, marks the action
committed, then permits predecessor retirement. Abort marks the staged route
ended and leaves the pointer unchanged. An expired preparation is aborted by
the same fenced owner/maintenance path. Writes occur on phase changes, not
heartbeats.

Do not add the last client sequence to replicated storage in v1. It is scoped
to `(incarnation_id, owner_epoch, client_instance_id)`. After owner loss, the
new owner increments `owner_epoch`; every old-epoch request is rejected, and
the client starts a freshly snapshotted sequence under the new epoch. This
prevents a captured pre-takeover mutation from becoming fresh state.

### 11.2 Metrics and events

Required counters/histograms:

- control exchanges by result and client platform;
- playback lease renewals/expirations by source;
- actor state transitions and hold reasons;
- producer deadline arms/fires/retries/outcomes;
- client stall observations and joined server classification;
- replacement proposals, prepares, commits, aborts, and phase latency;
- switch interruption and first-frame latency reported by clients;
- automatic moves by cause/rung/grade;
- cluster control relays, owner transitions, and stale-owner rejections;
- subtitle ready-window deficit;
- produced-ahead versus reported-runway distributions;
- analysis queue depth/age, index throughput, current stage, failures, marker
  provenance/confidence, and skip-destination prewarm effectiveness (§6.6).

Raw session UUIDs remain bearer capabilities and must use `session_log_id` in
logs. Client observations have bounded low-cardinality enums. Position,
bitrate, and runway are histogram values, never metric labels.

## 12. Security and abuse boundaries

- Keep capability authentication consistent with HLS resources. Never put the
  raw session ID in logs, metrics, successor reasons, or child arguments.
- Enforce body size before allocation and validate every numeric value for
  finiteness, sign, duration bounds, and maximum rate.
- Cap control frequency per session and user. Coalesce honest bursts; reject
  abusive ones without starving media serving.
- A control request cannot name an arbitrary file, user, playback ID, node, or
  URL. It acts only on the path capability's stored session.
- Successor recipes are server-normalized from stored file metadata and
  proven capabilities. Client-selected codec/grade fields are preferences,
  not ffmpeg arguments.
- Relay only to a node listed by durable ownership; never follow a client or
  peer-supplied redirect.
- Bound cached idempotent responses and completed action IDs by the session
  lifetime.

## 13. Implementation milestones

Each milestone is independently reviewable and feature-gated. The first
implementation PR after this plan is M1.

### 13.1 M1 — typed no-op control plane

- Add wire types, strict validation, `ControlBootstrap`, and the capability
  route.
- Add real per-session `ControlState` for VOD and rolling: generation,
  control epoch, bound client instance, last sequence, and prior action
  outcome.
- Add the separate bounded, write-authorized, owner-epoch-fenced cluster
  control relay; do not reuse the read relay authorization.
- Add a manager method that returns an observation snapshot and renews the
  existing activity clock with the explicit `control` reason only for a newly
  accepted sequence.
- Return `action: none`; do not change production, reaping, or client recovery.
- Add protocol/route contract tests and documentation examples.
- Gate advertisement with `playback.control_protocol_v1`, default off.

**Acceptance:** focused protocol/route tests pass; with the setting off a start
response is byte-compatible except normal JSON field ordering; with it on a
valid monotonically sequenced exchange returns the matching generation and an
`active` lease view reporting the actual legacy lifetime (rolling 60 seconds,
VOD 300 seconds); equal, stale, old-epoch, and malformed controls never touch
activity; takeover rejects the former control epoch before mutation.

### 13.2 M2 — passive reporters on all clients

- Implement one coalescing client controller per platform.
- Send actual intent, film position, contiguous runway, render state,
  selection, and available throughput evidence.
- Keep existing watchdog/reopen behavior but mark every legacy recovery event
  beside the protocol facts that preceded it.
- Exercise alternate-ingress retry.

**Acceptance:** web policy tests and Apple/Android controller tests prove
pause/seek/stall/foreground mapping, monotonic sequences, cancellation, and
one in-flight exchange; a playback-lab run joins each legacy recovery with the
preceding control snapshot.

### 13.3 M3 — actor and explicit playback lease

- Build `RollingSessionActor` behind the live recovery feature.
- Route playlist, segment, progress, child exit, flow control, control, end,
  and cluster fence events through it.
- Enable `PlaybackSessionLease`, explicit hold, and actual-runway production.
- Keep legacy `Session` selectable for rollback until equivalence passes.

**Acceptance:** actor model/property tests prove one terminal action under all
event orderings; a 30-minute virtual foreground pause with delivered
heartbeats retains the session without production; loss of both renewal
sources expires once; a suspended app follows §8.1's dormant/reopen contract;
disk and ahead caps hold under adversarial client values.

### 13.4 M4 — remove server watchdog pile

- Replace startup/lifetime watcher tasks with the one producer deadline.
- Remove in-place post-publication hardware/software and copy fallbacks.
- Convert post-publication failure into a typed proposal.
- Delete old child-transition atomics/locks and detached recovery loops.
- Add the repository watchdog-ownership check.

**Acceptance:** §7.3 symbols are absent or mapped only to the compatibility
engine; fault injection proves one pre-publication retry, no post-publication
child swap, and exactly one action proposal.

### 13.5 M5 — one client action owner

- Collapse each platform's stall watchdogs, reopen queues, and independent
  budgets into `PlaybackControlController`.
- Retain one playback-progress deadline that reports a stall.
- Make server actions the only automatic replacement path.

**Acceptance:** adversarial callback-order tests produce at most one action;
no platform code outside the controller can create a recovery session; VOD
completion, pause, seek, and background do not arm the playback deadline.

### 13.6 M5.5 — preparation feasibility and staged generations

- Run §5.4's physical dual-preparation spike on web, Apple, and Android.
- Freeze platform-specific metadata/buffer/first-frame acknowledgements from
  measured behavior and select a single-player fallback for each platform.
- Add `media_replacement_actions`, prepare-only activation, one-staged-
  successor constraint, committed-pointer CAS, abort, and expiry.
- Prove prepare neither calls the legacy supersession reap nor advances
  `media_playback_pointers`.

**Acceptance:** every platform has a recorded dual/single preparation result;
prepare leaves the predecessor current and running; commit advances the exact
expected pointer once; abort/expiry removes only the staged successor; owner
death during every phase leaves one durable outcome.

### 13.7 M6 — prepared recipe handoff and Auto policy

- Implement the replacement transaction and idempotent successor creation.
- Support resolution/bitrate first, then audio/subtitle burn, codec, and
  dynamic-range recipe axes.
- Add true make-before-break where capacity permits and §5.3's separately
  labeled one-slot alternatives/fallback.
- Preserve manual selection and outgoing bandwidth estimate.

**Acceptance:** scripted bandwidth cliffs and recovery satisfy §2.2; a true
make-before-break successor failure leaves the predecessor running; a one-slot
failure restarts the old recipe within its measured interruption bound;
duplicate acks/requests are harmless; physical clients record switch latency
without position regression.

### 13.8 M7 — subtitle windows, semantic indexes, and seek coalescing

- Report native/overlay readiness through `DeliveryView`.
- Drive bounded forward subtitle materialization from client demand.
- Coalesce seeks and cancel obsolete production by sequence.
- Join burn changes into an existing successor action.
- Persist authored/manual timeline annotations separately from the packed
  node-local `FragmentIndex` while presenting both as one analysis index.
- Add the lease-fenced analysis queue, force/retry/cancel API, Activity page,
  per-node coverage, local progress registry, and atomic generation publication.
- Add the optional bounded feature sidecar and enable recurring/visual/audio
  detectors only after their precision/recall gate; publish exact film-time
  intro/credits markers with provenance/confidence and preserve manual
  overrides.
- Use the control snapshot to prewarm marker destinations without seeking the
  client.

**Acceptance:** a 20-seek storm starts work only for the settled target;
subtitle work never replaces video independently; no advertised subtitle
interval returns an avoidable 404; a forced rebuild leaves the current index
serving until atomic publication; queue counts/stages/progress agree with the
claimed job; fixture seasons yield frame-refined repeatable markers; ambiguous
matches are not auto-skipped; clicking/auto-skipping lands at the stored exact
end time and a seek-back is recorded for detector evaluation.

### 13.9 M8 — cluster handoff

- Relay control with owner fencing.
- Implement planned drain, VOD resurrection, and rolling successor failover.
- Retire same-session rolling takeover except for the rollback compatibility
  lane.

**Acceptance:** three-node fault tests cover drain, hard kill during each
phase, duplicate commit, stale owner, and shared-store loss; one committed
owner and one client-visible transaction result in every case.

### 13.10 M9 — cutover and deletion

- Default protocol advertisement on after mixed-fleet evidence.
- Default the actor recovery engine on and remove the compatibility engine.
- Remove `/status` polling and deprecated client recovery code.
- Re-evaluate whether the `live-hls-recovery` feature remains necessary once
  VOD eligibility covers the supported catalog.

**Acceptance:** repository search finds only the three approved progress
deadlines and named lifecycle timers; playback-lab plus physical matrices are
green; rollback uses the previous release, not hidden dead code.

## 14. Test and adversarial review matrix

### 14.1 Protocol/model tests

- unknown protocol/version, fields, enums, NaN/infinity, negative and
  out-of-duration positions, invalid buffer order, huge body;
- equal/lower/skipped sequences, equal-sequence non-renewal, client-instance
  binding, owner-epoch rollover, and action idempotence;
- acknowledgement before prepare, wrong action, duplicate ack, ack after abort;
- pause/seek/background/end transitions and renewal sources;
- malformed controls cannot keep sessions alive;
- analysis claim epoch, expiry/retry, cancel at every phase, stale source,
  force deduplication, per-node coverage, and staged-publication CAS;
- action priority and combination tables over every proposal pair;
- actor event-order exploration around child exit, lease expiry, end, replace,
  cluster fence, and HTTP cancellation.

### 14.2 Delivery faults

| Fault | Expected result |
|---|---|
| network slows, producer healthy | proactive downshift proposal; current plays until successor ready |
| producer slows, network healthy | producer-constrained classification and eligible recipe/node action |
| decoder stalls, delivery healthy | client stall observation; codec/grade or player recovery proposal |
| child exits before publication | one validated retry then typed failure |
| child exits after publication | retained bytes stay readable; successor proposal, no in-place swap |
| successor start fails | abort successor; predecessor remains authoritative |
| control packets reorder | stale sequence rejected; no lease/action regression |
| captured control arrives after takeover | old control epoch rejected without renewal or mutation |
| heartbeats pause but media flows | media renewal keeps lease active |
| both renewal paths stop | one retirement and permit release |
| seek during preparation | abort/coalesce obsolete successor; latest target wins |
| subtitle selection during quality move | one combined successor or native readiness action |
| forced analysis while title plays | new generation queues/runs; current index remains serving |
| index worker dies mid-persist | claim expires, partial generation stays invisible, retry is safe |
| ambiguous repeated sequence | low-confidence/rejected marker; no default auto-skip |
| owner dies during prepare/commit | epoch fence plus durable phase yields one outcome |

### 14.3 Physical acceptance

Run representative VOD and rolling-fallback titles on web, Apple TV/iOS, and
Android TV/phone with copy, software transcode, hardware transcode, native
text, PGS overlay, burn, SDR, HDR10, and Dolby Vision where supported. Record:

- time to first frame;
- rebuffer count and duration;
- control loss/retry and owner move;
- downshift/upgrade decision evidence;
- prepare and switch latency;
- position error across switch;
- display-mode transition;
- producer deadline count;
- lease expiry/retirement reason;
- server CPU, disk, and bytes produced beyond reported demand.

Unit tests prove state machines. They do not prove decoder/display continuity,
background timer behavior, NAS latency, or cluster ingress recovery; those
must remain named physical acceptance evidence.

## 15. First PR boundary

M1 is intentionally useful but behavior-neutral. It establishes a reviewed
wire contract, explicit activity reason, and cluster-compatible route without
moving recovery authority before clients exist. The PR must not silently
enable the protocol or change production thresholds.

The implementation PR sequence is:

1. implement and self-inspect M1 without running unit tests;
2. open the PR so the diff and CI contract are stable;
3. run an independent adversarial review against the PR diff;
4. fix every valid correctness, security, cluster, compatibility, and test
   finding;
5. then run focused unit tests, broader repository gates, and fix failures;
6. make required checks green and merge through the repository's normal
   protected-branch path.

This ordering is deliberate for this effort: the adversarial review should
shape the tests rather than merely approve tests written around the first
implementation.
