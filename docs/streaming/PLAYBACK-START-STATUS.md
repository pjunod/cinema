# Playback start — continuation and merge ledger

**Status:** open · **Owner:** Claude (claude-opus-5-5), session
`session_01BCWL9ypnQEiQUyRaZtMhh3`, resumed from Codex on 2026-11-11 · **Updated:** 2026-11-12.

Companion to the [implementation plan](PLAYBACK-START-PLAN.md) and
[Phase 1 build contract](PLAYBACK-START-BUILD.md). Phase 1 merged in
#1454 (`b08b5b8ec`); Apple build 294 landed in #1466 (`09097650f`).
The 2026-11-09 21:16 UTC fleet verification has all four nodes on
`49c4ec103` (includes #1475/#1476), healthy and ready. This supersedes the
20:23 UTC mixed-build snapshot. Queue and physical-device acceptance remain
open: media1's 20:23 snapshot reported degraded, 3,841 eligible requests and
0 ready in 24 hours; no newer queue measurement is claimed here. Persisted
failure counts are not new failures in that interval.

## Merged batch — index timestamp repair and Source locate eligibility

S4 removes the index-only fatal muxer-diagnostic policy while retaining
HEVC tracing, configuration and complete-video validation. S12 uses the
canonical locate route policy to exclude learners in the existing media
peer query; ordinary media retains learner capacity. Both repairs come from
the [shared-library RCA](SHARED-LIBRARY-PLAYBACK-RCA.md). GPT 6.1 Sol built
S12; Codex owns S4, integration and merge.

Independent implementation review found no actionable findings. Local
Clippy, syntax checks and both focused regressions passed. PR #1475 merged
as `2ad533907` after exact-candidate promotion run 7052 passed. Deployment is included in the later verified `49c4ec103` fleet. The first
preflight stopped on the intentionally pinned membership file hash. Tables
31/32 were refreshed after confirming both mapped authority bodies are
byte-identical to the base; only the media directory changed. Local macOS Clippy
also exposed existing Linux-only processed-upgrade imports/test helpers;
the batch aligns their declarations with their consuming platform.

Apple build 294 is live in internal TestFlight for iOS and tvOS, both
`VALID` and `IN_BETA_TESTING`. Export and upload succeeded from the M5
with the signed-in Xcode account and Cloud Managed Apple Distribution;
[the publishing recovery notes](../PUBLISHING.md) retain the proven method.
The standing publisher has not been migrated or requalified. Android 193
and native prepared-switch/TTFF measurements remain open.

## Merged — A1/B1 first batch (#1495)

#1495 merged as `41e23c193` on 2026-11-12. It carries:

- the Source growing copy engine for authenticated bounded shared playback;
- canonical range observations, normalization and the companion verifier;
- the copy-range consumer (coordination claim, fenced full-source
  attestation, seal, content-addressed v3 blob, claimed
  `fragment_index_publish` job);
- the guarded v3 publication transaction.

The schema is SQLite v127 (coordination and v3 index) and v128
(managed-resource v6), after #1469's v125/v126; Hiqlite chains managed
v4→v5→v6. Older binaries refuse it, so the deploy needs the playbook's
pre-upgrade backup. Clients: Apple 297, Android 198.

The fast lane passed paperwork, preflight, web and Android on the merged head.
Its Rust and Apple jobs had failed on runner infrastructure on earlier heads,
and Paul had it merged. Three plurx-core unit tests are red on main from
before this batch (two `placeholder_census` tests and the source82 layout
test).

## Effort batch — A1/B1 gap filling (#1520, merged into the effort)

Owner: Claude (claude-opus-5-5), same session. The gap-fill draft was written
by a claude-fable-5-2 agent and finished by Opus. Items 1–3 below are built on
`claude/a1b1-gap-fill`; clippy and fmt are clean, and the new focused tests
pass:

1. The copy spawn and head census take `CopyGenerationOwner::{Source,
   Analysis}`.
2. `copyrange::fill_copy_gap` fills each first gap from an Analysis-owned
   generation into the same observation set.
3. The seal folds per-row HDR10 SEI facts (`Hdr10IndexMetadata::merge`) into
   the whole-title metadata the legacy index computes.

A claude-fable-5-2 adversarial review found four majors and six minors, all
addressed:

- The seal publishes the init contract that its own promotion serves
  (`served_init_contract_sha256`), not the head-frozen one.
- An unreadable HDR10 SEI marks the row `hdr10_conflict` instead of ending the
  viewer's copy. The set then refuses to seal, and the legacy pass owns the
  title.
- Each ready source runs as its own unit. A fill that waits on a paused budget
  no longer holds the other sources, and the inbox drains on every wake.
- An incomplete set checks `foreground_source_pressure`, which is read-only,
  before it enqueues or claims anything.
- Failures that recur on every repeat (row limit, unknown clock, unverified or
  late landing, a changed init) discard instead of retrying. The quarantine
  check and the cross-row mastering conflict are now found before any read.
- Fresh fill rows refresh the source's eviction age.

Decision (Paul away): the generation domain stays v1. Bumping it would leave
existing heads desiring the old generation forever. Instead, item 4's consumer
must refuse a v3 blob whose promotion lacks `indexed_hdr10_metadata`, which
item 5 then quarantines and rebuilds.

Coordination request rows of sources never observed again after a restart
stayed `queued` (one per file); item 5 retires them.

## Effort batch — A1/B1 v3 consumer and peer hydration (#1525, merged into the effort)

Owner: Claude (claude-opus-5-5), same session. The item-4 code and its review
fixes were written by a claude-fable-5-2 agent on local branch `item4`. Item 4
below is built; clippy and fmt are clean. No unit test has been run: the
tests run once in CI.

- `crates/plurxd/src/copy_index_v3.rs` is the consumer; the publisher's
  `install_blob`, `v3_root` and `v3_blob_path` are shared with it.
- The owed end-to-end test is `copy_analysis/gap_fill_tests.rs`. Three real
  fills through `fill_copy_gap` (whole title, a head retired early, the
  seeked fill of its gap; seven ffmpeg children with their censuses) must
  merge into the whole-title rows. Its phase is a well-formed value no store
  admitted, its Inputs use the test byte budget, and its head is an Analysis
  generation, not a `CompletedCopyRange`.

A claude-fable-5-2 adversarial review found six majors and six minors, all
addressed: the serve-time init contract, the engine check, the start-path
reads, holder rows that were never restored, the pinned corrupt blob, the
gap-fill test's orphaned children and name, and the store guards. What each
became is in item 4 below.

Decisions (Paul away):

- Cross-origin use needs this node's own full-source receipt, per the build
  contract's "matching full content attestation".
- A serve-time init mismatch cannot fall back inside the running generation,
  whose plan is already the v3 one. It fails that generation with its own
  cause and the next plan falls back.
- No eviction owner was added for the v3 root: evicting a publisher's only
  copy turns a healthy head into a rebuild, which is item 5's to route.

Still open: fleet acceptance of the items below and the limits listed under
items 4 and 5.

## Current batch — a local range's serving half (item 6, plan step 6a)

Owner: Claude (claude-opus-5-5), same session; local branch `step6`,
stacked on step 5 (#1555), not pushed. Clippy `-D warnings` and fmt are
clean, the inventory and formal pins are green. The tests are written and
wait for the fast lane.

Decision (Paul away): [plan step 6](LOCAL-COPY-ENGINE-BUILD.md) cannot land
safely as one PR, so only its range half is built. Nothing serves from a
range yet and no serving path changed:

- `LocalCopyRange::snapshot` is one view under one lock: the playlist,
  the committed segments behind it, init, origin, completion and the bytes
  the range holds in the playback cache, for its session's Activity line.
- `LocalCopyRange::wait_admitted` applies A1-N's one rule. That rule is
  now one function, `transcode::startup_admission_window_ms`; rolling
  publication and the Source range call it too, with the same values.
- A second range of a session whose settlement is removing the session's
  directory is refused `copy_range_session_settling`; the session then
  serves that start another way.

Ruling (2026-11-12): Paul delegated to Fable; Fable chose (b). The range
attaches in the rolling copy start that the `vod_index_pending` fallback
makes (`start_copy_with_audio_offset`). It is served rolling-shaped, as the
Source range is, with no client change, and copyseg answers every refusal.
The build contract (§5.1, steps 4 and 5) and plan steps 6, 8 and 10 are
amended to match.

Found, and stopped before (Paul away): the rolling session has no backing
seam. `Session` is the copyseg producer. It reads `dir`, locks its child
to suspend, replace and reap, feeds `SegmentIndex` from copyseg's playlist,
and owns copyseg's scratch, upload, retention and retained collection. The
cached asset is already a second backing, built as `cache_manifest`
branches. The seam is a typed `RollingBacking` with copyseg, cached and
range implementations. It rewrites every rolling and cached serve path and
the hand-built `Session` test fixtures, so it is its own change, made
before the attachment. Plan step 6 lists what it must answer.

## Previous batch — a local range's input admission (item 6, plan step 5)

Owner: Claude (claude-opus-5-5), same session; written by a claude-fable-5-2
agent on local branch `step5`, stacked on #1548, not pushed.
[Plan step 5](LOCAL-COPY-ENGINE-BUILD.md) is built, with the fixes from its
review (8 findings). Clippy `-D warnings` and fmt are clean, the inventory
and formal pins are green. No unit test has been run: the step's tests are
written and wait for the fast lane.

`LocalCopyRange` is the range half of a local VOD session. Nothing starts
one yet and nothing is served from one; step 6 attaches a session to it.

- It registers one foreground custody keyed by the local playback, with
  the keys a rolling local session registers. The census reads through an
  Input lent that custody and the primary's Input takes it over: one row,
  written once, as a rolling start's.
- Decision (review): it presses exactly as a rolling session does. The
  rolling source observation is now one function that maintenance and the
  range's read bank both call; a paused or buffering viewer still
  reporting lifts pressure while held. The Source range keeps its rule.
- Decision (Paul away): a local range takes no copy admission. No local
  copy holds an encoder-pool permit today, and taking one could queue or
  refuse a local copy start that never queues. The optional permit step 4
  left on the local generation owner is removed.
- It has no lease, loop or timer. The session's accepted control
  exchanges, maintenance and retirement drive it (`observe_control`,
  `refresh`, `settle`); settlement joins the child, the pipe tasks and the
  Input before it removes the payload, manifests and session directory.
  Dropped unsettled it retires; a panicking run task ends it.
- Boot clears `copy-ranges/observations` and `copy-ranges/local-observations`
  where it clears the transcode scratch that holds the payloads.
- Decision (Paul away): the calls from `vod/session.rs` into the range are
  left to step 6. A session cannot hold a range before it can attach one.
- Fixed on Paul's call: a killed Source range's manifests under
  `copy-ranges/observations` used to stay forever; boot now reaps them.
- The trace comparison is written, not run. Both sides' observations come
  from the shared function; the rolling flow's holds are replayed and the
  range's committed media is set by hand (the fixture is shorter than the
  low watermark); the plan lists what that leaves unmeasured.

## Previous batch — a local owner for the copy engine (item 6, plan step 4, #1548)

[Plan step 4](LOCAL-COPY-ENGINE-BUILD.md) is built on `step4`, with its
review fixes (2 majors, 8 minors). Its tests are written, not yet run.

- Range storage and generation ownership each have a typed local owner,
  made only from a live local viewer with a named playback. Nothing
  constructs one outside tests.
- One copy pipeline derivation for the Source range, Source and local
  renditions and the `vod_indexed` hint: the manager's injected
  capability and the one stored probe. One file, recipe, node and stored
  probe have one copy-index source key. Only a local copy start measures a
  missing HEVC census.
- Behaviour change, shared playback only: a stripped Dolby Vision HEVC
  copy on a node whose FFmpeg has `dovi_rpu` is re-keyed. The plan's step
  4 lists the deploy cost.
- Decision (Paul away): the Source rendition path and the planning hint
  were changed with the range, which the plan did not name; the rendition
  reads the index by the key the range publishes under.
- Found: no binary reads a checkpoint manifest back; compatibility is
  pinned by a golden file derived by hand from `4b125e6`.

Open, post-deploy: on a node whose FFmpeg has `dovi_rpu` (7.1 or later),
play a Dolby Vision title stripped over a share before and after a local
playback of it, and record that both resolve one v3 head.

## Previous batch — the copy index rides the content-analysis pass (item 6, plan step 3)

Owner: Claude (claude-opus-5-5), same session; written by a claude-fable-5-2
agent on local branch `step3`, stacked on `item6b`, not pushed.
[Plan step 3](LOCAL-COPY-ENGINE-BUILD.md) is built, with the fixes from its
adversarial review (2 majors, 9 minors). Clippy `-D warnings` and fmt are
clean, the inventory and formal pins are green. No unit test has
been run: the ride's tests are written and wait for the fast lane.

The cluster fragment-index build job's pass now also observes the whole v3
copy index of its lead identity from the read it already makes, and hands it
to the Analysis worker under the claim and source custody the pass holds.
The worker takes the coordination claim itself, seals and publishes. A
title indexed this way needs no second pass over its video. The standalone
form (a demand filled by `fill_copy_gap`) is unchanged and still builds
every title the ride does not.

- Added to the pass: one `framecrc` packet-clock output on descriptor 7,
  after the index outputs. No source read, no global option, nothing inside
  the argv the cluster cache key hashes. The child runs the copy recipe's
  bitstream filters a second time for that output and checksums every
  packet.
- The ride consumes the lead reader's own parsed fragments, by move, in
  place: no observer task, no byte copy, no second parse or Dolby Vision
  conversion. One thing is done again, the engine's per-row NAL-header
  walk for HDR10 SEI; the plan says why.
- Not ridden, by design: the non-cluster local pass (it holds no claim to
  name), sibling identities of a shared read, any container but Matroska
  and ISO base media, a source that already has a healthy current head, a
  source whose last riding pass failed, and any title whose format start
  is not a whole number of video ticks or whose first packet is not the
  stream's first picture.
- Decision (Paul away), a correction to the plan: the engine's clock needs
  the global `-copyts`, which the pass cannot take. It would shift the
  subtitle tracks the same pass stores, and it disables FFmpeg's
  discontinuity correction for the pass's own index. The ride instead
  recovers the source origin from the pass's probe (format start, checked
  against the video stream's first presentation time) and abandons on any
  disagreement. The plan's Step 3 "Clock" paragraph records this.
- Decision (Paul away): a pass's rows only ever add an index. A set that
  holds nothing but a pass's rows never quarantines a healthy head, and a
  pass never displaces rows already held or evicts a demand or a viewer's
  set. A viewer's own contradicting range still quarantines.
- An abandoned ride is counted
  (`plurx_copy_index_ride_total{outcome="abandoned"}`), logged at warn
  with its reason, shown on the pass's Analysis row and remembered, so
  that source's next pass does not ride.
- A second read remains for a non-HEVC pass: one whole-file hash phase,
  because only HEVC passes record a full-source receipt.

Open, post-deploy acceptance: the ride has only run against 12-second
generated fixtures in tests that have not been executed. On the fleet, a
UHD HDR10 remux and a Dolby Vision title (profile 7 and 8, preserved and
converted) must each ride to `observed` with no abandonment, seal the blob
a standalone fill seals, and leave the pass's wall time and the node's
memory within noise. Record titles, counters and any abandonment reason
here.

## Previous batch — A1/B1 local playback demand (item 6, first step)

Owner: Claude (claude-opus-5-5), same session; written by a claude-fable-5-2
agent on local branch `item6`, stacked on `item5`, not pushed. The first
step of item 6 below is built; clippy `-D warnings` and fmt are clean, the
inventory and formal pins are green. No unit test has been run. A
claude-fable-5-2 adversarial review found four majors and six minors, all
addressed below except the persisted trigger value, which the schema
forbids without a migration (item 6, Attribution).

Decision (Paul away): the full unification is not one safe PR. Moving local
rolling copy and indexed-VOD copy onto the growing engine replaces the local
serving path and needs a local owner at four Source-bound points of the
engine. This batch builds only what leaves every serving path untouched:
the typed local authority and a row-less demand the Analysis worker answers
with the same engine. The cost and the remainder are under item 6.

## Previous batch — A1/B1 discrepancy quarantine (item 5, #1532)

Owner: Claude (claude-opus-5-5), same session; written by a claude-fable-5-2
agent on branch `item5`, pushed as #1532 into the effort. Item 5 below is
built; clippy `-D warnings` and fmt are clean, the inventory and formal pins
are green. Its unit tests run once in CI. A claude-fable-5-2
adversarial review found three majors and six minors, all addressed in item
5 below.

Decisions (Paul away), each stated under item 5:

- A quarantined pointer is not readable. The plan said the old pointer stays
  readable until the successor publishes; the store has never done that, and
  a contradicted index must not keep serving.
- A quarantine on a doubt (no holder, a contradicting observation) does not
  ban its blob. A set that still holds contradicting rows cannot reseal the
  quarantined bytes, so an identical reseal means the observation was the
  faulty side and the index was good. A sealed-init mismatch does ban its
  blob: a reseal repeats the seal's own promotion, so identical bytes
  disprove nothing about the init a generation served.
- A coordinator retired as stranded does not use up its generation, or a
  restart would lock every half-observed source out for the 30-day prune.

## After the first A1/B1 merge — remaining build order

The first merge publishes a v3 index only when playback itself observed the
whole title. These items remain, in build order; each extends an existing
owner and adds no gate, timer or watchdog.

### 1. Common spawn adapter

`copyrange::spawn_source_generation` and `run_source_census` take
`SourceRenditionOwners`, a `SourceGenerationDispatch` and a
`SourceGenerationAuthority`. Give both a typed owner instead:

- `Source { owners, dispatch, authority }`: today's arguments, unchanged.
- `Analysis { phase }`: an exact `AnalysisPhysicalPhase` whose custody is
  already active, with `SourceInput::open_background_held` reading under
  `SourceReadBudget::from_analysis_phase`. No admission permit, no Source
  grant and no media-serving authority.

The ffmpeg recipe, `ObservedCopyMux`, normalizer and companion verifier stay
the same code on both paths. AV output is discarded only after the companion
verifies it. Every pipe task and the reap join before the phase finishes.

### 2. Background gap filling

When a pending source set (`copy_analysis::work::PendingSources`) is
incomplete and `begin_analysis_source_phase` admits it (its capacity clause
is the idle test), run one Analysis-owned generation through the adapter:

- From title start when `begin` is missing; otherwise from the first gap.
- Feed its rows into the same `CopyObservationSet`, so overlap equality with
  playback rows is checked, not assumed.
- Stop at the next observed row or EOF. Foreground pressure pauses the budget
  through the existing pause controller.
- A seeked start uses the verified seek-bootstrap landing, never a long
  sequential fallback.

The full-source attestation phase reads every byte. Fold the whole-title
HDR10 maximum scan into that same read (item 3).

### 3. Whole-title HDR10 metadata from row facts

The legacy local index promises `indexed_hdr10_metadata` with the scanned
`hdr10_sei` (`fragindex.rs`, the `--plurx-hdr10-index` argv revision). Each
observed row records its own HDR10 SEI facts. The seal folds them, in any
order, into that same maximum, and publishes the init contract of the
promotion it seals. A v3 index from before this fold lacks the flag; never
bridge one into `put_fragment_index`.

### 4. v3 consumer and peer hydration — built, on main (#1525, #1531)

VOD planning reads the exact v3 head by key and prefers a healthy, current
variant. It reads the variant locally or hydrates it from a listed holder
over the signed internal route, verifying the digest before install. It
refuses a pre-fold blob and checks the served init against the sealed
contract. Any miss falls back to v2, then the node-local index. #1525's
description records the review fixes and known limits.

### 5. Discrepancy quarantine — built, on main (#1532, #1540)

The worker quarantines a head under its claim, only on evidence about the
variant and only under the producer's engine:
- a contradicting range or fill;
- reader defects (an unindexed HDR10 blob, no holder, a same-engine
  sealed-init mismatch).

The set adopts the exact successor generation and rebuilds it. A doubt
quarantine (contradiction, no holder) never bans the same bytes; a
sealed-init mismatch does. `CopyIndexCacheOwner` evicts v3 blobs but never
the last holder of a readable head. Stranded coordination rows are retired,
carrying their attempts. #1532's description records the review fixes and
known limits.

### 6. Local playback observations — first step built

Built: local playback asks, and the one copy engine answers in the
background. The local serving paths are not migrated.

- `CopyObservationAuthority` has a `Local(LocalCopyAuthority)` variant. Only
  `LocalCopyAuthority::of` makes one, from a `PlaybackViewerDemand` that
  passes `require_local_authority`; a Sharing principal is refused, so it
  never reuses a Source grant. It holds no assignment and no Source
  interest, and grants no read.
- A local viewer's copy start that finds no v3 index sends a row-less
  `CopyIndexDemand` through the copy-analysis inbox, whether or not a
  legacy or v2 index serves that start: v3 is the index every node shares,
  and playing a title is what migrates it. The start plans from the
  existing index order and waits on nothing. The demand names the source
  the lookup itself derived (no second stat), the authority is checked
  first, a changed fragment-index engine asks for nothing, and a source
  already asked for is refused before the inbox until its work settles.
- The worker holds the demand as a source without rows, and never before
  one retry interval has passed, so its first attempt is a later dispatch
  wake and cannot run against the start that raised it. A demand makes
  room only among other demands; it never evicts a set that holds rows.
- When the source domain is idle the worker claims the coordinator and runs
  the copy engine itself: the first Analysis generation is given no
  contract to check, reads the whole title and establishes the set
  (`CopyObservationSet::from_analysis`), then attestation, seal and
  publication are items 1-2 unchanged. The set keeps the source as
  playback observes it, so later playback ranges still fold in. No fill
  runs on an engine that changed under the process.
- A demand that meets a quarantined head adopts its repair generation, so
  a local play of the title starts the rebuild item 5 left waiting for a
  Source observation. The one exception is a sealed-init mismatch on this
  node's own engine, which is not rebuilt.
- A generation whose coordination request already ended (`failed`,
  `cancelled`, `ready`) is dropped, logged at debug, instead of being asked
  about on every pass. A request retired as `coordination_stranded` is
  never counted as ended. This also applies to Source sets.

Attribution. The request a demand creates has the id
`local-play.u<user>.<playback>.<suffix>` (the playback is left out when it
does not fit 64 bytes or cannot sit in a URL path). `analysis_requests.
trigger` is constrained by the schema to `admin`, `background` and
`playback`, and `playback` means a waiting viewer to the claim and turn
logic, so a fourth value needs a table-rebuild migration on both backends
that this batch does not make; the stored trigger stays `background`. The
id shows wherever requests are listed: `GET /api/v1/analysis/jobs` and
`/api/v1/analysis/jobs/{id}` (`request_id`/`job_id`), and the Analysis page,
whose row details show it as Request and whose Trigger reads "Local
playback" for it, in the row and in Copy details. The row's Cancel button
stops the build. The worker also logs the user and playback at info.

Cost: one extra whole-file read per title and copy identity. The first
local copy start of a file under a given copy-video identity (plain copy,
Dolby Vision preserved, Dolby Vision converted are distinct identities)
without a v3 index leads to one background read of the whole file by the
copy engine, on top of the legacy node-local index pass's own read, and
where no full-source receipt exists one whole-file hash. Both are Analysis
physical phases, admitted only while the source domain is idle and paused
under foreground pressure.

The rest of item 6 has a build plan any agent can work:
[LOCAL-COPY-ENGINE-BUILD.md](LOCAL-COPY-ENGINE-BUILD.md), ten ordered
steps with an Execution log. Its step 1, durable demand, is built on
`item6b` (not pushed), with its review fixes:

- A demand makes its coordination request once, on its first attempt,
  before the source is idle. A demand dropped unbuilt retires it.
- A restarted worker takes up the `local-play.` requests its predecessor
  left, queued or still claimed by that dead process, and keeps them out of
  the stranded retirement. It opens no file: the request's recorded object
  version is carried and the unit's source fence refuses a changed object.
  A store error fails the step, which the next pass makes again.
- A recovered demand's audio is playback's default (the first track copied
  when the copy pipe carries its codec, encoded otherwise) until a fresh
  local demand brings its own recipe. A user who no longer exists is not
  named.
- Found, not fixed here: a coordination request newer than a legacy
  `fragment_index` request of the same file and target stops that legacy
  publication from advancing the v2 head (the "no newer request" guard of
  `background_job_publish_fragment_command`). Source coordination rows
  have done so since item 2. The fix redefines that trigger and is one
  schema version on both backends; it is plan step 2.

Plan steps 3, 4 and 5 are built on `step3`, `step4` and `step5`, and step
6's range half on `step6`: see the batches above.

Remaining for item 6 (plan step 2, step 6's attachment, steps 7-10):

- Local rolling copy and indexed-VOD copy still run their own producers
  (`copy_pipe_args` to the rolling segmenter, and plan-verified VOD
  generations). Neither runs the growing engine, so local playback commits
  no ranges and contributes no rows: a locally played title is indexed by a
  second read, not by the read that served it.
- The growing engine is still run only by the Source worker. A local range
  exists (plan steps 4 and 5) and nothing starts one: it still needs a
  local serving attachment and the session's calls into it (build contract
  step 4: growing attachment and A1-N admission for local sessions; plan
  step 6) before any local path can be moved onto it.
- `CopyObservationSet::observe` accepts a `Local` range, but nothing
  produces one. When the engine runs for local playback, the range's
  `CompletedCopyRange` carries `CopyObservationAuthority::Local` and the
  row-less demand becomes redundant for that start.
- A demand that has not yet made its first attempt (one retry interval
  after the start) is in memory only; a restart inside that window forgets
  it and the next local start asks again. Demands are dropped on non-unix
  builds, which have no copy engine.
- The persisted trigger is not `local_playback` (see Attribution). Doing
  that is one migration: rebuild `analysis_requests` with the wider CHECK
  on SQLite and hiqlite, and add the value to both `valid_request`s.
- Not unit tested: the VOD start emitting the demand, the Analysis page's
  label, the ceiling case of a stranded request, a demand refused because
  every held source has rows, and the worker's build of a demanded source
  end to end (the unseeded first generation runs in `gap_fill_tests`; the
  claim and phases around it are items 1-2's).

### Acceptance for the next merge

Items 1–4 reached the effort branch through #1520 (`f503c7c6a`) and #1525
(`4a0da349e`), with main merged in at `22d2a981b`. The effort → main PR's
fast lane is the one test run. The fleet checks below need Paul's deploy and
are recorded after it.

- A partially played shared title publishes after background filling, with
  no playback interruption on any node.
- An HDR10 and a DV title's v3 index serves the same init as their legacy
  index.
- A second node plays the title through hydration.

## Native admission — A1-N, PR #1476

**Owner:** Codex, chat `01a1c172-ef6a-77e3-8422-95798308bc5c`.
Implementation and local acceptance are recorded in
[PR #1476](http://forge.lan:3000/noirr/plurx/pulls/1476); that PR is the
current landing-state authority. This is a post-promotion repair to Phase 1,
not completion of A1's growing copy engine.

The fixed target remains 16 seconds. Exact hls.js retains its qualified short
bootstrap; native/absent/unknown transport admits three advertised target
intervals, with one bounded complete cut for initial legacy admission. The
normal download-frontier cap resumes afterward. No actor deadline or feature
switch changes. The shared Gran Turismo fixture pins the actual emitted
first snapshot; a count of three segments plus EVENT alone is insufficient.

Eight focused Rust cases and strict workspace Clippy passed. Matched-source
native WebKit off/cold/warm cases reached playing at 759/745/713 ms; the
24-second positive continued past the first reload. The old short controls
remain non-playing, and the completed short title starts. Successful cases
were retained while failed observer checks alone were rerun. These are local
matched-source receipts; full daemon/fleet acceptance remains open.

## Initial subtitle intent — F1-N, current corrective batch

**Owner:** Codex, chat `01a1c172-ef6a-77e3-8422-95798308bc5c`.
A1-N is deployed to all four servers. Real Safari 27.2 foreground Gran Turismo
started at 3,094 ms to native frame and 3,679 ms to `playing`, with progress.
Subsequent single-click trials exposed a separate automatic reopen: the
400 ms default-subtitle timer fires during create and routes through the
viewer-change owner. A captured stack pins `presentPlayerChrome → setSub →
restartPendingPlaybackOpen → play`; there was no subtitle change event.
Two foreground trials reached frames in 2,337/3,789 ms but each cancelled its
first create and opened another, so neither receives clean-start credit.
An earlier hidden-tab trial produced no frame and is retained as a control.

The current independent repair removes that timer and binds the server's
chosen default into the initial recipe, applying text captions at attachment.
Native track registration is attachment-owned and cleaned up on replacement or
Close; adversarial review identified this edge before validation. A Linux-only
qualification test helper now uses the same platform guard as its callers,
repairing the macOS lint failure found by the required hook.
Explicit viewer changes keep their existing owner. Regressions and plan
acceptance include the exact slow-create/default-forced-subtitle case.
Current landing and fleet acceptance authority: [PR #1480](http://forge.lan:3000/noirr/plurx/pulls/1480). Its source diff, review, current checks and later deployment/measurement receipts supersede this initial qualification snapshot; no ten-start acceptance was claimed at that snapshot.
The temporary non-admin Safari account protects the user's actively advancing
resume position and will be removed after qualification. Private runtime
receipts are retained outside the repository in the owning task's status page.

## Current process — one final candidate review and validation

Paul's 2026-11-09 direction supersedes the original per-lane/design-review
schedule: batch normal commits into a reviewable PR; adversarial review
when ready to merge; fix findings; run the final-candidate checks once and
repeat failed checks only. Required CI remains a merge condition. Keep PR
ownership, current state and remaining merge requirements explicit. All
task workspaces, scratch, receipts and build output use external agent
storage. No feature gates are added.

A1/B1 and later lanes remain unbuilt. S1–S3/S5–S11 join A1 rather than
introducing a separate shared-library engine. The two remote ownership
follow-ups in the history below remain open.

## Execution history


| Date | Lane | Model / session | Action |
|---|---|---|---|
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Rev 1 written from the review; rulings 1–4 recorded. |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Fleet state read (queue wedged at cap; release-as-fence); Fable review applied (rev 2, not what #1401 merged — bridge propagation). |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Astra review applied; rev 3 is the first revision build agents may claim. |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Ruling 3 mechanism settled (M6); Phase 1 build contracts written (QUEUE, CREATE). |
| 2026-11-07 | H2, C2, C3, C1, D2, R1, H1, G1, D1 | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Phase 1 claimed. Lanes integrate on `effort/playback-start`; per Paul (2026-11-07) one adversarial review and one fast-lane run on effort → main rather than per lane. H2/C2/C3 built; H1 built by a Sonnet sub-agent; C1, D2, R1 by Opus sub-agents. |
| 2026-11-08 | D2 (review 1 S4) | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Open gap, not fixed: a remote start whose reply is lost on both the ask and the one re-ask (`start_remote_recovering_lost_reply`) moves to the next candidate under the same incarnation while the first worker may still hold a session for it. That first producer runs until its confirmation watcher sees another owner's activation of the incarnation (≤ 500 ms after it lands) or, when no activation comes, to `REMOTE_ACTIVATION_CONFIRMATION_WINDOW`. Closing it needs the transport to tell "never delivered" from "delivered, reply lost" (`PeerTransportError` does not) so only the first moves on. Pinned by `media_sessions::tests::a_dropped_remote_start_reply_takes_the_session_the_worker_already_holds`. |
| 2026-11-08 | H2, C2, C3, H1, R1, D2, C1, G1 | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Landed on the effort: #1424, #1427, #1428, #1431, #1433, #1434. Fable review 1 (landed lanes) → fixes #1436, #1440; Fable review 2 (C1, G1) → fixes #1438. Combined effort head clippy-clean; paperwork vs main 16/16. D1 building; then D1 review, one fast-lane run, effort → main. |
| 2026-11-08 | D1 (review 3 S7) | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Open gap: remote-owned ordinary sessions never upgrade (the create registers the intent only where the ingress owns the route). Follow-up: carry the intent on the remote start dispatch and trigger from the owner actor's presented phase. |
| 2026-11-09 | continuation | Codex / 01a1c166-b5b2-7033-bd74-77816ba09733 | Phase 1 merge ledger reconciled against main `8ab61a779`. Media1 reports that build; queue still degraded (3,840 eligible, 0 ready in 24 h), so merged implementation is not fleet acceptance. Signing publisher is offline; its last attempted export reported no account and no distribution identity/private key. |
| 2026-11-09 | S4, S12 | Codex; GPT 6.1 Sol for S12 | Root-cause follow-ups from [shared-library RCA](SHARED-LIBRARY-PLAYBACK-RCA.md): index muxer timestamp correction and route-eligible Source locate directory implemented; review and validation pending. No feature gates. |
| 2026-11-09 | process | Paul; executed by Codex | Current direction supersedes the original per-lane/design review schedule: batch normal commits into a reviewable PR, adversarial review when ready to merge, fix findings, then one final-candidate validation run with only failed checks repeated. Keep all task storage on the external agent drive. |
| 2026-11-11 | A1/B1 | claude-opus-5-5 / session_01BCWL9ypnQEiQUyRaZtMhh3 | Resumed from Codex; layers checkpointed, main merged, compile/clippy clean; copy-range consumer and v3 publication trigger built with real-store regressions. |
