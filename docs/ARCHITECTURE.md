# Architecture — how plurx is built, and why

Companion to [FEATURES.md](FEATURES.md) (everything the server does) and
[OPERATIONS.md](OPERATIONS.md) (how to run it) — this is *how it's built and
why it's built that way*. One Rust binary, `plurxd`: run one for a normal
server, run three and they form an active-active HA cluster with no external
infrastructure. The founding decisions are recorded here with their rationale
and their scars, because a decision without its reason gets "cleaned up" by the
next person who reads the code. Ecosystem facts and versions were verified
2026-07.

Diagrams are ASCII on purpose: they render identically in a terminal, on
GitHub, and in the in-app docs viewer, and they diff cleanly in a PR instead of
rotting in a separate asset pipeline.

## 1. System overview

Every node runs the same binary and serves reads, streams, and transcodes. Raft
leadership is an internal write-serialization detail, while membership carries
a durable `cluster_nodes.role`: voters decide consensus; learners replicate and
relay but hold no vote and may not open a tuner.

```
        ┌──────────────────────── clients ─────────────────────────┐
        │  web / Tizen / webOS      tvOS · Android TV · Roku        │
        │  (native /api/v1)         (native /api/v1)                │
        │  Kodi-family Plex clients ──▶ Plex-compat façade + GDM    │
        └───────────────┬───────────────────────────┬──────────────┘
                        │ native JSON               │ Plex XML/JSON
                        ▼                           ▼
        ┌──────────────────────── plurxd node (×1 or ×3+) ─────────────────┐
        │                                                                   │
        │   axum HTTP  ──▶  auth (Argon2id, bearer tokens)                  │
        │      │                                                            │
        │      ├──▶ decision engine ─▶ stream server ─┬─ range (direct)    │
        │      │      (pure fn)                        ├─ remux  (fMP4)     │
        │      │                                       └─ transcode ─▶ HLS  │
        │      │                                            │               │
        │      │                                     ffmpeg (child proc)    │
        │      ├──▶ scanner + metadata agents                               │
        │      └──▶ Store trait ──▶ Hiqlite: voters + non-voting learners   │
        └───────────────┬───────────────────────────────┬──────────────────┘
                        │ read-only                      │ HTTPS
                        ▼                                ▼
              shared media storage            TMDB · AniList (metadata)
              (NFS / SMB / cephfs)            cached locally, then offline
```

The human-visible server name follows the **`Store` boundary** too.
`server.name` in TOML seeds a new store once; the durable replicated setting is
authoritative thereafter. Existing installs therefore retain their configured
name on upgrade, while a joining node cannot silently rename the logical server.
Cluster discovery keeps that logical identity while making each voter
addressable: Bonjour publishes the replicated id and name plus a local node id;
Plex GDM uses the node id as `Resource-Identifier` to avoid client-side
deduplication and carries the replicated id separately as `Logical-Identifier`.
Legacy single-node discovery remains byte-for-byte unchanged.

The load-bearing boundary is the **`Store` trait**: every read and write goes
through it, so one-voter and multi-voter stores use the same call sites. That
is what made HA a backend swap (Phase 4) instead of a rewrite — the boundary
existed from the first commit specifically so this promise could be kept.

## 2. Cluster & state (the differentiator)

Nobody else in this space has real HA. Jellyfin is architecturally
single-instance (SQLite single-writer, in-memory transcode sessions — its
community bolts on keepalived + rsync and calls it done), and no Rust media
server is both maintained and clustered. The specific hard problem worth solving
is **replicated playback/transcode session state** — the thing that turns "a
node died, my movie is gone, restart it" into "playback hiccuped for two
seconds."

### 2.1 Consensus & storage — embed the store, don't run a database

**Decided (Phase 3 spike): embed the maintained hiqlite 0.14 fork** —
raft-replicated SQLite built on openraft for the exact "1 node or 3+ nodes, no
external infra" shape. The spike confirmed its `execute`/`query_map`/`txn` API
maps onto the existing rusqlite row mappers, and the shipped fork now carries
the compatibility patches listed in
[`PLURX-PATCH.md`](../vendor/hiqlite/PLURX-PATCH.md). See
[PHASE3-SPIKE.md](cluster/PHASE3-SPIKE.md). All cluster access stays behind the
one internal `Store` trait; openraft is an implementation dependency, not a
second ready-to-select backend.

