# M6's gate — measure the progressive-remux seek origin before building anything

**Status:** open · **For:** a session at an Android TV ·
**Executes:** the measurement in §4.7 of
[PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)

M6 — the Android progressive-remux landing — is the one milestone of the
[playback surface contract](PLAYBACK-SURFACE-CONTRACT.md) that is **gated on a
measurement, not on a reading.** The suspected defect is that an Android
copied-video seek never lands within the 250 ms tolerance because the item is
not seeked forward from the keyframe origin the remux actually starts at, so
the target deadline eventually reports *"couldn't reach the requested
position"* over a picture that is moving perfectly well.

Nobody has taken the measurement. Until someone does, **M6 is not started**,
and a session that starts it is building on a reading.

## The gate, up front

> **Build M6 only if the achieved origin differs from the requested start by
> more than 250 ms AND the first frame landed at the origin.**

Both conditions, or no M6. §4.7's words, unchanged.

## 0. What you need

- An **Android TV** (or Google TV) with the current build installed — source
  claims versionCode 92; check what actually installed.
- A **progressive-remux** title. Confirm it in Playback debug: the transport
  reads **`Remux`** and there is **no session id**. A title with a session id
  is a copy-HLS session and is not this path.
- Access to the plurx server's log (`journalctl`/`docker logs` on the node
  serving the stream).
- `adb` to the device, for `logcat`.

## 1. The run

1. Start the title and let it present.
2. **Seek forward 10 minutes.** Note the requested target exactly — the seek
   target in milliseconds is what "requested start" means below.
3. **Wait 25 seconds.** Do not touch anything else.
4. Record what happened on screen: did the picture resume near the target, did
   a `recovering` indicator appear first (the target-deadline `recover` rung),
   or did a terminal *"couldn't reach the requested position"* appear over a
   moving picture?

That is §7 recipe (c). The third outcome is §7's explicitly **not allowed**
one and is the M6 defect itself; record it either way, because the numbers
below are what decides whether M6 gets built, not the overlay.

## 2. The two numbers

### (a) The achieved origin — `X-Plurx-Media-Origin-Ms`

The server puts the keyframe origin it actually started the remux at on the
progressive response as the header `X-Plurx-Media-Origin-Ms`
(`crates/plurxd/src/http/stream.rs`, around the `media_origin_ms` header
write). On the client it is read by
`ProgressiveMediaOrigin.acceptResponse` in
`clients/android/app/src/main/java/tv/plurx/app/player/MediaOrigin.kt` and
kept as `currentOriginMs()`.

**Be aware before you start: there is no client log line for it today.**
§4.7 says "capture from the client log", and `acceptResponse` does not log —
it has no logging import at all. So pick one of these, in this order:

1. **A temporary DEBUG-only probe (the reliable one).** Add one log statement
   in `acceptResponse` printing `uri` and `resolved`, build a Debug APK, run
   the measurement, read it with `adb logcat`, then **restore the release
   build on the device** — the same discipline the
   [player-input physical verification](PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md)
   used for its DEBUG-only probes. This measures what *this playback* got.
   Do not commit the probe.
2. **Re-request the same URL (no client change).** From any machine on the
   network, issue the same progressive request with the same `start=`
   parameter and read the response header:

   ```bash
   curl -sS -o /dev/null -D - "<the progressive media URL, same start= as the seek>" \
     | grep -i 'X-Plurx-Media-Origin-Ms'
   ```

   Caveat worth stating in the result: this is a **second** request, so the
   server re-probes. If the probe times out on one request and not the other
   (see (c)), the two answers legitimately differ. Say which method produced
   the number you are reporting.

### (b) The first-frame `realPosition()`

`Controller.realPosition()` is what Playback debug's position field reports
(`positionMs get() = realPosition()`), and on this path it is
`player.currentPosition + progressiveOriginMs` — the server's `make_zero`
makes the preceding keyframe player-local time zero, so the player's own
clock starts at 0 and the origin is the offset.

