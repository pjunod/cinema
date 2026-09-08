# The prepared-switch contract, for the three client adapters

**Status:** protocol shipped; **the staged successor has no producer yet**, so
no adapter can be finished · **Updated:** 2026-09-08 · **Audience:** whoever
builds the Apple, Android or web adapter

This is the one document a client adapter needs. The protocol half of §4 is
merged and will not change under you: `prepare_replacement` is offered,
acknowledgements are accepted, the commit is durable, and the predecessor
drains.

> ## Read this before starting
>
> **The successor you are offered is not playable yet.**
> `stage_prepared_successor` writes a durable row and reserves the actor's
> slot; it starts no encoder. `ControlAction::Prepare`'s own doc comment says
> so — *"This slice deliberately starts no worker behind that route."* The
> staged route carries `publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED`,
> and every public media path classifies such a route as `owner_transition`,
> so **fetching the offered `playlist_url` answers `425` until commit.**
>
> An adapter written today can therefore implement the message plumbing —
> declare, receive, acknowledge, commit, handle every refusal — and nothing
> else. It cannot build, buffer or present the successor, so acceptance
> criteria 2, 3 and 5 below are not achievable against the current server.
>
> Starting and priming that successor is the remaining server task. This
> banner comes out when it lands; until then, do not plan an adapter around
> being able to demonstrate a seamless switch.

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

Today a resolution or bitrate change tears the stream down and builds a new
one. The worst measured interruption is 2,246 ms on Safari; Apple's is
unmeasured because Apple never exercised that path at all.

**Only resolution-or-bitrate is ever prepared**, on its own or together with a
change of delivery method. Audio track, audio offset, burned subtitles,
dynamic range, and any other multi-axis combination all still fall back to
teardown, by design — so a track change producing no offer is the contract
working, not a bug to file.

The prepared switch is make-before-break: the server stages a successor while
the current stream keeps playing, the client builds it, and the swap happens on
a frame the client has already drawn. Nothing about it is seamless unless the
client actually holds both and swaps at the right moment — a client that tears
down the predecessor when the offer arrives has implemented the old behaviour
with extra steps.

## Before the exchange: the loop this rides on

`POST /api/v1/hls/{session}/control`. The URL, `protocol`, `generation` and
`control_epoch` all come from the `control` bootstrap on the create-session
response; a client never constructs them. The body is `ControlRequestV1`,
which carries seventeen required fields under `deny_unknown_fields`, so read
that struct rather than inferring the shape from examples. The whole request
is capped at 16 KiB, and over-limit is a plain `413` with no control error
body at all.

The exchange runs about every 5 s per session (`NEXT_EXCHANGE_MS`), and the
server's own deadline for one is 4 s — set the client timeout above that, or
you will abandon exchanges the server is about to answer. Everything below
rides that same request/response; there is no second endpoint and no push
channel.

`sequence` starts at **1** and increases monotonically. `capabilities` is
**required** on the first exchange and retained for the session afterwards.

### 1. Declare the capability

Two things, not one, and missing either means you are simply never offered a
prepare:

- `supported_actions` must contain `"prepare_replacement"`. At most 16 names,
  each 1–32 bytes.
- `capabilities.dual_player_preparation` must be **`true`**. All three shipped
  clients hardcode `false` today, so this is the first line you will change in
  each of them.
- `observed_download_bps` must be reported, and honestly. A prepare is refused
  unless the client's observed throughput is at least **twice** the delivered
  rate the server measures — there is no point staging a second stream a
  connection cannot carry alongside the first. A client that omits the field
  is refused silently for the same reason.

**The server never sends an action a client has not named.** That is what makes
the shipped protocol safe against every client that exists today, and it is
also why a mistake here fails silently rather than loudly: you get no offer and
no explanation. Unknown names in the list are ignored, not refused, so a later
client may name actions this server has not heard of.

### 2. Receive the offer

The response's `action` becomes:

```json
{ "type": "prepare",
  "action_id": "<uuid>",
  "session_id": "<successor session id>",
  "playlist_url": "/api/v1/hls/<session>/index.m3u8",
  "media_origin_ms": 600000,
  "effective_selection": {
    "quality_auto": true,
    "height": 1080,
    "audio_track": 1,
    "subtitle_burn": null,
    "audio_offset_ms": 0,
    "codec": "server_selected",
    "dynamic_range": "sdr" } }
```

