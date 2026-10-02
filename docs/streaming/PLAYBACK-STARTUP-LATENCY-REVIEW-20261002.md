# reference film / file 120 on lab6 fast-start proposal — review

**Status:** review delivered · **Verdict:** diagnosis APPROVED; proposal
CHANGES REQUESTED · **Written:** 2026-10-02 EDT · **Reviewed at:**
`189286828` (= deployed build `v0.3.0-5385-g189286828` = `origin/main`,
zero commits apart at review time)

Review of *"reference film / file 120 on lab6 — root cause and proposed fast-start fix"*. Source was
read from my own blobless clone at the deployed commit; the lab6 database was
read from a copy (deleted afterwards) and the lab6 daemon log was grepped. No
setting, restart, requeue, branch or push was made.

## 1. Verdict — the diagnosis is right; the proposal is a third plan for a solved-on-paper problem

Every source claim in §3 checks out at the pinned commit, and the timeline in
§2.1 matches the lab6 log line for line. The failure chain is correct: media
existed at 2.25 s, the served snapshot was withheld until 56.432 s of media at
15.11 s, the native master spent its 5 s budget waiting for an init that is
fenced behind that snapshot, and Safari's error 4 spent the codec rescue on a
route with no encode candidate.

What the proposal gets wrong is not the mechanism but where it sits:

1. **This is a repeat of an incident that already has an approved, unbuilt
   fix** (B1).
2. **Its server half restates an authorized, undispatched effort and
   contradicts two of that effort's rulings** (B2).
3. **The 4 s number cannot be met by any change the doc scopes for
   ordinary HLS, and the doc stops one step short of saying why** (B3).
4. **The "why is the index missing" question has a fleet-wide answer in the
   lab6 database** (B4).
5. **The incident ran nine seconds after a daemon restart** (B5), which the
   doc never mentions.

Fold it into the two existing contracts as an amendment. Don't land it as a
standalone plan.

## 2. What I verified

| Claim | Verdict | Anchor at `189286828` |
|---|---|---|
| Writer gate 12 s; playlist withheld until crossed | ✔ | `plurx-core/src/transcode/mod.rs:258`, `copyseg.rs` `write_segment` |
| `ROLLING_INITIAL_RUNWAY_MS` = 16 × 3 × 1000 | ✔ | `rolling/publication.rs:111` |
| No snapshot ⇒ publish only at EOF or `end_ms >= desired_end_ms` | ✔ | `rolling/session.rs` `publication_cycle_at`, the `None =>` arm |
| Init withheld until a current served snapshot exists | ✔ | `manager/publication.rs` ~1938 |
| Master init inspection runs under the 5 s response budget | ✔ | `http/hls/playlist.rs:148` → `context.rs` `exact_hls_context_before`; `RESPONSE_PUBLICATION_LIFECYCLE_BUDGET = 5 s` (`http/hls.rs:60`) |
| Child playlist waits under the playlist deadline, then takes 5 s for final admission | ✔ | `playlist.rs` `video_playlist_local_before`; `PLAYLIST_WAIT_BUDGET` = 12 + 30 + 13 = **55 s** |
| Native codes 3/4 count as media failure for the transcode fallback | ✔ | `web/player/transport.js` error handler, `mediaFailure:code===3\|\|code===4` |
| Startup scratch accounts for both gates plus one target | ✔ | `rolling/flow.rs:133` |
| Timeline §2.1 | ✔ | lab6 log 04:14:47.242 `vod_create refused 793 ms` · 51.412 first segment 7.8 s · 53.631 gate 16.475 s · 56.830 and 01.867 init-inspection timeouts · 59.629 `candidate_encode_route_unavailable` · 04.276 `writer_inventory_to_first_snapshot elapsed_ms=10589 produced_end_ms=56432` |
| 48.675 s frontier ⇒ first eligible segment ends at 56.432 | ✔ for an **explicit** lease | see N1 |

