# M6 Android client — status

**Milestone:** the Android half of M6, the prepared replacement.
**Brief:** [`M6-ANDROID-CLIENT-BUILD.md`](M6-ANDROID-CLIENT-BUILD.md).
**Contract:** [`M6-CLIENT-REPLACEMENT-CONTRACT.md`](M6-CLIENT-REPLACEMENT-CONTRACT.md).
**Branch:** `agent/m6-android-prepared-replacement`, cut from
`effort/decoder-selection-recovery` at `bc9d4100`, opened back into that effort.
**Scope:** `clients/android` only. Web and Apple are being built in parallel by
separate sessions against the same contract.

Kept current in the same commit as the work it describes; a stale line here is
a bug.

---

## Where it stands

| step | state | evidence |
|---|---|---|
| §5.1 wire types — vocabulary, `prepare`, acknowledgement | done | `PlaybackControlPreparedReplacementTest`, `PreparedActionDecodingTest` |
| §5.2 successor player | done | `PreparedReplacementLedgerTest`, `buildSuccessorPlayer` |
| §5.3 alignment | done | `SuccessorTimelineTest` |
| §5.4 readiness, switch, commit | done | `PreparedReplacementLedgerTest`, `PreparedSwitchPointTest` |
| §5.5 fallback ladder | done | the ladder's terminal states, and the existing reopen path unchanged |
| dev-tab enablement | done | `ControlCapabilitiesPreparationTest`, `PreparedReplacementRequirementsTest` |
| §C12 shared acceptances (8) | done | table below |
| lab re-run on the tunneled Google TV | not taken | no lab access from this session |

`./gradlew testDebugUnitTest :app:assembleDebug :app:lintDebug` — the
non-Docker equivalent of `make android-test` and `make android` — green.
**461 tests, 0 failures, 0 errors.**

---

## The finding this milestone did not act on, and the one it did

`dual_player_preparation` is a hardware claim: *this platform can hold two live
decode pipelines*. It is Gate A — it decides whether the server may build a
successor at all — and it is frozen per platform in protocol v1.

M5.5 measured it per **device class**:

| device class | codec/HDR case | same-codec case |
|---|---|---|
| both phones | pass | pass |
| tunneled Google TV | pass 20/20 | **fail 0/3** |

Same-codec is the only kind of change the server ever prepares — both admitted
`PREPARED_AXIS_SETS` rows are same-codec — so the phones pass the case that
matters and the television fails exactly it. A platform-wide `true` would
authorise the server to prime a second pipeline on the device with the measured
hard failure; a platform-wide `false` throws away two phones that passed. The
protocol has no way to say "yes on phones, no on tunneled televisions", and
adding one is a v2 decision.

**What this branch did not do:** change the literal to `true`, or add a row to
`PREPARED_AXIS_SETS`, or touch `crates/` or `clients/apple`.

**What it did instead:** the default is unchanged — a device that touches
nothing still reports `false`, and a new test pins that — and the value is read
from a switch in Settings → Developer, beside the measurements, advisory and
never gating. That is the narrowest honest statement protocol v1 leaves
available: the person holding the device decides for their own hardware,
knowing what M5.5 found. It is a deliberate departure from the brief's §6, made
on the repository owner's standing instruction that features are not to be
gated by literals in the code; it is recorded here and in the pull request
rather than buried.

**The measurement that would settle it, and is not code:** a same-codec dual
prime on the tunneled Google TV with **tunneling forced off**. That would turn
"this device cannot prepare" into "one tunneled pipeline per codec". No session
without lab hardware can produce it — `Caps.query` enumerates the real
`MediaCodecList` and an emulator answers with a software baseline — which is
the mechanical reason the capability is not a judgement call from here.

---

## The eight shared acceptances

| # | acceptance | test |
|---|---|---|
| 1 | the declared vocabulary, on the serialized body | `the encoded request declares the prepared vocabulary` |
| 2 | a literal-JSON fixture of the inbound action, key by key | `the server's prepare action decodes key by key` |
| 3 | an unknown `action.type` is still fatal | `an action other than none is terminal rather than obeyed` |
| 4 | a non-node-relative `playlist_url` is refused before any request | `a prepare pointing anywhere but this node is fatal before any request` |
| 5 | the three terminal acknowledgements round-trip; no `committed` on `end` | `the three terminal acknowledgements encode the server's field names`, `a committed acknowledgement is never built onto an ending exchange` |
| 6 | an abandoned preparation sends `failed` or `aborted` | `an abandoned preparation always owes a terminal state` |
| 7 | a repeated `action_id` is one preparation | `a repeated action id is one preparation` |
| 8 | `observed_download_bps` is populated on the serialized body | `observed download bps reaches the wire` |

