# Subtitle extraction on the cluster — why every first subtitle waits on one node's full read, and how the pool takes it over

**Status:** implementation handoff, **v2** — revised for the adversarial review
([SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md](SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md),
Codex, 2026-09-24, "request changes"); every finding R1–R7 is dispositioned in
§0 and its acceptance case is bound to a milestone in §5. Ready for an
executing session to claim under the work-board protocol. Nothing built.
· **Written:** 2026-09-24 (v1), revised 2026-09-24 (v2) · **Author:** Fable
(claude-fable-5-1) · **Reported by:** Paul, 2026-09-24, "every subtitle I try
tells me I have to wait for some work to be performed" while three server nodes
sit idle · **Base:** `main` at `0e2c3fd4` (the fleet runs `936157b4b`; the
difference that matters is PR #498, §2.4).

Companion to
[PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md)
§6, whose Fix C (the PGS ride-along on the fragment-index pass, merged as #456,
#466 and #463) this plan generalises, and whose §6.6 deliberately left three
things unsolved: coverage is per node, nothing backfills, and text tracks were
never in scope. Those three are this plan. Read that document's §6.2 table
first — every experiment it records is load-bearing here, and none is
repeated.

**How to execute this document.** It is written so a GPT, Claude or OpenRouter
session can build it without the author. Claim it on the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) (a draft
PR editing the row, `Agent-Model:` / `Agent-Session:` trailers), build the
milestones in §5 in order inside that one PR, record each in the Execution
log at the end, and stop at anything §5 marks **STOP** — those are Paul's
rulings, not judgement calls. The kickoff prompt in §9 is the whole brief.

---

## 0. Review dispositions (v1 → v2)

| ID | Finding, in one line | Disposition | Where it landed |
|---|---|---|---|
| R1 | `forced` priority means a fresh generation and a supersession, not "urgent"; playback misses would fork jobs and cancel backfill work | **Accepted.** A third priority, `foreground`, with `force_rebuild = 0`, promoted in place; a stable per-stamp generation; a bounded repair path when publications vanish behind a `ready` tombstone | §3.9 (request lifecycle), M2, M4 |
| R2 | "a publication row exists" is not "every eligible track is covered"; single-track hydration republishing as a replacement destroys coverage | **Accepted.** Publications are per (stamp, node, ordinal, representation) with a verdict and an `origin`; coverage is judged per ordinal from `extracted` rows only; hydration merges under a per-file lock and never republishes as a replacement | §3.10 (publication model), M3 |
| R3 | WebVTT cannot serve the burn path for ASS/SSA — `subtitle_file` excludes them on purpose and the burn `.mks` preserves styling | **Accepted.** Text tracks are stored in two representations where needed: `webvtt` for every text codec, and a `matroska` copy for `ass`/`ssa` (one input stream mapped twice); the VTT fast path moves to the VTT-keyed entry points, not `ensure_vtt_at` | §3.1, §3.2, §3.11, E1b |
| R4 | enqueue success is not progress: busy workers, uncharged preemption, cancellation and unreachable publishers all leave a waiter pending forever | **Accepted.** A claim-wait bound after which the requesting node claims the row itself as a foreground job (still one producer, still a progress row); `foreground` jobs are not preempted; cancellation is a memo'd terminal for `NEGATIVE_TTL`; unreachable publishers are a miss with a bounded repair | §3.9, §3.5, M4 |
| R5 | the clients' create-retry ladder is 3 rungs / 60 s absolute; a cold read cannot become visible "on the next retry" | **Accepted, and v1 was wrong.** Acceptance splits *warm* (peer hydration inside the ladder) from *cold* (the ladder is exhausted, the client shows `startup_exhausted` with Retry, the job continues, the manual retry succeeds). Today's inline path exhausts the same ladder, so this is not a regression, and the client contract question is flagged for Paul | §3.8, §6.2, §7.1 |
| R6 | the direct text endpoint maps any error to 500 and offline packaging awaits a VTT; neither has a pending protocol | **Accepted.** A consumer table names each consumer's representation, wait owner, budget, pending answer and fallback. The queue join lives *inside* the existing flight, so callers with an unbounded join keep awaiting and never see a pending error; the guardrail is narrowed to "no segment or start handler awaits" | §3.11 |
| R7 | `(dev, ino)` is local; a copied stamp breaks the burn consumer's identity rule, a replaced stamp discards it | **Accepted.** Publications carry a portable identity — size, mtime and the fragment-index sampled attestation digest (`object_version`, `s1:` regime) — and the receiver binds a hydrated artefact to its *own* held file's `(dev, ino)` only after re-attesting the same digest, with a fence against replacement during hydration | §3.10, M3 |
| §3 scope | "once per cluster" is not coordinated with index-pass rides; windows still read the source; the overnight check's "no hydration" contradicts peer serving | **Accepted, all three.** Promise restated as *at most one scheduled read per stamp, plus opportunistic rides, bounded*; the objective is *no inline whole-track read on the serving node*, windows stay and are skipped when the store answers; the overnight assertion is "no full-source extraction runs" | §1, §3.4, §6.2 |

## 1. Objective

A viewer who turns on any embedded subtitle track — text or bitmap — on a
title whose tracks the cluster holds gets it from whichever node is serving
the session, inside that client's existing budget, without that node reading
the source. A title the cluster does not yet hold gets **one scheduled
full-file read on an idle node** (opportunistic index-pass rides may add
bounded duplicates, §3.4), ahead of play wherever the library has been
backfilled and otherwise as a cluster job the first request enqueues. Every
byte of that work is attributable from the product — what, why, which node,
how far along, and a way to stop it — and the whole thing is one Developer
switch away from today's behaviour.

Precisely: **the serving node never runs an inline whole-track extraction
while the cluster queue is enabled.** Bounded window reads before the file's
midpoint remain (they are the bridge over the head of playback and cost a
fraction of a full read) and are skipped when the store already answers.

Not the objective: making one extraction faster (it is a line-rate read of the
whole file, PGS plan §6.2), or changing what any client does when a cold read
outlasts its retry ladder (§7.1 is Paul's).

## 2. Contract today

### 2.1 What the code does

There are two subtitle artefacts, one store, and — since PR #498 — one
replicated caption table.

**The WebVTT sidecar** (`crates/plurxd/src/subtitles.rs`, module doc lines
1–7): "extract once to a small WebVTT sidecar". It lives in the node-local
`<cache>/subs/` (`state.subs_dir`, `state.rs:705`; `cache/subs` per
`main.rs:5526`), keyed by `f<id>-s<n>-<size>-<mtime>.vtt` (`vtt_path`). On a
miss, `extract_vtt` (`subtitles.rs:2204`) spawns one `ffmpeg -i /dev/fd/3
-map 0:s:N -f webvtt` — a full demux of the source, started at byte zero.
The module's own constants say what that costs: `EXTRACTION_TIMEOUT = 600 s`
because "a cold extraction is a full-source read … has legitimately taken
~180 s" (`:29–35`), and `SIDECAR_JOIN_BUDGET = 5 s` because extracting one
track from file 5208 "read the entire file to produce 18,866 bytes, measured
at 402 s on m6" (`:51–68`). Its consumers are in the table in §3.11.

**The burn sidecar** (`ensure_burn_file`, `subtitles.rs:1050`): a
subtitle-only Matroska for burned transcodes, same full read, same cache
directory. `transcode.rs:15617` `subtitle_file` substitutes the VTT sidecar
for `subrip | srt | webvtt | mov_text` burns and **keeps ASS/SSA embedded
because converting it to WebVTT would discard positioning, styles, and the
release's authored typography** — the fact behind R3.

**The subtitle-source store** (`crates/plurxd/src/subtitle_source.rs`,
`<cache>/runtime/subtitle-source-v1/f<id>/`): PGS tracks kept by the
fragment-index pass (`subtitle_ride_along.rs`), read by `Consumer::Overlay`
and `Consumer::Burn` (`subtitle_source.rs:147`). It is **PGS only**: `plan()`
takes `pgs_ordinals_from_probe` (`fragindex.rs:1504`), the tee is `-c:s copy`
into `f=sup` and `f=framecrc` slaves (`subtitle_ride_along.rs:838–863`). It
is **node-local and best-effort by design** (PGS plan §6.6); `lookup`
classifies the miss on a node that hydrated its index as `hydrated_only`
(`subtitle_source.rs:410`) precisely so the miss rate could be measured.
`Manifest::source_matches` (`:188`) holds the burn consumer to `(dev, ino)`
of the file the session has open — a local identity (R7). `publish`
(`subtitle_ride_along.rs:1363`) swaps the manifest and then deletes artefacts
its predecessor named and it does not — a replacement, not a merge (R2).

**The seek windows** (`warm_vtt_window`, `subtitles.rs`; the R-M3
`SessionWindow` owner): a per-session partial extraction that bridges the
head of playback while the whole track is produced; it "declines itself past
the midpoint of the file" (`hls.rs:11432`). This plan does not touch the
owner registry; §3.2 says when a window is not started at all.

**What the cluster knows about any of this: nothing.** The analysis queue
(`analysis_requests`, `plurx-core/src/store/fragment_index_cluster.rs:88`)
admits two components, `fragment_index` and `skip_markers` (the CHECK at
`:306` and `hiqlite_fragment_index_cluster.rs:313`; `valid_request` at
`:1154` also pins `priority IN (normal, forced)` and `trigger IN (admin,
background)`; `request_file_analysis_for_identity` refuses anything else,
`state.rs:4132`). The job lease, the per-node slots
(`media:fragment-index:{slot}`, `state.rs:7291`), the peer transport and
`hydrate` (`fragment_index_cluster.rs:886`, `GET
/internal/media/fragment-index/{cache_key}` served by
`http/internal_media.rs:83`) — all of it is for fragment indexes. The
verified shared-cache mount (`shared_cache.rs`) does not cover `subs`.

### 2.2 The queue's actual identity rules (the facts behind R1)

- `analysis_request_generation` (`state.rs:2538`) returns a **fresh UUID when
  `force_rebuild`** and otherwise a fingerprint of `(file, size, mtime,
  component, pipeline_version, [attestation regime for fragment_index],
  video_identity)`.
- `valid_request` (`hiqlite_fragment_index_cluster.rs:1154`) requires
  `force_rebuild == (priority == "forced")`. So "forced" *is* "rebuild".
- The active unique index is over `(file_id, size, mtime, component,
  pipeline_version, video_identity, requested_generation, target_node_id)`
  (`:668`), so two forced requests never join — each has its own generation.
- `cluster_fragment_index_jobs` already knows a third priority,
  **`foreground`** (`CHECK (priority IN ('normal','forced','foreground'))`,
  `:238`, `:253`). `analysis_requests` does not. That is the precedent §3.9
  extends.

### 2.3 What the viewer sees, and why

When a session start needs a sidecar (a burn with a text track), the start
waits 5 s, then `session_start_error` (`http/hls.rs:3917`) maps `subtitle
sidecar is still being built` to a 503 `startup_timeout` with `Retry-After`.
Every client's create-retry ladder treats that code as `preparing`:
`CREATE_RETRY` in `web/playback-policy.js:868` is **three retries at
1 / 2 / 4 s with a 60 s absolute deadline**, restated by
`PlayerController.swift` (`CreateRetry`) and `PlaybackPolicy.kt`
(`createRetryStep`), and `web-policy.test.js` fails if the three drift. Four
creates that each spend the 5 s join budget exhaust the ladder in about 27 s;
the client then shows `startup_exhausted` with Close and Retry. **A 402 s
read cannot be answered by that ladder, today or after this plan** — R5, and
PGS plan §9 item 4, which is still open.

When an HLS session turns a track on mid-play, the rendition answers an empty
`no-store` segment while `warm_vtt` runs on that node in the background; the
player re-fetches until the sidecar exists. On a large remux that is minutes
of "selected but not showing".

### 2.4 PR #498 (merged 2026-09-24 15:08 ET) — what changed under the plan

`feat(subtitles): download missing captions from OpenSubtitles` added
**schema v45** (`DOWNLOADED_SUBTITLES_SCHEMA_VERSION`, `hiqlite.rs:108`; an
`ALTER TABLE files ADD COLUMN downloaded_subtitles`), a replicated, bounded
(256 KiB, 8 per file) WebVTT caption store keyed by file id/size/mtime, and
downloaded tracks appended to the selectable list *after* the embedded
ordinals. Consequences for this plan: the component migration is **v46**;
downloaded tracks are never `subtitle_source` targets (they are already
durable and replicated); and the existence of a replicated caption store is
an alternative for *text* representations that §7.4 puts to Paul rather than
deciding here.

### 2.5 Fleet evidence, 2026-09-24

Read from a copy of nynuc's hiqlite state machine and the three nodes' cache
directories (deploy key; nuc3 is a learner and holds no media cache).

| Fact | Value |
|---|---|
| files in the library | 6,204 |
| files with at least one subtitle stream | 5,217 — **28.8 TB** of source |
| text tracks (`subrip` 43,320 + `mov_text` 1,741 + `ass` 1,189) | **46,250** |
| PGS tracks (`hdmv_pgs_subtitle`) | 6,216 (plus 17 `dvd_subtitle`, 1 `dvb_subtitle`) |
| files with a fragment-index artefact | 4,892 |
| `analysis_requests` by state | 11,278 `ready` · 5,248 `failed` · 130 `queued` · 4 `submitted` (`fragment_index`); 5 `ready` (`skip_markers`) |
| VTT/burn sidecars in `<cache>/subs` | nynuc 43 · m6 31 · nuc4 6 — **80 sidecars for 46,250 text tracks** |
| the same track extracted on more than one node | 11 keys on both nynuc and m6 (`f5216-s0`, `f5323-s0`, `f5355-s0/s1`, `f5615-s0`, `f5699-s0`, `f5999-s0`, `f6071-s0`, `f6106-s0`, `f6247-s1`, `f9-s0`); `f5323-s0` and `f9-s0` on all three |
| subtitle-source store directories | nynuc 1 · m6 1 · **nuc4 46** — the PGS ride-along is producing on the node that runs index passes, and nowhere else |
| store size | 13 MB on nynuc; 186 MB of `subs` sidecars |

The duplication is real and exactly what §6.6 predicted. The ride-along's
coverage is one node of three. And the backlog is the library: 5,217 files,
28.8 TB, of which 4,892 already have an index — so **a backfill cannot ride
on the index pass; it is a deliberate second read of ~27 TB**, roughly 13 h
of wall time across three nodes at the fleet's line rate (~200 MB/s per
node), or two idle nights. That is the price of §3.6 and why it is a switch.

## 3. Change

Parts §3.1–§3.8 are the design; §3.9–§3.11 are the three contracts the review
asked for, and they take precedence over the prose wherever the two could be
read differently.

### 3.1 Text tracks ride along (the producer learns `webvtt` and `matroska`)

`RideAlongPlan` gains text tracks. The held-fd probe that already yields
`pgs_ordinals` yields text ordinals too — every subtitle stream whose codec is
not `is_bitmap_subtitle` and is one ffmpeg's `webvtt` encoder accepts
(`subrip`, `ass`/`ssa`, `mov_text`, `webvtt`, `text`); `dvd_subtitle`,
`dvb_subtitle` and anything unknown stay out, as bitmap tracks other than PGS
do today. Ordinals remain hard `-map 0:s:N` from the live probe, never from
scan-time facts (PGS plan §6.2 #7).

**Representations per track kind** (R3):

| kind | codec | stored representations | why |
|---|---|---|---|
| `pgs` | `hdmv_pgs_subtitle` | `sup` (+ `framecrc` verdict companion) | as today |
| `text` | `subrip`, `mov_text`, `webvtt`, `text` | `webvtt` | the VTT sidecar and, per `subtitle_file`, the burn |
| `text_styled` | `ass`, `ssa` | `webvtt` **and** `matroska` (`-c copy`) | the VTT sidecar for display; the `.mks` for the burn, which `subtitle_file` refuses to take from WebVTT |

The tee output carries per-stream codecs; a styled track is **mapped twice**
from the same input stream, once encoded and once copied:

```
-map 0:s:1 -map 0:s:3 -map 0:s:3 -map 0:s:0
-c:s:0 webvtt  -c:s:1 webvtt  -c:s:2 copy  -c:s:3 copy
-f tee "[select=0:f=webvtt:onfail=ignore]<stage>/s1.vtt|
        [select=0:f=framecrc:onfail=ignore]<stage>/s1.crc|
        [select=1:f=webvtt:onfail=ignore]<stage>/s3.vtt|
        [select=1:f=framecrc:onfail=ignore]<stage>/s3.crc|
        [select=2:f=matroska:onfail=ignore]<stage>/s3.mks|
        [select=3:f=sup:onfail=ignore]<stage>/s0.sup|
        [select=3:f=framecrc:onfail=ignore]<stage>/s0.crc|
        [f=null]-"
```

Everything after `pipe:1`, downstream of `copy_index_pipe_args`, so the
cluster cache key's pipeline digest is unchanged — the existing digest pin
test keeps proving it. The `webvtt` encode is what `extract_vtt` does today
(`-f webvtt` with ffmpeg's default subtitle encoder), so the stored `.vtt`
must be byte-identical to a sidecar the on-demand path would have produced;
the `.mks` must match `ensure_burn_file`'s output for the same track under
the derivation rule PGS plan §6.2 #10 established (`-copyts`, no
`-start_at_zero`). E1/E1b/E2 (§6.1) establish all of that before any code is
written.

**Verdict for a text track.** `framecrc` counts packets; the `.vtt` is parsed
for cues. `kept` when the cue count equals the packet count (E3 confirms the
1:1 for `subrip`, `ass` and `mov_text`, or records the deterministic ratio),
the file parses as WebVTT, and it is under `MAX_SIDECAR_BYTES` (8 MiB,
`subtitles.rs:101`). For `text_styled` the `.mks` is `kept` only when its
packet count (an `ffprobe -count_packets` on the stage file, or the same
`framecrc` companion) equals the same number. `empty` for a real track with
zero packets. `malformed` and `transient` exactly as for PGS. **Each
representation has its own verdict**; a `kept` `.vtt` beside a `malformed`
`.mks` is a valid, partially served track (§3.10).

**Manifest.** `MANIFEST_VERSION` stays 1; `TrackEntry` gains `kind` and a
`representations: [{format, verdict, attempts, file, sha256, bytes}]` list,
all `serde(default)` so a manifest written by today's build reads unchanged
(a legacy entry is one `sup` representation). A today's-build reader meeting
a `text` entry finds no `.sup` it understands and misses ("a miss is never an
error", module doc) — the only behaviour the one-deploy mixed-version window
needs.

**Cost.** One more muxer per representation on a pass that is already reading
every packet; E4 measures it. Disk: a dense SDH `.vtt` is ~200 KB and an ASS
`.mks` is about the same; 46,250 tracks is single-digit GB on a store capped
at 32 GiB (`MAX_STORE_BYTES`).

### 3.2 The VTT sidecar reads the store first (a third consumer)

`Consumer::Vtt` joins `Overlay` and `Burn`. **It is consulted at the
VTT-keyed entry points — `ensure_vtt`, `ensure_vtt_bytes` (`subtitles.rs:870`)
and `ensure_vtt_file` (`:1008`) — not in `ensure_vtt_at` (`:1607`), which is
also the machinery under `.mks` burn sidecars and window keys** (R3). On a
`kept` `webvtt` representation for the live stamp it copies the stored `.vtt`
into `<cache>/subs` under the existing key by an atomic rename and returns —
no ffmpeg, no flight, no memo. On `empty` it publishes the `WEBVTT\n\n`
sidecar. On a miss it falls through to the flight, which is where §3.9's
queue join lives. Validity is the store's own rule at use; the sha in the
stored name is checked on copy.

`Consumer::Burn` already reads the store; it now also accepts a `kept`
`matroska` representation for a `text_styled` track, under its existing
`(dev, ino)` rule as revised by §3.10.

The whole-track cache, its LRU, its negative memo and the window owner are
untouched. `warm_vtt_window` is **not started** when the store answers the
whole track (the rendition handler already reads `whole_track_state` before
warming, `hls.rs:11444`; the store lookup is folded into that state read).

### 3.3 Coverage becomes cluster-wide (publications and peer hydration)

The store stops being per node. The replicated publication rows of §3.10 say
which node holds which representation of which track for which source, and
`subtitle_source::lookup` on a local miss:

1. reads the rows for `(stamp, ordinal)`; rows with `origin = extracted` are
   coverage, rows with `origin = hydrated` are additional sources;
2. orders holders by reachable media peer (`membership.media_peers()`, the
   filter `hydrate` applies at `fragment_index_cluster.rs:886`);
3. fetches `GET /internal/media/subtitle-source/{file_id}/{ordinal}/{format}`
   from the first that answers — `PeerAuthMode::ExactRequest`, `PEER_DEADLINE`
   (8 s), a body cap of `MAX_TRACK_BYTES`; sha verified before the bytes are
   staged; a mismatch or a 404 forgets that row (best-effort, as `hydrate`
   does for a corrupt blob) and tries the next;
4. validates the artefact against **its own** source under §3.10's portable
   identity rule, then merges it into the local manifest under the per-file
   lock and writes its own `hydrated` publication row.

The handler is a sibling of the fragment-index one in `internal_media.rs`: it
serves only what its local manifest names, never reads the source, and
refuses when the switch is off. `hydrated_only` stops being a miss class the
viewer pays for and becomes the counter that says how often hydration ran;
`never_indexed` stays what it is: a file no node has ever read.

### 3.4 A `subtitle_source` analysis component — the pool does the read

The analysis queue gets a third component. Same table, same lease, same
claim sweep, same retry policy, same progress rows and lifecycle counters,
same Maintenance page. What is new is the work: **the ride-along without the
index** — the same `RideAlongPlan` argv with no `pipe:1` output and
`[f=null]-` as the only sentinel — so one `ffmpeg` reads the file once and
keeps every subtitle track it has, every representation, into this node's
store, then writes the `extracted` publication rows.

- **Target.** `target_node_id = ''` — any node — the way `skip_markers` is
  claimed (`claim_analysis_request`, `hiqlite_fragment_index_cluster.rs:1467`).
  A subtitle track is the same bytes whoever reads them and §3.3 moves them,
  so the job goes to whoever is idle. Learners never claim.
- **Who enqueues.** *Playback*: a consumer's store miss for `(stamp,
  ordinal, format)` that no `extracted` row anywhere settles (§3.10's
  eligibility) enqueues at `foreground` priority — through §3.9's
  enqueue-or-promote, never a fresh generation. *Backfill*: §3.6, at `normal`.
- **Coordination with index-pass rides — "once per cluster", qualified.**
  The component and the ride-along share the store but not a scheduler.
  The promise is: **at most one scheduled `subtitle_source` read per stamp
  is active cluster-wide** (the unique index guarantees it), and
  index-pass rides are opportunistic and may duplicate it, bounded by the
  number of index passes (≤ 3 per file, one per DV identity, PGS plan §6.2
  #12). Two mitigations, both cheap: the worker checks for a `kept`
  `extracted` row for every eligible ordinal before it starts (a ride that
  finished while the request queued makes the job a no-op that settles
  `ready`), and the ride-along skips a file whose eligible ordinals all have
  `extracted` rows already. Eviction, a source change, and R1's repair path
  legitimately re-read; none of those is a duplicate.
- **Schema (v46).** The `component` CHECK on `analysis_requests` admits
  `subtitle_source`; `priority` admits `foreground` (with the `valid_request`
  rule becoming `force_rebuild == (priority == "forced")` **and**
  `priority == "foreground" ⇒ component == "subtitle_source"`); `trigger`
  admits `playback`; the publication table of §3.10 is created. One
  migration in the shape v42 used to add `skip_markers`: rename to
  `analysis_requests_v45`, recreate, copy, re-create the indexes
  (`fragment_index_cluster.rs:295–330`), `MigrateFrom` arm, accepted-source
  list, the chain assertion, the `install_schema` object-count probe, a
  `replicated_v46_…` contract test, and the placeholder-order census
  extended to every new statement. A schema bump is a stop-the-fleet event
  on hiqlite (`schema_migration_action` refuses any other version on open),
  which the ansible `serial: 1` deploy handles — it ships in a deploy of its
  own, §6.3. `request_file_analysis_for_identity`'s `matches!`
  (`state.rs:4132`) and `resolve_analysis_request`'s dispatch (`:7799`) gain
  the arm; `pipeline_version` for the component is
  `subtitle-source:{MANIFEST_VERSION}:{ffmpeg engine digest}`, so an engine
  change re-requests.
- **What the job does not do.** It does not build a fragment index, it does
  not compile PGS PNGs (lazy and LRU, PGS plan §6.5), and it does not run on
  MPEG-TS sources (the ride-along's exclusion stands; those files stay on the
  inline path, §3.11).

### 3.5 The order of preference, in one place

A consumer asking for representation `r` of track `n` of file `f` on node
`N`, inside the flight it already owns:

1. `N`'s `<cache>/subs` has the sidecar → serve (today).
2. `N`'s store has a `kept` `r` for the live stamp → copy, serve (§3.2).
3. A publication row names a reachable holder → hydrate, validate, merge,
   serve (§3.3). An unreachable or lying holder is step 4's miss.
4. No settling `extracted` row anywhere, queue enabled → enqueue-or-promote
   at `foreground` (§3.9), and the flight **waits on the row**, not on an
   ffmpeg: it polls the store every `QUEUE_POLL = 2 s` for a settling row for
   `(stamp, n)` and returns to step 3 when one appears. If after
   `CLAIM_WAIT = 20 s` the row is still `queued` (no node was idle), **`N`
   claims the row itself as a foreground job** — bypassing
   `pretranscode_worker_idle()` for this one claim — and runs the worker
   inline in the flight, with a progress row. One producer either way.
5. Queue disabled (`cluster_fragment_index_enabled()` false, a single-node
   install, or the §3.7 switch off), enqueue refused, the row terminal
   (`attempt_limit`, `stored_probe_invalid`, `cancelled` inside its memo), or
   the source MPEG-TS → today's inline extraction on `N`, with its memo and
   its window (§2.1), byte-for-byte today.

Each step is bounded by the budget the caller already carries (§3.11); none
adds a wait a client can observe.

### 3.6 Backfill — an idle pool fills the store ahead of play

A discovery pass, `discover_subtitle_sources`, under a cluster lease
`media:subtitle-source:backfill` (one holder cluster-wide, the way the
`provider:artwork` lease at `state.rs:4943` fences artwork repair), run from
the same tick as `discover_cluster_fragment_indexes` and subject to the same
idleness rule on the enqueuing node. Each pass selects up to
`BACKFILL_PER_TICK = 8` files that have at least one eligible subtitle stream
in scanner facts, are not MPEG-TS, have **at least one eligible ordinal with
no settling `extracted` row** for their live stamp (§3.10 — a PGS-only
manifest from the ride-along does not count as text coverage), and have no
active or non-expired terminal `subtitle_source` request — newest
`scanned_at` first — and enqueues them at `normal` / `background` / target
any. **Off by default**; turned on from the Developer tab (§3.7). A viewer's
`foreground` request and the backfill's `normal` request for one file are one
row (§3.9); the foreground one claims first.

### 3.7 Attribution, and a way to stop it

- **The analysis progress row** (Maintenance → Analysis) shows
  `subtitle_source` jobs as *"reading `<title>` on `<node>` for 4 text + 1
  PGS tracks — 31.2 of 79.5 GB"* through `update_analysis_ride_along`
  (`state.rs:4330`), with the same cancel the page offers fragment-index
  jobs. A cancelled job charges no attempt, leaves no publication, and is a
  terminal for `NEGATIVE_TTL` (§3.9).
- **The Developer tab** gains `subtitle_cluster_sources` (covers §3.2–§3.6;
  the ride-along's `subtitle_stored_sources` keeps covering §3.1's producer)
  with an advisory enable section in the shape of `subtitle_stored_sources`
  (`developer.rs:701`): the analysis queue is enabled; every voter reports
  schema 46; at least one reachable media peer; the store is on a local
  filesystem with N GB free; and under it `subtitle_backfill` with the lease
  holder, files enqueued this process, files remaining (eligible ordinals
  without coverage), estimated remaining bytes.
- **Counters** on `/metrics` from `store_metrics_loop` (never the scrape
  path): lookups by outcome incl. `hydrated`; requests by trigger; jobs by
  verdict; bytes read by jobs; hydration bytes served/fetched; foreground
  self-claims; repairs.
- **Logs**: one `info` per job start and end with file, node, tracks,
  representations, bytes and duration.

### 3.8 What the client sees when it is done

No client change is in this plan. What changes:

- **Warm** (the cluster holds the track — after backfill, or after any node
  read it): the `preparing` surface and the empty rendition segment are not
  seen for a subtitle; a peer hydration is kilobytes inside the first rung.
- **Cold** (no node has read the file): exactly today's shape — the create
  ladder is exhausted in ~27 s and the client shows `startup_exhausted` with
  Retry; the HLS rendition keeps re-fetching an empty segment — **but the
  read is running on an idle node as an attributed job, and the manual Retry
  (or the rendition's next fetch after the job settles) succeeds**, on every
  node, for good. Whether the client should instead show a `preparing`
  surface that outlives the ladder with a progress reading is §7.1, Paul's
  ruling, and PGS plan §9 item 4.

### 3.9 Contract 1 — the request lifecycle

| Event | Rule |
|---|---|
| **Identity** | One request identity per `(file_id, size, mtime, component = subtitle_source, pipeline_version, video_identity = '', requested_generation, target = '')`. `requested_generation = fingerprint(stamp, component, pipeline_version, repair_epoch)` — **never a UUID**; `force_rebuild` is always `0` for this component (`valid_request` refuses `force_rebuild = 1` for it). `repair_epoch` is `0` until R1's repair path bumps it. |
| **Enqueue or join** | One store op, `enqueue_or_promote_subtitle_source(stamp, priority)`, in one replicated transaction: if an active row (`queued`/`running`/`submitted`) exists for the identity, return it (join); else insert. Three simultaneous misses on three nodes and one backfill row resolve to one row (the unique index at `hiqlite_fragment_index_cluster.rs:668` refuses the second insert, and the op re-reads on conflict). |
| **Priority promotion** | In the same op: if the joined row is `queued` at `normal` and the caller is `foreground`, `UPDATE … SET priority = 'foreground', updated_at_ms = now WHERE request_id = $1 AND state = 'queued'`. A `running` row is not touched — the work is already happening. Nothing is cancelled; nothing is superseded. `claim_analysis_request`'s ORDER BY sorts `foreground` before `forced` before `normal`. |
| **Claim** | As today (`resolve_analysis_requests`, `state.rs:7572`): idle worker, two per pass, lease, heartbeat. Plus the **foreground self-claim** of §3.5 step 4: after `CLAIM_WAIT` the requesting node calls `claim_analysis_request_foreground(request_id, node_id)` which claims *only that row* regardless of its own idleness, then runs the worker inline in the flight. A foreground self-claim is counted (`foreground_self_claims`). |
| **Preemption** | A `foreground` job is **never** preempted by the foreground-playback signal (it *is* playback). `normal` jobs keep today's uncharged yield. A `normal` job promoted to `foreground` while running keeps running (promotion does not apply to `running`), so it may still be preempted — the promoted-but-running case is accepted as bounded by the existing lease and retry. |
| **Completion** | The worker writes `extracted` publication rows for every probed ordinal and representation (with their verdicts), then settles the row `ready` with `result_cache_key = stamp`. A job that finds every eligible ordinal already covered settles `ready` without reading (`no_op` reason in the lifecycle counters). |
| **Cancellation** | Operator cancel (Maintenance) → row `cancelled`, no attempt charged, no publication. The consumer's flight, seeing `cancelled`, writes the existing **negative memo for `NEGATIVE_TTL` (120 s)** under the VTT key so polling clients do not re-enqueue inside it; after the memo a new miss may enqueue again. Stopping it for good is the switch. |
| **Terminal failure** | `attempt_limit`, `stored_probe_invalid`, `pipeline_version_unavailable` → today's tombstone semantics; the consumer releases to the inline path (§3.5 step 5) and the inline path's own memo applies. |
| **Artefact loss and repair** (R1) | A `ready` row with **no** `extracted` row anywhere for the stamp is a lost publication (all holders swept or gone). The miss path calls `retire_subtitle_source_ready(stamp)`: marks the `ready` row `cancelled` with `last_error_code = artifact_lost`, and the next enqueue uses `repair_epoch = 1 + count of prior artifact_lost rows for the stamp in the last 24 h`. **Bounded: `REPAIR_LIMIT = 3` per stamp per 24 h**; beyond it the miss releases to the inline path and the lifecycle counter records `repair_exhausted`. A stamp change (size/mtime) is a new identity, not a repair. |
| **Unreachable holders** | Hydration that cannot reach any holder is a miss for this attempt; the row is still `ready`, so the miss does not enqueue — it releases to the inline path after `CLAIM_WAIT` unless a holder becomes reachable inside it. Counted as `holder_unreachable`. (Today's behaviour, with one more chance.) |

### 3.10 Contract 2 — the publication model

**Table** `subtitle_source_publications` (v46), replicated:

| column | meaning |
|---|---|
| `file_id`, `source_size`, `source_mtime` | the stamp (portable part) |
| `source_attestation` | the fragment-index sampled digest of the source, `FragmentIndexSourceObservation.source_sha256` (`fragment_index_cluster.rs::attest_source`) — **portable identity** (R7). `object_version` under the `s1:` regime includes host-local metadata and is only a local fence. |
| `node_id` | the holder |
| `ordinal`, `kind` | the embedded subtitle ordinal and `pgs` / `text` / `text_styled` |
| `format` | `sup` / `webvtt` / `matroska` |
| `verdict`, `attempts` | `kept` / `empty` / `malformed` / `transient` — per representation |
| `origin` | `extracted` (this node ran ffmpeg on the source) or `hydrated` (copied from a peer) |
| `sha256`, `bytes` | of the artefact (absent for `empty`/`malformed`) |
| `published_at_ms` | |
| PK | `(file_id, source_size, source_mtime, node_id, ordinal, format)` |

**Definitions**

- *Extraction coverage* of `(stamp, ordinal)`: at least one `extracted` row
  for it with a **settled** verdict (`kept`, `empty`, `malformed`, or
  `transient` with `attempts ≥ TRANSIENT_ATTEMPTS`). Coverage is judged per
  ordinal, so a PGS-only manifest never covers a text ordinal (R2).
- *Availability* of `(stamp, ordinal, format)`: a `kept` row (any origin)
  on a reachable node whose bytes verify.
- *Eligible for enqueue* (playback or backfill): some eligible ordinal of
  the file has no coverage.
- *Settled without artefact*: `empty` is served as the empty sidecar;
  `malformed` and exhausted `transient` release the consumer to the inline
  path (which will fail the same way, and memo it) — the store never
  invents a track.

**Producer publish** (M3 changes `publish`, `subtitle_ride_along.rs:1363`):
under the per-file lock, rename artefacts, **merge** the manifest — entries
this pass produced replace the same `(ordinal, format)`; entries it did not
produce (other formats, other ordinals, `hydrated` entries) are kept — write
the manifest, then delete only artefacts this publish *replaced*. A ride that
produced nothing for an ordinal it did not map leaves that ordinal alone.

**Hydration merge** (R2): same lock, same merge, one `(ordinal, format)` at a
time; two concurrent hydrations of different ordinals both survive. A
hydrated entry is `origin = hydrated` in the manifest and in its row; it is a
source for peers but never coverage.

**Portable identity and local binding** (R7): the receiver accepts a peer's
artefact only when (a) the sha of the bytes matches the row, (b) the
receiver's own live `fstat` of the file gives the same size and mtime, and
(c) the receiver's own attestation of the file (the `cluster_fragment_index_sources` observation row's `source_sha256`
memo, or a fresh sampled digest — two seconds, PGS plan / repair doc) equals
the row's `source_attestation`. Only then does it write the manifest entry
**with its own `(dev, ino)`** from that same held fd. The fence: the held fd
is opened before (b), the manifest write happens under the lock with the fd
still held, and a `SourceStamp` re-read after the rename that differs from
the one bound discards the entry (the file was replaced under it). A
platform with no `(dev, ino)` binds size + mtime + attestation, as
`source_matches` already allows.

**Sweep**: removing a directory or an artefact deletes its rows in the same
step; a row whose artefact is gone is a lie a peer will discover as a 404 and
forget.

### 3.11 Contract 3 — the consumers

| Consumer | Where | Representation | Pending answer today | Wait owner and budget | With the queue | Fallback |
|---|---|---|---|---|---|---|
| Session start, text burn | `transcode.rs:15635` `ensure_text_subtitle` → `ensure_vtt_file` | `webvtt` (simple codecs only, `subtitle_file`) | 5 s join (`SIDECAR_JOIN_BUDGET`) then 503 `startup_timeout` + `Retry-After` (`hls.rs:3917`) | the start; 50 s `START_DEADLINE`, client ladder 27–60 s | §3.5 inside the flight; the 5 s join is unchanged; the pending 503 is unchanged | inline (§3.5 step 5) |
| Session start, styled/bitmap burn | `transcode.rs:19375` `ensure_burn_source` | `matroska` (`text_styled`) or `sup` | same 5 s join, same 503 | same | `Consumer::Burn` reads the new `matroska` representation; else as above | inline `ensure_burn_file` |
| HLS subtitle rendition (mid-play) | `hls.rs:11444` | `webvtt` | empty `WEBVTT\n\n` `no-store` segment; player re-fetches; 503 behind `subtitle_not_ready_503` | none — the handler never awaits (AVPlayer's 2 s) | store read folded into `whole_track_state`; the detached warm runs §3.5; window not started when the store answers | inline warm, window before midpoint |
| Direct text endpoint | `stream.rs:2496` `ensure_vtt_bytes` | `webvtt` | **none — awaits up to `SIDECAR_JOIN_UNBOUNDED` (11 min), error → 500** | the HTTP request itself | **unchanged**: the queue join is inside the flight, so the request keeps awaiting and completes when the pool publishes; it never sees a pending error (R6) | inline, as today |
| PGS overlay | `pgs_overlay::prepare` | `sup` | its own `PrepareState` async contract | the overlay | `Consumer::Overlay` unchanged; benefits from hydration | inline `prepare_stage` |
| Offline package / restore | `offline.rs:770`, `:1740`; `http/offline.rs:1531` | `webvtt` | awaits unbounded; error → 410/failed download | the job | **unchanged**, as the direct endpoint | inline |
| Downloaded captions (#498) | `files.downloaded_subtitles` | replicated WebVTT | n/a | n/a | **not a target** of this plan | — |

The guardrail, restated: **no segment handler and no session-start handler
awaits an extraction or a hydration** (the rendition publishes empty; the
start joins for 5 s). Callers that already await without a deadline keep
doing so — the queue changes where the bytes come from, not who waits.

## 4. Guardrails (non-goals)

- **The fragment-index cache key does not move.** Every argv change is
  downstream of `copy_index_pipe_args`; the digest pin test is the proof.
- **No segment or start handler awaits an extraction or a hydration** (§3.11).
- **No OCR, no PGS→text**, no change to which track auto-selects
  (`tracks.rs` `forced_or_default`), no change to the HDR burn guard, no PNG
  precompilation, no change to any client's retry ladder (§7.1 first).
- **No `-fs`, no exit-code reclassification, no `?` on a subtitle map, no
  scan-time ordinals** — every rule in PGS plan §6.2 holds for text.
- **Not a shared-cache-mount feature.** Bytes move over the peer transport
  the fragment index already trusts, and the plan stays correct with no
  mount.
- **The session does not deploy.** Milestones that need fleet evidence say
  so in §6 with the GPT prompt; nothing is marked done from a device
  observation nobody made.
- **The window owner registry (R-M3) is untouched.** Windows are started
  less often; their rules do not change.
- **`force_rebuild = 1` is never used for `subtitle_source`.** Urgency is
  `foreground`; rebuild is a stamp change or a repair epoch.

## 5. Milestones

One draft PR owns the plan (work-board rule 4); milestones are logical
commits and Execution-log rows. Each names its tests; the fast lane
(`make unit` locally for Rust) is what runs before merge, per Paul's
2026-09-17 cadence. **STOP** items are Paul's rulings: stop, flag in the PR
body and the board's Notes cell, do not guess.

### M0 — Experiments E1–E5 (§6.1), recorded in this document

No product code. Results go in a `### 6.1 results` subsection of this file,
in the plan PR, as PGS plan §6.2 recorded its table. **STOP if E1 or E1b
fails its §6.1 pass condition** — the tee cannot carry per-stream codecs,
the double map is refused, the `.vtt` is not byte-identical, or the `.mks`
is neither byte-identical nor cue-identical under `-copyts` without
`-start_at_zero`. The fallback is a second `ffmpeg` sharing the held fd,
which is a second read and needs its own justification. Paul directed the
executing session to choose and continue on 2026-09-24; the session chose
§6.1's explicit cue-identity allowance (M0 record below).

Files: `docs/clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md` only.

### M1 — Text tracks in the ride-along and the `Vtt` consumer (§3.1, §3.2)

Files: `crates/plurxd/src/subtitle_ride_along.rs` (text ordinals from the
probe beside `pgs_ordinals_from_probe`; `RideAlongPlan::args` per-stream
codecs and the double map; `tee_spec` `webvtt`/`matroska` slaves; the text
verdict; `plan()` skips a file whose eligible ordinals all have `extracted`
coverage — a store query, stubbed to "none" until M3), `subtitle_source.rs`
(`TrackEntry.kind`, `representations`, `Consumer::Vtt`, the `text_styled`
`matroska` read in `Consumer::Burn`), `subtitles.rs` (the store read at
`ensure_vtt` / `ensure_vtt_bytes` / `ensure_vtt_file` — **not** in
`ensure_vtt_at`; the `empty` short-circuit), `http/hls.rs` (`whole_track_state`
consults the store; no window when it answers), `fragindex.rs` (the ordinal
set passed to `plan`).

Tests (each one a named case): the digest pin; fixtures for a `subrip`, an
`ass` and a `mov_text` track with the first cue not at zero, on a zero and a
non-zero source start (PGS plan §6.7 rule); a text track with a corrupt
packet; the packet/cue count rule; **an ASS fixture with positioning and
styling burns identically through the stored `.mks` and through the source**
(R3 acceptance); **a stored `webvtt` cannot be published under a burn-sidecar
or window key by the generic helper** (R3); a manifest written by today's
build still reads; a `text` entry is a miss for a reader without the field; a
`kept` `.vtt` beside a `malformed` `.mks` serves the VTT and refuses the burn;
no flight starts when the store answers; the window warm is not started when
the store answered. Ships behind `subtitle_stored_sources`. Deployable alone.

### M2 — Schema v46: the component, the priority, the trigger, the publication table

Files: `crates/plurx-core/src/store/hiqlite.rs` (`SUBTITLE_SOURCE_SCHEMA_VERSION
= 46`, `AUTH_SCHEMA_VERSION`, the `MigrateFrom` arm, accepted sources, the
chain assertion), `hiqlite_fragment_index_cluster.rs` (the rebuild in the v42
shape; `valid_request` for `subtitle_source` / `foreground` / `playback`;
`claim_analysis_request` ORDER BY; `enqueue_or_promote_subtitle_source`,
`claim_analysis_request_foreground`, `retire_subtitle_source_ready`;
publication CRUD), `fragment_index_cluster.rs` (SQLite twin, same ops),
`placeholder_census.rs` (every new statement), `tests/store_contract.rs`
(`replicated_v46_…`; the three new ops on both backends).

Tests: **three simultaneous enqueues plus one existing backfill row yield
one active row; a `foreground` join promotes a `queued` `normal` row in place
and leaves a `running` one alone; a promoted row is claimed before `forced`
and `normal`; `force_rebuild = 1` is refused for the component; retiring a
`ready` row and re-enqueueing produces a new generation, and the fourth
repair in 24 h is refused** (R1 acceptance); the census over the new
statements; migration from v45 with live rows. The single stop-the-fleet
deploy. Nothing enqueues the component yet.

### M3 — Publications, hydration, portable identity (§3.3, §3.10)

Files: `subtitle_ride_along.rs` (`publish` becomes a merge under a per-file
`tokio::sync::Mutex` keyed by file id, held in a bounded map; rows written
after the rename; `source_attestation` from the fragment-index memo or a
fresh sampled digest), `subtitle_source.rs` (`lookup` steps 1–4 of §3.3;
`source_matches` takes the manifest entry's *bound* `(dev, ino)`, which
hydration sets from the receiver's fd; sweep deletes rows),
`http/internal_media.rs` + `http/mod.rs` (the
`/internal/media/subtitle-source/{file_id}/{ordinal}/{format}` route; refuses
with the switch off; serves only what the manifest names),
`plurx-cluster-check` (`cluster_activity.rs`: a scenario where node B looks up
a track node A published).

Tests: **hydrate two different ordinals concurrently and retain both; remove
the original complete publisher and request a third ordinal — the request
recovers through §3.9's repair; an old PGS-only manifest does not count as
text coverage; malformed, empty and exhausted-transient verdicts each take
their documented path** (R2 acceptance); **hydrate between two fixtures
whose source files have different `(dev, ino)` — the burn consumer accepts
the artefact bound to the receiver's own inode; replace the receiver's
source during hydration — the entry is discarded** (R7 acceptance); a row is
written only after the rename; a stale row (sha mismatch) is forgotten and
the next holder tried; a 404 holder is skipped; the handler refuses with the
switch off; body cap enforced.

### M4 — The `subtitle_source` job, the playback enqueue, the waits (§3.4, §3.5, §3.9)

Files: `state.rs` (the `subtitle_source` arm in `resolve_analysis_request` at
`:7799`; the no-index worker reusing `RideAlongPlan` with no `pipe:1`; the
no-op settle when coverage exists; `request_file_analysis_for_identity`
admits the component; the foreground self-claim; the progress row through
`update_analysis_ride_along`), `subtitles.rs` (the flight's §3.5 sequence:
enqueue-or-promote, `QUEUE_POLL`, `CLAIM_WAIT`, the release conditions, the
cancel memo), `pgs_overlay.rs` and `transcode.rs` (their consumers call the
same sequence through the store), `http/developer.rs` +
`web/pages/settings-developer.js` (`subtitle_cluster_sources` with its
enable section — regenerate `tests/ui-structure.golden`, do not hand-edit).

Tests: the argv has no `pipe:1` and ends in the sentinel; a job publishes
rows and a directory; a job that finds full coverage settles `ready` without
spawning; **hold every worker busy — the requester self-claims after
`CLAIM_WAIT` and exactly one producer runs; preempt a `normal` job
repeatedly — it yields uncharged as today, while a `foreground` job is never
preempted; cancel a job — the consumer memos for `NEGATIVE_TTL` and does not
re-enqueue inside it; make every holder unreachable — the consumer releases
to inline after `CLAIM_WAIT` and no second producer is created** (R4
acceptance); **a direct text request and an offline job against a cold
queued source complete (the fixture producer publishes) and never return
500** (R6 acceptance); a terminal `failed` row releases the inline path; a
learner never claims. Ships behind `subtitle_cluster_sources`, default
**off**.

### M5 — Backfill and its switch (§3.6, §3.7)

Files: `state.rs` (`discover_subtitle_sources` under
`media:subtitle-source:backfill`; the eligibility query; `BACKFILL_PER_TICK`),
`http/developer.rs` + `settings-developer.js` (`subtitle_backfill` under the
M4 item), `telemetry.rs` (the counters of §3.7).

Tests: the pass enqueues nothing when the worker is busy; at most
`BACKFILL_PER_TICK`; never a file with full coverage or an active/unexpired
terminal request; a PGS-only-covered file **is** enqueued for its text
ordinals; newest first; the lease is exclusive across two nodes; the
Developer item renders each requirement from the real probes. Default **off**.

## 6. Verification and rollout

### 6.1 Experiments before M1 (on nuc3, synthetic sources muxed with `-copyts`, as PGS plan §6 did)

| # | question | pass condition |
|---|---|---|
| E1 | can one `tee` output carry `webvtt`-encoded and `copy` streams to `f=webvtt` and `f=sup` slaves at once? | ffmpeg 8.0.1 exits 0 with all slaves written; the index on `pipe:1` is byte-identical to a pass with no ride-along; the `.vtt` is byte-identical to `extract_vtt`'s from the same source |
| E1b | can the same input stream be mapped twice (`webvtt` + `copy`) into one tee, with a `matroska` slave selecting the copy? | exits 0; the `.mks` is byte-identical (or cue-identical with `-copyts`, no `-start_at_zero`) to `ensure_burn_file`'s for that track, on a zero and a 7.5 s source start |
| E2 | does `ass`→`webvtt` in the tee equal `extract_vtt`'s? | byte-identical, including the styling loss both already have |
| E3 | is one `framecrc` packet one WebVTT cue for `subrip`, `ass`, `mov_text`? one packet in the `.mks`? | counts equal, or the deterministic ratio is recorded and used |
| E4 | what does a 12-text (+2 styled) + 3-PGS ride cost on an index pass? | CPU and RSS within noise of the bare pass; wall time unchanged |
| E5 | a text slave that fails mid-stream (corrupt packet, `ENOSPC` on the stage) | the index completes, exit 0, the failing representation is `malformed`/`transient`, the others are unaffected |

### 6.1 results — 2026-09-24, M0 cue-identity decision

The experiment ran on nuc3 with `ffmpeg` and `ffprobe` 8.0.1-3ubuntu2. A
six-second synthetic MKV was muxed with `-copyts`: MPEG-4 video, SRT with its
first cue at 1.25 s, styled ASS with its first cue at 1.50 s and
`{\pos(320,300)}`, and the repository's PGS fixture. The bare pass and the
ride-along pass used the same video-to-fragmented-MP4 output; the latter
appended hard subtitle maps for SRT, ASS twice and PGS, with per-stream
`webvtt`, `webvtt`, `copy`, `copy` codecs, per-slave `onfail=ignore` and the
mandatory null sentinel. The direct WebVTT comparison used
`-map 0:s:N -f webvtt`; the direct burn comparison used the current
`ensure_burn_file` argument shape (`-copyts -start_at_zero -map 0:s:1
-map 0:t? -c copy -avoid_negative_ts disabled -f matroska`).

| Experiment | Observed result | Disposition |
|---|---|---|
| E1, zero start | The tee exited 0 with empty stderr and wrote all four selected subtitle streams. Bare and tee index SHA-256 were both `6be1c084a6176e46f476f538f355174d1eb3bf5b385e3f448333c1af951b6f47`. The PGS `.sup` was 2,504 bytes. SRT WebVTT matched direct extraction at SHA-256 prefix `403c5f5a`; ASS WebVTT matched at `23ee845d`. | Pass for this fixture. |
| E1b, zero start | The double map and Matroska slave succeeded, but the 1,143-byte tee `.mks` (`f48be786a250704b3ad2d7d18f02e8910bea22c15e3a49e5b655644d1d5ebb70`) differed from the 1,143-byte direct burn `.mks` (`4a5961ca2768514c92507b1e5afcbbd3c9680aee476dcaba4fe53ea16b6db7c3`). First byte difference: offset 221. `ffprobe` found two matching ASS packets on each side: PTS 1.500 s and 3.600 s, matching durations, payload hashes and ASS extradata. | **Pass by cue identity** under E1b §6.1. The byte difference remains recorded. |
| E1/E1b, 7.5 s start | Tee exited 0; bare and tee index SHA-256 were both `7859d0b45e6500b3371e07d618422749b2b437cfa116d07dc28c091838a92d7c`. SRT and ASS WebVTT were byte-identical to direct extraction. Tee and direct ASS `.mks` bytes differed, but both had the same two packet timestamps, durations, sizes, payload hashes and extradata (first packet PTS 1.480 s). | Pass by §6.1 cue identity. |
| E2 | ASS WebVTT tee and direct output SHA-256 matched at zero start (`23ee845d0e7523e602d97c8d36caa9973942371b3606cb7f28a3c8bb2d41dae4`) and 7.5 s start (`eda8bc6e0a765acb998a222b1094337fe4ffc69bf510fa861ecbbc0056033b53`), including the same styling loss. | Pass. |
| E3 | SRT, ASS and `mov_text` each yielded two WebVTT cues and two `framecrc` packets; the ASS `.mks` had two packets. The `mov_text` fixture encoded the two-cue SRT in MP4, then used the production-shaped tee with a VTT, framecrc and null slave. | Pass: 1:1 for these fixtures. |
| E4 | Five paired runs of a 120 s, 20,222,983-byte synthetic file with 12 SRT, 2 ASS and 3 PGS tracks: bare and ride indexes had the same SHA-256 (prefix `7c2242b2`). Wall time was 0.04–0.05 s for both; user CPU was 0.03 s bare and 0.03–0.04 s riding; peak RSS was 57,916–58,472 KiB bare and 58,900–59,260 KiB riding (about 0.7–1.3 MiB more). | Pass for this synthetic case; the RSS increment is recorded, not called zero. |
| E5 | A WebVTT slave sent to `/dev/full` failed with `No space left on device`, but ffmpeg exited 0, the index matched baseline, and the other ASS/PGS outputs matched their controls; classify the failed representation `transient`. A corrupt SRT packet also left the index and other tracks intact, with FFmpeg reporting `Invalid UTF-8 in decoded subtitles text` and `Error decoding subtitles`; classify that representation `malformed`. Both its VTT cue count and framecrc count fell from two to one, so equality of those counts alone would **falsely publish an incomplete VTT as kept**. M1 must veto that decoder error and test it by name. | Isolation pass; verdict correction required in M1. |

Both experiment runs used private temporary directories on nuc3, then removed
and verified their removal. Exact generated argv and results were retained
locally while this record was written. M0 wrote no product code and made no
fleet deployment. The §8 audit against implementation base `f600d2823` also found that
`object_version` is host-local metadata, whereas
`FragmentIndexSourceObservation.source_sha256` is the portable sampled
digest; §3.10 needs correction before M3.

### 6.2 Acceptance — deliberately not a matrix, and split warm from cold

Per Paul's 2026-09-22 rule, the bar is the narrow real check. After M4 is
deployed with `subtitle_cluster_sources` on and the backfill **off**:

1. **Cold.** Pick one title per client (Apple TV, Android tablet, web) that no
   node has read (Maintenance shows no coverage). Turn on a text track.
   Expected: the pending answer; a `subtitle_source` job appears on
   Maintenance **on a node other than the serving one** (or, if none was
   idle, on the serving node as a *foreground self-claim*, so labelled);
   the client's ladder is exhausted in ~27 s and shows `startup_exhausted`
   with Retry — **that is the documented cold outcome, not a failure**;
   when the job settles, Retry shows the track; `journalctl` on the serving
   node shows no `ffmpeg … -f webvtt` of its own unless the self-claim ran.
2. **Warm.** Play the same title from a second node. Expected: the track
   shows inside the first rung; the serving node's log shows one hydration;
   no job runs.
3. **Off.** Turn the switch off and repeat 1 on a third title. Expected:
   exactly today's behaviour — an inline extraction on the serving node.
4. **Slow.** (R5) A fixture whose extraction is forced past 60 s: the client
   outcome is exactly `startup_exhausted` + Retry, and the Retry succeeds.

After M5, with the backfill on overnight: Maintenance shows the remaining
count falling; the next morning, step 2 is repeated on three random titles
from a node that did not produce them — **no full-source extraction runs**
(a hydration may), and the track arrives inside the first rung.

### 6.3 Rollout

- M1 deploys with the ordinary train; safe with the store empty.
- M2 is the schema bump: **all voters in one `serial: 1` deploy**, nothing
  else in it, a `STATUS.md` note naming v46. A mixed-version cluster refuses
  to open — the existing contract, not a new risk, but the deploy is not to
  be split across days.
- M3–M5 deploy with the ordinary train, both switches off, then
  `subtitle_cluster_sources` on, §6.2 run, then `subtitle_backfill` for one
  night with Maintenance watched.
- Rollback is the switch, on every milestone. Nothing deletes an existing
  sidecar.

### 6.4 What a session cannot do — the GPT prompt for the fleet steps

> On the plurx fleet (nynuc, m6, nuc4; ansible `media/deploy.yml -e
> sync=false`, serial), deploy the build carrying M1–M4 of
> `docs/clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md`. M2 is a hiqlite schema
> bump to v46: deploy all three voters in the same run and confirm each
> reports schema 46 on `/readyz` before moving on. Then in Settings →
> Developer turn on `subtitle_cluster_sources` (leave `subtitle_backfill`
> off), and run §6.2 steps 1–4 on the Apple TV, the TCL tablet and Safari.
> For each: the title, the serving node, what Maintenance → Analysis showed
> and on which node, the client surface seen and after how long, whether
> Retry showed the track, and `journalctl -u plurx --since -10m | grep -E
> 'ffmpeg|subtitle_source|hydrat|self-claim'` from the serving node. Paste
> the results into the plan's Execution log as one row per step.

## 7. Open questions — rulings that are Paul's

1. **The cold-start client contract (R5).** Today and after this plan, a
   cold read outlasts the 60 s ladder and the viewer sees `startup_exhausted`
   + Retry. The alternative is a `preparing` surface that outlives the
   ladder with a progress reading from the job's row — a client contract
   change on all three clients, and PGS plan §9 item 4 already asks this.
   Proposed: ship this plan without it (the pool makes the *second* attempt
   succeed everywhere), and decide the surface separately.
2. **Backfill scope.** All 5,217 files, newest-scanned first, ~two idle
   nights; or watched-first; or text-only. Proposed: all, because the switch
   is off by default and the queue yields to playback.
3. **Any-node vs. requester-first.** Proposed: any-node, with the foreground
   self-claim as the bound (§3.9).
4. **Text representations in the replicated caption store instead of the
   file store (#498).** PR #498 keeps downloaded WebVTT in Raft, 256 KiB × 8
   per file. Extracted text tracks (46,250 × ~50 KB ≈ 2 GB) would make every
   voter hold them and remove hydration for text entirely — at a Raft log
   and snapshot cost this plan has not measured. Proposed: the file store
   plus hydration now (it is needed for PGS and `.mks` regardless); measure
   the caption store's cost as a follow-up.
5. **`subtitle_cluster_sources` default.** Off until §6.2 has been run once.

## 8. Anchors, to re-verify at build time (at `0e2c3fd4`)

| Thing | Where |
|---|---|
| sidecar module doc, `EXTRACTION_TIMEOUT`, `SIDECAR_JOIN_BUDGET`, `SIDECAR_PENDING_PREFIX` | `crates/plurxd/src/subtitles.rs:1–86` |
| `MAX_SIDECAR_BYTES` | `crates/plurxd/src/subtitles.rs:101` |
| `ensure_vtt_bytes`, `ensure_vtt_file`, `ensure_burn_file`, `ensure_vtt_at`, `extract_vtt` | `crates/plurxd/src/subtitles.rs:870`, `:1008`, `:1050`, `:1607`, `:2204` |
| store module doc, `STORE_DIR`, `MAX_TRACK_BYTES`, `MAX_STORE_BYTES`, `TRANSIENT_ATTEMPTS` | `crates/plurxd/src/subtitle_source.rs:1–68` |
| `Consumer`, `source_matches`, `latched`, miss classes, `lookup`, `lookup_snapshot` | `crates/plurxd/src/subtitle_source.rs:147`, `:188`, `:209`, `:405–415`, `:495`, `:840` |
| ride-along module doc, `plan()`, `args`/`tee_spec`, `publish` | `crates/plurxd/src/subtitle_ride_along.rs:1–33`, `:684`, `:838–863`, `:1363` |
| `pgs_ordinals_from_probe` call, plan attach | `crates/plurxd/src/fragindex.rs:1504`, `:1690` |
| `PEER_PATH_PREFIX`, `PEER_DEADLINE`, `hydrate`, `SourceStamp`, `ATTESTATION_REGIME` | `crates/plurxd/src/fragment_index_cluster.rs:21–22`, `:886`, `:810`, `:58` |
| fragment-index peer handler | `crates/plurxd/src/http/internal_media.rs:83` |
| `analysis_request_generation`, `request_file_analysis_for_identity`, `update_analysis_ride_along`, `provider:artwork` lease, discovery slots, `resolve_analysis_requests`, `skip_markers` arm | `crates/plurxd/src/state.rs:2538`, `:4132`, `:4330`, `:4943`, `:7291`, `:7572`, `:7799` |
| `analysis_requests` schema and v42 rebuild | `crates/plurx-core/src/store/fragment_index_cluster.rs:88–115`, `:295–330` |
| hiqlite twin: `valid_request`, claim SQL, active unique index, `foreground` precedent | `crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs:1154`, `:1467`, `:668`, `:238`/`:253` |
| schema versions, `AUTH_SCHEMA_VERSION = 45` | `crates/plurx-core/src/store/hiqlite.rs:73–110` |
| downloaded captions (#498) | `crates/plurx-core/src/store/downloaded_subtitles.rs`, `crates/plurxd/src/online_subtitles.rs` |
| `subtitle_file` (ASS exclusion), `ensure_text_subtitle`, `ensure_burn_source` call | `crates/plurxd/src/transcode.rs:15617`, `:15635`, `:19375` |
| `session_start_error` → `startup_timeout`; rendition `whole_track_state` | `crates/plurxd/src/http/hls.rs:3917`, `:11444` |
| direct text endpoint | `crates/plurxd/src/http/stream.rs:2496` |
| offline consumers | `crates/plurxd/src/offline.rs:770`, `:1740`; `crates/plurxd/src/http/offline.rs:1531` |
| `CREATE_RETRY` ladder | `crates/plurxd/src/web/playback-policy.js:868` (mirrored in `PlayerController.swift` `CreateRetry`, `PlaybackPolicy.kt` `createRetryStep`) |
| Developer item `subtitle_stored_sources` | `crates/plurxd/src/http/developer.rs:701` |
| `media_peers` | `crates/plurx-core/src/cluster/membership.rs:6189` |

## 9. Kickoff prompt for the executing session

> You are building `docs/clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md` (v2)
> in the plurx repo (Forgejo, `noirr/plurx`, `main` at or after `0e2c3fd4`).
> Read the plan end to end, then `docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md`
> §6 and `docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md` (the claim
> protocol). Claim the plan: one draft PR (`WIP:` title) that edits the board
> row, `Agent-Model:` / `Agent-Session:` trailers on every commit, one
> Execution-log row per milestone in the plan file. Build M0 → M5 in order,
> in that one PR; M0 is experiments on nuc3 whose results you record in
> §6.1. §3.9–§3.11 are contracts and override any prose that reads
> differently. Every acceptance case named under a milestone is a test in
> that milestone, by that name. Anything marked **STOP** is Paul's ruling:
> stop, write what you found in the PR body and the board's Notes cell, and
> wait. Do not deploy; do not touch a client's retry ladder; do not use
> `force_rebuild = 1` for the new component; re-verify every §8 anchor
> against your base before relying on it. When the fast lane is green after
> the adversarial review, merge it yourself and delete the branch.

## Execution log

Executing sessions append one row per milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-24 | claude-fable-5-1 | https://claude.ai/code/session_01LUY4Gc3ZFwF8xzj6Eg9Dy1 | Plan v1 | [#497](http://192.168.4.7:3000/noirr/plurx/pulls/497) | Written from `936157b4b` and the 2026-09-24 fleet read in §2.5. Reviewed by Codex the same day: request changes, R1–R7. |
| 2026-09-24 | claude-fable-5-1 | https://claude.ai/code/session_01LUY4Gc3ZFwF8xzj6Eg9Dy1 | Plan v2 | [#497](http://192.168.4.7:3000/noirr/plurx/pulls/497) | All seven findings accepted and re-anchored at `0e2c3fd4` (every cited behaviour re-read in source); the three contracts added (§3.9–§3.11); schema moved to v46 after PR #498 took v45; acceptance split warm/cold; ready for an executing session. Nothing built. |
| 2026-09-24 | gpt-6 | none:openai:2026-09-24 | M0 | [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) | nuc3 E1 passed; E1b was cue-identical but byte-different. Paul directed the session to choose and continue; it chose §6.1's explicit cue-identity allowance. E1–E5 completed on nuc3; E5 exposed a text-verdict bug to fix in M1. |
| 2026-09-24 | gpt-6 | none:openai:2026-09-24 | M1 | [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) | `53b8b6fe2`: typed SRT/ASS/mov_text ride-along, double-mapped ASS, decoder-error veto from E5, stored VTT and styled burn consumers, and named cases. Pinned compile, format and Clippy pass; fast lane pending. |
| 2026-09-24 | gpt-6 | none:openai:2026-09-24 | M2 | [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) | `53b8b6fe2`: schema v46 on SQLite and Hiqlite, deterministic foreground join, bounded repair, publication CRUD, v45 migration and named store cases. Pinned compile, format and Clippy pass; fast lane pending. No schema deployment. |
| 2026-09-24 | gpt-6 | none:openai:2026-09-24 | M3 | [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) | `53b8b6fe2`: merge-safe publications, portable sampled digest and receiver inode binding, authenticated bounded peer route, node A → B replicated lookup scenario and named cases. Pinned compile, format and Clippy pass; fast lane pending. |
| 2026-09-24 | gpt-6 | none:openai:2026-09-24 | M4 | [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) | `53b8b6fe2`: no-index worker, foreground self-claim, bounded VTT/burn flights, typed progress and manual Developer switch; named cases include cold direct/offline requests. Pinned compile, format and Clippy pass; fast lane pending. |
| 2026-09-24 | gpt-6 | none:openai:2026-09-24 | M5 | [#507](http://192.168.4.7:3000/noirr/plurx/pulls/507) | `53b8b6fe2`: exclusive idle backfill, newest-first uncovered candidate query, telemetry and live advisory Developer diagnostics with named cases. UI baseline regenerated from 78 captures without browser errors. Pinned compile, format and Clippy pass; fast lane pending. |
