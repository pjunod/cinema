# M5 fleet results — client build 99 / versionCode 56 at `943d8a9a`
**Status:** deployment complete · acceptance not established ·
**Executed:** 2026-08-31 · **Source:** `943d8a9a` ·
**Clients:** Apple build 99 · Android versionCode 56

Companion to [M5-FLEET-ACCEPTANCE.md](M5-FLEET-ACCEPTANCE.md), which
defines the criteria. This document records only the first requested run:
shipping the clients from `943d8a9a` against the servers that were already
running. The later `c571a50d` server and client rollout is recorded separately
in [M5-FLEET-RESULTS-C571A50D.md](M5-FLEET-RESULTS-C571A50D.md).

## 1. Result — installation passed, behavioral acceptance did not

`scripts/ship-physical` completed its first end-to-end run without an error.
It built, signed, verified, and installed Apple build 99 and Android
versionCode 56 on every device it could reach.

The run did not establish M5 acceptance. The web player rendered media but
its control reporter failed before exchange one. The reachable Apple and
Android hardware was locked or asleep, so it did not produce an attributable
playback exchange or viewer observation.

Do not remove a client recovery path on this evidence. Installation proves the
artifacts can reach hardware; it does not prove that the replacement recovery
behavior ran.

## 2. Physical deployment — what received the requested build

The script ran unmodified from an isolated release mirror whose `origin/main`
was pinned to `943d8a9a`. This kept the existing working checkout untouched
while satisfying the script's requirement to build from `origin/main`.

| Device | Platform | Result |
|---|---|---|
| 17air | iOS | Apple build 99 installed |
| Bedroom | tvOS | Apple build 99 installed |
| iPad Mini | iPadOS | Apple build 99 installed |
| iPad Pro | iPadOS | Apple build 99 installed |
| Pixel 11 Pro XL | Android | versionCode 56 installed |
| 16pro | iOS | Unavailable; not installed |
| 17promax | iOS | Became unavailable before installation |
| Motorola razr ultra 2025 | Android | Offline; not present in ADB roster |
| Xiaomi 25019PNF3C | Android | Offline; not present in ADB roster |
| TCL 9445X | Android | Offline; optional target not installed |
| Pixel 10 Pro Fold | Android | Connected briefly, then disappeared from ADB |

The retained release worktree is:

```text
/private/tmp/plurx-943-release.SsOwZA/artifacts/plurx-943d8a9a07d9
```

## 3. Playback attempts — which platform produced which result

### Web rendered video but never asked the server

The web client played file 247, item 264, *Good Cop / Bad Cop*, against lab3.
It rendered VOD HLS using a remux and reported an approximately 2.9-second
start. Its Control panel remained:

```text
immutable VOD · active · awaiting first acceptance · exchange in flight
```

The reporter then threw:

```text
TypeError: Illegal invocation
    at Reporter.drain (.../assets/playback-control.js:237:35)
```

The failure occurs where the reporter stores native `setTimeout` and later
invokes it as `this.setTimer(...)`; the browser rejects that receiver. No web
control request reached the server.

### Apple could not reach the viewer test

Bedroom refused foreground launch because the Apple TV was asleep. iPad Mini
refused launch because it was locked. Direct launch probes on 17air and iPad
Pro also encountered locked-device behavior during the acceptance attempts.
No title was played through an Apple build 99 acceptance run.

### Android could not reach the viewer test

The Pixel accepted installation, but its UI remained on the secure keyguard.
No title was played through Android versionCode 56. The other requested Android
devices were offline.

## 4. Metrics — old-client traffic is not evidence for this run

Before the later server restart, the existing nodes contained the following
non-zero counters. They were present without a successful build 99 or
versionCode 56 playback and therefore cannot be attributed to this run.

| Node | Platform | `complete="true"` | `complete="false"` | suppressed | accepted | `none` | `hold` |
|---|---|---:|---:|---:|---:|---:|---:|
| lab6 | Apple | 0 | 4,657 | 0 | 4,657 | 1,218 | 3,439 |
| media1 | Android | 0 | 3 | 0 | 3 | 1 | 2 |

lab3 and lab4 were zero for all requested counters. Every unlisted
node/platform pair was also zero. The non-zero rows declare
`complete="false"`, which identifies incomplete older clients rather than the
full M5 vocabulary required for acceptance.

## 5. Acceptance evidence table — first request

| Platform | `vocabulary_total{complete="true"}` | `vocabulary_total{complete="false"}` | `actions_suppressed_total` | Terminal seen | Hold seen |
|---|---:|---:|---:|---|---|
| Web | 0 | 0 | 0 | No; reporter failed before exchange one | No |
| Apple | 0 attributable to build 99 | 4,657 old-client exchanges on lab6 | 0 | Not run; hardware locked/asleep | Not observed |
| Android | 0 attributable to versionCode 56 | 3 old-client exchanges on media1 | 0 | Not run; Pixel locked | Not observed |

**How to read this table:** zero `complete="true"` means no full-vocabulary
client proved an exchange. The non-zero `complete="false"` values are
disqualifying legacy traffic, not partial credit for the requested builds.

## 6. Section 4 observations — none of the rulings was settled

**§4.1 terminal:** no producer-refused source completed a control exchange.
There is no observation of buffered play-out, the server's sentence,
*Try again*, or *Force transcode* from build 99 or versionCode 56.

**§4.2 bounded ask:** no verdict-mediated physical stall ran, so the added
1.5-second wait and one extension to 3 seconds were not measured.

**§4.3 hold:** the old counters contain Apple and Android holds, but no viewer
was observed and the clients declared incomplete vocabulary. Those counters
cannot settle the ruling.

## 7. Starting-state discrepancies

The device state observed at execution time differed from the request's
baseline: reachable Apple devices reported build 94 rather than build 86, and
the Pixel reported versionCode 53 rather than 47. This does not change what
was installed, but it matters when attributing the pre-existing counters.

## 8. Verdict — keep every existing recovery path

The first request proves that `scripts/ship-physical` works end to end and that
the signed artifacts can be installed on reachable hardware. It does not prove
M5 behavior on any platform. The web reporter defect and inaccessible physical
viewers prevent acceptance.
