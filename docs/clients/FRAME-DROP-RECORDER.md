# Automatic web frame-drop recorder

**Status:** built · deployment and production incident acceptance pending. The recorder
collects evidence automatically when a browser's dropped-frame counter grows.
It does not establish the cause of the Avatar presentation losses. Paul owns
deployment; the next useful evidence is a naturally occurring incident from
the deployed player. [Avatar investigation](AVATAR-WEB-PLAYBACK-STATUS.md).

## Observation and ownership

All web transports use the same recorder. It observes the existing 500 ms
playback sampling tick and the existing owned video-frame callback. It adds
no frame subscription, playback request, rendering work, screenshot or console
stream. Browsers without native callback metadata retain the state evidence;
unavailable measurements remain null. A supported Long Tasks observer adds
main-thread scheduling evidence; unsupported browsers record that limitation.

The first counter reading establishes a baseline. A lower total or dropped
counter closes any pending incident as a reset, then establishes a new baseline.
An increase freezes the available history and gathers ten seconds of additional
state. Stop, attachment replacement and pagehide flush a shorter tail. Further
increases during that tail update the same incident. The per-player allowance
is three incidents with at least sixty seconds between triggers; it survives
attachment changes within that player. A new player starts a new allowance.

Every callback and sample checks player, element and attachment ownership.
Identity is frozen before asynchronous storage/upload: file, session, attempt,
method, browser label and attachment ID cannot be borrowed from a successor.
Account changes and diagnostic-history opt-out discard pending captures,
remove listeners and abort in-flight uploads. Clear diagnostic history also
clears this recorder and invalidates queued asynchronous writes/uploads.
Clearing cannot retract an incident already accepted by the server.

## Bounds and storage

| Evidence | Bound |
|---|---|
| State before trigger | Available last 60 seconds; at most 144 rows |
| Native callbacks before trigger | Available last 60 seconds; last 512 rows |
| Native callbacks after trigger | First 128 rows during the tail |
| Lifecycle events | 64 historical rows and up to 64 additional rows |
| Request timings | Last 16 available current-attachment transport rows |
| Serialized incident | 49,152 bytes; remove rows and disclose trim counts |
| Local fallback | Last three current-account incidents, 24-hour expiry |
| Upload | Existing authenticated client-log route, 8-second timeout |

At film frame rates the callback ring covers less than sixty seconds. State
history still covers the available sixty-second window. No request is made to
populate Resource Timing: only existing resource observations matching the
frozen owned playlist (including hls.js blob-backed playback) or progressive
transport origin/path and attachment start are copied. Resource rows
contain numeric kinds, sequence and timings/bytes; URLs and query credentials
are never copied. Missing timing entries do not prove a request did not occur.

The dedicated IndexedDB database `cinema-frame-drop-v1` falls back to bounded
memory when unavailable. It does not compete with consented support archives.
Other-account records are removed when the store is accessed after an account
change; logout clears it. Expired records are pruned on store access. One unsent
local incident is retried on the next attachment. HTTP success marks it submitted
but retains the local copy until expiry or eviction.

## Retrieval and interpretation

The existing [client-log API](../API.md#55-post-apiv1client-log) stores a valid
version 1 incident under `PlaybackEvent.extra.frame_drop`. The regular server
join adds available session producer/delivery facts. An administrator retrieves
incidents from the API node that received the report:

```text
GET /api/v1/system/playback-events?event=frame_drop&since=<epoch-ms>&limit=500
```

Storage is node-local, and the ingress API node need not be the transcode node.
Existing playback telemetry retention applies (default thirty days; zero
disables persistence). Client-log always returns 204, including suppression by
its limiter: a submitted flag is best-effort HTTP delivery, not durable receipt.
Browser termination can lose a pending tail or request; local retention is also
best effort when browser storage is unavailable or cleared.

`clock_origin_ms + relative_ms` aligns observations approximately with wall-clock
server/native traces. Browser quality counters may refresh in batches: trigger
is the polling observation, and `observed_from_ms` is the preceding change in
quality metrics, not a proven time of physical loss. Native callback receipt,
expected-display and presentation timestamps describe browser/compositor work;
they do not prove physical display scanout. Healthy buffer/decode observations
do not identify the compositor cause. The recorder cannot start an OS profiler.
A separate macOS Animation Hitches trace can expose image-queue/swap/surface
lifetimes; correlating such a trace remains necessary for a physical-loss claim.

## Version 1 wire matrices

All matrix cells are finite numbers or null. Relative times and durations below
are milliseconds. Only allowlisted fields and bounded numeric matrices are
accepted by the server; arbitrary string payloads are rejected.

| Matrix | Ordered columns |
|---|---|
| `samples` | observation time, media time, total frames, dropped frames, corrupted frames, ready state, flags, buffer runway, tick lag beyond 500 ms, callback receipts, seek sequence |
| `frames`, `post_frames` | callback receipt time, callback now, media time, presented frames, expected display time, presentation time, processing duration |
| `events` | observation time, event code, duration where applicable |
| `resources` | resource kind, segment sequence, request start, response start, response end, encoded body size, transfer size |
| `trimmed` | removed state rows, removed pre-trigger frames, removed events, removed post-trigger frames |

Flags: paused 1, seeking 2, document hidden 4, document focused 8, active wait 16.
Resource kinds: initialization 0, media segment 1, playlist 2, progressive 3.
Event codes: play 1, pause 2, seeking 3, seeked 4, waiting 5, playing 6,
stalled 7, loadedmetadata 8, loadeddata 9, canplay 10, emptied 11, error 12,
ratechange 13, visibilitychange 14, focus 15, blur 16, long task 17,
counter reset 18. Trim counts include rows removed while rolling the history.

## Validation

`tests/playback/frame-drop-recorder.test.js` exercises batched counters, resets,
coalescing, budgets, byte/row bounds, missing measurements, frozen ownership,
stale callbacks, prepared element handoffs, account/history changes, queued-write clearing and URL removal.
Astra reviewed the implementation independently. Four findings were addressed:
separate post-trigger event capacity, chronological local retention despite UUID
ordering and submission updates, frozen playlist matching for blob-backed HLS,
and memory retention through IndexedDB transaction failure/recovery. Each has a
focused regression; this records the single review and repairs, not a re-review.
The two `frame_drop_incident_` tests in
`crates/plurxd/src/http/system/frame_drop.rs` exercise typed telemetry round-trip
and rejection of oversized, unknown/string and invalid-bound payloads. Existing
asset-order/load, hitch-lifecycle and diagnostic-recorder checks cover integration.
Production capture acceptance and the original loss cause remain open.