Single-node mode is the same code path with a 1-voter raft (a supported
openraft/hiqlite pattern) — no "cluster edition" fork, and any single node can
later grow into a cluster by adding voters. A fresh data directory imports its
SQLite state into the replicated store on first boot, and activation is
one-way. If activation is interrupted, the next start permits one explicitly
warned unreplicated SQLite recovery boot; restart after that recovery completes
activation. The backup and restore boundary is specified in
[CLUSTER-BACKUP-AND-RESTORE.md](cluster/CLUSTER-BACKUP-AND-RESTORE.md).

### 2.2 Replication classes — not everything needs consensus

Data is sorted by how much its loss hurts, because paying raft's cost for a
regenerable thumbnail cache would be waste:

| Class | Examples | Storage | Loss tolerance |
|---|---|---|---|
| **Replicated-durable** | Users, auth tokens, settings, library metadata, watch state, playlists, playback sessions, node membership/health | Raft → SQLite through `Store` | None once acked |
| **Node-local, regenerable** | Transcode segment cache, image cache, thumbnails/trickplay | Local disk (optionally shared) | Free to lose |
| **Operator-owned** | The media files themselves | Shared storage | Read-only by default; only an admin-enabled library may opt into the Dolby Vision replacement contract |

The earlier replicated KV/cache tier was designed and not built. The shipped
hiqlite dependency does not enable its `cache` feature; adding an ephemeral
tier is a separate decision, not a property of session or membership state.

Write rates must be safe for raft. Each active-player heartbeat still reads
the item and durable watch state, but the M1d server coalescer now bounds
steady-state writes to one commit per ten seconds per user/item stream. The
first beat and a newly crossed 95% watched transition commit synchronously;
intermediate beats replace one pending newest value. A progress response never
pretends that pending value is durable: it returns the last committed
`WatchState`, while the active-player soft acknowledgement means only that the
coalescer accepted the beat. Manual unwatches and timestamped offline imports
invalidate or outrank pending values through compare-and-set flushes. Graceful
shutdown makes one bounded drain attempt; a storage outage can still lose an
unflushed intermediate beat, so this endpoint is the explicit exception to an
HTTP-level acknowledged-write durability promise. Store-method success itself
remains durable. Session state updates on segment boundaries, not per-chunk.
On Unix, the SIGTERM and SIGINT streams are installed before store activation,
hardware probing, and listener binding, then watched for the whole of startup
rather than only from the point `serve` first polls the drain. Installing the
streams keeps a signal off its default action; watching them is what lets
startup answer one. A signal observed before the server is built stops startup
at its next cancel-safe boundary and exits cleanly instead of binding a listener
the operator has already asked to go away. One stage is deliberately not
cancellable: `select_daemon_store` renames directories and fsyncs activation
markers in a fixed order, so a signal arriving inside it is acted on at the
boundary immediately after it rather than interrupting it, and an operator stop
grace period shorter than a single activation is still resolved by killing the
process — which the next boot recovers as an interrupted activation. Two
shipped-binary failpoints hold both halves: one raises SIGTERM after the
listener binds but before the drain future is first polled, so restoring lazy
registration fails deterministically instead of depending on a sub-millisecond
race; the other raises it as activation returns, so restoring the
buffer-until-serve behavior is caught by the daemon booting anyway.
This matters because raft commits every write to a quorum — a naive "save
position on every timeupdate" would put hundreds of writes/second through
consensus and melt it.

### 2.3 The failover mechanic — any node can serve segment N

Every operational number in this document is a named, checked value with a
link to the file that defines it. A bare duplicate is a bug: the code already
warns that a second copy is a failover defect waiting for the constant to
change.

The Phase 3 spike ([PHASE3-SPIKE.md](cluster/PHASE3-SPIKE.md)) measured the
deterministic-segment idea against constant-frame-rate, **sparse-keyframe**, and
VFR sources. The load-bearing property — *any node can produce a valid segment
N* — holds even in the sparse-keyframe worst case (accurate input-seek), and
independently-produced segments sequence to the correct total via the HLS
playlist.

