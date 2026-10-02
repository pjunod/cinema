# TCL Auto playback refusal — capability rejection and catalog read budgets

**Status:** open · diagnosis approved with changes by Opus; revision 3
addresses implementation-plan conditions ·
**Investigated / revised:** 2026-10-02 · **Implementation:** not started.

Companion to [PLAYBACK.md](../PLAYBACK.md) and the
[streaming reliability plan](STREAMING-RELIABILITY-IMPLEMENTATION.md).
This document explains the `candidate_encode_route_unavailable` refusal for
“One Last Shot” on the TCL, distinguishes the incident evidence from later
controlled reproductions, and specifies a repair for review. It does not
execute the repair, change the saved Developer switch, or authorize deployment.
The earlier [TCL probe mismatch](TCL-PROBE-MISMATCH-RCA-AND-FIX.md) was a
different refusal and is not the explanation for this incident.

## 1. Finding — the TCL fails a gate before catalog discovery starts

The TCL asked to play file **6666**, “One Last Shot,” using Android Media3 and
Auto quality. The server selected a transcode because of resolution, HDR
presentation and E-AC-3 audio constraints. Creation returned HTTP 409 with
`candidate_encode_route_unavailable`, before Media3 started. Three attempts
recorded `candidate_count=0`.

**Correction to the first draft:** its timeout attribution was wrong. The
first refusal occurred no more than **983 ms** after the preceding decision
completed. That cannot be the catalog's two-second deadline. Opus's review
also reports a settled-cluster recurrence within 1.019 seconds of decision,
and about 552 ms after the client's preparing event. The two defects below
must be fixed separately; removing repeated reads alone leaves the TCL gate.

**B1 — silent rejection of a capability document with too many video rows is
the strongly supported TCL root cause, pending the exact device count.**
`QualityCatalogRequest::is_valid` rejects more than 16 video entries before
fan-out, returning an empty catalog without a cause. The upstream decision
and create validator has no corresponding count check. Android's display-aware
capabilities enumerate decoder components and profile/level rows without
compaction. That can exceed 16, producing this fast, misleading route refusal.
The other request-validation clauses fit the logged request and stored source.
The original TCL body was not retained, so its exact count is **not measured**.
Section 5 specifies a diagnostic confirmation and a falsification condition.
This predicts failure for Android documents **exceeding 16 entries**, not for
every Android device.

**B2 — requests that pass B1 face a separate, deterministic read-budget
mismatch under the measured cluster conditions.** This source's catalog
performs at least 80 serial authority reads inside the enumeration loop.
Opus's steady-state histogram samples contain no successful authority reads
below 25 ms and means of 53–56 ms. At those costs the loop alone consumes at
least 2.0 seconds, or about 4.3–4.5 seconds at the mean, before its other work.
It cannot finish within its two-second budget. This is read amplification,
not a claim of occasional latency jitter. The histogram is an observed
window, not a permanent lower-bound guarantee for every future read.

Both paths erase the reason and reach the same empty-catalog classifier.
Nine worker API timeouts reproduce B2 with synthetic, small caps. A local
manager replay produces seven compatible routes but skips B1 and the cluster.
Neither experiment demonstrates what the original TCL capability document
contained. Safari's failures may follow B2, but their timings were not retained
and their incident attribution remains unconfirmed.

**Evidence provenance:** original incident logs, worker trials and the replay
were collected in this investigation. The later recurrence, decoder-registry
counts, rollout times and 05:29Z histograms below were supplied in Opus's
2026-10-02 review at `dea1a403`. The source paths and revised causal logic were
checked against the repository; those reviewer live measurements were not
independently repeated for this revision. Section 8 records every disposition.

## 2. Evidence — what was observed and what was controlled

### 2.1 Incident revision and timeline

The incident lab6 container was stamped:

```text
189286828ca0be981a8820d3191856f4e216364a
fix(streaming): retain the parent HLS target for candidate warnings (#701)
container started: 2026-10-02T04:14:22.772487506Z
```

Times below are UTC on October 2; the TCL attempts were **00:50 Eastern**.

| Time | Observation | Interpretation |
|---|---|---|
| 04:50:20.446 | `/files/6666/decision`: `method=Transcode`, delivered range SDR; resolution, HDR presentation and E-AC-3 reasons | A conversion is expected; direct-copy playback is not the planned route |
| 04:50:21.429 | Session creation: `encode_route_unavailable`, `encode_or_auto`, height 0, no explicit identity, candidate count 0 | Auto failed before selecting an encode recipe |
| 04:50:21.610 | Android: `stage=session_create`, code 409; “session create failed before Media3 started” | No decoder-start failure was reached |
| 04:50:25.996 | Retry: same route refusal and zero candidates | Retrying did not change the catalog result |
| 04:50:29.083 | Further retry: same refusal and zero candidates | Persistent over the observed attempts |
| 04:51:38–39 | Client closed the failed attempt | No successful TCL playback was observed |

The decision is logged at handler completion; creation follows its response.
Thus the first create handler took **at most 0.983 seconds**, not two seconds.
The review supplied a second occurrence on `dea1a403`, after all three nodes
had been up for 7–19 minutes with no deployment running:

| UTC time | Reviewer observation |
|---|---|
| 05:28:30.389 | Android decision for file 6666 |
| 05:28:30.856 | Client `surface_raised preparing`, about to create |
| 05:28:31.408 | Create refusal, `encode_route_unavailable`, zero candidates |

The 552 ms interval from preparing is an approximate handler bound, not a
server-side request duration. Both timelines exclude the two-second timeout.

The same code and zero-candidate condition appeared for Safari file 120 at
04:14:59 and file 6328 at 04:27:43, after copy-stream rejection triggered an
encode fallback. Their preceding browser decode failures are separate
triggers; this investigation does not explain those failures.

Read-only inspection of the replicated catalog confirmed
`playback.display_aware_auto=1`. No switch was changed during this investigation.

