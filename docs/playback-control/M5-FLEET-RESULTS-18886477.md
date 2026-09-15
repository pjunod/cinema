# M5 fleet results — web and Apple exchange at `18886477`

**Status:** protocol partially accepted · behavioral acceptance incomplete ·
**Executed:** 2026-09-01 · **Server:** `18886477` ·
**Clients:** Apple build 103 · Android versionCode 59

Companion to [M5-FLEET-ACCEPTANCE.md](M5-FLEET-ACCEPTANCE.md), which
defines the criteria. This document records the rerun requested after the
cluster repair. It separates protocol evidence from viewer evidence because a
counter moving proves that a client can receive a ruling; it does not prove
that the replacement behavior worked.

## 1. Result — web and Apple exchanged, Android did not run

The web gate passed on lab3. Its Control panel left *awaiting first
acceptance*, and the node's full-vocabulary web counter rose from zero to six
during the initial observation. It reached 27 before the final snapshot.

Apple build 103 also completed full-vocabulary exchanges. lab6 recorded 228
complete Apple exchanges and a real `server_hold` while an Apple viewer was
attached. No incomplete-vocabulary or suppressed-action counter moved.

The run did not complete M5 acceptance. Both usable Pixels returned to their
fingerprint lock screens, the Xiaomi refused ADB installation, and no Android
viewer ran. No platform received a `terminal`, the bounded `none` timing was
not measured, and the Apple hold explanation was not read from the screen.

Do not remove a client recovery path on this evidence.

## 2. Artifact identity — report the runtime and release source separately

All four servers reported the same runtime artifact:

```text
v0.3.0-64-g18886477
```

`origin/main` was `bbbf83be` when `scripts/ship-physical` built the Android
artifact. The commits between `18886477` and `bbbf83be` change only
`docs/playback-control/PLAYBACK-CONTROL-STATUS.md`; Apple and Android source and build numbers
are unchanged.

| Artifact | Commit or build | Observed result |
|---|---|---|
| Server and web client | `18886477` | Running identically on all four nodes |
| Mobile release source | `bbbf83be` | Documentation-only delta from the server commit |
| Apple | build 103 | Installed on every reachable Apple device |
| Android | versionCode 59 | Built and signature-verified; installed on both Pixels |

**How to read this table:** the server runtime SHA is the commit actually
serving requests. `bbbf83be` is the commit from which the shipping script built
the version-59 APK. The different SHAs do not represent different client code.

## 3. Cluster boundary — every voter was ready before acceptance began

The rerun did not redeploy the cluster. Another session repaired it, and this
run waited for the serial-deployment boundary before touching a client.

At 03:45:45Z, 03:45:57Z, and 03:46:09Z, lab3, lab4, lab6, and media1 each
returned `readyz=200` and the exact build
`v0.3.0-64-g18886477`. A final identity read after testing returned the same
build on every node.

| Node | Reported version | Reported build | Readiness boundary |
|---|---|---|---|
| lab3 | 0.3.0 | `v0.3.0-64-g18886477` | Three consecutive `readyz=200` readings |
| lab4 | 0.3.0 | `v0.3.0-64-g18886477` | Three consecutive `readyz=200` readings |
| lab6 | 0.3.0 | `v0.3.0-64-g18886477` | Three consecutive `readyz=200` readings |
| media1 | 0.3.0 | `v0.3.0-64-g18886477` | Three consecutive `readyz=200` readings |

## 4. Web gate — the reporter completed exchanges on lab3

The web player used *Eye for an Eye (2025)* on lab3. Mac output was muted
before playback and restored after the player was closed.

The Control panel changed to:

```text
demand-owned · active · accepted #21 · 1 s ago · scheduled
```

### Vocabulary before playback

```text
plurx_playback_control_vocabulary_total{complete="false",platform="web"} 0
plurx_playback_control_vocabulary_total{complete="false",platform="apple"} 0
plurx_playback_control_vocabulary_total{complete="false",platform="android"} 0
plurx_playback_control_vocabulary_total{complete="true",platform="web"} 0
plurx_playback_control_vocabulary_total{complete="true",platform="apple"} 0
plurx_playback_control_vocabulary_total{complete="true",platform="android"} 0
```

### Vocabulary immediately after playback began

```text
plurx_playback_control_vocabulary_total{complete="false",platform="web"} 0
plurx_playback_control_vocabulary_total{complete="false",platform="apple"} 0
plurx_playback_control_vocabulary_total{complete="false",platform="android"} 0
plurx_playback_control_vocabulary_total{complete="true",platform="web"} 6
plurx_playback_control_vocabulary_total{complete="true",platform="apple"} 0
plurx_playback_control_vocabulary_total{complete="true",platform="android"} 0
```

Every `actions_suppressed_total` label was zero before and after.

The reporter passed, but the media stream did not. The viewer remained on
*Preparing the stream…* and then read *The server couldn't build the stream*.
It did not produce a picture.

### Browser console since the gate began

The following are the fresh page-origin lines, verbatim. Extension messages
and page lines that predated the gate are excluded.

```text
2026-09-01T03:50:39.174Z http://lab3:32400/ [cinema] hls.js fatal networkError manifestLoadTimeOut
2026-09-01T03:51:14.158Z http://lab3:32400/ [cinema] playback stall diagnosis Object
```

## 5. Mobile deployment — matching builds reached only part of the roster