```
 normal playback            node A dies mid-stream        client recovers
 ───────────────            ─────────────────────         ───────────────
 client ─▶ node A           client ─▶ node A  ✗           client ─▶ node B
   HLS seg 0,1,2,3            (request fails)                │
   one ffmpeg session         session recipe is in          ├ reads recipe from raft
   writing forward            replicated raft state          ├ restarts ffmpeg,
                                     │                        │   input-seek to seg 4
                                     ▼                        ├ emits EXT-X-DISCONTINUITY
                             recipe: {file, args,             └ serves the next segment
                              seg=SEGMENT_SECONDS,           buffered segments stay valid
                              keyframes@N*SEGMENT_SECONDS}
                                                            cost: a few seconds, once
```

1. Every transcode session pins its full recipe in replicated state: source
   file, ffmpeg arg set, encode segment duration `` `SEGMENT_SECONDS` = 2 ``,
   and forced keyframes at its multiples. The copy path separately uses
   `` `COPY_SEGMENT_SECONDS` = 6 `` with
   `` `COPY_FIRST_SEGMENT_SECONDS` = 2 ``; all three are defined in
   [`transcode/mod.rs`](../crates/plurx-core/src/transcode/mod.rs). The
   **primary** path is one sequential ffmpeg session (Phase 2) — clean, no
   per-segment resets.
2. **Failover:** a surviving node restarts the session seeked to the last-served
   segment boundary. Accurate input-seek guarantees a valid segment N from any
   node, so the client keeps its already-buffered segments and continues; an
   `EXT-X-DISCONTINUITY` is emitted at the failover boundary so the player
   remaps its timeline cleanly. This is the roadmap's "restart-at-position" — the
   spike showed it *is* the clean design, not a fallback.
3. HLS playlists are generated (not stored), identical on every node.
4. Client-side failover: clients hold the node list (REQ-HA-6); on request
   failure they retry the next node, which restarts the session from the
   replicated recipe. Direct play and remux failover are the same minus ffmpeg
   (stateless range requests / deterministic remux).
5. ffmpeg always runs as a child process — a codec crash kills a session, never
   a raft voter. Process isolation is load-bearing for HA, not a convenience.
6. **Optional optimization:** x264 `threads=1` makes segments byte-identical
   across nodes (measured), so a replicated/shared segment cache serves
   re-requested segments for free.

Scanner and metadata-refresh jobs are leader-scheduled singletons (distributed
lock), so three nodes don't triple-hit TMDB or thrash shared storage.

## 3. Playback pipeline — get out of the way first

The whole pipeline is built around one belief: the server's best move is to send
the file untouched. Everything else is a fallback the server is forced into, and
it says so out loud in `/decision`.

**The decision engine is a pure function.** `(file streams + HDR/audio detail,
device profile, client-reported caps, user prefs, bandwidth) → Decision`. Device
profiles are TOML data shipped with the server and hot-fixable; the file-side
facts come from the scanner (§4). It's a pure function so it's unit-testable
without ffmpeg, a server, or a network — the correctness of "will this play?"
never depends on runtime state.

```
                 ┌───────────────────────────────┐
 file streams ──▶│  video codec in profile?      │── no ──┐
 device profile  │  resolution ≤ max?            │        │  any hard
 client caps     │  bitrate ≤ max?               │── no ──┼─▶ video/res/
 user prefs      │  HDR ok, or tone-map needed?  │        │  bitrate/HDR
                 └──────────────┬────────────────┘── no ──┘  mismatch
                    all ok │                     │
                           ▼                     ▼
                 ┌──────────────────┐      ┌───────────┐
                 │ container ok AND  │─no─▶ │ TRANSCODE │  hardware first,
                 │ audio codec ok?   │      │  → HLS    │  tone-map, sub burn-in
                 └────────┬──────────┘      └───────────┘
                     yes  │        └── container/audio only ──┐
                          ▼                                   ▼
                   ┌─────────────┐                      ┌──────────┐
                   │ DIRECT PLAY │                      │  REMUX   │  -c:v copy,
                   │  HTTP range │                      │  → fMP4  │  audio maybe re-enc
                   └─────────────┘                      └──────────┘
```

**Serve paths:**

- **Direct play** — HTTP range serving of the file with correct caching headers;
  zero transcode CPU. This is the goal state, not the consolation prize.
