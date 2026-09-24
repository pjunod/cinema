# Web VOD seek into missing media — RCA and proposed recovery fix

**Status:** reviewed proposal; client implementation in progress ·
**Written / revised:** 2026-09-23 · **Evidence:** Safari on `media1`, deployed
build `fad591a46`, reference film U (file `6341`). Source anchors below were
checked against `bafeb0876`; the implicated files match the deployed build.

Companion to [WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md](WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md)
(the existing seek contract), [PLAYBACK.md](../PLAYBACK.md) (media paths), and
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (stop ownership).
This document records the observed failure, the client deadline mismatch,
and the bounded client repair. Fable accepted the diagnosis and rejected the
first draft's buffered-only routing change; §4 incorporates that review.

## 1. Incident — two seeks, then an exhausted prompt

The viewer sought in reference film U in Safari. The first seek ran out of
browser buffer and spent the one automatic stall recovery on a restart. A
second local VOD seek to `0:32` had no buffered media. After 8.1 seconds, the
web UI raised **Playback is still stalled. One automatic recovery was already
tried.** A manual **Try again** reopened at `0:32` and playback resumed.

The Settings → System log ring recorded these browser-local times on
2026-09-23:

| Time | Observation | Meaning |
|---|---|---|
| 19:02:11 | `seek_local`, `copy_hls:vod` | First seek stayed on the attached VOD rendition. |
| 19:02:19–20 | `supply-persistent`, 8.1 s, 0 s buffered; `stall_recovery [attempt:restart]` | The progress watch spent the automatic recovery; the restart reopened the same rendition under the 20 s HLS startup seek clock. |
| 19:02:25 | `stall_recovery [recovered]`, 34.1 s buffered | That restart presented media about five seconds after it began. |
| 19:02:40 | `seek_local`, `copy_hls:vod` | Second seek stayed on the attached rendition. |
| 19:02:48 | `supply-persistent`, 8.1 s, 0 s buffered; `surface_raised ... owner_exhausted` | The client judged the unlanded seek stalled at the first configured block interval; the request trace was not captured. |

After the failure, the playback panel showed saved position `0:32`, **0.0 s
buffered on device**, and **0.0 s ready on server** from its `32,362 ms`
anchor. It also showed a later ready island at `296,296–494,494 ms` and a
producer holding its ahead window. Its server sample was about eight minutes
old when inspected, so these values describe the retained sample, not every
instant of the failed seek. Manual **Try again** subsequently showed `191.8 s`
contiguous server-ready media, `33.1 s` buffered on device, and advancing
presentation at `0:35`. The reopen proves the title could be served at that
position; it does not identify the old attachment's exact fragment GET.

## 2. Causal chain — the seek deadline expires before the delivery contract

1. [`PlaybackPolicy.seekRoute()`](../../crates/plurxd/src/web/playback-policy.js)
   deliberately returns `{route:"local", basis:"vod"}` for a finite VOD
   target without consulting browser buffer. VOD renditions are shared across
   sessions, so a new session at the same target does not create a separate
   producer. A target outside `v.buffered` can still be an already
   materialized segment that one local GET serves quickly.
2. [`seekTo()`](../../crates/plurxd/src/web/player/transport.js) assigns
   `currentTime`, marks the seek executed, and calls `armStall()` with
   `HLS_STARTUP.seek_deadline_ms = 20_000`. It then cleans up and returns for
   `basis === 'vod'`, before installing the other local routes' fallback.
   The earlier claim that Safari's `seeked` canceled a three-second VOD
   fallback was wrong: VOD never had that fallback. The existing
   [seek test](../../tests/playback/seek-control.test.js) covers the
   non-VOD behavior.
3. [`vodClientContract()`](../../crates/plurxd/src/web/player/player.js)
   declares an **8 s server block budget** against hls.js's **10 s
   first-byte timer**. A missing fragment GET may park for eight seconds,
   return a typed 503, and then enter hls.js's 1/2/4/8/8… second retry
   ladder. The ladder was sized to outlast the server's 30 s materialization
   watchdog. An admitted blocked GET can move the
   [VOD producer](../../crates/plurxd/src/prodsched.rs) back toward a sparse
   target; an idle playhead report alone does not.
4. The actual 8 s failure path is the
   [progress watch](../../crates/plurxd/src/web/player/transport.js): while
   `controlSeek.executed` has no presentation progress, it measures
   `landingAge` from the `currentTime` assignment. At
   [`PERSISTENT_STALL_MS = 8000`](../../crates/plurxd/src/web/player/measurements.js),
   it calls `persistentWait()` directly. `beginWait()` is not the source of
   this 8.1 s event; it returns while `video.seeking` is true. Thus the
   generic stall deadline can fire as the first parked GET interval ends,
   before hls.js has had the contract's retry opportunity.
5. [`stallRecoveries`](../../crates/plurxd/src/web/player/measurements.js)
   remains one after the first successful restart. The
   [recovery policy](../../crates/plurxd/src/web/playback-policy.js) therefore
   returns `prompt` on the second `persistentWait()`, raising
   `owner_exhausted`. The first restart and the manual Try again used the
   same shared rendition with a longer startup clock; their success does
   not establish that a fresh producer was necessary.

**Root cause:** the client's 8 s progress-watch landing threshold is shorter
than the fragment delivery and retry behavior the same client requested.
The VOD branch also lacks a seek-specific fallback if target media never
arrives. The trace explains why the UI stopped at 8.1 s and why its prior
recovery made the prompt immediate. It does not prove whether the target
segment was requested, parked, retried, or delivered late in this instance.
A Safari Network capture and matching wait-pool trace would answer that
specific transport question but are not required to establish the client
deadline mismatch.

