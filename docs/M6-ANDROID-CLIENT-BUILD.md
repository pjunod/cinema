# M6 Android client — the platform whose evidence points two ways

**Status:** ready to build · **Executes:** the Android half of M6 ·
**Written:** 2026-09-07 · **Revised:** 2026-09-08 · **Baseline:** effort
head `58503d2d` on `effort/decoder-selection-recovery`

This file is self-contained; you need no other document to start.
**Part I** is what to change in `clients/android`, in what order, and how to
know it worked. **Part II** is the wire contract the server already speaks —
the same text ships in the web and Apple briefs, and the `§C`-numbers
throughout Part I point into it. Web and Apple are being built in parallel by
separate sessions; do not change anything outside `clients/android`.

**Read Part II first**, especially §C5 — the `prepare_replacement` / `prepare`
name trap, which produces a silent no-op rather than a failing test.

**Standing instruction.** If a step seems to require changing
`dualPlayerPreparation`, adding a row to `PREPARED_AXIS_SETS`, or editing
anything under `crates/` or `clients/apple`, **stop and flag it**. Those are
§C13 non-goals, and Android's is the literal most likely to tempt you —
§1 says why, and why the temptation is wrong.

---

# Part I — the Android work

## 1. What you are building, and the finding you must not act on

You are building Android's half of a two-player replacement: declare the
vocabulary, accept a `prepare` action, stand up a second ExoPlayer on the
successor's playlist, align its timeline, report readiness, switch, commit.

**And no viewer will see it fire.** Android's `dualPlayerPreparation` is
`false` at
`clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSnapshotMapper.kt:276`,
so Gate A refuses before Gate B is consulted and the server stages nothing for
an Android device (§C2).

Here is the part that will make you want to change that literal, stated plainly
so you can recognise the temptation and decline it:

| device class | codec/HDR case | same-codec case |
|---|---|---|
| both phones | pass | pass |
| tunneled Google TV | pass 20/20 | **fail 0/3** |

Source: `PLAYBACK-CONTROL-STATUS.md:1217, 1233-1243`. The Google TV failed with
two successor-prime timeouts and one `ERROR_CODE_AUDIO_TRACK_WRITE_FAILED`,
with tunneling on — which reads as two *identical* tunneled pipelines
contending where two *different* codecs get distinct instances.

**Same-codec is the only kind of change the server ever prepares** (§C9.1: both admitted `PREPARED_AXIS_SETS` rows are same-codec). So the phones
pass the case that matters and the television fails exactly it. A platform-wide
`true` would authorise the server to prime a second pipeline on the device with
the measured hard failure; a platform-wide `false` throws away two phones that
passed. **The boolean cannot say "yes on phones, no on tunneled televisions",
because it is frozen in protocol v1** — that is the finding
`M6-IMPLEMENTATION-HANDOFF.md` §3 records, and fixing it is a protocol
decision, not a client one.

Your job is to make Android the platform that is *ready* on the day a narrower
capability lands. Not to land it.

**One measurement would move this, and it is not code.** A targeted re-run —
same-codec dual prime on the Google TV with **tunneling forced off** — would
name the constraint exactly, and turn "this device cannot prepare" into "one
tunneled pipeline per codec" (`PLAYBACK-CONTROL-STATUS.md:1240-1243`). If you
have lab access to that device, that re-run is worth more than any code in this
brief. Record it as a separate finding either way.

---

## 2. The Android client as it stands

**Build system.** Single Gradle module (`settings.gradle.kts:25-26`,
`include(":app")`), committed wrapper, version catalog at
`gradle/libs.versions.toml`.

| | value | declared at |
|---|---|---|
| Gradle | 9.6.1 | `gradle/wrapper/gradle-wrapper.properties:3` |
| AGP | 9.3.2 | `libs.versions.toml:2` |
| Kotlin | 2.3.10 | `libs.versions.toml:3` |
| Media3 / ExoPlayer | 1.10.1 | `libs.versions.toml:10` |
| compileSdk / targetSdk | 37 | `app/build.gradle.kts:41,47` |
| minSdk | 23 | `app/build.gradle.kts:46` |
| Java source/target | 17 (daemon runs JDK 25) | `app/build.gradle.kts:73-74` |

**Two non-obvious build inputs live outside `clients/android`**, and a sparse
checkout of the module alone will not configure:

- `crates/plurxd/src/web/` — the `generateReaderAssets` task declares it as an
  `@InputDirectory` (`app/build.gradle.kts:34-37`).
- `tests/contracts/` and `tests/playback/` — unit-test resource roots
  (`app/build.gradle.kts:80-85`), consumed by `ModelContractTest` and
  `PlaybackInfoContractTest`.

Three documents named an AGP version older than the catalog's. Two of them —
`clients/android/README.md` and `docs/PUBLISHING.md` — now quote the catalog,
and `tests/operations/test_mobile_build_claims.py` keeps every document that
names one honest.

**`clients/android/Dockerfile` still names the old one, deliberately.** The
Android CI job pulls
`noirr/android-build:$(sha256sum clients/android/Dockerfile | cut -c1-16)`, so
editing that file — even a comment — is a tag miss: the next Android job falls
through to a full `docker build` that re-downloads the whole SDK from Google,
then pushes the new tag. One job pays for it rather than every job, and on a
runner whose registry access has already been seen to fail that is still a poor
trade for a comment. The check excludes it by name and says why, and fails if
the Dockerfile ever stops naming a version.

**The control plane** is three files mirroring web's reference implementation,
which `PlaybackControlReporter.kt:20-24` states as an explicit contract —
"the three are meant to stay observably identical":

| file | job |
|---|---|
| `player/PlaybackControlReporter.kt` (801 lines) | wire types and the coalescing reporter |
| `player/PlaybackControlSession.kt` (378 lines) | transport plus the session `Controller` holds |
| `player/PlaybackControlSnapshotMapper.kt` (278 lines) | Media3 state → snapshot, and `controlCapabilities()` |

**The player.** Three `ExoPlayer.Builder` sites, never concurrent, one per
screen: `player/Controller.kt:1683` (the main VOD/transcode player),
`livetv/LiveTvPlayer.kt:83`, `data/offline/OfflineDownloads.kt:390`. Only the
first is in scope.

`buildPlayer` (`Controller.kt:1656-1699`) is called from exactly one place,
`player/PlayerScreen.kt:649`, inside `remember(plan) { … }` — so a quality
change today rebuilds the whole controller *and* the player. Attaches are
always `setMediaItem` + `prepare()` on the one instance; the HLS session attach
is `Controller.kt:875-879` and the stall reopen is `:1074-1078`.
`addMediaItem` / `setMediaItems` appear nowhere, and
`androidx.media3.exoplayer.source.preload` is not a dependency.

**What is missing on the wire:**