### 2.2 Source and worker facts contradict “no encoder exists”

The sanitized [source fixture](../evidence/tcl-candidate-catalog-source-2026-10-02.json)
retains technical probe fields and catalog luminance values. Media paths,
chapter content, and descriptive tags were removed; no token is retained.

| Property | Stored value |
|---|---|
| Source | Matroska, HEVC Main 10, 3840 × 1918 |
| Sample aspect ratio | 1:1 |
| Display aspect ratio | 1920:959 |
| Frame rate | `24000/1001` in both average and nominal fields |
| Dolby Vision | Profile 8, base compatibility 1, RPU present, no enhancement layer |
| HDR label | HDR10-compatible |
| Source size / mtime | 18,986,891,679 bytes / 1790349395 |
| Audio offset | 0 ms |
| Catalog luminance | MaxCLL 2259, MaxFALL 183, mastering maximum 1000 nits |

The real parser accepts the normalization transform, rotation 0, and SAR
1:1. A core-plan replay admitted normalized SDR plans at all eight requested
heights with software, VA-API and QSV encoder selections. That replay proves
planning compatibility, not those hardware routes on a particular host.

lab6's incident startup logs independently show validated VA-API and software
encoding. QSV and NVENC validation failed on that node; neither is required
for its admitted VA-API/CPU route. GPU tone-map probes failed, so its measured
HDR-to-SDR graph is the CPU chain. Dolby renderer and HDR passthrough probes
also succeeded. These observations rule out a blanket absence of encoders;
they do not promise realtime performance at every height.

### 2.3 Worker API reproduction preserves production authorization

Read-only POSTs used the existing
`/internal/v1/media/quality-candidates` contract from one committed peer to
another. Each request used a fresh nonce and the normal exact-request
signature. Signing keys stayed on their owning hosts and were never printed
or copied into the source replay. No session, encoder process, repair, or
catalog write was requested.

The diagnostic caps were intentionally simple: v2, H.264 SDR, AAC, HLS, no
explicit geometry ceiling. These are **not a recovered TCL caps document**.
The file ID, size, mtime, selected audio index 1, zero offset, and absence of
subtitle burn matched the investigated title/request facts.

| Target / request | HTTP result | Elapsed seconds |
|---|---|---|
| lab6, file 6666, VOD | 504 | 2.056 |
| lab6, file 6666, live | 504 | 2.057 |
| lab6, file 6666, VOD | 504 | 2.047 |
| lab6, file 120, VOD | 504 | 2.078 |
| lab6, file 6666, VOD | 504 | 2.061 |
| lab6, file 6666, VOD; counter window | 504 | 2.046 |
| lab4, file 6666, VOD | 504 | 2.066 |
| lab4, file 6666, VOD | 504 | 2.053 |
| lab4, file 6666, VOD; counter window | 504 | 2.051 |

The lab6 window added 40 `cluster/authority_read/voter` reads and one media
stat attestation totaling 0.982 ms. The lab4 window added 44 reads and two
stat attestations totaling 1.612 ms. **These counters cannot attribute reads
to the catalog.** No equal-duration idle baseline was retained. Opus's later
background rates were about 26 reads/s on lab6 and 15–16 reads/s on lab4 and
media1, sufficient to account for similarly sized windows. The earlier draft's
use of these counters as supporting attribution is withdrawn. The stat values
only describe that measured operation, not all engine or filesystem costs.

**Rolling-deploy confound:** these trials overlapped the rollout from
`189286828…` to `dea1a403e9a650fc51e35909b36e596be5a5ed13`, which included
schema v66→v67. Reviewer uptime/build observations place restarts at roughly
05:11:20Z (media1), 05:11:30Z (lab6), and 05:22:30Z (lab4). The six lab6
trials were not individually bracketed by build inspections; lab4 was still
on the incident build immediately before its three trials. That establishes
old worker code, not steady-state cluster performance. The catalog, endpoint,
resolver, candidate and planning files are unchanged across these revisions;
unchanged source does not remove the rollout/migration confound. No deployment
was performed by this investigation. Section 3.2 uses the review's later
steady-state measurements to evaluate the read budget.

### 2.4 Isolated manager replay yields seven routes

The retained [Rust replay](../evidence/tcl-candidate-catalog-replay.rs) invokes
`TranscodeManager::quality_candidates_with_copy_contract` in a source-only
archive of the incident commit. It seeds the sanitized probe into an
in-memory SQLite Store, models VA-API availability with a CPU renderer,
provides a cache identity, and requests H.264 SDR with audio index 1.

Observed heights:

```text
144, 240, 360, 480, 720, 1080, 1918
```

All seven rows were decoder-compatible. The initial diagnostic run passed in
1.46 seconds; the retained, assertion-based replay was then rerun and passed
in 1.39 seconds with Rust 1.97.1 (`8bab26f4f`). Both timings include test setup
and local media-engine inspection. There
was no real movie mounted and no complete cache or production-speed proof.
This proves that metadata and manager planning do not inherently produce an
empty catalog. It is not a production cluster performance measurement, a
successful session creation, or a physical TCL acceptance test.

**Explicit replay boundary:** the test calls the manager directly. It skips
`MediaPool::quality_catalog`, `QualityCatalogRequest::is_valid`,
`local_quality_candidates`, peer fan-out and the create handler, and uses
**one video capability entry**. It cannot reproduce B1, control snapshot
rejection, request validation, or production authority-read costs. Its success
is evidence of planning compatibility only.

An incidental finding is that normalized 1440p for this aspect ratio is
2882 pixels wide. The special 1440p rate profile admits at most 2560 pixels,
so that row is rejected with `auto_quality_rate_profile` invalid. Seven other
rows survive. Widening that measured profile is not a fix for this incident.

## 3. Causal chains — two paths become the same empty catalog

Read these sources at incident commit `189286828…`; Opus also checked them at
`dea1a403`. Function names are the anchors because the working checkout and
`main` advance independently.

