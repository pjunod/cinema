# M5 fleet results — server and clients at `c571a50d`

**Status:** deployment complete · acceptance failed/blocked ·
**Executed:** 2026-08-31 · **Server:** `c571a50d` ·
**Clients:** Apple build 101 · Android versionCode 57

Companion to [M5-FLEET-ACCEPTANCE.md](M5-FLEET-ACCEPTANCE.md), which
defines the criteria. This document records only the second requested run:
the serial server rollout at `c571a50d`, followed by Apple build 101 and
Android versionCode 57. The earlier client-only run is recorded in
[M5-FLEET-RESULTS-943D8A9A.md](M5-FLEET-RESULTS-943D8A9A.md).

## 1. Result — rollout passed, M5 acceptance did not

All four servers reached the exact requested commit and passed `readyz` before
the next server was touched. Apple build 101 and Android versionCode 57 were
installed on every reachable device.

M5 acceptance did not pass. The final web client fails before its first
control exchange, while the mobile viewers were locked, asleep, or offline.
Every final `complete="true"` and accepted-exchange counter remained zero.

Do not remove a client recovery path on this evidence.

## 2. Server deployment — one ready voter before the next

The Ansible deployment ran with `serial: 1` in inventory order:

```text
nynuc -> m6 -> nuc4 -> nuc3
```

Each node completed deployment, container health, and the published `readyz`
check before Ansible advanced. Post-deployment verification read the exact
commit directly from every node.

| Node | Commit | Reported build | `readyz` |
|---|---|---|---|
| nynuc | `c571a50d5fc58b32bdbc3d32efdea133e619b79f` | `v0.2.8-210-gc571a50d` | `ready` |
| m6 | `c571a50d5fc58b32bdbc3d32efdea133e619b79f` | `v0.2.8-210-gc571a50d` | `ready` |
| nuc4 | `c571a50d5fc58b32bdbc3d32efdea133e619b79f` | `v0.2.8-210-gc571a50d` | `ready` |
| nuc3 | `c571a50d5fc58b32bdbc3d32efdea133e619b79f` | `v0.2.8-210-gc571a50d` | `ready` |

## 3. Physical deployment — final installed roster

`scripts/ship-physical` again completed without an error. It built and
verified signed Apple Release artifacts and the signed Android APK before
installation.

| Device | Platform | Final result |
|---|---|---|
| 17air | iOS | Release 0.2.8 build 101 installed and reverified |
| Bedroom | tvOS | Release 0.2.8 build 101 installed; device asleep |
| iPad Mini | iPadOS | Release 0.2.8 build 101 installed and reverified |
| iPad Pro | iPadOS | Release 0.2.8 build 101 installed and reverified |
| Pixel 11 Pro XL | Android | 0.2.8 versionCode 57 installed; device locked |
| 16pro | iOS | Unavailable; not installed |
| 17promax | iOS | Unavailable; not installed |
| Motorola razr ultra 2025 | Android | Offline; not present in ADB roster |
| Xiaomi 25019PNF3C | Android | Offline; not present in ADB roster |
| TCL 9445X | Android | Offline; optional target not installed |
| Pixel 10 Pro Fold | Android | No longer present in ADB roster |

The retained release worktree is:

```text
/private/tmp/plurx-943-release.SsOwZA/artifacts/plurx-c571a50d5fc5
```

## 4. Playback observations — final web behavior and mobile blockers

### Normal web playback produced a picture but no verdict exchange

The web client played *Good Cop / Bad Cop* against nuc3. VOD HLS remux
rendered a picture and reported an approximately 2.6-second start. The Control
panel remained:

```text
immutable VOD · active · awaiting first acceptance · exchange in flight
```

The reporter reproduced `TypeError: Illegal invocation` at
`playback-control.js:237:35`. The defect is the same receiver error recorded in
the first run: native `setTimeout` is stored as a field and invoked as an
instance method. The request never reached `/playback/control`.

### A damaged-index source still produced a picture

*The Gang Deals with Alternate Reality* had an incomplete VOD analysis result.
The server used Live HLS remux, produced a picture, and reported an
approximately 3.2-second start. Control again stayed at `awaiting first
acceptance` because the reporter failed.

### The VC-1 compatibility case produced no picture or transcode

