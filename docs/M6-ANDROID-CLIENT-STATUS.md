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
- With the switch **on**, a prepared handoff still cannot fire on a **VOD**
  session, on any platform: `DeliveryView::from_status` leaves `delivered_bps`
  `None` on every VOD session and the throughput floor needs both numbers.
  Closing that is server work and out of scope for all three client briefs. It
  is the first thing to re-read if a prepared handoff turns out never to fire.
- Nothing on any of these paths is seamless, and nothing in this branch calls
  it that. Android's fallback interruption is measured at 353–766 ms, mean 471,
  on the Google TV.

---

## Follow-ups this branch deliberately left

- The lab re-run in §1 of the brief. Highest-value item and not code.
- `clients/android/README.md:249`, `clients/android/Dockerfile:36` and
  `docs/PUBLISHING.md:360` all say AGP 9.3.1; the version catalog says 9.3.2
  and the catalog is right. A courtesy fix, not this milestone's.
- A narrower capability keyed by axis and device class — protocol v2.