| Boundary | Source / behavior |
|---|---|
| Android capability enumeration | [`Caps.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/Caps.kt), `videoDecoderLimits`: decoder × type × profile/level rows; display-aware switch controls inclusion |
| Android wire document | [`CapsPolicy.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/CapsPolicy.kt), `videoEntries`: maps rows without deduplication; drops the hardware flag |
| Upstream validation | [`validate_device_caps`](../../crates/plurxd/src/http/stream.rs): audio/presentation checks, no video-entry bound |
| Catalog gate and fan-out | [`QualityCatalogRequest::is_valid`, `MediaPool::quality_catalog`](../../crates/plurxd/src/media_pool.rs): 16-entry gate returns empty before the two-second deadline starts |
| Control snapshot | [`DecoderCapsSnapshot`](../../crates/plurxd/src/playback_control.rs): 16-entry validation; `from_device_caps` converts errors to `None` |
| Live TV snapshot | [`live_tv_delivery.rs`](../../crates/plurxd/src/live_tv_delivery.rs): 32-entry bound |
| Worker route | [`quality_candidates`](../../crates/plurxd/src/http/internal_media.rs): four permits; immediate 429 when busy, not queued; worker timeout is 504 |
| Enumeration | [`quality_candidates_with_copy_contract`](../../crates/plurxd/src/transcode/manager/candidates.rs): sequential heights × HDR ask × normalization mode |
| Planning reads | [`encoder_and_grade_for`, `resolve_restricted_movie_plan`](../../crates/plurxd/src/transcode/manager/plan.rs): repeatedly read preferences and probe |
| Authority reads | [`hiqlite.rs`](../../crates/plurx-core/src/store/hiqlite.rs), [`hiqlite_media.rs`](../../crates/plurx-core/src/store/hiqlite_media.rs): consistent queries for settings/probe |
| Public refusal | [`resolve_plan`, `candidate_refusal_reason`](../../crates/plurxd/src/http/hls/create.rs): empty catalog without explicit identity becomes encode-route conflict |

```text
Display-aware Auto needs an encode
  |
  +-- caps.video has >16 entries
  |     -> request invalid -> immediate empty catalog           [B1]
  |
  +-- request valid -> local and remote catalog deadline (2 s)
        -> repeated authority reads exceed available budget    [B2]
        -> local timeout discards Vec; peer transport times out
  |
  +-- no candidate and cause erased
        -> HTTP 409 candidate_encode_route_unavailable
        -> client stops before decoder startup
```

### 3.1 B1 — inconsistent capability bounds reject ordinary decoder registries

The catalog's other validity checks are consistent with the known request:
positive file ID, nonnegative size/mtime, v2 caps, audio index 1, offset 0,
no subtitle burn, no copy contract, and the audio/presentation validation
already passed by decision. The count clause is unvalidated upstream and fits
the fast refusal. Confirm the actual create body/count before declaring the
physical incident closed; different caps between decision and create would
also need examination.

Android includes decoder entries when `Session.displayAwareAuto` is true.
The review's Google TV Streamer registry had about 65 mapped profile/level
rows, or an estimated 45–50 after excluding secure components. **Registry
rows are a proxy, not a captured wire count**, and the Streamer is a second
MediaTek device, not the TCL. The TCL model/SDK query answered once (9445X,
API 36), then adb refused; its wire entry count is still unknown.

The same mismatch affects control negotiation. `DecoderCapsSnapshot::try_from`
rejects >16 rows. `from_device_caps` swallows that error with `.ok()`, so create
gets `candidate_decoder_caps=None`. The `quality_enabled &&
candidate_decoder_caps.is_some()` condition then skips quality-owner discovery.
This is silent loss of negotiation evidence, not necessarily another HTTP
refusal. A directly deserialized control snapshot does report validation failure.
Raising only the catalog bound leaves this defect in place.

The catalog constant also bounds node encoder/renderer capabilities. Client
decoder evidence needs its own shared vocabulary limit; changing a node limit
globally is not a sufficient contract repair.

### 3.2 B2 — serial reads exhaust the budget before useful work can finish

Opus measured `plurx_store_operation_seconds` with
`class="authority_read", outcome="ok"` over a 40-second window at 05:29Z,
with all three nodes settled on `dea1a403`:

| Node | Successful reads | Mean duration | Observations below 25 ms |
|---|---|---|---|
| lab6 | 1,035 | 53.4 ms | 0 |
| lab4 | 613 | 54.4 ms | 0 |
| media1 | 632 | 56.0 ms | 0 |

The review also reports no sub-25-ms reads among process-lifetime totals of
20,136 on lab6 and 7,397 on lab4. These are aggregate histogram observations,
not per-request traces or measured p95 values. They provide a cost model for
the code's serial operations without attributing background counter deltas
to this request.

For file 6666, geometry is known and there are eight heights × two HDR asks ×
two normalization modes. The loop's conservative read accounting is:

| Evaluation | Serial authority reads |
|---|---|
| 16 SDR variants | Two encoder-preference reads plus probe read each: 48 |
| 16 HDR-requested variants | At least two preference reads each: 32 |
| Total loop lower bound | **80**, before additional eligible-HDR checks |

The 1080p HDR branches add preference work; file lookup, track preferences,
probe and DV-conversion settings also precede enumeration. Create has its own
setting and audio-selection reads before catalog entry. Even rejected variants
consume reads before their rejection. Candidate deduplication at the end cannot
recover that time.

At 25 ms per read, 80 reads take 2.0 seconds; at the observed means they take
4.27–4.48 seconds. Source/cache/engine work and transport need additional time.
An equivalent six-height ladder needs at least 60 loop reads (~3.2–3.4 seconds
at these means); five heights need at least 50 (~2.7–2.8 seconds). These counts
assume the same applicable planning branches and known geometry, not every
possible source or early exit. Under those measured costs, a complete catalog
cannot fit the two-second deadline. The retained nine 504s are consistent with
this budget mismatch, although their rollout timing prevents using them as an
unconfounded latency benchmark.

