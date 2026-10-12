# Playback startup path — where the seconds go, and what removes them

**Status:** open: two fixes are on `effort/startup-read-path`, unmerged; the
rest is ranked below, each with its owner or the ruling it waits on · **Written:** 2026-10-11

Companion to
[PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md](PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md)
(the measured starts) and
[STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md](STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md)
(the approved build for the rolling target). This document is the end-to-end
trace from the Play press to the first frame on every client and route, read
from source at `85d98cd6c`: which waits exist, which are fixed here, and which
remain.

The report it answers: startup is inconsistent, sometimes over ten seconds,
and should be about two.

## 1. The answer in one table

A start is the sum of four phases. The first two are the same on every
route; the third decides whether a start is two seconds or twelve.

| Phase | Before | After this branch | What is left |
|---|---|---|---|
| Decision request | about 0.4 to 0.8 s on a cluster (about a dozen consistent reads) | about 0.5 s less; the residual is unmeasured (§2) | a fleet measurement (§6) |
| Session create | 1.6 to 2.2 s on a cluster (25 to 34 consistent reads) | 1.1 to 1.5 s less; 0.1 s if the fleet matches loopback, up to 1 s if it does not (§2) | a fleet measurement (§6), then connection reuse if it is needed |
| First playable media | 0.5 to 2 s with a fragment index; **3.6 to 10.6 s without one** (the 48 s first-publication gate) | unchanged | §4.1, §4.2 |
| Client attach to visible picture | 0.2 to 2.3 s, plus 0.5 to 1.0 s of scrim on web | web scrim removed | §5 |

So this branch takes about 1.6 s off every clustered start (40 ms on each of
about forty reads) and up to a further second off every web start. It does not touch the wait that makes
some titles take ten seconds: a title with no current fragment index still
starts through the rolling engine and still waits for 48 s of media.

## 2. Every consistent read waited 40 ms on a delayed ACK

**Fixed here.** This is the cause of the 2.5 s create handler measured on
media1 and of the 1.6 to 2.2 s that remained after the read reductions in
PR #1019.

A consistent (linearizable) read asks OpenRaft to confirm leadership.
OpenRaft 0.9.25 does that by calling the network factory's `new_client` for
every other voter on every check (`handle_check_is_leader_request`), and the
vendored transport answers each `new_client` with a new TCP connection, TLS
handshake, WebSocket upgrade and challenge exchange
(`vendor/hiqlite/src/network/raft_client.rs`, `ws_handler`). The connection
carries one empty AppendEntries and is dropped.

The 40 ms is inside that connection setup:

```
 leader (connecting)                    follower (accepting, Nagle on)
   TLS finished + HTTP upgrade  ─────▶
                                ◀─────  101 Switching Protocols   (sent)
                                        handshake challenge       (held:
                                          Nagle waits for the ACK of the 101)
   has nothing to send until it
   reads the challenge, so its
   ACK is delayed ~40 ms        ─────▶
                                ◀─────  handshake challenge       (released)
```

The follower's listener never set `TCP_NODELAY`. plurxd's own HTTP listener
does, and has a test for it; the two cluster listeners did not.

**Measured** on a three-voter TLS cluster on loopback, 50 serial
`query_consistent_map` calls per node:

| | Per consistent read | Quorum write | Local read |
|---|---:|---:|---:|
| Before | 44.0 ms (p50 44.0, max 48.1) | 0.8 ms | 0.05 ms |
| After | 1.7 ms (p50 1.7, max 5.2) | 1.0 ms | 0.04 ms |
| Without TLS, before | 0.7 ms | 0.75 ms | 0.04 ms |

**How to read it:** a read cost fifty times a replicated write, which is
backwards, and it was constant to a tenth of a millisecond, which is a timer
and not work. A syscall trace of one read shows the follower's two writes
0.2 ms apart and the leader's next read 42 ms later. Without TLS the same
two writes do not stall, which is why the plain-TCP row is fast; the trace
does not say why, and nothing here depends on it.

