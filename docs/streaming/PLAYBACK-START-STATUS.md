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

## Current batch — A1/B1 gap filling (#1520, draft)

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
stay `queued` (one per file, as before this batch); retiring them belongs
with item 5.

## Current batch — A1/B1 v3 consumer and peer hydration (item 4)

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

Still open: fleet acceptance of all three items below, item 5's quarantine of
a refused or contradicted head, and the limits listed under item 4.

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

### 4. v3 consumer and peer hydration — built

- VOD planning (local and Source renditions) asks
  `copy_index_v3::resolve` first, unless the fragment-index engine changed,
  in which case it keeps the node-local order as v2 does. Without a usable
  head the v2 cluster and node-local order is unchanged.
- The start path costs one primary-key read: `copy_index_v3_variant` returns
  this node's own head (this node as origin, its own `inspect_source` object
  version), the variant row it points at and whether this node is a listed
  holder. The catalogue namespace is cached in VOD shared state.
- Only on a miss, and only when this node holds its own full-source receipt
  (a full v2 receipt or a regime-1 attestation, as the publication accepts),
  the per-file listing runs. Another origin's head is used only when that
  receipt names the variant's source digest, and never when this node's own
  head is quarantined for the same blob.
- The blob is read from this node's v3 root, or hydrated from a holder in
  `copy_index_v3_locations` over
  `GET /internal/media/copy-index-v3/{source_key}/{blob_sha256}` (the v2
  route's peer auth, read permits, size bound and learner-matrix entry). The
  route serves only a readable head's variant. It is verified by size and
  SHA-256 and installed with the publisher's staged write and disk budget.
- Holders: a hydration records this node best-effort, and so does a verified
  local hit the head read showed as unlisted, so the common start writes
  nothing. A peer's row is retired only on the route's own positive 404
  (`copy-index-v3-variant-absent`); a node without the route, a refusal or a
  damaged transfer retires nothing. A corrupt local blob (read in full, wrong
  digest or size) is removed after the read pin is dropped; a stat or read
  error deletes and retires nothing.
- Decode is `decode_cluster_fragment_index_blob_v3` against the artifact row's
  source and output identity, and the planning digest is recomputed with the
  row's init contract. A blob without `indexed_hdr10_metadata` is refused and
  planning falls back; item 5 quarantines it.
- Serve time: the recipe carries the sealed init contract. Each generation
  computes `served_init_contract_sha256(muxer init, index.promotion)` as soon
  as its muxer init exists, before any identity or init is written. A
  mismatch fails that generation as `SealedInit` (`rendition_init_changed`)
  and marks the contract unserved for this process, so the reopened session
  plans from its next index.
- A readable head is healthy and published for the generation it still
  desires, so a head repaired after a quarantine is readable too.

Known limits, for the items that follow:

- `copy_index_v3_heads` has no `(catalog_namespace, file_id)` index, so the
  per-file listing scans. It belongs in the next schema version; this batch
  adds no migration and reaches the listing only behind the receipt check.
- The v3 root has no eviction owner. Its blobs are charged to the playback
  cache budget but nothing registered evicts them (the v2 root has
  `FragmentCacheOwner`), so no eviction path forgets a v3 location today. An
  owner added later must forget this node's row when it removes a blob.
- A healthy head none of whose holders can supply the bytes falls back to the
  next index, as a miss does. Item 5 routes it to rebuild.
- A rendition planned from v3 is keyed by the contract key, for Source
  renditions as well. A session re-prepared after a v3 publication therefore
  gets a new rendition and re-cuts its segments once. The retained-manifest
  check compares the delivery recipe (`LogicalOutput`), not the index or
  plan, so it does not cover a plan change on resurrection; the same holds
  for the existing node-local to v2 switch. Equal rows give an equal plan.
  Binding a resurrected session to its original plan needs the plan's digest
  on the durable route and is not built.

### 5. Discrepancy quarantine

A consumer that finds a healthy head's bytes contradict a fresh observation
calls `quarantine_copy_index_v3_head`. The deterministic successor
generation re-enters items 1–2. The old pointer stays readable until the
successor publishes.

### 6. Local playback observations

Local rolling copy and indexed-VOD copy become the same growing engine.
`CopyObservationAuthority` gains a typed local demand variant that never
reuses a Source grant.

### Acceptance for the next merge

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