### 3.3 Deadlines, maintenance and erased evidence

Local work returns one `Vec` only after enumeration. Timeout discards all its
rows through `unwrap_or_default()`. Peer errors and non-success responses are
mostly dropped. The special `authority_refused` handling is not a complete
history of authority failures. `local_maintenance_active()` also yields an
empty local catalog; busy workers return 429 immediately.

Caller and worker independently allow two seconds, but the caller starts
first. A slow peer's 504 normally loses the race to the caller's transport
deadline. The direct diagnostic requests allowed eight seconds and therefore
received the worker's 504. Returning partial rows at the worker's existing
two-second cutoff would still be too late for real fan-out.

A separate latent deadline problem affects DV Profile 5. `require_dovi_renderer`
can run two FFmpeg probes with 30-second bounds and inserts its source proof
only after completion. The decision path uses render capabilities rather than
this per-source proof; `media_offer_probe` only reads an existing memo. Earlier
background planning/production can populate it through `encoder_for_file`,
but there is no guaranteed cold-source warm-up before catalog entry. Cancellation
can prevent memoization and make the next request repeat the work. Test cold
and warm memo behavior and bounded shared proof ownership. The memo is
process-local: every restart/deploy makes previously warmed Profile 5 sources
cold again. Include the first play after deploy in acceptance. File 6666 is
Profile 8 with a compatible base; this is not its cause.

### 3.4 R1 — one create can enumerate the same ladder three times

Opus's revision-2 re-review identified three sequential catalog calls in
`create_with_purpose` at `dea1a403`; the call sites were independently checked
in source for this revision:

| Call | Purpose | Capability input |
|---|---|---|
| 1: `resolve_plan` | Select a candidate | Request body caps |
| 2: quality-owner discovery | Keep worker node IDs from a fresh catalog | `planning_caps` |
| 3: `StartResponse.quality_candidates` | Populate the response ladder after dispatch | `candidate_decoder_caps.device_caps()` |

Each starts a new two-second deadline and repeats the loop on each worker.
A create that gets past selection can therefore spend up to roughly six
seconds in catalog waits; call 3 delays the response even though dispatch has
already happened. A failed call 1 ends the request, so three calls do not
explain the original fast TCL refusal.

B1's `candidate_decoder_caps=None` also hides calls 2 and 3. Repairing the
bound enables those calls for affected devices. If call 2 yields no rows,
the owner set shrinks to the locally inserted node. With a remote predecessor
and no candidate context, that can make `quality_negotiated=false`.

Call 3 reconstructs video evidence but supplies audio `["aac"]`, container
`mp4` and transport `hls`. It need not describe the body caps used for selection;
copy-contract inputs also differ between call 1 and the later calls. This
creates a risk of response recipes/identities differing from the selected
catalog. Consolidation must prove identity consistency, not merely save time.
The two-second peer/caller race in §3.3 applies to all three fan-outs.

## 4. Proposed repair — align caps, snapshot inputs and preserve outcomes

These interfaces and policies are proposals awaiting re-review, not shipped
APIs. Re-verify sources at implementation time. Tasks share files: use one
effort branch if split into multiple tasks. The disjoint-ownership exception
is in [AGENTS.md](../../AGENTS.md), “The one bounded exception.”

### 4.0 Give client capability documents one contract

Introduce a shared client decoder-entry bound used by decision/create,
catalog, control snapshots and Live TV. Keep node encoder-list limits separate.
Choose the value from representative compacted Android registries and the
designed codec/profile vocabulary: **32 or a justified larger value**, retaining
the existing 64 KiB internal request byte bound and bounded parsing. Merely
raising 16 to 32 does not prove a 45–50-row registry fits.

The re-review supplied this regular-component Streamer registry summary:

| Codec | Distinct registry profiles | Count |
|---|---|---|
| AVC | Baseline, ConstrainedBaseline, Main, High, ConstrainedHigh, High10 | 6 |
| HEVC | Main, Main10, Main10HDR10, MainStill | 4 |
| AV1 | Main8, Main10, Main10HDR10, Main10HDRPlus | 4 |
| VP9 | 0, 1, 2, 2HDR, 2HDRPlus | 5 |
| MPEG-2 | Main | 1 |
| Total | Distinct codec/profile pairs | 20 |

This supports choosing above 16; 32 is a plausible compacted bound for this
fixture, not yet a measured wire result. Crossed envelopes may require more
rows, while `videoProfileName` can merge unmapped profiles. Preserve those
mapping details in the fixture rather than assuming exactly 20 output rows.

**R2 rollout contract — compact first; enforce only for clients promising the
new contract.** Ship Android compaction before enabling the new hard count
check in `validate_device_caps`. Introduce an explicitly negotiated capability
contract revision that promises compaction; ordinary existing `caps.v=2` does
not make that promise. The protocol field/version allocation is a build-time
contract decision, not an existing field. Check old-server/new-client parsing
before the client advertises it; unsupported peers use the existing wire shape.

For clients advertising that contract, decision/create enforce the shared bound
before planning with HTTP 400 `invalid_capabilities`, `clause=video_entries`,
`observed` and `limit`. For installed pre-compaction clients, preserve existing
entry-count admission for direct, copy and manual-height playback: **do not
apply a new global 400** merely because their rows exceed the compacted bound.
Existing byte, shape and security bounds remain. Log count/contract diagnostics.
If optional owner/response evidence cannot fit a bounded decoder snapshot,
report that detail as unavailable without failing otherwise valid playback or
inventing decoder capabilities. Required Auto/explicit-candidate discovery still
requires valid evidence and must preserve its typed validation cause. This does
not override the saved Developer setting or promise Auto recovery to every old
build. Test response protocol claims when optional evidence is unavailable.