---

## Decisions taken without the owner present

1. **Tunneling: the successor inherits the incumbent's setting.** On a
   television that means the successor is tunneled too, and that reproduces
   M5.5's only hard failure rather than dodging it. The alternative — forcing
   tunneling off on the successor — would probably make that device pass, but
   the pipeline that primed would not be the pipeline that serves, and a
   measurement that cannot fail is the class of error M5.5's first instrument
   set was rejected for. The failure path here is `failed` plus the fallback
   ladder, which is bounded. Recorded beside the builder, revisit with
   evidence.
2. **`first_frame_unix_ms` comes from the successor's own first render, after
   it takes the surface.** A prepared successor has no surface, so it renders
   nothing until the switch; there is no earlier honest number. If no frame
   arrives within 5 s the commit goes out with the wall clock anyway, because
   by then the switch has demonstrably happened and a late commit beats holding
   the session's one preparation slot to the server's 330 s deadline.
3. **A `committed` on an ending exchange is dropped, not the whole request.**
   The pairing is a `400` that costs the `end` as well as the commit. Dropping
   the commit ends the session on time and leaves an unsettled preparation to
   be reaped; dropping the `end` would strand the session until its lease
   expired.
4. **Enablement lives on the device, not in a literal.** See above.

---

## What is unreachable, and what is merely unused

- With the switch off — the default — Gate A refuses before Gate B is
  consulted, so **no viewer sees this path fire**. Declaring
  `prepare_replacement` still changes one thing: `can_settle_preparation`
  becomes true, so the server performs a quorum store read for a staged
  generation on exchanges that would not otherwise have done one — those still
  on `owner_epoch == 1`, carrying no acknowledgement, and not ending. That read
  always answers `Absent` while nothing is staged. Correct, not free, and said
  here rather than discovered in a dashboard.
- With the switch **on**, a prepared handoff still fires on nothing, and the
  reason is bigger than any capability. **`stage_prepared_successor` is
  stage-only.** Its own comment says so — "a durable row and the actor's one
  successor slot, nothing produced yet" — and the roadmap's third phase,
  *reserve and prime*, is not implemented. The staged row carries
  `publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED`, so a GET of
  the successor's `playlist_url` answers `503 media_owner_transition` until the
  pointer moves, and commit does not publish it either. A client built from the
  contract's §C8 sequence diagram therefore stands up a second pipeline that
  can never become playable — **on every selection change, for the whole
  film**, which is a regression rather than a feature.

  So the ledger learns. The first successor that dies *before it was ever
  playable* — no track published, whether it errored or the readiness bound
  elapsed — ends the offers for that playback. Not a flag anyone sets, and
  forgotten when the player ends: one attempt is evidence about this playback,
  not about this attempt. Apple reached the same rule independently
  (`PreparedReplacementCoordinator.canOfferPreparation`), which is the sameness
  the three ports are meant to keep.
- On **VOD**: at this branch `DeliveryView::from_status` still leaves
  `delivered_bps` `None` on a VOD session and the floor needs both numbers, so
  VOD cannot reach a preparation here. That is changing — a parallel effort
  populates it and records that its absence "is what made preparation
  unreachable on the primary presentation" — so nothing in this client gates on
  `!isVod`. The requirement row names the mechanism (the server reports a
  delivered-throughput number, or it does not) rather than the presentation, so
  it stays true on both sides of that change.
- Nothing on any of these paths is seamless, and nothing in this branch calls
  it that. Android's fallback interruption is measured at 353–766 ms, mean 471,
  on the Google TV.

---

## What the adversarial review found

Two passes, told to assume the change was wrong until it had traced it. Both
passes found real defects; the second found defects introduced by the first
round of fixes, which is the reason it ran.

**Pass one — nine confirmed.** The ones worth remembering:

- `onStall`'s reopen was the only stream-replacing path that did not abandon
  the preparation, so a stall could reopen to a new session and the successor
  would then commit *over* it — putting the viewer back on the stream that had
  just stalled and orphaning the replacement they were actually watching.
- `offer()` checked liveness before identity, so the **ordinary commit**
  produced a third pipeline: the exchange carrying `buffer_ready` is still in
  flight when the client switches, and the server — which has settled nothing
  yet — replays the same `action_id` in its answer.
- The window between the swap and the successor's first frame was abortable, so
  a Back press in it told the server to tear down the incarnation its pointer
  was about to move to.
- The `aborted` published from `release()` was structurally guaranteed never to
  reach the wire: `end()` queues a `stop()` that cancels the very pump the
  urgent notify had just launched, and on the disposal path the composition's
  scope is cancelled in the same synchronous pass.
- `EffectiveSelection` gave Kotlin defaults to four fields the server declares
  plainly, so a truncated payload decoded to `height = 0`, passed validation,
  and was seeded into the stall budget.

**Pass two — six more, five of them introduced by the fixes.** The sharpest:

- `onPrepare` was the one exchange callback outside the generation fence, and
  it is the one that *builds an ExoPlayer*. A late exchange on a dead
  generation would stand a pipeline up against a session that no longer exists,
  with nothing left running to release it.
- `notifyUrgently` deliberately leaves an in-flight exchange alone, which is
  right while the pump is alive and wrong at teardown — the pump is on the
  scope being cancelled, and a coroutine killed inside `send` never clears
  `inFlight`, so the teardown poll could never finish. Fixed with a `settle`
  that re-homes the pump unconditionally.
- A `ControlProtocolException("body")` was classified as a null-status
  transport failure and retried forever. Making the selection's fields required
  widened the set of inputs that reach that arm, turning a diagnosable fatal
  into an undiagnosable loop. A protocol failure now stops the reporter, which
  is what it always should have done.
- `deliveredDolbyVisionProfile` did not move with `deliveredRange`, which is
  exactly the drift `adoptSessionDelivery` is one function rather than two
  lines to prevent: a Dolby Vision handover to an SDR transcode would have read
  "SDR · Profile 8".
- Releasing the predecessor on a one-second tick assumed a frame clock that is
  parked whenever the window is not visible. The surface owner collects it now,
  from the composition that re-points the view — the only place that knows.

**Pass three — four more, one of them the sharpest finding of the review.**
Cancelling a coroutine parked inside the exchange did not unwind it:
`CancellationException` is an `Exception`, every `mutex.withLock` on the way out
is uncontended, so the dying coroutine ran its whole failure tail — recording a
fabricated transport failure and arming a five-second replay of the request it
was being replaced by. The pump that took over then paced five seconds and, when
it did speak, sent the inherited request rather than the one it was handed. So
the teardown fix from pass two was a no-op or worse in exactly the case it was
written for. A cancellation is re-thrown now, and the hand-off re-asserts the
state the dead coroutine never released.

Also from pass three: the `end`-to-`active` rewrite changed one half of a pair
the mapper computes together, and `active` at rate zero is a `400` that takes
the commit with it; two commits between two composition passes dropped a
retired player on the floor; and `onSubtitleReady` was left as the only
unfenced exchange callback, so a stale exchange could fight the new session's
subtitle selection after a stall recovery.

**Pass four — clean.** Nothing new, and it answered two questions worth
recording: nothing outside the changed files depends on `Controller.player`
being stable (every reader is a composable read or the snapshot-observed
`AndroidView` update, and no `remember` caches a player), and the pattern behind
every defect that survived a pass is that it lived in `Controller` — the one
file the JVM unit lane cannot reach. Two of its decisions moved out in response,
into `PreparedReplacement.kt` where they are tested; the residue is named in a
comment listing all six paths that must abandon a preparation.

**Fixed after the fourth pass:** `observed_download_bps` measured average
throughput rather than headroom. The rate window divided bytes by *wall clock*,
and an HLS player with a full buffer fetches a segment in a burst and then idles
for seconds — so a window opened during one burst was closed by the first byte
of the next and its denominator was mostly idle. The reading oscillated between
roughly the link speed and near zero, and the low readings are the ones a floor
sees. The server's floor asks `observed >= 2 * delivered`, which is a question
about headroom; an average that includes the idle between segments answers
roughly "what is this stream's bitrate" and answers it with a number that can
never be twice itself.

