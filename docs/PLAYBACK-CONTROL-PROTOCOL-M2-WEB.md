# Playback control protocol M2: web shadow reporter

This slice adds the browser half of M2 from
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md). It is a
passive reporter only: it tells the server what the player is actually doing,
but the server still returns `action: none` and every existing playback
recovery remains authoritative.

The reporter starts only when a session response advertises `control`. The
replicated `playback.control_protocol_v1` setting still defaults off, so this
change is inert unless an operator enables the M1 preview and opens a new HLS
session.

## State sent

The browser sends a new snapshot on the server-advertised cadence and
immediately after play, pause, seek start/settle, rate change, waiting,
rendering, failure, end, subtitle selection, or visibility change. The
snapshot includes:

- explicit `active`, `hold`, or `end` demand;
- file-local film position and the contiguous buffered range containing it;
- `starting`, `rendering`, `waiting`, `stalled`, `seeking`, `ended`, or
  `failed` render state;
- playback rate, seek target, and hls.js's bandwidth estimate when available;
- current quality, audio, subtitle, AV-offset, codec, and dynamic-range intent;
- bounded web codec/range/height capabilities on sequence one and whenever
  that capability snapshot changes; and
- dropped-frame and decoder/error evidence where the browser exposes it.

One exchange may be in flight. New callbacks replace the queued snapshot
instead of allocating more requests, and the reporter enforces the server's
250 ms admission floor before sending the newest state. Sequences are assigned
only when an exchange starts. A transport failure retries the exact
unacknowledged sequence and body; a newer player snapshot stays queued for the
next sequence. Typed `425 owner_transition`, `429 control_rate_limited`, and
`503 control_unavailable` responses preserve the exact request and honor the
bounded server retry hint. A fenced `409 owner_changed` response abandons the
old request, adopts the returned generation/epoch, and sends a fresh sequence
one snapshot. Terminal authorization/stale-session errors and a response that
violates the passive protocol stop visibly instead of retrying forever.
Closing or replacing the player cancels its
request without turning cancellation into a playback failure.

A six-second client exchange deadline bounds a transport that never returns;
it is two seconds beyond M1's complete four-second ingress/relay budget. Its
expiry is reported as a control transport failure and retries the exact
request. This is a network-operation deadline, not a playback watchdog, and it
has no media mutation authority.

The response must match the advertised protocol, generation, control epoch,
and exact request sequence. M2 accepts only `action: none`; it cannot silently
gain recovery authority if a newer server sends an active action before the
client action milestone is deployed.

## Instrumentation

The playback information panel exposes shadow mode directly. Standard mode
shows whether the control exchange has connected and its last accepted
sequence. Debug mode also shows in-flight/queued state, the server-advertised
cadence, the actual legacy lease, and the most recent control failure. A
successful exchange clears a prior failure.

Client-side transport/protocol failures also reach Settings → Logs on the
first occurrence and at most once per minute while they persist; recovery is
logged once when the next exchange is accepted. These events state explicitly
that legacy playback recovery remained authoritative.

Every existing web `stall`, `stall_recovery`, `quality_switch`, stream rescue,
and failure beacon carries the last client-reported accepted control epoch,
sequence, demand, render state, position, runway endpoint, and observed
download rate. Persistent wait, startup stall, hls.js fatal, media failure, and
truncated-stream recovery also carry the exact current trigger snapshot because
legacy recovery cannot wait for that asynchronous exchange to be accepted.
The server validates bounds and enums, hashes the generation, and labels these
two objects `control_accepted_client` and `control_trigger_client`; they remain
client assertions unless a later authoritative server join confirms them.
This provides the shadow comparison required before any recovery owner is
removed without persisting a raw generation identifier.

M1's fixed-cardinality server metrics remain the fleet view:
`plurx_playback_control_exchanges_total`,
`plurx_playback_control_platform_exchanges_total`, and the relay counter and
latency histogram.

## Watchdogs that still exist

This PR removes no watchdog. That is intentional: M1 has no active action and
the M3 actor/lease does not exist yet. Removing a legacy recovery now would
turn the preview into a less reliable fallback instead of measuring it.

| Existing owner | What it still does | Why it remains in M2 |
|---|---|---|
| `waitTimer` / `persistentWait` | After an eight-second media wait, records the stall and directly reconnects or enters transcode fallback once | The control endpoint can observe the stall but cannot yet authorize a replacement |
| `stallTimer` / `armStall` / `stallDiagnose` | Bounds cold-start and seek startup, probes the stream, and exposes retry/transcode choices | The actor-owned producer deadline and single client progress deadline are not implemented |
| hls.js fatal and media-error handlers | Destroy/recreate a rejected stream or enter the compatibility transcode | M2 rejects active protocol actions; removing these would strand clients on errors |
| `maybeDecodeRescue` and the automatic fallback claim | Detect a non-rendering decode path and open one compatibility transcode | Decoder evidence is reported, but there is no server arbiter yet |
| `autoControllerTick`, ABR state, and `switchAutoRung` | Poll server/client evidence and directly reopen at another rung | Prepared, idempotent protocol handoff is M5.5/M6; M2 cannot switch transparently |
| `pollSessionHealth` | Reads `/status` for the stats panel and legacy ABR decisions | M2 response facts are not yet the authority for ABR, and direct/progressive transports do not report control |
| `handleEnded` truncated-stream branch and `endedTries` | Treats media ending before title runtime as failure and directly reopens up to its retry budget | The protocol reports the failed trigger, but `action: none` cannot yet authorize the required successor |
| server live-session idle/startup/progress/wait watchers | Reap inactive sessions and recover or fail stuck producers | They move into the M3 actor and M4 producer deadline only after passive equivalence is measured |

The reporter's cadence timer is not a playback watchdog: it only sends a
snapshot. It cannot reopen, destroy, replace, pause, or seek media. The final
three-watchdog design and the deletion mapping for every legacy symbol are in
the plan's §7.

## Rollback and current limitations

Turn off **Advertise playback control protocol v1**. New sessions omit the
bootstrap and this client creates no reporter; playback remains on the exact
legacy behavior. Existing sessions keep the contract they started with until
they end.

Browser fetches use the advertised relative URL on the current ingress.
Cross-origin alternate-ingress retry cannot be claimed without an explicit
CORS/auth contract. Cluster owner relay is already handled behind that URL;
native alternate-ingress behavior remains part of their M2 slices.

This slice does not change quality, HDR/Dolby Vision, audio, subtitle, cluster
owner, or session generation in response to control. Those become prepared,
fenced actions only after the passive evidence and actor milestones pass.