Catalog validation must preserve `request_invalid` and its clause instead of
empty success. Control conversion must return a typed error rather than silently
producing `None`; the caller handles legacy optional evidence explicitly under
the policy above. Keep final-contract readers/writers aligned and document the
legacy compatibility exception. Malformed bodies never become capacity retries.
Do not remove legacy acceptance on a timer: removing it is a separate supported-
client policy change backed by installed-build evidence.

**Landing dependency:** R1's one-catalog consolidation (§4.1) lands with or
before the server B1 repair. Raising the bound first activates calls 2 and 3
and magnifies the startup/read cost. Client compaction can also activate those
calls on old servers when it brings a document below 16, so deploy consolidation
before distributing that client change; mixed-build tests cover this case.

Compact Android rows at the wire-policy boundary:

- Deduplicate identical serialized rows after mapping, so hardware/software
  or low-latency components with identical evidence collapse. The hardware
  flag is not on the wire; retain the existing software-height policy before
  compaction.
- Remove a row only when another row of the same codec/profile and compatible
  presentation/DV evidence provably covers its complete capability envelope.
  Compare width, height, cadence and every other admitted limit together.
- Do not combine maxima from different rows into a decoder no component
  proves. Unknown values are not proof of dominance over known values.
  Normalize rational frame rates for comparison; preserve profile/grade
  distinctions and rows whose envelopes cross.
- Do not arbitrarily truncate the list to the limit. If safe compaction still
  exceeds it, return an explicit capability-contract diagnostic and use the
  reviewed bound; never silently drop codec support.

**Acceptance:** a realistic MediaTek registry fixture passes through Android's
actual `videoEntries` path, preserves admitted/rejected envelopes and fits the
chosen bound. The same document passes create/catalog/control/Live TV. Test
limit−1, limit and limit+1 under the new contract at every server boundary,
including direct control snapshot deserialization and `from_device_caps`.
Separately, a pre-compaction Android document above the new bound must still
succeed on the direct/copy/manual paths that succeeded before the server repair.

### 4.1 Discover once, snapshot once and budget the whole create path

**R1 acceptance: one catalog per display-aware create requiring discovery.**
Return a request-scoped catalog result from `resolve_plan` alongside the chosen
plan: canonical request inputs/fingerprint, worker IDs, rows, completeness,
causes and snapshot bindings. Quality-owner discovery and
`StartResponse.quality_candidates` consume this same result; neither invokes
enumeration again, including after dispatch. For paths that skip catalog
selection but need optional quality metadata, initialize the same request-scoped
result once before dispatch. Paths needing no discovery perform zero calls.
This is request-local reuse, not a cross-request cache with stale evidence.

Resolve effective caps, selected audio, offset, subtitle, presentation and copy
semantics once before enumeration. Preserve the full body capability semantics;
do not rebuild catalog input through `DecoderCapsSnapshot::device_caps()`.
Downstream filtering can narrow the same rows without changing recipe IDs or
claiming a worker/route absent from the retained evidence. If later reconciliation
would invalidate the canonical inputs, surface that inconsistency rather than
silently re-enumerating or relabeling an old result. Dispatch still revalidates
its selected recipe.

Prove the selected ID belongs to the retained catalog, response rows keep their
original IDs/recipe bindings, and a later explicit-ID request with unchanged
canonical inputs/source/settings can resolve each advertised selectable ID.
Also test audio other than AAC, copy versus encode, subtitles and remote
predecessors. Changed revisions retain explicit stale/unavailable semantics;
reuse is not permission to serve an old recipe. Assert owner negotiation uses
the retained worker set and does not silently collapse after a second timeout.

One bounded Store operation should return file/probe facts, relevant settings
and a source/settings revision. Include display-aware policy and track/audio
selection currently read separately by create. Capture rate-control and worker
capabilities explicitly; keep source/engine bindings required for truthful copy
and cache claims. Independent concurrent reads are not a coherent snapshot.

**Concrete revision mechanism (proposed):** settings currently have values and
`updated_at`, but no monotonic generation. Add a replicated settings-generation
counter, incremented in the same transaction as every relevant setting insert,
update or delete, including migrations/import paths. Scope and audit the key
set used by planning. Read that generation plus file identity/probe and bounded
settings values in one consistent `SELECT`, using scalar subqueries over the
file/probe/settings tables. This is one hiqlite authority read per worker
snapshot, rather than a Store method that hides many serial queries. Implement
equivalent Store semantics for SQLite tests. At dispatch, re-read the generation
and source/probe binding in one bounded check. Count that check in the budget.
Do not use `MAX(updated_at)` alone as a revision: tied timestamps, updates
within one timestamp tick and deleted keys need reliable invalidation.

Pass immutable inputs through shared encoder/grade and plan-resolution logic.
Avoid a second renderer selector. Deduplicate evaluations only where input
and output contracts are identical. Skip copy-only attestation when copy is
incompatible, while retaining evidence required for cache claims. Revalidate
source/settings revision, engine, serving authority and admission at dispatch;
a catalog never authorizes stale execution.

**Acceptance is absolute as well as asymptotic.** Instrument authority-read
counts and timings across create prework, **every catalog call**, aggregation,
final admission and response construction. The current path has up to three
calls; record that baseline. The repaired path must have exactly one when
discovery is required, and counts must remain constant for 2/6/8 heights:

```text
N_catalog_calls = 1  (0 only when the path needs no discovery)
N_reads_critical = prework + sum(all catalog calls' critical-path reads)
                   + admission/revalidation + response reads
N_reads_critical * measured p95 read cost
  + all non-Store work + transport/serialization reserve < T_CREATE_START_MS
```

Account for parallel worker branches by their critical path, and separately
report total cluster reads to expose amplification. The catalog retains its
two-second ceiling; pre-catalog reads consume the enclosing startup budget,
and catalog receives only the remaining permitted time. Do not give each
stage a fresh two seconds.