| type | line | gap |
|---|---|---|
| `SUPPORTED_ACTIONS` | `PlaybackControlReporter.kt:43` | no `prepare_replacement` |
| `ControlRequest` | `:310-333` | **no `acknowledgement` field** |
| `ControlAction` | `:335-349` | flat: `type`, `reason`, `afterMs`, `code`, `message` |
| `ControlResponse` | `:372-380` | no `effectiveSelection` (the server sends it; `ignoreUnknownKeys = true` drops it) |

**What is already right.** `player/MediaOrigin.kt` (155 lines) is a complete
media-origin implementation — `sessionMediaOriginMs(hls)` at `:15-21`,
`SessionPlaybackTimeline` at `:23-35`, `realMediaPositionMs` at `:38-52`, with
`Controller.baseMs` at `Controller.kt:137`. Generalise
`sessionPlaybackTimeline` to take a raw origin rather than an `HlsStart`; do
not duplicate it.

**One thing to check early, and it is load-bearing.**
`docs/M6-CALLER-HANDOFF.md:480` and `playback_control.rs:1074-1082` both state
that Android hardcodes `observed_download_bps` null. `PlaybackControlMapping.snapshot`
does forward `observation.observedDownloadBps`
(`PlaybackControlSnapshotMapper.kt:55`), and `Controller.observedBitsPerSecond`
exists (`Controller.kt:132`), so if there is a gap it is at the
`Controller.playbackControlObservation()` fill-in (`Controller.kt:1437-1488`).
**§C9.2 makes this decisive**: a client that reports no throughput can
never be offered a preparation, whatever its capability says. Capture a real
request body, settle it, and if it is null, fixing it is part of this brief.

**And the half you cannot fix:** `delivered_bps` is `None` on every VOD session
(§C9.2), so a prepared handoff cannot fire on VOD, on any platform.
Academic for Android today — Gate A is closed anyway — but it is the reason a
manual VOD experiment shows nothing, and worth knowing before someone spends an
afternoon on it.

---

## 3. Tripwires

**Will fail, correctly, and must be updated:**

- `PlaybackControlReporterTest.kt:489` asserts the vocabulary list.
- `PlaybackControlReporterTest.kt:821` asserts the **serialized** literal
  `"supported_actions":["hold","retry_resource","terminal"]`. This one is the
  good kind of test — it asserts what ships, not what is in memory. Extend it,
  do not weaken it. (`:814` is a neighbouring key-presence list that asserts
  only that `"supported_actions":` appears at all; it will not catch you.)

**Will keep passing and become a lie — rewrite it deliberately:**

`PlaybackControlReporterTest.kt:324`,
`` `an action other than none is terminal rather than obeyed` ``, feeds the
literal `"prepare_replacement"` as an `action.type`. The server's tag is
`"prepare"` (§C5), so this test keeps passing after you add the
handler, while its name and intent become false. Change the fixture to a
genuinely unknown type — the property it is protecting, that an unrecognised
action is fatal, is one you must preserve.

**Silently untested, and the reason to add a test:** there is no Android
assertion that `controlCapabilities(...).dualPlayerPreparation == false`.
Apple has the mirror image (`AppleClientTests.swift:5361-5378`). Add Android's
— asserting `false`, with a comment pointing at §1 — so that the literal cannot
be flipped by accident, in either direction, without a deliberate edit to a
test that explains itself. Three fixtures hardcode `false` today
(`PlaybackControlReporterTest.kt:37`,
`PlaybackControlSnapshotMapperTest.kt:23`, `PlaybackControlAskTest.kt:149`) and
none of them is that assertion.

**The reporter's fatal default.** `PlaybackControlReporter.kt:691-715`
dispatches on `action.type` and the `else ->` arm throws
`ControlProtocolException("action")`, which at `:641-647` records
`"protocol:action"` and calls `stop()` — the reporter is silent for the rest of
the film, though the player survives. Keep that. It means a malformed `prepare`
costs the viewer the whole control plane, so validate strictly and test the
refusal before the happy path.

---

## 4. Tunneling is the Android-specific design question

`Controller.kt:1665` sets `.setTunnelingEnabled(isTelevision(context))`. The
Google TV's only hard failure was same-codec dual prime **with tunneling on**,
and same-codec is the only case the server prepares.

So the successor's tunneling setting is a real decision, not a copy of the
incumbent's. Two defensible answers:

- **Successor inherits the incumbent's setting.** Honest, matches production,
  and reproduces the measured failure on televisions — which is fine, because
  Gate A is closed there anyway and the failure path is `failed` plus fallback.
- **Successor is built with tunneling off, always.** Would likely make the
  Google TV pass, and is the hypothesis §1's re-run would confirm — but it
  means the successor is not the pipeline that will serve after the switch, so
  a successful prime proves less than it appears to.

**Pick the first**, and record the second as the open question it is. A prime
that succeeds on a pipeline unlike the one that serves is the same class of
error as M5.5's rejected instrument set: a measurement that cannot fail. If
§1's re-run later names the constraint, revisit it with evidence.

Whichever you choose, write the choice and its reason into a comment beside the
successor builder, not only into the PR.

---

## 5. The work, in order

### 5.1 The wire types

`player/PlaybackControlReporter.kt` only.

1. `SUPPORTED_ACTIONS` at `:43` becomes
   `listOf("hold", "retry_resource", "terminal", "prepare_replacement")`.
2. `ControlAction` gains the five `prepare` fields as nullable properties with
   `@SerialName` mappings. **Keep it a flat `@Serializable data class`** — do
   not convert it to a sealed class. Web and Apple both keep a flat shape and
   `PlaybackControlReporter.kt:20-24` makes cross-client sameness a contract.
   (`QualitySelection` at `:182-193` is the file's only sealed class; it is not
   the pattern to follow here.)
3. `EffectiveSelection` becomes a real `@Serializable data class` (§C9), on both `ControlResponse` and `ControlAction`.
4. `ControlRequest` gains `acknowledgement: ActionAcknowledgement?`, with a
   five-value `AcknowledgementState` enum (§C7).
   **`Net.json` uses `explicitNulls = false`**, which is why `supportedActions`
   deliberately has no default at `:328-332` — the same trap applies to any new
   field that must appear on the wire. Check what your additions serialize to,
   with a test on the encoded string, not on the object.
5. The `:691-715` `when` gains a `"prepare" ->` arm — **`"prepare"`, not
   `"prepare_replacement"`**, §C5 — validating `actionId` parses as a
   `UUID`, `sessionId` is non-empty, `playlistUrl` is node-relative (§C6), `mediaOriginMs >= 0`, and `effectiveSelection` is in range. Anything
   else throws `ControlProtocolException("action")`, like the `else ->` arm.

**Acceptance:** `make android-test` green, with a literal-JSON decode fixture
(§C12.2), an encoded-body assertion for the new vocabulary, and a
negative fixture proving an absolute `playlistUrl` is refused before any
request.