- **Remux** — on-the-fly restream (MKV → fMP4/HLS) with `-c:v copy`; audio
  re-encoded only when the target can't take the source codec. Solves "right
  codecs, wrong container" (the tvOS/Roku staple) for pennies of CPU.
- **Transcode** — hardware first: QSV / VA-API (Linux), NVENC, VideoToolbox
  (macOS); software x264/x265 fallback. HDR→SDR tone mapping is
  **chosen by probe, per node**: `vpp_qsv`, `tonemap_vaapi`, `libplacebo`
  (Vulkan) and `tonemap_opencl` are candidates, and a node uses one only after
  it has proved itself end to end against real HDR10 — correct BT.709 output,
  a picture matching the CPU reference, and meaningfully faster than it. The
  CPU zscale chain is the floor and the fallback. Version sniffing would not
  do: a graph that parses can still produce clipped gray, and a driver that
  accepts the filter can still be slower than what it replaced. Image subs and ASS burn-in
  happen here: text through libass, bitmap (PGS/VobSub) as an `overlay`
  composite against a subtitle plane scaled to the output frame. Either sends
  the session back to the CPU chain, since both filters are system-memory.
- **Audio** — passthrough per device profile (TrueHD/DTS-HD where the chain
  allows), else transcode to EAC3/AC3/AAC with correct downmix.

**A stalled session repairs itself.** Hardware encoders can initialize cleanly
and then stall under concurrency (two QSV sessions on one iGPU is the classic
case). The prepublication actor exclusively commits the start verdict:
`` `ACTOR_HARDWARE_STARTUP_BUDGET` = 12 ``, then
`` `ACTOR_SOFTWARE_STARTUP_BUDGET` = 30 ``, while a running producer gets
`` `PROGRESS_STALL` = 10 `` seconds without output-timestamp progress. The
values are defined in
[`transcode.rs`](../crates/plurxd/src/transcode.rs); HTTP-side values are
compatibility mirrors that size playlist waiting and own no timer or recovery
decision. The ownership change is recorded in
[PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md](playback-control/PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).

ffmpeg is orchestrated as a **spawned CLI** (thin tokio process code), never
linked: crash isolation, license cleanliness, and drop-in support for the user's
ffmpeg build — **jellyfin-ffmpeg explicitly supported** and recommended for its
extra hwaccel/tone-mapping patches (and required for GPUs newer than the distro's
VA driver, e.g. Intel Arrow Lake on the `xe` kernel driver).

This section is the *server's* verdict. The client half — which transport each
browser actually uses to play a verdict, why Safari and Chromium diverge on
remux, and the copy-video HLS path that keeps Safari at source resolution — is
[PLAYBACK.md](PLAYBACK.md).

## 3a. Live TV — a tuner is a singleton, so it gets an owner

Everything in §3 assumes a file: seekable bytes that any node can open and any
node can serve segment N of. A tuner is the opposite. There is exactly one of
it, it cannot be reopened at an offset, and two processes reading it do not get
two streams — they get a fight. So Live TV keeps the same shape as the rest of
the system by adding one idea: **ownership**.

```
 HDHomeRun (private IPv4)          owner node                    any other node
 ┌──────────────────────┐    ┌────────────────────────┐    ┌────────────────────┐
 │ :80  discover.json   │◀───│ probe + sanitized      │    │                    │
 │      lineup.json     │    │ lineup, generation-    │    │  GET playlist      │
 │ :5004 /auto/vN  ─────┼───▶│ fenced                 │    │  GET segment       │
 └──────────────────────┘    │                        │    │        │           │
                             │ ONE GET → ONE ffmpeg   │    │        ▼           │
                             │      │                 │    │  signed relay over │
                             │      ▼                 │◀───┼── cluster API      │
                             │ 24-entry live window   │    │  (authenticated,   │
                             │ + opaque capability    │    │   bounded reads)   │
                             └────────────────────────┘    └────────────────────┘
```

- **The owner is configuration, not an election.** `live_tv.owner_node_id` is a
  replicated setting an administrator sets. An election would have to decide
  who holds a physical resource on the strength of a heartbeat, and a heartbeat
  cannot prove somebody else's FFmpeg process closed a tuner socket.
