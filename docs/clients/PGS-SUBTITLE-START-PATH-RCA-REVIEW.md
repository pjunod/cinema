# Review — PGS subtitles on the start path (RCA and plan)

**Status:** adversarial review complete — verdict APPROVE WITH CHANGES; the
rulings it asks Paul for are in §4, and the document it reviews stays `open`.

**Reviewing:** `docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md` as committed in
`8b92b958` · **Evidence base:** `origin/main` = `5c605768` (the merge of PR #445,
2026-09-23 00:48 UTC), a fresh Forgejo clone, PR #444/#445 via the API, and
`/api/v1/server` on m6, nynuc, nuc4 · **Reviewer:** Fable 5.1, adversarial ·
**Written:** 2026-09-22

**Verdict: APPROVE WITH CHANGES.** §2 and §4 are right, every anchor I could
re-derive resolves, and the arithmetic holds. The changes are in §5 and §6, which
is where the doc asked for them: §5.3's "code-provable regression" is not one
(the web ignores the server's policy pick and burns only what the viewer chose,
gate on or off), so B1's justification must change even though B1 stays; and §6's
open question has a definite answer — the fragment-index pass is one sequential
ffmpeg demux of the whole container, so every PGS track of a file can come out of
that pass at zero added I/O, which dissolves §6.4's "ten reads" premise. Rulings
Paul owes are in §4.

---

## 1. What was confirmed at `5c605768` (do not re-derive)

Diagnosis (§2):

- `START_DEADLINE = 50 s` — `media_sessions.rs:54`. `EXTRACTION_TIMEOUT = 600 s` —
  `subtitles.rs:35`. `CLUSTER_REPLACEMENT_GATE_WAIT = 3 s` — `transcode.rs:129`;
  `CLUSTER_REPLACEMENT_HOLD_CEILING = 120 s` — `transcode.rs:79`.
- The burn extraction is `-copyts -start_at_zero … -c copy -f matroska` with no
  `-ss` (`subtitles.rs:1069-1070`; the only `-ss` in the module is the windowed
  VTT path at `:1499`). The cache name is `f{id}-s{index}-{sha256(object_version)}-burn-v2.mks`
  (`:1043`), so the `<sha>` in §2.2 is the object-version digest, not a content hash.
- The placement-deadline arm answers a bare `ApiError::ServiceUnavailable("local
  media worker exceeded the placement deadline")` with no log line
  (`hls.rs:2479-2481`) — the §2.1 "asymmetry in the log is the tell" reading is right.
- `PlaybackIntent.playbackId = UUID.randomUUID()` — `PlaybackIntent.kt:16`. Attempt 3
  did get a fresh gate key.
- 79,519,453,096 B ÷ 402.639 s = 197.5 MB/s. The number is right.
- Attempt 2's 3,883 ms is the 3 s gate wait plus overhead — consistent with #437's RCA.

Fix A (§4) — **merged, not "open"**:

- `ExtractionLimits.join_budget` defaults to `SIDECAR_JOIN_UNBOUNDED` = 660 s
  (`subtitles.rs:74-75`, past the 600 s extraction timeout); `SIDECAR_JOIN_BUDGET
  = 5 s` (`:68`); `join_flight` is now `timeout(budget, join_flight_unbounded)`
  (`:1300-1319`). The one short-budget caller is `transcode.rs:19854`, inside
  `prepare_vod_encoding`. `session_start_error` maps `is_sidecar_pending_error`
  to `TypedRetry { 503, "startup_timeout", retry_after_seconds: 5 }`
  (`hls.rs:81`, `:3933-3939`), and the test at `:25453-25478` pins it.
- The ladder is rung-bounded on all three clients: web `playback-policy.js:871/:904`,
  Apple `PlayerController.swift:1550/:1590`, Android `PlaybackPolicy.kt:135/:183`.
  `createRetryStep` reads `attempt`, `elapsedMs`, `source` and nothing else; the
  only `retryAfter` consumers in the web tree are `prepared-switch-measurement.js:323`
  and `playback-control.js:61`, neither on the create path. The ~27 s figure is an
  upper bound (4 × ≤5 s + 7 s) and stands.

Fix B evidence (§5):

- Gate default `false`, read per request, store error propagated (`state.rs:750-766`
  — the "off is the expensive default" comment is at `:754-759` as quoted). Setting key
  `subtitles.pgs_overlay` at `store/mod.rs:1794`. Developer item id `pgs_overlay`,
  title "Serve PGS subtitles as an overlay" (`developer.rs:558-603`).