**What this does not prove about the fleet.** There the same read was about
55 to 76 ms. The fix removes the 40 ms. The other 15 to 35 ms is the rest of
the connection setup (two TLS handshakes and about seven round trips per
read) on real CPUs and a real network, and it stays. So expect roughly 15 to
35 ms per read on the fleet after the fix, not 1.7 ms, until §6 is measured.

**The fix** is vendor patch 26: the Raft and API listeners disable Nagle on
accepted streams in all three shapes `start_node` serves
(`vendor/hiqlite/src/start.rs`). It changes no wire format, consistency
class, call site or fence. It takes effect on a read once the *other* voters
run the build, because the stall is on the accepting side.

What a playback start gains: the decision makes about a dozen of these reads,
the create 25 to 34, and each authenticated request one more. Every other
request on a clustered server gains the same per read.

**Not built: reusing the leadership-check connection.** After the fix each
read still sets up a connection to every other voter. Parking a cleanly
finished connection for the next check would bring a read near the cost of a
write (one round trip) and end the connection churn, but it reaches into the
connection supervisor that patches 9 to 11 and 25 repaired. If the fleet
residual is 15 to 35 ms per read, the create still spends 0.4 to 1.0 s on
reads and this is the next thing to build; if it is a few milliseconds, it
is not worth the risk.

## 3. The web player covered a picture that was already moving

**Fixed here.** The progress tick proves presentation by seeing the film
clock and the presented-frame count advance between two of its own samples,
and it ran only on the 500 ms sampling timer. After `playing`, the first tick
is the baseline and the second is the proof. The "Preparing the stream"
scrim, and the HLS `ttff` stamp, therefore waited 500 to 1000 ms after the
picture started.

The presented-frame callback now runs the same tick for a newly attached
stream until it records the proof (`sampleStartupPresentation` in
`crates/plurxd/src/web/player/decode-tiers.js`). The evidence rule of the
playback surface contract is unchanged: `playing` still proves nothing, and
the clock and the frame count must both advance. Two consecutive frames now
satisfy it. Sampling stands down after one second, and while paused, seeking
or with a control seek pending, so a stream that never presents is diagnosed
by the timer exactly as before.

Web HLS `ttff` values fall by 500 to 1000 ms from this build on. That is the
measurement becoming correct, not a second improvement; direct-play `ttff`
was already stamped at `playing` and does not move.

## 4. What still takes seconds on the server

### 4.1 A title without a current fragment index waits for 48 s of media

This is the ten-second start, and the inconsistency. A copy start is served
as immutable VOD only when a fragment index exists for the exact engine
digest; otherwise it falls back to the rolling engine, whose first playlist
is withheld until 48 s of media past the viewer's position exists
(`ROLLING_INITIAL_RUNWAY_MS`, three times the 16 s target, in
`crates/plurxd/src/transcode/rolling/publication.rs`). The wall cost is
48 s divided by production speed: 3.6 s on warm media1, 10.6 s on lab6 under
boot load, longer on a resume because the origin lead is added.

Two things decide how often a viewer meets it:

