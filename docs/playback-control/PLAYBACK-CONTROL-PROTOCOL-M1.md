# Playback control protocol M1

M1 establishes the fenced, observable control path described in
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md). It is
behavior-neutral: the only action is `none`, and a newly accepted exchange
renews the selected engine's existing activity clock. It does not replace a
stream, change quality, alter production, or remove a watchdog yet.

## Enable the preview

The replicated setting `playback.control_protocol_v1` defaults off. An admin
can enable **Advertise playback control protocol v1** under Playback settings.
Only sessions created while it is enabled advertise `control`; disabling the
setting does not mutate the contract of a session already in progress.

An enabled session start includes:

```json
{
  "control": {
    "protocol": "plurx-playback-control-v1",
    "url": "/api/v1/hls/SESSION/control",
    "generation": "DELIVERY-INCARNATION-UUID",
    "control_epoch": 1,
    "next_exchange_ms": 5000,
    "lease_timeout_ms": 300000
  }
}
```

`lease_timeout_ms` reports the lifetime the selected compatibility engine
actually enforces: 300,000 ms for VOD and 60,000 ms for rolling live recovery.
It does not claim the actor-owned lease planned for M3.

## Exchange

Send a body no larger than 16 KiB to the advertised URL. Unknown fields and
enum values are rejected. The first sequence in an owner epoch is `1`; later
sequences strictly increase. One controller UUID is bound for the epoch.

```json
{
  "protocol": "plurx-playback-control-v1",
  "generation": "DELIVERY-INCARNATION-UUID",
  "control_epoch": 1,
  "client_instance_id": "PLAYER-CONTROLLER-UUID",
  "sequence": 1,
  "demand": "active",
  "position_ms": 120000,
  "buffered_from_ms": 118000,
  "buffered_through_ms": 145000,
  "playback_rate": 1.0,
  "render_state": "rendering",
  "seek_target_ms": null,
  "observed_download_bps": 18000000,
  "selection": {
    "quality": { "mode": "auto" },
    "audio_track": 0,
    "subtitle": { "mode": "off", "track": null },
    "audio_offset_ms": 0,
    "codec": "auto",
    "dynamic_range": "auto"
  },
  "capabilities": {
    "platform": "web",
    "max_height": 2160,
    "codecs": ["h264", "hevc"],
    "dynamic_ranges": ["sdr", "hdr10"],
    "dual_player_preparation": false
  },
  "observation": null,
  "acknowledgement": null
}
```

The response is a point-in-time join of the client report, server delivery
facts, durable owner tuple, and absolute legacy activity expiry:

```json
{
  "protocol": "plurx-playback-control-v1",
  "generation": "DELIVERY-INCARNATION-UUID",
  "control_epoch": 1,
  "accepted_sequence": 1,
  "server_time_unix_ms": 1787700000000,
  "lease": {
    "state": "active",
    "renew_after_ms": 5000,
    "expires_at_unix_ms": 1787700300000
  },
  "delivery": {
    "presentation": "vod",
    "producer_state": "complete",
    "produced_through_ms": 7200000,
    "fetched_through_ms": 145000,
    "delivered_bps": 12000000,
    "delivered_idle_ms": 240,
    "recent_producer_speed": null,
    "client_runway_ms": 25000,
    "admitted": true,
    "hold_reason": null,
    "owner_node_hash": "n-0123456789abcdef",
    "owner_epoch": 1
  },
  "effective_selection": {
    "quality_auto": true,
    "height": 1080,
    "audio_track": 0,
    "subtitle_burn": null,
    "audio_offset_ms": 0,
    "codec": "source",
    "dynamic_range": "dolby_vision"
  },
  "action": { "type": "none" }
}
```

An equal-sequence retry returns the prior action outcome and the unchanged
absolute expiry; it does not renew activity. A lower sequence, another client
instance in the same epoch, a stale generation, or an acknowledgement for an
action M1 never issued receives `409 stale_control`. A previous owner epoch
receives `409 owner_changed` with the current generation and epoch. An expired
owner lease receives `425 owner_transition` and a bounded retry hint.
Advancing faster than the 250 ms per-session admission floor returns
`429 control_rate_limited`; media delivery remains independent.

## Cluster and authorization

The public session UUID is still the bearer capability. Every ingress performs
an authoritative durable-route lookup. If the owner is remote, the ingress
sends a bounded envelope to a dedicated exact-write-authenticated internal
endpoint containing the expected generation, owner node, and owner epoch. The
worker repeats those checks against the durable route, then the session repeats
generation, epoch, client, and sequence checks against owner-local state before
activity can move. The generic read/media relay is not used for control.

## Instrumentation

`/metrics` exports `plurx_playback_control_exchanges_total` with one bounded
`outcome` label: `accepted`, `replay`, `invalid`, `stale`, `owner_changed`,
`owner_transition`, `session_gone`, `unavailable`, or `rate_limited`.
`plurx_playback_control_platform_exchanges_total` joins accepted/replayed
outcomes to the bounded `web`, `apple`, and `android` platform values.
`plurx_playback_control_relays_total` and
`plurx_playback_control_relay_seconds` expose bounded remote-owner outcomes
and full relay latency. Debug events contain only a hashed session
correlation, epoch, accepted sequence, replay flag, and the advertised legacy
timeout. Raw session capabilities, generation UUIDs, client UUIDs, node names,
URLs, and client error text are never metric labels or log fields.

M1 does not yet add client reporters. Existing media requests and all current
recovery behavior remain the fallback until M2 has shipped and been measured.