*17 Again* uses VC-1 video and TrueHD audio. With forced original quality, the
viewer showed a black screen while the clock advanced. Playback diagnostics
reported:

```text
Delivery method  Remux
Reason           forced original quality (no video transcode)
Rendered         0 / 0 frames
Buffer           approximately 2.4 s
```

The compatibility transcode did not run and did not produce a picture. This
is relevant to §4.1, but it is not the requested terminal proof: the producer
did not send `terminal` because the reporter never completed an exchange.

### Apple and Android did not reach playback

The repository's unattended Apple path requires a signed Debug build. A signed
Debug build 101 was built successfully, temporarily installed on 17air, iPad
Mini, and iPad Pro, and passed to `scripts/playback-lab device-run`. Each run
failed at `devicectl` foreground launch with:

```text
Unable to launch tv.plurx.app because the device was not, or could not be,
unlocked.
```

Release build 101 was then reinstalled and reverified on all three devices.
Bedroom reported `System is asleep - foreground app launch forbidden`.

The Pixel reported `mDreamingLockscreen=true` with the notification shade in
focus. No Android versionCode 57 viewer run occurred.

## 5. Final metrics — every node remained at zero

The order in each triplet below is `web / apple / android`.

| Node | `complete="true"` | `complete="false"` | suppressed | accepted by platform | global accepted |
|---|---:|---:|---:|---:|---:|
| nuc3 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 |
| nuc4 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 |
| m6 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 |
| nynuc | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 |

“Accepted by platform” is
`plurx_playback_control_platform_exchanges_total{outcome="accepted",platform="..."}`.
The global column is
`plurx_playback_control_exchanges_total{outcome="accepted"}`.

**How to read this table:** `complete="false"` being zero shows that no old
client reached these freshly restarted processes. It does not constitute
acceptance because `complete="true"` is also zero.

## 6. Section 5 evidence table — second request

| Platform | `vocabulary_total{complete="true"}` | `vocabulary_total{complete="false"}` | `actions_suppressed_total` | Terminal seen | Hold seen |
|---|---:|---:|---:|---|---|
| Web | 0 on every node | 0 on every node | 0 | No; reporter failed before exchange one | No |
| Apple | 0 on every node | 0 on every node | 0 | Not run; hardware locked/asleep | Not observed |
| Android | 0 on every node | 0 on every node | 0 | Not run; Pixel locked and remaining roster offline | Not observed |

## 7. Section 4 findings — what the viewer actually saw

### §4.1 terminal and compatibility transcode

No platform received a server `terminal` during the final run. Buffered
play-out, the server's sentence, *Try again*, and *Force transcode* therefore
remain unverified.

The web compatibility candidate showed a black screen and never switched from
remux to transcode. Because the control exchange was already broken, this is a
compatibility-path failure, not a clean falsification of the ruling about how
a received `terminal` is scoped.

### §4.2 bounded stall ask

No verdict-mediated stall completed. The extra 1.5-second wait, and its one
extension to 3 seconds, could not be compared with the previous recovery
timing. The normal web starts of approximately 2.6 and 3.2 seconds are startup
measurements, not stall-recovery measurements.

### §4.3 hold without churn

No final-build hold was emitted because no final client completed an exchange.
There is no viewer observation of a web/Apple explanation, Android silence,
or reconnect churn.

## 8. Contradictions and blockers

1. **The shipped web client cannot complete exchange one.** This contradicts
   the prerequisite that a server deploy cheaply places a working
   full-vocabulary web client on the fleet.
2. **The VC-1 viewer produced no picture and no compatibility transcode.** It
   exposes a real black-screen path, although the broken reporter prevents the
   requested terminal-scope conclusion.
3. **The physical roster was not fully available.** Two reported Apple devices
   were unavailable, three named Android devices were offline, and the
   reachable mobile viewers were locked or asleep.

## 9. Verdict — deployment is complete, acceptance is not

The exact requested server and client builds are installed wherever the roster
was reachable. M5 remains unaccepted: every full-vocabulary counter is zero,
the web reporter fails before exchange one, and no physical mobile viewer
completed §4.

Keep every existing client recovery path. Resume acceptance only after fixing
the web reporter and unlocking or waking the physical devices.