### 5.2 The successor player

`player/Controller.kt`.

The incumbent `ExoPlayer` stays authoritative. Add a second, built by a
successor-specific path — not by calling `buildPlayer` again, because
`buildPlayer` returns a `BuiltPlayer` bound to a `ProgressiveMediaOrigin`
transfer listener that belongs to the incumbent's data source.

```
 player           ── authoritative, volume up, on the surface ──▶ viewer
 preparedPlayer   ── volume 0, no surface, priming ────────────▶ nothing
```

Rules, each with its reason:

- **Never audible or visible before the switch** (§C13). Set
  `volume = 0f` and attach no surface; do not rely on the composition not
  rendering it.
- **One successor at a time.** A new `actionId` while one is live is `aborted`
  on the old, then build. A repeated `actionId` is the same preparation
  (§C12.7).
- **Every exit releases it** — `PlayerScreen` disposal, session end, seek,
  another quality change, process backgrounding. `remember(plan)` rebuilding
  the controller must not strand a prepared player.
- **Do not use `addMediaItem` / `setMediaItems` to fake a second pipeline.**
  A playlist is one decoder with a queue; you need two independently primeable
  pipelines.
- **Tunneling**: §4.

**Acceptance:** a unit test drives prepare → abandon and asserts the successor
was released and the incumbent's media item never changed.

### 5.3 Alignment

Generalise `sessionPlaybackTimeline` (`MediaOrigin.kt:23-35`) to take a raw
`mediaOriginMs` rather than an `HlsStart`, and prime the successor to the
incumbent's current **film** position via `realMediaPositionMs`.

**Acceptance:** `MediaOriginTest.kt` gains cases for VOD, live-recovery with a
non-zero origin, and the existing `HlsStart` path producing byte-identical
results to today.

### 5.4 Readiness, the switch, and the commit

| when | send |
|---|---|
| successor reaches `Player.STATE_READY`, tracks known | `metadata_ready` |
| successor buffered past the switch point | `buffer_ready` + `bufferedThroughMs` |
| successor on the surface and audible, incumbent released | `committed` + `firstFrameUnixMs` |
| `onPlayerError`, or the readiness bound elapsed | `failed` |
| viewer seeked, changed quality again, or left | `aborted` |

`first_frame_unix_ms` should come from the successor's own first render —
`AnalyticsListener.onRenderedFirstFrame` is the honest source — not from a
timer. §C7 has the validation rules; the one to restate is that **a
`committed` may not share an exchange with `demand: "end"`.**

**An Android-specific ordering hazard**, recorded at
`PLAYBACK-CONTROL-STATUS.md:281-311`: `onPlayerError` is a non-suspending
`Player.Listener` override, which is why Android *arms* rather than *awaits* a
verdict where Apple awaits. That document says explicitly that M6's
`buffered_break_before_make` will need the same treatment. Do not introduce a
suspending await inside a listener callback; use the existing arm-and-poll seam
(`PlaybackControlSession.askForAction`, `PlaybackControlSession.kt:195-248`,
bounded by `CONTROL_ASK_MS = 1_500` / `CONTROL_ASK_CAP_MS = 3_000` at
`Controller.kt:1552-1553`). `Controller.applyStallVerdict(verdict:
ControlAction)` at `Controller.kt:893` is the existing action dispatcher on the
player side and the natural place for the `prepare` handling to land.

**Acceptance:** a test per row asserting the **encoded** request body, plus one
asserting the `end` + `committed` combination is never constructed.

### 5.5 The fallback ladder

When the successor cannot be made ready, release it, send `failed`, fall back
to the existing in-place path. Android's fallback interruption is already
measured — 353–766 ms, mean 471, on the Google TV
(`PLAYBACK-CONTROL-STATUS.md:1248-1250`) — so this path is known to work and
known not to be seamless. Do not describe it as seamless anywhere.

**Acceptance:** a test forces the successor to fail and asserts the outbound
`failed`, the incumbent still playing, and exactly one player alive afterwards.

---

## 6. Non-goals

§C13 in full, plus:

- **Do not change `PlaybackControlSnapshotMapper.kt:276`.** §1. Add the
  assertion that pins it instead (§3).
- **Do not edit `crates/` or `clients/apple`.** Two other sessions are there.
- **Do not convert `ControlAction` to a sealed class.** §5.1.
- **Do not weaken `PlaybackControlReporterTest.kt:821`** into an in-memory
  check. Asserting the encoded string is what caught `explicitNulls = false`.
- **Do not remove the reporter's fatal `else ->` arm.** §3.
- **Do not touch `LiveTvPlayer.kt` or `OfflineDownloads.kt`.** Different
  screens, different lifecycles, out of scope.

---

## 7. Build environment

**JVM unit tests, lint and a debug APK need no Android hardware.** The
documented path is Docker, `linux/amd64` (AAPT2 is x86-64 only):

```bash
make android-test    # testDebugUnitTest + lintDebug in the pinned image
make android         # :app:assembleDebug
```

There is **no `make android-build` and no `make android-check`** — the targets
are `android`, `android-test`, `android-image`,
`android-instrumentation{,-build,-run}`, `android-publish`, `apk`
(`Makefile:610-677`). Local non-Docker equivalent, per
`clients/android/README.md:365`:

```bash
./gradlew testDebugUnitTest :app:assembleDebug :app:lintDebug
```

**Network is the real gate, not hardware.** The first `./gradlew` resolves the
whole dependency graph from `google()` + `mavenCentral()` under
`RepositoriesMode.FAIL_ON_PROJECT_REPOS` (`settings.gradle.kts:17-23`). Behind
a restrictive egress a cold Gradle cache may not resolve;
`clients/android/.gradle-docker/` and `.gradle-validation/` are gitignored
warm caches and change the answer.

**Requires hardware, and you probably cannot run it:**
`make android-instrumentation-run` needs `adb` and `PLURX_ANDROID_SERIAL`; CI
lane `android_device` (`ci.yml:674-780`) needs `/dev/kvm` readable *and*
writable plus the `android-tv` API 36 x86 image, and runs all 45 instrumented
tests. CI lane `android_jvm` (`ci.yml:563-601`) runs `make android-test` on
`[self-hosted, Linux, X64, lab, android-kvm]`.

**And the one thing no environment gives you:** `Caps.query(context)`
enumerates the real `MediaCodecList` and display HDR types. On an emulator that
is a software baseline, not a device answer — which is the exact axis M5.5
measured. **A session without lab hardware cannot produce the measurement that
would justify any change to the capability**, which is the mechanical reason §1
is a non-goal rather than a judgement call.

---

## 8. Definition of done

