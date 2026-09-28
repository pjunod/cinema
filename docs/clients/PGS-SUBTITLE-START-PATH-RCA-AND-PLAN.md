# PGS subtitles on the start path — why a 79.5 GB read blocks playback, and the three fixes

**Status:** the fixes below are deployed on the four server nodes; §5.5's
two-device physical bar passed on 2026-09-24 with the overlay gate on ·
§3 #437 · §4 #445 · §5: B1 and most of B5 in #447, B2 built and deliberately
reverted, B3 and B4 in #453 · §6 (Fix C, the subtitle ride-along on the index
pass) in #456 (the store's readers), #466 (the producer; first merged as #460,
which the forge did not keep — see `STATUS.md`) and #463 (attribution) · the
overlay gate `subtitles.pgs_overlay` remains on; a separate Google TV Streamer
stall remains open · reviewed — see
[PGS-SUBTITLE-START-PATH-RCA-REVIEW.md](PGS-SUBTITLE-START-PATH-RCA-REVIEW.md) ·
**Reviewer:** Fable, adversarial · **Written:** 2026-09-22, revised 2026-09-23 ·
**Reported by:** Paul, 2026-09-21 ~18:50 ET, Android on the TCL tablet

Companion to [PGS_OVERLAY_PLAN.md](PGS_OVERLAY_PLAN.md) (the overlay's own
milestones) and [SUBTITLE-RELIABILITY-ASSESSMENT.md](SUBTITLE-RELIABILITY-ASSESSMENT.md)
(the 2026-09-16 subtitle arc) — this is *why one title would not start, and
what to do about it*.

Read §2 before anything else: the reported symptom is the least interesting of
the three failures in the incident, and fixing it — already merged — does not
make the title play. §4, §5 and §6 are the fixes that do, and all three
have merged; what is left is hardware, in §5.5.

What review is being asked for is in §9. Everything before it is evidence and
proposal.

---

## 1. The incident, in one paragraph

Playing *Bad Boys: Ride or Die* (file 5208) on the TCL tablet, the player
showed `transcode capacity is temporarily unavailable: another replacement for
this player is still being committed` over Retry / Back, and Retry did not
clear it. Three start attempts failed across roughly seven minutes. The title
then started working on its own. The overlay Paul saw came from the second
attempt; the first and third failed for a different reason, took 50 seconds
each, and are why the movie would not play.

---

## 2. Diagnosis

### 2.1 Three attempts, two different failures, one cause

`docker logs plurxd` on **m6** (`192.168.4.14`, build
`v0.3.0-3135-g9deb58a2e`), times UTC. The container has since been recreated,
so these lines are the only copy.

```
22:54:56.892  decision: file 5208, method=Transcode, subtitle=2,
              subtitle_requires_burn_in=true, DV P8 -> SDR
22:54:58.672  admitted a held source on its media facts   file_id=5208
22:54:58.850  extracting embedded text subtitle to the sidecar cache
              file_id=5208 index=2
22:55:48.069  503 on POST /api/v1/files/5208/hls/...   latency=50679 ms
22:55:52.716  surface_cleared ... by=user action=retry
22:55:58.209  session create failed: transcode capacity is temporarily
              unavailable: another replacement ...   file=5208
22:55:58.209  503 ...   latency=3883 ms
22:56:28.137  surface_cleared ... by=user action=close
22:56:57      (reopen)
22:57:47.894  503 ...   latency=50723 ms
23:01:41.490  text subtitle sidecar cached  file_id=5208 index=2
              elapsed_ms=402639
23:01:41.749  vod session attached ...
```

| # | Client attempt | Latency | Server answer | Why |
|---|---|---|---|---|
| 1 | `a1/resume` | 50,679 ms | `local media worker exceeded the placement deadline` — **codeless 503** | `START_DEADLINE` spent waiting on the sidecar |
| 2 | `a1/fallback` (Retry) | 3,883 ms | the replacement-gate refusal | same `playback_id`, gate still held by #1's detached cleanup |
| 3 | `a1/cold-start` (Back, reopen) | 50,723 ms | the same codeless 503 | new `playback_id`, no contention, same wall |

Two facts fall out of this table and neither is obvious from the report.

**Only attempt 2 produced a `session create failed` line.** Attempts 1 and 3
never reach `session_start_error`; they are answered by the placement-deadline
arm in `hls.rs`, which logs nothing. That asymmetry in the log is the tell.

**Attempt 3 got a fresh gate, not a contended one.**
`PlaybackIntent.playbackId` is `UUID.randomUUID()`
(`clients/android/.../player/PlaybackIntent.kt:16`), so Back-then-reopen mints
a new id and the gate key `[user_id, playback_id]` is new. It did not queue
behind anything. It hit the same wall on its own.

### 2.2 The measurement

File 5208, from the store on m6:

```
id    | size            | duration_ms | width | height | video_codec
5208  | 79,519,453,096  | 6,958,976   | 3840  | 2160   | hevc
```

**79.5 GB, 116 minutes.** The artifact the start was waiting for:

```
-rw------- 1 pjunod pjunod    18866 Sep 21 23:01
  /srv/plurx/cache/subs/f5208-s2-<sha>-burn-v2.mks
```

**18,866 bytes, published after 402,639 ms.**

79.5 GB ÷ 402 s ≈ **198 MB/s**. The extraction was not slow. It was exactly as
slow as reading the film off the array. There is no tuning knob here, because
the operation *is* the read.

### 2.3 Why it must read the whole file

`ensure_burn_file` (`crates/plurxd/src/subtitles.rs:1014`) runs, in effect:

```bash
ffmpeg -hide_banner -loglevel error -copyts -start_at_zero \
  -i /dev/fd/3 \
  -map 0:s:2 -map 0:t? -c copy -avoid_negative_ts disabled \
  -f matroska -fs <cap> pipe:1
```

No `-ss`, no windowing. PGS packets are interleaved across the whole runtime,
so ffmpeg must demux the entire container to be certain it has them all. The
module's own test comment records the measured curve for *windowed* extraction:
a 200-second window costs 7 % of the file at the head, 57 % at the midpoint and
101 % near the end — cost tracks the window's *position*, not its length.

### 2.4 Why burn-in at all

Every subtitle track on file 5208 is `hdmv_pgs_subtitle`:

```json
[{"index":0,"codec":"hdmv_pgs_subtitle","title":"English [Full]"},
 {"index":1,"codec":"hdmv_pgs_subtitle","title":"English [SDH]"},
 {"index":2,"codec":"hdmv_pgs_subtitle","title":"English [Forced]",
  "default":true,"forced":true}, ...]
```

**PGS is pictures.** Blu-ray subtitles are a timed sequence of bitmaps; there
is no text in them. A client can only show one by burning it into the video or
by drawing the bitmaps itself. Index 2 is the *forced* track — the signs and
foreign-dialogue cues — which is why the output is 18 KB where the full tracks
on file 6085 are 11 MB each.

The log line is also lying: `"extracting embedded text subtitle to the sidecar
cache"` is the shared string in `ensure_vtt_at`, used by both the VTT path and
the burn path. Nothing text was involved.

### 2.5 Where the wait happens

