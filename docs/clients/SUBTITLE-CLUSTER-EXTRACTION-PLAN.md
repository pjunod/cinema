# Subtitle extraction on the cluster — why every first subtitle waits on one node's full read, and how the pool takes it over

**Status:** proposal, v1, awaiting adversarial review · **Written:** 2026-09-24
· **Author:** Fable (claude-fable-5-1) · **Reported by:** Paul, 2026-09-24,
"every subtitle I try tells me I have to wait for some work to be performed"
while three server nodes sit idle · **Fleet read against:**
`v0.3.0-3803-g936157b4b` on nynuc, m6 and nuc4 (deployed 2026-09-24 ~18:30 UTC).

Companion to
[PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md)
§6, whose Fix C (the PGS ride-along on the fragment-index pass, merged as #456,
#466 and #463) this plan generalises, and whose §6.6 deliberately left three
things unsolved: coverage is per node, nothing backfills, and text tracks were
never in scope. Those three are this plan. Read that document's §6.2 table
first — every experiment it records is load-bearing here, and none is
repeated.

Read §2 before §3: the reason the cluster does nothing for subtitles is not a
bug. Subtitle extraction predates clustering and was left as a lazy per-node
cache; nothing in the server ever *schedules* it. §3 puts it on the machinery
the fragment index already uses.

---

## 1. Objective

A viewer who turns on any embedded subtitle track — text or bitmap — on any
title the library has finished indexing gets it inside the client's normal
poll budget, from whichever node is serving the session, without that node
reading the source file. The full-file read that producing a subtitle
requires happens **once per file per cluster**, on an idle node, ahead of
play wherever the library has been backfilled and otherwise as a cluster job
the first request enqueues. Every byte of that work is attributable from the
product (what, why, which node, how far along, and a way to stop it), and the
whole thing is one Developer switch away from today's behaviour.

Not the objective: making one extraction faster. It is a line-rate read of
the whole file (§6.2 of the PGS plan); the win is doing it once, elsewhere,
earlier.

## 2. Contract today

### 2.1 What the code does

There are two subtitle artefacts and, since #466, one store.

**The WebVTT sidecar** (`crates/plurxd/src/subtitles.rs`, module doc lines
1–7): "extract once to a small WebVTT sidecar". It lives in the node-local
`<cache>/subs/` (`state.subs_dir`, `state.rs:705`; `cache/subs` per
`main.rs:5526`), keyed by `f<id>-s<n>-<size>-<mtime>.vtt` (`vtt_path`). On a
miss, `extract_vtt` (`subtitles.rs:2186`) spawns one `ffmpeg -i /dev/fd/3
-map 0:s:N -f webvtt` — a full demux of the source, started at byte zero.
The module's own constants say what that costs: `EXTRACTION_TIMEOUT = 600 s`
because "a cold extraction is a full-source read … has legitimately taken
~180 s" (`:29–35`), and `SIDECAR_JOIN_BUDGET = 5 s` because extracting one
track from file 5208 "read the entire file to produce 18,866 bytes, measured
at 402 s on m6" (`:51–68`). It is consumed by the text endpoint
(`http/stream.rs:2496`, unbounded join), the HLS subtitle rendition
(`http/hls.rs:10937`/`11116`, warms in the background and answers an empty
segment), the burn path (`transcode.rs:15646`, `17211`, `19376` — the last
with the 5 s budget), and offline packaging.

**The burn sidecar** (`ensure_burn_file`, `subtitles.rs:1044`): a
subtitle-only Matroska for burned transcodes, same full read, same cache
directory.

**The subtitle-source store** (`crates/plurxd/src/subtitle_source.rs`,
`<cache>/runtime/subtitle-source-v1/f<id>/`): PGS tracks kept by the
fragment-index pass (`subtitle_ride_along.rs`), read by two consumers —
`Consumer::Overlay` and `Consumer::Burn` (`subtitle_source.rs:147`). It is
**PGS only**: `plan()` takes `pgs_ordinals_from_probe` (`fragindex.rs:1504`),
the tee is `-c:s copy` into `f=sup` and `f=framecrc` slaves
(`subtitle_ride_along.rs:838–863`). Text tracks never enter it, and the VTT
sidecar never reads it. It is **node-local and best-effort by design** (PGS
plan §6.6): a node that hydrated its fragment index from a peer never ran the
pass and has no artefact; `lookup` classifies that miss as `hydrated_only`
(`subtitle_source.rs:410`) precisely so the miss rate could be measured.

**The seek windows** (`warm_vtt_window`, `subtitles.rs:1953`; the R-M3
`SessionWindow` owner): a per-session partial extraction that bridges the
head of playback while the whole track is produced. It "declines itself past
the midpoint of the file, where it would read the same bytes as the whole
track" (`hls.rs:11432`). Windows exist *because* the whole track is slow;
they are not the fix and this plan does not touch their owner registry.

**What the cluster knows about any of this: nothing.** The analysis queue
(`analysis_requests`, `plurx-core/src/store/fragment_index_cluster.rs:88`)
admits two components, `fragment_index` and `skip_markers` (the CHECK
constraint at `:306` and `hiqlite_fragment_index_cluster.rs:313`;
`request_file_analysis_for_identity` refuses anything else,
`state.rs:4140`). The job lease, the per-node slots
(`media:fragment-index:{slot}`, `state.rs:7294`), the peer transport and
`hydrate` (`fragment_index_cluster.rs:886`, `GET
/internal/media/fragment-index/{cache_key}` served by
`http/internal_media.rs:83`) — all of it is for fragment indexes. The
verified shared-cache mount (`shared_cache.rs`) does not cover `subs`.

### 2.2 What the viewer sees, and why

When a session start needs a sidecar (a burn with a text track), the start
waits 5 s, then `session_start_error` maps `subtitle sidecar is still being
built` to a 503 `startup_timeout` with `Retry-After`
(`http/hls.rs:3937–3955`). Every client's create-retry ladder treats that
code as `preparing` — `create_503_not_yet` in
`web/playback-policy.js:70`, `PlayerController.swift:1591`,
`PlaybackPolicy.kt:185` — and shows the wait surface. That surface is
honest: the serving node is reading a 30–80 GB file to get a few kilobytes
of text, and nothing else in the cluster is aware it is happening.

When an HLS session turns a track on mid-play, the rendition answers an
empty `no-store` segment while `warm_vtt` runs on that node in the
background; the player re-fetches until the sidecar exists. On a large remux
that is minutes of "selected but not showing", the class of report in
[SUBTITLE-RELIABILITY-ASSESSMENT.md](SUBTITLE-RELIABILITY-ASSESSMENT.md).

### 2.3 Fleet evidence, 2026-09-24

Read from a copy of nynuc's hiqlite state machine and the three nodes'
cache directories (deploy key; nuc3 is a learner and holds no media cache).

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
| subtitle-source store directories | nynuc 1 · m6 1 · **nuc4 46** — the PGS ride-along is producing on the node that has been running index passes, and nowhere else |
| store size | 13 MB on nynuc; 186 MB of `subs` sidecars |

Three things follow. The duplication is real and exactly what §6.6 predicted
(a fifth of nynuc's sidecars were also read in full on m6). The ride-along's
coverage is one node out of three, because the other two hydrate their
indexes. And the backlog is the library: 5,217 files, 28.8 TB, of which
4,892 already have an index — so **a backfill cannot ride on the index pass;
it is a deliberate second read of ~27 TB**, which at the fleet's measured
line rate (~200 MB/s per node, 402 s for 79.5 GB) is roughly 13 h of wall
time across three nodes, or two nights of idle time. That number is the
price of §3.6 and is why it is a switch, not a default.

## 3. Change

Five parts, each shippable alone, in dependency order. §5 gives them as
milestones.

### 3.1 Text tracks ride along (the producer learns `webvtt`)

`RideAlongPlan` gains text tracks. The held-fd probe that already yields
`pgs_ordinals` yields text ordinals too — every subtitle stream whose codec
is not `is_bitmap_subtitle` and is one ffmpeg's `webvtt` encoder accepts
(`subrip`, `ass`/`ssa`, `mov_text`, `webvtt`, `text`); `dvd_subtitle`,
`dvb_subtitle` and anything unknown stay out, as bitmap tracks other than PGS
do today. Ordinals remain hard `-map 0:s:N` from the live probe, never from
scan-time facts (PGS plan §6.2 #7).

The tee output carries a per-stream codec instead of one `-c:s copy`:

```
-map 0:s:1 -map 0:s:3 -map 0:s:0
-c:s:0 webvtt -c:s:1 webvtt -c:s:2 copy
-f tee "[select=0:f=webvtt:onfail=ignore]<stage>/s1.vtt|
        [select=0:f=framecrc:onfail=ignore]<stage>/s1.crc|
        [select=1:f=webvtt:onfail=ignore]<stage>/s3.vtt|
        [select=1:f=framecrc:onfail=ignore]<stage>/s3.crc|
        [select=2:f=sup:onfail=ignore]<stage>/s0.sup|
        [select=2:f=framecrc:onfail=ignore]<stage>/s0.crc|
        [f=null]-"
```

Everything after `pipe:1`, downstream of `copy_index_pipe_args`, so the
cluster cache key's pipeline digest is unchanged — the existing digest pin
test keeps proving it. The `webvtt` encode is what `extract_vtt` does today
(`-f webvtt` with ffmpeg's default subtitle encoder), so the stored `.vtt`
must be byte-identical to a sidecar the on-demand path would have produced
from the same source; experiment E1 (§6.1) establishes that before any code
is written, the way §6.2 of the PGS plan was established.

**Verdict for a text track.** `framecrc` counts packets; the `.vtt` is
parsed for cues. `kept` when the cue count equals the packet count the
`framecrc` companion recorded (a `webvtt` packet is one cue; E3 confirms the
1:1 holds for `ass` and `mov_text` after conversion, or the rule records the
measured ratio), the file parses as WebVTT, and it is under
`MAX_SIDECAR_BYTES` (8 MiB, `subtitles.rs:101`). `empty` for a real track
with zero packets. `malformed` and `transient` exactly as for PGS.

**Manifest.** `MANIFEST_VERSION` stays 1; `TrackEntry` gains
`kind: "pgs" | "text"` and `format: "sup" | "webvtt"`, both `serde(default)`
to `pgs`/`sup` so a manifest written by today's build reads unchanged. A
today's-build reader that meets a `text` entry treats the name it does not
understand as a miss ("a miss is never an error", module doc), which is the
only behaviour a mixed-version cluster needs for the one deploy window.

**Cost.** One more muxer per text track on a pass that is already reading
every packet. E4 measures it; the PGS measurement was "next to nothing" and
text packets are smaller than bitmaps. Disk: a dense SDH track is ~200 KB;
46,250 tracks is single-digit GB on a store capped at 32 GiB
(`MAX_STORE_BYTES`).

### 3.2 The VTT sidecar reads the store first (a third consumer)

`Consumer::Vtt` joins `Overlay` and `Burn`. `ensure_vtt_at`
(`subtitles.rs:1596`) consults the store **before** the flight: on a `kept`
text entry for the live `SourceStamp`, it copies the stored `.vtt` into
`<cache>/subs` under the existing key by an atomic rename and returns — no
ffmpeg, no flight, no memo. On `empty` it publishes the `WEBVTT\n\n`
sidecar the endpoint already knows how to serve. On a miss it falls through
to exactly today's path. Validity is the store's own rule (size + mtime at
use); the `.vtt` sha in the stored name is checked on copy.

This is the same "fallback inside the flight" shape #456 built for the burn
consumer, and it is what turns §3.1 into a viewer-visible change: on a node
that ran the riding pass, turning on a text track is a file copy. The
whole-track cache, its LRU, its negative memo and the window owner are
untouched — a stored answer simply arrives before any of them are needed,
and `warm_vtt_window` is not started when the whole track is already
published (the rendition handler already reads `whole_track_state` before
warming, `hls.rs:11435`).

### 3.3 Coverage becomes cluster-wide (publications and peer hydration)

The store stops being per node. Two additions:

**A replicated publication row.** `subtitle_source_publications(file_id,
source_size, source_mtime, node_id, manifest_sha256, tracks, published_at_ms)`,
primary key `(file_id, source_size, source_mtime, node_id)`. The producer
upserts it after `publish` succeeds — after the rename, never before, and the
row names the manifest's sha so a reader can tell a stale row from a
current one. `sweep` deletes the row when it removes the directory. This is
the subtitle analogue of `cluster_fragment_index_locations`, and it is what
lets a node — and the backfill in §3.6 — answer "does the cluster hold this
file's tracks?" from the store instead of asking every peer.

**Hydration.** `subtitle_source::lookup` on a local miss reads the
publication rows for the stamp, orders them by reachable media peer
(`membership.media_peers()`, the same filter `hydrate` applies), and fetches
`GET /internal/media/subtitle-source/{file_id}/{ordinal}` from the first
that answers — manifest first, then the one track the consumer asked for.
`PeerAuthMode::ExactRequest`, `PEER_DEADLINE` (8 s), a body cap of
`MAX_TRACK_BYTES`, sha verified before the bytes are staged; a mismatch or a
404 drops that row (best-effort forget, as `hydrate` does for a corrupt blob)
and tries the next. A hydrated track is published into the local store
exactly as the producer would publish it — private stage, rename, its own
publication row — so the next lookup on this node is local and the next
peer that asks has one more source. The handler is a sibling of the
fragment-index one in `internal_media.rs`: it serves only what its local
manifest names, never reads the source, and refuses the request when the
switch is off.

`hydrated_only` stops being a miss class the viewer pays for; it becomes the
counter that says how often hydration ran. `never_indexed` stays what it is:
a file no node has ever read, which is §3.4's job.

### 3.4 A `subtitle_source` analysis component — the pool does the read

The analysis queue gets a third component. Same table, same lease, same
claim sweep, same retry policy, same progress rows and lifecycle counters,
same Maintenance page — nothing new is invented for scheduling. What is new
is the work: **the ride-along without the index**, the same `RideAlongPlan`
argv with no `pipe:1` output, `[f=null]-` as the only sentinel, so one
`ffmpeg` reads the file once and keeps every subtitle track it has, text and
PGS, into this node's store, then publishes the row from §3.3.

- **Target.** `target_node_id = ''` — any node — the way `skip_markers`
  is claimed (`claim_analysis_request`, `hiqlite_fragment_index_cluster.rs:1467`).
  Fragment-index requests target the requesting node because the artefact
  is content-addressed per pipeline digest and every voter wants a local
  copy; a subtitle track is the same bytes whoever reads them and §3.3 moves
  them, so the job goes to whoever is idle. Learners never claim (the
  existing job authority rule).
- **Who enqueues.** Two callers. *Playback:* every consumer's store miss
  that is not `hydrated_only` (i.e. no publication row exists anywhere)
  enqueues `subtitle_source` for the file, priority `forced`, before it
  falls through. The consumer's own behaviour on that call is unchanged —
  the 5 s budget, the pending 503, the empty rendition segment — so no
  client contract moves. What changes is what happens in the next minutes:
  an idle node claims the request, reads the file, publishes; the
  requesting node's next lookup (the client's next retry, the rendition's
  next re-fetch) hydrates and serves. *Backfill:* §3.6.
- **Does the serving node still read the file itself?** Only when it has to.
  When the cluster queue is enabled and the enqueue succeeded, the
  on-demand flight is **not** started for that request — the miss is
  answered as it is today (pending / empty / wait), and the read happens on
  the pool. When the queue is disabled (`cluster_fragment_index_enabled()`
  false, a single-node install, or the switch in §3.7 off) or the enqueue
  fails, today's inline extraction runs unchanged. A request that reaches a
  terminal failure (`attempt_limit`, `stored_probe_invalid`) also releases
  the inline path, so a broken file degrades to exactly today, not to
  never. This is the one place a reviewer should look hardest: the serving
  node is a member of the pool and may be the idle one that claims its own
  request, which is fine — it is then reading the file once, as a job with
  a progress row, instead of once as an anonymous flight.
- **Schema.** The `component` CHECK on `analysis_requests` admits
  `('fragment_index','skip_markers')`. Admitting `subtitle_source` and
  adding `subtitle_source_publications` is one migration, **v45**, in the
  shape v42 used to add `skip_markers`: rename to `analysis_requests_v44`,
  recreate, copy, re-create the five indexes
  (`fragment_index_cluster.rs:295–330`), `MigrateFrom` arm, accepted-source
  list, the chain assertion, the `install_schema` object-count probe, and a
  `replicated_v45_…` contract test. A schema bump is a stop-the-fleet event
  on hiqlite (`schema_migration_action` refuses any other version on open),
  which the ansible `serial: 1` deploy already handles — but it means this
  milestone ships in a deploy of its own and is called out in §6.3.
  `request_file_analysis_for_identity`'s `matches!` at `state.rs:4140` and
  `resolve_analysis_request`'s dispatch at `:7799` gain the arm;
  `pipeline_version` for the component is the ride-along's own
  `MANIFEST_VERSION` plus the ffmpeg engine digest, so an engine change
  re-requests.
- **Dedup.** `analysis_requests_one_active_source` already keys on
  `(file_id, source_size, source_mtime, component, target_node_id)`, so a
  storm of playback misses across nodes for one file collapses to one queued
  request. A `ready` row with a publication behind it is the tombstone that
  stops re-requests; a file whose publication was swept re-requests on its
  next miss because the lookup finds no row.
- **What the job does not do.** It does not build a fragment index (that is
  the other component and stays targeted), it does not compile PGS PNGs
  (lazy and LRU, PGS plan §6.5), and it does not run on MPEG-TS sources
  (the ride-along's exclusion stands; those files stay on the inline path).

### 3.5 The order of preference, in one place

A consumer asking for track `n` of file `f` on node `N`:

1. `N`'s `<cache>/subs` has the sidecar → serve (today).
2. `N`'s store has a `kept` entry for the live stamp → copy, serve (§3.2).
3. A publication row names a reachable peer → hydrate, publish locally,
   serve (§3.3).
4. No row anywhere, queue enabled → enqueue `subtitle_source`, answer
   "pending" as today; the pool reads; step 3 succeeds on the retry (§3.4).
5. Queue disabled, enqueue failed, or the request terminally failed →
   today's inline extraction on `N`, with its memo and its window (§2.1).

Each step is bounded by the budget the caller already carries; none adds a
wait a client can observe, and step 5 is byte-for-byte today's behaviour.

### 3.6 Backfill — an idle pool fills the store ahead of play

A discovery pass, `discover_subtitle_sources`, under a cluster lease
`media:subtitle-source:backfill` (one holder cluster-wide, the way the
`provider:artwork` lease at `state.rs:4943` fences artwork repair), run from the same tick as `discover_cluster_fragment_indexes`
and subject to the same idleness rule (`transcode.pretranscode_worker_idle()`
on the *enqueuing* node, and the claim side already refuses when the worker
is busy or a foreground playback signal is up —
`analysis_hash_stop_signal_observes_foreground_playback`). Each pass:

- selects up to `BACKFILL_PER_TICK` (proposed 8) files that have at least
  one eligible subtitle stream in scanner facts, are not MPEG-TS, have no
  `subtitle_source_publications` row for their live stamp, and have no
  active or terminal `subtitle_source` request — newest `scanned_at` first,
  so recently imported titles are covered before the long tail;
- enqueues them as `subtitle_source`, priority `normal`, trigger
  `background`, target any.

It is **off by default**. It is turned on from the Developer tab's enable
section (§3.7), and it runs only in the idle windows the analysis queue
already respects. At the fleet's line rate the whole library is about two
idle nights (§2.3); the pass never reads a file itself, so its own cost is
one small query per tick. A file the backfill enqueues and a file a viewer's
miss enqueues are the same row — the unique index makes them one — and a
viewer's `forced` request sorts ahead of the backfill's `normal` ones
(`claim_analysis_request`'s ORDER BY already does this).

### 3.7 Attribution, and a way to stop it

Real disks for hours, so all of it is visible and stoppable from inside the
product, per the standing rule.

- **The analysis progress row** (Maintenance → Analysis, already the home of
  fragment-index and skip-marker jobs) shows `subtitle_source` jobs as
  *"reading `<title>` on `<node>` for 4 text + 1 PGS tracks — 31.2 of 79.5
  GB"* with the same stage/progress fields `update_analysis_ride_along`
  (`state.rs:4330`) already feeds, and the same cancel the page offers
  fragment-index jobs. A cancelled job charges no attempt and leaves no
  publication.
- **The Developer tab** gains one item, `subtitle_cluster_sources`, whose
  switch covers §3.2–§3.6 (the ride-along's own `subtitle_stored_sources`
  keeps covering §3.1's producer, so a wrong text artefact can be taken out
  of service without turning off the queue, and vice versa). Its enable
  section, advisory and never blocking the switch, in the shape of
  `subtitle_stored_sources` (`developer.rs:702–800`):
  - *the analysis queue is enabled* — `cluster_fragment_index_enabled()`;
  - *this build's schema is v45 on every voter* — read from the same place
    `schema_migration_action` reads it; a mixed cluster shows which node is
    behind;
  - *at least one reachable media peer* — `media_peers()`, so a single
    node sees plainly that hydration cannot happen and only §3.2 applies;
  - *the store is on a local filesystem with N GB free* — the ride-along's
    existing checks, re-reported;
  - *backfill* — a second switch under the first, `subtitle_backfill`, with
    the lease holder's node id, files enqueued this process, files
    remaining (`files with eligible streams − publications`), and the
    estimated remaining bytes.
- **Counters** on `/metrics` (from `store_metrics_loop`, never the scrape
  path): lookups by outcome now including `hydrated`, requests enqueued by
  trigger (`playback`/`backfill`), jobs by verdict, bytes read by jobs,
  hydration bytes served and fetched.
- **Logs**: one `info` per job start and end with file, node, tracks, bytes
  and duration, so `journalctl` on a node explains any long read on its own.

### 3.8 What the client sees when it is done

Nothing new: no client change is in this plan. The difference is that the
`preparing` surface and the empty rendition segment become rare — a title
the backfill covered never shows them for a subtitle, a title it has not
covered shows them for as long as an idle node needs to read the file, on
the first request only, cluster-wide. That is the acceptance in §6.2.

## 4. Guardrails (non-goals)

- **The fragment-index cache key does not move.** Every argv change is
  downstream of `copy_index_pipe_args`; the digest pin test is the proof and
  stays.
- **No handler awaits an extraction or a hydration.** The rendition handler
  keeps publishing the empty segment; the start path keeps its 5 s join;
  hydration runs inside the same detached flight the consumer already
  spawns, under `PEER_DEADLINE`. AVPlayer's two-second subtitle budget is the
  reason (SUBTITLE-RELIABILITY-ASSESSMENT).
- **No OCR, no PGS→text**, no change to which track auto-selects
  (`tracks.rs` `forced_or_default`), no change to the HDR burn guard, no PNG
  precompilation.
- **No `-fs`, no exit-code reclassification, no `?` on a subtitle map, no
  scan-time ordinals** — every rule in PGS plan §6.2 holds for text.
- **Not a shared-cache-mount feature.** `shared_cache.rs`'s verified mount
  could carry the store one day; this plan moves bytes over the peer
  transport the fragment index already trusts, and stays correct when no
  mount exists.
- **The session does not deploy.** Milestones that need fleet evidence say
  so in §6 with the GPT prompt; nothing here is marked done from a device
  observation nobody made.
- **The window owner registry (R-M3) is untouched.** Windows are started
  less often; their rules do not change.

## 5. Milestones

One draft PR owns the plan (work-board rule 4); milestones are logical
commits and Execution-log rows. Each names its tests; the fast lane is what
runs before merge (Paul's 2026-09-17 cadence), and `make unit` locally for
the Rust changes.

### M0 — Experiments E1–E5 (§6.1), recorded in this document

No product code. The results go in a `### 6.1 results` subsection of this
file in the plan PR, the way PGS plan §6.2 recorded its table. **If E1 fails
— the tee cannot carry per-stream codecs, or the `.vtt` is not
byte-identical — §3.1 is redesigned before anything else is built** (the
fallback is a second `ffmpeg` sharing the held fd, which is a second read
and would need its own justification).

### M1 — Text tracks in the ride-along and the `Vtt` consumer (§3.1, §3.2)

Producer: text ordinals from the probe, per-stream codecs, `webvtt` slaves,
the text verdict, manifest fields. Consumer: `Consumer::Vtt`, the store read
in `ensure_vtt_at`, the `empty` short-circuit, the copy-under-existing-key
publish. Tests: the digest pin; fixtures for a `subrip`, an `ass` and a
`mov_text` track (first cue not at zero, on a zero and a non-zero source
start — the fixture rule from PGS plan §6.7); a text track with a corrupt
packet; the packet/cue count rule; a manifest written by today's build still
reads; a `text` entry is a miss for a reader without the field; the
whole-track cache never starts a flight when the store answers; the window
warm is not started when the store answered. Ships behind
`subtitle_stored_sources`, which is already the switch for the producer.
**Deployable alone**: on nuc4 (46 stores) it changes what a text selection
costs today.

### M2 — Schema v45: the component and the publication table (§3.4 schema)

Migration, contract test, `MigrateFrom`, the chain assertion, the probe. The
component is admitted but nothing enqueues it yet. The single stop-the-fleet
deploy. Tests: `replicated_v45_admits_subtitle_source_and_publications`, the
placeholder-order census over the new statements (the class that took the
fragment index down for three days — `every_replicated_placeholder_is_introduced_in_order`
must cover the new module, not just its original one).

### M3 — Publications and hydration (§3.3)

Producer writes the row after publish; sweep removes it; `lookup` hydrates;
the internal handler. Tests: a row is written only after the rename (a
producer that fails between stage and rename leaves no row); a stale row
(sha mismatch) is forgotten and the next peer tried; a 404 peer is skipped;
a hydrated track republishes locally with its own row; the handler refuses
with the switch off and serves nothing its manifest does not name; body cap
enforced; the two-node harness (`plurx-cluster-check` has no scenario for
this — one is added, driving a lookup on node B for a track node A
published).

### M4 — The `subtitle_source` job and the playback enqueue (§3.4, §3.5)

The worker arm, the no-index argv, the enqueue from each consumer's miss, the
"do not start the inline flight when the pool has it" rule and its three
release conditions, dedup, the terminal-failure release. Tests: the argv
has no `pipe:1` and ends in the sentinel; a job publishes a row and a store
directory; a miss enqueues exactly one request across three concurrent
consumers; the inline flight does not start when the enqueue succeeds and
does when the queue is disabled; a terminal `failed` row releases the inline
path; a `forced` request claims ahead of `normal`; a learner never claims
it; the progress row carries tracks and bytes; cancel leaves no publication.
Ships behind `subtitle_cluster_sources`, default **off**.

### M5 — Backfill and the Developer section (§3.6, §3.7)

The discovery pass under its lease, the per-tick bound, the eligibility
query, the two switches and the enable section, counters. Tests: the pass
enqueues nothing when the worker is busy; enqueues at most `BACKFILL_PER_TICK`;
never enqueues a file with a row or an active/terminal request; newest first;
the lease is exclusive across two nodes; the Developer item renders each
requirement's status from the real probes (the `ui-structure.golden` will
move — regenerate it, do not hand-edit). Default **off**.

## 6. Verification and rollout

### 6.1 Experiments before M1 (on nuc3, synthetic sources muxed with `-copyts`, as PGS plan §6 did)

| # | question | pass condition |
|---|---|---|
| E1 | can one `tee` output carry `webvtt`-encoded and `copy` streams to `f=webvtt` and `f=sup` slaves at once? | ffmpeg 8.0.1 exits 0 with all slaves written; the index on `pipe:1` is byte-identical to a pass with no ride-along; the `.vtt` is byte-identical to `extract_vtt`'s from the same source |
| E2 | does `ass`→`webvtt` in the tee equal `extract_vtt`'s `ass`→`webvtt`? | byte-identical, including the styling loss both already have |
| E3 | is one `framecrc` packet one WebVTT cue for `subrip`, `ass`, `mov_text`? | counts equal, or the deterministic ratio is recorded and used |
| E4 | what does a 12-text + 3-PGS ride cost on an index pass? | CPU and RSS within noise of the bare pass; wall time unchanged |
| E5 | a text slave that fails mid-stream (corrupt packet, `ENOSPC` on the stage) | the index completes, exit 0, the `.vtt` is `malformed`/`transient` by the verdict rules, nothing else is affected |

### 6.2 Acceptance — deliberately not a matrix

Per Paul's 2026-09-22 rule, the bar is the narrow real check, not a grid.
After M4 is deployed with `subtitle_cluster_sources` on and the backfill
**off**:

1. Pick one title per client (Apple TV, Android tablet, web) that has never
   been played on the serving node and has no publication row (Maintenance
   shows it). Turn on a text track. Expected: the pending/empty answer, then
   a `subtitle_source` job appears on Maintenance **on another node**, then
   the track shows on the client's next retry — and `journalctl` on the
   serving node shows no `ffmpeg … -f webvtt` of its own.
2. Play the same title from a second node. Expected: the track shows inside
   one poll, the serving node's log shows a hydration, no job runs.
3. Turn the switch off and repeat 1 on a third title. Expected: exactly
   today's behaviour — an inline extraction on the serving node.

After M5, with the backfill on overnight: Maintenance shows the remaining
count falling; the next morning, 1 is repeated on three random titles and
step "a job appears" is replaced by "no job runs, no hydration runs — the
store answered locally or from a peer".

That covers the risk (the wrong node reads, or nothing reads, or the old
path silently stays), and it is three titles, not thirty.

### 6.3 Rollout

- M1 deploys with the ordinary train; it is safe with the store empty.
- M2 is the schema bump: **all voters in one `serial: 1` deploy**, nothing
  else in it, and a note in `STATUS.md` naming v45 the way earlier bumps
  were named. A mixed-version cluster refuses to open — that is the existing
  contract, not a new risk, but the deploy is not to be split across days.
- M3–M5 deploy with the ordinary train, both switches off, and are turned
  on from the Developer tab: `subtitle_cluster_sources` first, the
  acceptance in §6.2 run, then `subtitle_backfill` for one night with the
  Maintenance page watched.
- Rollback is the switch, on every milestone. Nothing here deletes an
  existing sidecar.

### 6.4 What the session cannot do — the GPT prompt

The experiments (§6.1) can be run from a session over the deploy key on
nuc3. The deploy and §6.2 cannot. For those:

> On the plurx fleet (nynuc, m6, nuc4; ansible `media/deploy.yml -e
> sync=false`, serial), deploy the build carrying M1–M4 of
> `docs/clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md`. M2 is a hiqlite schema
> bump to v45: deploy all three voters in the same run and confirm each
> reports `schema 45` on `/readyz` before moving on. Then in Settings →
> Developer turn on `subtitle_cluster_sources` (leave `subtitle_backfill`
> off), and run §6.2 steps 1–3 on the Apple TV, the TCL tablet and Safari.
> For each: the title, the serving node, what Maintenance → Analysis showed
> and on which node, whether the track appeared and after how many retries,
> and `journalctl -u plurx --since -10m | grep -E 'ffmpeg|subtitle_source|hydrat'`
> from the serving node. Paste the results into the plan's Execution log as
> one row per step.

## 7. Open questions — rulings that are Paul's

1. **Backfill scope.** §3.6 enqueues every file with an eligible track —
   28.8 TB, ~two idle nights. The alternatives are *watched-first* (files
   with any playback history, then the rest) or *text-only* (skip PGS, which
   the index pass already rides for newly indexed titles). Proposed: all,
   newest-scanned first, because the switch is off by default and the queue
   yields to playback anyway.
2. **Any-node vs. requester-first.** §3.4 targets any node. A variant
   prefers the requesting node when *it* is idle (no hydration needed
   afterwards) and falls back to any. Proposed: any-node, because the
   requesting node is by definition serving a session and the whole point
   is to move the read off it; the hydration cost is kilobytes.
3. **Whether the store should also carry the burn `.mks` for text tracks.**
   Today `ensure_vtt_file` derives simple-codec burns from the VTT and
   `ass` burns from the source. With the `.vtt` stored, simple-codec burns
   are covered; `ass` burns still read the source. A `matroska` copy slave
   per `ass` track would close that. Proposed: not in this plan — `ass` is
   1,189 of 46,250 tracks, and the burn path is on its way out for clients
   that draw overlays.
4. **Publication table vs. peer fan-out.** §3.3 adds a replicated table.
   With four nodes, asking every reachable peer on a miss (≤3 small GETs)
   would avoid the table but not the schema bump (§3.4 needs one anyway),
   and the backfill needs the table to know what is missing without a
   fan-out per file. Proposed: the table.
5. **`subtitle_cluster_sources` default.** Off, per this plan, until §6.2 has
   been run once. If Paul wants it on by default at M4 so the fleet starts
   healing without a visit to the Developer tab, that is a one-line change
   and the enable section still reports the requirements.

## 8. Anchors, to re-verify at build time (all at `936157b4b`)

| Thing | Where |
|---|---|
| sidecar module doc, `EXTRACTION_TIMEOUT`, `SIDECAR_JOIN_BUDGET`, `SIDECAR_PENDING_PREFIX` | `crates/plurxd/src/subtitles.rs:1–86` |
| `MAX_SIDECAR_BYTES` | `crates/plurxd/src/subtitles.rs:101` |
| `ensure_burn_file` | `crates/plurxd/src/subtitles.rs:1044` |
| `ensure_vtt_with` / `ensure_vtt_bounded` / `ensure_vtt_at` | `crates/plurxd/src/subtitles.rs:1557–1600` |
| `warm_vtt_window` | `crates/plurxd/src/subtitles.rs:1953` |
| `extract_vtt` argv | `crates/plurxd/src/subtitles.rs:2186–2197` |
| store module doc, `STORE_DIR`, `MAX_TRACK_BYTES`, `MAX_STORE_BYTES` | `crates/plurxd/src/subtitle_source.rs:1–68` |
| `Consumer` (Overlay, Burn) | `crates/plurxd/src/subtitle_source.rs:147` |
| miss classes incl. `hydrated_only` | `crates/plurxd/src/subtitle_source.rs:405–415`, `:668` |
| `lookup`, `lookup_snapshot` | `crates/plurxd/src/subtitle_source.rs:495`, `:840` |
| ride-along module doc and rules | `crates/plurxd/src/subtitle_ride_along.rs:1–33` |
| `plan()` | `crates/plurxd/src/subtitle_ride_along.rs:684` |
| `RideAlongPlan::args` / `tee_spec` | `crates/plurxd/src/subtitle_ride_along.rs:838–863` |
| `pgs_ordinals_from_probe` call, plan attach, ride-along in the pass | `crates/plurxd/src/fragindex.rs:1504`, `:1690`, `:1856`, `:2000` |
| `PEER_PATH_PREFIX`, `PEER_DEADLINE`, `hydrate` | `crates/plurxd/src/fragment_index_cluster.rs:21–22`, `:886` |
| `SourceStamp` | `crates/plurxd/src/fragment_index_cluster.rs:801` |
| fragment-index peer handler | `crates/plurxd/src/http/internal_media.rs:83`; route `http/mod.rs:441`, `:1801` |
| `request_file_analysis_for_identity` (component gate, target) | `crates/plurxd/src/state.rs:4132–4192` |
| discovery slots | `crates/plurxd/src/state.rs:7291–7315` |
| `resolve_analysis_requests` (per-pass bound, idleness) | `crates/plurxd/src/state.rs:7572–7600` |
| `skip_markers` dispatch arm | `crates/plurxd/src/state.rs:7799` |
| `update_analysis_ride_along` | `crates/plurxd/src/state.rs:4330` |
| `analysis_requests` schema, CHECK, indexes; v42 rebuild | `crates/plurx-core/src/store/fragment_index_cluster.rs:88–115`, `:295–330` |
| hiqlite twin, claim SQL (`skip_markers AND target_node_id = ''`) | `crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs:167`, `:313`, `:1397–1475` |
| schema versions, `AUTH_SCHEMA_VERSION = 44`, `schema_migration_action` | `crates/plurx-core/src/store/hiqlite.rs:73–108`, `:4495` |
| `media_peers` | `crates/plurx-core/src/cluster/membership.rs:6189` |
| pending → `startup_timeout` | `crates/plurxd/src/http/hls.rs:3937–3955` |
| rendition: whole-track state read before warm; window past midpoint | `crates/plurxd/src/http/hls.rs:11415–11450` |
| text endpoint | `crates/plurxd/src/http/stream.rs:2496` |
| burn with the 5 s budget | `crates/plurxd/src/transcode.rs:19376–19380` |
| Developer item `subtitle_stored_sources` and its enable section | `crates/plurxd/src/http/developer.rs:690–800` |
| Developer card markup | `crates/plurxd/src/web/pages/settings-developer.js:287–320` |
| clients' `startup_timeout` ladders | `web/playback-policy.js:70`, `clients/apple/Sources/PlayerController.swift:1591`, `clients/android/.../player/PlaybackPolicy.kt:185` |
| `subs_dir` | `crates/plurxd/src/state.rs:705`, `main.rs:5526` |

## Execution log

Executing sessions append one row per milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-24 | claude-fable-5-1 | https://claude.ai/code/session_01LUY4Gc3ZFwF8xzj6Eg9Dy1 | Plan | — | Written from `936157b4b` and the 2026-09-24 fleet read in §2.3. Awaiting adversarial review; nothing built. |