1. `make android-test` and `make android` green.
2. §C12's eight shared acceptances each have a named test, written in
   the style of `PlaybackControlReporterTest.kt` — backtick sentence names,
   `kotlin.test` assertions, the `Harness` at `:82-111`, one behaviour per test
   with the *reason* in a comment. (Read `PlaybackControlAskTest.kt:22-35`'s
   header before writing anything spanning the session/reporter seam; it
   explains why `runTest`'s virtual clock is wrong there.)
3. A new test asserts `controlCapabilities(...).dualPlayerPreparation == false`
   with §1's reason in a comment.
4. `PlaybackControlReporterTest.kt:324` rewritten to test an actually
   unknown action type.
5. `observed_download_bps` verified non-null on a real exchange, or fixed
   (§2, §C9.2).
6. The tunneling decision (§4) recorded in a comment beside the successor
   builder.
7. The PR states that Android's `dualPlayerPreparation` remains `false`, that
   the path is therefore unreachable in production, and links §C14 and
   §1 for why the work landed anyway.
8. If lab access to the tunneled Google TV exists: §1's same-codec,
   tunneling-off re-run, reported as its own finding in
   `PLAYBACK-CONTROL-STATUS.md`. This is optional for the milestone and is the
   highest-value thing in the brief.


---

# Part II — the wire contract, exactly as the server speaks it

Identical in the web, Apple and Android briefs, and maintained as a standalone
document at [`docs/M6-CLIENT-REPLACEMENT-CONTRACT.md`](M6-CLIENT-REPLACEMENT-CONTRACT.md).
**That copy is the one to edit** when the server changes; the three briefs are
snapshots taken at the baseline in this file's header. Everything below describes
shipped `main` behaviour, not proposed behaviour. Part I's `§C`-references
point here.

**Standing instruction.** Every type below was copied out of the tree at the
baseline in this file's header, with its line number. Re-verify each against
the cited file at build time in case it moved, and if a field's shape
disagrees with this document, **the code wins and this document is the bug** —
say so rather than reshaping the client to match prose.

**The second standing instruction.** Nothing in this contract is yours to
extend. If your platform seems to need a field, a state, or an action name
that is not written here, stop and flag it. All three clients are being built
at once against a server that is already frozen; a client that invents a sixth
acknowledgement state produces a `400` in the field and a merge conflict in
the protocol.

---

## C1. The situation, in one paragraph

**The server half is finished.** `ControlAction::Prepare` exists, staging
writes a durable row, the acknowledgement state machine is implemented and
fenced, commit and abort reach the store through a CAS, and a deadline timer
reaps a successor nobody claimed. All of it is covered by in-file tests. It is
nevertheless **completely dark in production**, for one reason: no shipped
client names `prepare_replacement` in `supported_actions`, so the server never
tells anyone a successor is staged. That single missing string is the whole
remaining gap, and closing it correctly is what these three briefs are for.

There is no `M6a` or `M6b` server milestone to wait for. There never was — the
letters do not appear anywhere in the repository. The slices that exist are
`M6-CALLER-HANDOFF.md` §3.1–§3.5, and all five are shipped.

---

## C2. Two gates, and they are independent

This is the most misread part of the system, so it is drawn rather than
narrated. A staged successor has to clear **both** gates before a client hears
about it, and they are checked at different times by different code.

```
 client's DynamicCapabilities                 client's supported_actions
   .dual_player_preparation                     contains "prepare_replacement"
            │                                              │
            │  GATE A — may the server                     │  GATE B — may the
            │  build a successor at all?                   │  server mention it?
            ▼                                              ▼
  decide_preparation_given_client              hls.rs:5648  can_settle_preparation
  playback_control.rs:1536                     hls.rs:5681  prepared_successor
            │                                              │
     false ─┴─▶ Fallback{ClientCannotPrepare}        false ─┴─▶ NotRequested
               nothing is staged                              staged row exists,
               nothing is spent                               client never told,
                                                              `suppressed` metric++,
                                                              reaped at 330 s
       true                                            true
            │                                              │
            ▼                                              ▼
      stage_prepared_successor                      ControlAction::Prepare
      hls.rs:6464 — real encoder,                   on the wire
      durable row, actor slot taken
```

**Gate A is a hardware claim.** It says *this platform can hold two live
decode pipelines*. M5.5 measured it on real devices and the answer is per
platform, frozen in protocol v1, and **not yours to change in a client brief**
(§C13).

**Gate B is a vocabulary claim.** It says *this build knows what to do with a
`prepare` action*. It costs nothing and is safe to declare the moment the
handling code exists, because a client that declares it and is never offered a
preparation behaves exactly as it does today.

The consequence that makes three parallel sessions possible: **all three
clients implement the same thing, and only Gate A decides whose path goes
live.** Apple's Gate A is already `true`, so Apple's work reaches viewers.
Web's and Android's stay `false`, so their work is proven by tests that inject
the action directly (§C12) and waits on a measurement, not on more code.

### C2.1 The one thing Gate B changes even when Gate A is false

Declaring `prepare_replacement` makes `can_settle_preparation` true
(`hls.rs:5648-5652`), which makes the server perform a quorum store read for a
staged generation on every exchange. On a client whose Gate A is `false`,
nothing is ever staged, so that read always returns `Absent` — correct, but not
free.

The cost is narrower than it first looks, because the predicate is a
disjunction:

```rust
let can_settle_preparation = request
    .accepts(PREPARE_REPLACEMENT_ACTION)
    || request.acknowledgement.is_some()
    || owner_epoch > 1
    || request.demand == PlaybackDemand::End;
```

A session already past its first owner epoch, or carrying an acknowledgement,
or ending, was doing the read anyway. Declaring the action adds it only for
sessions still on `owner_epoch == 1` with no acknowledgement and
`demand != end`. Say so in your PR; do not discover it in a dashboard.

---

## C3. The exchange, and where it lives

`POST /api/v1/hls/{session_id}/control` — registered at
`crates/plurxd/src/http/mod.rs:373-378` under the `/api/v1` prefix, handled by
`hls::control` at `crates/plurxd/src/http/hls.rs:4054`.

It is **poll-based**: the client POSTs its own state, the server answers with
at most one instruction. There is no push, no socket and no long poll. The
session UUID in the path **is** the bearer credential; there is no other auth
on this route.

**Cadence and bounds** (`crates/plurxd/src/playback_control.rs:21-62`):

| constant | value | meaning for a client |
|---|---|---|
| `PROTOCOL_V1` | `"plurx-playback-control-v1"` | echo verbatim in `protocol` |
| `NEXT_EXCHANGE_MS` | `5_000` | published as `next_exchange_ms`; the default pace |
| `MIN_CONTROL_INTERVAL` | `250 ms` | server-side floor; exceed it and you get `429` |
| `EXCHANGE_DEADLINE` | `4 s` | the server abandons its own work and answers `503` |
| `MAX_REQUEST_BYTES` | `16 KiB` | a larger body is refused before parsing |
| `MAX_RESPONSE_BYTES` | `64 KiB` | |
| `ROLLING_LEASE_TIMEOUT_MS` | `60_000` | one of exactly two legal `lease_timeout_ms` |
| `VOD_LEASE_TIMEOUT_MS` | `300_000` | the other |
| `TERMINAL_ACK_REPLAY_TTL_MS` | `60_000` | an accepted settlement replays for this long |
| `PREPARATION_DEADLINE_MS` | `330_000` | `VOD_LEASE_TIMEOUT_MS + 30_000`, `hls.rs:6446` |

**The client does not get to advertise itself into this route.** The control
block only appears in the session create response when the store setting
`PLAYBACK_CONTROL_PROTOCOL_V1` is `"1"` (`hls.rs:1854-1859`). A session created
with the gate off has no `control` block, and a control POST against it answers
`404 session_gone` with the message *"playback control was not advertised for
this session"*. This is how the whole protocol stays optional.

`ControlBootstrap`, what you receive in the create response
(`playback_control.rs:92-101`):

```rust
pub(crate) struct ControlBootstrap {
    pub protocol: String,
    pub url: String,             // "/api/v1/hls/{session_id}/control"
    pub generation: String,      // echo as `generation`
    pub control_epoch: u64,      // echo as `control_epoch`
    pub next_exchange_ms: u32,
    pub lease_timeout_ms: u32,   // exactly 60_000 or 300_000
}
```

---

## C4. The request

`ControlRequestV1` — `crates/plurxd/src/playback_control.rs:136-167`. There is
**no `rename_all`**: the Rust field names are already the wire names.
`#[serde(deny_unknown_fields)]` is on, so a key this server has never heard of
is a `400`, not a warning.

```rust
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlRequestV1 {
    pub protocol: String,
    pub generation: String,
    pub control_epoch: u64,
    pub client_instance_id: String,
    pub sequence: u64,
    pub demand: PlaybackDemand,
    pub position_ms: i64,
    pub buffered_from_ms: Option<i64>,
    pub buffered_through_ms: i64,
    pub playback_rate: f64,
    pub render_state: RenderState,
    pub seek_target_ms: Option<i64>,
    pub observed_download_bps: Option<u64>,
    pub selection: ClientSelection,
    pub capabilities: Option<DynamicCapabilities>,
    pub observation: Option<ClientObservation>,
    pub acknowledgement: Option<ActionAcknowledgement>,
    pub supported_actions: Option<Vec<String>>,
}
```

Two fields carry the whole of this milestone's client work: `supported_actions`
(§C5) and `acknowledgement` (§C7).

**`capabilities` is required on `sequence == 1`** and optional thereafter
(`playback_control.rs:245-249`). The server retains the last accepted document
on `ControlState::last_capabilities` (`:2907-2924`) and Gate A reads the
*retained* one, not this exchange's. So a client that sends capabilities once
and then omits them is behaving correctly and is still eligible to prepare.

**`supported_actions` bounds** (`:261-273`): at most `MAX_SUPPORTED_ACTIONS =
16` names, each non-empty and at most `MAX_ACTION_NAME_LEN = 32` bytes.
**Unrecognised names are ignored, never refused** — deliberately, so a client
from a later version can name actions this server has never heard of. Absent or
empty means *passive*, and a passive client is only ever answered `none`.

---

## C5. The name trap — read this twice

`prepare_replacement` and `prepare` are both correct, in different places, and
this is the single highest-risk item in the contract. Every other action's
declared name equals its wire tag; this one does not.

| where | string | source |
|---|---|---|
| what you **declare** in `supported_actions` | `"prepare_replacement"` | `PREPARE_REPLACEMENT_ACTION`, `playback_control.rs:53` |
| what arrives as `action.type` | `"prepare"` | `#[serde(tag = "type", rename_all = "snake_case")]` on the `ControlAction` enum, `:1609` |

The mapping is `ControlAction::vocabulary_name()` at
`playback_control.rs:1730-1739`. The comment there gives the reason: the action
is named for the transaction (`prepare_replacement` / `commit_replacement` /
`abort_replacement` in roadmap §3.1), while the tag is named for the moment.

A client that declares `"prepare"` is never offered anything — `accepts()` is a
literal string comparison (`:283-287`), it will not match, and the failure is
completely silent. A client that switches on `action.type == "prepare_replacement"`
never fires.

The server does assert the tag — `hls.rs:16375`,
`assert_eq!(prepare_body["action"]["type"], "prepare");`, against a serialized
body, and eight further sites read `["action"]["action_id"]` off one. **What
nothing asserts is the rest of the payload**: grep the tree for
`["action"]["playlist_url"]`, `["media_origin_ms"]` or `["effective_selection"]`
and you get nothing. So the tag is pinned and the four fields a client actually
consumes are not. Each client brief therefore requires a full literal-JSON
fixture of its own (§C12).

The full declared vocabulary after this milestone, on every platform:

```json
["hold", "retry_resource", "terminal", "prepare_replacement"]
```

Order is not significant — `accepts()` scans the list — but keep it in this
order across all three clients so the three test fixtures read the same.

---

## C6. `ControlAction`, in full

`crates/plurxd/src/playback_control.rs:1607-1685`. Internally tagged on `type`,
`rename_all = "snake_case"`. Note there is **no `deny_unknown_fields`** here:
a later server may add keys, and your parser must tolerate them.

| variant | `"type"` | payload | carries `action_id`? |
|---|---|---|---|
| `None` | `none` | — | no |
| `Hold` | `hold` | `reason` | no — advisory, recomputed on replay |
| `Terminal` | `terminal` | `code`, `message` | no |
| `RetryResource` | `retry_resource` | `after_ms`, `reason` | no |
| `Prepare` | `prepare` | see below | **yes** |

```rust
    Prepare {
        action_id: String,
        session_id: String,
        playlist_url: String,
        /// Exact source position the successor's session-relative zero maps
        /// to. Without it a client cannot align the second timeline with the
        /// first, and the commit boundary is expressed in film time.
        media_origin_ms: i64,
        /// What the successor will deliver. The client asked for a
        /// selection; this is the server's answer, and a client that no
        /// longer wants it can decline by simply not acknowledging.
        effective_selection: EffectiveSelection,
    },
```

**Field by field:**

- **`action_id`** — a UUID minted once per staging (`into_action()`,
  `:1712-1721`) and replayed byte-identically on every subsequent exchange until
  the staging settles. It is the transaction identity: you echo it in
  `acknowledgement.action_id`, and that is how a commit that arrives late names
  *this* staging rather than whichever one is current when it lands. Treat a
  repeat of the same `action_id` as *the same preparation*, not a new one.
- **`session_id`** — the successor's own session UUID. It is a full session in
  every respect but two: it holds no playback pointer, and it sits at
  `MEDIA_SESSION_PUBLICATION_BLOCKED`. You address it exactly as you would a
  session from a create response.
- **`playlist_url`** — node-relative, and validated to be exactly
  `/api/v1/hls/{session_id}/index.m3u8` or `…/master.m3u8`
  (`is_node_relative_playlist`, `:1759`; max 512 bytes; query and fragment
  allowed but ignored). Resolve it against the same origin as the current
  session. **Do not follow it as an absolute URL** even if a future server sends
  one — the relay path already refuses that (`a_relayed_preparation_cannot_point_a_client_anywhere`,
  `:13973`) and your client should too.
- **`media_origin_ms`** — the source position that the successor's
  session-relative zero maps to. This is the whole alignment mechanism; §C8.
- **`effective_selection`** — what the successor will actually deliver, not
  what you asked for. §C9. A client that no longer wants it declines by simply
  never acknowledging; the deadline reaps it.

**Ranking.** `resolve_action` (`:1929-1985`) returns a decided action verbatim
at `:1934-1936` and ranks it above every advisory one, so a `Prepare` outranks
a `Hold`. On the two *derived* branches — the producer-decision one at
`:1959-1966` and the hold one at `:1969-1970` — an action whose
`vocabulary_name()` you did not declare is replaced with `ControlAction::None`,
**never softened into a hold**. A decided action is vocabulary-resolved earlier,
at acceptance, so the client-facing rule is the same either way: **silence from
the server is not evidence that nothing is staged.**

**Exactly one action per exchange.** There is no array. A preparation and a
terminal cannot arrive together.

---

## C7. The acknowledgement

`ActionAcknowledgement` — `crates/plurxd/src/playback_control.rs:490-534`.
`deny_unknown_fields` is on.

```rust
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionAcknowledgement {
    pub action_id: String,
    pub state: AcknowledgementState,
    pub buffered_through_ms: Option<i64>,
    pub first_frame_unix_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcknowledgementState {
    MetadataReady,
    BufferReady,
    Committed,
    Failed,
    Aborted,
}
```

**Exactly five legal `state` strings**: `metadata_ready`, `buffer_ready`,
`committed`, `failed`, `aborted`. A sixth is a serde error, which means a
`400 invalid_control` with the generic message *"the control body is not valid
protocol v1 JSON"* and **no `invalid_field`** — it never reaches the semantic
validator, so you get no help identifying it. This is why §C1's second standing
instruction exists.

**Validation rules, all `400 invalid_control`:**

| rule | `invalid_field` |
|---|---|
| `action_id` must parse as a UUID | `acknowledgement.action_id` |
| `buffered_through_ms`, if present, ∈ `0..=MAX_MEDIA_MILLIS` | `acknowledgement.buffered_through_ms` |
| `first_frame_unix_ms`, if present, must be `> 0` | `acknowledgement.first_frame_unix_ms` |
| `buffer_ready` **requires** `buffered_through_ms` | `acknowledgement.buffered_through_ms` |
| `committed` **requires** `first_frame_unix_ms` | `acknowledgement.first_frame_unix_ms` |
| `demand == "end"` may not carry `state: "committed"` | `acknowledgement.state` |

That last one is worth a sentence of its own: **you may not commit a
replacement on the same exchange that ends the session**
(`playback_control.rs:253-259`). A client that switches players and then
immediately tears down on user-close must send the commit and the end as two
exchanges, in that order.

**The server's state machine** (`record_preparation_progress` `:3377-3410`,
`record_terminal_preparation_acknowledgement` `:3412-3444`):

