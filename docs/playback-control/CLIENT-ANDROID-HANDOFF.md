# Android prepared-switch adapter — handoff

**Status:** ready to build, message plumbing only · **Blocked from enabling:**
the capability is `false` on measured evidence and must stay that way ·
**Repo:** `noirr/plurx` on Forgejo, branch from
`effort/streaming-reliability` · **Written:** 2026-09-08

Read `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md` in the repo first — it is the
wire contract, verified against the server code, and shared with the Apple and
web adapters. This document is only what is specific to Android. Where the two
disagree, the contract wins.

**Standing instruction: this adapter changes no server code.** If a step seems
to require editing anything under `crates/`, stop and flag it — either the
contract is wrong or the server is, and both are worth more than a workaround.

---

## 1. Read this before you write anything

**Android's `dual_player_preparation` is `false`, and that is a correct answer
to a measurement — not an oversight to fix.**

`clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSnapshotMapper.kt:279`

The Google TV's same-codec dual path **failed 3/3 admission attempts**. Apple
declares `true` on its own 20/20 proof; web is `false` on Safari's 13/20.

Flipping it to make your adapter produce an offer would turn a refusal you can
see into a stall a viewer sees. **Do not.**

The known problem is that a bare boolean cannot say *yes for this recipe on
this device*, so a correct `false` also throws away two Android phones that
passed both cases. Narrowing the capability is a **v1 protocol change**,
deliberately not something to do inside an adapter PR. If you conclude the
adapter is untestable without it, that conclusion is the deliverable — write
it up and raise it; do not implement it.

**What this means practically:** you build the plumbing, and you test it
against a fake transport that hands you an offer. The real server will not
send this device one until the capability question is settled. That is fine
and expected — the same is true of the presentation half on every platform
(§2).

---

## 2. What nobody can demonstrate yet

**The staged successor has no producer.** `stage_prepared_successor` writes a
durable row and reserves the actor's slot and starts no encoder — its own doc
comment says so — and the staged route sits at the publication sentinel, so
fetching its playlist is refused until commit. That is a server gap blocked on
the open P2/D6 device measurement.

So every adapter, Android included, implements the message plumbing and stops:
declare, receive, acknowledge, commit, handle every refusal. Presentation is a
second PR, later, against a client already speaking the protocol correctly.

---

## 3. Where the code is

| File | Its job |
|---|---|
| `.../player/PlaybackControlReporter.kt` | The exchange: request/response models, `SUPPORTED_ACTIONS`, action dispatch |
| `.../player/PlaybackControlSession.kt` | Session lifecycle around the reporter |
| `.../player/PlaybackControlSnapshotMapper.kt` | Player state → request fields; also where `dualPlayerPreparation` is set |
| `.../player/MediaOrigin.kt` | Counts the wire rate that becomes `observed_download_bps` |
| `.../player/Controller.kt` | Playback control surface |

All under
`clients/android/app/src/main/java/tv/plurx/app/player/`. Tests are in
`clients/android/app/src/test/java/tv/plurx/app/player/` —
`PlaybackControlReporterTest.kt`, `PlaybackControlTransportTest.kt`,
`PlaybackControlSnapshotMapperTest.kt`, `PlaybackControlAskTest.kt`.

**The line that starts everything:**

```kotlin
// PlaybackControlReporter.kt:45
val SUPPORTED_ACTIONS = listOf("hold", "retry_resource", "terminal")
```

`"prepare_replacement"` is not in it. The server never sends an action a
client has not named — that is what makes the shipped server half safe against
every client in the field, and it is also why a typo here fails silently: no
offer, no explanation.

The action dispatch is around `PlaybackControlReporter.kt:781` (`"hold" -> …`).
That is where a `"prepare"` arm goes.

**Throughput is already wired.** Android sends the rate `MediaOrigin` counts
off the wire over a rolling 500 ms window as `observed_download_bps`. It is
nil until measured, and the server reads nil as a refusal rather than a pass —
so a test that never establishes a rate will never see an offer even from a
fake server that is otherwise willing.

---

## 4. The contract, in the order you will need it

Full detail in `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md`. The Android-shaped
summary:

**Declare.** Add `"prepare_replacement"` to `SUPPORTED_ACTIONS`. Leave
`dualPlayerPreparation` alone (§1). The capability document is read from the
**retained** copy — clients send `capabilities` only on `sequence == 1` — so
it is decided once per session and cannot be raised later.