**Budget owner:** propose one `CreateStartupBudget` owned by
`create_with_purpose`, starting at handler entry and ending when its response
is ready. `T_CREATE_START_MS = TBD` is an explicit blocking design value, not
an implemented deadline or a passing acceptance result. Select it with the
existing client startup/recovery owners, leaving time for response transit,
manifest retrieval and first-frame presentation, and carry any shorter upstream
remaining allowance. Android's
[`OpenPlaybackStallTracker`](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackTelemetry.kt)
defaults to 30,000 ms and its
[shared HTTP client](../../clients/android/app/src/main/java/tv/plurx/app/data/Net.kt)
has a 60-second read timeout; neither alone proves
a deadline covering this complete create path. Trace the owners' clock start
points across Android/Apple/web before assigning the server budget. Do not
introduce another client retry owner.

Record the numeric server budget, transport reserve and client remaining-time
contract before implementing deadline enforcement. Measure create and full
startup p95/p99 on the tested cluster before closing acceptance. A constant
10 reads already costs about 0.55 seconds at the observed mean; “constant”
alone does not pass. Current histogram means do not establish p95.

### 4.2 Preserve typed causes and propagate the caller's remaining budget

A versioned or negotiated result carries rows, completion state and bounded
causes. Completion is a field, not an inference from an empty array. Proposed
causes include:

```text
request_invalid(clause, observed, limit) | deadline | busy | maintenance
transport_unavailable | authority_unavailable | stale_source | source_missing
store_unavailable | plan_unavailable
```

Keep row-level rejection separate from worker failure. Include worker identity,
request correlation, stage, elapsed time, budget, considered and produced
counts. Avoid raw paths, probe JSON, credentials and unbounded process output.

At dispatch, propagate a bounded remaining-duration budget in the authenticated
request, or a deadline with an explicit clock-skew contract. The worker deducts
request transit allowance, response serialization/transfer allowance and a
measured RTT reserve, and finishes before the caller's cutoff. Charge peer
discovery and signing against the caller's remaining time. Clamp received
budgets to local limits; nonpositive remaining time yields an immediate typed
result. Account for local work under the same parent deadline. No retry resets
the total startup allowance.

Relative budgets need conservative transit accounting; clock-independent does
not mean transit-free. Protocol tests must introduce request/response latency
and verify partial rows arrive before the caller abandons the peer. An old
peer's successful bare array counts as completed evidence; failed old peers
remain unknown. Old workers without propagated budgets may still time out;
do not claim the new timing guarantee during mixed-version operation.

**Acceptance:** request invalid, local deadline, peer 504/429, transport failure,
maintenance, stale source, fencing and completed empty success survive
aggregation as different outcomes. Response size and worker count stay bounded.

### 4.3 Preserve valid rows and give initial Auto one recovery owner

First fix B1 and B2. Then make the collector return already fully validated
rows from one snapshot with `complete=false` when later work reaches its
subdeadline. Ensure cancellation does not leak tasks, permits or subprocesses;
any shared source-proof work has a bounded owner and lifetime.

Adopt Opus's recommendation as the **proposed policy for re-review**:

- Prefer complete evidence under the existing compatible-original, decoder,
  grade, cache-integrity and production-proof rules.
- Allow a fully validated **local** row from incomplete discovery for initial
  Auto startup, recording `complete=false`. It proves usability, not global
  optimality. Remote partial-row selection needs explicit policy coverage;
  successful transport alone does not authorize it.
- Later quality upgrades belong to the existing sustainable-quality owner
  introduced by #669. Do not add a separate retry/upgrade loop.
- An explicit candidate identity must not be declared absent while discovery
  is incomplete, or silently replaced by another identity.
- No usable row plus incomplete discovery returns typed HTTP 503
  catalog-unavailable. Completed semantic conflict remains 409; request errors
  remain 400. Preserve authority and maintenance classifications.

Test Android, Apple and web classification through their existing bounded
startup recovery owner. Changing only the HTTP code is insufficient. Snapshot
and dispatch validity checks apply equally to complete and partial rows.

### 4.4 Measure the repaired budget and investigate the read floor separately

Add spans for validation, create prework, snapshot acquisition, engine/source
attestation, plan enumeration, cache checks, peer aggregation, admission and
post-dispatch response construction. Record catalog-call count and purpose
(selection / owner discovery / response) for the whole correlated create so
an accidentally restored second or third fan-out is visible.
Count Store operations per request in tests. For live aggregate histograms,
bracket a request window with an equal-duration baseline; retain revision,
uptime, role, traffic conditions and cold/warm state. Never attribute an
unbaselined process counter delta to a catalog request.

**Independent investigation RF1 — consistent-read latency floor (open).**
The 25 ms histogram floor and 53–56 ms means tax every serial-read path.
Trace client/leader request timing, hiqlite/openraft read-index/quorum work,
scheduling/polling and telemetry boundaries. Read-index/heartbeat/poll timing
are hypotheses, not established causes. Also determine whether readiness
polling explains the observed 15–26 background reads/s. Compare measured LAN
RTT and idle/load histograms in a stable deployment. Deliver a separate cause,
change proposal and regression before modifying consistency behavior. The
catalog fix must work at the observed cost; do not make it depend on RF1 or
weaken quorum/fencing to meet its budget.

## 5. Regression and acceptance matrix

These tests are proposed, not claimed to exist or pass. Add actual
`Regression-Test: <path>::<test name>` entries to each behavior-fix PR and
landing message under [VALIDATION.md](../VALIDATION.md).

