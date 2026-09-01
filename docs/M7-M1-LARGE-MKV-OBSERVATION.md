# M7 M1 large-MKV observation — readiness and bounded subtitle publication

**Status:** O1 and O2 passed on local hardware · **Exercises:** M7 readiness
M1-B2 and remediation ruling R1 · **Observed:** 2026-09-01

Companion to the M7 remainder handoff in
[#740](https://github.com/pjunod/plurx/pull/740) — this is the dated hardware
evidence for the large-file `warming` → `ready` transition and the two
first-half publication-deadline observations. It records what the local run
proved, the exact cold-cache method, and what still needs a deployed client.

## Verdict — both local observations are green

| Requirement | Result | Evidence |
|---|---|---|
| O1: a selected native text track reports `warming`, then `ready`, without a client change | **Passed** | Accepted control sequence 1 reported `warming`; sequence 2 reported `ready` after the whole-track sidecar published |
| O2: two first-half anchors serve real cues within the publication deadline | **Passed** | 20-minute anchor: 122.461 ms; 30-minute anchor: 155.526 ms; deadline: 5,000 ms |

These results clear the local hardware observations named by the remediation
plan. They do not clear O3: the one-directed-retry observation on web, Apple,
and Android belongs after R-M2 is merged and deployed.

## Setup — exact `main`, one isolated large file

| Fact | Observed value |
|---|---|
| Source commit | `69d3f3de51820bbe138433e11e2f3782c0c23d11` |
| Binary build stamp | `69d3f3de` |
| Compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Runtime | local loopback-only `plurxd`; no deploy and no fleet traffic |
| Host | arm64 · macOS 26.6.2 · local storage |
| ffmpeg / ffprobe | 8.1.2 / 8.1.2 |
| File | `After Armageddon (2010) HDTV-720p.mkv` |
| File size | 2,252,078,306 bytes (2.10 GiB) |
| Duration | 4,917.160 seconds (81m 57.160s) |
| Video / audio | H.264 High 1280×720 · AC-3 5.1 English |
| Selected subtitle | native track 0 · SubRip · English |
| Subtitle window | default 200 seconds |
| HTTP publication budget | 5,000 ms, from `RESPONSE_PUBLICATION_LIFECYCLE_BUDGET` |

The source tree came from `git archive origin/main`, so neither the dirty
primary checkout nor the R-M1 candidate was in the runtime binary. The build
set `PLURX_BUILD_REF=69d3f3de`; the server startup line reported that stamp.

The server used fresh temporary data and cache roots. Its only library root
contained one hard link to the file above, so the scan could not wander into
another library. The scan added one file with zero errors. Playback control v1
was enabled only in that disposable store.

## Method — cold cache, accepted snapshots, exact subtitle segments

**O1:** a one-off HTTP driver opened a native-subtitle VOD HLS session using
the same request and control shapes as the shipped web reporter. The producer
started and published 192,040 ms of video presentation. The driver sent an
active control snapshot with native track 0 selected, fetched the subtitle
playlist to start the ordinary warm path, then sent the next control snapshot.
No client or server source was changed for this run.

**O2:** before each anchor, every live sidecar for this file and track was
moved out of the active cache and retained under an evidence suffix; nothing
was deleted. The driver then sent an accepted `seeking` control snapshot and
requested the exact subtitle segment covering the target. One bounded process
recorded the first response and polled that same segment every 25 ms for at
most 10 seconds. A response counted as real only when its WebVTT body contained
a cue timing line (`-->`).

This distinction matters: a cache-header transition alone would show
publication, but would not prove that the requested interval contained a cue.

## O1 — control readiness changed without a client change

| Accepted sequence | Server time (UTC) | Selected mode | Readiness |
|---:|---|---|---|
| 1 | 2026-09-01 18:59:09.659 | native track 0 | `warming` |
| 2 | 2026-09-01 18:59:42.273 | native track 0 | `ready` |

The subtitle playlist request started the existing whole-track extraction at
18:59:21.910 UTC. The sidecar published at 18:59:22.188 UTC, and the server
logged `elapsed_ms=278`. Sequence 2 therefore observed a completed cache fact;
it did not infer readiness from elapsed time or from playback state.

**How to read it:** the 32.614-second gap between control exchanges is manual
observation cadence, not extraction cost. The producer's own 278 ms log is the
whole-track materialization time on this host.

## O2 — two first-half anchors met the 5,000 ms deadline

| Seek snapshot | Target | Segment and source interval | First response | First real cues after empty | Polls | Result |
|---:|---:|---|---|---:|---:|---|
| 3 | 1,200 s (20:00) | `seg00153.vtt` · 1,203.760–1,212.640 s | 200 · `no-store` · 21.941 ms · no cues | 122.461 ms | 5 | **Passed** |
| 4 | 1,800 s (30:00) | `seg00228.vtt` · 1,793.520–1,800.920 s | 200 · `no-store` · 23.043 ms · no cues | 155.526 ms | 6 | **Passed** |

Both targets are before the source midpoint at 2,458.580 seconds. The first
responses were syntactically valid WebVTT with an `X-TIMESTAMP-MAP`, no cue
timing line, and `cache-control: no-store`. The later responses for the same
URLs contained real dialogue cues and remained `no-store`, proving that the
200-second window bridge — rather than the later whole-track sidecar — supplied
the first real answer.

The server logs agree with the client-side clock:

| Target | Window anchor | Window publication log | Whole-track publication log |
|---:|---:|---:|---:|
| 1,200 s | 1,200 s | 121 ms | 279 ms |
| 1,800 s | 1,600 s | 148 ms | 294 ms |

**How to read it:** lower is better, but the contract is binary at 5,000 ms.
The observed 122.461 ms and 155.526 ms results are both inside that budget by
more than 4.8 seconds. They establish ruling R1 on this arm64 local-storage
host and this 2.10 GiB MKV; they are not a NAS-performance claim.

## Boundaries — what this evidence does not claim

- It does not prove O3's client-directed retry. That requires merged and
  deployed R-M2 clients on web, Apple, and Android.
- It does not claim NAS timing. The file was on local storage; a slower storage
  path needs its own retained observation before anyone generalizes the
  measured milliseconds.
- It does not change the midpoint decline rule. Both measured anchors were in
  the first half, where windowing is admitted; past the midpoint, the
  whole-track warm remains the only producer and readiness must stay honest.
- It does not replace video with subtitle work, add a detector or recovery
  opinion, exercise burn-join, touch prewarm, or deploy anything.