**Receive.** `action` becomes `{"type": "prepare", …}` with `action_id`,
`session_id`, `playlist_url`, `media_origin_ms`, and a complete
`effective_selection` (seven fields; `codec` is only ever `"source"` or
`"server_selected"`; `dynamic_range` is one of four strings or null).
`media_origin_ms` is the **source** position the successor's timeline calls
zero — not a playhead. Keep it exactly.

Kotlin's `@SerialName` models are strict in the right direction here, but note
the response is not `deny_unknown_fields` on the client side — decode
defensively and do not fail the whole exchange on a field you do not model.

**The same offer repeats** on every accepted exchange while the slot stays
staged. Same `action_id` means same offer; do not start a second player per
exchange. It **expires** after about 330 seconds.

**Acknowledge.** States and required fields:

| `state` | Also required |
|---|---|
| `metadata_ready` | — |
| `buffer_ready` | `buffered_through_ms` |
| `committed` | `first_frame_unix_ms` **and** `committed_media_origin_ms` |
| `failed` / `aborted` | — |
| `switched` | `first_frame_unix_ms` — **do not send this** |

**The trap.** A `committed` whose `committed_media_origin_ms` does not equal
the offered `media_origin_ms` is **discarded, not refused**: `200`, nothing
commits, same offer returns. There is no error to catch — detect it by the
offer's persistence. Omitting the field entirely *is* a `400`. Three of the
four rules under that table behave this way.

**Also:** the exchange carrying `committed` must repeat the same `selection`
the offer was staged against. If the viewer changed a setting in between,
abandon the offer — committing anyway is `409 stale_control` *and* tears the
successor down.

**Never bundle `committed` or `switched` onto a request whose `demand` is
`end`.**

**Retry by resending, not re-sequencing.** Equal `sequence` with a
byte-identical body is an idempotent replay returning the original answer;
equal sequence with a different body is `409 stale_control`. A `503` can
follow a durably applied commit, so replaying the same sequence is the only
safe recovery.

---

## 5. Milestones

### 5.1 Declare

Add `"prepare_replacement"` to `SUPPORTED_ACTIONS`. Nothing else.

**Acceptance:** a reporter test asserts the serialized request body carries it
in `supported_actions`, and asserts `dual_player_preparation` is still
`false` — pin the current value so a later change is deliberate rather than
incidental.

### 5.2 Model the offer

A `prepare` arm in the action dispatch, decoding all five fields and the
complete `EffectiveSelection`.

**Acceptance:** a decode test over the exact JSON in the contract document,
asserting every field including nulls. A test that decodes a partial object
and passes is not this test.

### 5.3 The acknowledgement ladder

`metadata_ready` → `buffer_ready` → `committed`, bound to the offer's
`action_id`, echoing `media_origin_ms` verbatim.

**Acceptance:** a transport test driving the ladder against a fake server,
asserting the serialized bodies. Then the one that matters: a test where the
echoed origin is wrong asserts the client **notices the offer came back**
rather than waiting on an error. If it asserts a status code, it is testing
the wrong thing.

### 5.4 Refusals

Every row of the contract's failure table. `404 session_gone` — note the
status, it is not `410`. The three causes behind `409 stale_control` want
three different responses. `503` replays the same sequence.

**Acceptance:** a test per row asserting the client's next action. None may
end with a viewer on a spinner.

### 5.5 Teardown

An offer that expires, is withdrawn, or fails releases whatever was built for
it and leaves the playing stream untouched.

**Acceptance:** a test asserting nothing survives and the exchange loop
continues.

---

## 6. Non-goals — do not do these

- **Do not flip `dualPlayerPreparation` to `true`.** §1. If you believe the
  adapter cannot be finished without it, write that up as the finding.
- **Do not send `switched`.** Grep your diff and say so in the PR.
- **Do not build the second player's presentation path.** Nothing to present
  until the successor has a producer. Leave a marked seam.
- **Do not change any file under `crates/`.**
- **Do not touch `clients/apple/` or `crates/plurxd/src/web/index.html`.** Two
  other sessions own those; `index.html` is one large file that will conflict.

---

## 7. Delivery

Branch from `effort/streaming-reliability`, own PR into it, `WIP:` title
prefix until merge-ready, one adversarial review after that, findings
implemented, then the suites, then merge it yourself.

Android's own gates: the unit tests, lint, and the `effort Android compile` CI
job. Do not report the work as passing without running them — a skipped test
is a failed test.