- **Only committed voters may create a tuner session.** A learner or a node in
  maintenance relays; it does not open hardware.
- **Time is never proof.** There is no timeout-only takeover. Re-homing the
  owner wants a confirmed, signed drain of the previous owner, or — when that
  node is simply gone — an explicit administrator attestation that it was
  stopped and cannot restart (`live_tv_fenced_owner`, carrying the original
  owner and the drain cutoff generation). That is a deliberate refusal to
  guess, and it is why owner loss ends the session in flight rather than
  quietly moving it.
- **Capabilities are the only thing on the wire.** A session hands back one
  opaque capability, bound to the issuing node and to the settings generation.
  Relayed reads are authenticated between voters and bounded in size; the
  playlist bytes and their segment inventory are published atomically, so a
  reader never sees a playlist naming a segment that is not there yet.
- **The window is the bound.** The checked `LIVE_HLS_OUTPUT_ARGS` fields are
  `` `LIVE_HLS_OUTPUT_ARGS_HLS_TIME` = 1 `` and
  `` `LIVE_HLS_OUTPUT_ARGS_HLS_LIST_SIZE` = 24 `` in
  [`live_tv.rs`](../crates/plurxd/src/live_tv.rs): a 24-second encode window or
  24 source GOPs on a copy route, deleted behind the frontier. §3's live-HLS
  reaper reasons about a file that has an end; a channel does not, so Live TV
  bounds its scratch by construction instead and reserves its own namespace
  from the finite-media sweeper.
- **Always compiled, never on by default.** There is no Cargo feature and no
  build variant. One binary exposes the same capability everywhere, and
  `live_tv.enabled` is a replicated runtime setting an administrator flips
  after readiness passes. A capability that exists in one build and not another
  is a support problem disguised as a safety feature.

The client half — how each player judges that a live stream is still playing,
and what it does with an outcome it cannot classify — is
[PLAYBACK.md](PLAYBACK.md).

## 3b. Library channels — a clock over immutable finite media

A Library channel owns no stream while nobody watches it. Its replicated
definition is a bounded catalogue recipe plus one active immutable rotation
and, while an edit waits for a safe boundary, at most one pending replacement.
Both Store implementations persist the same definition, generation, entry,
favourite, and idempotency entities; the application supplies every timestamp,
random seed, digest, and conditional-write expectation.

Channel building reads one bounded catalogue projection in a single consistent
database query. The Rust evaluator applies the same recipe and ancestry rules
for SQLite and Hiqlite, pins one eligible file fingerprint and duration per
item, and derives a deterministic order. Two process slots and one replicated
120-second claim per channel bound builders. Claims renew every 30 seconds,
entries stage in bounded batches, and publication atomically verifies the
definition revision, claim, entry count, offsets, and digest before changing a
channel pointer. An interrupted or stale build is never visible.

Resolution is arithmetic over the published epoch and cumulative durations:
it loads the effective generation, binary-searches the current occurrence, and
returns the source offset computed from server UTC. Guide reads perform a
bounded merge over only the requested channel/window. Immutable vectors use a
node-local 64-generation/32 MiB LRU; visibility, enabled state, permissions,
and the effective generation are still read authoritatively before cache use.

Watching deliberately crosses back into §3's finite HLS service through a
dedicated channel-session route. A typed purpose containing channel,
generation, cycle, ordinal, and tune sequence is persisted in the session
identity before playback side effects. Control exchanges revalidate the
current channel while that purpose suppresses ordinary playback-start and
watch-history work. **Watch from start** creates an unrelated ordinary VOD
purpose, so personal progress and seeking retain their existing semantics.

There is no build feature or readiness veto. `library_channels.enabled` is a
replicated runtime admission switch, while Settings → Developer reports
storage, catalogue, and client facts as advice. Definitions remain editable
while admission is off.

## 4. Scanner & metadata — ffprobe is ground truth

