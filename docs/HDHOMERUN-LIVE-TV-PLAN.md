# HDHomeRun Live TV — one tuner, every plurx client

**Status:** M0/M1 merged; M2 review fixes under verification · **Effort:**
`effort/hdhomerun-live-tv` · **Written:** 2026-09-04

Companion to [PLAYBACK.md](PLAYBACK.md) (how finite files become streams),
[ARCHITECTURE.md](ARCHITECTURE.md) (how nodes and clients fit together), and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (how this effort is built
and qualified) — this plan adds network broadcast television without pretending
that a tuner is a media file. Read §1–§5 before changing code, then execute the
milestones in §8 in order. Re-verify every cited type and route against the
named source at build time; if an implementation step requires placing tuner
URLs in `MediaFile.path`, teaching the VOD engine about endless input, or
allocating tuners independently on every voter, stop and fix the design.

## 1. Objective — live television that owns its sharp edges

An administrator can pair one HDHomeRun device from Settings → Developer,
inspect its channel lineup, and enable Live TV after plurx proves the device,
FFmpeg, tuner owner, and network path are usable. Signed-in viewers then see a
Live TV destination in the web, Apple, and Android clients, choose a non-DRM
channel, and receive a bounded sliding HLS stream normalized for those clients.

The first shipped contract is deliberately exact:

- one HDHomeRun device, addressed by private IPv4 address;
- the device's HTTP `discover.json` and `lineup.json` surfaces;
- non-DRM ATSC 1.0, with non-DRM ATSC 3.0 accepted only when the installed
  FFmpeg proves it can decode the source and produce the configured output;
- H.264 video and AAC audio in a six-segment live HLS window;
- one configured owner node for the physical tuner, with signed start and
  media relay through any other ingress node;
- a conservative two-session default, never exceeding the device's reported
  tuner count; and
- channel browsing and live playback in every first-party client.

"HDHomeRun 4K" names the hardware. It is not a promise that plurx can decrypt
protected ATSC 3.0 or that every FFmpeg build decodes AC-4. The UI must say
which limitation it observed instead of turning either into a spinner.

## 2. Requirements — the behavior a finished effort proves

### 2.1 Pairing is an admin action with a measured readiness result

Settings → Developer contains one **HDHomeRun Live TV** card. It collects a
private IPv4 address, owner node, maximum concurrent sessions, and output
height. **Test connection** asks the selected owner to fetch bounded discovery
and lineup documents, checks every active serving node advertises the current
live-TV protocol, and exercises the exact synthetic production graph with the
selected encoder, its upload/filter path, conditional deinterlacing,
family-specific forced IDR, AAC, MPEG-TS HLS, and requested height. Compiled-in
codec names alone are not readiness. **Enable Live TV** is available only when
that checklist is green.

The card explains the prerequisites before presenting the switch: complete a
channel scan in the HDHomeRun app; reserve a stable private IPv4 address; prove
the selected owner's container/network namespace can reach ports 80 and 5004;
leave enough tuners for other household clients; expect non-DRM channels only;
and understand that ATSC 3.0 requires this owner's installed FFmpeg to decode
the actual source. It shows a green/red row for device identity, fresh lineup,
owner protocol/quorum, exact FFmpeg graph, session limit, and DRM boundary.

The feature ships in every ordinary binary. `live_tv.enabled` is a replicated
runtime setting, not a Cargo feature, `#[cfg]`, environment-only switch, or
compile-time constant. A build with `--no-default-features` still contains the
same live-TV code and API.

Disabling prevents new sessions and advances a serving fence that cancels and
drains every existing session on the old configuration. Owner changes, loss of
serving authority, and quorum loss do the same. Draining kills and waits for
FFmpeg, closes the upstream body, removes scratch state, and releases permits.
It does not alter the HDHomeRun lineup, retune the device outside an owned HTTP
stream, or delete any media.

**Acceptance:** an admin can save a valid device while disabled, cannot enable
an invalid/public/loopback address or unreachable owner, and sees the exact
failed prerequisite beside the control that fixes it.

### 2.2 The lineup is bounded, private, and honest

`GET /api/v1/live-tv/channels` returns a cached projection of `lineup.json`.
Each row exposes only plurx-owned fields: stable channel id, guide number,
guide name, favorite, DRM, and support state. It never exposes `DeviceAuth`,
the device base URL, or a lineup-supplied stream URL.

The response also carries `freshness` (`fresh` or `stale`), `age_seconds`,
`last_success_at`, and a sanitized optional `refresh_error`. All clients show
stale state without presenting it as healthy, and refuse a start if the owner
cannot refresh the selected row.

The client accepts at most 512 channels, 1 MiB of JSON, 32 bytes of guide
number, 256 bytes of name, and recognized tag tokens. Unknown JSON fields are
ignored because firmware adds fields; malformed required fields reject that
row rather than poisoning the whole lineup. Duplicate guide numbers reject the
document because a playback id would otherwise name two sources.

Support states are:

| State | Meaning | Viewer action |
|---|---|---|
| `ready` | Not tagged DRM; actual codecs are proved when the one ingest opens | Press Watch |
| `drm_unsupported` | The lineup says the channel is protected | Use a licensed HDHomeRun client |
| `disabled` | Live TV is not enabled | Ask an admin to complete the Dev card |
| `device_unavailable` | The bounded lineup refresh failed and no fresh cache exists | Check power, IP, VLAN, and Docker routing |

Codec failures discovered during session startup use a typed session error and
do not permanently blacklist a channel: broadcasters and firmware can change
the stream without changing the guide number.

**Acceptance:** hostile URLs, redirects, oversized bodies, duplicate ids, DRM
tags, stale caches/freshness fields, and partial/malformed rows have
deterministic tests.

### 2.3 Starting one viewer opens one tuner connection

Session creation opens the lineup-derived HDHomeRun HTTP stream exactly once
with the same pinned, proxy-free Reqwest client used for device documents.
FFmpeg never receives a device URL. The manager validates the HTTP status and
pumps the one response body through a single backpressured task into FFmpeg's stdin; a
cancel, stall, or child exit drops the response and therefore closes the tuner
TCP connection. FFmpeg probes and consumes `pipe:0` with bounded probe time.
There is no separate `ffprobe` request, because a second HTTP GET can consume a
second tuner.

The start request has a common 15-second budget, including the fresh lineup,
for a complete HLS playlist and first
segment. Success returns a short-lived capability URL. Failure kills and waits
for FFmpeg, removes scratch files, releases both tuner and transcode admission,
and returns one of:

- `live_tv_disabled`;
- `owner_unavailable`;
- `tuner_capacity`;
- `tuner_unavailable` (HDHomeRun `503`, which is intentionally not narrowed to
  "busy" because the device also uses it for tune/authorization failures);
- `channel_not_found`;
- `drm_unsupported`;
- `codec_unsupported`;
- `startup_timeout`; or
- `stream_failed`.

**Acceptance:** cancellation at every startup await leaks no permit, child,
scratch directory, or upstream tuner connection.

### 2.4 Live HLS is endless input with bounded local state

The live engine owns a six-entry playlist of 4-second MPEG-TS segments. It uses
FFmpeg's `delete_segments`, `temp_file`, `independent_segments`, and
`omit_endlist` flags, `hls_list_size=6`, and `hls_delete_threshold=1`; it never
uses VOD playlist type or suspends the ingest process. A tuner stream is
realtime input; pausing its reader overflows somebody else's buffer rather
than creating a useful lead. Scratch inventory admits only the playlist, one
temporary file, six listed segments, and one deletion-lag segment. A 128 MiB
per-session byte ceiling terminates a runaway producer.

The normalized profile uses the node's already selected H.264 encoder and
shared foreground transcode admission, AAC stereo at 192 kb/s, and conditional
deinterlacing. The configured output height is 720p or 1080p; 720p is the safe
default. Video bitrate is 4 Mb/s at 720p and 8 Mb/s at 1080p. The encoder must
force an IDR at every 4-second boundary so each segment is independently
decodable.

