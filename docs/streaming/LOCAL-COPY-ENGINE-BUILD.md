# Local copy on the one engine — build plan for the rest of item 6

**Status:** open. Step 1 built on `item6b`. Step 2's publication guard is in
qualification. Steps 3, 4 and 5 are built on `effort/playback-start-phase2`
(PRs #1543, #1548, #1555) but are not on `main`; they are being re-landed
one step at a time (see "Re-land on main"). Step 6a is drafted in #1557.
Step 2's trigger, step 6b and steps 7-10 are not started.

**Executes:** item 6 of [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md)
under the [A1/B1 build contract](PLAYBACK-START-A1-B1-BUILD.md) ·
**Written:** 2026-11-12 against `item6` @ `92c0e28` ·
**Revised:** 2026-11-27 against `main` @ `f7c06f0ee`

Any agent may work a step. Claim it by adding a row to the Execution log at
the end, with your exact model identifier and session, before you start.
Work the steps in order unless a step says it is independent. Every path is
under `crates/plurxd/src/` unless it starts with `plurx-core`; re-verify
names by function, not by line. Unit tests are written with each step and
run once, in the fast lane, when its PR is together; a step's log row says
whether that run has happened.

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

- Live HLS plays every title. The rolling copy path keeps serving every
  start the engine refuses, permanently (§3 step 10 says exactly what is
  removed and what stays).
- A leader restart does not interrupt playback. No step adds a Store write
  to the path between a start request and its first segment, or to segment
  serving.
- There is one copy engine. Every step moves work onto `copyrange` or
  leaves it where it is; none adds a producer.

## 2. What exists

| Path | Producer | Rows for the index |
|---|---|---|
| Shared (Source) copy | `copyrange::CopyRangeProduction`, started by `transcode/source_actor/growing.rs::prepare_source_growing` | Committed ranges, sent as `CompletedCopyRange` under `CopyObservationAuthority::Source` |
| Local cold copy | The rolling copy segmenter (`plurx_core::transcode::copy_pipe_args`, `copyseg`) | None |
| Local indexed copy | Plan-verified VOD generations (`vod/generation.rs::spawn_generation`) from an existing index | None |
| Local split copy | `vod/serve/split.rs`: the video-only copy served once, with every soundtrack as an alternate (P49; 3047e1083) | None |
| Background | `copyrange::fill_copy_gap` under an Analysis phase (`copy_analysis/work.rs::fill_gaps`) | `AnalysisCopyRows` |
| Content analysis | The fragment-index pass (`fragindex.rs`, one whole-file `ffmpeg` with ride-alongs such as `subtitle_ride_along`) | Node-local index rows only |

Item 6 step 1 (#1535) added `CopyObservationAuthority::Local` and a
row-less `CopyIndexDemand`: a local copy start without a v3 index asks, and
the Analysis worker builds the index by `fill_copy_gap` when the source is
idle. That is the standalone form of the copy-index job.

The growing engine is bound to the Source worker at nine points. P1–P4 are
structural; P5–P9 are behaviour a local owner must supply itself.

| # | Point | Where | Source-only today |
|---|---|---|---|
| P1 | Range storage identity | `copyrange/storage.rs::CopyRangeFiles::create` | Takes a `SourceDispatchAssignment`; payload directory is `source-copy-ranges/<incarnation>/<range>`; manifests record the Source incarnation, request and owner key |
| P2 | Generation ownership | `copyrange/pipes.rs::spawn_source_generation`, `CopyGenerationOwner` | Variants are `Source { owners, dispatch, authority }` and `Analysis { phase }` |
| P3 | Input admission | `growing.rs` via `foreground_source_request` and `SourceInput::open_foreground_held` | The request is keyed by the Source incarnation |
| P4 | Serving | `growing.rs::SourceGrowingBacking`, `CopyRangeFiles::playlist` | The range is served by the Source viewer actor's own playlist, not a VOD session's plan |
| P5 | Recipe and probe derivation | `growing.rs` against `vod/generation.rs::copy_video_pipeline` | The two paths once passed a different `have_dovi_bsf`, so one Dolby Vision HEVC file copied with DV stripped got two source keys. Closed on main by df5fb09f5 (the Source takes the node's boot-measured `dovi_rpu`); step 4 records the decision and the census split it leaves |
| P6 | Range liveness and the owner loop | `growing.rs`, the loop after `complete_source_media_session_publication` | The Source lease heartbeat renews the gate, refreshes the read bank and ends the range on expiry or revocation |
| P7 | Seek and control state | `CopyRangeProduction::observe_demand`, `rolling_source_watermarks` | Demand snapshots and watermarks come from the Source viewer's control state |
| P8 | Crash-orphan reaping | Source session recovery over `source-copy-ranges/<incarnation>` | A range a dead process left is reaped through its Source incarnation |
| P9 | Rendered milestone | `checkpoint.first_frame_rendered`, set from the Source route's accepted rendered report | No observation is sent before it |

## 3. Steps

### Step 1 — Durable demand (built)

A demand survives a restart.

- **Owner surfaces:** `copy_analysis/work.rs`, `copy_analysis.rs`,
  `copy_analysis/physical.rs`; `plurx-core/src/store/analysis_coordination.rs`
  and the trait in `store/background_jobs.rs`.
- **Change:** a demand makes its coordination request on its first attempt,
  once, before the foreground-pressure check. On the first pass of a
  process the worker reads the `local-play.` requests its predecessor left
  (queued, or still claimed by that dead process), rebuilds each demand
  from the catalogue and the request's own recorded object and video
  identity without opening the file, hands a dead claim back, and keeps
  those requests out of the stranded retirement. A demand dropped unbuilt
  retires its request.
- **Acceptance:** a title played locally before a restart is indexed after
  it without being played again; a demand whose file changed in the
  catalogue is dropped and one whose object changed is refused by the
  unit's source fence; no write and no file read is added to any start or
  to boot.
- **Tests (written, in `copy_analysis/quarantine_tests.rs` and
  `plurx-core/src/store/copy_index_v3_tests.rs`):** the store listing,
  keep-list and retire-by-id; recovery and its drop cases; the first pass
  calling recovery and keeping the request; the request made before the
  source is idle and not made twice.

### Step 2 — `local_playback` trigger and the legacy head guard (independent of 3–10)

The publication guard can qualify independently of the new trigger value.
Both stores replace the existing publication trigger atomically and keep
their schema versions, so the old writer can still submit the same command.
The new trigger value still needs the mixed-version rollout described below.

- **Owner surfaces:** `plurx-core/src/store/fragment_index_cluster.rs`
  (schema and both `valid_request`s), `store/sqlite/fragment_index_cluster.rs`,
  `store/hiqlite_fragment_index_cluster.rs`, the migration lists of both
  backends, `store/background_jobs_schema.sql`;
  `copy_analysis/work.rs::coordination_request`; `web/pages/analysis.js`;
  `http/analysis.rs` filters if they enumerate triggers.
- **Change, one schema version on both backends:**
  - `analysis_requests.trigger` accepts `local_playback`. Demand-driven
    requests enqueue with it; the `local-play.` id stays as the carrier of
    user and playback. Recovery (step 1) selects by trigger. `playback`
    keeps its meaning of a waiting viewer; nothing reads `local_playback`
    as one.
  - Existing rows with trigger `background` and a `local-play.` id under a
    `coordination-contract:` pipeline are rewritten to `local_playback` in
    the same version.
  - `background_job_publish_fragment_command` is redefined so its "no newer
    request" guard ignores `coordination-contract:` rows. Verified today: a
    queued, running or `ready` coordination request newer than a legacy
    `fragment_index` request of the same file and target stops that legacy
    request's publication from advancing `cluster_fragment_index_heads`.
    Source coordination rows have done this since item 2; `local-play.`
    rows widen it.
- **Hazards the migration must answer:**
  - `analysis_coordination_roles` and the other children reference
    `analysis_requests` with `ON DELETE CASCADE`, and
    `analysis_coordination_preserve_live_role` is a `BEFORE DELETE` trigger
    that silently ignores the delete of a live coordination row. A rebuild
    by copy-and-drop either deletes every child or leaves the old table
    half emptied. Drop the dependent triggers first, move the rows with
    the table renamed rather than deleted from, and recreate every trigger
    and index by name.
  - Release sequencing, with no gate: the version that reads, migrates and
    accepts the value ships first; a node writes the value only once the
    cluster's active protocol range (`MembershipManager::active_protocol_range`,
    the existing rollout mechanism) says every member accepts it.
- **Acceptance:** child row counts, triggers and indexes identical before
  and after, by name; an old binary reads the migrated table and a
  mixed-version Hiqlite cluster (one old voter) keeps enqueueing and
  claiming; rollback defined before the writer is enabled, as the
  contract's step 1 requires; the Analysis page and
  `GET /api/v1/analysis/jobs` show the stored trigger; a legacy head
  advances with a newer coordination row present.
- **Tests:** migration from the previous version with live rows in every
  state and with roles, interests and attempts, both backends
  (`make unit-core`); the mixed-version case; enqueue accepts the value and
  refuses a fifth; the head guard with a newer coordination row.

### Step 3 — The copy index rides the content-analysis pass (built on phase2, re-landing)

Paul's standing rule: every analysis job rides along with the analyze
pass, reading the same unfiltered input, and also runs standalone (item 6
step 1). This step is the ride-along; the ledger's step-3 batch and the
code (`copy_ride_along.rs`, `fragindex.rs`) hold the detail.

- **Owner surfaces:** `fragindex.rs`, `copy_ride_along.rs`,
  `copy_analysis.rs`, `copy_analysis/work.rs`, `copyrange/observed.rs`,
  `census.rs`, `copy_normalization.rs`, `state.rs` (`pass_may_ride`,
  `settle_copy_ride`).
- **Authority, no second claim service:** the pass's own claimed
  `fragment_index` build job, its custody and `SourceReadBudget`. The ride
  hands the worker one whole-title observation naming that claim
  (`IndexPassCustody`, made only from a live `FragmentIndexBuild` fence and
  bound to the file and attested object); the worker takes the
  metadata-only coordination claim, seals and publishes.
- **Which passes ride:** the cluster build job's lead identity, on a node
  running the Analysis worker, Matroska or ISO base media, a probed clock,
  no healthy current head, and no failed ride of this source. Others are
  built by the standalone form.
- **Rows:** the lead reader's own parsed video-only fragments, by move,
  paired with packets by `associate_fragment_clock`, checked against
  `CopyNormalizationContract::from_head`, inspected and measured as the
  engine does; over-target rows refused as the kernel refuses them. No
  observer task or backlog; held fragments bounded by `MAX_HELD`.
- **Clock:** the engine's packet clock on descriptor 7 after the index
  outputs, never inside the hashed argv, without the global `-copyts`
  (it would move the stored subtitle tracks); the source origin comes
  from the pass's probe, else the ride is abandoned.
- **A ride only adds:** it never displaces held rows, never quarantines a
  healthy head (`ridden`), and evicts only another ridden set.
- **Product:** the Analysis row's `copy_index_riding`, `copy_index_rows`,
  `copy_index_abandoned`; `plurx_copy_index_ride_total{outcome}`; a warn
  log per abandonment. Cancelling the job stops both.
- **Post-deploy acceptance, not yet run:** a UHD HDR10 remux and Dolby
  Vision profile 7 and 8 titles ride to `observed`, seal a standalone
  fill's blob, within noise of a non-riding pass. Record it in the ledger.
- **Tests (written):** `copy_analysis/gap_fill_tests.rs`,
  `fragindex/copy_ride_tests.rs`, `copy_ride_along/tests.rs`,
  `copy_analysis/quarantine_tests.rs`,
  `tests/web/analysis-node-names.test.js`.

### Step 4 — A local owner at P1, P2 and P5 (built on phase2, re-landing)

No behaviour change for local playback. Phase2's P5 change altered shared
playback; main has since fixed the same split another way (below).

- **Owner surfaces:** `copyrange/storage.rs`, `copyrange/pipes.rs`,
  `source_generation.rs`, `copy_analysis.rs` (`LocalPlaybackOwner`); one
  derivation in `vod/generation.rs` (`node_copy_video_pipeline`) used by
  `growing.rs`, both rendition paths of `vod/serve/create.rs` and the
  planning hint in `http/stream.rs`.
- **P1, range storage:** `CopyRangeFiles::create` takes a `CopyRangeOwner`:
  `Source(Box<SourceDispatchAssignment>)` or `Local { session, viewer:
  LocalPlaybackOwner }`. Directories and manifest binding come from the
  owner: `source-copy-ranges/<incarnation>/<range>` as before,
  `local-copy-ranges/<session>/<range>` for local. `LocalPlaybackOwner`
  has one constructor, from a live local viewer with a named playback; an
  authority read back from a durable request can never own a range.
- **P2, generation ownership:** `CopyGenerationOwner::Local` is the
  session rendition's dispatch ledger with no Source assignment, one
  dispatch and the owner. A local generation is refused on a rendition a
  Source viewer is attached to, before any child and again at
  registration, where `registered_local` decides under the ledger lock
  and a refused child is retired and reaped before the refusal returns.
- **P5, decided 2026-11-27: closed on main by df5fb09f5; phase2's P5
  commits (eb069a9d6, 54820ca3d) are not re-landed.** The Source takes
  the node's boot-measured `dovi_rpu` (`dv_strippable()`) wherever it
  builds a copy identity, so for one file, recipe, node and stored probe
  the Source range, both renditions and the planning hint derive one key.
  Porting `node_copy_video_pipeline` would rewrite `vod/serve/create.rs`
  against moved code and drop main's authority-bound, 1 MiB-bounded Source
  probe read, for no remaining keying case.
  - **Known non-unification, by decision:** the HEVC parameter-set census.
    A local copy start records a missing census first; a Source start
    reads the stored probe as it stands. Keys differ only for an HEVC
    file with no census, started from Source first, whose later census
    is `Varying`. Then the argvs really differ, so separate keys are
    correct. Phase2 kept the same split.
  - **When a local range goes live (step 5 or 7):** build its video
    options through the manager function the Source range uses
    (`source_copy_video_options`, generalised, reading `dv_strippable()`),
    so ranges share one derivation by structure.
- **Manifests:** no binary reads a checkpoint manifest back, so
  compatibility is by construction and pinned by
  `copyrange/source_manifest_4b125e6.json`, a populated Source manifest
  derived by hand from `4b125e6`'s declaration. A Source manifest is that
  binary's, byte for byte. A local one carries `owner: "local"`, the
  session and the viewer, under `copy-ranges/local-observations/<range>`,
  beside the Source `copy-ranges/observations`.
- **Tests (written):** `transcode/tests/source_actor.rs` (on a real Source
  session, one pipeline and source key from the Source rendition's and
  the local rendition's whole preparations and the range's derivation,
  capability injected both ways; the golden manifest through a real
  binding; a stripped Dolby Vision file's key per capability; on main,
  df5fb09f5's tests cover the key, and the re-land adds the local owner's
  cases beside them);
  `copyrange/storage_tests.rs`; `source_generation.rs`.
- **Not covered by a test:** `prepare_source_growing` as a whole; the
  retire-and-reap of a local child refused at registration; a recorded
  census changing a Source key.

### Step 5 — Local input admission, liveness, control and reaping (P3, P6, P7, P8) (built on phase2, re-landing)

Nothing is served from a local range and no session starts one: this step
is the range's own half. The session's half, the calls into it, lands with
the attachment in step 6.

- **Owner surfaces:** a new `copyrange/local.rs` (`LocalCopyRange`, the
  local range's actor), `copyrange/pipes.rs`, `copyrange/production.rs`
  (the read bank's rolling rule, the run task's end guard),
  `copyrange/storage.rs` (two fields visible to the range's tests),
  `source_input.rs` (Inputs under a custody their owner registered),
  `main.rs` (boot reaping), `transcode/rolling/flow.rs` (the rolling
  source observation, one function) and `transcode/manager/maintenance.rs`
  (calls it). `source_custody.rs` needed no change.
- **P3, Input admission:** `LocalCopyRange::start` registers one foreground
  custody under a request keyed by the local playback
  (`local_range_source_request`): the session is the request's lifetime
  and the policy is `Rolling`, the keys a rolling local session registers.
  The head census reads through an Input lent that custody
  (`open_foreground_lent`: closing it joins its reads and neither marks nor
  releases the custody); once the census has joined it, the primary's
  Input takes the custody over (`open_foreground_with_custody`) and
  releases it when it closes. One row, written once, from start to end,
  as a rolling start's: the read is charged once.
- **Decision, copy admission: a Local owner charges memory through the
  same permit, and waits for nothing.** Phase2 built this step before
  copies held a memory permit, and gave a local range no copy admission.
  Main now admits verified copies against the node's memory total
  (e6835217a, 4d7fc8d3e, 64a8ebdc2; open PR #1915): a copy holds a
  `CopyMemoryPermit` and is charged what it holds. A Local owner's range
  takes that same existing permit, so a local copy is counted in the
  node's memory as every copy is. It adds no new wait: a viewer's copy
  always grows (4d7fc8d3e), so the first segment is not delayed by
  admission. A local range holds no encoder-pool permit, as a rolling
  session (`sw_permit` is empty for a copy) and a plan-verified rendition
  do not; the memory permit is the only charge added. The re-land drops
  phase2's "optional permit removed" wording and wires
  `CopyGenerationOwner::Local` to the memory permit. Its read stays
  charged once, to the source read.
  - Acceptance adds: a local range's held bytes appear in the node's
    memory accounting and release at settlement; a start under memory
    pressure still reaches its first segment as a viewer's rolling copy
    does today.
- **P6, liveness:** the range has no lease, loop or timer. It is live
  while its production is and its held object is unchanged
  (`is_live`, `physical_fence`). The session's retirement calls `settle`,
  which stops the range, joins the child, every pipe task and the Input
  (releasing the custody), then removes the payload, the manifests and the
  empty `local-copy-ranges/<session>/`; a removal failure beside a failed
  range is logged. A range dropped unsettled is retired by its `Drop`: the
  run task reaps the child and closes the Input, and only its files are
  left, for boot. A run task that unwinds stores `copy_range_panicked`
  through its end guard, so `settle` is bounded by the task's end. A start
  stopped once its generation ran returns the range retired, as the Source
  range does; a start that fails after its files were made removes them,
  and a start future dropped there removes them on the runtime.
- **P7, control:** `observe_control(demand, startup_protected)` takes the
  session's newly accepted `PlaybackDemandSnapshot` and its lease's
  startup protection; `refresh(startup_protected)` is for its existing
  maintenance. Both drive the engine's read bank under the rolling rule,
  `rolling_source_observation`, the function `maintenance.rs` now calls:
  watermarks from the demand's rate, lead from its estimated position,
  fresh while the demand is at most 5 s old and the startup unprotected.
- **Decision, pressure: a local range presses exactly as a rolling session
  does.** A paused or buffering viewer still reporting lifts pressure while
  held. The Source range keeps its own rule (fresh only while rendering).
- **P8, reaping:** a range dies with its process. Its payload is in the
  transcode scratch every boot already clears, which is the recovery that
  reaps a rolling session's directory. Its manifests are under the data
  directory, so `reap_range_manifests` clears `copy-ranges/observations`
  (Source) and `copy-ranges/local-observations` (local) at the same point of
  boot, under the same daemon lock. Nothing constructs a range but
  `CopyRangeFiles::create` and nothing reads a manifest back, so no range is
  resumed and every manifest at boot is an orphan. A failed reap is logged
  and costs no boot. This fixes the Source leak found while building the
  step: before it, a killed Source range's manifests stayed forever.
- **Acceptance, as measured:** the trace test below. For every existing
  serving path the answer is by construction: `spawn_source_generation`'s
  Source and Analysis arms are as they were; rolling maintenance calls the
  same arithmetic moved into one function; the Source read bank keeps its
  rule; an owned Input opens and closes as before.
- **Tests (written, in `copyrange/local_tests.rs`):**
  - The trace: on a real store and fixture, a rolling session's custody
    (its request and `SourceInput::open`, then `rolling`/`refill` in
    `maintenance.rs`'s order, each observation from
    `rolling_source_observation`) and a local range's (`start`'s own
    census and custody, the primary Input taking it over, the read bank
    through `observe_control`/`refresh`) give the same rows and pressure
    at eight points: opened, below high, held above high, a seek ahead,
    held with a buffering, a paused and a startup-protected viewer,
    closed. Committed media is set by hand: the 12 s fixture is shorter
    than the 48 s low watermark.
  - Census then primary on the real engine, reads held: one row with one
    owner id and attempt from census to settlement.
  - A held range dropped unsettled releases its custody; its files stay.
  - A run task that panics ends its range as `copy_range_panicked`.
  - A whole range on the real engine settles to no row, no pressure, no
    payload, no manifest and no session directory.
  - Boot reaping removes both owners' manifests; a live range's stay.
- **Not covered by a test:** a stop between the production's start and the
  range's return, and a start future dropped with its files made (neither
  can be reached without a race); a generation reading a bank deeper than
  the fixture; a running rolling session (its flow's holds are replayed);
  an Analysis phase pausing on the recorded pressure; a kill and a real
  boot.
- **Left for step 6, with the attachment:** the session's calls
  (`observe_control`, `refresh`, `settle`); see step 6 for their owner.

### Step 6 — Local serving attachment and rendered milestone (P4, P9; build contract step 4) (6a drafted in #1557; seam and attachment not started)

- **Ruling (2026-11-12): Paul delegated to Fable; Fable chose (b).** The
  range attaches in the rolling copy start the `vod_index_pending`
  fallback makes (`transcode/manager`, `start_copy_with_audio_offset`) and
  is served rolling-shaped, as the Source range is: EVENT, `vod: false`,
  rolling lease, nonzero origin. No client change; copyseg answers every
  refusal. VOD sessions are untouched.
- **Owner surfaces:** `transcode/manager` (start, publication, control,
  maintenance), `transcode/rolling` (session, flow, retention,
  retirement), `copyrange/local.rs`, Activity.
- **Change:** the cold local copy start attaches a local range. Its flow
  evaluation calls `observe_control` and `refresh`, its retirement
  `settle`, its rendered report `mark_rendered`; its Activity line shows
  the range's `payload_bytes` and Stop retires it. A typed range refusal
  starts copyseg for that start, as today. A1-N is the session's own
  publication clock, the same rule `wait_admitted` applies.
- **Drafted (6a, #1557, `claude/a1b1-range-serving`), the range's serving half, `copyrange/local.rs`:**
  `snapshot()`, one view under one lock (playlist, its committed segments,
  init, origin, completion, the bytes held in the playback cache);
  `wait_admitted()` by A1-N's one rule, now one function
  (`transcode::startup_admission_window_ms`) that rolling publication and
  the Source range also call; a second range of a session whose
  settlement is removing its directory refused
  `copy_range_session_settling`. Nothing serves from a range yet.
- **Found: the rolling session has no backing seam.** `Session` is the
  copyseg producer: it reads `dir/index.m3u8` and `dir/<segment>` (about
  25 sites), locks its `AttemptChild` to suspend, replace and reap (about
  15), feeds `SegmentIndex` from that playlist (about 23), and owns
  copyseg's scratch grant, upload endpoint, retained collection,
  retention and permits. The cached asset is a second backing already,
  done as 12 `cache_manifest` branches. A range must not be a third.
- **Next, its own change before the attachment (6b): the seam.** A typed
  `RollingBacking` the session serves, with one implementation per
  producer: copyseg (child, `dir`, scratch, upload, retention, retained
  collection), the cached manifest, and the local range. It answers the
  playlist read, the object open and size, the produced end, hold and
  resume (copyseg: stop the child; range: its read bank), retention (the
  range keeps its committed segments under its playback-cache charge) and
  retirement (reap; `settle`). Copyseg's and the cache's behaviour must be
  unchanged, held by the existing `transcode/tests` suites, which build
  `Session` by hand and move with it. No branch is added inside copyseg.
- **Acceptance:** the contract's 48-second native rate-1 case; a cold
  local start at 0 and at 90 minutes plays from the range on web, Apple and
  Android (device acceptance of the existing rolling descriptors); a
  leader restart does not interrupt it; every refusal falls to copyseg and
  plays; Activity shows the range and Stop retires it.
- **Tests:** a cold local copy start served from a range; a refusal
  falling back to copyseg; Stop retiring the range; the Activity disk use;
  the existing rolling and cached suites unchanged.

### Step 7 — Local ranges commit observations

- **Owner surfaces:** the local range's actor from step 6,
  `copy_analysis.rs`, `copy_analysis/work.rs`.
- **Change:** after the rendered milestone, each committed local
  checkpoint is sent as a `CompletedCopyRange` under
  `CopyObservationAuthority::Local`, as `growing.rs` does for Source. The
  start still sends its row-less demand, and the demand is kept until a
  checkpoint of that range has committed and folded: only then do rows
  stand in for it. A set created or extended by a Local range takes
  `asked_by` from the range's authority, so its request is attributed, and
  is a demanded source: it makes its request before the source is idle and
  retires it if dropped, exactly as a demand does.
- **Acceptance:** a partially played local title publishes after
  background filling of its gaps, with no second read of what was played;
  a range that never commits leaves the demand to build the index.
  Measure physical source bytes read per locally played title: they drop,
  because what was played is not read again. Tap-to-first-presented-frame
  is a no-regression measure under matched cold or warm cache and load,
  not a target for this step.
- **Tests:** a local range's rows fold with an Analysis fill into the
  whole-title set (extend `gap_fill_tests`); the demand survives until the
  first checkpoint; attribution and persist-first for a row set;
  contradiction with a healthy head still quarantines.

### Step 8 — Indexed local copy starts on the engine, old path as fallback

- **Owner surfaces:** `vod/generation.rs`, `vod/plan.rs`,
  `vod/serve/create.rs`, `vod/serve/split.rs`.
- **Change:** an indexed local copy start runs a seeked range of the engine
  verified against the index. The plan-verified generation stays in the
  tree and serves every start the range refuses. Nothing is removed.
  The split family (`vod/serve/split.rs`, video once with every soundtrack
  as an alternate) is a local indexed copy too: the engine either covers
  it, or the start explicitly falls back to the split path and says so as
  a typed refusal that step 9 counts. It never silently drops the
  alternates.
- **Acceptance:** seeked and sequential rows equal (contract step 3);
  every refusal plays through the existing generation.
- **Tests:** the seeked-range verifier; each typed refusal falling back,
  including a split-family start.

### Step 9 — Fleet acceptance

No code. On the fleet, over an agreed window, count local copy starts by
who served them: the engine, the rolling path after a typed refusal (by
refusal), the plan-verified generation after a refusal. Run the acceptance
list of [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md) for A1/B1.
Record the counts and the titles behind each refusal in the status ledger.
Measure two things. Physical source bytes read per locally played title
should drop: nothing played is read a second time. Tap-to-first-presented-
frame is a no-regression measure under matched cold or warm cache and load.
Steps 4-9 do not shorten native HLS admission (see the two-second section).

### Step 10 — Removal, as its own change

- **Owner surfaces:** `copyseg`, `transcode/manager` rolling paths,
  `vod/generation.rs`, `tests/playback/rolling-producer-owners.toml`.
- **What stays, permanently:** the rolling copy path is what plays a title
  the engine refuses. It is removed only as the default for starts the
  engine serves. Its code stays for every refusal class step 9 counted
  above zero, and those classes are listed in the ledger by name. A class
  leaves that list only when the engine has learned to serve it and a
  later fleet window counts it at zero. Live HLS therefore plays every
  title before, during and after this step.
- **Change:** the rolling session shell stays permanently as the EVENT
  presentation. Only copyseg as the default for starts the engine serves
  and the plan-verified generation are removed; a refusal class's copyseg
  code goes only under the rule above.
- **Acceptance:** no title that played before fails to play.
- **Tests:** the inventory shrinks by exactly the removed producers; each
  retained refusal class still has its fallback test.

## 4. Guardrails

- A local owner never holds a Source assignment, gate or grant, and a
  Source owner never holds a `LocalCopyAuthority`.
- Receivers stay relays; only an origin node that can open the held object
  runs the engine.
- Processed Dolby Vision identity, Live TV and the encoded paths are not
  touched.

## The two-second startup acceptance

For starts from the beginning and saved resume positions, prepare the verified
init and playable media ahead of Play. Join that prepared range to the same
VOD rendition, timeline and session used for continued playback. Key reuse to
the source version, resolved video and audio recipe, and verified video
coverage at the requested cursor. Charge preparation to the existing cache
and source budgets. Keep native HLS admission for every fallback.

A published index describes where to read. Qualification for two seconds
requires ready media or a measured on-demand path that fits the remaining
budget. Local range ownership and serving in steps 4–9 still matter for
unprepared positions. Those steps do not themselves shorten native HLS
admission, so measure index-only and completely unprepared starts separately.

Measure Play to the first displayed frame at the requested position through
Cinema on physical Apple TV. Use the saved positions from the *Two Towers*
and *Axel F* incident, and run long enough to cross the prepared range and
seek again. Check selected audio, forced and selected captions, HDR/Dolby
Vision and recovery across ingress changes. Retain the Gran Turismo short
window Safari regression and verify the affected Android and web routes.
Count fallback, timeout and wrong-position attempts against the two-second
result.

## Re-land on main (2026-11-27)

Steps 3, 4 and 5 were built and merged into `effort/playback-start-phase2`
on 2026-11-12 and 13 (PRs #1543, #1548, #1555). They never reached `main`.
`main` has since moved 2656 commits (merge base `e6940abe7`), and a trial
merge conflicts in 18 files. Decision (2026-11-27, effort E-BFB7D832E3):
re-land each step onto current `main` as its own task branch, using phase2
and #1557 as the reference. Do not merge `main` into phase2.

Order: step 4, step 5, step 6a (#1557), the `RollingBacking` seam as its
own change, the step 6 attachment, step 7, step 8. Step 3 does not serve
the chain and may re-land in parallel or later. Step 2 is not a build
prerequisite for step 7, but its head-guard half lands before step 7
reaches the fleet: local row sets add coordination rows that stall legacy
head publication.

Three design changes on `main` since phase2 are why a copy is not enough:

- **Memory admission.** Copies hold a `CopyMemoryPermit` against the node's
  memory (e6835217a, 4d7fc8d3e, 64a8ebdc2; open PR #1915). Step 5 changes.
- **Source copy identity.** df5fb09f5 closed P5 on main, so step 4
  re-lands only P1 and P2.
- **Split copy.** `vod/serve/split.rs` (3047e1083) is a new local indexed
  producer. Section 2 and step 8 cover it.

Source on phase2, per step:

| Step | Commits and PRs on phase2 |
|---|---|
| 3 | #1543: b55bae714, fda526245, 839fad128, a002719d1, 75eaa9785, 663988f32, 4aebd160d |
| 4 | #1548: 967eb2357, 4585b1bfa (P1, P2), 73e68f964; not eb069a9d6, 54820ca3d (P5, closed by df5fb09f5) |
| 5 | #1555: a429d9c79, dfbf220fe, 72df26fdf, 32c713b26 |
| 6a | #1557, branch `claude/a1b1-range-serving` |

Each re-land carries its own tests and runs them once in the fast lane.

## Execution log

One row per step, added when it is claimed and updated when it lands.
**Model** is the runtime's exact model identifier; **Session** is the
session id or URL. **Tests** says whether the step's tests have been run.

| Date | Step | Model | Session | Branch / PR | Outcome / evidence | Tests |
|---|---|---|---|---|---|---|
| 2026-11-12 | 1 Durable demand | claude-fable-5-2 | https://claude.ai/code/session_01BCWL9ypnQEiQUyRaZtMhh3 | `item6b` | Built, with its review fixes; clippy `-D warnings` and fmt clean, inventory and pins green | Written, not yet run: the fast lane is their one run |
| 2026-11-12 | 3 Copy index rides the content-analysis pass | claude-fable-5-2 | session_01BCWL9ypnQEiQUyRaZtMhh3 | `step3` (local, on `item6b`) | Built, with its review fixes (2 majors, 9 minors): the ride consumes the lead reader's parsed fragments in place, `ridden` is pass-only, custody is bound to file and object. Clippy `-D warnings` and fmt clean, inventory and pins green. Post-deploy UHD/DV qualification open | Written, not yet run: the fast lane is their one run |
| 2026-11-12 | 4 A local owner at P1, P2 and P5 | claude-fable-5-2 | session_01BCWL9ypnQEiQUyRaZtMhh3 | `step4` (local, on `step3`) | Built, with its review fixes (2 majors, 8 minors): `CopyRangeOwner` and `CopyGenerationOwner::Local`, constructed only by tests and only from a live named playback; one copy pipeline derivation (capability and probe evidence) for the Source range, Source and local renditions and the planning hint, which re-keys stripped Dolby Vision Source copies on nodes with the strip filter. Clippy `-D warnings` and fmt clean, inventory and pins green | Written, not yet run: the fast lane is their one run |
| 2026-11-12 | 5 Local input admission, liveness, control and reaping | claude-fable-5-2 | session_01BCWL9ypnQEiQUyRaZtMhh3 | `step5` (local, on #1548) | Built, with its review fixes (8 findings): `LocalCopyRange` reads under one custody carried from census to primary, presses by the rolling rule (`rolling_source_observation`, shared with maintenance), takes no copy admission, has no lease or timer, retires on drop, survives a panicking run task, and is reaped at boot; nothing starts one yet. The session's calls into it are left to step 6. Clippy `-D warnings` and fmt clean, inventory and pins green | Written, not yet run: the fast lane is their one run |
| 2026-11-12 | 6 Local serving attachment | claude-opus-5-5 | session_01BCWL9ypnQEiQUyRaZtMhh3 | `step6` (local, on #1555) | 6a built (range serving half: snapshot, A1-N admission, settling refusal). Ruling: Paul delegated to Fable; Fable chose (b). Stopped before the rolling-session seam (step 6, "Next"); nothing serves from a range. Clippy `-D warnings` and fmt clean, inventory and pins green | Written, not yet run: the fast lane is their one run |
| 2026-11-27 | Re-land plan | claude-opus-5-5 | effort E-BFB7D832E3 | claude/p2-local-copy-plan-revision | Plan revised against main f7c06f0ee; steps 3-5 and 6a to be re-landed per step | No code |