Read the position **at the first frame after the seek** — the first position
Playback debug reports once the picture is back, before playback has advanced
meaningfully. Record the number and the wall-clock moment you read it.

**"The first frame landed at the origin"** means this number is the achieved
origin, not the requested target — within a frame or two, not exactly. If the
first frame reports the *requested* target, the client already compensated
and M6 has nothing to do.

### (c) Did the server's probe time out?

The origin probe is bounded at **one second**
(`PROGRESSIVE_MEDIA_ORIGIN_PROBE_TIMEOUT`); on expiry the server falls back to
the requested start and logs:

```
progressive media-origin probe exceeded startup budget; using requested start
```

with `start_seconds` and `timeout_ms` fields. Search the serving node's log
for that sentence over the window of your run.

**This is why the measurement is not optional.** If the probe timed out, the
achieved origin *is* the requested start, the difference is zero, and no
amount of client work fixes a landing the server never offset — the problem
would be a server-side probe budget, which is a different change with a
different owner.

## 3. Deciding

Fill this in and apply the gate literally.

| | value |
|---|---|
| Requested start (ms) | |
| Achieved `X-Plurx-Media-Origin-Ms` (ms) | |
| Difference (requested − achieved) | |
| First-frame `realPosition()` (ms) | |
| Did the first frame land at the origin? | yes / no |
| Did the server log "using requested start"? | yes / no |
| Method used for the origin (probe / re-request) | |
| What appeared on screen (§7 recipe (c) outcome) | |

### GO — build M6

**Both** of these are true: the difference is **more than 250 ms**, **and**
the first frame landed at the origin.

Then build it, per §4.7:

- In `executeSeek`'s remux branch and `restartAt`'s remux path, after
  `acceptResponse` resolves the origin, call
  `player.seekTo(requested − originMs)` **once**, guarded by
  `mediaMutationEpoch` — the same shape as `successorAttachPositionMs` for
  prepared successors.
- Commit/rollback: the compensating seek belongs to the epoch that attached
  the item; a newer epoch cancels it; a failed item (`onPlayerError`) before
  the seek lands leaves `pendingSeek` to the target deadline as it does today.
- Acceptance: a `PlaybackIntentTest` case (target 90 000, origin 86 000 →
  lands after compensation); `PlaybackTargetDeadlineTest` **unchanged**;
  recipe (c) passes on the device with **no `playback_target_timeout`**.
- Branch `android/remux-seek-origin`, its own PR, and the file ownership in
  §8: `Controller.kt`'s remux paths, `PlaybackIntentTest`, and `MediaOrigin.kt`
  if needed — nothing else.
- Nothing Kotlin can be compiled without a toolchain; if you are writing it
  without one, say so in the PR the way M3 and M5 did, and hand the compile
  to [the Android build prompt](PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md).

### NO-GO — do not build M6

Any of: the difference is **250 ms or less**; the first frame landed at the
requested target rather than the origin; or the server logged *"using
requested start"* (in which case re-run once with a warm source before
concluding — a cold source is the most likely cause of a probe timeout, and
that is a server-side budget question, not M6).

Then: **write the measurement down and stop.** Record the table above in the
physical-verification record —
`docs/clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-<date>.md`, per
[the physical verification prompt](PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md)
— and update §4.7 of the implementation plan to say the measurement was taken,
what it said, and that M6 is closed rather than pending. An unbuilt milestone
with a measurement behind it is a finished piece of work; an unbuilt milestone
with nothing behind it is the state we are in now.

## 4. What to report

1. The filled-in table from §3, including which method produced the origin.
2. The GO/NO-GO verdict, stated as the gate's two conditions, each answered.
3. What appeared on screen for §7 recipe (c), and whether it was in that
   recipe's allowed set.
4. `playback_target_timeout`, if one was logged, and any
   `surface_disagreement` row seen during the run.
5. If you added a DEBUG probe: confirmation that the release build was
   restored on the device and that the probe is not committed.
