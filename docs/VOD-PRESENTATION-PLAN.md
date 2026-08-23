# VOD presentation — every title is a film, not a broadcast

**Status:** M0 RUN, M1 BLOCKED — P0's fidelity proof failed clause (b) and
§2.2 needs a review decision before any product code; results are in §12 and
the decision is requested in
[VOD-PRESENTATION-M0-REVIEW-BRIEF.md](VOD-PRESENTATION-M0-REVIEW-BRIEF.md) ·
**Review:** [VOD-PRESENTATION-PLAN-REVIEW.md](VOD-PRESENTATION-PLAN-REVIEW.md)
· **Response:**
[VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md](VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md)
(finding-by-finding assessment; B5's factual premise refuted there)
· **Supersedes:** the live-HLS presentation contract in
[PLAYBACK.md](PLAYBACK.md) once executed · **Companions:**
[ADAPTIVE-QUALITY.md](ADAPTIVE-QUALITY.md) (rung policy, unchanged by this
plan) and [SEGMENTER-PLAN.md](SEGMENTER-PLAN.md) (the copy segmenter this plan
reuses) · **Written:** 2026-08-23 against `origin/main` @ `9cace96e` —
**re-verify every cited line at build time; the file is the truth, this doc is
the map**

How to work this plan: read §0–§2 before anything else — §2 is the contract
and everything after it is consequences. Milestones (§8) go in order; each
ends with an acceptance check that is a runnable command or an observable
fact on a named machine. M0 is a feasibility-and-measurement spike that can
still change three decisions (D1, D4, D6 in §9) — and M0-P0 is a proof, not
a measurement; do not start M1 until M0's results are recorded. **M0 ran on
2026-08-23 and §12 records it: D1, D4 and D6 are resolved there, P0 clause
(b) failed, and §2.2 is flagged for re-decision — so M1 is blocked.**
If a step seems to require changing something §7 forbids, **stop and flag it
instead of improvising** — the guardrails are load-bearing.

## 0. Why — the live presentation is the root cause, not the bugs

plurx currently presents a fixed-length film as a live event stream, to
every client, for the entire duration of playback: a growing EVENT playlist
behind a publish gate, a client deliberately parked behind a "live edge,"
`#EXT-X-ENDLIST` only near the film's end, and a session whose identity dies
on every seek, track change, or recovery. Every player framework plurx
targets — hls.js, AVPlayer, Media3 — is an excellent VOD player and a
mediocre, paranoid live player. The result, measured on this repo's own
history from 2026-07-25 to 2026-08-23:

- ~188 playback-stability commits, across ~30 PRs, in 30 days.
- ~35 distinct named recovery mechanisms across the four layers.
- Recovery/stall/telemetry machinery is now ~47 % of the web player
  (~2,400 of ~5,200 player lines incl. `playback-policy.js`), ~42–52 % of
  `PlayerController.swift` (~2,100 of 4,951 lines plus the ~480-line
  open/reopen pipeline), and ~45 % of the Android player core.
- The fixes have begun fighting each other. Build 63's tvOS delivery
  watchdog killed *healthy* players every ~2.4 minutes and was corrected the
  next day (`276a1f4b`). A global reopen brake (`RecoveryReopenBudget`) had
  to be added because the per-detector budgets, each resetting on different
  evidence, together produced nine reopens in sixteen seconds. The
  ahead-window release threshold moved three times in four days (#87, #91,
  and its partial revert). Android's stall budget was rewritten three times
  inside one PR series (#437) and patched again the day after merge
  (`229a0f1f`). Three clients implement "how many reopens before giving up"
  with three different numbers (2, 3, 3-per-60 s).

That is not a bug tail converging. It is an architecture generating defects
faster than they can be fixed, because every client must compensate — with
watchdogs, budgets, reopen queues, and origin arithmetic — for five
consequences of the live presentation:

| # | Server behavior | Where | What clients built to cope |
|---|---|---|---|
| 1 | Live-edge semantics for a known-length film: growing playlist, publish gate, ENDLIST withheld until the end | gate `COPY_PUBLISH_GATE_SECS` `plurx-core/src/transcode/mod.rs:226`; transcode cushion `transcode.rs:617-632` | web reload keeper (`index.html:5209`), Apple momentary-end machinery (`PlayerController.swift:3388-3513`), cushion/gate constants retuned per client |
| 2 | The playlist **mutates under the client** ~3 min in: same URL flips EVENT → sliding, MEDIA-SEQUENCE starts advancing at the first prune | `served_live_playlist` `transcode.rs:648-728`; its own comment names the AVPlayer stall it causes | Apple seek-landing verification and live-edge holdback; the honest shape (`playback.hls_typeless_sliding`) is a **default-off experiment** |
| 3 | Every seek, track change, quality change, and recovery is a full session teardown + create; the predecessor's directory is deleted at create start so its in-flight fetches 404 — "by design" (PLAYBACK.md:1109); reopens land on the keyframe *before* the target | `reap_superseded` `transcode.rs:7561`, `retire_session` `:8809` | reopen queues, generation counters, create coordinators, stall budgets on all three clients; the "resumes a few seconds back" symptom |
| 4 | The 60 s idle reaper kills sessions under healthy buffered players; the status poll deliberately does not count as life | `SESSION_IDLE_SECS` `transcode.rs:37`, `reap_loop` `:9669`, test `polling_status_does_not_keep_a_session_alive` | `preferredForwardBufferDuration=60` as a fetch-cadence hack, retention widened to 180 s to chase AVPlayer's real lead, reopen-on-404 machinery |
| 5 | Session-scoped timelines: each session's t=0 is wherever it started | `media_origin` correction `transcode.rs:1482-1491` | `baseMs` / `media_origin_ms` arithmetic in all three clients (`MediaOrigin.kt`, `PlayerController.swift:4394`, `index.html` offset rebasing) — a standing source of position bugs |

The user-visible symptoms map exactly: *freeze then error* = a recovery
budget exhausting or a typed refusal; *freeze then resume* = one of the
~12 watchdogs winning a reopen; *freeze then resume seconds back* = the
keyframe-floor reopen (row 3), plus a suspected Android sliding-window
position bug (§6 S1).

**The counterfactual already ships.** A pretranscode cache hit is served as
`vod: true` — a complete playlist with ENDLIST (`serve_cached`
`transcode.rs:5243`) — and on that path Apple seeks natively in under a
second, the web player needs no keeper, and essentially none of the
machinery above engages. The problem is that only transcode requests can
ever reach it: `serve_cached` has exactly one production call site
(`transcode.rs:7805`, the transcode lane) — **the copy path never consults
the cache at all**, so every watch of a remux re-runs from scratch under
live semantics.

This plan makes the VOD shape the *only* shape a supporting client ever
sees, from the first byte of the first watch.

## 1. The contract change in one paragraph

Serve every title as VOD from the first byte: a complete, film-time
playlist available at t=0 — every segment listed with its real duration,
`#EXT-X-ENDLIST` present, one URL whose bytes never change for the life of
the playback. Segments are still produced just-in-time; a GET for a segment
that does not exist yet **blocks briefly** while the producer catches up or
repositions, instead of the client discovering media by re-polling a
playlist. Sessions demote from identity-bearing objects to handles for
auth, telemetry, and flow control; recovery becomes an idempotent HTTP
retry against immutable resources instead of a teardown that loses the
buffer, the position, and the track selections. The clients then get to be
what their frameworks are good at: plain VOD players.

Two facts make this feasible rather than aspirational. First, segment
durations are knowable upfront on both paths: the transcode path already
forces keyframes on a fixed 2 s grid (`-force_key_frames
expr:gte(t,n_forced*2)` + `-hls_time 2`, `plurx-core/src/transcode/mod.rs:
1030-1067`), and on the copy path a whole-title **fragment index** built by
the production-shaped copy pipe (the same pipe `scripts/gop-census` launches
and parses — its input is the remuxed fragment stream, not a cheap packet
probe; review finding B1) captures every fact the cut decision needs, so the
entire segmentation is computed once per file and persisted (§2.2). Second,
the hard server pieces exist in embryo: request-blocking segment serving
(`SEGMENT_WAIT`, `transcode.rs:97` — today 20 s for listed-not-yet-written
segments), a completed-VOD assembly path (`produce::assemble` →
`publish_from` → `complete_cache_entry`), and reader-guarded eviction
(`cachekeep.rs`).

## 2. The presentation contract

Everything in this section is normative for the new shape. The legacy live
shape remains in the codebase, served to clients that do not opt in
(§2.7), and to NULL-duration files (§2.6).

### 2.1 The playlist — immutable, complete, film-addressed

- One media playlist per (file, recipe, rung), generated from the segment
  plan (§2.2), not from producer progress. It lists **every** segment of
  the whole title with its planned `#EXTINF`, ends with `#EXT-X-ENDLIST`,
  carries `#EXT-X-PLAYLIST-TYPE:VOD` and `#EXT-X-MEDIA-SEQUENCE:0`, and
  its `#EXT-X-TARGETDURATION` is the ceiling of the largest planned
  duration. fMP4 playlists keep `#EXT-X-MAP:URI="init.mp4"`.
- The playlist's bytes are **immutable for the life of the playback**. No
  EVENT→sliding rewrite, no MEDIA-SEQUENCE advance, no publish gate: the
  playlist is available the moment the session answers, before any media
  exists. `served_live_playlist` never runs for a VOD-presentation session.
- Segment names are plan-indexed and film-time-stable: `seg00000.m4s` /
  `.ts` is the segment covering the plan's first entry, forever, for every
  session sharing the store (§2.4). A reopened or resurrected (§2.5)
  session serves the identical playlist.
- Timeline zero is **file time zero**. There is no `start_seconds` rebase:
  resume and seek are the client's own `startPosition`/`seekTo` against the
  full timeline. `media_origin_ms` mapping, `baseMs` arithmetic, and
  subtitle cue re-shifting all become dead code on this path.
- Serving stays `Cache-Control: no-store` for the playlist and
  `private, max-age=3600, immutable` for segments, unchanged
  (`http/hls.rs:531,1775`).

### 2.2 The segment plan — normative, indexed, and film-time addressed

The plan is the single source of truth for playlist text, segment naming,
segment timestamps, and producer scheduling. It is **normative, not
predictive** (review B1, ledger D10): the materializing producer cuts *at*
the plan's boundaries; it never re-decides them. That dissolves the trap
the review caught — a cheap packet index cannot reproduce
`fmp4::classify`'s NAL-level clean/dirty verdicts (`fmp4.rs:1556-1610`) or
`Segmenter`'s output-byte accounting (`pending_bytes += fragment.len()`,
`fmp4.rs:2338`) — because nothing needs to *reproduce* today's cuts; the
plan needs boundaries that are provably clean, floor/ceiling-respecting,
and computable upfront.

- **Copy path — the fragment index.** The indexer runs the
  production-shaped copy pipe (the same `copy_pipe_args` shape,
  **video-only**, unpaced) and feeds the fragment stream through the
  existing reader + `classify`, recording per fragment: first-sample DTS,
  duration ticks, output byte length, and the clean/dirty verdict. This is
  exactly what `scripts/gop-census` does today minus the census math — the
  index is that walk, persisted. Video-only makes the index (and therefore
  the plan) valid for **every audio selection** of the file; the byte
  ceiling is applied to video fragment bytes with a fixed audio headroom
  (audio adds at most bitrate × duration, bounded by the probe), proven in
  M0-P0. The index is persisted per file, keyed by the file's identity
  recipe like the cache, built in the background at scan/analyze time or on
  first demand. **A file with no index keeps the legacy live presentation
  for that watch** and converts from the next one — no cold play ever waits
  on indexing. Index build cost is M0-P1; plan-vs-production fidelity is
  M0-P0 and gates everything (D4).
- **Copy plan entries** are `CutPolicy` applied over the index's clean
  boundaries: per entry `(start_ticks, end_ticks, duration, est_bytes)`,
  keyed by film-time DTS — not by byte offset (the copy path has no
  byte-seek mechanism, `mod.rs:1164-1173`) and not by fragment ordinal (so
  an ffmpeg upgrade that re-fragments differently is detected, not silently
  misaligned).
- **Transcode path:** `ceil(duration / 2)` entries of 2.0 s, last entry the
  remainder — the encoder is instructed to cut exactly there. Real emitted
  durations jitter ~±0.08 s against the nominal EXTINF; whether every stack
  tolerates that drift in a VOD playlist is **M0-P2**, and the fallback is
  D6-B: route the encode through the copy path's fMP4 segmenter, which cuts
  the planned boundaries exactly.
- Audio tails at end-of-stream follow the c58a4307 rule (trailing audio
  split at the policy ceiling into audio-only entries), computed from the
  probe's per-track durations — the final EXTINFs are honest upfront.

**The media-time contract** (review B2 — a stable name is nothing without
stable time inside the bytes):

- Every materialized segment's timeline is rebased to its plan entry's
  film-time start. Copy: the segmenter writes the published segment's
  `tfdt` as plan `start_ticks` and stamps `mfhd.sequence_number` = plan
  index + 1 — it already owns both boxes when merging
  (`fmp4.rs:1896-1944`); the producer's `-avoid_negative_ts make_zero`
  near-zero timeline (`mod.rs:1403-1416`) is an input the segmenter
  corrects, deterministically, no matter which process generation produced
  the fragment. Transcode: the restarted encoder gets
  `-output_ts_offset <plan start>` — the exact mechanism the live probe in
  [PERF2-PLAN-REVIEW.md](PERF2-PLAN-REVIEW.md) §2 R1 validated when it
  proved `-start_number` alone moves names, not PTS.
- A producer repositioned to a plan boundary seeks with `-noaccurate_seek
  -ss <boundary>` (lands at the RAP at-or-before) and the segmenter
  discards fragments until the first whose DTS equals the boundary; a
  mismatch (never-equal) is a typed producer failure and an index
  invalidation, not a silent drift.
- The rendition's `init.mp4` is written by its first process generation and
  stored as **the** init. Every later generation's init must be
  byte-identical or the generation is refused with a typed
  `producer_failed` — an evicted-and-regenerated segment URI therefore
  either serves byte-compatible media or fails loudly, never quietly
  incompatible bytes. Init reproducibility for same-source-same-args is
  proven in M0-P0 before this rule is relied on.
- M2's acceptance includes the review's noncontiguous test: materialize
  segment 0, kill the producer, materialize a far segment first, fill both
  neighbors from separate generations, and prove continuous video/audio
  timestamps and clean playback on hls.js, AVPlayer, and Media3.

### 2.3 Segment serving — blocking materialization with one hard deadline

A GET for a planned segment has exactly three outcomes, and **every blocked
request ends through exactly one named path** (review B4 — v1 gave the wait
a budget and an escape hatch in the same paragraph; the escape hatch is
deleted):

1. **Materialized** → 200, bytes, immediately. The overwhelmingly common
   case: the producer runs ahead of the playhead (§2.4).
2. **Not yet materialized** → the response **blocks** until the segment
   lands, bounded by a **hard** per-request deadline: `min(server cap,
   the client's declared budget)`. The create body carries
   `block_budget_secs` — what this stack's own shorter timer allows
   (hls.js: whatever `fragLoadingTimeOut` the player configures; Media3:
   inside OkHttp's 60 s read timeout; AVPlayer: the M0-P3 measured value);
   the server default `playback.vod_block_secs` (15 s until P3 says
   otherwise) caps it. **Deadline expiry answers a typed, retryable
   `segment_pending` 503 with `Retry-After`** — never an open-ended wait,
   never a bare 404. All three stacks retry fragment loads; M0-P3 measures
   each stack's actual 503 handling to set budgets and retry configs, not
   to choose the shape.
3. **Producer dead or the request is beyond repair** → a **typed** 5xx with
   the refusal vocabulary the playlist path grew in `64a24854`
   (`producer_failed`, `session_failed`), because at that point an error is
   the truth. A planned segment is never 404'd; today's bare segment 404
   (`http/hls.rs:1669`) survives only for genuinely unknown names.

Bounding the wait pool (review B4's operational half): a client disconnect
**cancels** the server-side wait (response-body drop aborts it); blocked
GETs are capped per session (4) and globally (setting), with excess answered
`segment_pending` immediately; N waiters on one segment coalesce into one
unit of producer demand. M3's acceptance tests all four named cases:
disconnect mid-wait, producer death mid-wait, ten concurrent waiters on one
segment, and a 20-seek storm across distant segments.

A far seek — a GET for a segment well beyond the producer's position —
triggers **repositioning** (§2.4) and then blocks as case 2. The client
experiences a buffering spinner of the same 2–10 s a session re-create
costs today, except nothing is torn down, nothing 404s, no state is lost,
and no watchdog fires.

### 2.4 The title store and the producer scheduler

Segment storage moves from session-scoped scratch (deleted on retire,
`transcode.rs:8831`) to a **title-scoped store unified with the
pretranscode cache**: key = (file identity, recipe) — exactly the cache's
key discipline, which already excludes `start_seconds` on purpose
(`recipe.rs:187-191`).

- Live production writes into a cache generation. Segments are addressable
  by plan index; a generation may have **holes** (a far seek materializes
  segment 900 while 400–899 don't exist). The manifest is per-segment
  state, and it separates **three facts the v1 bitmap conflated** (review
  B3): *planned* (the segment belongs to this immutable rendition),
  *materialized now* (bytes exist and may be served), and *durably
  admitted* (the whole rendition has been atomically published as a cache
  hit). Eviction clears *materialized*, never *admitted*; completion
  requires every segment *materialized simultaneously under the completed-
  cache budget*, so an evicted hole can never be published as a hit.
- Two budgets, named separately: the **working set** (live materialized
  bytes, governed by the `playback.hls_ahead_max_bytes` /
  `playback.hls_scratch_max_bytes` successors) and the **completed cache**
  (`cache.max_gb`, 50 GB default, `PERF-PLAN.md:1681`). A rendition is
  **admissible** only if its planned total size (the plan knows it) fits an
  admission threshold under the completed-cache budget; admissible
  renditions reserve their space when backfill begins. An over-budget title
  — the plan's own 69 Mb/s reference remux is ~62 GB — is a
  **working-set-only rendition**: still VOD-presented, never
  cache-completed, honestly re-materialized on a later watch. So the copy
  path gains a cache **for admissible titles** — most transcodes and
  ordinary remuxes — not a false promise for every 4K remux. (This
  subsumes what PERF2-PLAN's N3 prefix-cache milestone wanted; N3 should
  be re-scoped against this plan rather than built separately.)
- Eviction is per-segment, budgeted by the existing scratch settings
  (`playback.hls_ahead_max_bytes` / `playback.hls_scratch_max_bytes`
  semantics carry over as store budgets), and **reader-guarded**: never
  evict inside any attached reader's window (playhead − back-window ..
  frontier + ahead-window). The `cachekeep` machinery (`cachekeep.rs`,
  guards at `transcode.rs:5260,5372`) is the reuse target. The 180 s
  retention constant stops being a client-visible contract and becomes a
  mere eviction preference — a rewind past it re-materializes instead of
  404ing.
- **One producer per (file, recipe, rung)**, shared by however many
  readers are attached (normally one). The scheduler's whole policy:
  produce while `materialized_through < max(attached demand frontiers) +
  AHEAD_HORIZON` (default 180 s — today's ahead-window value, unchanged);
  suspend beyond it (the existing SIGSTOP/SIGCONT machinery,
  `apply_ahead_window`, is reused with demand defined by *requests* instead
  of a per-session fetch ratchet); **reposition** (kill + restart at the
  plan entry's RAP) when a demanded segment is more than `REPOSITION_GAP`
  (default 60 s) ahead of the producer's current position — closer than
  that, catching up at ≥2× realtime is cheaper than a pipeline restart.
- Producer lifetime decouples from session lifetime: an idle-reaped
  session (§2.5) stops contributing demand; the producer suspends when no
  demand remains and is reclaimed after a grace period. Reaping a session
  **never deletes media** — the store owns the bytes.

### 2.5 Sessions become handles — and reaping becomes harmless

The session keeps its job as the unit of auth, telemetry attribution,
flow-control demand, and the stall-reopen contract — and loses its job as
the owner of media, timeline, and playlist identity.

- The create request/response shapes are unchanged except for the opt-in
  field (§2.7) and `vod: true` + full-duration semantics in the response.
  `playback_id` supersession, `request_id` idempotency, and the
  `previous_session_id`/`reopen_reason` stall contract
  (`http/hls.rs:105-140`) all survive as-is — a stall reopen still answers
  a one-rung-lower session (ADAPTIVE-QUALITY.md's boundary is untouched);
  it is just enormously cheaper, because the new session serves the same
  store and the same film-time addresses.
- Sessions persist a **lifecycle, not only a recipe** (review B6 — HLS
  child requests are autonomous, so a predecessor's late fetch must never
  re-animate a handle the server deliberately retired):

  ```text
  active ── idle reap ──▶ dormant ── authorized media GET ──▶ active
    │
    ├── explicit DELETE / supersession / admin stop ──▶ terminal
    ├── access revoked ────────────────────────────────▶ terminal
    └── file identity invalidated ─────────────────────▶ terminal

  terminal ── any old capability GET ──▶ 410 (tombstone), never resurrection
  ```

  Only an **idle reap** produces a *dormant* handle; a playlist or segment
  GET naming a dormant session resurrects it from the persisted recipe and
  serves normally — so the 60 s reaper keeps its housekeeping job without
  ever again killing a playback. Explicit DELETE, supersession by a newer
  create, admin stop, access revocation, and file-identity invalidation
  write a *terminal* tombstone: those ids answer typed `410 session_gone`
  forever (tombstone rows pruned after 24 h to a plain 404), and terminal
  transitions drop the handle's producer demand immediately. The dormant
  TTL is **sliding** — refreshed by every authorized media GET — with the
  supported pause named as a setting (`playback.vod_dormant_ttl_secs`,
  default 6 h): a player paused inside that window simply resumes, its next
  fetch re-materializing the window around its playhead. M3's acceptance
  proves idle-reap resumption AND that each terminal cause stays terminal.
- Flow-control accounting (per-session bytes, the global scratch cap with
  drainable-release, hold reasons in `SessionInfo`) moves its denominator
  from session scratch to store working set; the reporting surface
  (`hold_reason`, `resume_below_*`) is unchanged.

### 2.6 What stays live

Files with NULL/unprobed duration cannot be planned; they keep today's
live presentation end-to-end (and remain the reason the legacy path is not
deleted server-side). A follow-up worth doing but out of scope here:
probe-at-import so the population shrinks toward zero. Genuinely unbounded
sources (a future live-TV feature) would use the legacy path by nature.

### 2.7 Rollout mechanics — the client asks for it

A new create-body field `presentation: "vod"` (absent = legacy) declares
that the client understands the VOD contract. The server honors it only
when the setting `playback.vod_presentation` is enabled (default off until
M7). This is the entire compatibility story: old clients never see a new
shape, new clients on old servers get the legacy shape and keep their
machinery (which is why client compensations are deleted only in M8, after
the fleet flips). No wire version, no double-serving, no migration window
where a shipped client breaks.

## 3. Server work plan

The change concentrates in `crates/plurxd/src/transcode.rs` (17,301 lines
today) and `http/hls.rs`. New code should land as new modules, not more
transcode.rs — this plan is also the excuse to start paying that file down.

| Piece | Shape | Reuses |
|---|---|---|
| `plurx-core::segplan` | pure segment-plan builder: fragment-index type + `CutPolicy` over its clean boundaries for copy, grid plan for transcode; §2.2's media-time rules; serialization for persistence | `fmp4::CutPolicy`, `fmp4::classify`, the merger's `tfdt`/`mfhd` ownership |
| Fragment indexer | daemon-side run of the production-shaped video-only copy pipe through the existing fmp4 reader, recording DTS/duration/bytes/verdict per fragment; persisted in the store keyed by file identity; background job at scan/analyze | the `gop-census` walk, scan job plumbing |
| Title store | plan-indexed segment storage with bitmap manifest, per-segment reader-guarded eviction, generation completion into the cache | `cachekeep.rs`, `produce.rs::assemble`/`publish_from`, cache tables |
| Producer scheduler | one producer per (file, recipe, rung): demand tracking from requests, suspend/resume, reposition-at-RAP, health → typed refusals | `apply_ahead_window`, the SIGSTOP machinery, watchdog serialization from PR #244 |
| VOD serving | playlist-from-plan, blocking segment GET (three-outcome contract §2.3), session resurrection | `Manager::playlist` long-poll internals, `SEGMENT_WAIT` loop `transcode.rs:9243-9422`, typed refusals from `64a24854` |

Explicitly **unchanged**: encoders, rate control, the ladder and its
numbers, DV pipelines, admission, burn-in, subtitles, offline packaging,
progressive/direct-play serving, telemetry ingestion, the stall-reopen
rung-step contract. The legacy live path stays compilable and serves
non-opted-in clients and NULL-duration files.

## 4. Client work plans

Each client milestone has two halves: **adopt** (send `presentation:
"vod"`, handle the VOD shape — which every client already handles for
cache hits today) and, later in M8, **delete** (remove the live-presentation
compensations). Adopt is small; delete is the payoff. Nothing is deleted
until the fleet default flips (§8 M7), so a client build works against both
server generations throughout.

### 4.1 Web (`crates/plurxd/src/web/index.html` + `playback-policy.js`)

Adopt: send the flag; treat every session like today's `vod` sessions
(`startPosition` resume, native seeks); raise `fragLoadingTimeOut` to
cover `playback.vod_block_secs` + margin. Delete (M8): the reload keeper
(`index.html:5209-5241`), `truncated_stream` end-guessing (`:7879-7925`),
seek/audio/quality-as-reopen paths and their generation tokens, offset
rebasing and subtitle cue re-shifting, the "Still preparing" playlist-wait
machinery. Keep: ABR (`decideRung` and its `recent_speed` evidence — the
JIT `min(link, encode)` estimate problem is orthogonal and stays), decode
rescue and learned decode limits, hitch/rate-chase/beacon telemetry, the
compatibility fallback ladder.

### 4.2 Apple (`clients/apple/Sources/PlayerController.swift`)

Adopt: the flag; the existing `hls.vod` handling (`usesDirectTimeline`,
native seeks, duration-corroborated end) becomes the only path. Delete
(M8): momentary-end machinery (`PlayerItemEndAction` reopen/stop arms and
`lastUncorroboratedEndMs`), seek-as-reopen and the reopen queue's seek
half, `baseMs`/`media_origin` mapping and the keyframe-delta corrective
seek, the unestablished 30 s leash sized to the publish gate. Keep,
retargeted: the position-clock stall detector and
`DeliveryStarvationDetector` (genuine starvation still exists; their
reopen arms become rare), `BlackFrameWatchdog`, the codec/HDR
compatibility ladder, `StallReopenBudget` (the rung step-down still rides
session replacement), all telemetry. Budgets should shrink toward one
number once the reopen causes collapse.

### 4.3 Android (`clients/android/.../player/`)

Adopt: the flag; VOD timeline (the `sessionIsVod` arm becomes the only
arm). Delete (M8): `MediaOrigin`'s session arm, most of
`ControllerStallGuard`/`SessionCreateCoordinator` (user seeks stop being
server mutations racing stall reopens), the live-seek budget-reset special
case (`229a0f1f`). Keep: the compatibility rescue ladder, telemetry,
`StallReopenBudget` for the rung step-down. Fix as part of adoption, not
M8, because they are live bugs today (§6): the retrospective stall
detector and silent budget exhaustion.

## 5. What recovery looks like afterwards

Worth stating as the target end-state, because it is the acceptance
criterion for the whole plan: a stall is either **genuine starvation**
(link or encode too slow — the client buffers, the ABR steps a rung down
via the existing reopen contract, one mechanism per client) or **producer
death** (a typed 5xx — the client shows a real error with Try Again). The
categories that today produce freeze-then-error, freeze-then-resume, and
freeze-then-resume-behind — playlist mutation stalls, reap 404s,
prune 404s, publish-gate timeouts, momentary ends, keyframe-floor rewinds,
watchdog false positives — **cease to exist as categories**, because the
inputs that caused them are gone. The ~35-mechanism inventory shrinks to:
per-client compatibility ladders (device facts), one stall detector + one
rung-step budget per client, black-frame watchdog (Apple), and telemetry.

## 6. Stopgaps that land first, independent of this plan

Live defects found during this review; each is a small PR against main,
none depends on the presentation change, and none is made redundant by it
soon enough to skip:

- **S1 — Android position integrity under a sliding playlist (suspected,
  verify on device first).** Media3's `currentPosition` is window-relative;
  once the served playlist starts sliding (~3 min in), `baseMs +
  currentPosition` under-reports by the pruned prefix — flattening
  scrobbles and dragging stall-reopen resume points backward. Nothing in
  `MediaOrigin.kt:38-52` compensates, and no `EXT-X-PROGRAM-DATE-TIME` is
  served to re-anchor. If confirmed: re-anchor on
  `Timeline`/window-offset change. This is the best current suspect for
  Android "resumes from a few seconds back".
- **S2 — Android hard-stall escape + audible exhaustion.**
  `BufferingStallTracker.sample` only emits when buffering *ends* or the
  playhead moves (`PlaybackTelemetry.kt:284-306`) — so the #349 downgrade
  fires only retrospectively, and a stall that never resolves is never
  escaped by it; separately, `StallReopenBudget` exhaustion is silent
  (`Controller.kt:734-736`). Fix: emit at threshold while stalled, and
  beacon exhaustion.
- **S3 — Apple: observe the item's error-log notifications.** A mid-stream
  404 on a pruned segment lands in `errorLog()` unobserved (no
  `AVPlayerItemNewErrorLogEntry` observer) — structurally invisible until
  something else fails. One observer + one beacon.
- **S4 — Android 400-fallback leaks its predecessor.** The unbound retry
  strips `previous_session_id` after `sessionId` was already nulled without
  a DELETE (`StallReopen.kt:142-161`), so the predecessor's encoder slot
  is held until the reaper (~up to 75 s). DELETE it explicitly.

## 7. Non-goals and guardrails

- **No multivariant/seamless ABR in this plan.** Quality switching remains
  one encode + session replacement (ADAPTIVE-QUALITY Phase 3 stays a
  separate, later decision). This plan is Phase 3's prerequisite — film-time
  addressing is what would make rungs alignable — not its implementation.
- **No encoder, rate-control, ladder-number, DV, or admission changes.**
  The producer a scheduler starts is exactly the producer that runs today.
- **No offline/downloads changes.** That path has its own contract.
- **Do not retune recovery thresholds in passing.** Client machinery is
  deleted in M8 or left alone; "improving" a budget mid-migration recreates
  the last month.
- **Do not break the legacy path.** NULL-duration files and non-opted-in
  clients use it for the foreseeable future; its tests stay green.
- **Telemetry parity is mandatory.** Every beacon and server event that
  exists today still fires (or has a named successor) under the VOD
  presentation — the diagnosis capability this month's work bought is not
  spent.
- **The cluster contract is decided here, not deferred** (review B5, ledger
  D11): **a VOD rendition is bound to its owner node for its lifetime, and
  no takeover ever reuses its URIs.** [CLUSTERING-PLAN.md](CLUSTERING-PLAN.md)'s
  accepted takeover has a survivor rebuild the recipe with its locally
  valid encoder — protocol-compatible, not byte-identical output
  (`CLUSTERING-PLAN.md:322-327`) — and advance discontinuity sequence,
  which an immutable playlist cannot absorb. So on owner death, an active
  VOD playback gets a typed `producer_failed`/`session_failed` refusal and
  the client's existing reopen creates a **fresh rendition on a survivor**
  — a bounded interruption, which is
  [CLUSTER-MEDIA-POOL-PLAN.md](CLUSTER-MEDIA-POOL-PLAN.md)'s own promise —
  and no immutable URI ever names two different byte streams. Completed
  renditions are shared across nodes only where local digests match
  (CLUSTERING-PLAN's existing rule). M2's review includes a cluster-aware
  pass; M7's acceptance includes a power-pull during active VOD copy and
  transcode playbacks. Beyond that decided boundary, the standing rule
  holds: if the store/cache unification (§2.4) fights either cluster plan
  or hiqlite semantics anywhere else, stop and flag rather than inventing
  a parallel store — the M1c/M1d reviews are the scar.


## 8. Milestones

Each milestone is a PR (or a small stack) off main, gated by `make check`,
and ends with its named acceptance. Client milestones bump build numbers and
their doc claims per `validation/mobile_versions.py` + `doc_versions`;
corrective client commits need `tests/client-fixes.toml` anchor rows.
STATUS.html and PLAYBACK.md move in the same commit as the behavior they
describe. Efforts are relative t-shirt sizes for one focused agent.

### M0 — feasibility proofs + measurement spike (S–M, no product code)

One proof, then three probes, all recorded in an appendix commit to this
doc; D1/D4/D6 are decided by these results, not by taste. **P0 runs first
and gates the rest** (review B1: timing an index that cannot determine the
answer is not a feasibility result).

- **P0 — plan fidelity proof.** Build the fragment index (§2.2) and prove,
  over the fixture corpus and ≥10 representative real files: (a) the index
  is deterministic — two runs of the production-shaped video-only pipe
  yield identical fragment DTS/verdict streams; (b) every planned boundary
  the plan emits is a clean cut when the *production* pipe (with audio)
  materializes it, and the discard-until-boundary-DTS rule always
  converges; (c) planned `est_bytes` + audio headroom bounds the real
  segment bytes under the ceiling; (d) `init.mp4` is byte-identical across
  process generations for same source + args. Any (a)–(d) failure stops
  the plan for redesign — that is the point of running it first.
- **P1 — index cost.** Time the fragment-index build (the production-shaped
  video-only pipe, which reads the whole file) on ≥6 real titles on nynuc
  over NFS (biggest DV MKV included, cold cache). Decides how aggressive
  background indexing at import must be; first plays of unindexed files use
  the legacy presentation regardless (§2.2), so no cold play waits on this
  number (D4).
- **P2 — EXTINF-drift tolerance.** A synthetic VOD playlist whose EXTINFs
  are nominal 2.0 s against segments with the real ±0.08 s jitter; play on
  hls.js (playback-lab), Safari, a real AVPlayer device, and Media3.
  Decides transcode plan D6-A (nominal) vs D6-B (segmenter-exact).
- **P3 — blocked-fetch numbers.** A stub server that delays segment
  responses 5/15/30/60 s mid-stream and answers deadline expiry with the
  typed `segment_pending` 503 + `Retry-After` (§2.3); measure each stack's
  tolerated block and its actual 503 retry behavior. Sets
  `playback.vod_block_secs`, each client's declared `block_budget_secs`,
  and the retry configs — the numbers, not the shape (review B4).
- Reuse `scripts/playback-lab` for the web halves; the device halves are
  gpt runs with exact commands provided.

**Acceptance:** the appendix exists with the P0 proofs and P1–P3 numbers,
and D1/D4/D6 are marked resolved.

### M1 — segment plan + fragment index (M)

`plurx-core::segplan` (pure plan builder over the index) + the daemon
indexer + persistence + the background job, productionizing what P0
prototyped. No serving changes.

**Acceptance:** the P0 fidelity suite re-runs green as `cargo test`-able
fixtures plus the nynuc real-file sweep; plan build from a persisted index
is <100 ms; index invalidation on file-identity change is tested;
`cargo test --workspace` green.

### M2 — title store + producer scheduler (L, the big one)

Store with bitmap manifest and reader-guarded eviction; scheduler with
demand/suspend/reposition; generation completion into the cache. Legacy
serving still the only client-visible surface — this lands dark.

**Acceptance:** integration tests: two readers one producer; far-seek
reposition materializes without a session create; **the review's
noncontiguous timestamp test** — segment 0 materialized, producer killed, a
far segment materialized first, both neighbors filled by separate process
generations, timestamps continuous and film-time-correct (checked at the
fMP4/TS level here; on-device in M4–M6); eviction respects reader windows
under a tight budget; **a synthetic title larger than both scratch budgets
plays end to end with no false cache completion, no unbounded backfill, and
no missing segment under any admitted row** (review B3); kill-producer →
typed refusal; admissible completed rendition → cache hit on the next
create (copy path included — the first-ever copy cache hit is the headline
assertion). M2's design review includes a cluster-aware pass against
CLUSTERING-PLAN.md and CLUSTER-MEDIA-POOL-PLAN.md (review B5). Legacy
behavior provably unchanged: `playback-lab normalize` base-vs-candidate
byte-identical.

### M3 — VOD serving behind the opt-in (M)

`presentation: "vod"` + `playback.vod_presentation` + playlist-from-plan +
blocking segment GET + resurrection. Curlable end to end.

**Acceptance:** contract tests for §2.1/§2.3/§2.5: immutable playlist
bytes across the session's life; the three GET outcomes including deadline
expiry → typed `segment_pending` 503; **the review's four wait-pool cases**
— client disconnect cancels the wait, producer death mid-wait answers
typed, ten concurrent waiters on one segment coalesce to one demand, a
20-seek storm stays inside the blocked-GET caps (review B4); idle reap →
fetch → resurrection with zero client-visible failure, **and every terminal
cause — DELETE, supersession, admin stop, revoked access, file replacement
— answers 410 and never resurrects** (review B6); sliding dormant-TTL
refresh proven. A playback-lab VOD suite passes; a recorded curl transcript
of a full life (create → playlist → blocking fetch → far seek → reap →
resurrect → ENDLIST-complete) goes in docs/PLAYBACK-TESTING.md.

### M4 — web adoption (M)

Send the flag; VOD-path handling; timeout configs from P3. No deletions.

**Acceptance:** playback-lab stall-recovery + VOD suites green; on nynuc:
a 2-hour 4K remux plays end to end with zero keeper fires and zero
reopens; a 20-seek storm lands every seek natively with no session create
(server log assertion); sleep/wake mid-film resumes without error.

### M5 — Apple adoption (M–L)

Flag + VOD path + timeout behavior; build bump; STATUS.html device rows.

**Acceptance (gpt device runs, exact protocol in STATUS.html):** the
historical killer cases on real hardware — the 2160p HEVC copy title that
wedged tvOS (Three Months, file 5836), a DV P5 episode, the
trailing-audio title (Dexter S01E01), a >60 s pause deep in a film, an
autoplay episode boundary, a 30-minute scrub torture run — all with zero
terminal screens, zero resume-behind, and stall beacons only for genuine
starvation.

### M6 — Android adoption (M)

Flag + VOD timeline + S1/S2 fixes riding along; build bump.

**Acceptance:** device runs on Google TV + phone: the M5 case list minus
DV P5 (plus a Google TV DV title), scrobble positions monotone and
absolute (the S1 assertion), stall-downgrade still demonstrably works
under playback-lab's shaped-link scenario.

### M7 — fleet flip + burn-in (S)

Deploy server to all nodes (ansible); flip `playback.vod_presentation`
default on; one week of `playback_events` comparison (stall rate, reopen
rate, session_end reasons, TTFF) against the prior week, reviewed and
recorded in STATUS.html.

**Acceptance:** the week's numbers show reopens and terminal failures down,
not merely moved; no new event kind appears; rollback is one setting; and
**a power-pull on the owner node during active VOD copy and VOD transcode
playbacks proves no immutable URI changes bytes and no playlist mutates** —
the affected clients recover through the typed-refusal → reopen path onto a
survivor (review B5).

### M8 — the deletion pass (M, the payoff)

Per-client removal of the live-presentation compensations (§4 lists), the
legacy-shape special cases clients no longer need, and the PLAYBACK.md
rewrite describing the VOD contract as *the* contract with the legacy
shape as the NULL-duration footnote.

**The version floor** (review S1 — v1's opt-in seam stopped composing the
moment the machinery it fell back on was deleted): deletion is gated on the
fleet's server floor being proven at or above the VOD build — every node
checked via `/api/v1/server` and recorded in STATUS.html — and post-M8
clients carry a minimum server protocol: `vod: false` (or a missing
`presentation` acknowledgment) from an older server renders a **typed
"this server needs updating" refusal naming the server build**, never an
undefined-behavior playback attempt. The compact-legacy-arm alternative was
rejected: an indefinitely-kept "small" live arm is how the current
35-mechanism inventory started. NULL-duration files (§2.6) still play
post-M8: the frameworks' *native* live-HLS handling remains — what M8
deletes is the compensation machinery around it — so those titles keep
today's baseline behavior without its bodyguards, an accepted degradation
for a small population that probe-at-import (§2.6's follow-up) shrinks
toward zero.

**Acceptance:** net-negative diff on all three clients with suites green;
PLAYBACK.md's behavior table has no row whose "client must" clause exists
to absorb playlist mutation, prune 404s, reap, or publish-gate semantics;
a client pointed at a pre-VOD server shows the typed version refusal.

## 9. Decisions ledger

Numbered so the review can attack them individually; each carries its why
and the rejected alternative.

1. **Block first — behind one hard deadline — for planned-but-unmade
   segments (D1, revised per review B4; RESOLVED by M0-P3, §12.7).** AVPlayer demonstrably tolerates
   slow bytes and demonstrably distrusts errors and mutation (the whole
   build-63 arc; PLAYBACK.md holds playlist requests open for exactly this
   reason), so blocking stays the primary path — but the wait is bounded by
   `min(server cap, the client's declared budget)` and expiry answers a
   typed, retryable `segment_pending` 503 + `Retry-After`, so every request
   ends through one named path. Rejected: v1's "answer 200 whenever we can"
   escape hatch (an unbounded response the client cannot retry) and
   503-as-primary (spends AVPlayer's error tolerance on the common case).
   M0-P3 sets the numbers and proves each stack's 503 retry behavior.
   **Resolved:** the deadline must sit strictly below the shortest client
   first-byte timeout or expiry never reaches the client — web's is 10 s, so
   `playback.vod_block_secs` defaults to **8** pending the device halves
   (§12.7, §12.9).
2. **Client opt-in via create body, server gate via setting (D2).** The
   rollout needs old-client/new-server and new-client/old-server to both
   keep working with zero double-serving; a per-request declaration is the
   only place that composes. Rejected: server-global flip day one.
3. **Title store = the pretranscode cache, not a third store (D3).** One
   eviction discipline, one reader-guard mechanism, and the copy path
   finally gets cached; a parallel store would re-create the M1c scar
   (two safety mechanisms, both inert). Consequence accepted: cache
   generations learn holes/bitmaps.
4. **Copy plan from a fragment index built by the production-shaped pipe,
   not a container-header probe (D4, revised per review B1; RESOLVED by
   M0-P0, §12.2 — upheld).** Only the
   remuxed fragment stream carries `classify`'s NAL-level clean/dirty
   verdicts and output-shaped byte counts; an ffprobe packet index cannot
   (`fmp4.rs:1556-1610`). The index is video-only so one plan serves every
   audio selection, persisted per file identity, built in the background at
   import; a file with no index keeps the legacy presentation for that
   watch, so no cold play ever waits on indexing. M0-P0 proves fidelity
   before M0-P1 prices it.
5. **Sessions resurrect from persisted recipes rather than never reaping —
   and only from the dormant state (D5, extended per review B6).** Keeps
   memory bounded and the reaper's real job (resource housekeeping) intact
   while deleting its failure mode (killing playback); the
   dormant/terminal lifecycle (§2.5) keeps a deliberately retired handle
   retired against a predecessor's late autonomous fetch. Rejected:
   sessions immortal for title duration (leaks producers under churn) and
   recipe-only resurrection (re-animates what DELETE/supersession/admin
   action/revocation meant to end).
6. **Transcode EXTINFs nominal-2.0 (D6-A) unless M0-P2 objects, then
   segmenter-exact (D6-B). RESOLVED: D6-A, on the web half (§12.6).** A is a no-op to the encode pipeline; B
   unifies both paths through `fmp4::Segmenter` at the cost of touching
   the transcode output path. Measured, not argued.
7. **Rung switching stays session-replacement (D7).** One encode is a
   founding constraint (ADAPTIVE-QUALITY's first paragraph); this plan
   makes the replacement cheap and state-preserving, which is most of what
   seamless switching buys, for none of its cost.
8. **The publish gate is deleted on the VOD path, not retuned (D8).** Its
   entire purpose was keeping players off a live edge that no longer
   exists; the first-segment wait moves into the segment GET where every
   stack already has patience. The 2 s short first segment stays (fast
   first frame is still real).
9. **`AHEAD_HORIZON` and the flow-control budgets keep today's values
   (D9).** This plan changes *what* they govern (demand-driven store
   production), not *how much*; retuning would confound the M7 comparison.
10. **The plan is normative — the producer cuts at planned boundaries and
    never re-decides them (D10, new per review B1).** Prediction required
    reproducing recipe-specific runtime facts upfront, which B1 proved
    impossible from cheap inputs; authority only requires boundaries that
    are clean, policy-respecting, and stable, which the fragment index
    provides. The cost — v2 cuts may differ from what today's
    producer-driven policy would have chosen — is invisible to clients and
    accepted. A planned boundary the materializing pipe cannot honor
    (DTS never matches) is a typed failure plus index invalidation, never
    a silent re-cut.
11. **A VOD rendition is bound to its owner node; takeover never reuses
    its URIs (D11, new per review B5).** Cross-node takeover rebuilds
    recipes with locally valid encoders — protocol-compatible, not
    byte-identical (`CLUSTERING-PLAN.md:322-327`) — which an immutable
    playlist cannot absorb. Owner death = typed refusal → the client's
    reopen builds a fresh rendition on a survivor: a bounded interruption,
    no URI ever naming two byte streams. Rejected: byte-identical
    cross-node production (unenforceable on a mixed fleet, §7 non-goal 4
    of the clustering plan) and post-failure playlist mutation (breaks
    §2.1's founding invariant).
12. **Cache completion requires admission; over-budget titles are
    working-set-only (D12, new per review B3).** *Planned*,
    *materialized-now*, and *durably-admitted* are three facts, not one
    bitmap; completion demands all segments simultaneously present under
    the completed-cache budget, reserved before backfill. An inadmissible
    title is still VOD-presented and honestly re-materialized on a later
    watch. Rejected: bits that survive eviction (publishes directories
    with holes as cache hits) and unbounded backfill.

## 10. Risks, honestly

- **M2 is core-server surgery.** transcode.rs's session/scratch coupling is
  deep; the mitigation is that M2 lands dark behind the legacy surface with
  a byte-identical playback-lab gate, and the setting keeps rollback
  one-flag cheap through M7.
- **Fragment-index cost on NAS.** The index reads the whole file through
  the copy pipe, so a huge DV remux over NFS may take minutes. No first
  play ever waits on it — unindexed files keep the legacy presentation for
  that watch by design (§2.2) — so the real exposure is how long a library
  takes to convert after the flip; M0-P1's numbers size the background
  indexing job, and the M7 comparison week naturally reflects the
  mixed-presentation fleet.
- **EXTINF drift on some stack** → D6-B exists and is bounded.
- **Blocked-fetch semantics on a stack we didn't measure** (older tvOS,
  odd Android forks) → the opt-in flag means an unhappy client build
  simply doesn't send it.
- **Cluster interactions.** The owner-binding decision (§7, D11) settles
  the takeover conflict review B5 found; the residual risk is the store
  fighting the cluster plans somewhere else, which §7's narrowed
  stop-and-flag rule and M2's cluster-aware review pass cover.
- **Scope creep via adjacency.** The ABR estimate problem, multivariant,
  probe-at-import, and transcode.rs decomposition beyond what §3 needs are
  all *named and excluded*; the guardrails section exists because every
  prior plan that touched this area grew.

## 11. What this plan is kept honest by

M0-P0 is a falsifiable proof that runs before any product code; M2/M3's
contract tests are the exhaustive claims (§2.1/§2.3/§2.5) as executable
assertions; playback-lab normalize pins legacy invariance until M7; the M7
week is the empirical verdict, recorded in STATUS.html rather than asserted
here. If the M7 numbers do not show the failure categories gone, M8 does
not run — deletion is earned, not scheduled. The review
([VOD-PRESENTATION-PLAN-REVIEW.md](VOD-PRESENTATION-PLAN-REVIEW.md)) and
its response
([VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md](VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md))
record why v2 says what it says; a future edit that contradicts a resolved
finding should say which one and why.

## 12. M0 results — what the probes measured

**Run:** 2026-08-23, branch `agent/vod-m0`, against `main` @ `912e8c5e` ·
**Harnesses:** [`scripts/vod-plan-probe`](../scripts/vod-plan-probe) (P0, P1),
[`scripts/vod-probe-stub`](../scripts/vod-probe-stub) (P2, P3),
[`scripts/vod-probe-fixtures.sh`](../scripts/vod-probe-fixtures.sh) (the corpus)
· **Raw evidence:** `target/vod-probe/*.json`

**Verdict: P0 clauses (a) and (d) pass; (b) and (c) fail, in two different
ways.** Clause (c)'s failures are numbers — a headroom constant sized from the
argument list instead of from measurement, and a pre-existing property of
`CutPolicy` this plan did not cause. Clause (b)'s failure is structural: §2.2's
media-time addressing rule cannot be implemented as written. **Per §8 M0, M1
does not start.** §12.8 says exactly what a re-review has to settle.

The corpus is built to make wrong answers fail. Every HEVC fixture runs at
24000/1001 fps with `keyint=42`, so one GOP is 1.751750 s — not a whole number
of milliseconds, and not a divisor of the 6 s floor. A GOP that divided the
`-ss` offset would have hidden every timing finding below.

| Fixture | Exists to catch | Fragments |
|---|---|---|
| `closed-gop-2397` | the control: every keyframe an IDR | 52 |
| `open-gop-2397` | CRA with leading pictures — the disc-remux shape | 52 |
| `clean-cra-2397` | CRA without leading pictures — the case a lazy classifier calls dirty | 52 |
| `h264-2997` | the other branch of `classify`, at 30000/1001 fps | 60 |
| `mixed-2397` | clean and dirty interleaved — the only shape on which cut ordering is falsifiable | 52 |
| `audiotail-2397` | 25 s of audio past the video — the `c58a4307` case | 18 |
| `dense-2397` | 143 Mb/s, every fragment clean — clean cut vs byte ceiling | 23 |
| `dense-open-2397` | 143 Mb/s, every fragment dirty — the `ByteCeiling` branch | 18 |
| `surround-2397` | AC-3 5.1, so the `-b:a 320k` + `-channel_layout:a 5.1` branch | 35 |

### 12.1 P0(a) and P0(d) — pass, 9/9

Two runs of the production-shaped **video-only** copy pipe over each fixture
produced byte-identical fragment streams — the same first-sample DTS, sample
durations, output byte counts, and `classify` verdicts in the same order — and
the same `init.mp4` SHA-256. Both runs are verified complete before the clause
is scored: the child's exit status, a clean box walk, and fragment coverage
against the probed duration. An incomplete read is not a determinism result,
because two identically truncated runs agree perfectly and mean nothing.

`init.mp4` was byte-identical across process generations on every fixture,
including generations started with `-noaccurate_seek -ss` in the middle of the
film. That is the precondition §2.2 needs before an evicted segment URI may be
re-materialized by a later generation, and it holds.

### 12.2 The index describes the production pipe's fragments exactly — D4 stands

This is the claim review finding B1 forced into M0, and it is the one that
survives cleanly. On **9 of 9** fixtures the video-only index and the full
production pipe agree on:

- fragment count;
- the `classify` verdict of every fragment;
- every fragment's duration in ticks, after the first;
- every fragment's output video byte count.

So a plan built from the video-only index cuts at boundaries the production
pipe really emits, for any audio selection. D4 is upheld on the evidence, not
on the argument.

### 12.3 P0(b) — fail: the plan's boundaries are real, its film-time addressing is not

Three measurements, each of which independently breaks a sentence in §2.2.

**The index and the production pipe do not share a zero.** The production
pipe's video DTS grid sits a *constant* offset ahead of the video-only index's,
and the offset depends on the audio branch: **0 ticks** with no audio delay,
**+6** on the open-GOP fixtures, **+678** on `clean-cra-2397` — 678 ticks at a
16000 timescale is 42.4 ms, the AAC encoder's own delay. `-avoid_negative_ts
make_zero` computes its shift from the earliest timestamp across *all* output
streams, so adding audio moves the video timeline. §2.2's "video-only makes the
index valid for every audio selection" is true of the fragment **sequence**
(§12.2) and false of the **timeline**. Consequence: a plan whose entries are
keyed by index DTS addresses nothing in the stream that materializes it. With
the constant offset applied, all 9 fixtures resolve every boundary and every
clean-origin boundary is clean.

**A repositioned producer's DTS carries no film time at all.** §2.2: "the
segmenter discards fragments until the first whose DTS equals the boundary; a
mismatch (never-equal) is a typed producer failure." Across **43 sampled
boundaries on 9 fixtures the raw DTS equalled the boundary 3 times**, and all
three were coincidences on one fixture whose seeked grid happens to contain the
same tick values. Seeking to 3.503 s and to 10.510 s produce the *same* emitted
DTS sequence, because `make_zero` rebases every generation to zero. `-copyts`
does not change this in any placement tested, with or without
`-avoid_negative_ts disabled`. Written literally, the rule fires a typed
producer failure and an index invalidation on every reposition there will ever
be.

**`-ss` lands early, always, and by an amount nothing predicts from the
boundary.** With the `{:.3}` string the producer formats
(`transcode/mod.rs:1165-1174`), `-noaccurate_seek -ss` landed at least one
fragment before the target in **43 of 43** cases. A boundary at 3.5035 s is not
expressible in milliseconds, and on Matroska the demuxer lands at the enclosing
cluster besides. So the discard step is mandatory, not an edge case, and its
length is not derivable from the requested time.

**What does work, measured.** Two candidate mechanisms were scored against the
same 43 cases:

| Mechanism | Resolves the landing | Notes |
|---|---|---|
| `keyframe_probe_args` origin + emitted DTS | exact 20/43 · within one frame 34/43 · worst error 1014 ticks (63 ms) | agrees with the true landing on 28/43; it filters packets to `≥ start`, so on a boundary that *is* a keyframe timestamp it reports the keyframe the demuxer did not land on |
| first fragment's output byte count matched against the index | **unique 43/43** | needs no timestamp; the index already stores the number, and video samples are copied so the count is identical |

And from the byte-resolved landing, the repositioned generation reproduced the
t=0 fragment sequence in **43 of 43** cases. So repositioning is sound; only
§2.2's stated way of recognising the boundary is not.

### 12.4 P0(c) — fail: one constant is wrong, one ceiling was never a ceiling

**`bound_holds` — 8 of 9.** Planned `est_bytes` plus a headroom of
`nominal_bitrate × duration` bounds the materialized segment everywhere except
`surround-2397`, where ffmpeg's AAC encoder overshoots its `-b:a 320k` target
by **1.2235×**. The nominal bitrate in the argument list is a request, not a
ceiling. **M1's headroom constant has to be measured** — either a margin of at
least 1.25 over nominal, or the probe's observed rate per file. The
trailing-audio fixture measured an overshoot of 2.95 inside its tail group,
which the group bound still absorbs.

**`ceiling_respected` — 7 of 9, and not this plan's doing.** Segments on the
143 Mb/s fixtures reach **125.2 MB** against `COPY_SEGMENT_MAX_BYTES` of 64 MiB
(5 segments on `dense-2397`, 4 on `dense-open-2397`). The mechanism is in the
shipped policy: `CutPolicy::cut_before` returns `None` until the floor is
passed (`fmp4.rs:1774-1776`), so the ceilings cannot bind inside the first 6 s
of a segment, and 6 s at 143 Mb/s is already 107 MB. Today's producer does the
same on the same file. It reaches this plan only because §2.1 promises an
upfront `#EXT-X-TARGETDURATION` and the web player mirrors a 64 MB expectation
(`index.html:6849-6850`) — so the plan should say what it does with a segment
the policy cannot bound, rather than assume the ceiling holds.

**The audio tail changes `TARGETDURATION`, and only a fixture with one shows
it.** `audiotail-2397` plans 4 video entries plus **2 audio-tail entries
covering 24.991 s**, and its honest `TARGETDURATION` is **15**, not the 8 a
video-only plan would emit. An understated `EXT-X-TARGETDURATION` is a spec
violation players act on. §2.2 already requires the tail be computed from the
probe's per-track durations; this is the number that says it matters.

### 12.5 P1 — index cost

In the sandbox, on local disk, the video-only index builds at **40–549 MB of
source per second** (0.067 s for a 2.7 MB fixture, 1.301 s for 714.6 MB). The
throughput rises with bitrate because the cost is dominated by remuxing
fragments, not by fragment count.

**That number does not answer P1.** P1 asks what indexing costs on nynuc over
NFS with a cold cache on real titles, which is the number that sizes the
background job, and no such file is reachable from here. The sweep is an
operator protocol — see [STATUS.html](STATUS.html)'s VOD operator checklist.
D4's other half already protects the user experience whatever the number says:
a file with no index keeps the legacy presentation for that watch, so no cold
play ever waits on indexing.

### 12.6 P2 — EXTINF drift: D6-A, on the web half

A VOD playlist declaring flat 2.000 s over media whose real segments deviate by
up to 2 frames (`max |EXTINF − 2.0| = 0.084 s`, worst accumulated divergence
**+0.583 s** at segment 27) plays through hls.js 1.6.16 to `ended` with **one**
buffered `TimeRange`, **zero** fatal errors, and every seek landing within one
frame. So does a fixture with **11× the drift** (0.250 s per segment, **+6.500
s** accumulated).

The mechanism is visible in the instrumented run and is why the result is
robust rather than lucky: **hls.js overwrites `frag.start` with measured PTS
when the fragment parses.** On the amplified fixture a fragment the playlist
places at 50.000 s is repositioned to **56.249 s** at `FRAG_BUFFERED`. The
EXTINF column is a lookup hint on this stack, not a timeline. `video.duration`
ends at the media's 60.021 s, not the playlist's 60.008 s.

**D6-A holds for the web player.** Its residual cost is that
`duration_at_loadedmetadata` is the EXTINF sum, so the scrubber is wrong by the
accumulated divergence until the first fragment parses. AVPlayer and Media3 are
not covered — see §12.7.

### 12.7 P3 — blocked fetches: the deadline, not the block, is the constraint

The first measurement of this probe modelled a server with no deadline — the
one §2.3 explicitly deletes — and concluded that stock hls.js tolerates ~50 s
of block and dies at 60 s. Composed the way §2.3 actually specifies (a hard
per-request deadline answering a typed `segment_pending` 503 with
`Retry-After`, over a producer that materializes when it materializes), the
answer inverts. Four server models × four block lengths, **stock hls.js
defaults**:

| Server model | 5 s block | 15 s | 30 s | 60 s |
|---|---|---|---|---|
| **hard deadline 8 s → 503** (§2.3) | end, 1 attempt | end, 2 attempts, 1×503 | end, 4 attempts, 3×503 | **end, 6 attempts, 5×503, zero fatal** |
| hard deadline 15 s → 503 | end | end, 1 abort | end, 2 aborts | **FATAL** `fragLoadTimeOut`, 5 aborts, **0×503** |
| no deadline (v1's reading) | end | end, 1 abort | end, 2 aborts | **FATAL**, 5 aborts |
| per-request delay | end | **FATAL** | **FATAL** | **FATAL** |

**hls.js aborts a blocked fetch at its own `maxTimeToFirstByteMs` of 10 000 ms,
then retries 4 times with `retryDelayMs: 0`.** A server deadline *above* that is
worse than useless: at 15 s the client aborts five times and the server never
gets to write the typed refusal the whole mechanism exists to deliver — 0 of 5
attempts saw a 503. Below it, arbitrarily long real blocks survive on
503-and-retry with no client configuration change at all.

Two further numbers, both from hls.js's own source as well as from the runs:

- **`Retry-After` is ignored.** The header appears once in the vendored
  bundle, in the content-steering controller, gated on HTTP 429. Fragment
  retries back off on hls.js's own ladder — measured **1083 / 2100 / 4100 /
  8100 / 8101 / 8099 ms**, identical whether the server sends `Retry-After: 1`
  or `Retry-After: 3`. The server cannot steer web retry cadence; it can only
  bound its own deadline.
- **~31.6 s is the ceiling on continuous `segment_pending`.** hls.js gives 7
  attempts (1 + `errorRetry.maxNumRetry: 6`) and then a fatal `fragLoadError`.
  A producer that never materializes must fail typed before that, not after.

Raising `fragLoadingTimeOut` to 65 000 also survives every block length on one
attempt, but it is the worse answer twice over: it costs a client release, and
it means a genuinely dead origin goes unnoticed for 65 s instead of 10 s. It
also silently drops `maxLoadTimeMs` from 120 000 to 65 000 through hls.js's
deprecated-config shim, so a client that wants it must set
`fragLoadPolicy.default` explicitly.

### 12.8 What M0 did not cover

Named so nobody reads a green run as coverage it is not:

- **Every device half.** P2 and P3 speak for hls.js only. AVPlayer and Media3
  have their own first-byte timeouts and their own 503 handling, and D1's
  number is the **minimum across all three**. Protocols are in STATUS.html.
- **P1 on real titles** (§12.5).
- **P0 on ≥10 representative real files.** §8 M0-P0 asks for the fixture corpus
  *and* real files; only the corpus ran here. The fixtures were built to be
  adversarial rather than representative, and a real 4K DV remux over NFS can
  still surprise the index.
- **Dolby Vision preservation.** `-tag:v dvh1`, `-strict unofficial`, and the
  Profile-5 branch that omits `-bsf:v` entirely change the copied NAL stream
  and therefore every byte count in §12.4. No DV source was reachable.
- **A/V-corrected sources.** With `audio_offset_ms != 0` on the copy-audio
  branch, production builds a *second* `-itsoffset` input
  (`transcode/mod.rs:1220-1234`); two demuxers interleaving into one muxer is a
  different fragment stream, and it was not measured.
- **Producer pacing.** `-readrate`/`-readrate_initial_burst` rate-limit the
  input and cannot change output bytes, but the probe's argv differs from the
  daemon's logged one.

### 12.9 Ledger outcomes, and the one thing that must be re-decided

**D1 — resolved; shape unchanged, numbers set.** Blocking behind one hard
deadline is right, and §12.7 shows it works on stock clients. The number is not
free: `playback.vod_block_secs` must be **strictly below the shortest client
first-byte timeout**, or expiry never reaches the client. Web's is 10 s, so the
default becomes **8**, not the placeholder 15 — pending the device halves,
which can only lower it. Each client's `block_budget_secs` declaration stays as
designed, and no client needs a raised fragment timeout for the server to work.

**D4 — resolved; upheld.** The fragment index built by the production-shaped
video-only copy pipe is deterministic (§12.1) and describes the production
fragment stream exactly (§12.2). Its *timeline* does not transfer, which is
§2.2's problem, not D4's.

**D6 — resolved: D6-A** on the web half (§12.6), pending the device halves.
Routing the encode through `fmp4::Segmenter` (D6-B) buys nothing hls.js needs.

**§2.2 — stop-and-flag, not resolved.** Three sentences in a normative section
are contradicted by §12.3, and amending §2.2 is not one of the three decisions
M0 may change (§8 M0). They are:

1. "the segmenter discards fragments until the first whose DTS equals the
   boundary" — no repositioned generation emits film-time DTS;
2. "a mismatch (never-equal) is a typed producer failure and an index
   invalidation" — as written this fires on every reposition;
3. "Video-only makes the index (and therefore the plan) valid for every audio
   selection" — true of the sequence, false of the timeline.

The measurements point at a replacement — identify the landing fragment by its
output byte count against the index (43/43 unique), then count planned
boundaries forward from there, and let the segmenter rebase `tfdt` to the plan's
`start_ticks` as §2.2 already requires — but choosing it is a review decision.
A revision should also say what the plan does with a segment `CutPolicy` cannot
hold under the byte ceiling (§12.4), and should replace the nominal audio
headroom with a measured one.