| Scenario | Required assertion |
|---|---|
| Diagnostic confirmation on TCL and Streamer, Fit Auto to display on | Log actual create `video_entries`, limit, failing clause and elapsed stage; correlate decision/create with the same attempt |
| Realistic Android registry → wire caps → create-level admission | Original >16-row fixture reproduces fast request rejection; compacted fixture traverses catalog and preserves a valid control snapshot |
| Shared-bound boundary cases under the new capability contract | limit−1 and limit accepted when otherwise valid; limit+1 typed invalid at decision/create/catalog/control/Live TV, never empty success or silent `None` |
| Pre-compaction Android document above the new bound | Direct, copy and manual playback remain successful after server rollout; no blanket 400; optional evidence unavailability is explicit |
| Old/new client and server combinations | Compaction advertisement negotiated safely; no new field breaks an old parser; legacy v2 does not imply compaction |
| Display-aware create requiring discovery | Exactly one catalog call; owner set and response candidates derive from it; no fan-out after dispatch |
| Canonical body caps differ from synthesized AAC/mp4/hls caps | Selected/advertised IDs and recipe bindings preserved; explicit-ID follow-up with unchanged inputs succeeds |
| Remote predecessor and retained worker evidence | Quality negotiation uses the single catalog's owner evidence; no local-only collapse from repeated discovery |
| Duplicate and crossing decoder envelopes; unknown dimensions; rational cadence | Only equivalent or provably dominated rows removed; no manufactured decode support |
| Sanitized file 6666, modeled VA-API/CPU route | Compatible routes remain; incidental 1440p rejection does not erase others |
| Fixed snapshot, 2/6/8 heights and measured read costs | Constant reads plus absolute critical-path budget including create prework/admission; no per-variant Store reads |
| Local completed rows followed by slow work | Fully validated rows survive with `complete=false`, selectable only under reviewed policy |
| Peer partial result with simulated request/response delay | Worker finishes early enough for the caller to receive it; total deadline remains bounded |
| Local deadline; peer 504/429; transport; maintenance; invalid request | Each cause survives; none becomes completed no-route evidence |
| Healthy and slow workers; mixed old/new protocol | Healthy evidence survives; successful old arrays complete, failed old peers unknown |
| Explicit identity with incomplete discovery | No false absence or silent substitution |
| Stale source/settings/engine; authority loss | Dispatch revalidation and fences hold; same-timestamp updates and relevant setting deletion bump generation and invalidate prior snapshots |
| Genuine completed decoder conflict; unknown cache/speed proof | Semantic refusal remains; no unsafe fallback or invented sustainability |
| DV Profile 5: first play after deploy/restart, warm replay, cancellation | Process memo cold after restart; proof warming and ownership bounded; no orphan probes or false memo success |
| Android/Apple/web 400, 409 and catalog 503 | Invalid caps terminal diagnostic; semantic conflict preserved; only existing bounded availability-recovery owner retries |
| Repeated Play/cancellation | No leaked workers, permits, processes or growing read queue |

**First confirmation milestone:** a diagnostic-only server change preserves
`request_invalid`, its clause, `video_entries=<n>`, limit and timing. It adds
no new global count rejection or client-contract requirement. After
normal review/build/deployment gates, run one TCL and one Streamer Auto encode
without changing the client. The predicted TCL result is `video_entries > 16`.
If it is ≤16 or another clause fails, B1's physical attribution is falsified
and must be reopened before describing the caps repair as the incident fix.
The Streamer test confirms that device's wire count; its registry estimate
alone does not.

Physical closure requires both devices with **Fit Auto to display enabled**,
the TCL on “One Last Shot,” cold and warm runs, exact client/server revisions,
entry counts before/after compaction, catalog causes/timing, selected recipe,
session creation, first presented frame and sustained playback. Verify control
snapshot/quality-owner negotiation as well as startup. Exercise Safari encode
fallback separately. Manual 1080p success cannot close Auto acceptance.

Use pinned Rust 1.97.1 before implementation edits, focused regressions,
Clippy, formatting and affected Android compilation/tests. Replicated Store
tests need `hiqlite-store` or `make unit-core`; SQLite replay is not evidence
for that backend. These are future implementation checks, not validation of
this documentation revision.

## 6. Reproduce the retained manager evidence

Run this from a checkout that contains this document and its two evidence
files. It creates a temporary source-only archive, appends the diagnostic test
there, and builds it with the pinned compiler. No production credentials,
source media, server connection, or database copy is needed. Use a dedicated
archive target directory so an old revision does not rebuild the active
checkout's incremental cache. The command below deliberately starts cold.

```bash
repo_root=$(git rev-parse --show-toplevel)
rca_root=$(mktemp -d /tmp/plurx-candidate-replay.XXXXXX)
git archive 189286828ca0be981a8820d3191856f4e216364a |
  tar -x -C "$rca_root"
cat "$repo_root/docs/evidence/tcl-candidate-catalog-replay.rs" >> \
  "$rca_root/crates/plurxd/src/transcode/manager/candidates.rs"
export PLURX_RCA_FIXTURE="$repo_root/docs/evidence/tcl-candidate-catalog-source-2026-10-02.json"
export CARGO_TARGET_DIR="$rca_root/target"
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo test --manifest-path "$rca_root/Cargo.toml" \
  -p plurxd --bin plurxd incident_catalog_replay -- --nocapture
```

**How to read the result:** the diagnostic asserts the seven heights and their
compatibility under its declared caps. Passing establishes that this planning
path can construct routes. It does not assert that the catalog fits a
production deadline, that the TCL supports every listed height, or that
those routes encode fast enough. The replay models the ordinary settings
rather than cloning the live settings database.

## 7. Implementation order, mitigations and remaining decisions

1. Add typed validation/outcomes and stage/read accounting, including catalog
   call count and purpose per create. Ship the diagnostic-only milestone in
   §5 through normal gates to confirm the TCL wire count.
2. R1: consolidate selection, owner discovery and response rows onto one
   request-scoped catalog. Verify identities and owner negotiation.
3. B1 under R2: distribute Android compaction with negotiated contract support,
   then enable the shared hard count check only for clients advertising that
   contract. Preserve legacy direct/copy/manual admission and explicitly handle
   optional evidence as specified in §4.0. Existing v2 is not compaction proof.
   Land the server B1 change with or after step 2, **never before**; deploy
   consolidation before compacted clients can reactivate duplicate catalog
   work on older servers. B2 still prevents long ladders from completing, so
   this milestone alone is not playback closure.
4. Add the coherent snapshot and generation mechanism, absolute read budget
   and propagated worker subdeadline. Verify at the measured read cost.