```
 library root
     │  inotify + periodic reconcile (incremental: skip unchanged by size+mtime)
     ▼
 identify ──▶ filename/structure parse (movie · show S/E · anime absolute #)
     │
 inspect  ──▶ ffprobe -print_format json  ─┐  codecs, profiles, bit depth,
     │        (pure-Rust pre-scan skips     │  HDR10/HDR10+/DV profile+level,
     │         unchanged files cheaply)     │  audio layouts, subtitle tracks
     ▼                                      └─▶ fed VERBATIM to the decision engine
 match    ──▶ TMDB (movies/TV)  ·  AniList (anime, no key)
     │        anime detection routes to absolute numbering, not TVDB seasons
     ▼
 cache    ──▶ provider JSON + artwork cached (replicated / shared)
              a scanned library works offline forever
```

The `home` branch (home video & photos) skips the provider entirely — there is
nothing to match a camera file against:

```
 home root
     │  same walk, plus still images (jpg/png/heic…)
     ▼
 mirror   ──▶ directory tree becomes folder items ("2019" ▸ "Beach Trip")
     │        titles are the filename verbatim (NOT the movie parser, which
     │        would turn "Christmas 2019.mp4" into "Christmas")
     ▼
 seed     ──▶ <basename>.nfo read ONCE per item, then never again
     │        date ladder: NFO ▸ container creation_time / EXIF ▸ filename ▸ mtime
     ▼
 art      ──▶ local art beside the file, else an ffmpeg frame grab
              → artwork cache, never next to the media
```

The scan result is published *before* metadata enrichment starts, so the UI
shows real file counts and any problems while posters are still fetching — a scan
that found nothing tells you immediately, instead of looking like it's still
working. Enrichment runs as a second phase and is leader-coordinated so a cluster
doesn't triple-hit the providers.

## 5. API design — one service, two façades

**Native API** (`/api/v1`) — JSON over HTTP, enumerated route by route in
[API.md](API.md). Auth: opaque bearer tokens from local login, plus scoped
`plx_` keys for machine callers; Argon2id at rest, SHA-256 token lookup.
There is no WebSocket or SSE push channel. Most client state is polled, while
the playback-control exchange is a bounded long poll the server may hold for
`` `EXCHANGE_DEADLINE` = 4 `` seconds, defined in
[`playback_control.rs`](../crates/plurxd/src/playback_control.rs); a prepared
server decision therefore arrives on a held client request. [API.md](API.md)
§10 specifies its cadence and floor. Two things this design called for are not
built: an OpenAPI description (the routes were meant to generate one and do
not, so API.md and the `tests/contracts/native-api.json` fixture are the
specification the five client platforms work from), and the optional OIDC
(Google/Apple) code flow mapping to local accounts that REQ-USER-2 asks for.

Cluster activity uses one separate, non-public application RPC:
`/_internal/v1/activity-snapshot`. Membership retains explicitly advertised
plurxd endpoints internally and signs each request with short-lived cluster
authority from a durable per-node Ed25519 key, bound to the live sender and
intended target; user/admin/Plex bearers, shared cluster secrets, and HLS
capability ids are never forwarded. The response is a byte-exact bounded,
minimal node-local delivery snapshot. Fan-out is concurrent under one
two-second deadline, caps the roster, rejects redirects and wrong-node responses, and
distinguishes unhealthy, unreachable, invalid, and timed-out peers for the
public aggregator. A SQLite or never-joined node has no peers and performs no
fan-out.

**Plex-compat façade** (Tier 1, REQ-PLEX-1) — a stateless translation layer over
the *same* services, plus a GDM responder (UDP 239.0.0.250:32414, LAN-only).
Implements the endpoint set the Kodi-family clients actually use: `/identity`,
`/library`, `/library/sections`, `/library/sections/{id}/all`,
`/library/metadata/...`, `/photo/:/transcode`, part serving, `/:/timeline`,
`/:/scrobble`, `/:/unscrobble`, `/search` and `/hubs/search` — and nothing
else, so the transcode-decision, `/:/progress` and `/playlists` endpoints an
older draft of this section listed are absent. Responses are XML
`MediaContainer`; content negotiation is not implemented. `X-Plex-Token`
values are plurx tokens. plex.tv is never contacted (REQ-PLEX-3); plex.tv *emulation* for
Infuse/official apps is deferred Tier 2 (see [CLIENTS.md](CLIENTS.md) §3).

It's a façade over shared services rather than a fork so that a bug fixed in the
decision engine is fixed for both a native client and Kodi at once — there is one
source of truth for "how does this file play," not two.

