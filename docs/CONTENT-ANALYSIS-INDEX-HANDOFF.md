# Content-analysis index — exact skip markers, an explicit queue, and its instrumentation

**Status:** ready to build · **Executes:** item 8 of
[PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md)
§6, specified in [PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
§6.5 and §6.6 · **Written:** 2026-08-30 · **Suggested branch prefix:** `agent/analysis-`

Companion to the protocol plan (what the system must become) — this is *the
milestone that indexes content and makes skip markers exact*, written so it can
be built without reading the rest of the control-protocol programme.

## 1. Orientation — read this, work like this

Read, in order: plan §6.5 and §6.6 in full; then §2 and §3 of this file, which
record what the code actually looks like today so you do not have to rediscover
it; then work milestone by milestone from §7.

**This item is deliberately separable from the playback-control rewrite.** It
shares no file with the control actor. That is the reason it can be built in
parallel, and it is also a constraint: if a step seems to require editing
`crates/plurxd/src/playback_control.rs`, **stop and flag it** rather than
editing. See §6 for the exact boundary.

Three standing instructions:

- **Authored chapters first, detection much later.** §7's M1–M3 require no new
  media analysis at all and deliver most of the user-visible value. Detection
  (M5+) is a bounded research pass with an evaluation gate. Do not blur them.
- **Write the test before the implementation** for anything involving claims,
  leases, cancellation, or state transitions. The queue in §5 is a distributed
  state machine; its bugs do not reproduce on demand.
- **A wrong automatic skip is worse than no skip.** This governs every default
  in this document. When in doubt, show a button rather than skipping.

## 2. What exists today — verified 2026-08-30

Re-verify each of these against the named file at build time in case it moved.

| Thing | Where | State |
|---|---|---|
| `SourceIdentity` (size · mtime · argv fingerprint) | `crates/plurx-core/src/segplan.rs:91` | Built. This is the identity everything in §4 and §5 keys on. |
| `Marker` wire struct | `crates/plurxd/src/http/stream.rs:448` | Built. `kind` · `label` · `start_ms` · `end_ms` · `chapter`. Three clients decode it. |
| `DecisionResponse.markers` | `crates/plurxd/src/http/stream.rs:597` | Built, and already on the live path. |
| `markers_from_chapters` | `crates/plurxd/src/http/stream.rs:955` | Built and pure. ffprobe chapters → markers, with title classification and a boundary/duration fallback. |
| `markers_for` | `crates/plurxd/src/http/stream.rs:1067` | Built. Reads stored probe chapters, falls back to probing, backfills the cache. **Recomputed per request; nothing is persisted as an annotation.** |
| Fragment index build | `crates/plurxd/src/fragindex.rs` (`build`, `identity_for`) | Built. Video-only; **does not decode audio, frames, or OCR text.** |
| Fragment index store | `crates/plurx-core/src/store/mod.rs:2346` (`put_fragment_index` / `fragment_index` / `forget_fragment_index`) | Built. Node-local packed sidecar. |
| Background indexing walk | `crates/plurxd/src/state.rs:3669`, scheduled by `DueJob::BuildFragmentIndexes` (`crates/plurxd/src/schedule.rs:41`) | Built. **Cursor-only walk** — this is what §5 replaces. Off by default; interval setting `playback.vod_index_mins` (`crates/plurx-core/src/store/mod.rs:398`). |
| `POST /api/v1/items/{id}/reanalyze` | `crates/plurxd/src/http/items.rs:157` | Built. **Re-runs ffprobe synchronously to repair displayed facts.** It is not an index queue. Do not redefine it. |
| SQLite migrations | `crates/plurx-core/src/store/sqlite/mod.rs:45` (`MIGRATIONS`), version = `MIGRATIONS.len()` | Append-only array. Add new tables as a new entry; never edit an existing one. |
| Hiqlite (replicated) schema | `crates/plurx-core/src/store/hiqlite.rs:116` onward | `CREATE TABLE IF NOT EXISTS` block. Replicated tables go here **and** in the SQLite migration. |
| Store contract suite | `crates/plurx-core/tests/store_contract.rs` (~15k lines) | Every store method is proved against both SQLite and a three-voter cluster. New methods must be added here. |

### 2.1 The gap, stated plainly

Skip markers today are **derived per request from ffprobe chapters and never
persisted**. There is no annotation record, no provenance, no confidence, no
manual override, and no way for an operator to see or correct one. A file with
no chapters gets a duration-derived guess labelled `chapter: false`.

Everything in §4 exists to replace that derivation with a persisted, versioned,
correctable annotation set — without changing the wire shape three shipped
clients already decode.

## 3. The three storage components

"The index" is one operator-visible thing with three storage components, not
one Rust struct. Plan §6.5 is explicit about why, and the reason is load-bearing:

```
  ┌──────────────────────────────────────────────────────────────┐
  │ analysis generation  (what the UI shows as one thing)        │
  ├────────────────────┬───────────────────┬─────────────────────┤
  │ structural         │ timeline          │ detector feature    │
  │ fragment index     │ annotation set    │ sidecar             │
  ├────────────────────┼───────────────────┼─────────────────────┤
  │ node-local         │ REPLICATED        │ node-local          │
  │ pipeline-versioned │ source-versioned  │ detector-versioned  │
  │ EXISTS TODAY       │ BUILD THIS        │ BUILD LATER (M5+)   │
  └────────────────────┴───────────────────┴─────────────────────┘
```

**Why the split is not negotiable.** `FragmentIndex` is serialized into a packed
node-local sidecar of structural rows. Adding a Serde field to it would *not*
make that field replicated, and it *would* invalidate every structural index on
the cluster the moment a detector version changed. Annotations are semantic,
cheap, and must survive node loss; fragment rows are structural, expensive, and
are legitimately per-node. They are different data with different lifetimes.

The UI presents all three as one analysis generation with explicit per-node
coverage. That is a presentation decision, not a storage one.

## 4. Contract — the timeline annotation set

### 4.1 The records

From plan §6.5, to be placed in `plurx-core` beside `SourceIdentity`:

```rust
pub struct TimelineAnnotation {
    pub kind: AnnotationKind,       // intro | recap | credits | preview
    pub start_ticks: i64,
    pub end_ticks: i64,
    pub timescale: u32,
    pub start_ms: i64,              // normalized convenience value
    pub end_ms: i64,
    pub provenance: AnnotationProvenance,
    pub confidence_millis: u16,     // 0..=1000, not a float
    pub detector_version: String,
    pub manual_override_revision: Option<u64>,
}

pub struct TimelineAnnotationSet {
    pub source_identity: SourceIdentity,
    pub generation_id: String,
    pub version: u32,
    pub annotations: Vec<TimelineAnnotation>,
}
```

Every literal here earns its place:

- **Ticks are the authority; milliseconds are a convenience.** Ticks use the
  file timeline. Milliseconds are stored so that three clients do not each
  invent their own rounding and disagree by a frame.
- **`confidence_millis: u16`, 0..=1000 — not a float.** It is replicated and
  compared; a float would make two nodes disagree about equality.
- **`manual_override_revision`** is what makes an administrator's boundary
  survive re-analysis. See §4.3.

Enforce on write: `start < end <= duration`; overlaps of the same `kind` are
normalized; a marker is keyed by current source identity plus
detector/manual revision.

**The seek target is the marker's exact end time.** A rolling producer may
start at the clean boundary *before* that time, but the player still seeks
forward to the exact film time after attach. Do not conflate the two.

### 4.2 Evidence ranking

Detection is ranked, and the ranking is the specification:

| Rank | Source | Provenance | Auto-skip? |
|---|---|---|---|
| 1 | Authored chapters with recognized intro/recap/credits titles | `authored` | yes |
| 2 | Recurring audio/video fingerprints across episodes, **plus a second boundary-refinement pass** locating the actual frame/audio transition | `detected` | only above the confidence floor |
| 3 | Credits-specific visual/text and audio transition evidence, after the detector passes its evaluation gate | `detected` | only above the confidence floor |
| 4 | Duration-only estimates | `estimated` | **never by default** |
| 5 | An administrator's manual boundary | `manual` | yes, highest confidence |

Rank 2 exists to stop a coarse fingerprint window being stored as though it
were an exact boundary. A fingerprint tells you *roughly where*; it does not
tell you the frame. Storing the window as the marker means every skip lands
seconds early or late, forever, with full apparent precision.

**Precision in a stored timestamp is not proof the semantic classification is
right.** Clients auto-skip only authored, manual, or detector results above the
configured confidence floor. Lower-confidence markers may still show a clearly
labelled manual button.

### 4.3 Manual override

A `manual` boundary survives ordinary re-analysis. Replacing one requires a
separately confirmed **"discard manual override"** action — not a flag on the
force request, and not a side effect of a rebuild. An operator who corrected a
marker by hand should never lose that correction to a background job.

### 4.4 The wire stays additive

`DecisionResponse.markers` is decoded by web, Apple and Android today. Its
existing fields — `kind`, `start_ms`, `end_ms`, `label`, and the compatibility
`chapter` field — **stay exactly as they are**. New `provenance`, `confidence`,
`generation` and `detector_version` fields are **optional** until all three
clients have migrated.

`crates/plurxd/src/http/stream.rs:448` is the struct; treat its comment
("The wire shape is fixed — three clients decode it") as binding.

There is a wire-conformance test at
`tests/validation/test_control_wire_conformance.py` that pins the *control*
protocol's field names across all four ports. It does not cover markers today.
Extending the same pattern to the marker wire when the optional fields land
would be welcome and is cheap.

### 4.5 What the control actor does with annotations

The decision/start payload returns the persisted annotations. The control actor
also consumes them: when the playhead approaches a marker, or automatic skip is
enabled, it raises the marker end as a likely demand boundary, so VOD can
materialize that segment and its selected subtitle window and rolling delivery
can reposition without waiting for the click.

**A skipped destination is a hint. It is not lease renewal, and it is not
permission to seek the client.** Producing the annotations is this item's job;
the actor-side consumption is a later, separate change on the control side —
see §6.

## 5. Contract — the analysis queue

Replaces the cursor-only background walk at `crates/plurxd/src/state.rs:3669`.

### 5.1 Endpoints (admin-only)

```text
POST   /api/v1/files/{file}/analysis
       { "force": false, "components": ["fragment_index", "skip_markers"] }
GET    /api/v1/analysis/jobs?state=queued,running,failed
GET    /api/v1/analysis/jobs/{job}
POST   /api/v1/analysis/jobs/{job}/retry
DELETE /api/v1/analysis/jobs/{job}
```

### 5.2 Four replicated records

```text
analysis_jobs
  job_id · file_id · source_identity · component · target_node_id?
  pipeline_or_detector_version · requested_generation · priority · trigger
  state · attempt · not_before_ms · cancel_requested · created/updated_ms

analysis_attempts
  job_id · attempt · claim_node_id · claim_epoch · claim_expires_at_ms
  phase · started_at_ms · phase_updated_at_ms · terminal_code?

analysis_artifacts
  file_id · source_identity · component · generation · version
  payload_or_digest · state(staged|published|rejected) · published_at_ms

analysis_node_coverage
  file_id · source_identity · pipeline_version · node_id
  fragment_generation · state · verified_at_ms
```

### 5.3 Work identity — three different rules, on purpose

- A **structural or feature** job is unique on
  `(file_id, source_identity, component, pipeline_version, target_node_id)`,
  so every eligible media node builds its own node-local artifact.
- **Semantic correlation** is unique on
  `(file_id, source_identity, detector_version, requested_generation)` and is
  cluster-owned **once**.
- A **non-force duplicate joins the active identity** and returns its `job_id`.
  Force allocates a new requested generation but still permits only one active
  forced successor per component/file.

### 5.4 Durable state machine

```text
 queued ──▶ claimed ──▶ running ──▶ staged ──▶ published
    │          │           │           │
    │          └────────▶ retry_wait ◀─┘
    │                      │
    └────────▶ canceled ◀──┴──────────▶ failed

 any nonterminal state ── source identity changed ──▶ stale
```

- **Claim** is a CAS from `queued|retry_wait` with `not_before <= now`,
  incrementing `attempt` and `claim_epoch`.
- **Renew, phase summary, stage, publish, fail and cancel all require the exact
  `(job_id, attempt, claim_node_id, claim_epoch)`.** An expired claim returns to
  `retry_wait`, and its old worker can no longer stage or publish. This is the
  whole safety property; test it directly.
- **Cancellation** sets `cancel_requested` durably; the owner cooperatively
  stops at a safe boundary and fences its partial generation. A queued job
  cancels immediately. **A published artifact is never deleted by cancelling a
  job.**

### 5.5 Retry policy is typed

| Failure | Policy |
|---|---|
| Transient I/O, preemption, lease loss | Capped exponential backoff with jitter, operator-visible next attempt |
| Source unsupported, deterministic validation failure | **Terminal** until source or pipeline/detector version changes, or an admin explicitly retries |

Numeric attempt and backoff ceilings must be **chosen and tested before
enablement**. They are settings with safe maximums, not wire constants. Do not
invent them in code and do not copy a number out of this document — there
isn't one here on purpose.

### 5.6 Publication is compare-and-swap

Publication CASes on current source identity and expected predecessor
generation. Bytes are fully written, hashed and verified in `staged` before the
replicated pointer moves to `published`. A stale or failed successor leaves the
previous published generation current.

**`force: true` always creates a new generation and never deletes the currently
serving index first.** The old generation stays readable until the new one
validates and publishes atomically, so a failed rebuild cannot turn a playable
title into an outage.

### 5.7 What replicates and what does not

Only **claim renewal, throttled phase summaries, and terminal/publication
changes** replicate. High-frequency byte and media-time progress is node-local,
in the worker's bounded activity registry, and reaches an ingress through the
existing peer Activity snapshot.

This keeps per-fragment progress out of Raft. Putting it in would make a
long indexing pass a sustained write load on the consensus log.

Semantic annotations replicate after validation. Fragment bytes and
pipe-specific indexes stay node-local and publish an explicit coverage row for
the node and pipeline version that produced them.

### 5.8 Source attestation — what is hashed, and what it is for

Before a node builds or serves a fragment index it attests the source: it
opens the file, takes `object_version` — device, inode, size, mtime and ctime
to the nanosecond — checks the scanner's own size and mtime against it, reads,
and then takes `object_version` again and requires it to be unchanged. That
identity check, twice, is what actually guarantees the bytes the indexer read
are the bytes the catalog names. The digest sits on top of it and answers a
narrower question: has this file been rewritten in a way that preserved
device, inode, size, mtime *and* ctime to the nanosecond? Userspace cannot
produce that, so the digest is a belt beside a brace.

Which is why it does not read the whole file. **A source above 64 MiB is
hashed over 64 one-megabyte extents** — the head, the tail, and 62 interior
extents at deterministic, 4 KiB-aligned offsets; **a source at or below 64 MiB
is read whole**, because sampling a file smaller than the sample buys nothing.
The digest's preamble carries the domain token, the file's size, the extent
width and count, and each extent's offset and length ahead of its bytes, so a
sampled digest can never collide with a whole-file SHA-256 of the same file,
two layouts cannot collide with each other, and a file that grew changes
digest even when every extent it kept is identical. The result is still 64
lowercase hex characters, so cache keys, the blob header and every
`source_sha256` column are untouched.

What this buys: attestation costs about 64 MiB of reads no matter how large
the source is — roughly two seconds, against forty-three minutes for a 43 GB
title. That matters twice over. Attestation shares the node with playback, and
`wait_for_cluster_fragment_index_stop` cancels it the moment a foreground
session is admitted; a forty-three-minute hash needs a forty-three-minute idle
window, and a two-second one does not. And every node attests for itself —
`vodserve` refuses to serve a fixed-timeline index until *this* node holds an
observation for the current `object_version` — so the whole-file cost was
being paid once per node per file.

What it gives up, deliberately: a change confined strictly to the gaps between
sampled extents does not move the digest. A test pins that so nobody "fixes"
it by accident. Any such write moves mtime and ctime, which the identity check
already refuses.

`ATTEST_TIMEOUT` stays at **ten minutes on both paths, and now means a hung
mount** rather than a bound on file size: the read it bounds is at most 64
MiB, so ten minutes is reached only when the filesystem has stopped answering.
For that reason the analysis path's timeout is **uncharged** — the retry is
refunded by `retry_analysis_request` and still takes its backoff, so an
unreachable mount backs off instead of spending five attempts on its way to a
terminal `attempt_limit` the file can never leave. The cluster-job path
reports `source_attestation_timeout` rather than folding a deadline into
`source_attestation_failed`, because those two send an operator to different
places.

**Existing observations are grandfathered.** The memo predicate compares
`object_version`, size and mtime — not how the digest was computed — so a node
that already holds a whole-file observation for a file keeps it, keeps its
cache key, and keeps its artifact; nothing already indexed is rebuilt. A node
that has *not* attested that file computes a sampled digest and therefore a
different cache key, and the two nodes each serve their own artifact. That is
the independence the design already has (every node attests for itself;
artifacts are shared by key), and it costs at most one extra build per file
where nodes disagree, converging as memos age out on the next
`object_version` change.

Non-forced analysis requests carry the regime in their generation fingerprint
(`ANALYSIS_ATTESTATION_GENERATION`). `enqueue_analysis_request` refuses a
generation that already exists in **any** state, terminal included, so rows
stranded terminal by a queue fault block every later request for those files.
Moving the token moves every non-forced generation exactly once, which lets
background discovery re-request the library over successive passes without a
single row being deleted or edited — the tombstones stay as history beside
their successors.

## 6. Non-goals and guardrails

Each of these has cost someone something, or is load-bearing for work in flight.

1. **Do not edit `crates/plurxd/src/playback_control.rs`.** It is ~16k lines
   and the playback-control rewrite is live in it. Two agents in that file will
   conflict on ideas, not just on text. The actor-side consumption of
   annotations (§4.5) belongs to that programme, not this one.
2. **Do not redefine `POST /items/{id}/reanalyze`.** It repairs ffprobe facts
   synchronously and operators rely on that meaning. New behaviour goes on the
   new endpoints in §5.1.
3. **Do not add a Serde field to `FragmentIndex`** to carry annotations. §3
   explains why; it would not replicate and it would invalidate every
   structural index on a detector change.
4. **Do not change the existing `Marker` fields.** Additive optional fields
   only, until all three clients have migrated (§4.4).
5. **Do not enable detection by default.** Rollout is opt-in until a labelled
   fixture corpus and a real-series sample meet recorded precision/recall
   gates, with false positives weighted more heavily.
6. **Do not describe feature extraction as free output from the existing
   pipe.** The current fragment indexer maps video only. A feature job may
   share queue, admission scheduling and source I/O budgets, but it is new
   decode work and must be budgeted as such.
7. **Do not put file paths, media titles, raw fingerprints or user identities
   in metric labels.** The Activity API may return authorized display names;
   logs use file IDs and bounded reasons.
8. **Do not write a stuck-job reaper that mutates jobs.** A stuck-job alert
   keys off expired claim lease and no progress. An independent task that
   "fixes" jobs is a second owner, and two owners is the bug.
9. **Do not touch `docs/STATUS.html` mobile build numbers or
   `tests/playback/rolling-producer-owners.toml`** unless your change actually
   requires it — both conflict constantly with parallel work (§8).

## 7. Milestones

Each ends with an acceptance check that is a runnable command or an observable
fact.

### M1 — persist what is already derived

Store the annotation set, sourced from the chapter classification that
`markers_from_chapters` already performs. No new media analysis. Provenance is
`authored` for title-classified chapters and `estimated` for the
duration-derived fallback.

- New `plurx-core` records (§4.1), SQLite migration, Hiqlite table, store
  methods, and store-contract coverage in
  `crates/plurx-core/tests/store_contract.rs` for both backends.
- `markers_for` reads persisted annotations when current for the source
  identity, and falls back to today's derivation when absent.

**Acceptance:** `make cluster-store-check` passes with the new contracts, and a
file whose chapters are deleted from disk still returns its stored markers.

### M2 — manual override

Admin endpoint to set, correct, and discard a boundary. `manual` provenance,
highest confidence, `manual_override_revision` set.

**Acceptance:** a manual marker survives a forced rebuild; discarding it
requires the separate confirmed action, proved by a test that a rebuild alone
does not clear it.

### M3 — the queue

The four records, the state machine, claim/renew/stage/publish/cancel with
exact `(job_id, attempt, claim_node_id, claim_epoch)` fencing, typed retry, and
the endpoints in §5.1. Replace the cursor walk at
`crates/plurxd/src/state.rs:3669`.

**Acceptance:** a three-voter test where a worker's claim expires mid-run and
the old worker's `publish` is refused while a new claim succeeds. This is the
single most important test in the item.

### M4 — operator surfaces and telemetry

Item detail gets **Analyze now** / **Rebuild analysis**; the Activity page gets
an Analysis section with the states, stages, progress and actions listed in
plan §6.6. All required telemetry from that section, with the labelling rule in
§6.7 above.

**Acceptance:** `make operations-check` passes; the Activity section renders
every state in the §5.4 diagram.

### M5+ — detection (separate, gated)

Feature sidecar, series/season correlator, boundary refinement, evaluation
corpus, precision/recall gates. **Do not start M5 until M1–M4 are merged**, and
treat the evaluation gate as a deliverable rather than a formality.

**Acceptance:** recorded precision/recall on a labelled corpus and a real
series, with the false-positive rate reported separately, before any default
changes.

## 8. How to work in this repo

The effort train is documented in
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md). What bites in practice:

- **`make check`** is the local gate: catalog lint, history, operations,
  benchmark, and the Rust baseline. **Do not repeatedly run the broad unit
  suite** — the pipeline is built around not doing that.
- **`validation/points.toml`** governs every file. A new file that maps to no
  functionality point fails `validation-lint`. Relevant points here are
  `playback.pipeline`, `library.catalog`, `server.api`, `cluster.operations`.
- **`make history-check`** classifies commits: a subject that reads as a bug
  fix (`fix:`, or words like *refuses* / *failure*) demands regression evidence
  in `validation/regressions.d/`, and a client fix additionally needs an anchor
  row in `tests/client-fixes.toml`. Both are real requirements, not lint —
  budget for them.
- **`cargo fmt --all --check`** runs in the fast Rust gate. If you cannot run
  rustfmt locally, expect one red cycle and apply the gate's own diff verbatim.
- **Any change under `clients/android/app/src/main/` or
  `clients/apple/Sources/` forces a build-number bump** (`versionCode`,
  `CURRENT_PROJECT_VERSION`), and the gate requires the bump to clear *current
  main*, not the commit you branched from. Two client PRs in flight means the
  second always re-bumps. This item should mostly avoid client code until §4.4's
  optional fields land.
- **CI only runs on PRs based on `main`.** A stacked PR gets skipped checks that
  look green. Do not read a stacked PR's checks as a pass.
- **Merge with merge commits, never squashes.** The regression ledger addresses
  commits by id; a squash has broken main before.

## 9. Open questions for Paul

These are decisions this document deliberately does not make:

1. **Confidence floor for auto-skip.** Plan §6.5 says thresholds come from the
   evaluation, not from the document. M1–M4 can ship with auto-skip restricted
   to `authored` and `manual` only, which needs no threshold at all.
2. **Retry and backoff ceilings** (§5.5) — settings with safe maximums, chosen
   and tested before enablement.
3. ~~**Whether `preview` is in scope for M1.** `AnnotationKind` includes it;
   nothing in the current chapter classifier produces one.~~ **Answered
   2026-09-01:** the chapter classifier produces `preview` (PR #737). A
   labelled preview ends the credits window rather than starting it, and is
   offered but never auto-skipped.
