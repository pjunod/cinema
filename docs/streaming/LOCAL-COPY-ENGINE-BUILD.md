# Local copy on the one engine — build plan for the rest of item 6

**Status:** open — step 1 claimed, steps 2–8 not started ·
**Executes:** item 6 of [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md)
under the [A1/B1 build contract](PLAYBACK-START-A1-B1-BUILD.md) ·
**Written:** 2026-11-12 against `item6` @ `92c0e28`

Any agent may work a step. Claim it by adding a row to the Execution log at
the end, with your exact model identifier and session, before you start.
Work the steps in order unless a step says it is independent. Every path is
under `crates/plurxd/src/` unless it starts with `plurx-core`; re-verify
names by function, not by line.

**Stop and flag** if a step seems to need any of: a second claim service or
a third copy engine; a feature gate, timer, poll or watchdog; a local
viewer made from a Sharing principal; a start that waits on an Analysis
write; removing a serving path before its replacement has served the same
titles on the fleet.

## 1. Objective

One copy engine cuts what a local viewer is served and, in the same read,
the canonical rows of the title's v3 copy index. Today only shared (Source)
playback does that. Local playback is served by two other producers and its
index is built by a second read.

Three properties hold after every step, not only at the end:

- Live HLS plays every title. The rolling copy path stays until the engine
  has served the same starts on the fleet (step 8 removes it, nothing
  earlier does).
- A leader restart does not interrupt playback. No step adds a Store write
  to the path between a start request and its first segment, or to segment
  serving.
- There is one copy engine. Every step moves work onto
  `copyrange` or leaves it where it is; none adds a producer.

## 2. What exists

| Path | Producer | Rows for the index |
|---|---|---|
| Shared (Source) copy | `copyrange::CopyRangeProduction`, started by `transcode/source_actor/growing.rs::prepare_source_growing` | Committed ranges, sent as `CompletedCopyRange` under `CopyObservationAuthority::Source` |
| Local cold copy | The rolling copy segmenter (`plurx_core::transcode::copy_pipe_args`, `copyseg`) | None |
| Local indexed copy | Plan-verified VOD generations (`vod/generation.rs::spawn_generation`) from an existing index | None |
| Background | `copyrange::fill_copy_gap` under an Analysis phase (`copy_analysis/work.rs::fill_gaps`) | `AnalysisCopyRows` |
| Content analysis | The fragment-index pass (`fragindex.rs`, one whole-file `ffmpeg` with ride-alongs such as `subtitle_ride_along`) | Node-local index rows only |