- Overlay demux is `-map 0:s:{index} -c:s copy -f sup -fs 256 MiB` with no `-ss`
  (`pgs_overlay.rs:505-527`). `prepare_with` returns `PrepareState::Preparing`
  and spawns (`:238`, `:249`, `:309`); `PREPARE_TIMEOUT = 600 s`, negative TTL
  120 s, capacity semaphore of **2** per process (`:29-35`, `:126-128`).
- Cache bounds exactly as stated (`:32-35`).
- **Both native clients poll for up to 600 s** — `PGSOverlay.swift:160
  maximumPrepareSeconds = 600`, `PGSOverlay.kt:138 maximumPrepareMs = 10 min` —
  and the server's `retry_after_ms` is 1000 (`http/pgs_overlay.rs:187`). So a
  402 s prepare lands inside every budget; §5.1's "subtitles would have appeared
  six minutes later" is true for the native clients, not asserted.
- `dto.rs:485-497` hardcodes `deliverable_as_default(…, false, false)` with the
  quoted comment. B2 is real.
- `PLAYBACK.md:582-592` says "it does not select an overlay automatically";
  `stream.rs:2199-2211` passes `pgs_overlay` into `deliverable_as_default`
  (`tracks.rs:235-244`: `overlay_enabled && is_pgs_subtitle(codec)` is a
  default-eligible arm). B5's contradiction is real. `APPLE-PGS-OVERLAY-ACCEPTANCE.md:50-54`
  still shows `PLURX_PGS_OVERLAY=1` as the way to enable it; `PLAYBACK.md:586`
  says the env var only seeds the setting once.
- B3: `PGSOverlayTest.kt` has no seek; the Apple PGS tests
  (`AppleClientTests.swift:7951-7985`, `:10679`) cover track matching, PiP,
  menu copy and manifest validity. Neither drives `reconcile`
  (`AndroidPGSOverlay.kt:113`) or `refreshPGSOverlayWindow` — which is at
  `PlayerController.swift:4881`, not `:3440` as the doc says.
- The web player has no PGS renderer: no file under `web/player/` references
  PGS beyond the `decode-tiers.js:955` comment and the stats counters.

§8 constraint 2: `main` at `5c605768` carries Apple **175** / Android **116**;
PR #444's branch (`fix/playback-wait-copy`, now merged) carries 176 / 117. B goes
to ≥ 177 / 118.

Not re-verified: the §2.1 log lines (container recreated, as the doc says), the
windowed-extraction 7 % / 57 % / 101 % curve attributed to a test comment, the
four transcode reasons in §2.7, and whether `5c605768` is on any node —
`/api/v1/server` answers only `0.3.0` on m6, nynuc and nuc4; `git describe` on the
node is the only way to know.

---

## 2. Findings

### F1 — Status lines are stale (editorial, fix before the doc is cited)

Header says "§4 merged-pending"; §4 says "PR #445, open, fast lane in progress".
PR #445 merged as `5c605768` at 00:48 UTC on 2026-09-23. Both should read
"merged, not deployed". The §2.5 stack should also name the post-fix shape:
`join_flight` is the bounded wrapper, `join_flight_unbounded` the loop.

### F2 — §5.3's regression claim is wrong in mechanism; B1 survives on a different argument (**blocker for the doc, not for the work**)

The doc says turning the gate on "silently downgrades every web direct-play to a
full transcode" because `/decision` auto-selects a PGS track for every client.
Three things in the tree say otherwise:

1. **The web does not act on the server's policy pick.** `preplay-selection.js:135-141`:
   "`subtitle` is null unless the viewer chose one explicitly. The server echoes
   its OWN policy subtitle in `selection` for an audio-only request, and reading
   that echo as a choice would burn a track nobody asked for into a film that was
   direct-playing." `prePlayBurnNeeded` runs only on a viewer choice.
2. **The +400 ms in-player auto-apply keys on the container's `default` flag, not
   the policy.** `decode-tiers.js:938-943` picks `PLAYER.subs.find(s=>s.default)`;
   `sub_tracks` (`stream.rs:895-911`) copies `default: s.default` from the stream.
   The gate changes neither input, and the auto-apply is skipped on a transcode
   decision anyway. (The `web-policy.test.js:2697` pin is about a bitmap default
   the server no longer sends — the container flag path for a *forced* PGS track
   on an SDR remux is unchanged by this arc and by the gate.)
