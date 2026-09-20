# Web player recovery and local seek — worker on, one decoder rescue, seek without a reopen

**Status:** ready for review · **Executes:** Q10 / W4, W7, W3 / F-web-5 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) (which file holds
what) and [PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (who may
stop the player and when a surface may be raised) — this is *what changes in
the finite web player*, in four PRs.

Read first: the review's Q10 row (§3.1), W3 and W7 rows (§3.5), and the
assessment rows Q10, W3, W4, W7, F-web-5, F-web-6, F-web-7, F-web-9 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md).
Every "required disposition" there is a guardrail in §4 with the line that
honours it. Then read the code in §2 in the order given. Work milestone by
milestone (§5); each is one draft PR into `main` under the fast lane.

The standing instruction: **if a step seems to require changing the
attempt fences (`observesCurrent`, `hlsStartupCurrent`,
`playbackAttemptTerminallyStopped`), the surface classes, the
control-protocol snapshot, or the server's publication/retention rules, stop
and flag it.** These PRs add one decision each inside the fences that exist;
they do not move a fence. Line numbers are from `88a3957a` and will drift —
re-verify each anchor by function name before editing.

**Correction to the review:** none needed for the code facts. One
narrowing the review already made stands and is load-bearing here: direct
play and immutable VOD *already* seek locally
(`transport.js:733`); the scope of W3 is rolling HLS (copy and transcode)
and progressive remux only.

---

## 1. Objective

Three independent behaviour changes to `crates/plurxd/src/web/player/`:

1. **Q10 / W4.** hls.js demuxes in a worker again, and a fatal `MEDIA_ERROR`
   that is not an incompatible-codec error gets exactly one
   `recoverMediaError()` per attach *before* the attempt is reported failed,
   spending the attach's existing retry budget. `swapAudioCodec()` is called
   only when the fault is an append to the audio SourceBuffer.
2. **W7.** A load-time throw or an unhandled rejection anywhere in the
   sixty-four-file shell reaches `/client-log`, redacted, capped and
   de-duplicated, and a viewer looking at a page that never booted is told
   whether it is slow, crashed, or timed out — not one banner for all three.
3. **W3 / F-web-5.** A seek on a rolling HLS session or a progressive remux
   whose target is already in `v.buffered`, or inside the playlist's
   currently advertised range, moves `currentTime` instead of creating a new
   server session. Everything else keeps the reopen path unchanged.

Done means: four PRs merged, `make web-check` green (including the two
tests §4.8 of the review found red — see §6), and the browser checks in §6
recorded in the PR bodies.

---

## 2. Contract today

Copied from `main` @ `88a3957a`; **re-verify at build time**.

### 2.1 hls.js construction and the fatal handler

[`player/player.js`](../../crates/plurxd/src/web/player/player.js):

| Fact | Where |
|---|---|
| Vendored hls.js is `1.6.16` (`crates/plurxd/src/web/hls.min.js`; the version string is in the bundle). It contains `recoverMediaError` and `swapAudioCodec`; the app calls neither. | `hls.min.js` |
| `attachHls(video, playlistUrl, startAt)` — 378 lines, `:499-876` | `:499` |
| Per-attach budget reset: `attachedPlayer.hlsRetryUsed=0;` with the comment "ONE `startLoad` per attach, shared between the network-class fatal and the `segment_503_not_yet` row (M5 §4.6.2)" | `:519` |
| `new Hls({ enableWorker:false, … loader:createHlsStartupLoader(StockLoader,startup) … })` — the custom loader subclasses `Hls.DefaultConfig.loader` | `:558-593` |
| Second construction site, the prepared successor: `new Hls({ enableWorker:false, … })` in `preparedHlsAttach` | [`prepared-replacement.js:273-283`](../../crates/plurxd/src/web/player/prepared-replacement.js) |
| The attempt fence every handler uses: `observesCurrent=()=>attachment.current()&&attachedPlayer.hls===hls&&playbackOwnsAttachedMedia(attachedPlayer)&&!playbackAttemptTerminallyStopped(attachedPlayer,startup.mediaAttachment)` | `:555-557` |
| `hls.on(Hls.Events.ERROR, …)` — non-fatal branch returns at `:743`; fatal path begins `:745` | `:711-864` |
| Fatal path order today: `playbackControlHlsFatal(d, started)` → `isMedia` → **`notifyPlaybackControl("failed", observation)`** (`:749`) → `clientLog hls_fatal` → `finishStallRecovery("failed")` → `fallbackAction` (copy→transcode, once per item via `triedFallback`) → refusal/`STREAM_FAILURE` branch → network `scheduleHlsNetworkRetry` (`:852`) → `stopPlayerForExhaustion()` for a media fatal with the ladder spent (`:859`) | `:746-863` |
| Comment recording the gap: "hls.js retries nothing further unless the app calls recoverMediaError" | `:762-763` |
| `scheduleHlsNetworkRetry(video,player,detail)` — one application retry per attachment; reads `episode.retry.state` or `PlaybackPolicy.hlsRetryAllowed({used:player.hlsRetryUsed})` | `:473-498` |