5. Implement and validate the reviewed partial-selection policy using the
   existing startup/quality owners.
6. Pursue RF1 separately; it is not a prerequisite for removing catalog
   amplification or correcting capability rejection.

Manual 1080p without an explicit candidate identity avoids the display-aware
Auto selection branch. Disabling **Settings → Developer → Fit Auto to display**
also avoids it and clears Android's detailed decoder entries. Neither is a
verified TCL recovery in this investigation, and manual selection can still
encounter the separate control-snapshot limit. These are operator choices.
Do not override the saved switch; follow the
[Developer lifecycle rule](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md#developer-lifecycle--every-card-graduates).

Scope excludes widening codec claims, forcing HDR, weakening cache integrity
or serving fences, library-wide reprobes, and widening the measured 1440p
profile. It also excludes the earlier E-AC-3 probe-equality repair and Safari's
initial copy-decoder failure. None addresses the capability-count mismatch.

**Decisions required before implementation:** choose the shared bound using
compacted registry fixtures and allocate the negotiated compaction-contract
revision; approve the local partial-start policy; specify peer budget/reserve
encoding. Set **`T_CREATE_START_MS = TBD`**, owned by `create_with_purpose`,
and its numeric transit/first-frame reserves after tracing client deadline
owners. The placeholder cannot pass absolute-budget acceptance. Confirm the
settings-generation key scope and transactional migration/write coverage.
The proposed work remains unimplemented and undeployed.

## 8. Opus review dispositions — diagnosis accepted, plan conditions addressed

The user supplied the 2026-10-02 review titled “TCL Auto playback refusal RCA —
review,” verdict **CHANGES REQUESTED** at `dea1a403`. Its live data came from
read-only server/metrics/log inspection and a Streamer registry queried via
adb. Revision 2 accepted its central correction. The table records document
dispositions, not completed fixes. The later verdict and conditions are recorded
below; no reviewer approval of this newly edited revision is implied.

| Finding | Disposition |
|---|---|
| B1: wrong TCL mechanism, 16-entry gate | Accepted; §§1, 2.1, 3.1 and 4.0 distinguish gate rejection from timeout. Exact TCL count remains the §5 confirmation gate. Device-wide universal claims narrowed to documents exceeding the bound. |
| B2: deterministic read-budget mismatch | Accepted under measured conditions; §3.2 records histogram/count arithmetic, §4.1 budgets the full critical path, §4.4 opens independent RF1. No unmeasured p95 claimed. |
| M1: counters lack a baseline | Accepted; §2.3 withdraws attribution and §4.4 requires baselines or per-request accounting. |
| M2: rollout confounds nine trials | Accepted; §2.3 records migration/restart window and separates later settled-cluster evidence. |
| M3: equal peer/caller deadline loses partials | Accepted; §§3.3 and 4.2 require propagated budget, transit/return margin and simulated-latency regression. Re-review R1 (§3.4) identifies the same race at the owner and response calls; §4.1 removes those duplicate fan-outs. |
| M4: replay skips the failing gate | Accepted; §2.4 explicitly names skipped boundaries and one-row caps; §5 adds realistic create-level regression. |
| N1: exception citation | Corrected to AGENTS.md in §4. |
| N2: old source shares active target | §6 now uses an isolated archive target; historic replay result is unchanged and was not rerun for this prose revision. |
| N3: public-mirror naming | Host aliases normalized to lab6/lab4/media1; no LAN addresses included. User-supplied title retained; public title anonymization remains an editorial choice before publication. |
| N4: cold DV5 probes outlive budget | Source traced in §3.3: decision does not guarantee memo warm-up; background work may populate it. Cold/warm cancellation coverage added to §5. |
| N5: maintenance missing | Added to typed vocabulary, causal analysis and test matrix. |
| Review answers: snapshot, partial policy, protocol, measurements, devices | Incorporated as proposals in §§4–5; control conversion's swallowed error also documented. |

### 8.1 Revision-2 re-review — APPROVE WITH CHANGES

The user supplied “TCL Auto playback refusal RCA, revision 2 — re-review,”
written 2026-10-02 at `dea1a403`, reviewing the 654-line document. Opus accepted
the diagnosis and required R1 and R2 before implementation. This revision
addresses those conditions in the plan; the new code claims were checked at
that revision. The registry profile counts remain reviewer-supplied evidence.

| Finding | Revision-3 disposition |
|---|---|
| R1: up to three catalogs per successful create | §§3.4/4.1 document all calls and require one retained result for selection, owners and response. §5 tests exact call count, IDs, non-AAC inputs and remote predecessor negotiation. §7 puts consolidation before B1. |
| R2: global count enforcement breaks old clients | §4.0 chooses compaction first plus negotiated contract enforcement; legacy v2 direct/copy/manual admission is preserved. §§5/7 cover old/new rollout and prohibit bound-first deployment. |
| Bound evidence: 20 codec/profile pairs | §4.0 records the registry table and distinguishes it from actual compacted wire rows; fixture determines the final bound. |
| N1: settings revision mechanism unspecified | §4.1 chooses a transactional generation counter and one consistent query; same-timestamp/delete invalidation tests added. |
| N2: restart clears DV5 memo | §§3.3/5 explicitly cover first play after deploy/restart and subsequent warm play. |
| N3: enclosing startup budget unnamed | §4.1 names proposed server owner and `T_CREATE_START_MS = TBD`; §7 makes numeric budget/reserves a decision required before deadline enforcement. Existing Android timers are evidence to trace, not an assumed outer deadline. |
| N4: M3 also affects calls 2 and 3 | Original M3 disposition now links R1 and the removal of both additional fan-outs. |

**Revision validation:** documentation-index/relative-link checks cover this
untracked document and its retained evidence. No production Rust or Android
code was changed for this revision; the prior manager replay proves only the
narrow scope stated in §2.4. Physical count confirmation and repair acceptance
remain future work.