Both video and audio are required inputs. A demuxer that sees no usable audio
must not silently publish video-only HLS while advertising AAC. Missing
decoder and required-stream diagnostics map to `codec_unsupported`; unrelated
FFmpeg failures remain `stream_failed`. Recognition uses bounded stderr and
the upstream [decoder diagnostic](https://ffmpeg.org/doxygen/trunk/ffmpeg__demux_8c_source.html)
and [required-map diagnostic](https://ffmpeg.org/doxygen/trunk/ffmpeg__opt_8c_source.html),
without returning raw process logs or claiming that every missing audio stream
is AC-4.

One session owns:

```text
 tuner permit · foreground encoder permit · FFmpeg child · scratch directory
       │                    │                     │              │
       └────────────────────┴──────────┬──────────┴──────────────┘
                                      ▼
                      one drop-safe LiveTvSession owner
```

A 45-second capability idle timer reaps sessions whose playlist, segment,
status, or keepalive has not been touched. Independently, a producer watchdog
requires either FFmpeg `out_time` or the committed HLS media sequence to
advance within 30 seconds, and the upstream pump has its own read no-progress
deadline. Client polling never refreshes either producer clock. DELETE is
idempotent. Daemon shutdown kills and waits for every child before returning.

**Acceptance:** a synthetic endless source advances media sequence for ten
windows while scratch inventory and bytes stay bounded; orderly source EOF and
teardown never publish `#EXT-X-ENDLIST`; DELETE, idle, producer stall, client
loss, source EOF, FFmpeg failure, and shutdown each close the source once and
release every permit.

### 2.5 One owner prevents a four-node cluster claiming sixteen tuners

The replicated settings name one `live_tv.owner_node_id`. Only that process
fetches device documents, opens streams, evaluates local FFmpeg readiness, or
owns tuner permits. Every ingress obtains lineup/readiness snapshots from that
owner through signed peer requests; an ingress-local LAN path is never treated
as proof that the selected owner can reach the tuner. An ingress that receives
a start, playlist, segment, status, keepalive, or delete request for another
owner uses exact-request Ed25519 authentication and redirect-free peer
transport.

Readiness, start, activation, playlist, status, keepalive, and stop control
responses additionally sign the HTTP status and exact bounded body, bound to
both node identities, the request nonce, and the route. Activation URLs are
constructed locally from the validated capability. Drain acknowledgements
carry a separately signed exact owner/target/nonce/generation/count tuple.
MPEG-TS response bytes remain streaming and require a trusted cluster network
(or TLS) for transport confidentiality/integrity; a request signature alone
does not authenticate a streamed response body.

Live TV also has an always-compiled `live_tv_v1` node capability in the current
heartbeat declaration. Enabling and starting require the owner and every
active serving node to advertise the compatible protocol; rolling deployments
therefore leave the runtime feature disabled until all possible ingress nodes
understand its capability and internal API.

The public capability format is:

```text
ltv1.<base64url(owner_node_id)>.<uuid-v4>
```

The complete value is a bearer capability and is redacted from access logs.
The owner node id is routing data, not authority; the random UUID is the
authority. Internal handlers re-parse and verify that the capability names the
receiving node before touching the local session registry.

Each accepted session captures the current replicated configuration generation
and `ServingAuthority` loss generation. The owner watches both. Disable,
device/owner change, serving-fence loss, quorum loss, or node demotion makes
that generation stale and immediately begins cancel-and-drain. New starts
recheck both generations before and after admission and before publication.

An owner/device transition uses one expected-generation compare-and-swap
transaction to atomically replace the complete tuple and increment its
configuration generation. Concurrent writers cannot publish two different
tuples with the same generation; a loser receives `settings_conflict`, reloads,
and may explicitly retry. Every manager read obtains the tuple from one
transactional snapshot, never five independent setting reads.

Disable atomically records the original owner and a fixed drain-before
generation. Disabled edits preserve that barrier, including A→B→C owner
changes. The new owner refuses admissions until an authenticated drain proof
clears it, or an administrator makes a separate, exact-generation physical
fencing attestation while disabled: **I have stopped or powered off the
previous owner and prevented it from restarting until it can synchronize the
current configuration.** Recovery remains disabled; readiness and enable are
separate actions. Unreachability alone is not proof of physical cleanup.

A wall-clock timeout cannot prove the old socket/process has stopped and is
not an automatic recovery mechanism. Serving-authority loss still cancels the
old owner's sessions without a Store round trip. The owner installs its
monotonic drain floor under the same registry lock as insertion, and shutdown
permanently closes insertion before collecting sessions. Neither delayed
starts nor stale drains can cross those fences. Failed child reaping retains
the child and encoder reservation; failed scratch cleanup retains the registry
entry and can be retried. Neither failure produces a successful drain proof.

This release does not take over a live session when the tuner owner dies.
TCP closure frees the physical tuner and the player receives
`owner_unavailable`; pressing Watch creates a new session after the owner is
healthy or an admin selects a different reachable owner. This is a visible
availability trade-off, not silent per-voter over-allocation.

**Acceptance:** with four test nodes and a device count of four, concurrent
starts admit at most the configured global limit; every ingress can serve one
owner's capability; a forged owner, stale peer, invalid signature, redirect, and
relay stall fail closed. Partition, disable through a remote voter, in-flight
start, and concurrent owner/device changes fence the old generation, drain it,
and never overlap admission generations.

### 2.6 Live controls do not write finite-file state

Every client uses a live-specific player adapter. It may reuse HLS attachment,
fullscreen, audio output, and session cleanup, but it must not write progress,
resume, scrobble, timeline annotations, skip markers, autoplay-next, or VOD
quality restarts.

Controls are Play/Pause · Mute/volume · captions when proved · Fullscreen ·
Close. The timeline reads **LIVE** and exposes no forward/back seek controls.
Changing channels deletes the old session before creating the new one.
Owner loss, producer stall, capability expiry, or returning from background
after idle reap stops the live adapter, attempts one best-effort DELETE, and
shows a typed Watch-again state. It never invokes VOD recovery or resume.

**Acceptance:** web, Apple, and Android client tests assert that live playback
starts and closes exactly one session and issues zero finite-file state writes.

## 3. Protocol and trust boundary — the tuner is untrusted LAN input

### 3.1 Control URLs come from one validated address

The settings API accepts a canonical IPv4 literal only. It permits RFC 1918
and link-local unicast addresses; it rejects loopback, unspecified, multicast,
broadcast, documentation, carrier-grade NAT, and public ranges. DNS names are
out of scope because rebinding would make a saved safe address unsafe later.

plurx derives the discovery URL itself and reconstructs every later request on
the same pinned IPv4 origin:

```text
http://<device-ip>/discover.json
http://<device-ip>/lineup.json
http://<device-ip>:5004/auto/v<validated-guide-number>
```

`BaseURL`, `LineupURL`, and channel `URL` are untrusted routing hints. Their
advertised host is never required to equal the configured literal because real
firmware may use a `.local` name, and it is never resolved or contacted. The
parser bounds and validates their syntax, accepts only the exact expected
document or `/auto/v<guide>` path and ports 80 or 5004, then reconstructs the
request with the configured IPv4. Userinfo, queries, fragments, unexpected
ports, and any other path are rejected. The dedicated Reqwest client disables
redirects and environment/system proxies and applies a 2-second connect,
10-second document deadline, and 15-second envelope for stream headers plus the
first complete playlist/segment. That envelope is not a whole-body timeout on
the endless response. After activation, the body has no total deadline and is
governed by the 30-second upstream-read and producer-progress watchdogs.

Guide numbers match `^[0-9]{1,4}(\.[0-9]{1,4})?$`. This is deliberately
narrower than "a URL path"; a guide number has no reason to contain a slash,
query, escape, userinfo, or control character.

### 3.2 DeviceAuth and capabilities never cross the browser boundary

`DeviceAuth` is accepted only so the parser can ignore/redact it. No shipped
feature needs SiliconDust cloud authorization, so plurx neither stores nor
logs it. Full tuner URLs and session capabilities are replaced by route-shaped
redactions in traces and support bundles.

The public channel list and admin readiness response may include device id,
model, firmware, tuner count, and friendly name. These are operator facts, not
credentials.

### 3.3 DRM fails before a tuner is opened

A tokenized, case-insensitive `drm` lineup tag makes the row unplayable. The
server still treats a runtime `503` as ambiguous: stale lineups exist, and the
HTTP protocol uses the same response for capacity, tune, authorization, and
protection failures. plurx does not attempt to decrypt, record, tunnel around,
or relabel protected content.

## 4. Component contract — a separate live-TV subsystem

Re-verify these paths and signatures before each milestone; they describe the
intended boundary, not permission to force stale names into current code.

### 4.1 Core types (`crates/plurxd/src/live_tv.rs`)

```rust
pub(crate) struct LiveTvManager { /* client, cache, sessions, permits */ }

pub(crate) struct LiveTvConfig {
    pub enabled: bool,
    pub device_ipv4: Ipv4Addr,
    pub owner_node_id: String,
    pub max_sessions: usize,
    pub output_height: u16,
    pub generation: u64,
}

pub(crate) struct LiveTvDevice {
    pub device_id: String,
    pub friendly_name: String,
    pub model_number: String,
    pub firmware_version: String,
    pub tuner_count: usize,
}

pub(crate) struct LiveTvChannel {
    pub id: String,
    pub guide_number: String,
    pub guide_name: String,
    pub favorite: bool,
    pub support: LiveTvChannelSupport,
}

impl LiveTvManager {
    pub(crate) async fn readiness(&self, config: &LiveTvConfig)
        -> Result<LiveTvReadiness, LiveTvError>;
    pub(crate) async fn channels(&self) -> Result<LiveTvLineup, LiveTvError>;
    pub(crate) async fn start_local(&self, request: LiveTvStart)
        -> Result<LiveTvStartResponse, LiveTvError>;
    pub(crate) async fn resource_local(&self, session: &str, resource: LiveTvResource)
        -> Result<axum::response::Response, LiveTvError>;
    pub(crate) async fn stop_local(&self, session: &str) -> Result<(), LiveTvError>;
}
```

`LiveTvManager` joins `AppState` as `Arc<LiveTvManager>`. It does not join
`TranscodeManager.sessions`, `Library`, `Item`, or `MediaFile`. The only shared
playback surface is an always-compiled foreground admission method returning a
drop guard plus the selected encoder arguments.

`LiveTvResource` is a closed enum for playlist, status, keepalive, and an
inventory-verified segment name; no arbitrary relative path reaches a file
open. Every resource has an explicit byte ceiling, end-to-end deadline, and
read no-progress limit. Segment names are numeric and must already be present
in the session's parsed playlist inventory; separators, controls, dot entries,
temporary names, and symlinks are rejected.

`LiveTvStart` carries an ingress-generated UUID request id, source user/node,
and expected configuration/serving generations. The owner registry keys a
provisional start by `(source_node, source_serving_generation, user_id, request_id)` and stores an
immutable fingerprint of channel, profile, user/node, and both generations. A
repeated signed request with that fingerprint returns the same `created` or
`recovered` result instead of opening a second tuner; a mismatched replay is a
conflict. The response contains a short activation token and lease. Activation
is also idempotent: duplicate requests with the same token return the same
`activated` result, including after a lost response, while any other token is
rejected. Unactivated starts are reaped. The public client sees success only
after confirmed activation. This handles lost start or activation responses
without blind retry or leaked capacity.

The provisional deadline is 40 seconds from first publication, longer than
the controller's common 24-second start exchange plus two 5-second activation
attempts. A retired request retains its immutable fingerprint and typed
terminal error for 60 seconds. Admission reserves room in the bounded
256-entry active-plus-terminal history instead of evicting an unexpired
request and permitting a delayed replay to open a second tuner.

### 4.2 Settings keys (`crates/plurx-core/src/store/mod.rs`)

| Key | Default | Bound |
|---|---:|---:|
| `live_tv.enabled` | `0` | Boolean |
| `live_tv.device_ipv4` | empty | Canonical private/link-local IPv4 |
| `live_tv.owner_node_id` | empty | Existing reachable committed voter |
| `live_tv.max_sessions` | `2` | `1..=min(TunerCount, 4)` |
| `live_tv.output_height` | `720` | `720` or `1080` |
| `live_tv.config_generation` | `0` | Monotonic nonnegative `i64`; server-managed |
| `live_tv.transition_from_owner_node_id` | empty | Original owner awaiting drain/fencing; server-managed |
| `live_tv.transition_drain_before` | `0` | Fixed cutoff preserved through disabled edits; server-managed |

The Settings PUT remains PATCH-shaped but reads and validates the complete
effective live-TV tuple from one snapshot before its first write. Any changed
live-TV field uses an expected-generation compare-and-swap transaction to
write the complete tuple and its next generation in one replicated batch; a
half-saved combination or two different tuples at the same generation must
never be visible.

### 4.3 Public HTTP (`crates/plurxd/src/http/live_tv.rs`)

```text
GET    /api/v1/live-tv/readiness                       AdminUser
POST   /api/v1/live-tv/readiness/refresh               AdminUser
GET    /api/v1/live-tv/channels                        AuthUser
POST   /api/v1/live-tv/channels/{channel}/sessions     AuthUser
GET    /api/v1/live-tv/sessions/{capability}/index.m3u8 capability
GET    /api/v1/live-tv/sessions/{capability}/status     capability
PUT    /api/v1/live-tv/sessions/{capability}/keepalive  capability
DELETE /api/v1/live-tv/sessions/{capability}            capability
GET    /api/v1/live-tv/sessions/{capability}/{segment}  capability
```

Start success:

```json
{
  "session_id": "ltv1.<owner>.<random>",
  "playlist_url": "/api/v1/live-tv/sessions/<capability>/index.m3u8",
  "channel": { "id": "7.1", "guide_number": "7.1", "guide_name": "WABC" },
  "live": true,
  "output": { "container": "hls", "video": "h264", "audio": "aac", "height": 720 }
}
```

Capability routes bypass account auth exactly as existing HLS media does, but
all start and lineup routes require a user. Maintenance admits existing
capability reads and DELETE while refusing lineup refresh and new start.
Learners may relay/serve existing owner capabilities but may not become the
tuner owner or start a new session.

### 4.4 Internal HTTP (`crates/plurxd/src/http/internal_live_tv.rs`)

```text
POST /_internal/v1/live-tv/start
POST /_internal/v1/live-tv/activate
POST /_internal/v1/live-tv/snapshot
POST /_internal/v1/live-tv/drain
POST /_internal/v1/live-tv/resource
POST /_internal/v1/live-tv/stop
```

Every body is at most 16 KiB, uses `deny_unknown_fields`, names the expected
owner, and is bound to method, path, body, timestamp, nonce, source node, and
target node by exact-request Ed25519 signatures. MPEG-TS responses stream
with bounded backpressure; small control responses and playlists are buffered
under explicit limits to authenticate their complete bytes. Local segment
responses admit four concurrent readers per session, retain at most two
64 KiB queued chunks each, and expire after five seconds without downstream
progress or 30 seconds total, including unpolled response bodies.

`start` is idempotent by request UUID plus immutable request fingerprint and
answers `created` or `recovered`. `activate` idempotently confirms the same
provisional activation token and returns `activated` on every matching retry.
`snapshot` runs on the selected owner and is the sole source of device
readiness and lineup facts.
`drain` fences a proposed configuration generation, waits for every old
session/child, and signs its acknowledgement. All requests include the
expected protocol capability and configuration/serving generations. A
mismatch fails closed and drains a stale provisional session.

## 5. Data flow — one physical lease from click to cleanup

```text
 client                  ingress node                 tuner owner           HDHR
   │ GET channels             │ signed owner snapshot     │                   │
   │─────────────────────────▶│──────────────────────────▶│ GET lineup        │
   │                          │◀──────────────────────────│◀──────────────────│
   │◀─────────────────────────│ cached/public projection │                   │
   │ POST Watch               │                           │                   │
   │─────────────────────────▶│ signed peer start         │                   │
   │                          │──────────────────────────▶│ take permits       │
   │                          │                           │ GET /auto/v7.1     │
   │                          │                           │──────────────────▶│
   │                          │          first segment    │◀══════════════════│
   │                          │◀── provisional capability│                   │
   │                          │ signed activate          │                   │
   │                          │──────────────────────────▶│                   │
   │ playlist capability      │◀──────────────────────────│                   │
   │◀─────────────────────────│                           │                   │
   │ GET playlist / segment   │ signed streaming relay   │                   │
   │─────────────────────────▶│──────────────────────────▶│                   │
   │◀═════════════════════════│◀══════════════════════════│                   │
   │ DELETE / idle            │ signed stop               │                   │
   │─────────────────────────▶│──────────────────────────▶│ kill + wait ──────╳
```

The double line is streaming backpressure. The request owner drops the
upstream response when the downstream disconnects, but the live session stays
alive until explicit close or 45 seconds without any capability touch; one
temporary segment fetch ending must not switch the channel off.

## 6. Reliability and observability — absence is not a green state

### 6.1 Cache and retry policy

Successful device and lineup snapshots live for 30 seconds on the selected
owner and are keyed by owner plus configuration generation. A refresh is
single-flight. A non-owner may cache the sanitized signed snapshot for the
same 30 seconds, but the owner remains its authority. A failed refresh may
serve the last successful snapshot for at most 5 minutes and marks it stale;
after that, channel reads fail with `device_unavailable`. Session starts always
require a fresh owner-side channel row so a removed or newly DRM-tagged channel
cannot ride a stale cache into a tuner GET.

Document reads have no automatic retry. A peer start may retry only with the
identical request UUID; the owner's provisional registry recovers the same
result and never opens a second upstream. A device stream GET is attempted
exactly once and is never automatically retried before headers, after headers,
or after bytes begin. The viewer retries a definitive failure by pressing
Watch, which creates a new request UUID.

### 6.2 Metrics and activity

Expose:

```text
plurx_live_tv_enabled
plurx_live_tv_device_ready
plurx_live_tv_lineup_channels{support="ready|drm_unsupported"}
plurx_live_tv_sessions{state="starting|active"}
plurx_live_tv_starts_total{outcome="..."}
plurx_live_tv_session_ends_total{reason="..."}
plurx_live_tv_relay_bytes_total
```

Settings readiness shows device identity, firmware, channel counts, selected
owner, active/max sessions, last successful lineup time, and the most recent
sanitized error. Activity shows live-TV sessions separately from finite media
and identifies channel number/name, user, owner node, encoder, age, and output
height. It never shows the device URL or capability.

**How to read it:** enabled + device_ready 0 means configuration exists but
the owner cannot currently prove the device; sessions pinned at `starting`
beyond 15 seconds are a cleanup bug; `tuner_unavailable` rising with active
sessions below the plurx cap usually means another HDHomeRun client, weak
signal, or channel authorization—not permission to raise the cap blindly.

## 7. Non-goals — guardrails for the first complete release

- **No DRM playback or circumvention.** plurx has no licensed protected-media
  path, and a retry loop does not create one.
- **No DVR, recording schedule, pause/rewind buffer, EPG, or guide-data
  subscription.** `lineup.json` is a channel list, not a program guide. Each is
  a separate storage/product contract.
- **No channel scan control.** Scan from the HDHomeRun application; plurx reads
  the resulting lineup and cannot strand the household in a retune.
- **No same-multiplex sharing.** One HTTP GET is one tuner lease. Sharing needs
  a transport-stream demux and independent downstream lifetime accounting.
- **No automatic owner takeover.** Restarting playback after an owner failure
  is honest; two owners believing they each inherited four tuners is not.
- **No lineup-supplied arbitrary URL following.** The configured private IPv4
  is the complete SSRF boundary.
- **No silent captions claim.** ATSC 608/708 captions are marked unsupported
  until an end-to-end fixture proves the transcode and all three clients.
- **No compile-time feature gate.** Operational readiness belongs in Settings,
  not in a binary variant an operator cannot diagnose.

## 8. Milestones — reviewable PRs into one effort branch

### 8.1 M0 — plan, review, and project status

Create this plan and [HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md),
link the live GitHub status issue, and run an independent adversarial review.
Resolve every critical/high finding in the plan before opening the task PR.
After the PR exists, an adversarial agent reviews its exact diff. Implement
accepted findings, rerun focused proof, and request re-review until the verdict
is Approve before merging to the effort. Update both status pages after the
commit, PR, review, fixes, gate, and merge.

**Acceptance:** `git diff --check` · documentation contract checks · adversary
verdict is Approve or every Request Changes item is incorporated and recorded.

### 8.2 M1 — device boundary, settings, and lineup API

Add keys, atomic settings validation, the redirect-free bounded HDHomeRun
client, device/lineup cache, typed errors, readiness endpoints, channel API,
route policy, the `live_tv_v1` heartbeat capability, owner snapshot relay, and
parser/mock-server tests. Do not start FFmpeg yet. Open the task PR to the
effort, obtain exact-diff adversarial review, implement accepted findings, and
re-review to Approve before merge. Record branch, PR/SHA, evidence, review,
blockers, decisions, gate, and merge in both status pages as each changes.

**Focused acceptance:** parser and HTTP boundary tests, including hostile
redirect/proxy behavior · Settings generation/wire tests · route-matrix and
mixed-version capability tests · concurrent settings writers prove CAS and
single-snapshot reads · pinned `cargo check`, Clippy, and static no-feature-
gate contract.

### 8.3 M2 — live engine, shared admission, and cluster relay

Add always-compiled foreground transcode admission, the one-open FFmpeg live
session fed through validated bounded stdin, sliding HLS resource serving,
capability redaction, idempotent provisional start/activation, selected-owner
peer relay, configuration/serving fences, lifecycle reaper, shutdown,
activity, and metrics. Open the task PR to the effort, obtain exact-diff
adversarial review, implement accepted findings, and re-review to Approve
before merge. Record every state transition in both status pages.

**Focused acceptance:** synthetic endless-source suite · one-GET/no-retry and
startup-versus-endless-body deadline suite · scratch/resource budget suite ·
lost start/activation response, replay fingerprint, and idempotency suite ·
concurrency and leak suite ·
partition/owner-transition two-node relay suite · pinned check/Clippy/rustfmt ·
`cargo check` and focused tests with `--no-default-features`.

### 8.4 M3 — web Live TV

Add `#/live`, navigation, accessible channel cards, readiness/unsupported
states, the live-player adapter, HLS startup/status/keepalive, channel switch,
and cleanup. Extend the UI golden and browser contract tests.
Open the task PR to the effort, obtain exact-diff adversarial review, implement
accepted findings, and re-review to Approve before merge. Record every state
transition in both status pages.

**Focused acceptance:** Node policy/unit tests · UI structure golden ·
Playwright live fixture, including zero progress/scrobble calls, exactly one
DELETE on close/switch, and typed recovery after owner loss, producer stall,
capability expiry, and background idle reap.

### 8.5 M4 — Apple and Android parity

Add API models, navigation destination, channel list, live player mode, native
HLS attachment, and cleanup to iOS/tvOS and Android/mobile-TV. Reuse visual
tokens and player surfaces; keep finite-file state out of the live adapter.
Advance the repository mobile version for this client-visible change. Open the
task PR to the effort, obtain exact-diff adversarial review, implement accepted
findings, and re-review to Approve before merge. Record every state transition
in both status pages.

**Focused acceptance:** Swift unit/UI model tests and iOS/tvOS compile · Android
JVM tests and debug compile · route/control parity assertions · typed recovery
after owner loss, producer stall, capability expiry, and background idle reap.

### 8.6 M5 — operations, hardware acceptance, and final qualification

Update README, FEATURES, REQUIREMENTS, ROADMAP, ARCHITECTURE, PLAYBACK,
OPERATIONS, CHEATSHEET, SECURITY, deployment networking, and native API
contracts. Add the validation functionality point and hardware acceptance
script. Exercise one real HDHomeRun where reachable; record unavailable
channels/firmware as evidence, never fabricate a pass.

Open the documentation/hardware task PR to the effort, obtain exact-diff
adversarial review, implement accepted findings, and re-review to Approve
before merge. Record every state transition in both status pages.

Freeze task merges, merge current `main` into the effort, open the effort PR to
`main`, run one adversarial whole-diff review, implement every accepted
finding, and then run the complete Main promotion qualification exactly once
on the final tree. A code fix after qualification invalidates the receipt and
requires one replacement full run.

**Acceptance:** every task PR's focused proof and Effort development gate are
green · the frozen promotion PR's Main promotion gate and qualification receipt
are green/current · merge commit reaches `main`.

## 9. Adversarial review ledger — decisions survive the review

Record plan and PR findings here as they arrive. Each row names the disposition
so an accepted risk cannot disappear into a resolved conversation.

| Review | Finding | Disposition |
|---|---|---|
| Architecture pre-review | VOD `Presentation::Live` still assumes a finite `MediaFile` | Accepted: separate `LiveTvManager`; no file rows or VOD session reuse |
| Architecture pre-review | Per-voter capacity multiplies physical tuners | Accepted: explicit owner and signed relay in §2.5 |
| Protocol pre-review | Four CONNECT/FLEX 4K tuners are not four interchangeable ATSC 3.0 tuners | Accepted: default two; runtime limit never exceeds reported count; no capacity promise by channel type |
| Protocol pre-review | `503` is ambiguous and DRM is a hard boundary | Accepted: typed but non-speculative errors in §2.3 and fail-closed DRM in §3.3 |
| Protocol pre-review | Lineup URLs are an SSRF boundary | Accepted: derive all URLs from one validated IPv4 in §3.1 |
| Protocol pre-review | A second probe connection can consume another tuner | Accepted: one FFmpeg ingest in §2.3 |
| Protocol pre-review | Slow consumers and endless input can create unbounded state | Accepted: fixed window, backpressure, idle reap, and lifecycle tests in §2.4 |
| Plan adversary | FFmpeg opening a URL bypasses pinned redirect/proxy policy | Accepted: proxy-free Reqwest owns the only GET and pumps bounded bytes to `pipe:0` in §2.3/§3.1 |
| Plan adversary and subsequent Astra review | Disable, owner/device change, or quorum loss can leave an old owner consuming tuners | Configuration/serving generations plus authenticated drain; automatic lease-expiry recovery rejected in favor of exact admin physical fencing in §2.5 |
| Plan adversary | A lost peer-start response makes retries duplicate or leak a tuner | Accepted: stable request UUID, provisional lease, recovered response, and signed activation in §4.1/§4.4 |
| Plan adversary | Ingress-local lineup/readiness does not prove owner access | Accepted: authoritative owner snapshot relay in §2.5/§4.4 |
| Plan adversary | Mixed-version ingress/owner nodes can disagree on the protocol | Accepted: all active serving nodes and owner must advertise `live_tv_v1` in §2.5 |
| Plan adversary | Client polling can conceal a frozen producer | Accepted: separate upstream and producer-progress watchdogs in §2.4 |
| Plan adversary | Codec-name presence does not prove the production graph | Accepted: readiness exercises the selected encoder/filter/IDR/AAC/HLS graph in §2.1 |
| Plan adversary | Generic resources permit traversal and unbounded responses | Accepted: closed resource enum, inventory validation, byte/time limits in §4.1 |
| Plan adversary | HLS flags did not guarantee an endless or bounded window | Accepted: `omit_endlist`, deletion threshold, inventory and byte budgets in §2.4 |
| Plan adversary | Every PR needs exact-diff review and status evidence | Accepted: review/re-review/status gate is explicit in every §8 milestone |
| Plan adversary | URL hostname equality can reject real `.local` firmware data | Accepted: advertised hosts are never resolved; safe paths are rebuilt on the pinned IPv4 in §3.1 |
| Plan adversary | No-feature-gate intent lacked executable proof | Accepted: static contract plus pinned no-default compile/focused test in §8.2/§8.3 |
| Plan adversary | Stale and failed live sessions lacked client recovery contract | Accepted: freshness fields and typed Watch-again behavior in §2.2/§2.6 |
| Plan re-review | Concurrent settings writers could publish different tuples at one generation | Accepted: expected-generation CAS and single-snapshot tuple reads in §2.5/§4.2 |
| Plan re-review | Activation retry and request-id payload binding remained ambiguous | Accepted: immutable request fingerprint plus idempotent activation response in §4.1/§4.4 |
| Plan re-review | Tuner GET retry and endless-body timeout remained ambiguous | Accepted: exactly one attempt under every outcome; startup envelope ends after first segment and only progress watchdogs govern the active body in §3.1/§6.1 |

## 10. Decisions to revisit — chosen autonomously for this effort

1. **Manual IPv4 is the supported pairing path.** UDP broadcast discovery is
   unreliable from Docker bridge networks; a visible address field and Test
   connection work everywhere the data path can work. Revisit host-network
   discovery only after it can diagnose namespaces and VLANs.
2. **The tuner owner is explicit.** Automatic failover without a replicated
   physical lease can double-allocate. Revisit after live-session leases have
   a store schema and fencing token.
3. **720p is the default normalization.** It keeps ATSC 1.0 interlaced sources
   within the measured class of home hardware. 1080p is available after the
   owner proves its selected encoder. Revisit 2160p when a non-DRM ATSC 3.0
   fixture and AC-4-capable engine exist.
4. **Web, Apple, and Android ship together.** The API is not called complete
   while first-party living-room clients cannot open it. Roku and third-party
   Plex Live TV compatibility remain outside this effort.
5. **Captions are explicitly unsupported at first.** Silence would imply a
   feature that accessibility users discover missing only after playback.

## 11. Sources — what the protocol claims and what it does not

- SiliconDust's [HTTP development guide](https://www.silicondust.com/hdhomerun/hdhomerun_http_development.pdf)
  defines `lineup.json`, guide numbers/names/tags/URLs, one tuner allocation
  per HTTP stream, MPEG-TS delivery, TCP-close release, and the ambiguous
  `404`/`503` errors. It is dated 2014 and predates ATSC 3.0, so this plan uses
  it for HTTP lifecycle—not as proof of modern codec/container compatibility.
- The current [device discovery API](https://info.hdhomerun.com/info/discovery_api)
  defines device id, tuner count, base and lineup URLs, and the official
  discovery library. This effort validates those reported fields but keeps
  manual IPv4 as its deployable pairing contract.
- SiliconDust's [FLEX hardware table](https://info.hdhomerun.com/info/flex)
  states that the FLEX 4K has four tuners, only two of which accept ATSC 3.0.
- SiliconDust's [DRM requirements](https://info.hdhomerun.com/info/drm) state
  that protected channels require an approved protected path and that common
  third-party clients, including Plex and VLC, can access only unprotected
  channels.
- The upstream [libhdhomerun source](https://github.com/Silicondust/libhdhomerun)
  is the current reference for discovery, lock, receive-buffer, and tuner
  cleanup semantics. plurx does not link it in this effort, avoiding a C/FFI
  packaging and licensing surface for behavior the HTTP API already supplies.