`ThroughputWindow` counts only the time a transfer is actually open — a count
rather than a flag, because Media3 fetches a playlist and a segment
concurrently — and closes after a second of transfer however long that second
takes to accumulate. Lifted out of the transfer listener so it has no Media3 in
it: the arithmetic was untestable while it lived there, and it was wrong the
whole time.

**And the tests were checked against the code they replaced.** The first draft
of them passed against the old algorithm eight times out of nine, because the
old reading *oscillated* — roughly the link speed at the end of a burst, a
fraction of it on the first byte after a gap — and every case sampled at the
end. They sample at the gap now, and the old algorithm was reinstated behind
the same API to prove it: eight of ten reject it, and the two that do not are
named in the file with what they do pin instead.

**What this does not fix, and nothing client-side can.** A CDN that paces a
segment — dripping it at some multiple of the bitrate rather than as fast as
the link allows — keeps the transfer open for most of the window, so active
time is close to wall time and the measured rate is the paced rate rather than
the link's capacity. On such a server `observed` can sit below `2 × delivered`
on a link with ample headroom, and the refusal is indistinguishable from a
tight link. TCP slow start biases the same way. Both under-report, which is the
safe direction for a floor that authorises priming a second decoder on a
viewer's device — but read the `throughput_insufficient` counter with this in
mind rather than concluding Android has no headroom.

---

## What a parallel effort is about to change, and what to do about it

Recorded from another session's trace of the same server. None of it is in this
effort branch yet; all of it changes a client.

- **`ActionAcknowledgement`'s fifth field, `committed_media_origin_ms` —
  now carried.** It is required on `Committed` on `main` and compared against
  the staged successor's own `media_origin_ms`; a commit that omits it is a
  `400 invalid_control`. The earlier note here said not to add it early, on the
  grounds that `deny_unknown_fields` would make a fifth key refuse the whole
  exchange against this branch's older server. That was right about the
  mechanism and wrong about the risk:

  `explicitNulls` is off, so the field is **absent from the wire on every state
  except `committed`** — the four other acknowledgements are byte-identical to
  before and this branch's server accepts them unchanged. Only a `committed`
  carries it, and a `committed` cannot happen here: the successor can never
  become playable while staging does not prime, so `canOfferPreparation`
  retires the path after the first attempt. Against `main` it is required. Safe
  on this branch, correct on the one it is heading for.

  It is echoed from the offer and never recomputed, which is the whole point of
  the field. `action_id` says which offer is being answered; this says the
  client built the thing that offer described. A changed *ask* is already caught
  at commit by the desired digest — what the digest does not cover is position,
  so a successor primed for one point in the film and committed after the viewer
  seeked elsewhere was indistinguishable from a correct commit. A number derived
  from this client's own player would agree with itself whatever the viewer did.
- **`ControlRequestV1` gains `intent`**, which no client sends yet.
- **VOD stops being excluded**, as above.

**Where the three ports actually are, 2026-09-08.** Apple's
`PreparedReplacement.swift` and the five-field acknowledgement are on `main`;
the web half is PR #125 into `main`; Android is here, on the effort branch,
because that is the baseline its brief named. So the effort branch's merge into
`main` is where the three meet — and `test_control_wire_conformance`'s
`ActionAcknowledgement` arm on `main` already reads Android's Kotlin, which is
why the field is carried now rather than left for whoever performs that merge to
find as a red gate.

The general rule this branch already follows: the code wins and the contract
document is the bug. Re-derive every mirrored rule from the Rust at build time.

---

## Follow-ups this branch deliberately left

- The lab re-run in §1 of the brief. Highest-value item and not code.
- ~~The three documents naming a stale AGP version.~~ Done: `README.md` and
  `docs/PUBLISHING.md` quote the catalog now, and a check keeps every document
  that names one honest. The Dockerfile's comment is left alone on purpose: the
  Android CI job pulls a tag that *is* `sha256sum clients/android/Dockerfile`,
  so editing it costs the next Android job a full SDK re-download and a push.
  One job, not every job — but a poor trade for a comment on a runner whose
  registry access has already been seen to fail.
- A narrower capability keyed by axis and device class — protocol v2.