| you send | server does |
|---|---|
| `metadata_ready` | records progress, if the prior ack is absent or `metadata_ready` |
| `buffer_ready` | records progress, if the prior ack is absent, `metadata_ready`, or `buffer_ready` — monotonic |
| `committed` | `PreparationDirective::Commit`; the store CAS runs; if the deadline or an abort already won, you get `Abort { acknowledgement_rejected: true }` |
| `failed` / `aborted` | `PreparationDirective::Abort { acknowledgement_rejected: false }` |

**An acknowledgement whose `action_id` does not match the currently bound
`Prepare` is silently ignored** (`bound_preparation_acknowledgement`,
`:3361-3375`) — *"Unknown action IDs belong to an older staging and are
deliberately harmless."* No error comes back. Do not treat a `200` as proof
your commit was accepted; read the response.

**Progress states are optional; a terminal state is not.** Nothing forces you
to send `metadata_ready` or `buffer_ready`, and the server behaves correctly
without them — they exist so the operator can see how far a preparation got
before it died. But a preparation you abandon **must** get `failed` or
`aborted`. Leaving it to the 330-second deadline holds the predecessor's
preparation slot for the rest of the session (`M6-CALLER-HANDOFF.md` §3.5), so
that session gets exactly one preparation, ever, good or not.

