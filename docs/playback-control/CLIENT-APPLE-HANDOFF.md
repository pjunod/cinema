# Apple prepared-switch adapter — handoff

**Status:** ready to build, message plumbing only · **Platform:** the one to
build first · **Repo:** `noirr/plurx` on Forgejo, branch from
`effort/streaming-reliability` · **Written:** 2026-09-08

Read `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md` in the repo first — it is the
wire contract, it is verified against the server code, and it is shared with
the Android and web adapters. This document is only what is specific to
Apple. Where the two disagree, the contract wins.

**Standing instruction: this adapter changes no server code.** If a step
seems to require editing `crates/plurxd/src/playback_control.rs` or anything
else under `crates/`, stop and flag it — either the contract is wrong or the
server is, and both are worth more than a workaround.

---

## 1. Why Apple first

Apple is the only platform with a **measured yes** behind the capability that
gates this feature.

| Platform | `dual_player_preparation` | Evidence |
|---|---|---|
| **Apple** | **`true`, shipped** | M5.5's 20/20 commit proof, both recipes, both devices |
| Android | `false` | Google TV's same-codec dual path failed 3/3 admission attempts |
| Web | `false` | Safari's codec/HDR case reached 13/20 |

`clients/apple/Sources/Caps.swift:295` sets it, and a test pins the literal
so a silent revert cannot look like a quiet fleet. **You do not need to change
it.** The other two platforms would each need a `false` flipped, and those
`false` values are correct answers to a measurement — which is why they are
not being asked to go first.

---

## 2. What you can and cannot demonstrate

**The staged successor has no producer yet.** `stage_prepared_successor`
writes a durable row and reserves the actor's slot and starts no encoder —
`ControlAction::Prepare`'s own doc comment says so. The staged route sits at
the publication sentinel, so fetching its playlist is refused until commit.

That is a server-side gap blocked on the open P2/D6 device measurement, not
something this adapter can route around.

**So this adapter implements the message plumbing and stops there:** declare,
receive the offer, report progress, commit, handle every refusal. It cannot
build, buffer or present the successor, and it must not pretend to. When the
producer lands, the presentation half is a second PR against a client that is
already speaking the protocol correctly.

---

## 3. Where the code is

| File | Its job |
|---|---|
| `clients/apple/Sources/PlaybackControlReporter.swift` | The exchange: request/response models, `supportedActions`, the loop |
| `clients/apple/Sources/PlaybackControlSession.swift` | Session lifecycle around the reporter |
| `clients/apple/Sources/PlaybackControlSnapshotMapper.swift` | Player state → the request's observation fields |
| `clients/apple/Sources/Caps.swift` | Capability document, including `dualPlayerPreparation: true` |

Tests live beside them in `clients/apple/Tests/`:
`PlaybackControlReporterTests.swift`, `PlaybackControlSessionTests.swift`,
`PlaybackControlSnapshotMapperTests.swift`,
`PlaybackControlTransportTests.swift`.

**The line that starts everything:**

```swift
// clients/apple/Sources/PlaybackControlReporter.swift:26
static let supportedActions = ["hold", "retry_resource", "terminal"]
```

`"prepare_replacement"` is not in it, which is why this client has never seen
an offer. The server never sends an action a client has not named — that is
what makes the shipped server half safe against every client in the field,
and it is also why a typo here fails silently: you simply get no offer, and
no explanation.

---

## 4. The contract, in the order you will need it

Full detail in `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md`. The Apple-shaped
summary:

**Declare.** Add `"prepare_replacement"` to `supportedActions`. Confirm
`dualPlayerPreparation` is `true` in the capability document actually sent
(it is), and that `observedBitrate` from the newest access-log event is
reaching `observed_download_bps` (it is — AVFoundation's, already wired). A
prepare is refused unless observed throughput is at least **twice** the
delivered rate the server measures, and a nil reads as a refusal rather than
a pass.

**Receive.** The response's `action` becomes `{"type": "prepare", …}` with
`action_id`, `session_id`, `playlist_url`, `media_origin_ms` and a complete
`effective_selection`. `media_origin_ms` is the **source** position the
successor's timeline calls zero — not a playhead, not an offset. Keep it
exactly; you need it back at commit.

**The same offer repeats** on every accepted exchange while the slot stays
staged. Treat a repeated `action_id` as the same offer; do not spin up a
second player per exchange. It **expires** after about 330 seconds.

**Acknowledge.** The states and what each must carry:

| `state` | Also required |
|---|---|
| `metadata_ready` | — |
| `buffer_ready` | `buffered_through_ms` |
| `committed` | `first_frame_unix_ms` **and** `committed_media_origin_ms` |
| `failed` / `aborted` | — |
| `switched` | `first_frame_unix_ms` — **do not send this** |

**The trap, and it is the one most likely to cost you a day.** A `committed`
whose `committed_media_origin_ms` does not equal the offered
`media_origin_ms` is **discarded, not refused**: the exchange returns `200`,
nothing commits, and the same offer comes back. There is no error to catch.
Detect it by the offer's persistence. Omitting the field entirely *is* a
`400`. Three of the four rules under that table behave this way — silent
discard, not refusal.

**Also:** the exchange carrying `committed` must repeat the same `selection`
the offer was staged against. If the viewer changed a setting in between,
abandon the offer rather than commit — committing anyway is `409
stale_control` *and* tears the staged successor down.

**Never bundle `committed` or `switched` onto a request whose `demand` is
`end`.** That is exactly where a teardown path is tempted to put it, and it
is a `400`.

**Retry by resending, not re-sequencing.** An equal `sequence` with a
byte-identical body is an idempotent replay that returns the original answer;
an equal sequence with a different body is `409 stale_control`. A `503` can
follow a durably applied commit, so replaying the same sequence is the only
safe recovery — never increment to retry.

---

## 5. Milestones

Each ends with an acceptance check that is a runnable command or an
observable fact.

### 5.1 Declare and observe

Add `"prepare_replacement"` to `supportedActions`. Nothing else.

**Acceptance:** a reporter test asserts the encoded request body contains
`prepare_replacement` in `supported_actions`, and that
`dual_player_preparation` is `true` in the same body. The existing
`PlaybackControlReporterTests.swift:1106` already asserts on encoded field
names — extend that shape rather than inventing one.

### 5.2 Model the offer

Decode `ControlAction.prepare` with all five fields and a complete
`EffectiveSelection` (seven fields, `codec` is only ever `"source"` or
`"server_selected"`, `dynamic_range` is one of four strings or null).

**Acceptance:** a decode test over the exact JSON in the contract document,
asserting every field including the nulls. A test that decodes a partial
object and passes is not this test.

### 5.3 The acknowledgement ladder

Emit `metadata_ready` → `buffer_ready` → `committed` with the right fields on
each, bound to the offer's `action_id`, echoing `media_origin_ms` verbatim.

**Acceptance:** a test drives a fake transport through the ladder and asserts
the encoded bodies. Then the one that matters: a test where the echoed origin
is wrong asserts the client **notices the offer came back** rather than
waiting on an error. If that test asserts a status code, it is testing the
wrong thing.

### 5.4 Refusals

Handle every row of the contract's failure table. `404 session_gone` — note
the status, it is not a `410`. The three causes behind `409 stale_control`
want three different responses. `503` replays the same sequence.

**Acceptance:** a transport test per row asserting the client's next action.
None of them may end with a viewer on a spinner.

### 5.5 Teardown

An offer that expires, is withdrawn, or fails must release whatever the
client built for it, and must leave the stream that is playing untouched.

**Acceptance:** a test asserting no player instance survives, and that the
current session's exchange loop continues normally afterwards.

---

## 6. Non-goals — do not do these

- **Do not send `switched`.** Grep your diff for it and say so in the PR.
  Every node accepts it, but no client may send one until the whole fleet
  runs a binary from #148 — `AcknowledgementState` has no unknown-value
  fallback, so an early one wedges rather than degrades.
- **Do not build the second player's presentation path.** There is nothing to
  present until the successor has a producer. Leave a clearly-marked seam.
- **Do not change `dualPlayerPreparation`.** It is already `true` and it is
  pinned by a test on purpose.
- **Do not change any file under `crates/`.**
- **Do not touch `clients/android/` or `crates/plurxd/src/web/index.html`.**
  Two other sessions own those, and `index.html` in particular is a single
  large file that will conflict badly.

---

## 7. Delivery

Branch from `effort/streaming-reliability`, own PR into it, `WIP:` title
prefix until merge-ready, one adversarial review after that, findings
implemented, then the suites, then merge it yourself.

Apple's own gates: the iOS and tvOS simulator suites, and the `effort Apple
compile` CI job. Do not report the work as passing without running them — a
skipped test is a failed test.