```
POST /files/5208/hls/...
  └─ start_task                       <─ placement_deadline = START_DEADLINE (50 s)
      └─ create_cluster_session_with_priority
          └─ acquire_cluster_replacement_gate   <─ gate held from here
              └─ create_session_inner
                  └─ prepare_vod_encoding
                      └─ subtitles::ensure_burn_file
                          └─ ensure_vtt_at
                              └─ join_flight    <─ UNBOUNDED loop { notified.await }
```

`join_flight` had no timeout. The extraction task is deliberately detached — a
cancelled caller must not abandon a running ffmpeg — so the caller inherited
the extraction's whole runtime whether or not it had that long to give.

Anchors: `START_DEADLINE = 50 s` at `crates/plurxd/src/media_sessions.rs:54`;
`EXTRACTION_TIMEOUT = 600 s` at `crates/plurxd/src/subtitles.rs:35`;
`CLUSTER_REPLACEMENT_GATE_WAIT = 3 s` at `crates/plurxd/src/transcode.rs`.

### 2.6 Why attempt 2 was different

The replacement gate is a process-local `tokio::sync::Mutex` keyed on
`(user_id, playback_id)` with nothing that ages or force-releases it, and every
production holder is a detached task. Attempt 1 answered the client at
22:55:48; its `Drop for StartedSessionGuard` cleanup still held the key at
22:55:58. **That is fixed and merged** — see §3.

### 2.7 What this is not

- **Not the M6 prepared-replacement path.** `/metrics` on all four nodes:
  `plurx_playback_preparation_staged_total{outcome="staged"} 0`, every
  `cancelled_total{reason}` bucket `0`, `observations_total{seam="replacement"}
  0`. Nothing was being prepared or committed anywhere in the fleet. The word
  "committed" in the message is the gate's vocabulary, not a preparation.
- **Not a Dolby Vision problem**, though the title is DV P8. The transcode was
  forced by four independent reasons (resolution above device maximum, DV not
  proven by the client so tone-mapping to SDR, TrueHD unsupported, DV metadata
  removed) and would have happened with no subtitle selected at all.
- **Not slow hardware.** 198 MB/s off the array is the array.

---

## 3. What is already merged

**PR #437** (`5c48ed5ab`, merged 2026-09-21) — the replacement gate is now
reclaimed on evidence rather than on a clock: a holder that has declared itself
abandoned (its request answered, its guard living inside cleanup) or one past a
120 s ceiling loses the key; anything else keeps its player. Retirement and
ownership share one lock so judging a hold and claiming it are mutually
exclusive. Counter: `plurx_playback_replacement_reclaimed_total{reason}`.

**It fixes attempt 2 only.** Attempts 1 and 3 — 100 of the ~420 seconds, and
the reason the title would not play — are untouched by it.

Full write-up:
[../playback-control/REPLACEMENT-GATE-SUPERSESSION-RCA.md](../playback-control/REPLACEMENT-GATE-SUPERSESSION-RCA.md).

**PR #445** (`5c605768`, merged 2026-09-22) — Fix A, §4. A session start no
longer awaits a full-film demux, and a pending sidecar is a named
`startup_timeout` rather than a codeless 503 no client retries. Bounded for the
start path **only**.

**PR #447** (`883cf4d42`, merged 2026-09-22) — Fix B's first half, §5.4. The
PGS overlay is now offered per caller rather than per node. It changes nothing
while the gate is off; it is what makes turning the gate on safe.

---

## 4. Fix A — stop spending the start budget to learn nothing

**Status: PR #445, merged as `5c605768` (2026-09-23 00:48 UTC). Not deployed.**

### 4.1 What it does

`ExtractionLimits` gains `join_budget`. `ensure_burn_file` takes it as a
parameter. The **one** production caller that passes the short value (5 s) is
the burn on the start path (`transcode.rs`, inside `prepare_vod_encoding`).
On expiry the caller gets a string prefixed `SIDECAR_PENDING_PREFIX`, which
`session_start_error` maps to `503` + `Retry-After: 5` +
`{"code":"startup_timeout"}`.

`startup_timeout` is reused deliberately: it already means *"initialization
media is not ready yet; retry shortly"* and is already in the fixture's
`create_503_not_yet` row on all three clients. **No new code, no fixture
change, no client change, no version bump.**

Bounding the *wait* abandons nothing — the extraction is detached by design, so
it finishes, publishes, and the next caller is served warm. That is asserted,
not assumed.

### 4.2 What adversarial review corrected

Three findings, all taken. They matter to §5 and §6 too:

1. **The first version bounded a shared layer.** `join_flight` is also reached
   by offline restore (`http/offline.rs:1531`, which maps *any* error to **410
   Gone**), the subtitle VTT endpoint (`http/stream.rs:2463`, **500**) and
   offline package production (`transcode.rs:17723`). None has a deadline to
   protect, and a slow success became a terminal failure on three endpoints
   that never had one. The default is now `SIDECAR_JOIN_UNBOUNDED`, past the
   extraction's own 600 s timeout; only the start path narrows it.
2. **The placement-deadline arm was made retryable and has been reverted.**
   With the burn sidecar bounded, a cold sidecar no longer reaches that arm.
   What does is a start that spent 50 s queuing for an encoder slot, where the
   codeless 503 is correct back-pressure — retrying it adds three create posts
   per viewer at the moment the node is saturated, and the abandoned start is
   detached rather than aborted, so the node pays for all of them.
3. **The claim "the retry joins it warm" was false.** See §4.3.

### 4.3 What Fix A does NOT fix — read this before §5

The create-retry ladder is **rung-bounded, not deadline-bounded**:
`backoff_ms = [1000, 2000, 4000]`, three rungs, then `exhausted("ladder_spent")`.
`crates/plurxd/src/web/playback-policy.js:871`,
`clients/apple/Sources/PlayerController.swift:1550`,
`clients/android/.../player/PlaybackPolicy.kt` all agree.
`retry_after_seconds` is **not consulted** by `createRetryStep` at all.

For file 5208: 4 attempts × ~5 s + 1 + 2 + 4 s of backoff ≈ **27 seconds**,
then `exhausted` — a blocking surface reading "Playback is stalled." The
extraction needs **402 seconds**. The 60 s absolute deadline never binds
because the rungs run out first.

So Fix A converts

> 50-second hang → terminal overlay quoting an internal sentence, dead Retry

into

> ~5-second answer → honest `preparing` surface → `exhausted` at ~27 s

That is a real improvement and it is **not a fix**. The title still does not
play until the extraction lands on its own. §5 and §6 are the fix.

---

## 5. Fix B — finish the PGS overlay (B1 merged, gate still off)

**The consensus answer in this problem space, already most of the way built in
this repo, and switched off.** #447 closed B1 and most of B5; B2 was tried and
deliberately reverted; B3 and B4 are built in PR #453 (not yet merged), and
the gate does not flip until §5.5's check runs.

### 5.1 Why this is the fix

If the client draws the bitmaps, there is no burn, no `.mks` sidecar on the
play path, and subtitles stop forcing a transcode at all. The server side of
`pgs-v1` is complete (M1), Apple is complete (M2), Android is complete (M3).
The gate is `subtitles.pgs_overlay`, default `false`
(`crates/plurxd/src/state.rs:762`), surfaced at **Settings → Developer → Serve
PGS subtitles as an overlay** (`http/developer.rs:601`), read per request so it
needs no restart.