Every `effective_selection` field is always present. `codec` is only ever
`"source"` or `"server_selected"` — never a codec name. `dynamic_range` is
`"dolby_vision" | "hdr10" | "hlg" | "sdr"` or null. `audio_track` and
`subtitle_burn` are integers or null. `playlist_url` may end `master.m3u8`
rather than `index.m3u8`.

`effective_selection` is the server's answer to the selection you asked for. A
client that no longer wants it declines by simply not acknowledging — there is
no "refuse the offer" message.

`media_origin_ms` is the **source** position that the successor's timeline
calls zero. It is not a playhead and it is not an offset into the current
stream. You will need it again at commit, exactly.

**The same offer repeats.** The identical `action_id` is re-announced on every
accepted exchange while the slot stays staged. Treat a repeated offer as the
same offer; do not spin up a second successor player per exchange.

**The offer expires.** A staged successor has a deadline about **330 seconds**
after staging. Past it the offer stops being announced and a `committed`
naming it is refused.

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

A missing required field is a `400` whose body carries `code:
"invalid_control"` and `invalid_field: "acknowledgement.<field>"`. **The other
three rules below are not refusals — they are silent discards, and that
distinction is the single easiest thing to get wrong here.** A discarded
acknowledgement returns `200` with the same offer still attached, so *the offer
still being there on the next response is how you detect it*, not a status
code.

- **`action_id` must name the offer currently being announced.** An older
  `action_id` is ignored — deliberately, so a retransmit cannot resurrect a
  stale staging.
- **`committed_media_origin_ms` must equal the offered `media_origin_ms`.**
  This is the fence that stops a client committing to a successor built for a
  point in the film the viewer has since seeked away from. Echo the offer; do
  not recompute it from your own playhead. A mismatch is **discarded, not
  refused** — `200`, nothing commits, same offer returns. Omitting the field
  entirely *is* a `400`.
- **Progress does not go backwards.** `buffer_ready` after `metadata_ready` is
  fine; the reverse is silently dropped. Terminal states are always recorded.
- **The selection must not have changed.** The exchange carrying `committed`
  has to repeat the same `selection` the offer was staged against. If the
  viewer changed a setting in between, abandon the offer rather than commit —
  committing anyway is refused with `409 stale_control` *and* tears the staged
  successor down.
- **Neither `committed` nor `switched` may ride a request whose `demand` is
  `end`.** One teardown, one path — and the teardown path is exactly where a
  client is tempted to bundle a final commit.

**Numbers you have to decide, because the server only bounds them.**
`buffered_through_ms` is validated against `0..=MAX_MEDIA_MILLIS` and
`first_frame_unix_ms` only against `> 0`; neither is checked for meaning. Pick
one convention across all three adapters and write it down here when you do:
`buffered_through_ms` in **source/film time**, matching `media_origin_ms`, and
`first_frame_unix_ms` as the client's wall clock for the frame it is switching
on — at `committed` that is the frame it is *about* to present, which is a
prediction, and is why the origin rather than this number is the fence.

### 4. What a commit costs

An accepted `committed` is the server's transaction: the playback pointer
advances to the successor and **the predecessor begins a ten-second drain**. It
keeps serving and keeps its encoder for that window, so the client's old player
may finish presenting without a gap.

Ten seconds of holding one of two hardware encoder permits is the real cost of
this feature. That is what `switched` exists to shorten.

**Open question the adapters must not each answer differently.** The commit
response is built from the *predecessor's* route — its `generation`, its
`control_epoch`, and `action: none`. The offer carried the successor's
`session_id` and `playlist_url` but never its `generation` or `control_epoch`,
and the client needs both on every subsequent request. There is today no
defined way for a client to learn them, which means there is no defined way to
continue the control loop after a commit. **Do not invent one per platform.**
This is a server-side gap; raise it rather than working around it, and it will
be answered here.

### 5. `switched` — not in this release

Every node accepts `switched` and releases the drain immediately on it. **No
client may send it until every node in the fleet runs a binary that accepts
it**, which is not yet true of any deployed fleet.

`AcknowledgementState` has no unknown-value fallback: a `switched` sent to an
older owner is a deserialize failure, an empty `400`, and then a `503` with a
`retry_after_ms` the client retries forever. Sending it early does not degrade,
it wedges.