[`playback-policy.js`](../../crates/plurxd/src/web/playback-policy.js):

```js
const HLS_RETRY = Object.freeze({ delay_ms: 2_000, per_attach: 1 });   // :918
function hlsRetryAllowed({ used = 0 } = {}) {                          // :998
  return (Number(used) || 0) < HLS_RETRY.per_attach;
}
function fallbackAction({ method, alreadyTried=false, playbackIsReal=false,
  mediaFailure=true }) { … }                                           // :637
```

[`player/session.js:157-173`](../../crates/plurxd/src/web/player/session.js)
`playbackControlHlsFatal(data,started)` classifies `media_failure` as
`data.type===Hls.ErrorTypes.MEDIA_ERROR || /codec|decode|parsing/i.test(details)`
and returns the control observation `{decoder_state, error_code, error_detail}`.

hls.js 1.6.16 detail names present in the bundle (grep-confirmed):
`bufferAppendError`, `bufferAppendingError`, `bufferAddCodecError`,
`bufferIncompatibleCodecsError`, `fragParsingError`. The `ERROR` payload
for buffer faults carries the SourceBuffer it concerned (`sourceBufferName`
in 1.4+, older builds `parent`) — confirm the field on the vendored build
before relying on it (§5.2).

### 2.2 Where errors go today

- [`core/api.js:114-129`](../../crates/plurxd/src/web/core/api.js)
  `clientLog(ev)` — POSTs `{ua, method, title, file_id, vcodec, …ev,
  control}` to `/api/v1/client-log` with the bearer; never throws.
- Server: [`http/system.rs:820`](../../crates/plurxd/src/http/system.rs)
  `client_log` — requires auth, answers 204 always, and rate-limits with
  `CLIENT_LOG_USER_PER_MIN = 240`, `CLIENT_LOG_GLOBAL_PER_MIN = 1_000`,
  `CLIENT_LOG_USER_BUCKETS_MAX = 4_096` (`:686-688`). Unknown JSON fields
  are dropped by serde: a field this plan sends must be added to
  `struct ClientLog` (`:590`) or it is never seen.
- The only `error` listener is on the `<video>` (`transport.js:527`).
  There is no `window.onerror`, no `unhandledrejection` listener, and no
  boot sentinel: `boot()` (`core/auth.js:64`) renders "Server unreachable."
  only when `/server` fails.
- Load order: `core/theme.js` is the only `<head>` row; the seven sidecars
  (`cluster-panel.js`, `playback-policy.js`, …, `hls.min.js`) load first in
  `<body>`, then the sixty body rows starting at `core/app.js`
  ([`index.html`](../../crates/plurxd/src/web/index.html)). A reporter
  installed in `core/app.js` cannot see a sidecar's load-time throw.

### 2.3 Seeking today

[`player/transport.js`](../../crates/plurxd/src/web/player/transport.js):