## 6. Tech stack (verified 2026-07)

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust (stable, pinned toolchain) | Single static binary; cross-compile amd64/arm64 |
| HTTP | axum 0.8 + tower-http | Streaming bodies, range serving; hyper 1.x |
| Cluster | hiqlite 0.14, vendored and patched | [`PLURX-PATCH.md`](../vendor/hiqlite/PLURX-PATCH.md) |
| Local DB | SQLite (rusqlite), STRICT tables + FTS5 search | Relational metadata, append-only migrations |
| Transcode | ffmpeg CLI spawn; jellyfin-ffmpeg supported | §3 |
| Inspection | ffprobe JSON; `symphonia` / `matroska` pre-scan | §4 |
| HLS | generated playlists (`m3u8`) | §2.3 |
| Discovery | mDNS `_plurx._tcp` + Plex GDM responder | LAN only |
| Passwords / tokens | Argon2id (at rest) · SHA-256 (token lookup) | §5 |
| Observability | `tracing` + Prometheus exporter | REQ-OPS-1 |
| Web app | embedded static app, `` `WEB_ASSETS` = 64 `` files, no bundler or framework | [shell layout](clients/WEB-SHELL-LAYOUT.md), [source table](../crates/plurxd/src/http/web.rs) |
| Avoided | sled (stalled), rocksdb (C++ dep), external DBs, ffmpeg linking | — |

The web app is a deliberate non-choice: a hand-written shell loads the checked
asset table, all compiled into the binary. No npm, no bundler, no framework
churn — the admin UI ships in the same static binary as the server and can
never version-skew against the API it talks to.

## 7. Key decisions, each with its reason

1. **One binary, one code path for 1 or N nodes.** A "cluster edition" fork
   doubles the test surface and rots the single-node path. Cost accepted: the
   single-node case carries a 1-voter raft it doesn't strictly need. Worth it —
   there is exactly one code path to keep correct.
2. **The `Store` trait from commit one.** Everything touches storage through it,
   so Phase 4 swaps SQLite for hiqlite behind the trait. The scar this avoids:
   media servers that grew a database assumption into 500 call sites and could
   never cluster without a rewrite.
3. **ffmpeg is spawned, never linked.** A codec crash must not take down a raft
   voter, the license stays clean, and users can drop in jellyfin-ffmpeg. Cost:
   process-spawn overhead per session and parsing ffmpeg's stderr for progress.
4. **Direct play is the goal, transcode is the failure.** The decision engine is
   biased toward sending the file untouched and reports every reason it couldn't.
   This is why `/decision` exists as a first-class endpoint: the server can always
   explain itself.
5. **ffprobe output is treated as ground truth and stored verbatim.** The raw
   JSON is kept so a future decision-engine rule can use a field we didn't parse
   yet, without a re-scan of the whole library.
6. **Chapters, not fingerprinting, for skip intro/credits/preview.** Real chapter titles
   (MakeMKV, anime OP/ED, hand-authored) are honest and cheap — one ffprobe at
   playback start. We do *not* guess an intro from a model, because a "Skip Intro"
   button that jumps into the middle of a scene is worse than no button. A title
   alone is not enough either: the match must sit where the title claims (an
   intro in the first half, credits in the last 30%), or the broad substrings
   turn a scene called "Closing Time" into a mid-episode Skip Credits button.
   When no title names the credits we infer the window — from the final chapter
   boundary when it lands in a plausible tail, from the runtime when it does not
   — and the API marks either inference `chapter:false` so the UI can hedge.
   A labelled *preview* ends that window rather than starting it, and when it
   leaves no chapter boundary to infer from we say nothing at all: the region
   before a preview is story, and a guessed "Skip Credits" over it would seek a
   viewer out of the episode. A chapter earns the preview kind on position and
   structure, not on its title: it must sit in the credits window *and* be the
   last thing in the file. Title matching alone is not safe there, because the
   position bound rejects nothing above 70% and a false preview in the tail
   does not add a spare button — it deletes the file's real Skip Credits marker
   and, on the web, offers one that reports the episode watched. A preview is offered but never auto-skipped —
   it is new footage every week, and the toggle is a standing preference about
   repeated material.