Critically, the overlay's own extraction is **already asynchronous**.
`prepare()` returns `PrepareState::Preparing` immediately and demuxes in a
`tokio::spawn`, with single-flight, a negative memo, a capacity limiter and a
600 s timeout; the client polls the manifest route. **Nothing blocks a session
start.** Had the overlay been on that night, the movie would have started
immediately and subtitles would have appeared six minutes later.

### 5.2 What the overlay does *not* fix

`crates/plurxd/src/pgs_overlay.rs:515`:

```rust
command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&input)
    .args(["-map", &format!("0:s:{index}"), "-c:s", "copy",
           "-f", "sup", "-fs", &maximum_demux_bytes]).arg(&sup)
```

Same shape as the burn extraction: no `-ss`, no windowing. **It reads the same
79.5 GB and takes the same ~400 s.** The overlay does not avoid the work; it
moves it off the play path. That is why §6 exists.

### 5.3 The real blocker, and it is not a matrix

**This section is written as it stood before #447, because it is the argument
for #447.** Every "there is no" below was true when it was written and is not
true now; §5.4 says what replaced each one.

The plan's M4 says *"enable default/forced PGS overlay selection for approved
clients"*. **There was no notion of an approved client anywhere in the tree.**
The gate was a single node-wide boolean; nothing in a request said whether the
caller could render `pgs-v1`. Both native clients read the `overlay` field off
the response (`clients/apple/Sources/Models.swift`,
`clients/android/.../Models.kt`) and neither declared a capability.

And **the web client has no renderer at all** — that part is still true. It
knows the overlay exists and routes around it (`decode-tiers.js`, the `preBurn`
override):

> "The server's plan is a remux (or a direct play) when the PGS application
> overlay is enabled — a delivery this player does not implement"

so it force-overrides `initialRoute` to `'transcode_hls'`.

**Consequence, before #447: turning the gate on downgraded a web direct-play
to a full transcode** — or, on an HDR title, put a degraded notice in front of
a viewer who had chosen no subtitle at all.

The trigger is not narrow. With the gate on, `deliverable_as_default`
(`crates/plurx-core/src/tracks.rs:235`) admits **every** PGS track, forced or
not, and it does so on a branch that short-circuits before the HDR term — so
the HDR guard does not protect the *selection*, only the burn that follows.
`forced_or_default` then picks any eligible track the container itself flags
as default, with no step that would prefer a text track sitting beside it. **A
Blu-ray remux with a `default`-flagged English PGS track is the common case in
a library like this one** — including the 79.5 GB remux that started this
incident.

The repo already knew: `select_tracks_with`'s own doc comment
(`plurx-core/src/tracks.rs`) says a remux like that *"had the PGS stamped as
the default; on an HDR base delivery that track can only be shown by an SDR
burn the HDR guard then refuses, so the viewer got the refusal notice on the
web client and silence on the native ones — for a choice nobody made."*

The server stamped that pick onto the wire `default` flag (`stream.rs`, the
`s.default` loop after `sub_tracks`). The web applies the server's default
400 ms after open (`decode-tiers.js`, *"Server-chosen default subtitle"*),
`subNeedsBurn` is true for any bitmap track (`menus.js` — `s.text===false`),
and `setSub` (`audio-sync.js`) turns that into a burn, or into the `keep_hdr`
notice. `docs/PLAYBACK.md` documents the path as settled fact under
`server.track-selection`.

> **This paragraph was wrong twice, in opposite directions, and the second time
> was mine.** The first revision called it a code-provable regression, which
> was right. A later revision — mine, after the first adversarial review —
> declared it false on the grounds that *"the web never reads the server's
> policy pick"*, citing `web/detail/preplay-selection.js:135`. That citation is
> about a different mechanism: the echo of an **explicit viewer pick**, which
> the web is right to ignore. It says nothing about the `default` flag, which
> the web does apply. The over-generalisation is corrected here and in #447's
> own body.

The web's local `initialRoute` override does **not** save it: that branch is
reached only when `preBurn` is non-null, which happens only for an explicit
viewer pick (`web/detail/preplay-selection.js`, `prePlayApplication`). Nothing
stood between the auto-pick and the burn.

Underneath the bytes there is a defect about honesty, and it is the one #447
fixes: the server issued a plan the caller cannot execute and then described
the delivery in terms untrue for that caller. **#447 closed both.** The overlay
term is now narrowed per caller, so a client with no renderer **that sends a
capabilities document** is not offered the PGS default at all, and there is no
longer a regression waiting behind the gate. (The qualifier is load-bearing and
§5.5 says why.) The rest of §5.4 is what that took.

### 5.4 Shape — B1 built, B2 deliberately not, B5 partial (#447)

#447 (`883cf4d42`) settled three of the five, and not the way this section
proposed:

| | proposed | shipped |
|---|---|---|
| **B1** capability negotiation | a boolean the server ANDs in | built, as a **list** of protocol names, with two limits the review added |
| **B2** thread the switch into item detail | do it | **implemented, reviewed, reverted** — the proposal rested on a claim that is false for the web |
| **B3** seek test on each native client | — | **built** (PR #453, not yet merged): one refresh rule on both clients, held to `tests/playback/pgs-overlay-cases.json`, and driven through each controller (`PGSOverlayControllerTest` on Android; the `PlayerController` overlay tests on Apple). Fixed Android's stale frame after a seek into the refresh margin, and a tick or player event that cancelled the load covering it on both |
| **B4** overlay-failure guardrail | — | **built** (PR #453): a failed preparation is a typed terminal `pgs_overlay_prepare_failed`, capacity a retryable 503 `pgs_overlay_capacity`; both clients read the code, stop polling and raise the existing `degraded_notice` once, and a failed window is retried silently after 5 s and 30 s, then left for a seek |
| **B5** correct the documentation | three docs | `PLAYBACK.md` done; the acceptance doc's retired env gate done in PR #453; the missing Android equivalent still open |

Each proposal is kept verbatim below, with what actually landed beneath it.

**B1 — client capability negotiation.** Add an overlay capability to the caps
the client already sends, thread it into `decide`, and make
`deliverable_as_default` take *this client can render pgs-v1* rather than
*this node has the switch on*. Touches `http/stream.rs`,
`plurx-core/src/tracks.rs:235`, `http/dto.rs:497`, and the three caps senders
(`clients/apple/Sources/AppModel.swift`,
`clients/android/.../data/PlurxApi.kt`, `web/player/session.js`).

> **Built**, as `DeviceCaps.subtitle_overlays: Vec<String>` — a list of
> protocol names shaped like `transports`, so a later `pgs-v2` is a new entry
> rather than a second flag and it lines up with `SubTrackDto.overlay`, which
> already carries the string. Apple and Android claim `["pgs-v1"]`; the web
> claims nothing and says why in a comment. `overlay_for_caller` in
> `http/stream.rs` is the single place the switch and the claim meet.
>
> Two limits the review added, both deliberate. **No caps document at all is
> not a refusal**: the legacy `GET /decision` query has no slot for a claim and
> is a *mixed-fleet* path rather than an old-client one — both native clients
> fall back to it on any 400/404/405 — so silence keeps the answer this server
> gave before the claim existed, the switch alone. Reading it as "cannot" would
> have sent a capable Apple TV off to re-encode a whole film. And **the
> `overlay` field on the track keeps the server's own answer**, because it
> describes what this process can deliver rather than what this caller can
> paint, and it is the only surface that answers that question; what a client
> must not be told is that the delivery needs no burn, and that is
> `subtitle_requires_burn_in` and `subtitle_route`, both narrowed.
>
> `tests/validation/test_caps_wire_conformance.py` pins the field name across
> all four ports. `DeviceCaps` has no `deny_unknown_fields` and every field is
> `#[serde(default)]`, so a misspelled claim is not refused — it is silently
> dropped and the server reads "not claimed".
>
> Note which way that fails, because an earlier wording had it backwards: a
> dropped claim costs a **needless burn**, not an unplayable delivery. The
> client is told the track needs burning in, the server burns it, the viewer
> sees subtitles — and an HDR source has been re-encoded to SDR to draw
> pictures the device could have drawn itself. The delivery is always playable;
> it is just expensive and quietly worse. The unplayable case is the opposite
> drop — losing the whole caps document, which takes the `None` branch and
> reads as capable — and that is the branch the paragraph above is about.

**B2 — `dto.rs:497` hardcodes overlay-off.** `FileDto::from_media_file` passes
`false` because it "has no access to the setting", so pre-play and `/decision`
disagree and Apple papers over it with hedged copy
(`clients/apple/Sources/TrackFacts.swift:223`). Thread the setting in.

> *(The quoted rationale no longer exists in the tree: #447 deleted the
> "has no access to the setting" comment at `dto.rs:487`, and this document is
> now the only place it survives.)*
>
> **Not built, on purpose — this proposal was wrong.** It was implemented in
> `efadf3943`, reviewed, and reverted in `471754f7f`, both inside #447.
> Threading the switch in rests on the claim that each client narrows it
> locally, and that claim is false for the web *on the default*: nothing under
> `web/detail/` narrows the **default** by a renderer, `track-facts.js` stamps
> the chip "plays by default" straight from `selected_index`, and the one
> renderer check there (`prePlayBurnNeeded`) is reached solely for an explicit
> viewer pick. With the switch on, item detail would promise a browser a PGS track it
> will never draw, on the exact chip where a viewer takes the server at its
> word; old native builds would read it the same way. Now that the default is
> per-client, an honest answer needs a capabilities document and this surface
> has none. Too narrow for a capable client is a missing convenience;
> confidently wrong is a lie. Apple's hedged copy stays until item detail
> learns to ask.

**B3 — a seek test on each native client.** Both have seek-reconciliation code
(`AndroidPGSOverlay.kt:113` `reconcile`,
`PlayerController.swift:4881` `refreshPGSOverlayWindow`) and **no test drives a
seek on either.** Cue timing and render position are well covered; seek is a
genuine hole in M2/M3 acceptance, not only M4.

**B4 — an overlay-failure guardrail.** The existing guardrail
(`hdr_subtitle_burn_refused`) refuses a *burn* on HDR. There is no symmetric
contract for "the overlay was selected, the client could not fetch or render
it" — today the viewer silently gets no subtitles on a DV/HDR title.

**B5 — correct the documentation.** `docs/PLAYBACK.md:589` currently states
that enabling the gate "does not select an overlay automatically", which
`http/stream.rs:2205` contradicts. `APPLE-PGS-OVERLAY-ACCEPTANCE.md:52` still
names the retired `PLURX_PGS_OVERLAY` env gate, and there is no Android
equivalent.

> **Built** for `PLAYBACK.md`, with the correction dated in the text so the
> next reader can see the paragraph used to say the opposite. The acceptance
> doc's retired env gate is corrected by PR #453; the missing Android
> equivalent is still open.

### 5.5 The proof bar — deliberately not a matrix

#### 2026-09-24 physical check: failed, acceptance open

The Google TV Streamer ran Android client `0.3.0` (versionCode `119`) against
server build `v0.3.0-3649-g99d4abf8c`. *Casino* (file `5226`) is HDR10 with
English SDH PGS at subtitle index `0`. Playback time advanced and a forward
seek reached 15:02, but no cue appeared. On nynuc the overlay preparation
ended after 584,654 ms with `PGS safety limit exceeded: normalized RGBA
output exceeds 268435456 bytes`. Cue timing, placement, and backward-seek
acceptance were therefore not reached. A protected-video screenshot cannot
establish visible picture or HDR output, so those observations remain open.

On the TCL tablet, *Casino* started immediately and playback advanced, but
the first m6 extraction timed out after 600,001 ms. The tablet's Playback info
reported `HDR → SDR`, 3840×2160 source to 1920×1080 playback, `Transcode ·
VA-API · 1080p`, and `PGS overlay · unavailable`. It cannot establish the
required HDR/direct-or-remux result. After the m6 index pass stored all seven
PGS tracks for file `5226` (472 MB, one file), a second preparation served a
stored track and failed after 610 ms on the same aggregate RGBA limit. The
stored subtitle path is working; the normalizer limit is the remaining failure
for this title.

On iPhone 17 Pro Max, *Bad Boys: Ride or Die* (file `5208`, Dolby Vision
Profile 8, forced PGS index `2`) played at 3840×2160 with a remux decision,
Dolby Vision rendering, and no buffering interruptions. Nynuc's cold PGS
preparation timed out after 600,002 ms, so no DV PGS cue was validated. A
separate SDR control, *The Good Son* (file `6641`), prepared its stored PGS
track in 2,330 ms and visibly drew a centered cue on the iPhone. That proves
the renderer can draw a real cue; its forward/backward seek behavior remains
unproven because no post-seek cue was captured.

The browser check passed its narrower contract: *Casino* opened with
subtitles Off; manually selecting PGS displayed “That subtitle requires an
SDR burn-in. HDR playback was kept unchanged.” At that point the two-device
HDR proof bar below had not been met. The overlay gate remained on for
qualification. The streaming normalizer change required the HDR hardware
retest recorded below.
After those failed trials, current-main Apple build `181` was installed and
confirmed on the six reachable physical Apple devices, and current-main
Android versionCode `121` was installed and confirmed on the TCL tablet. The
newer client versions have not yet passed the HDR PGS retest.

#### 2026-09-24 recheck: the two-device HDR bar passed

| Device and selected track | Start and cues | Forward seek | Backward seek | Grade and method | Playback cost |
|---|---|---|---|---|---|
| iPhone 18 Pro, Apple build 182, *Bad Boys: Ride or Die* (5208), Dolby Vision P8, English Full PGS index 0 | Video started while PGS prepared for 413,519 ms; dialogue cues appeared centered and timed to speech | New cue appeared at the destination | New cue appeared at the earlier destination | 3840×2160 Dolby Vision rendering; remux | No buffering interruptions or visible stutter in the observed run |
| Pixel 11 Pro XL, Android versionCode 124, *Casino* (5226), HDR10, English SDH PGS index 0 | Video started before PGS preparation finished (489,545 ms); cues appeared centered and timed to speech | 10:11 to 57:36: new dialogue cue, continuous picture | Return to 10:11: new dialogue cue, continuous picture | 3840×2160 HDR10 rendering; remux | No observed stall after the seek fix; the earlier eight-minute run recorded zero buffering interruptions |

The first Pixel 11 run exposed a client seek timeout even with subtitles Off:
the remux rendered a frame 282 ms before the requested position, outside the
client's 250 ms first-frame window. Android versionCode 124 waits for later
rendered video to cross that target before settling a progressive-remux seek.
The versionCode 124 package passed both PGS seek directions before the
subsequent foreground guard review fix. The final package with that guard was
installed on six physical Android devices, each reporting versionCode 124.
On the unlocked Pixel 11 Pro XL, the final package played *Casino* with English
SDH PGS overlay through a backward seek from about 1:06 to about 10 minutes
and a forward seek to 58:16. Each destination showed continuous video and a
new timed PGS cue. Its playback panel reported 3840×2160, HDR10 rendering,
remux, Playing, and zero buffering interruptions after both seeks. No seek
timeout appeared in the observed run. Six reachable physical Apple devices
reported build 182. These installations
used local development builds; they are not evidence of store-signed release
packages.

The web check remained safe with the gate on: *Casino* defaulted to subtitles
Off, and a manual PGS selection showed the HDR-preserving SDR burn-in refusal.
The gate remains on. A separate Google TV Streamer trial reached an initial
cue but later stalled on a `503 Service Unavailable` stream response; it is
not the Android device used for the two-device bar and needs its own playback
follow-up. This result does not claim that every fleet device passed playback.

Stored-track readiness was met on all four nodes in the 2026-09-24 snapshot:

| Node | Startup self-test | Cache filesystem | Free / required margin | Stored tracks | Ride |
|---|---|---|---|---|---|
| m6 | 78 ms, met | ext4 | 189.1 / 8.8 GiB | 472 MB, one file | None |
| nynuc | 74 ms, met | ext4 | 72.3 / 10.3 GiB | 13 MB, one file | None |
| nuc4 | 101 ms, met | ext4 | 65.1 / 9.3 GiB | 4.0 GB, 39 files | *Life* running |
| nuc3 | 77 ms, met | ext4 | 28.7 / 6.4 GiB | Zero directories | None |

The plan's M4/M5 acceptance asks for an "executed compatibility matrix" and a
"complete physical validation matrix". Those are ceremony for this feature. A
bitmap overlay can be wrong in exactly three ways a screen reveals: a cue lands
at the wrong time, it lands in the wrong place, or decoding 4K-canvas bitmaps
costs too much on the weakest device. So the bar proposed here is:

> **Two devices — one Android, one Apple — each playing one real PGS title,
> one of them DV and one HDR10, with a seek in each direction.** Confirm cues
> appear at the right time and in the right place, and that playback does not
> degrade. Plus B3's automated seek test on both native clients.

Fifteen minutes on hardware, not a program. The review tightened this from
"three surfaces" to two devices with a specified grade each, for a reason worth
keeping: the web has no renderer, so playing a PGS title there proves only that
the local override still works — and the HDR path is where an overlay can fail
in a way a 4K SDR title will never show, so leaving the grade unspecified is
how a check passes without testing anything.

The first check failed; the 2026-09-24 recheck above closes §5.5's two-device
proof bar. The Google TV Streamer stall remains a separate playback finding.
`overlay_for_caller` removes the reason
the switch was unsafe to flip — no client **that sends a capabilities
document** is now offered a track it cannot draw, and none is told a delivery
needs no burn when for it one does. That qualifier is load-bearing: a caller
arriving with no document at all is deliberately read as capable, so a
renderer-less client on the legacy `GET /decision` path would still be told the
delivery needs no burn. In practice that set is empty — the web never falls
back (`askDecision` skips the fallback whenever `progressive_hevc_sample_entries`
is present, and `decode-tiers.js` always sends it) and the only clients that do
fall back are the two that claim the protocol — but "empty in practice" is not
"impossible", and it is the seam to watch if a fourth client ever appears.

None of that makes the renderers proven on real hardware, and only one of those
two things is a code question.

### 5.6 The stated reason the gate is off

`docs/PLAYBACK.md:584` — *"off by default while physical-device HDR/Dolby
Vision acceptance remains incomplete"*. And
`docs/clients/PGS-OVERLAY-M0-FEASIBILITY.md:417` holds two parser items still
marked Partial, with *"Record that result on the parent issue before the
feature gate is considered for enablement."* §9 asks whether those close
empirically.

Note also `crates/plurxd/src/state.rs:754`, which inverts the usual reading and
is worth weighing:

> "Off is not the safe default here: it is the expensive one. A selected PGS
> track with the overlay off becomes a burn-in, which re-encodes the video and
> drops it to SDR."

---

## 6. Fix C — ride the extraction on the index pass (approved design, v3.1)

> **Revision history, kept because each round overturned something load-bearing.**
> **v1** added plain file outputs to the index ffmpeg and rescued the index from
> a subtitle failure by reclassifying the exit code. Experiments on ffmpeg 8.0.1
> — the major version production ships — showed one failed output terminates
> the whole process, the index still gets a well-formed trailer, and the row
> check cannot tell a complete index from one cut in its last two seconds.
> **v2** isolated the outputs inside a `tee`, which holds, but trusted a
> derivation experiment that was itself flawed (the fixtures had already moved
> each track's first cue to zero), judged track completeness by parsing (a
> truncated PGS stream is a valid shorter stream), and took track ordinals from
> scan-time facts (a stale ordinal kills the index or files one track's cues
> under another's number). **v3** fixed those; the third review approved it
> with changes, folded in as **v3.1** below — exact `framecrc` arithmetic, a
> complete verdict table, a latch that can actually retry, publish ordering, the
> MPEG-TS exclusion v3 had dropped, and an off switch that also stops the
> consumers. Every experiment cited was run on nuc3 against synthetic sources
> muxed with `-copyts`, so cue times survive.

### 6.1 The finding that motivates it

There are currently **two independent full reads of the same PGS packets,
producing two formats**:

| Artifact | Producer | Path |
|---|---|---|
| `f<id>-s<n>-<sha>-burn-v2.mks` | `subtitles::ensure_burn_file` | `<cache>/subs/` |
| `track.sup` → PNG generation | `pgs_overlay::prepare` | `<cache>/subs/pgs/<generation>/` |

Same source, same packets, 79.5 GB each. And a third read already happens for
reasons of its own: the **fragment-index** job demuxes the whole file to build
the segment plan. It is one `ffmpeg` child, started at byte zero, no `-ss`,
writing fragmented MP4 to `pipe:1`; the subtitle packets stream past its
demuxer and are discarded by `-sn`.

So Fix C is not a new job. It is extra outputs on a pass that is already paid
for — measured at next to nothing in CPU or memory, with no back-pressure
between outputs, since separate outputs have separate muxers.

### 6.2 What the code and the experiments established

| # | fact | consequence |
|---|---|---|
| 1 | `pipeline_digest_for_transform` hashes `copy_index_pipe_args` itself, and that digest is in every cluster fragment-index `cache_key` | extra outputs go **downstream** of the hashed function; a test pins the digest unchanged |
| 2 | one failing output terminates the whole ffmpeg process (`exit=183`, pipe cut at 300.7 of 600 s); `-xerror` changes nothing; an unopenable output kills it with zero bytes | isolate subtitle outputs **inside ffmpeg**, in a `tee` — §6.3 |
| 3 | on abort ffmpeg still writes the index's `mfro` trailer, and `VideoCompletionExpectation::covers` accepts anything within 2 s of expected | **no exit-code reclassification** — a non-zero exit stays `IndexProcessFailed` |
| 4 | the index argv has no `-y` and stdin is null; a leftover stage file makes ffmpeg exit **0 with zero bytes**, which the worker records as a final `Unsupported` | a fresh private stage per attempt, `-nostdin -y`, a drop guard |
| 5 | `-fs` exits **0** leaving a truncated artifact that looks valid, and is ignored on a `tee` output | bounds are checked **after** the pass, by size |
| 6 | a PGS stream truncated at a packet boundary is a valid shorter stream — it ends cleanly on an END segment and passes `plurx_pgs`'s structural rule | completeness needs an independent count — §6.3 |
| 7 | with `-map 0:s:N` a stale ordinal either kills the index (`exit=234`) or, with `?`, shifts later streams so one track's cues are published under another's number | ordinals come from the **held-fd probe of the file being read**, hard maps only |
| 8 | the `tee` spec unescapes slave paths: `C:\…` becomes a different path, every slave fails, exit 0, nothing stored | stage paths are escaped for the tee syntax |
| 9 | the ride-along `.sup` is **byte-identical** to `pgs_overlay::prepare_stage`'s own extraction | the overlay can use it on any container |
| 10 | a `.mks` derived from that `.sup` with `-copyts` **and no** `-start_at_zero` has cue times identical to `ensure_burn_file` from the source, on both a zero and a 7.5 s source start; **with** `-start_at_zero` — the burn path's argv — every cue moves earlier by the time of the first cue | derive with `-copyts` only; the first test uses a fixture whose first cue is not at the source start |
| 11 | that derivation runs in 0.04 s for an 18 KB `.sup` and 0.25 s for a 10 MB one | fits inside the start path's 5 s join budget with room to spare |
| 12 | up to three index passes per file (one per DV identity); cluster jobs for one node run sequentially per slot; a node often settles a job by hydrating a peer's blob and never runs ffmpeg | a **persistent** per-file latch, riding on the first pass this node runs |
| 13 | `source_sha256` exists only in the cluster worker; the non-cluster path and both consumers never have it | node-local key |
| 14 | `<cache>/subs` is LRU-pruned at 256 entries; `sweep_local_orphans` considers only `*.idx`/`*.tmp` | its own home and its own sweep |

### 6.3 Shape

**Ordinals.** Take the PGS tracks from the held-fd probe
`probe_completion_expectation` already runs (`held_source_index_probe_json` on
the same open file): streams with `codec_name == hdmv_pgs_subtitle`, as their
subtitle ordinals. Hard `-map 0:s:N` for exactly those; **never** `?` on a
subtitle map. Scan-time `subtitle_streams` are not used to build the argv.

**One extra output, a `tee`**, appended after `pipe:1` in
`fragindex::index_pass` — never in `copy_index_pipe_args*` — with, per track,
a `sup` slave and a `framecrc` companion, then a mandatory `null` sentinel:

```
-map 0:s:N0 -map 0:s:N1 … -c:s copy -f tee
  "[select=0:f=sup:onfail=ignore]<stage>/s<N0>.sup|
   [select=0:f=framecrc:onfail=ignore]<stage>/s<N0>.crc|
   [select=1:f=sup:onfail=ignore]<stage>/s<N1>.sup|
   [select=1:f=framecrc:onfail=ignore]<stage>/s<N1>.crc|
   [f=null]-"
```

`select=K` follows the output's `-map` order (tested). `onfail=ignore` is per
slave, so a track ffmpeg cannot copy, or a stage file it cannot open, drops
that slave and nothing else. The sentinel is **mandatory**: a `tee` whose every
slave fails reports "All tee outputs failed" and takes the process down. With
it, the index pipe is byte-identical to baseline whether a slave fails or not
(same sha256, tested at `-loglevel error` and `warning`). The worst case of
the ride-along is therefore "no subtitle artifacts", never "no index".

**The index's rules do not change.** Exit code, row check, retry charging,
`Unsupported` — exactly as today.

**Per-track verdict**, taken only after the child is reaped (a slave read early
can be caught before its final flush):

Slave numbering: track *i*'s `sup` slave is `#2i`, its `framecrc` companion is
`#2i+1`, and the sentinel is last.

| verdict | condition |
|---|---|
| **kept** | **all of:** walking the `.sup` as segments of `13 + len` bytes consumes the file exactly (`Σ(13 + lenᵢ) == size`, nothing left over); `Σ(3 + lenᵢ)` equals the sum of the `framecrc` size column (the **5th** comma-separated field of each non-`#` line, read by position — optional `F=`/`S=` fields follow it); the `.sup` parses end to end with `plurx_pgs`; ≤ `MAX_TRACK_BYTES` (256 MiB); stderr reported no failure for slave `#2i` or `#2i+1`; the index was `Built` |
| **empty** | the `.crc` exists with its `#` header lines and lists **zero** packets, and stderr reported no failure for either slave — a real track with no cues, e.g. a forced track on some discs |
| **transient** | stderr reported a failure for either slave that is an OS error (ENOSPC, EIO, EACCES, ENOENT), or the `.crc` is missing or never opened |
| **malformed** | anything else — the totals disagree, the walk leaves bytes over, the parse fails, or stderr reported a non-OS failure for either slave. **Any** reported slave failure means the track is not kept, even when the totals happen to agree (a failure at close, after all data was written) |

Why 10 and not 13 bytes of header per segment: a `.sup` segment on disk is
`PG` (2) + PTS (4) + DTS (4) + type (1) + length (2) + payload, but type and
length are already inside the packet ffmpeg carries, so the `sup` muxer adds
only the first 10. The measured intact track (`3756` bytes, 24 segments, several
per packet) equals its `framecrc` total of `3516` only at 10. The segment count
must come from walking the `.sup`, never from the packet count. **Required
test:** a multi-segment-per-packet fixture where the sums agree on the intact
track, disagree on a corrupted-first-segment track, and where "minus 13" fails
on the intact one.

The `framecrc` count is the primary check because `framecrc` does not parse
PGS — a bad segment cannot make it fail — so its total is every byte that
reached the tee for that stream:

```
s0: sup payload bytes=3516  framecrc packet bytes=3516  packets=6   => kept
s1: sup payload bytes=16408 framecrc packet bytes=33988 packets=58  => truncated
```

If the `.crc` file itself fails (disk full), both are cut at about the same
moment and would have to agree to the byte, and a mid-segment cut is caught by
the parser's trailing-bytes check. **The stderr scan is the second check, not
the first**: its lines (`Slave muxer #k failed`, `error opening`) are free text
from `libavformat/tee.c`, the production build is patched, and a wording change
would fail open. They are emitted at `-loglevel error`, so the index's level
does not change. `build_with_args` makes no decisions from stderr today; the
full scan and the existing bounded diagnostic tail share one reader.

**Stage and publish.** A fresh private stage directory per attempt, on the
same filesystem as the store so publishing is an atomic rename, removed by a
drop guard on every exit route including `foreground_preempted` and lease loss.
`-nostdin -y`. Stage paths escaped for the tee syntax (`\`, `|`, `[`, `]`, `'`),
with a test using a stage path that contains a backslash. The stage must
be on **local** disk — a blocked file output stalls the shared demuxer — so
when `<cache>` is not a local filesystem the ride-along does not run: a
`statfs` type check that rejects NFS, CIFS/SMB and FUSE. That same rule is why
two nodes never write one store.

**Publish order**, after the pass's existing freshness checks
(`source_still_matches`, and the cluster worker's `still_current`) so a pass
that raced a rescan cannot recreate a directory the sweep just removed:

1. rename the new content-named `.sup` files into place;
2. write the manifest to a temp file and rename it over the old one (atomic);
3. only then delete `.sup` files the previous manifest named and this one does
   not;
4. write the first `.access`, so a new directory is not first in line under the
   size cap.

**Readers** read the manifest, then open the `.sup` it names. A missing file —
swept or republished in between — is a miss that falls through, never an error.
On open, a reader checks the `.sup`'s sha256 against the manifest. On Windows a
delete fails while a reader holds the file without `FILE_SHARE_DELETE`; the
sweep tolerates that and retries on its next pass. An on-demand consumer never
writes into the store, so it cannot race the producer.

**Home, key and latch.**

| | |
|---|---|
| directory | `<cache>/runtime/subtitle-source-v1/f<file_id>/` |
| manifest | `manifest.json`: source `size`, `mtime`, `object_version`, the probed PGS ordinals, and per track the verdict and, when kept, the `.sup` file name and its sha256 |
| artifacts | `s<ordinal>-<sha256-prefix>.sup` — content-named, so no field needs parsing out of a file name |
| **latch** | the manifest keeps `attempts` per track. It is *current* when its `size`/`mtime` match a live `fstat` **and** every probed track is `kept`, `empty` or `malformed`, or `transient` with `attempts ≥ 3`. The check runs inside `fragindex::build_from_attested_file*`, **after** `probe_completion_expectation` — the one place both the cluster and non-cluster paths pass through, and the first point at which the ordinals exist — and hands `index_pass` a ride-along plan (ordinals + stage directory) or none. It rides on whichever identity's pass this node runs first. A transient track waits for the next pass this node runs for that file, which may be the next re-index |
| validity at use | re-checked against a **live** `fstat`. The overlay keeps its size+mtime rule. The burn path additionally requires `(dev, ino, size, mtime)` to match the open file's — enough to reject a file replaced in place with a new inode, without the ctime that a hardlink or `chmod` from an importer would change, which would otherwise make the burn path miss on that file forever while the latch never re-rides |
| access | an `.access` marker per `f<id>/`, written on each hit with failures ignored, copying `pgs_overlay::record_access` — atime is meaningless on relatime/noatime mounts |
| sweep | its own rule and cursor, on both the cluster and non-cluster paths: delete a directory whose file row is gone or whose `size`/`mtime` no longer match; **stop, not delete**, when reading the row fails, as `sweep_local_orphans` already does |
| bound | a total-size cap, evicting whole directories by `.access`, oldest first — a safety rail set well above any real library's PGS footprint |

**Consumers.** Each looks for a current manifest first and falls through to
today's extraction when there is none. The ride-along is an optimisation, never
a correctness requirement.

- **Overlay** (`pgs_overlay::prepare_stage`): a `kept` track is used in place
  of the demux, on any container.
- **Burn** (`subtitles::ensure_burn_file`) — **only when the source container
  is not MPEG-TS.** The ride-along has no `-copyts`, so on a timestamp
  discontinuity it gets ffmpeg's correction and the source extraction does not;
  continuous timelines agree (tested), discontinuous ones were never tested.
  Until a discontinuous-m2ts fixture proves otherwise, an MPEG-TS source uses
  today's extraction. The overlay has no such restriction.
  - `kept` → derive the `.mks` from the `.sup` with `-copyts` and **without**
    `-start_at_zero`, `-map 0:s:0 -c copy -avoid_negative_ts disabled -f
    matroska`, under the existing name
    `f{id}-s{n}-{sha256(object_version)}-burn-v2.mks` in `<cache>/subs`, which
    stays disposable in its LRU because it is cheap to rebuild. `-map 0:t?` is
    not carried; fonts mean nothing to a bitmap track.
  - `empty` → answer "nothing to burn" without extracting — a new return shape
    (`Burn::File` / `Burn::Nothing`), with the session start at
    `transcode.rs:19950` building its pipeline without a subtitle overlay for
    `Nothing`, and its own test — instead of reading
    the whole source to publish an empty sidecar — which on file 5208 would be
    402 s spent to learn nothing.
  - if the derivation fails, or the derived `.mks` exceeds `MAX_BURN_BYTES`
    (possible for a stored `.sup` between 64 and 256 MiB), fall back to
    today's source extraction **inside the same flight**, and never record the
    derivation failure in the negative memo — the memo would block the
    fallback for its TTL.

**Off means off for both sides.** The Developer switch that stops the producer
also makes both consumers ignore the store, so a wrong artifact published by a
bad build can be taken out of service by one switch without a redeploy.

**Capability gate — behaviour, not presence.** At startup, run the tee against a
tiny synthetic source: one corrupted slave, one good slave, and the sentinel.
Require exit 0, an intact index output, the expected per-track verdicts from
the `framecrc` rule, and the stderr line recognised. If any of those fails, the
ride-along is off, and the Developer enable section says which check failed.
`PLURX_FFMPEG` can point at a different build than the one the design was
tested on, and listing `tee` and `sup` in `-muxers` proves neither isolation nor
wording.

### 6.4 Attribution, and a way to stop it

This is background work on real disks, so it has to be visible from inside the
product:

- the analysis progress row says the pass is **also extracting N PGS tracks**,
  and the bytes written;
- the cache diagnostics show the subtitle store's size and directory count;
- counters for **lookups by outcome** — `hit`, `empty`, and misses by reason:
  `absent`, `stale`, `never_indexed`, `hydrated_only` (a named store query: a
  fragment-index location row exists for this node and its `built_by_node_id`
  is another node) —
  plus ride-along tracks attempted and verdicts by kind;
- a Developer setting that turns the ride-along off, whose enable section
  states what it needs — the startup self-test, and a local cache filesystem —
  and whether each is met. Advisory, never blocking the switch.

### 6.5 Which tracks, and what it costs

**All PGS tracks on the file**, as probed. v1's scope question — default/forced
only, or on first selection — was about ten separate 79.5 GB reads; on one pass
it dissolves. Disk is small: a real PGS track is single-digit megabytes, and
the one on file 5208 was 18,866 bytes. **PNG compilation stays lazy and LRU** —
that is the expensive form, and pre-compiling ten generations per film would
evict other films' work to store pictures nobody asked for.

### 6.6 What this deliberately does not solve

**Coverage is per node, and best-effort.** A node that hydrates its index from
a peer does not run the pass, so it has no artifact until it runs one for
another reason, and the on-demand path serves it exactly as today. The
`hydrated_only` counter is what would show whether that is good enough;
shipping the artifact over the peer transport is the follow-up if it is not.

**Backfill.** Nothing re-indexes a library to collect subtitles. A deliberate
backfill is a separate decision with a real I/O cost, to be made against the
measured miss rate.

**The first start of a never-indexed file.** If a title is played before its
index has run, the start path is exactly what #445 made it: a pending answer in
seconds and the extraction continuing behind it.

### 6.7 Build order

Three PRs, each shippable alone, each with its own review:

1. **The store and the consumers**, with nothing producing into it yet:
   manifest format and reader, key, sweep, `.access`, the overlay and burn
   lookups, the `.mks` derivation with its timestamp fixture (first cue at 60 s,
   on a zero and a non-zero source start), the `empty` short-circuit and its
   `Burn::Nothing` shape, the fallback-inside-the-flight rule, the MPEG-TS
   exclusion, the lookup counters, and the consumer half of the off switch.
   Tested with hand-written manifests. Changes no behaviour on a fleet with an
   empty store — every lookup misses and falls through — but it refactors the
   existing flight, memo and cancellation path to put the fallback inside the
   flight, so the whole existing `subtitles.rs` suite runs unchanged as its
   safety net. `fragment_index_cluster::object_version` (or a `(dev, ino)`
   helper) becomes `pub(crate)`.
2. **The producer, with its own safety**: probe ordinals, the tee argv
   downstream of the digest, the stage and drop guard, the per-track verdict,
   publish (with its ordering), the latch and its test, the local-filesystem
   check, the startup self-test that switches the ride-along off when ffmpeg
   does not behave, and the Developer switch with its enable section — a
   producer never ships without both. Tests: the digest pin, and fixtures for a
   corrupted-first-segment track, a stale ordinal, an empty track and a
   backslash stage path.
3. **Visibility**: the progress-row attribution and the cache diagnostics.

---

## 7. Non-goals

- **Making the extraction itself faster.** It is an array read at line rate.
  Any design that claims to speed it up is wrong about what it is doing.
- **Windowed extraction for the burn path.** The module's own measurement says
  a window past the midpoint costs as much as the whole track, and a burn needs
  the whole timeline anyway.
- **Converting PGS to text (OCR).** A separate feature with its own accuracy
  and language problems; not on this path.
- **Removing the burn path.** Clients that cannot draw bitmaps still need it,
  so it stays whatever happens to the overlay.
- **Changing `START_DEADLINE`.** 50 s is not the problem; awaiting a 400 s
  artifact inside any budget is.

---

## 8. Constraints any implementation must respect

Both bit this work already:

1. **`tests/client-fixes.toml` anchor row.** `validation/history.py` requires a
   corrective commit whose subject matches `^fix(` and which touches
   `clients/` to carry a `[[fixes]]` row with `id, commits, source,
   source_anchor, test, test_anchor`, both anchors literally present. A
   `validation/regressions.d/` row does **not** satisfy it. The row cannot name
   its own sha, so it goes in a follow-up commit whose subject does not start
   with `fix(`.
2. **`validation.mobile_versions`.** Touching `clients/apple/Sources/` or
   `clients/android/app/src/main/` requires the release counters to move:
   `clients/apple/project.yml` `CURRENT_PROJECT_VERSION` and
   `clients/android/app/build.gradle.kts` `versionCode`. **PR #444 has already
   claimed Apple 176 / Android 117**, so B must bump past those.

---

## 9. What review is asked for

Ranked by how much the answer changes the work.

1. ~~**§5.4 B1 — is capability negotiation the right shape?**~~ **Answered:
   yes, and it shipped in #447** — as a list of protocol names rather than a
   boolean. A web renderer remains the alternative that would remove the skew
   surface entirely, and remains more work; nothing in #447 forecloses it.
2. ~~**§6.2 — does the fragment-index pass already read the whole file?**~~
   **Answered: yes.** Fix C attaches to that job. See §6.2.
3. **§6.4 — which tracks get built ahead of play?** Eager for all is ten full
   reads on a 10-track disc; lazy-then-queued means the first selection is
   still slow.
4. **§4.3 — is `exhausted` at ~27 s acceptable as an interim?** The alternative
   is a `preparing` surface that can outlast the ladder with a progress
   reading, which is a client contract change. PR #444 is already working on
   wait copy and may collide.
5. **§5.6 — do the two Partial M0 parser items close empirically?** There are
   now real PGS extracts in the fleet (including the 18 KB one from 5208). Is
   running the parser over real library extracts sufficient evidence, or is the
   requirement asking for something else?
6. **§5.5 — is the narrower proof bar defensible**, or does HDR/DV acceptance
   genuinely need the matrix the plan asks for?
7. **Anything in §2 that is wrong.** The container was recreated, so the log
   lines in §2.1 cannot be re-fetched; everything else is re-derivable from the
   store and the code.

---

## 10. Anchors, to re-verify at build time

| Thing | Where |
|---|---|
| `START_DEADLINE = 50 s` | `crates/plurxd/src/media_sessions.rs:54` |
| `EXTRACTION_TIMEOUT = 600 s` | `crates/plurxd/src/subtitles.rs:35` |
| unbounded join (pre-fix) | `crates/plurxd/src/subtitles.rs` `join_flight` |
| burn extraction command | `crates/plurxd/src/subtitles.rs` `ensure_burn_file` |
| placement-deadline arm | `crates/plurxd/src/http/hls.rs`, `exceeded the placement deadline` |
| overlay demux command | `crates/plurxd/src/pgs_overlay.rs:515` |
| overlay async contract | `crates/plurxd/src/pgs_overlay.rs` `PrepareState` |
| overlay cache bounds | `crates/plurxd/src/pgs_overlay.rs:32` |
| gate default `false` | `crates/plurxd/src/state.rs:762` |
| gate setting key | `crates/plurx-core/src/store/mod.rs:1794` |
| Developer surface | `crates/plurxd/src/http/developer.rs:601` |
| auto-select predicate | `crates/plurx-core/src/tracks.rs:235` |
| auto-select call site | `crates/plurxd/src/http/stream.rs:2230` |
| per-caller overlay term | `crates/plurxd/src/http/stream.rs` `overlay_for_caller` |
| the claim on the wire | `crates/plurx-core/src/playback/caps.rs` `subtitle_overlays` |
| pre-play hardcoded off | `crates/plurxd/src/http/dto.rs:506` |
| web applies the server default | `web/player/decode-tiers.js` "Server-chosen default subtitle" |
| web force-override (explicit pick only) | `web/player/decode-tiers.js`, the `preBurn` branch |
| bitmap needs a burn | `web/player/menus.js` `subNeedsBurn` |
| the burn decision | `web/player/audio-sync.js` `setSub` |
| web ignores the pre-play echo | `web/detail/preplay-selection.js` `prePlayApplication` |
| which track the policy picks | `crates/plurx-core/src/tracks.rs` `forced_or_default` |
| retry ladder (web) | `crates/plurxd/src/web/playback-policy.js:871` |
| retry ladder (Apple) | `clients/apple/Sources/PlayerController.swift:1550` |
| `playbackId` per open | `clients/android/.../player/PlaybackIntent.kt:16` |
