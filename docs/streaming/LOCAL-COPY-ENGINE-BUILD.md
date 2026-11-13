# Local copy on the one engine — build plan for the rest of item 6

**Status:** open — step 1 built on `item6b`, step 3 built on `step3`,
step 4 built on `step4`, step 5 built on `step5`, steps 2 and 6–10 not
started ·
**Executes:** item 6 of [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md)
under the [A1/B1 build contract](PLAYBACK-START-A1-B1-BUILD.md) ·
**Written:** 2026-11-12 against `item6` @ `92c0e28`

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
| Background | `copyrange::fill_copy_gap` under an Analysis phase (`copy_analysis/work.rs::fill_gaps`) | `AnalysisCopyRows` |
| Content analysis | The fragment-index pass (`fragindex.rs`, one whole-file `ffmpeg` with ride-alongs such as `subtitle_ride_along`) | Node-local index rows only |

Item 6 step 1 (#1535) added `CopyObservationAuthority::Local` and a
row-less `CopyIndexDemand`: a local copy start without a v3 index asks, and
the Analysis worker builds the index by `fill_copy_gap` when the source is
idle. That is the standalone form of the copy-index job.

The growing engine is bound to the Source worker at nine points. P1–P4 are
structural; P5–P9 are behaviour a local owner must supply itself. Step 4
answered P1, P2 and P5; their rows say what was Source-only before it.

| # | Point | Where | Source-only today |
|---|---|---|---|
| P1 | Range storage identity | `copyrange/storage.rs::CopyRangeFiles::create` | Takes a `SourceDispatchAssignment`; payload directory is `source-copy-ranges/<incarnation>/<range>`; manifests record the Source incarnation, request and owner key |
| P2 | Generation ownership | `copyrange/pipes.rs::spawn_source_generation`, `CopyGenerationOwner` | Variants are `Source { owners, dispatch, authority }` and `Analysis { phase }` |
| P3 | Input admission | `growing.rs` via `foreground_source_request` and `SourceInput::open_foreground_held` | The request is keyed by the Source incarnation |
| P4 | Serving | `growing.rs::SourceGrowingBacking`, `CopyRangeFiles::playlist` | The range is served by the Source viewer actor's own playlist, not a VOD session's plan |
| P5 | Recipe and probe derivation | `growing.rs` (`CopyVideoOptions::from_probe(file, probe, false, preserve)`) against `vod/generation.rs::copy_video_pipeline` (`has_dovi_rpu()`) | The two paths pass a different `have_dovi_bsf`. Verified: for a Dolby Vision HEVC file copied with DV stripped, that flag changes the bitstream filter argv and therefore `identity_for`, so on a node whose ffmpeg has the DV filter one file gets two source keys, one per path |
| P6 | Range liveness and the owner loop | `growing.rs`, the loop after `complete_source_media_session_publication` | The Source lease heartbeat renews the gate, refreshes the read bank and ends the range on expiry or revocation |
| P7 | Seek and control state | `CopyRangeProduction::observe_demand`, `rolling_source_watermarks` | Demand snapshots and watermarks come from the Source viewer's control state |
| P8 | Crash-orphan reaping | Boot: the transcode scratch clear (payloads) and `reap_range_manifests` (manifests, both owners) | No range is resumed, so every range a dead process left is reaped at the next boot |
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

### Step 3 — The copy index rides the content-analysis pass (built)

Paul's standing rule: every analysis job rides along with the analyze
pass, reading the same unfiltered input, and also runs standalone. The
standalone form exists (item 6 step 1). This step is the ride-along.

- **Owner surfaces:** `fragindex.rs` (`build_attested_pass`,
  `build_with_args_owned`, `index_stream_with_progress`, `LeadFragments`,
  `IndexPass::riding_args`), a new `copy_ride_along.rs` beside
  `subtitle_ride_along.rs`, `copy_analysis.rs` and `copy_analysis/work.rs`
  (a third observation source), `copyrange/observed.rs`
  (`inspect_canonical_row`, `copy_cut_policy`) and `census.rs`
  (`head_origin`) for the shared row and inspection extraction,
  `copy_normalization.rs` (`begin_canonical`, `admit`),
  `state.rs::pass_may_ride` and `settle_copy_ride`.
- **Authority, with no second claim service:** the pass runs under its own
  claimed `fragment_index` build job, the source custody that claim holds
  and its `SourceReadBudget`; those stay the only authority for its read.
  The ride-along does not claim the coordinator. It hands the worker one
  whole-title observation that names the pass's claim and custody:
  `IndexPassCustody`, whose only constructor reads a `JobFence` that is
  live when asked, of kind `FragmentIndexBuild`, with custody owned by
  that exact claim, and binds it to the job's file id and the object
  version the job attested. The worker, which is the one coordinator, then
  takes the metadata-only coordination claim itself (it reserves no
  reader) and seals and publishes. The set accepts the observation on the
  same checks as an Analysis fill (`CopyObservationSet::observe_pass`),
  with the pass's custody in place of a coordination phase, only for that
  file and object, and only as a whole title. The pass awaits room in the
  worker's inbox while it holds the claim; losing the claim ends the wait.
- **Which passes ride:** the cluster build job's pass
  (`run_cluster_fragment_index_job`), for its lead identity, when all of
  these hold: this node runs the Analysis worker; the container is
  Matroska or ISO base media (`mkv`, `webm`, `mp4`, `m4v`, `mov`); the
  probe gives a clock; one `copy_index_v3_variant(...).readable()` read
  finds no healthy, current head; and this source's last riding pass did
  not fail (`FAILED_RIDES`, in memory, per file and source stamp, as
  `subtitle_ride_along` keeps). The non-cluster local pass holds no claim
  or custody to name and does not ride. Sibling identities of a shared
  read are not ridden. Each of those is still built by the standalone
  form.
- **Rows, and why a video-only observation is enough:** an `IndexRow` is
  defined on the video-only canonical pipe, which is what the pass emits on
  `pipe:1`. The engine checks an AV companion only because it also serves
  AV; the index does not contain it. The ride consumes the lead reader's
  own parsed fragments, by move, once the index has measured them: no byte
  copy, no second parse and no second Dolby Vision conversion (the lead's
  converter is the engine's, over the same track; a converting pass keeps
  the unconverted sample tables, not the media, for the clock). Each
  fragment is paired with its packets by the engine's
  `associate_fragment_clock`, checked against
  `CopyNormalizationContract::from_head`, inspected by
  `inspect_canonical_row` and measured by `CopyCompanionVerifier` with the
  canonical stream standing as its own companion. A row or a segment of
  rows longer than the engine's 16 s target is refused as the engine's
  kernel refuses it.
  - **Not removed, and why:** `inspect_canonical_row` still walks each
    fragment's NAL headers for HDR10 SEI. It is the engine's own
    inspection and reads length prefixes and NAL header bytes, not
    payload. Taking the per-row facts from the lead reader instead would
    mean changing the lead's cumulative HDR10 scan in `plurx-core`, which
    is what keeps the node-local index byte-identical with and without
    the ride.
  - A divergence that slipped through is still caught at use: every
    generation is verified against the index, and item 5 quarantines a
    contradicted head.
- **No task, no backlog:** the ride has no observer task and no queue of
  copied bytes. Its state is fed in place, under one lock, by the three
  readers the pass already runs (lead output, stderr header trace, clock).
  It holds a fragment only until its packets and inspections arrive, and
  is abandoned if that ever exceeds the engine's own `MAX_HELD` (96 MiB).
- **Clock:** the ride-along appends the engine's packet clock block
  (`transcode::copy_packet_clock_output_args`) after the index outputs, on
  descriptor 7, never inside `copy_index_pipe_args*` (the rule
  `subtitle_ride_along` keeps, so no cluster cache key changes). It does
  **not** add the engine's global `-copyts`. That option would move the
  timestamps of every other output of the pass; the stored subtitle tracks
  are written without it and their cue times depend on that
  (`subtitles.rs`, "`-copyts` and no `-start_at_zero`"). Without it
  FFmpeg's input clock sits the format start time before the source's.
  Rows are differences from the first packet, so they are unaffected. The
  source origin is recovered from the pass's own probe of the held
  descriptor: the format start, only when it is a whole number of stream
  ticks, and checked against the video stream's probed first presentation
  time. Anything else abandons the ride. The clock reader drains to end
  of file whatever the ride does. A read error abandons the ride; the
  reader then stops reading and holds the descriptor open, with no retry,
  until the pass ends, so the child never sees a broken pipe. The reader is joined only when the lead built (the child exited
  and closed the clock); otherwise it is stopped. Nothing waits on it
  without a bound.
- **A ride only adds:** rows that disagree with rows already held are
  dropped and the held set stands. `ridden` means a set holds a pass's
  rows and nothing else: such a set never quarantines a healthy head. A
  viewer's range folding in clears it, a pass folding into a viewer's or a
  fill's rows never sets it, and a viewer's range that disagrees with a
  pass's rows replaces them, so a viewer's own contradicting range still
  quarantines. At the pending bound a pass may evict only another ridden
  set, never a demand or a viewer's set.
- **Attestation:** not a second read where the pass records its own
  full-source receipt, which `physical::cached_full_source` already accepts
  (`LegacyFull`). That is every HEVC pass (`attest_copy_source_persistent`
  hashes the whole file under `hevc-full-v1:`). A pass of any other codec
  records only a sampled receipt, so one whole-file hash phase remains for
  it; that is a second read of the file, not of the video.
- **Product:** the Analysis page's live row says the pass is also observing
  the copy index, with its row count, or that the observation was
  abandoned (`copy_index_riding`, `copy_index_rows`,
  `copy_index_abandoned` on the progress row), beside the subtitle
  ride-along's line. Rides are counted in
  `plurx_copy_index_ride_total{outcome}` (`observed`, `abandoned`,
  `not_built`), and an abandonment is a warn log with the observer's own
  reason. Cancelling the Analysis job stops both: the ride has no child
  and no timer of its own.
- **Cost added to the pass:** no source read. In the child: one more
  output, which runs the copy recipe's bitstream-filter chain a second
  time over the video and has `framecrc` checksum every packet, and writes
  a text line per packet to a pipe. In the daemon: per fragment, a
  sample-table comparison, a NAL-header walk and one row measurement, in
  the reader that delivered the last piece; memory is the fragments held
  unpaired, by move, bounded by `MAX_HELD` (96 MiB), plus up to
  `MAX_PACKETS` packet records. There is no separate backlog.
- **Acceptance:** a title the pass indexes gets a v3 head from that read;
  its blob equals the one `fill_copy_gap` seals for the same file and
  recipe; the node-local index and its cluster cache key are byte-identical
  with and without the ride-along; a failed ride-along, and one that falls
  behind for longer than the pass runs, cost the index nothing and do not
  lengthen the pass.
- **Post-deploy acceptance, not yet run:** on the fleet, a UHD HDR10 remux
  and a Dolby Vision title (profile 7 and 8, preserved and converted
  identities) each ride to `observed` with no abandonment, their v3 blob
  equal to a standalone fill's of the same title, and the pass's wall time
  and the node's memory within noise of a non-riding pass. Until then the
  ride is proven only on the 12-second generated fixtures. Record the
  titles, `plurx_copy_index_ride_total` and any abandonment reason in the
  ledger.
- **Tests (written):** row, inspection, origin, init and blob equality
  against a whole-title fill on the HEVC and H.264 fixtures
  (`copy_analysis/gap_fill_tests.rs`); the index equal with and without
  the ride, the hashed argv unchanged, a failed ride leaving
  `IndexOutcome::Built` and not riding again, a ride that falls behind
  (`fragindex/copy_ride_tests.rs`); the origin recovery, the container
  allow-list, abandonment counted and latched, no unbounded join
  (`copy_ride_along/tests.rs`); the worker's fold, custody binding,
  `ridden`, eviction, and a viewer's range still quarantining
  (`copy_analysis/quarantine_tests.rs`); the Analysis row
  (`tests/web/analysis-node-names.test.js`).
- **Not covered by a test:** `pass_may_ride` and `settle_copy_ride` under
  a real claim, the awaited hand-over, and the publication that follows,
  run only on a cluster. No fixture converts Dolby Vision, so the
  converting path's use of the lead's conversion is covered only by the
  post-deploy acceptance.

### Step 4 — A local owner at P1, P2 and P5 (built)

No behaviour change for local playback. One for shared playback, under P5.

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
- **P5, one derivation:** `node_copy_video_pipeline` takes the recipe, the
  node's capability and the kind of start, and reads the probe itself.
  - **Capability:** the transcode manager's injected `dv_strippable()`,
    handed to its VOD server and again when that server is replaced.
  - **Probe evidence:** the one stored record, `files.probe_json`. A local
    copy start records a missing HEVC census first. Shared playback and
    planning reads take the record as it stands and never measure one. An
    encoded Source rendition reads none.
  - **Unified:** for one file, recipe, node and stored probe, the Source
    range, a Source rendition, a local rendition and the `vod_indexed`
    hint derive one argv fingerprint, fragment-index identity and
    copy-index source key.
  - **Not unified:** a Source start of an HEVC file with no recorded
    census derives without it; if a later local start records one that
    says the parameter sets vary, the key changes once, for both. Callers
    outside playback starts still read `ffmpeg::has_dovi_rpu()`, the same
    boot measurement.
  - **What changes:** a Dolby Vision HEVC copy served stripped over a
    share, on a node whose FFmpeg has `dovi_rpu`, gains
    `dovi_rpu=strip=1` as local playback has, so its v3 source key,
    Source rendition key and v1 index identity change. The Source range
    also now reads a stored probe over 1 MiB instead of treating it as
    absent. Nothing else changes.
  - **Cost on deploy, for those titles on those nodes:** old Source heads
    and cached Source renditions are not found (heads are not deleted;
    the local head serves if there is one, else the next Source range
    rebuilds it); a Source rendition may now find a local-built v1 index
    and serve indexed VOD where it took the range; and while a fleet's
    nodes disagree on `dovi_rpu` (a rolling FFmpeg upgrade) a node does
    not adopt the other kind's head for such a title, as local playback
    already did not.
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
  binding; a stripped Dolby Vision file's key per capability);
  `copyrange/storage_tests.rs`; `source_generation.rs`.
- **Not covered by a test:** `prepare_source_growing` as a whole; the
  retire-and-reap of a local child refused at registration; a recorded
  census changing a Source key.

### Step 5 — Local input admission, liveness, control and reaping (P3, P6, P7, P8) (built)

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
- **Decision, copy admission: a local range takes none.** A local copy
  holds no encoder-pool permit today, as a rolling session (`sw_permit` is
  empty for a copy) or as a plan-verified VOD rendition, so taking one
  would not be "exactly as today": a local copy start could queue or be
  refused for capacity where it never is, and background work would yield
  to a pipeline it was not charged for before. Shared playback takes one
  because a remote grant's work is charged to the origin's governor.
  `CopyGenerationOwner::Local` therefore carries no permit; the optional
  one step 4 left is removed. The local copy's one charge is its source
  read.
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
- **Left for step 6, with the attachment:** the calls from
  `vod/session.rs` (`observe_control` where a control exchange is
  accepted, `refresh` from maintenance, `settle` from the session's
  terminal cleanup). A session cannot hold a range until it can attach
  one, so those call sites would be dead wiring here.

### Step 6 — Local serving attachment and rendered milestone (P4, P9; build contract step 4)

- **Owner surfaces:** `vod/session.rs`, `vod/shared.rs`,
  `vod/serve/create.rs`, the retained-manifest path, the A1-N publication
  helper, playback-control's rendered report.
- **Change:** a local VOD session can attach to a growing range as its
  rendition: one coherent snapshot backs plan, bytes and playlist (contract
  §5.1), the target is fixed before publication (§5.2), held EVENT views
  stay stable (§5.3), and admission is A1-N's. The first use is the cold
  local copy start, the case the rolling path serves today. Create still
  answers without awaiting an Analysis write. The session's accepted
  rendered report sets the range's `first_frame_rendered`. The rolling
  path remains the answer for any start the range refuses (unknown clock,
  oversized GOP, changed init), by the engine's own typed refusals.
- **Product:** a local range's payload is charged to the playback cache
  budget and shown with its session in Activity; stopping the session
  retires the range and its payload.
- **Acceptance:** the contract's 48-second native rate-1 case; a cold
  local start at 0 and at 90 minutes plays from the range on web, Apple and
  Android; a leader restart during such a session does not interrupt it;
  every refusal falls to the rolling path and plays. Device and fleet
  checks are run by us or by Paul.
- **Tests:** attachment, snapshot coherence and refusal fallbacks in
  `vod/tests`; client attachment and seek tests named by the contract's
  step 5.

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
- **Tests:** a local range's rows fold with an Analysis fill into the
  whole-title set (extend `gap_fill_tests`); the demand survives until the
  first checkpoint; attribution and persist-first for a row set;
  contradiction with a healthy head still quarantines.

### Step 8 — Indexed local copy starts on the engine, old path as fallback

- **Owner surfaces:** `vod/generation.rs`, `vod/plan.rs`,
  `vod/serve/create.rs`.
- **Change:** an indexed local copy start runs a seeked range of the engine
  verified against the index. The plan-verified generation stays in the
  tree and serves every start the range refuses. Nothing is removed.
- **Acceptance:** seeked and sequential rows equal (contract step 3);
  every refusal plays through the existing generation.
- **Tests:** the seeked-range verifier; each typed refusal falling back.

### Step 9 — Fleet acceptance

No code. On the fleet, over an agreed window, count local copy starts by
who served them: the engine, the rolling path after a typed refusal (by
refusal), the plan-verified generation after a refusal. Run the acceptance
list of [PLAYBACK-START-STATUS.md](PLAYBACK-START-STATUS.md) for A1/B1.
Record the counts and the titles behind each refusal in the status ledger.

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
- **Change:** remove the plan-verified copy generation and the rolling
  path's use for starts the engine serves; remove a refusal class's
  rolling code only under the rule above.
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