## 3. Findings

### B1. This is the reference film G incident again, and its fix is approved but unbuilt

[`NATIVE-HLS-STARTUP-RCA.md`](docs/streaming/NATIVE-HLS-STARTUP-RCA.md)
(2026-09-17, nynuc) has the same signature: a master 503 after 5,001 ms
waiting for init, a second master also timing out at 5 s, then Safari error
→ codec rescue. Its build contract,
[`NATIVE-HLS-STARTUP-IMPLEMENTATION.md`](docs/streaming/NATIVE-HLS-STARTUP-IMPLEMENTATION.md),
decided **Track N**:

- Readiness comes before native source assignment: fetch the master and the
  selected child successfully, then set `src`.
- A readiness failure before `src` is assigned never enters the
  video-element error ladder.
- There is one same-session native reload before the compatible fallback.
- A persistent code 4 is attributed "unverified, refused before decode".

Both docs are still `open` in `docs/README.md`. The deployed
`attachNativeHls` (`web/player/player.js:1280`) still assigns the source
immediately. So none of Track N shipped, and reference film / file 120 is the second incident
that Track N would have absorbed.

The proposal's §5.4 designs a different mechanism without citing either
doc: it probes after error 4 has already fired. Track N is stronger, because
the error never fires. It is also smaller, because it is client-only and
already reviewed. Keep exactly one idea from §5.4 and add it to Track N: a
**route preflight before the compatible fallback**. When no encode candidate
exists, report the refusal instead of spending the rescue on a predictable
409.

### B2. The server half duplicates the startup-latency effort and contradicts it in two places

[`PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md`](docs/streaming/PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md)
(2026-09-29) already owns this work:

- "separate startup readiness from steady reserve" (§4)
- "do not replace every use of `rolling_initial_runway_ms` mechanically"
- the same reload inequality as the proposal's §5.1
- M1 "establish why exact indexes are unavailable"
- an M2 transport sweep

[`PLAYBACK-STARTUP-LATENCY-BUILD.md`](docs/streaming/PLAYBACK-STARTUP-LATENCY-BUILD.md)
authorizes Sol to build it, but "dispatch pending" is still its status: no
`codex/playback-startup-latency` branch exists on the forge. The reference film / file 120
proposal is that effort's M0 evidence, and it should land there.

It conflicts with the effort in two places, and these are Paul's to
reconcile:

- **LL-HLS.** The effort lists "low-latency HLS protocol introduction" as a
  **non-goal**. The proposal recommends evaluating it.
- **Threshold.** The effort sweeps candidates of 12, 16, 24, 32 and 48 s.
  The proposal names 4 s, which is below the effort's smallest candidate.

### B3. Ordinary HLS cannot deliver a 4 s start on this geometry

After the first snapshot, three serial delays apply. The doc names only the
third.

| Delay | Value at `189286828` | Source |
|---|---|---|
| The source's first cut | 7.8 s on reference film / file 120; 4 s rounds up to it | log; `COPY_FIRST_SEGMENT_SECONDS = 2` is a floor, not the GOP |
| The server's next publication | `available_at + 16 s`. The early arm is `+8 s`, and only once `render_state == Rendering` and the runway is ≤ 10 s | `session.rs:1209`, `wants_early_publication` |
| Safari's next reload of a changed playlist | ≥ `TARGETDURATION` = 16 s (RFC 8216 §6.3.4) | target forced to 16 in `segment_index.rs` |

The client loads S1 at `a + δ`. The earliest it can see new media is
`a + δ + 16`, and server publication at `a + 16` is never later than that.
So a stall-free ordinary-HLS start needs **coverage ≥ 16 s + δ + one
segment's transfer**. That comes to ~20 s. Segment-aligned on this title,
it means the third segment, ~24 s.

The start-position concern is already handled: the served snapshot strips
`PLAYLIST-TYPE` and writes `EXT-X-START:TIME-OFFSET=0`.