7. **The NFO is a seed, not a store.** A Kodi `<basename>.nfo` in a home
   library is read once, at first ingest, to build the item — and after that
   the DB owns the metadata: plurx never re-reads the sidecar and never writes
   one. This is what lets home video exist without an exception to "plurx never
   writes to media storage" (§8), and it means a hand edit can never be
   clobbered by a file on disk. Accepted trade-off: a DB rebuilt from scratch
   re-seeds from the NFOs and loses post-seed edits — that is what backups are
   for.
8. **Watch state is judged on the probed duration, not the stream's.** A
   progressive stream's `video.duration` grows as it buffers; trusting it marked
   partially-watched items as fully watched. The file's `ffprobe` duration is the
   authority. Scar: this bug shipped once and is why the rule is now explicit.
9. **DVR reverses §8's "no scheduler", decided 2026-09-13.** Recording was
   refused because it introduces a writer with a schedule, a retention policy
   and a conflict resolver. Paul accepted the choices in
   [LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md](features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md)
   on 2026-09-13; the constraints that make the writer safe are in
   [LIVE-TV-DVR-IMPLEMENTATION.md](features/LIVE-TV-DVR-IMPLEMENTATION.md) — a
   dedicated `dvr.root` outside every library, a free-space floor, a tuner
   reserve, `dvr.enabled` off by default, and
   `` `DVR_TICK` = 15 `` seconds as defined in
   [`dvr.rs`](../crates/plurxd/src/live_tv/dvr.rs). §8's read-only rule is
   unchanged: plurx still never writes into media storage.

## 8. Non-goals (what the architecture deliberately refuses)

Named here because an architecture is defined as much by what it won't do — every
one of these is a door we're keeping shut on purpose:

- **No external database, broker, or cache service.** The moment plurx needs
  Postgres or Redis to run, "lean and boring to operate" is dead. The embedded
  raft store is the whole point.
- **Media is read-only unless an administrator opts one library into the named
  replacement workflow.** Scanning, metadata, playback, home video, and ordinary
  library maintenance never organize, rename, or delete media. The sole
  exception is the off-by-default Dolby Vision Profile 7 → 8.1 conversion: it
  replaces verified media bytes only under the dedicated-account, writable
  filesystem, and backup contract in [OPERATIONS.md](OPERATIONS.md). Home-video
  NFO seeding remains one-way and generated thumbnails stay in the artwork
  cache. Renaming a folder in the UI changes the DB title, never the directory.
- **No cloud dependency, no phone-home.** Everything works on a LAN that never
  touches the internet. There is no plurx.tv and there never needs to be.
- **No linking GPL/ffmpeg into the process.** See decision 3.
- **Not an everything-server (yet).** Music is out of scope (photos: supported
  in `home` libraries since 2026-07); the data model won't preclude music, but
  it is not bolted on speculatively.
- **DVR never writes into a library.** The off-by-default recorder writes only
  under its dedicated `dvr.root`; its scheduler does not weaken the media
  read-only boundary above.
- **No transcode-by-default.** The server does not automatically "optimize" a
  library into pre-baked renditions. An off-by-default pre-transcode pass exists
  when `jobs.cache_produce_mins` is greater than zero, and it never outranks a
  live viewer; [OPERATIONS.md](OPERATIONS.md) documents that queue boundary.

## 9. Risks & mitigations

| Risk | Mitigation |
|---|---|
| hiqlite is a small project (bus factor) | The maintained fork and its fifteen patches are explicit in `PLURX-PATCH.md`; `Store` isolates callers, while carrying the fork is now the accepted cost |
| Deterministic-segment failover has sharp edges (VFR, keyframe drift) | Spiked at the Phase 3 gate; worst case = session restart-at-position, still ahead of everyone |
| Plex-compat drift / client quirks | Tier 1 targets a small, testable client set; contract tests against recorded Composite/PKC traffic; official API docs exist now |
| DV/HDR correctness is genuinely hard | Profiles are data; a test-file corpus per DV profile (P5/P8) from day one; HDR10 base-layer + tone-map fallbacks |
| Concurrent hardware transcode stalls | Software-fallback watchdog (§3); QSV-preferred on Arc-class GPUs |
| Solo-dev scope creep | Roadmap phases are gates; anything not in [REQUIREMENTS.md](REQUIREMENTS.md) is a "later" by default |