---

## C8. The protocol, end to end

```
 CLIENT                                                  SERVER
   │  POST control  { supported_actions: [... ,          │
   │                  "prepare_replacement"],            │
   │                  capabilities: { dual_player_       │
   │                    preparation: <measured> } }      │
   │ ──────────────────────────────────────────────────▶ │
   │                                                     │ viewer changes quality,
   │                                                     │ or the actor proposes one
   │                                                     │ decide_preparation → Prepare
   │                                                     │ stage_prepared_successor:
   │                                                     │   mint incarnation + session,
   │                                                     │   start a real encoder,
   │                                                     │   durable row, pointer UNTOUCHED
   │                                                     │ arm 330 s deadline
   │  ◀────────────────────────────────────────────────  │
   │   action { type:"prepare", action_id, session_id,   │
   │            playlist_url, media_origin_ms,           │
   │            effective_selection }                    │
   │                                                     │
   ├─ build SECOND pipeline on playlist_url              │
   ├─ align its zero to media_origin_ms  ────────────┐   │
   │                                                 │   │
   │  POST { acknowledgement: {action_id,            │   │
   │          state:"metadata_ready"} }              │   │
   │ ──────────────────────────────────────────────────▶ │ progress recorded
   │                                                 │   │
   ├─ prime the successor's buffer                   │   │
   │  POST { acknowledgement: {action_id,            │   │
   │          state:"buffer_ready",                  │   │
   │          buffered_through_ms} }                 │   │
   │ ──────────────────────────────────────────────────▶ │ progress recorded
   │                                                 │   │
   ├─ SWITCH: successor visible, predecessor retired │   │
   ├─ note the first qualifying frame's wall clock ◀─┘   │
   │  POST { acknowledgement: {action_id,                │
   │          state:"committed",                         │
   │          first_frame_unix_ms} }                     │
   │ ──────────────────────────────────────────────────▶ │ Commit → store CAS from the
   │                                                     │ expected predecessor to the
   │                                                     │ staged incarnation
   │  ◀────────────────────────────────────────────────  │
   │   action { type:"none" }   ← there is no            │ predecessor state != active,
   │                              commit_replacement     │ ledger cleared, slot freed
   │                              action; §C11
   ▼
```

**The unhappy paths, all of which you must implement:**