```js
async function seekTo(targetSec, forceReopen=false, autoHeightOverride=null,
  viewerInitiated=true, recoveryEpisode=null){                              // :690
  …
  const seekIntent=beginPlaybackControlSeek(PLAYER,targetSec,viewerInitiated); // :724
  endWait(false);
  if(restartPendingPlaybackOpen(PLAYER,forceReopen?"stall-restart":"seek")) return;
  await new Promise(done=>setTimeout(done,100));                            // :730
  if(!PLAYER||PLAYER.controlSeek!==seekIntent||hasPendingPlaybackOpen(PLAYER)) return;
  // Direct play and film-addressed VOD seek without opening a new session.
  if(!forceReopen && (PLAYER.method==='direct_play' || PLAYER.vod)){        // :733
    try{ v.currentTime=Math.max(0,targetSec-(PLAYER.offset||0)); }catch(e){}
    markPlaybackControlSeekExecuted(PLAYER,targetSec);
    playerActivity(); return;
  }
  … direct-play stall-restart branch :741-761 …
  return requestPlaybackMediaChange(PLAYER,{method:PLAYER.method,
    copyHls:!!PLAYER.copyHls, reason:forceReopen?"stall-restart":"seek", …}); // :766
}
```

- `PLAYER.offset` is the film position of media-time zero
  (`decode-tiers.js:1035, 1164, 1270`; `prepared-replacement.js:494`).
  Film position = `offset + v.currentTime` everywhere (`transport.js:17`).
- `playbackControlBufferedRange(v,p,positionMs)`
  ([`session.js:126`](../../crates/plurxd/src/web/player/session.js))
  already maps `v.buffered` through `offset` to film time for the control
  snapshot; the seek target rides `p.controlSeek.targetMs`.
- The rolling HLS playlist's advertised range is what hls.js holds in
  `hls.levels[hls.currentLevel].details` (`fragments[0].start`,
  `edge`/last fragment end, `live`, `targetduration`); `LEVEL_LOADED` at
  `player.js:661-673` already reads `targetduration` and `live` from it.
- The progressive remux (`method==='remux'`, `!copyHls`) is
  `/api/v1/files/{id}/stream.mp4?start=…` (`session.js:38 remuxUrl`); its
  media timeline starts at `offset`. There is no published range for it —
  only `v.buffered`.
- The Apple client's window routing, which this mirrors, is
  `PlayerController.seekRoute` (`PlayerController.swift:7498-7540`): exact
  containment in any advertised range first, snap to the live edge minus a
  holdback only for targets just past the newest range, otherwise reopen.