The doc leaves out the quantity that decides how much any of this costs: the
producer ran at **3.75–3.9×**. That is 8.675 s of media in 2.218 s, then
39.957 s in 10.645 s, inside `-readrate_initial_burst`. The wall-clock cost of
a readiness threshold is therefore coverage ÷ burst speed:

| Readiness (media after origin) | Satisfying segment | Server-ready after create |
|---|---|---|
| One segment (≥ 4 s) | #1, 7.8 s | 2.25 s, observed |
| 16 s | #2, 16.475 s | 4.47 s, observed |
| **24 s** | **#3, ~24–25 s** | **~6.5 s, extrapolated** |
| 48.675 s (today) | 56.432 s | 15.11 s, observed |

The 24 s row comes from a one-condition change: the `None =>` first-publication
predicate in `publication_cycle_at`, plus the startup-bytes sizing. It saves
~8.6 s and stays inside RFC reload behaviour. The steady 48 s publication
budget already resumes on the next cycle: `earned` and `floor` select
against the unchanged `desired_end_ms`. LL-HLS saves a further ~4 s on this
title. In exchange it needs part publication on the copy route, blocking
reload and preload hints, plus Apple, Android and hls.js engagement. That is
a protocol project the authorized effort excludes, and it is not a fix for
this incident.

The proposal also reads "start after a few seconds are buffered" as *media*
seconds. If Paul meant a few seconds of *waiting*, ordinary HLS meets it.
Ask him rather than design for the strict reading.

### B4. The exact index is missing everywhere after every engine change

The fragment-index pipeline version is `fragment_index_engine_digest()`: the
ffmpeg executable **plus every loaded library**
(`ffmpeg.rs:2107`, `engine_objects_are_current`). Any image rebuild that
touches a shared object therefore opens a new keyspace. The lab6 copy of the
replicated DB shows four digests since 2026-09-09:
`c62a…` → `625c…` → `953a…` → `85e6…` (current since 2026-09-28).

| Fragment-index requests | `953a…` (previous) | `85e6…` (current) |
|---|---|---|
| Files with a ready index | 1,523 | **310** |
| Files queued | 1,037 | **909** |

For file 120 there are six ready artifacts, all under older digests, and no
job row has ever been claimed under `85e6…`. Its current-digest requests are:

- `de810949` (target lab6, `normal`, queued 2026-09-30 22:37), touched at
  **04:15:34 with `foreground_preempted`**, attempts 0
- one more `foreground_preempted`
- one plain queued request
- an admin request from 03:27 today, attempts 0

The play neither promoted any of them to foreground priority nor got one
built. So `vod_index_pending` is the expected state for most of the library
four days after an engine change. The proposal's preferred path, "exact
indexed VOD whenever available", is the minority path in practice.

That is the startup-latency effort's M1 answered. Whether the digest must
cover every loaded library is a separate decision: the identity was chosen
deliberately, so don't change it inside this effort. The foreground-promotion
gap belongs in M1.

Context: `dv_conversions` for file 120 failed on 2026-09-04 with a
read-only filesystem, so this title always needs the on-the-fly P7→P8.1
route.

### B5. The incident ran 9 s after a daemon restart

`plurxd starting … build=v0.3.0-5385-g189286828` was logged at
**04:14:31.245**, with listening at 31.719. The playback decision followed at
04:14:40.805. In the same window lab6 was running:

- the boot storage probe, reading `/20t/movies` at 335 MB/s, the mount this
  title lives on
- **20 Live TV caption-graph proof encodes**, software and VA-API, from
  04:14:34 to 04:14:59, i.e. across the entire startup
- slow Raft applies of 149–171 ms

The doc's production speed, its pre-creation interval and its §2.2 comparison film / file 9
comparison are all under boot load. The M0 replay must run on a warm daemon,
and once more right after a restart. Report the two separately; the
after-restart case is real, because Paul tests right after deploys.

