# PGS subtitles on the start path — why a 79.5 GB read blocks playback, and the three fixes

**Status:** §4 merged (PR #445, `5c605768`), not deployed · §5 and §6 proposed and unbuilt · reviewed — see [PGS-SUBTITLE-START-PATH-RCA-REVIEW.md](PGS-SUBTITLE-START-PATH-RCA-REVIEW.md) ·
**Reviewer:** Fable, adversarial · **Written:** 2026-09-22 ·
**Reported by:** Paul, 2026-09-21 ~18:50 ET, Android on the TCL tablet

Companion to [PGS_OVERLAY_PLAN.md](PGS_OVERLAY_PLAN.md) (the overlay's own
milestones) and [SUBTITLE-RELIABILITY-ASSESSMENT.md](SUBTITLE-RELIABILITY-ASSESSMENT.md)
(the 2026-09-16 subtitle arc) — this is *why one title would not start, and
what to do about it*.

Read §2 before anything else: the reported symptom is the least interesting of
the three failures in the incident, and fixing it — already merged — does not
make the title play. §4 is in flight. **§5 and §6 are the parts worth
reviewing before they are built.**

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
deliberately reverted; B3 and B4 are open, and the gate does not flip until
§5.5's check runs.

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

The plan's M4 says *"enable default/forced PGS overlay selection for approved
clients"*. **There is no notion of an approved client anywhere in the tree.**
The gate is a single node-wide boolean; nothing in a request says whether the
caller can render `pgs-v1`. Both native clients read the `overlay` field off
the response (`clients/apple/Sources/Models.swift:746`,
`clients/android/.../Models.kt:427`) and neither declares a capability.

And **the web client has no renderer at all.** It knows the overlay exists and
routes around it — `crates/plurxd/src/web/player/decode-tiers.js:955`:

> "The server's plan is a remux (or a direct play) when the PGS application
> overlay is enabled — a delivery this player does not implement"

so it force-overrides `initialRoute` to `'transcode_hls'`.

**Consequence, before #447: turning the gate on downgraded a web direct-play
to a full transcode** whenever the language policy picked a PGS track — or, on
an HDR title, replaced the subtitle with a degraded notice. `/decision`
auto-selected a PGS track for every caller (`deliverable_as_default` at
`crates/plurx-core/src/tracks.rs:235`, called from `http/stream.rs:2230`),
including the one that cannot draw it, and stamped it `default` on the wire
(`stream.rs:2376-2379`). The web then applies that pick 400 ms after open
(`web/player/decode-tiers.js:935-948`), `subNeedsBurn` is true for any bitmap
track (`menus.js:23`, `s.text===false`), and `setSub` turns it into a burn or
a `keep_hdr` refusal (`audio-sync.js:85-117`). `docs/PLAYBACK.md`
(`server.track-selection`) documents this path as settled fact.

> **This paragraph was wrong twice, in opposite directions, and the second time
> was mine.** The first revision called it a code-provable regression, which
> was right. A later revision — mine, after the first adversarial review —
> declared it false on the grounds that *"the web never reads the server's
> policy pick"*, citing `web/detail/preplay-selection.js:135`. That citation is
> about a different mechanism: the echo of an **explicit viewer pick**, which
> the web is right to ignore. It says nothing about the `default` flag, which
> the web does apply. The over-generalisation is corrected here and in #447's
> own body.

The web's local `initialRoute` override (`decode-tiers.js:965`) does **not**
save it: that branch is reached only when `preBurn` is non-null, which happens
only for an explicit viewer pick (`web/detail/preplay-selection.js:139-158`).
Nothing stood between the auto-pick and the burn.

Underneath the bytes there is a defect about honesty, and it is the one #447
fixes: the server issued a plan the caller cannot execute and then described
the delivery in terms untrue for that caller. **#447 closed both.** The overlay
term is now narrowed per caller, so a client with no renderer is not offered
the PGS default at all, and there is no longer a regression waiting behind the
gate. The rest of §5.4 is what that took.

### 5.4 Shape — B1 built, B2 deliberately not, B5 partial (#447)

#447 (`883cf4d42`) settled three of the five, and not the way this section
proposed:

| | proposed | shipped |
|---|---|---|
| **B1** capability negotiation | a boolean the server ANDs in | built, as a **list** of protocol names, with two limits the review added |
| **B2** thread the switch into item detail | do it | **implemented, reviewed, reverted** — the proposal rested on a claim that is false for the web |
| **B3** seek test on each native client | — | open |
| **B4** overlay-failure guardrail | — | open |
| **B5** correct the documentation | three docs | `PLAYBACK.md` done; the acceptance doc's retired env gate and the missing Android equivalent still open |

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
> doc's retired env gate and the missing Android equivalent are still open.

### 5.5 The proof bar — deliberately not a matrix

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

The gate stays off until this runs. `overlay_for_caller` removes the reason
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

## 6. Fix C — build the artifact on the analysis queue (proposed)

### 6.1 The finding that motivates it

There are currently **two independent full reads of the same PGS packets,
producing two formats**:

| Artifact | Producer | Path |
|---|---|---|
| `f<id>-s<n>-<sha>-burn-v2.mks` | `subtitles::ensure_burn_file` | `/srv/plurx/cache/subs/` |
| `track.sup` | `pgs_overlay::prepare` | `<subs_dir>/pgs/<generation>/` |

Same source, same packets, 79.5 GB each, different container. If the queue
builds the `.sup` once, the burn path can derive from it and the second read
disappears.

### 6.2 Proposed shape

The machinery exists and the precedent is exact: the analysis queue /
fragment-index job is the same animal — an expensive per-file full-pass
artifact, produced in the background, with `vod_index_pending` as the
retryable refusal while it is not ready. A subtitle-sidecar job keyed on
`(file_id, track_index, source identity)` belongs in it.

**Checked, and it does.** The fragment-index pass is already one sequential
demux of the whole container, confirmed in review. So extracting every PGS
track during that same read costs close to nothing rather than a second full
pass, and Fix C should attach to that job rather than be a new one. This was
§9's second question; it is answered.

### 6.3 The eviction problem

The overlay cache is an LRU: `MAX_CACHE_BYTES = 2 GB`,
`MAX_CACHE_TRACKS = 128`, `MAX_TRACK_BYTES = 256 MB`, with `prune()` on every
exit path (`pgs_overlay.rs:32`). A queue-built artifact dropped into that cache
will be evicted and rebuilt at the worst possible moment. A pre-built artifact
needs a durable home, or the queue needs to be the cache's floor rather than
one of its writers.

### 6.4 Scope question

Building every PGS track of every file eagerly is a lot of I/O for tracks
nobody selects. A 10-track disc is ten 79.5 GB reads. Candidate policies:
default/forced tracks only · on first selection, then queued · on scan for
titles whose delivery plan would need it. §9 asks.

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
| web applies the server default | `crates/plurxd/src/web/player/decode-tiers.js:935` |
| web force-override (explicit pick only) | `crates/plurxd/src/web/player/decode-tiers.js:965` |
| web ignores the pre-play echo | `crates/plurxd/src/web/detail/preplay-selection.js:135` |
| retry ladder (web) | `crates/plurxd/src/web/playback-policy.js:871` |
| retry ladder (Apple) | `clients/apple/Sources/PlayerController.swift:1550` |
| `playbackId` per open | `clients/android/.../player/PlaybackIntent.kt:16` |
