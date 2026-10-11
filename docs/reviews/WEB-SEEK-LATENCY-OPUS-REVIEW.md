# Web seek latency RCA — review

**Record:** Opus review supplied by the user. Host naming is normalized to
`media1`; review findings are otherwise retained. This is the reviewer’s
assessment, including claims qualified in the
[author dispositions](../streaming/WEB-SEEK-LATENCY-RCA-AND-FIX.md#9-opus-review--verified-dispositions).

**Status:** review delivered · **Verdict:** diagnosis APPROVED; repair plan
APPROVED WITH CHANGES (one P1 on what the hls.js update actually does) ·
**Written:** 2026-10-02 EDT · **Reviewed at:** incident commit `4fa50b79e`;
`origin/main` = `cfb52bf40` is three docs-only commits ahead, nothing under
`crates/plurxd/src/web/` differs.

This reviews `docs/streaming/WEB-SEEK-LATENCY-RCA-AND-FIX.md` and its two
evidence files, uncommitted in the working tree on the Mac. Source came from my
own blobless Forgejo clone at `4fa50b79e`. Upstream facts came from the npm
tarballs of hls.js 1.6.16 and 1.6.19 (diffed `src/`), the hls.js PR and
release pages, and WebKit commit/bug 319330@main / 321919. I re-ran the
mechanism replay. No setting, deploy, branch or push.

## 1. Verdict

The causal story is right and well-evidenced. Every code claim I checked holds.
The controlled `resumeBuffering()` intervention on the same session is the
strongest single piece of evidence in the doc, and the upstream records confirm
it (§3, B2).

The required changes are about **what the proposed repair will actually
exercise**, plus three places where the doc understates a defect:

- **P1 · B1.** hls.js 1.6.19 flips `preferManagedMediaSource` to **false**. On
  macOS Safari, the incident platform, hls.js will stop using
  ManagedMediaSource altogether. The `seeking` workaround the doc describes
  never runs there. The doc's acceptance on macOS will pass because of a
  transport change, not because of the workaround, and the one place the
  workaround matters (MMS-only Safari, i.e. iPhone) is an optional acceptance
  row.
- **P2 · B3.** The false presentation flag is not a rare race. It fires on
  essentially every forward local VOD seek made while playing. The 20 s seek
  fallback from the September 23 repair has effectively never run for forward
  seeks.
- **P2 · B4.** 1.6.19 changes the TS→MP4 remuxer's `initPTS` logic, and every
  plurx transcode is MPEG-TS through hls.js. That is the riskiest intervening
  change, and the doc doesn't name it.

## 2. What I verified

| Claim | Verdict | Anchor |
|---|---|---|
| Bundled hls.js is 1.6.16 | ✔ | `hls.min.js` version string |
| `preferManagedMediaSource` defaults true | ✔ | `ml` default config in `hls.min.js` |
| `endstreaming` → `pauseBuffering()` (only when MS `open`); only `startstreaming` → `resumeBuffering()`; no `seeking` resume | ✔ | buffer controller `_onEndStreaming` / `_onStartStreaming` |
| Main stream controller's `doTickIdle` loads nothing while `buffering` is false | ✔ | `if(this.buffering){…getNextFragment…}` |
| 1.6.19 adds `seeking`→resume, ignores `endstreaming` while seeking, removes the listener on detach | ✔ | `src/controller/buffer-controller.ts` diff |
| 1.6.19 "prefers standard MSE when available" | ✔, and it is a **default flip**, see B1 | `src/config.ts`: `preferManagedMediaSource: false` |
| WebKit 319330@main explains the cycle | ✔. Commit message: 315668@main moved the pending seek target, `monitorSourceBuffer()` read the renderer's pre-seek time, "HLS.js player is waiting for that event … so seek never completed" | WebKit bug 321919 |
| Progress watch credits a clock jump plus a stale frame baseline | ✔ | `transport.js` `playbackProgressTick`. Key excludes the execution edge; `markPlaybackControlSeekExecuted` moves `frameFloor` but not `watch.frames` |
| `localVodPresented` cancels the fallback while `seeking` | ✔ | same; `localVodSeekCleanup()` → `localVodSeekFallbackPending=false`; 20 s timer early-returns on the flag |
| `PERSISTENT_STALL_MS=8000`; `beginWait` returns while seeking; progress watch calls `persistentWait` directly; 20 s defer ceiling | ✔ | `measurements.js` |
| `seek_deadline_ms=20000` | ✔ | `playback-policy.js` `HLS_STARTUP` |
| `seekTo` awaits 100 ms, then `naturalBoundaryQualityCandidate` with a 1500 ms abort | ✔ | `transport.js`, `stall-diagnosis.js` |
| `playbackControlSnapshot`: `decoder_state` ready from `p.started`; `seeking` outranks `stalled` | ✔ | `session.js` |
| Native production runway 48 s (3 × 16 s); 32 s startup is `#[cfg(test)]` only | ✔ | `rolling/publication.rs` `ROLLING_INITIAL_RUNWAY_MS`, `RollingStartupPolicy` |
| Replay reproduces both mechanisms | ✔ re-run against `4fa50b79e`: `presented:true, fallbackPending:false, 405/405, stillSeeking:true`; MMS pause/resume both true | `docs/evidence/web-seek-mechanism-replay.cjs` |
| Evidence JSON carries no token, URL, host or path | ✔ | grep |
| Linked docs and tests exist; M4 anchor `#11-execution-record--update-in-the-implementation-commits` resolves | ✔ | |

## 3. Findings

### B1 · P1 — The 1.6.19 upgrade changes the transport on macOS; the workaround is only exercised on MMS-only Safari

The full upstream change in 1.6.19 is two independent things:

1. `config.ts`: `preferManagedMediaSource: true` → **`false`**. With
   `getMediaSource(false)`, any browser that has `window.MediaSource` gets
   standard MSE. macOS Safari has it, and so does iPadOS. Only iPhone Safari
   (MMS-only) still gets ManagedMediaSource.
2. `buffer-controller.ts`: the `seeking` resume and `endstreaming`-while-seeking
   guard, active only when `appendSource` (i.e. MMS) is in use.

plurx constructs `new Hls({…})` in `player.js`, `prepared-replacement.js` and
`live-tv-controls.js` without setting `preferManagedMediaSource`. Taking 1.6.19
as-is therefore moves every macOS Safari hls.js session from MMS to MSE. On
that platform there is no `endstreaming` and no deadlock, so the §6.1
acceptance ("the same Safari/title/ready-target seek starts fetching") will
pass on media1 **without the seeking workaround ever running**.

Consequences the doc needs to own:

- **Acceptance.** §7.3 lists "a Safari MMS-only platform where available".
  That row must be **required**, not optional, because it is the only device
  class where the workaround that §3.1 calls "the supported repair" is
  exercised. If no iPhone Safari run is possible, write the GPT prompt for it
  rather than dropping the row.
- **Probe and transport must agree.** `mseCanTake()` in `decode-tiers.js`
  probes `window.ManagedMediaSource||window.MediaSource`. After the flip, the
  probe asks MMS and hls.js instantiates MSE. Make the probe ask the same class
  hls.js will use. **Trap:** `Hls.getMediaSource()` (static) calls
  `getMediaSource()` with its *default argument* `true`, so it still returns
  MMS whatever the config says. Use the config's preference explicitly.
- **It is a product choice, so record it.** MMS vs MSE on macOS changes
  - eviction: MMS evicts under memory pressure, while MSE refuses appends at
    quota. The budget in `decode-margin.js` (`MSE_QUOTA_BYTES=144e6`) was
    measured on Chrome, and Safari hls.js sessions now enter it for the first
    time
  - remote playback: hls.js sets `disableRemotePlayback` only for MMS. Check
    what the AirPlay control shows for an hls.js session after the flip
  - power use

  Either accept the new default deliberately (my recommendation: it removes
  the failure class on macOS) and say so in §6.1, or pin
  `preferManagedMediaSource:true`. Pinning would make the seeking workaround
  the only macOS fix, and macOS acceptance would then cover it. Don't let the
  default change silently.
- **A second WebKit fix exists.** Bug 321919 is RESOLVED FIXED (319330@main
  plus a follow-up). A later Safari 27.x may restart streaming on its own.
  Then the MMS path works with or without the workaround, which is one more
  reason the MMS acceptance must record the Safari build.

### B2 · P2 (corroboration) — The onset is identifiable

§5 says the exact onset needs Safari update history. The upstream record
narrows it: the hls.js 1.6.19 release names the cherry-pick "Workaround for
**macOS and iOS 27** beta regression in ManagedMediaSource 'startstreaming'".
The WebKit commit is titled REGRESSION(315668@main), and bug 321919 was filed
2026-08-17. So the trigger is the Safari 27 upgrade on a client using MMS.
This also explains why Chrome never showed the VOD deadlock: Chrome has no MMS.

Keep the doc's caveat that the installed binary wasn't inspected, but replace
"onset unknown" with this, and add one line: a Safari 26 client should not
reproduce it.

**Related risk, medium confidence.** The WebKit message says
`monitorSourceBuffer()` itself stopped running correctly, not only on seeks.
The controlled baseline shows buffering disabled and `next_load` frozen at
1286.32 s for 15.7 s of ordinary playback, while the forward buffer drained
from ~30 s to ~14 s. Whether MMS ever fires `startstreaming` on a pure drain
on Safari 27 is unmeasured. The 1.6.19 workaround covers seeks only. On
iPhone Safari, add a "play 5 minutes, no seeks, buffer never empties" row.

### B3 · P2 — The false presentation flag fires on nearly every forward seek, not on a rare race

§3.2 presents 404/405 as "a concrete reproducer". The real sequence makes it
the common case:

1. `beginPlaybackControlSeek` publishes the intent. The watch key
   (`…:${pending.sequence}:…`) changes, so the watch resets on the next tick.
2. The old media keeps playing through the 100 ms coalescing and the catalog
   await of up to 1500 ms. On each tick `moved` is true, and `watch.frames`
   follows the counter.
3. The tick runs every **500 ms** (`playbackSamplingTick`, `decode-margin.js`
   and `prepared-switch-measurement.js`). At 24 fps, roughly 12 frames are
   presented between the last tick and execution.
4. `markPlaybackControlSeekExecuted` sets `frameFloor` to the current counter
   but leaves the key, and so `watch.frames`, alone.
5. On the first tick after `v.currentTime=…`, the clock has jumped forward,
   `frames > watch.frames`, and the position is exactly the target. That sets
   `localVodPresented=true` and cancels the fallback.

So for forward local VOD seeks made while playing with `requestVideoFrameCallback`,
the September 23 fallback (`528316c24`) has effectively been dead code since
it shipped. That is consistent with every failing VOD seek in the evidence
stalling at ≈8.07 s and none at 20 s. §5's row for that repair should say "the
protection never engaged for forward seeks", not "accepted weaker evidence".

The flag is **forward-only**. `moved` requires `clock > watch.clock`, so a
backward out-of-buffer seek in the deadlock keeps the protection and should
take the 20 s path today. That asymmetry is a useful extra §7.2 case: today a
backward seek takes 20 s and a forward one takes 8 s. It also means §3.3's
warning ("fixing frame proof alone turns 8 s into 20 s") describes the
behaviour the September 23 design intended and never had.

The §7.2 regression should be written as this sequence (publish → old frames
over ≥ one tick → execute → tick), not only as a hand-set 404/405 state.

### B4 · P2 — Name the 1.6.16 → 1.6.19 changes that touch plurx

§6.1 says to "review intervening changes". The `src/` diff has four outside
the MMS fix. One matters a lot:

- **`remux/mp4-remuxer.ts` `initPTS` rework.**
  - Audio and video `initPTS` are recomputed when `timeOffset === 0` and the
    new base is lower.
  - `initPTS` is now bound to the lowest video **DTS** instead of PTS.
  - Rollover uses `offset` instead of `offset + timescale`.

  Every plurx transcode emits `-hls_segment_type mpegts` (`plurx-core`
  `transcode/mod.rs`), so every transcode on Chrome and on Safari-hls.js is
  transmuxed by this code, and Live TV probably is too. Changed `initPTS`
  binding is how A/V offset and seek-landing regressions show up. Acceptance
  needs a transcode A/V-sync check and a transcode seek, not only copy-HLS VOD.
- **`base-playlist-controller.ts`:** reload only when
  `details.requestScheduled <= now`. This changes live playlist refresh timing
  for Live TV and for rolling sessions on Chrome.
- **`level-helper.ts`:** LL-HLS parts off-by-one and a `fragmentHint?.duration`
  guard. Low risk.
- **`base-stream-controller.ts`:** key-loading refactor. No effect, because
  plurx uses no encryption.

### B5 · P2 — The paused sole-frame hazard already exists in deployed code

§6.2 and §8 R1 frame this as a hazard the repair must avoid. It is already a
live defect:

- `queuePlaybackFrame` captures `epoch` when it queues.
- An already-armed request survives ordinary seeks by design.
- `markPlaybackControlSeekExecuted` increments `controlPresentationEpoch`.
- `step()` settles only when the epochs match.

So a paused seek's single destination frame arrives with the old epoch, the
settle is skipped, and the callback re-queues. No second frame comes while
paused. The intent stays `seeking`, with `seek_target_ms` on every control
exchange, until Play.

This is harmless today only because the progress watch is inactive while
paused. Say it is pre-existing in §3, and add a "paused seek settles without
Play" assertion that fails on `4fa50b79e`.

### B6 · P2 — Native rolling: right after a reopen, the next ordinary press is outside seekable

The native traces show the reopen chain more clearly than §4 states. At the
next press, 3–8 s after each reopen landed, the leads past the successor's
origin were:

| Trace | Seekable lead | Buffered lead |
|---|---:|---:|
| `a5:3` | 9.1 s | 50.9 s |
| `a6:4` | 10.9 s | 37.2 s |
| `a7:5` | 13.9 s | 25.2 s |

Any +10/+30 press within the first ~30 s after a reopen therefore falls outside
`seekable` by construction, and each press in a chain pays another
6.5–9.5 s reopen.

- **§6.5 option 1 can't help in that window.** A steady-state allowance only
  applies after startup.
- **Option 2 needs a concrete goal.** The successor's *initial* publication
  should already admit the next +30. That means a published lead of at least
  target + 30 s + the native exclusion (~48 s). The successor can usually
  reach that from scratch the predecessor already produced: `a7:6` reopened
  to 3313.2 s while the browser held bytes through 3350.9 s.

State this as the option 2 acceptance case ("five +30 presses on native
rolling") and measure seekable lead at successor attach.

### Minor

- **N1.** §2.2 headlines a "small seek", but `a10:9` (target 1198.106 s) was
  issued from element 1049.989 s, a **+148 s** jump (coalesced presses or a
  scrub). The small-seek cases are `a9:2` (+28.7 s) and the controlled
  `a11:11` (+28.8 s). Label `a10:9` accurately. The mechanism is the same.
- **N2.** §2.3's "Before seek" sample is 15.7 s older than the dispatch sample
  (epochs …514692 vs …530387). The element was at 1272.35 s at route time,
  not 1256.74 s. The conclusion stands; state the sample age.
- **N3.** `naturalBoundaryQualityCandidate` can also turn a local seek into a
  `requestPlaybackMediaChange`, which is a full reopen. §6.4 covers only its
  latency. Report how often the seek-boundary candidate fires, so it is
  excluded as a reopen source in the physical runs.
- **N4.** Public-mirror naming (PR #332): `media1` appears in the intro and in
  §7.3 and must be `media1`. The real titles in prose remain your open
  decision.
- **N5.** §7.2 should also pin that the MMS `seeking` listener is attached per
  attachment. `prepared-replacement.js` attaches a second `Hls` to a spare
  element, so detach on the retired element must not remove the successor's
  listener. The upstream code keys the listener on `this.media`, which is
  fine, but it is a cheap assertion given the prepared-handoff history.

## 4. Amended order

1. **Fix the loader.** Update to 1.6.19, or a reviewed 1.7.x. Make an explicit
   decision about MMS vs MSE and record it in §6.1. Align `mseCanTake` with
   it. Acceptance:
   - macOS Safari 27 VOD seeks (MSE)
   - iPhone Safari VOD seeks plus the no-seek drain run (MMS, required)
   - Chrome transcode A/V-sync and seek (remuxer change)
   - Live TV start on Chrome
2. **Fix frame proof** with the B3 sequence test, the backward-seek case, and
   the B5 paused test (fails today, passes after).
3. §6.3 stage wiring and §6.4 catalog-off-the-critical-path work, as written.
4. **Native rolling.** Successor-first per B6. Only after that, the measured
   steady-coverage work.

The doc's own boundaries all still hold:

- no new watchdog
- no raising 8 s to 20 s
- the overall incident stays open until both transports pass