The control channel cannot currently extend this deadline based on active
materialization: [`RetryResource`](../../crates/plurxd/src/playback_control.rs)
is emitted for a producer-decision status, not for a target index with an
armed materialization clock.

## 3. Decision — preserve local VOD seeks

The first draft proposed reopening every VOD target outside `v.buffered`.
Fable rejected it, and this revision withdraws it. Browser buffer absence
does not imply server media absence. That route would turn ordinary
already-materialized far seeks into session deletion, creation, manifest
load, and HLS reattachment, and would restore session churn during repeated
scrubs. The existing local VOD route remains the correct fast path.

| Ruling | Decision | Reason |
|---|---|---|
| R1 — route or deadline | Keep `seekRoute()` local for VOD; fix seek landing timing and fallback. | The observed failure is a deadline mismatch; materialized far seeks should remain local. |
| R2 — clock | Use the existing `HLS_STARTUP.seek_deadline_ms` (20 s). | `armStall()` already receives this value for the same seek; one named clock is easier to audit. |
| R3 — server verdict | Defer a materializing-target `RetryResource` verdict to a separate change. | The client repair can close this failure without changing producer or control protocol behavior. |

## 4. Proposed client repair — one 20 s local attempt, then one reopen

**VOD local seek.** Keep `seekRoute()` unchanged. In `seekTo()`, retain the
`direct` early return, but give `basis === 'vod'` a fallback at
`HLS_STARTUP.seek_deadline_ms`. Retire that fallback only when the target is
covered by `playbackSeekBufferCovers(v, me, atMs)` or presentation has
advanced at the target. A `seeked` event alone is insufficient. Preserve the
existing player, attachment, and seek-intent `current()` fence so an old
timer cannot reopen after a newer seek or source replacement. On expiry,
log `seek_local_fallback` and call the existing forced-reopen path **once**
at the committed target, with the same selection and viewer intent.

**Other stall timers.** While an executed VOD seek still awaits target
coverage or target presentation, apply the 20 s seek landing deadline to the
pending seek rather than the generic 8 s `PERSISTENT_STALL_MS` threshold in
the progress watch. Guard `beginWait()` and any wait timer already armed for
that same intent as well: after `seeked`, `video.seeking` may be false while
target media is still absent. When target bytes first arrive, start a fresh
8 s observation for presentation rather than charging the fragment wait to
that stall. Once the target presents, normal playback stalls remain subject
to 8 s. At the 20 s boundary, the seek fallback must
own the corrective transition: a progress-watch tick, wait timer, or startup
watchdog must not race it into `persistentWait()` or an exhausted surface.
The implementation should explicitly test this ordering, including a tick
at the deadline and an already-running reopen.

**Budget and failure behavior.** The one local-seek fallback is a seek
outcome, not an automatic stall recovery. It must leave `stallRecoveries`
unchanged. A later genuine stall still uses the existing playback-wide
budget and owner surface. Keep the 30 s server watchdog, routing for other
media paths, and existing typed-error handling unchanged.

The 20 s value allows the first 8 s blocked interval, a retry, and time for
the producer to reposition. It is **shorter** than the server's 30 s
materialization watchdog, so a fallback at 20 s cannot guarantee that the
watchdog's typed failure reaches hls.js on the original attachment. The
proposal relies on one bounded reopen if the local attempt does not land;
tests must define the terminal behavior if that reopen also fails. Do not
claim that the 20 s deadline alone observes the 30 s server verdict.

## 5. Acceptance — verify the incident and the fast path

| Case | Required result | Evidence |
|---|---|---|
| Uncovered VOD target | `seekRoute({vod:true})` remains local; a 20 s fallback is armed without a session create. | Extend `tests/playback/seek-control.test.js`. |
| Target arrives | Target buffer coverage or target presentation retires the fallback; `seeked` alone does not. | Timer and event harness. |
| Target stays missing | Exactly one forced reopen at the same target on expiry; no `stallRecoveries` increment or competing `owner_exhausted`. | Full-open harness, including the 20 s boundary. |
| Seek superseded | Previous intent's timer cannot reopen; only the winning target remains attached. | Generation and attachment fence test. |
| Unlanded VOD seek versus later stall | No `supply-persistent` before the 20 s seek deadline, even if `seeked` has fired and `beginWait()` runs; after presentation, a true starvation stall still fires at 8 s. | Progress-watch and wait-event harness. |
| Incident replay | Safari on `media1`, reference film U/file `6341`, `4:57 → 0:32` resumes or performs its one bounded fallback without the early exhausted prompt. | Physical browser log and playback panel. |
| Common materialized seek | A 60 s forward seek on an already-materialized rendition stays local, presents in under one second, and creates zero sessions. | Physical Safari timing plus session-create count. |

Run the focused web checks after implementation:

```bash
node --test tests/playback/seek-control.test.js tests/playback/web-control.test.js
python3 tests/operations/test_docs_index.py
```

Before promotion, add the result and command to the
[web recovery plan's](WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md) execution log.
Update [PLAYBACK.md](../PLAYBACK.md) to state the VOD local-seek deadline
and bounded fallback while preserving its local-seek promise. Capture a
Safari fragment GET/503/retry trace with matching VOD blocked-demand events
if available; that trace refines the transport timeline but does not gate
the client deadline repair.
