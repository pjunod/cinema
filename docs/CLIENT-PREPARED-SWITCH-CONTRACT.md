# The prepared-switch contract, for the three client adapters

**Status:** server half shipped; no client implements it · **Updated:**
2026-09-08 · **Audience:** whoever builds the Apple, Android or web adapter

This is the one document a client adapter needs. The server side of §4 is
merged and will not change under you: `prepare_replacement` is offered,
acknowledgements are accepted, the commit is durable, and the predecessor
drains. What does not exist is a client that uses any of it.

The three adapters are independent work in three codebases and can be built in
parallel. They share this contract and nothing else. Where this document and
the code disagree, the code wins and this document is wrong — say so rather
than working around it.

Companion reading, in this order and only as far as you need it:
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) for how the control
plane came to be shaped this way, and §4 of
[STREAMING-RELIABILITY-HANDOFF.md](STREAMING-RELIABILITY-HANDOFF.md) for the
server-side transaction and the drain.

## What this buys the viewer

Today a quality change, a track change or a resolution change tears the stream
down and builds a new one. The worst measured interruption is 2,246 ms on
Safari; Apple's is unmeasured because Apple never exercised that path at all.

The prepared switch is make-before-break: the server stages a successor while
the current stream keeps playing, the client builds it, and the swap happens on
a frame the client has already drawn. Nothing about it is seamless unless the
client actually holds both and swaps at the right moment — a client that tears
down the predecessor when the offer arrives has implemented the old behaviour
with extra steps.

## The exchange, end to end

The control exchange already runs every ~5 s per session
(`NEXT_EXCHANGE_MS`). Everything below rides that same request/response; there
is no second endpoint and no push channel.

### 1. Declare the capability

Send `supported_actions` containing `"prepare_replacement"`.

**The server never sends an action a client has not named.** That is what makes
the shipped server half safe against every client that exists today, and it is
also why a typo here fails silently rather than loudly: you simply never get an
offer. Unknown names in the list are ignored, not refused, so a later client
may name actions this server has not heard of.

### 2. Receive the offer

The response's `action` becomes:

```json
{ "type": "prepare",
  "action_id": "<uuid>",
  "session_id": "<successor session id>",
  "playlist_url": "/api/v1/hls/<session>/index.m3u8",
  "media_origin_ms": 600000,
  "effective_selection": { "quality_auto": true, "height": 1080, "codec": "h264", … } }
```

`media_origin_ms` is the **source** position that the successor's timeline
calls zero. It is not a playhead and it is not an offset into the current
stream. You will need it again at commit, exactly.

Start building the successor. Do not touch the stream that is playing.

### 3. Report progress

Each subsequent exchange may carry an `acknowledgement` naming that same
`action_id`:

| `state` | Also required | Means |
|---|---|---|
| `metadata_ready` | — | The successor's manifest parsed; nothing is buffered yet |
| `buffer_ready` | `buffered_through_ms` | Enough successor media is buffered to present |
| `committed` | `first_frame_unix_ms` **and** `committed_media_origin_ms` | The client is switching to it |
| `failed` | — | The client could not build it |
| `aborted` | — | The client gave up on it deliberately (the viewer moved on) |
| `switched` | `first_frame_unix_ms` | The successor is on screen. **Do not send this yet — see below.** |

Rules the server enforces, each of which will refuse your packet with `400` and
the offending field name:

- **`action_id` must name the offer from the preceding accepted exchange.** An
  older `action_id` is not an error, it is ignored — deliberately, so a
  retransmit cannot resurrect a stale staging.
- **`committed_media_origin_ms` must equal the offered `media_origin_ms`.**
  This is the fence that stops a client committing to a successor built for a
  point in the film the viewer has since seeked away from. Echo the offer; do
  not recompute it from your own playhead.
- **Progress does not go backwards.** `buffer_ready` after `metadata_ready` is
  fine; the reverse is dropped. Terminal states are always recorded.
- **`switched` may not ride a request whose `demand` is `end`.** One teardown,
  one path.

### 4. What a commit costs