### Non-blocking

- **N1 — state the lease mode.** The 48.675 s arithmetic holds only for an
  explicit lease with a position that is not advancing. Under the legacy
  wall-clock budget, desired would have been (15.11 − 4.47) + 48 ≈ 58.6 s.
  It would not have published at 56.432.
- **N2 — rate scaling only goes up.** `rolling_initial_runway_ms` clamps to
  `[48 000, RESERVE_MAX]`, so 0.25× still waits 48 s. Say whether the
  bootstrap scales down below 1×.
- **N3 — drop the "separately fenced prepared-init reader" (§5.3).** Once
  the first snapshot is early, the existing init fence costs nothing. A
  second reader is a second authority to keep honest.
- **N4 — the writer gate only matters below 12 s.** At ≥ 16 s readiness the
  rolling clock already dominates it. The effort keeps the gate at 12 for
  this reason.
- **N5 — `probePlaybackSource` can't do what §5.4 assumes.** `inspectMedia`
  is enabled only for `method==='transcode'`. On a master it finds no
  `.ts`/`.m4s` line, so it never reaches the child, the `EXT-X-MAP` init or
  a segment. This is moot under Track N, which fetches master and child
  itself.
- **N6 — check Safari's patience before relying on a server-side hold.**
  Aligning the master with the child path means a native master can hold for
  up to 55 s. Safari's tolerance for that is unmeasured. Track N's `fetch()`
  preflight makes the hold safe either way. Without Track N it is an
  experiment.
- **N7 — pre-creation time is unanalysed.** The decision at 40.805 and the
  create at 49.163 are 8.4 s apart, and the create phase alone is 2,714 ms.
  The doc tabulates both but analyses neither. The §6 target of 2–4 s
  click-to-first-frame is unreachable until this interval is explained
  (B5 is the first suspect).
- **N8 — naming, if this lands in the repo.** Change `lab6` → `lab6`,
  `forge.lan:3000` links → `forge.lan:3000`, and the titles → reference
  films (PR #364 rule). `reference-film-120-startup-evidence.json` is neither in the tree
  nor on the Mac. Commit it, sanitized, or drop the link.

## 4. Rulings for Paul (my picks; override freely)

1. **Fold, don't fork.** Amend the startup-latency effort with this
   incident as M0 evidence, and amend NATIVE-HLS-STARTUP Track N with the
   route preflight. *Pick: yes.*
2. **Readiness target.** Choose between ordinary HLS with the effort's sweep
   (24 s expected, ~6.5 s server-ready here) and LL-HLS, which the effort
   currently excludes. *Pick: ordinary HLS now. LL-HLS only as a separate
   effort, and only if the measured ordinary result misses what you meant by
   "a few seconds".*
3. **Master deadline.** Should the master wait for preparation under the
   playlist deadline, as the child already does, and keep 5 s for final
   admission? This touches the 09-17 non-goal "enlarge the server's
   five-second publication deadline". My read is that the change aligns two
   paths rather than enlarging the deadline, but it is your call. *Pick: do
   it, after Track N.*
4. **Index keyspace.** Open a separate investigation into whether the digest
   must cover every loaded library, since it is the biggest lever on
   `vod_index_pending`. *Pick: yes, outside this effort.*

## 5. Amended order

1. **Track N plus route preflight.** It is client-only, it would have turned
   both incidents into a delayed start rather than a failure, and its review
   is already done.
2. **Startup-latency M0/M1.** Use this incident on a warm daemon and after a
   restart. M1 starts from the keyspace table in B4.
3. **M2 sweep, then M3.** M3 is expected to be the one-predicate change
   behind the 24 s row, plus scratch sizing.
4. **Master/child deadline alignment**, per ruling 3.
**Packaging note:** host, forge and film names are normalized for repository review. Database and boot findings retain the supplied review’s provenance; this is not a new runtime inspection. The companion amendment responds to the evidence-file location claim.