Precisely: sent directly to an old owner it is a `400 invalid_control` with a
body and no `invalid_field`. On a multi-node fleet it is worse — the old owner
answers a relayed request with a bare `400` carrying no body at all, the
relaying ingress cannot parse that as a control error, and it converts it to
`503 control_unavailable` with `retry_after_ms: 500`, which the client then
retries forever.

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

The error body is `{ code, message, invalid_field?, generation?,
control_epoch?, retry_after_ms? }`. `retry_after_ms` is present on 425, 429 and
503, and on nothing else.

| Status | `code` | What the client should do |
|---|---|---|
| 400 | `invalid_control` | A bug in the client; `invalid_field` names it (absent when the body would not parse at all). Do not retry the same packet. |
| 404 | `session_gone` | The capability does not name a live controllable session. Stop the control loop. **Note the status: this one is not a 410.** |
| 409 | `owner_changed` | The session moved to another owner. Re-resolve it. |
| 409 | `stale_control` | Three different causes, and they want different responses: a stale `generation` means re-resolve the session; the client-instance or sequence fence means a bug in your sequencing; a rejected preparation acknowledgement means the offer is gone — drop the successor and keep playing what you have. |
| 410 | `session_ended` | Over. Stop the control loop. |
| 410 | `owner_lost` | Nothing can adopt this session. Stop; the viewer must reopen. |
| 425 | `owner_transition` | Retryable. Honour `retry_after_ms`. Also what the *staged* successor's playlist answers today — see the banner at the top. |
| 429 | `control_rate_limited` | Honour `retry_after_ms`. Usually your own pace; there is also a node-wide budget across all sessions, so a well-behaved client can still see this. |
| 503 | `control_unavailable` | Transient. Honour `retry_after_ms`; do not escalate to the viewer. **Retry by resending the identical body with the same `sequence`** — see below. |

Pacing: exchanges under **250 ms** apart are refused (measured from the last
*accepted* one), and **8 per second per session**. `sequence` must increase
monotonically.

**A session binds to the first `client_instance_id` it accepts.** A second
instance id is refused with `409 stale_control` for the life of that owner
epoch — there is no client-side reset, so "mint a fresh instance and start
over" is a recovery path that can never succeed. The sequence space restarts at
1 only when the owner epoch advances, and that first packet of a new epoch must
carry `sequence: 1` and `capabilities`.

**Retrying is resending, not re-sequencing.** An equal `sequence` with a
byte-identical body is an idempotent replay and returns the original answer; an
equal sequence with a *different* body is `409 stale_control`. Never increment
the sequence to retry.

**Which refusals are clean.** The client-instance, sequence and rate fences all
run before anything is applied, so `400`, `429` and a `409` from those fences
mean nothing happened. **A `503` does not:** it can follow a durably applied
commit — the store write can succeed and the response still fail. That is
exactly why the recovery is a replay of the same sequence rather than a new
one.

## What each adapter must demonstrate

The same list for all three. A PR that cannot show these is not done, and
"the unit tests pass" is not a demonstration of any of them.

**Achievable now** — the message plumbing:

1. **A client that does not declare the action is never offered one** — and
   still plays. This is the compatibility floor, and it is the one that
   protects every shipped client from this work.
2. **A commit that names the wrong origin is discarded, and the client
   notices.** Force a seek between offer and commit, and show the client
   detecting that the offer came back rather than waiting on an error that
   never arrives. This is the acceptance criterion most likely to be built
   wrong, because the failure is a `200`.
3. **An offer that expires or is withdrawn mid-preparation is torn down.**
   Nothing leaks a player instance, and the stream that is playing is
   untouched.
4. **Every failure row above is exercised at least once**, and none of them
   ends with a viewer looking at a spinner forever. Include the `503` replay
   path — same sequence, identical body.
5. **`switched` is not sent.** Grep the diff for it and say so in the PR.

**Blocked on the successor's producer** — the reason the feature exists. Do
not claim these until the banner at the top of this document is gone:

6. **The predecessor keeps playing while the successor is built.** If the
   picture stops when the offer arrives, the feature is inverted.
7. **The swap presents from a frame the client already has.** Not a seek, not a
   reload, no visible gap. Measure it; do not assert it.

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