- **Index coverage.** Every ffmpeg or library change opens a new index
  keyspace. Owned by the index work already in flight (PR #1023 measures
  coverage per node; PR #1027 adds shared-read indexing). Nothing here
  duplicates it.
- **The gate itself.** `RollingStartupPolicy` has one production value,
  `Conservative`. Shortening it is Part A of the build handoff, which is
  approved for encoded sessions only and waits on the lab runs that choose
  the bootstrap. Copy sessions keep the 16 s target by ruling: a shorter
  target on a copy cuts at keyframes that are not clean.

### 4.2 Costs that only some starts pay

Each is real, bounded, and the reason two starts of similar titles differ.

| Cost | Bound | When |
|---|---|---|
| HEVC parameter-set census | 6 s | first play of an HEVC source revision |
| Dolby Vision Profile 5 pixel proof | two probes, 30 s each | first P5 play of a title after each daemon restart; the proof is held in memory only |
| Held-source ffprobe | 5 s budget | every encoded-VOD start, never cached by design |
| Fragment index hydrated from a peer | 8 s | the index exists but not on this node |
| Admission behind a background encode | 0.25 to 5 s | a background producer holds the slot |
| Hardware encoder start failure, then software | 12 s | the hardware path fails to start |
| Create "not yet" ladder on the client | sleeps of 1, 2 and 4 s | any typed not-ready answer (§5) |
| The minute after a restart | unbounded in practice | storage probe, caption-graph proofs, cold caches |

The Profile 5 proof is the one worth persisting: it is the largest bound, it
recurs after every deploy, and Paul tests right after deploys.

## 5. What still takes time on the clients

None of these is built here. Each needs a client build and a check on the
device it affects.

| # | Client | Wait | Size | Change |
|---|---|---|---|---|
| 1 | Android TV | The display-mode match runs to completion **before** the session is created (`Controller.startAt`). | up to 2 s, when the refresh rate changes | Create the session while the display switches; hold only the first render on the switch. |
| 2 | Apple | A resume on VOD or direct play calls `play()`, loads from zero, then seeks (`seekWhenReady`). | 0.2 to 2.5 s, by first-segment size | Seek the item before it is attached. |
| 3 | Apple | `automaticallyWaitsToMinimizeStalling` is on for the first start of a growing playlist. | 0 to several seconds, unmeasured | Start with `playImmediately` once the item is ready, as the reopen paths already do. |
| 4 | Apple TV | Display criteria are applied only if the item is already ready at that instant, and after `play()`. | a 1 to 3 s resync the `ttff` clock cannot see | Set criteria from the decision's facts at attach and wait for the switch. |
| 5 | All three | A typed "not yet" from create is retried after 1 s, then 2 s, then 4 s. | at least 1 s per occurrence | A shorter first step. The ladder is mirrored in all three clients and pinned by a shared fixture, so it moves in one change. |
| 6 | Web, Android | The continuous-quality start adds a candidates request before the create and a family request after it. | 2 to 3 round trips | Carry both in the decision and the create response. |
| 7 | Safari | Track N fetches the master and media playlist before `src`, and Safari fetches both again. | 2 round trips | Skip the preflight when the create response says the playlists are published. |
| 8 | Android | The player re-fetches the item detail the detail screen already holds. | 1 round trip | Pass it. |

Items 1 to 4 are worth more than the rest together, and item 1 is the only
one that is a plain ordering mistake.

## 6. What to measure after the deploy

The create handler already logs what is needed
(`create startup read budget finished`, target `plurxd::http::hls`).

```bash
# One line per session create: reads, writes, store time, handler time.
# Read the node's plurxd log however that node keeps it; for a container:
docker logs --since 10m plurx 2>&1 | grep "create startup read budget finished"
```

**How to read it:** before this branch `store_ms` was within a few percent of
`elapsed_ms` and both were 1500 to 2500 on a cluster. After every voter runs
the build, `store_ms` divided by `authority_reads` is the per-read cost. It
should be about 40 ms lower than the same node showed before; unchanged
means a voter is still on the old build. Where it settles decides §2's
follow-up: above about 10 ms, build the connection reuse. `elapsed_ms` well
above `store_ms` points at the costs in §4.2.

## 7. Non-goals

- **No change to the 48 s gate, the 16 s target or any rolling constant.**
  They are governed by the build handoff and its rulings, and the lab
  receipt that would justify a change does not exist yet.
- **No stale, cached or local read substituted for a consistent one.** The
  fix makes the consistent read cheap; it does not avoid it.
- **No speculative session create on focus or hover.** It would spend an
  encoder slot on titles nobody plays, and the create is no longer the cost.
- **No client timing constants changed blind.** The clients' retry and
  watchdog values are mirrored and device-qualified; they move with a device
  check or not at all.