Apple bundle version 103 was read directly from every reachable Apple
device. Android versionCode 59 was read from both connected Pixels.
`scripts/ship-physical --android` rebuilt and signature-verified version 59,
then attempted the reachable supported roster.

| Device | Platform | Result |
|---|---|---|
| 17air | iOS | Apple build 103 verified |
| 17promax | iOS | Apple build 103 verified and used for the Apple arm |
| Bedroom | tvOS | Apple build 103 verified |
| iPad Mini | iPadOS | Apple build 103 verified |
| iPad Pro | iPadOS | Apple build 103 verified |
| 16pro | iOS | Unavailable |
| Pixel 11 Pro XL | Android | versionCode 59 verified; fingerprint-locked at playback time |
| Pixel 10 Pro Fold | Android | versionCode 59 verified; fingerprint-locked at playback time |
| Xiaomi 25019PNF3C | Android | Install failed: `INSTALL_FAILED_USER_RESTRICTED: Install canceled by user` |
| Motorola razr ultra 2025 | Android | Offline; absent from ADB roster |
| TCL 9445X | Android | Offline; absent from ADB roster |
| Google TV Streamer | Android TV | versionCode 59 present; not substituted for a phone |

Installation failed on the Xiaomi because MIUI opened its ADB-install approval
path and immediately canceled it. No existing app was removed to work around
that device policy.

## 6. Apple arm — complete vocabulary and a real hold on lab6

Launching build 103 on the iPhone 17 Pro Max reattached an Apple viewer to
*Heavy Is the Head* on lab6. The Apple complete-vocabulary counter was already
moving when first sampled and reached 228 before the final snapshot.

lab6 recorded a real `server_hold`. During the hold, repeated client reports
kept the same delivery attempt and named `outcome=server_hold`; the server
held and later resumed the producer. That is evidence against an immediate
reopen during the hold, but the viewer's on-screen explanation was not read.
The full §4.3 ruling therefore remains unaccepted.

Earlier in the overall session, before the cluster restart reset its counters,
Apple build 103 played the VC-1 *17 Again* compatibility transcode and the
viewer confirmed a picture. That observation is useful, but it is not counted
as clean rerun evidence because it cannot be joined to the post-restart metric
snapshot.

## 7. Final metrics — only lab3 web and lab6 Apple moved

| Node | Platform | `complete="true"` | `complete="false"` | Suppressed |
|---|---|---:|---:|---:|
| lab3 | Web | 27 | 0 | 0 |
| lab3 | Apple | 0 | 0 | 0 |
| lab3 | Android | 0 | 0 | 0 |
| lab4 | Web | 0 | 0 | 0 |
| lab4 | Apple | 0 | 0 | 0 |
| lab4 | Android | 0 | 0 | 0 |
| lab6 | Web | 0 | 0 | 0 |
| lab6 | Apple | 228 | 0 | 0 |
| lab6 | Android | 0 | 0 | 0 |
| media1 | Web | 0 | 0 | 0 |
| media1 | Apple | 0 | 0 | 0 |
| media1 | Android | 0 | 0 | 0 |

**How to read this table:** lab3 proves the deployed web client can complete a
full-vocabulary exchange. lab6 proves an installed Apple build 103 can do the
same. Android's zero means no Android client ran; it is not a pass or an
incomplete-vocabulary failure. Every `complete="false"` value remaining zero
means no observed client declared an older vocabulary.

## 8. Section 4 observations — protocol passed farther than behavior

| Platform | Terminal seen | Hold seen | What the viewer read |
|---|---|---|---|
| Web | No | No | *Preparing the stream…*, then *The server couldn't build the stream* |
| Apple | No | Yes, server-side | Hold wording not observed during this rerun |
| Android | Not run | Not run | No viewer; both usable Pixels were locked |

### §4.1 terminal and compatibility transcode

No post-restart client received a server `terminal`. Buffered play-out, the
server's sentence, *Try again*, *Force transcode*, and the compatibility
fallback after that verdict remain unverified as one attributable run.

### §4.2 bounded `none` timing

No real stall against a server answering `none` was measured. The extra
1.5-second wait and its one extension to at most 3 seconds remain unverified.
The web `manifestLoadTimeOut` is not a substitute for that measurement.

### §4.3 hold without churn

Apple produced a real `server_hold`, and server/client logs retained the same
delivery attempt during the hold. The on-screen explanation was not observed,
so the viewer half of the ruling remains open. Web did not produce a hold, and
Android did not run.

## 9. Cleanup and verdict — preserve every recovery owner

The web player was closed and Mac audio restored. The iPhone process was
terminated. Plurx was force-stopped on every Android phone touched by the run.
The Pixel 11's original 30-minute screen timeout, disabled stay-awake value,
unmuted state, and 9/25 media volume were restored and verified.

This rerun establishes two facts that earlier fleet attempts did not:

1. **The deployed web reporter can complete a full-vocabulary exchange.** The
   `Illegal invocation` failure no longer blocks exchange one.
2. **Apple build 103 can complete full-vocabulary exchanges and remain attached
   through a server hold.** The viewer explanation and terminal path still
   need direct observation.

Android and every terminal-dependent behavior remain unaccepted. Keep every
existing client recovery path until the remaining physical runs provide the
viewer evidence required by [M5-FLEET-ACCEPTANCE.md](M5-FLEET-ACCEPTANCE.md).