```
 successor will not build   ──▶  ack {state:"failed"}     ──▶ Abort, slot freed
 viewer seeks / changes     ──▶  ack {state:"aborted"}    ──▶ Abort, slot freed
   quality again
 you never answer           ──▶  330 s deadline reaps it  ──▶ slot freed by the actor,
                                                              your late commit gets
                                                              acknowledgement_rejected
 commit lands after abort   ──▶  Abort{acknowledgement_rejected:true}
```

---

## C9. `effective_selection`

`crates/plurxd/src/playback_control.rs:861-891`. It appears **twice on the
wire**: as `ControlResponseV1.effective_selection` (what the *current* session
delivers) and inside `Prepare` (what the *successor* will deliver). Comparing
the two is how a client knows what is about to change.

```rust
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectiveSelection {
    pub quality_auto: bool,
    pub height: i64,
    pub audio_track: Option<i64>,
    pub subtitle_burn: Option<i64>,
    pub audio_offset_ms: i64,
    pub codec: String,
    pub dynamic_range: Option<String>,
}
```

**`codec` is not a codec name.** Legal values are exactly `"source"` and
`"server_selected"`, and they distinguish direct-play/remux from transcode —
the `PreparationAxis::DeliveryMethod` axis. A client that renders this string
to a viewer as a codec is wrong.

`dynamic_range` ∈ `{"dolby_vision", "hdr10", "hlg", "sdr"}`, or absent/null.
`height` ∈ `0..=MAX_HEIGHT`. `audio_offset_ms` ∈ `-15_000..=15_000`.
`audio_track` and `subtitle_burn` ∈ `0..=1_024` when present.

On the response path the height comes from the stored `StartResponse` and the
grade from `start.delivered_dynamic_range` (`hls.rs:4183-4187`) — **the
encoder's answer, not the request's**.

### C9.1 Which changes can ever be prepared

`PREPARED_AXIS_SETS` — `playback_control.rs:1376-1381` — admits exactly two:

```rust
const PREPARED_AXIS_SETS: [PreparationAxisSet; 2] = [
    PreparationAxisSet::of(&[PreparationAxis::ResolutionOrBitrate]),
    PreparationAxisSet::of(&[
        PreparationAxis::ResolutionOrBitrate,
        PreparationAxis::DeliveryMethod,
    ]),
];
```

plus a directionality guard (`successor_rate_is_bounded`, `:1408-1410`): the
two-axis pair is admitted **only toward a `server_selected` successor**.

Both admitted sets are **same-codec**. A codec or dynamic-range change is never
prepared today, on any platform. This matters when you read the M5.5 numbers
(§C13) because the case web failed is not a case the server would have offered
it.

`FallbackReason` strings a client may see in metrics, never on the wire
(`:1091-1103`): `client_cannot_prepare`, `axis_not_proven`, `multiple_axes`,
`throughput_unreported`, `throughput_insufficient`.

### C9.2 The throughput floor, and why `observed_download_bps` is load-bearing