- Tests: `tests/playback/seek-control.test.js` slices
  `playbackControlBufferedRange`/`playbackControlSnapshot` out of the shell
  by name and runs them under `node --test`;
  [`PLAYBACK.md`](../PLAYBACK.md) (the "Apple seeks route by the advertised
  window first" bullet) records that "Web and Android still reopen for every
  non-VOD seek; adopting the same window routing there is open work."

---

## 3. Change

### 3.1 Q10 part 1 — the worker

Delete `enableWorker:false` at `player.js:560` and
`prepared-replacement.js:275`, so both constructions take hls.js's default
(`true`). hls.js 1.6 builds the demuxer worker from an inline blob and, when
the worker fails to start (a `worker-src`-restricting CSP, an extension's
injected policy, a browser without blob workers), logs and falls back to
main-thread transmuxing itself. That fallback is upstream's code; the PR
does not add one. What the PR must add is the *proof* that the fallback
works with our custom loader (§2.1): the loader runs on the main thread
either way, but the fragment payload now crosses a worker boundary, and a
`Uint8Array` the loader hands over must not be reused after `postMessage`.
See §5.1 for the exercise.

### 3.2 Q10 part 2 — one decoder rescue per attach, before the report

A new pure decision in `playback-policy.js`, called from the fatal handler
*before* `notifyPlaybackControl("failed", …)` at `player.js:749`:

```js
// One hls.js media recovery per attach, spent from the same per-attach
// budget as the network startLoad, and never for a codec the browser has
// said it cannot add.
const HLS_MEDIA_RECOVERY = Object.freeze({
  per_item: 2,          // across reopens of the same title (see §4)
  settle_ms: 4_000,     // a second media fatal inside this is the same fault
});
function hlsMediaFatalAction({ type, details, sourceBufferName = null,
  retryUsed = 0, itemRecoveries = 0, recoveredAtMs = null, nowMs = 0 }) {
  if (type !== "mediaError") return "none";
  if (details === "bufferIncompatibleCodecsError"
      || details === "bufferAddCodecError") return "fallback";
  if (!hlsRetryAllowed({ used: retryUsed })) return "fallback";
  if (itemRecoveries >= HLS_MEDIA_RECOVERY.per_item) return "fallback";
  if (recoveredAtMs != null
      && nowMs - recoveredAtMs < HLS_MEDIA_RECOVERY.settle_ms) return "fallback";
  if ((details === "bufferAppendError" || details === "bufferAppendingError")
      && sourceBufferName === "audio" && itemRecoveries > 0) return "swap_audio";
  return "recover";
}
```

`"fallback"` means "continue into the handler exactly as today". The
handler change, in order, inserted between `:746` and `:749`:

1. Compute `action` with `retryUsed: attachedPlayer.hlsRetryUsed`,
   `itemRecoveries: PLAYER.mediaRecoveries|0`,
   `recoveredAtMs: PLAYER.mediaRecoveredAtMs`.
2. If `recover` or `swap_audio`: `attachedPlayer.hlsRetryUsed += 1`
   (the shared budget — one application retry per attach, whichever kind
   fires first); `PLAYER.mediaRecoveries += 1`;
   `PLAYER.mediaRecoveredAtMs = now`; `clientLog({event:"hls_media_recovery",
   detail: d.details, message: action})`; raise
   `owner_recovery_step` with "Recovering the decoder…" (a `recovering`
   step, not a new class — the same shape the network retry at `:846` uses);
   call `hls.recoverMediaError()` or `hls.swapAudioCodec()` **inside a
   `try`**, and `return`. Neither `startup.decoderFailed` nor the control
   `failed` report is touched: the attempt is not over.
3. Otherwise fall through unchanged: `startup.decoderFailed=true`,
   `notifyPlaybackControl("failed", …)`, the ladder.

Why the report moves behind the recovery (F-web-7): today `:749` tells the
control protocol the attempt failed, and a `recoverMediaError()` placed after
that would be recovering an attempt the server has already been told is
terminal. Recovery is only meaningful while the attempt is still live.

Why `per_item` exists as well as the per-attach budget (F-web-7: "a
reset-per-attach flag alone permits renewed attempts on every reopen"):
`hlsRetryUsed` is reset at `:519` on every attach, and a media fault that
recurs after each reopen would otherwise be recovered forever. `PLAYER` is
mutated in place across reopens and replaced per title (the same reason
`triedFallback` lives there — `:769-771`), so a per-`PLAYER` counter is
per item.

Why `swapAudioCodec` only for audio append faults after one recovery (Q10
row; F-web-7 "do not swap audio codec for arbitrary video failures"):
upstream's own guidance is recover first, swap only if the *same* append
fault recurs, and only when the failing buffer is audio. A video or manifest
fault never reaches it.

### 3.3 W7 — the reporter and the sentinel

A new `<head>` row, `core/errors.js`, served immediately after
`core/theme.js` — before the sidecars, so a load-time throw in
`playback-policy.js` or `hls.min.js` is seen (F-web-9: "install a minimal
reporter early enough to observe failures before core/api.js loads"). It
has no dependencies and installs:

- `window.addEventListener("error", …)` and
  `window.addEventListener("unhandledrejection", …)`. Each event becomes
  `{event:"client_error", level:"error", detail:<kind>, message, src,
  line, col, stack}` after redaction.
- **Redaction:** every URL loses its query string entirely (`?token=` is
  the case that matters, but `?stream=`/`?start=` carry session facts too);
  any `Bearer <x>` substring becomes `Bearer [redacted]`; `message` is
  capped at 512 chars and `stack` at 2,048; `title`/`file_id` are *not*
  auto-filled by this reporter (it runs before `PLAYER` exists and must not
  reach for it).
- **Dedupe and cap:** key = `(message, src, line)`; a repeat increments a
  count and is not re-sent; at most 20 sends per page load, then one per
  10 s. Reason: the server allows 240/user/min and a render loop throwing
  per frame would spend that in a second and silence the playback events
  that matter more.
- **Recursion guard:** a module-level `reporting` flag; anything thrown
  while building or sending a report is dropped, and the send is
  `fetch(...).catch(()=>{})` with `keepalive:true`. The reporter never
  calls `clientLog` — it has its own minimal POST — so an api.js failure
  cannot take the reporter with it.
- **Queue before auth:** `/client-log` is 401 without a bearer, and the
  bearer is read in `core/app.js`. Events before `TOKEN` exists are queued
  (max 20) and drained by `core/api.js` once `TOKEN` is set; the drain adds
  the `ua` label the server keys on.
- **Boot sentinel:** `core/theme.js` already runs before first paint; the
  reporter stamps `document.documentElement.dataset.boot="loading"` and
  records `performance.now()`. `router.js`'s first successful `render()`
  (or `renderLogin`/`renderSetup`/the "Server unreachable" card) sets
  `"ready"`. At 5 s and again at 20 s, if still `"loading"`, the sentinel
  classifies:

```
 script error captured? ── yes ──▶ "crashed" — names the file (banner at 5 s)
        │ no
 document.readyState !== "complete"? ── yes ──▶ "slow" — no banner at 5 s;
        │ no                                    "Still loading (Ns)…" at 20 s
        ▼
 boot() never marked ready ──▶ "bootstrap timed out" (banner at 20 s,
                               with the elapsed time and a Reload control)
```

Only "crashed" is reported to `/client-log` by the sentinel itself (the
throw was already sent; the sentinel sends one `boot_sentinel` event with
the classification). "slow" is never reported — a slow connection is not a
defect (F-web-9).

Server side: add `src: Option<String>`-adjacent fields the reporter sends
that `struct ClientLog` lacks (`line`, `col`, `stack`), each bounded in
`client_log_line`. Without this the fields are dropped (§2.2).

### 3.4 W3 — local seek for rolling HLS and progressive remux

A new pure decision in `playback-policy.js`, the web twin of Apple's
`seekRoute`:

```js
// Where a finite seek lands. `bufferedMs`/`publishedMs` are film-time ranges
// (offset already applied by the caller). `publishedMs` is null for a
// progressive remux, which advertises nothing.
function seekRoute({ method, copyHls = false, vod = false, forceReopen = false,
  changing = false, targetMs, bufferedMs = [], publishedMs = null,
  holdbackMs = 0 }) {
  if (forceReopen || changing) return { route: "reopen" };
  if (method === "direct_play" || vod) return { route: "local", atMs: targetMs };
  const rolling = copyHls || method === "transcode";
  if (!rolling && method !== "remux") return { route: "reopen" };
  for (const r of bufferedMs)
    if (targetMs >= r.from && targetMs <= r.through) return { route: "local", atMs: targetMs };
  if (rolling && publishedMs) {
    const safeThrough = publishedMs.through - holdbackMs;
    if (targetMs >= publishedMs.from && targetMs <= safeThrough)
      return { route: "local", atMs: targetMs };
  }
  return { route: "reopen" };
}
```

Applied in `seekTo()` between `:737` and `:741` for `PLAYER.hls` rolling
sessions and progressive remux:

- `bufferedMs` from `v.buffered` through `PLAYER.offset`, every range (not
  only the one around the playhead as `playbackControlBufferedRange`
  returns).
- `publishedMs` for rolling HLS only: `hls.levels[hls.currentLevel].details`
  → `{from: offset + fragments[0].start, through: offset + edge}`; `null`
  when there is no level or no fragments. A published range is **not** a
  buffered one (W3 row): landing inside it makes hls.js fetch the fragment,
  so the seek still shows a spinner, but it does not create a session.
- `holdbackMs` is **per stream**: one `targetduration` of that playlist
  (2 s on the rolling transcode, 6 s on copy segments, whatever a playlist
  actually says), not Apple's fixed 1.5 s. Reason: hls.js will not start a
  fragment the playlist has not finished advertising, and the server
  publishes only completed objects, so a target inside the last target
  duration is a target hls.js cannot yet load.
- On `local`: `v.currentTime = atMs/1000 - PLAYER.offset` inside a `try`,
  `markPlaybackControlSeekExecuted(PLAYER, targetSec)`, `clientLog
  {event:"seek_local", detail: copyHls?"copy_hls":method}`, `armStall(...)`
  with `PlaybackPolicy.HLS_STARTUP.seek_deadline_ms` exactly as the reopen
  branch does, and one **fallback timer**: if `seeked` has not fired and
  `v.buffered` has not grown to cover `atMs` within `SEEK_LOCAL_SETTLE_MS`
  (`3_000`, a new frozen constant), call the reopen branch with the same
  target and `clientLog {event:"seek_local_fallback"}`. The generation
  checks before the fallback are the ones `seekTo` already makes at `:731`
  plus `PLAYER.mediaAttachment.current()`.
- The progressive remux case (`method==='remux'`, no `hls`) uses
  `bufferedMs` only. The `stream.mp4` response is not range-addressable, so
  a browser that re-requests on seek instead of using its buffer will hit
  the fallback timer; §6 names the browser check that measures which
  browsers do.
- The control reporter already learns the new position: `beginPlaybackControlSeek`
  publishes the target before any of this runs (`:724`), and the server's
  frontier follows the reported playhead. Nothing new is sent.

`PLAYBACK.md`'s "open work" sentence is updated in the same PR to say web
routes by the advertised window; Android remains open work there.

---

## 4. Guardrails (non-goals)

Each item names the assessment disposition it honours and how.

- **No independent recovery loop, no second controller** (Q10, F-web-7).
  The rescue is one call inside the existing `hls.on(ERROR)` handler,
  fenced by `observesCurrent()`, budgeted by `hlsRetryUsed`, and bounded per
  item by `mediaRecoveries`. There is no timer that re-arms it.
- **The report moves, the ladder does not** (F-web-7). After a spent or
  refused recovery the handler runs byte-for-byte as today from `:748`.
  `fallbackAction`, `triedFallback`, the refusal branch and
  `stopPlayerForExhaustion` are untouched.
- **No `swapAudioCodec` for video or manifest faults** (Q10 row, F-web-7).
  The policy returns it only for `bufferAppend(ing)Error` on the audio
  buffer after one plain recovery.
- **Do not claim an H.264 ceiling** (Q10 row, F-web-14). The capability
  probe is out of scope here; nothing in these PRs touches `PLAY_CAPS`.
- **Worker enabling is tested in the exact vendored build with the custom
  loader** (F-web-6). §5.1's acceptance is a browser run with a
  `worker-src 'none'` CSP injected, not a unit test.
- **Reporter redacts, bounds, dedupes and cannot recurse** (W7, F-web-8,
  F-web-9). §3.3 lists each mechanism; §5.3's tests assert each one.
- **A five-second sentinel does not call a slow connection a crash**
  (F-web-9). "slow" gets no banner at 5 s and is never reported.
- **Local seek only when offset, generation, and retention allow it**
  (W3, F-web-5). The route is computed after the 100 ms coalesce and the
  `controlSeek` identity check at `:731`; `publishedMs` is what the
  playlist advertises *now*, read from hls.js, not a server field that may
  be stale; the fallback reopens on a landing that does not settle.
- **A published range is not a buffered range** (W3). The two are separate
  inputs to `seekRoute` and the log event says which one admitted the seek.
- **Holdback is per-stream policy, not 1.5 s** (F-web-5). It is the
  playlist's `targetduration`.
- **Direct play, VOD and the stall-restart branches are untouched.** They
  already route locally (`:733`) or deliberately reconnect (`:741`); the new
  decision runs after both.
- **Nothing changes recipe identity, cache digests or a settings key.** No
  new setting: the seek change carries its own reopen fallback in the same
  code path, so rollback is a revert, and Paul refuses in-code gates.
- **No metric on `/metrics`.** Client-log event names are client-supplied
  strings, so a counter labelled by them would be unbounded. The evidence
  is the `plurxd::client` log lines named in §6.
- **Do not touch the Android seek path.** W3 says Android has the same gap;
  that is a separate plan against `Controller.kt`.

---

## 5. Milestones

Each is one draft PR into `main` under the fast lane
([DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)): local `make
web-check` until it is together, `WIP:`, adversarial review, full suite
once, un-WIP, merge.

### 5.1 Worker on (Q10 part 1)

Delete the two `enableWorker:false` lines and their comment at
`player.js:189-192`. Add a browser check step to
`scripts/web-hls-startup-browser-check` (the existing CDP harness) that runs
one rolling copy-HLS start twice: once as shipped, once with a
`Content-Security-Policy: worker-src 'none'` *response* header added to the
shell document through CDP `Fetch.enable` + `Fetch.fulfillRequest` (a
request-header injection cannot set a CSP). Both runs must reach first
frame; the second must log hls.js's worker fallback line on the console.

**Acceptance:** `scripts/web-hls-startup-browser-check` prints
`worker: started` for run one and `worker: fallback-inline` for run two,
both with `first_frame_ms` < the existing deadline; and
`grep -c "enableWorker" crates/plurxd/src/web/player/*.js` prints `0`.

### 5.2 One decoder rescue per attach (Q10 part 2)

`hlsMediaFatalAction` and `HLS_MEDIA_RECOVERY` in `playback-policy.js`
(exported, frozen); the handler insertion of §3.2; `mediaRecoveries` and
`mediaRecoveredAtMs` initialised in the `PLAYER={…}` literal
(`decode-tiers.js:833`) and cleared where `triedFallback` is. Confirm the
SourceBuffer field name on the vendored build (`sourceBufferName` vs
`parent`) with a one-line grep of `hls.min.js` and read whichever exists.

Tests in `tests/playback/web-policy.test.js`: incompatible-codec → fallback;
first media fatal → recover; second inside `settle_ms` → fallback; audio
append after one recovery → swap_audio; video append after one recovery →
fallback; `retryUsed` already 1 → fallback; third reopen of the same item →
fallback. Plus a shell-slice test asserting the recovery call precedes the
`notifyPlaybackControl("failed"` call in the handler source (position in
`bodyScript`, per WEB-SHELL-LAYOUT §6).

**Acceptance:** `node tests/playback/web-policy.test.js` passes those seven
cases plus the ordering assertion; `make web-check` is green.

### 5.3 Global error reporter and boot sentinel (W7)

`core/errors.js` as a new `<head>` row: `WEB_ASSETS` row in `web.rs`, tag in
`index.html`, row in WEB-SHELL-LAYOUT.md §2 (all three, one commit — its §3
rule); the drain hook in `core/api.js`; the `ready` stamps in
`core/auth.js`/`router.js`; `ClientLog` fields `src`, `line`, `col`,
`stack` in `system.rs` with length caps in `client_log_line`.

Tests: `tests/web/asset-order.test.js` and `asset-layout.test.js` accept
the new row; a new `tests/web/error-reporter.test.js` runs `core/errors.js`
in a `vm` context with a fake `fetch` and asserts: query strings stripped,
`Bearer` redacted, 512/2,048 caps, dedupe count, 20-per-load cap then
1/10 s, no send while `reporting` is set, queue drained on `TOKEN`, the
three sentinel classifications from three fake timelines. Rust:
`client_log_accepts_error_reports_with_bounded_stack` beside the existing
`client_log_requires_auth_and_accepts_reports` (`http/mod.rs:6697`).

**Acceptance:** `node tests/web/error-reporter.test.js` passes;
`cargo test -p plurxd client_log_` passes; in a browser with
`core/cards.js` deliberately renamed in the served table, the page shows
the "crashed — core/cards.js" banner within 5 s and `journalctl -u plurxd`
on the test node shows one `plurxd::client` line with `client_error` and
`src=/assets/core/cards.js` and **no** query string.

### 5.4 `seekRoute` policy and the transport wiring (W3)

`seekRoute` and `SEEK_LOCAL_SETTLE_MS` in `playback-policy.js` (exported);
the `seekTo` branch of §3.4; `PLAYBACK.md` sentence updated.

Tests in `tests/playback/seek-control.test.js` (the brief's home for these):
rolling target inside `v.buffered` → local; inside the published range but
not buffered → local (event says `published`); inside the last
`targetduration` → reopen; behind `fragments[0].start` → reopen;
progressive remux inside buffered → local; progressive outside → reopen;
`forceReopen` → reopen; `changing` → reopen; and one harness case that
drives the fallback timer (no `seeked`, no buffer growth) and asserts the
reopen path is entered with the same target. Direct/VOD cases assert the
result equals today's branch.

**Acceptance:** `node --test tests/playback/seek-control.test.js` passes
all cases; `scripts/web-hls-startup-browser-check` gains a scrub step (±10 s
inside the buffer on a rolling copy session) that records `seek_local` in
the console log and **no** `POST /api/v1/hls/…` session create in the CDP
network log for that seek.

---

## 6. Verification and rollout

Fast lane per milestone: `make web-check` (which runs `js-check`,
`asset-order`, `asset-load`, `asset-layout`, `web-policy`, `web-control`,
`seek-control`, and the two CDP browser checks) plus `cargo test -p plurxd
client_log_` for 5.3.

Prerequisite the review found (§4.8): `node
tests/playback/web-policy.test.js` is red on `main` at `:6007` (a stale
Android call-count assertion) and `web-control.test.js` fails at `:3164`
under Node 22.22.2. The behaviour-test replacement for `:6007` is the
review's own §5.1 item 15; land it before 5.2, or 5.2's gate cannot be
green for the right reason. Do not widen a timeout to make `:3164` pass.

What only a browser proves, recorded in each PR body:

- 5.1: Chrome and Safari, worker started vs fallback, first-frame times.
- 5.4: which browsers honour an in-buffer `currentTime` assignment on the
  non-range `stream.mp4` without re-requesting (Chrome, Firefox, Safari);
  any that re-request are documented as taking the fallback timer, and if
  Safari is one of them the progressive branch is restricted to
  `!useNativeHls(video)` browsers in the same PR.

Evidence on the fleet after deploy: `journalctl -u plurxd -t plurxd | grep
plurxd::client | grep -E 'hls_media_recovery|seek_local|client_error|boot_sentinel'`
over a week, counted per event. A `seek_local_fallback` rate above 5 % of
`seek_local` is the signal that `SEEK_LOCAL_SETTLE_MS` or the holdback is
wrong for some stream class, and the log line carries the delivery method
to say which.

Rollout: each PR deploys with the normal server deploy (the web app is
`include_str!`-embedded); no client build is involved. Rollback is a
revert.

GPT prompt (device-side check that no lab run covers — an iPad in Safari
uses native HLS and skips hls.js entirely, but the progressive remux
branch of 5.4 and the reporter of 5.3 run there):

```
On the iPad, open the web app at http://10.42.0.10:8080, sign in, play
"Harbor Lights" (a title that plays as a progressive remux — check Playback
info says remux, not HLS). Let it buffer 30 s, then press ←/→ three times.
Report: did the picture jump without a spinner, and does Settings → Logs
show seek_local or seek_local_fallback lines for those presses? Then, with
the page open, put the iPad on a network with no route to the server and
reload: report which banner appears at 5 s and at 20 s and its exact text.
```

---

## 7. Open questions

1. **Which `ERROR` payload field names the SourceBuffer** in hls.js 1.6.16
   (`sourceBufferName` or `parent`)? Settled by a grep of the bundle at 5.2
   build time; the policy takes the value, not the field.
2. **Does any shipped browser re-request on an in-buffer seek of the
   progressive `stream.mp4`?** Only the 5.4 browser check answers it; the
   plan already carries the fallback timer either way.
3. **Should `boot_sentinel` "timed out" also POST when the token exists
   but `/me` hung?** Today `boot()` has no deadline on `/me`. This plan
   reports the classification and leaves the deadline as a note for the
   §2.5 listener-timeout work, which owns request deadlines.
4. **Head-row placement of `core/errors.js`** puts a second script before
   first paint. It is ~2 KB and has no DOM work; if the layout test
   (`asset-layout.test.js`) or WEB-SHELL-LAYOUT §1.2 is read as "one head
   row by rule", the alternative is first *body* row before the sidecars,
   which loses sidecar coverage. Flag in the 5.3 PR; the plan prefers head.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