3. **An explicit `subtitle_burn` on create is honoured regardless of the gate**
   (`hls.rs:623`, `:1390`, `:1890-1898`). A viewer-chosen PGS track on web burns
   with the gate on exactly as it burns today.

So enabling the gate is a **no-op for the web player** — no new transcodes, no
lost subtitles. What the gate does change for web is honesty, not routing: for a
viewer-chosen PGS track `/decision` answers `subtitle_requires_burn_in=false`
(`stream.rs:963`) and `overlay:"pgs-v1"` (`:908`), keeps the remux verdict, and
the web overrides it locally at `decode-tiers.js:955-962` because it knows better.
The server is issuing a delivery plan the client cannot execute, and the client is
quietly correcting it. **That** is the argument for B1: per-client capability
makes the server's plan true for the client that asked. B1 stays; §5.3 should be
rewritten to say this, and the words "code-provable regression" should go.

Consequence for scope: B1 is no longer the gate's *blocker*. With F2 corrected,
nothing in the tree prevents flipping `subtitles.pgs_overlay` on for the native
clients today except the acceptance evidence in §5.5/§5.6 — which matches Paul's
stated position that a matrix is not a bar.

### F3 — §6.2's open question: yes, the index pass reads the whole file, and it can carry every PGS track for free

`fragindex.rs:1-12`: "one ffmpeg child, one pass over the file … the same
`FragmentReader` over the same production-shaped pipe". The production
command is built by `copy_index_pipe_args` (`plurx-core/src/transcode/mod.rs:2139-2160`):
`-map_chapters -1 -map 0:v:0? -an -sn -c:v copy … -f mp4 pipe:1`, spawned at
`fragindex.rs:1664` (the test twin at `:2798-2816` has the same shape).
ffmpeg's Matroska demuxer reads clusters sequentially and discards unmapped
blocks, so all 79.5 GB go through the process whether or not audio and subtitles
are mapped. Since PR #877 (`d0e5c630`) the source attestation is a 64 × 1 MiB
sampled read, so there is no *other* full pass to attach to — the index pass is
the one.

ffmpeg accepts several outputs in one invocation. Appending
`-map 0:s:0 -c:s copy -f sup -fs <cap> <dir>/s0.sup -map 0:s:1 … s1.sup …` for
every PGS track produces all of them from the same read. Output bytes are the
track sizes (18 KB to ~11 MB), not the film. **§6.4's "a 10-track disc is ten
79.5 GB reads" is therefore false under ride-along** — it is one read for all
ten, and the scope question collapses to: build **every PGS track** of a file
whenever its index is built.

Constraints that materially shape the design (all in the tree today):