`PreparationConditions::headroom_refusal` (`:1173-1194`) requires **both**
`observed_download_bps` (yours) and `delivered_bps` (the server's) to be
present, `delivered > 0`, and `observed >= 2 * delivered`. Either missing gives
`ThroughputUnreported` and no preparation.

**A client that sends `observed_download_bps: null` can never be offered a
preparation, whatever its capability says.** Dual preparation doubles network
demand, and M5.5 never tested a contended link
(`M6-IMPLEMENTATION-HANDOFF.md` §8) — the floor is the only thing standing in
for that measurement. If your platform currently sends null, fixing that is
part of your brief, not an optimisation.

**And the half you cannot fix from a client: `delivered_bps` is `None` on every
VOD session.** The `FallbackReason::ThroughputUnreported` doc says it in as
many words (`playback_control.rs:1074-1082`): *"`DeliveryView::from_status`
leaves `delivered_bps` `None` on every VOD session, which is most of them."*
Since the floor needs both numbers, **a prepared handoff cannot fire on a VOD
session today, on any platform, however capable the client is.** Only live and
live-recovery sessions can reach it.

Three consequences worth carrying into every plan:

- The `ThroughputUnreported` counter is currently measuring its own missing
  inputs, not tight links. Do not read it as evidence about networks.
- **A hardware acceptance must be taken on a live session.** A tester who
  changes quality on a VOD title and sees no preparation has reproduced this
  gap, not a client bug, and will spend a day on it.
- The same doc comment names this as *"the residual worth re-reading if
  prepared handoffs turn out never to fire"*. If yours never fires, start here
  before you start in your own code.

Closing the VOD half is server work, out of scope for all three client briefs.
File it; do not fix it in a client PR.

---

## C10. Terminal reasons — 16 values, 3 of them permanent

`ProducerDecisionReason` — `playback_control.rs:3847+`. These strings appear in
`delivery.producer_decision`, in `Terminal.code`, and in `RetryResource.reason`.

```
startup_deadline · progress_deadline · exit_classification_deadline
process_exit · partial_success_exit · unsupported · invalid_configuration
reader_failed · flow_stop_failed · flow_resume_failed · flow_stop_deadline
flow_resume_deadline · install_deadline · executor_lost · source_decode_retry
source_decode_failed
```

`is_permanent()` is exactly `{unsupported, invalid_configuration,
source_decode_failed}`. A permanent reason becomes `Terminal`; everything else
becomes `RetryResource`.

**The last two are new in this effort, they describe the same finding, and the
difference between them is the one most worth reading twice.** The decoder's
own diagnostics said the source did not decode — the failure this whole effort
is named after, where the process may exit zero having decoded nothing, and
where every timing reason tells a client to try again and so reproduces the
fault. What separates the two is whether the server has a software-decode
alternate to install:

- **`source_decode_retry`** — it does. Impermanent, so it reaches a client as
  `retry_resource`. The server is bringing up a successor that reads the same
  source differently, and a client that tears down here abandons a recovery
  that was about to work. **No server build emits this yet** — the vocabulary
  landed before the installer that produces it, so handle it and do not wait
  for it. Until that installer ships, this situation reaches you as one of the
  timing reasons above, most often `progress_deadline` or `startup_deadline`.
- **`source_decode_failed`** — it does not. Permanent, so it reaches a client
  as `terminal`, and it is the one verdict in this vocabulary that means *stop
  asking*.

Two cautions:

1. `terminal_message()` has a sentence for `source_decode_failed` but the
   general rule stands: **do not render `message` to a viewer unmodified.** Map
   `code` to your own localised copy.
2. There is a **second, unrelated** terminal vocabulary for session teardown in
   `crates/plurxd/src/vodserve.rs:141-192` (`deleted`, `superseded`,
   `admin_stop`, `revoked`, `replaced`), delivered on `410` answers. It is not
   `ProducerDecisionReason` and must not share a switch statement with it.

---

## C11. There is no `commit_replacement` action

Roadmap §3.1 names three transactions — `prepare_replacement`,
`commit_replacement`, `abort_replacement` — and only the first is an action.
Commit and abort are expressed as **client acknowledgement states**, and the
server's reply on the settling exchange is `{"type":"none"}`
(asserted at `hls.rs:15547`, and again at `:16402` and `:16719`).

So: the client decides when to switch. The server does not order the switch and
cannot; it only stages a successor, waits, and settles what it is told. Any
design that waits for a `commit_replacement` action will wait forever.

---

## C12. Acceptance shared by all three clients

Every platform's brief inherits these. Each is a runnable test or an observable
fact.

1. **The declared vocabulary is exactly
   `["hold","retry_resource","terminal","prepare_replacement"]`**, asserted
   against the serialized request body, not against the in-memory list. The
   serializer is what ships.
2. **A literal-JSON fixture** for the inbound action, asserted key by key:
   ```json
   {"type":"prepare",
    "action_id":"…uuid…",
    "session_id":"…uuid…",
    "playlist_url":"/api/v1/hls/…/index.m3u8",
    "media_origin_ms":0,
    "effective_selection":{"quality_auto":true,"height":1080,
      "audio_track":null,"subtitle_burn":null,"audio_offset_ms":0,
      "codec":"server_selected","dynamic_range":"sdr"}}
   ```
   This exists in **no** test in the repository today. Write it before you write
   the handler; it is the assertion that catches the `prepare` /
   `prepare_replacement` trap.
3. **An unknown `action.type` is still fatal** — the existing behaviour on all
   three clients — and a test proves the new vocabulary did not weaken it.
4. **A `prepare` with a `playlist_url` that is not node-relative is refused**
   by the client, without a network request. Mirror
   `a_relayed_preparation_cannot_point_a_client_anywhere`.
5. **The three terminal acknowledgements round-trip**: `committed` carries
   `first_frame_unix_ms`, `buffer_ready` carries `buffered_through_ms`, and a
   `committed` on a `demand: "end"` exchange is never constructed.
6. **A preparation the client abandons sends `failed` or `aborted`** — proven
   by a test that drives the abandon path and asserts the outbound body, not by
   inspection.
7. **A repeated `action_id` is one preparation.** Feed the same action twice
   and assert exactly one pipeline is built.
8. **`observed_download_bps` is populated** on every exchange where the
   platform can measure it, asserted on the serialized body.

---

## C13. Non-goals — guardrails, each with its reason

- **Do not change `dual_player_preparation` in a client brief.** It is a frozen
  v1 field whose value was measured on hardware
  (`M6-IMPLEMENTATION-HANDOFF.md` §1), and three literals were already written
  by assumption once. Changing it authorises the server to prime a second
  pipeline on a viewer's device. If your measurement disagrees with the table
  in §C14, that is a finding to file, not a literal to edit.
- **Do not add a row to `PREPARED_AXIS_SETS`.** It needs a hardware receipt run
  above the throughput floor. The table's own doc carries the reason.
- **Do not call any prepared path "seamless"** in UI copy, in comments, or in a
  PR description. Plan §5.2 draws the line and M5.5 gave it numbers: web/Safari
  fallback interruption 271–2,246 ms (mean 1,121), Android/Google TV
  353–766 ms (mean 471). Apple's fallback is **unmeasured**, because Apple
  passed dual preparation and never exercised it.
- **Do not describe a finite retained buffer as a running predecessor.**
- **Do not delete the client-side recovery budgets.** They bound the *count* of
  reconnections; `retry_resource { after_ms }` bounds only the *rate*, and
  nothing in the protocol bounds the count (`M6-IMPLEMENTATION-HANDOFF.md` §2,
  Q3). M5c and M5h were struck for exactly this reason.
- **Do not invent a sixth acknowledgement state, a second action name, or an
  extra request field.** §C1.
- **Do not make the second pipeline audible or visible before the switch.** The
  predecessor is the authoritative stream until the moment you commit.

---

## C14. What is measured, what is assumed, and what is open

**Measured on hardware, 2026-09-01 to 2026-09-03**
(`PLAYBACK-CONTROL-STATUS.md:1214-1218`, `M6-AXIS-CASE-HANDOFF.md`):

| platform | `dual_player_preparation` | evidence |
|---|---|---|
| Apple | **`true`** | iPhone 17 Pro Max and Apple TV 4K, 20/20 on both cases, zero stalls, after a first instrument set was rejected and repaired |
| web | `false` | Safari **same-codec reached 20 consecutive**; codec/HDR only 13/20 |
| Android | `false` platform-wide | both phones passed both cases; the tunneled Google TV passed codec/HDR and **failed same-codec 0/3** |

**The finding this document is obliged to record, because it changes what two
of these three sessions are building toward.** The only axes the server will
ever prepare are same-codec (§C9.1). Line the two tables up:

- **Web's `false` is protecting against a case the server cannot offer it.**
  Safari's failure was codec/HDR; its same-codec result was 20/20 — the exact
  axis `PREPARED_AXIS_SETS` admits.
- **Android's `false` is correct for the television class and wrong for
  phones.** The Google TV's only hard failure was on same-codec, which *is* the
  common case; the phones passed both.

Neither observation authorises editing a literal, because the field is
platform-wide and frozen: a `true` on web is also a `true` on every browser
that is not Safari, and a `true` on Android is also a `true` on the Google TV
that failed. **The fix is a narrower capability keyed by axis and device
class, which is a protocol v2 decision, not a client one**
(`M6-IMPLEMENTATION-HANDOFF.md` §3, `PLAYBACK-CONTROL-STATUS.md:1221-1227`).
Recording it here so that three sessions building the client half do not each
rediscover it and each reach a different conclusion.

**Assumed, and recorded as an assumption** (no user decision was available):
all three clients implement the full replacement path now, and Gate A decides
whose path is live. The alternative — build Apple only — was rejected because
it leaves two platforms with no tested code path on the day a narrower
capability lands, which is precisely when the measurement pressure will be
highest.

**Open, and not answerable from this document:**

- How long a client may wait for an action before falling back (roadmap §8 Q1).
- Whether `terminal` ends playback or offers a *Try again* (Q2). This is a
  product decision, and the three clients currently differ.
- Twenty consecutive commits on a realistic runway — nobody has them; M5.5's
  20/20 ran on a fixture short enough to buffer completely.
- Apple decoder memory across repeated trials: AVFoundation decodes in
  `mediaserverd` and the tooling exposes no resident memory for
  `mediaplaybackd` / `videocodecd`, so the one measurement that would close the
  runway residual is the one that cannot currently be taken.
- Behaviour under an active throughput drop. The whole spike ran on a steady
  shaped 80 Mbit/s link. The throughput floor (§C9.2) stands in for this and has
  not been validated against a real contended link.