An accepted `committed` is the server's transaction: the playback pointer
advances to the successor, the successor becomes the session every later
exchange is about, and **the predecessor begins a ten-second drain**. It keeps
serving and keeps its encoder for that window, so the client's old player may
finish presenting without a gap.

Ten seconds of holding one of two hardware encoder permits is the real cost of
this feature. That is what `switched` exists to shorten.

### 5. `switched` — not in this release

Every node accepts `switched` and releases the drain immediately on it. **No
client may send it until every node in the fleet runs a binary that accepts
it**, which is not yet true of any deployed fleet.

`AcknowledgementState` has no unknown-value fallback: a `switched` sent to an
older owner is a deserialize failure, an empty `400`, and then a `503` with a
`retry_after_ms` the client retries forever. Sending it early does not degrade,
it wedges.

There is also no way for a client to *ask* whether an owner accepts it. The
obvious answer — advertise the vocabulary on the response — was built and
withdrawn: `ControlResponseV1` carries `#[serde(deny_unknown_fields)]` and the
*relaying* node re-parses it, so a new field on that struct breaks every
cross-node exchange during a rolling upgrade. That negotiation needs
`deny_unknown_fields` relaxed and rolled out on its own first.

**So: implement through `committed` now. Leave a clearly-marked seam for
`switched` and do not wire it.** The release that turns it on is a decision
about fleet state, not a code change you can make safely today.

## Failures, and what each one means

| Status | `code` | What the client should do |
|---|---|---|
| 400 | *(field name in body)* | A bug in the client. Do not retry the same packet. |
| 409 | `owner_changed`, `stale_control` | The session moved or was replaced. Re-resolve the session; do not retry. |
| 410 | `session_ended`, `session_gone` | Over. Stop the control loop. |
| 410 | `owner_lost` | Nothing can adopt this session. Stop; the viewer must reopen. |
| 425 | `owner_transition` | Retryable. Honour `retry_after_ms`. |
| 429 | `control_rate_limited` | You are exchanging too fast. Honour `retry_after_ms`. |
| 503 | `control_unavailable` | Transient. Honour `retry_after_ms`; do not escalate to the viewer. |

Two pacing bounds you will hit if you get this wrong: exchanges are refused
under **250 ms apart** for one `client_instance_id`, and **8 per second per
session** across all of them. `sequence` must increase monotonically within a
`client_instance_id`; a new instance id resets the sequence space, so do not
mint a new one to escape a refusal.

**A refused exchange proves nothing happened.** The server applies the
client-instance and sequence fences before it acts on anything in the packet,
so a refusal never half-applies.

## What each adapter must demonstrate

The same list for all three. A PR that cannot show these is not done, and
"the unit tests pass" is not a demonstration of any of them.

1. **A client that does not declare the action is never offered one** — and
   still plays. This is the compatibility floor.
2. **The predecessor keeps playing while the successor is built.** If the
   picture stops when the offer arrives, the feature is inverted.
3. **The swap presents from a frame the client already has.** Not a seek, not a
   reload, no visible gap. Measure it; do not assert it.
4. **A commit that names the wrong origin is refused and recovered from.** Force
   a seek between offer and commit and show the client handling the `400`
   without stalling the stream it is already playing.
5. **An offer withdrawn mid-preparation is torn down.** The successor's
   resources are released; nothing leaks a player instance.
6. **Every failure row above is exercised at least once**, and none of them
   ends with a viewer looking at a spinner forever.
7. **`switched` is not sent.** Grep the diff for it and say so in the PR.

## Coordination between the three

- **One owner of `crates/plurxd/src/web/index.html` at a time.** The web
  adapter lives in that single large file alongside every settings panel;
  two branches editing it in parallel will conflict badly.
- **Nothing in this list needs a server change.** If you find yourself editing
  `playback_control.rs` to make an adapter work, stop and raise it — either
  this document is wrong, or the contract is, and both are worth more than
  the workaround.
- Each adapter is its own PR into `effort/streaming-reliability`, its own
  adversarial review, and its own qualification. They do not stack.