- **The index build is cancelled by any playback admission** —
  `wait_for_cluster_fragment_index_stop` (`state.rs:7940`) and the `select!`
  that keeps nothing partial (STATUS.md, "Source attestation was hashing whole
  films"). A ride-along `.sup` dies with it and restarts from zero next time. So
  ride-along is the *eager* producer only; the on-demand producers
  (`ensure_burn_file`, `pgs_overlay::prepare`) stay, and on a busy node the
  eager artifact may never land. Do not make the on-demand path wait on the queue.
- **The index budget is `film_secs / 8 + 30 s`, clamped to [90 s, 30 min]**
  (`state.rs:1791-1806`). File 5208 gets 900 s; 402 s fits. The subtitle outputs
  add no read time and must not add a timeout of their own.
- **Each voter targets itself** (from the queue-repair diagnosis: a healthy queue
  builds every file up to 4×). That is what a node-local sidecar cache wants —
  each node ends up with its own `.sup` — but it means the artifact is per node,
  not replicated. Do not put it in the store.
- **Index identity includes the DV pass** (`DolbyVisionPass` in `fragindex.rs`).
  A converting re-index would re-extract subtitles needlessly; key the sidecar on
  `(file_id, track_index, object_version)` and skip the `-map` when it exists.
- **Derive the burn `.mks` from the `.sup`**, not the other way round:
  `ffmpeg -i track.sup -c copy -f matroska` is a remux of kilobytes. The one thing
  to prove at build time is timeline equivalence — `-copyts -start_at_zero` on the
  direct path versus a `.sup` whose PTS are absolute 90 kHz — with a fixture
  assertion that the derived `.mks` cue times match a direct extraction's. If they
  do not, the ride-along should emit the `.mks` as a third output instead of
  deriving it.

### F4 — §6.3 is right; the durable home already exists

The overlay LRU (`pgs_overlay.rs:32-35`, `prune()` on exit paths) must not be the
queue's target. The burn sidecar directory (`/srv/plurx/cache/subs/`) already
outlives sessions — the 18 KB file from 5208 was still there after 23:01 — so put
the ride-along `.sup` beside the `.mks` under the same object-version-keyed name,
and make `pgs_overlay::prepare` *hydrate* its generation directory from there
(copy megabytes) before it considers a demux (read gigabytes). The LRU keeps
pruning decoded PNG generations; the durable `.sup` is its floor.

### F5 — §4.3's interim surface lies, and Retry re-runs the ladder

After `exhausted("ladder_spent")` the surface reads "Playback is stalled" for a
file that is merely being read; a viewer who presses Retry gets a fresh 27 s
ladder, so the honest description of the interim is "Retry every half minute for
up to seven minutes". Acceptable as an interim (ruling R4 below), but the doc
should say it plainly, and if PR #444's wait copy has a "still preparing" line it
belongs here.

### F6 — Two things §5.1 should add

- With the gate on, file 5208 **still transcodes** (§2.7's four reasons). The
  overlay removes the burn and the sidecar wait from the start path; it does not
  make the title a remux. The sentence "subtitles stop forcing a transcode at all"
  is true of the subtitle's contribution only.
- The prepare semaphore is **2 per process** (`pgs_overlay.rs:128`). Two viewers
  opening cold PGS titles on one node saturate it; a third prepare is refused or
  queued behind two 400 s demuxes (which of the two, I did not trace). Fix C is
  what makes that bound irrelevant; until then it is the overlay's own version of
  the wait this doc is about.

### F7 — Minor corrections

- `refreshPGSOverlayWindow` is at `PlayerController.swift:4881` (doc: `:3440`).
- `createRetryStep` anchors: web `:871` for the ladder, `:904` for `ladder_spent`
  (doc: `:868`); Apple `:1550/:1590` (doc: `:1546`).
- §2.2's `<sha>` is `sha256(object_version)`, worth saying since Fix C keys on it.
- §8 item 2: main is 175/116, #444 is 176/117 and merged; B is ≥ 177/118.

---

## 3. Answers to §9

**Q1 — B1 or a web renderer?** B1, and it is not either/or. Capability
negotiation is needed *regardless* of a web renderer: client versions skew (the
physical Apple TV ran a pre-guard build for weeks), a node-wide switch describes
the server and not the caller, and the server today issues plans a client cannot
execute (F2). A web renderer is an M2/M3-sized milestone — canvas compositing over
both hls.js and Safari-native HLS, PiP and fullscreen semantics — and when it
lands it flips one capability bit. Build B1 now; a web renderer is its own
milestone later, if ever. Two things to fold into B1's shape: the web's local
override at `decode-tiers.js:955-962` becomes dead once the server answers
`subtitle_requires_burn_in=true` for a client that cannot overlay — remove it and
pin the server behaviour in `web-policy.test.js` instead; and `deliverable_as_default`
should take `client_can_overlay` (gate AND capability), not the gate alone.

**Q2 — does the index pass read the whole file?** Yes (F3). Fix C attaches to it,
and the cost argument is not "close to free" but exactly free on the read side.

**Q3 — which tracks?** All PGS tracks of a file, at index time — the per-track
cost is the track's own bytes (F3). The scope question only survives for
**backfill** of files already indexed before this ships, where it *is* a second
full read per file: there, default/forced tracks only, lowest queue priority,
cancellable by playback like the index itself, and only for files whose PGS
tracks the library's language preferences would ever pick. The on-demand
producers remain the answer for everything else.

**Q4 — `exhausted` at ~27 s as interim?** Yes, on the understanding in F5, and
no to the "preparing surface that outlasts the ladder" alternative — it is a
fixture change on three clients for a state that B and C remove for PGS. There
is one alternative worth Paul's eye because it fixes the *web* too, which B does
not: start the session **without** the burn when the sidecar is cold, answer 200
with a `subtitle_pending` field, and deliver the subtitle as a prepared
replacement when the sidecar lands (the M6 handoff machinery is merged and is
exactly a second session start, seamlessly committed). It is the burn path
borrowing the overlay's asynchrony. Same size as B1 (a wire field, three
clients), and it is the only design in which a web viewer choosing a PGS track on
a cold 80 GB file sees the film start. Not a blocker; a ruling.

**Q5 — do the Partial M0 items close empirically?** Yes. Items 1 and 4
(`PGS-OVERLAY-M0-FEASIBILITY.md:408`, `:411`) share one evidence gap stated at
`:415-418`: "a controlled, sanitized production corpus must exercise authoring
patterns that cannot be inferred from the generated fixture. Record that result."
Demuxed real-library `.sup` extracts *are* that corpus. Make it a script — run
the parser over a directory of `.sup` files and print files parsed, cue count,
every `Malformed`, and specifically whether the `0xC0` epoch-continue PCS state
(item 4's open question) occurred — and commit the output as the record. Fix C's
ride-along produces the corpus as a by-product on every node. Note also `:345`
and `:353`: the only prior production cold-demux attempt (file 5559) was stopped
at 179 s incomplete, so the 5208 run — 402,639 ms, 18,866 bytes, forced track —
is the first completed production measurement and should be recorded there.

**Q6 — is the narrower proof bar defensible?** Yes, with two edits. The stated
reason the gate is off is *HDR/DV* acceptance, so the one title per native client
must be a **DV or HDR10 title** composited over an HDR surface (legibility and
colour-matrix were M0-measured items), not any PGS title; and the seek should be
both directions across a cue boundary, since `reconcile`/`refreshPGSOverlayWindow`
are the untested paths (B3). For **web** there is nothing to prove on hardware —
it has no renderer; the web check is a test, not a session: with the gate on, a
viewer-chosen PGS track still burns and a forced container default still applies
(F2). So the bar is two devices, one DV title each, one seek each way, plus B3's
automated tests. That covers the three ways a bitmap overlay can be wrong.

**Q7 — anything wrong in §2?** No. F1's stale status and the §2.5 naming are the
only edits. Every re-derivable anchor in §2 resolves at `5c605768`.

---

## 4. Rulings for Paul

- **R1** — Rewrite §5.3 per F2 and drop "code-provable regression" (my pick:
  yes; it is wrong as written and it changes what blocks enablement).
- **R2** — Fix C as ride-along on the index pass, all PGS tracks, durable home
  beside the burn sidecars, on-demand producers unchanged (F3/F4). My pick: yes.
- **R3** — Backfill policy for already-indexed files: default/forced only, lowest
  priority, playback-cancellable (Q3). My pick: yes, and gated behind the same
  Developer enable block as the overlay so it is attributable and stoppable.
- **R4** — Accept `exhausted` at ~27 s as the interim (Q4). My pick: yes.
- **R5** — The `subtitle_pending` / start-without-burn alternative (Q4): build it
  or not? My pick: **not now** — B + C remove the case for every native client,
  and the remaining web case is a viewer choosing a bitmap track on a cold 80 GB
  file. Revisit if that shows up in a report.
- **R6** — Enable order: B1 + B2 + B5 first (server + caps, no hardware), flip the
  gate on with the F2 correction in hand, then the Q6 two-device check, then C.
  My pick: yes — C is an optimisation once the play path no longer waits.

---

## 5. Notes for whoever builds B and C

- Fix A's 5 s join still runs **under** the replacement gate (`transcode.rs:19854`
  is inside `create_session_inner`). With #437's abandon-marking that is fine; it
  is 5 s of hold per attempt. Do not move the burn ahead of the gate as part of B
  or C — that was #437's "still open" list and it is answered by bounding.
- The `startup_timeout` message text reaching the client is the
  `SIDECAR_PENDING_PREFIX` sentence; the fixture's `preparing` class shows its own
  copy, so the sentence is log-only. Keep it that way.
- The `.sup` ride-along must honour `-fs` per output (256 MiB, `MAX_TRACK_BYTES`)
  and inherit the index's cancellation; a partial `.sup` is not a sidecar, for the
  same reason a partial index is not an index.
- The M0 corpus script (Q5) and the Q6 check are hardware/fleet work a GPT session
  can run; the prompt for it is short: demux every PGS track of N library files
  on one node into `.sup`, run the parser, report the four numbers.