Item 6 step 1 (#1535) added `CopyObservationAuthority::Local` and a
row-less `CopyIndexDemand`: a local copy start without a v3 index asks, and
the Analysis worker builds the index by `fill_copy_gap` when the source is
idle. That is the standalone form of the copy-index job.

The growing engine is bound to the Source worker at four points:

| # | Point | Where | Source-only today |
|---|---|---|---|
| P1 | Range storage identity | `copyrange/storage.rs::CopyRangeFiles::create` | Takes a `SourceDispatchAssignment`; payload directory is `source-copy-ranges/<incarnation>/<range>`; manifests record the Source incarnation, request and owner key |
| P2 | Generation ownership | `copyrange/pipes.rs::spawn_source_generation`, `CopyGenerationOwner` | Variants are `Source { owners, dispatch, authority }` and `Analysis { phase }` |
| P3 | Input admission | `growing.rs` via `foreground_source_request` and `SourceInput::open_foreground_held` | The request is keyed by the Source incarnation |
| P4 | Serving | `growing.rs::SourceGrowingBacking`, `CopyRangeFiles::playlist` | The range is served by the Source viewer actor's own playlist, not a VOD session's plan |

## 3. Steps

### Step 1 — Durable demand

A demand survives a restart.

- **Owner surfaces:** `copy_analysis/work.rs`, `copy_analysis.rs`;
  `plurx-core/src/store/analysis_coordination.rs` and the trait in
  `store/background_jobs.rs`.
- **Change:** the worker enqueues and establishes a demand's coordination
  request on its first attempt, before the foreground-pressure check, and
  remembers that it did. On the first pass of a process, before stranded
  requests are retired, it reads this node's queued `local-play.` requests
  with their roles and turns each back into a demand: the file from the
  catalogue, the object version from a stat that must match the role, the
  copy recipe as the candidate whose identity matches the role. Rows that
  cannot be rebuilt are retired with the rest.
- **Acceptance:** a title played locally before a restart is indexed after
  it without being played again; a demand whose file changed is dropped;
  no write is added to any start.
- **Tests:** store listing (SQLite); recovery into a held demand and the
  drop cases; persist-once.

### Step 2 — `local_playback` trigger (independent of 3–8)

- **Owner surfaces:** `plurx-core/src/store/fragment_index_cluster.rs`
  (schema and both `valid_request`s), `store/sqlite/fragment_index_cluster.rs`,
  `store/hiqlite_fragment_index_cluster.rs`, the migration lists of both
  backends; `copy_analysis/work.rs::coordination_request`;
  `web/pages/analysis.js`; `http/analysis.rs` filters if they enumerate
  triggers.
- **Change:** one schema version that rebuilds `analysis_requests` with
  `CHECK (trigger IN ('admin','background','playback','local_playback'))`,
  carrying every index and trigger of the table across, on SQLite and
  hiqlite. Demand-driven requests then enqueue with that trigger, and the
  `local-play.` id stays as the carrier of user and playback. Recovery
  (step 1) selects by trigger instead of by id prefix. `playback` keeps its
  meaning of a waiting viewer; nothing reads `local_playback` as one.
- **Acceptance:** an old binary reads the new table (rollback defined
  before the writer is enabled, as the contract's step 1 requires); the
  Analysis page and `GET /api/v1/analysis/jobs` show the stored trigger.
- **Tests:** migration from the previous version with live rows in every
  state, both backends (`make unit-core`); every trigger and index of the
  table still present by name; enqueue accepts the value and refuses a
  fifth.

### Step 3 — The copy index rides the content-analysis pass

Paul's standing rule: every analysis job rides along with the analyze
pass, reading the same unfiltered input, and also runs standalone. The
standalone form exists (step 1 of item 6). This step is the ride-along.

- **Owner surfaces:** `fragindex.rs` (`index_pass`, `shared_index_argv`,
  `build_riding_shared`), a new `copy_ride_along.rs` beside
  `subtitle_ride_along.rs`, `copy_analysis.rs` (a third observation source),
  `copyrange/observed.rs` and `census.rs` for the shared row and
  inspection extraction.
- **Change:** the fragment-index pass already reads the whole file once and
  emits the video-only canonical pipe on `pipe:1`. The ride-along appends,
  after `pipe:1` and never inside `copy_index_pipe_args*` (the rule
  `subtitle_ride_along` keeps, so no cluster cache key changes), the packet
  clock the engine reads on descriptor 7. The pass feeds its canonical
  output through the same row extraction and per-row inspection
  (`CopyRowInspection`, HDR10 facts, HEVC range inspection) the engine's
  observed mux uses, and hands the worker one whole-title observation under
  the pass's own Analysis phase. The worker seals and publishes as for any
  complete set. A pending row-less demand for the same source is then met
  with no second read.
- **Acceptance:** a title the analyze pass indexes gets a v3 head from that
  same read; its blob equals the one `fill_copy_gap` seals for the same
  file and recipe; the node-local index and its cluster cache key are
  byte-identical with and without the ride-along; a failed ride-along
  costs the index nothing.
- **Tests:** row and blob equality against `gap_fill_tests`' whole-title
  fill on the HEVC and H.264 fixtures; argv digest unchanged; a ride-along
  failure leaves `IndexOutcome::Built`.

### Step 4 — A local owner at P1 and P2 (no behaviour change)

- **Owner surfaces:** `copyrange/storage.rs`, `copyrange/pipes.rs`,
  `copyrange/census.rs`, `source_generation.rs`.
- **Change:** `CopyRangeFiles::create` takes a range owner enum:
  `Source(SourceDispatchAssignment)` or `Local { session, viewer:
  LocalCopyAuthority }`. The payload directory and manifest binding come
  from the owner (`local-copy-ranges/<session>/<range>` for local).
  `CopyGenerationOwner` gains `Local`, owned by the local VOD session's
  producer slot and admission permit, with the same join-before-release
  discipline as the other two. Source and Analysis callers are unchanged.
- **Acceptance:** Source playback and gap fill behave exactly as before; a
  local owner can create, commit and retire a range in a test.
- **Tests:** the existing Source and gap-fill suites; storage tests for a
  local owner's directory, manifest and retirement; the producer-ownership
  inventory updated.

### Step 5 — Local input admission (P3)

- **Owner surfaces:** `source_input.rs`, `source_custody.rs`,
  `transcode/manager` foreground request helpers.
- **Change:** a local range opens its Inputs with a foreground request
  keyed by the local playback, under the read bank and watermarks the
  Source range uses. One foreground input is charged once: the local
  session's existing source custody is the range's, not a second one.
- **Acceptance:** background work pauses under a local range exactly as
  under local playback today; no double charge.
- **Tests:** custody and pressure tests with a local range as the holder.

### Step 6 — Local serving attachment (P4; build contract step 4)

- **Owner surfaces:** `vod/session.rs`, `vod/shared.rs`,
  `vod/serve/create.rs`, the retained-manifest path, the A1-N publication
  helper.
- **Change:** a local VOD session can attach to a growing range as its
  rendition: one coherent snapshot backs plan, bytes and playlist (contract
  §5.1), the target is fixed before publication (§5.2), held EVENT views
  stay stable (§5.3), and admission is A1-N's. The first use is the cold
  local copy start, the case the rolling path serves today. Create still
  answers without awaiting an Analysis write. The rolling path remains the
  answer for any start the range refuses (unknown clock, oversized GOP,
  changed init), by the engine's own typed refusals.
- **Acceptance:** the contract's 48-second native rate-1 case; a cold
  local start at 0 and at 90 minutes plays from the range on web, Apple and
  Android; a leader restart during such a session does not interrupt it;
  every refusal falls to the rolling path and plays.
- **Tests:** attachment, snapshot coherence and refusal fallbacks in
  `vod/tests`; client attachment and seek tests named by the contract's
  step 5. Fleet and device acceptance are owed separately.

### Step 7 — Local ranges commit observations

- **Owner surfaces:** the local range's actor from step 6,
  `copy_analysis.rs`, `copy_analysis/work.rs`.
- **Change:** after the strict rendered milestone, each committed local
  checkpoint is sent as a `CompletedCopyRange` under
  `CopyObservationAuthority::Local`, as `growing.rs` does for Source. The
  worker already folds a Local range. A start the range serves no longer
  sends a row-less demand.
- **Acceptance:** a partially played local title publishes after
  background filling of its gaps, with no second read of what was played.
- **Tests:** a local range's rows fold with an Analysis fill into the
  whole-title set (extend `gap_fill_tests`); contradiction with a healthy
  head still quarantines.

### Step 8 — Indexed copy on the engine; retire the rolling producer

- **Owner surfaces:** `vod/generation.rs`, `vod/plan.rs`, `copyseg`,
  `transcode/manager` rolling paths, `tests/playback/rolling-producer-owners.toml`.
- **Change:** an indexed local copy start runs a seeked range of the engine
  verified against the index instead of a plan-verified generation. Only
  when steps 6–7 have fleet acceptance and this path serves every title
  the rolling path served is the rolling copy producer removed.
- **Acceptance:** the acceptance list of
  [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md) for A1/B1; no title
  that played before fails to play.
- **Tests:** seeked and sequential rows equal (contract step 3); the
  inventory shrinks by the rolling producer's entries.

## 4. Guardrails

- A local owner never holds a Source assignment, gate or grant, and a
  Source owner never holds a `LocalCopyAuthority`.
- Receivers stay relays; only an origin node that can open the held object
  runs the engine.
- Processed Dolby Vision identity, Live TV and the encoded paths are not
  touched.

## Execution log

One row per step, added when it is claimed and updated when it lands.
**Model** is the runtime's exact model identifier; **Session** is the
session id or URL.

| Date | Step | Model | Session | Branch / PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-11-12 | 1 Durable demand | claude-fable-5-2 | https://claude.ai/code/session_01BCWL9ypnQEiQUyRaZtMhh3 | `item6b` | Claimed |
